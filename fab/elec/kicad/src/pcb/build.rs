//! Construction of a standalone board from the reader's front-form intermediate model.

use super::file::*;
use crate::report::{GapKind, ImportReport, SourceFile};
use crate::pro::{ProjectFile, ProjectClass, CLASS_DEFAULTS};
use crate::pro::pattern::{expand_bus, Matcher};
use crate::sexpr::mm_to_nm;
use fab_elec_circuit::check::CheckKind;
use fab_elec_board::{
    Anchor, Area, Board, BoardKeepout, BoardNet, Graphic, ItemKind, Origin, Placed, Rules, StackKind,
    StackLayer, Stackup, Text, Track, Via, Zone, ZoneFill,
};
use fab_elec_circuit::{ClassRules, NetClass, PairRule, ViaSize};
use fab_elec_core::{
    canon::Canon, geom::arc_bbox, BBox, BoardId, ClassId, CompId, Contour, Index, ItemId,
    LayerId, LayerSet, NetId, Nm, OnTrack, PadIx, Path, Seg, Severity, Side, P,
};
use fab_elec_parts::{Footprint, Gfx};
use std::collections::{HashMap, HashSet, VecDeque};
use std::ops::ControlFlow;
use std::sync::Arc;

/// A usable board and the combined parse/build diagnostics.
#[derive(Clone, Debug)]
pub struct PcbRead {
    /// Evaluator for the mapped companion rules; pass it to the board's rule resolver.
    pub rules: Option<crate::dru::KicadRuleSource>,
    pub board: Board,
    /// Entry i belongs to board.placed[i]; construction preserves file footprint order.
    pub placements: Vec<PlacementData>,
    pub report: ImportReport,
    pub severities: Vec<(CheckKind, Option<Severity>)>,
    /// The file's zone layer list of each via whose rings follow the copper, for the comparison
    /// after a fill.
    pub zone_flash: ZoneFlashList,
    /// Source parsing (including the project and custom rules), for import hosts.
    pub parse_time: std::time::Duration,
    /// Board construction, kept separate from parsing for import hosts.
    pub build_time: std::time::Duration,
}

/// Each via's zone layer list as the file records it: the writer's cache of where its own fill
/// connects the via. Only a via whose rings follow the copper is listed, since a zone changes no
/// other via's rings.
#[derive(Clone, Debug, Default)]
pub struct ZoneFlashList {
    pub vias: Vec<(ItemId, LayerSet, Location)>,
}

impl ZoneFlashList {
    /// Compares each listed via with our fill: on the layers where the via has no fixed ring, the
    /// file's layers against the offered rings our fill reaches. Each via that differs is reported
    /// as `via.zone_flash_differs` at its location; returns how many differ. `board` must be the
    /// board these were read with, and `fills` its fill.
    pub fn compare(&self, board: &Board, fills: &[ZoneFill], report: &mut ImportReport) -> usize {
        if self.vias.is_empty() {
            return 0;
        }
        let mut reached = HashMap::<ItemId, LayerSet>::new();
        for (via, layer) in board.reached_rings(fills) {
            let set = reached.entry(via).or_insert(LayerSet::EMPTY);
            *set = set.with(layer);
        }
        let mut differs = 0;
        for &(id, layers, at) in &self.vias {
            let Some((ItemKind::Via, i)) = board.locate(id) else { continue };
            let via = &board.vias[i];
            let file = layers.minus(board.rings(via).fixed);
            if file != reached.get(&id).copied().unwrap_or(LayerSet::EMPTY) {
                report.gap("via.zone_flash_differs", GapKind::Approximated, at);
                differs += 1;
            }
        }
        differs
    }
}

/// Build without file access, threads or retained fill/connectivity caches.
pub fn build_board(file: &PcbFile, project: Option<&ProjectFile>, name: &str) -> PcbRead {
    let start = std::time::Instant::now();
    let mut report = file.report.clone();
    report.current = SourceFile::Board;
    // The standalone board parse cannot know whether its caller will supply a project.
    report.gaps.remove("rules.project_file");
    if let Some(project) = project { report.merge(&project.report); }
    if project.is_none() && !file.setup.legacy_rules {
        report.gap("rules.project_file", GapKind::Skipped, file.location);
    }
    let mut board = Board::new(BoardId(0), name);
    board.stackup = stackup(file);
    board.rules = rules(file, project, &mut report);
    let class_ids = if !file.classes.is_empty() || project.is_none() { classes(file, &mut board, &mut report) } else { Vec::new() };
    let nets = Nets::new(file, &mut board, &class_ids, &mut report);
    if let Some(project) = project {
        report.current = SourceFile::Project;
        if file.classes.is_empty() { project_classes(project, &mut board, &mut report); }
        else if let Some(at) = project.classes_location { report.gap("project.net_settings_unused", GapKind::Skipped, at); }
        project_presence(project, &mut report);
        report.current = SourceFile::Board;
    }
    if let Some(default) = board.classes.first() {
        board.rules.base.clearance = default.rules.clearance;
        board.rules.base.via = default.rules.via;
        board.rules.base.pair_gap = default.rules.pair.map(|p| p.gap);
    }

    let mut shared = HashMap::<_, Vec<Arc<Footprint>>>::new();
    let mut placements = Vec::with_capacity(file.footprints.len());
    for (k, fp) in file.footprints.iter().enumerate() {
        let bucket = shared.entry(fp.footprint.content_hash()).or_default();
        let footprint = if let Some(same) = bucket.iter().find(|same| same.as_ref() == &fp.footprint) {
            report.count_made("footprints.shared");
            same.clone()
        } else {
            let value = Arc::new(fp.footprint.clone());
            bucket.push(value.clone());
            value
        };
        let pad_nets = (0..footprint.pads.len())
            .map(|i| fp.pad_nets.get(i).and_then(|r| nets.resolve(r, fp.location, &mut report)))
            .collect();
        let id = board.alloc_id();
        board.placed.push(Placed {
            id, comp: CompId(k as u32),
            path: if fp.path.is_empty() { fp.refdes.clone() } else { fp.path.clone() },
            refdes: fp.refdes.clone(), footprint, pad_nets, at: fp.at, rot: fp.rot,
            side: fp.side, locked: fp.locked, origin: Origin::Authored, body_z: None,
            ref_label: None, value_label: None, legend: None,
            value: if fp.data.fields[1].value.is_empty() { None } else { Some(fp.data.fields[1].value.clone()) },
        });
        placements.push(fp.data.clone());
    }
    // Vias are needed to decide joins, but receive their ids after the merged tracks.
    for v in &file.vias {
        board.vias.push(Via {
            id: ItemId(0), net: nets.copper(&v.net, v.location, &mut report), at: v.at,
            drill: v.drill, pad: v.size, span: v.span, kind: v.kind, tent: v.tent,
            backdrill: None, locked: v.locked, origin: Origin::Authored,
            flash: v.flash,
        });
    }
    let contacts = Contacts::new(&board);
    let track_nets: Vec<_> = file.tracks.iter()
        .map(|t| nets.copper(&t.net, t.location, &mut report)).collect();
    let locations = merge_tracks(file, &track_nets, &contacts, &mut board);
    for i in 0..board.vias.len() {
        board.vias[i].id = board.alloc_id();
    }
    let zone_flash = ZoneFlashList {
        vias: file.vias.iter().zip(&board.vias)
            .filter_map(|(f, v)| f.zone_layers.filter(|_| f.flash.follows_copper()).map(|layers| (v.id, layers, f.location)))
            .collect(),
    };
    anchor_tracks(file, &nets, &contacts, &locations, &mut board, &mut report);
    for z in &file.zones {
        let layers = z.layers.intersect(board.copper_set());
        if layers.is_empty() {
            report.gap("zone.no_layers", GapKind::Dropped, z.location);
            continue;
        }
        let id = board.alloc_id();
        board.zones.push(Zone {
            id, net: nets.resolve(&z.net, z.location, &mut report), name: z.name.clone(),
            layers, area: Area::Poly(z.area.clone()), priority: z.priority,
            settings: z.settings.clone(), locked: z.locked, origin: Origin::Authored,
        });
    }
    for k in &file.keepouts {
        let id = board.alloc_id();
        board.keepouts.push(BoardKeepout {
            id, area: k.area.clone(), layers: k.layers, forbid: k.forbid,
            reason: k.reason.clone(), origin: Origin::Authored, z: None,
        });
    }
    let mut graphics: Vec<_> = file.graphics.clone();
    outline(file, &mut board, &mut graphics, &mut report);
    graphics.sort_by_key(|g| g.location);
    for g in graphics {
        let id = board.alloc_id();
        board.graphics.push(Graphic { id, layer: g.layer, gfx: g.gfx, locked: g.locked, origin: Origin::Authored });
    }
    for t in &file.texts {
        let id = board.alloc_id();
        board.texts.push(Text {
            id, layer: t.layer, text: t.text.clone(), at: t.at, rot: t.rot, size: t.size,
            thick: t.thick, mirrored: t.mirrored, locked: t.locked, origin: Origin::Authored,
        });
    }
    board.reindex();
    board.snap_anchors();
    for (key, count) in [
        ("footprints", board.placed.len()), ("tracks", board.tracks.len()),
        ("vias", board.vias.len()), ("zones", board.zones.len()),
        ("keepouts", board.keepouts.len()), ("graphics", board.graphics.len()),
        ("texts", board.texts.len()), ("nets", board.nets.len()),
        ("classes", board.classes.len()), ("outline.contours", board.outline.len()),
    ] {
        report.made.insert(key.into(), count as u64);
    }
    PcbRead { board, placements, report, rules: None, severities: severities(project), zone_flash, parse_time: std::time::Duration::ZERO, build_time: start.elapsed() }
}

