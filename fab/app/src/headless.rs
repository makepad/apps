//! Desktop command entry. Evaluates the saved plate and its electronics synchronously.
use crate::elec_export;
use fab_elec_circuit::{CheckKind, Violation};
use fab_elec_core::{Sel, Severity};
use fab_elec_host::{req_actions::{self, AppFacts}, Engine};
use fab_elec_req::Status;
use fab_elec_view::{board::findings::FindingCounts, DesignSnapshot};
#[cfg(test)]
use fab_model::project;
use makepad_strict_json::{obj, s, Value};
use std::ffi::OsString;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
#[cfg(test)]
use std::sync::Arc;

struct Commit {
    sha: Option<String>,
    source: &'static str,
}

/// Read the inner Fab repository beside the invocation. The project under review
/// may itself be a Git repository and has no bearing on this identity.
fn fab_commit() -> Commit {
    let sha = std::env::current_dir().ok().and_then(|cwd| {
        for ancestor in cwd.ancestors() {
            for root in [ancestor.to_owned(), ancestor.join("apps/apps")] {
                if root.join(".git").exists() && root.join("fab/app/Cargo.toml").is_file() {
                    let repo = makepad_git::Repository::open(&root).ok()?;
                    return repo.head_oid().ok().map(|id| id.to_hex());
                }
            }
        }
        None
    });
    Commit { source: if sha.is_some() { "checkout at run time" } else { "not run beside its checkout" }, sha }
}

fn evaluate(path: &Path) -> Result<(Engine, AppFacts), String> {
    let runner = crate::journal::replay::Runner::open(path, crate::part_models::library().as_deref())?;
    Ok((runner.engine, runner.facts))
}

fn finding(snap: &DesignSnapshot, v: &Violation) -> Value {
    let unit = v.unit.or_else(|| v.items.iter().find_map(Sel::unit)).and_then(|id| snap.design.unit(id));
    let kind = match &v.kind { CheckKind::Custom(name) => name.clone(), kind => format!("{kind:?}") };
    obj(vec![
        ("id", s(format!("{:016x}", v.id))), ("unit", unit.map_or(Value::Null, |u| s(&u.name))),
        ("check", s(kind)), ("rule", s(&v.rule)), ("severity", s(match v.severity {
            Severity::Error => "error", Severity::Warning => "warning", Severity::Info => "info",
        })),
        ("message", s(&v.message)), ("fix", s(&v.fix_hint)),
        ("at_nm", v.at.map_or(Value::Null, |p| Value::Arr(vec![Value::Int(p.x.into()), Value::Int(p.y.into())]))),
    ])
}

struct Output<'a> {
    json: bool,
    stdout: &'a mut dyn Write,
    stderr: &'a mut dyn Write,
}

