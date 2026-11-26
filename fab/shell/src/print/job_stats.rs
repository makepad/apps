//! All the stats beside Monitor's view: everything the printer reports, and the job's own numbers
//! from the slice that went to it.
//!
//! Main-loop cost: one walk over the job's beads per job (their filament and length, a few ms for
//! a million beads), and a few lines formatted on a draw, once a second while a job runs.

use super::colour_map::to_srgb;
use super::trays::DrawFabChip;
use super::*;
use crate::api::AppState;
use fab_printer_link::model::LoadedSlot;
use fab_printer_link::Snapshot;

script_mod! {
    use mod.prelude.fab.*
    use mod.widgets.*

    mod.widgets.FabColourRowsBase = #(FabColourRows::register_widget(vm))
    mod.widgets.FabColourRows = set_type_default() do mod.widgets.FabColourRowsBase{
        width: Fill
        height: Fit
        draw_chip: mod.draw.DrawFabChip{}
        draw_text: mod.draw.DrawText{
            color: fab.color_text_dim
            text_style: theme.font_regular{ font_size: fab.font_size_small }
        }
    }
}

/// The share of one filament printed by the end of each of the job's layers.
#[derive(Clone, Debug)]
struct FilamentSpent {
    /// 0-based slot, as in [`FilamentUse::slot`].
    slot: usize,
    by_layer: Vec<f32>,
}

/// The job's own numbers, from its slice.
#[derive(Clone, Debug, Default)]
pub struct JobStats {
    filaments: Vec<FilamentSpent>,
    /// When the printing filament changes (preview seconds), ascending.
    changes: Vec<f64>,
}

impl JobStats {
    pub fn of(pv: &PrintPreview) -> Self {
        let layers = pv.layers.len().max(1);
        let mut volumes: Vec<Vec<f64>> = Vec::new();
        let mut changes = Vec::new();
        let mut last = None;
        let mut at = 0;
        for bead in pv.beads.each_in_time() {
            let b = Bead::of(bead);
            let t0 = b.t0 as f64;
            while at + 1 < pv.layers.len() && pv.layers[at + 1].t_start <= t0 {
                at += 1;
            }
            // filament (1-based) + 256 * its joints, `join_beads`.
            let slot = (b.filament as u32 % 256).saturating_sub(1) as usize;
            if last.is_some_and(|l| l != slot) {
                changes.push(t0);
            }
            last = Some(slot);
            if volumes.len() <= slot {
                volumes.resize(slot + 1, vec![0.0; layers]);
            }
            let d = [b.b[0] - b.a[0], b.b[1] - b.a[1], b.b[2] - b.a[2]];
            let length = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
            // its width and height, negative at a free end
            volumes[slot][at] += (length * b.width.abs() * b.height.abs()) as f64;
        }
        let filaments = volumes.into_iter().enumerate().filter_map(|(slot, by_layer)| {
            let total: f64 = by_layer.iter().sum();
            (total > 0.0).then(|| {
                let mut sum = 0.0;
                let by_layer = by_layer.iter().map(|v| { sum += v; (sum / total) as f32 }).collect();
                FilamentSpent { slot, by_layer }
            })
        }).collect();
        JobStats { filaments, changes }
    }

    /// The tool changes done by time `t`, and how many there are.
    pub fn changes_at(&self, t: f64) -> (usize, usize) {
        (self.changes.partition_point(|c| *c <= t), self.changes.len())
    }

    /// The share of filament `slot` printed by time `t`, by its layers (within a layer, by time).
    fn printed(&self, pv: &PrintPreview, slot: usize, t: f64) -> f64 {
        let Some(f) = self.filaments.iter().find(|f| f.slot == slot) else { return 0.0 };
        let Some(layer) = pv.layers.get(pv.layer_at(t)) else { return 0.0 };
        let i = pv.layer_at(t);
        let before = if i == 0 { 0.0 } else { f.by_layer[i - 1] as f64 };
        let after = f.by_layer[i] as f64;
        let within = ((t - layer.t_start) / (layer.t_end - layer.t_start).max(1e-6)).clamp(0.0, 1.0);
        before + (after - before) * within
    }
}

/// When a job with this much left finishes, on the local clock: "14:32", "tomorrow 07:10".
pub fn finish_at(left_minutes: f64) -> String {
    use makepad_civil_time as civil;
    let now = civil::now_secs();
    let today = civil::local_or_utc(now).day_number();
    let t = civil::local_or_utc(now + (left_minutes * 60.0).round() as i64);
    if t.day_number() == today {
        t.hm()
    } else if t.day_number() == civil::add_days(today, 1) {
        format!("tomorrow {}", t.hm())
    } else {
        format!("{} {} {}", t.day, &civil::month_name(t.month)[..3], t.hm())
    }
}