fn stackup(file: &PcbFile) -> Stackup {
    let mut out = Stackup::standard_2_layer();
    if let Some(source) = &file.stackup {
        out.layers = source.layers.iter().map(|l| l.value.clone()).collect();
        out.finish = source.finish.unwrap_or_default();
        out.mask_color = source.mask_color.unwrap_or(out.mask_color);
        out.silk_color = source.silk_color.unwrap_or(out.silk_color);
    } else {
        let copper = &file.layers.copper;
        let thickness = file.thickness.as_ref().map_or(1_600_000, |v| v.value) as i64;
        let gaps = copper.len().saturating_sub(1);
        let remaining = thickness - copper.len() as i64 * 35_000 - 20_000;
        let layer = |kind, thickness, material: &str, er, loss_tan| StackLayer {
            kind, thickness, material: material.into(), er, loss_tan,
            role: Default::default(), velocity: None,
        };
        out.layers = vec![
            layer(StackKind::Silk(Side::Top), 0, "silk", None, None),
            layer(StackKind::Mask(Side::Top), 10_000, "solder mask", Some(3.3), None),
        ];
        for (i, &id) in copper.iter().enumerate() {
            out.layers.push(StackLayer { role: file.layers.role(id),
                ..layer(StackKind::Copper(id), 35_000, "copper", None, None) });
            if i < gaps {
                // Distribute whole-nm remainders from the top, keeping the total exact.
                let n = gaps as i64;
                let value = remaining.div_euclid(n) + i64::from((i as i64) < remaining.rem_euclid(n));
                out.layers.push(layer(StackKind::Dielectric { core: gaps == 1 || (i + 1) % 2 == 0 },
                    nm(value), "FR-4", Some(4.5), Some(0.02)));
            }
        }
        out.layers.push(layer(StackKind::Mask(Side::Bottom), 10_000, "solder mask", Some(3.3), None));
        out.layers.push(layer(StackKind::Silk(Side::Bottom), 0, "silk", None, None));
    }
    out.thickness = file.thickness.as_ref().map_or_else(
        || nm(out.layers.iter().map(|l| l.thickness as i64).sum()), |v| v.value);
    out
}

fn nm(value: i64) -> Nm { value.clamp(Nm::MIN as i64, Nm::MAX as i64) as Nm }

fn rules(file: &PcbFile, project: Option<&ProjectFile>, report: &mut ImportReport) -> Rules {
    let setup = &file.setup;
    let mut rules = Rules::standard();
    let base = &mut rules.base;
    base.track = Some(200_000); base.annular = Some(100_000); base.drill = Some(300_000);
    base.hole_to_hole = Some(250_000); base.hole_clearance = Some(250_000); base.edge = Some(500_000);
    base.silk_clearance = Some(0); base.mask_margin = Some(0); base.paste_margin = Some(0); base.paste_ratio_ppm = Some(0);
    base.clearance = Some(200_000); base.via = Some(ViaSize { drill: 300_000, pad: 600_000 }); base.pair_gap = Some(250_000);
    macro_rules! overlay {
        ($($field:ident),*) => { $(if let Some(value) = setup.$field { base.$field = Some(value); })* };
    }
    overlay!(mask_margin, paste_margin, paste_ratio_ppm, track, annular, drill, hole_to_hole);
    if setup.clearance.is_some_and(|v| v > 0) {
        report.gap("rules.min_clearance", GapKind::Skipped, setup.location.unwrap_or(file.location));
    }
    if setup.legacy_rules {
        base.edge = Some(setup.edge_clearance.unwrap_or_else(|| {
            file.edges.iter().chain(file.footprints.iter().flat_map(|fp| &fp.edges)).map(|e| e.width).max().unwrap_or(0).max(0) / 2
        }));
        if let Some(at) = project.and_then(|p| p.design_settings) {
            report.current = SourceFile::Project;
            report.gap("project.design_settings_unused", GapKind::Skipped, at);
            report.current = SourceFile::Board;
        }
        return rules;
    }
    // Settings loaded from a project (even an absent one) give a missing annular minimum the
    // reference's stored-setting default of 0.5 mm; only boards with old-style setup rules keep
    // the 0.1 mm of a fresh design.
    base.annular = Some(500_000);
    let Some(project) = project else { return rules };
    report.current = SourceFile::Project;
    for &(key, default, min, max) in crate::pro::file::RULES {
        let given = project.rule(key).or_else(|| {
            if key == "min_silk_line_width" && project.design_version.value <= 3 { project.rule("min_text_thickness") } else { None }
        });
        let Some(given) = given else { continue };
        let raw = if key == "min_resolved_spokes" { given.value.trunc() } else { given.value };
        let value = if (min..=max).contains(&raw) { raw } else { default };
        let length = Some(mm_to_nm(value));
        match key {
            "min_track_width" => base.track = length, "min_via_annular_width" => base.annular = length,
            "min_through_hole_diameter" => base.drill = length, "min_hole_to_hole" => base.hole_to_hole = length,
            "min_hole_clearance" => base.hole_clearance = length, "min_silk_clearance" => base.silk_clearance = length,
            "min_copper_edge_clearance" => {
                base.edge = Some(mm_to_nm(value.max(0.0)));
                if value < 0.0 { report.gap("rules.legacy_edge_clearance", GapKind::Skipped, given.at); }
            }
            "min_clearance" => if value > 0.0 { report.gap("rules.min_clearance", GapKind::Skipped, given.at); },
            _ if value != default => {
                let feature = match key {
                    "min_via_diameter" => "rules.via_min_size", "min_microvia_diameter" | "min_microvia_drill" => "rules.microvia",
                    _ => "rules.board_minima",
                };
                report.gap_value(feature, GapKind::Skipped, given.at, key.into());
            }
            _ => (),
        }
    }
    if project.design_version.value < 2 {
        for (key, destination, present) in [
            ("solder_mask_clearance", &mut base.mask_margin, setup.mask_margin.is_some()),
            ("solder_paste_clearance", &mut base.paste_margin, setup.paste_margin.is_some()),
        ] {
            if !present { if let Some(v) = project.rule(key) { *destination = Some(mm_to_nm(v.value)); } }
        }
        if setup.paste_ratio_ppm.is_none() {
            if let Some(v) = project.rule("solder_paste_margin_ratio") { base.paste_ratio_ppm = Some((v.value * 1_000_000.0).round() as i32); }
        }
        if let Some(v) = project.rule("solder_mask_min_width") { report.gap("rules.mask_min_width", GapKind::Skipped, v.at); }
    }
    if let Some(first) = project.size_presets.first() {
        report.gap_value("rules.size_presets", GapKind::Skipped, first.at,
            project.size_presets.iter().map(|v| v.value.as_str()).collect::<Vec<_>>().join(","));
    }
    if let Some(v) = &project.exclusions { gap_count(report, "drc.exclusions", v); }
    if let Some(v) = &project.external_fillets { if v.value { report.gap("zone.external_fillets", GapKind::Skipped, v.at); } }
    report.current = SourceFile::Board;
    rules
}
fn gap_count(report: &mut ImportReport, feature: &'static str, value: &Located<usize>) {
    if value.value > 0 {
        report.gap(feature, GapKind::Skipped, value.at);
        if let Some(gap) = report.gaps.get_mut(feature) { gap.count = gap.count.saturating_add(value.value.saturating_sub(1).min(u32::MAX as usize) as u32); }
    }
}
fn project_presence(project: &ProjectFile, report: &mut ImportReport) {
    for (feature, value) in [("project.text_variables", &project.text_variables), ("component_classes", &project.component_classes)] {
        if let Some(v) = value { if v.value > 0 { report.gap(feature, GapKind::Skipped, v.at); } }
    }
}

