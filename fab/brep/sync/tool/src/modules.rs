use crate::strip::Source;
use crate::table::{at_least, Tree};
use crate::SyncError;
use relax_lex::lexer::T;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::ops::Range;

#[derive(Clone)]
struct Module {
    name: String,
    path: Option<String>,
}

enum Target {
    Module { inline: Vec<Module>, child: Module },
    OpaqueModule(Option<String>),
    InlineDirectory(Vec<Module>),
    Include(String),
}

pub struct ModDecl {
    pub(crate) from: String,
    pos: usize,
    line: usize,
    removed: bool,
    target: Target,
}

#[derive(Default)]
pub struct Graph {
    pub deleted: BTreeSet<String>,
    pub listed: BTreeSet<String>,
    pub includes_kept: BTreeSet<String>,
    pub includes_deleted: BTreeSet<String>,
    // Outside C1 and used exclusively by test code. Require existence at
    // the same pin before accepting the graph, without staging the fixtures.
    pub external: BTreeMap<String, String>,
    pub kept_by_pattern: BTreeSet<String>,
}

fn directory(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(dir, _)| dir)
}

fn join(base: &str, path: &str, floor: &str) -> Result<String, SyncError> {
    if path.is_empty() || path.starts_with('/') || path.contains(['\\', ':', '\0']) {
        return Err(format!("invalid relative path {path:?}"));
    }
    let mut parts: Vec<_> = base.split('/').filter(|s| !s.is_empty()).collect();
    let min = if floor.is_empty() { 0 } else { floor.split('/').count() };
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." if parts.len() > min => { parts.pop(); }
            ".." => return Err(format!("{path:?} leaves {}", if floor.is_empty() { "repository" } else { floor })),
            _ => parts.push(part),
        }
    }
    let result = parts.join("/");
    if !floor.is_empty() && result != floor && !result.strip_prefix(floor).is_some_and(|p| p.starts_with('/')) {
        return Err(format!("{path:?} leaves {floor}"));
    }
    Ok(result)
}

pub(crate) fn literal(s: &str) -> Result<&str, SyncError> {
    if let Some(raw) = s.strip_prefix('r') {
        if let Some((hashes, body)) = raw.split_once('"') {
            if hashes.bytes().all(|b| b == b'#') {
                if let Some(text) = body.strip_suffix(hashes).and_then(|s| s.strip_suffix('"')) {
                    return Ok(text);
                }
            }
        }
    } else if let Some(value) = s.strip_prefix('"').and_then(|s| s.strip_suffix('"')) {
        if !value.contains('\\') { return Ok(value); }
    }
    Err(format!("expected a literal path, found {s}"))
}

fn path_attribute(s: &Source<'_>, keyword: usize) -> Result<Option<String>, SyncError> {
    let mut start = keyword;
    if start > 0 && s.kind(start - 1) == T::CloseParen {
        let mut p = start - 1;
        while p > 0 && s.kind(p) != T::OpenParen { p -= 1; }
        if p > 0 && s.kind(p - 1) == T::KwPub { start = p - 1; }
    } else if start > 0 && s.kind(start - 1) == T::KwPub {
        start -= 1;
    }
    let mut i = s.attr_start(start);
    let mut path = None;
    while i < start {
        if s.kind(i) != T::Pound { i += 1; continue; }
        let end = s.after(i + 1);
        match s.text(i + 2).trim_start_matches("r#") {
            "path" => {
                if i + 6 != end || s.kind(i + 3) != T::Eq || path.is_some() {
                    return Err("expected exactly one literal #[path]".into());
                }
                path = Some(literal(s.text(i + 4))?.to_owned());
            }
            "cfg_attr" if (i + 3..end).any(|j| s.text(j).trim_start_matches("r#") == "path") => {
                return Err("conditional #[path] cannot be resolved".into());
            }
            _ => {}
        }
        i = end;
    }
    Ok(path)
}

