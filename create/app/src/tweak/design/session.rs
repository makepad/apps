//! The owner's working source and the stages of one exclusive AppLink operation.
use super::{DesignStateView, GhostView};
use super::fs_ops::{self, FileId, TempCheckError, WriterLiveness};
use crate::tweak::{BakeScope, ThemeInstall, DesignPlace, DesignReplyKind, DesignRequest, DesignOp, DesignVerb, Rect, Seat, StepDir, WindowId};
use crate::tweak::session::Who;
use makepad_strict_json::{self as json, Value};
use makepad_widgets::makepad_platform::thread::{Class, TaskHandle, TaskPool};
use makepad_town_core::{event::Id, literal::is_identifier};
use makepad_widgets::{
    design_data::locate::source_hash,
    makepad_platform::script_mod_doc::{DocKind, ScriptModDoc},
    makepad_script::syntax::{edit::Replace, literal_class::parse_literal_class, widget, write},
};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs, sync::{Arc, atomic::{AtomicBool, Ordering}},
    io::{ErrorKind, Read, Write},
    path::{Path, PathBuf},
};

const DISK_CHANGED: &str = "file changed on disk; reset or export patch first";

pub(crate) fn validate_design_inputs(req: &DesignRequest) -> Result<(), String> {
    let ty = match req {
        DesignRequest::Insert { ty, .. } | DesignRequest::Wrap { ty, .. }
        | DesignRequest::GhostInsert { ty, .. } => Some(ty),
        _ => None,
    };
    if let Some(ty) = ty {
        if ty.split('.').next() == Some("scope") || !ty.split('.').all(is_identifier) {
            return Err("invalid design widget type".into());
        }
    }
    let name = match req {
        DesignRequest::Insert { name, .. } | DesignRequest::Duplicate { name, .. }
        | DesignRequest::Wrap { name, .. } => name.as_deref(),
        DesignRequest::Rename { name, .. } => Some(name.as_str()),
        _ => None,
    };
    if name.is_some_and(|name| !unreserved(name)) {
        return Err("invalid or reserved design widget name".into());
    }
    if let DesignRequest::Insert { props, .. } = req {
        let mut seen = BTreeSet::new();
        for (key, value) in props {
            if !key.split('.').all(unreserved) {
                return Err(format!("invalid design property `{key}`"));
            }
            if !seen.insert(key) {
                return Err(format!("duplicate design property `{key}`"));
            }
            if value.contains("#(") || value.contains("@(") {
                return Err(format!("property `{key}`: interpolation is outside the literal class"));
            }
            parse_literal_class(value).map_err(|err| format!("property `{key}`: {err}"))?;
        }
    }
    Ok(())
}

fn unreserved(s: &str) -> bool {
    write::key(s).is_ok_and(|key| key == s)
}

/// Map a live byte boundary across sorted edits expressed in the pre-ghost source.
pub(crate) fn map_live_to_pre_ghost(live_offset: usize, edits: &[Replace]) -> Result<usize, String> {
    let (mut pre, mut live) = (0usize, 0usize);
    for e in edits {
        let unchanged = e.range.start.checked_sub(pre).ok_or("overlapping ghost edits")?;
        let start = live.checked_add(unchanged).ok_or("invalid ghost offset")?;
        if live_offset < start {
            return Ok(pre + (live_offset - live));
        }
        live = start.checked_add(e.text.len()).ok_or("invalid ghost length")?;
        if live_offset < live {
            return Err("cannot target the active ghost".into());
        }
        pre = e.range.end;
    }
    pre.checked_add(live_offset - live).ok_or_else(|| "invalid source offset".into())
}

#[derive(Clone, Debug)]
pub(crate) struct FileSlot {
    pub path: String,
    pub disk_text: String,
    pub disk_hash: u64,
    pub working: ScriptModDoc,
    pub undo: Vec<String>,
    pub redo: Vec<String>,
}

#[derive(Clone, Debug)]
pub(crate) struct ActiveGhost {
    pub seat: Seat,
    pub agent: Option<Id>,
    pub file: String,
    pub existed_before: bool,
    pub before: ScriptModDoc,
    block: usize,
    base_hash: u64,
    live_hash: u64,
    pub pre_ghost_edits: Vec<Replace>,
    pub pre_ghost_source_open: Option<usize>,
    pub target_key: (String, DesignPlace, String),
    pub op: &'static str,
    pub label: String,
    pub focus_name: Option<String>,
    pub focus_path: Option<String>,
    pub rect: Option<Rect>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum HistoryOp { PushUndo, PopUndo, PopRedo, Reset, GhostDwell, RestoreGhost, RevertGhost }

#[derive(Clone)]
struct LocatedInfo {
    raw: Value,
    file: String,
    hash: u64,
    block: usize,
    open: usize,
    preview: bool,
}

impl LocatedInfo {
    fn parse(raw: Value) -> Result<Self, String> {
        let index = |k| raw.get(k).and_then(Value::as_u64).and_then(|n| n.try_into().ok()).ok_or("invalid locate offset");
        Ok(Self {
            file: string(&raw, "file")?.into(), hash: hash_field(&raw)?,
            block: index("block")?, open: index("open")?,
            preview: raw.get("preview").and_then(Value::as_bool).ok_or("missing locate preview flag")?, raw,
        })
    }
}

struct StagedMutation {
    file: String,
    // A newly opened or externally refreshed clean baseline, installed only on success.
    baseline: Option<FileSlot>,
    previous_doc: Option<ScriptModDoc>,
    base_hash: u64,
    next_doc: ScriptModDoc,
    compact_edits: Option<String>,
    next_hash: u64,
    history: HistoryOp,
    focus_path: Option<String>,
}

enum Preparation {
    State(Value),
    Reply(DesignReplyKind, Value),
    Edits(Vec<StagedMutation>),
    Ghost(StagedMutation, ActiveGhost),
    RetractFirst,
    BakeInputs(BakeInputs),
    LocateThemeRoot(BakeInputs),
    Baked(Vec<StagedMutation>, Vec<(String, String)>),
}
#[derive(Default)]
pub(in crate::tweak) struct BakeInputs {
    live_props: Option<Arc<BTreeMap<(String, String), String>>>,
    paths: std::collections::VecDeque<String>,
    props: BTreeMap<String, Vec<(String, String)>>,
    skipped: Vec<(String, String)>,
    install: Option<Arc<ThemeInstall>>,
    root_path: Option<String>,
    theme: Option<BakeTheme>,
    locations: BTreeMap<String, LocatedInfo>,
    theme_root: Option<LocatedInfo>,
    definition: Option<FileSlot>,
}

struct BakeTheme {
    base: String,
    sheet: Option<String>,
    tokens: Vec<(String, String)>,
    file: Option<String>,
}
type Prepared = Result<(PathBuf, Preparation), String>;
enum PipelineStage {
    Start, Read(DesignReplyKind), VerifyRoot, LocatePrimary, LocateTarget(LocatedInfo),
    Restart, GhostRect,
    BakeTheme, BakeTree, BakeRoot, BakeProp(String),
    Preparing(TaskHandle<Prepared>),
    Upload { id: String, next: usize, restoring: bool },
    Preview { full: bool, restoring: bool },
    Verify { restoring: bool },
    AwaitingAppend { link_open: bool },
}

pub(super) struct PendingDesignOp {
    pub who: Who,
    pub request: DesignRequest,
    stage: PipelineStage,
    next: Option<(Option<WindowId>, DesignOp)>,
    staged: Vec<StagedMutation>,
    window: Option<WindowId>,
    ghost: Option<ActiveGhost>,
    retracting: bool,
    origin: Origin,
    // Revocation and publication race for this one claim. Once publication has started,
    // settle its actual result just like an already submitted append.
    patch_claim: Option<Arc<AtomicBool>>,
    bake: BakeInputs,
    baked: Vec<(String, String)>,
    // The first N files changed in the app. Rollback walks these in reverse.
    applied: usize,
    aborted: Option<String>,
    failure: Option<String>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(in crate::tweak) enum Origin { Owner, Participant, Cleanup }

pub(in crate::tweak) enum Completed {
    Reply(DesignReplyKind, Value),
    GhostStatus { resume: bool, reply: bool },
    CleanupFailed(String),
    Mutation { op: &'static str, path: String, focus_path: Option<String>, old: String, new: String },
}

type Progress = Result<Option<Completed>, String>;

#[derive(Default)]
pub(crate) struct DesignSession {
    workspace_root: Option<PathBuf>,
    run_dir: Option<String>,
    pool: Option<TaskPool>,
    pub(in crate::tweak) files: Arc<BTreeMap<String, FileSlot>>,
    timeline_undo: Vec<String>,
    timeline_redo: Vec<String>,
    ghost: Option<Arc<ActiveGhost>>,
    // A failed mandatory restore keeps the live baseline for retry, with the lease revoked.
    restore_required: bool,
    pending: Option<PendingDesignOp>,
    last_verify: Option<Value>,
}

impl DesignSession {
    #[cfg(test)]
    pub(crate) fn with_root(root: PathBuf) -> Self {
        Self { workspace_root: root.canonicalize().ok().filter(|p| p.is_dir()), ..Self::default() }
    }

    pub(crate) fn from_run_dir(run_dir: Option<&str>) -> Self {
        Self { workspace_root: run_dir.and_then(|s| discover_workspace(Path::new(s))), ..Self::default() }
    }

