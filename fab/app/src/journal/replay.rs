//! Verified replay of a single opening of a plate journal.
use super::*;
use std::{borrow::Cow, path::Component};
use reader::Kind;

fn field<'a>(row: &'a Value, key: &str) -> io::Result<&'a Value> { row.get(key).ok_or_else(invalid) }
fn string<'a>(row: &'a Value, key: &str) -> io::Result<&'a str> { field(row, key)?.as_str().ok_or_else(invalid) }
fn array<'a>(row: &'a Value, key: &str) -> io::Result<&'a [Value]> { field(row, key)?.as_arr().ok_or_else(invalid) }
fn digest<'a>(row: &'a Value, key: &str) -> io::Result<Option<&'a str>> {
    let v = field(row, key)?;
    if v.is_null() { return Ok(None) }
    let hex = v.as_str().ok_or_else(invalid)?;
    if hex.len() != 64 || !hex.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) { return Err(invalid()) }
    Ok(Some(hex))
}
fn file_name(row: &Value) -> io::Result<&str> {
    let name = string(row, "name")?;
    if name.contains(['\\', ':']) || name.split('/').any(|s| matches!(s, "" | "." | ".."))
        || !Path::new(name).components().all(|c| matches!(c, Component::Normal(_))) { return Err(invalid()) }
    Ok(name)
}
fn blob(dir: &Path, hash: &str) -> io::Result<Vec<u8>> {
    let _read = reader::ReadPermit::take(dir)?;
    let blobs = dir.join("blobs");
    if !fs::symlink_metadata(&blobs)?.is_dir() { return Err(invalid()) }
    let mut file = open_regular(&blobs.join(hash))?.ok_or(io::ErrorKind::NotFound)?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    if sha256_hex(&bytes) != hash { return Err(invalid()) }
    Ok(bytes)
}
/// One recorded change, verified against the bytes before it and the hash after it.
/// Copy as script applies the same change over owned bytes.
pub(super) fn apply<B: AsRef<[u8]> + From<Vec<u8>>>(dir: &Path, files: &mut BTreeMap<String, B>, change: &Value) -> io::Result<()> {
    let name = file_name(change)?;
    let mut old = files.remove(name);
    if digest(change, "before")? != old.as_ref().map(|b| sha256_hex(b.as_ref())).as_deref() { return Err(invalid()) }
    let Some(hash) = digest(change, "after")? else { return Ok(()) };
    let contents = if field(change, "blob")?.as_bool() == Some(true) {
        blob(dir, hash)?
    } else {
        let mut bytes = old.take().ok_or_else(invalid)?.as_ref().to_vec();
        let mut limit = bytes.len();
        for edit in array(change, "edits")?.iter().rev() {
            let index = |key| field(edit, key)?.as_u64().and_then(|x| usize::try_from(x).ok()).ok_or_else(invalid);
            let start = index("start")?;
            let end = index("end")?;
            if end > limit || start > end { return Err(invalid()); }
            bytes.splice(start..end, string(edit, "text")?.bytes());
            limit = start;
        }
        if sha256_hex(&bytes) != hash { return Err(invalid()) }
        bytes
    };
    files.insert(name.into(), contents.into());
    Ok(())
}

fn apply_record(dir: &Path, files: &mut Files, row: &Value, only: Option<&str>) -> io::Result<()> {
    match string(row, "kind")? {
        "checkpoint" => for file in array(row, "files")? {
            let name = file_name(file)?;
            if only.is_some_and(|wanted| wanted != name) { continue }
            let bytes = blob(dir, digest(file, "hash")?.ok_or_else(invalid)?)?;
            if files.insert(name.to_owned(), bytes.into()).is_some() { return Err(invalid()) }
        },
        "effect" | "history" | "reload" => {
            for change in array(row, "changes")? {
                let name = file_name(change)?;
                if only.is_none_or(|wanted| wanted == name) { apply(dir, files, change)?; }
            }
        }
        "call" | "done" | "lost" => {}
        _ => return Err(invalid()),
    }
    Ok(())
}


#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode { State, Calls }

pub struct Request<'a> {
    pub journal: &'a Path,
    pub upto: Option<u64>,
    pub into: &'a Path,
    pub mode: Mode,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StepKind { Checkpoint, Call, Effect, History, Reload, Lost }

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    Applied,
    Matched,
    Mismatch { reason: String },
    Gap { reason: String },
    NotReplayable { reason: String },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Step {
    pub order: u64,
    pub kind: StepKind,
    pub action: Option<String>,
    pub outcome: Outcome,
}

pub struct Report {
    pub steps: usize,
    pub stopped: Option<Step>,
    pub manifest: Option<String>,
    pub into: PathBuf,
}

fn gap(order: u64, at: u64) -> Step {
    Step { order, kind: StepKind::Lost, action: None, outcome: Outcome::Gap {
        reason: format!("the journal has damage at byte {at} between the checkpoint and the requested record"),
    } }
}

/// File positions are enough to decide whether a revision is reachable. A panel
/// selection supplies these from its index, and supplies document rows on demand.
#[derive(Default)]
pub(super) struct Positions {
    records: BTreeMap<u64, (u64, Kind)>,
    checkpoints: BTreeMap<u64, u64>,
    losses: Vec<Loss>,
    damage: Vec<u64>,
}

struct Loss {
    order: u64,
    offset: u64,
    diagnostic: u64,
    range: Option<(u64, u64)>,
    empty: bool,
    repaired: bool,
    foreign: bool,
}

impl Positions {
    pub(super) fn record(&mut self, order: u64, offset: u64, kind: Kind, loss: Option<&Value>) {
        self.records.insert(order, (offset, kind));
        if kind == Kind::Checkpoint { self.checkpoints.insert(order, offset); }
        if let Some(row) = loss {
            // These rows belonged to another journal. Show the marker, but never open a gap here.
            if row.get("cause").and_then(Value::as_str) == Some("unwritten") && row.get("journal").is_some() { return }
            self.losses.push(Loss {
                order, offset, diagnostic: row.get("at").and_then(Value::as_u64).unwrap_or(offset),
                range: loss::file_range(row), empty: row.get("count").and_then(Value::as_u64) == Some(0),
                repaired: torn_at(row, offset).is_some(),
                foreign: row.get("foreign").and_then(Value::as_bool) == Some(true),
            });
        }
    }

    pub(super) fn damaged(&mut self, offset: u64) { self.damage.push(offset); }