impl Output<'_> {
    fn export(&mut self, snap: &DesignSnapshot, folder: &Path, board: Option<&str>) -> io::Result<u8> {
        if snap.profile_diags.iter().any(|d| d.severity == Severity::Error) {
            self.stage("export", "error", "invalid factory profile document", vec![])?;
            return Ok(2);
        }
        let unit = match fab_elec_host::edit_actions::board_unit(&snap.design, "--board", board) {
            Ok(unit) => unit,
            Err(diag) => {
                let names = snap.design.units.iter().filter(|u| u.board.is_some()).map(|u| u.name.as_str()).collect::<Vec<_>>();
                let why = if board.is_none() && names.len() > 1 {
                    format!("choose a unit with --board: {}", names.join(", "))
                } else { diag.message };
                self.stage("export", "error", &why, vec![])?;
                return Ok(2);
            }
        };
        let job = elec_export::ExportJob::from_snapshot(snap, unit, None, elec_export::Folder::NewOrEmpty(folder.to_owned()));
        let written = match elec_export::export(job) {
            Ok(written) => written,
            Err(why) => {
                self.stage("export", "error", &why, vec![])?;
                return Ok(2);
            }
        };
        let last = written.last.expect("a completed export identifies its board and profile");
        let mut failures = Vec::new();
        for check in written.validation.failures() {
            let file = folder.join(&check.file);
            writeln!(self.stderr, "{}: {}", file.display(), check.cause)?;
            failures.push(obj(vec![("file", s(&check.file)), ("reason", s(&check.cause))]));
        }
        let (code, status, verdict) = if failures.is_empty() {
            (0, "passed", "ok".into())
        } else {
            (1, "failed", format!("failed: {} problems", failures.len()))
        };
        let message = format!("{} files to {} ({}), validation {verdict}", written.manifest.len(), folder.display(), last.profile);
        let manifest = written.manifest.into_iter().map(|(name, size)| obj(vec![("name", s(name)), ("size", Value::Int(size as i64))])).collect();
        self.stage("export", status, &message, vec![
            ("board", s(&unit.name)), ("profile", s(last.profile)), ("folder", s(folder.to_string_lossy())),
            ("manifest", Value::Arr(manifest)), ("validation_failures", Value::Arr(failures)),
        ])?;
        Ok(code)
    }

    fn req(&mut self, snap: &DesignSnapshot, app: &AppFacts) -> io::Result<u8> {
        let docs = snap.docs.clone().unwrap_or_default();
        let shown = req_actions::shown(&docs, &req_actions::app_facts(snap, app)).unwrap_or_default();
        let report = &shown.report;
        let (met, total) = report.met_count();
        let mut message = format!("{met} of {total} met ({} failed, {} unknown, {} waived)", report.fail, report.unknown, report.waived);
        let (code, status) = if report.notes.iter().any(|diag| diag.severity == Severity::Error) {
            message.push_str(": invalid requirements document");
            (2, "error")
        } else if total == 0 {
            message.push_str(": no requirements declared");
            (1, "failed")
        } else if report.met() {
            (0, "passed")
        } else {
            (1, "failed")
        };

        let mut rows = Vec::new();
        for (row, (document, line)) in report.rows.iter().zip(&shown.at) {
            let reason = match &row.status {
                Status::Unknown(why) | Status::Waived { reason: why, .. } => why.clone(),
                Status::Pass | Status::Fail => format!("{}: {} ({})", row.measure, row.value.as_deref().unwrap_or(row.status.word()), row.bound),
            };
            if matches!(row.status, Status::Fail | Status::Unknown(_)) {
                writeln!(self.stderr, "{document}:{line}: {}: {}: {reason}", row.name, row.status.word())?;
            }
            if self.json {
                rows.push(obj(vec![
                    ("id", s(&row.name)), ("verdict", s(row.status.word())), ("reason", s(reason)),
                    ("document", s(document.as_ref())), ("line", Value::Int(*line as i64)),
                ]));
            }
        }
        let mut diagnostics = Vec::new();
        for (diag, located) in report.notes.iter().zip(&shown.mistakes) {
            writeln!(self.stderr, "{located}")?;
            if self.json {
                let (document, line) = req_actions::located(&docs, &diag.sel);
                let severity = match diag.severity { Severity::Error => "error", Severity::Warning => "warning", Severity::Info => "info" };
                diagnostics.push(obj(vec![
                    ("document", s(document)), ("line", Value::Int(line as i64)),
                    ("severity", s(severity)), ("message", s(&diag.message)),
                ]));
            }
        }
        self.stage("req", status, &message, vec![
            ("met", Value::Int(met as i64)), ("total", Value::Int(total as i64)),
            ("failed", Value::Int(report.fail as i64)), ("unknown", Value::Int(report.unknown as i64)),
            ("waived", Value::Int(report.waived as i64)), ("requirements", Value::Arr(rows)),
            ("diagnostics", Value::Arr(diagnostics)),
        ])?;
        Ok(code)
    }

    fn check(&mut self, snap: &DesignSnapshot) -> io::Result<u8> {
        if snap.profile_diags.iter().any(|d| d.severity == Severity::Error) {
            self.stage("check", "error", "invalid factory profile document", vec![])?;
            return Ok(2);
        }
        if snap.docs.is_none() {
            self.stage("check", "not_applicable", "not applicable", vec![("findings", Value::Arr(vec![]))])?;
            return Ok(0);
        }
        if !fab_elec_host::edit_actions::checks_current(snap) || !snap.awaiting.is_empty() {
            self.stage("check", "error", "board checks did not settle", vec![])?;
            return Ok(2);
        }

        let mut totals = FindingCounts::default();
        let mut records = Vec::new();
        // The engine snapshot merges the whole project's findings by stable ID.
        for v in snap.violations.value.iter() {
            totals.count(v);
            if self.json { records.push(finding(snap, v)); }
        }
        let FindingCounts { errors, unconnected, warnings, info } = totals;
        let message = format!("{errors} errors ({unconnected} unconnected), {warnings} warnings, {info} info");
        let code = u8::from(errors > 0);
        self.stage("check", if code == 0 { "passed" } else { "failed" }, &message, vec![
            ("errors", Value::Int(errors as i64)), ("unconnected", Value::Int(unconnected as i64)),
            ("warnings", Value::Int(warnings as i64)), ("info", Value::Int(info as i64)), ("findings", Value::Arr(records)),
        ])?;
        Ok(code)
    }

    fn stage(&mut self, stage: &str, status: &str, message: &str, fields: Vec<(&str, Value)>) -> io::Result<()> {
        if self.json {
            let mut record = vec![("stage", s(stage)), ("status", s(status)), ("message", s(message))];
            record.extend(fields);
            writeln!(self.stdout, "{}", obj(record).to_json())?;
        } else {
            writeln!(self.stdout, "{stage}: {}", message.lines().collect::<Vec<_>>().join(" "))?;
        }
        if matches!(status, "failed" | "error") {
            writeln!(self.stderr, "{stage}: {message}")?;
        }
        Ok(())
    }
}

const REPLAY_HELP: &str = "--headless --replay <journal> --into <directory> [--to <order>] [--calls]\nThe plate's own output appears as it does when the plate runs in Fab. The report is replay-report.json in the destination directory.";

fn replay_command(args: &[OsString], stdout: &mut dyn Write) -> io::Result<u8> {
    use crate::journal::replay::{self, Counts, Mode, Request};
    if args.iter().any(|arg| arg == "--help") { writeln!(stdout, "{REPLAY_HELP}")?; return Ok(0) }
    let mut args = args.iter().peekable();
    let path = |arg: &&OsString| !arg.is_empty() && !arg.to_string_lossy().starts_with("--");
    let journal = args.next_if(path).map(PathBuf::from);
    let mut into = None;
    let mut upto = None;
    let mut mode = Mode::State;
    let mut valid = true;
    while let Some(flag) = args.next() {
        match flag.to_str() {
            Some("--into") if into.is_none() => { into = args.next_if(path).map(PathBuf::from); valid &= into.is_some(); }
            Some("--to") if upto.is_none() => {
                upto = args.next_if(path).and_then(|v| v.to_str()?.parse::<u64>().ok());
                valid &= upto.is_some();
            }
            Some("--calls") if mode == Mode::State => mode = Mode::Calls,
            _ => valid = false,
        }
    }
    let Some((journal, into)) = journal.zip(into).filter(|_| valid) else {
        writeln!(stdout, "{REPLAY_HELP}")?;
        return Ok(2)
    };
    let mut counts = Counts::default();
    let report = match replay::replay(Request { journal: &journal, into: &into, upto, mode }, &mut |s| counts.add(s)) {
        Ok(r) => r,
        Err(e) => {
            let reason = if e.to_string() == replay::RESERVED { replay::RESERVED.to_owned() }
                else { format!("replay could not read the journal or write the destination ({})", e.kind()) };
            writeln!(stdout, "{reason}")?;
            return Ok(2)
        }
    };
    writeln!(stdout, "{} steps", report.steps)?;
    for (outcome, count) in counts.outcomes { writeln!(stdout, "{outcome} {count}")?; }
    for (reason, count) in counts.reasons { writeln!(stdout, "not_replayable {count} {reason}")?; }
    if let Some(s) = &report.stopped { writeln!(stdout, "stop {} {} {}", s.order, s.kind.name(), s.outcome.name())?; }
    writeln!(stdout, "{}", replay::REPORT_FILE)?;
    Ok(u8::from(report.stopped.is_some()))
}

