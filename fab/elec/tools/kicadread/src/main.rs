//! Sequential board-reader timings, with optional board engines and import reports.

use fab_elec_board::{connect, drc, fill_all, resolve_rules, BoardOp, RuleSource, IndexChange, PieceIndex};
use fab_elec_board::zone::{neck_census, NeckCensus, ZoneFill};
use fab_elec_route::check::{check, State};
use fab_elec_kicad::{read_kicad_board, KicadSources, GapKind, ImportReport, Refusal};
use fab_elec_core::{Sel, Severity};
use fab_elec_lang::{evaluate, std_libs, SourceRange, Sources, UnitState};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
    time::Instant,
};

struct AreaCheck {
    patterns: Vec<String>,
    min: f64,
}

impl AreaCheck {
    fn describe(&self, board: &fab_elec_board::Board, findings: &fab_elec_board::DrcResult, line: &mut String) {
        use fab_elec_kicad::{CheckKind, Measure};
        let areas: Vec<_> = board.keepouts.iter().filter(|area| {
            self.patterns.iter().any(|pattern| fab_elec_kicad::dru::wildcard_matches(pattern, &area.reason))
        }).collect();
        let (mut total, mut inside, mut inside_ok) = (0, 0, 0);
        let mut names = BTreeMap::<&str, usize>::new();
        for finding in &findings.violations {
            if finding.kind != CheckKind::Clearance { continue; }
            total += 1;
            if finding.at.is_some_and(|at| areas.iter().any(|area| area.area.contains(at))) {
                inside += 1;
                if matches!(finding.actual, Some(Measure::Len(actual)) if actual as f64 >= self.min) { inside_ok += 1; }
                *names.entry(&finding.rule).or_default() += 1;
            }
        }
        line.push_str(&format!("\n  area_check total={total} inside={inside} inside_ok={inside_ok} outside={}", total - inside));
        for (name, count) in names { line.push_str(&format!("\n  area_check rule={name:?} inside={count}")); }
        // Every clearance finding by the kinds of its two items and the clearance it asked for,
        // with the range of what was measured: says which rule or model difference they share.
        let kind = |sel: &fab_elec_core::Sel| match sel {
            fab_elec_core::Sel::Item(_, id) => board.locate(*id).map_or("item", |(kind, _)| match kind {
                fab_elec_board::ItemKind::Placed => "placed", fab_elec_board::ItemKind::Track => "track",
                fab_elec_board::ItemKind::Via => "via", fab_elec_board::ItemKind::Zone => "zone",
                fab_elec_board::ItemKind::Hole => "hole", _ => "other",
            }),
            fab_elec_core::Sel::Pin(_) => "pad",
            _ => "other",
        };
        let mut groups = BTreeMap::<(String, i64), (usize, i64, i64)>::new();
        for finding in findings.violations.iter().filter(|f| f.kind == CheckKind::Clearance) {
            let mut kinds: Vec<_> = finding.items.iter().map(kind).collect();
            kinds.sort_unstable();
            let required = match finding.required { Some(Measure::Len(v)) => i64::from(v), _ => -1 };
            let actual = match finding.actual { Some(Measure::Len(v)) => i64::from(v), _ => -1 };
            let entry = groups.entry((kinds.join("/"), required)).or_insert((0, i64::MAX, i64::MIN));
            entry.0 += 1;
            entry.1 = entry.1.min(actual);
            entry.2 = entry.2.max(actual);
        }
        for ((pair, required), (count, low, high)) in groups {
            line.push_str(&format!("\n  clearance_by pair={pair} required_um={} count={count} actual_um={}..{}",
                required / 1000, low / 1000, high / 1000));
        }
        // Each finding more than 1 µm short, with its items' nets, to tell a model gap from a fault.
        let net_of = |sel: &fab_elec_core::Sel| -> String {
            let fab_elec_core::Sel::Item(_, id) = sel else { return format!("{sel:?}") };
            let net = match board.locate(*id) {
                Some((fab_elec_board::ItemKind::Track, i)) => Some(board.tracks[i].net),
                Some((fab_elec_board::ItemKind::Via, i)) => Some(board.vias[i].net),
                Some((fab_elec_board::ItemKind::Zone, i)) => board.zones[i].net,
                _ => None,
            };
            let name = net.and_then(|n| board.nets.get(n.ix())).map_or("-", |n| n.name.as_str());
            // A track has one layer, which is the finding's.
            let layer = match board.locate(*id) {
                Some((fab_elec_board::ItemKind::Track, i)) => format!("@{:?}", board.tracks[i].layer),
                _ => String::new(),
            };
            format!("{}:{name}{layer}", kind(sel))
        };
        for finding in findings.violations.iter().filter(|f| f.kind == CheckKind::Clearance) {
            let (Some(Measure::Len(required)), Some(Measure::Len(actual))) = (finding.required, finding.actual) else { continue };
            if required - actual <= 1000 { continue; }
            let items: Vec<_> = finding.items.iter().map(net_of).collect();
            let at = finding.at.map_or(String::new(), |p| format!("({:.3},{:.3})", p.x as f64 * 1e-6, p.y as f64 * 1e-6));
            line.push_str(&format!("\n  clearance_item at={at} required_um={} actual_um={} items={}",
                required / 1000, actual / 1000, items.join(" | ")));
        }
    }
}

