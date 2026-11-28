use makepad_solidsync::{rules::prepare, stage, table::RULES, SyncError};
use std::collections::BTreeSet;
use std::path::Path;
use std::time::Instant;

const USAGE: &str = "usage: solidsync sync --dry-run --no-fetch --pin <sha> --clone <dir>";

fn report_paths(label: &str, paths: &BTreeSet<String>, krate: &str) {
    println!("{label}: {}", paths.len());
    let prefix = format!("{krate}/");
    for path in paths { println!("  {}", path.strip_prefix(&prefix).unwrap_or(path)); }
}

fn sync() -> Result<(), SyncError> {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("sync") => {}
        Some("verify" | "report" | "patch") => return Err("not yet implemented".into()),
        _ => return Err(USAGE.into()),
    }
    let (mut dry_run, mut no_fetch) = (false, false);
    let (mut pin, mut clone) = (None, None);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--dry-run" if !dry_run => dry_run = true,
            "--no-fetch" if !no_fetch => no_fetch = true,
            "--pin" if pin.is_none() => pin = Some(args.next().ok_or(USAGE)?),
            "--clone" if clone.is_none() => clone = Some(args.next().ok_or(USAGE)?),
            _ => return Err(format!("unexpected option {arg}: {USAGE}")),
        }
    }
    if !dry_run || !no_fetch {
        return Err(format!("fetch and install are not yet implemented: {USAGE}"));
    }
    let start = Instant::now();
    let clone = clone.ok_or(USAGE)?;
    let clone = Path::new(&clone);
    let (pin, mut tree) = stage::stage(clone, &pin.ok_or(USAGE)?)?;
    let bytes: usize = tree.values().map(Vec::len).sum();
    println!("C1 staged {} files, {bytes} bytes in target/solidsync/{pin}/raw/", tree.len());
    println!("C2 denied paths: 0");
    let mut prepared = prepare(&tree, RULES)?;
    let fixtures = prepared.graphs.values().flat_map(|g| g.external.keys().cloned()).collect::<Vec<_>>();
    let blobs = stage::fixture_blobs(clone, &pin, &fixtures)?;
    prepared.apply_to(&mut tree, &blobs)?;
    for outcome in &prepared.outcomes {
        if outcome.rule == "S2" { continue; }
        println!("{} {}: {} removals in {} files, 0 refusals", outcome.rule, outcome.crate_, outcome.count, outcome.files.len());
    }
    for (krate, graph) in prepared.graphs {
        println!("S2 {krate}");
        report_paths("files deleted", &graph.deleted, krate);
        report_paths("kept and listed", &graph.listed, krate);
        report_paths("include targets kept", &graph.includes_kept, krate);
        report_paths("include targets deleted", &graph.includes_deleted, krate);
        report_paths("test-only include targets outside C1, verified at pin", &graph.external.into_keys().collect(), krate);
        println!("S3 keep list: {} files", graph.kept_by_pattern.len());
    }
    println!("remaining test-cfg tokens: {}", prepared.remaining_test_cfg);
    let bytes: usize = tree.values().map(Vec::len).sum();
    let lines: usize = tree.values().map(|b| b.iter().filter(|&&c| c == b'\n').count()
        + usize::from(!b.is_empty() && !b.ends_with(b"\n"))).sum();
    println!("total kept: {} files, {lines} lines, {bytes} bytes", tree.len());
    println!("elapsed {:.3} s", start.elapsed().as_secs_f64());
    Ok(())
}

fn main() {
    if let Err(error) = sync() {
        eprintln!("solidsync: {error}");
        std::process::exit(1);
    }
}
