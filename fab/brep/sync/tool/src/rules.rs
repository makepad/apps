use crate::strip::{apply, qualify, Scan, Source};
use crate::modules::{collect, drop_module, inspect_graph, module_paths, Graph};
use crate::table::{at_least, CrateSpec, Outcome, Table, Tree};
use crate::SyncError;
use std::collections::{BTreeMap, BTreeSet, HashSet};

use crate::manifest::edition;
use std::ops::Range;

#[derive(Clone, Debug)]
pub struct Skip {
    pub krate: String,
    pub item: String,
    pub kind: String,
    pub reason: String,
    pub line: usize,
}

pub fn parse_skips(input: &str) -> Result<Vec<Skip>, String> {
    let mut skips = Vec::new();
    let mut seen = BTreeSet::new();
    for (i, text) in input.lines().enumerate() {
        let text = text.trim();
        if text.is_empty() || text.starts_with('#') { continue; }
        let mut words = text.split_whitespace();
        let (Some(krate), Some(item), Some(kind), Some(first)) =
            (words.next(), words.next(), words.next(), words.next()) else {
            return Err(format!("skip.txt:{}: expected crate, item, kind and reason", i + 1));
        };
        if !matches!(kind, "slow" | "rustc" | "platform" | "relax-bug" | "module" | "feature")
            || !seen.insert((krate, item)) { return Err(format!("skip.txt:{}: unknown kind or duplicate item", i + 1)); }
        let reason = std::iter::once(first).chain(words).collect::<Vec<_>>().join(" ");
        skips.push(Skip { krate: krate.into(), item: item.into(), kind: kind.into(), reason, line: i + 1 });
    }
    Ok(skips)
}

#[derive(Default)]
pub struct Summary {
    pub kinds: BTreeMap<String, usize>,
    pub denied_tests: usize,
    pub kept_tests: usize,
    pub removed_tests: usize,
    pub losses: BTreeMap<String, usize>,
    pub whole_modules: BTreeMap<String, String>,
    pub named_removals: Vec<String>,
    pub ignored: Vec<String>,
    pub skips: Vec<String>,
    pub statements: usize,
    pub sites: Vec<String>,
    pub inventory: crate::inventory::Inventory,
    pub production_denied: Vec<String>,
}

pub struct Prepared {
    pub outcomes: Vec<Outcome>,
    pub graphs: BTreeMap<String, Graph>,
    pub summaries: BTreeMap<String, Summary>,
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

pub(crate) struct File<'a> {
    pub source: Source<'a>,
    pub scan: Scan,
    pub modules: Vec<String>,
    pub cuts: Vec<Range<usize>>,
}

pub(crate) fn contains(cuts: &[Range<usize>], pos: usize) -> bool {
    cuts.get(cuts.partition_point(|r| r.end <= pos)).is_some_and(|r| r.contains(&pos))
}

pub(crate) fn ancestors(path: &str) -> impl Iterator<Item = &str> {
    std::iter::successors(Some(path), |s| s.rsplit_once("::").map(|(parent, _)| parent))
}

pub(crate) fn parent_path(path: &str) -> &str {
    path.rsplit_once("::").map_or("", |(p, _)| p)
}

// Keep the qualified removal that justifies each missing local name. A
// grouped use can bind several names and nested test modules often import
// their parent's bindings with "use super::*".
fn removed_bindings(files: &BTreeMap<&str, File<'_>>, removed: &BTreeSet<String>,
    graph: &Graph) -> BTreeMap<String, BTreeSet<String>>
{
    let mut pending: Vec<_> = removed.iter().map(|r| (r.clone(), r.clone())).collect();
    let mut globs = BTreeMap::<String, BTreeSet<String>>::new();
    for (&path, file) in files {
        for item in &file.scan.items {
            for m in &file.modules {
                let key = qualify(m, &item.path);
                let whole = ancestors(&key).skip(1).find(|p| removed.contains(*p));
                if let Some(parent) = whole {
                    pending.push((key.clone(), parent.to_owned()));
                }
                if file.source.kind(item.keyword) != relax_lex::lexer::T::KwUse { continue; }
                let (names, sources) = file.source.use_names(item, m);
                let scope = parent_path(&key);
                let cause = whole.or_else(|| removed.contains(&key).then_some(key.as_str()));
                if let Some(cause) = cause {
                    for name in names { pending.push((qualify(scope, &name), cause.to_owned())); }
                }
                if !graph.deleted.contains(path) {
                    for source in sources { globs.entry(source).or_default().insert(scope.to_owned()); }
                }
            }
        }
    }
    let mut missing = BTreeMap::<String, BTreeSet<String>>::new();
    while let Some((path, cause)) = pending.pop() {
        if !missing.entry(path.clone()).or_default().insert(cause.clone()) { continue; }
        if let Some(users) = globs.get(parent_path(&path)) {
            let name = path.rsplit("::").next().unwrap();
            for scope in users { pending.push((qualify(scope, name), cause.clone())); }
        }
    }
    missing
}

