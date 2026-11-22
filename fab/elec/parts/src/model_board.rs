//! Where each part model on this computer came from, and how this computer gets it (EKMODELS):
//! a small process-wide board the app writes as models arrive or fail and as the person's choice
//! changes, and the part cards and the AI read. A note is found by the model's project path
//! (`parts/kicad/3d/9/Package_SO.3dshapes/SOIC-8.glb`, `parts/lcsc/C2040.glb`) and by the mesh key a
//! design resolves that path to, so a part's body names its note whether its model is here or not.
//!
//! A choice is this computer's, per KiCad model: whether it gets that model from KiCad's library on
//! this computer, from its download, or either (local first). It changes nothing in the design;
//! which source a part's body uses at all (KiCad, EasyEDA or its plain body) is the design's, an
//! `e.body` statement. Setting a choice is the same call from the part card and from the AI
//! ([`choose`]), and refuses a source that is off in 3D Model Sources.
//!
//! Cost: one short lock per call (held for a map lookup or insert, never across I/O).

use crate::{Body3d, BodySource};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, Mutex};

/// How this computer gets one KiCad model.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Hash)]
pub enum Choice {
    /// The default: KiCad's library on this computer first, then its download.
    #[default]
    Auto,
    /// Only from KiCad's library on this computer.
    Local,
    /// Only KiCad's library download (at the board's library tag).
    Download,
}

impl Choice {
    pub const ALL: [Choice; 3] = [Choice::Auto, Choice::Local, Choice::Download];

    /// The word the choices file, the AI and the card use.
    pub fn key(self) -> &'static str {
        match self {
            Choice::Auto => "auto",
            Choice::Local => "local",
            Choice::Download => "download",
        }
    }

    pub fn of_key(key: &str) -> Option<Choice> {
        Choice::ALL.into_iter().find(|c| c.key() == key.trim())
    }
}

/// Which source a model is of: which choices apply to it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Family {
    /// A model of KiCad's library: auto, this computer, download.
    Kicad,
    /// A model of the EasyEDA part service: no choice of how (one service).
    EasyEda,
}

impl Family {
    /// The family of a project path, None for a model of the plate's own files.
    pub fn of_path(path: &str) -> Option<Family> {
        if path.starts_with("parts/kicad/3d/") {
            Some(Family::Kicad)
        } else if path.starts_with("parts/lcsc/") {
            Some(Family::EasyEda)
        } else {
            None
        }
    }

    pub fn choices(self) -> &'static [Choice] {
        match self {
            Family::Kicad => &Choice::ALL,
            Family::EasyEda => &[],
        }
    }
}

/// The 3D model sources that are on, as the person's 3D Model Sources setting says (the bits of
/// Fab's `ModelSource::bit`): the one place the workers, the cards and the actions read them. A
/// choice that needs a source that is off is refused.
static SOURCES: AtomicU8 = AtomicU8::new(SOURCE_LOCAL | SOURCE_DOWNLOAD);
/// Bumped by every change of a note, a choice or the sources: a card made from an older count
/// makes its text again.
static GENERATION: AtomicU64 = AtomicU64::new(1);

pub const SOURCE_LOCAL: u8 = 1;
pub const SOURCE_DOWNLOAD: u8 = 2;
pub const SOURCE_EASYEDA: u8 = 4;

pub fn set_sources(bits: u8) {
    if SOURCES.swap(bits, Ordering::AcqRel) != bits {
        GENERATION.fetch_add(1, Ordering::AcqRel);
    }
}

pub fn sources() -> u8 {
    SOURCES.load(Ordering::Acquire)
}

pub fn generation() -> u64 {
    GENERATION.load(Ordering::Acquire)
}

/// Whether `choice` of a `family` model can be used with the sources that are on now.
pub fn available(family: Family, choice: Choice) -> bool {
    let on = sources();
    match (family, choice) {
        (Family::Kicad, Choice::Auto) => on & (SOURCE_LOCAL | SOURCE_DOWNLOAD) != 0,
        (Family::Kicad, Choice::Local) => on & SOURCE_LOCAL != 0,
        (Family::Kicad, Choice::Download) => on & SOURCE_DOWNLOAD != 0,
        (Family::EasyEda, _) => false,
    }
}

/// One model's note: its project path and where its mesh came from, or why there is none.
#[derive(Clone, PartialEq, Debug)]
pub struct ModelNote {
    pub path: String,
    pub family: Family,
    /// Where it came from, short: "KiCad library 9.0.0", "KiCad 9 library on this computer",
    /// "EasyEDA part service", or "KiCad model not here".
    pub line: String,
    /// The rest: "from GitLab on 2026-11-19", the library folder, the part number,
    /// or why the model is not here.
    pub detail: String,
    /// Whether the model is here (its mesh drawn); else the part shows its plain body meanwhile.
    pub here: bool,
}

#[derive(Default)]
struct Board {
    /// Notes by project path and by mesh key.
    notes: HashMap<String, Arc<ModelNote>>,
    /// The person's choices by project path; absent: auto.
    choices: HashMap<String, Choice>,
}