fn run(args: Vec<OsString>, stdout: &mut dyn Write, stderr: &mut dyn Write) -> io::Result<u8> {
    if args.first().is_some_and(|arg| arg == "--replay") { return replay_command(&args[1..], stdout) }
    if args.as_slice() == [OsString::from("--help")] { writeln!(stdout, "{REPLAY_HELP}")?; return Ok(0) }
    let mut out = Output { json: args.iter().any(|a| a == "--json"), stdout, stderr };
    let project = args.first().filter(|p| !p.to_string_lossy().starts_with("--")).map(PathBuf::from);
    let mut code = 0;
    let mut check = false;
    let mut req = false;
    let mut export = None;
    let mut board = None;
    let mut flags = args.iter().skip(usize::from(project.is_some())).peekable();
    let mut invalid = None;
    while let Some(flag) = flags.next() {
        match flag.to_str() {
            Some("--check") => check = true,
            Some("--json") => (),
            Some("--req") => req = true,
            Some("--export") if export.is_none() => {
                export = flags.next_if(|a| !a.is_empty() && !a.to_string_lossy().starts_with("--")).map(PathBuf::from);
                if export.is_none() { invalid = Some("--export requires a destination directory".into()); break }
            }
            Some("--board") if board.is_none() => {
                board = flags.next_if(|a| !a.to_string_lossy().starts_with("--")).and_then(|a| a.to_str()).filter(|s| !s.trim().is_empty());
                if board.is_none() { invalid = Some("--board requires a unit name".into()); break }
            }
            _ => { invalid = Some(format!("unexpected argument {}", flag.to_string_lossy())); break }
        }
    }
    if invalid.is_none() && board.is_some() && export.is_none() { invalid = Some("--board requires --export <dir>".into()); }
    if project.is_none() { invalid = Some("expected --headless <project.fab.splash>".into()); }
    if let Some(why) = invalid {
        out.stage("arguments", "error", &why, vec![])?;
        code = 2;
    } else if let Some(path) = &project {
        match evaluate(path) {
            Err(why) => {
                out.stage("evaluate", "error", &why, vec![])?;
                code = 2;
            }
            Ok((engine, app)) => {
                let snap = engine.snapshot();
                for message in crate::elec_host::profile_mistakes(&snap) {
                    writeln!(out.stderr, "{message}")?;
                }
                let units = snap.design.units.len();
                let noun = if units == 1 { "unit" } else { "units" };
                out.stage("evaluate", "passed", &format!("ok ({units} electronics {noun})"), vec![("units", Value::Int(units as i64))])?;
                if check {
                    code = out.check(&snap)?;
                }
                if req {
                    code = code.max(out.req(&snap, &app)?);
                }
                if let Some(folder) = &export {
                    code = code.max(out.export(&snap, folder, board)?);
                }
            }
        }
    }
    let path = project.as_deref().map(|p| p.to_string_lossy());
    let commit = fab_commit();
    let identity = match &commit.sha {
        Some(sha) => format!("commit {sha} ({})", commit.source),
        None => format!("commit unknown: {}", commit.source),
    };
    let summary = format!("{} · {identity} · exit {code}", path.as_deref().unwrap_or("(no project)"));
    out.stage("summary", match code { 0 => "passed", 1 => "failed", _ => "error" }, &summary, vec![
        ("project", path.as_deref().map_or(Value::Null, s)), ("commit", commit.sha.map_or(Value::Null, s)),
        ("commit_source", s(commit.source)), ("exit_code", Value::Int(code.into())),
    ])?;
    Ok(code)
}