fn class_track(name: &str, width: Nm, at: Location, base: Option<Nm>, report: &mut ImportReport) {
    if Some(width) != base {
        report.gap_value("class.track_width", GapKind::Skipped, at, format!("{name}={}", width as f64 / 1_000_000.0));
    }
}
fn class_rules(c: &FileNetClass, fallback: ViaSize, base_track: Option<Nm>, report: &mut ImportReport) -> ClassRules {
    let pair = match (c.diff_pair_width, c.diff_pair_gap) {
        (Some(width), Some(gap)) => Some(PairRule { width, gap }), (None, None) => None,
        _ => { report.gap("class.pair_partial", GapKind::Skipped, c.location); None }
    };
    if let Some(width) = c.trace_width { class_track(&c.name, width, c.location, base_track, report); }
    ClassRules {
        clearance: c.clearance, track: None, pair,
        via: (c.via_dia.is_some() || c.via_drill.is_some()).then_some(ViaSize {
            drill: c.via_drill.unwrap_or(fallback.drill), pad: c.via_dia.unwrap_or(fallback.pad),
        }), ..ClassRules::default()
    }
}
fn classes(file: &PcbFile, board: &mut Board, report: &mut ImportReport) -> Vec<ClassId> {
    let base = ViaSize { drill: 300_000, pad: 600_000 };
    let default = file.classes.iter().position(|c| c.name == "Default");
    let legacy = file.version.version < 20201115;
    let inherited = ViaSize {
        drill: if legacy { file.setup.via_drill.unwrap_or(base.drill) } else { base.drill },
        pad: if legacy { file.setup.via_size.unwrap_or(base.pad) } else { base.pad },
    };
    let mut rules = default.map(|i| class_rules(&file.classes[i], inherited, board.rules.base.track, report)).unwrap_or_default();
    if legacy { rules.clearance = rules.clearance.or(file.setup.trace_clearance); }
    rules.clearance = rules.clearance.or(Some(200_000));
    rules.via = rules.via.or(Some(inherited));
    rules.pair = rules.pair.or(Some(PairRule { width: 200_000, gap: 250_000 }));
    if default.is_none_or(|i| file.classes[i].trace_width.is_none()) {
        class_track("Default", 200_000, default.map_or(file.location, |i| file.classes[i].location), board.rules.base.track, report);
    }
    let fallback = rules.via.unwrap_or(base);
    board.classes.push(NetClass { id: ClassId(0), name: "Default".into(), rules });
    let mut ids = vec![ClassId(0); file.classes.len()];
    for (i, c) in file.classes.iter().enumerate() {
        if Some(i) == default { continue; }
        let Ok(index) = u16::try_from(board.classes.len()) else { continue };
        let id = ClassId(index); ids[i] = id;
        board.classes.push(NetClass { id, name: c.name.clone(), rules: class_rules(c, fallback, board.rules.base.track, report) });
    }
    ids
}

fn source_default(project: &ProjectFile) -> ProjectClass {
    let configured = project.classes.iter().find(|c| c.name.value == "Default");
    let mut class = configured.cloned().unwrap_or_else(|| empty_class("Default", project.location, f64::INFINITY));
    for (field, default) in class.fields.iter_mut().zip(CLASS_DEFAULTS) {
        if field.is_none() { *field = Some(Located { at: class.name.at, value: default }); }
    }
    class
}
fn empty_class(name: &str, at: Location, priority: f64) -> ProjectClass {
    ProjectClass { name: Located { at, value: name.into() }, fields: std::array::from_fn(|_| None),
        priority: Some(Located { at, value: priority }), pcb_color: None, tuning_profile: None, wire_width: None, bus_width: None, line_style: None, schematic_color: None, nets: Vec::new() }
}
fn fields(source: &ProjectClass, default: &ProjectClass) -> [f64; 9] {
    std::array::from_fn(|i| source.fields[i].as_ref().or(default.fields[i].as_ref()).map_or(CLASS_DEFAULTS[i], |v| v.value))
}
fn specified_color(s: &str) -> bool {
    if s.trim().is_empty() { return false; }
    let s = s.trim();
    if s.starts_with('#') { return s.len() != 9 || s[1..].chars().any(|c| c != '0'); }
    if let Some(inner) = s.strip_prefix("rgba(").and_then(|s| s.strip_suffix(')')) {
        return !inner.split(',').all(|n| n.trim().parse::<f64>().ok() == Some(0.0));
    }
    true
}
/// `configured`: a class the project defines, whose own fields are reported; implicit and
/// composite classes only repeat values already reported.
fn emit_class(source: &ProjectClass, default: &ProjectClass, configured: bool, board: &mut Board, report: &mut ImportReport) -> Option<ClassId> {
    let id = ClassId(u16::try_from(board.classes.len()).ok()?);
    let v = fields(source, default); let name = source.name.value.clone();
    if configured { class_gaps(source, default, &v, board.rules.base.track, report); }
    board.classes.push(NetClass { id, name, rules: ClassRules {
        clearance: Some(mm_to_nm(v[0])), track: None,
        via: Some(ViaSize { drill: mm_to_nm(v[3]), pad: mm_to_nm(v[2]) }),
        pair: Some(PairRule { width: mm_to_nm(v[6]), gap: mm_to_nm(v[7]) }), ..ClassRules::default()
    } });
    Some(id)
}
fn class_gaps(source: &ProjectClass, default: &ProjectClass, v: &[f64; 9], base_track: Option<Nm>, report: &mut ImportReport) {
    let name = source.name.value.clone();
    class_track(&name, mm_to_nm(v[1]), source.fields[1].as_ref().map_or(source.name.at, |v| v.at), base_track, report);
    // Microvia dimensions present on this class have no counterpart in the board class model.
    for v in source.fields[4..6].iter().flatten() { report.gap("class.microvia", GapKind::Skipped, v.at); }
    if v[8] != fields(default, default)[8] { report.gap_value("class.pair_via_gap", GapKind::Skipped, source.name.at, name.clone()); }
    if let Some(v) = &source.pcb_color {
        if specified_color(&v.value) { report.gap_value("class.color", GapKind::Skipped, v.at, name.clone()); }
    }
    if let Some(v) = &source.tuning_profile {
        if !v.is_empty() { report.gap_value("class.tuning_profile", GapKind::Skipped, v.at, name); }
    }
}
fn project_classes(project: &ProjectFile, board: &mut Board, report: &mut ImportReport) {
    let default = source_default(project);
    let mut sources = vec![default.clone()];
    sources.extend(project.classes.iter().filter(|c| c.name.value != "Default").cloned());
    let configured_count = sources.len();
    let mut names: HashMap<String, usize> = sources.iter().enumerate().map(|(i, c)| (c.name.value.clone(), i)).collect();
    let mut ids = Vec::new();
    let absent_default = empty_class("Default", project.location, f64::INFINITY);
    ids.push(emit_class(project.classes.iter().find(|c| c.name.value == "Default").unwrap_or(&absent_default), &default, true, board, report));
    for c in sources.iter().skip(1) { ids.push(emit_class(c, &default, true, board, report)); }
    let mut patterns = Vec::new(); let mut seen = HashSet::new();
    for p in &project.patterns {
        let (expanded, approx) = expand_bus(&p.pattern.value);
        if approx { report.gap_value("class.bus_pattern", GapKind::Approximated, p.pattern.at, p.pattern.value.clone()); }
        for member in expanded {
            if !seen.insert((member.clone(), p.class.value.clone())) { continue; }
            let matcher = Matcher::new(&member);
            if matcher.regex_unsupported {
                report.gap_value("class.pattern_regex_unsupported", GapKind::Approximated, p.pattern.at, member);
            }
            patterns.push((matcher, &p.class));
        }
    }
    let assignments: HashMap<&str, &Vec<Located<String>>> = project.assignments.iter().map(|a| (a.name.value.as_str(), &a.classes)).collect();
    let mut sets = Vec::with_capacity(board.nets.len()); sets.push(Vec::new());
    // Allocate all implicit classes before composites, while discovering both in board net order.
    for i in 1..board.nets.len() {
        let mut set = Vec::new();
        let explicit = assignments.get(board.nets[i].name.as_str()).into_iter().flat_map(|a| a.iter());
        let matched = patterns.iter().filter(|(matcher, _)| matcher.is_match(&board.nets[i].name)).map(|(_, class)| *class);
        let assigned: Vec<_> = explicit.chain(matched).collect();
        for name in assigned {
            let index = if let Some(&index) = names.get(&name.value) { index } else {
                let c = empty_class(&name.value, name.at, f64::INFINITY);
                let id = emit_class(&c, &default, false, board, report);
                if id.is_some() { report.count_made("classes.implicit"); }
                ids.push(id); let index = sources.len(); sources.push(c); names.insert(name.value.clone(), index); index
            };
            set.push(index);
        }
        set.sort_unstable(); set.dedup();
        set.sort_by(|&a, &b| {
            // Source categories keep implicit and default classes below every finite priority.
            let rank = |i: usize| if i == 0 { 2 } else if i >= configured_count { 1 } else { 0 };
            let priority = |i: usize| {
                let n = sources[i].priority.as_ref().map_or(-1.0, |v| v.value);
                if n == 0.0 { 0.0 } else { n }
            };
            rank(a).cmp(&rank(b)).then_with(|| priority(a).total_cmp(&priority(b)))
                .then_with(|| sources[a].name.value.cmp(&sources[b].name.value))
        });
        sets.push(set);
    }
    let mut composites = HashMap::<Vec<usize>, Option<ClassId>>::new();
    for (ni, set) in sets.into_iter().enumerate().skip(1) {
        if set.is_empty() { continue; }
        let id = if set.iter().any(|&i| ids[i].is_none()) { None }
        // An implicit class takes every field from the default class, so it is complete as it is.
        else if set.len() == 1 && (set[0] == 0 || set[0] >= configured_count || sources[set[0]].fields.iter().all(Option::is_some)) { ids[set[0]] }
        else if let Some(id) = composites.get(&set) { *id }
        else {
            let mut composite = empty_class("", sources[set[0]].name.at, -1.0);
            let mut used_default = false;
            for i in 0..9 {
                composite.fields[i] = set.iter().find_map(|&c| sources[c].fields[i].clone());
                if composite.fields[i].is_none() { composite.fields[i] = default.fields[i].clone(); used_default = true; }
            }
            composite.pcb_color = set.iter().find_map(|&c| sources[c].pcb_color.clone()).or_else(|| default.pcb_color.clone());
            composite.tuning_profile = set.iter().find_map(|&c| sources[c].tuning_profile.clone()).or_else(|| default.tuning_profile.clone());
            let mut labels: Vec<&str> = set.iter().map(|&i| sources[i].name.value.as_str()).collect();
            if used_default && !set.contains(&0) { labels.push("Default"); }
            composite.name.value = labels.join(",");
            let id = emit_class(&composite, &default, false, board, report);
            if id.is_some() { report.count_made("classes.composite"); }
            composites.insert(set.clone(), id); id
        };
        if let Some(id) = id { board.nets[ni].class = id; }
        else { report.gap("class.out_of_range", GapKind::Dropped, sources[set[0]].name.at); }
    }
    for v in &project.net_chains { if v.value > 0 { report.gap("net_chains", GapKind::Skipped, v.at); } }
    if let Some(v) = &project.net_colors { if v.value > 0 { report.gap("net.colors", GapKind::Skipped, v.at); } }
}

