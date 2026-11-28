//! The printer's filament at a glance: [`FabTrayGrid`] draws every tray of
//! the linked printer (each AMS unit as a row of spools with its real colour,
//! material and what is left, the external spool last). It reads
//! `AppState::print` and draws directly. A virtual tray opens its spool menu.

use super::*;
use crate::api::AppState;

script_mod! {
    use mod.prelude.fab.*
    use mod.widgets.*

    /// A rounded box with a fill, a ring and an optional "spool" look (the
    /// colour as a disc with a hub).
    mod.draw.DrawFabChip = mod.std.set_type_default() do #(DrawFabChip::script_shader(vm)){
        ..mod.draw.DrawQuad
        fill: vec4(0.5, 0.5, 0.5, 1.0)
        ring: vec4(0.0, 0.0, 0.0, 0.0)
        ring_width: 1.0
        radius: 3.0
        spool: 0.0
        empty: 0.0
        level: 1.0
        disc: 0.0
        wedge: 0.0
        pixel: fn() {
            let sdf = Sdf2d.viewport(self.pos * self.rect_size)
            let w = self.rect_size.x
            let h = self.rect_size.y
            if self.disc > 0.5 {
                if self.wedge > 0.0 {
                    let v = vec2(w * 0.5 - 11.0, h * 0.5 + 8.0)
                    sdf.move_to(v.x + 22.0, v.y)
                    sdf.line_to(v.x, v.y)
                    sdf.line_to(v.x + 20.0 * cos(self.wedge), v.y - 20.0 * sin(self.wedge))
                    sdf.stroke(self.fill, 2.0)
                    return sdf.result
                }
                sdf.circle(w * 0.5, h * 0.5, self.disc * 0.5)
                sdf.fill_keep(self.fill)
                sdf.stroke(self.ring, 1.5)
                return sdf.result
            }
            if self.spool > 0.5 {
                let c = vec2(w * 0.5, h * 0.5)
                let r = min(w, h) * 0.5 - 1.0
                sdf.circle(c.x, c.y, r)
                if self.empty > 0.5 {
                    sdf.fill_keep(vec4(fab.color_input.xyz, 1.0))
                    sdf.stroke(fab.color_border_light, 1.0)
                    sdf.move_to(c.x - r * 0.5, c.y + r * 0.5)
                    sdf.line_to(c.x + r * 0.5, c.y - r * 0.5)
                    sdf.stroke(fab.color_text_well_muted, 1.2)
                    return sdf.result
                }
                sdf.fill_keep(vec4(self.fill.xyz, 1.0))
                sdf.stroke(vec4(0.0, 0.0, 0.0, 0.35), 1.0)
                // What is left: an arc band near the rim.
                sdf.circle(c.x, c.y, r * 0.42)
                sdf.fill_keep(vec4(fab.color_editor.xyz, 1.0))
                sdf.stroke(vec4(1.0, 1.0, 1.0, 0.18), 1.0)
                return sdf.result
            }
            let inset = self.ring_width * 0.5
            sdf.box(inset, inset, w - self.ring_width, h - self.ring_width, self.radius)
            sdf.fill_keep(self.fill)
            sdf.stroke(self.ring, self.ring_width)
            return sdf.result
        }
    }

    // The app's tooltip look (the `TipLayer` bubble's fill and corner, its text), for what rides
    // the pointer as a tip would: a drag's chip (a feature row, a file, a tab). Its ring is its
    // fill, so the edge is the fill's (a clear ring would lighten it).
    mod.draw.FabTipChip = mod.draw.DrawFabChip{ radius: 4.0 fill: #x10141bf2 ring: #x10141bf2 }
    mod.draw.FabTipText = mod.draw.DrawText{
        color: #xdfe6ec
        text_style: theme.font_regular{ font_size: 9 }
    }

    mod.widgets.FabTrayGridBase = #(FabTrayGrid::register_widget(vm))
    mod.widgets.FabTrayGrid = set_type_default() do mod.widgets.FabTrayGridBase{
        width: Fill
        height: Fit
        draw_chip: mod.draw.DrawFabChip{}
        draw_name: mod.draw.DrawText{
            color: fab.color_text
            text_style: theme.font_regular{ font_size: fab.font_size_ui }
        }
        draw_small: mod.draw.DrawText{
            color: fab.color_text_dim
            text_style: theme.font_regular{ font_size: fab.font_size_small }
        }
    }
}