    pub(super) fn through(&self, upto: Option<u64>) -> Walk<'_> {
        let highest = self.records.last_key_value().map_or(0, |(&order, _)| order);
        // A recovery-only life contributes evidence about older rows, but no state.
        let end = upto.unwrap_or_else(|| self.records.iter().rev().find(|(_, (_, k))| *k != Kind::Lost)
            .and_then(|(&n, _)| self.records.range(..=(n | LIFE_MASK)).next_back().map(|(&last, _)| last)).unwrap_or(highest));
        let base = end & !LIFE_MASK;
        let Some((&start, &checkpoint_at)) = self.checkpoints.range(base..=end).next_back() else {
            let order = self.records.range(base..=end).next().map_or(base, |(&order, _)| order);
            return Walk { positions: self, start: order, end, gap: Some(Step {
                order, kind: StepKind::Lost, action: None,
                outcome: Outcome::Gap { reason: "no checkpoint in the requested range".into() },
            }) }
        };
        let mut first_gap = None;
        let mut mark = |order, at| {
            if first_gap.as_ref().is_none_or(|old: &Step| order < old.order) { first_gap = Some(gap(order, at)); }
        };
        // A late marker may describe an unwritten low order inside an earlier selection.
        for lost in &self.losses {
            if let Some((from, to)) = lost.range {
                let first = from.max(start);
                // Latest state includes a known unwritten tail after its last durable row.
                let last = to.min(if upto.is_none() { end | LIFE_MASK } else { end });
                if lost.empty || first > last { continue }
                let mut want = first;
                for (&order, _) in self.records.range(first..=last) {
                    if order != want { break }
                    want += 1;
                }
                if want <= last { mark(want, lost.offset); }
            } else if !(lost.foreign && lost.repaired) && (start..=end).contains(&lost.order) {
                mark(lost.order, lost.diagnostic);
            }
        }
        if !self.damage.is_empty() {
            // An undecodable line has no order. Prefer the first missing order,
            // otherwise the first selected row after it. A recovered tear before
            // the checkpoint supersedes damage in its disk prefix.
            let recovered = self.losses.iter().filter(|m| m.offset < checkpoint_at && m.order < start && m.repaired).map(|m| m.offset).max();
            let mut through = checkpoint_at;
            let mut expected = start;
            let mut missing = None;
            for (&order, &(offset, _)) in self.records.range(start..=end) {
                through = through.max(offset);
                if order != expected { missing.get_or_insert(expected); }
                expected = order + 1;
            }
            if expected <= end.min(highest) { missing.get_or_insert(expected); }
            // A peer doing startup repair cannot tag the tear as foreign. If this
            // packed life subsequently wrote again and every selected slot is present,
            // that repaired fragment cannot be a missing row of this walk.
            let dense = base != 0 && missing.is_none() && self.records.contains_key(&end);
            if let Some(&at) = self.damage.iter().find(|&&at| !self.losses.iter().any(|m| m.repaired && m.diagnostic == at
                && (m.foreign || (dense && m.order & !LIFE_MASK != base && m.offset < through)))
                && recovered.is_none_or(|last| at > last)
                && (missing.is_some() || (at > checkpoint_at && (upto.is_none_or(|u| u > highest) || at <= through)))) {
                let first = missing.or_else(|| self.records.range(start..=end)
                    .find(|(_, (offset, _))| *offset >= at).map(|(&order, _)| order));
                mark(first.unwrap_or(expected), at);
            }
        }
        Walk { positions: self, start, end, gap: first_gap }
    }
}

pub(super) struct Walk<'a> {
    positions: &'a Positions,
    pub(super) start: u64,
    end: u64,
    gap: Option<Step>,
}

struct Archive {
    positions: Positions,
    rows: BTreeMap<u64, Value>,
}

impl Archive {
    fn read(dir: &Path, upto: Option<u64>, source: Option<&reader::VerifiedLog>) -> io::Result<Self> {
        let _read = reader::ReadPermit::take(dir)?;
        let opened = match source {
            Some(log) => log.file()?,
            None => open_regular(&dir.join(LOG))?.ok_or(io::ErrorKind::NotFound)?,
        };
        let mut file = &opened;
        loop {
            // A pool job may start after the selected journal was moved. Keep
            // using the verified handle even when a contended sample needs a retry.
            if let Some(log) = source { log.require_current(dir)?; }
            file.rewind()?;
            let boundary = live::read_boundary(dir, file, None)?;
            // Under the shared sample no cooperating writer is mid-row, so an unterminated
            // tail is a dead writer's tear. Only a contended sample may hold a row in flight.
            let archive = Self::read_prefix(file, boundary.end, upto, boundary.settled || boundary.repairing)?;
            if let Some(log) = source { log.require_current(dir)?; }
            if boundary.still_current(dir, file)? { return Ok(archive) }
        }
    }

    fn read_prefix(file: impl Read, end: u64, upto: Option<u64>, torn_is_damage: bool) -> io::Result<Self> {
        let mut positions = Positions::default();
        let mut rows = BTreeMap::new();
        let tail = scan(file.take(end), reader::parse, |offset, row| {
            let Some((order, kind, row)) = row.and_then(|r| Some((r.get("order")?.as_u64()?,
                Kind::from_name(r.get("kind")?.as_str()?), r))) else {
                positions.damaged(offset);
                return
            };
            positions.record(order, offset, kind, (kind == Kind::Lost).then_some(&row));
            if upto.is_none_or(|end| order <= end) { rows.insert(order, row); }
        })?;
        if let Some((at, _)) = tail.filter(|_| torn_is_damage) { positions.damaged(at); }
        Ok(Self { positions, rows })
    }

    fn row(&self, order: u64) -> io::Result<Cow<'_, Value>> {
        self.rows.get(&order).map(Cow::Borrowed).ok_or_else(invalid)
    }
}

// Only the paths claimed by this replay are cleaned up. In particular a conflicting
// file in an initially empty destination belongs to whoever created it first.
struct Copy<'a> {
    root: &'a Path,
    dirs: BTreeSet<PathBuf>,
    files: BTreeSet<PathBuf>,
    keep: bool,
    written: Files,
}

impl<'a> Copy<'a> {
    fn claim(root: &'a Path) -> io::Result<Self> {
        let mut dirs = BTreeSet::new();
        match fs::symlink_metadata(root) {
            Ok(m) if m.is_dir() && fs::read_dir(root)?.next().is_none() => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => { fs::create_dir(root)?; dirs.insert(root.to_owned()); }
            _ => return Err(io::ErrorKind::AlreadyExists.into()),
        }
        Ok(Self { root, dirs, files: BTreeSet::new(), keep: false, written: Files::new() })
    }