/// Each head: what feeds it, in that filament's colour, and which one is printing.
pub fn head_rows(printer: &Snapshot) -> Vec<(Option<[f32; 4]>, String)> {
    let s = &printer.state;
    s.heads().into_iter().filter_map(|extruder| {
        let id = extruder.id?;
        let printing = if s.busy() && s.active_extruder == Some(id) { ", printing" } else { "" };
        let name = s.head_name(id)?;
        if s.numbered_heads {
            let tray = s.tray(fab_printer_link::model::TrayId { ams: id.0, slot: 0 });
            if tray.is_some_and(|t| t.present == Some(false)) {
                return Some((None, format!("{name}{printing}: empty")));
            }
            let material = tray.and_then(|t| t.material.as_deref()).filter(|m| !m.is_empty()).unwrap_or("no filament reported");
            let colour = tray.and_then(|t| t.colour.as_ref()).map(|c| c.0);
            return Some((colour, format!("{name}{printing}: {material}")));
        }
        let Some(LoadedSlot::Tray(id)) = extruder.loaded else {
            return Some((None, format!("{name}{printing}: no tray feeding it")));
        };
        let tray = s.tray(id);
        let material = tray.and_then(|t| t.material.clone()).filter(|m| !m.is_empty()).unwrap_or_else(|| "filament".into());
        let left = tray.and_then(|t| t.remain).filter(|_| !matches!(id.ams, 254 | 255)).map(|r| format!(", {r:.0} % left")).unwrap_or_default();
        let colour = tray.and_then(|t| t.colour.as_ref()).map(|c| c.0);
        Some((colour, format!("{name}{printing}: {material} from {}{left}", super::trays::tray_label(Some(id)))))
    }).collect()
}

/// Everything else the printer says, a line each: how far, speed, fans, its connection and errors.
/// `heard`: seconds since its last report.
pub fn printer_lines(printer: &Snapshot, heard: Option<f64>) -> String {
    let s = &printer.state;
    let mut lines = Vec::new();
    if s.busy() {
        let mut far = Vec::new();
        if let (Some(l), Some(t)) = (s.layer, s.total_layers.filter(|t| *t > 0)) {
            far.push(format!("Layer {l} / {t}"));
        }
        if let Some(p) = s.progress {
            far.push(format!("{p:.0} %"));
        }
        if let Some(m) = s.remaining_minutes.filter(|m| *m > 0.0) {
            far.push(format!("{} left, finish about {}", format_time(m * 60.0), finish_at(m)));
        }
        if !far.is_empty() {
            lines.push(far.join(" · "));
        }
    }
    let level = s.speed_level.map(|l| match l {
        1 => "Silent".to_string(),
        2 => "Standard".to_string(),
        3 => "Sport".to_string(),
        4 => "Ludicrous".to_string(),
        n => format!("level {n}"),
    });
    match (level, s.speed_percent) {
        (Some(l), Some(p)) => lines.push(format!("Speed: {l}, {p:.0} %")),
        (Some(l), None) => lines.push(format!("Speed: {l}")),
        (None, Some(p)) => lines.push(format!("Speed: {p:.0} %")),
        _ => {}
    }
    let fans: Vec<String> = [("part cooling", s.fans.part), ("heatbreak", s.fans.heatbreak), ("aux", s.fans.aux), ("chamber", s.fans.chamber)]
        .into_iter().filter_map(|(name, level)| level.map(|l| format!("{name} {l}/15"))).collect();
    if !fans.is_empty() {
        lines.push(format!("Fans: {}", fans.join(" · ")));
    }
    let mut link = vec![format!("Connection: {}", printer.status.label())];
    if let Some(age) = heard {
        link.push(format!("last report {} ago", format_time(age)));
    }
    if let Some(w) = &s.wifi {
        link.push(format!("Wi-Fi {w}"));
    }
    lines.push(link.join(" · "));
    if let Some(f) = &s.firmware {
        lines.push(format!("Firmware updater: {f}"));
    }
    if let Some(e) = s.print_error.filter(|e| *e != 0) {
        lines.push(format!("Printer error {e:08X}"));
    }
    for (field, value) in &s.errors {
        lines.push(format!("{field}: {value}"));
    }
    for h in &s.hms {
        let severity = match h.severity { 1 => "fatal", 2 => "serious", 3 => "common", 4 => "info", _ => "unknown severity" };
        lines.push(format!("{} ({severity})", h.code()));
    }
    lines.join("\n")
}

