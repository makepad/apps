use std::fmt::Write;
use makepad_solidsync::{install, notices, patch, summary::{self, Counts, Pin, Report}};
use makepad_solidsync::{build_script, manifest, rules::{parse_skips, prepare}, stage, table::{dest_root, Table, Tree}, SyncError};
use std::collections::BTreeSet;
use std::path::Path;
use std::time::Instant;

const USAGE: &str = "usage: solidsync sync [--dry-run] --no-fetch --pin <sha> --clone <dir> [--accept-build-rs <sha256>] [--dest <dir>] | verify [--dest <dir>] | patch save <NNNN> <name> | doc";

fn report_paths(label: &str, paths: &BTreeSet<String>, krate: &str) {
    println!("{label}: {}", paths.len());
    let prefix = format!("{krate}/");
    for path in paths { println!("  {}", path.strip_prefix(&prefix).unwrap_or(path)); }
}

fn line_count(bytes: &[u8]) -> usize {
    bytes.iter().filter(|&&b| b == b'\n').count() + usize::from(!bytes.is_empty() && !bytes.ends_with(b"\n"))
}

fn sync() -> Result<(), SyncError> {
    let mut args = std::env::args().skip(1);
    let root = install::checkout()?;
    let _lock = install::lock(&root)?;
    match args.next().as_deref() {
        Some("sync") => {}
        Some("doc") if args.next().is_none() => return makepad_solidsync::doc::write(),
        Some("patch") => return save_patch(&root, args),
        Some("verify") => {
            let alternate = match args.next().as_deref() {
                None => None,
                Some("--dest") => Some(args.next().ok_or(USAGE)?),
                _ => return Err(USAGE.into()),
            };
            if args.next().is_some() { return Err(USAGE.into()); }
            let dest = install::destination(&root, alternate.as_deref())?;
            if let Some(dir) = install::interrupted(&root.join("target/solidsync"), &dest)? {
                return Err(format!("incomplete install in {}; run sync to resume", dir.display()));
            }
            let count = install::verify(&dest, &Table::load()?.crates)?;
            println!("verify: {count} files match sync/manifest.txt");
            return Ok(());
        }
        Some("report") => return Err("not yet implemented".into()),
        _ => return Err(USAGE.into()),
    }
    let (mut dry_run, mut no_fetch) = (false, false);
    let mut accepted = None;
    let mut alternate = None;
    let (mut pin, mut clone) = (None, None);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--dry-run" if !dry_run => dry_run = true,
            "--no-fetch" if !no_fetch => no_fetch = true,
            "--accept-build-rs" if accepted.is_none() => accepted = Some(args.next().ok_or(USAGE)?),
            "--dest" if alternate.is_none() => alternate = Some(args.next().ok_or(USAGE)?),
            "--pin" if pin.is_none() => pin = Some(args.next().ok_or(USAGE)?),
            "--clone" if clone.is_none() => clone = Some(args.next().ok_or(USAGE)?),
            _ => return Err(format!("unexpected option {arg}: {USAGE}")),
        }
    }
    if !no_fetch { return Err(format!("use the local clone with --no-fetch: {USAGE}")); }
    let start = Instant::now();
    let clone = clone.ok_or(USAGE)?;
    let clone = Path::new(&clone);
    let requested = pin.ok_or(USAGE)?;
    let table = Table::load()?;
    let dest = install::destination(&root, alternate.as_deref())?;
    let stages = root.join("target/solidsync");
    if let Some(dir) = install::interrupted(&stages, &dest)? {
        if dry_run { return Err("pending install journal; run sync to resume".into()); }
        println!("resuming journaled install");
        print!("{}", install::resume(&dir, &dest, &table.crates)?);
        if dir.file_name().and_then(|n| n.to_str()) == Some(&stage::resolve(clone, &requested)?) { return Ok(()); }
    }
    let pin = stage::resolve(clone, &requested)?;
    let dir = stages.join(&pin);
    let before = install::snapshot(&dest, &table.crates, true)?;
    let previous = before.get(install::PIN).map(|data| Pin::read(data)).transpose()?;
    let patches = if alternate.is_some() && previous.is_none() && !dest.join("sync/patches").try_exists().map_err(|e| e.to_string())? {
        patch::load(&dest_root())?
    } else { patch::load(&dest)? };
    if let Some(old) = &previous {
        let expected = before.get(install::MANIFEST).ok_or("missing installed manifest.txt")?;
        if pin != old.commit {
            install::rederive(&old.commit, &pin, expected, |old_pin| {
                let resolved = stage::resolve(clone, old_pin)?;
                derive(clone, &resolved, &dest, &table, None, &patches, alternate.is_some(), false).map(|(tree, _)| tree)
            })?;
            install::verify(&dest, &table.crates)?;
            println!("re-derived installed pin {}", old.commit);
        }
    } else {
        if before.contains_key(install::MANIFEST) { return Err("manifest.txt exists without pin.toml".into()); }
        println!("first sync: nothing to re-derive");
    }
    let (mut after, report) = derive(clone, &pin, &dest, &table, accepted.as_deref(), &patches, alternate.is_some(), true)?;
    let (date, subject) = stage::metadata(clone, &pin)?;
    let metadata = Pin { commit: pin, date, subject, tool: env!("CARGO_PKG_VERSION").into(), inventory: report.inventory.clone() };
    after.insert(install::MANIFEST.into(), install::manifest(&after).into_bytes());
    after.insert(install::PIN.into(), metadata.encode()?);
    let summary = summary::summarize(&dir, &dest, &table, &before, &after, previous.as_ref(), &metadata, &report)?;
    if dry_run {
        std::fs::write(dir.join("summary.txt"), &summary).map_err(|e| format!("summary.txt: {e}"))?;
    } else {
        install::install(&dir, &dest, &table.crates, &before, &after, &summary)?;
    }
    print!("{summary}");
    println!("elapsed {:.3} s", start.elapsed().as_secs_f64());
    Ok(())
}