    pub(crate) fn check_confined(&self, file: &str) -> Result<PathBuf, String> {
        let root = self.workspace_root.as_ref().ok_or("no workspace root for this shared app")?;
        if file.is_empty() || file.contains('\0') {
            return Err("empty or invalid design source path".into());
        }
        let canon = fs::canonicalize(file).map_err(|e| format!("cannot resolve {file}: {e}"))?;
        if !canon.starts_with(root) {
            return Err("design source is outside the workspace".into());
        }
        if !canon.is_file() || !matches!(canon.extension().and_then(|e| e.to_str()), Some("rs" | "splash")) {
            return Err("design source must be a .rs or .splash file".into());
        }
        Ok(canon)
    }

    /// Build an unopened slot. Its caller publishes it only after the app accepts the preview.
    pub(crate) fn read_slot(&self, file: &str) -> Result<FileSlot, String> {
        let canon = self.check_confined(file)?;
        let path = canon.to_str().ok_or("design source path is not UTF-8")?.to_owned();
        let kind = if path.ends_with(".rs") { DocKind::RustScriptMod } else { DocKind::Splash };
        let disk_text = fs::read_to_string(&canon).map_err(|e| format!("read design source: {e}"))?;
        let working = ScriptModDoc::parse(&path, disk_text.clone(), kind)?;
        Ok(FileSlot { path, disk_hash: working.hash(), disk_text, working, undo: vec![], redo: vec![] })
    }

    pub(crate) fn state_view(&self) -> DesignStateView {
        let ghost = self.ghost.as_ref();
        let dirty_files = self.files.values().filter(|slot| {
            let text = ghost.filter(|g| g.file == slot.path)
                .map_or(slot.working.text.as_str(), |g| &g.before.text);
            text != slot.disk_text
        }).count();
        DesignStateView {
            active: self.workspace_root.is_some() || self.run_dir.is_some(),
            dirty_files,
            undo: self.timeline_undo.len(),
            redo: self.timeline_redo.len(),
            ghost: ghost.map(|g| GhostView { seat: g.seat, label: g.focus_name.as_ref().unwrap_or(&g.label).clone(), rect: g.rect }),
        }
    }

    /// Called by the local owner's Commit action. Preflight the entire dirty set before
    /// replacing the first source. Each successful rename advances that slot's disk baseline.
    pub(crate) fn commit_to_disk(&mut self) -> Result<(), String> {
        if self.pending.is_some() || self.ghost.is_some() {
            return Err("settle the current design operation before committing".into());
        }
        let pid = std::process::id();
        let self_start = makepad_remote::group::start_time(pid).filter(|&t| t > 0);
        let mut preflighted = Vec::new();
        let mut stale_temps = Vec::new();
        for slot in self.files.values().filter(|s| s.disk_text != s.working.text) {
            let canon = self.check_confined(&slot.path)?;
            if canon.to_str() != Some(slot.path.as_str()) {
                return Err(DISK_CHANGED.into());
            }
            let mut original = fs::File::open(&canon).map_err(|e| format!("open design source: {e}"))?;
            let metadata = original.metadata().map_err(|e| format!("inspect design source: {e}"))?;
            let modified = metadata.modified().map_err(|e| format!("inspect source modification time: {e}"))?;
            let mut disk = String::new();
            original.read_to_string(&mut disk).map_err(|e| format!("read design source: {e}"))?;
            if source_hash(&disk) != slot.disk_hash || disk != slot.disk_text {
                return Err(DISK_CHANGED.into());
            }
            collect_stale_temps(&canon, self_start, &mut stale_temps)?;
            preflighted.push((canon, slot, modified, metadata.permissions()));
        }

        for stale in stale_temps {
            if let Some(crashed) = quarantine_crashed_temp(&stale)? {
                makepad_widgets::log!(
                    "Town design commit: moved crashed temporary source {} (pid {}, start {}) aside to {}",
                    stale.path.display(), stale.pid, stale.start, crashed.display(),
                );
            }
        }
        let suffix = self_start.map_or_else(|| ".town-tmp".into(), |start| format!(".town-tmp.{pid}.{start}"));
        let mut ready = Vec::new();
        for (canon, slot, modified, permissions) in preflighted {
            let temp = sibling_path(&canon, &suffix)?;
            let output = fs::File::create_new(&temp).map_err(|e| {
                if e.kind() == ErrorKind::AlreadyExists { format!("temporary source already exists: {}", temp.display()) }
                else { format!("create temporary source: {e}") }
            })?;
            let mut temporary = TemporarySource { path: temp, renamed: false, _lock: Some(output) };
            let output = temporary._lock.as_mut().expect("new temporary source");
            #[cfg(any(unix, windows))]
            output.try_lock().map_err(|e| format!("lock temporary source: {e}"))?;
            output.set_permissions(permissions).map_err(|e| format!("preserve source permissions: {e}"))?;
            output.write_all(slot.working.text.as_bytes()).map_err(|e| format!("write temporary source: {e}"))?;
            ready.push((slot.path.clone(), modified, temporary));
        }
        for (path, modified, mut temporary) in ready {
            let canon = self.check_confined(&path)?;
            let metadata = fs::metadata(&canon).map_err(|e| format!("inspect design source: {e}"))?;
            let slot = Arc::make_mut(&mut self.files).get_mut(&path).expect("preflighted slot");
            if canon.to_str() != Some(path.as_str()) || metadata.len() != slot.disk_text.len() as u64
                || metadata.modified().ok() != Some(modified) {
                return Err(DISK_CHANGED.into());
            }
            #[cfg(not(unix))]
            drop(temporary._lock.take());
            fs::rename(&temporary.path, &canon).map_err(|e| format!("replace design source: {e}"))?;
            temporary.renamed = true;
            slot.disk_text.clone_from(&slot.working.text);
            slot.disk_hash = slot.working.hash();
        }
        Ok(())
    }
}

struct TemporarySource {
    path: PathBuf,
    renamed: bool,
    _lock: Option<fs::File>,
}

impl Drop for TemporarySource {
    fn drop(&mut self) {
        if !self.renamed {
            #[cfg(not(unix))]
            drop(self._lock.take());
            let _ = fs::remove_file(&self.path);
        }
    }
}

pub(in crate::tweak) struct StaleTemp {
    path: PathBuf,
    canon: PathBuf,
    pid: u32,
    start: u64,
    id: FileId,
}

fn sibling_path(canon: &Path, suffix: &str) -> Result<PathBuf, String> {
    let mut name = canon.file_name().ok_or("design source has no filename")?.to_os_string();
    name.push(suffix);
    Ok(canon.with_file_name(name))
}

fn temp_writer(suffix: &[u8]) -> Option<(u32, u64)> {
    let suffix = std::str::from_utf8(suffix.strip_prefix(b".")?).ok()?;
    let (pid_s, start_s) = suffix.split_once('.')?;
    let pid: u32 = pid_s.parse().ok()?;
    let start: u64 = start_s.parse().ok()?;
    (pid > 0 && start > 0 && pid.to_string() == pid_s && start.to_string() == start_s).then_some((pid, start))
}

pub(in crate::tweak) fn collect_stale_temps(canon: &Path, self_start: Option<u64>, stale: &mut Vec<StaleTemp>) -> Result<(), String> {
    let legacy = sibling_path(canon, ".town-tmp")?;
    let prefix = legacy.file_name().expect("named temporary source").as_encoded_bytes();
    let parent = canon.parent().ok_or("design source has no parent")?;
    let scan_error = |e| format!("scan temporary sources in {}: {e}", parent.display());
    for entry in fs::read_dir(parent).map_err(scan_error)? {
        let entry = entry.map_err(scan_error)?;
        let name = entry.file_name();
        let Some(suffix) = name.as_encoded_bytes().strip_prefix(prefix) else { continue };
        if !suffix.is_empty() && !suffix.starts_with(b".") { continue; }

        let path = entry.path();
        let ambiguous = || format!("temporary source already exists: {}", path.display());
        let metadata = fs::symlink_metadata(&path).map_err(|_| ambiguous())?;
        if !fs_ops::is_plain_file(&metadata) || self_start.is_none() { return Err(ambiguous()); }
        let (pid, start) = temp_writer(suffix).ok_or_else(ambiguous)?;
        let in_use = || format!("temporary source is in use by pid {pid}: {}", path.display());
        match fs_ops::probe_writer(pid, start) {
            WriterLiveness::Dead => {}
            WriterLiveness::Alive => return Err(in_use()),
            WriterLiveness::Unknown => return Err(ambiguous()),
        }
        let id = match fs_ops::inspect_regular_unlocked(&path) {
            Ok(id) => id,
            Err(TempCheckError::InUse) => return Err(in_use()),
            Err(TempCheckError::Ambiguous) => return Err(ambiguous()),
        };
        stale.push(StaleTemp { path, canon: canon.to_owned(), pid, start, id });
    }
    Ok(())
}

pub(in crate::tweak) fn quarantine_crashed_temp(stale: &StaleTemp) -> Result<Option<PathBuf>, String> {
    let _guard = match fs_ops::verify_and_lock_stale(&stale.path, stale.id) {
        Ok(Some(guard)) => guard,
        Ok(None) => return Ok(None),
        Err(TempCheckError::InUse) => {
            return Err(format!("temporary source is in use by pid {}: {}", stale.pid, stale.path.display()));
        }
        Err(TempCheckError::Ambiguous) => {
            return Err(format!("temporary source already exists: {}", stale.path.display()));
        }
    };
    let mut attempt = 0u64;
    loop {
        let suffix = if attempt == 0 {
            format!(".town.{}.{}.crashed", stale.pid, stale.start)
        } else {
            format!(".town.{}.{}.{attempt}.crashed", stale.pid, stale.start)
        };
        let dst = sibling_path(&stale.canon, &suffix)?;
        match fs_ops::move_no_replace(&stale.path, &dst) {
            Ok(()) => return Ok(Some(dst)),
            Err(e) if e.kind() == ErrorKind::AlreadyExists => attempt += 1,
            Err(e) if e.kind() == ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(format!("quarantine crashed temporary source {}: {e}", stale.path.display())),
        }
    }
}

fn discover_workspace(run_dir: &Path) -> Option<PathBuf> {
    if !run_dir.is_absolute() { return None }
    let dir = run_dir.canonicalize().ok().filter(|d| d.is_dir())?;
    for ancestor in dir.ancestors() {
        if ancestor.join(".git").try_exists().ok()? {
            return Some(ancestor.to_owned());
        }
        match fs::read_to_string(ancestor.join("Cargo.toml")) {
            Ok(manifest) if manifest.lines().any(|line| line.split('#').next().unwrap_or("").trim() == "[workspace]") => {
                return Some(ancestor.to_owned());
            }
            Ok(_) => {}
            Err(e) if e.kind() == ErrorKind::NotFound => {}
            Err(_) => return None,
        }
    }
    Some(dir)
}

fn string<'a>(v: &'a Value, key: &str) -> Result<&'a str, String> {
    v.get(key).and_then(Value::as_str).ok_or_else(|| format!("missing design field {key}"))
}

fn hash_field(v: &Value) -> Result<u64, String> {
    u64::from_str_radix(string(v, "file_hash")?, 16).map_err(|_| "invalid design file hash".into())
}

fn errors(v: &Value) -> Result<Vec<&str>, String> {
    v.get("errors").and_then(Value::as_arr).ok_or("missing reload errors")?.iter()
        .map(|e| e.as_str().ok_or_else(|| "invalid reload error".into())).collect()
}

fn operation(req: &DesignRequest) -> &'static str {
    use DesignRequest::*;
    match req {
        Insert { .. } => "insert", Move { .. } => "move", Step { dir, .. } => match dir {
            StepDir::Up => "up", StepDir::Down => "down", StepDir::Out => "out",
        },
        Delete { .. } => "delete", Duplicate { .. } => "duplicate", Wrap { .. } => "wrap",
        Rename { .. } => "rename", Bake { .. } => "bake", Undo => "undo", Redo => "redo", Reset => "reset",
        _ => "design",
    }
}

