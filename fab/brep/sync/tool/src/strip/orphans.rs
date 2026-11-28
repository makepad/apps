use super::{qualify, Source, ROOT};
use crate::modules::Graph;
use crate::rules::{ancestors, contains, outer_ranges, parent_path, File};
use crate::table::Orphan;
use relax_lex::lexer::T;
use std::collections::{BTreeMap, BTreeSet};
use std::ops::Range;

fn within(module: &str, root: &str) -> bool {
    root.is_empty() || module == root || module.strip_prefix(root).is_some_and(|s| s.starts_with("::"))
}

struct Target<'a> {
    rule: &'a Orphan,
    file: &'a str,
    keyword: usize,
    span: Range<usize>,
    scope: String,
}

impl Orphan {
    fn label(&self) -> String {
        match &self.name {
            Some(name) => format!("O1 {} (name {name})", self.item),
            None => format!("O1 {}", self.item),
        }
    }
}

// Production items need not be in S1's test-item index. Look only at
// module item boundaries in files that can declare the configured path.
fn locate<'a>(rule: &'a Orphan, files: &BTreeMap<&'a str, File<'_>>, graph: &Graph,
    cfg_modules: &BTreeSet<String>) -> Result<Target<'a>, String>
{
    let module = parent_path(&rule.item);
    let mut found = None;
    for (&path, file) in files {
        if graph.deleted.contains(path) || !file.modules.iter().any(|m| within(module, m)) { continue; }
        let s = &file.source;
        for start in 0..s.toks.len() {
            if !s.module_scope(start) || !s.item_boundary(start) || s.in_macro(start) { continue; }
            let head = s.after_attrs(start);
            let keyword = s.item_head(head);
            let kind = s.kind(keyword);
            if !matches!(kind, T::KwUse | T::KwFn | T::KwStatic | T::KwStruct | T::KwConst) { continue; }
            if contains(&file.cuts, s.toks[keyword].lo as usize) { continue; }
            let local = s.item_path(keyword);
            if !file.modules.iter().any(|m| qualify(m, &local) == rule.item) { continue; }
            if found.is_some() || file.modules.len() != 1 {
                return Err(format!("{}: ambiguous item at {path}:{}", rule.label(), s.line(keyword)));
            }
            if rule.name.is_some() != (kind == T::KwUse) {
                return Err(format!("{}: only use items require a name", rule.label()));
            }
            let public = s.kind(head) == T::KwPub;
            let test = ancestors(module).any(|p| cfg_modules.contains(p)) ||
                file.scan.items.iter().any(|i| i.cfg && i.span.contains(&(s.toks[keyword].lo as usize)));
            if rule.production && public { return Err(format!("{}: production item must be private", rule.label())); }
            if !test && !rule.production { return Err(format!("{}: production item requires scope = \"production\"", rule.label())); }
            let scope = if !public { module } else {
                match (s.kind(head + 1), s.kind(head + 2), s.kind(head + 3)) {
                    (T::OpenParen, T::KwSuper, T::CloseParen) => parent_path(module),
                    (T::OpenParen, T::KwSelfValue, T::CloseParen) => module,
                    _ => "",
                }
            };
            let end = s.item(head, s.limit(start)).map_err(|e| format!("{}: {e}", rule.label()))?
                .ok_or_else(|| format!("{}: unrecognised item", rule.label()))?;
            found = Some(Target { rule, file: path, keyword, scope: scope.into(),
                span: s.toks[start].lo as usize..s.end(end) });
        }
    }
    found.ok_or_else(|| format!("{}: expected exactly 1 item, found 0", rule.label()))
}

// Return whether every leaf of this use tree goes. Only emit cuts at
// boundaries with surviving siblings, so nested groups and adjacent
// entries cannot leave an empty path or claim the same comma twice.
fn prune_use(s: &Source<'_>, tokens: Range<usize>, names: &mut BTreeMap<&str, usize>,
    cuts: &mut Vec<Range<usize>>) -> bool
{
    let mut i = tokens.start;
    while i < tokens.end && s.kind(i) != T::OpenBrace { i += 1; }
    if i == tokens.end {
        let mut leaf = s.word(i - 1);
        if leaf == "self" {
            let mut open = s.parent[i - 1];
            while open != ROOT {
                if open >= 2 && s.kind(open - 1) == T::PathSep { leaf = s.word(open - 2); break; }
                open = s.parent[open];
            }
        }
        if let Some(count) = names.get_mut(leaf) { *count += 1; return true; }
        return false;
    }
    let close = s.mate[i];
    let mut child = i + 1;
    let mut runs: Vec<Range<usize>> = Vec::new();
    let mut all = true;
    while child < close {
        let mut end = child;
        while end < close && s.kind(end) != T::Comma { end = s.after(end); }
        if prune_use(s, child..end, names, cuts) {
            if let Some(last) = runs.last_mut().filter(|r| r.end + 1 == child) {
                last.end = end;
            } else { runs.push(child..end); }
        } else { all = false; }
        child = end + 1;
    }
    if all { return true; }
    for run in runs {
        let (first, end) = if s.kind(run.end) == T::Comma {
            (run.start, run.end + 1)
        } else { (run.start - 1, run.end) };
        let mut hi = s.end(end);
        if s.kind(end - 1) == T::Comma && end < close {
            let next = s.toks[end].lo as usize;
            if s.src[hi..next].bytes().all(|b| matches!(b, b' ' | b'\t')) { hi = next; }
        }
        cuts.push(s.toks[first].lo as usize..hi);
    }
    false
}