pub(crate) fn outer_ranges(cuts: &mut Vec<Range<usize>>) -> Result<(), String> {
    cuts.sort_by_key(|r| (r.start, std::cmp::Reverse(r.end)));
    let mut end = 0;
    let mut overlap = false;
    cuts.retain(|r| {
        if r.start < end { overlap |= r.end > end; return false; }
        end = r.end;
        true
    });
    if overlap { Err("S1: intersecting item boundaries".into()) } else { Ok(()) }
}

pub fn prepare(tree: &Tree, table: &Table, skips: &[Skip]) -> Result<Prepared, SyncError> {
    let mut result = Prepared { outcomes: Vec::new(), graphs: BTreeMap::new(), replacements: Tree::new(), summaries: BTreeMap::new() };
    for skip in skips {
        if !table.crates.iter().any(|c| c.upstream.rsplit('/').next() == Some(&skip.krate)) {
            return Err(format!("skip.txt:{}: unknown crate {}", skip.line, skip.krate));
        }
    }
    for rule in &table.removals {
        if !table.crates.iter().any(|c| c.upstream == rule.upstream) { return Err(format!("unknown crate for named removal {}", rule.item)); }
    }
    for spec in &table.crates {
        prepare_crate(tree, table, spec, skips, &mut result)?;
    }
    Ok(result)
}