fn request_path(req: &DesignRequest) -> Option<&str> {
    use DesignRequest::*;
    match req {
        Insert { path, .. } | Move { path, .. } | Step { path, .. } | Delete { path, .. }
        | Duplicate { path, .. } | Wrap { path, .. } | Rename { path, .. } => Some(path),
        State { path } | Verify { path } | Bake { path, .. } => path.as_deref(),
        GhostInsert { target, .. } | GhostMove { target, .. } => Some(target),
        _ => None,
    }
}

impl DesignSession {
    pub(in crate::tweak) fn set_run_dir(&mut self, dir: Option<&str>) -> bool {
        if self.workspace_root.is_some() || self.run_dir.is_some() { return false }
        self.run_dir = dir.filter(|d| Path::new(d).is_absolute()).map(str::to_owned);
        self.run_dir.is_some()
    }

    pub(in crate::tweak) fn configure_worker(&mut self, pool: &TaskPool) {
        self.pool = Some(pool.clone());
    }

    pub(in crate::tweak) fn current(&self) -> Option<(Who, &DesignRequest)> {
        self.pending.as_ref().filter(|p| !matches!(p.stage, PipelineStage::AwaitingAppend { .. })).map(|p| (p.who, &p.request))
    }

    pub(in crate::tweak) fn holder(&self) -> Option<Who> {
        self.ghost.as_ref().map(|g| Who { seat: g.seat, agent: g.agent, req: 0 })
    }

    pub(in crate::tweak) fn cleaning(&self) -> bool {
        self.pending.as_ref().is_some_and(|p| p.origin == Origin::Cleanup)
    }

    pub(in crate::tweak) fn abort(&mut self, why: &str) {
        if let Some(p) = self.pending.as_mut().filter(|p| p.origin != Origin::Cleanup) {
            if p.patch_claim.as_ref().is_none_or(|claim| !claim.swap(true, Ordering::AcqRel)) {
                p.aborted.get_or_insert_with(|| why.into());
            }
        }
    }

    pub(in crate::tweak) fn link_lost(&mut self) -> Option<Who> {
        // A promoted ghost still needs its pre-drag snapshot for the outstanding signed append.
        if !self.pending.as_ref().is_some_and(|p| matches!(p.request, DesignRequest::GhostCommit)
            && matches!(p.stage, PipelineStage::AwaitingAppend { .. })) {
            self.clear_ghost();
        }
        if let Some(PipelineStage::AwaitingAppend { link_open }) = self.pending.as_mut().map(|p| &mut p.stage) {
            *link_open = false;
            return None;
        }
        if self.pending.as_ref().is_some_and(|p| p.patch_claim.is_some()) {
            self.abort("the shared app disconnected");
            return None;
        }
        self.pending.take().map(|p| p.who)
    }

    pub(in crate::tweak) fn record_saved(&mut self) -> (Vec<(String, String)>, Option<Value>) {
        let verified = self.pending.take().expect("structural append has a verified preview");
        if matches!(verified.request, DesignRequest::GhostCommit) {
            let ghost = self.ghost.take().expect("verified ghost promotion");
            self.clear_redo();
            let files = Arc::make_mut(&mut self.files);
            let ghost = Arc::unwrap_or_clone(ghost);
            files.get_mut(&ghost.file).expect("previewed ghost slot").undo.push(ghost.before.text);
            self.timeline_undo.push(ghost.file);
        }
        let report = (!verified.bake.skipped.is_empty()).then(|| verified.bake_report(true));
        for file in verified.staged { self.publish(file); }
        (verified.baked, report)
    }

    /// A refused signed append must restore every preview that was waiting for that append.
    pub(in crate::tweak) fn record_failed(&mut self, why: &str) -> bool {
        let Some(p) = &mut self.pending else { return false };
        match p.stage {
            PipelineStage::AwaitingAppend { link_open: true } => {
                p.stage = PipelineStage::Start;
                p.aborted = Some(why.into());
                true
            }
            PipelineStage::AwaitingAppend { link_open: false } => { self.pending = None; self.clear_ghost(); false }
            _ => false,
        }
    }

    pub(in crate::tweak) fn next_app_op(&mut self) -> Option<(Option<WindowId>, DesignOp)> {
        self.pending.as_mut()?.next.take()
    }

    fn advance(&mut self, step: impl FnOnce(&mut PendingDesignOp, &mut Self) -> Progress) -> Progress {
        let mut p = self.pending.take().expect("active structural operation");
        let outcome = match step(&mut p, self) {
            Err(why) if p.origin == Origin::Cleanup => {
                self.restore_required = true;
                Ok(Some(Completed::CleanupFailed(why)))
            }
            outcome => outcome,
        };
        if matches!(outcome, Ok(None | Some(Completed::Mutation { .. } | Completed::GhostStatus { resume: true, .. }))) { self.pending = Some(p); }
        outcome
    }

    pub(in crate::tweak) fn begin(&mut self, who: Who, request: DesignRequest, window: Option<WindowId>, bake: BakeInputs, origin: Origin) -> Progress {
        debug_assert!(self.pending.is_none());
        let patch_claim = matches!(request, DesignRequest::WritePatch { .. }).then(|| Arc::new(AtomicBool::new(false)));
        self.pending = Some(PendingDesignOp { who, request, stage: PipelineStage::Start, next: None,
            staged: Vec::new(), window, ghost: None, retracting: false, origin, patch_claim, bake, baked: Vec::new(), applied: 0, aborted: None, failure: None });
        self.advance(|p, s| p.start(s, window))
    }

    pub(in crate::tweak) fn poll(&mut self) -> Progress {
        self.advance(|p, s| {
            if p.aborted.is_some() && p.failure.is_none() {
                if let (Some(_), PipelineStage::Preparing(job)) = (&p.patch_claim, &mut p.stage) {
                    // The worker removes its staged export before the FIFO is released.
                    if job.try_take().is_none() { return Ok(None); }
                }
                return p.fail(p.aborted.clone().unwrap());
            }
            if matches!(p.stage, PipelineStage::Restart) { return p.start(s, p.window); }
            if let PipelineStage::Preparing(job) = &mut p.stage {
                if let Some(result) = job.try_take() {
                    return p.prepared(s, result.map_err(|e| format!("design worker: {e:?}"))??);
                }
            }
            Ok(None)
        })
    }

    pub(in crate::tweak) fn reply(&mut self, window: Option<WindowId>, ok: bool, body: &str) -> Progress {
        let v = crate::tweak::parse_json(body);
        let result = if ok { v.map_err(str::to_owned) } else {
            Err(v.ok().as_ref().and_then(|v| v.get("err")).and_then(Value::as_str).unwrap_or(body).into())
        };
        self.advance(|p, s| {
            if ok && result.is_err() && matches!(p.stage, PipelineStage::Preview { restoring: false, .. }) {
                p.applied += 1; // A malformed success can still have installed the candidate.
            }
            p.answer(s, window, result)
        })
    }
}

impl PendingDesignOp {
    fn send(&mut self, stage: PipelineStage, window: Option<WindowId>, verb: DesignVerb, args: Vec<(&str, String)>) -> Progress {
        self.stage = stage;
        self.next = Some((window, DesignOp::Design { verb, args: args.into_iter().map(|(k,v)| (k.into(),v)).collect() }));
        Ok(None)
    }