static BOARD: Mutex<Option<Board>> = Mutex::new(None);

fn with<R>(f: impl FnOnce(&mut Board) -> R) -> R {
    let mut guard = BOARD.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    f(guard.get_or_insert_with(Board::default))
}

/// Keep `note` under its project path and under `key` (the mesh it resolved to), if any.
pub fn note(note: ModelNote, key: Option<&str>) {
    note_all(vec![(note, key.map(str::to_string))]);
}

/// Keep several notes under one lock; a note equal to the one kept announces nothing.
pub fn note_all(notes: Vec<(ModelNote, Option<String>)>) {
    let changed = with(|board| {
        let mut changed = false;
        for (note, key) in notes {
            if board.notes.get(&note.path).is_some_and(|old| **old == note)
                && key.as_ref().is_none_or(|key| board.notes.get(key).is_some_and(|old| **old == note))
            {
                continue;
            }
            let note = Arc::new(note);
            if let Some(key) = key {
                board.notes.insert(key, note.clone());
            }
            board.notes.insert(note.path.clone(), note);
            changed = true;
        }
        changed
    });
    if changed {
        GENERATION.fetch_add(1, Ordering::AcqRel);
    }
}

/// The note of a project path or a mesh key.
pub fn lookup(path_or_key: &str) -> Option<Arc<ModelNote>> {
    with(|board| board.notes.get(path_or_key).cloned())
}

/// This computer's choice for a model, by project path.
pub fn choice(path: &str) -> Choice {
    with(|board| board.choices.get(path).copied().unwrap_or_default())
}

/// Every choice that is not auto, by project path in path order (for the choices file).
pub fn choices() -> Vec<(String, Choice)> {
    let mut all: Vec<(String, Choice)> = with(|board| board.choices.iter().map(|(p, c)| (p.clone(), *c)).collect());
    all.sort_by(|a, b| a.0.cmp(&b.0));
    all
}

/// The choices file's choices, read at start: no change is announced.
pub fn load_choices(all: Vec<(String, Choice)>) {
    with(|board| board.choices = all.into_iter().filter(|(_, c)| *c != Choice::Auto).collect());
}

/// What a part's body is, as its card and `elec.part.model` say it.
#[derive(Clone, PartialEq, Debug)]
pub struct BodyModel {
    /// The project path of the model it names, when it names one.
    pub path: Option<String>,
    /// The model's source, when it is one of the part library's.
    pub family: Option<Family>,
    /// Short, then the rest (see [`ModelNote`]).
    pub line: String,
    pub detail: String,
    /// Whether the body drawn is that model (false: the plain body stands in).
    pub here: bool,
}

/// Describe `body`: its model's note when the board has one, else what can be said from the body
/// and the sources that are on. One lookup.
pub fn of_body(body: &Body3d) -> BodyModel {
    let plain = |line: &str, detail: String| BodyModel { path: None, family: None, line: line.into(), detail, here: true };
    let named = match (&body.missing_mesh, &body.source) {
        (Some(path), _) => path.as_str(),
        (None, BodySource::Mesh(mesh)) => mesh.as_str(),
        (None, BodySource::Extrude) => return plain("plain body", "the footprint's outline raised to its height".into()),
        (None, BodySource::Package(kind)) => return plain("generated body", format!("{kind:?} package, from the footprint")),
        (None, BodySource::Script(name)) => return plain("object script", name.clone()),
    };
    let here = body.missing_mesh.is_none();
    if let Some(note) = lookup(named) {
        return BodyModel { path: Some(note.path.clone()), family: Some(note.family), line: note.line.clone(),
            detail: note.detail.clone(), here: here && note.here };
    }
    let family = Family::of_path(named);
    let (line, detail) = match (family, here) {
        (None, true) => ("the design's own model".to_string(), file_name(named).to_string()),
        (None, false) => ("model not in the design".to_string(), format!("{named}: the plain body stands in")),
        (Some(Family::Kicad), _) if !available(Family::Kicad, Choice::Auto) =>
            ("KiCad model: sources off".to_string(), "plain body; 3D Model Sources turns KiCad's library on".to_string()),
        (Some(Family::Kicad), _) => ("KiCad model: being got".to_string(), file_name(named).to_string()),
        (Some(Family::EasyEda), _) if sources() & SOURCE_EASYEDA == 0 =>
            ("EasyEDA model: EasyEDA is off".to_string(), "plain body; 3D Model Sources turns it on".to_string()),
        (Some(Family::EasyEda), _) => ("EasyEDA model: being fetched".to_string(), file_name(named).to_string()),
    };
    BodyModel { path: Some(named.to_string()).filter(|_| family.is_some() || !here), family, line, detail, here }
}

fn file_name(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().unwrap_or(path)
}