/// "layer height 0.20 mm", "first layer 0.25 mm, then 0.20 mm", "first layer 0.25 mm, then 0.08 to 0.20 mm".
fn heights_text(pv: &PrintPreview) -> String {
    let Some(first) = pv.layers.first().map(|l| l.z) else { return String::new() };
    let steps = pv.layers.windows(2).map(|w| w[1].z - w[0].z).filter(|h| *h > 1e-4);
    let (lo, hi) = steps.fold((f32::MAX, 0.0f32), |(lo, hi), h| (lo.min(h), hi.max(h)));
    let same = |a: f32, b: f32| (a - b).abs() < 0.005;
    if lo > hi || (same(lo, hi) && same(lo, first)) {
        format!("layer height {first:.2} mm")
    } else if same(lo, hi) {
        format!("first layer {first:.2} mm, then {lo:.2} mm")
    } else {
        format!("first layer {first:.2} mm, then {lo:.2} to {hi:.2} mm")
    }
}

/// The job's own numbers: planned against elapsed, its layers, its tool changes. `now`: as
/// `CommittedJob::sent_at`.
pub fn job_lines(p: &PrintState, pv: &PrintPreview, stats: &JobStats, now: f64) -> String {
    let mut lines = Vec::new();
    let planned = format!("Planned {} ({} of it printer routines)", format_time(pv.total), format_time(pv.prepare_seconds));
    let ran = |job: &&jobs::CommittedJob| !matches!(job.state, jobs::JobState::Queued | jobs::JobState::Preparing | jobs::JobState::EndedUnseen);
    lines.push(match p.follow.shown().filter(ran) {
        Some(job) => format!("Elapsed {} · {planned}", format_time(job.ended_at.unwrap_or(now) - job.sent_at)),
        None => planned,
    });
    if p.follow.live > 0.0 {
        lines.push(format!("{} of {} done by Fab's slice timing", format_time(p.follow.live), format_time(pv.total)));
    }
    let tall = pv.layers.last().map_or(0.0, |l| l.z);
    lines.push(format!("{} layers, {tall:.2} mm tall, {}", pv.max_layer() + 1, heights_text(pv)));
    let (done, total) = stats.changes_at(p.follow.live);
    lines.push(if total == 0 { "No tool changes".to_string() } else { format!("Tool changes: {done} of {total} done") });
    lines.join("\n")
}

/// Each filament of the job, in its colour: used and left, by Fab's slice timing.
pub fn filament_rows(mode: fab_plate::ColourMode, p: &PrintState, pv: &PrintPreview, stats: &JobStats) -> Vec<(Option<[f32; 4]>, String)> {
    pv.filament_use.iter().map(|f| {
        let done = stats.printed(pv, f.slot, p.follow.live);
        let colour = pv.colours.shown(mode).get(f.slot).map(|c| {
            let s = to_srgb([c[0], c[1], c[2]]);
            [s.x, s.y, s.z, 1.0]
        });
        (colour, format!(
            "Filament {}: {:.1} g used, {:.1} g left of {:.1} g · {:.2} of {:.2} m",
            f.slot + 1, f.grams * done, f.grams * (1.0 - done), f.grams, f.metres * done, f.metres,
        ))
    }).collect()
}

/// Rows of a colour and a line: the printer's heads, or the job's filaments.
#[derive(Script, ScriptHook, Widget)]
pub struct FabColourRows {
    #[uid]
    uid: WidgetUid,
    #[walk]
    walk: Walk,
    #[redraw]
    #[live]
    draw_chip: DrawFabChip,
    #[live]
    draw_text: DrawText,
    /// The job's filaments (else the printer's heads).
    #[live]
    job: bool,
    /// Each line as drawn, its accessible items.
    #[rust]
    items: Vec<(String, Rect)>,
}

const ROW_H: f64 = 18.0;
const CHIP: f64 = 11.0;

impl Widget for FabColourRows {
    fn semantics(&self, _cx: &Cx, node: &mut a11y_tree::Node) {
        node.role = a11y_tree::Role::List;
        makepad_platform::a11y::set_string(&mut node.name, if self.job { "Filaments of the job" } else { "The printer's heads" });
        node.set_size = self.items.len() as u32;
    }

    fn semantics_parts(&self, _cx: &Cx, out: &mut Vec<(u64, Option<u64>, a11y_tree::Node)>) {
        for (i, (label, rect)) in self.items.iter().enumerate() {
            let mut node = a11y_tree::Node { role: a11y_tree::Role::ListItem, bounds: makepad_platform::a11y::semantic_rect(*rect), ..Default::default() };
            makepad_platform::a11y::set_string(&mut node.name, label);
            out.push((i as u64 + 1, None, node));
        }
    }