    fn start(&mut self, s: &mut DesignSession, window: Option<WindowId>) -> Progress {
        use DesignRequest::*;
        if let Some(g) = &s.ghost {
            if matches!(self.request, GhostCancel) && (g.seat != self.who.seat || g.agent != self.who.agent) {
                return Ok(Some(Completed::Reply(DesignReplyKind::State, s.state_view().json())));
            }
            if s.restore_required && self.request.is_ghost() {
                if matches!(self.request, GhostCommit) { return Err("the drag was cancelled".into()); }
                return self.retract(s);
            }
            if self.request.is_ghost() {
                if g.seat != self.who.seat || g.agent != self.who.agent {
                    return Err("Another seat is currently dragging in this app.".into());
                }
                if let Some((key, op)) = drag_key(&self.request) {
                    if key == g.target_key && op == g.op {
                        return Ok(Some(Completed::Reply(DesignReplyKind::State, s.state_view().json())));
                    }
                    if op != g.op || key.2 != g.target_key.2 { return self.retract(s); }
                }
                if matches!(self.request, GhostCommit) {
                    self.stage = PipelineStage::AwaitingAppend { link_open: true };
                    return Ok(Some(Completed::Mutation { op: g.op, path: g.target_key.0.clone(), focus_path: g.focus_path.clone(),
                        old: format!("{:016x}", g.base_hash), new: format!("{:016x}", g.live_hash) }));
                }
            }
            if matches!(self.request, GhostCancel) || (!self.request.is_ghost() && !self.request.is_read_only()
                && !matches!(self.request, WritePatch { .. })) { return self.retract(s); }
        }
        match &self.request {
            GhostCancel => Ok(Some(Completed::Reply(DesignReplyKind::State, s.state_view().json()))),
            GhostCommit => Err("no active ghost to settle".into()),
            GhostMove { source, target, .. } => {
                let path = if s.ghost.is_some() { target } else { source };
                self.send(PipelineStage::LocatePrimary, window, DesignVerb::Locate, vec![("path", path.clone())])
            }
            Diff { .. } | WritePatch { .. } => {
                let req = self.request.clone();
                let origin = self.origin;
                let claim = self.patch_claim.clone();
                self.worker(s, move |snapshot| snapshot.patch(&req, origin, claim.as_deref()))
            }
            Bake { scope, .. } => {
                if *scope == BakeScope::Props { self.collect_bake(s, None) } else {
                    self.send(PipelineStage::BakeTheme, None, DesignVerb::Theme, vec![("get", "1".into())])
                }
            }
            State { path: None } => Ok(Some(Completed::Reply(DesignReplyKind::State, s.state_view().json()))),
            Palette { .. } => self.send(PipelineStage::Read(DesignReplyKind::Palette), None, DesignVerb::Palette, vec![]),
            DropTarget { window: w, x, y, exclude } => {
                if window != Some(WindowId(*w)) { return Err("That window is outside this share.".into()); }
                let mut args = vec![("at", format!("{x},{y}"))];
                if let Some(path) = exclude { args.push(("exclude", path.clone())); }
                self.send(PipelineStage::Read(DesignReplyKind::DropTarget), window, DesignVerb::Drop, args)
            }
            Verify { path } => {
                let args = if let Some(path) = path { vec![("path", path.clone())] }
                else if let Some(file) = s.files.keys().next() { vec![("file", file.clone())] }
                else {
                    // No-argument verify belongs to the app's local Tweaker when installed.
                    return self.send(PipelineStage::VerifyRoot, window, DesignVerb::Tree,
                        vec![("depth", "1".into()), ("offset", "0".into()), ("limit", "1".into())]);
                };
                self.send(PipelineStage::Read(DesignReplyKind::Verify), window, DesignVerb::Verify, args)
            }
            Undo | Redo | Reset => self.prepare(s, None, None),
            _ if request_path(&self.request).is_some() => {
                if s.workspace_root.is_none() && s.run_dir.is_none() { return Err("no workspace root for this shared app".into()); }
                let path = request_path(&self.request).unwrap().to_owned();
                self.send(PipelineStage::LocatePrimary, window, DesignVerb::Locate, vec![("path", path)])
            }
            _ => Err("This design operation is unavailable.".into()),
        }
    }

    fn prepare(&mut self, s: &DesignSession, primary: Option<LocatedInfo>, target: Option<LocatedInfo>) -> Progress {
        let history_file = match self.request {
            DesignRequest::Undo => Some(s.timeline_undo.last().ok_or("nothing to undo")?.clone()),
            DesignRequest::Redo => Some(s.timeline_redo.last().ok_or("nothing to redo")?.clone()),
            _ => None,
        };
        let req = self.request.clone();
        let who = self.who;
        self.worker(s, move |snapshot| snapshot.prepare_source(who, &req, primary, target, history_file.as_deref()))
    }

    fn worker(&mut self, s: &DesignSession, work: impl FnOnce(DesignSession) -> Result<Preparation, String> + Send + 'static) -> Progress {
        let root = s.workspace_root.clone();
        let dir = s.run_dir.clone();
        let files = s.files.clone();
        let ghost = s.ghost.clone();
        let task = move || {
            let root = root.or_else(|| DesignSession::from_run_dir(dir.as_deref()).workspace_root)
                .ok_or("no workspace root for this shared app")?;
            let snapshot = DesignSession { workspace_root: Some(root.clone()), files, ghost, ..Default::default() };
            let prepared = work(snapshot)?;
            Ok((root, prepared))
        };
        let pool = s.pool.as_ref().ok_or("design worker unavailable")?;
        self.stage = PipelineStage::Preparing(pool.submit_named(Class::Interactive, "Town design source", task)
            .map_err(|e| format!("start design worker: {e:?}"))?);
        Ok(None)
    }

    fn prepared(&mut self, s: &mut DesignSession, (root, prepared): (PathBuf, Preparation)) -> Progress {
        s.workspace_root = Some(root);
        match prepared {
            Preparation::State(located) => {
                let Value::Obj(mut fields) = s.state_view().json() else { unreachable!() };
                fields.push(("located".into(), located));
                Ok(Some(Completed::Reply(DesignReplyKind::State, Value::Obj(fields))))
            }
            Preparation::Reply(kind, value) => Ok(Some(Completed::Reply(kind, value))),
            Preparation::RetractFirst => self.retract(s),
            Preparation::BakeInputs(bake) => { self.bake = bake; self.next_bake(s) }
            Preparation::LocateThemeRoot(bake) => { self.bake = bake; self.locate_theme_root() }
            Preparation::Baked(edits, skipped) => {
                self.bake.skipped = skipped;
                if edits.is_empty() {
                    return Ok(Some(Completed::Reply(DesignReplyKind::State, self.bake_report(false))));
                }
                self.staged = edits;
                self.preview(false, false)
            }
            Preparation::Ghost(edit, ghost) => {
                let full = s.ghost.is_some();
                self.staged = vec![edit];
                self.ghost = Some(ghost);
                self.preview(full, false)
            }
            Preparation::Edits(edits) => {
                self.staged = edits;
                self.preview(false, false)
            }
        }
    }

    fn preview(&mut self, full: bool, restoring: bool) -> Progress {
        let idx = if restoring { self.applied - 1 } else { self.applied };
        let m = &self.staged[idx];
        let mut args = vec![("file", m.file.clone())];
        if (restoring && m.previous_doc.is_none()) || (!restoring && matches!(m.history, HistoryOp::Reset | HistoryOp::RevertGhost)) {
            return self.send(PipelineStage::Preview { full: true, restoring }, None, DesignVerb::Revert, args);
        }
        if !full && !restoring {
            if let Some(encoded) = &m.compact_edits {
                args.extend([("base_hash", format!("{:016x}", m.base_hash)), ("edits", encoded.clone())]);
                return self.send(PipelineStage::Preview { full: false, restoring }, None, DesignVerb::Preview, args);
            }
        }
        let text = if restoring { &m.previous_doc.as_ref().unwrap().text } else { &m.next_doc.text };
        if (restoring || matches!(m.history, HistoryOp::GhostDwell | HistoryOp::RestoreGhost)) && text.len() <= 32 * 1024 {
            args.push(("text", text.clone()));
            return self.send(PipelineStage::Preview { full: true, restoring }, None, DesignVerb::Preview, args);
        }
        // An aborted upload can persist in the app. A new attempt always starts a new upload.
        static NEXT_UPLOAD: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let serial = NEXT_UPLOAD.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let id = format!("{:032x}-{serial}", self.who.req);
        self.upload(id, 0, restoring)
    }

    fn upload(&mut self, id: String, seq: usize, restoring: bool) -> Progress {
        let m = &self.staged[if restoring { self.applied - 1 } else { self.applied }];
        let text = if restoring { &m.previous_doc.as_ref().unwrap().text } else { &m.next_doc.text };
        let mut args = vec![("file", m.file.clone())];
        if let Some(chunk) = text.as_bytes().chunks(48 * 1024).nth(seq).or_else(|| (seq == 0).then_some(&[][..])) {
            args.extend([("id", id.clone()), ("seq", seq.to_string()), ("data", makepad_network::digest::base64_encode(chunk))]);
            self.send(PipelineStage::Upload { id, next: seq + 1, restoring }, None, DesignVerb::PreviewChunk, args)
        } else {
            args.push(("upload", id));
            self.send(PipelineStage::Preview { full: true, restoring }, None, DesignVerb::Preview, args)
        }
    }

    fn fail(&mut self, err: String) -> Progress {
        self.next = None;
        self.failure.get_or_insert(err);
        if self.applied == 0 { return Err(self.failure.take().unwrap()); }
        self.preview(true, true)
    }