fn can_name_module_item(s: &Source<'_>, i: usize, module_names: &BTreeSet<String>) -> bool {
    if i == 0 || s.kind(i - 1) != T::PathSep { return true; }
    if i < 2 { return false; }
    match s.kind(i - 2) {
        T::KwSelfValue | T::KwSuper | T::KwCrate => true,
        T::Ident | T::RawIdent => module_names.contains(s.word(i - 2)),
        _ => false,
    }
}

pub(crate) fn remove<'a>(rules: &[&'a Orphan], files: &mut BTreeMap<&'a str, File<'_>>,
    graph: &Graph, cfg_modules: &BTreeSet<String>, module_names: &BTreeSet<String>) -> Result<Vec<String>, String>
{
    let mut items = BTreeMap::new();
    let mut targets = Vec::new();
    for &rule in rules {
        let target = locate(rule, files, graph, cfg_modules)?;
        items.entry((target.file, target.keyword)).or_insert_with(Vec::new).push(targets.len());
        targets.push(target);
    }
    let mut changed = BTreeSet::new();
    for ((path, keyword), entries) in items {
        let file = files.get_mut(path).unwrap();
        let first = &targets[entries[0]];
        let mut whole = true;
        if file.source.kind(keyword) == T::KwUse {
            let mut names = BTreeMap::new();
            for i in entries {
                let entry = targets[i].rule;
                if names.insert(entry.name.as_deref().unwrap(), 0).is_some() {
                    return Err(format!("{}: duplicate entry", entry.label()));
                }
            }
            let s = &file.source;
            let semi = s.toks.partition_point(|t| (t.hi as usize) < first.span.end);
            whole = prune_use(s, keyword + 1..semi, &mut names, &mut file.cuts);
            for (name, count) in names {
                if count != 1 { return Err(format!("O1 {} (name {name}): expected exactly 1 name, found {count}", first.rule.item)); }
            }
        } else if entries.len() != 1 { return Err(format!("{}: duplicate entry", first.rule.label())); }
        if whole { file.cuts.push(first.span.clone()); }
        changed.insert(path.to_owned());
    }
    for path in &changed { outer_ranges(&mut files.get_mut(path.as_str()).unwrap().cuts)?; }

    for target in &targets {
        let rule = target.rule;
        let name = rule.name.as_deref().unwrap_or_else(|| rule.item.rsplit("::").next().unwrap());
        if name == "*" {
            let file = &files[target.file];
            let s = &file.source;
            let open = s.parent[target.keyword];
            let body = if open == ROOT { 0..s.src.len() } else { s.end(open + 1)..s.toks[s.mate[open]].lo as usize };
            let mut pos = body.start;
            for cut in file.cuts.iter().filter(|r| body.contains(&r.start)) {
                if !s.src[pos..cut.start].trim().is_empty() { break; }
                pos = cut.end;
            }
            if !s.src[pos..body.end].trim().is_empty() {
                return Err(format!("{}: stale glob, kept content in {}:{}", rule.label(), target.file, s.line(target.keyword)));
            }
            continue;
        }
        for (&path, file) in files.iter() {
            if graph.deleted.contains(path) || !file.modules.iter().any(|m| within(m, &target.scope) || within(&target.scope, m)) { continue; }
            let s = &file.source;
            for (i, tok) in s.toks.iter().enumerate() {
                if tok.kind.is_ident_like() && s.word(i) == name && !contains(&file.cuts, tok.lo as usize)
                    && can_name_module_item(s, i, module_names)
                    && file.modules.iter().any(|m| within(&qualify(m, &s.scope(i)), &target.scope))
                {
                    return Err(format!("{}: stale, kept identifier {name} at {path}:{}", rule.label(), s.line(i)));
                }
            }
        }
    }
    Ok(changed.into_iter().collect())
}
