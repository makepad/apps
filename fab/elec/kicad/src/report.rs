//! Located failures and aggregated, deterministic plain-text import diagnostics.

use std::collections::HashMap;
use std::fmt::{self, Write};

/// A fatal lexical, value or board-structure error at a byte position.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Refusal {
    pub line: u32,
    pub column: u32,
    pub message: String,
}

impl Refusal {
    pub fn new(at: (u32, u32), message: impl Into<String>) -> Self {
        Self { line: at.0, column: at.1, message: message.into() }
    }
}
impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "line {}:{}: {}", self.line, self.column, self.message)
    }
}
impl std::error::Error for Refusal {}

/// How a feature differs from the source document.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum GapKind { Skipped, Approximated, Dropped }

/// Source order also decides which location is reported first.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub enum SourceFile { #[default] Board, Project, Rules }

impl SourceFile {
    fn label(self) -> &'static str {
        match self { Self::Board => "", Self::Project => "project ", Self::Rules => "rules " }
    }
}

/// Aggregated occurrences of one unsupported or approximated feature.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Gap {
    pub feature: String,
    pub kind: GapKind,
    pub count: u32,
    pub first: (SourceFile, u32, u32),
    pub values: Vec<(String, u32)>,
}

/// An unrecognised keyword path and the first place it occurred.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Unknown {
    pub count: u32,
    pub first: (SourceFile, u32, u32),
}

/// Provenance, source and result counts, and losses accumulated by parse and build.
#[derive(Clone, Debug, Default)]
pub struct ImportReport {
    pub current: SourceFile,
    pub version: i64,
    pub generator: Option<String>,
    pub generator_version: Option<String>,
    pub too_recent: bool,
    pub read: HashMap<String, u64>,
    pub made: HashMap<String, u64>,
    pub gaps: HashMap<String, Gap>,
    pub unknown: HashMap<String, Unknown>,
    value_indices: HashMap<String, HashMap<String, usize>>,
}

impl ImportReport {
    pub fn count_read(&mut self, kind: &str) {
        if let Some(count) = self.read.get_mut(kind) { *count = count.saturating_add(1); }
        else { self.read.insert(kind.into(), 1); }
    }
    pub fn count_made(&mut self, kind: &str) {
        if let Some(count) = self.made.get_mut(kind) { *count = count.saturating_add(1); }
        else { self.made.insert(kind.into(), 1); }
    }
    pub fn gap(&mut self, feature: &str, kind: GapKind, at: (u32, u32)) {
        let at = (self.current, at.0, at.1);
        let entry = self.gaps.entry(feature.into()).or_insert_with(|| Gap {
            feature: feature.into(), kind, count: 0, first: at, values: Vec::new(),
        });
        entry.count = entry.count.saturating_add(1);
        entry.first = entry.first.min(at);
    }
    pub fn gap_value(&mut self, feature: &str, kind: GapKind, at: (u32, u32), value: String) {
        self.gap(feature, kind, at);
        let indices = self.value_indices.entry(feature.into()).or_default();
        if let Some(gap) = self.gaps.get_mut(feature) {
            if let Some(&i) = indices.get(&value) {
                if let Some((_, count)) = gap.values.get_mut(i) { *count = count.saturating_add(1); }
            } else {
                indices.insert(value.clone(), gap.values.len());
                gap.values.push((value, 1));
            }
        }
    }
    pub fn unknown(&mut self, path: String, at: (u32, u32)) {
        let at = (self.current, at.0, at.1);
        let entry = self.unknown.entry(path).or_insert(Unknown { count: 0, first: at });
        entry.count = entry.count.saturating_add(1);
        entry.first = entry.first.min(at);
    }
    /// Merge diagnostics without replacing this report's board provenance or current source.
    pub fn merge(&mut self, other: &ImportReport) {
        for (dest, source) in [(&mut self.read, &other.read), (&mut self.made, &other.made)] {
            for (key, count) in source {
                let value = dest.entry(key.clone()).or_default();
                *value = value.saturating_add(*count);
            }
        }
        for (feature, source) in &other.gaps {
            let gap = self.gaps.entry(feature.clone()).or_insert_with(|| Gap {
                feature: feature.clone(), kind: source.kind, count: 0, first: source.first, values: Vec::new(),
            });
            gap.count = gap.count.saturating_add(source.count);
            gap.first = gap.first.min(source.first);
            let indices = self.value_indices.entry(feature.clone()).or_default();
            for (value, count) in &source.values {
                if let Some(&i) = indices.get(value) {
                    gap.values[i].1 = gap.values[i].1.saturating_add(*count);
                } else {
                    indices.insert(value.clone(), gap.values.len());
                    gap.values.push((value.clone(), *count));
                }
            }
        }
        for (path, source) in &other.unknown {
            let entry = self.unknown.entry(path.clone()).or_insert(Unknown { count: 0, first: source.first });
            entry.count = entry.count.saturating_add(source.count);
            entry.first = entry.first.min(source.first);
        }
    }
    pub fn render(&self) -> String {
        let mut out = format!("version {}{}\n", self.version, if self.too_recent { " (newer than reader)" } else { "" });
        if let Some(s) = &self.generator { let _ = writeln!(out, "generator {s}"); }
        if let Some(s) = &self.generator_version { let _ = writeln!(out, "generator_version {s}"); }
        for (label, counts) in [("read", &self.read), ("made", &self.made)] {
            let mut counts: Vec<_> = counts.iter().collect();
            counts.sort_by(|a, b| a.0.cmp(b.0));
            for (kind, count) in counts { let _ = writeln!(out, "{label} {kind}: {count}"); }
        }
        let mut gaps: Vec<_> = self.gaps.values().collect();
        gaps.sort_by(|a, b| b.count.cmp(&a.count).then(a.feature.cmp(&b.feature)));
        for gap in gaps {
            let _ = write!(out, "{:?} {}: {} at {}{}:{}", gap.kind, gap.feature, gap.count, gap.first.0.label(), gap.first.1, gap.first.2);
            for (value, count) in &gap.values { let _ = write!(out, " {value:?}={count}"); }
            out.push('\n');
        }
        let mut unknown: Vec<_> = self.unknown.iter().collect();
        unknown.sort_by(|a, b| b.1.count.cmp(&a.1.count).then(a.0.cmp(b.0)));
        for (path, value) in unknown {
            let _ = writeln!(out, "Unknown {path}: {} at {}{}:{}", value.count, value.first.0.label(), value.first.1, value.first.2);
        }
        out
    }
}
impl fmt::Display for ImportReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str(&self.render()) }
}
