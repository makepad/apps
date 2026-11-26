//! The asset size scan: every live model of a store root, with
//! its native bounds, its calibration, its size category and real-world
//! range, the size Sandbox's drop-in gave it before the real-size rule
//! (`today`) and the size the rule gives it (`real`,
//! `makepad_asset_data::real_size`). Rows are sorted by how far `today` is
//! from the category's range.
//!
//!   cargo run --release -p makepad-asset-store --example asset_sizes -- <root> [--md <file>] [--check]
//!
//! `--md` writes the table as Markdown. `--check` lists every model whose
//! metadata needed a correction (`SIZECHECK drift`: an import or a game's
//! model published at other than its real size, which the rule brings
//! back) and exits 1 when a vehicle, a person or a weapon would still
//! spawn outside its category's range (`SIZECHECK FAIL`).
//!
//! Point it at a COPY of a live root (`cp -c -R`), never at one a server
//! is using: opening a root runs its migration, and a browse keeps the
//! search index's state.

use makepad_asset_data::real_size::{self, Calibration, Category};
use makepad_asset_data::{AssetAlias, AssetKind, AssetManifest};
use makepad_asset_store::{AssetServerCore, Budgets, SearchFilters, SearchQuery, SearchViewer, ViewerScope};
use std::fmt::Write as _;

/// Sandbox's drop-in before the real-size rule (view/dropin.rs `play_scale`
/// at 352d5561): a car not authored in metres was fitted to the stock
/// 3.4 m chassis, a plane outside 2..80 m to a 10 m span, a rig drawn at
/// its own units (x1.7 below 41 joints). Props went by Sandbox's pack table,
/// which this tool does not have: `None`.
fn today(drop: &str, metric: bool, extent: [f32; 3], joints: u64) -> Option<f32> {
    let len = extent[0].max(extent[2]).max(0.001);
    match drop {
        "vehicle" => Some(if metric { 1.0 } else { 3.4 / len }),
        "plane" => Some(if metric || (2.0..=80.0).contains(&len) { 1.0 } else { 10.0 / len }),
        "character" => Some(if metric || joints >= 41 { 1.0 } else { 1.7 }),
        _ => metric.then_some(1.0),
    }
}

struct Row {
    alias: String,
    kind: AssetKind,
    title: String,
    drop: &'static str,
    extent: [f32; 3],
    calibration: Calibration,
    category: Option<Category>,
    today: Option<f32>,
    real: real_size::RealSize,
    placeholder: bool,
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let root = std::path::PathBuf::from(args.get(1).expect("usage: asset_sizes <root> [--md <file>] [--check]"));
    let md = args.iter().position(|a| a == "--md").and_then(|i| args.get(i + 1)).cloned();
    let check = args.iter().any(|a| a == "--check");
    let core = AssetServerCore::open(&root, Budgets::default_v1()).expect("open root");
    let viewer = SearchViewer { principal: None, scope: ViewerScope::All };
    let mut rows = Vec::new();
    for kind in [AssetKind::Mesh, AssetKind::Character, AssetKind::Weapon, AssetKind::Vehicle, AssetKind::Prop, AssetKind::ModelProgram] {
        let filters = SearchFilters { kind: Some(kind), live_only: true, ..SearchFilters::default() };
        let query = SearchQuery { text: "", filters, expand: false, page_size: 100, newest: false, facets: 0 };
        let mut cursor = None;
        loop {
            let page = core.search().search(&query, &viewer, cursor.as_deref()).expect("browse");
            for hit in &page.hits {
                let Some(alias) = hit.alias.as_deref() else { continue };
                // The build cache is products by program hash, never browsed.
                if alias.starts_with("gen/built/") { continue; }
                let Ok(a) = AssetAlias::new(alias) else { continue };
                let Some(target) = core.catalog().resolve_asset_alias(&a).ok().flatten() else { continue };
                let Some(bytes) = core.catalog().asset_revision_manifest(&target.revision).ok().flatten() else { continue };
                let Ok(m) = AssetManifest::from_canonical_bytes(&bytes) else { continue };
                let tags = core.search().annotation(&hit.asset_id).ok().flatten().map(|n| n.tags).unwrap_or_default();
                let b = m.bounds;
                let extent = [b.max.x - b.min.x, b.max.y - b.min.y, b.max.z - b.min.z];
                // A publish that never measured its model leaves the unit cube.
                let placeholder = [b.min.x, b.min.y, b.min.z] == [-0.5; 3] && [b.max.x, b.max.y, b.max.z] == [0.5; 3];
                let calibration = Calibration::of_manifest(&m);
                let category = real_size::categorize(Some(m.kind), alias, &hit.title, &tags);
                let drop = match (m.kind, category) {
                    (AssetKind::Character, _) => "character",
                    (_, Some(Category::Plane | Category::Helicopter)) if m.kind == AssetKind::Vehicle || tags.iter().any(|t| t == "vehicle") => "plane",
                    (AssetKind::Vehicle, _) => "vehicle",
                    (_, Some(Category::Car | Category::Kart | Category::Van | Category::Truck | Category::Bus)) => "vehicle",
                    _ => "prop",
                };
                let today = today(drop, calibration == Calibration::Metric, extent, m.metrics.joints as u64);
                let real = real_size::real_size(calibration, category, extent);
                rows.push(Row { alias: alias.to_string(), kind: m.kind, title: hit.title.replace(['\t', '\n', '|'], " "), drop, extent, calibration, category, today, real, placeholder });
            }
            cursor = page.cursor;
            if cursor.is_none() { break; }
        }
    }
    let off_today = |r: &Row| r.today.map_or(1.0, |s| real_size::off_factor(r.category, r.extent, s));
    rows.sort_by(|a, b| off_today(b).total_cmp(&off_today(a)).then(a.alias.cmp(&b.alias)));