#[derive(Script, ScriptHook, Debug)]
#[repr(C)]
pub struct DrawFabChip {
    #[deref]
    draw_super: DrawQuad,
    #[live]
    pub fill: Vec4f,
    #[live]
    pub ring: Vec4f,
    #[live]
    pub ring_width: f32,
    #[live]
    pub radius: f32,
    #[live]
    pub spool: f32,
    #[live]
    pub empty: f32,
    #[live]
    pub level: f32,
    /// A disc of this diameter (points) centred in the rect; with `wedge` it is an angle of that
    /// many radians drawn as two rays instead.
    #[live]
    pub disc: f32,
    #[live]
    pub wedge: f32,
}

fn rgba(c: [f32; 4]) -> Vec4f {
    vec4(c[0], c[1], c[2], c[3])
}

/// A tray's label: "A1" for AMS 0 slot 0, "Ext L" / "Ext R" for the external spools.
pub fn tray_label(id: Option<fab_printer_link::model::TrayId>) -> String {
    match id {
        Some(id) if id.ams == 254 => "Ext L".to_string(),
        Some(id) if id.ams >= 255 => "Ext R".to_string(),
        Some(id) => super::colours::tray_text(id),
        None => "–".to_string(),
    }
}

#[derive(Script, ScriptHook, Widget)]
pub struct FabTrayGrid {
    #[uid]
    uid: WidgetUid,
    #[walk]
    walk: Walk,
    #[redraw]
    #[live]
    draw_chip: DrawFabChip,
    #[live]
    draw_name: DrawText,
    #[live]
    draw_small: DrawText,
    /// Each tray as drawn: (label, rect), its accessible items.
    #[rust]
    items: Vec<(String, Rect, Option<fab_printer_link::model::TrayId>)>,
    /// The drawn grid receives presses on virtual spools.
    #[rust] area: Area,
    /// A menu pick applies to the cell that opened it.
    #[rust] editing: Option<(EditOwner, colours::Position)>,
    #[rust] owner: EditOwner,
}

/// The virtual report contains every head and its tray assignments from the link's own profile.
pub(super) fn editable_layout(p: &PrintState) -> Option<colours::Layout> {
    let report = p.shown_printer().filter(|r| r.virtual_printer)?;
    let count = report.state.extruders.len();
    let profile = colours::Offline {
        heads: (0..count).map(|i| fab_plate::machine::nozzle_name(count, i)).collect(),
        printer: report.name.clone(),
        slots: Vec::new(),
    };
    let mut layout = colours::layout(Some((&report.name, &report.state)), None, &profile, &|_| None);
    colours::drop_missing_heads(&mut layout);
    Some(layout)
}

const SPOOL: f64 = 34.0;
const CELL_W: f64 = 64.0;
const CELL_H: f64 = 74.0;

impl Widget for FabTrayGrid {
    fn semantics(&self, _cx: &Cx, node: &mut a11y_tree::Node) {
        node.role = a11y_tree::Role::List;
        makepad_platform::a11y::set_string(&mut node.name, "Printer trays");
        node.set_size = self.items.len() as u32;
    }