    fn write(&mut self, name: &str, bytes: &[u8]) -> io::Result<()> {
        let path = self.root.join(name);
        let mut parent = self.root.to_owned();
        for part in Path::new(name).parent().ok_or_else(invalid)?.components() {
            parent.push(part);
            if !self.dirs.contains(&parent) {
                fs::create_dir(&parent)?;
                self.dirs.insert(parent.clone());
            }
        }
        let mut file = OpenOptions::new().write(true).create_new(true).open(&path)?;
        self.files.insert(path);
        file.write_all(bytes)?;
        file.sync_data()
    }
}

impl Drop for Copy<'_> {
    fn drop(&mut self) {
        if !self.keep {
            for file in &self.files { let _ = fs::remove_file(file); }
            for dir in self.dirs.iter().rev() { let _ = fs::remove_dir(dir); }
        }
    }
}

fn manifest(row: &Value, files: &Files) -> Option<String> {
    if let Some(v) = row.get("manifest") {
        let name = v.as_str()?;
        if !fab_model::project::is_project(Path::new(name)) || !files.contains_key(name) { return None }
        return Some(name.into())
    }
    let mut names = files.keys().filter(|name| fab_model::project::is_project(Path::new(name)));
    let first = names.next()?;
    names.next().is_none().then(|| first.clone())
}

impl Walk<'_> {
    /// Reconstruct all files, or a single file for a diff, using exactly the same
    /// record ordering, damage boundaries and byte-range checks.
    pub(super) fn visit<'r>(&self, dir: &Path, only: Option<&str>, after: Option<u64>, files: &mut Files,
        read: impl FnMut(u64) -> io::Result<Cow<'r, Value>>,
        visit: impl FnMut(Step, &Value, &Files) -> io::Result<Option<Step>>) -> io::Result<Option<Step>>
    {
        self.visit_controlled(dir, only, after, files, read, visit, None)
    }

    fn visit_controlled<'r>(&self, dir: &Path, only: Option<&str>, after: Option<u64>, files: &mut Files,
        mut read: impl FnMut(u64) -> io::Result<Cow<'r, Value>>,
        mut visit: impl FnMut(Step, &Value, &Files) -> io::Result<Option<Step>>,
        mut control: Option<&mut WalkProgress<'_>>) -> io::Result<Option<Step>>
    {
        let opening = if self.positions.checkpoints.contains_key(&self.start) {
            read(self.start)?.get("checkpoint").and_then(Value::as_u64).unwrap_or(self.start)
        } else { self.start };
        let first = after.map_or(self.start, |n| n.saturating_add(1).max(self.start));
        if first > self.end { return Ok(self.gap.clone()) }
        let records = self.positions.records.range(first..=self.end)
            .take_while(|(&order, _)| self.gap.as_ref().is_none_or(|gap| order < gap.order));
        // Count the same iterator we walk, including linked results skipped below.
        let total = if control.is_some() { records.clone().count() } else { 0 };
        for (done, (&order, &(at, kind))) in records.enumerate() {
            if let Some(check) = &mut control { check(done, total)?; }
            if kind == Kind::Done || (only.is_some() && kind == Kind::Call) { continue }
            let row = read(order)?;
            let row = row.as_ref();
            if row.get("checkpoint").and_then(Value::as_u64).is_some_and(|cp| cp != opening && cp != self.start) { continue }
            let kind = match row.get("kind").and_then(Value::as_str) {
                Some("checkpoint") => StepKind::Checkpoint,
                Some("call") => StepKind::Call,
                Some("done") => continue, // the answer belongs to its Call step
                Some("effect") => StepKind::Effect,
                Some("history") => StepKind::History,
                Some("reload") => StepKind::Reload,
                Some("lost") => StepKind::Lost,
                _ => return Ok(Some(gap(order, at))),
            };
            if let Err(error) = apply_record(dir, files, row, only) {
                return match error.kind() {
                    // The diff presents the missing blob separately from damaged bytes.
                    io::ErrorKind::NotFound if only.is_some() => Err(error),
                    io::ErrorKind::InvalidData | io::ErrorKind::NotFound => Ok(Some(gap(order, at))),
                    _ => Err(error),
                }
            }
            let action = row.get("action").and_then(|action| action.as_str().or_else(|| action.get("name")?.as_str())).map(str::to_owned);
            match visit(Step { order, kind, action, outcome: Outcome::Applied }, row, files) {
                Ok(Some(stop)) => return Ok(Some(stop)),
                Ok(None) => {}
                Err(e) if e.kind() == io::ErrorKind::InvalidData => return Ok(Some(gap(order, at))),
                Err(e) => return Err(e),
            }
        }
        if let Some(check) = control { check(total, total)?; }
        Ok(self.gap.clone())
    }
}

pub(super) struct Snapshot {
    pub(super) rows: usize,
    pub(super) checkpoint: Option<Ordered>,
}

/// A fixed byte prefix sampled under the append lock. Only this writer owns
/// the selected life. The scan and verified walk run after releasing the lock.
pub(super) fn snapshot(dir: &Path, file: impl Read, through: u64, end: u64, base: u64) -> io::Result<Snapshot> {
    let archive = Archive::read_prefix(file, end, Some(through), true)?;
    let walk = archive.positions.through(Some(through));
    let rows = archive.positions.records.range(walk.start..=through).count();
    // Rollover seals the previous segment. Even an outstanding final local
    // with no loss marker or damaged line must prevent a partial snapshot.
    if rows as u64 != through - walk.start + 1 { return Ok(Snapshot { rows, checkpoint: None }) }
    let mut files = Files::new();
    let mut name = None;
    let mut opening = None;
    let stop = walk.visit(dir, None, None, &mut files, |n| archive.row(n), |step, row, _| {
        if step.kind == StepKind::Checkpoint {
            name = row.get("manifest").and_then(Value::as_str).map(str::to_owned);
            opening = Some(row.get("checkpoint").and_then(Value::as_u64).unwrap_or(step.order));
        }
        Ok(None)
    })?;
    let checkpoint = stop.is_none().then(|| Ordered {
        reservation: Reservation { order: base, time_ms: time_ms() }, checkpoint: opening,
        record: Record::Checkpoint { manifest: name, files: Arc::new(files) },
    });
    Ok(Snapshot { rows, checkpoint })
}

