//! A board's fabrication files written from the app: `elec.export` (the board strip's Export
//! button, and the AI's `elec_export` call once the person approves it) and `elec.export.validate`.
//!
//! The files come from `fab_elec_cam` (plot through a factory profile, read every file back,
//! compare it with the board) and go to the project's `fabrication/<board>/` folder, or the Fab
//! documents folder's for a plate never saved. Everything here runs on the pool: plotting is
//! O(board items x layers), the read-back O(n log n) in the vertices, and the writes are the
//! files' bytes. The UI thread only gathers the inputs (Arc clones) and reads the answer.

use fab_ai::out;
use fab_elec_board::ZoneFill;
use fab_elec_cam::tables::{fill_generic_tables, PlacementRules};
use fab_elec_cam::{Check, OutFile, ResolvedProfile};
use fab_elec_core::BoardId;
use fab_elec_design::Design;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// The revision the files are named with.
const REV: &str = "A";

/// The folder exports go under: `fabrication` beside the saved project, else in the Fab documents
/// folder.
pub fn root(project: Option<&Path>) -> PathBuf {
    project.and_then(Path::parent).map_or_else(|| crate::model_host::documents_dir().join("fabrication"), |dir| dir.join("fabrication"))
}

/// A name as a file or folder name: anything but letters, digits, `-` and `_` turned into `-`.
fn file_name(name: &str) -> String {
    let stem: String = name.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '-' }).collect();
    let stem = stem.trim_matches('-');
    if stem.is_empty() { "board".into() } else { stem.into() }
}

/// What an export made, kept for `elec.export.validate` without a folder.
#[derive(Clone, Debug)]
pub struct LastExport {
    pub unit: BoardId,
    pub profile: String,
    pub folder: PathBuf,
}

/// A finished export or validation: the call's answer, any notice for the board, and the export to keep.
pub struct Exported {
    pub answer: String,
    pub notice: Option<String>,
    pub last: Option<LastExport>,
}

/// One export's inputs, gathered on the UI from the committed snapshot.
pub struct ExportJob {
    pub design: Arc<Design>,
    pub fills: Arc<Vec<ZoneFill>>,
    pub unit: BoardId,
    pub profile: String,
    pub folder: PathBuf,
}

/// The profile `name` for the unit `unit_name`, its tables filled from the board. O(parts).
fn profile_for(design: &Design, unit: BoardId, name: &str) -> Result<ResolvedProfile, String> {
    let unit = design.unit(unit).ok_or("the board is gone")?;
    let board = unit.board.as_ref().ok_or("the unit has no board")?;
    let mut profile = ResolvedProfile::builtin(name, &file_name(&unit.name), REV)
        .ok_or_else(|| format!("profile: no factory profile {name}; the built-in ones are {}", fab_elec_cam::PROFILES.join(", ")))?;
    let rules = if profile.name == fab_elec_cam::PROFILES[1] { PlacementRules::jlc() } else { PlacementRules::default() };
    fill_generic_tables(&mut profile, board, &unit.circuit, &rules);
    Ok(profile)
}

fn mm(nm: i32) -> out::S {
    out::mm(f64::from(nm) / 1e6)
}

fn failed(checks: &[&Check]) -> out::S {
    out::arr(checks.iter().map(|check| {
        let mut fields = vec![
            ("file", out::s(check.file.clone())),
            ("check", out::s(format!("{:?}", check.kind).to_lowercase())),
            ("cause", out::s(check.cause.clone())),
            ("hint", out::s(check.hint.clone())),
        ];
        if let Some(at) = check.at {
            fields.push(("at", out::arr([mm(at.x), mm(at.y)])));
        }
        out::line(out::obj(fields))
    }))
}

/// The count and first failure for the board notice. A clean export needs no notice.
fn verdict(failures: &[&Check]) -> Option<String> {
    let first = failures.first()?;
    Some(format!("{} check{} failed, first {}: {}", failures.len(), if failures.len() == 1 { "" } else { "s" }, first.file, first.cause))
}

