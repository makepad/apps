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
    // (the app's own sections, rows and fields: `PropSection` is Fab's, as every editor's)
    use mod.widgets.*

    // A section nested in the Legend: its bar a level in (under the Legend's icon, as the Part
    // panel's parameter groups), its rows where the Legend's other rows are.
    let LegendPart = PropSection{
        // a grip to move it within the Legend (never out of it)
        grip: true
        // the sections' gap above it, none below: every bar stands that gap from the next, the
        // Legend's own next section's too (the Legend's body keeps no room of its own)
        margin: Inset{top: theme.prop_section_gap * 2.0}
        header +: {hdr +: {margin: Inset{left: 10.0} padding: Inset{left: theme.prop_label_gap right: theme.prop_pad}}}
        body: View{
            width: Fill height: Fit flow: Down
            // its rows a level in too: they start where its bar does
            padding: Inset{left: 10.0 top: 4 bottom: 6}
        }
    }

    mod.widgets.FabSlicePanelBase = #(FabSlicePanel::register_widget(vm))
    mod.widgets.FabSlicePanel = set_type_default() do mod.widgets.FabSlicePanelBase{
        width: Fill
        height: Fill
        flow: Down
        show_bg: true
        draw_bg +: { color: fab.color_editor }
        // the toolbar: the area's editor selector (and this editor's tools, when it has any)
        editor_bar := mod.widgets.FabEditorBar{
            // filters the rows below by name (a section's title shows it whole)
            search := mod.widgets.FabSearch{width: Fill{min: 80} row +: {input +: {empty_text: "Filter"}}}
        }
        // the preview's settings as the Part editor's: sections that fold, each a bar with its
        // glyph; a section with nothing to show hides (`draw_walk`)
        body := mod.widgets.FabScroll{
            padding: Inset{left: theme.prop_pad right: theme.prop_pad top: #(crate::ui::widgets::PANEL_TOP)}
            a11y: {role: "group" label: "Print preview legend"}
            // nothing sliced yet: what the slice is waiting for
            none := mod.widgets.FabLabelDim{ width: Fill text: "" margin: Inset{left: 4 top: 4 bottom: 8} }
            // its slot: the box the section moves in when dragged by its grip (`section_reorder`)
            display_slot := View{width: Fill height: Fit
                display := PropSection{
                    header +: {hdr +: {icon +: {visible: true draw_icon +: {svg: crate_resource("self://resources/icons/eye.svg")}} title +: {text: "Display"}}}
                    body +: {
                        shows := mod.widgets.FabFieldRow{
                            a11y: {role: "radio_group" label: "Sliced shows"}
                            name +: {text: "Show"}
                            mod.widgets.FabTip{ text: "The toolpaths, move by move"
                                show_paths := mod.widgets.FabSegmentTab{ text: "Toolpaths" }
                            }
                            mod.widgets.FabTip{ text: "The parts as they print, layer by layer up to the layer slider"
                                show_surface := mod.widgets.FabSegmentTab{ text: "Printed surface" }
                            }
                        }
                        colors := mod.widgets.FabFieldRow{
                            name +: {text: "Colour by"}
                            color_by := mod.widgets.FabDropdownButton{ tag: @fab_slice_color owner: @fab_slice_color label +: { text: "Feature" } }
                        }
                        resin := mod.widgets.FabFieldRow{
                            visible: false
                            name +: {text: "Plate"}
                            mod.widgets.FabTip{ text: "Show the part hanging from the plate, the way it prints; off, upright on it"
                                hang := mod.widgets.FabCheckBox{ text: "On top" a11y: {label: "Plate on top"} }
                            }
                        }
                        checks := mod.widgets.FabFieldRow{
                            name +: {text: "Overlays"}
                            overhangs := mod.widgets.FabCheckBox{ text: "Overhang shading" }
                            markers := mod.widgets.FabCheckBox{ text: "Markers" }
                        }
                    }
                }
            }
            // its slot: the box the section moves in when dragged by its grip (`section_reorder`)
            compare_slot := View{width: Fill height: Fit
                compare := PropSection{
                    visible: false
                    header +: {hdr +: {icon +: {visible: true draw_icon +: {svg: crate_resource("self://resources/icons/section.svg")}} title +: {text: "Compare"}}}
                    body +: {
                        compare_controls := View{
                            visible: false width: Fill height: Fit flow: Right{wrap: true} spacing: 6 align: Align{y: 0.5}
                            compare_box := View{
                                visible: false width: Fill height: Fit flow: Right{wrap: true} spacing: 6 align: Align{y: 0.5}
                                compare_modes := mod.widgets.FabButton{ text: "Compare with modes off" a11y: {label: "Compare with modes off"} }
                                compare_label := mod.widgets.FabLabel{ text: "Modes on" }
                            }
                            compare_planar := mod.widgets.FabCheckBox{ visible: false text: "Planar ghost" a11y: {label: "Planar ghost"} }
                        }
                        mode_stats := mod.widgets.FabLabelDim{ visible: false width: Fill text: "" a11y: {label: "Mode comparison"} }
                    }
                }
            }
            // its slot: the box the section moves in when dragged by its grip (`section_reorder`)
            legend_box_slot := View{width: Fill height: Fit
                legend_box := PropSection{
                    header +: {hdr +: {icon +: {visible: true draw_icon +: {svg: crate_resource("self://resources/icons/palette.svg")}} title +: {text: "Legend"}}}
                    body +: {
                        // (its parts keep the sections' gap between them: no room of its own)
                        padding: Inset{left: theme.prop_pad right: theme.prop_pad top: 0 bottom: 0}
                        // nested: the colours' key (titled as the colour mode) with the print
                        // time, then the materials
                        // (each in its slot: the box it moves in when dragged by its grip)
                        legend_parts := View{
                            width: Fill height: Fit flow: Down
                            key_box_slot := View{width: Fill height: Fit
                                key_box := LegendPart{
                                    header +: {hdr +: {title +: {text: "Feature"}}}
                                    body +: { legend := mod.widgets.FabSliceLegend{part: 1} }
                                }
                            }
                            material_box_slot := View{width: Fill height: Fit
                                material_box := LegendPart{
                                    header +: {hdr +: {title +: {text: "Material"}}}
                                    body +: { materials := mod.widgets.FabSliceLegend{part: 2} }
                                }
                            }
                        }
                    }
                }
            }
            // its slot: the box the section moves in when dragged by its grip (`section_reorder`)
            printability_slot := View{width: Fill height: Fit
                printability := PropSection{
                    header +: {hdr +: {icon +: {visible: true draw_icon +: {svg: crate_resource("self://resources/icons/printer.svg")}} title +: {text: "Printability"}}}
                    body +: {
                        issues := mod.widgets.FabIssueSummary{ headless: true }
                        fix := mod.widgets.FabButton{ visible: false text: "Make imported part printable" }
                    }
                }
            }
            // its slot: the box the section moves in when dragged by its grip (`section_reorder`)
            overhang_list_slot := View{width: Fill height: Fit
                overhang_list := PropSection{
                    header +: {hdr +: {icon +: {visible: true draw_icon +: {svg: crate_resource("self://resources/icons/supports.svg")}} title +: {text: "Overhangs"}}}
                    body +: {
                        supports := mod.widgets.FabSupportList{ headless: true }
                    }
                }
            }
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
        if let Event::Actions(actions) = event {
            if self.view.text_input(cx, ids!(editor_bar.search.row.input)).changed(actions).is_some() {
                self.view.redraw(cx);
            }
        }
        // a section held by its grip moves with the pointer, the others sliding aside
        // (`section_reorder`): first, and with its animation's frames too
        if matches!(event, Event::MouseDown(_) | Event::MouseMove(_) | Event::MouseUp(_) | Event::NextFrame(_)) {
            let order: Vec<(LiveId, WidgetRef, WidgetRef)> = [(live_id!(display_slot), live_id!(display)), (live_id!(compare_slot), live_id!(compare)), (live_id!(legend_box_slot), live_id!(legend_box)), (live_id!(printability_slot), live_id!(printability)), (live_id!(overhang_list_slot), live_id!(overhang_list))]
                .into_iter().map(|(slot, id)| (slot, self.view.widget(cx, &[id]), self.view.widget(cx, &[slot]))).collect();
            crate::ui::widgets::section_reorder(cx, event, live_id!(slice_sections), &order, self.view.area());
            // and the Legend's own, which move within it
            let nested: Vec<(LiveId, WidgetRef, WidgetRef)> = [(live_id!(key_box_slot), live_id!(key_box)), (live_id!(material_box_slot), live_id!(material_box))]
                .into_iter().map(|(slot, id)| (slot, self.view.widget(cx, &[id]), self.view.widget(cx, &[slot]))).collect();
            crate::ui::widgets::section_reorder(cx, event, live_id!(slice_legend_parts), &nested, self.view.area());
        }
        // the sections' headers first (their hover, and a drag across them folding them all)
        if matches!(event, Event::MouseMove(_) | Event::MouseDown(_) | Event::MouseUp(_) | Event::MouseLeave(_) | Event::ClearHover) {
            // (the Legend's nested Feature and Material sections among them)
            let sections = [ids!(display), ids!(compare), ids!(legend_box), ids!(key_box), ids!(material_box), ids!(printability), ids!(overhang_list)].map(|path| self.view.widget(cx, path));
            crate::ui::widgets::section_headers(cx, event, &sections);
        }
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
        if b.button(cx, ids!(compare_controls.compare_box.compare_modes)).clicked(actions) {
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
        // the sections in the order they were dragged into
        let body = self.view.widget(cx, ids!(body));
        crate::ui::widgets::apply_section_order(cx, live_id!(slice_sections), &body);
        let legend_parts = self.view.widget(cx, ids!(legend_parts));
        crate::ui::widgets::apply_section_order(cx, live_id!(slice_legend_parts), &legend_parts);
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
                // the Compare section only while there is something to compare
                b.widget(cx, ids!(compare)).set_visible(cx, p.compare.is_some() || has_planar || !p.mode_stats.is_empty());
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
            let fixable = fixable_object(state).is_some();
            b.button(cx, ids!(fix)).set_visible(cx, fixable);
            // a section with nothing to show hides: the legend and the overhangs come with a
            // slice, Printability with a slice or something found
            b.widget(cx, ids!(legend_box)).set_visible(cx, has);
            // the key's section is titled as the colour mode (the overhang key while it shows)
            let key = if p.overhangs && !p.play.active { "Overhang" } else { p.play.color.label() };
            b.label(cx, ids!(key_box.title)).set_text(cx, key);
            b.widget(cx, ids!(material_box)).set_visible(cx, has && crate::print::timeline::legend_has_materials(state));
            b.widget(cx, ids!(printability)).set_visible(cx, fixable || crate::print::timeline::issues_shown(p));
            b.widget(cx, ids!(overhang_list)).set_visible(cx, crate::print::support_list::has_lines(p));
            let none = b.label(cx, ids!(none));
            none.set_visible(cx, !has);
            if !has {
                none.set_text(cx, &crate::print::panel::slice_status(p).0);
            }
        }
        // the toolbar's search filters the sections' rows (`filter_sections`)
        let query = self.view.text_input(cx, ids!(editor_bar.search.row.input)).text();
        let sections = [ids!(display), ids!(compare), ids!(legend_box), ids!(printability), ids!(overhang_list)].map(|path| self.view.widget(cx, path));
        crate::ui::widgets::filter_sections(cx, live_id!(slice_sections), &query, &sections);
        self.view.draw_walk(cx, scope, walk)
    }
}
