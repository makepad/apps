//! The Slice panel: what the slice shows and what it holds, beside the parts (Design's left
//! column, there while the Slice render mode is on and brought forward with it). The colour
//! mode, the modes-off comparison, and the legend with each feature's time, share and length.
//! The plate view keeps the layer slider and the playback, and its view bar what the view draws
//! (the supports, travel, the parts under the toolpaths).

use crate::api::*;
use crate::print::{ColorBy, PrintAction};
use crate::ui::popover::{dropdown_clicked, menu_picked, open_menu, MenuItem, MenuPlace};
use makepad_widgets_core::*;

const COLOR_MENU: LiveId = live_id!(fab_slice_color);

script_mod! {
    use mod.prelude.widgets_internal.*
    use mod.prelude.fab.*

    mod.widgets.FabSlicePanelBase = #(FabSlicePanel::register_widget(vm))
    mod.widgets.FabSlicePanel = set_type_default() do mod.widgets.FabSlicePanelBase{
        width: Fill
        height: Fill
        flow: Down
        show_bg: true
        draw_bg +: { color: fab.color_editor }
        body := mod.widgets.FabScroll{
            flow: Down
            spacing: 8
            padding: 10
            a11y: {role: "group" label: "Print preview legend"}
            shows := View{
                width: Fill height: Fit flow: Right align: Align{y: 0.5} spacing: 2
                a11y: {role: "radio_group" label: "Sliced shows"}
                mod.widgets.FabTip{ text: "The toolpaths, move by move"
                    show_paths := mod.widgets.FabSegmentTab{ text: "Toolpaths" }
                }
                mod.widgets.FabTip{ text: "The parts as they print, layer by layer up to the layer slider"
                    show_surface := mod.widgets.FabSegmentTab{ text: "Printed surface" }
                }
            }
            colors := View{
                width: Fill height: Fit flow: Right align: Align{y: 0.5} spacing: 6
                mod.widgets.FabLabelDim{ text: "Colour by" }
                color_by := mod.widgets.FabDropdownButton{ tag: @fab_slice_color owner: @fab_slice_color label +: { text: "Feature" } }
            }
            resin := View{
                visible: false width: Fill height: Fit flow: Right align: Align{y: 0.5} spacing: 6
                mod.widgets.FabTip{ text: "Show the part hanging from the plate, the way it prints; off, upright on it"
                    hang := mod.widgets.FabCheckBox{ text: "Plate on top" a11y: {label: "Plate on top"} }
                }
            }
            compare_controls := View{
                visible: false width: Fill height: Fit flow: Right{wrap: true} spacing: 6 align: Align{y: 0.5}
                compare_box := View{
                    visible: false width: Fill height: Fit flow: Right{wrap: true} spacing: 6 align: Align{y: 0.5}
                    compare := mod.widgets.FabButton{ text: "Compare with modes off" a11y: {label: "Compare with modes off"} }
                    compare_label := mod.widgets.FabLabel{ text: "Modes on" }
                }
                compare_planar := mod.widgets.FabCheckBox{ visible: false text: "Planar ghost" a11y: {label: "Planar ghost"} }
            }
            mode_stats := mod.widgets.FabLabelDim{ visible: false width: Fill text: "" a11y: {label: "Mode comparison"} }
            none := mod.widgets.FabLabelDim{ width: Fill text: "" }
            legend := mod.widgets.FabSliceLegend{}
            issues := mod.widgets.FabIssueSummary{}
            supports := mod.widgets.FabSupportList{}
            checks := View{
                width: Fill height: Fit flow: Right{wrap: true} spacing: 12
                overhangs := mod.widgets.FabCheckBox{ text: "Overhang shading" }
                markers := mod.widgets.FabCheckBox{ text: "Markers" }
            }
            fix := mod.widgets.FabButton{ visible: false text: "Make imported part printable" }
        }
    }
}

/// The imported part the report can repair in one click, if any.
fn fixable_object(state: &AppState) -> Option<String> {
    state.print.report().issues.iter().filter(|i| i.fix == Some("make_printable")).find_map(|i| {
        let object = i.object.as_ref()?;
        state.plate.parts.iter().any(|p| p.object == *object && p.imported_mesh).then(|| object.clone())
    })
}

#[derive(Script, ScriptHook, Widget)]
pub struct FabSlicePanel {
    #[deref]
    view: View,
    #[rust]
    synced: Option<(Option<u64>, Option<u64>, bool, bool)>,
}