    fn answer(&mut self, s: &mut DesignSession, window: Option<WindowId>, response: Result<Value,String>) -> Progress {
        let stage = std::mem::replace(&mut self.stage, PipelineStage::Start);
        let restoring = self.failure.is_some();
        let result = match stage {
            PipelineStage::Preview { full, restoring } => {
                if !restoring && !full && self.aborted.is_none() && response.as_ref().err().is_some_and(|e| e == "hash_mismatch") {
                    return self.preview(true, false);
                }
                // On transport refusal the app did not accept this override. An invalid success
                // body is ambiguous, so conservatively restore the candidate too.
                response.and_then(|v| {
                    if !restoring { self.applied += 1; }
                    let problems = errors(&v)?;
                    if !problems.is_empty() { return Err(problems.join("; ")); }
                    if v.get("ok").and_then(Value::as_bool) != Some(true) { return Err("preview refused".into()); }
                    let pending = v.get("pending_reload").and_then(Value::as_bool).ok_or("missing preview reload state")?;
                    if !restoring {
                        if let Some(why) = &self.aborted { return Err(why.clone()); }
                        if self.staged[self.applied-1].history != HistoryOp::RevertGhost && hash_field(&v)? != self.staged[self.applied-1].next_hash { return Err("unexpected preview file hash".into()); }
                    }
                    if pending { self.verify(restoring, window) } else { self.verified(s, restoring) }
                })
            }
            PipelineStage::Verify { restoring } => response.and_then(|v| {
                let problems = errors(&v)?;
                let pending = v.get("pending_reload").and_then(Value::as_bool) == Some(true);
                let ok = v.get("ok").and_then(Value::as_bool) == Some(true) && problems.is_empty() && !pending;
                let rolled_back = !ok && !pending && v.get("rolled_back").and_then(Value::as_bool) == Some(true);
                if !restoring && rolled_back { self.applied -= 1; }
                if !restoring {
                    s.last_verify = Some(v.clone());
                    if let Some(why) = &self.aborted { return Err(why.clone()); }
                }
                if ok {
                    self.verified(s, restoring)
                } else if !problems.is_empty() {
                    Err(problems.join("; "))
                } else if pending {
                    Err("target app did not finish reload before verify".into())
                } else {
                    Err("reload verification failed".into())
                }
            }),
            _ if self.aborted.is_some() && !restoring => Err(self.aborted.clone().unwrap()),
            PipelineStage::Upload { id, next, restoring } => response.and_then(|v| {
                if v.get("ok").and_then(Value::as_bool) != Some(true) || v.get("next_seq").and_then(Value::as_u64) != Some(next as u64) {
                    return Err("invalid chunk acknowledgement".into());
                }
                self.upload(id, next, restoring)
            }),
            PipelineStage::GhostRect => {
                let ghost = self.ghost.as_mut().expect("verified ghost");
                ghost.rect = response.ok().and_then(|v| {
                    let (path, _, rect) = crate::tweak::app::picked(&v).ok()?;
                    (self.window.is_none_or(|w| w.0 == rect.window) && (Some(&path) == ghost.focus_path.as_ref()
                        || v.get("path").and_then(Value::as_str) == ghost.focus_path.as_deref())).then_some(rect)
                });
                self.publish_ghost(s)
            }
            PipelineStage::BakeTheme => response.and_then(|v| self.collect_bake(s, Some(v))),
            PipelineStage::BakeTree => response.and_then(|v| {
                let path = tree_root(&v)?.to_owned();
                self.send(PipelineStage::BakeRoot, window, DesignVerb::Locate, vec![("path", path)])
            }),
            PipelineStage::BakeRoot => response.and_then(LocatedInfo::parse).and_then(|loc| {
                self.bake.theme_root = Some(loc);
                self.next_bake(s)
            }),
            PipelineStage::BakeProp(path) => response.and_then(LocatedInfo::parse).and_then(|loc| {
                self.bake.locations.insert(path, loc);
                self.next_bake(s)
            }),
            PipelineStage::VerifyRoot => response.and_then(|v| {
                let path = tree_root(&v)?;
                self.send(PipelineStage::Read(DesignReplyKind::Verify), window, DesignVerb::Verify, vec![("path", path.into())])
            }),
            PipelineStage::Read(kind) => response.and_then(|v| {
                let value = match &self.request {
                    DesignRequest::Palette { query, offset, limit } => palette_page(v, query.as_deref(), *offset, *limit)?,
                    DesignRequest::Verify { .. } => { s.last_verify = Some(v.clone()); v }
                    _ => v,
                };
                Ok(Some(Completed::Reply(kind, value)))
            }),
            PipelineStage::LocatePrimary => response.and_then(LocatedInfo::parse).and_then(|primary| {
                if s.ghost.is_none() {
                    if let DesignRequest::Move { target, .. } | DesignRequest::GhostMove { target, .. } = &self.request {
                        return self.send(PipelineStage::LocateTarget(primary), window, DesignVerb::Locate, vec![("path", target.clone())]);
                    }
                }
                self.prepare(s, Some(primary), None)
            }),
            PipelineStage::LocateTarget(primary) => response.and_then(LocatedInfo::parse).and_then(|target| self.prepare(s, Some(primary), Some(target))),
            _ => Err("unexpected structural app reply".into()),
        };
        match result {
            Err(err) if restoring && self.failure.is_none() => Err(err),
            Err(err) if restoring => Err(format!("{}; restore: {err}", self.failure.as_deref().unwrap_or("design rollback"))),
            Err(err) => self.fail(err),
            ok => ok,
        }
    }

    fn verify(&mut self, restoring: bool, window: Option<WindowId>) -> Progress {
        let file = self.staged[self.applied - 1].file.clone();
        self.send(PipelineStage::Verify { restoring }, window, DesignVerb::Verify, vec![("file", file)])
    }

    fn verified(&mut self, s: &mut DesignSession, restoring: bool) -> Progress {
        if restoring {
            self.applied -= 1;
            if self.applied == 0 { return Err(self.failure.take().unwrap()); }
            return self.preview(true, true);
        }
        if self.applied < self.staged.len() { return self.preview(false, false); }
        if self.retracting {
            s.clear_ghost();
            self.staged.clear();
            self.applied = 0;
            self.retracting = false;
            let resume = !matches!(self.request, DesignRequest::GhostCancel);
            self.stage = PipelineStage::Restart;
            return Ok(Some(Completed::GhostStatus { resume, reply: !resume && self.origin != Origin::Cleanup }));
        }
        if let Some(g) = &self.ghost {
            if let Some(path) = &g.focus_path {
                return self.send(PipelineStage::GhostRect, self.window, DesignVerb::Pick, vec![("path", path.clone())]);
            }
            return self.publish_ghost(s);
        }
        let op = operation(&self.request);
        let path = request_path(&self.request).unwrap_or("/").to_owned();
        let focus_path = self.staged.last().and_then(|m| m.focus_path.clone());
        let (old, new) = if let [m] = self.staged.as_slice() {
            (format!("{:016x}", m.base_hash), format!("{:016x}", m.next_hash))
        } else { (format!("{} previewed files", self.staged.len()), if op == "reset" { "disk".into() } else { "baked".into() }) };
        self.stage = PipelineStage::AwaitingAppend { link_open: true };
        Ok(Some(Completed::Mutation { op, path, focus_path, old, new }))
    }
}