pub(super) fn state(journal: &Path, upto: Option<u64>, into: &Path, progress: &mut dyn FnMut(&Step)) -> io::Result<Report> {
    let archive = Archive::read(journal, upto, None)?;
    let walk = archive.positions.through(upto);
    let mut files = Files::new();
    let mut steps = 0;
    let mut plate = None;
    let stopped = walk.visit(journal, None, None, &mut files, |n| archive.row(n), |step, row, files| {
        if step.kind == StepKind::Checkpoint { plate = manifest(row, files); }
        steps += 1;
        progress(&step);
        Ok(None)
    })?;
    if let Some(stop) = &stopped {
        steps += 1;
        progress(stop);
    } else {
        let mut copy = Copy::claim(into)?;
        for (name, bytes) in files { copy.write(&name, &bytes)?; }
        copy.keep = true;
    }
    Ok(Report { steps, stopped, manifest: plate, into: into.to_owned() })
}

use crate::elec_host::{CaseBridge, CaseUpdate, plate_project};
use fab_elec_design::By;
use fab_elec_host::{Cmd, Docs, Engine, Mailbox, Ticket, req_actions::AppFacts};
use fab_model::project;

fn document_error(docs: &Docs) -> Option<String> {
    use fab_elec_core::{Diag, Sel, Severity};
    use fab_elec_lang::UnitState;
    let view = docs.view();
    let describe = |diag: &Diag| {
        let site = diag.sel.iter().find_map(|sel| match sel {
            Sel::Source { doc, start, .. } => {
                let source = docs.sources().doc(*doc)?;
                let (line, col) = docs.sources().line_col(*doc, *start)?;
                Some(format!("{}:{line}:{col}: ", source.name))
            }
            _ => None,
        }).unwrap_or_default();
        format!("{site}{}", diag.message)
    };
    let mut failures = Vec::new();
    let units = view.units.iter().map(|(_, name, state)| ("unit", name, state));
    let benches = docs.bench_views().iter().map(|bench| ("bench", &bench.out.name, &bench.out.state));
    for (kind, name, state) in units.chain(benches) {
        if *state != UnitState::Built {
            let why = state.diags().iter().map(&describe).collect::<Vec<_>>().join(", ");
            failures.push(format!("{kind} {name} did not evaluate: {why}"));
        }
    }
    failures.extend(view.notes.iter().filter(|d| d.severity == Severity::Error).map(describe));
    (!failures.is_empty()).then(|| failures.join("; "))
}


/// One headless plate, including the case bridge the window drives between edits.
pub(crate) struct Runner {
    pub(crate) engine: Engine,
    session: fab_ai::Session,
    bridge: CaseBridge,
    seq: u64,
    pub(crate) facts: AppFacts,
}

impl Runner {
    pub(crate) fn open(path: &Path, part_library: Option<&Path>) -> Result<Self, String> {
        Self::load(path, part_library, None)
    }

    fn load(path: &Path, part_library: Option<&Path>, previous: Option<fab_ai::Session>) -> Result<Self, String> {
        if !project::is_project(path) {
            return Err("expected a .fab.splash project".into());
        }
        let (model, (engine, bridge, update)) = project::load_with_env(path, |texts, assets| {
            let (design, docs, keys, library) = plate_project(texts, assets, part_library);
            if let Some(why) = docs.as_ref().and_then(document_error) {
                return Err(why);
            }
            let mut engine = Engine::new(design, Arc::default(),
                Arc::default(), Arc::default(), "headless".into()).with_app_tools().with_mesh_keys(keys).with_library_keys(library);
            if let Some(docs) = docs {
                engine = engine.with_docs(docs);
            }
            let mut bridge = CaseBridge::default();
            let update = bridge.cover(engine.design().clone()).ok_or("the case did not supply initial inputs")?;
            let CaseUpdate::Set { env, .. } = &update else {
                return Err("the case supplied no environment for the new plate".into());
            };
            Ok((env.clone(), (engine, bridge, update)))
        })?;
        let session = if let Some(mut previous) = previous {
            // Electronics revisions live outside model history. Preserve its generation
            // and request receipts across a board gesture, reload or skipped board call.
            previous.model.set_docs(model.docs().clone());
            previous.model.set_elec_assets(model.elec_assets().clone());
            previous.model.set_elec_memory(model.elec_memory().clone());
            previous
        } else {
            let mut session = fab_ai::Session::new(crate::print::CheckSetup::fab_defaults().machine()?,
                path.parent().ok_or("missing plate directory")?.to_owned());
            session.path = Some(path.to_owned());
            session.model = model;
            session
        };
        let mut runner = Self { engine, session, bridge, seq: 0, facts: AppFacts::default() };
        let mail = Mailbox::default();
        runner.publish(update, &mail)?;
        runner.settle(&mail)?;
        Ok(runner)
    }

    fn publish(&mut self, update: CaseUpdate, mail: &Mailbox) -> Result<(), String> {
        let checked = match update {
            CaseUpdate::Set { generation, env } => {
                let inputs = Arc::downgrade(&env.inputs);
                self.session.model.set_env(env);
                let evaluated = self.session.rebuild()?;
                if let Some(lost) = evaluated.lost.first() {
                    return Err(format!("{}:{}: {}", lost.file, lost.line, lost.message));
                }
                self.bridge.reply(&inputs, Some(generation), Ok(evaluated))
                    .ok_or("the case did not accept its evaluated board inputs")?
            }
            checked => checked,
        };
        let CaseUpdate::Checked { run, commands, failed } = checked else {
            return Err("the case did not publish its checks".into())
        };
        self.facts.case_run = Some(run);
        self.facts.case_failed = failed;
        for cmd in commands { mail.push(cmd); }
        Ok(())
    }

    fn settle(&mut self, mail: &Mailbox) -> Result<Option<bool>, String> {
        let mut answer = None;
        loop {
            let before = self.engine.design().generation;
            let step = self.engine.step(mail.take(), &mut |_| {});
            for (_, outcome) in step.outcomes {
                answer = Some(matches!(outcome, fab_elec_host::Outcome::Done { .. }));
            }
            if answer.is_none() {
                if let Some((_, reason)) = step.rejected.first() { return Err(reason.clone()) }
            }
            let mut more = !step.evals.is_empty() || !step.fills.is_empty() || !step.checks.is_empty() || !step.routes.is_empty();
            for job in step.evals { mail.result(job.run()); }
            for job in step.fills { mail.result(job.run()); }
            for job in step.checks { mail.result(job.run()); }
            for job in step.routes {
                self.seq += 1;
                run_job(job, self.seq, mail)?;
            }
            if let Some(written) = step.docs {
                self.session.model.set_docs(written.docs);
                let mut assets = (**self.session.model.elec_assets()).clone();
                assets.retain(|name, _| written.assets.contains_key(name));
                for (name, bytes) in written.assets.iter() {
                    if let Some(bytes) = bytes { assets.insert(name.clone(), bytes.clone()); }
                }
                self.session.model.set_elec_assets(Arc::new(assets));
            }
            if self.engine.design().generation != before {
                if let Some(update) = self.bridge.cover(self.engine.design().clone()) {
                    self.publish(update, mail)?;
                    more = true;
                }
            }
            if !more { return Ok(answer) }
        }
    }
}

