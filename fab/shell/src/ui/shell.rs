//! The application shell: the header, the Dock with the one 3D view and the
//! panels around it, and the overlay layers (menus,
//! tooltips, modals, perf graph).
//!
//! A **workspace** is a tree of areas (design FABWS), each a stack of editors
//! like a Dock tab set: one editor shows no tab bar, only its area's nub; two or
//! more show the Dock's tab bar with the nub at its left end. The Dock's map is
//! derived from the tree ([`present`]) and every change to the tree is a
//! [`WorkspaceOp`] through the state: a tab or a nub dragged (the layout
//! reflows as if dropped, a ghost in the space, until the drop applies one
//! `Move`; leaving the Dock or Escape puts it back), the nub's menu, a sash, a
//! tab pressed. Switching swaps the Dock's node map (the bodies stay resident,
//! so the viewport's GPU caches survive) and never touches the document.

use crate::api::*;
use crate::print::trays::DrawFabChip;
use crate::ui::review_strip::ReviewStrip;
use fab_elec_view::snapshot::{ElecLive, ProposalId, ProposalView};
use std::sync::{Arc, Weak};
use crate::ui::device3d::FabDevice3d;
use crate::ui::keymap::{self, ShortcutId as Key};
use crate::ui::popover::{menu_picked, open_menu, MenuItem, MenuPlace};
use fab_elec_view::board::tools::{BoardStripAction, ARRANGEMENTS};
use fab_elec_view::explorer::tools::StripAction;
use fab_elec_view::board::BoardPane;
use fab_elec_view::explorer::view::ExplorerView;
use fab_elec_view::instruments::logic_view::LogicScope;
use fab_elec_view::live::controls_strip::{ControlsStripAction, ElecControlsStrip};
use fab_elec_view::live::views::{EditOrigin, ElecEditor, ElecOutsideEdit};
use crate::workspaces::{ListOp, ShownBy, WorkspaceDef, WorkspaceId, Workspaces};
#[cfg(test)]
use crate::ui::areas::present;
use crate::ui::areas::{ AreaId, AreaTree, DockMap, DropAt, Instance, Moving, Present, Side, WorkspaceOp, GHOST_TAB};
use crate::ui::editors::{Editor, MENU};
use crate::ui::stage::{StageAction, PART_SIDE_HYST, PART_SIDE_MIN_VIEW};
use crate::ui::elec_layouts as layouts;
use crate::ui::area_notch::{Corner, CornerPhase, FabAreaNotches, NotchedArea, NubAction, DRAG_DISTANCE};
use makepad_widgets_core::*;
use makepad_widgets_data::dock::*;