impl DesignSession {
    // Runs on the task pool. The immutable map snapshot outlives the task even on disconnect.
    fn prepare_source(&self, who: Who, req: &DesignRequest, located: Option<LocatedInfo>, target: Option<LocatedInfo>,
        history_file: Option<&str>) -> Result<Preparation, String> {
        let Some(info) = located else {
            let keys: Vec<&str> = match history_file {
                Some(file) => vec![file],
                None => self.files.keys().map(String::as_str).collect(),
            };
            if keys.is_empty() { return Err("no design previews to reset".into()); }
            let mut staged = Vec::new();
            for key in keys {
                let slot = self.files.get(key).ok_or("design history file is missing")?;
                let disk = self.read_slot(key)?;
                if disk.path != key { return Err(DISK_CHANGED.into()); }
                let (history, next) = match req {
                    DesignRequest::Reset => (HistoryOp::Reset, disk.working),
                    DesignRequest::Undo | DesignRequest::Redo => {
                        if disk.disk_hash != slot.disk_hash { return Err(DISK_CHANGED.into()); }
                        let (history, stack) = if matches!(req, DesignRequest::Undo) { (HistoryOp::PopUndo, &slot.undo) }
                            else { (HistoryOp::PopRedo, &slot.redo) };
                        let text = stack.last().ok_or("empty file history")?.clone();
                        (history, ScriptModDoc::parse(key, text, slot.working.kind)?)
                    }
                    _ => return Err("missing design location".into()),
                };
                staged.push(StagedMutation { file: key.into(), baseline: None, previous_doc: Some(slot.working.clone()),
                    base_hash: slot.working.hash(), next_hash: next.hash(),
                    compact_edits: if history == HistoryOp::Reset { None } else { compact_edits(&replacement(&slot.working.text, &next.text)) },
                    next_doc: next, history, focus_path: None });
            }
            return Ok(Preparation::Edits(staged));
        };
        let disk = self.read_slot(&info.file)?;
        let file = disk.path.clone();
        if let Some(t) = &target {
            if self.check_confined(&t.file)? != Path::new(&file) || t.block != info.block {
                return Err("cannot move a widget across files or script blocks".into());
            }
            if t.hash != info.hash || t.preview != info.preview { return Err("source changed between locations".into()); }
        }
        if req.is_ghost() && self.ghost.as_ref().is_some_and(|g| g.file != file) {
            return Ok(Preparation::RetractFirst);
        }
        let existing = self.files.get(&file);
        let drift = existing.is_some_and(|s| s.disk_hash != disk.disk_hash || s.disk_text != disk.disk_text);
        if drift && (info.preview || existing.is_some_and(|s| s.working.text != s.disk_text)) {
            return Err(DISK_CHANGED.into());
        }
        if info.preview && existing.is_none() { return Err("the shared app already has an independent preview".into()); }
        let live = existing.filter(|_| !drift).map_or(&disk.working, |s| &s.working);
        if live.hash() != info.hash { return Err("the running app and the design source differ; locate again".into()); }
        let previous_doc = info.preview.then(|| live.clone());
        let prior = self.ghost.as_ref().filter(|_| req.is_ghost());
        let doc = prior.map_or(live, |g| &g.before);
        let source_open = if let Some(g) = prior {
            if matches!(req, DesignRequest::GhostMove { .. }) {
                if info.block != g.block { return Err("cannot move a widget across script blocks".into()); }
                g.pre_ghost_source_open.ok_or("missing ghost move source")?
            } else { map_live_to_pre_ghost(info.open, &g.pre_ghost_edits)? }
        } else { info.open };
        let syntax = doc.block_syntax(info.block)?;
        let object = |offset| -> Result<_, String> {
            let byte = doc.block_offset_from_file(info.block, offset)?;
            widget::find_object_by_open_brace(&syntax, byte).ok_or_else(|| "no widget at located brace".into())
        };
        let loc = object(source_open)?;
        let declared_name = || widget::parent_field_of(&syntax, &loc)
            .and_then(|(p,i)| syntax.node(&p)?.object()?.1.get(i)?.name.as_deref());
        if matches!(req, DesignRequest::State { .. } | DesignRequest::Delete { force_rust_refs: false, .. }
            | DesignRequest::Rename { force_rust_refs: false, .. }) {
            let refs = declared_name().map(|name| doc.find_rust_id_refs(name)).unwrap_or_default();
            let sites = Value::Arr(refs.iter().map(|r| json::obj(vec![("line", Value::Int(r.line as i64)),
                ("snippet", json::s(&r.snippet))])).collect());
            if let DesignRequest::State { .. } = req {
                let Value::Obj(mut fields) = info.raw else { return Err("invalid locate reply".into()) };
                fields.push(("rust_refs".into(), sites));
                return Ok(Preparation::State(Value::Obj(fields)));
            }
            if !refs.is_empty() { return Err(json::obj(vec![("warning", json::s("Rust references require force_rust_refs")),
                ("rust_refs", sites)]).to_json()); }
        }
        let colors = if doc.kind == DocKind::RustScriptMod { write::ColorStyle::MacroHashX } else { write::ColorStyle::Hash };
        let place = |p| match p {
            DesignPlace::Before => widget::Place::Before, DesignPlace::Inside => widget::Place::Inside,
            DesignPlace::After => widget::Place::After,
        };
        let edit = match req {
            DesignRequest::Insert { place: p, ty, name, props, .. } => {
                let props = props.iter().filter(|(k,_)| !k.contains('.'))
                    .map(|(k,v)| (k.clone(), write::Value::Verbatim(v.clone()))).collect::<Vec<_>>();
                widget::insert_widget(&syntax, &loc, place(*p), ty, name.as_deref(), &props, colors)
            }
            DesignRequest::GhostInsert { place: p, ty, .. } => {
                let container = if *p == DesignPlace::Inside { loc.clone() } else {
                    widget::parent_field_of(&syntax, &loc).ok_or("expected a widget sibling")?.0
                };
                let stem = ty.rsplit('.').next().unwrap().to_ascii_lowercase();
                let stem = if unreserved(&stem) { stem } else { format!("new_{stem}") };
                let name = widget::fresh_child_name(&syntax, &container, &stem);
                widget::insert_widget(&syntax, &loc, place(*p), ty, Some(&name), &[], colors)
            }
            DesignRequest::Move { place: p, .. } | DesignRequest::GhostMove { place: p, .. } => {
                let open = match prior {
                    Some(g) => map_live_to_pre_ghost(info.open, &g.pre_ghost_edits)?,
                    None => target.ok_or("missing move target")?.open,
                };
                let target = object(open)?;
                widget::move_widget(&syntax, &loc, &target, place(*p))
            }
            DesignRequest::Step { dir, .. } => match dir {
                StepDir::Up => widget::step_widget_up(&syntax, &loc),
                StepDir::Down => widget::step_widget_down(&syntax, &loc),
                StepDir::Out => widget::hoist_widget_out(&syntax, &loc),
            },
            DesignRequest::Delete { .. } => widget::delete_widget(&syntax, &loc),
            DesignRequest::Rename { name, .. } => widget::rename_widget(&syntax, &loc, name),
            DesignRequest::Wrap { ty, name, .. } => widget::wrap_widget(&syntax, &loc, ty, name.as_deref()),
            DesignRequest::Duplicate { name, .. } => widget::duplicate_widget(&syntax, &loc, name.as_deref()),
            _ => return Err("invalid structural operation".into()),
        }.map_err(|e| e.to_string())?;
        let result = doc.apply_block_edit(info.block, edit)?;
        let mut next = ScriptModDoc::parse(&file, result.next_text, doc.kind)?;
        let mut edits = result.file_edits;
        if let DesignRequest::Insert { props, .. } = req {
            for (k,v) in props.iter().filter(|(k,_)| k.contains('.')) {
                let syn = next.block_syntax(info.block)?;
                let open = next.block_offset_from_file(info.block, result.focus_open_byte.ok_or("missing inserted widget")?)?;
                let at = widget::find_object_by_open_brace(&syn, open).ok_or("missing inserted widget")?;
                let prop = widget::set_widget_prop(&syn, &at, k, v, colors).map_err(|e| e.to_string())?;
                next = ScriptModDoc::parse(&file, next.apply_block_edit(info.block, prop)?.next_text, doc.kind)?;
            }
            if props.iter().any(|(k,_)| k.contains('.')) { edits = replacement(&doc.text, &next.text); }
        }

        let focus_path = result.focus_name.as_deref().and_then(|name| focus_path(req, name));
        let next_hash = next.hash();
        let ghost = drag_key(req).map(|(target_key, op)| ActiveGhost {
            seat: who.seat, agent: who.agent, file: file.clone(), existed_before: prior.map_or(info.preview, |g| g.existed_before),
            before: doc.clone(), block: info.block, base_hash: doc.hash(), live_hash: next_hash,
            pre_ghost_source_open: (op == "move").then_some(source_open), label: target_key.2.clone(), target_key,
            op, focus_name: result.focus_name,
            focus_path: focus_path.clone(), rect: None, pre_ghost_edits: edits.clone(),
        });
        let baseline = (existing.is_none() || drift).then_some(disk);
        let mut staged = StagedMutation { file, baseline, previous_doc, base_hash: info.hash, next_hash, next_doc: next,
            compact_edits: compact_edits(&edits), history: HistoryOp::PushUndo, focus_path };
        if let Some(ghost) = ghost {
            staged.history = HistoryOp::GhostDwell;
            Ok(Preparation::Ghost(staged, ghost))
        } else { Ok(Preparation::Edits(vec![staged])) }
    }

    fn publish(&mut self, m: StagedMutation) {
        if m.history == HistoryOp::PushUndo { self.clear_redo(); }
        let files = Arc::make_mut(&mut self.files);
        if m.history == HistoryOp::Reset {
            files.remove(&m.file);
            self.timeline_undo.clear();
            self.timeline_redo.clear();
            return;
        }
        if let Some(slot) = m.baseline {
            self.timeline_undo.retain(|f| f != &m.file);
            self.timeline_redo.retain(|f| f != &m.file);
            files.insert(m.file.clone(), slot);
        }
        let slot = files.get_mut(&m.file).expect("verified design slot");
        let before = std::mem::replace(&mut slot.working, m.next_doc).text;
        match m.history {
            HistoryOp::PushUndo => { slot.undo.push(before); self.timeline_undo.push(m.file); }
            HistoryOp::PopUndo => {
                slot.undo.pop(); self.timeline_undo.pop();
                slot.redo.push(before); self.timeline_redo.push(m.file);
            }
            HistoryOp::PopRedo => {
                slot.redo.pop(); self.timeline_redo.pop();
                slot.undo.push(before); self.timeline_undo.push(m.file);
            }
            HistoryOp::GhostDwell => {},
            HistoryOp::Reset | HistoryOp::RevertGhost | HistoryOp::RestoreGhost => unreachable!(),
        }
    }
}

fn compact_edits(edits: &[Replace]) -> Option<String> {
    // A payload larger than this even before JSON quoting cannot fit in the compact command.
    if edits.iter().map(|e| e.text.len()).sum::<usize>() > 24 * 1024 { return None; }
    let json = Value::Arr(edits.iter().map(|e| Value::Arr(vec![Value::Int(e.range.start as i64),
        Value::Int(e.range.end as i64), json::s(&e.text)])).collect()).to_json();
    (json.len() <= 24 * 1024).then(|| makepad_network::digest::base64_encode(json.as_bytes()))
}

// Compose successive checked edits into one splice, retaining common UTF-8 prefix and suffix.
fn replacement(before: &str, after: &str) -> Vec<Replace> {
    let start = before.floor_char_boundary(before.bytes().zip(after.bytes()).take_while(|(a,b)| a == b).count());
    let tail = before[start..].bytes().rev().zip(after[start..].bytes().rev()).take_while(|(a,b)| a == b).count();
    let end = before.ceil_char_boundary(before.len()-tail);
    vec![Replace::new(start..end, &after[start..after.len()-(before.len()-end)])]
}

fn focus_path(req: &DesignRequest, name: &str) -> Option<String> {
    let (target, inside) = match req {
        DesignRequest::Insert { path, place, .. } | DesignRequest::GhostInsert { target: path, place, .. } => (path, *place == DesignPlace::Inside),
        DesignRequest::Move { target, place, .. } | DesignRequest::GhostMove { target, place, .. } => (target, *place == DesignPlace::Inside),
        DesignRequest::Rename { path, .. } | DesignRequest::Wrap { path, .. } | DesignRequest::Duplicate { path, .. } => (path, false),
        _ => return None,
    };
    let sep = if target.contains('/') { '/' } else { '.' };
    let parent = if inside { target.trim_end_matches(sep) } else { target.rsplit_once(sep).map_or("", |(p,_)| p) };
    let path = if parent.is_empty() && sep == '.' { name.into() } else { format!("{parent}{sep}{name}") };
    (path.len() <= crate::tweak::wire::MAX_DESIGN_PATH_LEN).then_some(path)
}