fn include(s: &Source<'_>, group: usize, krate: &str) -> Result<String, SyncError> {
    let first = group + 1;
    let mut limit = s.after(group) - 1;
    if limit > first && s.kind(limit - 1) == T::Comma { limit -= 1; }
    if first + 1 == limit {
        return join(directory(s.path), literal(s.text(first))?, "");
    }
    // The pinned kernel uses this exact expansion to refer to fixtures
    // outside src/. Never evaluate macros or consult the host environment.
    if s.text(first) == "concat" && s.kind(first + 1) == T::Not && s.kind(first + 2) == T::OpenParen
        && s.after(first + 2) == limit && s.text(first + 3) == "env" && s.kind(first + 4) == T::Not
        && s.kind(first + 5) == T::OpenParen && s.text(first + 6) == "\"CARGO_MANIFEST_DIR\""
        && s.after(first + 5) == first + 8 && s.kind(first + 8) == T::Comma
    {
        let tail = first + 10;
        if tail == limit - 1 || (tail + 1 == limit - 1 && s.kind(tail) == T::Comma) {
            let path = literal(s.text(first + 9))?.strip_prefix('/').ok_or("expected a manifest-relative suffix")?;
            return join(krate, path, "");
        }
    }
    Err("cannot resolve include target from these tokens".into())
}

pub(crate) fn drop_module(s: &Source<'_>, line: &str) -> Result<Vec<Range<usize>>, SyncError> {
    let mut ranges = Vec::new();
    for i in 0..s.toks.len() {
        if s.kind(i) != T::KwPub || !s.at_root(i) || s.kind(i + 1) != T::KwMod
            || !matches!(s.kind(i + 2), T::Ident | T::RawIdent) || s.kind(i + 3) != T::Semi { continue; }
        let start = s.toks[i].lo as usize;
        let end = s.toks[i + 3].hi as usize;
        if &s.src[start..end] != line { continue; }
        let line_start = s.src[..start].rfind('\n').map_or(0, |p| p + 1);
        let line_end = s.src[end..].find('\n').map_or(s.src.len(), |p| end + p);
        if s.src[line_start..line_end].trim_end_matches('\r') != line { continue; }
        if s.attr_start(i) != i {
            return Err(format!("D1 {}:{}: unexpected attributes or docs on {line}",
                s.path, s.src[..start].lines().count() + 1));
        }
        ranges.push(line_start..(line_end + usize::from(line_end < s.src.len())));
    }
    Ok(ranges)
}