/// Run the usual route/placement worker on the replay caller. Inline jobs publish the
/// same final proposal; dropping its handle at that point ends the worker's request loop.
fn run_job(start: fab_elec_host::RouteStart, seq: u64, mail: &Mailbox) -> Result<(), String> {
    use fab_elec_route::{route_channel, RouteEvent, RouteHandle, RouteRequest, RouteWorker, Spawn};
    struct Inline;
    impl Spawn for Inline { fn spawn(&self, task: Box<dyn FnOnce() + Send>) { task(); } }
    let channel = Arc::new(Mutex::new(None::<RouteHandle>));
    let result = Arc::new(Mutex::new(None));
    let link = Arc::downgrade(&channel);
    let output = result.clone();
    let (handle, inbox) = route_channel(move || {
        let Some(channel) = link.upgrade() else { return };
        let mut slot = channel.lock().unwrap();
        while let Some(event) = slot.as_ref().and_then(RouteHandle::try_recv) {
            if let RouteEvent::Proposal(p) = event {
                if p.last {
                    *output.lock().unwrap() = Some(p);
                    slot.take();
                    break;
                }
            }
        }
    });
    let tag = start.job.id().tag();
    handle.request(RouteRequest::Base(start.base)).map_err(|_| "could not queue the board")?;
    handle.request(start.job.request()).map_err(|_| "could not queue the job")?;
    *channel.lock().unwrap() = Some(handle);
    RouteWorker::new(inbox, Arc::new(Inline)).run();
    let proposal = result.lock().unwrap().take().ok_or("the worker returned no final proposal")?;
    if let Some(reason) = proposal.metrics.pair_refusal.clone()
        .or_else(|| proposal.metrics.tune.as_ref()?.as_ref().err().cloned()) {
        mail.push(Cmd::RefusePreview { seq, tag, reason });
    } else {
        mail.preview(fab_elec_host::job_sample(Arc::new(proposal), start.by, seq).ok_or("expected a job proposal")?);
        mail.push(Cmd::CommitPreview { seq, tag });
    }
    Ok(())
}