fn derive(clone: &Path, pin: &str, dest: &Path, table: &Table, accepted: Option<&str>, patches: &Tree,
    alternate: bool, persist: bool) -> Result<(Tree, Report), String> {
    let mut tree = stage::stage_at(clone, pin, table, persist)?;
    let mut counts = Counts::new();
    for id in ["C1", "C2", "C3", "B1", "D1", "S1", "S2", "S3", "S4", "M1", "F1", "I1", "I2", "A1", "N1"] {
        counts.insert(id.into(), 0);
    }
    counts.insert("C1".into(), tree.len());
    let mut inventory = Counts::new();
    let raw = notices::project(&tree, &table)?;
    let bytes: usize = tree.values().map(Vec::len).sum();
    println!("C1 staged {} files, {bytes} bytes in target/solidsync/{pin}/raw/", tree.len());
    println!("C2 denied paths: 0");
    let skips = std::fs::read_to_string(dest_root().join("sync/skip.txt")).map_err(|e| format!("skip.txt: {e}"))?;
    let mut prepared = prepare(&tree, &table, &parse_skips(&skips)?)?;
    let fixtures = prepared.graphs.values().flat_map(|g| g.external.keys().cloned()).collect::<Vec<_>>();
    let blobs = stage::fixture_blobs(clone, &pin, &fixtures)?;
    prepared.apply_to(&mut tree, &blobs)?;
    for outcome in &prepared.outcomes {
        *counts.entry(outcome.rule.into()).or_default() += outcome.count;
        if matches!(outcome.rule, "S2" | "O1") { continue; }
        println!("{} {}: {} removals in {} files, 0 refusals", outcome.rule, outcome.crate_, outcome.count, outcome.files.len());
    }
    println!("O1: {} orphans removed", counts["O1"]);
    for (krate, graph) in prepared.graphs {
        *counts.entry("S3".into()).or_default() += graph.kept_by_pattern.len();
        println!("S2 {krate}");
        report_paths("files deleted", &graph.deleted, &krate);
        report_paths("kept and listed", &graph.listed, &krate);
        report_paths("include targets kept", &graph.includes_kept, &krate);
        report_paths("include targets deleted", &graph.includes_deleted, &krate);
        report_paths("test-only include targets outside C1, verified at pin", &graph.external.into_keys().collect(), &krate);
        println!("S3 keep list: {} files", graph.kept_by_pattern.len());
    }
    for (krate, summary) in &prepared.summaries {
        println!("S1 {krate}: {} test fns kept, {} removed", summary.kept_tests, summary.removed_tests);
        println!("{} modules removed whole", summary.whole_modules.len());
        for (module, reason) in &summary.whole_modules { println!("  {module}: {reason}"); }
        println!("{} test fns removed individually by denied-token kind (a fn can use several):", summary.denied_tests);
        for (kind, count) in &summary.kinds { println!("  {kind}: {count}"); }
        println!("{} named removals", summary.named_removals.len());
        for site in &summary.named_removals { println!("  {site}"); }
        let mut cost: Vec<_> = summary.losses.iter().collect();
        cost.sort_by_key(|(module, count)| (std::cmp::Reverse(**count), *module));
        println!("Largest losses by module (top 10):");
        for (module, count) in cost.into_iter().take(10) { println!("  {count} {module}"); }
        println!("{} ignored tests removed", summary.ignored.len());
        for site in &summary.ignored { println!("  {site}"); }
        for site in &summary.skips { println!("  {site}"); }
        println!("{} statement-level test cfgs left verbatim", summary.statements);
        println!("0 helper candidates; 0 stale entries; 0 denied tokens in kept src/");
        println!("{} non-test denied-token sites", summary.production_denied.len());
        for site in summary.production_denied.iter().chain(&summary.sites) { println!("  {site}"); }
    }
    let build = build_script::check(&tree, table, dest, accepted,
        prepared.summaries.values().map(|s| &s.inventory))?;
    counts.insert("B1".into(), 1);
    println!("B1 {} SHA-256 {}\ninputs {} -> $OUT_DIR/{}\n{}", table.build.producer, build.hash, table.build.inputs, table.build.output, build.details.trim_end());
    for (kind, sites) in &build.sites { println!("  {kind} ({}): {}", sites.len(), sites.join(", ")); }
    let mut full_sites = String::new();
    let mut issues = Vec::new();
    for spec in &table.crates {
        let generated = manifest::generate(&tree, spec, &table, &pin)?;
        *counts.entry("C3".into()).or_default() += 1;
        *counts.entry("M1".into()).or_default() += 1;
        println!("C3 {}: license and staged notices verified", spec.upstream);
        tree.insert(format!("{}/Cargo.toml", spec.upstream), generated.into_bytes());
        let prefix = format!("{}/", spec.upstream);
        let (mut files, mut lines, mut bytes) = (0usize, 0usize, 0usize);
        for (_, data) in tree.range(prefix.clone()..).take_while(|(p, _)| p.starts_with(&prefix)) {
            files += 1;
            lines += line_count(data);
            bytes += data.len();
        }
        println!("{} kept: {files} files, {lines} lines, {bytes} bytes", spec.upstream);
        let inv = &prepared.summaries[&spec.upstream].inventory;
        *counts.entry("F1".into()).or_default() += inv.forbid;
        for (kind, n) in &inv.i1 {
            *inventory.entry(format!("I1 {kind}")).or_default() += n;
            *counts.entry("I1".into()).or_default() += n;
        }
        for (kind, sites) in &inv.i2 {
            *inventory.entry(format!("I2 {kind}")).or_default() += sites.len();
            *counts.entry("I2".into()).or_default() += sites.len();
        }
        if inv.forbid != 1 { issues.push(format!("F1 {}/src/lib.rs: expected exactly 1, found {}", spec.upstream, inv.forbid)); }
        println!("F1 {}/src/lib.rs: {} forbid(unsafe_code)", spec.upstream, inv.forbid);
        println!("I1 {} (production and tests)", spec.upstream);
        for (kind, n) in &inv.i1 { println!("  {kind}: {n}"); }
        for helper in &inv.shim_helpers { println!("  kept test helper outside serde shim: {helper}"); }
        println!("I2 {} (production)", spec.upstream);
        for (kind, sites) in &inv.i2 {
            println!("  {kind}: {}", sites.len());
            for (i, site) in sites.iter().enumerate() {
                if i < 10 { println!("    {site}"); }
                writeln!(full_sites, "{kind}\t{site}").unwrap();
            }
        }
        if let Err(error) = inv.check(&table.inventory) { issues.push(error); }
    }
    let dir = Path::new("target/solidsync").join(&pin);
    if persist {
        let tmp = dir.join(format!("i2.{}.tmp", std::process::id()));
        let mut output = std::fs::File::create_new(&tmp).map_err(|e| format!("I2 {}: {e}", tmp.display()))?;
        std::io::Write::write_all(&mut output, full_sites.as_bytes()).map_err(|e| format!("I2: {e}"))?;
        drop(output);
        std::fs::rename(&tmp, dir.join("i2.txt")).map_err(|e| format!("I2: {e}"))?;
    }
    if !issues.is_empty() { return Err(issues.join("\n")); }
    let mut changes = notices::rule_changes(&table, &prepared.outcomes);
    let mut files = notices::project(&tree, &table)?;
    let section = std::fs::read_to_string(dest_root().join(&table.attribution_section))
        .map_err(|e| format!("A1 {}: {e}", table.attribution_section))?;
    let a1 = notices::attribution(&mut files, &table, &section, &mut changes)?;
    println!("A1: {a1} attribution sections appended");
    if persist { patch::stage_ruled(&dir, &files)?; }
    let applied = patch::run(&dir, &pin, &table.crates, &mut files, &patches, &mut changes,
        |path| patch::installed_file(dest, alternate, path))?;
    println!("patches applied: {:?}; refreshed: {:?}", applied.applied, applied.refreshed);
    let n1 = notices::apply(&raw, &mut files, &changes)?;
    println!("N1: {n1} files given a modification notice");
    for spec in &table.crates {
        let path = format!("{}/Cargo.toml", spec.dest);
        let data = files.get(&path).ok_or_else(|| format!("patch queue removed required {path}"))?;
        println!("M1 {path}\n{}", std::str::from_utf8(data).map_err(|e| e.to_string())?);
    }
    let bytes: usize = files.values().map(Vec::len).sum();
    let lines: usize = files.values().map(|data| line_count(data)).sum();
    println!("total kept: {} crate files, {lines} lines, {bytes} bytes", files.len());
    counts.insert("A1".into(), a1);
    counts.insert("N1".into(), n1);
    for (name, bytes) in applied.patches { files.insert(format!("sync/patches/{name}"), bytes); }
    Ok((files, Report { rules: counts, inventory, applied: applied.applied, refreshed: applied.refreshed }))
}