fn palette_page(v: Value, query: Option<&str>, offset: u32, limit: u16) -> Result<Value,String> {
    let rows = v.get("palette").and_then(Value::as_arr).ok_or("invalid design palette")?;
    let query = query.unwrap_or("").to_lowercase();
    let filtered: Vec<_> = rows.iter().filter(|row| ["name", "group"].iter().any(|k|
        row.get(k).and_then(Value::as_str).is_some_and(|s| s.to_lowercase().contains(&query)))).collect();
    let next = (offset as usize).saturating_add(limit as usize);
    let page = filtered.iter().skip(offset as usize).take(limit as usize).map(|r| (*r).clone()).collect();
    Ok(json::obj(vec![("palette", Value::Arr(page)), ("total", Value::Int(filtered.len() as i64)),
        ("offset", Value::Int(offset.into())), ("next_offset", if next < filtered.len() { Value::Int(next as i64) } else { Value::Null })]))
}

fn drag_key(req: &DesignRequest) -> Option<((String, DesignPlace, String), &'static str)> {
    match req {
        DesignRequest::GhostInsert { target, place, ty } => Some(((target.clone(), *place, ty.clone()), "insert")),
        DesignRequest::GhostMove { target, place, source } => Some(((target.clone(), *place, source.clone()), "move")),
        _ => None,
    }
}

fn tree_root(v: &Value) -> Result<&str, String> {
    v.get("parents").and_then(Value::as_arr).into_iter().flatten()
        .filter_map(|row| row.get("nodes")?.as_arr()).flatten()
        .find_map(|node| node.get("path")?.as_str().filter(|s| !s.is_empty()))
        .ok_or_else(|| "no root widget in the shared window".into())
}

impl DesignSession {
    fn clear_redo(&mut self) {
        let files = Arc::make_mut(&mut self.files);
        for file in self.timeline_redo.drain(..) {
            if let Some(slot) = files.get_mut(&file) { slot.redo.clear(); }
        }
    }

    pub(in crate::tweak) fn clear_ghost(&mut self) {
        self.restore_required = false;
        let Some(ghost) = self.ghost.take() else { return };
        let ghost = Arc::unwrap_or_clone(ghost);
        let files = Arc::make_mut(&mut self.files);
        if ghost.existed_before {
            files.get_mut(&ghost.file).expect("previewed ghost slot").working = ghost.before;
        } else { files.remove(&ghost.file); }
    }

    fn patch(&self, req: &DesignRequest, origin: Origin, claim: Option<&AtomicBool>) -> Result<Preparation, String> {
        use super::patch;
        if claim.is_some_and(|c| c.load(Ordering::Acquire)) { return Err("patch export cancelled".into()); }
        let root = self.workspace_root.as_deref().ok_or("no workspace root for this shared app")?;
        let file = match req {
            DesignRequest::Diff { file, .. } | DesignRequest::WritePatch { file } => file,
            _ => unreachable!(),
        };
        let selected = file.as_ref().map(|f| {
            let path = root.join(f);
            self.check_confined(path.to_str().ok_or("invalid patch source path")?)
        }).transpose()?;
        let mut diff = String::new();
        for slot in self.files.values().filter(|s| selected.as_ref().is_none_or(|p| p == Path::new(&s.path))) {
            let working = self.ghost.as_ref().filter(|g| g.file == slot.path)
                .map_or(&slot.working.text, |g| &g.before.text);
            if working != &slot.disk_text {
                diff.push_str(&patch::unified_patch(root, Path::new(&slot.path), &slot.disk_text, working)?);
            }
        }
        match req {
            DesignRequest::Diff { offset, limit, .. } => {
                let (page, next) = patch::page(&diff, *offset, *limit);
                Ok(Preparation::Reply(DesignReplyKind::Diff, json::obj(vec![
                    ("file", file.as_ref().map_or(Value::Null, json::s)), ("total_bytes", Value::Int(diff.len() as i64)),
                    ("offset", Value::Int((*offset).into())), ("patch", json::s(page)),
                    ("next_offset", next.map_or(Value::Null, |n| Value::Int(n.into())))])))
            }
            DesignRequest::WritePatch { .. } => {
                let stem = selected.as_ref().and_then(|p| p.file_stem()).and_then(|s| s.to_str()).unwrap_or("design");
                let claim = claim.expect("patch export claim");
                let path = if origin == Origin::Owner {
                    claim_export(claim)?;
                    patch::write_patch(root, stem, &diff)?
                } else { export_new_patch(root, stem, &diff, claim)? };
                Ok(Preparation::Reply(DesignReplyKind::WritePatch, json::obj(vec![("path", json::s(path.to_string_lossy()))])))
            }
            _ => unreachable!(),
        }
    }
}

fn claim_export(claim: &AtomicBool) -> Result<(), String> {
    if claim.swap(true, Ordering::AcqRel) { Err("patch export cancelled".into()) } else { Ok(()) }
}

/// Stage the participant's patch beside its destination and claim it just before publication.
pub(in crate::tweak) fn export_new_patch(root: &Path, stem: &str, diff: &str, claim: &AtomicBool) -> Result<PathBuf, String> {
    let root = root.canonicalize().map_err(|e| format!("open workspace root: {e}"))?;
    let mut dir = root.clone();
    for part in ["local", "design"] {
        dir.push(part);
        match fs::create_dir(&dir) {
            Ok(()) => {}
            Err(e) if e.kind() == ErrorKind::AlreadyExists => {}
            Err(e) => return Err(format!("create patch directory: {e}")),
        }
        dir = dir.canonicalize().map_err(|e| format!("resolve patch directory: {e}"))?;
        if !dir.starts_with(&root) || !dir.is_dir() { return Err("invalid workspace patch directory".into()); }
    }
    let stem = super::patch::sanitize_stem(stem);
    let (temp, mut file, nonce) = loop {
        let mut bytes = [0; 16];
        makepad_network::tls::os_random(&mut bytes).map_err(|e| format!("allocate patch name: {e}"))?;
        let nonce = makepad_town_core::hex(&bytes);
        let temp = dir.join(format!(".export-{nonce}.tmp"));
        match fs::File::create_new(&temp) {
            Ok(file) => break (temp, file, nonce),
            Err(e) if e.kind() == ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(format!("create temporary patch: {e}")),
        }
    };
    let written = file.write_all(diff.as_bytes());
    drop(file);
    if let Err(e) = written {
        let _ = fs::remove_file(&temp);
        return Err(format!("write temporary patch: {e}"));
    }
    if let Err(err) = claim_export(claim) {
        let _ = fs::remove_file(&temp);
        return Err(err);
    }

    let primary = dir.join(format!("{stem}.patch"));
    let published = match fs_ops::move_no_replace(&temp, &primary) {
        Ok(()) => Ok(primary),
        Err(e) if e.kind() == ErrorKind::AlreadyExists => {
            let fallback = dir.join(format!("{}-{nonce}.patch", &stem[..stem.len().min(31)]));
            fs_ops::move_no_replace(&temp, &fallback).map(|()| fallback)
        }
        Err(e) => Err(e),
    };
    match published {
        Ok(path) => Ok(path),
        Err(e) => {
            let _ = fs::remove_file(&temp);
            if e.kind() == ErrorKind::Unsupported {
                write_new_patch_direct(&dir, &stem, &nonce, diff)
            } else {
                Err(format!("publish new patch: {e}"))
            }
        }
    }
}

pub(in crate::tweak) fn write_new_patch_direct(dir: &Path, stem: &str, nonce: &str, diff: &str) -> Result<PathBuf, String> {
    let stem = super::patch::sanitize_stem(stem);
    let primary = dir.join(format!("{stem}.patch"));
    let (output, mut file) = match fs::File::create_new(&primary) {
        Ok(file) => (primary, file),
        Err(e) if e.kind() == ErrorKind::AlreadyExists => {
            let fallback = dir.join(format!("{}-{nonce}.patch", &stem[..stem.len().min(31)]));
            let file = fs::File::create_new(&fallback).map_err(|e| format!("publish new patch: {e}"))?;
            (fallback, file)
        }
        Err(e) => return Err(format!("publish new patch: {e}")),
    };
    if let Err(e) = file.write_all(diff.as_bytes()) {
        drop(file);
        let _ = fs::remove_file(&output);
        return Err(format!("publish new patch: {e}"));
    }
    Ok(output)
}

impl PendingDesignOp {
    fn retract(&mut self, s: &DesignSession) -> Progress {
        self.retracting = true;
        self.worker(s, |snapshot| {
            let g = snapshot.ghost.as_ref().ok_or("no active ghost")?;
            let live = &snapshot.files[&g.file].working;
            Ok(Preparation::Edits(vec![StagedMutation {
                file: g.file.clone(), baseline: None, previous_doc: Some(live.clone()),
                base_hash: g.live_hash, next_hash: g.base_hash, next_doc: g.before.clone(),
                compact_edits: None, focus_path: None,
                history: if g.existed_before { HistoryOp::RestoreGhost } else { HistoryOp::RevertGhost },
            }]))
        })
    }

    fn publish_ghost(&mut self, s: &mut DesignSession) -> Progress {
        s.publish(self.staged.pop().expect("verified provisional edit"));
        s.ghost = self.ghost.take().map(Arc::new);
        Ok(Some(Completed::GhostStatus { resume: false, reply: true }))
    }