fn severities(project: Option<&ProjectFile>) -> Vec<(CheckKind, Option<Severity>)> {
    use CheckKind::*;
    use Severity::{Error, Warning};
    [
        ("clearance", Clearance, Error), ("copper_edge_clearance", EdgeClearance, Error),
        ("hole_clearance", HoleClearance, Error), ("hole_to_hole", HoleToHole, Warning),
        ("track_width", TrackWidth, Error), ("via_diameter", ViaSize, Error),
        ("annular_width", AnnularRing, Error), ("drill_out_of_range", Drill, Error),
        ("courtyards_overlap", CourtyardOverlap, Error), ("silk_over_copper", SilkOverPad, Warning),
        ("text_height", TextSize, Warning), ("unconnected_items", Unconnected, Error),
        ("shorting_items", Short, Error), ("track_dangling", Dangling, Warning),
        ("via_dangling", DanglingVia, Warning), ("items_not_allowed", Keepout, Error),
        ("isolated_copper", ZoneIsland, Warning), ("diff_pair_gap_out_of_range", PairGap, Error),
        ("diff_pair_uncoupled_length_too_long", PairUncoupled, Error), ("length_out_of_range", Length, Error),
        ("skew_out_of_range", Skew, Error),
    ].into_iter().map(|(key, kind, default)| {
        let value = project.and_then(|p| p.severity(key).or_else(|| if key == "hole_to_hole" { p.severity("hole_near_hole") } else { None }));
        (kind, match value { Some("ignore") => None, Some("warning") => Some(Warning), Some(_) => Some(Error), None => Some(default) })
    }).collect()
}

struct Nets {
    codes: HashMap<i64, NetId>,
    names: HashMap<String, NetId>,
}
impl Nets {
    fn new(file: &PcbFile, board: &mut Board, classes: &[ClassId], report: &mut ImportReport) -> Self {
        let mut out = Self { codes: HashMap::new(), names: HashMap::new() };
        board.nets.push(BoardNet { name: String::new(), class: ClassId(0) });
        out.codes.insert(0, NetId(0));
        out.names.insert(String::new(), NetId(0));
        for n in &file.nets {
            if out.codes.contains_key(&n.code) { continue; }
            let id = NetId(board.nets.len() as u32);
            board.nets.push(BoardNet { name: n.name.clone(), class: ClassId(0) });
            out.codes.insert(n.code, id);
            out.names.entry(n.name.clone()).or_insert(id);
        }
        let mut references = Vec::new();
        for fp in &file.footprints {
            references.extend(fp.pad_nets.iter().map(|r| (r.location.unwrap_or(fp.location), r)));
        }
        references.extend(file.tracks.iter().map(|t| (t.net.location.unwrap_or(t.location), &t.net)));
        references.extend(file.vias.iter().map(|v| (v.net.location.unwrap_or(v.location), &v.net)));
        references.extend(file.zones.iter().map(|z| (z.net.location.unwrap_or(z.location), &z.net)));
        references.sort_by_key(|r| r.0);
        for (_, r) in references {
            if let Some(name) = r.zone_name.as_ref().or(r.name.as_ref()) {
                if !out.names.contains_key(name) {
                    let id = NetId(board.nets.len() as u32);
                    out.names.insert(name.clone(), id);
                    board.nets.push(BoardNet { name: name.clone(), class: ClassId(0) });
                }
            }
        }
        for (c, &id) in file.classes.iter().zip(classes) {
            for assignment in &c.add_net {
                if let Some(net) = out.names.get(&assignment.value).filter(|n| n.0 != 0) {
                    board.nets[net.ix()].class = id;
                    if id == ClassId(0) && c.name != "Default" { report.gap("class.out_of_range", GapKind::Dropped, assignment.at); }
                } else {
                    report.gap("class.unknown_net", GapKind::Skipped, assignment.at);
                }
            }
        }
        out
    }

    fn resolve(&self, r: &NetRef, location: Location, report: &mut ImportReport) -> Option<NetId> {
        let location = r.location.unwrap_or(location);
        let nonzero = |n: Option<NetId>| n.filter(|n| n.0 != 0);
        if let Some(name) = &r.zone_name {
            return nonzero(self.names.get(name).copied());
        }
        if let Some(code) = r.code {
            let Some(&id) = self.codes.get(&code) else {
                report.gap("net.unknown_code", GapKind::Dropped, location);
                return None;
            };
            if let Some(name) = &r.name {
                if self.names.get(name) != Some(&id) {
                    report.gap("net.code_name_mismatch", GapKind::Dropped, location);
                    return None;
                }
            }
            nonzero(Some(id))
        } else {
            nonzero(r.name.as_ref().and_then(|n| self.names.get(n)).copied())
        }
    }

    fn copper(&self, r: &NetRef, location: Location, report: &mut ImportReport) -> NetId {
        self.resolve(r, location, report).unwrap_or_else(|| {
            report.gap("net.netless_copper", GapKind::Approximated, location);
            NetId(0)
        })
    }
}

#[derive(Clone, Copy)]
enum ContactKind { Via(usize), Pad(usize, PadIx) }
struct Contact { at: P, net: NetId, layers: LayerSet, kind: ContactKind }
struct Contacts { values: Vec<Contact>, index: Index<usize> }
impl Contacts {
    fn new(board: &Board) -> Self {
        let copper = board.copper_set();
        let n = board.copper_count();
        let mut values = Vec::new();
        for (i, v) in board.vias.iter().enumerate() {
            values.push(Contact { at: v.at, net: v.net, layers: v.span_layers(copper), kind: ContactKind::Via(i) });
        }
        for (i, fp) in board.placed.iter().enumerate() {
            for (j, net) in fp.pad_nets.iter().enumerate() {
                let Some(net) = net else { continue; };
                let Ok(j) = u16::try_from(j) else { continue; };
                let pad = PadIx(j);
                if let Some(at) = fp.pad_at(pad) {
                    values.push(Contact { at, net: *net, layers: fp.pad_layers(pad, copper, n), kind: ContactKind::Pad(i, pad) });
                }
            }
        }
        let index = Index::build(values.iter().enumerate().map(|(i, c)| (BBox::point(c.at), i)).collect());
        Self { values, index }
    }
    fn at(&self, point: P, layer: LayerId, net: NetId) -> Option<ContactKind> {
        let mut best = None;
        self.index.query(BBox::point(point).inflate(10), |_, i| {
            let c = &self.values[i];
            if c.net == net && c.layers.has(layer) && c.at.dist2(point) <= 100 && best.is_none_or(|old| i < old) {
                best = Some(i);
            }
            ControlFlow::Continue(())
        });
        best.map(|i| self.values[i].kind)
    }
}