    fn handle_event(&mut self, _cx: &mut Cx, _event: &Event, _scope: &mut Scope) {}

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        self.items.clear();
        let rows = scope.data.get::<AppState>().map_or_else(Vec::new, |state| {
            let p = &state.print;
            if self.job {
                p.job_stats().map_or_else(Vec::new, |(pv, stats)| filament_rows(state.view.colours, p, pv, stats))
            } else {
                p.shown_printer().map_or_else(Vec::new, |printer| head_rows(printer))
            }
        });
        let rect = cx.walk_turtle(Walk { height: Size::Fixed(rows.len() as f64 * ROW_H), ..walk });
        for (i, (colour, text)) in rows.into_iter().enumerate() {
            let y = rect.pos.y + i as f64 * ROW_H;
            if let Some([r, g, b, a]) = colour {
                self.draw_chip.spool = 0.0;
                self.draw_chip.empty = 0.0;
                self.draw_chip.fill = vec4(r, g, b, a);
                self.draw_chip.ring = vec4(1.0, 1.0, 1.0, 0.25);
                self.draw_chip.radius = 3.0;
                self.draw_chip.draw_abs(cx, Rect { pos: dvec2(rect.pos.x, y + 2.0), size: dvec2(CHIP, CHIP) });
            }
            self.draw_text.draw_abs(cx, dvec2(rect.pos.x + CHIP + 6.0, y + 1.0), &text);
            self.items.push((text, Rect { pos: dvec2(rect.pos.x, y), size: dvec2(rect.size.x, ROW_H) }));
        }
        DrawStep::done()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fab_printer_link::{model::Reports, ConnectionStatus};

    /// Four layers of ten seconds: filament 1 on the first two, filament 2 on the others, then
    /// back to filament 1 on the last.
    fn two_colours() -> Arc<PrintPreview> {
        let mut pv = follow::tests::preview_of(4);
        let mut data = Vec::new();
        for (l, filament) in [1.0f32, 1.0, 2.0, 1.0].into_iter().enumerate() {
            let t = 10.0 + l as f32 * 10.0;
            data.extend_from_slice(&crate::print::Bead { a: [0.0, 0.0, 0.2], b: [10.0, 0.0, 0.2], t0: t, dur: 10.0, width: 0.4, height: 0.2, value: 100.0, layer: l as f32, role: 1.0, filament: filament + 256.0 * (127.0 + 256.0 * 127.0), ..Default::default() }.pack());
        }
        let p = Arc::get_mut(&mut pv).unwrap();
        p.beads = crate::print::BeadStreams::surface(RetainedInstances::new(PREVIEW_SLOTS, data.into()).unwrap());
        p.filament_use = vec![FilamentUse { slot: 0, grams: 3.0, metres: 1.5, ..Default::default() }, FilamentUse { slot: 1, grams: 1.0, metres: 0.5, ..Default::default() }];
        p.colours.material = vec![[1.0, 0.0, 0.0, 1.0], [0.0, 0.0, 1.0, 1.0]];
        p.colours.model = vec![[0.0, 1.0, 0.0, 1.0], [1.0; 4]];
        pv
    }

    #[test]
    fn the_jobs_numbers_come_from_its_slice() {
        let pv = two_colours();
        let stats = JobStats::of(&pv);
        // The changes: into filament 2, and back.
        assert_eq!(stats.changes_at(29.0), (0, 2));
        assert_eq!(stats.changes_at(30.0), (1, 2));
        assert_eq!(stats.changes_at(45.0), (2, 2));
        let mut p = PrintState::default();
        // Half way through the second layer: half of filament 1's three layers' first two thirds.
        p.follow.live = 25.0;
        let rows = filament_rows(fab_plate::ColourMode::Material, &p, &pv, &stats);
        assert_eq!(rows[0].1, "Filament 1: 1.5 g used, 1.5 g left of 3.0 g · 0.75 of 1.50 m");
        assert_eq!(rows[1].1, "Filament 2: 0.0 g used, 1.0 g left of 1.0 g · 0.00 of 0.50 m");
        let model = filament_rows(fab_plate::ColourMode::Model, &p, &pv, &stats);
        assert_eq!(model[0].1, rows[0].1, "changing mode keeps the quantities");
        assert_ne!(model[0].0, rows[0].0, "the job's model and spool swatches differ");
        // Printed through: all of it.
        p.follow.live = pv.total;
        let rows = filament_rows(fab_plate::ColourMode::Material, &p, &pv, &stats);
        assert_eq!(rows[1].1, "Filament 2: 1.0 g used, 0.0 g left of 1.0 g · 0.50 of 0.50 m");
        // The lines: planned, the layers and their heights, the changes.
        p.follow.live = 30.0;
        let lines = job_lines(&p, &pv, &stats, 0.0);
        assert!(lines.contains("Planned 0:50 (0:10 of it printer routines)"), "{lines}");
        assert!(lines.contains("4 layers, 0.80 mm tall, layer height 0.20 mm"), "{lines}");
        assert!(lines.contains("Tool changes: 1 of 2 done"), "{lines}");
        assert!(lines.contains("0:30 of 0:50 done by Fab's slice timing"), "{lines}");
    }