    fn locate_theme_root(&mut self) -> Progress {
        if let Some(path) = self.bake.root_path.clone() {
            self.send(PipelineStage::BakeRoot, self.window, DesignVerb::Locate, vec![("path", path)])
        } else {
            self.send(PipelineStage::BakeTree, self.window, DesignVerb::Tree,
                vec![("depth", "1".into()), ("offset", "0".into()), ("limit", "1".into())])
        }
    }

    fn bake_report(&self, baked: bool) -> Value {
        json::obj(vec![("baked", Value::Bool(baked)), ("skipped", Value::Arr(self.bake.skipped.iter().map(|(path, prop)|
            Value::Arr(vec![json::s(path), json::s(prop)])).collect()))])
    }

    fn collect_bake(&mut self, s: &DesignSession, raw: Option<Value>) -> Progress {
        let req = self.request.clone();
        let mut inputs = std::mem::take(&mut self.bake);
        self.worker(s, move |_| {
            inputs.collect(&req, raw)?;
            Ok(Preparation::BakeInputs(inputs))
        })
    }

    fn next_bake(&mut self, s: &DesignSession) -> Progress {
        if let Some(path) = self.bake.paths.pop_front() {
            return self.send(PipelineStage::BakeProp(path.clone()), self.window, DesignVerb::Locate, vec![("path", path)]);
        }
        let inputs = std::mem::take(&mut self.bake);
        self.baked = inputs.props.iter().flat_map(|(path, kv)| kv.iter().map(|(k, _)| (path.clone(), k.clone()))).collect();
        self.worker(s, move |snapshot| snapshot.prepare_bake(inputs))
    }
}

impl BakeInputs {
    pub(in crate::tweak) fn snapshot(props: &Arc<BTreeMap<(String, String), String>>,
        install: Option<&Arc<ThemeInstall>>, root_path: Option<&String>) -> Self {
        Self { live_props: Some(props.clone()), install: install.cloned(), root_path: root_path.cloned(), ..Default::default() }
    }

    fn collect(&mut self, req: &DesignRequest, raw: Option<Value>) -> Result<(), String> {
        let DesignRequest::Bake { scope, path, prop } = req else { unreachable!() };
        if let Some(raw) = raw {
            self.theme = Some(BakeTheme::read(&raw, self.install.as_deref())?);
        }
        if let Some(path) = path { self.root_path = Some(path.clone()); }
        if *scope != BakeScope::Theme {
            let live = self.live_props.take().expect("owner property snapshot");
            for ((p, k), v) in live.iter() {
                if path.as_ref().is_some_and(|want| want != p) || prop.as_ref().is_some_and(|want| want != k) { continue; }
                if !k.split('.').all(unreserved) || v.contains("#(") || v.contains("@(") || parse_literal_class(v).is_err() {
                    self.skipped.push((p.clone(), k.clone()));
                } else { self.props.entry(p.clone()).or_default().push((k.clone(), v.clone())); }
            }
        }
        self.paths.extend(self.props.keys().cloned());
        Ok(())
    }
}

impl BakeTheme {
    fn read(raw: &Value, install: Option<&ThemeInstall>) -> Result<Self, String> {
        use crate::tweak::theme;
        let snap = theme::ThemeSnapshot::from_value(raw).ok_or("invalid live theme snapshot")?;
        let mut tokens = BTreeMap::new();
        if let Some(i) = install.filter(|i| i.base == snap.base && i.sheet == snap.sheet) {
            for (name, val) in i.globals.iter().chain(&i.overrides) {
                tokens.insert(name.clone(), theme::literal(*val));
            }
        }
        for (name, value) in snap.edits {
            if value.contains("#(") || value.contains("@(") { return Err(format!("invalid theme literal for {name}")); }
            parse_literal_class(&value).map_err(|e| format!("theme token {name}: {e}"))?;
            let value = theme::scalar(&value).map(theme::literal).unwrap_or(value);
            tokens.insert(name, value);
        }
        for (name, value) in &tokens {
            if !unreserved(name) { return Err(format!("invalid theme token name {name}")); }
            parse_literal_class(value).map_err(|e| format!("theme token {name}: {e}"))?;
        }
        Ok(Self { base: snap.base, sheet: snap.sheet, tokens: tokens.into_iter().collect(),
            file: raw.get("define_file").and_then(Value::as_str).map(str::to_owned) })
    }
}

struct BakeFile {
    props: Vec<(LocatedInfo, Vec<(String, String)>)>,
    theme: Option<(BakeTheme, usize)>,
    root: Option<LocatedInfo>,
}

impl DesignSession {
    fn prepare_bake(&self, mut bake: BakeInputs) -> Result<Preparation, String> {
        let mut by_file: BTreeMap<PathBuf, BakeFile> = BTreeMap::new();
        let empty = || BakeFile { props: Vec::new(), theme: None, root: None };
        if let Some(theme) = bake.theme.take() {
            let (file, block) = if let Some(loc) = &bake.theme_root {
                let file = self.check_confined(&loc.file)?;
                if bake.definition.as_ref().is_some_and(|d| Path::new(&d.path) != file) {
                    return Err("the located root does not identify the theme definition block".into());
                }
                (file, loc.block)
            } else if let Some(file) = theme.file.as_ref().and_then(|f| self.check_confined(f).ok()) {
                let disk = self.read_slot(file.to_str().ok_or("invalid theme definition path")?)?;
                let doc = self.files.get(&disk.path).filter(|s| s.disk_hash == disk.disk_hash)
                    .map_or(&disk.working, |s| &s.working);
                let block = theme_block(doc, &theme.base)?;
                bake.definition = Some(disk);
                let Some(block) = block else {
                    // theme get may itself return the app root. Locate the widget to disambiguate
                    // when that Rust file has several script_mod! blocks and no theme declaration.
                    bake.theme = Some(theme);
                    return Ok(Preparation::LocateThemeRoot(bake));
                };
                (file, block)
            } else {
                bake.theme = Some(theme);
                return Ok(Preparation::LocateThemeRoot(bake));
            };
            let target = by_file.entry(file).or_insert_with(empty);
            target.theme = Some((theme, block));
            target.root = bake.theme_root.take();
        }
        for (path, props) in bake.props {
            let loc = bake.locations.remove(&path).ok_or("missing bake location")?;
            by_file.entry(self.check_confined(&loc.file)?).or_insert_with(empty).props.push((loc, props));
        }
        let mut edits = Vec::new();
        for (file, mut group) in by_file {
            let disk = match bake.definition.take_if(|s| Path::new(&s.path) == file) {
                Some(disk) => disk,
                None => self.read_slot(file.to_str().ok_or("invalid design source path")?)?,
            };
            let existing = self.files.get(&disk.path);
            let drift = existing.is_some_and(|s| s.disk_text != disk.disk_text);
            let existing_preview = existing.is_some() && !drift;
            if drift && existing.is_some_and(|s| s.disk_text != s.working.text) { return Err(DISK_CHANGED.into()); }
            let doc = existing.filter(|_| !drift).map_or(&disk.working, |s| &s.working);
            let base_hash = doc.hash();
            for loc in group.props.iter().map(|(loc,_)| loc).chain(group.root.iter()) {
                if loc.hash != base_hash || (loc.preview && !existing_preview) { return Err("the live bake source differs from the working file".into()); }
            }
            let mut next = doc.clone();
            let colors = if doc.kind == DocKind::RustScriptMod { write::ColorStyle::MacroHashX } else { write::ColorStyle::Hash };
            // Later braces are edited first so offsets from all earlier locates stay valid.
            group.props.sort_by_key(|(loc,_)| std::cmp::Reverse(loc.open));
            for (loc, props) in group.props {
                for (key, val) in props {
                    let syntax = next.block_syntax(loc.block)?;
                    let byte = next.block_offset_from_file(loc.block, loc.open)?;
                    let widget = widget::find_object_by_open_brace(&syntax, byte).ok_or("missing bake widget")?;
                    let prop = widget::set_widget_prop(&syntax, &widget, &key, &val, colors).map_err(|e| e.to_string())?;
                    next = ScriptModDoc::parse(&disk.path, next.apply_block_edit(loc.block, prop)?.next_text, doc.kind)?;
                }
            }
            if let Some((theme, block)) = group.theme {
                let edit = next.apply_theme_tokens(block, Some(&theme.base), theme.sheet.as_deref(), &theme.tokens)?;
                next = ScriptModDoc::parse(&disk.path, edit.next_text, doc.kind)?;
            }
            let previous_doc = existing_preview.then(|| doc.clone());
            let compact = compact_edits(&replacement(&doc.text, &next.text));
            edits.push(StagedMutation { file: disk.path.clone(), base_hash, previous_doc,
                baseline: (!existing_preview).then_some(disk), next_hash: next.hash(), next_doc: next,
                history: HistoryOp::PushUndo, focus_path: None, compact_edits: compact });
        }
        Ok(Preparation::Baked(edits, bake.skipped))
    }
}

fn theme_block(doc: &ScriptModDoc, base: &str) -> Result<Option<usize>, String> {
    use makepad_widgets::makepad_script::syntax::Kind;
    let name = format!("mod.themes.{base}");
    if doc.blocks.len() <= 1 { return Ok(Some(0)); }
    for block in 0..doc.blocks.len() {
        let syntax = doc.block_syntax(block)?;
        if syntax.statements().iter().any(|stmt| match &stmt.kind {
            Kind::Entry { name, op, .. } => name == "mod.theme" && op == "+:",
            Kind::Assign { target, .. } => target.ident() == Some(&name),
            _ => false,
        }) { return Ok(Some(block)); }
    }
    Ok(None)
}