#[derive(Default)]
struct Count {
    files: u64,
    count: u64,
}

#[derive(Default)]
struct Totals {
    files: u64,
    read: u64,
    refused: u64,
    parse_ms: f64,
    build_ms: f64,
    gaps: HashMap<(String, &'static str), Count>,
    unknown: HashMap<String, Count>,
    necks: NeckCensus,
}

impl Totals {
    fn necks(&mut self, fills: &[ZoneFill], line: &mut String) {
        for fill in fills {
            for layer in &fill.layers {
                let census = neck_census(fill.lattice, layer);
                self.necks.f_islands += census.f_islands;
                self.necks.width_islands += census.width_islands;
                self.necks.neck_islands += census.neck_islands;
                self.necks.splitting += census.splitting;
                self.necks.foreign += census.foreign;
                self.necks.loneless += census.loneless;
                if census.neck_islands == 0 {
                    continue;
                }
                line.push_str(&format!(
                    "\nnecks zone={} layer={} f={} width={} necks={} splitting={} foreign={} loneless={}",
                    fill.zone.0, layer.layer.name(), census.f_islands, census.width_islands,
                    census.neck_islands, census.splitting, census.foreign, census.loneless,
                ));
                for site in census.sites.iter().take(20) {
                    let b = site.bbox;
                    line.push_str(&format!(
                        "\nneck at=({:.6},{:.6})-({:.6},{:.6})mm width_islands={} own={}",
                        b.min.x as f64 * 1e-6, b.min.y as f64 * 1e-6,
                        b.max.x as f64 * 1e-6, b.max.y as f64 * 1e-6, site.width_islands, site.own,
                    ));
                }
            }
        }
    }

    fn report(&mut self, report: &ImportReport) {
        for gap in report.gaps.values() {
            let kind = match gap.kind {
                GapKind::Skipped => "Skipped",
                GapKind::Approximated => "Approximated",
                GapKind::Dropped => "Dropped",
            };
            let count = self.gaps.entry((gap.feature.clone(), kind)).or_default();
            count.files += 1;
            count.count += u64::from(gap.count);
        }
        for (path, unknown) in &report.unknown {
            let count = self.unknown.entry(path.clone()).or_default();
            count.files += 1;
            count.count += u64::from(unknown.count);
        }
    }