script_mod! {
    use mod.prelude.fab.*
    use mod.widgets.*

    // An action on a canvas's tool strip: a momentary button the size of the strip's tools.
    let StripAct = mod.widgets.FabIconButton{
        width: 32
        height: 32
        padding: Inset{left: 8 right: 8 top: 8 bottom: 8}
    }
    let BoardSettingRow = View{
        width: Fill height: Fit flow: Down spacing: 2
        line := View{
            width: Fill height: 25 flow: Right spacing: 6 align: Align{y: 0.5}
            caption := mod.widgets.FabLabelDim{width: 87}
            value := mod.widgets.FabInput{width: Fill height: 24}
            swatch := mod.widgets.ColorSwatch{visible: false width: 20 height: 20}
            limit := mod.widgets.FabLabelSmall{visible: false}
            raised := mod.widgets.FabLabelSmall{visible: false draw_text +: {color: fab.color_warning}}
        }
        refusal := RoundedView{
            visible: false width: Fill height: Fit padding: Inset{left: 5 right: 5 top: 2 bottom: 2}
            draw_bg +: {color: fab.color_panel border_color: fab.color_error border_size: 1.0 border_radius: fab.radius}
            reason := mod.widgets.FabLabelSmall{width: Fill draw_text +: {color: fab.color_error}}
        }
    }
    // A button on a canvas's status bar (zoom).
    let BarAct = mod.widgets.FabIconButton{ width: 24 height: 22 padding: Inset{left: 6 right: 6 top: 5 bottom: 5} }
    // The Board's status fields (V10): borderless wells, the value in the code face.
    let BoardBarField = mod.widgets.ComboBox{
        width: 72
        height: 18
        margin: 0
        padding: Inset{left: 6 right: 16 top: 0 bottom: 0}
        draw_bg +: {
            border_size: 0.0
            border_radius: fab.radius
            color: theme.prop_field
            color_hover: theme.prop_field_hover
            color_focus: theme.prop_field_hover
            color_down: theme.prop_field_active
            arrow_color: fab.color_text_dim
            arrow_color_hover: fab.color_text_dim
        }
        input +: {
            draw_text +: {
                color: fab.color_text
                color_hover: fab.color_text
                color_focus: fab.color_text
                color_down: fab.color_text
                text_style: theme.font_code{font_size: fab.font_size_ui}
            }
        }
    }

    // A dock tab: the panel's name, half-way between the dim and the body ink at rest (a name to
    // read, not a caption), bright with an accent underline when it is the shown one.
    mod.widgets.FabPanelTab = mod.widgets.TabFlat{
        closeable: false
        height: 28
        padding: Inset{left: 10 right: 10 top: 0 bottom: 0}
        margin: 0.
        draw_text +: {
            text_style: theme.font_regular{ font_size: fab.font_size_ui }
            color: mix(fab.color_text_dim, fab.color_text, 0.5)
            color_hover: fab.color_text
            color_active: fab.color_text_active
        }
        draw_bg +: {
            pixel: fn() {
                let sdf = Sdf2d.viewport(self.pos * self.rect_size)
                sdf.rect(0.0, 0.0, self.rect_size.x, self.rect_size.y)
                sdf.fill(fab.color_header.mix(fab.color_toolbar, self.active))
                let w = self.rect_size.x - 12.0
                sdf.box(6.0, self.rect_size.y - 2.0, w, 2.0, 0.5)
                sdf.fill(vec4(fab.color_accent.xyz, fab.color_accent.w * self.active))
                return sdf.result
            }
        }
    }

    // The dock's tab bar: its ground, and a dark line along its foot, a whole point on device
    // pixels (the shown tab, drawn over it, stays open to its panel).
    mod.widgets.FabPanelTabBar = mod.widgets.TabBarFlat{
        height: 28
        CloseableTab := mod.widgets.FabPanelTab{closeable: true}
        PermanentTab := mod.widgets.FabPanelTab{closeable: false}
        draw_bg +: {
            pixel: fn() {
                let y = self.pos.y * self.rect_size.y
                let line = step(self.rect_size.y - 1.0, y)
                return vec4(mix(fab.color_header.xyz, fab.color_header.xyz * 0.6, line), 1.0)
            }
        }
    }

    mod.widgets.FabShellBase = #(FabShell::register_widget(vm))
    mod.widgets.FabShell = set_type_default() do mod.widgets.FabShellBase{
        width: Fill
        height: Fill
        flow: Overlay
        show_bg: true
        draw_bg +: {
            color: fab.color_area
        }
        // a drag's chip looks like the app's tooltips
        draw_chip: mod.draw.FabTipChip{}
        draw_chip_text: mod.draw.FabTipText{}
        color_chip_text: #xdfe6ec
        color_chip_warn: fab.color_warning
        main := View{
            a11y: {role: "group" label: "Workspace"}
            width: Fill
            height: Fill
            flow: Down
            // The temporary layout's strip (FABWS 4b): over its areas while it shows, what it
            // shows, then Keep and Back. Part of its frame: the person's workspaces never show it.
            temp_strip := View{
                visible: false
                width: Fill height: 28
                flow: Right align: Align{y: 0.5} spacing: 8
                padding: Inset{left: 12 right: 6}
                show_bg: true
                draw_bg +: {
                    pixel: fn() {
                        let sdf = Sdf2d.viewport(self.pos * self.rect_size)
                        sdf.rect(0.0, 0.0, self.rect_size.x, self.rect_size.y)
                        sdf.fill(fab.color_header)
                        sdf.rect(0.0, self.rect_size.y - 1.0, self.rect_size.x, 1.0)
                        sdf.fill(fab.color_border)
                        return sdf.result
                    }
                }
                spark := FabIcon{
                    icon_walk: Walk{width: 14 height: 14}
                    draw_icon +: {svg: crate_resource("self://resources/icons/spark.svg") color: fab.color_text_dim}
                }
                note := mod.widgets.FabLabel{text: ""}
                View{width: Fill height: 1}
                keep := mod.widgets.FabButton{text: "Keep" height: 22 a11y: {label: "Keep this layout"}}
                back := mod.widgets.FabButton{text: "Back" height: 22 a11y: {label: "Back to your workspace"}}
            }
            review_area := ScrollYView{
                visible: false width: Fill height: Fit{max: FitBound.Rel{base: Base.Full factor: 0.5}}
                flow: Down
                review := mod.widgets.ReviewStrip{}
            }
            dock := Dock{
                width: Fill
                height: Fill
                padding: Inset{left: 0 top: 0 right: 0 bottom: 0}
                // the bar holds only the area's tabs: the area's nub is in its editor's toolbar
                // (`FabEditorBar`) or corner (`FabAreaNotches`)
                tab_bar: mod.widgets.FabPanelTabBar{}
                round_corner +: { border_radius: 0. }
                // one-editor areas show no tab bar and still take a drag's drop
                drop_on_hidden_tab_bars: true
                // Every pane edge is a sash: a hairline, lit after a short rest of the pointer.
                splitter: mod.widgets.Sash{
                    size: fab.splitter_size
                    // The left column is never narrower than its tabs (Plates, Parts and
                    // the Slice view's): the floor of a side-by-side split's first pane.
                    min_vertical: #(FIRST_PANE_WIDTH)
                    max_vertical: #(SECOND_PANE_FLOOR)
                    // A pane above or below a split is never shorter than one row (the recorder's
                    // strip folds to one).
                    min_horizontal: #(FIRST_PANE_HEIGHT)
                    max_horizontal: #(SECOND_PANE_FLOOR)
                    draw_bg +: {
                        color: fab.color_border
                        color_hover: fab.color_accent
                        color_drag: fab.color_accent_hover
                    }
                }

                // Before the first draw only: the shown workspace's areas replace it (`present`).
                root := DockSplitter{
                    axis: SplitterAxis.Horizontal
                    align: SplitterAlign.FromA(280.0)
                    a: @left_split
                    b: @tabs_view
                }
                left_split := DockSplitter{
                    axis: SplitterAxis.Vertical
                    align: SplitterAlign.FromA(260.0)
                    a: @tabs_parts
                    b: @tabs_part
                }
                tabs_view := DockTabs{ tabs: [@tab_viewport] selected: 0 closable: false hide_tab_bar: true }
                tabs_parts := DockTabs{ tabs: [@tab_parts] selected: 0 closable: false }
                tabs_part := DockTabs{ tabs: [@tab_part] selected: 0 closable: false }

                tab_viewport := DockTab{ name: "Plate" template: @PermanentTab kind: @Viewport }
                tab_parts := DockTab{ name: "Parts" template: @PermanentTab kind: @Parts }
                tab_part := DockTab{ name: "Part" template: @PermanentTab kind: @Part }

                Viewport := FabStage{}
                Device := FabDevice3d{}
                Plates := FabPlatesPanel{}
                Parts := FabPartsPanel{}
                Part := FabPartPanel{}
                Slicer := FabSlicerPanel{}
                Overrides := FabOverridesPanel{}
                Strength := FabStrengthPanel{}
                Library := FabLibraryPanel{}
                Checks := FabChecksPanel{}
                Requirements := FabRequirementsPanel{}
                Bom := FabBomPanel{}
                BoardControls := FabBoardControlsPanel{}
                ModelSources := FabModelSources{}
                Printer := FabPrinterPanel{}
                Mechanics := FabMechanicsPanel{}
                Features := FabFeaturesPanel{}
                Steps := FabStepsPanel{}
                Code := FabCodePanel{}
                Slice := FabSlicePanel{}
                Arm := FabArmPanel{}
                ArmCell := FabArmCell{}
                ArmCompare := FabArmCompare{}
                Scan := FabScanPanel{}
                ScanView := FabScanView{}
                Force := FabForcePanel{}
                Board := BoardPane{
                    flags +: {radius: fab.radius radius_lg: fab.radius_lg}
                    // The Board canvas's one tool strip, down its left edge
                    // (`fab_elec_view::board::tools` names its parts): the modes, then routing
                    // (Route all, Route selected, Clear routes, the method's flyout, the Shove
                    // switch), then the board's own actions. Groups part at a hairline.
                    // A pane too short for one column stands the board's actions beside the rest.
                    tools: View{
                        width: Fit
                        height: Fit
                        strip := mod.widgets.FabFloat{
                            new_batch: true
                            flow: Down
                            padding: 4
                            spacing: 1
                            a11y: {role: "toolbar" label: "Board tools"}
                            col_a := View{
                            width: Fit
                            height: Fit
                            flow: Down
                            spacing: 1
                            View{
                                width: Fit
                                height: Fit
                                flow: Down
                                spacing: 1
                                a11y: {role: "radio_group" label: "Board modes"}
                                FabTip{ place: TipPlace.Right text: "Select (V, Esc): click a part, Shift-click to add or remove a part, or drag to move"
                                    t_select := mod.widgets.FabTool{ a11y: {label: "Select"} draw_icon +: { svg: crate_resource("self://resources/icons/select.svg") } }
                                }
                                FabTip{ place: TipPlace.Right text: "Route a track (X): drag copper from a pad or a track's end, once. Double-click (or X twice) keeps it"
                                    t_route := mod.widgets.FabTool{ a11y: {label: "Route a track"} draw_icon +: { svg: crate_resource("self://resources/icons/route.svg") } }
                                }
                                FabTip{ place: TipPlace.Right text: "Route pair (Shift+X). Drag from either pad of a differential pair to draw both tracks"
                                    t_pair := mod.widgets.FabTool{ a11y: {label: "Route pair"} draw_icon +: { svg: crate_resource("self://resources/icons/route_pair.svg") } }
                                }
                                FabTip{ place: TipPlace.Right text: "Tune length (T). Drag along a track to select its span and sideways to set the fold height"
                                    t_tune := mod.widgets.FabTool{ a11y: {label: "Tune net length"} draw_icon +: { svg: crate_resource("self://resources/icons/tune.svg") } }
                                }
                                FabTip{ place: TipPlace.Right text: "Place a via: click where it goes; it takes the net of the copper there"
                                    t_via := mod.widgets.FabTool{ a11y: {label: "Place a via"} draw_icon +: { svg: crate_resource("self://resources/icons/via.svg") } }
                                }
                                FabTip{ place: TipPlace.Right text: "Draw a copper zone: drag its rectangle on the active copper layer; it fills with the net of the copper where the drag starts, else ground"
                                    t_zone := mod.widgets.FabTool{ a11y: {label: "Draw a copper zone"} draw_icon +: { svg: crate_resource("self://resources/icons/zone.svg") } }
                                }
                                FabTip{ place: TipPlace.Right text: "Drag a rectangle to replace the outer board outline"
                                    t_outline := mod.widgets.FabTool{ a11y: {label: "Outline"} draw_icon +: { svg: crate_resource("self://resources/icons/shape.svg") } }
                                }
                                FabTip{ place: TipPlace.Right text: "Drag a keepout for copper and parts on the active layer"
                                    t_keepout := mod.widgets.FabTool{ a11y: {label: "Keepout"} draw_icon +: { svg: crate_resource("self://resources/icons/no_connect.svg") } }
                                }
                                FabTip{ place: TipPlace.Right text: "Drag a rectangular cutout through the board"
                                    t_cutout := mod.widgets.FabTool{ a11y: {label: "Cutout"} draw_icon +: { svg: crate_resource("self://resources/icons/cut_box.svg") } }
                                }
                                FabTip{ place: TipPlace.Right text: "Measure a distance: drag from one point to another"
                                    t_measure := mod.widgets.FabTool{ a11y: {label: "Measure a distance"} draw_icon +: { svg: crate_resource("self://resources/icons/measure.svg") } }
                                }
                            }
                            mod.widgets.FabToolSep{ width: Fill margin: Inset{top: 3 bottom: 3} }
                            FabTip{ place: TipPlace.Right text: "Route all: route every unrouted connection"
                                a_route_all := StripAct{ a11y: {label: "Route all"} draw_icon +: { svg: crate_resource("self://resources/icons/route_all.svg") } }
                            }
                            FabTip{ place: TipPlace.Right text: "Route selected: route the connections of the selected parts"
                                a_route_selected := StripAct{ a11y: {label: "Route selected"} draw_icon +: { svg: crate_resource("self://resources/icons/route_selected.svg") } }
                            }
                            FabTip{ place: TipPlace.Right text: "Clear routes: remove every routed track"
                                a_clear_routes := StripAct{ a11y: {label: "Clear routes"} draw_icon +: { svg: crate_resource("self://resources/icons/trash.svg") } }
                            }
                            FabTip{ place: TipPlace.Right text: "Route method: Straight or Organic"
                                a_method := StripAct{ a11y: {label: "Route method"} draw_icon +: { svg: crate_resource("self://resources/icons/route_method.svg") } }
                            }
                            FabTip{ place: TipPlace.Right text: "Shove (S): copper you draw pushes the tracks in its way. Off, it walks round them"
                                a_shove := mod.widgets.FabIconCheck{ a11y: {label: "Shove"} width: 32 height: 32 padding: Inset{left: 8 right: 8 top: 8 bottom: 8} draw_icon +: { svg: crate_resource("self://resources/icons/shove.svg") } }
                            }
                            }
                            sep_b := mod.widgets.FabToolSep{ width: Fill margin: Inset{top: 3 bottom: 3} }
                            col_b := View{
                            width: Fit
                            height: Fit
                            flow: Down
                            spacing: 1
                            selection := View{
                                width: Fit
                                height: Fit
                                flow: Down
                                spacing: 1
                                a11y: {role: "group" label: "Selection"}
                                FabTip{ place: TipPlace.Right text: "Rotate 90° counter-clockwise (R). Shift+R turns clockwise."
                                    a_rotate := StripAct{ a11y: {label: "Rotate parts"} draw_icon +: { svg: crate_resource("self://resources/icons/rotate.svg") } }
                                }
                                FabTip{ place: TipPlace.Right text: "Flip to the other side of the board (B)"
                                    a_flip := StripAct{ a11y: {label: "Flip parts"} draw_icon +: { svg: crate_resource("self://resources/icons/drag_x.svg") } }
                                }
                                FabTip{ place: TipPlace.Right text: "Align or distribute selected parts by their courtyards"
                                    a_arrange := StripAct{ a11y: {label: "Arrange parts"} draw_icon +: { svg: crate_resource("self://resources/icons/arrange.svg") } }
                                }
                            }
                            mod.widgets.FabToolSep{ width: Fill margin: Inset{top: 3 bottom: 3} }
                            FabTip{ place: TipPlace.Right text: "Place: the placer puts the selected parts on again where their nets pull them; with none selected, the parts not on the board yet. Stop or Discard on the route line."
                                a_place := StripAct{ a11y: {label: "Place parts"} draw_icon +: { svg: crate_resource("self://resources/icons/place_parts.svg") } }
                            }
                            FabTip{ place: TipPlace.Right text: "Move selected parts to legal spots. With no parts selected, tidy the whole board."
                                a_tidy := StripAct{ a11y: {label: "Tidy: move parts to legal spots"} draw_icon +: { svg: crate_resource("self://resources/icons/place_plate.svg") } }
                            }
                            FabTip{ place: TipPlace.Right text: "Fill: pour the ground net over the board on the active copper layer (F pours the net under the pointer)"
                                a_fill := StripAct{ a11y: {label: "Fill"} draw_icon +: { svg: crate_resource("self://resources/icons/pour.svg") } }
                            }
                            stitch_options := View{
                                visible: false width: Fit height: Fit flow: Down spacing: 1
                                FabTip{ place: TipPlace.Right text: "Stitch the selected pour or the net under the pointer with through vias"
                                    a_stitch := mod.widgets.FabFlatButton{width: 42 height: 25 padding: 0 text: "Stitch" a11y: {label: "Stitch pours"}}
                                }
                                FabTip{ place: TipPlace.Right text: "Stitch grid pitch in millimetres"
                                    stitch_pitch := mod.widgets.FabInput{width: 42 height: 22 text: "5 mm" a11y: {label: "Stitch pitch in millimetres"}}
                                }
                            }
                            FabTip{ place: TipPlace.Right text: "Board stackup and base rules"
                                a_board := mod.widgets.FabFlatButton{width: 42 height: 25 padding: 0 text: "Board" a11y: {label: "Board settings"}}
                            }
                            FabTip{ place: TipPlace.Right text: "Check rules: the board's findings against its design rules"
                                a_check := StripAct{ a11y: {label: "Check rules"} draw_icon +: { svg: crate_resource("self://resources/icons/check.svg") } }
                            }
                            FabTip{ place: TipPlace.Right text: "Update from the circuit: place the circuit's parts the board does not have yet"
                                a_update := StripAct{ a11y: {label: "Update from the circuit"} draw_icon +: { svg: crate_resource("self://resources/icons/sync.svg") } }
                            }
                            FabTip{ place: TipPlace.Right text: "Export: write the board's fabrication files (Gerber, drill, BOM, placement) and check them read back"
                                a_export := StripAct{ a11y: {label: "Export board files"} draw_icon +: { svg: crate_resource("self://resources/icons/export.svg") } }
                            }
                            }
                        }
                    }
                    settings: mod.widgets.FabFloat{
                        visible: false width: 320 height: Fit flow: Down padding: 11 spacing: 4
                        a11y: {role: "group" label: "Board settings"}
                        factory := BoardSettingRow{line +: {
                            caption +: {text: "Factory"}
                            value +: {visible: false}
                            picker := PropDropDown{width: Fill height: 24 a11y: {label: "Board factory"}}
                        }}
                        mod.widgets.FabHeaderLabel{text: "Stackup"}
                        thickness := BoardSettingRow{line +: {
                            caption +: {text: "Thickness (mm)"}
                            value +: {a11y: {label: "Board thickness in millimetres"}}
                        }}
                        finish := BoardSettingRow{line +: {
                            caption +: {text: "Finish"}
                            value +: {visible: false}
                            picker := PropDropDown{width: Fill height: 24 a11y: {label: "Surface finish"}}
                        }}
                        mask := BoardSettingRow{line +: {
                            caption +: {text: "Mask colour"}
                            value +: {a11y: {label: "Solder mask colour"}}
                            swatch +: {visible: true}
                        }}
                        silk := BoardSettingRow{line +: {
                            caption +: {text: "Silk colour"}
                            value +: {a11y: {label: "Silkscreen colour"}}
                            swatch +: {visible: true}
                        }}
                        mod.widgets.FabToolSep{width: Fill}
                        mod.widgets.FabHeaderLabel{text: "Base rules (mm)"}
                        track := BoardSettingRow{line +: {
                            caption +: {text: "Track"}
                            value +: {empty_text: "Inherited" a11y: {label: "Minimum track width in millimetres"}}
                        }}
                        clearance := BoardSettingRow{line +: {
                            caption +: {text: "Clearance"}
                            value +: {empty_text: "Inherited" a11y: {label: "Minimum clearance in millimetres"}}
                        }}
                        drill := BoardSettingRow{line +: {
                            caption +: {text: "Drill"}
                            value +: {empty_text: "Inherited" a11y: {label: "Minimum drill in millimetres"}}
                        }}
                        annular := BoardSettingRow{line +: {
                            caption +: {text: "Annular ring"}
                            value +: {empty_text: "Inherited" a11y: {label: "Minimum annular ring in millimetres"}}
                        }}
                    }
                    status: View{
                        visible: true
                        width: Fill
                        height: 28
                        bar := View{
                            width: Fill
                            height: Fill
                            flow: Right
                            align: Align{y: 0.5}
                            padding: Inset{left: 10 right: 4}
                            spacing: 3
                            show_bg: true
                            a11y: {role: "status" label: "Board status"}
                            draw_bg +: {
                                pixel: fn() {
                                    let sdf = Sdf2d.viewport(self.pos * self.rect_size)
                                    sdf.rect(0.0, 0.0, self.rect_size.x, self.rect_size.y)
                                    sdf.fill(fab.color_statusbar)
                                    sdf.rect(0.0, 0.0, self.rect_size.x, 1.0)
                                    sdf.fill(fab.color_border)
                                    return sdf.result
                                }
                            }
                            View{
                                width: Fill
                                height: Fit
                                clip_x: true
                                s_counts := mod.widgets.FabLabelSmall{ text: "" max_lines: 1 draw_text +: {color: fab.color_text} }
                            }
                            mod.widgets.FabLabelSmall{ text: "Layer" }
                            s_swatch := RoundedView{
                                width: 10 height: 10
                                draw_bg +: {color: #0000 border_radius: fab.radius material: 0.0}
                            }
                            s_layer := BoardBarField{ width: 104 accepts_typed: false a11y: {label: "Active layer"} }
                            mod.widgets.FabLabelSmall{ text: "Track" margin: Inset{left: 4} }
                            s_track := BoardBarField{ accepts_typed: true a11y: {label: "Track width"} }
                            mod.widgets.FabLabelSmall{ text: "Via" margin: Inset{left: 4} }
                            s_via := BoardBarField{ accepts_typed: true a11y: {label: "Via diameter"} }
                            mod.widgets.FabLabelSmall{ text: "Grid" margin: Inset{left: 4} }
                            s_grid := BoardBarField{ accepts_typed: true a11y: {label: "Grid spacing"} }
                        }
                    }
                }
                Circuit := ExplorerView{
                    flags +: {radius: fab.radius radius_lg: fab.radius_lg}
                    // The Circuit canvas's one tool strip, down its left edge
                    // (`fab_elec_view::explorer::tools` names its parts): the modes, then the
                    // place actions, and undo and redo at its foot. Groups part at a hairline.
                    tools: View{
                        width: Fit
                        height: Fill
                        strip := mod.widgets.FabFloat{
                            new_batch: true
                            flow: Down
                            height: Fill
                            padding: 3
                            spacing: 1
                            a11y: {role: "toolbar" label: "Circuit tools"}
                            View{
                                width: Fit
                                height: Fit
                                flow: Down
                                spacing: 1
                                a11y: {role: "radio_group" label: "Circuit modes"}
                                FabTip{ place: TipPlace.Right text: "Select (V, Esc): pick, and drag to move. A drag from a pin wires too"
                                    t_select := mod.widgets.FabTool{ a11y: {label: "Select"} draw_icon +: { svg: crate_resource("self://resources/icons/select.svg") } }
                                }
                                FabTip{ place: TipPlace.Right text: "Wire (W): drag a wire from a pin, once. Double-click (or W twice) keeps it"
                                    t_wire := mod.widgets.FabTool{ a11y: {label: "Wire"} draw_icon +: { svg: crate_resource("self://resources/icons/wire.svg") } }
                                }
                                FabTip{ place: TipPlace.Right text: "Net label (L): click a wire or a connected pin to name its net"
                                    t_label := mod.widgets.FabTool{ a11y: {label: "Net label"} draw_icon +: { svg: crate_resource("self://resources/icons/label.svg") } }
                                }
                                FabTip{ place: TipPlace.Right text: "Global label (Shift-L): click a pin to tie it to a net by name"
                                    t_glabel := mod.widgets.FabTool{ a11y: {label: "Global label"} draw_icon +: { svg: crate_resource("self://resources/icons/glabel.svg") } }
                                }
                                FabTip{ place: TipPlace.Right text: "No-connect (Q): click a pin to mark it unconnected. X marks the pin under the pointer at once"
                                    t_no_connect := mod.widgets.FabTool{ a11y: {label: "No-connect"} draw_icon +: { svg: crate_resource("self://resources/icons/no_connect.svg") } }
                                }
                                FabTip{ place: TipPlace.Right text: "Probe (B): click a pin or a wire to probe its net"
                                    t_probe := mod.widgets.FabTool{ a11y: {label: "Probe"} draw_icon +: { svg: crate_resource("self://resources/icons/probe.svg") } }
                                }
                            }
                            mod.widgets.FabToolSep{ width: Fill margin: Inset{top: 3 bottom: 3} }
                            a_blocks_tip := FabTip{ place: TipPlace.Right text: "Blocks mode"
                                a_blocks := mod.widgets.FabIconCheck{ a11y: {label: "Blocks mode"} width: 32 height: 32 padding: Inset{left: 8 right: 8 top: 8 bottom: 8} draw_icon +: {svg: crate_resource("self://resources/icons/area_schematic.svg")} }
                            }
                            a_arch_tip := FabTip{ place: TipPlace.Right text: "Draft architecture"
                                a_arch := StripAct{ a11y: {label: "Draft architecture"} draw_icon +: {svg: crate_resource("self://resources/icons/spark.svg")} }
                            }
                            FabTip{ place: TipPlace.Right text: "Place a part (A): opens the Library with the cursor in its search"
                                a_place := StripAct{ a11y: {label: "Place part"} draw_icon +: { svg: crate_resource("self://resources/icons/add.svg") } }
                            }
                            FabTip{ place: TipPlace.Right text: "Power (P): pick a rail, then click pins to tie them to it"
                                a_power := StripAct{ a11y: {label: "Power"} draw_icon +: { svg: crate_resource("self://resources/icons/power.svg") } }
                            }
                            mod.widgets.FabToolSep{ width: Fill margin: Inset{top: 3 bottom: 3} }
                            FabTip{ place: TipPlace.Right text: "Annotate: number the parts R1, R2, C1 ... by their place on the sheet"
                                a_annotate := StripAct{ a11y: {label: "Annotate"} draw_icon +: { svg: crate_resource("self://resources/icons/annotate.svg") } }
                            }
                            FabTip{ place: TipPlace.Right text: "Check: the pins left unconnected and the rails with no supply, listed in the Checks editor"
                                a_check := StripAct{ a11y: {label: "Check"} draw_icon +: { svg: crate_resource("self://resources/icons/check.svg") } }
                            }
                            View{ width: Fit height: Fill }
                            FabTip{ place: TipPlace.Right text: #(keymap::tooltip(Key::Undo))
                                a_undo := StripAct{ a11y: {label: "Undo"} draw_icon +: { svg: crate_resource("self://resources/icons/undo.svg") } }
                            }
                            FabTip{ place: TipPlace.Right text: #(keymap::tooltip(Key::Redo))
                                a_redo := StripAct{ a11y: {label: "Redo"} draw_icon +: { svg: crate_resource("self://resources/icons/redo.svg") } }
                            }
                        }
                    }
                    // Its status bar along the bottom (V7): the pointer in mm, the grid, what the
                    // tool does, the counts, and zoom.
                    status: View{
                        width: Fill
                        height: 28
                        bar := View{
                            width: Fill
                            height: Fill
                            flow: Right
                            align: Align{y: 0.5}
                            padding: Inset{left: 10 right: 4}
                            spacing: 14
                            show_bg: true
                            a11y: {role: "status" label: "Circuit status"}
                            draw_bg +: {
                                pixel: fn() {
                                    let sdf = Sdf2d.viewport(self.pos * self.rect_size)
                                    sdf.rect(0.0, 0.0, self.rect_size.x, self.rect_size.y)
                                    sdf.fill(fab.color_statusbar)
                                    sdf.rect(0.0, 0.0, self.rect_size.x, 1.0)
                                    sdf.fill(fab.color_border)
                                    return sdf.result
                                }
                            }
                            s_pos := mod.widgets.FabLabelSmall{ width: 150 text: "" }
                            mod.widgets.FabLabelSmall{ text: "grid 1.27 mm" }
                            View{
                                width: Fill
                                height: Fit
                                clip_x: true
                                s_hint := mod.widgets.FabLabelSmall{ text: "" }
                            }
                            s_counts := mod.widgets.FabLabelSmall{ text: "" draw_text +: { color: fab.color_text } }
                            View{
                                width: Fit
                                height: Fit
                                flow: Right
                                spacing: 0
                                z_out := BarAct{ a11y: {label: "Zoom out"} draw_icon +: { svg: crate_resource("self://resources/icons/minus.svg") } }
                                z_in := BarAct{ a11y: {label: "Zoom in"} draw_icon +: { svg: crate_resource("self://resources/icons/plus.svg") } }
                                FabTip{ place: TipPlace.Top text: "Fit the sheet (F)"
                                    z_fit := BarAct{ a11y: {label: "Fit"} draw_icon +: { svg: crate_resource("self://resources/icons/fit.svg") } }
                                }
                            }
                        }
                    }
                    // Its Library (v10), floating over the sheet's top right: folds to a tab and
                    // swaps to the sheet's other side.
                    library: mod.widgets.FabLibraryFloat{}
                }
                // Fab's corners, never the sheet's raw one (square on a square sheet)
                Bench := BenchSetup{
                    radius: fab.radius radius_lg: fab.radius_lg
                    controls_panel +: {radius: fab.radius radius_lg: fab.radius_lg}
                }
                Scope := LogicScope{radius: fab.radius radius_lg: fab.radius_lg color_chrome: fab.color_editor}
                // A drag's ghost: the room its drop would take, before it is dropped.
                Ghost := View{
                    width: Fill height: Fill
                    show_bg: true
                    draw_bg +: {
                        pixel: fn() {
                            let sdf = Sdf2d.viewport(self.pos * self.rect_size)
                            sdf.box(2.0, 2.0, self.rect_size.x - 4.0, self.rect_size.y - 4.0, fab.radius_lg)
                            sdf.fill_keep(vec4(fab.color_accent.xyz, 0.12))
                            sdf.stroke(vec4(fab.color_accent.xyz, 0.7), 1.5)
                            return sdf.result
                        }
                    }
                }
                // The recorder (V10.1) as an editor of its own: the run board's controls summed up
                // in a header (its press brings the Board controls editor forward; hidden while a
                // Bench shows them) over the take's timeline and the transport. Its area is as tall
                // as what it holds (`fit_transport`); the area's notch sits in the left gutter.
                Transport := View{
                    width: Fill height: Fill flow: Down
                    // past the area's notch (38 wide) and its fillet (8)
                    padding: Inset{left: 46}
                    show_bg: true draw_bg +: {color: fab.color_editor}
                    controls := mod.widgets.ElecControlsStrip{
                        hidden: true
                        color_head: fab.color_header
                        color_rule: fab.color_border
                        color_hover: fab.color_button_hover
                        color_title: fab.color_text_dim
                        color_text: fab.color_text_dim
                    }
                    recorder := mod.widgets.ElecTransport{}
                }
                Firmware := mod.widgets.FirmwarePane{
                    head +: {
                        // the steps start past the area's notch (38 wide) and its fillet (8)
                        padding: Inset{left: 46 right: 10 top: 6 bottom: 6}
                        // the steps' corners are the sheet's control radius
                        steps +: { words +: {
                            statement +: { draw_bg +: { border_radius: fab.radius } }
                            entry +: { draw_bg +: { border_radius: fab.radius } }
                            out +: { draw_bg +: { border_radius: fab.radius } }
                        } }
                        fold +: { draw_icon +: { svg: crate_resource("self://resources/icons/chevron_down.svg") } }
                        unfold +: { draw_icon +: { svg: crate_resource("self://resources/icons/chevron_up.svg") } }
                    }
                }
            }
        }
        // Each area's nub (V10.2, FABWS Q2), over the dock at the area's top left or its tab
        // bar's left end (a layer of the shell, so the workspace's flow never moves it; it takes
        // presses before the dock).
        area_nubs := mod.widgets.FabAreaNotches{}
        // the printer camera floating over the window (the Camera panel's Floating)
        camera_float := mod.widgets.FabCameraFloat{}
        keymap_help := FabKeymapHelp{}
        about := FabAbout{}
        command_palette := FabCommandPalette{}
        // Menus sit under the tooltip layer and over everything else:
        // children handle events in reverse order, so only a top-of-shell
        // layer wins the press race against the dock.
        menu_layer := FabMenuLayer{}
        tip_layer := TipLayer{}
    }
}

/// The recorder's height before its first draw measures it (its single row).
const RECORDER_H: f64 = 55.0;

/// The owner id of an area's menu, and its rows past the editors.
const AREA_MENU: LiveId = live_id!(area_menu);
/// The owner id of a tab bar's new-tab menu (the editors, by group).
const ADD_MENU: LiveId = live_id!(area_add_menu);
const TOOLS: LiveId = live_id!(area_tools);
const SPLIT_RIGHT: LiveId = live_id!(area_split_right);
const SPLIT_DOWN: LiveId = live_id!(area_split_down);
const MAXIMIZE: LiveId = live_id!(area_maximize);
const RESTORE: LiveId = live_id!(area_restore);
const CLOSE_TAB: LiveId = live_id!(area_close_tab);
const CLOSE_AREA: LiveId = live_id!(area_close);
/// A Swap row's id: the other area's id mixed with this.
const SWAP_ROW: u64 = 0x5357_4150_524f_5753;
/// An Areas here row's id (a phone's area menu): the area's id mixed with this.
const HERE_ROW: u64 = 0x4845_5245_4152_4541;
/// Below this width only the area in use shows ([`FabShell::alone`]), and nothing drags.
const COMPACT: f64 = 520.0;

// Fab's sash floors, also used to decide if a proposed split has room along its axis.
const FIRST_PANE_WIDTH: f64 = 216.0;
const FIRST_PANE_HEIGHT: f64 = 28.0;
const SECOND_PANE_FLOOR: f64 = 50.0;

/// The sash width in Fab's Dock theme.
const DOCK_SPLITTER_W: f64 = 3.0;

/// First visible stack with the lowest rank. Walks the tree without cloning its stacks.
fn preferred_area(tree: &AreaTree, alone: Option<AreaId>,
                  rank: &impl Fn(AreaId, &[Instance], usize) -> u8) -> Option<(u8, AreaId)> {
    match tree {
        AreaTree::Area { id, tabs, selected } => {
            alone.is_none_or(|a| a == *id).then(|| (rank(*id, tabs, *selected), *id))
        }
        AreaTree::Split { a, b, .. } => {
            let left = preferred_area(a, alone, rank);
            if left.is_some_and(|(r, _)| r == 0) { return left; }
            let right = preferred_area(b, alone, rank);
            match (left, right) {
                (Some((l, _)), Some((r, _))) if r < l => right,
                _ => left.or(right),
            }
        }
    }
}

fn part_host_area(tree: &AreaTree, alone: Option<AreaId>) -> Option<AreaId> {
    preferred_area(tree, alone, &|_, tabs, selected| {
        if tabs.iter().any(|i| i.editor == Editor::Parts) { 0 }
        else if tabs.iter().any(|i| i.editor == Editor::Plates) { 1 }
        else if tabs.get(selected).is_some_and(|i| i.editor != Editor::Viewport) { 2 }
        else { 3 }
    }).map(|(_, id)| id)
}

struct PartPlacement {
    offered: bool,
    want_tab: bool,
    host: Option<AreaId>,
    forward: bool,
    open_sidebar: bool,
}

fn part_placement(tree: &AreaTree, width: f64, narrow: bool, alone: Option<AreaId>,
                  mode: Option<PlateMode>, test_mode: bool, page: Option<Editor>,
                  was_offered: bool, was_side_shown: bool, was_part_front: bool) -> PartPlacement {
    let design = mode == Some(PlateMode::Design) && page.is_none()
        && !tree.any_instance(&|i| i.editor == Editor::Part);
    let threshold = if was_offered { PART_SIDE_MIN_VIEW - PART_SIDE_HYST } else { PART_SIDE_MIN_VIEW };
    let offered = design && !test_mode && tree.lead_plate_area(alone).and_then(|area| {
        tree.area_width(area, width, narrow, alone, DOCK_SPLITTER_W, (FIRST_PANE_WIDTH, SECOND_PANE_FLOOR))
    }).is_some_and(|w| w >= threshold);
    let want_tab = design && !offered;
    let host = if want_tab { part_host_area(tree, alone) } else { None };
    PartPlacement { offered, want_tab, host, forward: want_tab && was_side_shown,
        open_sidebar: offered && was_part_front }
}

/// Which panels a node map shows.
#[cfg(test)]
fn panels_in(map: &DockMap) -> Vec<Editor> {
    Editor::all().filter(|p| map.contains_key(&p.tab())).collect()
}

/// A drag of a tab or an area's nub, from its start to its drop or its end.
struct AreaDrag {
    what: Moving,
    /// What it carries: the ghost shows where they would land.
    ghost: Vec<Instance>,
    /// Where its drop lands now: the node and the place on it, in the tree as it is.
    target: Option<(AreaId, DropAt)>,
    /// The tree that drop makes, shown (none: the tree as it is shows).
    shown: Option<AreaTree>,
    /// The dock drew what it shows now: its rects answer where the pointer is.
    drawn: bool,
    /// The pointer, where the chip naming what the drag carries rides.
    at: Option<DVec2>,
}

impl AreaDrag {
    fn new(what: Moving, ghost: Vec<Instance>) -> AreaDrag {
        AreaDrag { what, ghost, target: None, shown: None, drawn: true, at: None }
    }

    /// The chip's words: the editor it carries, and how many more ride with it.
    fn chip(&self) -> DropChip {
        let name = self.ghost.first().map_or("", |i| i.editor.name());
        let text = match self.ghost.len() {
            0 | 1 => name.to_string(),
            n => format!("{name} and {} more", n - 1),
        };
        DropChip { text, warn: false }
    }
}

/// Where a drop at the pointer lands, as the shown layout answers it.
enum Aim {
    /// On the drag's own ghost, or nowhere a drop could go: the target stays as it is.
    Keep,
    At(AreaId, DropAt),
}

#[derive(Clone, Copy, Debug)]
enum CornerAim {
    Undecided,
    Split { side: Side, align: SplitterAlign },
    Join { neighbour: AreaId },
}

/// Geometry at the press, before either preview could move an edge or hide the sibling.
struct CornerGesture {
    area: AreaId,
    rect: Rect,
    neighbours: Vec<(AreaId, Rect)>,
    preview: CornerPreview,
}

#[derive(Clone, Copy)]
enum CornerPreview {
    None,
    Split { side: Side, editor: Editor, align: SplitterAlign, splitter: LiveId },
    Join { neighbour: AreaId },
}

fn corner_align(rect: Rect, side: Side, at: DVec2) -> SplitterAlign {
    let offset = at - rect.pos;
    SplitterAlign::FromA(match side {
        Side::Left | Side::Right => offset.x,
        Side::Up | Side::Down => offset.y,
    })
}

/// Aim against the original layout, the way Blender reads an area corner: motion into the area
/// splits it, motion out of it across the corner's edge joins the area there. The motion's
/// dominant axis picks the edge. A split needs the pointer inside and room for both pane floors.
/// A join needs the single sibling across that edge and the pointer over it. The split decision
/// is O(1). The sibling and its original rectangle take O(areas) to find.
fn corner_aim(tree: &AreaTree, area: AreaId, rect: Rect, corner: Corner, start: DVec2, at: DVec2, neighbours: &[(AreaId, Rect)]) -> CornerAim {
    let motion = at - start;
    if motion.length() <= DRAG_DISTANCE {
        return CornerAim::Undecided;
    }
    let left = matches!(corner, Corner::BottomLeft);
    let up = matches!(corner, Corner::TopRight);
    let across = motion.x.abs() >= motion.y.abs();
    // Toward the corner's own edge on that axis: out of the area.
    let outward = if across { (motion.x < 0.0) == left } else { (motion.y < 0.0) == up };
    if !outward {
        if !rect.contains(at) {
            return CornerAim::Undecided;
        }
        let (side, room, floor) = if across {
            (if left { Side::Left } else { Side::Right }, rect.size.x, FIRST_PANE_WIDTH)
        } else {
            (if up { Side::Up } else { Side::Down }, rect.size.y, FIRST_PANE_HEIGHT)
        };
        if room < floor + SECOND_PANE_FLOOR {
            return CornerAim::Undecided;
        }
        return CornerAim::Split { side, align: corner_align(rect, side, at) };
    }
    let Some((parent, axis, _, second)) = tree.parent_split(area) else { return CornerAim::Undecided };
    let crossed = match axis {
        SplitterAxis::Horizontal if second => left && at.x < rect.pos.x,
        SplitterAxis::Horizontal => !left && at.x > rect.pos.x + rect.size.x,
        SplitterAxis::Vertical if second => up && at.y < rect.pos.y,
        SplitterAxis::Vertical => !up && at.y > rect.pos.y + rect.size.y,
    };
    if crossed {
        if let Some((_, a, b)) = tree.parts(parent) {
            let neighbour = if second { a } else { b };
            if tree.parts(neighbour).is_none() && neighbours.iter().any(|(id, r)| *id == neighbour && r.contains(at)) {
                return CornerAim::Join { neighbour };
            }
        }
    }
    CornerAim::Undecided
}

#[derive(Script, ScriptHook, Widget)]
pub struct FabShell {
    #[deref]
    view: View,
    /// The dock can resume a draw after a child yields.
    #[rust]
    cameras_frame: Option<u64>,
    /// The Board pane and unit that opened the Arrange menu.
    #[rust]
    arrange_from: Option<(WidgetUid, fab_elec_core::BoardId)>,
    /// The editor shown alone over the workspace ([`crate::api::UiState::page`]), as laid out.
    #[rust]
    page: Option<Editor>,
    /// The workspace laid out, and its shape then ([`WorkspaceDef::shape`]).
    #[rust]
    shown: Option<(WorkspaceId, u64)>,
    /// The instance revision applied to the current dock layout. A layout load clears it.
    #[rust]
    instance_rev: Option<u64>,
    /// The opening plate's views until the dock creates their bodies.
    #[rust]
    sheet_restore: Option<SavedView2d>,
    #[rust]
    board_restore: Option<SavedView2d>,
    #[rust]
    scope_restore: std::collections::BTreeMap<u32, ScopeView>,
    /// Repeat Scope numbers seen in layouts. Their dock bodies can outlive the layout.
    #[rust]
    repeat_scopes: std::collections::BTreeSet<u32>,
    /// A layout loaded since the Plate views were bound ([`Self::bind_plates`]), and the list's
    /// revision they were bound at.
    #[rust(true)]
    plates_stale: bool,
    #[rust]
    plates_rev: Option<u64>,
    /// Its kind ([`WorkspaceDef::kind`]).
    #[rust]
    kind: Option<Workspace>,
    /// The areas on screen (the workspace's tree as last laid out, sash moves and tabs pressed
    /// since), and the one maximized.
    #[rust]
    areas: Option<AreaTree>,
    #[rust]
    maximized: Option<AreaId>,
    /// The area the last press landed in, with its workspace: a Reveal of a list no area near
    /// it holds puts it there ([`WorkspaceOp::Reveal`]).
    #[rust]
    pressed_area: Option<(WorkspaceId, AreaId)>,
    /// The map the dock was last given, its fronts kept as tabs are pressed: the nubs and the
    /// menus read it.
    #[rust]
    layout: DockMap,
    /// A tab or a nub being dragged.
    #[rust]
    drag: Option<AreaDrag>,
    #[rust]
    corner: Option<CornerGesture>,
    /// What decided the recorder's area height last ([`Self::fit_transport`]).
    #[rust]
    transport_fit: Option<(bool, u64, Option<(usize, usize, bool)>, u64)>,
    /// The area whose menu is open.
    #[rust]
    area_menu: Option<AreaId>,
    /// The area whose new-tab menu is open.
    #[rust]
    add_menu: Option<AreaId>,
    /// The dock's shown editors changed since the state last heard ([`Self::publish_screen`]).
    #[rust]
    screen_stale: bool,
    /// The window is under [`COMPACT`] wide: only the area in use shows, full size.
    #[rust]
    compact: bool,
    /// The Plate draws its header strip (in Mechanics): its nub is the strip's left end.
    #[rust]
    plate_strip: bool,
    /// The temporary layout's strip as last synced: its words and whether the AI opened it.
    #[rust]
    temp_strip: Option<(String, bool)>,
    #[rust]
    review_elec: bool,
    #[rust]
    review_proposals: Option<Weak<Vec<Arc<ProposalView>>>>,
    #[rust]
    review_notice: Option<Arc<str>>,
    /// At a phone's width, an editor asked for that no area held yet: the area its Reveal puts
    /// it in becomes the one in use.
    #[rust]
    follow: Option<Editor>,
    /// The window is under [`NARROW`] wide: pinned side columns keep at most a third.
    #[rust]
    narrow: bool,
    /// The width the narrow sashes were last fitted to.
    #[rust]
    narrow_fit: f64,
    /// Sliced and Force as the shell last saw them: their editors join the stacks beside the
    /// parts and the part while they are on.
    #[rust]
    slice_shown: bool,
    #[rust]
    force_shown: bool,
    /// The mode's editor that came forward when its mode came on, until another tab of its
    /// stack is pressed.
    #[rust]
    extra_front: Vec<(AreaId, Editor)>,
    #[rust]
    part_side_offered: bool,
    #[rust]
    part_tab_shown: bool,
    #[rust]
    part_host: Option<AreaId>,
    /// View › Experimental's Arm and Scan, as the area menu offers their editors.
    #[rust]
    experimental: (bool, bool),
    /// A panel of a workspace to bring forward once that workspace's layout shows ([`ShowPanelIn`]).
    #[rust]
    panel_in: Option<(Workspace, Editor)>,
    /// The rails the Circuit's open Power flyout lists, by row ([`StripAction::PowerMenu`]).
    #[rust]
    power_rails: Vec<String>,
    #[rust]
    library_restored: bool,
    /// Place part asked for the Library editor: its search takes the keyboard after the next draw
    /// (the draw that brings the editor forward, or makes it).
    #[rust]
    library_search: bool,
    /// The window's width at the last draw.
    #[rust]
    width: f64,
    /// Files from outside dragged over the window, and where the pointer is: the chip saying
    /// what their drop does rides it ([`UiState::drop_chip`]).
    #[rust]
    file_drag: Option<(Vec<std::path::PathBuf>, DVec2)>,
    #[rust]
    chip_list: Option<DrawList2d>,
    #[live]
    draw_chip: DrawFabChip,
    #[live]
    draw_chip_text: DrawText,
    #[live]
    color_chip_text: Vec4f,
    #[live]
    color_chip_warn: Vec4f,
}

/// Below this width a pinned side column keeps at most a third of its split.
pub const NARROW: f64 = 1100.0;

const ELECTRONICS_PANELS: &[(Editor, fn(&WidgetRef, &mut Cx))] = &[
    (Editor::Board, |body, cx| { if let Some(mut pane) = body.borrow_mut::<BoardPane>() { pane.release(cx); } }),
    (Editor::Circuit, |body, cx| { if let Some(mut view) = body.borrow_mut::<ExplorerView>() { view.release(cx); } }),
];

/// Bodies that fit their content to their area when a layout is applied (a press of a layout or
/// its reset, never a sash drag; V9).
const ELECTRONICS_FIT: &[(Editor, fn(&WidgetRef, &mut Cx))] = &[
    (Editor::Board, |body, cx| { if let Some(mut pane) = body.borrow_mut::<BoardPane>() { pane.fit_when_shown(cx); } }),
    (Editor::Circuit, |body, cx| { if let Some(mut view) = body.borrow_mut::<ExplorerView>() { view.fit_when_shown(cx); } }),
];

/// Bodies to park when a layout switch leaves them off screen: each keeps its slot and state and
/// gives back what it holds on the GPU. O(entries) per load.
const ELECTRONICS_PARK: &[(Editor, fn(&WidgetRef, &mut Cx))] = &[
    (Editor::Device, |body, cx| {
        if let Some(mut device) = body.borrow_mut::<FabDevice3d>() { device.park(cx); }
    }),
];

impl FabShell {
    /// A focus from any electronics view (a Checks row, the explorer, code, the board itself) to
    /// the board pane, shown or dormant: it glides to what is focused (it ignores its own focuses
    /// and selections off its board) and lights the focused net of its board, or none for a focus
    /// that is no net. O(1) per focus action here; the pane's own cost is in `answer_focus`.
    /// The Circuit strip's Undo and Redo: the app's one history, sent as the caption's buttons
    /// send it. A dock lookup and two button lookups per action pass in the Electronics workspace.
    fn circuit_strip_actions(&mut self, cx: &mut Cx, actions: &Actions) {
        let dock = self.view.dock(cx, ids!(main.dock));
        let tab = Editor::Circuit.tab();
        let body = dock.borrow_mut().and_then(|mut dock| dock.items().get(&tab).map(|(_, body)| body.clone()));
        let Some(body) = body else { return };
        if body.button(cx, ids!(a_undo)).clicked(actions) {
            cx.action(ShellAction::Command("undo".into()));
        }
        if body.button(cx, ids!(a_redo)).clicked(actions) {
            cx.action(ShellAction::Command("redo".into()));
        }
    }

    /// Restores the Library once when its panel is ready. Saves user changes to its lists or place.
    fn library_prefs(&mut self, cx: &mut Cx, actions: &Actions, workspaces: Option<&Workspaces>) {
        use fab_elec_view::library::{EntryKey, LibraryAction, LibraryPanel};

        if let Some(workspaces) = workspaces.filter(|_| !self.library_restored) {
            let circuit = self.view.dock(cx, ids!(main.dock)).item(Editor::Circuit.tab());
            let panel = circuit.widget(cx, ids!(lib_open.library));
            let widgets = (circuit.borrow_mut::<ExplorerView>(), panel.borrow_mut::<LibraryPanel>());
            if let (Some(mut view), Some(mut library)) = widgets {
                let (pinned, recent) = crate::view_prefs::load_library();
                let keys = |pairs: Vec<(String, String)>| {
                    pairs.into_iter().map(|(lib, name)| EntryKey { lib, name }).collect()
                };
                library.set_remembered(cx, keys(pinned), keys(recent));
                let state = workspaces.instance_state(Instance::first(Editor::Circuit));
                view.set_library_place(cx, state.tools_left, !state.tools);
                self.library_restored = true;
            }
        }
        for action in actions.iter().filter_map(|a| a.downcast_ref::<LibraryAction>()) {
            match action {
                LibraryAction::Remember { pinned, recent } => {
                    // the Library editor's changes reach the float (the editor takes the float's)
                    let circuit = self.view.dock(cx, ids!(main.dock)).item(Editor::Circuit.tab());
                    if let Some(mut library) = circuit.widget(cx, ids!(lib_open.library)).borrow_mut::<LibraryPanel>() {
                        library.set_remembered(cx, pinned.clone(), recent.clone());
                    }
                    let pairs = |keys: &[EntryKey]| {
                        keys.iter().map(|key| (key.lib.clone(), key.name.clone())).collect::<Vec<_>>()
                    };
                    crate::view_prefs::save_library(&pairs(pinned), &pairs(recent));
                }
                LibraryAction::Place { left, folded } => cx.action(ShellAction::EditWorkspaces(ListOp::Region {
                    instance: Instance::first(Editor::Circuit), tools: Some(!*folded), left: Some(*left),
                })),
                LibraryAction::Arm(_) => {}
            }
        }
    }

    /// Open the Circuit's Power flyout to the button's right and return the picked rail or flag.
    fn power_flyout(&mut self, cx: &mut Cx, actions: &Actions) {
        for action in actions.iter().filter_map(|a| a.downcast_ref::<StripAction>()) {
            if let StripAction::PowerMenu { rails, has_power_flag, anchor } = action {
                self.power_rails = rails.iter().map(|(name, _)| name.clone()).collect();
                let mut items: Vec<_> = rails.iter().enumerate()
                    .map(|(i, (name, note))| MenuItem::new(LiveId(i as u64 + 1), name).note(note)).collect();
                if *has_power_flag {
                    if !items.is_empty() { items.push(MenuItem::sep()); }
                    items.push(MenuItem::new(live_id!(circuit_power_flag), "Power flag").note("supply from off the board"));
                }
                let at = Rect { pos: dvec2(anchor.pos.x + anchor.size.x + 4.0, anchor.pos.y), size: dvec2(0.0, 0.0) };
                open_menu(cx, live_id!(circuit_power), items, at, MenuPlace::At);
            }
        }
        if let Some(row) = menu_picked(actions, live_id!(circuit_power)) {
            if row == live_id!(circuit_power_flag) {
                cx.action(StripAction::PowerFlag);
            } else if let Some(name) = (row.0 as usize).checked_sub(1).and_then(|i| self.power_rails.get(i)) {
                cx.action(StripAction::Rail(name.clone()));
            }
        }
    }

    /// The Board opens its flyouts beside their buttons. An Arrange pick returns only
    /// to the pane and board that requested it, even when a second Board view is open.
    fn board_flyouts(&mut self, cx: &mut Cx, actions: &Actions) {
        for action in actions.iter().filter_map(|a| a.downcast_ref::<BoardStripAction>()) {
            let (owner, items, anchor) = match action {
                BoardStripAction::MethodMenu { rows, anchor } => {
                    let items = rows.iter().enumerate()
                        .map(|(i, (name, on))| MenuItem::new(LiveId(i as u64 + 1), *name).radio(*on)).collect();
                    (live_id!(board_route_method), items, anchor)
                }
                BoardStripAction::ArrangeMenu { pane, unit, enabled, anchor } => {
                    self.arrange_from = Some((*pane, *unit));
                    let items = ARRANGEMENTS.iter().enumerate()
                        .map(|(i, (name, _))| MenuItem::new(LiveId(i as u64 + 1), *name).enabled_if(enabled[i])).collect();
                    (live_id!(board_arrange), items, anchor)
                }
                _ => continue,
            };
            let at = Rect { pos: dvec2(anchor.pos.x + anchor.size.x + 4.0, anchor.pos.y), size: dvec2(0.0, 0.0) };
            open_menu(cx, owner, items, at, MenuPlace::At);
        }
        if let Some(LiveId(row)) = menu_picked(actions, live_id!(board_route_method)) {
            if let Some(i) = (row as usize).checked_sub(1) {
                cx.action(BoardStripAction::Method(i));
            }
        }
        if let Some(LiveId(row)) = menu_picked(actions, live_id!(board_arrange)) {
            if let (Some((pane, unit)), Some((_, command))) = (self.arrange_from.take(), (row as usize).checked_sub(1).and_then(|i| ARRANGEMENTS.get(i))) {
                cx.action(BoardStripAction::Arrange { pane, unit, command: *command });
            }
        }
    }

    fn answer_board_focus(&mut self, cx: &mut Cx, focus: &fab_elec_design::Focus) {
        let dock = self.view.dock(cx, ids!(main.dock));
        let tab = Editor::Board.tab();
        let body = dock.borrow_mut().and_then(|mut dock| dock.items().get(&tab).map(|(_, body)| body.clone()));
        let Some(body) = body else { return };
        let Some(mut pane) = body.borrow_mut::<fab_elec_view::board::BoardPane>() else { return };
        pane.answer_focus(cx, focus);
        let unit = pane.drawing().map(|draw| draw.unit);
        let net = match &focus.sel {
            fab_elec_core::Sel::Net(net) if Some(net.board) == unit => Some(net.net),
            _ => None,
        };
        pane.set_lit_net(cx, net);
    }

    /// Another view selected a group of parts (the schematic's box or its shift-clicks): the board
    /// pane selects the same parts, so its Place and arrange tools take them. O(selected).
    fn answer_board_group(&mut self, cx: &mut Cx, selected: &[fab_elec_core::Sel]) {
        let dock = self.view.dock(cx, ids!(main.dock));
        let tab = Editor::Board.tab();
        let body = dock.borrow_mut().and_then(|mut dock| dock.items().get(&tab).map(|(_, body)| body.clone()));
        let Some(body) = body else { return };
        let Some(mut pane) = body.borrow_mut::<fab_elec_view::board::BoardPane>() else { return };
        pane.set_selection(cx, selected);
    }

    /// A view cleared the selection: the board pane clears its own and its lit net.
    fn answer_board_unfocus(&mut self, cx: &mut Cx) {
        let dock = self.view.dock(cx, ids!(main.dock));
        let tab = Editor::Board.tab();
        let body = dock.borrow_mut().and_then(|mut dock| dock.items().get(&tab).map(|(_, body)| body.clone()));
        let Some(body) = body else { return };
        let Some(mut pane) = body.borrow_mut::<fab_elec_view::board::BoardPane>() else { return };
        pane.set_selection(cx, &[]);
        pane.set_lit_net(cx, None);
    }

    /// The Scope's selection lane follows the views' selection (V8.3).
    fn scope_follows(&mut self, cx: &mut Cx, sel: Option<&fab_elec_core::Sel>) {
        let dock = self.view.dock(cx, ids!(main.dock));
        let tab = Editor::Scope.tab();
        let body = dock.borrow_mut().and_then(|mut dock| dock.items().get(&tab).map(|(_, body)| body.clone()));
        let Some(body) = body else { return };
        let Some(mut scope) = body.borrow_mut::<fab_elec_view::instruments::logic_view::LogicScope>() else { return };
        scope.follow_selection(cx, sel);
    }

    /// Highlights the net's lane in the Scope (the one whose dock tab is `tab`) once it is
    /// available; `show` also brings the Scope forward (an explicit Scope button), a plain click on
    /// a wire never does.
    fn open_in_scope(&mut self, cx: &mut Cx, tab: LiveId, net: &str, show: bool) {
        if show {
            self.show_panel(cx, Editor::Scope);
        }
        let dock = self.view.dock(cx, ids!(main.dock));
        let body = dock.borrow_mut().and_then(|mut dock| dock.items().get(&tab).map(|(_, body)| body.clone()));
        let Some(body) = body else { return };
        let Some(mut scope) = body.borrow_mut::<fab_elec_view::instruments::logic_view::LogicScope>() else { return };
        scope.show_lane(cx, net);
    }

    /// Includes a tab body retained by the dock while another workspace is on screen.
    fn resident_body(&self, cx: &mut Cx, editor: Editor) -> WidgetRef {
        self.view.dock(cx, ids!(main.dock)).borrow_mut()
            .and_then(|mut dock| dock.items().get(&editor.tab()).map(|(_, body)| body.clone()))
            .unwrap_or_default()
    }

    fn resident_scopes(&self, cx: &mut Cx) -> Vec<(u32, WidgetRef)> {
        let dock = self.view.dock(cx, ids!(main.dock));
        let Some(mut dock) = dock.borrow_mut() else { return Vec::new() };
        let items = dock.items();
        std::iter::once(0).chain(self.repeat_scopes.iter().copied()).filter_map(|n| {
            let tab = Instance { editor: Editor::Scope, n }.tab();
            items.get(&tab).map(|(_, body)| (n, body.clone()))
        }).collect()
    }

    pub fn elec_views(&mut self, cx: &mut Cx) -> (Option<SavedView2d>, Option<SavedView2d>, Vec<(u32, ScopeView)>) {
        let sheet = self.resident_body(cx, Editor::Circuit).borrow::<ExplorerView>()
            .map_or(self.sheet_restore, |body| body.held_view());
        let board = self.resident_body(cx, Editor::Board).borrow::<BoardPane>()
            .map_or(self.board_restore, |body| body.held_view());
        let mut scopes = self.scope_restore.clone();
        for (n, body) in self.resident_scopes(cx) {
            if let Some(view) = body.borrow::<LogicScope>().and_then(|scope| scope.held_view(cx)) {
                scopes.entry(n).or_insert(view);
            }
        }
        (sheet, board, scopes.into_iter().collect())
    }

    pub fn restore_elec_views(&mut self, cx: &mut Cx, sheet: Option<SavedView2d>, board: Option<SavedView2d>, scopes: Vec<(u32, ScopeView)>) {
        self.sheet_restore = sheet;
        self.board_restore = board;
        self.scope_restore = scopes.into_iter().collect();
        self.hand_over_elec_views(cx, true);
    }

    /// At a switch, even an empty view supersedes the previous plate's. After a dock load,
    /// deliver any views that were waiting for a body.
    fn hand_over_elec_views(&mut self, cx: &mut Cx, switched: bool) {
        if switched || self.sheet_restore.is_some() {
            if let Some(mut body) = self.resident_body(cx, Editor::Circuit).borrow_mut::<ExplorerView>() {
                body.restore_view(cx, self.sheet_restore.take());
            }
        }
        if switched || self.board_restore.is_some() {
            if let Some(mut body) = self.resident_body(cx, Editor::Board).borrow_mut::<BoardPane>() {
                body.restore_view(cx, self.board_restore.take());
            }
        }
        if switched || !self.scope_restore.is_empty() {
            for (n, body) in self.resident_scopes(cx) {
                if let Some(mut scope) = body.borrow_mut::<LogicScope>() {
                    let view = self.scope_restore.remove(&n);
                    if switched || view.is_some() {
                        scope.restore_view(cx, view);
                    }
                }
            }
        }
    }

    /// Release electronics panes the load will drop because Drop has no Cx.
    /// The check costs O(electronics panels) map lookups per layout load, nothing per frame.
    fn load_layout(&mut self, cx: &mut Cx, map: DockMap) {
        let dock = self.view.dock(cx, ids!(main.dock));
        for &(panel, release) in ELECTRONICS_PANELS {
            let drops = match map.get(&panel.tab()) {
                Some(DockItem::Tab { kind, .. }) => *kind != panel.kind(),
                None => false,
                Some(_) => true,
            };
            if !drops {
                continue;
            }
            let mut body = dock.item(panel.tab());
            if body.is_empty() {
                // `item` hides dormant bodies; an id reused after another workspace still drops one.
                body = dock.borrow_mut().and_then(|mut dock| {
                    dock.items().get(&panel.tab()).map(|(_, body)| body.clone())
                }).unwrap_or_default();
            }
            release(&body, cx);
        }
        for &(panel, park) in ELECTRONICS_PARK {
            if dock.contains_item(panel.tab()) && !map.contains_key(&panel.tab()) {
                park(&dock.item(panel.tab()), cx);
            }
        }
        dock.load_state_preserving_items(cx, map.clone());
        self.hand_over_elec_views(cx, false);
        self.layout = map;
        self.instance_rev = None;
        self.screen_stale = true;
        self.plates_stale = true;
        self.transport_fit = None;
        self.narrow_fit = self.width;
        self.nubs(cx);
    }

    fn apply_layout(&mut self, cx: &mut Cx, map: DockMap) {
        self.load_layout(cx, map);
        self.view.redraw(cx);
    }

    /// An electronics editor is in one of the areas.
    fn shows_elec(&self) -> bool {
        self.areas.as_ref().is_some_and(|tree| tree.any_instance(&|i| i.editor.family() == Some(Family::Electronics)))
    }

    /// The Schematic and Board that `tree` shows fit their new areas at their next draw. Called
    /// when a layout is applied; a body not made yet fits on its first draw anyway. A plate's
    /// saved view not shown yet places them instead, the first time (7b). O(areas) lookups per
    /// applied layout.
    fn fit_elec_views(&mut self, cx: &mut Cx, tree: &AreaTree) {
        for &(panel, fit) in ELECTRONICS_FIT {
            if tree.area_of_tab(panel.tab()).is_none() {
                continue;
            }
            fit(&self.resident_body(cx, panel), cx);
        }
    }

    /// The modes' editors as the stacks show them: Slice beside the parts while the slice is
    /// on, Force beside the slicer while the forces show, each in front from when its mode came on
    /// until another tab of its stack is pressed.
    fn mode_extras(&self, tree: &AreaTree) -> Vec<(Editor, Editor, bool)> {
        let mut out = Vec::new();
        for (shown, beside, editor) in [(self.slice_shown, Editor::Parts, Editor::Slice),
                                        (self.force_shown, Editor::Slicer, Editor::Force)] {
            if shown {
                let forward = tree.showing(beside).is_some_and(|area| self.extra_front.contains(&(area, editor)));
                out.push((beside, editor, forward));
            }
        }
        out
    }

    fn front_extra(&mut self, area: AreaId, editor: Editor) {
        self.extra_front.retain(|(a, e)| *a != area && *e != editor);
        self.extra_front.push((area, editor));
    }

    fn mode_changed(&mut self, tree: &AreaTree, beside: Editor, editor: Editor, on: bool) {
        self.extra_front.retain(|(_, e)| *e != editor);
        if on && !tree.any_instance(&|i| i.editor == editor) {
            if let Some(area) = tree.showing(beside) {
                self.front_extra(area, editor);
            }
        }
    }

    /// `tree` as the dock shows it now (the modes' editors, the window's width, the area
    /// maximized), the drag's `ghost` where its drop would put them. O(areas + tabs).
    fn present_now(&mut self, tree: &AreaTree, ghost: &[Instance]) -> DockMap {
        self.repeat_scopes.extend(tree.instances().into_iter().filter_map(|(_, i)| {
            (i.editor == Editor::Scope && i.n != 0).then_some(i.n)
        }));
        let extra = self.mode_extras(tree);
        let narrow = (self.narrow && self.width > 0.0).then_some(self.width);
        let part = self.part_host.map(|area| (area, Editor::Part, self.extra_front.contains(&(area, Editor::Part))));
        Present { extra: &extra, ghost, narrow, maximized: self.alone(tree), page: self.page }.with_area_extra(tree, part)
    }

    /// The area shown alone: at a phone's width the one in use (the last pressed or picked in
    /// Areas here, else the maximized one, else the first a view fronts, else the first), else
    /// the maximized one. O(areas).
    fn alone(&self, tree: &AreaTree) -> Option<AreaId> {
        self.alone_for(tree, self.shown.map(|(id, _)| id), self.maximized, None)
    }

    fn alone_for(&self, tree: &AreaTree, ws: Option<WorkspaceId>, maximized: Option<AreaId>,
                 follow: Option<Editor>) -> Option<AreaId> {
        if !self.compact { return maximized; }
        let pressed = follow.and_then(|e| tree.showing(e)).or_else(|| {
            self.pressed_area.filter(|(id, _)| Some(*id) == ws).map(|(_, area)| area)
        });
        preferred_area(tree, None, &|id, tabs, selected| {
            if Some(id) == pressed { 0 }
            else if Some(id) == maximized { 1 }
            else if tabs.get(selected).is_some_and(|i| i.editor == Editor::Viewport) { 2 }
            else if tabs.get(selected).is_some_and(|i| i.editor.keeps_corner()) { 3 }
            else { 4 }
        }).map(|(_, area)| area)
    }

    /// Set placement before any layout load so the dock constructs the final tab sets once.
    /// A width-only change uses the returned flag to edit just the existing tab bar.
    fn update_part_placement(&mut self, state: &mut AppState, upcoming: Option<&WorkspaceDef>) -> bool {
        let fresh = upcoming.is_some_and(|d| self.shown.is_none_or(|(id, _)| id != d.id));
        let (tree, alone) = if let Some(d) = upcoming {
            (&d.tree, self.alone_for(&d.tree, Some(d.id), d.maximized, self.follow))
        } else if let Some(tree) = &self.areas {
            (tree, self.alone(tree))
        } else {
            return false;
        };
        let was_front = self.part_host.is_some_and(|a| self.extra_front.contains(&(a, Editor::Part)));
        let next = part_placement(tree, self.width, self.narrow, alone, state.ui.mode(), state.print.test_mode,
            self.page, !fresh && self.part_side_offered, !fresh && state.ui.part_side_shown, !fresh && was_front);
        self.part_side_offered = next.offered;
        state.ui.part_side_offered = next.offered;
        if !next.offered { state.ui.part_side_shown = false; }
        if next.open_sidebar { state.ui.part_sidebar = true; }
        let changed = next.want_tab != self.part_tab_shown || next.host != self.part_host;
        if changed || fresh {
            self.extra_front.retain(|(_, e)| *e != Editor::Part);
            if let Some(area) = next.host.filter(|_| next.forward || (!fresh && was_front)) {
                self.front_extra(area, Editor::Part);
            }
        }
        self.part_host = next.host;
        self.part_tab_shown = next.want_tab;
        changed
    }

    /// Keep the existing tab bar and every sash alive, including the one under the pointer.
    fn sync_part_placement(&mut self, cx: &mut Cx, state: &mut AppState) {
        if !self.update_part_placement(state, None) { return; }
        let Some(tree) = &self.areas else { return };
        let dock = self.view.dock(cx, ids!(main.dock));
        let part = Editor::Part.tab();
        if let Some(set) = self.set_of_tab(part) {
            let area = self.area_of_set(set);
            let underlying = area.and_then(|a| tree.instance(a));
            if let Some(DockItem::Tabs { tabs, selected, hide_tab_bar, .. }) = self.layout.get_mut(&set) {
                let front = tabs[*selected];
                dock.close_tab_preserving_item(cx, part);
                tabs.retain(|t| *t != part);
                let restore = if front == part { underlying.map(Instance::tab) } else { Some(front) };
                if let Some(k) = restore.and_then(|tab| tabs.iter().position(|t| *t == tab)) {
                    *selected = k;
                    if front == part { dock.select_tab(cx, tabs[k]); }
                }
                let hidden = tabs.len() == 1 && underlying.is_some_and(|i| i.editor.bare_alone());
                if *hide_tab_bar != hidden {
                    *hide_tab_bar = hidden;
                    dock.set_tab_bar_hidden(cx, set, hidden);
                }
            }
            self.layout.remove(&part);
        }
        if let Some(area) = self.part_host {
            let set = if self.alone(tree) == Some(area) || tree.id() == area { live_id!(root) } else { area.area_live_id() };
            if let Some(DockItem::Tabs { tabs, selected, hide_tab_bar, .. }) = self.layout.get_mut(&set) {
                let after = tabs.iter().position(|t| *t == Editor::Parts.tab());
                let at = after.map_or(tabs.len(), |k| k + 1);
                let kind = Editor::Part.kind();
                let template = live_id!(PermanentTab);
                let name = Editor::Part.name().to_owned();
                dock.create_tab(cx, set, part, kind, name.clone(), template, after);
                tabs.insert(at, part);
                if self.extra_front.contains(&(area, Editor::Part)) {
                    *selected = at;
                    dock.select_tab(cx, part);
                } else if at <= *selected {
                    *selected += 1;
                }
                if *hide_tab_bar {
                    *hide_tab_bar = false;
                    dock.set_tab_bar_hidden(cx, set, false);
                }
                self.layout.insert(part, DockItem::Tab { kind, name, template });
            }
        }
        self.screen_stale = true;
        self.nubs(cx);
        dock.redraw(cx);
    }

    /// A signal can reach a layout without a viewport to invalidate. Request a draw then,
    /// and bind changed rows only as they are drawn. Unchanged identities cost O(1).
    fn sync_review(&mut self, cx: &mut Cx, drawing: bool) {
        let live = cx.get_global_ref::<ElecLive>().filter(|_| self.review_elec);
        let proposals = live.filter(|l| !l.proposals_reloading && !l.snap.proposals.is_empty()).map(|l| &l.snap.proposals);
        let notice = live.and_then(|l| l.ended_proposals.as_ref());
        if proposals.map(Arc::as_ptr) == self.review_proposals.as_ref().map(Weak::as_ptr)
            && notice.map(Arc::as_ptr) == self.review_notice.as_ref().map(Arc::as_ptr) {
            return;
        }
        if !drawing {
            self.view.area().redraw(cx);
            return;
        }
        let was_visible = self.review_proposals.is_some() || self.review_notice.is_some();
        let proposals = proposals.cloned();
        self.review_proposals = proposals.as_ref().map(Arc::downgrade);
        self.review_notice = notice.cloned();
        let area = self.view.view(cx, ids!(main.review_area));
        let visible = proposals.is_some() || self.review_notice.is_some();
        if visible != was_visible {
            area.set_visible(cx, visible);
            self.view.area().redraw(cx);
        }
        let widget = area.widget(cx, ids!(review));
        if let Some(mut strip) = widget.borrow_mut::<ReviewStrip>() {
            strip.sync(cx, proposals.as_ref().map_or(&[], |p| p.as_slice()), self.review_notice.clone());
        };
    }

    /// The temporary layout's strip shown with `strip`'s words (the spark in the AI's colour when
    /// the AI opened it), or hidden. On a change only.
    fn sync_temp_strip(&mut self, cx: &mut Cx, strip: Option<(String, bool)>) {
        let view = self.view.view(cx, ids!(main.temp_strip));
        view.set_visible(cx, strip.is_some());
        if let Some((words, ai)) = &strip {
            view.label(cx, ids!(note)).set_text(cx, words);
            let color = if *ai {
                let ai = fab_elec_view::canvas::looks::Tokens::for_theme(base_theme(cx) != BaseTheme::Light).ai;
                vec4(ai[0], ai[1], ai[2], ai[3])
            } else {
                self.color_chip_text
            };
            let mut spark = view.widget(cx, ids!(spark));
            script_apply_eval!(cx, spark, { draw_icon +: { color: #(color) } });
        }
        self.temp_strip = strip;
        self.view.redraw(cx);
    }

    /// The areas laid out again as they are (a mode came or went, the window crossed a width,
    /// a drag ended without a drop). A dock load, bodies kept.
    fn relayout(&mut self, cx: &mut Cx) {
        if let Some(tree) = self.areas.clone() {
            let map = self.present_now(&tree, &[]);
            self.apply_layout(cx, map);
        }
    }

    /// Lays out workspace `def` (the caption's choice, or its areas changed shape) as its tree
    /// says. Every editor keeps its state (`load_state_preserving_items`). O(areas + tabs) and a
    /// dock load, on a switch or an edit.
    pub fn show_workspace(&mut self, cx: &mut Cx, def: &WorkspaceDef) {
        self.shown = Some((def.id, def.shape));
        self.kind = Some(def.kind());
        self.maximized = def.maximized;
        self.drag = None;
        self.reset_corner(cx);
        self.fit_elec_views(cx, &def.tree);
        // the next load after a phone's Reveal is the tree it made
        if let Some(area) = self.follow.take().and_then(|editor| def.tree.showing(editor)) {
            self.pressed_area = Some((def.id, area));
        }
        self.areas = Some(def.tree.clone());
        self.review_elec = self.shows_elec();
        let map = self.present_now(&def.tree, &[]);
        self.apply_layout(cx, map);
    }

    /// The shown workspace laid out again from the state's list (a test drives the shell so).
    #[cfg(test)]
    pub fn sync_with(&mut self, cx: &mut Cx, state: &mut AppState) {
        let def = state.ui.workspaces.current().clone();
        if self.shown != Some((def.id, def.shape)) {
            self.show_workspace(cx, &def);
        }
        self.bind_plates(cx, state);
        self.sync_regions(cx, &state.ui.workspaces);
    }

    /// Binds each Plate view the dock holds to its pane before it hears an event or draws
    /// (Plate 0 to pane 0, a later one to a pane it leases), and releases a later Plate view no
    /// workspace holds any more, in the list or the temporary layout: its pane and its
    /// viewport's passes and target go back. Then the state knows each bound Plate's pane
    /// ([`ViewportState::plate_instances`]). Once after each layout load or change of the list,
    /// O(resident bodies x the workspaces' tabs); one compare otherwise.
    pub(crate) fn bind_plates(&mut self, cx: &mut Cx, state: &mut AppState) {
        let rev = state.ui.workspaces.rev;
        if !self.plates_stale && self.plates_rev == Some(rev) {
            return;
        }
        self.plates_stale = false;
        self.plates_rev = Some(rev);
        let ws = &state.ui.workspaces;
        let mut held: Vec<Instance> = Vec::new();
        for tree in ws.list.iter().map(|d| &d.tree).chain(ws.temp.iter().map(|t| &t.def.tree)).chain(self.areas.iter()) {
            for (_, i) in tree.instances() {
                if i.editor == Editor::Viewport && !held.contains(&i) {
                    held.push(i);
                }
            }
        }
        let first = Instance::first(Editor::Viewport);
        let kind = Editor::Viewport.kind();
        let bodies: Vec<(LiveId, WidgetRef)> = self.view.dock(cx, ids!(main.dock)).borrow_mut().map(|mut dock| {
            dock.items().iter().filter(|(_, (k, _))| *k == kind).map(|(tab, (_, body))| (*tab, body.clone())).collect()
        }).unwrap_or_default();
        let mut bound = Vec::new();
        for (tab, body) in bodies {
            let Some(mut stage) = body.borrow_mut::<crate::ui::stage::FabStage>() else { continue };
            match held.iter().copied().find(|i| i.tab() == tab).or((tab == first.tab()).then_some(first)) {
                Some(instance) => stage.ensure_bound(cx, state, instance),
                None => stage.release(cx, state),
            }
            bound.extend(stage.bound_plate().filter(|(i, _)| i.n > 0).map(|(i, pane)| (i.n, pane)));
        }
        bound.sort_unstable_by_key(|(n, _)| *n);
        state.view.plate_instances = bound;
        // each later Plate view's link (to its source's pane, free while that is not bound) and
        // mode, as its instance keeps them (View › Underside view, View › Traced view)
        for (n, pane) in state.view.plate_instances.clone() {
            let kept = state.ui.workspaces.instance_state(Instance { editor: Editor::Viewport, n });
            let link = kept.link.and_then(|(how, source)| Some(PaneLink::Linked { how, to: state.view.plate_pane(source)? }));
            state.show_plate_view(pane, link.unwrap_or(PaneLink::Free), kept.traced);
        }
    }

    /// Apply shelves before drawing the dock, only when the instance revision changes or a
    /// layout load clears the key. An unchanged frame checks one counter and visits no bodies.
    fn sync_regions(&mut self, cx: &mut Cx, workspaces: &Workspaces) {
        if self.instance_rev == Some(workspaces.instance_rev) {
            return;
        }
        self.instance_rev = Some(workspaces.instance_rev);
        let dock = self.view.dock(cx, ids!(main.dock));
        let plates = self.areas.as_ref().map(|tree| tree.instances()).unwrap_or_default();
        for plate in plates.into_iter().map(|(_, i)| i).filter(|i| i.editor == Editor::Viewport) {
            if let Some(mut stage) = dock.item(plate.tab()).borrow_mut::<crate::ui::stage::FabStage>() {
                stage.set_tools_shown(cx, workspaces.instance_state(plate).tools);
            }
        }
        if self.library_restored {
            let circuit = Instance::first(Editor::Circuit);
            if let Some(mut view) = dock.item(circuit.tab()).borrow_mut::<ExplorerView>() {
                let state = workspaces.instance_state(circuit);
                view.set_library_place(cx, state.tools_left, !state.tools);
            }
        } else {
            self.library_prefs(cx, &[], Some(workspaces));
        }
    }

    /// The dock's id of a split in the tree being presented, including a preview tree.
    fn split_dock_id(tree: &AreaTree, split: AreaId) -> LiveId {
        if tree.id() == split { live_id!(root) } else { split.split_live_id() }
    }

    /// The area the dock's tab set `set` shows (its root: the area shown alone, or the only one).
    fn area_of_set(&self, set: LiveId) -> Option<AreaId> {
        let tree = self.areas.as_ref()?;
        if let (Some(area), true) = (self.alone(tree), set == live_id!(root)) {
            return Some(area);
        }
        tree.area_of_live_id(set)
    }

    /// The dock's tab set holding `tab` now.
    fn set_of_tab(&self, tab: LiveId) -> Option<LiveId> {
        self.layout.iter().find_map(|(id, item)| match item {
            DockItem::Tabs { tabs, .. } if tabs.contains(&tab) => Some(*id),
            _ => None,
        })
    }

    /// The area whose stack shows `tab` (a mode's editor shows in the stack it joined).
    fn area_of_shown_tab(&self, tab: LiveId) -> Option<AreaId> {
        self.set_of_tab(tab).and_then(|set| self.area_of_set(set))
    }

    /// The recorder's controls header hidden while a Bench is on screen (its front panel holds
    /// the same controls), and the recorder's area as tall as what it holds (the header over the
    /// recorder) when the area is the lower part of a split pinned from the bottom, as the
    /// defaults have it. Before each draw of an areas workspace showing the recorder: O(1).
    fn fit_transport(&mut self, cx: &mut Cx) {
        // under a page the recorder is not laid out: its area keeps its height
        if self.page.is_some() {
            return;
        }
        let dock = self.view.dock(cx, ids!(main.dock));
        let body = dock.item(Editor::Transport.tab());
        if body.is_empty() {
            // made at its first draw
            return;
        }
        let Some(tree) = &self.areas else { return };
        let bench = tree.showing(Editor::Bench).is_some();
        // The tab's own header strip, its direct child: the recorder below it has `controls` too.
        let strip = body.child(id!(controls));
        let Some(mut strip) = strip.borrow_mut::<ElecControlsStrip>() else { return };
        strip.set_hidden(cx, bench);
        let Some(area) = tree.showing(Editor::Transport) else { return };
        let Some((split, SplitterAxis::Vertical, SplitterAlign::FromB(h), true)) = tree.parent_split(area) else { return };
        let recorder = Some(body.widget(cx, ids!(recorder)).area().rect(cx).size.y).filter(|h| *h > 0.0).unwrap_or(RECORDER_H);
        let key = (bench, recorder.to_bits(), strip.shown_counts(), h.to_bits());
        if self.transport_fit == Some(key) {
            return;
        }
        self.transport_fit = Some(key);
        let want = (strip.wanted_height() + recorder).ceil();
        drop(strip);
        if (h - want).abs() < 0.5 {
            return;
        }
        let align = SplitterAlign::FromB(want);
        dock.set_splitter_align(cx, Self::split_dock_id(tree, split), align, true);
        if let Some(tree) = &mut self.areas {
            let _ = tree.apply(WorkspaceOp::Resize { split, align });
        }
        cx.action(ShellAction::EditAreas(WorkspaceOp::Resize { split, align }));
        self.view.redraw(cx);
    }

    /// Follow AI: an edit from outside the app changed editors the shown workspace does not show,
    /// so the first workspace that shows one of them comes forward. Nothing moves while Follow AI
    /// is off or the edit is on screen already. O(workspaces x areas), once per outside edit.
    fn follow_ai(&mut self, cx: &mut Cx, state: &AppState, edit: &ElecOutsideEdit) {
        if !state.ui.follow_ai {
            return;
        }
        let panels: Vec<Editor> = edit.editors.iter().map(|e| elec_editor(*e)).collect();
        let shows = |d: &WorkspaceDef| d.tree.areas().iter().any(|(_, shown)| panels.contains(&shown.editor));
        if shows(state.ui.workspaces.current()) {
            return;
        }
        if let Some(d) = state.ui.workspaces.list.iter().find(|d| state.ui.offered(d) && shows(d)) {
            cx.action(ShellAction::SwitchWorkspace(d.id));
        }
    }

    /// The areas that show what an outside edit changed light their outline for a few seconds
    /// (V9.3), whichever layout shows them by then.
    fn outline_edited(&mut self, cx: &mut Cx, edit: &ElecOutsideEdit) {
        let kinds: Vec<LiveId> = edit.editors.iter().map(|e| elec_editor(*e).kind()).collect();
        if let Some(mut nubs) = self.view.widget(cx, ids!(area_nubs)).borrow_mut::<FabAreaNotches>() {
            nubs.light(cx, &kinds, matches!(edit.origin, EditOrigin::Ai | EditOrigin::Agent));
        }
    }

    /// The Firmware pane washes the lines an outside edit changed in its document (V9.3).
    fn flash_firmware(&mut self, cx: &mut Cx, edit: &ElecOutsideEdit) {
        if edit.lines.is_empty() {
            return;
        }
        let dock = self.view.dock(cx, ids!(main.dock));
        let body = dock.borrow_mut().and_then(|mut dock| dock.items().get(&Editor::Firmware.tab()).map(|(_, body)| body.clone()));
        let Some(body) = body else { return };
        let Some(mut pane) = body.borrow_mut::<fab_elec_view::firmware::FirmwarePane>() else { return };
        for (doc, lines) in &edit.lines {
            pane.flash_lines(cx, doc, lines.clone());
        }
    }

    /// Each area the dock shows with its rectangle in window points, its tab bar included, as
    /// last drawn (fab.workspace tells the AI where each area is). O(tab sets).
    pub fn area_rects(&self, cx: &mut Cx) -> Vec<(AreaId, Rect)> {
        let dock = self.view.dock(cx, ids!(main.dock));
        self.layout.iter().filter_map(|(set, item)| match item {
            DockItem::Tabs { tabs, selected, hide_tab_bar, .. } => {
                let area = self.area_of_set(*set)?;
                let mut rect = dock.item(*tabs.get(*selected)?).area().rect(cx);
                if !*hide_tab_bar {
                    rect.pos.y -= crate::ui::area_notch::BAR_H;
                    rect.size.y += crate::ui::area_notch::BAR_H;
                }
                (rect.size.x > 0.0 && rect.size.y > 0.0).then_some((area, rect))
            }
            _ => None,
        }).collect()
    }

    /// The areas' nubs as the dock shows them: one per tab set at its front tab, in the corner
    /// of a set showing no tab bar, else at the bar's left end; none on a drag's ghost.
    /// O(tab sets), on a load or a tab pressed.
    fn nubs(&mut self, cx: &mut Cx) {
        let dock = self.view.dock(cx, ids!(main.dock));
        // a page over the workspace has Back, not an area's nub
        let layout = if self.page.is_some() { &DockMap::new() } else { &self.layout };
        // a tab may close where its area has another, or there is another area ("Close tab")
        let area_count = layout.values().filter(|item| matches!(item, DockItem::Tabs { .. })).count();
        let mut areas: Vec<(LiveId, NotchedArea)> = layout.iter().filter_map(|(set, item)| match item {
            DockItem::Tabs { tabs, selected, hide_tab_bar, .. } => {
                let tab = *tabs.get(*selected)?;
                match layout.get(&tab) {
                    Some(DockItem::Tab { kind, .. }) if tab != GHOST_TAB => {
                        let strip = self.plate_strip && *kind == Editor::Viewport.kind();
                        let closable = (tabs.len() > 1 || area_count > 1) && !self.is_transient_tab(tab);
                        let last = tabs.last().copied().filter(|_| !hide_tab_bar);
                        Some((*set, NotchedArea { tab, kind: *kind, bar: !hide_tab_bar, strip, closable, last }))
                    }
                    _ => None,
                }
            }
            _ => None,
        }).collect();
        // a map is no order; the widget tree lists the nubs as the areas go
        areas.sort_by_key(|(set, _)| self.area_of_set(*set).map_or(u32::MAX, |a| a.0));
        let areas = areas.into_iter().map(|(_, nub)| nub).collect();
        if let Some(mut nubs) = self.view.widget(cx, ids!(area_nubs)).borrow_mut::<FabAreaNotches>() {
            // A maximized area shows no sibling or sash to split or join with: no corners.
            nubs.set_corners_enabled(cx, !self.compact && self.maximized.is_none());
            nubs.set_areas(cx, dock, areas);
        }
    }

    /// Show one column per offered editor group under the nub, with the area's commands
    /// in the footer. At a phone's width, where only this area shows, the first column is Areas
    /// here (each area by its front editor; a pick shows that one) and the footer keeps Swap
    /// with, Tools for an editor with a shelf, and the closes. O(editors + layout items + tabs + areas) per open.
    fn open_area_menu(&mut self, cx: &mut Cx, tab: LiveId, anchor: Rect, workspaces: Option<&Workspaces>) {
        let Some(area) = self.area_of_shown_tab(tab) else { return };
        let Some(tree) = &self.areas else { return };
        let Some((tabs, selected)) = tree.stack_of(area) else { return };
        let front = tabs.get(selected).copied();
        let (arm, scan) = self.experimental;
        let offered = |e: &Editor| match e.family() {
            Some(Family::Arm) => arm,
            Some(Family::Scan) => scan,
            _ => true,
        };
        let mut columns: Vec<(String, Vec<MenuItem>)> = Vec::new();
        if self.compact {
            let here = tree.areas().into_iter().map(|(a, i)| MenuItem::new(LiveId(a.0 as u64 ^ HERE_ROW), i.editor.title()).current(a == area)).collect();
            columns.push(("Areas here".to_owned(), here));
        }
        columns.extend(MENU.iter().filter_map(|(heading, editors)| {
            let rows: Vec<_> = editors.iter().filter(|e| offered(e))
                .map(|e| MenuItem::new(e.tab(), e.title()).current(front.is_some_and(|i| i.editor == *e)))
                .collect();
            (!rows.is_empty()).then(|| ((*heading).to_owned(), rows))
        }));
        let mut footer = Vec::new();
        let others: Vec<MenuItem> = tree.areas().into_iter().filter(|(a, _)| *a != area)
            .map(|(a, i)| MenuItem::new(LiveId(a.0 as u64 ^ SWAP_ROW), i.editor.title())).collect();
        let areas = others.len() + 1;
        if !others.is_empty() {
            footer.push(MenuItem::new(live_id!(area_swap), "Swap with").flyout(others));
        }
        if let (Some(ws), Some(instance)) = (workspaces, front.filter(|i| i.tab() == tab && i.editor.has_tools())) {
            footer.push(MenuItem::new(TOOLS, "Tools").current(ws.instance_state(instance).tools));
        }
        if !self.compact {
            footer.push(MenuItem::new(SPLIT_RIGHT, "Split right"));
            footer.push(MenuItem::new(SPLIT_DOWN, "Split down"));
            footer.push(match self.maximized {
                Some(_) => MenuItem::new(RESTORE, "Restore"),
                None => MenuItem::new(MAXIMIZE, "Maximize").enabled_if(areas > 1),
            });
        }
        if !footer.is_empty() {
            footer.push(MenuItem::sep());
        }
        footer.push(MenuItem::new(CLOSE_TAB, "Close tab").enabled_if((tabs.len() > 1 || areas > 1) && !self.is_transient_tab(tab)));
        footer.push(MenuItem::new(CLOSE_AREA, "Close area").enabled_if(areas > 1));
        self.area_menu = Some(area);
        crate::ui::popover::open_menu_columns(cx, AREA_MENU, columns, footer, anchor, MenuPlace::Below);
    }

    /// A tab bar's new-tab menu: every editor the area menu offers, by its groups; those the
    /// area holds are marked (picking one brings it forward).
    fn open_add_menu(&mut self, cx: &mut Cx, tab: LiveId, anchor: Rect) {
        let Some(area) = self.area_of_shown_tab(tab) else { return };
        let Some(tree) = &self.areas else { return };
        let Some((tabs, _)) = tree.stack_of(area) else { return };
        let (arm, scan) = self.experimental;
        let offered = |e: &Editor| match e.family() {
            Some(Family::Arm) => arm,
            Some(Family::Scan) => scan,
            _ => true,
        };
        let columns: Vec<(String, Vec<MenuItem>)> = MENU.iter().filter_map(|(heading, editors)| {
            let rows: Vec<_> = editors.iter().filter(|e| offered(e))
                .map(|e| MenuItem::new(e.tab(), e.title()).current(tabs.iter().any(|i| i.editor == *e)))
                .collect();
            (!rows.is_empty()).then(|| ((*heading).to_owned(), rows))
        }).collect();
        self.add_menu = Some(area);
        crate::ui::popover::open_menu_columns(cx, ADD_MENU, columns, Vec::new(), anchor, MenuPlace::Below);
    }

    /// A row of an area menu picked.
    fn area_menu_picked(&mut self, cx: &mut Cx, id: LiveId, workspaces: Option<&Workspaces>) {
        let Some(area) = self.area_menu.take() else { return };
        if let (Ok(here), Some((ws, _))) = (u32::try_from(id.0 ^ HERE_ROW), self.shown) {
            // a phone's Areas here: that area is the one in use
            if here != area.0 {
                self.pressed_area = Some((ws, AreaId(here)));
                self.relayout(cx);
            }
            return;
        }
        let Some(tree) = &self.areas else { return };
        let front = tree.instance(area);
        if id == TOOLS {
            if let (Some(ws), Some(instance)) = (workspaces, front.filter(|i| i.editor.has_tools())) {
                cx.action(ShellAction::EditWorkspaces(ListOp::Region {
                    instance, tools: Some(!ws.instance_state(instance).tools), left: None,
                }));
            }
            return;
        }
        let op = if let Some(editor) = Editor::all().find(|e| e.tab() == id) {
            WorkspaceOp::SetEditor { area, editor }
        } else if id == SPLIT_RIGHT || id == SPLIT_DOWN {
            let side = if id == SPLIT_RIGHT { Side::Right } else { Side::Down };
            let editor = split_editor(tree, front.map(|i| i.editor));
            WorkspaceOp::Split { area, side, editor, align: SplitterAlign::Weighted(0.5) }
        } else if id == MAXIMIZE || id == RESTORE {
            WorkspaceOp::Maximize { area: (id == MAXIMIZE).then_some(area) }
        } else if id == CLOSE_TAB {
            let Some(instance) = front else { return };
            WorkspaceOp::CloseTab { area, instance }
        } else if id == CLOSE_AREA {
            WorkspaceOp::Close { area }
        } else {
            match u32::try_from(id.0 ^ SWAP_ROW) {
                Ok(other) => WorkspaceOp::Swap { a: area, b: AreaId(other) },
                Err(_) => return,
            }
        };
        cx.action(ShellAction::EditAreas(op));
    }

    fn is_transient_tab(&self, tab: LiveId) -> bool {
        [Editor::Part, Editor::Slice, Editor::Force].into_iter().any(|e| e.tab() == tab)
            && self.areas.as_ref().is_some_and(|tree| !tree.any_instance(&|i| i.tab() == tab))
    }

    /// A tab pressed: it is in front of its area (the tree keeps it, the file once the hand lets
    /// go); a mode's editor stays in front until another tab of its stack is pressed.
    fn tab_pressed(&mut self, cx: &mut Cx, tab: LiveId) {
        if let Some(set) = self.set_of_tab(tab) {
            if let Some(DockItem::Tabs { tabs, selected, .. }) = self.layout.get_mut(&set) {
                if let Some(k) = tabs.iter().position(|t| *t == tab) {
                    *selected = k;
                }
            }
        }
        if let Some(area) = self.area_of_shown_tab(tab) {
            self.extra_front.retain(|(a, _)| *a != area);
            if self.is_transient_tab(tab) {
                if let Some(editor) = [Editor::Part, Editor::Slice, Editor::Force].into_iter().find(|e| e.tab() == tab) {
                    self.front_extra(area, editor);
                }
            } else if let Some(tree) = &mut self.areas {
                if let Some(instance) = tree.instance_of_tab(tab) {
                    let op = WorkspaceOp::Select { area, instance };
                    if tree.apply(op).is_ok() { cx.action(ShellAction::EditAreas(op)); }
                }
            }
        }
        self.screen_stale = true;
        self.nubs(cx);
    }

    /// Dispatch the notch layer's actions in order. A batch with only these actions is fully
    /// handled here, so moving a corner sash never walks the editor and nub widgets a second time.
    fn nub_actions(&mut self, cx: &mut Cx, actions: &Actions) -> bool {
        let uid = self.view.widget(cx, ids!(area_nubs)).widget_uid();
        let mut handled = 0;
        for action in actions.filter_widget_actions_cast::<NubAction>(uid) {
            match action {
                NubAction::Drag(tab) => self.drag_area(cx, tab),
                NubAction::Corner { tab, corner, start, at, phase } => self.corner_action(cx, tab, corner, start, at, phase),
                NubAction::None => continue,
            }
            handled += 1;
        }
        handled > 0 && handled == actions.len()
    }

    /// The areas' own actions: the area menus, a nub pressed, the controls header
    /// pressed, a sash moved, a tab pressed.
    fn area_actions(&mut self, cx: &mut Cx, actions: &Actions, workspaces: Option<&Workspaces>) {
        if let Some(id) = menu_picked(actions, AREA_MENU) {
            self.area_menu_picked(cx, id, workspaces);
        }
        // a tab bar's new-tab menu: the editor picked opens as a new tab of that area
        if let Some(id) = menu_picked(actions, ADD_MENU) {
            if let (Some(area), Some(editor)) = (self.add_menu.take(), Editor::all().find(|e| e.tab() == id)) {
                cx.action(ShellAction::EditAreas(WorkspaceOp::AddTab { area, editor }));
            }
        }
        // a plate view's toolbar Tools toggle: its tool rails on or off, as its area menu's Tools
        if let Some(ws) = workspaces {
            let dock = self.view.dock(cx, ids!(main.dock));
            let plates = self.areas.as_ref().map(|tree| tree.instances()).unwrap_or_default();
            for plate in plates.into_iter().map(|(_, i)| i).filter(|i| i.editor == Editor::Viewport) {
                let body = dock.item(plate.tab());
                // (the plate's View options, or Mechanics' bar's toggle)
                let asked = actions.iter().any(|a| a.downcast_ref::<crate::ui::stage::ToolRailsAsked>().is_some_and(|t| t.0 == plate));
                if asked || body.radio_button(cx, ids!(mechanics_ui.mech_tools)).clicked(actions) {
                    cx.action(ShellAction::EditWorkspaces(ListOp::Region {
                        instance: plate, tools: Some(!ws.instance_state(plate).tools), left: None,
                    }));
                }
            }
        }
        let nubs = self.view.widget(cx, ids!(area_nubs));
        let pressed = nubs.borrow::<FabAreaNotches>().and_then(|n| n.pressed(cx, actions));
        if let Some((tab, chip)) = pressed {
            self.open_area_menu(cx, tab, chip, workspaces);
        }
        let added = nubs.borrow::<FabAreaNotches>().and_then(|n| n.add_pressed(cx, actions));
        if let Some((tab, button)) = added {
            self.open_add_menu(cx, tab, button);
        }
        // a toolbar's close button: its area's front tab closes, as the area menu's Close tab
        let closed = nubs.borrow::<FabAreaNotches>().and_then(|n| n.closed(cx, actions));
        if let Some(tab) = closed {
            if let Some(area) = self.area_of_shown_tab(tab) {
                if let Some(instance) = self.areas.as_ref().and_then(|tree| tree.instance(area)) {
                    cx.action(ShellAction::EditAreas(WorkspaceOp::CloseTab { area, instance }));
                }
            }
        }
        let strip = self.view.dock(cx, ids!(main.dock)).item(Editor::Transport.tab()).child(id!(controls)).widget_uid();
        if let Some(ControlsStripAction::Pressed) = actions.find_widget_action(strip).map(|a| a.cast()) {
            self.show_panel(cx, Editor::BoardControls);
        }
        for action in actions {
            match action.as_widget_action().cast() {
                DockAction::SplitPanelChanged { panel_id, align, .. } => {
                    // a sash dragged: the tree takes its place (the dock has it already)
                    let split = self.areas.as_ref().and_then(|tree| tree.split_of_live_id(panel_id));
                    if let (Some(split), Some(tree)) = (split, self.areas.as_mut()) {
                        if tree.apply(WorkspaceOp::Resize { split, align }).is_ok() {
                            if let Some(DockItem::Splitter { align: stored, .. }) = self.layout.get_mut(&panel_id) {
                                *stored = align;
                            }
                            cx.action(ShellAction::EditAreas(WorkspaceOp::Resize { split, align }));
                            // the nubs follow their areas' corners
                            self.view.widget(cx, ids!(area_nubs)).redraw(cx);
                        }
                    }
                }
                DockAction::TabWasPressed(tab) => self.tab_pressed(cx, tab),
                _ => {}
            }
        }
    }

    /// A tab's drag began: what it moves, the tab (a mode's editor as its first instance). At a
    /// phone's width the drag only carries its ghost: nothing takes its drop.
    fn drag_tab(&mut self, tab: LiveId) {
        if tab == Editor::Part.tab() && self.is_transient_tab(tab) { return; }
        let Some(tree) = &self.areas else { return };
        let instance = tree.instance_of_tab(tab).or_else(|| Editor::all().find(|e| e.tab() == tab).map(Instance::first));
        self.drag = instance.filter(|_| !self.compact).map(|i| AreaDrag::new(Moving::Tab(i), vec![i]));
    }

    /// A nub's drag began: the whole area moves, its front editor first on the chip. At a
    /// phone's width there is no other area on screen to drop it by: nothing moves.
    fn drag_area(&mut self, cx: &mut Cx, tab: LiveId) {
        if self.compact {
            return;
        }
        // the chip's press opened the area's menu: a drag moves the area instead
        if let Some(mut layer) = self.view.widget(cx, ids!(menu_layer)).borrow_mut::<crate::ui::menu::FabMenuLayer>() {
            layer.close(cx);
        }
        let Some(area) = self.area_of_shown_tab(tab) else { return };
        let Some((mut tabs, selected)) = self.areas.as_ref().and_then(|tree| tree.stack_of(area)) else { return };
        if selected < tabs.len() {
            let front = tabs.remove(selected);
            tabs.insert(0, front);
        }
        cx.start_dragging(vec![DragItem::FilePath { path: String::new(), internal_id: Some(tab) }]);
        cx.set_cursor(MouseCursor::Grabbing);
        self.drag = Some(AreaDrag::new(Moving::Area(area), tabs));
    }

    /// An independent layout change supersedes the original press geometry and its preview.
    fn reset_corner(&mut self, cx: &mut Cx) {
        self.corner = None;
        if let Some(mut nubs) = self.view.widget(cx, ids!(area_nubs)).borrow_mut::<FabAreaNotches>() {
            nubs.cancel_corner(cx);
        }
    }

    fn corner_action(&mut self, cx: &mut Cx, tab: LiveId, corner: Corner, start: DVec2, at: DVec2, phase: CornerPhase) {
        let Some(mut gesture) = self.corner.take() else {
            // A maximized leaf fills the dock even after its tree is split. There is no
            // visible sibling or sash in that presentation to preview or commit.
            if matches!(phase, CornerPhase::Move) && start == at && !self.compact && self.maximized.is_none() {
                let Some(area) = self.area_of_shown_tab(tab) else { return };
                let neighbours = self.area_rects(cx);
                let Some(&(_, rect)) = neighbours.iter().find(|(id, _)| *id == area) else { return };
                self.corner = Some(CornerGesture { area, rect, neighbours, preview: CornerPreview::None });
            }
            return;
        };
        match phase {
            CornerPhase::Move => {
                self.move_corner(cx, &mut gesture, corner, start, at);
                self.corner = Some(gesture);
            }
            CornerPhase::End => match gesture.preview {
                CornerPreview::Split { side, editor, align, .. } => {
                    cx.action(ShellAction::EditAreas(WorkspaceOp::Split { area: gesture.area, side, editor, align }));
                }
                CornerPreview::Join { neighbour } => {
                    if gesture.neighbours.iter().any(|(id, r)| *id == neighbour && r.contains(at)) {
                        cx.action(ShellAction::EditAreas(WorkspaceOp::Close { area: neighbour }));
                    } else {
                        self.relayout(cx);
                    }
                }
                CornerPreview::None => {}
            },
            CornerPhase::Cancel => {
                if !matches!(gesture.preview, CornerPreview::None) {
                    self.relayout(cx);
                }
            }
        }
    }

    /// Load a split preview once. Later moves set that sash in O(1). Join targeting costs
    /// O(areas) per move against the press geometry and loads a layout only when the target
    /// changes, including when the pointer leaves the sibling and the original layout returns.
    fn move_corner(&mut self, cx: &mut Cx, gesture: &mut CornerGesture, corner: Corner, start: DVec2, at: DVec2) {
        if let CornerPreview::Split { side, splitter, align, .. } = &mut gesture.preview {
            *align = corner_align(gesture.rect, *side, at);
            self.view.dock(cx, ids!(main.dock)).set_splitter_align(cx, *splitter, *align, false);
            return;
        }
        let Some(base) = self.areas.as_ref() else { return };
        let aim = corner_aim(base, gesture.area, gesture.rect, corner, start, at, &gesture.neighbours);
        if let CornerPreview::Join { neighbour } = gesture.preview {
            if matches!(aim, CornerAim::Join { neighbour: target } if target == neighbour) {
                return;
            }
            // Coming back inside withdraws the join. The next move may choose a split.
            gesture.preview = CornerPreview::None;
            self.relayout(cx);
            return;
        }
        let area = gesture.area;
        let op = match aim {
            CornerAim::Split { side, align } => {
                let editor = split_editor(base, base.instance(area).map(|i| i.editor));
                WorkspaceOp::Split { area, side, editor, align }
            }
            CornerAim::Join { neighbour } => WorkspaceOp::Close { area: neighbour },
            CornerAim::Undecided => return,
        };
        let mut tree = base.clone();
        if tree.apply(op).is_err() {
            return;
        }
        let mut map = self.present_now(&tree, &[]);
        gesture.preview = match op {
            WorkspaceOp::Split { side, editor, align, .. } => {
                let Some((parent, ..)) = tree.parent_split(area) else { return };
                let splitter = Self::split_dock_id(&tree, parent);
                let Some(DockItem::Splitter { align: shown, .. }) = map.get_mut(&splitter) else { return };
                // The narrow layout normally fits pinned columns to a third. This sash is
                // under the pointer, including on the first draw of the preview.
                *shown = align;
                CornerPreview::Split { side, editor, align, splitter }
            }
            WorkspaceOp::Close { area: neighbour } => CornerPreview::Join { neighbour },
            _ => return,
        };
        self.apply_layout(cx, map);
    }

    /// Where a drop at `pos` lands in the tree as it is (`base`), read off the layout shown now
    /// (`shown`, a preview when `preview`): its tab sets and splits are base's but for the drag's
    /// own ghost (`ghost`), and over the ghost the target stays.
    fn aim(&self, base: &AreaTree, shown: &AreaTree, preview: bool, ghost: &[Instance], pos: DropPosition) -> Aim {
        let root = live_id!(root);
        // the preview's ghost: a new area (or split) whose id may be one the drag left
        let ghostly = |node: AreaId| preview && shown.stack_of(node).is_some_and(|(tabs, _)| tabs.iter().any(|i| ghost.contains(i)));
        match pos.part() {
            DropPart::Tab => {
                let tab = pos.id();
                if tab == GHOST_TAB {
                    return Aim::Keep;
                }
                if let Some(area) = base.area_of_tab(tab) {
                    let k = base.stack_of(area).and_then(|(tabs, _)| tabs.iter().position(|i| i.tab() == tab)).unwrap_or(0);
                    return Aim::At(area, DropAt::Before(k));
                }
                // a mode's editor: into the stack it joined
                match self.area_of_shown_tab(tab).filter(|a| base.stack_of(*a).is_some()) {
                    Some(area) => Aim::At(area, DropAt::Center),
                    None => Aim::Keep,
                }
            }
            DropPart::Bar => {
                let split = shown.split_of_live_id(pos.id());
                if split.and_then(|s| shown.parts(s)).is_some_and(|(_, a, b)| ghostly(a) || ghostly(b)) {
                    return Aim::Keep;
                }
                let parts = split.and_then(|split| base.parts(split));
                match parts {
                    Some((axis, a, _)) => Aim::At(a, DropAt::Side(if axis == SplitterAxis::Horizontal { Side::Right } else { Side::Down })),
                    None => Aim::Keep,
                }
            }
            part => {
                let at = match part {
                    DropPart::Left => DropAt::Side(Side::Left),
                    DropPart::Right => DropAt::Side(Side::Right),
                    DropPart::Top => DropAt::Side(Side::Up),
                    DropPart::Bottom => DropAt::Side(Side::Down),
                    _ => DropAt::Center,
                };
                let node = if pos.id() == root {
                    // the dock's root: the area maximized, else the shown tree's top (the whole
                    // tree for its outer band)
                    self.maximized.unwrap_or(shown.id())
                } else {
                    match shown.area_of_live_id(pos.id()) {
                        Some(area) => area,
                        None => return Aim::Keep,
                    }
                };
                if ghostly(node) {
                    return Aim::Keep;
                }
                let node = if base.contains(node) { node } else if pos.id() == root { base.id() } else { return Aim::Keep };
                if at == DropAt::Center && base.stack_of(node).is_none() {
                    return Aim::Keep;
                }
                Aim::At(node, at)
            }
        }
    }

    /// The pointer of a drag moved over the dock: where its drop would land, and when that
    /// changed, the layout as if it were dropped there, a ghost in its room. Per drag event an
    /// O(tab sets + splits) drop query and O(nodes) mapping; a tree edit and a dock load only
    /// when the target changes. The query waits until the last preview was drawn (its rects).
    fn drag_over(&mut self, cx: &mut Cx, abs: DVec2) {
        let Some(drag) = &self.drag else { return };
        if !drag.drawn {
            return;
        }
        let Some(base) = self.areas.clone() else { return };
        let Some(pos) = self.view.dock(cx, ids!(main.dock)).drop_position(cx, abs) else { return };
        let shown = drag.shown.as_ref().unwrap_or(&base);
        let target = match self.aim(&base, shown, drag.shown.is_some(), &drag.ghost, pos) {
            Aim::Keep => return,
            Aim::At(to, at) => Some((to, at)),
        };
        if target == drag.target {
            return;
        }
        let what = drag.what;
        let preview = target.and_then(|(to, at)| {
            let mut tree = base.clone();
            tree.apply(WorkspaceOp::Move { what, to, at }).ok().map(|_| tree)
        });
        let ghost = drag.ghost.clone();
        let was_shown = drag.shown.is_some();
        let map = match &preview {
            Some(tree) => Some(self.present_now(tree, &ghost)),
            // a drop there changes nothing: the areas as they are, loaded again only when a
            // preview shows
            None => was_shown.then(|| self.present_now(&base, &[])),
        };
        if let Some(drag) = &mut self.drag {
            drag.target = target;
            drag.shown = preview;
            drag.drawn = map.is_none();
        }
        if let Some(map) = map {
            self.apply_layout(cx, map);
        }
    }

    /// The drag left the dock: the areas as they are, nothing aimed at.
    fn drag_off(&mut self, cx: &mut Cx) {
        let Some(drag) = &mut self.drag else { return };
        if drag.target.take().is_none() {
            return;
        }
        if drag.shown.take().is_some() {
            drag.drawn = false;
            self.relayout(cx);
        }
    }

    /// The drag dropped on the dock: the one `Move` the preview showed, through the state (the
    /// shown layout is what it makes; the state lays it out again at its next draw). Nothing
    /// aimed at: the areas as they were.
    fn drop_areas(&mut self, cx: &mut Cx) {
        let Some(drag) = self.drag.take() else { return };
        match (drag.target, drag.shown.is_some()) {
            (Some((to, at)), true) => cx.action(ShellAction::EditAreas(WorkspaceOp::Move { what: drag.what, to, at })),
            (_, shown) => {
                if shown {
                    self.relayout(cx);
                }
            }
        }
    }

    /// What an outside edit asks of the screen, wherever it shows: Follow AI, the Firmware's
    /// washed lines, the areas' outlines.
    fn outside_edits(&mut self, cx: &mut Cx, state: Option<&AppState>, actions: &Actions) {
        for edit in actions.iter().filter_map(|a| a.downcast_ref::<ElecOutsideEdit>()) {
            if let Some(state) = state {
                self.follow_ai(cx, state, edit);
            }
            self.flash_firmware(cx, edit);
            self.outline_edited(cx, edit);
        }
    }

    /// Bring a panel forward: its tab in front where the dock shows it, else the areas change
    /// to show it (a [`WorkspaceOp::Reveal`] through the state, as every area edit goes).
    /// At a phone's width the area holding it becomes the one in use, or, held by none, the area
    /// its Reveal puts it in.
    pub fn show_panel(&mut self, cx: &mut Cx, panel: Editor) {
        if panel == Editor::Part && !self.part_tab_shown {
            if self.part_side_offered { cx.action(StageAction::PartSidebar(true)); }
            return;
        }
        if self.set_of_tab(panel.tab()).is_some() {
            self.view.dock(cx, ids!(main.dock)).select_tab(cx, panel.tab());
            self.screen_stale = true;
            self.tab_pressed(cx, panel.tab());
            return;
        }
        if self.compact {
            let held = self.areas.as_ref().and_then(|tree| tree.instances().into_iter().find(|(_, i)| i.editor == panel).map(|(area, i)| (area, i, tree.instance(area) == Some(i))));
            if let (Some((area, instance, front)), Some((ws, _))) = (held, self.shown) {
                self.pressed_area = Some((ws, area));
                if front {
                    self.relayout(cx);
                } else {
                    // the tree's reload shows it, in front of the area now in use
                    cx.action(ShellAction::EditAreas(WorkspaceOp::Select { area, instance }));
                }
                return;
            }
            self.follow = Some(panel);
        }
        let from = self.pressed_area.filter(|(ws, _)| self.shown.is_some_and(|(shown, _)| shown == *ws)).map(|(_, area)| area);
        cx.action(ShellAction::EditAreas(WorkspaceOp::Reveal { editor: panel, from }));
    }

    /// Keeps the area a press lands in (a tab's, or the body's under it), for a Reveal the press
    /// leads to. Per press: the dock's drop query, O(tab sets + splits), and O(nodes) mapping.
    fn note_press(&mut self, cx: &Cx, abs: DVec2) {
        let (Some((ws, _)), Some(tree)) = (self.shown, self.areas.as_ref()) else { return };
        let Some(pos) = self.view.dock(cx, ids!(main.dock)).drop_position(cx, abs) else { return };
        let area = match pos.part() {
            DropPart::Tab => tree.area_of_tab(pos.id()),
            DropPart::Bar => None,
            _ => self.area_of_set(pos.id()),
        };
        self.pressed_area = area.map(|area| (ws, area));
    }

    /// The dock's own actions: a tab's drag starts (the areas take its drop, `handle_event`), a
    /// file from outside dragged over or dropped (it opens).
    fn dock_actions(&mut self, cx: &mut Cx, actions: &Actions) {
        let dock = self.view.dock(cx, ids!(main.dock));
        for action in actions {
            let action = action.as_widget_action().cast();
            // a tab forward or a drop: what shows may have changed
            self.screen_stale |= matches!(action, DockAction::TabWasPressed(_) | DockAction::Drop(_) | DockAction::TabCloseWasPressed(_));
            match action {
                DockAction::ShouldTabStartDrag(tab_id) => {
                    self.drag_tab(tab_id);
                    dock.tab_start_drag(cx, tab_id, DragItem::FilePath { path: String::new(), internal_id: Some(tab_id) });
                }
                // The dock is under every panel, so it sees a drag first: a file from outside
                // opens (the areas answer their own drags in `handle_event`).
                DockAction::Drag(drag) => {
                    if has_outside_file(&drag.items) {
                        *drag.response.lock().unwrap() = DragResponse::Copy;
                    }
                }
                DockAction::Drop(drop) => {
                    if !is_internal(&drop.items) {
                        open_dropped_files(cx, &drop.items);
                    }
                }
                _ => {}
            }
        }
    }

    /// A drag of the areas' own over the window: the preview follows it over the dock, leaving
    /// the dock puts the areas back, its drop applies its move, its end without one (Escape)
    /// puts them back.
    fn area_drag_event(&mut self, cx: &mut Cx, event: &Event) {
        match event {
            Event::Drag(e) if self.drag.is_some() && is_internal(&e.items) => {
                if let Some(drag) = &mut self.drag {
                    drag.at = Some(e.abs);
                }
                self.redraw_chip(cx);
                cx.set_cursor(MouseCursor::Grabbing);
                if self.view.dock(cx, ids!(main.dock)).area().rect(cx).contains(e.abs) {
                    *e.response.lock().unwrap() = DragResponse::Move;
                    self.drag_over(cx, e.abs);
                } else {
                    self.drag_off(cx);
                }
            }
            Event::Drop(e) if self.drag.is_some() && is_internal(&e.items) => {
                self.drop_areas(cx);
                self.redraw_chip(cx);
            }
            Event::DragEnd => {
                if let Some(drag) = self.drag.take() {
                    if drag.shown.is_some() {
                        self.relayout(cx);
                    }
                    self.redraw_chip(cx);
                }
            }
            _ => {}
        }
    }
}

/// The editor an electronics view names.
fn elec_editor(e: ElecEditor) -> Editor {
    match e {
        ElecEditor::Schematic => Editor::Circuit,
        ElecEditor::Board => Editor::Board,
        ElecEditor::Firmware => Editor::Firmware,
        ElecEditor::Bench => Editor::Bench,
    }
}

/// What a split beside `front` shows: in electronics the first of its editors not on screen;
/// elsewhere the first editor of `front`'s group no area holds, else the first of any group
/// (but Experimental's), else another Scope.
fn split_editor(tree: &AreaTree, front: Option<Editor>) -> Editor {
    if front.is_some_and(|e| e.family() == Some(Family::Electronics)) {
        return layouts::next_editor(tree);
    }
    let free = |e: &&Editor| tree.showing(**e).is_none();
    let group = MENU.iter().find(|(_, editors)| front.is_some_and(|f| editors.contains(&f)));
    group.and_then(|(_, editors)| editors.iter().find(free))
        .or_else(|| MENU[..MENU.len() - 1].iter().flat_map(|(_, editors)| editors.iter()).find(free))
        .copied()
        .unwrap_or(Editor::Scope)
}

/// A drag the app started itself (a tab, a nub): its items carry an internal id.
fn is_internal(items: &[DragItem]) -> bool {
    matches!(items.first(), Some(DragItem::FilePath { internal_id: Some(_), .. }))
}

/// A file from outside the app (dock tab drags carry no path).
fn has_outside_file(items: &[DragItem]) -> bool {
    items.iter().any(|i| matches!(i, DragItem::FilePath { path, internal_id: None } if !path.is_empty()))
}

/// The files from outside the app a drag carries.
fn outside_files(items: &[DragItem]) -> Vec<std::path::PathBuf> {
    items.iter().filter_map(|i| match i {
        DragItem::FilePath { path, internal_id: None } if !path.is_empty() => Some(std::path::PathBuf::from(path)),
        _ => None,
    }).collect()
}

fn open_dropped_files(cx: &mut Cx, items: &[DragItem]) {
    let paths = outside_files(items);
    if !paths.is_empty() {
        cx.action(ShellAction::DropFiles(paths));
    }
}

/// Raise a panel from anywhere (menus, the header, the AI): the shell docks it.
#[derive(Clone, Copy, Debug)]
pub struct ShowPanel(pub Editor);

#[derive(Clone, Copy, Debug)]
pub struct RevealProposal(pub ProposalId);

/// Bring a panel of a workspace forward once that workspace's layout shows: at once when it shows
/// now, else right after the switch (which loads its own layout first). The newest wins.
#[derive(Clone, Copy, Debug)]
pub struct ShowPanelIn(pub Workspace, pub Editor);

/// The AI pointed a Scope at a net (`fab.workspace_edit`'s `focus`): the net's lane in the Scope
/// whose dock tab is `tab`, revealed and lit (the focus brought the tab forward already).
#[derive(Clone, Debug)]
pub struct ShowLane {
    pub tab: LiveId,
    pub net: String,
}

/// The editors an AI layout change pointed at: their areas take the fading AI outline
/// ([`FabAreaNotches::light`]), in the order named.
#[derive(Clone, Debug)]
pub struct LightEditors(pub Vec<Editor>);

impl Widget for FabShell {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        if matches!(event, Event::Signal) { self.sync_review(cx, false); }
        if let Event::Actions(actions) = event {
            if self.nub_actions(cx, actions) {
                return;
            }
        }
        if let Event::MouseDown(e) = event {
            self.note_press(cx, e.abs);
        }
        self.view.handle_event(cx, event, scope);

        if let Event::Actions(actions) = event {
            // the temporary layout's strip: Keep moves it into the row, Back drops it
            if self.view.button(cx, ids!(temp_strip.keep)).clicked(actions) {
                cx.action(ShellAction::EditWorkspaces(ListOp::Keep(None)));
            }
            if self.view.button(cx, ids!(temp_strip.back)).clicked(actions) {
                cx.action(ShellAction::EditWorkspaces(ListOp::Dismiss));
            }
            self.dock_actions(cx, actions);
            for choice in actions.iter().filter_map(|a| a.downcast_ref::<fab_elec_view::live::transport::BenchChosen>()) {
                crate::view_prefs::save_bench(&choice.project, &choice.name);
            }
            let state = scope.data.get::<AppState>();
            let workspaces = state.map(|s| &s.ui.workspaces);
            self.area_actions(cx, actions, workspaces);
            self.outside_edits(cx, state, actions);
            for a in actions.iter().filter_map(|a| a.downcast_ref::<ShowPanel>()) {
                self.show_panel(cx, a.0);
            }
            for reveal in actions.iter().filter_map(|a| a.downcast_ref::<RevealProposal>()) {
                let area = self.view.view(cx, ids!(main.review_area));
                let widget = area.widget(cx, ids!(review));
                let y = widget.borrow_mut::<ReviewStrip>().and_then(|mut strip| strip.reveal(cx, reveal.0));
                if let Some(y) = y { area.set_scroll_pos(cx, dvec2(0.0, y)); }
            }
            // an electronics view's ask for one of its editors
            for a in actions.iter().filter_map(|a| a.downcast_ref::<fab_elec_view::ElecPanel>()) {
                self.show_panel(cx, match a {
                    fab_elec_view::ElecPanel::Library => Editor::Library,
                    fab_elec_view::ElecPanel::Checks => Editor::Checks,
                    fab_elec_view::ElecPanel::BoardControls => Editor::BoardControls,
                });
                // Place part: the Library's search takes the keyboard
                self.library_search |= *a == fab_elec_view::ElecPanel::Library;
            }
            for &ShowPanelIn(ws, panel) in actions.iter().filter_map(|a| a.downcast_ref::<ShowPanelIn>()) {
                if self.kind == Some(ws) {
                    self.panel_in = None;
                    self.show_panel(cx, panel);
                } else {
                    self.panel_in = Some((ws, panel));
                }
            }
            if self.shows_elec() {
                self.circuit_strip_actions(cx, actions);
                self.library_prefs(cx, actions, workspaces);
                self.power_flyout(cx, actions);
                self.board_flyouts(cx, actions);
            }
            // A probe's card, or a wire clicked, on the sheet or the board shows its lane in the Scope.
            for a in actions.iter().filter_map(|a| a.downcast_ref::<fab_elec_view::live::flags::OpenInScope>()) {
                self.open_in_scope(cx, Editor::Scope.tab(), &a.0, false);
            }
            for a in actions.iter().filter_map(|a| a.downcast_ref::<fab_elec_view::instruments::logic_view::ShowInScope>()) {
                self.open_in_scope(cx, Editor::Scope.tab(), &a.0, true);
            }
            // The AI's `focus` on a Scope: its lane of the net in that Scope.
            for a in actions.iter().filter_map(|a| a.downcast_ref::<ShowLane>()) {
                self.open_in_scope(cx, a.tab, &a.net, false);
            }
            // The AI's layout change: the areas it pointed at take the fading AI outline.
            for a in actions.iter().filter_map(|a| a.downcast_ref::<LightEditors>()) {
                let kinds: Vec<LiveId> = a.0.iter().map(|editor| editor.kind()).collect();
                if let Some(mut nubs) = self.view.widget(cx, ids!(area_nubs)).borrow_mut::<FabAreaNotches>() {
                    nubs.light(cx, &kinds, true);
                }
            }
            // A part card's 3D model rows ask for the External Resources and Licences page.
            if actions.iter().any(|a| a.downcast_ref::<fab_elec_view::explorer::view::ShowModelSources>().is_some()) {
                cx.action(ShellAction::ShowModelSources(Some(crate::api::SourcesPage::Licences)));
            }
            if actions.iter().any(|a| matches!(a.downcast_ref::<ShellAction>(), Some(ShellAction::ShowPlates))) {
                self.show_panel(cx, Editor::Plates);
            }
            // An electronics view's focus, plain or from its widget: the board pane answers it.
            for a in actions.iter() {
                let view_action = a.downcast_ref::<fab_elec_view::ViewAction>()
                    .or_else(|| a.as_widget_action().and_then(|w| w.action.downcast_ref::<fab_elec_view::ViewAction>()));
                match view_action {
                    Some(fab_elec_view::ViewAction::Focus(focus)) => {
                        self.answer_board_focus(cx, focus);
                        self.scope_follows(cx, Some(&focus.sel));
                    }
                    Some(fab_elec_view::ViewAction::Unfocus) => {
                        self.answer_board_unfocus(cx);
                        self.scope_follows(cx, None);
                    }
                    _ => {}
                }
                if let Some(group) = a.as_widget_action().and_then(|w| w.action.downcast_ref::<fab_elec_view::ViewState>()) {
                    self.answer_board_group(cx, &group.selected);
                }
            }
        }

        if let Some(state) = scope.data.get_mut::<AppState>() {
            self.bind_plates(cx, state);
            self.publish_screen(cx, state);
            // a sash moved: the file keeps it once the hand lets go
            let let_go = match event {
                Event::MouseUp(_) | Event::KeyUp(_) => true,
                Event::TouchUpdate(e) => e.touches.iter().all(|t| matches!(t.state, makepad_widgets_core::event::TouchState::Stop | makepad_widgets_core::event::TouchState::Cancel)),
                _ => false,
            };
            if state.ui.workspaces.unsaved && let_go {
                state.ui.workspaces.save();
            }
        }
        self.area_drag_event(cx, event);
        self.follow_file_drag(cx, event);
        // Files dropped anywhere on the window open: over the panels through the dock
        // (`dock_actions`), elsewhere (the header) here.
        match event.drag_hits(cx, self.view.area()) {
            DragHit::Drag(drag) => {
                if has_outside_file(&drag.items) {
                    *drag.response.lock().unwrap() = DragResponse::Copy;
                }
            }
            DragHit::Drop(drop) => open_dropped_files(cx, &drop.items),
            _ => {}
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let mut want_ws: Option<WorkspaceDef> = None;
        let mut relay = false;
        if let Some(state) = scope.data.get_mut::<AppState>() {
            if self.cameras_frame != Some(cx.drawing_id()) {
                state.view.settle_cameras();
                self.cameras_frame = Some(cx.drawing_id());
            }
            let current = state.ui.workspaces.current();
            if self.shown.is_none_or(|(id, _)| id != current.id) { self.extra_front.clear(); }
            // the modes' editors come and go beside Parts and Slicer
            let slice = state.print.play.active;
            if self.slice_shown != slice {
                self.slice_shown = slice;
                self.mode_changed(&current.tree, Editor::Parts, Editor::Slice, slice);
                relay = true;
            }
            let force = state.force_shown();
            if self.force_shown != force {
                self.force_shown = force;
                self.mode_changed(&current.tree, Editor::Slicer, Editor::Force, force);
                relay = true;
            }
            self.experimental = (state.ui.arm_shown, state.ui.scan_shown);
            // the temporary layout's strip comes, goes or changes its words: O(1) per draw
            let temp = state.ui.workspaces.temp.as_ref().filter(|t| t.def.id == state.ui.workspaces.current).map(|t| match t.by {
                ShownBy::Ai => (format!("Shown by the AI: {}", t.def.name), true),
                ShownBy::Person => (format!("Temporary layout: {}", t.def.name), false),
            });
            if self.temp_strip != temp {
                self.sync_temp_strip(cx, temp);
            }
            // the Plate's header strip (Mechanics' controls) comes or goes: its nub's look
            let strip = state.ui.in_mode(PlateMode::Mechanics);
            if self.plate_strip != strip {
                self.plate_strip = strip;
                self.nubs(cx);
            }
            // a page over the workspace comes or goes (Back): the dock laid out again, bodies kept
            if self.page != state.ui.page {
                self.page = state.ui.page;
                relay = true;
            }
            if self.shown != Some((current.id, current.shape)) {
                want_ws = Some(current.clone());
            }
        }
        let width = cx.peek_walk_turtle(walk).size.x;
        if width > 0.0 {
            self.width = width;
            // a phone's width: the area in use alone
            let compact = width < COMPACT;
            let narrow = width < NARROW;
            if compact != self.compact || narrow != self.narrow {
                self.compact = compact;
                self.narrow = narrow;
                relay = true;
            }
        }
        if let Some(state) = scope.data.get_mut::<AppState>() {
            if relay || want_ws.is_some() {
                self.update_part_placement(state, want_ws.as_ref());
            } else {
                self.sync_part_placement(cx, state);
            }
        }
        if let Some(def) = want_ws {
            self.show_workspace(cx, &def);
            let kind = def.kind();
            if let Some((_, panel)) = self.panel_in.take_if(|(asked, _)| *asked == kind) {
                self.show_panel(cx, panel);
            }
        } else if relay {
            self.drag = None;
            self.reset_corner(cx);
            self.relayout(cx);
        } else if self.narrow && self.width != self.narrow_fit {
            // resized while narrow: the pinned columns keep their third, no load
            self.narrow_fit = self.width;
            if let Some(tree) = &self.areas {
                let dock = self.view.dock(cx, ids!(main.dock));
                if self.maximized.is_none() {
                    for (split, align) in tree.narrow_sizes(self.width) {
                        dock.set_splitter_align(cx, Self::split_dock_id(tree, split), align, false);
                    }
                }
            }
        }
        if let Some(drag) = &mut self.drag {
            drag.drawn = true;
        }
        if let Some(state) = scope.data.get_mut::<AppState>() {
            self.bind_plates(cx, state);
            self.sync_regions(cx, &state.ui.workspaces);
            self.publish_screen(cx, state);
            self.fit_library(cx, state);
        }
        // Fitting the recorder can send a persistent Resize. Its measurements must come from
        // the workspace's own geometry once the corner gesture has settled.
        if self.corner.is_none() {
            self.fit_transport(cx);
        }
        self.sync_review(cx, true);
        let step = self.view.draw_walk(cx, scope, walk);
        if step.is_step() {
            return step;
        }
        if std::mem::take(&mut self.library_search) {
            let library = self.view.dock(cx, ids!(main.dock)).item(Editor::Library.tab());
            library.widget(cx, ids!(library.search)).set_key_focus(cx);
        }
        let chip = scope.data.get::<AppState>().and_then(|s| s.ui.drop_chip.clone());
        self.draw_drop_chip(cx, chip.as_ref());
        DrawStep::done()
    }
}

impl FabShell {
    /// The Schematic's Library float shows only while no Library editor is on screen (it is the
    /// Schematic's quick-open; the editor holds the same library). Before each draw: O(1).
    fn fit_library(&mut self, cx: &mut Cx, state: &AppState) {
        let circuit = self.view.dock(cx, ids!(main.dock)).item(Editor::Circuit.tab());
        if let Some(mut view) = circuit.borrow_mut::<ExplorerView>() {
            view.set_library_hidden(cx, state.ui.on_screen(Editor::Library));
        };
    }

    /// Tell the state which editors the dock shows ([`UiState::screen`]) and which Plate views
    /// it holds hidden ([`UiState::plates_hidden`]; they let go of the scene host), once after each change;
    /// a change redraws the dock, for a panel may offer what is not on screen (the Part panel's
    /// Parameters). O(resident bodies) per change, nothing otherwise.
    fn publish_screen(&mut self, cx: &mut Cx, state: &mut AppState) {
        if !std::mem::take(&mut self.screen_stale) {
            return;
        }
        let dock = self.view.dock(cx, ids!(main.dock));
        let (screen, plates_hidden, plates_shown) = {
            let Some(mut dock) = dock.borrow_mut() else { return };
            let shown: Vec<LiveId> = dock.visible_items().map(|(id, _)| id).collect();
            let items = dock.items();
            let plate = Editor::Viewport.kind();
            let hidden: Vec<LiveId> = items.iter().filter(|(id, (kind, _))| *kind == plate && !shown.contains(id)).map(|(id, _)| *id).collect();
            let plates: Vec<WidgetRef> = shown.iter().filter_map(|id| items.get(id).filter(|(kind, _)| *kind == plate)).map(|(_, body)| body.clone()).collect();
            (Editor::screen_bits(shown.iter().filter_map(|id| items.get(id).map(|(kind, _)| *kind))), hidden, plates)
        };
        // the Plate views on screen by instance number: the first leads ([`ViewportState::lead_plate`])
        let mut shown: Vec<(u32, PaneId)> = plates_shown.iter()
            .filter_map(|body| body.borrow::<crate::ui::stage::FabStage>().and_then(|stage| stage.bound_plate()))
            .map(|(i, pane)| (i.n, pane)).collect();
        shown.sort_unstable_by_key(|(n, _)| *n);
        let shown: Vec<PaneId> = shown.into_iter().map(|(_, pane)| pane).collect();
        if shown != state.view.shown_plates {
            // the lead's one-per-window chrome moves with it
            state.view.shown_plates = shown;
            for body in &plates_shown {
                body.redraw(cx);
            }
        }
        if plates_hidden != state.ui.plates_hidden {
            // A Plate the dock stops showing lets go of the scene host: the Plate on screen takes
            // it at its next draw, and with it the studio and environment work every pane's
            // picture shares. It keeps its tiles and passes for its return, and draws what
            // changed meanwhile when it is shown again.
            let newly: Vec<LiveId> = plates_hidden.iter().filter(|tab| !state.ui.plates_hidden.contains(tab)).copied().collect();
            let again: Vec<LiveId> = state.ui.plates_hidden.iter().filter(|tab| !plates_hidden.contains(tab)).copied().collect();
            state.ui.plates_hidden = plates_hidden;
            for (tab, hidden) in newly.into_iter().map(|tab| (tab, true)).chain(again.into_iter().map(|tab| (tab, false))) {
                let body = dock.borrow_mut().and_then(|mut dock| dock.items().get(&tab).map(|(_, body)| body.clone()));
                if let Some(body) = body {
                    if let Some(mut stage) = body.borrow_mut::<crate::ui::stage::FabStage>() {
                        if hidden { stage.hidden(state) } else { stage.shown_again(cx) }
                    }
                }
            }
        }
        if screen != state.ui.screen {
            // A Product view the dock stops showing (its tab behind another) parks, as one a
            // layout leaves off screen does: an unparked one would keep the shared layers (the
            // traced field parts) on the device's poses, so a part dragged on the plate would show
            // where it was until the drop.
            let device = 1u64 << Editor::Device as u64;
            if state.ui.screen & device != 0 && screen & device == 0 {
                for &(panel, park) in ELECTRONICS_PARK {
                    let body = dock.borrow_mut().and_then(|mut dock| dock.items().get(&panel.tab()).map(|(_, body)| body.clone()));
                    if let Some(body) = body { park(&body, cx); }
                }
            }
            state.ui.screen = screen;
            dock.redraw(cx);
        }
    }

    /// Files from outside over the window, wherever it is (the dock and the header both take
    /// their drop): the app hears of each new set and of the end, the chip follows the pointer.
    fn follow_file_drag(&mut self, cx: &mut Cx, event: &Event) {
        match event {
            Event::Drag(drag) => {
                let paths = outside_files(&drag.items);
                if paths.is_empty() {
                    return;
                }
                if self.file_drag.as_ref().is_none_or(|(shown, _)| *shown != paths) {
                    cx.action(ShellAction::DragFiles(paths.clone()));
                }
                self.file_drag = Some((paths, drag.abs));
                self.redraw_chip(cx);
            }
            Event::DragEnd if self.file_drag.take().is_some() => {
                cx.action(ShellAction::DragFilesEnd);
                self.redraw_chip(cx);
            }
            _ => {}
        }
    }

    /// The chip's overlay draws again (also once the drag ends, to erase it).
    fn redraw_chip(&mut self, cx: &mut Cx) {
        self.view.redraw(cx);
        if let Some(list) = &self.chip_list {
            list.redraw(cx);
        }
    }

    /// The chip at the pointer of a file drag: what the drop does, above everything.
    fn draw_drop_chip(&mut self, cx: &mut Cx2d, chip: Option<&DropChip>) {
        // a file's drop, else a tab's or an area's drag: what it carries
        let area_chip = self.drag.as_ref().and_then(|drag| Some((drag.at?, drag.chip())));
        let shown = self.file_drag.as_ref().map(|(_, at)| *at).zip(chip.cloned()).or(area_chip);
        if shown.is_none() && self.chip_list.is_none() {
            return;
        }
        let list = self.chip_list.get_or_insert_with(|| DrawList2d::new(cx));
        list.begin_overlay_last(cx);
        let pass = cx.current_pass_size();
        cx.begin_root_turtle(pass, Default::default());
        if let Some((at, chip)) = &shown {
            let at = *at;
            self.draw_chip_text.color = if chip.warn { self.color_chip_warn } else { self.color_chip_text };
            let tw = self.draw_chip_text.prepare_single_line_run(cx, &chip.text).map_or(120.0, |r| r.width_in_lpxs as f64);
            let size = dvec2(tw + 16.0, 24.0);
            // beside the pointer, below and right of it, kept inside the window
            let pos = dvec2((at.x + 16.0).min(pass.x - size.x - 4.0).max(4.0), (at.y + 18.0).min(pass.y - size.y - 4.0).max(4.0));
            self.draw_chip.draw_abs(cx, Rect { pos, size });
            self.draw_chip_text.draw_abs(cx, pos + dvec2(8.0, 6.0), &chip.text);
        }
        cx.end_pass_sized_turtle();
        list.end(cx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspaces::{ListOp, Origin, ShownBy, Workspaces};

    /// A shell on the first electronics workspace (Schematic), with its families and panels
    /// registered as the app does, the state it shows, and the script errors that registration and
    /// the workspace raised.
    fn electronics_shell() -> (Cx, FabShell, AppState, Vec<String>) {
        let mut cx = Cx::new(Box::new(|_, _| {}));
        let mut shell = cx.with_vm(|vm| {
            // The families the app registers (the umbrella's order, fab first): the shell's
            // widgets build on the fab family's controls (FabLabel, FabColorWheel).
            makepad_widgets_core::script_mod_with(vm, WindowFamilies::default(), &[
                makepad_widgets_fab::fab_mod,
                makepad_widgets_pickers::pickers_mod,
                makepad_widgets_data::dock_mod,
                makepad_widgets_data::data_mod,
                makepad_widgets_extras::glass_mod,
                makepad_widgets_extras::color_mod,
                makepad_widgets_extras::charts_mod,
                // The Firmware pane's Monitor lines are a LogList.
                makepad_widgets_rich_text::rich_text_mod,
            ]);
            // What the shell's own registration and its panels raise from here on (a draw shader
            // that fails to compile is noted apart from the script errors).
            let _ = vm.take_errors();
            let _ = makepad_widgets_core::makepad_platform::shader_error::take();
            vm.capture_errors();
            crate::script_mod(vm);
            FabShell::script_new_with_default(vm)
        });
        let mut state = AppState::default();
        state.ui.enter(Workspace::Electronics);
        shell.sync_with(&mut cx, &mut state);
        let mut errors = cx.with_vm(|vm| vm.take_errors());
        errors.extend(makepad_widgets_core::makepad_platform::shader_error::take());
        (cx, shell, state, errors)
    }

    /// What `f` asks for applied to the state as the app applies it, and the shell laid out again
    /// from the state (as its next draw would).
    fn run(cx: &mut Cx, shell: &mut FabShell, state: &mut AppState, f: impl FnOnce(&mut Cx, &mut FabShell, &mut AppState)) {
        let actions = cx.capture_actions(|cx| f(cx, shell, state));
        for action in shell_actions(&actions) {
            state.apply_core(action);
        }
        shell.sync_with(cx, state);
    }

    /// `actions` through the shell's event pass, then [`run`]'s way.
    fn act(cx: &mut Cx, shell: &mut FabShell, state: &mut AppState, actions: Vec<Box<dyn ActionTrait>>) {
        run(cx, shell, state, |cx, shell, state| shell.handle_event(cx, &Event::Actions(actions), &mut Scope::with_data(state)));
    }

    /// The default workspace `origin` started as.
    fn of_origin(state: &AppState, origin: Origin) -> WorkspaceId {
        state.ui.workspaces.list.iter().find(|d| d.origin == Some(origin)).map(|d| d.id).expect("the default is in the row")
    }

    fn show(cx: &mut Cx, shell: &mut FabShell, state: &mut AppState, origin: Origin) {
        let id = of_origin(state, origin);
        assert!(state.apply_core(&ShellAction::SwitchWorkspace(id)) || state.ui.workspaces.current == id);
        shell.sync_with(cx, state);
    }

    /// The dock and the shell must agree even on tab order, selection and hidden bars.
    fn check_dock(cx: &mut Cx, shell: &FabShell) {
        use makepad_widgets_core::makepad_micro_serde::SerRon;
        let dock = shell.view.dock(cx, ids!(main.dock)).clone_state().unwrap();
        assert_eq!(dock.len(), shell.layout.len());
        for (id, item) in &shell.layout {
            assert_eq!(dock[id].serialize_ron(), item.serialize_ron(), "dock item {id}");
        }
    }

    fn front_of(shell: &FabShell, editor: Editor) -> LiveId {
        let set = shell.set_of_tab(editor.tab()).expect("editor has a tab set");
        let DockItem::Tabs { tabs, selected, .. } = &shell.layout[&set] else { unreachable!() };
        tabs[*selected]
    }

    #[test]
    fn part_editor_switches_between_sidebar_and_tab_with_hysteresis_and_exclusion() {
        let (mut cx, mut shell, mut state, _) = electronics_shell();
        show(&mut cx, &mut shell, &mut state, Origin::Design);
        let saved = state.ui.workspaces.to_ron();
        for (width, offered, was_shown, front) in [
            (1594.0, false, false, Editor::Parts), (1654.0, true, false, Editor::Parts),
            (1644.0, true, false, Editor::Parts), (1634.0, false, false, Editor::Parts),
            (1654.0, true, false, Editor::Parts), (1634.0, false, true, Editor::Part),
        ] {
            shell.width = width;
            state.ui.part_side_shown = was_shown;
            shell.sync_part_placement(&mut cx, &mut state);
            assert_eq!(state.ui.part_side_offered, offered, "shell width {width}");
            assert_eq!(shell.layout.contains_key(&Editor::Part.tab()), !offered);
            assert_eq!(front_of(&shell, Editor::Parts), front.tab());
            if !offered {
                let set = shell.set_of_tab(Editor::Parts.tab()).unwrap();
                assert!(matches!(&shell.layout[&set], DockItem::Tabs { tabs, .. }
                    if tabs == &[Editor::Plates.tab(), Editor::Parts.tab(), Editor::Part.tab()]));
                assert!(!state.ui.part_side_shown);
            }
            check_dock(&mut cx, &shell);
            assert_eq!(state.ui.workspaces.to_ron(), saved, "placement never edits the saved layout");
            assert!(!saved.split(|c: char| !c.is_alphanumeric()).any(|word| word == "Part"));
            assert!(!state.ui.workspaces.unsaved);
        }
    }

    #[test]
    fn part_tab_fallback_covers_test_prints_compact_and_custom_layout_without_parts() {
        let (mut cx, mut shell, mut state, _) = electronics_shell();
        show(&mut cx, &mut shell, &mut state, Origin::Design);
        shell.width = 1920.0;
        for tests in [false, true, false] {
            state.print.test_mode = tests;
            shell.sync_part_placement(&mut cx, &mut state);
            assert_eq!(state.ui.part_side_offered, !tests);
            assert_eq!(shell.layout.contains_key(&Editor::Part.tab()), tests);
            if tests { assert_eq!(shell.part_host, shell.area_of_shown_tab(Editor::Parts.tab())); }
            check_dock(&mut cx, &shell);
        }
        shell.width = 560.0;
        shell.narrow = true;
        for (tree, host) in [
            (AreaTree::beside(AreaTree::area(Editor::Viewport), AreaTree::area(Editor::Slicer), 0.5), Editor::Slicer),
            (AreaTree::area(Editor::Viewport), Editor::Viewport),
        ] {
            let mut def = state.ui.workspaces.current().clone();
            def.tree = tree.numbered();
            shell.update_part_placement(&mut state, Some(&def));
            shell.show_workspace(&mut cx, &def);
            assert_eq!(shell.part_host, def.tree.showing(host));
            let set = shell.set_of_tab(Editor::Part.tab()).unwrap();
            assert!(matches!(&shell.layout[&set], DockItem::Tabs { tabs, hide_tab_bar: false, .. }
                if tabs == &[host.tab(), Editor::Part.tab()]));
            check_dock(&mut cx, &shell);
        }
        shell.compact = true;
        shell.width = 480.0;
        let def = state.ui.workspaces.current().clone();
        shell.update_part_placement(&mut state, Some(&def));
        shell.show_workspace(&mut cx, &def);
        assert!(!state.ui.part_side_offered);
        assert_eq!(shell.set_of_tab(Editor::Part.tab()), Some(live_id!(root)));
        for origin in [Origin::Mechanics, Origin::Monitor] {
            show(&mut cx, &mut shell, &mut state, origin);
            shell.sync_part_placement(&mut cx, &mut state);
            assert!(!state.ui.part_side_offered && !shell.layout.contains_key(&Editor::Part.tab()));
            check_dock(&mut cx, &shell);
        }
    }

    #[test]
    fn part_tab_and_force_extra_front_are_independent_and_transient() {
        use crate::ui::popover::{ui_actions, FabUiAction};
        let (mut cx, mut shell, mut state, _) = electronics_shell();
        show(&mut cx, &mut shell, &mut state, Origin::Design);
        shell.width = 1594.0;
        shell.force_shown = true;
        shell.mode_changed(&state.ui.workspaces.current().tree, Editor::Slicer, Editor::Force, true);
        shell.relayout(&mut cx);
        shell.sync_part_placement(&mut cx, &mut state);
        shell.drag_tab(Editor::Part.tab());
        assert!(shell.drag.is_none());
        let actions = cx.capture_actions(|cx| shell.show_panel(cx, Editor::Part));
        assert!(shell_actions(&actions).next().is_none());
        assert_eq!(front_of(&shell, Editor::Parts), Editor::Part.tab());
        assert_eq!(front_of(&shell, Editor::Slicer), Editor::Force.tab());
        let menu = cx.capture_actions(|cx| shell.open_area_menu(cx, Editor::Part.tab(), Rect::default(), Some(&state.ui.workspaces)));
        assert!(ui_actions(&menu).any(|a| matches!(a, FabUiAction::OpenMenu { owner, items, .. }
            if *owner == AREA_MENU && items.iter().any(|row| row.id == CLOSE_TAB && !row.enabled))));
        shell.slice_shown = true;
        shell.mode_changed(&state.ui.workspaces.current().tree, Editor::Parts, Editor::Slice, true);
        shell.relayout(&mut cx);
        for editor in [Editor::Force, Editor::Slice, Editor::Part] {
            shell.view.dock(&mut cx, ids!(main.dock)).select_tab(&mut cx, editor.tab());
            shell.tab_pressed(&mut cx, editor.tab());
            assert_eq!(front_of(&shell, editor), editor.tab());
            assert_eq!(front_of(&shell, Editor::Slicer), Editor::Force.tab());
        }
        // Removing Part while Slice is to its right must restore the real selected tab.
        state.ui.part_sidebar = false;
        shell.width = 1654.0;
        shell.sync_part_placement(&mut cx, &mut state);
        assert!(state.ui.part_sidebar);
        assert_eq!(front_of(&shell, Editor::Parts), Editor::Parts.tab());
        check_dock(&mut cx, &shell);
        let actions = cx.capture_actions(|cx| shell.show_panel(cx, Editor::Part));
        assert!(actions.iter().any(|a| matches!(a.downcast_ref::<StageAction>(), Some(StageAction::PartSidebar(true)))));
        assert!(shell_actions(&actions).next().is_none());
        shell.slice_shown = false;
        shell.mode_changed(&state.ui.workspaces.current().tree, Editor::Parts, Editor::Slice, false);
        shell.relayout(&mut cx);
        assert!(!shell.layout.contains_key(&Editor::Slice.tab()));
        assert_eq!(front_of(&shell, Editor::Slicer), Editor::Force.tab());
        check_dock(&mut cx, &shell);
    }

    #[test]
    fn electronics_device_body_controls_its_leased_pane_and_kit() {
        use makepad_widgets_core::radio_button::RadioButtonAction;
        use std::sync::Arc;

        let (mut cx, mut shell, mut ui_state, _) = electronics_shell();
        cx.with_vm(|vm| vm.capture_errors());
        run(&mut cx, &mut shell, &mut ui_state, |cx, shell, _| shell.show_panel(cx, Editor::Device));
        let errors = cx.with_vm(|vm| vm.take_errors());
        assert!(errors.is_empty(), "the device editor loads without a script error: {errors:#?}");
        let body = shell.view.dock(&cx, ids!(main.dock)).item(Editor::Device.tab());
        assert!(body.borrow::<FabDevice3d>().is_some(), "the Electronics layout creates a Device 3D body");

        let mut state = AppState::default();
        state.scene = Arc::new(Scene::from_model(crate::model::fixture::parts(), &mut |_| {}));
        let part = state.scene.elements[0].id;
        Arc::make_mut(&mut state.device.case).insert(part);
        let clicks = cx.capture_actions(|cx| {
            for id in [ids!(case), ids!(kit)] {
                cx.widget_action(body.widget(cx, id).widget_uid(), RadioButtonAction::Clicked);
            }
        });
        let actions = cx.capture_actions(|cx| body.handle_event(cx, &Event::Actions(clicks), &mut Scope::with_data(&mut state)));
        let pane = shell_actions(&actions).find_map(|action| match action {
            ShellAction::SetPaneCase { pane, shown: false } => Some(*pane),
            _ => None,
        }).expect("Case targets the device editor's pane");
        assert_eq!(state.pane_poses(pane), PanePoses::Set(PoseSetId::DEVICE), "the editor leases a pane showing device poses");
        for action in shell_actions(&actions) {
            state.apply_core(action);
        }
        assert!(!state.is_drawn_in(part, pane), "Case off hides the case part in the device pane");
        assert!(state.is_drawn_in(part, crate::api::PaneId::PRIMARY), "the plate pane still draws the case part");
        assert!(state.device.kit, "the Kit segment sets the shared device to kit mode");
    }

    #[test]
    fn the_electronics_panels_are_registered_panel_kinds() {
        let (mut cx, mut shell, mut state, errors) = electronics_shell();
        assert!(errors.is_empty(), "the shell and its Electronics panels load without a script or draw shader error: {errors:#?}");
        let dock = shell.view.dock(&cx, ids!(main.dock));
        let transport = dock.item(Editor::Transport.tab());
        assert!(transport.widget(&cx, ids!(recorder)).borrow::<fab_elec_view::live::transport::ElecTransport>().is_some()
            && transport.child(id!(controls)).borrow::<ElecControlsStrip>().is_some(),
            "the Transport editor holds the recorder under the controls header");
        assert!(dock.item(Editor::Circuit.tab()).widget(&cx, ids!(lib_open.library)).borrow::<fab_elec_view::library::LibraryPanel>().is_some(),
            "the Schematic's Library float holds the electronics library panel");
        assert!(dock.item(Editor::Scope.tab()).borrow::<fab_elec_view::instruments::logic_view::LogicScope>().is_some(),
            "Electronics creates the registered Scope as a panel of its dock");
        show(&mut cx, &mut shell, &mut state, Origin::DeviceScope);
        let controls = dock.item(Editor::BoardControls.tab());
        assert!(controls.widget(&cx, ids!(body.panel)).borrow::<fab_elec_view::instruments::controls_panel::ControlsPanel>().is_some(),
            "Device + Scope gives the run board's controls their own editor");
        // the Firmware workspace, then the Bench one
        show(&mut cx, &mut shell, &mut state, Origin::Firmware);
        let dock = shell.view.dock(&cx, ids!(main.dock));
        let firmware = dock.item(Editor::Firmware.tab());
        assert!(firmware.borrow::<fab_elec_view::firmware::FirmwarePane>().is_some(),
            "Electronics creates the registered Firmware pane as a panel of its dock");
        assert!(!firmware.widget(&cx, ids!(state.at)).is_empty() && !firmware.widget(&cx, ids!(state.regs)).is_empty(),
            "the Firmware pane holds the device's state panel");
        use fab_elec_view::live::fw_steps::FwSteps;
        assert!(firmware.widget(&cx, ids!(head.steps)).borrow::<FwSteps>().is_some(),
            "the Firmware pane's head holds the firmware steps");
        show(&mut cx, &mut shell, &mut state, Origin::Bench);
        let dock = shell.view.dock(&cx, ids!(main.dock));
        assert!(dock.item(Editor::Bench.tab()).borrow::<fab_elec_view::instruments::bench_setup::BenchSetup>().is_some(),
            "Electronics creates the registered Bench as a panel of its dock");
        // the panels' draw shaders compiled as the workspaces were shown
        let mut errors = cx.with_vm(|vm| vm.take_errors());
        errors.extend(makepad_widgets_core::makepad_platform::shader_error::take());
        assert!(errors.is_empty(), "the Device + Scope, Firmware and Bench layouts load without a script or draw shader error: {errors:#?}");
    }

    /// The transport's Setup popover hangs above its button, so a live status that grew a row as
    /// its numbers changed moved every control in it, several times a second, out from under the
    /// pointer. Its status keeps its size, and each row its place, whatever the run says.
    #[test]
    fn the_setup_status_keeps_its_size_whatever_the_run_says() {
        use fab_elec_view::live::sim::{LiveLimit, LiveSplit, LiveStatus};
        use fab_elec_view::live::transport::StatusLines;
        use makepad_widgets_core::makepad_draw::cx_draw::CxDraw;
        let (mut cx, _shell, _, _) = electronics_shell();
        let root = cx.with_vm(|vm| {
            let value = vm.eval(script! {
                use mod.prelude.widgets_internal.*
                use mod.widgets.*
                View{width: 220 height: Fit flow: Down status := ElecSetupStatus{}}
            });
            WidgetRef::script_from_value(vm, value)
        });
        let (pass, mut list) = (DrawPass::new(&mut cx), DrawList2d::new(&mut cx));
        let rows = [ids!(status), ids!(status_run), ids!(status_speed), ids!(status_steps), ids!(share_solver), ids!(share_scope), ids!(share_emulator)];
        // the rows as the popover shows them, drawn as a window draws a frame
        let mut frame = |cx: &mut Cx, lines: &StatusLines| -> Vec<Rect> {
            lines.show(cx, &root.widget(cx, ids!(status)));
            let size = dvec2(220.0, 200.0);
            pass.set_size(cx, size);
            cx.redraw_all();
            let event = std::mem::take(&mut cx.new_draw_event);
            {
                let mut draw = CxDraw::new(cx, &event);
                let mut cx2d = Cx2d::new(&mut draw);
                cx2d.begin_pass(&pass, Some(1.0));
                list.begin_always(&mut cx2d);
                cx2d.begin_root_turtle(size, makepad_widgets_core::Layout::flow_down());
                root.draw_all(&mut cx2d, &mut Scope::empty());
                cx2d.end_pass_sized_turtle();
                list.end(&mut cx2d);
                cx2d.end_pass(&pass);
            }
            rows.iter().map(|id| root.widget(cx, *id).area().rect(cx)).collect()
        };
        // the run the person watched jump: continuous, about real time, the solver holding it back
        let mut running = StatusLines::default();
        running.write(&LiveStatus {
            playing: true, continuous: true, t: 2.84, rate_achieved: Some(1.0), limited_by: Some(LiveLimit::Solver), res: 100e-6,
            split: Some(LiveSplit { solver: 0.54, scope: 0.43, emulator: 0.03 }), ..LiveStatus::default()
        }, None);
        let long = "re-rendering · 1.45 / 2.00 s · an edit whose name is far wider than the popover · from 1.20 s".to_string();
        let longest = StatusLines {
            run: long.clone(), run_tip: long, speed: "0.0123× real time · emulator".into(), steps: "res 37.5 µs · 888 timing cells".into(), steps_tip: String::new(),
            shares: ["solver 100 %", "scope 100 %", "emulator 100 %"].map(String::from),
        };
        let at_rest = frame(&mut cx, &StatusLines::default());
        assert!(at_rest.iter().all(|r| r.size.x > 0.0 && r.size.y > 0.0), "the status draws each of its rows, empty ones too: {at_rest:?}");
        assert!(at_rest[1..].iter().all(|r| r.size.y == at_rest[1].size.y), "every row is one line high: {at_rest:?}");
        for (case, lines) in [("the run", &running), ("the longest lines", &longest), ("at rest again", &StatusLines::default())] {
            assert_eq!(frame(&mut cx, lines), at_rest, "{case}: the status and each of its rows keep their rects (status, run, speed, steps, shares)");
        }
    }

    /// A wire clicked (`OpenInScope`) changes no layout; a Scope button (`ShowInScope`) in a
    /// workspace without a Scope brings one forward in an area; Follow AI brings forward the first
    /// workspace that shows what an outside edit changed.
    #[test]
    fn a_wire_click_keeps_the_layout_a_scope_button_shows_the_scope_and_follow_ai_follows_outside_edits() {
        let (mut cx, mut shell, mut state, _) = electronics_shell();
        let map = |cx: &Cx, shell: &FabShell| shell.view.dock(cx, ids!(main.dock)).clone_state().expect("the dock has a state");
        show(&mut cx, &mut shell, &mut state, Origin::Board3d);
        let before = map(&cx, &shell);
        assert!(!before.contains_key(&Editor::Scope.tab()), "Board + 3D shows no Scope");
        act(&mut cx, &mut shell, &mut state, vec![Box::new(fab_elec_view::live::flags::OpenInScope("led0".into()))]);
        assert_eq!(panels_in(&map(&cx, &shell)), panels_in(&before), "a wire clicked changes nothing on screen");
        act(&mut cx, &mut shell, &mut state, vec![Box::new(fab_elec_view::instruments::logic_view::ShowInScope("led0".into()))]);
        let after = map(&cx, &shell);
        assert!(shell.areas.as_ref().is_some_and(|t| t.showing(Editor::Scope).is_some()) && after.contains_key(&Editor::Scope.tab()),
            "a Scope button brings a Scope forward in an area");
        // Follow AI: an edit from outside the app brings the first workspace that shows it forward
        let edit = |editors: &[ElecEditor]| -> Vec<Box<dyn ActionTrait>> {
            vec![Box::new(ElecOutsideEdit { origin: fab_elec_view::live::views::EditOrigin::Disk, label: String::new(), sites: vec![], editors: editors.to_vec(), lines: vec![] })]
        };
        act(&mut cx, &mut shell, &mut state, edit(&[ElecEditor::Firmware]));
        assert_eq!(panels_in(&map(&cx, &shell)), panels_in(&after), "with Follow AI off an outside edit moves nothing");
        assert!(state.apply_core(&ShellAction::FollowAi(true)));
        act(&mut cx, &mut shell, &mut state, edit(&[ElecEditor::Firmware]));
        assert_eq!(state.ui.workspaces.current, of_origin(&state, Origin::Firmware), "an outside edit of the Firmware brings the Firmware workspace forward");
        assert!(map(&cx, &shell).contains_key(&Editor::Firmware.tab()), "and lays it out");
        act(&mut cx, &mut shell, &mut state, edit(&[ElecEditor::Board, ElecEditor::Firmware]));
        assert_eq!(state.ui.workspaces.current, of_origin(&state, Origin::Firmware), "an edit the shown workspace shows moves nothing");
        act(&mut cx, &mut shell, &mut state, edit(&[ElecEditor::Bench]));
        assert_eq!(state.ui.workspaces.current, of_origin(&state, Origin::Bench), "an outside edit of the Bench brings the Bench workspace forward");
    }

    /// The electronics workspaces: each shows its arrangement over the recorder; an area edit and
    /// a sash moved stay with the workspace across a switch (as in Blender), and the file keeps
    /// only what was changed; Reset puts its default back; Duplicate keeps a copy beside it,
    /// shown; a rename names it. An editor no area holds, brought forward, joins the area of its
    /// nearest default neighbour, else the area asked from, never covering a view; the Part
    /// panel's Parameters button, offered while no area shows Parameters, brings it so.
    #[test]
    fn workspaces_keep_their_edits_and_sizes_reset_duplicate_and_rename() {
        let (mut cx, mut shell, mut state, _) = electronics_shell();
        let map = |cx: &Cx, shell: &FabShell| shell.view.dock(cx, ids!(main.dock)).clone_state().expect("the dock has a state");
        show(&mut cx, &mut shell, &mut state, Origin::SchematicBoard);
        let crossprobe = map(&cx, &shell);
        assert_eq!(panels_in(&crossprobe), vec![Editor::Board, Editor::Circuit, Editor::Scope, Editor::Transport],
            "Schematic | Board shows the Schematic, the Board and the Scope over the recorder");
        assert!(matches!(crossprobe.get(&live_id!(root)), Some(DockItem::Splitter { axis: SplitterAxis::Vertical, align: SplitterAlign::FromB(_), .. })),
            "the recorder's area is pinned at the bottom");
        let tree = |shell: &FabShell| shell.areas.clone().expect("an areas workspace shows");
        let original = tree(&shell);
        let board = original.showing(Editor::Board).expect("the Board has an area");
        run(&mut cx, &mut shell, &mut state, |cx, _, _| cx.action(ShellAction::EditAreas(WorkspaceOp::Close { area: board })));
        assert!(!map(&cx, &shell).contains_key(&Editor::Board.tab()), "closing the Board's area takes it off screen");
        show(&mut cx, &mut shell, &mut state, Origin::Schematic);
        show(&mut cx, &mut shell, &mut state, Origin::SchematicBoard);
        assert!(!map(&cx, &shell).contains_key(&Editor::Board.tab()), "an area edit stays with its workspace across a switch");
        // a sash dragged, as the dock reports it
        let split = tree(&shell).sizes().iter().map(|(id, _)| *id).find(|id| *id != tree(&shell).id()).expect("a split of the areas'");
        let dock = shell.view.dock(&cx, ids!(main.dock));
        dock.set_splitter_align(&mut cx, split.split_live_id(), SplitterAlign::Weighted(0.3), true);
        let moved = cx.capture_actions(|cx| cx.widget_action(dock.widget_uid(),
            DockAction::SplitPanelChanged { panel_id: split.split_live_id(), axis: SplitterAxis::Vertical, align: SplitterAlign::Weighted(0.3) }));
        act(&mut cx, &mut shell, &mut state, moved);
        show(&mut cx, &mut shell, &mut state, Origin::Schematic);
        show(&mut cx, &mut shell, &mut state, Origin::SchematicBoard);
        let align = |cx: &Cx, shell: &FabShell| match map(cx, shell).get(&split.split_live_id()) { Some(DockItem::Splitter { align: SplitterAlign::Weighted(w), .. }) => Some(*w), _ => None };
        assert_eq!(align(&cx, &shell), Some(0.3), "a workspace keeps the sash positions it was left at");
        // Save the edited layout and another workspace with Scope 2 left of Scope.
        let mut saved = state.ui.workspaces.clone();
        let scope = |n| AreaTree::Area { id: AreaId(0), tabs: vec![Instance { editor: Editor::Scope, n }], selected: 0 };
        saved.list[0].tree = AreaTree::beside(scope(1), scope(0), 0.5);
        let text = saved.to_ron();
        assert_eq!(text.matches("tree:").count(), 2, "only the changed workspaces' areas are written:\n{text}");
        let read = Workspaces::from_ron(&text).expect("the file reads back");
        assert!(read.list.iter().map(|d| d.tree.clone()).eq(saved.list.iter().map(|d| d.tree.clone().numbered())), "each workspace reads back as it was");
        let scope_numbers = |row: &Workspaces| row.list[0].tree.instances().into_iter().map(|(_, i)| i.n).collect::<Vec<_>>();
        assert_eq!(scope_numbers(&read), [1, 0], "Scope 2 stays left of Scope across a save and load");
        saved.list[0].tree = AreaTree::beside(scope(0), AreaTree::beside(scope(2), scope(2), 0.5), 0.5);
        let repaired = Workspaces::from_ron(&saved.to_ron()).expect("a file with duplicate Scope numbers reads back");
        assert_eq!(scope_numbers(&repaired), [0, 2, 1], "the later duplicate takes the lowest free Scope number");
        let plate2 = Instance { editor: Editor::Viewport, n: 1 };
        saved.list[0].tree = AreaTree::Area { id: AreaId(0), tabs: vec![plate2], selected: 0 };
        let lone = Workspaces::from_ron(&saved.to_ron()).expect("a workspace of Plate 2 alone reads back");
        assert_eq!(lone.list[0].tree.instances().into_iter().map(|(_, i)| i).collect::<Vec<_>>(), [plate2], "Plate 2 alone stays Plate 2");
        assert!(!lone.instance_state(plate2).tools, "a later Plate view opens with its rails folded");
        let plate = Instance::first(Editor::Viewport);
        assert!(!text.contains("instances:"), "default shelves need no file entry");
        assert_eq!(read.instance_state(plate), crate::workspaces::InstanceState::default(),
            "a file without instances uses the default shelf state");
        let tools = |shown| ShellAction::EditWorkspaces(ListOp::Region { instance: plate, tools: Some(shown), left: None });
        let revision = state.ui.workspaces.instance_rev;
        assert!(state.apply_core(&tools(false)), "the Plate shelf folds");
        assert!(!state.apply_core(&tools(false)), "folding a folded shelf changes nothing");
        assert_eq!(state.ui.workspaces.instance_rev, revision + 1, "only a changed shelf advances the revision");
        let folded = Workspaces::from_ron(&state.ui.workspaces.to_ron()).expect("the file with a folded shelf reads back");
        assert!(!folded.instance_state(plate).tools, "the Plate shelf stays folded across a restart");
        let id = state.ui.workspaces.current;
        let edited = state.ui.workspaces.current().tree.clone();
        assert!(!read.can_undo(id), "loading the saved row starts a fresh layout history");
        assert!(state.apply_core(&ShellAction::EditWorkspaces(ListOp::Reset(id))));
        shell.sync_with(&mut cx, &mut state);
        assert!(map(&cx, &shell).contains_key(&Editor::Board.tab()) && align(&cx, &shell) != Some(0.3),
            "Reset puts the default's areas and sashes back");
        let undo = ShellAction::EditWorkspaces(ListOp::UndoLayout(id));
        assert!(state.apply_core(&undo), "Reset records the previous layout");
        assert_eq!(state.ui.workspaces.current().tree, edited, "undoing Reset restores the edited areas and sash");
        assert!(state.apply_core(&undo), "closing the Board's area records a layout change");
        shell.sync_with(&mut cx, &mut state);
        assert_eq!(tree(&shell), original, "undoing Close lays out the original tree again");
        assert!(!state.ui.workspaces.instance_state(plate).tools, "undoing the layouts leaves the Plate shelf folded");
        assert!(!state.ui.workspaces.can_undo(id), "dragging the sash outside a group adds no undo entry");
        assert!(state.apply_core(&tools(true)) && !state.ui.workspaces.to_ron().contains("instances:"),
            "restoring the default shelf removes its file entry");

        state.apply_core(&ShellAction::LayoutGroup(true));
        let resize = WorkspaceOp::Resize { split: original.id(), align: SplitterAlign::Weighted(0.4) };
        assert!(state.apply_core(&ShellAction::EditAreas(resize)), "the grouped resize moves the root sash");
        assert!(state.apply_core(&ShellAction::EditAreas(WorkspaceOp::Maximize { area: Some(board) })), "the group maximizes the Board");
        state.apply_core(&ShellAction::LayoutGroup(false));
        assert!(state.apply_core(&undo), "the group records one layout change");
        assert_eq!(state.ui.workspaces.current().tree, original, "undoing the group restores the sash from before its first edit");
        assert_eq!(state.ui.workspaces.current().maximized, None, "the same undo restores the maximized state");
        assert!(!state.ui.workspaces.can_undo(id), "one undo exhausts the two-edit group");
        assert!(state.apply_core(&ShellAction::EditWorkspaces(ListOp::Duplicate(id))));
        shell.sync_with(&mut cx, &mut state);
        let at = state.ui.workspaces.position(id).expect("the original stays");
        let copy = &state.ui.workspaces.list[at + 1];
        assert!(copy.id == state.ui.workspaces.current && copy.name == "Schematic | Board 2", "Duplicate keeps a copy right after it, shown: {:?}", copy.name);
        let copy = copy.id;
        assert!(state.apply_core(&ShellAction::EditWorkspaces(ListOp::Rename(copy, "Probing".into()))));
        assert_eq!(state.ui.workspaces.get(copy).map(|d| d.name.as_str()), Some("Probing"), "a workspace takes the name it is given");
        assert!(state.apply_core(&ShellAction::EditWorkspaces(ListOp::Delete(copy))));
        assert_eq!(state.ui.workspaces.current, id, "deleting the shown one shows the one before it, as in Blender");
        // the temporary layout: a copy shown over the row and edited, a second one in its place,
        // then Back: the row and its file as they were. Keep adds exactly one.
        let (row, file) = (state.ui.workspaces.list.clone(), state.ui.workspaces.to_ron());
        let temp = |note: &str, by| ShellAction::EditWorkspaces(ListOp::Temporary { from: id, note: note.into(), by });
        assert!(state.apply_core(&temp("Net VCC", ShownBy::Ai)));
        let shown = state.ui.workspaces.current;
        assert!(state.ui.workspaces.temp_shown() && state.ui.workspaces.current().name == "Net VCC", "the temporary layout shows, titled by its note");
        shell.sync_with(&mut cx, &mut state);
        let board = state.ui.workspaces.current().tree.showing(Editor::Board).expect("the copy shows the Board");
        assert!(state.apply_core(&ShellAction::EditAreas(WorkspaceOp::Close { area: board })));
        shell.sync_with(&mut cx, &mut state);
        assert!(!map(&cx, &shell).contains_key(&Editor::Board.tab()), "the temporary layout takes area ops as any workspace does");
        assert!(state.apply_core(&temp("Net GND", ShownBy::Ai)));
        let again = state.ui.workspaces.current();
        assert!(again.id == shown && again.name == "Net GND" && again.tree.showing(Editor::Board).is_some(), "a second one takes the first's place: its tab, a fresh copy");
        assert!(state.apply_core(&ShellAction::EditWorkspaces(ListOp::Dismiss)));
        shell.sync_with(&mut cx, &mut state);
        assert!(state.ui.workspaces.current == id && state.ui.workspaces.temp.is_none() && map(&cx, &shell).contains_key(&Editor::Board.tab()),
            "Back shows the workspace it came over, as it was");
        assert!(state.ui.workspaces.list == row && state.ui.workspaces.to_ron() == file, "the row and its file as they were");
        assert!(state.apply_core(&temp("", ShownBy::Person)) && state.apply_core(&ShellAction::EditWorkspaces(ListOp::Keep(None))));
        let kept = state.ui.workspaces.list.last().expect("a row");
        assert!(state.ui.workspaces.list.len() == row.len() + 1 && state.ui.workspaces.temp.is_none() && kept.id == state.ui.workspaces.current && kept.name == "Schematic | Board 2",
            "Keep adds exactly one, at the row's end, shown: {:?}", kept.name);
        // brought forward with no area holding it: Overrides joins the Part's stack (Design's lower
        // stack closed), in front
        let front = |t: &AreaTree, e: Editor| t.showing(e).and_then(|a| t.instance(a)).map(|i| i.editor);
        let mut design = Origin::Design.tree().0;
        let below = design.showing(Editor::Overrides).expect("Design shows Overrides");
        design.apply(WorkspaceOp::Close { area: below }).expect("the lower stack closes");
        design.apply(WorkspaceOp::Reveal { editor: Editor::Overrides, from: None }).expect("Overrides shows");
        assert!(design.showing(Editor::Overrides) == design.showing(Editor::Slicer) && front(&design, Editor::Slicer) == Some(Editor::Overrides),
            "Overrides joins its nearest neighbour's stack, in front");
        // no neighbour on screen: the area it was asked from; a view's never (the last area splits)
        let mut scan = Origin::Scan.tree().0;
        let panel = scan.showing(Editor::Scan).expect("Scan's panel");
        scan.apply(WorkspaceOp::Reveal { editor: Editor::Strength, from: Some(panel) }).expect("Strength shows");
        assert_eq!(scan.showing(Editor::Strength), Some(panel), "a list joins the area it was asked from");
        let mut mech = Origin::Mechanics.tree().0;
        let plate = mech.showing(Editor::Viewport).expect("the plate");
        let areas = mech.areas().len();
        mech.apply(WorkspaceOp::Reveal { editor: Editor::Strength, from: Some(plate) }).expect("Strength shows");
        assert!(mech.areas().len() == areas + 1 && front(&mech, Editor::Viewport) == Some(Editor::Viewport), "a list never covers a view");
        let column = mech.showing(Editor::Strength).and_then(|a| mech.parent_split(a));
        assert!(matches!(column, Some((_, SplitterAxis::Horizontal, SplitterAlign::FromB(w), true)) if w == 320.0)
            && mech.parent_split(plate).map(|(split, ..)| split) == column.map(|(split, ..)| split),
            "a list asked from a view opens as a side column at its right, as wide as the side columns: {column:?}");
        // asked from nowhere, the same beside the workspace's first view (Electronics' sheet,
        // not the recorder under it, whose area is as tall as what it holds)
        let mut sheet = Origin::Schematic.tree().0;
        sheet.apply(WorkspaceOp::Reveal { editor: Editor::Checks, from: None }).expect("Checks shows");
        let circuit = sheet.showing(Editor::Circuit).expect("the sheet");
        assert!(sheet.parent_split(circuit).map(|(split, ..)| split) == sheet.showing(Editor::Checks).and_then(|a| sheet.parent_split(a)).map(|(split, ..)| split),
            "Checks opens beside the sheet");
        // Design without its lower stack (an old file's): no area shows Overrides; asked for from
        // the Slicer's area, it comes into the Slicer's stack, in front
        show(&mut cx, &mut shell, &mut state, Origin::Design);
        let below = tree(&shell).showing(Editor::Overrides).expect("Design shows Overrides");
        run(&mut cx, &mut shell, &mut state, |cx, _, _| cx.action(ShellAction::EditAreas(WorkspaceOp::Close { area: below })));
        shell.publish_screen(&mut cx, &mut state);
        assert!(!state.ui.on_screen(Editor::Overrides) && state.ui.on_screen(Editor::Slicer), "no area shows Overrides");
        let part = tree(&shell).showing(Editor::Slicer).expect("the Slicer's area");
        shell.pressed_area = shell.shown.map(|(ws, _)| (ws, part));
        act(&mut cx, &mut shell, &mut state, vec![Box::new(ShowPanel(Editor::Overrides)) as Box<dyn ActionTrait>]);
        shell.publish_screen(&mut cx, &mut state);
        assert!(tree(&shell).showing(Editor::Overrides) == Some(part) && front(&tree(&shell), Editor::Slicer) == Some(Editor::Overrides) && state.ui.on_screen(Editor::Overrides),
            "the press brings Overrides into the Slicer's stack, in front");

        // a Plate the dock holds but does not show is published hidden and lets go of the scene
        // host, which the Plate on screen takes with the shared studio work; it stays unparked
        let plate = Instance::first(Editor::Viewport);
        assert!(state.ui.plate_shown(plate), "Design shows its Plate");
        state.scene_gpu.borrow_mut().host = Some(PaneId::PLATE_ZERO);
        show(&mut cx, &mut shell, &mut state, Origin::Schematic);
        shell.publish_screen(&mut cx, &mut state);
        assert!(!state.ui.plate_shown(plate) && state.ui.plates_hidden == [plate.tab()], "the Schematic holds the Plate hidden: {:?}", state.ui.plates_hidden);
        assert!(state.scene_gpu.borrow().host.is_none() && !state.view.pane(PaneId::PLATE_ZERO).parked, "the hidden Plate lets go of the host and keeps its pane");
        // bound to a pane of its own, the stage's viewport, overlays, grab and gizmo name it
        let lease = state.lease_pane(PaneRole::Plate);
        let body = shell.view.dock(&cx, ids!(main.dock)).borrow_mut()
            .and_then(|mut dock| dock.items().get(&plate.tab()).map(|(_, body)| body.clone()))
            .expect("the hidden Plate's body stays resident");
        body.borrow_mut::<crate::ui::stage::FabStage>().expect("the Plate's body is a stage").bind(&mut cx, &mut state, lease.id(), plate);
        let named = [
            body.widget(&cx, ids!(viewport)).borrow::<crate::viewport::FabViewport>().map(|w| w.pane()),
            body.widget(&cx, ids!(design_overlay.tool_overlay)).borrow::<crate::tools::overlay::FabToolOverlay>().map(|w| w.pane),
            body.widget(&cx, ids!(design_overlay.part_gizmo)).borrow::<crate::tools::transform::FabPartGizmo>().map(|w| w.pane),
            body.widget(&cx, ids!(design_overlay.shape_handles)).borrow::<crate::tools::handles::FabShapeHandles>().map(|w| w.pane),
            body.widget(&cx, ids!(design_overlay.test_cube)).borrow::<crate::tools::test_cube::FabTestCube>().map(|w| w.pane),
            body.widget(&cx, ids!(design_overlay.force_editor)).borrow::<crate::tools::force_edit::FabForceEditor>().map(|w| w.pane),
            body.widget(&cx, ids!(mech_grab)).borrow::<crate::tools::mech_grab::FabMechGrab>().map(|w| w.pane),
            body.widget(&cx, ids!(nav_col.gizmo)).borrow::<crate::nav::gizmo::FabNavGizmo>().map(|w| w.pane),
        ];
        assert_eq!(named, [Some(lease.id()); 8], "every part of the bound stage draws and acts through its pane");
        body.borrow_mut::<crate::ui::stage::FabStage>().expect("the Plate's body is a stage").bind(&mut cx, &mut state, PaneId::PLATE_ZERO, plate);
        drop(lease);

        // Plate 2 beside Plate: bound before it draws to a pane of its own, at the lead's camera,
        // its rails folded; Plate, the lowest on screen, leads; the one pressed is active
        show(&mut cx, &mut shell, &mut state, Origin::Design);
        shell.publish_screen(&mut cx, &mut state);
        let area = tree(&shell).showing(Editor::Viewport).expect("Design shows its Plate");
        run(&mut cx, &mut shell, &mut state, |cx, _, _| cx.action(ShellAction::EditAreas(WorkspaceOp::Split {
            area, side: Side::Right, editor: Editor::Viewport, align: SplitterAlign::Weighted(0.5),
        })));
        shell.publish_screen(&mut cx, &mut state);
        let second = Instance { editor: Editor::Viewport, n: 1 };
        let held = tree(&shell).instances().into_iter().find(|(_, i)| *i == second).map(|(area, _)| area).expect("the split opens Plate 2");
        let pane = state.view.plate_pane(1).expect("Plate 2 is bound to a pane");
        assert!(pane != PaneId::PLATE_ZERO && state.view.pane(pane).role == PaneRole::Plate, "Plate 2 leases a Plate pane: {pane:?}");
        assert_eq!(*state.view.camera(pane), *state.view.camera(PaneId::PLATE_ZERO), "Plate 2 opens at the lead's camera");
        assert_eq!(state.view.shown_plates, [PaneId::PLATE_ZERO, pane], "both Plates are on screen, Plate first");
        assert_eq!((state.view.lead_plate(), state.view.active_plate()), (PaneId::PLATE_ZERO, PaneId::PLATE_ZERO), "Plate leads and, unpressed, is active");
        assert!(!state.ui.workspaces.instance_state(second).tools, "Plate 2 opens with its rails folded");
        state.view.pressed_plate = pane;
        assert_eq!(state.view.active_plate(), pane, "the Plate pressed is the active one");
        // closed, no workspace holds Plate 2: its pane goes back, the lead is active again
        run(&mut cx, &mut shell, &mut state, |cx, _, _| cx.action(ShellAction::EditAreas(WorkspaceOp::Close { area: held })));
        shell.publish_screen(&mut cx, &mut state);
        state.drain_released_panes();
        assert!(state.view.plate_pane(1).is_none() && state.view.panes[pane.0 as usize].is_none(), "Plate 2's pane goes back");
        assert_eq!((state.view.shown_plates.as_slice(), state.view.active_plate()), ([PaneId::PLATE_ZERO].as_slice(), PaneId::PLATE_ZERO),
            "the Plate left on screen leads and is active");

        // View › Underside view of Plate: a fresh Plate view beside it, Flipped to pane 0, its
        // rails folded; See through is kept with it; off and on again gives the same areas
        use crate::workspaces::PlatePair;
        let under = |cx: &mut Cx, shell: &mut FabShell, state: &mut AppState| {
            run(cx, shell, state, |cx, _, _| cx.action(ShellAction::EditWorkspaces(ListOp::Pair { source: 0, pair: PlatePair::Underside })));
            shell.publish_screen(cx, state);
            state.drain_released_panes();
        };
        let without = tree(&shell).numbered();
        under(&mut cx, &mut shell, &mut state);
        let with = tree(&shell).numbered();
        let (_, below) = state.ui.workspaces.paired(0, PlatePair::Underside).expect("the Underside view opens as a Plate view");
        let pane = state.view.plate_pane(below.n).expect("the Underside view is bound");
        assert_eq!(state.view.pane(pane).link, PaneLink::Linked { how: LinkHow::Flipped, to: PaneId::PLATE_ZERO }, "it looks up at Plate from below");
        assert!(!state.ui.workspaces.instance_state(below).tools, "the Underside view opens with its rails folded");
        assert!(state.apply_core(&ShellAction::SetPaneLink { pane, link: PaneLink::Linked { how: LinkHow::Through, to: PaneId::PLATE_ZERO } }));
        let kept = Workspaces::from_ron(&state.ui.workspaces.to_ron()).expect("the file reads back");
        assert_eq!(kept.instance_state(below).link, Some((LinkHow::Through, 0)), "See through is kept with the view, across a restart");
        under(&mut cx, &mut shell, &mut state);
        assert!(tree(&shell).numbered() == without && state.view.plate_pane(below.n).is_none(), "off closes it and lets its pane go");
        under(&mut cx, &mut shell, &mut state);
        assert!(tree(&shell).numbered() == with, "on, off, on gives the same areas");

        // the AI's temporary layout shows fresh Plate views: a camera move there leaves the
        // person's Plate and the file as they were, and Back lets them go
        let camera = *state.view.camera(PaneId::PLATE_ZERO);
        let file = state.ui.workspaces.to_ron();
        let from = state.ui.workspaces.current;
        run(&mut cx, &mut shell, &mut state, |cx, _, _| cx.action(ShellAction::EditWorkspaces(ListOp::Temporary { from, note: "a look".into(), by: ShownBy::Ai })));
        shell.publish_screen(&mut cx, &mut state);
        let ai = state.view.active_plate();
        assert!(ai != PaneId::PLATE_ZERO && state.view.shown_plates.len() == 2, "the AI's Plate views are fresh ones: {:?}", state.view.shown_plates);
        state.view.camera_mut(ai).eye.x += 1.0;
        run(&mut cx, &mut shell, &mut state, |cx, _, _| cx.action(ShellAction::EditWorkspaces(ListOp::Dismiss)));
        shell.publish_screen(&mut cx, &mut state);
        state.drain_released_panes();
        assert_eq!(*state.view.camera(PaneId::PLATE_ZERO), camera, "the person's Plate keeps its camera");
        assert_eq!(state.ui.workspaces.to_ron(), file, "the settings file is as it was");
        assert!(state.view.panes[ai.0 as usize].is_none(), "Back lets the AI's Plate views go");
    }

    /// The area menu's edits: another Scope opens beside one, an editor on screen swaps places, a
    /// split shows the first editor not on screen, and the last area does not close.
    #[test]
    fn the_new_tab_button_adds_a_tab_brings_one_forward_or_moves_a_lone_editor() {
        use crate::ui::areas::OpError;
        let mut tree = layouts::defaults()[1].1.clone();
        let area_of = |t: &AreaTree, e: Editor| t.showing(e).expect("the editor has an area");
        let scope_area = area_of(&tree, Editor::Scope);
        let stack = |t: &AreaTree, a| t.stack_of(a).map(|(tabs, k)| (tabs.to_vec(), k)).expect("the area's stack");
        let (before, _) = stack(&tree, scope_area);
        // a repeatable editor the area shows: another instance, as a new tab in front
        tree.apply(WorkspaceOp::AddTab { area: scope_area, editor: Editor::Scope }).expect("a second Scope opens");
        let (tabs, k) = stack(&tree, scope_area);
        assert!(tabs.len() == before.len() + 1 && tabs[k].editor == Editor::Scope && tabs[k].tab() != Editor::Scope.tab(), "another Scope, in front: {tabs:?}");
        let before = tabs;
        // a lone editor another area holds moves here, as a new tab in front
        let board_area = area_of(&tree, Editor::Board);
        tree.apply(WorkspaceOp::AddTab { area: scope_area, editor: Editor::Board }).expect("the Board moves here");
        let (tabs, k) = stack(&tree, scope_area);
        assert!(tabs.len() == before.len() + 1 && tabs[k].editor == Editor::Board, "the Board is a new tab, in front: {tabs:?}");
        assert!(tree.area_of_tab(Editor::Board.tab()) != Some(board_area), "it left the area it was in");
        // one no area holds: a new tab of it, in front
        tree.apply(WorkspaceOp::AddTab { area: scope_area, editor: Editor::Slice }).expect("Slice opens here");
        let (tabs, k) = stack(&tree, scope_area);
        assert_eq!(tabs[k].editor, Editor::Slice, "the new tab is in front");
        assert_eq!(tabs.last().map(|i| i.editor), Some(Editor::Slice), "after the others");
        // one the area holds, behind: it comes forward, no tab added
        tree.apply(WorkspaceOp::AddTab { area: scope_area, editor: Editor::Board }).expect("the Board comes forward");
        let (again, k) = stack(&tree, scope_area);
        assert!(again.len() == tabs.len() && again[k].editor == Editor::Board, "brought forward, not added");
        assert_eq!(tree.apply(WorkspaceOp::AddTab { area: scope_area, editor: Editor::Board }).err(), Some(OpError::Unchanged), "already in front");
    }

    #[test]
    fn area_menu_edits_put_swap_split_and_close() {
        use crate::ui::areas::OpError;
        use crate::ui::popover::{ui_actions, FabUiAction};
        use crate::ui::workspace_tool;
        let defaults = layouts::defaults();
        let mut tree = defaults[1].1.clone();
        let area_of = |t: &AreaTree, e: Editor| t.showing(e).expect("the editor has an area");
        let scope_area = area_of(&tree, Editor::Scope);
        let circuit_area = area_of(&tree, Editor::Circuit);
        tree.apply(WorkspaceOp::SetEditor { area: circuit_area, editor: Editor::Scope }).expect("a second Scope opens");
        let second = tree.instance(circuit_area).expect("the Schematic's area");
        assert!(second.editor == Editor::Scope && second.tab() != Editor::Scope.tab(), "a Scope already shown opens another one");
        assert!(tree.showing(Editor::Circuit).is_none(), "the Schematic it replaced leaves the screen");
        let board_area = area_of(&tree, Editor::Board);
        tree.apply(WorkspaceOp::SetEditor { area: circuit_area, editor: Editor::Board }).expect("the Board swaps in");
        assert_eq!(tree.area_of_tab(second.tab()), Some(board_area), "an editor on screen swaps places with the one asked");
        let editor = layouts::next_editor(&tree);
        assert_eq!(editor, layouts::ELEC_3D, "a split shows the first editor not on screen, the 3D view");
        tree.apply(WorkspaceOp::Split { area: scope_area, side: Side::Right, editor, align: SplitterAlign::Weighted(0.5) }).expect("the Scope's area splits");
        let new = area_of(&tree, layouts::ELEC_3D);
        tree.apply(WorkspaceOp::Close { area: new }).expect("the new area closes");
        assert_eq!(tree.area_of_tab(Editor::Scope.tab()), Some(scope_area), "the area beside takes the room back");
        let mut one = defaults[1].1.clone();
        for editor in [Editor::Board, Editor::Scope] {
            let area = area_of(&one, editor);
            one.apply(WorkspaceOp::Close { area }).expect("areas close down to one");
        }
        let last = area_of(&one, Editor::Circuit);
        assert_eq!(one.apply(WorkspaceOp::Close { area: last }).err(), Some(OpError::LastArea), "the last area does not close");

        let (mut cx, mut shell, mut state, _) = electronics_shell();
        show(&mut cx, &mut shell, &mut state, Origin::Mechanics);
        let plate = Instance::first(Editor::Viewport);
        let plate_area = area_of(&state.ui.workspaces.current().tree, plate.editor).0;
        let mechanics_area = area_of(&state.ui.workspaces.current().tree, Editor::Mechanics).0;
        let tools_row = |cx: &mut Cx, shell: &mut FabShell, ws: &Workspaces, editor: Editor| {
            let open = cx.capture_actions(|cx| shell.open_area_menu(cx, editor.tab(), Rect::default(), Some(ws)));
            let rows = ui_actions(&open).find_map(|a| match a {
                FabUiAction::OpenMenu { owner, items, .. } if *owner == AREA_MENU => Some(items),
                _ => None,
            }).expect("the nub opens the area's menu");
            rows.iter().find(|row| row.label == "Tools").map(|row| (row.id, row.current))
        };
        let mut probe = state.ui.workspaces.clone();
        assert_eq!(tools_row(&mut cx, &mut shell, &state.ui.workspaces, plate.editor), Some((TOOLS, true)), "the Plate offers its open shelf");
        act(&mut cx, &mut shell, &mut state, vec![Box::new(FabUiAction::MenuPicked { owner: AREA_MENU, id: TOOLS })]);
        assert!(!state.ui.workspaces.instance_state(plate).tools, "picking Tools folds the Plate shelf");
        assert_eq!(tools_row(&mut cx, &mut shell, &state.ui.workspaces, plate.editor), Some((TOOLS, false)), "the menu reads the folded shelf");
        assert_eq!(tools_row(&mut cx, &mut shell, &state.ui.workspaces, Editor::Mechanics), None, "the Mechanics panel has no Tools row");

        let mut mode = state.ui.plate_mode;
        let edit = |ws: &mut Workspaces, mode: &mut PlateMode, text: String| workspace_tool::run(&text, ws, mode, &|_| true);
        let edits = edit(&mut probe, &mut mode, format!("region({plate_area}, tools: off)")).expect("the statement folds the Plate shelf");
        assert!(matches!(edits.as_slice(), [workspace_tool::WsEdit::List(ListOp::Region { instance, tools: Some(false), left: None })] if *instance == plate));
        assert_eq!(probe.instance_state(plate), state.ui.workspaces.instance_state(plate), "the statement and the menu change the same instance");
        let read = workspace_tool::describe(&probe, mode, &[], false, &|_| true);
        assert!(read.contains(&format!("area: {plate_area} editors: [\"Plate\"] front: \"Plate\" tools: off")), "the read shows the Plate shelf: {read}");
        let error = edit(&mut probe, &mut mode, format!("region({plate_area}, side: left)")).expect_err("the Plate shelf stays in place");
        assert!(error.contains("Plate has a fixed tool shelf"), "{error}");
        let error = edit(&mut probe, &mut mode, format!("region({mechanics_area}, tools: off)")).expect_err("the Mechanics panel has no shelf");
        assert!(error.contains("Mechanics has no tool shelf") && error.contains("Plate and Schematic"), "{error}");
        let schematic = of_origin(&state, Origin::Schematic);
        let sheet_area = area_of(&probe.get(schematic).unwrap().tree, Editor::Circuit).0;
        edit(&mut probe, &mut mode, format!("switch(\"Schematic\") region({sheet_area}, side: left) region({sheet_area}, tools: off)"))
            .expect("the Library can move and fold independently");
        let read = workspace_tool::describe(&probe, mode, &[], false, &|_| true);
        assert!(read.contains("front: \"Schematic\" tools: off side: left"), "folding keeps the Library on the chosen side: {read}");
    }

    #[test]
    fn every_workspace_shows_its_view_and_each_panel_once() {
        for ws in Workspace::ALL {
            let (tree, _) = Origin::of_kind(ws).tree();
            // Design starts with the Program beside the plate; the code toggle closes its tab
            let mut without_code = tree.clone();
            if let Some(area) = without_code.showing(Editor::Code) {
                without_code.apply(WorkspaceOp::CloseTab { area, instance: Instance::first(Editor::Code) }).expect("the Program's tab closes");
            }
            for (tree, code) in [(tree.clone(), ws == Workspace::Design), (without_code, false)] {
            for narrow in [None, Some(900.0)] {
            let layout = present(&tree, &Present { narrow, ..Present::default() });
            let view = match ws {
                Workspace::Arm => Editor::ArmCell,
                Workspace::Scan => Editor::ScanView,
                Workspace::Electronics => Editor::Circuit,
                Workspace::Design | Workspace::Mechanics | Workspace::Monitor => Editor::Viewport,
            };
            assert!(layout.contains_key(&view.tab()), "{ws:?} includes its {view:?} view");
            let mut seen = Vec::new();
            for item in layout.values() {
                if let DockItem::Tabs { tabs, .. } = item {
                    for t in tabs {
                        assert!(!seen.contains(t), "{ws:?} lists each panel in only one tab set");
                        seen.push(*t);
                        assert!(layout.contains_key(t), "{ws:?} has a panel for each tab");
                    }
                }
            }
            assert!(layout.contains_key(&live_id!(root)), "{ws:?} has a dock root");
            if ws == Workspace::Electronics {
                assert_eq!(panels_in(&layout), vec![Editor::Circuit, Editor::Scope, Editor::Transport],
                    "Electronics starts on its first workspace: the Schematic over the Scope, over the recorder");
                assert!(matches!(layout.get(&live_id!(root)), Some(DockItem::Splitter { axis: SplitterAxis::Vertical, align: SplitterAlign::FromB(_), a, b })
                    if *a == AreaId(1).split_live_id() && layout.get(b).is_some_and(|i| matches!(i, DockItem::Tabs { tabs, hide_tab_bar: true, .. } if *tabs == [Editor::Transport.tab()]))),
                    "the recorder's area is pinned under the others, its tab bar hidden (its notch is its menu)");
                assert!(matches!(layout.get(&AreaId(1).split_live_id()), Some(DockItem::Splitter { axis: SplitterAxis::Vertical, align: SplitterAlign::Weighted(w), a, b })
                    if (*w - 0.64).abs() < 1e-9 && layout.get(a).is_some_and(|i| matches!(i, DockItem::Tabs { tabs, hide_tab_bar: true, .. } if *tabs == [Editor::Circuit.tab()]))
                        && layout.get(b).is_some_and(|i| matches!(i, DockItem::Tabs { tabs, hide_tab_bar: true, .. } if *tabs == [Editor::Scope.tab()]))),
                    "the first workspace's areas are the Schematic over the Scope, 64 to 36, each with no tab row (its notch is its menu)");
            }
            assert_eq!(layout.contains_key(&Editor::Code.tab()), code, "{ws:?}: the code split follows its toggle");
            }
            }
        }
    }
}