pub(crate) fn collect(s: &Source<'_>, krate: &str, cuts: &[Range<usize>], module_names: &mut BTreeSet<String>)
    -> Result<Vec<ModDecl>, SyncError>
{
    let mut decls = Vec::new();
    let mut inline: Vec<(usize, Module)> = Vec::new();
    let mut cuts = cuts.iter().peekable();
    for i in 0..s.toks.len() {
        let kind = s.kind(i);
        if kind != T::KwMod && !(matches!(kind, T::Ident | T::RawIdent) && s.kind(i + 1) == T::Not
            && matches!(s.text(i).trim_start_matches("r#"), "include_bytes" | "include_str" | "include")) { continue; }
        let pos = s.toks[i].lo as usize;
        let line = s.lines().partition_point(|&start| start as usize <= pos);
        let at = |msg: String| format!("S2 {}:{line}: {msg}", s.path);
        while inline.last().is_some_and(|(end, _)| *end <= i) { inline.pop(); }
        while cuts.peek().is_some_and(|r| r.end <= pos) { cuts.next(); }
        let removed = cuts.peek().is_some_and(|r| r.start <= pos);
        let target = if kind == T::KwMod {
            if s.in_macro(i) {
                // Expansion may occur in a different module. A literal module
                // name protects matching files anywhere in the source tree.
                let ordinary = matches!(s.kind(i + 1), T::Ident | T::RawIdent) && s.kind(i + 2) == T::Semi
                    && s.attr_start(i) == i;
                if s.kind(i + 2) != T::OpenBrace {
                    let name = ordinary.then(|| s.text(i + 1).trim_start_matches("r#").to_owned());
                    if let Some(name) = &name { module_names.insert(name.clone()); }
                    decls.push(ModDecl { from: s.path.into(), pos, line, removed, target: Target::OpaqueModule(name) });
                }
                continue;
            }
            if s.in_attribute(i) {
                return Err(at("mod token in an opaque macro or attribute token tree".into()));
            }
            if !matches!(s.kind(i + 1), T::Ident | T::RawIdent) {
                return Err(at("expected a module name".into()));
            }
            let child = Module {
                name: s.text(i + 1).trim_start_matches("r#").to_owned(),
                path: path_attribute(s, i).map_err(at)?,
            };
            // Even inline modules with no file edges can qualify a kept reference in O1.
            module_names.insert(child.name.clone());
            if s.kind(i + 2) == T::OpenBrace {
                let has_path = child.path.is_some();
                inline.push((s.after(i + 2), child));
                if !has_path { continue; }
                Target::InlineDirectory(inline.iter().map(|(_, m)| m.clone()).collect())
            } else {
                if s.kind(i + 2) != T::Semi { return Err(at("expected an outlined module".into())); }
                Target::Module { inline: inline.iter().map(|(_, m)| m.clone()).collect(), child }
            }
        } else {
            if s.text(i).trim_start_matches("r#") == "include" {
                // B1 checks generated code after S1/S2 determine which sites survive.
                continue;
            }
            if !matches!(s.kind(i + 2), T::OpenParen | T::OpenBracket | T::OpenBrace) {
                return Err(at("expected include arguments".into()));
            }
            if s.in_macro(i) {
                return Err(at("include in an opaque macro token tree".into()));
            }
            Target::Include(include(s, i + 2, krate).map_err(at)?)
        };
        decls.push(ModDecl { from: s.path.to_owned(), line, pos, removed, target });
    }
    Ok(decls)
}

// A #[path]-loaded file, like mod.rs, owns its directory. For x.rs
// found by name, ordinary children live under x/ but a #[path] is
// relative to the declaring file. Inline modules own their virtual dir.
fn module_dirs(root: &str, file: &str, owns_dir: bool, inline: &[Module])
    -> Result<(String, String), SyncError>
{
    let mut attr_base = directory(file).to_owned();
    let mut base = if owns_dir { attr_base.clone() } else {
        file.strip_suffix(".rs").ok_or("module source has no .rs suffix")?.to_owned()
    };
    for module in inline {
        base = match &module.path {
            Some(path) => join(&attr_base, path, root)?,
            None => join(&base, &module.name, root)?,
        };
        attr_base = base.clone();
    }
    Ok((base, attr_base))
}

fn resolve(tree: &Tree, root: &str, file: &str, owns_dir: bool, inline: &[Module], child: &Module)
    -> Result<Option<(String, bool)>, SyncError>
{
    let (base, attr_base) = module_dirs(root, file, owns_dir, inline)?;
    if let Some(path) = &child.path {
        let path = join(&attr_base, path, root)?;
        if !tree.contains_key(&path) { return Ok(None); }
        if !path.ends_with(".rs") { return Err(format!("module {path} is outside the Rust source inventory")); }
        return Ok(Some((path, true)));
    }
    let named = format!("{base}/{}.rs", child.name);
    let nested = format!("{base}/{}/mod.rs", child.name);
    match (tree.contains_key(&named), tree.contains_key(&nested)) {
        (true, false) => Ok(Some((named, false))),
        (false, true) => Ok(Some((nested, true))),
        (true, true) => Err(format!("ambiguous module: both {named} and {nested} exist")),
        _ => Ok(None),
    }
}


pub(crate) fn module_paths(tree: &Tree, krate: &str, decls: &[ModDecl]) -> Result<BTreeMap<String, Vec<String>>, String> {
    let root = format!("{krate}/src");
    let mut edges = BTreeMap::<&str, Vec<&ModDecl>>::new();
    for d in decls {
        if matches!(d.target, Target::Module { .. }) { edges.entry(&d.from).or_default().push(d); }
    }
    let mut paths: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut queue = VecDeque::from([(format!("{root}/lib.rs"), true, String::new(), Vec::<String>::new())]);
    let mut possible = BTreeSet::new();
    for (file, refs) in &edges {
        for d in refs {
            let Target::Module { inline, child } = &d.target else { continue; };
            for mode in [true, false] {
                if let Ok(Some((target, _))) = resolve(tree, &root, file, mode, inline, child) { possible.insert(target); }
            }
        }
    }
    for file in tree.keys().filter(|f| f.starts_with(&format!("{root}/")) && f.ends_with(".rs")
        && !f.ends_with("/lib.rs") && !possible.contains(*f)) {
        queue.push_back((file.clone(), file.ends_with("/mod.rs"), format!("<{file}>"), Vec::new()));
    }
    while let Some((file, owned, path, mut parents)) = queue.pop_front() {
        if parents.contains(&file) { return Err(format!("S2: cyclic module through {file}")); }
        let routes = paths.entry(file.clone()).or_default();
        if routes.contains(&path) { continue; }
        routes.push(path.clone());
        parents.push(file.clone());
        for d in edges.get(file.as_str()).into_iter().flatten() {
            let Target::Module { inline, child } = &d.target else { continue; };
            let target = resolve(tree, &root, &file, owned, inline, child)?;
            let target = if target.is_none() && path.starts_with('<') {
                resolve(tree, &root, &file, !owned, inline, child)?
            } else { target };
            let Some((target, owned)) = target else {
                return Err(format!("S2 {}:{}: missing module {}", file, d.line, child.name));
            };
            let mut next = path.clone();
            for component in inline.iter().chain(std::iter::once(child)) {
                next = crate::strip::qualify(&next, &component.name);
            }
            queue.push_back((target, owned, next, parents.clone()));
        }
    }
    Ok(paths)
}

impl ModDecl {
    pub(crate) fn mark(&mut self, cuts: &[Range<usize>]) {
        let i = cuts.partition_point(|c| c.end <= self.pos);
        self.removed = cuts.get(i).is_some_and(|c| c.contains(&self.pos));
    }
}

fn keep_match(pattern: &str, path: &str) -> Result<bool, SyncError> {
    let Some((prefix, suffix)) = pattern.split_once('*') else { return Ok(path == pattern); };
    if suffix.contains('*') { return Err(format!("S3: unsupported keep pattern {pattern}")); }
    Ok(path.strip_prefix(prefix).and_then(|rest| rest.strip_suffix(suffix)).is_some_and(|s| !s.contains('/')))
}

pub(crate) fn inspect_graph(tree: &Tree, krate: &str, decls: &[ModDecl], keep: &[String], minimum: usize) -> Result<Graph, SyncError> {
    let root = format!("{krate}/src");
    let prefix = format!("{root}/");
    let lib = format!("{root}/lib.rs");
    if !tree.contains_key(&lib) { return Err(format!("S2: missing crate root {lib}")); }
    let mut edges = BTreeMap::<&str, Vec<&ModDecl>>::new();
    for decl in decls { edges.entry(&decl.from).or_default().push(decl); }
    let mut queue = VecDeque::from([(lib, Some(true), true)]);
    let mut visited = BTreeSet::new();
    let mut reached = BTreeSet::new();
    let mut live_files = BTreeSet::new();
    let mut live_includes = BTreeSet::new();
    let mut includes = BTreeSet::new();
    let mut result = Graph::default();

    // First traverse from lib.rs, including removed edges as evidence.
    // Then start at unconnected roots, so their kept declarations protect
    // children and their removed declarations can still prove exclusions.
    // Keep any remainder with no provable root (e.g. an unconnected cycle).
    for pass in 0..3 {
        if pass > 0 {
            let mut possible_children = BTreeSet::new();
            if pass == 1 {
                for (&file, refs) in &edges {
                    if !file.starts_with(&prefix) || reached.contains(file) { continue; }
                    for decl in refs {
                        let Target::Module { inline, child } = &decl.target else { continue; };
                        for owned in [false, true] {
                            // Only establish possible incoming edges here.
                            // The traversal below checks the actual mode.
                            if let Ok(Some((target, _))) = resolve(tree, &root, file, owned, inline, child) {
                                possible_children.insert(target);
                            }
                        }
                    }
                }
            }
            for file in tree.keys().filter(|p| p.starts_with(&prefix) && p.ends_with(".rs")) {
                if !reached.contains(file) && !possible_children.contains(file) {
                    result.listed.insert(file.clone());
                    // No declaration establishes how Rust would load this
                    // file. Protect children in either possible directory.
                    queue.push_back((file.clone(), file.ends_with("/mod.rs").then_some(true), true));
                }
            }
        } else {
            let build = format!("{krate}/build.rs");
            if tree.contains_key(&build) { queue.push_back((build, Some(true), true)); }
        }
        while let Some((file, owns_dir, live)) = queue.pop_front() {
            if !visited.insert((file.clone(), owns_dir, live)) { continue; }
            reached.insert(file.clone());
            if live { live_files.insert(file.clone()); }
            let Some(refs) = edges.get(file.as_str()) else { continue; };
            for decl in refs {
                let kept = live && !decl.removed;
                let at = || format!("S2 {}:{}", file, decl.line);
                let modes: &[bool] = match owns_dir {
                    Some(true) => &[true], Some(false) => &[false], None => &[false, true],
                };
                match &decl.target {
                    Target::OpaqueModule(name) => {
                        if !kept { continue; }
                        for target in tree.keys().filter(|p| p.starts_with(&prefix) && p.ends_with(".rs")) {
                            let relative = &target[prefix.len()..];
                            let stem = relative.strip_suffix("/mod.rs").or_else(|| relative.strip_suffix(".rs")).unwrap();
                            if name.as_deref().is_some_and(|n| stem.rsplit('/').next() != Some(n)) { continue; }
                            result.listed.insert(target.clone());
                            queue.push_back((target.clone(), target.ends_with("/mod.rs").then_some(true), true));
                        }
                    }
                    Target::InlineDirectory(inline) => {
                        for &owned in modes {
                            module_dirs(&root, &file, owned, inline).map_err(|e| format!("{}: {e}", at()))?;
                        }
                    }
                    Target::Module { inline, child } => {
                        let mut found = false;
                        for &owned in modes {
                            if let Some((target, child_owns_dir)) = resolve(tree, &root, &file, owned, inline, child)
                                .map_err(|e| format!("{}: {e}", at()))?
                            {
                                found = true;
                                queue.push_back((target, Some(child_owns_dir), kept));
                            }
                        }
                        if !found {
                            return Err(format!("{}: missing module {}", at(), child.path.as_deref().unwrap_or(&child.name)));
                        }
                    }
                    Target::Include(path) => {
                        if tree.contains_key(path) {
                            includes.insert(path.clone());
                            if kept { live_includes.insert(path.clone()); }
                        } else if !kept && !path.starts_with(&prefix) {
                            result.external.insert(path.clone(), at());
                        } else {
                            return Err(format!("{}: missing include {path}", at()));
                        }
                    }
                }
            }
        }
    }
    for pattern in keep {
        let mut found = 0;
        for path in tree.keys().filter(|p| p.starts_with(&prefix)) {
            if keep_match(pattern, &path[krate.len() + 1..])? {
                result.kept_by_pattern.insert(path.clone());
                found += 1;
            }
        }
        at_least(&format!("S3 {pattern}"), minimum, found)?;
    }
    for path in tree.keys().filter(|p| p.starts_with(&prefix)) {
        if live_includes.contains(path) || result.kept_by_pattern.contains(path) { continue; }
        if path.ends_with(".rs") {
            if reached.contains(path) && !live_files.contains(path) { result.deleted.insert(path.clone()); }
        } else if includes.contains(path) {
            result.deleted.insert(path.clone());
        } else {
            result.listed.insert(path.clone());
        }
    }
    for path in includes {
        if result.deleted.contains(&path) { result.includes_deleted.insert(path); }
        else { result.includes_kept.insert(path); }
    }
    Ok(result)
}

impl Graph {
    pub(crate) fn check_external(&self, blobs: &BTreeSet<String>) -> Result<(), SyncError> {
        for (path, site) in &self.external {
            if !blobs.contains(path) { return Err(format!("{site}: missing include {path}")); }
        }
        Ok(())
    }
}

// Both kept and removed declarations come from the single pre-strip lex.
pub fn module_graph(tree: &Tree, krate: &str, decls: &[ModDecl], keep: &[String], minimum: usize) -> Result<Graph, SyncError> {
    let graph = inspect_graph(tree, krate, decls, keep, minimum)?;
    graph.check_external(&BTreeSet::new())?;
    Ok(graph)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::rules::{parse_skips, prepare};
    use crate::table::Table;

    #[test]
    fn module_graph_follows_mod_and_path_rules() {
        let krate = "sample";
        let path = |name: &str| format!("{krate}/{name}");
        let mut tree = Tree::new();
        for (name, text) in [
            ("Cargo.toml", "[package]\nedition = \"2024\"\n"),
            ("src/lib.rs", "mod a; mod c; opaque!(mod generated;); #[cfg(test)] mod old;\npub mod native;\n"),
            ("src/a.rs", "mod b; #[path = \"x_impl.rs\"] mod x;"),
            ("src/a/b.rs", "mod inline { mod sub; }"),
            ("src/a/b/inline/sub.rs", ""),
            ("src/x_impl.rs", "mod sibling;"),
            ("src/sibling.rs", ""),
            ("src/c/mod.rs", "mod d;"),
            ("src/c/d.rs", ""), ("src/c/shared.txt", "keep"),
            ("src/old.rs", "mod child; const S: &str = include_str!(\"c/shared.txt\");"),
            ("src/old/child.rs", "const D: &[u8] = include_bytes!(\"fixture.bin\");"),
            ("src/old/fixture.bin", "gone"),
            ("src/native.rs", "mod binary;"), ("src/native/binary.rs", ""),
            ("src/orphan.rs", "const TEXT: &str = \"pub mod native;\";"),
            ("src/orphan.txt", "hold"), ("src/generated.rs", ""),
        ] { tree.insert(path(name), text.as_bytes().to_vec()); }
        let rules = sample_rules();
        let old = "sample old module const S includes shared.txt at line 1";
        let skips = parse_skips(old).unwrap();
        let mut prepared = prepare(&tree, &rules, &skips).unwrap();
        let g = &prepared.graphs[krate];
        assert_eq!(g.deleted, ["src/native.rs", "src/native/binary.rs", "src/old.rs",
            "src/old/child.rs", "src/old/fixture.bin"].map(path).into(), "only removed subtrees");
        assert_eq!(g.listed, ["src/generated.rs", "src/orphan.rs", "src/orphan.txt"].map(path).into());
        assert_eq!(g.includes_kept, [path("src/c/shared.txt")].into(), "explicit keep protects a shared include target");
        assert_eq!(g.includes_deleted, [path("src/old/fixture.bin")].into());
        let mut done = tree.clone();
        prepared.apply_to(&mut done, &BTreeSet::new()).unwrap();
        assert_eq!(&done[&path("src/lib.rs")], b"mod a; mod c; opaque!(mod generated;); \n");
        assert!(!done.contains_key(&path("src/native/binary.rs")));
        assert!(done.contains_key(&path("src/x_impl.rs")) && done.contains_key(&path("src/sibling.rs")));
        let mut shared = tree.clone();
        shared.insert(path("src/orphan.rs"), b"#[path = \"native/binary.rs\"] mod shared; #[cfg(test)] mod child;".to_vec());
        shared.insert(path("src/orphan/child.rs"), b"fn uses_it() { fs::read(\"x\"); }".to_vec());
        let with_child = parse_skips(&format!("{old}\nsample <sample/src/orphan.rs>::child module uses_it reads a file at line 1")).unwrap();
        prepare(&shared, &rules, &with_child).unwrap().apply_to(&mut shared, &BTreeSet::new()).unwrap();
        assert!(shared.contains_key(&path("src/native/binary.rs")), "reference in a kept orphan protects its target");
        assert!(!shared.contains_key(&path("src/orphan/child.rs")), "removed declaration in a kept orphan");
        for (file, contents, expected) in [
            ("src/a.rs", "#[path = \"../escape.rs\"] mod x;", "leaves"),
            ("src/a.rs", "#[path = \"../escape\"] mod x {}", "leaves"),
            ("src/a.rs", "mod missing;", "missing module"),
            ("src/a.rs", "const X: &str = include_str!(\"missing\");", "missing include"),
            ("src/old.rs", "mod child; const X: &str = include_str!(\"../../missing\");", "missing include"),
            ("src/lib.rs", "#[cfg(test)] mod old;\n", "D1: expected exactly 1, found 0"),
            ("src/lib.rs", "#[cfg(test)] mod old;\npub mod native;\npub mod native;\n", "D1: expected exactly 1, found 2"),
        ] {
            let mut bad = tree.clone();
            bad.insert(path(file), contents.as_bytes().to_vec());
            let before = bad.clone();
            let err = prepare(&bad, &rules, &skips)
                .and_then(|mut change| change.apply_to(&mut bad, &BTreeSet::new())).err().expect("must refuse");
            assert!(err.contains(expected), "{err}");
            assert_eq!(bad, before, "failed rule partially changed the tree");
        }
    }

    pub(crate) fn sample_rules() -> Table {
        Table::parse("rules.toml", r#"
[workspace]
manifest = "Cargo.toml"
root_files = ["Cargo.toml"]
shim_root = "shims"
attribution_section = "section.md"
[deny]
names = []
directories = []
extensions = []
[expect]
c1_each_path_min = 1
c2_denied = 0
s1_min = 1
s2_min = 0
s3_each_pattern_min = 1
a1_per_crate = 1
[[crate]]
upstream = "sample"
dest = "dest"
package = "example"
dev_dependencies = []
sections = ["package"]
report_sites = []
rust_roots = ["src/"]
copy = ["src/"]
keep = ["src/c/shared.txt"]
dependencies = []
[build]
sha256 = "sample"
inputs = "src/flags/*.md"
producer = "sample/build.rs"
consumer = "sample/src/generated.rs"
output = "generated.rs"
[inventory]
licenses = ["Apache-2.0"]
license_files = ["LICENSE", "NOTICE"]
serde_shim = []
i1 = []
i2 = []
[[drop_module]]
upstream = "sample"
file = "src/lib.rs"
line = "pub mod native;"
expected = 1
"#).unwrap()
    }

    #[test]
    fn strip_removes_ignored_tests_and_skip_entries() {
        let mut table = sample_rules();
        table.drops.clear();
        table.crates[0].keep.clear();
        let source = "#[cfg(test)] mod tests {
            #[test] #[ignore] fn a() {}
            #[ignore = \"measured\"] #[test] fn b() {}
            #[test] fn c() {} #[test] fn d() {}
        }";
        let mut tree = Tree::from([
            ("sample/Cargo.toml".into(), b"[package]\nedition = \"2021\"".to_vec()),
            ("sample/src/lib.rs".into(), source.as_bytes().to_vec()),
        ]);
        let skips = crate::rules::parse_skips("sample tests::c slow measured 3 s under JIT").unwrap();
        let mut result = prepare(&tree, &table, &skips).unwrap();
        let stats = &result.summaries["sample"];
        assert_eq!((stats.ignored.len(), stats.skips.len(), stats.kept_tests, stats.removed_tests), (2, 1, 1, 3));
        result.apply_to(&mut tree, &BTreeSet::new()).unwrap();
        let kept = std::str::from_utf8(&tree["sample/src/lib.rs"]).unwrap();
        assert!(kept.contains("fn d()") && !kept.contains("ignore") && !kept.contains("fn c()"));
        for (count, text) in [(0, source.replace("fn c()", "fn x()")), (2, source.replace("fn d()", "fn c()"))] {
            tree.insert("sample/src/lib.rs".into(), text.into_bytes());
            let error = prepare(&tree, &table, &skips).err().unwrap();
            assert!(error.contains("skip.txt:1") && error.contains(&format!("expected exactly 1, found {count}")), "{error}");
        }

        let manifest = "[package]\nname = \"sample\"\nversion = \"1\"\nedition = \"2021\"\n\
            rust-version = \"1.85\"\nlicense = \"Apache-2.0\"\n[features]\nprofile = []\nvia = [\"profile\"]\n";
        table.crates[0].sections.push("features".into());
        table.expect.s1_min = 0;
        tree.insert("Cargo.toml".into(), Vec::new());
        tree.insert("LICENSE".into(), Vec::new());
        tree.insert("NOTICE".into(), Vec::new());
        tree.insert("sample/Cargo.toml".into(), manifest.as_bytes().to_vec());
        tree.insert("sample/src/lib.rs".into(), br#"#[cfg(all(unix, feature = "profile"))] mod gated;
            #[cfg(test)] mod tests { #[cfg(feature = "profile")] #[test] fn own() {} #[test] fn keep() {} }"#.to_vec());
        tree.insert("sample/src/gated.rs".into(), b"#[cfg(test)] mod tests { #[test] fn inherited() {} }".to_vec());
        let skips = parse_skips("sample gated::tests::inherited feature profile: off\nsample tests::own feature profile: off").unwrap();
        let mut prepared = prepare(&tree, &table, &skips).unwrap();
        let stats = &prepared.summaries["sample"];
        assert_eq!((stats.skips.len(), stats.kept_tests, stats.removed_tests), (2, 1, 2));
        let mut kept = tree.clone();
        prepared.apply_to(&mut kept, &BTreeSet::new()).unwrap();
        assert!(!std::str::from_utf8(&kept["sample/src/gated.rs"]).unwrap().contains("fn inherited"));
        let kept_lib = std::str::from_utf8(&kept["sample/src/lib.rs"]).unwrap();
        assert!(kept_lib.contains("fn keep") && !kept_lib.contains("fn own"));
        tree.insert("sample/Cargo.toml".into(), format!("{manifest}default = [\"via\"]").into_bytes());
        let err = prepare(&tree, &table, &skips).err().unwrap();
        assert!(err.contains("stale feature skip") && err.contains("enabled by default"), "{err}");
        tree.insert("sample/Cargo.toml".into(), manifest.as_bytes().to_vec());
        for cfg in ["", r#"#[cfg(feature = "other")]"#, r#"#[cfg(any(feature = "profile", unix))]"#] {
            let source = format!("{} mod gated; #[cfg(test)] mod tests {{ #[test] fn own() {{}} }}", cfg);
            tree.insert("sample/src/lib.rs".into(), source.into_bytes());
            let err = prepare(&tree, &table, &skips[..1]).err().unwrap();
            assert!(err.contains("stale feature skip gated::tests::inherited") && err.contains("profile"), "{err}");
        }
    }
}
