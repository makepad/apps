//! Byte-oriented KiCad board and project import, with separate parsing and construction.

pub mod dru;
pub mod json;
pub mod pcb;
pub mod pro;
pub mod report;
pub mod sexpr;
pub mod splash;

pub use dru::{parse_kicad_dru, KicadRuleSource};
pub use fab_elec_circuit::check::{CheckKind, Measure};
pub use pcb::*;
pub use pro::{parse_kicad_pro, Assignment, Pattern, ProjectClass, ProjectFile, CLASS_DEFAULTS, CLASS_FIELDS};
pub use report::{Gap, GapKind, ImportReport, Refusal, SourceFile, Unknown};

/// Sources supplied by a host; the reader never accesses the filesystem.
pub struct KicadSources<'a> {
    pub board: &'a [u8],
    pub project: Option<&'a [u8]>,
    pub rules: Option<&'a [u8]>,
    pub name: &'a str,
}

/// Read a board with optional companion files. Only a board refusal refuses the import.
pub fn read_kicad_board(sources: &KicadSources<'_>) -> Result<PcbRead, Refusal> {
    let start = std::time::Instant::now();
    let file = parse_kicad_pcb(sources.board)?;
    let project = sources.project.map(parse_kicad_pro).transpose();
    let parse_time = start.elapsed();
    let mut read = build_board(&file, project.as_ref().ok().and_then(Option::as_ref), sources.name);
    if let Err(refusal) = project {
        read.report.current = SourceFile::Project;
        read.report.gap_value("project.refused", GapKind::Dropped, (refusal.line, refusal.column), refusal.message);
    }
    let start = std::time::Instant::now();
    if let Some(bytes) = sources.rules {
        read.report.current = SourceFile::Rules;
        match parse_kicad_dru(bytes) {
            Ok(file) => read.rules = Some(file.append_to(&mut read.board, &mut read.report)),
            Err(error) => {
                dru::record_variables(bytes, &mut read.report);
                read.report.gap_value("rules.custom_unreadable", GapKind::Dropped,
                    (error.line, error.column), error.message);
            }
        }
    }
    read.parse_time = parse_time + start.elapsed();
    read.report.current = SourceFile::Board;
    Ok(read)
}

/// Read a board without a project or custom rules file.
pub fn read_kicad_pcb(bytes: &[u8], name: &str) -> Result<PcbRead, Refusal> {
    read_kicad_board(&KicadSources { board: bytes, project: None, rules: None, name })
}