impl Widget for FabSlicePanel {
    fn semantics(&self, _cx: &Cx, node: &mut a11y_tree::Node) {
        node.role = a11y_tree::Role::Group;
        makepad_platform::a11y::set_string(&mut node.name, "Slice");
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope);
        let Event::Actions(actions) = event else { return };
        let Some(state) = scope.data.get::<AppState>() else { return };
        let b = self.view.view(cx, ids!(body));
        if let Some(anchor) = dropdown_clicked(actions, COLOR_MENU) {
            let items = ColorBy::ALL.iter().map(|c| MenuItem::new(c.id(), c.label()).checked(*c == state.print.play.color)).collect();
            open_menu(cx, COLOR_MENU, items, anchor, MenuPlace::Below);
        }
        if let Some(id) = menu_picked(actions, COLOR_MENU) {
            if let Some(c) = ColorBy::ALL.iter().find(|c| c.id() == id) {
                cx.action(PrintAction::SetColorBy(*c));
            }
        }
        if b.radio_button(cx, ids!(shows.show_paths)).clicked(actions) {
            cx.action(PrintAction::PrintedSurface(false));
        }
        if b.radio_button(cx, ids!(shows.show_surface)).clicked(actions) {
            cx.action(PrintAction::PrintedSurface(true));
        }
        if b.button(cx, ids!(compare_controls.compare_box.compare)).clicked(actions) {
            cx.action(PrintAction::Compare(!state.print.showing_b));
        }
        if let Some(on) = b.check_box(cx, ids!(compare_controls.compare_planar)).changed(actions) {
            cx.action(PrintAction::ComparePlanar(on));
        }
        if let Some(on) = b.check_box(cx, ids!(resin.hang)).changed(actions) {
            cx.action(PrintAction::ResinUpright(!on));
        }
        if let Some(on) = b.check_box(cx, ids!(checks.overhangs)).changed(actions) {
            cx.action(PrintAction::Overhangs(on));
        }
        if let Some(on) = b.check_box(cx, ids!(checks.markers)).changed(actions) {
            cx.action(PrintAction::Markers(on));
        }
        if b.button(cx, ids!(fix)).clicked(actions) {
            if let Some(object) = fixable_object(state) {
                cx.action(crate::api::PartAction::MakePrintable { object });
            }
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        if let Some(state) = scope.data.get::<AppState>() {
            let p = &state.print;
            let b = self.view.view(cx, ids!(body));
            let has = p.showing().is_some();
            let key = (p.preview.as_ref().map(|p| p.id), p.compare.as_ref().map(|p| p.id), p.showing_b, p.play.compare_planar);
            if self.synced != Some(key) {
                self.synced = Some(key);
                let has_planar = p.preview.as_ref().is_some_and(|pv| pv.modes.iter().any(|m| m.mode == 8));
                b.view(cx, ids!(compare_controls)).set_visible(cx, p.compare.is_some() || has_planar);
                b.view(cx, ids!(compare_controls.compare_box)).set_visible(cx, p.compare.is_some());
                b.label(cx, ids!(compare_controls.compare_box.compare_label)).set_text(cx, if p.showing_b { "Modes off" } else { "Modes on" });
                let planar = b.check_box(cx, ids!(compare_controls.compare_planar));
                planar.set_visible(cx, has_planar);
                planar.set_active(cx, p.play.compare_planar, Animate::No);
                let stats = b.label(cx, ids!(mode_stats));
                stats.set_visible(cx, !p.mode_stats.is_empty());
                stats.set_text(cx, &p.mode_stats);
            }
            b.view(cx, ids!(colors.color_by)).label(cx, ids!(label)).set_text(cx, p.play.color.label());
            b.radio_button(cx, ids!(shows.show_paths)).set_active(cx, !p.finish, Animate::No);
            b.radio_button(cx, ids!(shows.show_surface)).set_active(cx, p.finish, Animate::No);
            // a resin printer's slice is its layers: no toolpaths to colour, the stack hangs or stands
            let resin = p.resin_shown().is_some();
            b.view(cx, ids!(shows)).set_visible(cx, !resin);
            b.view(cx, ids!(colors)).set_visible(cx, !p.finish && !resin);
            b.view(cx, ids!(resin)).set_visible(cx, resin);
            b.check_box(cx, ids!(resin.hang)).set_active(cx, !p.upright, Animate::No);
            b.check_box(cx, ids!(checks.overhangs)).set_active(cx, p.overhangs, Animate::No);
            b.check_box(cx, ids!(checks.markers)).set_active(cx, !p.hide_markers, Animate::No);
            b.button(cx, ids!(fix)).set_visible(cx, fixable_object(state).is_some());
            let none = b.label(cx, ids!(none));
            none.set_visible(cx, !has);
            if !has {
                none.set_text(cx, &crate::print::panel::slice_status(p).0);
            }
        }
        self.view.draw_walk(cx, scope, walk)
    }
}