    #[test]
    fn everything_the_printer_reports_has_a_line() {
        let report = r#"{"print":{"msg":0,"gcode_state":"RUNNING","layer_num":40,"total_layer_num":100,"mc_percent":40,"mc_remaining_time":19,
            "cooling_fan_speed":"10","heatbreak_fan_speed":"15","big_fan1_speed":"0","big_fan2_speed":"5","spd_lvl":2,"spd_mag":100,"wifi_signal":"-52dBm",
            "print_error":50348045,"fail_reason":"nozzle clog","hms":[{"attr":83951616,"code":131074}],
            "upgrade_state":{"status":"IDLE"},
            "ams":{"ams":[{"id":"1","info":"1003","tray":[{"id":"1","tray_type":"PLA","tray_color":"8E9089FF","remain":80}]}]},
            "device":{"extruder":{"state":17,"info":[{"id":1,"temp":14418140,"snow":257},{"id":0,"temp":30,"snow":65535}]}}}}"#;
        let state = Reports::default().apply(report.as_bytes()).unwrap().unwrap();
        let printer = Snapshot { name: "Sim".into(), status: ConnectionStatus::Connected, state: Arc::new(state), ..Default::default() };
        let lines = printer_lines(&printer, Some(2.0));
        for want in [
            "Layer 40 / 100 · 40 % · 19:00 left, finish about ",
            "Speed: Standard, 100 %",
            "Fans: part cooling 10/15 · heatbreak 15/15 · aux 0/15 · chamber 5/15",
            "Connection: Connected · last report 0:02 ago · Wi-Fi -52dBm",
            "Firmware updater: IDLE",
            "Printer error 0300400D",
            "fail_reason: nozzle clog",
            "(serious)",
        ] {
            assert!(lines.contains(want), "{want} in {lines}");
        }
        let heads = head_rows(&printer);
        assert_eq!(heads[0].1, "Left nozzle, printing: PLA from B2, 80 % left");
        assert!(heads[0].0.is_some());
        assert_eq!(heads[1], (None, "Right nozzle: no tray feeding it".to_string()));
    }

    #[test]
    fn layer_heights_say_what_differs() {
        let mut pv = follow::tests::preview_of(3);
        assert_eq!(heights_text(&pv), "layer height 0.20 mm");
        let p = Arc::get_mut(&mut pv).unwrap();
        p.layers[0].z = 0.25;
        p.layers[1].z = 0.45;
        p.layers[2].z = 0.65;
        assert_eq!(heights_text(&pv), "first layer 0.25 mm, then 0.20 mm");
        let p = Arc::get_mut(&mut pv).unwrap();
        p.layers[2].z = 0.53;
        assert_eq!(heights_text(&pv), "first layer 0.25 mm, then 0.08 to 0.20 mm");
    }

    #[test]
    fn numbered_head_rows_show_printing_empty_and_unknown_spools() {
        let mut state = super::super::panel::tests::numbered_report();
        let rows = |state| head_rows(&Snapshot { state: Arc::new(state), ..Default::default() });
        assert_eq!(rows(state.clone()).into_iter().map(|(_, text)| text).collect::<Vec<_>>(), [
            "Tool 1: PLA", "Tool 2: PLA", "Tool 3, printing: PLA", "Tool 4: PLA",
        ]);
        state.ams[0].trays[0].present = Some(false);
        state.extruders.iter_mut().find(|e| e.id == Some(fab_printer_link::model::CloudNozzleId(0))).unwrap().loaded = None;
        state.ams[3].trays[0].material = None;
        let rows = rows(state);
        assert_eq!(rows[0], (None, "Tool 1: empty".into()));
        assert_eq!(rows[3], (None, "Tool 4: no filament reported".into()));
    }
}