/// The decision about headless inputs is shared by direct calls and outer electronics tools.
enum Dispatch<'a> {
    Catalog(&'a str, Vec<(String, String)>),
    Model(&'a str),
    Refused,
    Unavailable(&'static str),
}

fn dispatch<'a>(tool: &'a str, args: &'a Value, reopened_head: Option<fab_model::Head>) -> Dispatch<'a> {
    use Dispatch::*;
    let tool = crate::chat::short_tool(tool);
    let (action, fields, envelope) = match tool {
        // These envelopes carry the catalog action and its complete Splash arguments.
        "elec_edit" | "elec_run" | "elec_read" | "elec_external" => {
            let Some(action) = args.get("action").and_then(Value::as_str) else { return Refused };
            let fields = match args.get("args") {
                None | Some(Value::Null) => Some(Vec::new()),
                Some(Value::Obj(fields)) => Some(fields.iter().map(|(k, v)| (k.clone(), crate::splash_arg(v))).collect()),
                _ => None,
            };
            (action, fields, true)
        }
        // A recorded press keeps the catalog's ordered (name, Splash value) list.
        direct if direct.starts_with("elec.") => {
            let fields = args.as_arr().and_then(|rows| rows.iter().map(|r|
                Some((r.get("name")?.as_str()?.to_owned(), r.get("value")?.as_str()?.to_owned()))).collect());
            (direct, fields, false)
        }
        // These variants also need the slicer's plan or the active print setup.
        "section" if args.get("supports").and_then(Value::as_bool) == Some(true) =>
            return Unavailable("needs the host slicer"),
        "measure" if args.get("what").and_then(Value::as_str) == Some("mass") =>
            return Unavailable("needs the active print setup"),
        // Reads of the model, its scripts and its evaluated geometry use the copy.
        "inspect" | "guide" | "params" | "exact" | "tree" | "edges" | "check_mesh" |
        "section" | "settings" | "colors" | "surface" | "joints" | "interference" |
        // These tools commit model or plate edits described in their arguments.
        "apply" | "script" | "place" | "duplicate" | "remove" | "fill" | "measure" | "lay_flat" => {
            // A copied model revision has its file content, but a fresh session generation.
            if let (Some(head), Some(text)) = (reopened_head, args.get("expected").and_then(Value::as_str)) {
                if fab_model::Head::parse(text).is_some_and(|(g, _)| g != head.generation
                    && (fab_model::Head { generation: g, ..head }).matches(text)) {
                    return Unavailable("needs the original model revision token")
                }
            }
            return Model(tool)
        }
        // History and selections can include actions from before the checkpoint.
        "history" | "pick" | "select" | "round" | "shell" | "pull" | "hole" | "annotate" | "annotations" => return Unavailable("needs session history or selection"),
        // The active layout, pane or rendered frame belongs to the window.
        "workspace" | "workspace_edit" | "show" | "render" => return Unavailable("needs a window"),
        // These tools ask a device, a live service, or the person for confirmation.
        "voice" | "printer" | "print" | "image_to_mesh" => return Unavailable("needs a device or live service"),
        // The running slicer owns these plans and their selection.
        "slice" | "supports" | "weak_spots" | "strengthen" | "overhang_view" => return Unavailable("needs the host slicer"),
        // Opening/saving a different plate or importing a path involves the person's filesystem.
        "new" | "save" | "import" | "plates" | "export" | "picture" | "colour_read" => return Unavailable("needs files outside the replay copy"),
        _ => return Unavailable("not known to be replayable headless"),
    };
    match action {
        // Board geometry, routing jobs and circuit edits with a resolver on Engine::with_app_tools.
        "elec.route" | "elec.place" | "elec.net.tune" | "elec.pair.route" | "elec.zone.stitch" |
        "elec.board.outline" | "elec.keepout.add" | "elec.cutout.add" |
        "elec.part.place" | "elec.part.rotate" | "elec.part.flip" | "elec.part.align" |
        "elec.part.distribute" | "elec.value.set" | "elec.part.body.set" | "elec.factory.set" |
        "elec.stackup.set" | "elec.rules.set" | "elec.part.add" | "elec.part.remove" |
        "elec.part.rename" | "elec.part.field" | "elec.net.connect" | "elec.net.disconnect" |
        "elec.net.rename" | "elec.pin.nc" | "elec.unit.mount" | "elec.unit.unmount" |
        "elec.link.add" | "elec.link.remove" | "elec.link.replace" |
        // Bench declarations and BOM edits also resolve entirely against the copied documents.
        "elec.bench.supply" | "elec.bench.source" | "elec.bench.probe" | "elec.bom.field.set" |
        "elec.bom.dnp.set" | "elec.bom.alt.add" | "elec.bom.alt.use" => {
            if envelope {
                crate::chat::register_elec_actions();
                if fab_elec_design::action::find(action).and_then(|def| fab_ai::elec_tool_of(def.risk)) != Some(tool) {
                    return Refused
                }
            }
            match fields {
                Some(fields) => Catalog(action, fields),
                None if envelope => Refused,
                None => Unavailable("missing the recorded catalog arguments"),
            }
        }
        "elec.capture.save" if !fields.iter().flatten().any(|(key, _)| key == "capture") => Unavailable("needs a live capture"),
        // These have app-owned handlers outside Cmd::Invoke.
        "elec.capture.save" => Unavailable("needs the app capture handler"),
        "elec.req.add" | "elec.doc.read" | "elec.doc.list" | "elec.step.propose" | "elec.step.list" =>
            Unavailable("needs the app document handler"),
        _ => Unavailable("not known to be replayable headless"),
    }
}

type HashChanges = BTreeMap<String, (Option<String>, Option<String>)>;

struct Effects {
    files: Files,
    saved: super::mechanical::Saved,
    commits: Vec<HashChanges>,
}

struct Observer {
    electronics: String,
    state: Mutex<Effects>,
}

impl fab_elec_host::JournalSink for Observer {
    fn receive(&self, event: fab_elec_host::JournalEvent<'_>) {
        let fab_elec_host::JournalEvent::Effect { ticket, changes, .. } = event else { return };
        let mut state = self.state.lock().unwrap();
        let mut effect = HashChanges::new();
        let mut change = |name: &str, bytes: Option<Arc<[u8]>>| {
            let name = format!("{}/{name}", self.electronics);
            let before = state.files.get(&name);
            if before == bytes.as_ref() { return }
            effect.insert(name.clone(), (before.map(|b| sha256_hex(b)), bytes.as_deref().map(sha256_hex)));
            if let Some(bytes) = bytes { state.files.insert(name, bytes); } else { state.files.remove(&name); }
        };
        for (name, text) in changes.texts() { change(name, text.map(|s| Arc::from(s.as_bytes()))); }
        for asset in changes.assets() { change(&asset.name, asset.available.then(|| asset.bytes.0.clone())); }
        if ticket.is_some() { state.commits.push(effect); }
    }
}

impl fab_model::JournalSink for Observer {
    fn receive(&self, event: fab_model::JournalEvent<'_>, old: &fab_model::State, model: &fab_model::FabModel) {
        let mut state = self.state.lock().unwrap();
        let changes = state.saved.changes(old, model);
        if matches!(event, fab_model::JournalEvent::Commit { .. }) {
            state.commits.push(changes.into_iter().map(|c| (c.name, (c.before, c.after))).collect());
        }
    }
}

impl Runner {
    fn observe(&mut self, manifest: &str, files: &Files) -> Arc<Observer> {
        let observer = Arc::new(Observer {
            electronics: project::elec_dir(Path::new(manifest)).to_string_lossy().into_owned(),
            state: Mutex::new(Effects { files: files.clone(), saved: super::mechanical::Saved::opened(manifest, files,
                self.session.model.state()), commits: Vec::new() }),
        });
        self.session.model.set_journal(Some(observer.clone()));
        observer
    }

    fn invoke(&mut self, call: Dispatch<'_>, args: &Value, by: By, order: u64) -> Result<&'static str, String> {
        match call {
            Dispatch::Model(tool) => {
                let author = match by { By::Ai { .. } => fab_ai::Author::Ai, By::Agent { .. } => fab_ai::Author::Agent, _ => fab_ai::Author::Person };
                let reply = self.session.as_author(author).call(tool, args, None);
                if let Some((_, evaluated)) = reply.update {
                    let inputs = Arc::downgrade(&self.session.model.env().inputs);
                    if let Some(update) = self.bridge.reply(&inputs, None, Ok(evaluated)) {
                        let mail = Mailbox::default();
                        self.publish(update, &mail)?;
                        self.settle(&mail)?;
                    }
                }
                Ok(if reply.ok { "ok" } else if crate::model_host::reason(&reply.text) == "cancelled" { "cancelled" } else { "refused" })
            }
            Dispatch::Catalog(tool, fields) => {
                if self.session.model.docs().is_empty() { return Ok("refused") }
                let mail = Mailbox::default();
                mail.push(Cmd::Invoke { tool: tool.into(), args: fields, preview: false, by, ticket: Ticket(order) });
                match self.settle(&mail)? {
                    Some(true) => Ok("ok"), Some(false) => Ok("refused"), None => Err("no answer to the call".into()),
                }
            }
            Dispatch::Refused => Ok("refused"),
            Dispatch::Unavailable(_) => Err("expected a replayable call".into()),
        }
    }
}

fn author(row: &Value) -> io::Result<By> {
    let actor = field(row, "actor")?;
    let n = |key| field(actor,key)?.as_u64().ok_or_else(invalid);
    match string(actor, "kind")? {
        "person" => Ok(By::Person), "agent" => Ok(By::Agent { call: n("call")? }),
        "ai" => Ok(By::Ai { turn: n("turn")?, call: n("call")? }), "disk" => Ok(By::Disk),
        _ => Err(invalid()),
    }
}

#[derive(Default)]
struct Recorded<'a> {
    done: Option<&'a Value>,
    effects: Vec<&'a Value>,
}

impl Archive {
    fn calls(&self, start: u64, opening: u64) -> BTreeMap<u64, Recorded<'_>> {
        let mut calls = BTreeMap::<u64, Recorded<'_>>::new();
        for (_, row) in self.rows.range(start..=(start | LIFE_MASK)) {
            if row.get("checkpoint").and_then(Value::as_u64).is_some_and(|cp| cp != start && cp != opening) { continue }
            match row.get("kind").and_then(Value::as_str) {
                Some("effect") => if let Some(id) = row.get("call").and_then(Value::as_u64) {
                    calls.entry(id).or_default().effects.push(row);
                },
                Some("done") => if let Some(id) = row.get("id").and_then(Value::as_u64) {
                    calls.entry(id).or_default().done = Some(row);
                },
                _ => {}
            }
        }
        calls
    }
}

fn mismatch(why: &str) -> Outcome { Outcome::Mismatch { reason: why.into() } }

fn compare_call(runner: &mut Runner, observer: &Observer, row: &Value, record: &Recorded<'_>, call: Dispatch<'_>) -> io::Result<Outcome> {
    let done = record.done.ok_or_else(invalid)?;
    observer.state.lock().unwrap().commits.clear();
    let order = field(row,"order")?.as_u64().ok_or_else(invalid)?;
    let args = field(row, "args")?;
    // The wire records rejected JSON verbatim. Re-run that parser before either dispatcher.
    let result = if field(row, "actor")?.get("wire_id").is_some()
        && args.as_str().is_some_and(|raw| makepad_strict_json::parse(raw.as_bytes()).is_err()) {
        Ok("refused")
    } else {
        runner.invoke(call, args, author(row)?, order)
    };
    let expected_outcome = string(done, "outcome")?;
    if !matches!(expected_outcome, "ok" | "refused" | "cancelled") { return Err(invalid()) }
    let Ok(observed_outcome) = result else { return Ok(mismatch("headless did not deliver a call outcome")) };
    if observed_outcome != expected_outcome {
        return Ok(mismatch(&format!("recorded {expected_outcome}, replayed {observed_outcome}")))
    }
    let actual = std::mem::take(&mut observer.state.lock().unwrap().commits);
    if actual.len() != record.effects.len() { return Ok(mismatch("number of document effects differs")) }
    for (observed, effect) in actual.iter().zip(&record.effects) {
        let mut expected = HashChanges::new();
        for c in array(effect, "changes")? {
            let name = file_name(c)?;
            let before = digest(c,"before")?.map(str::to_owned);
            let after = digest(c,"after")?.map(str::to_owned);
            expected.entry(name.into()).or_insert((before, None)).1 = after;
        }
        for name in expected.keys().chain(observed.keys()) {
            if expected.get(name) != observed.get(name) {
                return Ok(mismatch(&format!("document effect differs for {name}")))
            }
        }
    }
    Ok(Outcome::Matched)
}

/// Calls are invoked once. Their linked effect rows still pass through the
/// verified byte walk; only independent changes require refreshing the runner.
struct Calls<'a> {
    recorded: BTreeMap<u64, Recorded<'a>>,
    matched: BTreeSet<u64>,
    live: Option<(Runner, Arc<Observer>)>,
    reload: bool,
    model_changed: bool,
    reopened_model: bool,
}

