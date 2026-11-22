//! Severities and located diagnostics shared by checks, model consistency and the language.

use crate::sel::Sel;

/// How bad a finding is. Ordered: `Error` is the greatest.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug, PartialOrd, Ord, Default)]
pub enum Severity {
    Info,
    #[default]
    Warning,
    Error,
}

crate::canon_tags!(Severity);

/// A finding about the model, located by what it is about. The language adds source ranges
/// through its `SourceMap` (C5); checks use `Violation` (`fab_elec_circuit::check`).
#[derive(Clone, PartialEq, Debug)]
pub struct Diag {
    pub severity: Severity,
    pub message: String,
    pub sel: Vec<Sel>,
}

impl Diag {
    pub fn error(message: impl Into<String>, sel: Vec<Sel>) -> Diag {
        Diag { severity: Severity::Error, message: message.into(), sel }
    }
    pub fn warning(message: impl Into<String>, sel: Vec<Sel>) -> Diag {
        Diag { severity: Severity::Warning, message: message.into(), sel }
    }
}
