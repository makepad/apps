use crate::strip::{apply, Source};
use crate::modules::{collect, drop_module, inspect_graph, Graph};
use crate::table::{Outcome, Rule, RuleKind, Tree};
use crate::SyncError;
use std::collections::{BTreeMap, BTreeSet};

// Read just the two edition spellings used by Cargo in these crates.
// In particular a crate's explicit edition wins over workspace.package.
fn edition_field(tree: &Tree, path: &str, section: &str) -> Result<Option<u16>, SyncError> {
    let bytes = tree.get(path).ok_or_else(|| format!("missing {path}"))?;
    let src = std::str::from_utf8(bytes).map_err(|e| format!("{path}: {e}"))?;
    let mut active = false;
    let mut value = None;
    for (idx, line) in src.lines().enumerate() {
        let line = line.split('#').next().unwrap_or("").trim();
        if line.starts_with('[') { active = line == section; continue; }
        if !active { continue; }
        let Some((key, rhs)) = line.split_once('=') else { continue; };
        let rhs = rhs.trim();
        let field = match key.trim() {
            "edition.workspace" if section == "[package]" && rhs == "true" => None,
            "edition" if matches!(rhs, "\"2015\"" | "\"2018\"" | "\"2021\"" | "\"2024\"") => {
                Some(rhs[1..5].parse().unwrap())
            }
            "edition" | "edition.workspace" => return Err(format!("{path}:{}: unsupported edition {rhs}", idx + 1)),
            _ => continue,
        };
        if value.replace(field).is_some() { return Err(format!("{path}: duplicate edition")); }
    }
    value.ok_or_else(|| format!("{path}: missing edition in {section}"))
}

pub(crate) fn edition(tree: &Tree, krate: &str) -> Result<u16, SyncError> {
    if let Some(edition) = edition_field(tree, &format!("{krate}/Cargo.toml"), "[package]")? {
        return Ok(edition);
    }
    edition_field(tree, "solidcore/Cargo.toml", "[workspace.package]")?.ok_or("missing workspace edition".into())
}

pub struct Prepared {
    pub outcomes: Vec<Outcome>,
    pub graphs: BTreeMap<&'static str, Graph>,
    pub remaining_test_cfg: usize,
    replacements: Tree,
}

impl Prepared {
    pub fn apply_to(&mut self, tree: &mut Tree, blobs: &BTreeSet<String>) -> Result<(), SyncError> {
        for graph in self.graphs.values() { graph.check_external(blobs)?; }
        tree.append(&mut self.replacements);
        for graph in self.graphs.values() {
            for path in &graph.deleted { tree.remove(path); }
        }
        Ok(())
    }
}

pub fn prepare(tree: &Tree, rules: &[Rule]) -> Result<Prepared, SyncError> {
    let mut result = Prepared { outcomes: Vec::new(), graphs: BTreeMap::new(), replacements: Tree::new(), remaining_test_cfg: 0 };
    for rule in rules {
        result.outcomes.push(Outcome { rule: rule.id, crate_: rule.crate_, count: 0, files: Vec::new() });
    }
    let crates: BTreeSet<_> = rules.iter().map(|r| r.crate_).collect();
    for krate in crates {
        let edition = edition(tree, krate)?;
        let prefix = format!("{krate}/");
        let mut errors = Vec::new();
        let mut drops = Vec::new();
        let (mut strip, mut modules) = (None, None);
        for (idx, rule) in rules.iter().enumerate().filter(|(_, r)| r.crate_ == krate) {
            match rule.kind {
                RuleKind::DropModule { file, line } => drops.push((format!("{prefix}{file}"), line, idx)),
                RuleKind::StripCfgTest if strip.is_none() => strip = Some(idx),
                RuleKind::ModuleGraph if modules.is_none() => modules = Some(idx),
                _ => return Err(format!("{}: unsupported or duplicate rule", rule.id)),
            }
        }
        let mut decls = Vec::new();
        for (path, bytes) in tree.iter().filter(|(p, _)| p.starts_with(&prefix) && p.ends_with(".rs")) {
            let source = match Source::parse(path, bytes, edition) {
                Ok(source) => source,
                Err(e) => { errors.push(e); continue; }
            };
            let mut cuts = Vec::new();
            for (_, line, idx) in drops.iter().filter(|(file, _, _)| file == path) {
                let ranges = drop_module(&source, line)?;
                if !ranges.is_empty() {
                    result.outcomes[*idx].count += ranges.len();
                    result.outcomes[*idx].files.push(path.clone());
                    cuts.extend(ranges);
                }
            }
            if let Some(idx) = strip {
                let edits = source.edits();
                result.remaining_test_cfg += edits.remaining;
                if edits.count > 0 {
                    result.outcomes[idx].count += edits.count;
                    result.outcomes[idx].files.push(path.clone());
                }
                if !edits.errors.is_empty() { errors.extend(edits.errors); continue; }
                cuts.extend(edits.ranges);
            }
            cuts.sort_by_key(|r| r.start);
            if cuts.windows(2).any(|w| w[0].end > w[1].start) {
                return Err(format!("D1/S1: overlapping rule edits in {path}"));
            }
            if modules.is_some() {
                match collect(&source, krate, &cuts) {
                    Ok(refs) => decls.extend(refs),
                    Err(e) => errors.push(e),
                }
            }
            if !cuts.is_empty() {
                result.replacements.insert(path.clone(), apply(bytes, &cuts));
            }
        }
        if !errors.is_empty() {
            let removed = strip.map_or(0, |idx| result.outcomes[idx].count);
            return Err(format!("{krate}: S1 {removed} proven removals, {} refusals\n{}", errors.len(), errors.join("\n")));
        }
        for (idx, rule) in rules.iter().enumerate().filter(|(_, r)| r.crate_ == krate && !matches!(r.kind, RuleKind::ModuleGraph)) {
            rule.expect.check(rule.id, result.outcomes[idx].count)?;
        }
        if let Some(idx) = modules {
            let graph = inspect_graph(tree, krate, &decls)?;
            result.outcomes[idx].count = graph.deleted.len();
            result.outcomes[idx].files.extend(graph.deleted.iter().cloned());
            rules[idx].expect.check(rules[idx].id, result.outcomes[idx].count)?;
            result.graphs.insert(krate, graph);
        }
    }
    Ok(result)
}

pub fn run_rules(tree: &mut Tree, rules: &[Rule]) -> Result<Vec<Outcome>, SyncError> {
    let mut prepared = prepare(tree, rules)?;
    prepared.apply_to(tree, &BTreeSet::new())?;
    Ok(prepared.outcomes)
}