pub(super) fn main(args: impl Iterator<Item = OsString>) -> std::process::ExitCode {
    match run(args.collect(), &mut io::stdout(), &mut io::stderr()) {
        Ok(code) => code.into(),
        Err(error) => {
            eprintln!("headless output: {error}");
            2.into()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{collections::BTreeMap, fs};

    fn contents(dir: &Path) -> BTreeMap<PathBuf, Option<Vec<u8>>> {
        fn scan(dir: &Path, tree: &mut BTreeMap<PathBuf, Option<Vec<u8>>>) {
            for entry in fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    tree.insert(path.clone(), None);
                    scan(&path, tree);
                } else {
                    tree.insert(path.clone(), Some(fs::read(path).unwrap()));
                }
            }
        }
        let mut tree = BTreeMap::new();
        scan(dir, &mut tree);
        tree
    }

    fn command(path: &Path, flags: &[&str]) -> (u8, String, String) {
        let mut args = vec![path.as_os_str().to_owned()];
        args.extend(flags.iter().map(OsString::from));
        let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
        let dir = path.parent().unwrap();
        let before = contents(dir);
        let code = run(args, &mut stdout, &mut stderr).unwrap();
        assert_eq!(contents(dir), before, "{flags:?} must only read the project");
        (code, String::from_utf8(stdout).unwrap(), String::from_utf8(stderr).unwrap())
    }

    fn json_lines(stdout: &str) -> Vec<Value> {
        stdout.lines().map(|line| makepad_strict_json::parse(line.as_bytes()).unwrap()).collect()
    }

    fn check_exports(project: &Path, scratch: &Path, snap: &DesignSnapshot) {
        let folder = scratch.join("fabrication");
        let dest = folder.to_str().unwrap();
        let (code, stdout, stderr) = command(project, &["--check", "--req", "--export", dest, "--json"]);
        assert_eq!(code, 1, "{stdout} {stderr}");
        let stages = json_lines(&stdout);
        assert_eq!(stages.iter().map(|r| r.get("stage").and_then(Value::as_str).unwrap()).collect::<Vec<_>>(),
            ["evaluate", "check", "req", "export", "summary"]);
        let export = &stages[3];
        assert_eq!(export.get("status"), Some(&s("passed")));
        assert_eq!(export.get("board"), Some(&s("ledctl")));
        assert_eq!(export.get("profile"), Some(&s(fab_elec_cam::PROFILES[0])));
        assert!(export.get("validation_failures").and_then(Value::as_arr).unwrap().is_empty());
        let manifest = export.get("manifest").and_then(Value::as_arr).unwrap();
        let files = fs::read_dir(&folder).unwrap().map(|entry| {
            let entry = entry.unwrap();
            (entry.file_name().into_string().unwrap(), entry.metadata().unwrap().len())
        }).collect::<BTreeMap<_, _>>();
        let declared = manifest.iter().map(|file| (
            file.get("name").and_then(Value::as_str).unwrap().to_owned(),
            file.get("size").and_then(Value::as_u64).unwrap(),
        )).collect::<BTreeMap<_, _>>();
        assert_eq!(manifest.len(), files.len());
        assert_eq!(declared, files);
        assert!(files.keys().any(|name| name.ends_with(".zip")));

        let unit = snap.design.unit_by_name("ledctl").unwrap();
        let fills = elec_export::committed_fills(snap, unit.id);
        assert!(!fills.is_empty(), "the ledctl export must include its settled pours");
        let last = elec_export::LastExport { unit: unit.id, profile: fab_elec_cam::PROFILES[0].into(), folder: folder.clone() };
        let disk = elec_export::validate(snap, &last).unwrap();
        assert!(disk.validation.ok(), "{:?}", disk.validation.failures().collect::<Vec<_>>());

        fs::write(folder.join("README.txt"), "keep this version").unwrap();
        let before = contents(&folder);
        let (code, _, stderr) = command(project, &["--export", dest]);
        assert_eq!(code, 2);
        assert!(stderr.contains("new or empty directory"));
        assert_eq!(contents(&folder), before);
        let absent = scratch.join("missing-parent/plots");
        assert_eq!(command(project, &["--export", absent.to_str().unwrap()]).0, 2);
        assert!(!absent.parent().unwrap().exists(), "only the requested directory may be created");
        assert_eq!(command(project, &["--export", "--json"]).0, 2);

        // A second board has its own factory and two placed parts sharing a reference.
        // CAM writes the placement table but its rows cannot both match the same pose.
        let docs = project::elec_dir(project);
        let circuit = docs.join("daughter.ckt.splash");
        let pcb = docs.join("daughter.pcb.splash");
        fs::write(&circuit, "use mod.elec.*\nuse mod.std.*\nlet e = mod.elec\n\
            let a = e.r(1kohm, nil, nil, {package: \"0402\" ref: \"R.Pair\"})\n\
            let b = e.r(2kohm, nil, nil, {package: \"0402\" ref: \"R.Pair\"})\n").unwrap();
        fs::write(&pcb, format!("board(\"daughter\")\nfactory(\"{}\")\n\
            outline([[[0mm, 0mm], [20mm, 0mm], [20mm, 20mm], [0mm, 20mm], [0mm, 0mm]]])\n\
            place(\"a\", [5mm, 5mm])\nplace(\"b\", [15mm, 15mm])\n", fab_elec_cam::PROFILES[1])).unwrap();
        let other = scratch.join("selected");
        let dest = other.to_str().unwrap();
        let (code, _, stderr) = command(project, &["--export", dest]);
        assert_eq!(code, 2, "{stderr}");
        assert!(stderr.lines().any(|l| l.contains("--board") && l.contains("ledctl") && l.contains("daughter")));
        let (code, _, stderr) = command(project, &["--export", dest, "--board", "missing"]);
        assert_eq!(code, 2);
        assert!(stderr.contains("no unit named missing"));
        assert!(!other.exists());

        fs::create_dir(&other).unwrap();
        let (code, stdout, stderr) = command(project, &["--export", dest, "--board", "ledctl"]);
        assert_eq!(code, 0, "{stdout} {stderr}");
        assert!(stdout.lines().any(|l| l == format!("export: {} files to {dest} ({}), validation ok", files.len(), last.profile)));
        let invalid = scratch.join("invalid");
        let (code, stdout, stderr) = command(project, &["--export", invalid.to_str().unwrap(), "--board", "daughter", "--json"]);
        assert_eq!(code, 1, "{stdout} {stderr}");
        let stages = json_lines(&stdout);
        let failed = &stages[1];
        assert_eq!(failed.get("status"), Some(&s("failed")));
        assert_eq!(failed.get("profile"), Some(&s(fab_elec_cam::PROFILES[1])));
        let failures = failed.get("validation_failures").and_then(Value::as_arr).unwrap();
        assert!(failures.iter().any(|f| f.get("file") == Some(&s("daughter-pos.csv")) && f.get("reason").and_then(Value::as_str).unwrap().contains("R.Pair")));
        for failure in failures {
            let name = failure.get("file").and_then(Value::as_str).unwrap();
            let why = failure.get("reason").and_then(Value::as_str).unwrap();
            assert!(stderr.contains(&format!("{}: {why}", invalid.join(name).display())));
        }
        assert!(invalid.join("daughter-pos.csv").is_file());
        assert!(!invalid.join("daughter-A.zip").exists());
        fs::remove_file(circuit).unwrap();
        fs::remove_file(pcb).unwrap();

        let house = docs.join("pilot.export.splash");
        let layout = docs.join("ledctl.pcb.splash");
        let original = fs::read_to_string(&layout).unwrap();
        fs::write(&house, r#"profile("Pilot", { format: "JLCPCB 2-layer" layers: [2]
            track: 0.15mm clearance: 0.15mm drill: 0.3mm annular: 0.13mm via_in_pad: false
            board_min: [5mm 5mm] board_max: [400mm 500mm] thickness: [1.6mm] thickness_tol_percent: 5 })"#).unwrap();
        fs::write(&layout, format!("{original}\nfactory(\"Pilot\")\n")).unwrap();
        let pilot = scratch.join("pilot");
        let (code, stdout, stderr) = command(project, &["--export", pilot.to_str().unwrap(), "--json"]);
        assert_eq!(code, 0, "{stdout}\n{stderr}");
        assert_eq!(json_lines(&stdout)[1].get("profile"), Some(&s("Pilot")));
        assert!(fs::read_to_string(pilot.join("README.txt")).unwrap().contains("Pilot"));
        assert!(fs::read_to_string(pilot.join("ledctl-bom.csv")).unwrap().lines().next().unwrap().contains("LCSC Part #"));
        fs::write(&layout, format!("{original}\nfactory(\"Pilot\")\nrules({{track: 0.10mm}})\n")).unwrap();
        let (code, stdout, stderr) = command(project, &["--check", "--json"]);
        assert_eq!(code, 1, "{stdout} {stderr}");
        let stages = json_lines(&stdout);
        let check = &stages[1];
        let warnings: Vec<_> = check.get("findings").unwrap().as_arr().unwrap().iter()
            .filter(|v| v.get("severity").and_then(Value::as_str) == Some("warning")).collect();
        assert_eq!(check.get("warnings").and_then(Value::as_u64), Some(warnings.len() as u64));
        assert_eq!(warnings.iter().filter(|v| v.get("rule").and_then(Value::as_str) == Some("factory:Pilot:track")).count(), 1);

        fs::write(&house, "// Missing capabilities\nprofile(\"Pilot\", {})").unwrap();
        let invalid_profile = scratch.join("invalid-profile");
        for flags in [vec!["--check", "--json"], vec!["--export", invalid_profile.to_str().unwrap(), "--json"]] {
            let (code, stdout, stderr) = command(project, &flags);
            assert_eq!(code, 2, "{stdout} {stderr}");
            assert_eq!(json_lines(&stdout)[1].get("status"), Some(&s("error")));
            assert!(stderr.contains("pilot.export.splash:2:"), "{stderr}");
        }
        assert!(!invalid_profile.exists());
        fs::write(layout, original).unwrap();
        fs::remove_file(house).unwrap();
    }

    #[test]
    fn headless_req_checks_planned_block_erc_clean() {
        let dir = std::env::temp_dir().join(format!("fab-architecture-{}-{}", std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        let path = dir.join("device.fab.splash");
        let docs = project::elec_dir(&path);
        fs::create_dir_all(&docs).unwrap();
        fs::write(&path, "use mod.fab.*\n").unwrap();
        fs::write(docs.join("device.req.splash"), r#"require("ERC clean", checks(@erc).errors, {max: 0})"#).unwrap();
        for (cap, wanted) in [(20, 1), (30, 0)] {
            let text = r#"let e = mod.elec
let reg = e.block({name: "Source" planned: true ports: {out: e.port(@power_out, {current: CAPmA})}})
let load = e.block({name: "Load" planned: true ports: {v: e.port(@power_in, {current: 30mA})}})
let rail = e.net({name: "VDD" kind: @power})
let src = reg({out: rail})
let device = load({v: rail})
"#.replace("CAP", &cap.to_string());
            fs::write(docs.join("device.ckt.splash"), text).unwrap();
            let (code, stdout, stderr) = command(&path, &["--check", "--req"]);
            assert_eq!(code, wanted, "the headless exit code must reflect the {cap} mA rail's ERC result: {stdout} {stderr}");
            assert!(stdout.contains(if wanted == 0 { "1 of 1 met" } else { "0 of 1 met" }),
                "the requirements report must judge the architecture: {stdout} {stderr}");
        }
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn headless_req_passes_erc_clean_after_filled_schematic() {
        let dir = std::env::temp_dir().join(format!("fab-filled-schematic-{}", std::process::id()));
        let path = dir.join("device.fab.splash");
        let docs = project::elec_dir(&path);
        fs::create_dir_all(&docs).unwrap();
        fs::write(&path, "use mod.fab.*\n").unwrap();
        fs::write(docs.join("device.req.splash"), r#"require("ERC clean", checks(@erc).errors, {max: 0})
require("All placed", board.unplaced, {max: 0})"#).unwrap();
        fs::write(docs.join("device.blocks.splash"), r#"let e = mod.elec
let load = e.block({name: "Load" ports: {a: e.port(@passive) b: e.port(@passive)} build: fn(p, o) {
    let r = e.r(1kohm, p.a, p.b, {package: "0402"})
    let c = e.c(1uF, p.a, p.b, {package: "0603"})
}})"#).unwrap();
        fs::write(docs.join("device.ckt.splash"), r#"use mod.project.*
let e = mod.elec
let signal = e.net({name: "SENSE"})
let gnd = e.net({name: "GND" kind: @ground})
let input = load({a: signal b: gnd})"#).unwrap();
        let (code, stdout, stderr) = command(&path, &["--check", "--req", "--json"]);
        fs::remove_dir_all(dir).unwrap();
        assert_eq!(code, 1, "the unplaced parts must keep the headless gate open: {stdout} {stderr}");
        let stages = json_lines(&stdout);
        let rows = stages.iter().find_map(|s| s.get("requirements").and_then(Value::as_arr)).expect("the gate must report its requirements");
        let verdict = |name| rows.iter().find(|r| r.get("id").and_then(Value::as_str) == Some(name))
            .and_then(|r| r.get("verdict")).and_then(Value::as_str);
        assert_eq!(verdict("ERC clean"), Some("pass"), "the filled schematic must pass ERC before layout");
        assert_eq!(verdict("All placed"), Some("fail"), "the schematic proposal must leave placement for the board stage");
    }

    #[test]
    fn headless_req_passes_on_brief_ledctl_routed_board() {
        use fab_elec_fixtures::brief_ledctl as fixture;

        let dir = std::env::temp_dir().join(format!("fab-routed-brief-{}", std::process::id()));
        let plate = dir.join("brief_ledctl.fab.splash");
        let docs = project::elec_dir(&plate);
        fs::create_dir_all(&docs).unwrap();
        fs::write(&plate, "use mod.fab.*\n").unwrap();
        for &(name, text) in fixture::REQUIREMENTS.iter().chain(fixture::PARTS).chain(fixture::SCHEMATIC) {
            fs::write(docs.join(name), text).unwrap();
        }
        fs::write(docs.join("brief_ledctl.pcb.splash"), fixture::ROUTED).unwrap();

        let (code, stdout, stderr) = command(&plate, &["--check", "--req", "--json"]);
        let saved = fs::read_to_string(docs.join("brief_ledctl.req.splash")).unwrap();
        fs::remove_dir_all(dir).unwrap();
        assert_eq!(code, 0, "the routed plate must pass --check --req after reopening: {stdout} {stderr}");
        let stages = json_lines(&stdout);
        let check = stages.iter().find(|s| s.get("stage").and_then(Value::as_str) == Some("check")).expect("missing board check");
        for key in ["errors", "unconnected"] {
            assert_eq!(check.get(key).and_then(Value::as_u64), Some(0), "the saved board must reopen with zero {key}");
        }

        let rows = stages.iter().find_map(|s| s.get("requirements").and_then(Value::as_arr)).expect("missing graded requirements");
        let verdicts: Vec<_> = rows.iter().map(|r| (r.get("id").and_then(Value::as_str), r.get("verdict").and_then(Value::as_str))).collect();
        let names = ["fits the enclosure", "two copper layers", "every part placed", "fully routed", "ERC clean", "DRC clean"];
        assert_eq!(verdicts, names.map(|name| (Some(name), Some("pass"))), "exactly the brief's six measurable targets must pass");
        let (requirements, errors) = fab_elec_req::read(fab_elec_core::DocId(0), &saved);
        assert!(errors.is_empty(), "the accepted requirements must still read: {errors:?}");
        assert_eq!(requirements.wishes.iter().map(|wish| wish.words.as_str()).collect::<Vec<_>>(),
            ["Keep silkscreen labels clean and readable."], "the silkscreen request must survive as an ungraded wish");
    }

    #[test]
    fn saved_plate_reports_its_committed_findings_without_writing() {
        struct Scratch(PathBuf);
        impl Drop for Scratch {
            fn drop(&mut self) { let _ = fs::remove_dir_all(&self.0); }
        }
        let dir = std::env::temp_dir().join(format!("fab-headless-{}-{}", std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        fs::create_dir(&dir).unwrap();
        let scratch = Scratch(dir);
        let project_dir = scratch.0.join("project");
        fs::create_dir(&project_dir).unwrap();
        let path = project_dir.join("ledctl.fab.splash");
        fs::write(&path, "use mod.fab.*\n").unwrap();
        let (code, stdout, _) = command(&path, &["--check"]);
        assert_eq!(code, 0, "{stdout}");
        assert!(stdout.lines().any(|l| l == "check: not applicable"));
        let (code, stdout, stderr) = command(&path, &["--req"]);
        assert_eq!(code, 1);
        assert!(stdout.contains("no requirements declared") && stderr.contains("no requirements declared"));

        let docs = project::elec_dir(&path);
        fs::create_dir(&docs).unwrap();
        let requirements = docs.join("ledctl.req.splash");
        let needs_slice = "require(\"print budget\", print.filament, {max: 40g})\n";
        fs::write(&requirements, format!("{needs_slice}propose_waiver(\"print budget\", \"prototype\")\n")).unwrap();
        let (code, stdout, stderr) = command(&path, &["--req"]);
        assert_eq!(code, 1, "{stdout} {stderr}");
        assert!(stdout.lines().any(|l| l == "req: 0 of 1 met (0 failed, 1 unknown, 0 waived)"));
        assert!(stderr.contains("ledctl.req.splash:1: print budget: unknown: not sliced yet"));
        fs::write(&requirements, format!("{needs_slice}waive(\"print budget\", \"prototype\")\n")).unwrap();
        let (code, stdout, stderr) = command(&path, &["--req"]);
        assert_eq!(code, 0, "{stdout} {stderr}");
        assert!(stdout.lines().any(|l| l == "req: 1 of 1 met (0 failed, 0 unknown, 1 waived)"));
        assert!(stderr.is_empty());

        for doc in fab_elec_fixtures::docs("ledctl").unwrap().docs() {
            fs::write(docs.join(&doc.name), &*doc.text).unwrap();
        }
        fs::write(&requirements, fab_elec_fixtures::LEDCTL_REQ).unwrap();
        let before = contents(&project_dir);
        let (code, stdout, stderr) = command(&path, &[]);
        assert_eq!(code, 0, "{stdout} {stderr}");
        let commit = fab_commit();
        let words = match &commit.sha {
            Some(sha) => format!("commit {sha} (checkout at run time)"),
            None => "commit unknown: not run beside its checkout".to_owned(),
        };
        assert_eq!(stdout.lines().count(), 2);
        assert!(stdout.lines().last().unwrap().contains(&words));
        assert!(stderr.is_empty());

        let (code, stdout, stderr) = command(&path, &["--check", "--req", "--json"]);
        assert_eq!(code, 1, "{stdout} {stderr}");
        let rows = json_lines(&stdout);
        assert_eq!(rows.iter().map(|r| r.get("stage").and_then(Value::as_str)).collect::<Vec<_>>(),
            [Some("evaluate"), Some("check"), Some("req"), Some("summary")]);
        let summary = &rows[3];
        assert_eq!(summary.get("project").and_then(Value::as_str), path.to_str());
        assert_eq!(summary.get("commit"), Some(&commit.sha.map_or(Value::Null, s)));
        assert_eq!(summary.get("exit_code").and_then(Value::as_i64), Some(1));

        let snap = Engine::new(fab_elec_fixtures::ledctl(), Arc::new(fab_elec_cam::builtin_factories()),
            Arc::default(), Arc::default(), "fixture".into()).snapshot();
        let findings = rows[1].get("findings").and_then(Value::as_arr).unwrap();
        assert_eq!(findings.len(), snap.violations.value.len());
        for (record, v) in findings.iter().zip(snap.violations.value.iter()) {
            assert_eq!(record.get("id"), Some(&s(format!("{:016x}", v.id))));
            assert_eq!(record.get("message").and_then(Value::as_str), Some(v.message.as_str()));
        }
        let count = |key| rows[1].get(key).and_then(Value::as_u64).unwrap();
        assert_eq!(count("errors") as usize, snap.violations.value.iter().filter(|v| v.severity == Severity::Error).count());
        assert_eq!(count("unconnected") as usize, snap.violations.value.iter().filter(|v| v.kind == CheckKind::Unconnected).count());
        assert!(count("unconnected") > 0 && stderr.contains("unconnected"));

        let (engine, app) = evaluate(&path).unwrap();
        let settled = engine.snapshot();
        let facts = req_actions::app_facts(&settled, &app);
        let (fixture, errors) = fab_elec_req::read(fab_elec_core::DocId(0), fab_elec_fixtures::LEDCTL_REQ);
        assert!(errors.is_empty());
        let judge = fab_elec_req::judge(&[fixture], &facts);
        let (met, total) = judge.met_count();
        let req = &rows[2];
        for (key, n) in [("met", met), ("total", total), ("failed", judge.fail), ("unknown", judge.unknown), ("waived", judge.waived)] {
            assert_eq!(req.get(key).and_then(Value::as_u64), Some(n as u64), "{key}");
        }
        let records = req.get("requirements").and_then(Value::as_arr).unwrap();
        assert_eq!(records.len(), total);
        for (record, row) in records.iter().zip(&judge.rows) {
            assert_eq!(record.get("id"), Some(&s(&row.name)));
            assert_eq!(record.get("verdict"), Some(&s(row.status.word())));
            assert_eq!(record.get("document"), Some(&s("ledctl.req.splash")));
            let line = fab_elec_fixtures::LEDCTL_REQ.lines().position(|l| l.starts_with(&format!("require(\"{}\"", row.name))).unwrap() + 1;
            assert_eq!(record.get("line").and_then(Value::as_u64), Some(line as u64));
            let reason = record.get("reason").and_then(Value::as_str).unwrap();
            match &row.status {
                Status::Unknown(why) | Status::Waived { reason: why, .. } => assert_eq!(reason, why),
                Status::Pass | Status::Fail => assert!(reason.contains(row.value.as_deref().unwrap()) && reason.contains(&row.bound)),
            }
            if matches!(row.status, Status::Fail | Status::Unknown(_)) {
                assert!(stderr.contains(&format!("ledctl.req.splash:{line}: {}: {}: {reason}", row.name, row.status.word())));
            }
        }
        assert!(stderr.contains("ledctl.req.splash:5: fully routed: fail:") && stderr.contains("ledctl.req.splash:7: DRC clean: fail:"));
        assert!(stderr.contains("not checked yet: run Check bench on bringup") && stderr.contains("not sliced yet: slice the plate"));
        let (code, stdout, stderr) = command(&path, &["--req"]);
        assert_eq!(code, 1, "{stdout} {stderr}");
        assert!(stdout.lines().any(|l| l == format!("req: {met} of {total} met ({} failed, {} unknown, {} waived)", judge.fail, judge.unknown, judge.waived)));
        check_exports(&path, &scratch.0, &settled);
        assert_eq!(contents(&project_dir), before);

        let bad_req = docs.join("broken.req.splash");
        fs::write(&bad_req, "// Malformed second document beside the complete fixture.\nrequire(\"broken\", ]").unwrap();
        let (code, stdout, stderr) = command(&path, &["--check", "--req", "--json"]);
        assert_eq!(code, 2, "{stdout} {stderr}");
        let stages = json_lines(&stdout);
        assert_eq!(stages[1].get("status"), Some(&s("failed")));
        assert_eq!(stages[2].get("status"), Some(&s("error")));
        assert_eq!(stages[3].get("exit_code"), Some(&Value::Int(2)));
        let diag = &stages[2].get("diagnostics").and_then(Value::as_arr).unwrap()[0];
        assert_eq!(diag.get("document"), Some(&s("broken.req.splash")));
        assert_eq!(diag.get("line"), Some(&Value::Int(2)));
        let why = diag.get("message").and_then(Value::as_str).unwrap();
        assert!(why.starts_with("syntax:") && stderr.contains(&format!("broken.req.splash:2: {why}")));
        fs::remove_file(bad_req).unwrap();

        fs::write(docs.join("broken.ckt.splash"), "circuit(\n").unwrap();
        let mixed = contents(&project_dir);
        let (code, stdout, stderr) = command(&path, &["--check", "--json"]);
        assert_eq!(code, 2);
        assert!(stderr.contains("unit broken did not evaluate"), "{stderr}");
        let stages = json_lines(&stdout);
        assert_eq!(stages.len(), 2);
        assert_eq!(stages[0].get("status").and_then(Value::as_str), Some("error"));
        assert_eq!(contents(&project_dir), mixed);

        assert_eq!(command(&project_dir.join("missing.fab.splash"), &[]).0, 2);
        fs::write(&path, "place(\n").unwrap();
        let broken = contents(&project_dir);
        assert_eq!(command(&path, &[]).0, 2);
        assert_eq!(contents(&project_dir), broken);

        // A non-stock name forces the CAD script to use this plate's board during open.
        // Leaving object unspecified makes the project loader compile the script right there.
        fs::remove_file(docs.join("broken.ckt.splash")).unwrap();
        for ext in ["ckt", "pcb", "bench"] {
            let file = docs.join(format!("ledctl.{ext}.splash"));
            let source = fs::read_to_string(&file).unwrap().replace("ledctl", "panel5");
            fs::remove_file(file).unwrap();
            fs::write(docs.join(format!("panel5.{ext}.splash")), source).unwrap();
        }
        fs::write(&path, "use mod.fab.*\nscript(\"case.cad.splash\", name: \"case\")\nplace(\"case\")\n").unwrap();
        fs::write(project_dir.join("case.cad.splash"), "use mod.model.*\nuse mod.fab.*\nlet shell = enclosure(board(\"panel5\"), {})\nobject(shell.base, name: \"Base\")\n").unwrap();
        let cased = contents(&project_dir);
        let (engine, app) = evaluate(&path).unwrap();
        let snap = engine.snapshot();
        assert_eq!(snap.design.units[0].name, "panel5");
        assert!(snap.cases.iter().any(|case| case.cased && case.unit == snap.design.units[0].id));
        assert!(req_actions::app_facts(&snap, &app).cases.iter().any(|case| case.unit == "panel5" && case.has_case && case.current));
        assert!(fab_elec_host::edit_actions::checks_current(&snap) && snap.awaiting.is_empty());
        assert_eq!(contents(&project_dir), cased);
    }
}

#[cfg(test)]
#[test]
fn headless_journal_calls_keep_the_report_separate_and_stop_at_the_first_difference() {
    use crate::journal::{calls_fixture, replay::REPORT_FILE};
    use std::fs;
    let root = crate::journal::test_dir();
    let mut reference = None;
    for (folder, print, corrupt, reserved) in [
        ("plain", false, false, false), ("printing", true, false, false),
        ("different", false, true, false), ("collision", false, false, true),
    ] {
        let journal = calls_fixture(&root.join(folder), print, corrupt, reserved);
        let into = root.join(format!("copy-{folder}"));
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let code = run(vec!["--replay".into(), journal.clone().into(), "--into".into(), into.clone().into(), "--calls".into()],
            &mut stdout, &mut stderr).unwrap();
        let status = String::from_utf8(stdout).unwrap();
        if reserved {
            assert_ne!(code, 0);
            assert!(status.contains(REPORT_FILE));
            assert!(!into.exists());
            continue
        }
        let bytes = fs::read(into.join(REPORT_FILE)).unwrap();
        let json = makepad_strict_json::parse(&bytes).unwrap();
        assert_eq!(code, u8::from(corrupt), "fixture {folder}: {:?}", json.get("stop"));
        for text in [status.as_str(), std::str::from_utf8(&bytes).unwrap()] {
            for marker in ["private-argument-marker", "private-reply-marker", "private-plate-output", "let block ="] {
                assert!(!text.contains(marker));
            }
        }
        assert_eq!(status.lines().last(), Some(REPORT_FILE));
        assert!(stderr.is_empty());
        let steps = json.get("steps").unwrap().as_arr().unwrap();
        let outcome = |action| steps.iter().find(|s| s.get("action").and_then(Value::as_str) == Some(action))
            .and_then(|s| s.get("outcome")?.as_str());
        assert_eq!(outcome("fab.workspace_edit"), Some("not_replayable"));
        assert_eq!(outcome("fab.unknown_fixture_action"), Some("not_replayable"));
        assert_eq!(outcome("fab.inspect"), Some("matched"));
        if corrupt {
            let stop = json.get("stop").unwrap();
            assert_eq!(stop.get("action").and_then(Value::as_str), Some("fab.script"));
            assert_eq!(stop.get("outcome").and_then(Value::as_str), Some("mismatch"));
            assert_eq!(outcome("fab.params"), None);
            assert_eq!(fs::read_dir(&into).unwrap().count(), 1);
        } else {
            assert_eq!(outcome("fab.script"), Some("matched"));
            assert_eq!(outcome("fab.params"), Some("matched"));
            assert_eq!(fs::read(into.join("kept.txt")).unwrap(), b"after gesture");
            assert!(fs::read_to_string(into.join("block.cad.splash")).unwrap().contains("private-plate-output") == print);
            assert_eq!(json.get("manifest").and_then(Value::as_str), Some("piece.fab.splash"));
            assert_eq!(json.get("counts").unwrap().get("not_replayable").and_then(Value::as_u64), Some(2));
            if let Some(first) = &reference { assert_eq!(&bytes, first); } else { reference = Some(bytes); }
            if folder == "plain" {
                let mut records = fs::OpenOptions::new().append(true).open(journal.join("records.jsonl")).unwrap();
                writeln!(records, "damaged").unwrap();
                records.sync_data().unwrap();
                let stopped = root.join("state-gap");
                let code = run(vec!["--replay".into(), journal.into(), "--into".into(), stopped.clone().into()],
                    &mut Vec::new(), &mut Vec::new()).unwrap();
                assert_ne!(code, 0);
                let report = makepad_strict_json::parse(&fs::read(stopped.join(REPORT_FILE)).unwrap()).unwrap();
                assert_eq!(report.get("stop").and_then(|s| s.get("outcome")).and_then(Value::as_str), Some("gap"));
                assert_eq!(fs::read_dir(stopped).unwrap().count(), 1);
            }
        }
    }
    fs::remove_dir_all(root).unwrap();
}