type End = (usize, usize);
fn merge_tracks(file: &PcbFile, nets: &[NetId], contacts: &Contacts, board: &mut Board) -> Vec<[Location; 2]> {
    let mut ends = HashMap::<(LayerId, P), Vec<End>>::new();
    for (i, t) in file.tracks.iter().enumerate() {
        ends.entry((t.layer, t.start)).or_default().push((i, 0));
        ends.entry((t.layer, t.end)).or_default().push((i, 1));
    }
    let mut links = vec![[None; 2]; file.tracks.len()];
    for ((layer, point), ends) in ends {
        let [a, b] = ends.as_slice() else { continue; };
        let ta = &file.tracks[a.0];
        let tb = &file.tracks[b.0];
        if nets[a.0] == nets[b.0] && ta.width == tb.width && ta.locked == tb.locked
            && contacts.at(point, layer, nets[a.0]).is_none()
        {
            links[a.0][a.1] = Some(*b);
            links[b.0][b.1] = Some(*a);
        }
    }
    let terminal = |start: End| {
        let mut at = start;
        while let Some(next) = links[at.0][at.1] {
            at = (next.0, 1 - next.1);
            if at.0 == start.0 { return None; }
        }
        Some(at)
    };
    let mut used = vec![false; file.tracks.len()];
    let mut locations = Vec::new();
    for first in 0..file.tracks.len() {
        if used[first] { continue; }
        let start = match (terminal((first, 0)), terminal((first, 1))) {
            (Some(a), Some(b)) => a.min(b),
            _ => (first, 0),
        };
        let source = &file.tracks[first];
        let piece = &file.tracks[start.0];
        let mut path = Path::new(if start.1 == 0 { piece.start } else { piece.end });
        let mut at = start;
        let last = loop {
            used[at.0] = true;
            let t = &file.tracks[at.0];
            let end = if at.1 == 0 { t.end } else { t.start };
            path.segs.push(match t.mid { Some(mid) => Seg::Arc { mid, end }, None => Seg::Line(end) });
            let Some(next) = links[at.0][1 - at.1] else { break at.0; };
            if used[next.0] { break at.0; }
            at = next;
        };
        locations.push([piece.location, file.tracks[last].location]);
        let id = board.alloc_id();
        board.tracks.push(Track { id, net: nets[first], layer: source.layer, w: source.width,
            widths: None, path, ends: [Anchor::Free; 2], locked: source.locked, origin: Origin::Authored });
    }
    locations
}

fn anchor_tracks(file: &PcbFile, nets: &Nets, contacts: &Contacts, locations: &[[Location; 2]], board: &mut Board, report: &mut ImportReport) {
    let segments = Index::build(board.tracks.iter().enumerate().flat_map(|(i, t)| {
        t.path.steps().enumerate().map(move |(j, (a, s))| {
            let bbox = match s { Seg::Line(b) => BBox::new(a, b), Seg::Arc { mid, end } => arc_bbox(a, mid, end) };
            (bbox, (i, j))
        })
    }).collect());
    let copper_count = board.copper_count();
    let placed = &board.placed;
    let pad_shapes: Vec<_> = contacts.values.iter().flat_map(|c| {
        c.layers.iter().filter_map(move |layer| match c.kind {
            ContactKind::Pad(i, pad) => placed[i].pad_shape(pad, layer, copper_count).map(|shape| (c.net, layer, shape)),
            ContactKind::Via(_) => None,
        })
    }).collect();
    let pads = Index::build(pad_shapes.iter().enumerate().map(|(i, (_, _, s))| (s.bbox(), i)).collect());
    // Resolve zone references only once in the output stage; classification must not add gaps.
    let mut ignored = ImportReport::default();
    let zone_nets: Vec<_> = file.zones.iter().map(|z| nets.resolve(&z.net, z.location, &mut ignored)).collect();
    let zones = Index::build(file.zones.iter().enumerate().map(|(i, z)| (z.area.bbox(), i)).collect());
    for ti in 0..board.tracks.len() {
        let t = &board.tracks[ti];
        let mut ends = [Anchor::Free; 2];
        for (e, point) in [t.path.start, t.path.end()].into_iter().enumerate() {
            ends[e] = match contacts.at(point, t.layer, t.net) {
                Some(ContactKind::Via(i)) => Anchor::Via(board.vias[i].id),
                Some(ContactKind::Pad(i, pad)) => Anchor::Pad { placed: board.placed[i].id, pad },
                None => {
                    let mut hosts = HashMap::<usize, (i128, usize, OnTrack, P)>::new();
                    segments.query(BBox::point(point).inflate(10), |_, (i, j)| {
                        let host = &board.tracks[i];
                        if i == ti || host.layer != t.layer || host.net != t.net { return ControlFlow::Continue(()); }
                        let seg = host.path.segs[j];
                        let a = if j == 0 { host.path.start } else { host.path.segs[j - 1].end() };
                        let mid = match seg { Seg::Arc { mid, .. } => Some(mid), Seg::Line(_) => None };
                        // These are the same projection and tie-break used by track_anchor's
                        // Path::locate, restricted to spatially relevant segments.
                        let at = OnTrack::along(j as u32, a, mid, seg.end(), point);
                        let q = host.path.point_on(at);
                        let distance = point.dist2(q);
                        if distance <= 100 {
                            let old = hosts.entry(i).or_insert((distance, j, at, q));
                            if (distance, j) < (old.0, old.1) {
                                *old = (distance, j, at, q);
                            }
                        }
                        ControlFlow::Continue(())
                    });
                    hosts.into_iter().min_by_key(|(i, (_, _, _, q))| {
                        let host = &board.tracks[*i];
                        (*q == host.path.start || *q == host.path.end(), host.id)
                    }).map_or(Anchor::Free, |(i, (_, _, at, _))| Anchor::Track { id: board.tracks[i].id, at })
                }
            };
            let kind = match ends[e] {
                Anchor::Via(_) => "anchors.via", Anchor::Pad { .. } => "anchors.pad",
                Anchor::Track { .. } => "anchors.track", _ => "anchors.free",
            };
            report.count_made(kind);
            if ends[e] == Anchor::Free {
                let mut in_pad = false;
                pads.query(BBox::point(point), |_, i| {
                    let (net, layer, shape) = &pad_shapes[i];
                    if *net == t.net && *layer == t.layer && fab_elec_core::point_clearance(point, shape) == 0 {
                        in_pad = true;
                        return ControlFlow::Break(());
                    }
                    ControlFlow::Continue(())
                });
                let mut in_zone = false;
                zones.query(BBox::point(point), |_, i| {
                    let z = &file.zones[i];
                    if zone_nets[i] == Some(t.net) && z.layers.has(t.layer) && z.area.contains(point) {
                        in_zone = true;
                        return ControlFlow::Break(());
                    }
                    ControlFlow::Continue(())
                });
                let feature = if in_pad { "track.end_in_pad_off_centre" }
                    else if in_zone { "track.end_in_zone" } else { "track.free_end" };
                report.gap(feature, GapKind::Approximated, locations[ti][e]);
            }
        }
        board.tracks[ti].ends = ends;
    }
}

fn reversed(path: &Path) -> Path {
    let steps: Vec<_> = path.steps().collect();
    Path { start: path.end(), segs: steps.into_iter().rev().map(|(a, seg)| match seg {
        Seg::Line(_) => Seg::Line(a), Seg::Arc { mid, .. } => Seg::Arc { mid, end: a },
    }).collect() }
}