    fn print(&self) {
        println!(
            "kicadread total files={} read={} refused={} parse_ms={:.3} build_ms={:.3}",
            self.files, self.read, self.refused, self.parse_ms, self.build_ms,
        );
        let mut gaps: Vec<_> = self.gaps.iter().collect();
        gaps.sort_by(|a, b| {
            b.1.files.cmp(&a.1.files)
                .then(b.1.count.cmp(&a.1.count))
                .then(a.0.cmp(b.0))
        });
        for ((feature, kind), count) in gaps {
            println!(
                "kicadread gap {feature} kind={kind} files={} count={}",
                count.files, count.count,
            );
        }
        let mut unknown: Vec<_> = self.unknown.iter().collect();
        unknown.sort_by(|a, b| {
            b.1.files.cmp(&a.1.files)
                .then(b.1.count.cmp(&a.1.count))
                .then(a.0.cmp(b.0))
        });
        for (path, count) in unknown {
            println!("kicadread unknown {path} files={} count={}", count.files, count.count);
        }
    }
}

fn files_in(root: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    let mut visited = HashSet::new();
    while let Some(dir) = pending.pop() {
        // Follow directory links once, including links back to an ancestor.
        let entries = fs::canonicalize(&dir).and_then(|canonical| {
            if visited.insert(canonical) {
                fs::read_dir(&dir).map(Some)
            } else {
                Ok(None)
            }
        });
        let entries = match entries {
            Ok(Some(entries)) => entries,
            Ok(None) => continue,
            Err(e) => {
                eprintln!("kicadread cannot walk file={}: {e}", dir.strip_prefix(root).unwrap_or(&dir).display());
                continue;
            }
        };
        let mut paths = Vec::new();
        for entry in entries {
            match entry {
                Ok(entry) => paths.push(entry.path()),
                Err(e) => eprintln!("kicadread cannot walk file={}: {e}", dir.strip_prefix(root).unwrap_or(&dir).display()),
            }
        }
        paths.sort();
        for path in paths.into_iter().rev() {
            if path.is_dir() {
                pending.push(path);
            } else if path.extension().is_some_and(|ext| ext == "kicad_pcb") {
                files.push(path);
            }
        }
    }
    files.sort();
    files
}

fn companion(path: &Path, extension: &str) -> Option<Vec<u8>> {
    match fs::read(path.with_extension(extension)) {
        Ok(bytes) => Some(bytes),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => { eprintln!("kicadread cannot read companion {extension}: {e}"); None },
    }
}

/// Where an evaluation's time goes, measured from outside the language, each step on its own and
/// best of two (so first-use JIT compiles stay out): the standard libraries alone, the part
/// documents' load on top of them, the board document's syntax parse and its whole lowering, and
/// the converter's own steps.
fn eval_split(stem: &str, project: &fab_elec_kicad::splash::SplashProject, pcb_lower: &dyn Fn() -> bool) {
    let libs = std_libs();
    let best = |f: &mut dyn FnMut() -> Option<f64>| f().unwrap_or(f64::NAN).min(f().unwrap_or(f64::NAN));
    let timed = |f: &mut dyn FnMut() -> bool| {
        let start = Instant::now();
        f().then(|| start.elapsed().as_secs_f64() * 1000.0)
    };
    let libs_ms = |parts: bool| -> Option<f64> {
        let mut s = Sources::new();
        s.add(format!("{stem}.ckt.splash"), "use mod.elec.*\nlet e = mod.elec\n".to_string()).ok()?;
        if parts {
            for (name, text) in project.docs.iter().skip(2) {
                s.add(name.clone(), text.clone()).ok()?;
            }
        }
        let e = evaluate(&s, &libs).ok()?;
        Some(e.units.first()?.phases?.libs.as_secs_f64() * 1000.0)
    };
    let std_ms = best(&mut || libs_ms(false));
    let parts_ms = best(&mut || libs_ms(true)) - std_ms;
    let pcb_text = project.docs.get(1).map_or("", |(_, text)| text.as_str());
    let board_units = fab_elec_lang::board_doc::units();
    let pcb_parse_ms = best(&mut || timed(&mut || makepad_script::syntax::parse_with(pcb_text, &board_units).is_ok()));
    let pcb_lower_ms = best(&mut || timed(&mut || pcb_lower()));
    println!(
        "splash {stem} split std_libs_ms={std_ms:.1} parts_ms={parts_ms:.1} pcb_parse_ms={pcb_parse_ms:.1} pcb_lower_ms={pcb_lower_ms:.1} convert_parts_ms={:.1} convert_circuit_ms={:.1} convert_board_ms={:.1}",
        project.parts_ms, project.circuit_ms, project.board_ms,
    );
}

fn write_splash(pcb: &fab_elec_kicad::PcbRead, stem: &str, dir: &Path, engines: bool) -> Result<(), String> {
    let start = Instant::now();
    let project = fab_elec_kicad::splash::to_splash(pcb, stem)?;
    let convert_ms = start.elapsed().as_secs_f64() * 1000.0;
    let folder = dir.join(stem);
    let mut sources = Sources::new();
    for (name, text) in &project.docs {
        let path = folder.join(name);
        if let Some(parent) = path.parent() { fs::create_dir_all(parent).map_err(|e| format!("{name}: {e}"))?; }
        fs::write(&path, text).map_err(|e| format!("{name}: {e}"))?;
        sources.add(name.clone(), text.clone()).map_err(|d| d.message)?;
    }
    let ckt_bytes = project.docs.first().map_or(0, |(_, text)| text.len());
    let pcb_text = project.docs.get(1).map_or("", |(_, text)| text.as_str());
    let part_bytes: usize = project.docs.iter().skip(2).map(|(_, text)| text.len()).sum();
    let total_bytes = ckt_bytes + pcb_text.len() + part_bytes;
    let libs = std_libs();
    let start = Instant::now();
    let result = evaluate(&sources, &libs);
    let eval_ms = start.elapsed().as_secs_f64() * 1000.0;
    // A second evaluation of the same texts in the same process: the first one also pays for
    // code the JIT compiles on first use, this one for the evaluation alone.
    let start = Instant::now();
    let again = evaluate(&sources, &libs);
    let eval2_ms = start.elapsed().as_secs_f64() * 1000.0;
    let phases2 = again.as_ref().ok().and_then(|e| e.units.first()).and_then(|u| u.phases).unwrap_or_default();
    drop(again);
    let (evaluated, diags) = match &result {
        Ok(e) => (Some(e), e.units.iter().flat_map(|u| u.state.diags()).chain(&e.notes).chain(&e.system.diags).collect::<Vec<_>>()),
        Err(d) => (None, d.iter().collect()),
    };
    let unit = evaluated.and_then(|e| e.units.first());
    let phases = unit.and_then(|u| u.phases).unwrap_or_default();
    let took = unit.map_or(0.0, |u| u.took.as_secs_f64() * 1000.0);
    let state = match unit.map(|u| &u.state) {
        Some(UnitState::Built) => "built", Some(UnitState::Kept(_)) => "kept", _ => "failed",
    };
    println!(
        "splash {stem} ckt_bytes={ckt_bytes} pcb_bytes={} parts={} part_bytes={part_bytes} total_bytes={total_bytes} pcb_lines={} convert_ms={convert_ms:.3} eval_ms={eval_ms:.3} took_ms={took:.3} read_ms={:.3} libs_ms={:.3} run_ms={:.3} lower_ms={:.3} state={state} diags={}",
        pcb_text.len(), project.docs.len().saturating_sub(2), pcb_text.lines().count(),
        phases.read.as_secs_f64() * 1000.0, phases.libs.as_secs_f64() * 1000.0,
        phases.run.as_secs_f64() * 1000.0, phases.lower.as_secs_f64() * 1000.0, diags.len(),
    );
    println!(
        "splash {stem} again eval_ms={eval2_ms:.3} read_ms={:.3} libs_ms={:.3} run_ms={:.3} lower_ms={:.3}",
        phases2.read.as_secs_f64() * 1000.0, phases2.libs.as_secs_f64() * 1000.0,
        phases2.run.as_secs_f64() * 1000.0, phases2.lower.as_secs_f64() * 1000.0,
    );
    // What the evaluation made, as hashes a change that must not alter it is checked against: the
    // circuit and board in canonical form, and every source site the map holds (sorted).
    if let (Some(e), Some(design_unit)) = (evaluated, evaluated.and_then(|e| e.design.units.first())) {
        use fab_elec_core::canon::Canon;
        use std::hash::{Hash, Hasher};
        let mut sites: Vec<String> = sources.docs().iter().flat_map(|d| e.map.of_doc(d.id)).map(|entry| format!("{entry:?}")).collect();
        sites.extend(e.map.value_sites_of(design_unit.id).iter().map(|entry| format!("{entry:?}")));
        sites.extend(e.map.pin_sites_of(design_unit.id).iter().map(|entry| format!("{entry:?}")));
        sites.sort();
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        sites.hash(&mut hasher);
        println!(
            "splash {stem} hashes circuit={} board={} sites={} map={:016x}",
            design_unit.circuit.content_hash().hex(),
            design_unit.board.as_deref().map_or_else(|| "-".to_string(), |b| b.content_hash().hex()),
            sites.len(), hasher.finish(),
        );
    }
    if let Some(design_unit) = evaluated.and_then(|e| e.design.units.first()) {
        let doc = sources.find(&format!("{stem}.pcb.splash"));
        let lower = || doc.is_some_and(|doc| fab_elec_lang::board_doc::lower(doc, pcb_text, &design_unit.circuit, None).is_ok());
        eval_split(stem, &project, &lower);
    }
    let counts = |board: &fab_elec_board::Board| -> [usize; 12] {
        [board.placed.len(), board.placed.iter().map(|p| p.footprint.pads.len()).sum(),
         board.tracks.len(), board.tracks.iter().map(|t| t.path.segs.len()).sum(),
         board.vias.len(), board.zones.len(), board.keepouts.len(), board.holes.len(),
         board.cutouts.len(), board.graphics.len(), board.texts.len(), board.nets.len()]
    };
    let imported = counts(&pcb.board);
    let built = evaluated.and_then(|e| e.design.units.first()).and_then(|u| u.board.as_deref()).map_or([0; 12], counts);
    let names = ["placed", "pads", "tracks", "segs", "vias", "zones", "keepouts", "holes", "cutouts", "graphics", "texts", "nets"];
    let items: Vec<_> = names.iter().zip(imported).zip(built).map(|((name, a), b)| format!("{name}={a}/{b}")).collect();
    println!("splash {stem} items {}", items.join(" "));
    let mut losses: Vec<_> = project.losses.iter().map(|(key, count)| format!("{key}={count}")).collect();
    // References the language renumbered: known only once the project is evaluated.
    if let Some(design_unit) = evaluated.and_then(|e| e.design.units.first()) {
        losses.push(format!("comp.refdes_changed={}", fab_elec_kicad::splash::refdes_changes(&design_unit.circuit)));
    }
    println!("splash {stem} losses {}", losses.join(" "));
    for diag in diags.iter().take(5) {
        let locations: Vec<_> = diag.sel.iter().filter_map(|s| match s {
            Sel::Source { doc, start, end } => Some(sources.describe(&libs, &SourceRange { doc: *doc, span: *start..*end })),
            _ => None,
        }).collect();
        println!("splash {stem} diag {}: {}", if locations.is_empty() { "project".into() } else { locations.join(", ") }, diag.message);
    }
    if state == "failed" { return Err("Splash project evaluation failed".into()); }
    if engines {
        // The round trip's effect on the checks: the same engines over the evaluated board, which
        // has no custom-rule source (the documents keep custom rules but nothing evaluates them).
        if let Some(board) = evaluated.and_then(|e| e.design.units.first()).and_then(|u| u.board.as_deref()) {
            let rules = resolve_rules(board, None);
            let fills = fill_all(board, &rules);
            let conn = connect(board, &fills);
            let findings = drc(board, &conn, &rules, &fills, None);
            let mut kinds: BTreeMap<String, usize> = BTreeMap::new();
            for violation in &findings.violations {
                let name = format!("{:?}", violation.kind);
                *kinds.entry(name.split(['(', ' ', '{']).next().unwrap_or_default().to_string()).or_default() += 1;
            }
            let names: Vec<String> = kinds.iter().map(|(kind, n)| format!("{kind}={n}")).collect();
            println!("splash {stem} engines rats={} findings={} drc {}", conn.ratsnest.len(), findings.violations.len(), names.join(" "));
        }
    }
    Ok(())
}

fn read_file(path: &Path, label: &Path, engines: bool, necks: bool, report: bool, splash: Option<&Path>, area_check: Option<&AreaCheck>, totals: &mut Totals) -> Result<(), Refusal> {
    let start = Instant::now();
    let bytes = fs::read(path).map_err(|e| Refusal::new((0, 0), e.to_string()))?;
    let project = companion(path, "kicad_pro");
    let custom = companion(path, "kicad_dru");
    let read_ms = start.elapsed().as_secs_f64() * 1000.0;
    let name = path.file_stem().unwrap_or_default().to_string_lossy();
    let start = Instant::now();
    let mut pcb = match read_kicad_board(&KicadSources { board: &bytes, project: project.as_deref(), rules: custom.as_deref(), name: &name }) {
        Ok(pcb) => pcb,
        Err(error) => { totals.parse_ms += start.elapsed().as_secs_f64() * 1000.0; return Err(error); }
    };
    let parse_ms = pcb.parse_time.as_secs_f64() * 1000.0;
    let build_ms = pcb.build_time.as_secs_f64() * 1000.0;
    totals.parse_ms += parse_ms;
    totals.build_ms += build_ms;
    let project = if pcb.report.gaps.contains_key("project.refused") { "refused" } else if project.is_some() { "yes" } else { "no" };
    let rules = if custom.is_some() { "yes" } else { "no" };
    totals.read += 1;
    let board = &pcb.board;
    let pads: usize = board.placed.iter().map(|placed| placed.footprint.pads.len()).sum();
    let segs: usize = board.tracks.iter().map(|track| track.path.segs.len()).sum();
    // Gap and unknown counts here are distinct features; the tables count their occurrences.
    let mut line = format!(
        "kicadread file={} bytes={} version={} project={project} rules={rules} classes={} read_ms={read_ms:.3} parse_ms={parse_ms:.3} build_ms={build_ms:.3} placed={} pads={pads} tracks={} segs={segs} vias={} zones={} keepouts={} outline={} gaps={} unknown={}",
        label.display(), bytes.len(), pcb.report.version, board.classes.len(), board.placed.len(), board.tracks.len(),
        board.vias.len(), board.zones.len(), board.keepouts.len(), board.outline.len(),
        pcb.report.gaps.len(), pcb.report.unknown.len(),
    );
    line.push_str(&format!(" custom={} mapped={}",
        pcb.report.read.get("rules.custom").copied().unwrap_or(0),
        pcb.report.made.get("rules.custom").copied().unwrap_or(0)));
    if engines || necks || area_check.is_some() {
        let start = Instant::now();
        let rules = resolve_rules(board, pcb.rules.as_ref().map(|s| s as &dyn RuleSource));
        let rules_ms = start.elapsed().as_secs_f64() * 1000.0;
        let start = Instant::now();
        let fills = fill_all(board, &rules);
        let fill_ms = start.elapsed().as_secs_f64() * 1000.0;
        // Collect the census after filling, outside the fill timing and only when requested.
        let mut neck_lines = String::new();
        if necks {
            totals.necks(&fills, &mut neck_lines);
        }
        let fill_polys: usize = fills.iter().flat_map(|fill| &fill.layers)
            .flat_map(|layer| layer.kept()).map(|(_, poly)| poly.rings().count()).sum();
        let start = Instant::now();
        let conn = connect(board, &fills);
        let conn_ms = start.elapsed().as_secs_f64() * 1000.0;
        let start = Instant::now();
        let findings = drc(board, &conn, &rules, &fills, None);
        let drc_ms = start.elapsed().as_secs_f64() * 1000.0;
        let findings_kept = findings.violations.iter().filter(|v| {
            !pcb.severities.iter().any(|(kind, severity)| *kind == v.kind && severity.is_none())
        }).count();
        // Vias whose zone-connected layers differ from the file's own fill: the import's fidelity
        // measure for per-layer via rings.
        let zone_flash_differs = pcb.zone_flash.compare(board, &fills, &mut pcb.report);
        line.push_str(&format!(
            " rules_ms={rules_ms:.3} fill_ms={fill_ms:.3} fill_polys={fill_polys} conn_ms={conn_ms:.3} rats={} drc_ms={drc_ms:.3} findings={} findings_kept={findings_kept} zone_flash_differs={zone_flash_differs}",
            conn.ratsnest.len(), findings.violations.len(),
        ));
        if let Some(area) = area_check {
            area.describe(board, &findings, &mut line);
        }
        // Findings by kind: on a board its own tool passes, most of them name what the import lost.
        let mut kinds: BTreeMap<String, usize> = BTreeMap::new();
        for violation in &findings.violations {
            let name = format!("{:?}", violation.kind);
            let name = name.split(['(', ' ', '{']).next().unwrap_or_default().to_string();
            let severity = match pcb.severities.iter().find(|(kind, _)| *kind == violation.kind) {
                Some((_, Some(Severity::Error))) => "error",
                Some((_, Some(_))) => "warning",
                Some((_, None)) => "ignore",
                None => "ours",
            };
            *kinds.entry(format!("{name}[{severity}]")).or_default() += 1;
        }
        let names: Vec<String> = kinds.iter().map(|(kind, n)| format!("{kind}={n}")).collect();
        line.push_str(&format!("\n  drc {}", names.join(" ")));
        // The first finding of each kind, to tell an import loss from a real fault.
        let mut shown = HashSet::new();
        for violation in &findings.violations {
            let name = format!("{:?}", violation.kind);
            let name = name.split(['(', ' ', '{']).next().unwrap_or_default().to_string();
            if shown.insert(name.clone()) {
                let at = violation.at.map(|p| format!(" at=({:.3},{:.3})mm", p.x as f64 * 1e-6, p.y as f64 * 1e-6)).unwrap_or_default();
                line.push_str(&format!("\n  drc.first {name}{at} rule={} {}", violation.rule, violation.message));
            }
        }
        // One router check sample: the gate every route proposal passes, here for removing the
        // middle track of the file (refill of the pours it reaches, index, connectivity, findings).
        if let Some(track) = board.tracks.get(board.tracks.len() / 2) {
            let start = Instant::now();
            let index = PieceIndex::build(board, &fills);
            let index_ms = start.elapsed().as_secs_f64() * 1000.0;
            let since = IndexChange::all();
            let state = State { rules: &rules, fills: &fills, index: &index, conn: &conn, violations: &findings.violations, since: &since };
            let mut changed = board.clone();
            let start = Instant::now();
            let outcome = check(&mut changed, &[BoardOp::Remove(track.id)], &state);
            let check_ms = start.elapsed().as_secs_f64() * 1000.0;
            match outcome {
                Ok(outcome) => line.push_str(&format!(
                    "\n  check index_ms={index_ms:.3} remove_track_ms={check_ms:.3} findings={} new={}",
                    outcome.violations.len(), outcome.new.len(),
                )),
                Err(e) => line.push_str(&format!("\n  check index_ms={index_ms:.3} refused={e:?}")),
            }
        }
        line.push_str(&neck_lines);
    }
    // After the engines, so the totals count what the fill comparison adds.
    totals.report(&pcb.report);
    println!("{line}");
    if report {
        for line in pcb.report.render().lines() {
            println!("  {line}");
        }
    }
    if let Some(dir) = splash {
        write_splash(&pcb, &name, dir, engines).map_err(|e| Refusal::new((0, 0), e))?;
    }
    Ok(())
}

fn main() {
    let mut engines = false;
    let mut necks = false;
    let mut area_patterns: Option<Vec<String>> = None;
    let mut area_min = None;
    let mut report = false;
    let mut splash = None;
    let mut positional = false;
    let mut arguments = Vec::new();
    let mut args = std::env::args_os().skip(1);
    while let Some(arg) = args.next() {
        if !positional && arg == "--engines" {
            engines = true;
        } else if !positional && arg == "--necks" {
            necks = true;
        } else if !positional && arg == "--report" {
            report = true;
        } else if !positional && arg == "--splash" {
            let Some(dir) = args.next().filter(|v| !v.to_string_lossy().starts_with("--")) else {
                eprintln!("kicadread --splash needs a directory");
                std::process::exit(2);
            };
            splash = Some(PathBuf::from(dir));
        } else if !positional && arg == "--area-check" {
            let Some(patterns) = args.next().and_then(|s| s.into_string().ok())
                .filter(|s| !s.starts_with("--") && s.split(',').all(|p| !p.is_empty())) else {
                eprintln!("kicadread --area-check needs comma-separated patterns");
                std::process::exit(2);
            };
            area_patterns = Some(patterns.split(',').map(str::to_owned).collect());
        } else if !positional && arg == "--area-min" {
            let Some(min) = args.next().and_then(|s| s.to_str().and_then(|s| s.parse::<f64>().ok()))
                .filter(|v| v.is_finite() && (*v * 1e6).is_finite()) else {
                eprintln!("kicadread --area-min needs a finite length in mm");
                std::process::exit(2);
            };
            area_min = Some(min * 1e6);
        } else if !positional && arg == "--" {
            positional = true;
        } else {
            arguments.push(PathBuf::from(arg));
        }
    }
    let area_check = match (area_patterns, area_min) {
        (Some(patterns), Some(min)) => Some(AreaCheck { patterns, min }),
        (None, None) => None,
        _ => {
            eprintln!("kicadread --area-check and --area-min must be supplied together");
            std::process::exit(2);
        }
    };
    if arguments.is_empty() {
        eprintln!("usage: fab-elec-kicadread [--engines] [--necks] [--report] [--splash <dir>] [--area-check <patterns> --area-min <mm>] <file-or-folder>...");
    }
    let mut totals = Totals::default();
    let mut missing = false;
    for arg in arguments {
        let metadata = fs::metadata(&arg);
        if matches!(&metadata, Err(e) if e.kind() == std::io::ErrorKind::NotFound) {
            missing = true;
        }
        let directory = metadata.is_ok_and(|metadata| metadata.is_dir());
        let files = if directory { files_in(&arg) } else { vec![arg.clone()] };
        for file in files {
            let label = if directory {
                file.strip_prefix(&arg).unwrap_or(&file)
            } else {
                file.file_name().map(Path::new).unwrap_or(&file)
            };
            totals.files += 1;
            if let Err(e) = read_file(&file, label, engines, necks, report, splash.as_deref(), area_check.as_ref(), &mut totals) {
                totals.refused += 1;
                println!("kicadread refused file={} at {}:{}: {}", label.display(), e.line, e.column, e.message);
            }
        }
    }
    totals.print();
    if necks {
        let census = &totals.necks;
        println!(
            "necks total f={} width={} necks={} splitting={} foreign={} loneless={}",
            census.f_islands, census.width_islands, census.neck_islands,
            census.splitting, census.foreign, census.loneless,
        );
    }
    if missing {
        std::process::exit(2);
    }
}