/// Plot, write and check the board's files. On the pool.
pub fn export(job: ExportJob) -> Result<Exported, String> {
    let unit = job.design.unit(job.unit).ok_or("the board is gone")?;
    let board = unit.board.as_ref().ok_or("the unit has no board")?;
    let profile = profile_for(&job.design, job.unit, &job.profile)?;
    let made = fab_elec_cam::export(board, &unit.circuit, &job.fills, &profile).map_err(|e| format!("the files could not be plotted: {e}"))?;
    std::fs::create_dir_all(&job.folder).map_err(|e| format!("{}: {e}", job.folder.display()))?;
    let written: Vec<&OutFile> = made.files.iter().chain(made.zip.iter()).collect();
    for file in &written {
        let path = job.folder.join(&file.name);
        std::fs::write(&path, &file.bytes).map_err(|e| format!("{}: {e}", path.display()))?;
    }
    let failures: Vec<&Check> = made.validation.failures().collect();
    let folder = job.folder.display().to_string();
    let answer = out::text(&out::obj(vec![
        ("folder", out::s(folder.clone())),
        ("files", out::arr(made.files.iter().map(|f| out::s(f.name.clone())))),
        ("zip", out::opt(made.zip.as_ref().map(|zip| out::s(zip.name.clone())))),
        ("profile", out::s(profile.name.clone())),
        ("validation", out::obj(vec![
            ("valid", out::flag(failures.is_empty())),
            ("passed", out::int(made.validation.checks.len() - failures.len())),
            ("failed", failed(&failures)),
        ])),
    ]));
    let notice = verdict(&failures).map(|why| format!("Wrote {} board files to {folder}: {why}", written.len()));
    Ok(Exported { answer, notice, last: Some(LastExport { unit: job.unit, profile: profile.name, folder: job.folder }) })
}

/// Read the profile's files back from `last.folder` and check them against the board as it is
/// now. On the pool. A file the folder lacks is a failed check, as the validator reports it.
pub fn validate(design: &Design, fills: &[ZoneFill], last: &LastExport) -> Result<Exported, String> {
    let unit = design.unit(last.unit).ok_or("the board is gone")?;
    let board = unit.board.as_ref().ok_or("the unit has no board")?;
    let profile = profile_for(design, last.unit, &last.profile)?;
    let entries = std::fs::read_dir(&last.folder).map_err(|e| format!("folder: {}: {e}", last.folder.display()))?;
    let mut files = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if profile.files.iter().any(|spec| spec.name == name) {
            let bytes = std::fs::read(entry.path()).map_err(|e| format!("{name}: {e}"))?;
            files.push(OutFile { name, bytes });
        }
    }
    let validation = fab_elec_cam::validate_export(board, &unit.circuit, fills, &profile, &files);
    let failures: Vec<&Check> = validation.failures().collect();
    let folder = last.folder.display().to_string();
    let answer = out::text(&out::obj(vec![
        ("folder", out::s(folder.clone())),
        ("profile", out::s(profile.name.clone())),
        ("valid", out::flag(failures.is_empty())),
        ("passed", out::int(validation.checks.len() - failures.len())),
        ("failed", failed(&failures)),
    ]));
    let notice = verdict(&failures).map(|why| format!("Checked the board files in {folder}: {why}"));
    Ok(Exported { answer, notice, last: None })
}

/// A folder named relative to the fabrication root: no absolute path, drive or `..`.
pub fn relative_folder(root: &Path, text: &str) -> Result<PathBuf, String> {
    let path = Path::new(text.trim());
    let drive = matches!(text.trim().as_bytes(), [b'A'..=b'Z' | b'a'..=b'z', b':', ..]);
    if text.trim().is_empty() || path.has_root() || drive || text.starts_with('\\') {
        return Err("folder: a folder relative to the project's fabrication folder, such as the board's name".into());
    }
    if text.split(['/', '\\']).any(|part| part == "..") {
        return Err("folder: a fabrication folder cannot contain ..".into());
    }
    Ok(root.join(path))
}

/// The folder an export of `unit_name` writes.
pub fn folder_of(root: &Path, unit_name: &str) -> PathBuf {
    root.join(file_name(unit_name))
}