impl<'a> Calls<'a> {
    fn new(archive: &'a Archive, start: u64, opening: u64) -> Self {
        Self { recorded: archive.calls(start, opening), matched: BTreeSet::new(), live: None,
            reload: true, model_changed: false, reopened_model: false }
    }

    fn step(&mut self, step: &Step, row: &Value, files: &Files, manifest: Option<&str>, copy: &mut Copy<'_>)
        -> io::Result<Outcome>
    {
        let mut call = None;
        match step.kind {
            StepKind::Checkpoint => {}
            StepKind::Call => {
                if row.get("replay").and_then(Value::as_bool) == Some(false) {
                    return Ok(Outcome::NotReplayable { reason: "a person's decision; its edits are recorded as its effects".into() });
                }
                let action = string(row, "action")?;
                let args = field(row, "args")?;
                let how = dispatch(action, args, None);
                let id = field(row, "id")?.as_u64().ok_or_else(invalid)?;
                let record = self.recorded.get(&id);
                let unavailable = match (&how, record.and_then(|r| r.done)) {
                    (Dispatch::Unavailable(why), _) => Some(*why),
                    (_, None) => Some("call has no recorded answer in range"),
                    (_, Some(done)) if done.get("elsewhere").is_some() => Some("call also affects a different plate"),
                    _ => None,
                };
                if let Some(why) = unavailable { return Ok(Outcome::NotReplayable { reason: why.into() }) }
                let record = record.ok_or_else(invalid)?;
                let seqs = array(record.done.ok_or_else(invalid)?, "seqs")?;
                let present: BTreeSet<_> = record.effects.iter().filter_map(|r| r.get("seq")?.as_u64()).collect();
                if !seqs.iter().all(|s| s.as_u64().is_some_and(|n| present.contains(&n))) {
                    return Ok(Outcome::NotReplayable { reason: "call has unsettled effects in this range".into() })
                }
                call = Some((id, how, record));
            }
            StepKind::Effect | StepKind::Reload | StepKind::History => {
                let changes = array(row, "changes")?;
                if !changes.is_empty() && !row.get("call").and_then(Value::as_u64).is_some_and(|id| self.matched.contains(&id)) {
                    self.reload = true;
                    if let Some((_, observer)) = &self.live {
                        self.model_changed |= changes.iter().any(|c| !c.get("name").and_then(Value::as_str)
                            .is_some_and(|name| Path::new(name).starts_with(&observer.electronics)));
                    }
                }
                return Ok(Outcome::Applied)
            }
            StepKind::Lost => return Ok(Outcome::Applied),
        }
        if self.reload {
            copy.sync(files)?;
            let Some(name) = manifest else { return Ok(Outcome::Gap { reason: "no unambiguous plate manifest".into() }) };
            let previous = self.live.take().and_then(|(r, _)| (!self.model_changed).then_some(r.session));
            self.reopened_model |= self.model_changed;
            let Ok(mut runner) = Runner::load(&copy.root.join(name), None, previous) else {
                return Ok(mismatch("the copied plate could not be evaluated"))
            };
            let observer = runner.observe(name, files);
            runner.engine = runner.engine.with_journal(Some(observer.clone()));
            self.live = Some((runner, observer));
            self.reload = false;
            self.model_changed = false;
        }
        let Some((id, how, record)) = call else { return Ok(Outcome::Applied) };
        let (runner, observer) = self.live.as_mut().ok_or_else(invalid)?;
        if self.reopened_model && matches!(how, Dispatch::Model(_)) {
            if let Dispatch::Unavailable(reason) = dispatch(string(row, "action")?, field(row, "args")?, Some(runner.session.model.head())) {
                return Ok(Outcome::NotReplayable { reason: reason.into() })
            }
        }
        let outcome = compare_call(runner, observer, row, record, how)?;
        if outcome == Outcome::Matched { self.matched.insert(id); }
        Ok(outcome)
    }
}

pub const REPORT_FILE: &str = "replay-report.json";
pub const RESERVED: &str = "the plate already uses replay-report.json";

impl Copy<'_> {
    /// Materialize only changed revisions. Each newly written file is exclusively created,
    /// including a replacement: a conflicting file is never opened for truncation.
    fn sync(&mut self, new: &Files) -> io::Result<()> {
        let old = std::mem::take(&mut self.written);
        let mut ancestors = BTreeSet::new();
        for (name, bytes) in &old {
            if new.get(name).is_some_and(|v| Arc::ptr_eq(v, bytes)) { continue }
            let path = self.root.join(name);
            fs::remove_file(&path)?;
            self.files.remove(&path);
            ancestors.extend(path.ancestors().skip(1).take_while(|p| *p != self.root).map(Path::to_owned));
        }
        for dir in ancestors.into_iter().rev() {
            match fs::remove_dir(&dir) {
                Ok(()) => { self.dirs.remove(&dir); }
                Err(e) if e.kind() == io::ErrorKind::DirectoryNotEmpty => {}
                Err(e) => return Err(e),
            }
        }
        for (name, bytes) in new {
            if !old.get(name).is_some_and(|v| Arc::ptr_eq(v, bytes)) { self.write(name, bytes)?; }
        }
        self.written = new.clone();
        Ok(())
    }
}