    let m = |v: f32| format!("{v:.2}");
    let size = |e: [f32; 3], s: f32| format!("{} x {} x {}", m(e[0] * s), m(e[1] * s), m(e[2] * s));
    let cal = |c: Calibration| match c {
        Calibration::Metric => "metric".to_string(),
        Calibration::Stated(k) => format!("{k:.4} m/u"),
        Calibration::Unknown => "unknown".to_string(),
    };
    let mut out = String::new();
    let categorized = rows.iter().filter(|r| r.category.is_some()).count();
    let off: Vec<&Row> = rows.iter().filter(|r| r.category.is_some() && off_today(r) > 1.0).collect();
    let _ = writeln!(out, "{} live models; {categorized} in a size category; {} of those dropped outside their category's real range before the rule.\n", rows.len(), off.len());
    let _ = writeln!(out, "| off x | alias | title | kind | drop | category: real range | native extent (x y z) | calibration | today (m) | real (m) | basis |");
    let _ = writeln!(out, "|---|---|---|---|---|---|---|---|---|---|---|");
    let mut failures = Vec::new();
    let mut drift = Vec::new();
    for r in &rows {
        let range = r.category.map(|c| c.range());
        let real_extent = size(r.extent, r.real.scale);
        // A vehicle, a person or a weapon must spawn inside its range:
        // anything else is the rule (or a path around it) failing.
        // (A rig whose units nothing states is drawn as authored: its
        // metadata is what is wrong, listed as drift below.)
        let unstated = r.real.basis == real_size::Basis::Unknown;
        let outside = real_size::check(r.category.filter(|c| c.corrects()), r.extent, r.real.scale);
        if let Some(problem) = outside.as_ref().filter(|_| !unstated) {
            failures.push(format!("{}: spawns as {problem}", r.alias));
        }
        // Metadata that needs the correction: an importer or a game that
        // published a model at other than its real size, or a rig whose
        // units nothing states and that is no person's size as authored.
        if matches!(r.real.basis, real_size::Basis::Corrected | real_size::Basis::Estimated) || (unstated && outside.is_some()) {
            drift.push(format!("{}: {}, spawns at {} m ({})", r.alias, r.category.map_or("-", |c| c.as_str()), size(r.extent, r.real.scale), r.real.basis.as_str()));
        }
        let _ = writeln!(
            out,
            "| {:.2} | `{}` | {} | {:?} | {} | {} | {}{} | {} | {} | {} | {} |",
            off_today(r),
            r.alias,
            r.title,
            r.kind,
            r.drop,
            r.category.zip(range).map_or("-".to_string(), |(c, g)| format!("{} {} {}..{}", c.as_str(), g.measure.as_str(), g.lo, g.hi)),
            size(r.extent, 1.0),
            if r.placeholder { " (unmeasured)" } else { "" },
            cal(r.calibration),
            r.today.map_or("pack table".to_string(), |s| size(r.extent, s)),
            real_extent,
            r.real.basis.as_str(),
        );
    }
    match md {
        Some(path) => std::fs::write(&path, &out).expect("write md"),
        None => print!("{out}"),
    }
    eprintln!(
        "{} models, {categorized} categorized, {} outside their range before the rule; {} sized by a correction, {} still outside",
        rows.len(), off.len(), drift.len(), failures.len()
    );
    if check {
        for d in &drift {
            eprintln!("SIZECHECK drift {d}");
        }
        for f in &failures {
            eprintln!("SIZECHECK FAIL {f}");
        }
        if !failures.is_empty() {
            std::process::exit(1);
        }
    }
}