fn save_patch(root: &Path, mut args: impl Iterator<Item=String>) -> Result<(), String> {
    if args.next().as_deref() != Some("save") { return Err("expected patch save <NNNN> <name>".into()); }
    let number = args.next().ok_or("expected patch number")?;
    let name = args.next().ok_or("expected patch name")?;
    let mut pin = None;
    let mut alternate = None;
    let mut info = patch::Save { number: &number, name: &name, why: None, guard: None, upstreamable: None };
    while let Some(key) = args.next() {
        let value = args.next().ok_or_else(|| format!("missing value for {key}"))?;
        match key.as_str() {
            "--pin" if pin.is_none() => pin = Some(value),
            "--dest" if alternate.is_none() => alternate = Some(value),
            "--why" if info.why.is_none() => info.why = Some(value),
            "--guard" if info.guard.is_none() => info.guard = Some(value),
            "--upstreamable" if info.upstreamable.is_none() => info.upstreamable = Some(value),
            _ => return Err(format!("unexpected option {key}")),
        }
    }
    let dest = install::destination(root, alternate.as_deref())?;
    let pin = match pin {
        Some(p) => p,
        None => Pin::read(&std::fs::read(dest.join(install::PIN)).map_err(|e| format!("pin.toml: {e}; provide --pin for a first sync"))?)?.commit,
    };
    println!("{}", patch::save(&Path::new("target/solidsync").join(&pin), &dest, &pin, &Table::load()?.crates, info,
        |path| patch::installed_file(&dest, alternate.is_some(), path))?);
    Ok(())
}

fn main() {
    if let Err(error) = sync() {
        eprintln!("solidsync: {error}");
        std::process::exit(1);
    }
}