fn prepare_crate(tree: &Tree, table: &Table, spec: &CrateSpec, skips: &[Skip], result: &mut Prepared) -> Result<(), String> {
    let krate = &spec.upstream;
    let edition = edition(tree, krate, &table.workspace_manifest)?;
    let prefix = format!("{krate}/");
    let mut files = BTreeMap::new();
    let mut decls = Vec::new();
    let mut module_names = BTreeSet::new();
    let mut errors = Vec::new();
    for (path, bytes) in tree {
        let Some(rel) = path.strip_prefix(&prefix) else { continue; };
        if !rel.ends_with(".rs") || !spec.rust_roots.iter().any(|root| rel.starts_with(root)) { continue; }
        let source = Source::parse(path, bytes, edition)?;
        match source.scan() {
            Ok(scan) => {
                decls.extend(collect(&source, krate, &[], &mut module_names)?);
                files.insert(path.as_str(), File { source, scan, modules: Vec::new(), cuts: Vec::new() });
            }
            Err(error) => errors.push(error),
        }
    }
    if !errors.is_empty() { return Err(format!("{} S1 refusals\n{}", errors.len(), errors.join("\n"))); }
    let mut paths = module_paths(tree, krate, &decls)?;
    for (&path, file) in &mut files {
        file.modules = paths.remove(path).unwrap_or_else(|| vec![format!("<{path}>")]);
    }
    let outcome = |rule| Outcome { rule, crate_: krate.clone(), count: 0, files: Vec::new() };
    for drop in table.drops.iter().filter(|d| d.upstream == *krate) {
        let path = format!("{prefix}{}", drop.file);
        let file = files.get_mut(path.as_str()).ok_or_else(|| format!("D1 missing {path}"))?;
        let ranges = drop_module(&file.source, &drop.line)?;
        let count = ranges.len();
        if count != drop.expected { return Err(format!("D1: expected exactly {}, found {count}", drop.expected)); }
        file.cuts.extend(ranges);
        result.outcomes.push(Outcome { count, files: vec![path], ..outcome("D1") });
    }
    let mut summary = Summary::default();
    let cfg_modules: BTreeSet<String> = files.values().flat_map(|file| {
        file.modules.iter().flat_map(|m| file.scan.items.iter().filter(|i| i.cfg && i.module)
            .map(move |i| qualify(m, &i.path)))
    }).collect();
    let in_test_module = |path: &str| ancestors(path).any(|a| cfg_modules.contains(a));
    let crate_name = krate.rsplit('/').next().unwrap();
    let mut whole_hits = BTreeMap::<String, Vec<String>>::new();
    for file in files.values_mut() {
        file.source.index_test_items(&mut file.scan, file.modules.iter().any(|m| in_test_module(m)))?;
        for item in file.scan.items.iter().filter(|i| !i.test && !i.module) {
            let hits = &file.scan.denied[item.hits.clone()];
            if hits.is_empty() { continue; }
            for m in &file.modules {
                let parent = qualify(m, parent_path(&item.path));
                if in_test_module(&parent) {
                    whole_hits.entry(parent).or_default().push(file.source.helper_reason(item, hits));
                }
            }
        }
    }
    // A whole parent takes its descendants with it. Their declarations
    // are never visited by the recursive rule and need no skip entries.
    let whole: BTreeSet<_> = whole_hits.keys().filter(|m|
        !ancestors(m).skip(1).any(|p| whole_hits.contains_key(p))).cloned().collect();
    let mut named = BTreeMap::new();
    let mut features = BTreeMap::new();
    let mut gated_modules = BTreeMap::new();
    for skip in skips.iter().filter(|s| s.krate == crate_name) {
        if skip.kind == "feature" {
            let feature = skip.reason.split_whitespace().next().unwrap_or("").trim_end_matches(':');
            features.insert(skip.item.as_str(), feature);
            gated_modules.entry(feature).or_insert_with(|| {
                let mut modules = BTreeSet::new();
                for file in files.values() {
                    for local in file.source.feature_modules(feature) {
                        for m in &file.modules { modules.insert(qualify(m, &local)); }
                    }
                }
                modules
            });
        }
        if skip.kind == "module" {
            if whole.contains(&skip.item) {
                summary.whole_modules.insert(skip.item.clone(), skip.reason.clone());
            } else {
                errors.push(format!("skip.txt:{}: stale module {}: expected exactly 1, found 0", skip.line, skip.item));
            }
        }
        named.insert(skip.item.as_str(), (format!("skip.txt:{} ({})", skip.line, skip.kind), skip.kind.as_str(), 0));
    }
    for key in &whole {
        if !summary.whole_modules.contains_key(key) {
            errors.push(format!("S1 {key} needs whole removal; proposed skip.txt entry:\n{crate_name} {key} module {}",
                whole_hits[key].join("; ")));
        }
    }
    if !errors.is_empty() { return Err(errors.join("\n")); }
    let rules: Vec<_> = table.removals.iter().filter(|r| r.upstream == *krate).collect();
    for rule in &rules {
        if named.insert(&rule.item, (rule.reason.clone(), "remove", 0)).is_some() {
            return Err(format!("duplicate named removal {}", rule.item));
        }
    }

    let mut removed = BTreeSet::new();
    let defaults = if features.is_empty() { BTreeSet::new() } else {
        crate::manifest::default_features(tree, spec, table)?
    };
    let mut individual = BTreeMap::new();
    let mut s1_files = BTreeSet::new();
    let mut skips_applied = outcome("S4");
    for (&path, file) in &mut files {
        for item in &file.scan.items {
            let hits = &file.scan.denied[item.hits.clone()];
            let mut routes = 0;
            let mut live_routes = 0;
            for m in &file.modules {
                let key = qualify(m, &item.path);
                if let Some(&feature) = features.get(key.as_str()) {
                    if defaults.contains(feature) {
                        return Err(format!("stale feature skip {key}: {feature} is enabled by default"));
                    }
                    if !file.source.item_requires_feature(item, feature) &&
                        !ancestors(parent_path(&key)).any(|p| gated_modules[feature].contains(p)) {
                        return Err(format!("stale feature skip {key}: missing required cfg(feature = {feature:?})"));
                    }
                }
                if item.ignored { summary.ignored.push(format!("{path}:{} {key}", item.line)); }
                if ancestors(&key).skip(1).any(|a| whole.contains(a)) {
                    // Per-function skip entries count their upstream match
                    // even when the enclosing module already supplies the cut.
                    if let Some((label, kind, count)) = named.get_mut(key.as_str()) {
                        if item.test && !matches!(*kind, "module" | "remove") {
                            *count += 1;
                            summary.skips.push(format!("{label} {key}"));
                        }
                    }
                    continue;
                }
                live_routes += 1;
                let whole_module = item.module && whole.contains(&key);
                let by_token = !hits.is_empty() && (item.cfg || item.test && in_test_module(&key));
                let automatic = whole_module || by_token || item.ignored;
                let mut cut = automatic;
                let mut named_caller = false;
                if let Some((label, kind, count)) = named.get_mut(key.as_str()) {
                    let matches = match *kind {
                        "module" => whole_module,
                        "remove" if item.module => {
                            return Err(format!("named removal {key}: whole modules require a matching module entry in skip.txt"));
                        }
                        "remove" => !automatic,
                        _ => item.test,
                    };
                    if matches {
                        if file.modules.len() != 1 {
                            return Err(format!("{label}: cannot remove {key} independently in shared file {path} ({:?})", file.modules));
                        }
                        *count += 1;
                        cut = true;
                        if *kind == "remove" {
                            named_caller = true;
                            summary.named_removals.push(format!("{key}: {label}"));
                        } else if *kind != "module" {
                            summary.skips.push(format!("{label} {key}"));
                        }
                    }
                }
                if cut {
                    routes += 1;
                    if !named_caller { removed.insert(key); }
                    if automatic || named_caller {
                        s1_files.insert(path);
                    } else {
                        skips_applied.count += 1;
                        if skips_applied.files.last().map(String::as_str) != Some(path) {
                            skips_applied.files.push(path.into());
                        }
                    }
                }
                if by_token && item.test {
                    let kinds = hits.iter().map(|(_, k)| *k).collect::<BTreeSet<_>>();
                    individual.insert((path, item.span.start), kinds);
                }
            }
            if routes != 0 {
                if routes != live_routes { return Err(format!("S1 ambiguous test removal in shared file {path}: {}", item.path)); }
                file.cuts.push(item.span.clone());
            }
        }
        outer_ranges(&mut file.cuts)?;
    }
    for (item, (label, _, n)) in named {
        if n != 1 { return Err(format!("{label}: {item}: expected exactly 1, found {n}")); }
    }
    // A named chain must end at a real removal. Two stale entries cannot
    // justify each other, regardless of their order in the table.
    let mut callers = BTreeMap::<&str, Vec<&str>>::new();
    for r in &rules { callers.entry(&r.uses).or_default().push(&r.item); }
    let mut pending: Vec<_> = removed.iter().cloned().collect();
    while let Some(target) = pending.pop() {
        if let Some(items) = callers.remove(target.as_str()) {
            for item in items {
                if removed.insert(item.to_owned()) { pending.push(item.to_owned()); }
            }
        }
    }
    for rule in rules {
        if !removed.contains(&rule.uses) {
            return Err(format!("stale named removal {}: uses {} which is not removed", rule.item, rule.uses));
        }
    }
    for d in &mut decls { d.mark(&files[d.from.as_str()].cuts); }
    let graph = inspect_graph(tree, krate, &decls, &spec.keep, table.expect.s3_each_pattern_min)?;
    let mut strip = outcome("S1");
    for (&path, file) in &files {
        if !graph.deleted.contains(path) {
            strip.count += file.cuts.len();
            if s1_files.contains(path) { strip.files.push(path.into()); }
        }
    }
    let orphan_rules: Vec<_> = table.orphans.iter().filter(|r| r.upstream == *krate).collect();
    let mut orphans = outcome("O1");
    let graph = if orphan_rules.is_empty() { graph } else {
        orphans.files = crate::strip::orphans::remove(&orphan_rules, &mut files, &graph, &cfg_modules, &module_names)?;
        orphans.count = orphan_rules.len();
        for d in &mut decls { d.mark(&files[d.from.as_str()].cuts); }
        inspect_graph(tree, krate, &decls, &spec.keep, table.expect.s3_each_pattern_min)?
    };
    let bindings = removed_bindings(&files, &removed, &graph);
    let removed_names: HashSet<_> = bindings.keys().map(|s| s.rsplit("::").next().unwrap()).collect();
    let mut candidates = BTreeSet::new();
    for (&path, file) in &files {
        let deleted = graph.deleted.contains(path);
        let module = &file.modules[0];
        let s = &file.source;
        for item in file.scan.items.iter().filter(|i| i.test) {
            if deleted || contains(&file.cuts, item.span.start) {
                summary.removed_tests += 1;
                let group = qualify(module, parent_path(&item.path));
                *summary.losses.entry(group).or_default() += 1;
                if !deleted {
                    if let Some(kinds) = individual.get(&(path, item.span.start)) {
                        summary.denied_tests += 1;
                        for kind in kinds { *summary.kinds.entry(kind.to_string()).or_default() += 1; }
                    }
                }
            } else { summary.kept_tests += 1; }
        }
        if deleted { continue; }
        summary.statements += file.scan.contexts.iter().filter(|(r, _, statement)| *statement && !contains(&file.cuts, r.start)).count();
        let test_file = file.modules.iter().all(|m| ancestors(m).any(|a| cfg_modules.contains(a)));
        let mut test_ranges: Vec<_> = file.scan.items.iter().filter(|i| i.test || i.cfg).map(|i| i.span.clone())
            .chain(file.scan.contexts.iter().map(|(span, _, _)| span.clone())).collect();
        outer_ranges(&mut test_ranges)?;
        crate::inventory::inventory(s, &file.cuts, &test_ranges, test_file, table, path == format!("{prefix}src/lib.rs"), &mut summary.inventory)?;
        for &(pos, kind) in &file.scan.denied {
            if contains(&file.cuts, pos) { continue; }
            let test = test_file || contains(&test_ranges, pos);
            let line = s.lines().partition_point(|&n| n as usize <= pos);
            let site = format!("{kind} {path}:{line}");
            if !test { summary.production_denied.push(site.clone()); }
            errors.push(site);
        }
        for i in 0..s.toks.len() {
            if !s.kind(i).is_ident_like() || !removed_names.contains(s.text(i).trim_start_matches("r#")) { continue; }
            let pos = s.toks[i].lo as usize;
            if contains(&file.cuts, pos) { continue; }
            for m in &file.modules {
                for target in s.reference(i, m) {
                    let Some(causes) = bindings.get(&target) else { continue; };
                    let owner = file.scan.items.iter().rev().find(|item| item.span.contains(&pos))
                        .map(|item| qualify(m, &item.path))
                        .or_else(|| ancestors(m).find(|a| cfg_modules.contains(*a)).map(String::from))
                        .unwrap_or_else(|| format!("unclassified in {m}"));
                    for cause in causes {
                        candidates.insert(format!("{path}:{} {owner} -> {cause}", s.line(i)));
                    }
                }
            }
        }
        if !file.cuts.is_empty() {
            result.replacements.insert(path.into(), apply(s.src.as_bytes(), &file.cuts));
        }
    }
    if !errors.is_empty() {
        return Err(format!("S1 denied tokens in kept source:\n{}\nnon-test sites:\n{}", errors.join("\n"), summary.production_denied.join("\n")));
    }
    if !candidates.is_empty() {
        return Err(format!("S1 {} cross-module helper candidates:\n{}", candidates.len(), candidates.into_iter().collect::<Vec<_>>().join("\n")));
    }
    for probe in &spec.report_sites {
        let (rel, line) = probe.rsplit_once(':').ok_or_else(|| format!("bad report site {probe}"))?;
        let line = line.parse::<usize>().map_err(|_| format!("bad report site {probe}"))?;
        let path = format!("{prefix}{rel}");
        let file = files.get(path.as_str()).ok_or_else(|| format!("missing report site {probe}"))?;
        let offset = *file.source.lines().get(line.checked_sub(1).ok_or("invalid line 0")?).ok_or_else(|| format!("missing line {probe}"))? as usize;
        let removed = graph.deleted.contains(&path) || contains(&file.cuts, offset);
        summary.sites.push(format!("{probe} {}", if removed { "removed" } else { "kept" }));
    }
    at_least("S1", table.expect.s1_min, strip.count)?;
    at_least("S2", table.expect.s2_min, graph.deleted.len())?;
    result.outcomes.push(strip);
    result.outcomes.push(skips_applied);
    result.outcomes.push(orphans);
    result.outcomes.push(Outcome { count: graph.deleted.len(), files: graph.deleted.iter().cloned().collect(), ..outcome("S2") });
    result.graphs.insert(krate.clone(), graph);
    result.summaries.insert(krate.clone(), summary);
    Ok(())
}

pub fn run_rules(tree: &mut Tree, table: &Table) -> Result<Vec<Outcome>, SyncError> {
    let mut prepared = prepare(tree, table, &[])?;
    prepared.apply_to(tree, &BTreeSet::new())?;
    Ok(prepared.outcomes)
}