/// Why a choice was refused.
#[derive(Clone, PartialEq, Debug)]
pub enum Refused {
    /// No part model of a known source at that path.
    NotAModel(String),
    /// The choice does not apply to that model's source.
    NotOffered { path: String, choice: Choice },
    /// Its source is off in 3D Model Sources; only the person turns it on there.
    SourceOff { path: String, choice: Choice },
}

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Refused::NotAModel(path) => write!(f, "{path} is not a model of KiCad's library or the EasyEDA part service"),
            Refused::NotOffered { path, choice } => write!(f, "{path}: {} does not apply to this model", choice.key()),
            Refused::SourceOff { path, choice } => write!(f,
                "{path}: {} needs a source that is off in 3D Model Sources; only the person can turn it on there", choice.key()),
        }
    }
}

/// Set this computer's choice for a KiCad model, as the part card's switch and the AI's
/// `elec.part.model.get_from` do: refused for a choice that does not apply to the model or whose
/// source is off. The app reads the change ([`generation`]), keeps it in the choices file and asks
/// for the model again.
pub fn choose(path: &str, choice: Choice) -> Result<(), Refused> {
    let family = Family::of_path(path).ok_or_else(|| Refused::NotAModel(path.to_string()))?;
    if !family.choices().contains(&choice) {
        return Err(Refused::NotOffered { path: path.to_string(), choice });
    }
    if !available(family, choice) {
        return Err(Refused::SourceOff { path: path.to_string(), choice });
    }
    let changed = with(|board| {
        let old = board.choices.get(path).copied().unwrap_or_default();
        if choice == Choice::Auto { board.choices.remove(path); } else { board.choices.insert(path.to_string(), choice); }
        old != choice
    });
    if changed {
        GENERATION.fetch_add(1, Ordering::AcqRel);
        CHOICES_CHANGED.fetch_add(1, Ordering::AcqRel);
    }
    Ok(())
}

/// Bumped by [`choose`] only: the app compares it to keep the file and ask again.
static CHOICES_CHANGED: AtomicU64 = AtomicU64::new(0);

pub fn choices_changed() -> u64 {
    CHOICES_CHANGED.load(Ordering::Acquire)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn choices_apply_to_their_family_and_need_their_source() {
        // The process-wide board: this test uses paths of its own.
        let kicad = "parts/kicad/3d/9/Test_Board.3dshapes/CHOICE.glb";
        let lcsc = "parts/lcsc/C99999999.glb";
        set_sources(SOURCE_LOCAL | SOURCE_DOWNLOAD);
        assert_eq!(choice(kicad), Choice::Auto, "auto until chosen");
        let before = (generation(), choices_changed());
        choose(kicad, Choice::Download).expect("download is on");
        assert_eq!(choice(kicad), Choice::Download);
        assert!(generation() > before.0 && choices_changed() > before.1, "a change is announced");
        assert!(choices().contains(&(kicad.to_string(), Choice::Download)), "and kept for the file");
        set_sources(SOURCE_LOCAL);
        assert_eq!(choose(kicad, Choice::Download), Err(Refused::SourceOff { path: kicad.into(), choice: Choice::Download }),
            "a source that is off cannot be chosen");
        set_sources(SOURCE_LOCAL | SOURCE_DOWNLOAD);
        assert_eq!(choose(lcsc, Choice::Local), Err(Refused::NotOffered { path: lcsc.into(), choice: Choice::Local }),
            "an EasyEDA model has no choice of how");
        assert!(matches!(choose("parts/other/x.glb", Choice::Auto), Err(Refused::NotAModel(_))));
        choose(kicad, Choice::Auto).expect("back to auto");
        assert!(!choices().iter().any(|(p, _)| p == kicad), "auto is not kept");
        let body = |source: BodySource, missing: Option<&str>| Body3d {
            source, offset: fab_elec_core::Xform3::IDENTITY, height: 0, ports: vec![], surface: None,
            missing_mesh: missing.map(str::to_string),
        };
        let waiting = of_body(&body(BodySource::Extrude, Some(kicad)));
        assert!(!waiting.here && waiting.line.contains("being got") && waiting.path.as_deref() == Some(kicad), "{waiting:?}");
        note(ModelNote { path: kicad.into(), family: Family::Kicad, line: "KiCad 9 library on this computer".into(),
            detail: "/kicad/3dmodels".into(), here: true }, Some("/cache/x.glb"));
        assert_eq!(lookup("/cache/x.glb").map(|n| n.path.clone()), Some(kicad.to_string()), "a note is found by its mesh key");
        assert_eq!(lookup(kicad).map(|n| n.here), Some(true), "and by its path");
        let arrived = of_body(&body(BodySource::Mesh("/cache/x.glb".into()), None));
        assert!(arrived.here && arrived.path.as_deref() == Some(kicad) && arrived.line == "KiCad 9 library on this computer", "{arrived:?}");
        let generation_before = generation();
        note_all(vec![(lookup(kicad).map(|n| (*n).clone()).expect("kept"), Some("/cache/x.glb".into()))]);
        assert_eq!(generation(), generation_before, "the same note again announces nothing");
        let easyeda = of_body(&body(BodySource::Extrude, Some(lcsc)));
        assert!(easyeda.line.contains("EasyEDA is off"), "{easyeda:?}");
        assert_eq!(of_body(&body(BodySource::Extrude, None)).line, "plain body");
        assert_eq!(Choice::of_key(" download "), Some(Choice::Download));
    }
}