    fn semantics_parts(&self, _cx: &Cx, out: &mut Vec<(u64, Option<u64>, a11y_tree::Node)>) {
        for (i, (label, rect, _)) in self.items.iter().enumerate() {
            let mut node = a11y_tree::Node { role: a11y_tree::Role::ListItem, bounds: makepad_platform::a11y::semantic_rect(*rect), pos_in_set: i as u32 + 1, set_size: self.items.len() as u32, ..Default::default() };
            makepad_platform::a11y::set_string(&mut node.name, label);
            out.push((i as u64 + 1, None, node));
        }
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        use crate::ui::popover::{menu_picked, open_menu, MenuPlace};
        let Some(state) = scope.data.get::<AppState>() else { return };
        let p = &state.print;
        let menu = LiveId::from_str(&format!("fab_grid_tray:{:?}", self.uid));
        if let Event::Actions(actions) = event {
            if let Some(edit) = menu_picked(actions, menu).and_then(colours::tray_edit_pick) {
                if let Some((owner, pos)) = self.editing.take() { cx.action(PrintAction::EditTray(owner, pos, edit)); }
            }
        }
        if !self.owner.matches(p) || !p.shown_printer().is_some_and(|s| s.virtual_printer) { return; }
        if let Hit::FingerUp(fe) = event.hits(cx, self.area) {
            if !fe.is_over || fe.cancelled { return; }
            let Some((_, rect, Some(id))) = self.items.iter().find(|(_, r, _)| r.contains(fe.abs)) else { return };
            let Some(layout) = editable_layout(p) else { return };
            if let Some(tray) = layout.trays.iter().find(|t| t.pos.tray == *id) {
                self.editing = Some((self.owner.clone(), tray.pos));
                open_menu(cx, menu, colours::tray_edit_menu(tray, &layout.heads), *rect, MenuPlace::Below);
            }
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        self.items.clear();
        let Some(printer) = scope.data.get::<AppState>().and_then(|s| {
            self.owner.capture(PrinterScope::Shown, &s.print);
            s.print.shown_printer().cloned()
        }) else {
            cx.walk_turtle(Walk { height: Size::Fixed(0.0), ..walk });
            return DrawStep::done();
        };
        let s = &printer.state;
        // Rows: each AMS unit, then the external spool(s).
        let mut rows: Vec<(String, Vec<&fab_printer_link::model::Tray>)> = Vec::new();
        if s.numbered_heads {
            for head in s.heads() {
                let Some(id) = head.id else { continue };
                let Some(name) = s.head_name(id) else { continue };
                let trays = s.tray(fab_printer_link::model::TrayId { ams: id.0, slot: 0 }).into_iter().collect();
                rows.push((name, trays));
            }
        } else {
            for unit in &s.ams {
                let side = unit.extruder.and_then(|id| s.head_name(id)).map(|name| format!(" · {}", name.to_lowercase())).unwrap_or_default();
                let humidity = unit.humidity_percent.map(|h| format!(" · {h:.0}% RH")).unwrap_or_default();
                let name = format!("AMS {}{side}{humidity}", super::colours::unit_label(unit.id.unwrap_or(0)));
                rows.push((name, unit.trays.iter().collect()));
            }
            if !s.external.is_empty() {
                rows.push((if s.external.len() > 1 { "External spools".to_string() } else { "External spool".to_string() }, s.external.iter().collect()));
            }
        }
        // The tray feeding the nozzle that prints stands out.
        let feeding = s.active_extruder.filter(|_| s.busy()).and_then(|id| s.extruders.iter().find(|e| e.id == Some(id))).and_then(|e| e.loaded).and_then(|slot| match slot {
            fab_printer_link::model::LoadedSlot::Tray(id) => Some(id),
            fab_printer_link::model::LoadedSlot::NoneOrExternal => None,
        });
        // One spool per group (each tool of a toolchanger, each head): one grid of them all, each
        // cell named by its group. Otherwise each unit's trays under its name.
        let single = rows.len() > 1 && rows.iter().all(|(_, trays)| trays.len() <= 1);
        let tags: Vec<String> = if single { rows.iter().filter(|(_, t)| !t.is_empty()).map(|(name, _)| name.clone()).collect() } else { Vec::new() };
        if single {
            let all = rows.iter().flat_map(|(_, trays)| trays.iter().copied()).collect();
            rows = vec![(String::new(), all)];
        }
        // a grid: as many trays to a row as the width takes, wrapping onto more rows
        let header_h = if single { 0.0 } else { 18.0 };
        let width = cx.peek_walk_turtle(walk).size.x;
        let columns = ((width + 6.0) / CELL_W).floor().max(1.0) as usize;
        let lines = |n: usize| n.div_ceil(columns).max(1) as f64;
        let h = rows.iter().map(|(_, trays)| header_h + lines(trays.len()) * CELL_H + 6.0).sum::<f64>();
        let rect = cx.walk_turtle(Walk { height: Size::Fixed(h.max(0.0)), ..walk });
        cx.add_aligned_rect_area(&mut self.area, rect);
        let mut y = rect.pos.y;
        for (name, trays) in &rows {
            if !name.is_empty() {
                self.draw_small.draw_abs(cx, dvec2(rect.pos.x, y + 2.0), name);
            }
            y += header_h;
            for (i, tray) in trays.iter().enumerate() {
                let x = rect.pos.x + (i % columns) as f64 * CELL_W;
                let y = y + (i / columns) as f64 * CELL_H;
                // no plate behind a tray; the one feeding the nozzle that prints is ringed
                let printing = feeding.is_some() && tray.id == feeding;
                if printing {
                    self.draw_chip.spool = 0.0;
                    self.draw_chip.fill = vec4(0.0, 0.0, 0.0, 0.0);
                    self.draw_chip.ring = vec4(1.0, 1.0, 1.0, 0.75);
                    self.draw_chip.radius = 3.0;
                    self.draw_chip.draw_abs(cx, Rect { pos: dvec2(x, y), size: dvec2(CELL_W - 6.0, CELL_H - 6.0) });
                }
                let empty = tray.present == Some(false) || (tray.material.is_none() && tray.colour.is_none());
                self.draw_chip.spool = 1.0;
                self.draw_chip.empty = if empty { 1.0 } else { 0.0 };
                self.draw_chip.fill = rgba(tray.colour.map(|c| c.0).unwrap_or([0.4, 0.4, 0.4, 1.0]));
                let cx0 = x + (CELL_W - 6.0 - SPOOL) * 0.5;
                self.draw_chip.draw_abs(cx, Rect { pos: dvec2(cx0, y + 5.0), size: dvec2(SPOOL, SPOOL) });
                self.draw_chip.spool = 0.0;
                self.draw_chip.empty = 0.0;
                let material = if empty { "Empty".to_string() } else { tray.material.clone().unwrap_or_else(|| "?".into()) };
                // in one grid of single spools: the group's name, the material under it
                let tag = tags.get(i).cloned();
                let label = match &tag {
                    Some(tag) => tag.clone(),
                    None if s.numbered_heads => material.clone(),
                    None => format!("{} {}", tray_label(tray.id), material),
                };
                let tw = self.draw_name.prepare_single_line_run(cx, &label).map(|r| r.width_in_lpxs as f64).unwrap_or(40.0);
                self.draw_name.draw_abs(cx, dvec2(x + ((CELL_W - 6.0 - tw) * 0.5).max(2.0), y + SPOOL + 9.0), &label);
                let colour = tray.colour.map(|c| { let [r, g, b, _] = c.0; format!(" colour #{:02x}{:02x}{:02x}", (r * 255.0) as u8, (g * 255.0) as u8, (b * 255.0) as u8) }).unwrap_or_default();
                let left = tray.remain.filter(|r| *r >= 0.0 && !empty && !s.numbered_heads && !tray.id.is_some_and(|i| matches!(i.ams, 254 | 255))).map(|r| format!(", {r:.0}% left")).unwrap_or_default();
                let printing = if printing { " feeding the nozzle that prints" } else { "" };
                // Spoken as drawn, group first: "Tool 1 PLA", or the unit's name then its tray (a single
                // grid has no row name, so its cells carry their group).
                let said = match &tag {
                    Some(tag) => format!("{tag} {material}"),
                    None if name.is_empty() => label.clone(),
                    None => format!("{name} {label}"),
                };
                self.items.push((format!("{said}{colour}{left}{printing}"), Rect { pos: dvec2(x, y), size: dvec2(CELL_W - 6.0, CELL_H) }, tray.id));
                // An external spool has no tag to read what is left from (it reports 0).
                let external = tray.id.is_some_and(|i| matches!(i.ams, 254 | 255));
                if tag.is_some() {
                    let tw = self.draw_small.prepare_single_line_run(cx, &material).map(|r| r.width_in_lpxs as f64).unwrap_or(20.0);
                    self.draw_small.draw_abs(cx, dvec2(x + ((CELL_W - 6.0 - tw) * 0.5).max(2.0), y + SPOOL + 24.0), &material);
                } else if let Some(r) = tray.remain.filter(|r| *r >= 0.0 && !empty && !external && !s.numbered_heads) {
                    let t = format!("{r:.0}%");
                    let tw = self.draw_small.prepare_single_line_run(cx, &t).map(|r| r.width_in_lpxs as f64).unwrap_or(20.0);
                    self.draw_small.draw_abs(cx, dvec2(x + (CELL_W - 6.0 - tw) * 0.5, y + SPOOL + 24.0), &t);
                }
            }
            y += lines(trays.len()) * CELL_H + 6.0;
        }
        DrawStep::done()
    }
}
