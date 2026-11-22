//! Chip-neutral firmware diagnostics and their source locations.

use crate::{CompileError, Compiled, Owner};
use std::ops::Range;

/// Why a document does not build.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diagnostic {
    /// Byte span in the document; None where the cause is not in it (the clock, or a constant the
    /// document does not declare).
    pub span: Option<Range<usize>>,
    /// The bench constant it is about.
    pub param: Option<String>,
    pub message: String,
}

impl Diagnostic {
    pub fn new(span: Option<Range<usize>>, message: impl Into<String>) -> Diagnostic {
        Diagnostic { span, param: None, message: message.into() }
    }
}

impl CompileError {
    /// Attribute source and bench-constant errors, retaining only spans inside the document.
    pub fn diagnostics(self, source_len: usize) -> Vec<Diagnostic> {
        let inside = |start: usize, end: usize| (start <= end && end <= source_len).then_some(start..end);
        match self {
            Self::Source(errors) => errors.into_iter().map(|e| Diagnostic::new(inside(e.start, e.end), e.message)).collect(),
            Self::Consts(errors) => errors.into_iter().map(|e| Diagnostic {
                span: e.at.and_then(|(a, b)| inside(a as usize, b as usize)), param: Some(e.name), message: e.message,
            }).collect(),
        }
    }
}

impl Compiled {
    /// The entry's last statement starting at or before the module code position, else the
    /// entry itself, provided its span is inside the source.
    pub fn code_span(&self, entry: usize, code: u32, source_len: usize) -> Option<Range<usize>> {
        let handler = self.lowered.entries.get(entry)?;
        let stmt = self.marks.iter().filter(|&&(at, id)| at <= code && self.stmts[id as usize].owner == Owner::Handler(entry as u32)).last()
            .map(|&(_, id)| (self.stmts[id as usize].site.start as usize, self.stmts[id as usize].site.end as usize));
        let (start, end) = stmt.unwrap_or((handler.span.start, handler.span.end));
        (start <= end && end <= source_len).then_some(start..end)
    }
}