fn outline(file: &PcbFile, board: &mut Board, graphics: &mut Vec<FileGraphic>, report: &mut ImportReport) {
    let mut pieces = file.edges.clone();
    for (fp, placed) in file.footprints.iter().zip(&board.placed) {
        pieces.extend(fp.edges.iter().map(|e| EdgePiece { path: e.path.mapped(|p| placed.to_board(p)), ..e.clone() }));
    }
    pieces.sort_by_key(|e| e.location);
    let index = Index::build(pieces.iter().enumerate().filter(|(_, e)| !e.closed).flat_map(|(i, e)| {
        [(BBox::point(e.path.start), (i, 0)), (BBox::point(e.path.end()), (i, 1))]
    }).collect());
    let mut used = vec![false; pieces.len()];
    for first in 0..pieces.len() {
        if used[first] { continue; }
        used[first] = true;
        if pieces[first].closed {
            board.outline.push(Contour::new(pieces[first].path.clone()));
            continue;
        }
        let mut chain = VecDeque::from([(first, false)]);
        let mut start = pieces[first].path.start;
        let mut end = pieces[first].path.end();
        // Grow both ends so a file's first piece may lie in the middle of an open chain.
        for front in [false, true] {
            while start.dist2(end) > 100_000_000 {
                let point = if front { start } else { end };
                let mut next = None;
                index.query(BBox::point(point).inflate(10_000), |_, candidate: End| {
                    let (i, e) = candidate;
                    let p = if e == 0 { pieces[i].path.start } else { pieces[i].path.end() };
                    if !used[i] && p.dist2(point) <= 100_000_000 && next.is_none_or(|old| candidate < old) {
                        next = Some(candidate);
                    }
                    ControlFlow::Continue(())
                });
                let Some((i, e)) = next else { break; };
                used[i] = true;
                let reverse = if front { e == 0 } else { e == 1 };
                if front {
                    start = if reverse { pieces[i].path.end() } else { pieces[i].path.start };
                    chain.push_front((i, reverse));
                } else {
                    end = if reverse { pieces[i].path.start } else { pieces[i].path.end() };
                    chain.push_back((i, reverse));
                }
            }
        }
        if start.dist2(end) <= 100_000_000 {
            let mut path = Path::new(start);
            for (i, reverse) in chain {
                let piece = &pieces[i].path;
                path.segs.extend(if reverse { reversed(piece).segs } else { piece.segs.clone() });
            }
            if let Some(last) = path.segs.last_mut() {
                match last { Seg::Line(end) | Seg::Arc { end, .. } => *end = start }
            }
            board.outline.push(Contour::new(path));
        } else {
            for (i, _) in chain {
                let e = &pieces[i];
                graphics.push(FileGraphic { location: e.location, layer: LayerId::EDGE,
                    gfx: Gfx::Stroke { path: e.path.clone(), w: e.width }, locked: e.locked });
                report.gap("outline.open", GapKind::Approximated, e.location);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{parse_kicad_pcb, read_kicad_pcb};
    use fab_elec_board::{connect, drc, fill_all, resolve_rules, BoardOp, Field, Finish, Flash};
    use fab_elec_circuit::CheckKind;
    use fab_elec_core::{Angle, Sel};
    use fab_elec_parts::{Forbid, PadShape};
    use std::collections::HashSet;

    #[test]
    fn legacy_board_build_and_engines() {
        let read = read_kicad_pcb(br#"(kicad_pcb (version 20171130) (host pcbnew "5.0.2")
 (general (thickness 1.6))
 (layers (0 TOP signal) (1 POWER power) (2 MIXED mixed) (31 BOTTOM signal)
  (32 B.Mask user) (33 F.Mask user) (38 B.SilkS user) (44 Edge.Cuts user))
 (setup (trace_clearance 0.2) (via_size 0.8) (via_drill 0.4) (trace_min 0.15)
  (clearance_min 0.12) (pad_to_mask_clearance 0.06) (pad_to_paste_clearance -0.02)
  (pad_to_paste_clearance_ratio -0.1) (via_min_annulus 0.08) (via_min_drill 0.25)
  (hole_to_hole_min 0.3))
 (net 9 CLK) (net 0 "") (net 3 GND)
 (net_class Fast "clock" (trace_width 0.25) (via_drill 0.35)
  (diff_pair_width 0.25) (diff_pair_gap 0.2) (add_net CLK) (add_net missing))
 (net_class Default "normal" (trace_width 0.3) (via_dia 0.7) (add_net GND))
 (gr_line (start 0 0) (end 12 0) (layer Edge.Cuts) (width 0.05))
 (gr_line (start 12 10) (end 12 0) (layer Edge.Cuts) (width 0.05))
 (gr_arc (start 10 10) (end 12 10) (angle 90) (layer Edge.Cuts) (width 0.05))
 (gr_line (start 0 12) (end 10 12) (layer Edge.Cuts) (width 0.05))
 (gr_line (start 0 0.005) (end 0 12) (layer Edge.Cuts) (width 0.05))
 (module lib:back (layer BOTTOM) (at 5 5 90) locked
  (fp_text reference R1 (at 0 0 90) (layer B.SilkS) (effects (font (size 1 1)) (justify mirror)))
  (fp_arc (start 0 0) (end 2 0) (angle 90) (width 0.1) (layer B.SilkS))
  (pad 1 thru_hole circle (at 1 0 0) (size 1 1) (drill 0.4 (offset 0.2 0.1))
   (layers *.Cu *.Mask) (net 3 GND))
  (pad 2 smd trapezoid (at -1 0 90) (size 1 0.8) (rect_delta 0.1 0.2)
   (layers BOTTOM B.Mask) (net 9 CLK)))
 (module lib:empty (layer TOP) (at 9 1))
 (module lib:empty (layer TOP) (at 10 1))
 (segment (start 1 1) (end 2 1) (width 0.3) (layer TOP) (net 3))
 (segment (start 3 1) (end 2 1) (width 0.3) (layer TOP) (net 3))
 (segment (start 3 1) (end 4 1) (width 0.3) (layer TOP) (net 3))
 (segment (start 2.5 1) (end 2.5 2) (width 0.3) (layer TOP) (net 3))
 (segment (start 5.200005 4.1) (end 6.2 4.1) (width 0.3) (layer TOP) (net 3))
 (segment (start 5 6) (end 6 6) (width 0.25) (layer BOTTOM) (net 9))
 (segment (start 7.2 7.2) (end 7.7 7.2) (width 0.3) (layer TOP) (net 3))
 (segment (start 5.4 4.3) (end 5.5 4.4) (width 0.3) (layer TOP) (net 3))
 (via (at 4.000005 1) (size 0.8) (drill 0.4) (layers TOP BOTTOM) (net 3))
 (zone (net 3) (layer TOP) (name guard)
  (keepout (tracks not_allowed) (vias not_allowed) (copperpour allowed))
  (polygon (pts (xy 9 9) (xy 10 9) (xy 10 10) (xy 9 10))))
 (zone (net 3) (layer TOP) (name pour) (connect_pads yes) (min_thickness 0.2)
  (fill (island_removal_mode 1))
  (polygon (pts (xy 7 7) (xy 8 7) (xy 8 8) (xy 7 8)))
  (filled_polygon (pts (xy unread unread))))
 (gr_line (start 9 2) (end 10 2) (layer B.SilkS) (width 0.1))
 (gr_text label (at 9 3) (layer B.SilkS) (effects (font (size 1 1)))))"#, "legacy")
            .expect("authored legacy board reads");
        let mut board = read.board;
        let report = read.report;
        assert_eq!(board.name, "legacy", "caller supplies board name");
        assert_eq!(board.unit, BoardId(0), "standalone board has unit zero");
        assert_eq!(board.stackup.copper(), [LayerId::F_CU, LayerId(1), LayerId(2), LayerId::B_CU], "custom layer names preserve physical order");
        assert_eq!(board.copper_count(), 4, "copper count comes from constructed stackup");
        assert_eq!(board.stackup.layers.iter().map(|l| l.thickness as i64).sum::<i64>(), 1_600_000, "synthesised layers sum to board thickness");
        let dielectrics: Vec<_> = board.stackup.layers.iter().filter_map(|l| match l.kind {
            StackKind::Dielectric { core } => Some((core, l.thickness, l.er, l.loss_tan)), _ => None,
        }).collect();
        assert_eq!(dielectrics, [(false, 480_000, Some(4.5), Some(0.02)), (true, 480_000, Some(4.5), Some(0.02)), (false, 480_000, Some(4.5), Some(0.02))], "FR-4 alternates prepreg and core with equal thickness");
        assert_eq!(board.stackup.layers[4].role, fab_elec_board::LayerRole::Plane, "power copper keeps its role");
        assert_eq!(board.stackup.layers[6].role, fab_elec_board::LayerRole::Mixed, "mixed copper keeps its role");
        assert_eq!(board.nets.iter().map(|n| n.name.as_str()).collect::<Vec<_>>(), ["", "CLK", "GND"], "net zero is first regardless of table order");
        assert_eq!(board.nets[1].class, ClassId(1), "named net gets non-default class");
        assert_eq!(board.nets[2].class, ClassId(0), "default class assignment");
        assert_eq!(board.classes[0].name, "Default", "default class moves to index zero");
        assert!(board.classes.iter().all(|class| class.rules.track.is_none()), "legacy router widths are not class DRC minima");
        assert_eq!(board.classes[0].rules.clearance, Some(200_000), "legacy setup fills absent default clearance");
        assert_eq!(board.classes[0].rules.via, Some(ViaSize { drill: 400_000, pad: 700_000 }), "legacy via drill fills missing default half");
        assert_eq!(board.classes[1].rules.via, Some(ViaSize { drill: 350_000, pad: 700_000 }), "other class inherits missing via diameter");
        assert_eq!(board.classes[1].rules.pair, Some(PairRule { width: 250_000, gap: 200_000 }), "complete pair rule survives");
        assert_eq!((board.rules.base.track, board.rules.base.clearance, board.rules.base.annular, board.rules.base.drill, board.rules.base.hole_to_hole), (Some(150_000), Some(200_000), Some(80_000), Some(250_000), Some(300_000)), "setup minima and default class clearance supply base rules");
        assert_eq!((board.rules.base.mask_margin, board.rules.base.paste_margin, board.rules.base.paste_ratio_ppm), (Some(60_000), Some(-20_000), Some(-100_000)), "signed mask and paste setup values survive");
        assert_eq!(board.outline.len(), 1, "four lines and legacy arc form one contour");
        let edge = &board.outline[0].path;
        assert_eq!(edge.start, edge.end(), "outline closing gap snaps exactly");
        assert_eq!(edge.segs.len(), 5, "chaining preserves the five outline pieces");
        assert_eq!(edge.segs.iter().filter(|s| matches!(s, Seg::Arc { .. })).count(), 1, "outline arc remains analytic");
        let fp = &board.placed[0];
        assert_eq!((fp.side, fp.rot), (Side::Bottom, Angle::deg(270.0)), "back placement applies the front-form rotation");
        assert_eq!(fp.to_board(fp.footprint.pads[0].at), P::mm(5.2, -4.1), "offset pad copper centre has the board position");
        assert_eq!(fp.to_board(fp.footprint.pads[1].at), P::mm(5.0, -6.0), "back trapezoid has the board position");
        let pad = &fp.footprint.pads[0];
        let drill = pad.drill.as_ref().expect("through pad retains drill");
        assert_eq!(fp.to_board(pad.at.add(drill.offset.rotated(pad.rot))), P::mm(5.0, -4.0), "offset drill keeps its hole centre");
        assert!(matches!(fp.footprint.pads[1].stack.front, PadShape::Trapezoid { .. }), "back trapezoid stays analytic");
        assert!(matches!(&fp.footprint.graphics[0].1, Gfx::Stroke { path, .. } if matches!(path.segs[0], Seg::Arc { mid, .. } if mid == P::new(1_414_214, 1_414_214))), "legacy footprint arc midpoint stays in front form");
        assert_eq!(fp.path, "R1", "missing schematic path falls back to reference");
        assert!(Arc::ptr_eq(&board.placed[1].footprint, &board.placed[2].footprint), "equal footprint contents share one Arc");
        assert_eq!(report.made.get("footprints.shared"), Some(&1), "sharing count records reused copies");
        assert_eq!(board.tracks.len(), 6, "three route pieces merge into one track");
        assert_eq!(board.tracks[0].path.segs.len(), 3, "merged route keeps each original segment");
        assert_eq!(board.tracks[0].path.start, P::mm(1.0, -1.0), "chain starts at first source terminal");
        assert_eq!(board.tracks[0].ends[1], Anchor::Via(board.vias[0].id), "via centre wins within ten nm");
        assert_eq!(board.tracks[0].path.end(), board.vias[0].at, "via anchor snaps the endpoint");
        assert!(matches!(board.tracks[1].ends[0], Anchor::Track { id, .. } if id == board.tracks[0].id), "T junction anchors to the passing track");
        assert_eq!(board.tracks[2].ends[0], Anchor::Pad { placed: fp.id, pad: PadIx(0) }, "through pad gets a pad anchor");
        assert_eq!(board.tracks[2].path.start, fp.to_board(pad.at), "pad anchor snaps the endpoint");
        assert_eq!(board.tracks[3].ends[0], Anchor::Pad { placed: fp.id, pad: PadIx(1) }, "back SMD pad anchors on the flipped copper layer");
        assert_eq!(report.made.get("anchors.free"), Some(&8), "every remaining free end is counted");
        assert_eq!(report.gaps["track.free_end"].count, 4, "ordinary free ends have their own gap count");
        assert_eq!(report.gaps["track.end_in_zone"].count, 2, "pour stubs stay free and are distinguished");
        assert_eq!(report.gaps["track.end_in_pad_off_centre"].count, 2, "off-centre pad stubs stay free and are distinguished");
        assert_eq!(report.gaps["zone.stored_fill"].count, 1, "parse diagnostics survive construction");
        assert_eq!(report.gaps["class.unknown_net"].count, 1, "class assignment never creates a missing net");
        assert!(board.keepouts[0].forbid.has(Forbid::TRACKS.with(Forbid::VIAS)), "rule-area prohibitions survive");
        assert_eq!(board.zones[0].net, Some(NetId(2)), "zone resolves sparse net code");
        assert_eq!((board.graphics.len(), board.texts.len()), (1, 1), "board graphics and text are retained");
        let ids: Vec<_> = board.placed.iter().map(|x| x.id)
            .chain(board.tracks.iter().map(|x| x.id)).chain(board.vias.iter().map(|x| x.id))
            .chain(board.zones.iter().map(|x| x.id)).chain(board.keepouts.iter().map(|x| x.id))
            .chain(board.graphics.iter().map(|x| x.id)).chain(board.texts.iter().map(|x| x.id)).collect();
        assert_eq!(ids.iter().copied().collect::<HashSet<_>>().len(), ids.len(), "all item ids are unique");
        assert_eq!(ids, (0..board.next_item).map(ItemId).collect::<Vec<_>>(), "ids follow the required kind and file order");
        assert_eq!(board.index.len(), ids.len(), "returned board is indexed");
        for p in &board.placed {
            assert_eq!(p.pad_nets.len(), p.footprint.pads.len(), "pad nets parallel footprint pads");
        }
        for t in board.tracks.clone() {
            assert!(board.apply(&BoardOp::SetField { item: t.id, field: Field::Ends(t.ends) }).is_ok(), "board's own anchor validation accepts imported track {}", t.id.0);
            for (end, anchor) in t.ends.into_iter().enumerate() {
                if let Some(pos) = board.anchor_pos(anchor) {
                    assert_eq!(pos, if end == 0 { t.path.start } else { t.path.end() }, "anchored endpoint matches its anchor position");
                }
            }
        }
        let rules = resolve_rules(&board, None);
        let fills = fill_all(&board, &rules);
        assert!(!fills.is_empty(), "zone fill engine processes the imported pour");
        let connectivity = connect(&board, &fills);
        let findings = drc(&board, &connectivity, &rules, &fills, None);
        for t in &board.tracks {
            for (anchor, at) in [(t.ends[0], t.path.start), (t.ends[1], t.path.end())] {
                if anchor != Anchor::Free {
                    assert!(!findings.violations.iter().any(|v| v.kind == CheckKind::Dangling && v.at == Some(at) && v.items.contains(&Sel::Item(board.unit, t.id))), "DRC does not call an anchored endpoint dangling");
                }
            }
        }
    }

    #[test]
    fn explicit_stackup_named_nets_and_open_edges() {
        let mut file = parse_kicad_pcb(br##"(kicad_pcb (version 20260206) (generator "fab")
 (layers (0 "F.Cu" signal) (31 "B.Cu" signal) (44 "Edge.Cuts" user) (36 "F.SilkS" user))
 (setup (stackup
  (layer "F.Mask" (thickness 0.01) (color "#123456"))
  (layer "F.Cu" (thickness 0.035))
  (layer "dielectric 1" (type "core") (thickness 0.7) (material "FR-4") (epsilon_r 4.2))
  (layer "B.Cu" (thickness 0.035))
  (layer "B.Mask" (thickness 0.01)) (copper_finish "ENIG")))
 (net 7 "TABLE")
 (net_class "Signal" "" (diff_pair_width 0.2) (add_net "VIA") (add_net "ZONE"))
 (via (at 1 1) (size 0.6) (drill 0.3) (layers "F.Cu" "B.Cu") (net "VIA"))
 (zone (net 7) (net_name "ZONE") (layer "F.Cu")
  (polygon (pts (xy 4 4) (xy 5 4) (xy 5 5))))
 (segment (start 0 0) (end 1 0) (width 0.2) (layer "F.Cu") (net "0"))
 (footprint "blank" (layer "F.Cu") (at 2 2)
  (pad "1" smd circle (size 1 1) (layers "F.Cu") (net "PAD"))
  (fp_rect (start 0 0) (end 1 1) (layer "Edge.Cuts") (width 0.05) (fill none)))
 (segment (start 6 0) (end 7 0) (width 0.2) (layer "F.Cu") (net 99 "TABLE"))
 (segment (start 8 0) (end 9 0) (width 0.2) (layer "F.Cu") (net 7 "PAD"))
 (segment (start 10 0) (end 11 0) (width 0.2) (layer "F.Cu"))
 (gr_line (start 20 0) (end 21 0) (layer "Edge.Cuts") (width 0.05))
 (gr_line (start 0 0) (end 1 0) (layer "F.SilkS") (width 0.1))
 (gr_text "note" (at 0 2) (layer "F.SilkS") (effects (font (size 1 1)))))"##)
            .expect("authored current board parses");
        let read = build_board(&file, None, "named");
        let board = &read.board;
        assert_eq!(board.stackup.thickness, 790_000, "explicit layer sum supplies missing general thickness");
        assert_eq!((board.stackup.finish, board.stackup.mask_color), (Finish::Enig, 0x123456ff), "explicit finish and colour survive");
        assert_eq!(board.nets.iter().map(|n| n.name.as_str()).collect::<Vec<_>>(), ["", "TABLE", "VIA", "ZONE", "0", "PAD"], "new net names follow source locations across item kinds");
        assert_eq!(board.classes[0].rules, ClassRules {
            clearance: Some(200_000), via: Some(ViaSize { drill: 300_000, pad: 600_000 }),
            pair: Some(PairRule { width: 200_000, gap: 250_000 }), ..ClassRules::default()
        }, "absent Default class receives source-format defaults");
        assert_eq!(board.nets[2].class, ClassId(1), "class can assign an item-created net");
        assert_eq!(board.zones[0].net, Some(NetId(3)), "zone legacy name overrides code");
        assert_eq!(board.tracks[0].net, NetId(4), "numeric name zero is an ordinary named net");
        assert_eq!(board.placed[0].pad_nets, [Some(NetId(5))], "named pad net is resolved");
        assert_eq!(read.report.gaps["class.pair_partial"].count, 1, "incomplete pair rule counted");
        assert_eq!(read.report.gaps["net.unknown_code"].count, 1, "unknown code counted once");
        assert_eq!(read.report.gaps["net.code_name_mismatch"].count, 1, "code/name mismatch counted once");
        assert_eq!(read.report.gaps["net.netless_copper"].count, 3, "unresolved and absent copper nets become net zero");
        assert_eq!(read.report.gaps["outline.open"].count, 1, "unclosed edge survives as a drawing with a gap");
        assert_eq!(board.graphics[0].layer, LayerId::EDGE, "open edge keeps its source position among graphics");
        assert_eq!(board.outline.len(), 1, "footprint edge rectangle contributes a closed contour");
        assert_eq!(board.outline[0].bbox(), BBox::new(P::mm(2.0, -3.0), P::mm(3.0, -2.0)), "footprint outline copy is in board coordinates");
        assert_eq!(board.placed[0].footprint.graphics.len(), 1, "footprint retains its own edge drawing");
        file.thickness = Some(Located { at: (1, 1), value: 1_200_000 });
        assert_eq!(build_board(&file, None, "thickness").board.stackup.thickness, 1_200_000, "general thickness overrides explicit layer sum");
    }

    #[test]
    fn joins_rings_arcs_and_anchor_priority() {
        let read = read_kicad_pcb(br#"(kicad_pcb (version 20260206)
 (layers (0 F.Cu signal) (31 B.Cu signal))
 (segment (start 0 0) (end 1 0) (width 0.2) (layer F.Cu) (net A))
 (arc (start 2 0) (mid 1.5 -0.5) (end 1 0) (width 0.2) (layer F.Cu) (net A))
 (segment (start 2 0) (end 3 0) (width 0.2) (layer F.Cu) (net A))
 (segment (start 5 0) (end 6 0) (width 0.2) (layer F.Cu) (net A))
 (segment (start 6 0) (end 6 1) (width 0.2) (layer F.Cu) (net A))
 (segment (start 6 1) (end 5 0) (width 0.2) (layer F.Cu) (net A))
 (segment (start 8 0) (end 9 0) (width 0.2) (layer F.Cu) (net A))
 (segment (start 9 0) (end 10 0) (width 0.2) (layer F.Cu) (net A))
 (via (at 9 0) (size 0.6) (drill 0.3) (layers F.Cu B.Cu) (net A))
 (footprint same (layer F.Cu) (at 9 0)
  (pad 1 smd circle (size 1 1) (layers F.Cu) (net A)))
 (segment (start 12 0) (end 13 0) (width 0.2) (layer F.Cu) (net A))
 (segment (start 13 0) (end 14 0) (width 0.3) (layer F.Cu) (net A))
 (segment (start 12.5 0) (end 12.5 1) (width 0.3) (layer F.Cu) (net A))
 (segment (start 12.5 0) (end 12.5 -1) (width 0.4) (layer F.Cu) (net A))
 (segment (start 20 0) (end 21 0) (width 0.2) (layer F.Cu) (net A))
 (segment (start 21 0) (end 22 0) (width 0.2) (layer F.Cu) (net A))
 (segment (start 21 0) (end 21 1) (width 0.2) (layer F.Cu) (net B)))"#, "joins")
            .expect("authored joining cases read");
        let b = read.board;
        assert_eq!(b.tracks.len(), 11, "joins stop at via, width changes and all-net endpoint degree");
        assert_eq!(b.tracks[0].path.segs[1], Seg::Arc { mid: P::mm(1.5, 0.5), end: P::mm(2.0, 0.0) }, "reversing an arc keeps its midpoint");
        assert_eq!((b.tracks[1].path.start, b.tracks[1].path.end()), (P::mm(5.0, 0.0), P::mm(5.0, 0.0)), "ring is cut at the first piece start");
        assert_eq!(b.tracks[1].ends, [Anchor::Free; 2], "ring never anchors to itself");
        assert_eq!(b.tracks[2].ends[1], Anchor::Via(b.vias[0].id), "via takes precedence over coincident pad");
        assert_eq!(b.tracks[3].ends[0], Anchor::Via(b.vias[0].id), "via centre blocks merging from both sides");
        assert!(matches!(b.tracks[4].ends[1], Anchor::Track { id, .. } if id == b.tracks[5].id), "width boundary anchors to neighbour");
        assert!(matches!(b.tracks[5].ends[0], Anchor::Track { id, .. } if id == b.tracks[4].id), "endpoint cycles remain valid");
        for i in [6, 7] {
            assert!(matches!(b.tracks[i].ends[0], Anchor::Track { id, .. } if id == b.tracks[4].id), "passing track wins over coincident endpoint host");
            assert_eq!(b.tracks[i].ends[0], b.track_anchor(b.tracks[4].id, b.tracks[i].path.start).expect("host exists"), "indexed projection agrees with board track_anchor");
        }
        for t in &b.tracks {
            assert!(t.ends.iter().all(|a| !matches!(a, Anchor::Track { id, .. } if *id == t.id)), "no track anchor names its own track");
        }
    }

    #[test]
    fn via_flags_set_the_ring_mode_and_the_file_zone_layers_meet_our_fill() {
        // Every flag form on a through via of net A over an In1 pour of net A. The zone layer lists
        // agree with our fill on the vias at 5 and 22 (the latter rings In1 for its track, so the
        // pour changes nothing there), and differ at 8 (lists none) and 20 (lists In2 too).
        let read = read_kicad_pcb(br#"(kicad_pcb (version 20240108)
 (layers (0 "F.Cu" signal) (1 "In1.Cu" signal) (2 "In2.Cu" signal) (31 "B.Cu" signal) (44 "Edge.Cuts" user))
 (net 0 "") (net 1 "A")
 (gr_line (start 0 0) (end 24 0) (layer "Edge.Cuts") (width 0.05))
 (gr_line (start 24 0) (end 24 10) (layer "Edge.Cuts") (width 0.05))
 (gr_line (start 24 10) (end 0 10) (layer "Edge.Cuts") (width 0.05))
 (gr_line (start 0 10) (end 0 0) (layer "Edge.Cuts") (width 0.05))
 (via (at 2 5) (size 0.6) (drill 0.3) (layers "F.Cu" "B.Cu") (net 1))
 (via (at 5 5) (size 0.6) (drill 0.3) (layers "F.Cu" "B.Cu") (remove_unused_layers) (zone_layer_connections "In1.Cu") (net 1))
 (via (at 8 5) (size 0.6) (drill 0.3) (layers "F.Cu" "B.Cu") (remove_unused_layers yes) (keep_end_layers yes) (zone_layer_connections) (net 1))
 (via (at 11 5) (size 0.6) (drill 0.3) (layers "F.Cu" "B.Cu") (start_end_only yes) (net 1))
 (via (at 14 5) (size 0.6) (drill 0.3) (layers "F.Cu" "B.Cu") (keep_end_layers) (net 1))
 (via (at 17 5) (size 0.6) (drill 0.3) (layers "F.Cu" "B.Cu") (remove_unused_layers no) (zone_layer_connections "In2.Cu") (net 1))
 (via (at 20 5) (size 0.6) (drill 0.3) (layers "F.Cu" "B.Cu") (remove_unused_layers) (zone_layer_connections "In1.Cu" "In2.Cu") (net 1))
 (via (at 22 5) (size 0.6) (drill 0.3) (layers "F.Cu" "B.Cu") (remove_unused_layers) (zone_layer_connections "In1.Cu") (net 1))
 (segment (start 22 5) (end 23 5) (width 0.2) (layer "In1.Cu") (net 1))
 (zone (net 1) (layer "In1.Cu") (name "pour")
  (polygon (pts (xy 0.5 0.5) (xy 23.5 0.5) (xy 23.5 9.5) (xy 0.5 9.5)))))"#, "flash").expect("via flag board reads");
        let board = &read.board;
        let modes: Vec<Flash> = board.vias.iter().map(|v| v.flash).collect();
        assert_eq!(modes, [Flash::All, Flash::Connected, Flash::ConnectedEnds, Flash::Ends, Flash::All, Flash::All,
            Flash::Connected, Flash::Connected], "each flag form, bare or yes/no, maps to its ring mode");
        assert!(!read.report.gaps.contains_key("via.layer_flashing"), "the flags are read, not skipped");
        assert_eq!(read.zone_flash.vias.iter().map(|z| z.0).collect::<Vec<_>>(),
            [board.vias[1].id, board.vias[2].id, board.vias[6].id, board.vias[7].id],
            "only vias whose rings follow the copper keep the file's zone layers");
        let in1 = LayerId::inner(1);
        assert_eq!(board.rings(&board.vias[7]).fixed, LayerSet::one(in1), "the In1 track rings the last via there");
        let rules = resolve_rules(board, None);
        let fills = fill_all(board, &rules);
        let reached = board.reached_rings(&fills);
        assert_eq!(reached, [(board.vias[1].id, in1), (board.vias[2].id, in1), (board.vias[6].id, in1)],
            "the pour reaches the offered In1 ring of each following via without a fixed one");
        let mut report = read.report.clone();
        assert_eq!(read.zone_flash.compare(board, &fills, &mut report), 2, "the vias at 8 and 20 differ from the file");
        let gap = &report.gaps["via.zone_flash_differs"];
        assert_eq!((gap.kind, gap.count), (GapKind::Approximated, 2), "each differing via is reported once");
        assert_eq!(gap.first.1, 10, "the report points at the first differing via's line");
    }
}