impl StepKind {
    pub fn name(self) -> &'static str {
        match self { Self::Checkpoint => "checkpoint", Self::Call => "call", Self::Effect => "effect",
            Self::History => "history", Self::Reload => "reload", Self::Lost => "lost" }
    }
}

impl Outcome {
    pub fn name(&self) -> &'static str {
        match self { Self::Applied => "applied", Self::Matched => "matched", Self::Mismatch { .. } => "mismatch",
            Self::Gap { .. } => "gap", Self::NotReplayable { .. } => "not_replayable" }
    }
}

fn step_json(step: &Step) -> Value {
    let mut fields = vec![("order", number(step.order)), ("kind", s(step.kind.name())),
        ("action", step.action.as_deref().map_or(Value::Null, s)), ("outcome", s(step.outcome.name()))];
    match &step.outcome {
        Outcome::Mismatch { reason } | Outcome::NotReplayable { reason } => fields.push(("reason", s(reason))),
        // The State diagnostic contains a physical byte offset. It depends on record contents.
        Outcome::Gap { .. } => fields.push(("reason", s("incomplete or damaged journal"))),
        _ => {}
    }
    obj(fields)
}

#[derive(Default)]
pub(crate) struct Counts {
    pub(crate) outcomes: BTreeMap<&'static str, usize>,
    pub(crate) reasons: BTreeMap<String, usize>,
}
impl Counts {
    pub(crate) fn add(&mut self, step: &Step) {
        *self.outcomes.entry(step.outcome.name()).or_default() += 1;
        if let Outcome::NotReplayable { reason } = &step.outcome {
            *self.reasons.entry(reason.clone()).or_default() += 1;
        }
    }
}

/// Called before the first record, between records and after the last. Returning
/// an error leaves the destination subject to the usual claimed-file cleanup.
pub(crate) type WalkProgress<'a> = dyn FnMut(usize, usize) -> io::Result<()> + 'a;

pub fn replay(request: Request<'_>, progress: &mut dyn FnMut(&Step)) -> io::Result<Report> {
    replay_controlled(request, progress, None, None)
}

pub(crate) fn replay_controlled(request: Request<'_>, progress: &mut dyn FnMut(&Step),
    control: Option<&mut WalkProgress<'_>>, source: Option<&reader::VerifiedLog>) -> io::Result<Report> {
    let archive = Archive::read(request.journal, request.upto, source)?;
    let walk = archive.positions.through(request.upto);
    // Check the whole selected opening, including files created by a later effect,
    // before claiming a destination or running code from the plate.
    let opening = archive.rows.get(&walk.start).and_then(|r| r.get("checkpoint")?.as_u64()).unwrap_or(walk.start);
    for (_, row) in archive.rows.range(walk.start..=walk.end) {
        if row.get("checkpoint").and_then(Value::as_u64).is_some_and(|cp| cp != walk.start && cp != opening) { continue }
        for key in ["files", "changes"] {
            for item in row.get(key).and_then(Value::as_arr).into_iter().flatten() {
                if item.get("name").and_then(Value::as_str).and_then(|s| s.split('/').next()) == Some(REPORT_FILE) {
                    return Err(io::Error::new(io::ErrorKind::AlreadyExists, RESERVED))
                }
            }
        }
    }
    let mut copy = Copy::claim(request.into)?;
    let mut files = Files::new();
    let mut calls = (request.mode == Mode::Calls).then(|| Calls::new(&archive, walk.start, opening));
    let mut manifest_name = None;
    let mut steps = Vec::new();
    let mut counts = Counts::default();
    let mut publish = |step: &Step| {
        counts.add(step);
        steps.push(step_json(step));
        progress(step);
    };
    let stopped = walk.visit_controlled(request.journal, None, None, &mut files, |n| archive.row(n), |mut step, row, files| {
        if step.kind == StepKind::Checkpoint {
            manifest_name = manifest(row, files);
        }
        if let Some(calls) = &mut calls {
            step.outcome = calls.step(&step, row, files, manifest_name.as_deref(), &mut copy)?;
            if matches!(step.outcome, Outcome::Mismatch { .. } | Outcome::Gap { .. }) { return Ok(Some(step)) }
        }
        publish(&step);
        Ok(None)
    }, control)?;
    if let Some(stop) = &stopped { publish(stop); }
    drop(calls);
    if stopped.is_some() { files.clear(); }
    copy.sync(&files)?;
    let count = steps.len();
    let document = obj(vec![
        ("steps", Value::Arr(steps)), ("stop", stopped.as_ref().map_or(Value::Null, step_json)),
        ("counts", Value::Obj(counts.outcomes.iter().map(|(k,v)| (k.to_string(), number(*v as u64))).collect())),
        ("not_replayable_reasons", Value::Obj(counts.reasons.into_iter().map(|(k,v)| (k, number(v as u64))).collect())),
        ("manifest", manifest_name.as_deref().map_or(Value::Null, s)),
    ]);
    copy.write(REPORT_FILE, document.to_json().as_bytes())?;
    copy.keep = true;
    Ok(Report { steps: count, stopped, manifest: manifest_name, into: request.into.to_owned() })
}
