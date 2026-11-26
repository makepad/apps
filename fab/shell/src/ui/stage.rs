//! The plate view: Fab's primary 3D view with everything that works on it drawn
//! over it. Its Underside and Traced views are Plate views of their own beside it.
//!
//! The render switch sits at the top of the view: Normal (the look View ›
//! Shading sets), Sliced or Force, with the view bar beside it: the
//! orthographic toggle in every mode, and in Sliced what the slice shows (its
//! supports, its travel, the parts under it). The bar changes with the mode
//! alone, and each of its toggles is the View menu's too.
//!
//! Model: a tool strip (select, move, turn, scale, drop to the plate,
//! arrange, clip, measure) at the left, the view cube at the right, the part
//! gizmo on the selected part. The plate notice names its blocking issues.
//!
//! Sliced, a render mode over the same plate: the tool strip, gizmo and handles
//! stay (in Design), the vertical layer slider joins at the right edge, the
//! playback bar with the move slider along the bottom, and, while there is no
//! current slice, its state and progress. The legend is the Slice panel's.
//!
//! Monitor: while the printer runs a job Fab sliced, the slice view follows
//! it (the playback bar gets a "Following printer" toggle and the job's
//! layer bar); a job Fab has no slice of gets a card that says so, with what
//! the printer reports (`print::follow`).
//!
//! The printer camera can sit in the bottom-right corner (picture in
//! picture). Every control edits through the existing actions.

use crate::api::*;
use crate::api::PaneId;
use crate::params::AreaKind;
use crate::ui::keymap::{self, ShortcutId as Key};
use crate::print::panel::slice_status;
use crate::print::timeline::{layer_span, shown_top_layer};
use crate::print::{format_time, PrintAction, SliceStatus};
use crate::ui::parts::active_part;
use crate::ui::popover::{dropdown_clicked, menu_picked, open_menu, speed_in_menu, speed_label, speed_menu, speed_pick, MenuItem, MenuPlace, SpeedMenuKind};
use crate::ui::editors::Editor;
use crate::ui::shell::ShowPanel;
use fab_paint_field::StrokeOp;
use makepad_widgets_core::event::TouchState;
use makepad_widgets_core::tip::TipWidgetRefExt;
use makepad_widgets_core::*;

const SPEED_MENU: LiveId = live_id!(fab_stage_speed);
const CONTEXT_MENU: LiveId = live_id!(fab_stage_context);
/// The navigation gizmo's row's top inset; Mechanics adds its header strip's height, the other
/// modes the top toolbar's.
const NAV_TOP: f64 = 6.0;
/// The room the chosen tool's row takes under the toolbar (its gap above, its tallest, the Paint
/// row with its dials, and a gap below), that the navigation gizmo keeps clear of.
const CONTEXT_ROW: f64 = 10.0 + 50.0 + 6.0;
/// The top toolbar's top inset (`render_row`: none, it is the view's toolbar) and its height (an
/// editor toolbar's).
const RENDER_ROW_TOP: f64 = 0.0;
const RENDER_ROW_H: f64 = crate::ui::area_notch::TOOLBAR_H;
/// One row of Mechanics' header strip (a toolbar's height), and the gap under the strip before
/// Mechanics' tools.
const MECH_STRIP_ROW: f64 = crate::ui::area_notch::TOOLBAR_H;
const MECH_TOOLS_GAP: f64 = 12.0;
const PANE_LABEL_LEFT: f64 = 56.0;
const PANE_LABEL_PAD: f64 = 10.0;
/// The chip's small icon (`fab.icon_size_sm`) and its gap to the number.
const PANE_LABEL_ICON: f64 = 12.0 + 4.0;

/// How far down the view a slice's layer slider starts (px): the render modes and the slice's
/// chip sit above it. Its tags never reach higher, so a progress card over the slice ends above
/// it ([`crate::ui::progress_card`]).
pub const SLIDER_TOP: f64 = 300.0;

/// The Part sidebar's width at first, its range as its left edge is dragged, and the narrowest
/// Plate view that shows it (narrower, the view keeps its room and the toolbar's Part stays off).
pub const PART_SIDE_W: f64 = 300.0;
pub const PART_SIDE_MIN_W: f64 = 240.0;
pub const PART_SIDE_MAX_W: f64 = 640.0;
const PART_SIDE_MIN_VIEW: f64 = 640.0;
/// The Part sidebar's gap to the view's right edge.
const PART_SIDE_RIGHT: f64 = 10.0;
/// The playback card's width (its pad as wide, less its padding).
const PAD_CARD_W: f64 = 264.0;

/// What a playhead step ([`PrintAction::moves_playhead`]) changes: the slice in the view, the
/// layer slider, the transport with its time and layer readouts, a resin printer's layer and
/// the headset's view. Everything else in the stage and the panels stays as it was drawn.
pub const PLAYHEAD: &[&[LiveId]] = &[ids!(viewport), ids!(slice_ui.bottom.bar_row.bar), ids!(slice_ui.resin_ui), ids!(xr)];

script_mod! {
    use mod.prelude.fab.*
    use mod.widgets.*

    // A floating card over the view (the tool strips' card).
    let Float = mod.widgets.FabFloat{}
    // A floating row of the chosen tool's tools: as wide as its tools, never wider than the view
    // (then it scrolls sideways: drag it, or the wheel).
    let ToolRow = mod.widgets.FabToolScroll{
        width: Fit{max: FitBound.Rel{base: Base.Full factor: 1.0}}
        height: Fit
        cursor: MouseCursor.Default
        show_bg: true
        draw_bg: mod.draw.FabFloatBg{}
    }
    // The slice status chip's floating face, sized for a pane caption.
    let PaneLabel = Float{
        visible: false
        grab_key_focus: false
        flow: Right
        align: Align{y: 0.5}
        margin: Inset{left: 10 top: 10}
        padding: Inset{left: #(PANE_LABEL_PAD) right: #(PANE_LABEL_PAD) top: 5 bottom: 5}
        icon := mod.widgets.FabIconSmall{ visible: false margin: Inset{right: 4} }
        label := mod.widgets.FabLabelSmall{ text: "" }
    }
    let Tool = mod.widgets.FabTool{}
    // A toggle of the view bar, as tall as the render switch's segments.
    // A tile of View options' grid: its icon over its name, lit (the accent, black ink) while on.
    let OptionTile = mod.widgets.FabIconCheck{
        width: 88 height: 60
        flow: Down align: Align{x: 0.5 y: 0.5}
        padding: Inset{left: 4 right: 4 top: 8 bottom: 6}
        icon_walk: Walk{width: 18 height: 18}
        label_walk: Walk{width: Fit height: Fit margin: Inset{top: 6}}
        draw_text +: {
            color: fab.color_text
            color_active: fab.color_text_on_accent
            text_style: theme.font_regular{font_size: fab.font_size_small}
        }
    }
    // A pill of the view pie: its name on the floating cards' ground, lit (the accent, black
    // ink) while the pointer points at it (`hot`).
    let PiePill = View{
        width: #(PIE_PILL.x) height: #(PIE_PILL.y)
        align: Align{x: 0.5 y: 0.5}
        show_bg: true
        draw_bg +: {
            hot: instance(0.0)
            pixel: fn() {
                let sdf = Sdf2d.viewport(self.pos * self.rect_size)
                sdf.box(0.5, 0.5, self.rect_size.x - 1.0, self.rect_size.y - 1.0, self.rect_size.y * 0.5 - 0.5)
                sdf.fill_keep(mix(fab.color_float, fab.color_accent, self.hot))
                sdf.stroke(fab.color_float_border, 1.0)
                return sdf.result
            }
        }
        label := mod.widgets.FabLabel{
            text: ""
            draw_text +: {
                hot: instance(0.0)
                get_color: fn() { return mix(fab.color_text, fab.color_text_on_accent, self.hot) }
            }
        }
    }
    let ViewToggle = mod.widgets.FabIconCheck{
        width: 28
        height: 24
        padding: Inset{left: 6 right: 6 top: 4 bottom: 4}
    }
    // A button of the view bar (an action, not a toggle), as big as its toggles.
    let ViewButton = mod.widgets.FabIconButton{
        width: 28
        height: 24
        padding: Inset{left: 6 right: 6 top: 4 bottom: 4}
        icon_walk: Walk{width: 16 height: 16}
    }
    let Sep = mod.widgets.FabToolSep{}
    // the separator in a row of tools (the context bar's)
    let VSep = View{ width: 1 height: 20 margin: Inset{left: 5 right: 5} show_bg: true draw_bg +: {
        color: uniform(#xffffff1a)
        pixel: fn() { return vec4(self.color.xyz * self.color.w, self.color.w) }
    } }

    // The primary pane keeps all tools and gizmos, clipped to its own rectangle.
    let Primary = View{
        width: Fill
        height: Fill
        flow: Overlay
        // Frontmost controls, then Mechanics' picker, then the viewport.
        event_order: EventOrder.Up
        show_bg: true
        draw_bg +: { color: fab.color_vp_bg_bottom }

        viewport := FabViewport{}
        design_overlay := View{
            width: Fill height: Fill flow: Overlay
            tool_overlay := FabToolOverlay{}
            part_gizmo := mod.widgets.FabPartGizmo{}
            shape_handles := mod.widgets.FabShapeHandles{}
            test_cube := mod.widgets.FabTestCube{}
            force_editor := mod.widgets.FabForceEditor{}
        }
        mech_grab := mod.widgets.FabMechGrab{}

        // ---- model view ----
        model_ui := View{
            width: Fill
            height: Fill
            flow: Overlay
            left := View{
                width: Fit
                height: Fill
                flow: Right
                spacing: 4
                // under the view's toolbar, a gap clear of it
                padding: Inset{left: 10 top: #(crate::ui::area_notch::TOOLBAR_H + 10.0)}
                // The rail and the shape bar draw in lists of their own: the view redrawing
                // (each step of a drag) reuses them. What they show is set as the stage draws.
                tools := Float{
                    new_batch: true
                    flow: Down
                    padding: 3
                    spacing: 1
                    FabTip{ text: #(keymap::tooltip(Key::Select))
                        t_select := Tool{ a11y: {label: "Select"} draw_icon +: { svg: crate_resource("self://resources/icons/select.svg") } }
                    }
                    FabTip{ text: #(keymap::tooltip(Key::Move))
                        t_move := Tool{ a11y: {label: "Move"} draw_icon +: { svg: crate_resource("self://resources/icons/move.svg") } }
                    }
                    rotate_tip := FabTip{ text: #(keymap::tooltip(Key::Rotate))
                        t_rotate := Tool{ a11y: {label: "Turn"} draw_icon +: { svg: crate_resource("self://resources/icons/rotate.svg") } }
                    }
                    FabTip{ text: #(keymap::tooltip(Key::Scale))
                        t_scale := Tool{ a11y: {label: "Scale"} draw_icon +: { svg: crate_resource("self://resources/icons/scale.svg") } }
                    }
                    FabTip{ text: #(keymap::tooltip(Key::Shape))
                        t_shape := Tool{ a11y: {label: "Edit shape"} draw_icon +: { svg: crate_resource("self://resources/icons/shape.svg") } }
                    }
                    FabTip{ text: #(format!("{}: its tools open at the top of the view", keymap::tooltip(Key::Paint)))
                        t_paint := Tool{ a11y: {label: "Paint"} draw_icon +: { svg: crate_resource("self://resources/icons/paint.svg") } }
                    }
                    FabTip{ text: #(format!("{}: its tools open at the top of the view", keymap::tooltip(Key::Sculpt)))
                        t_sculpt := Tool{ a11y: {label: "Sculpt"} draw_icon +: { svg: crate_resource("self://resources/icons/sculpt.svg") } }
                    }
                    FabTip{ text: #(format!("{}: its kinds open at the top of the view", keymap::tooltip(Key::Force)))
                        t_force := Tool{ a11y: {label: "Force"} draw_icon +: { svg: crate_resource("self://resources/icons/force.svg") } }
                    }
                    Sep{}
                    section_tip := FabTip{ text: #(keymap::tooltip(Key::Section))
                        t_clip := Tool{ a11y: {label: "Clip"} draw_icon +: { svg: crate_resource("self://resources/icons/clip.svg") } }
                    }
                    FabTip{ text: #(keymap::tooltip(Key::Measure))
                        t_measure := Tool{ a11y: {label: "Measure"} draw_icon +: { svg: crate_resource("self://resources/icons/measure.svg") } }
                    }
                    no_trees_tip := FabTip{ text: "No trees here: drag on the bed where no tree support may stand or grow (m.no_trees in the nearest object's script). Alt draws a disc"
                        t_no_trees := Tool{ a11y: {label: "No trees here"} draw_icon +: { svg: crate_resource("self://resources/icons/no_trees.svg") } }
                    }
                    settings_here_tip := FabTip{ text: #(SETTINGS_HERE_TIP)
                        t_settings_here := Tool{ a11y: {label: "Print settings here"} draw_icon +: { svg: crate_resource("self://resources/icons/settings_here.svg") } }
                    }
                    Sep{}
                    // Arrange: its row across the top (the gap, and the button that arranges)
                    FabTip{ text: "Arrange every part on the plate: set the gap, then Arrange"
                        arrange_all := Tool{ independent: true a11y: {label: "Arrange plate"} draw_icon +: { svg: crate_resource("self://resources/icons/arrange.svg") } }
                    }
                }
            }
            // Test prints: the tests and their properties in a floating sidebar at the view's bottom
            // right (the toolbar's Tests), growing upward no higher than under the gizmo's column
            tests_side := View{
                visible: false
                width: Fill height: Fill flow: Right
                align: Align{x: 1.0 y: 1.0}
                padding: Inset{top: #(crate::ui::area_notch::TOOLBAR_H + 116.0) right: 10 bottom: 10}
                // as high as its tests need, never past the view's room
                tests_card := Float{
                    width: 380 height: Fit{max: FitBound.Rel{base: Base.Full factor: 1.0}} flow: Down
                    padding: Inset{left: 4 right: 4 top: 6 bottom: 6}
                    View{
                        width: Fill height: Fit flow: Right align: Align{y: 0.5}
                        padding: Inset{left: 8 right: 2 bottom: 4}
                        mod.widgets.FabLabel{ width: Fill text: "Tests" draw_text +: { text_style: theme.font_bold{font_size: fab.font_size_ui} } }
                        side_add_tip := FabTip{ text: #(crate::ui::test_prints::ADD_TIP)
                            side_add := ViewButton{ a11y: {label: "Add test cube"} draw_icon +: { svg: crate_resource("self://resources/icons/plus.svg") } }
                        }
                        FabTip{ text: "Close the tests"
                            tests_close := ViewButton{ a11y: {label: "Close tests"} draw_icon +: { svg: crate_resource("self://resources/icons/close.svg") } }
                        }
                    }
                    tests_totals := mod.widgets.FabLabelSmall{ visible: false width: Fill margin: Inset{left: 8 right: 8 bottom: 4} text: "" }
                    tests_scroll := mod.widgets.FabScroll{
                        height: Fit{max: FitBound.Rel{base: Base.Full factor: 1.0}}
                        padding: Inset{left: 2 right: 2 top: 0}
                        tests := mod.widgets.FabTestSections{}
                    }
                }
            }
            // Design: the picked part's editor in a sidebar at the view's right edge, under the
            // gizmo's column (the toolbar's Part), while a part is picked and the view has room
            // for it; at the view's foot, as tall as its sections, never past the view's room
            // (then it scrolls).
            part_side := View{
                visible: false
                width: Fill height: Fill flow: Right
                align: Align{x: 1.0 y: 0.0}
                padding: Inset{top: #(crate::ui::area_notch::TOOLBAR_H + 116.0) right: #(PART_SIDE_RIGHT) bottom: 10}
                part_col := View{
                    width: Fit height: Fill flow: Down
                    // the room the card may grow into, upward from its foot
                    part_room := View{
                        width: Fit height: Fill flow: Down
                        align: Align{y: 1.0}
                        part_card := Float{
                            width: #(PART_SIDE_W) height: Fit{max: FitBound.Rel{base: Base.Full factor: 1.0}} flow: Overlay
                            padding: Inset{left: 3 right: 3 top: 3 bottom: 3}
                            // on the panels' ground, so its sections sit on their own colour
                            draw_bg +: { color: fab.color_editor }
                            part := mod.widgets.FabPartPanel{
                                show_bg: false
                                height: Fit{max: FitBound.Rel{base: Base.Full factor: 1.0}}
                                editor_bar +: {
                                    padding: Inset{left: 8 right: 2}
                                    draw_bg +: { line: 0.0 }
                                    rule +: { visible: false }
                                    title +: { visible: true }
                                    side_close +: { visible: true }
                                }
                                body +: { height: Fit{max: FitBound.Rel{base: Base.Full factor: 1.0}} }
                            }
                            // its left edge: a drag makes the sidebar wider or narrower
                            part_grip := View{
                                width: 6 height: Fill
                                margin: Inset{left: -3}
                                cursor: MouseCursor.EwResize
                                show_bg: true
                                draw_bg +: {
                                    hover: instance(0.0)
                                    pixel: fn() {
                                        let line = 1.0 - step(1.0, abs(self.pos.x * self.rect_size.x - 3.0))
                                        return vec4(fab.color_accent.xyz, 1.0) * line * self.hover
                                    }
                                }
                            }
                        }
                    }
                }
            }
            // The chosen tool's own tools (Paint, Edit shape, Sculpt, Force): a floating row across the
            // top of the view under its toolbar, centred in the room right of the rail.
            context := View{
                width: Fill
                height: Fit
                align: Align{x: 0.5}
                // clear of the rail on the left and the axis gizmo on the right
                padding: Inset{left: 58 right: 96 top: #(crate::ui::area_notch::TOOLBAR_H + 10.0)}
                    // The paint tools and the brush's settings and colour, for as long as Paint is the tool.
                    paint_bar := ToolRow{
                        visible: false
                        flow: Right
                        align: Align{y: 0.5}
                        padding: Inset{left: 8 right: 8 top: 3 bottom: 3}
                        spacing: 1
                        a11y: {role: "radio_group" label: "Paint tools"}
                        FabTip{ text: #(format!("{}: drag on a part. Hold {} to erase", keymap::tooltip(Key::PaintBrush), keymap::shortcut(Key::PaintInvert)))
                            p_brush := Tool{ a11y: {label: "Paint brush"} draw_icon +: { svg: crate_resource("self://resources/icons/paint.svg") } }
                        }
                        FabTip{ text: #(format!("{}: paint the part's own colour back. Hold {} to paint", keymap::tooltip(Key::PaintErase), keymap::shortcut(Key::PaintInvert)))
                            p_erase := Tool{ a11y: {label: "Erase"} draw_icon +: { svg: crate_resource("self://resources/icons/erase.svg") } }
                        }
                        FabTip{ text: #(format!("{}: click a feature to fill it; {} adds more. Hold {} to erase", keymap::tooltip(Key::PaintFeature), keymap::shortcut(Key::PaintExtend), keymap::shortcut(Key::PaintInvert)))
                            p_feature := Tool{ a11y: {label: "Fill feature"} draw_icon +: { svg: crate_resource("self://resources/icons/fill_feature.svg") } }
                        }
                        FabTip{ text: #(format!("{}: click a part to fill all of it. Hold {} to erase", keymap::tooltip(Key::PaintPart), keymap::shortcut(Key::PaintInvert)))
                            p_part := Tool{ a11y: {label: "Fill part"} draw_icon +: { svg: crate_resource("self://resources/icons/fill_part.svg") } }
                        }
                        FabTip{ text: #(format!("{}: the fill spreads until an edge sharper than the angle. Hold {} to erase", keymap::tooltip(Key::PaintFlood), keymap::shortcut(Key::PaintInvert)))
                            p_smooth := Tool{ a11y: {label: "Smooth fill"} draw_icon +: { svg: crate_resource("self://resources/icons/fill_smooth.svg") } }
                        }
                        FabTip{ text: #(format!("{}: click a part to fill it with a pattern of its own colour and the paint colour, through its volume", keymap::tooltip(Key::PaintPattern)))
                            p_pattern := Tool{ a11y: {label: "Pattern fill"} draw_icon +: { svg: crate_resource("self://resources/icons/fill_pattern.svg") } }
                        }
                        dials_sep := VSep{}
                        FabTip{ text: #(format!("Brush diameter in mm ({} in the view, {} and {}): drag up or down, or scroll", keymap::shortcut(Key::PaintSizeScroll), keymap::shortcut(Key::PaintSmaller), keymap::shortcut(Key::PaintLarger)))
                            diameter := mod.widgets.FabBrushDial{ width: 38}
                        }
                        FabTip{ text: "Smooth fill angle: the sharpest edge it spreads over. Drag up or down, or scroll"
                            angle := mod.widgets.FabBrushDial{ width: 38 angle: true }
                        }
                        FabTip{ text: "Pattern: cells, noise, marble, bands or a ramp up the part. Click for the next"
                            pattern_kind := mod.widgets.FabPatternDial{ width: 38 which: 0 }
                        }
                        FabTip{ text: "Pattern size in mm: a cell, a swirl, a band. Drag up or down, or scroll"
                            pattern_scale := mod.widgets.FabPatternDial{ width: 38 which: 1 }
                        }
                        FabTip{ text: "Pattern seed: another number, another pattern of the kind. Drag up or down, or scroll"
                            pattern_seed := mod.widgets.FabPatternDial{ width: 38 which: 2 }
                        }
                        FabTip{ text: "Sharpness: 0 blends the colours (printed dithered), 1 turns one into the other at once. Drag up or down, or scroll"
                            pattern_sharp := mod.widgets.FabPatternDial{ width: 38 which: 3 }
                        }
                        FabTip{ text: "Balance: more of the paint colour (up) or of the part's own (down). Drag up or down, or scroll"
                            pattern_balance := mod.widgets.FabPatternDial{ width: 38 which: 4 }
                        }
                        palette_sep := VSep{}
                        palette_tip := FabTip{ text: "Material colour: click a swatch to paint with it"
                            palette := mod.widgets.FabPaintPalette{ width: Fit }
                        }
                        supports_sep := VSep{}
                        FabTip{ text: "Block supports: paint where no support may stand or touch (m.support_block). Erase takes it off"
                            p_block := Tool{ independent: true a11y: {label: "Block supports"} draw_icon +: { svg: crate_resource("self://resources/icons/support_block.svg") } }
                        }
                        FabTip{ text: "Enforce supports: paint where supports always go, however shallow the face (m.support_enforce). Erase takes it off"
                            p_enforce := Tool{ independent: true a11y: {label: "Enforce supports"} draw_icon +: { svg: crate_resource("self://resources/icons/supports.svg") } }
                        }
                    }
                    shape_bar := ToolRow{
                        new_batch: true
                        visible: false
                        flow: Right
                        align: Align{y: 0.5}
                        padding: Inset{left: 8 right: 8 top: 3 bottom: 3}
                        spacing: 1
                        a11y: {role: "radio_group" label: "Shape tools"}
                        // What a click picks (for the AI's chat): the kind filter.
                        FabTip{ text: "Pick any: what lies under the pointer, an edge near it, else the face or feature"
                            k_any := Tool{ a11y: {label: "Pick any"} draw_icon +: { svg: crate_resource("self://resources/icons/pick_any.svg") } }
                        }
                        FabTip{ text: "Pick faces: a part's face (on a part drawn from its field, the edit)"
                            k_face := Tool{ a11y: {label: "Pick faces"} draw_icon +: { svg: crate_resource("self://resources/icons/pick_face.svg") } }
                        }
                        FabTip{ text: "Pick edges: a part's edge (on a part drawn from its field, a crease or an edit's edges)"
                            k_edge := Tool{ a11y: {label: "Pick edges"} draw_icon +: { svg: crate_resource("self://resources/icons/pick_edge.svg") } }
                        }
                        FabTip{ text: "Pick features: a hole, a boss, a fillet, as the model names them"
                            k_feature := Tool{ a11y: {label: "Pick features"} draw_icon +: { svg: crate_resource("self://resources/icons/pick_feature.svg") } }
                        }
                        FabTip{ text: "Pick bodies: the whole solid under the pointer"
                            k_body := Tool{ a11y: {label: "Pick bodies"} draw_icon +: { svg: crate_resource("self://resources/icons/pick_body.svg") } }
                        }
                        VSep{}
                        FabTip{ text: "Subtract a box from the picked node (or the part): drag its handles to place it"
                            s_cut_box := Tool{ a11y: {label: "Subtract box"} draw_icon +: { svg: crate_resource("self://resources/icons/cut_box.svg") } }
                        }
                        FabTip{ text: "Subtract a cylinder from the picked node (or the part): drag its handles to place it"
                            s_cut_cyl := Tool{ a11y: {label: "Subtract cylinder"} draw_icon +: { svg: crate_resource("self://resources/icons/cut_cylinder.svg") } }
                        }
                        FabTip{ text: "Subtract a sphere from the picked node (or the part): drag its handles to place it"
                            s_cut_sphere := Tool{ a11y: {label: "Subtract sphere"} draw_icon +: { svg: crate_resource("self://resources/icons/cut_sphere.svg") } }
                        }
                        VSep{}
                        FabTip{ text: "Add box to part: a plinth under the part to stand it on"
                            s_add_box := Tool{ a11y: {label: "Add box to part"} draw_icon +: { svg: crate_resource("self://resources/icons/add_box.svg") } }
                        }
                        FabTip{ text: "Add cylinder to part: a disc under the part to stand it on"
                            s_add_cyl := Tool{ a11y: {label: "Add cylinder to part"} draw_icon +: { svg: crate_resource("self://resources/icons/add_cylinder.svg") } }
                        }
                        FabTip{ text: "Add sphere to part: attach it to the picked node (or the part) and drag its handles to place it"
                            s_add_sphere := Tool{ a11y: {label: "Add sphere to part"} draw_icon +: { svg: crate_resource("self://resources/icons/add_sphere.svg") } }
                        }
                        VSep{}
                        FabTip{ text: "Cut bottom: cut everything below a level just over the bed (drag its handle to set the height)"
                            s_cut_bottom := Tool{ a11y: {label: "Cut bottom"} draw_icon +: { svg: crate_resource("self://resources/icons/cut_bottom.svg") } }
                        }
                        FabTip{ text: "Cut with a plane: a slab to move and turn; keeps the other side"
                            s_cut_plane := Tool{ a11y: {label: "Cut with plane"} draw_icon +: { svg: crate_resource("self://resources/icons/cut_plane.svg") } }
                        }
                        FabTip{ text: "Keep both halves, add pins: split the part where its cut plane is (or level through its middle) with alignment pins sized for the printer; each half goes on the plate flat side down"
                            s_split := Tool{ a11y: {label: "Keep both halves, add pins"} draw_icon +: { svg: crate_resource("self://resources/icons/split_pins.svg") } }
                        }
                        VSep{}
                        FabTip{ text: "Place on plate: sit the part's lowest point on the bed"
                            s_place := Tool{ a11y: {label: "Place on plate"} draw_icon +: { svg: crate_resource("self://resources/icons/place_plate.svg") } }
                        }
                        FabTip{ text: #(format!("Lay flat: click a surface of the part, or of a test print's piece, to turn it face down onto the bed ({} cancels)", keymap::shortcut(Key::Cancel)))
                            s_lay_flat := Tool{ independent: true a11y: {label: "Lay flat"} draw_icon +: { svg: crate_resource("self://resources/icons/lay_flat.svg") } }
                        }
                    }
                    sculpt_bar := ToolRow{
                        visible: false
                        flow: Right
                        align: Align{y: 0.5}
                        padding: Inset{left: 8 right: 8 top: 3 bottom: 3}
                        spacing: 1
                        a11y: {role: "radio_group" label: "Sculpt tools"}
                        FabTip{ text: #(format!("{}: drag on a part. Hold {} to lower", keymap::tooltip(Key::SculptRaise), keymap::shortcut(Key::SculptInvert)))
                            s_raise := Tool{ a11y: {label: "Raise"} draw_icon +: { svg: crate_resource("self://resources/icons/sculpt_raise.svg") } }
                        }
                        FabTip{ text: #(format!("{}: drag on a part. Hold {} to raise", keymap::tooltip(Key::SculptLower), keymap::shortcut(Key::SculptInvert)))
                            s_lower := Tool{ a11y: {label: "Lower"} draw_icon +: { svg: crate_resource("self://resources/icons/sculpt_lower.svg") } }
                        }
                        FabTip{ text: #(format!("{}: cut a sharp groove", keymap::tooltip(Key::SculptCrease)))
                            s_crease := Tool{ a11y: {label: "Crease"} draw_icon +: { svg: crate_resource("self://resources/icons/sculpt_crease.svg") } }
                        }
                        VSep{}
                        FabTip{ text: #(format!("Brush diameter in mm ({} in the view, {} and {}): drag up or down, or scroll", keymap::shortcut(Key::SculptSizeScroll), keymap::shortcut(Key::SculptSmaller), keymap::shortcut(Key::SculptLarger)))
                            diameter := mod.widgets.FabBrushDial{ width: 38 sculpt: true }
                        }
                        FabTip{ text: "Sculpt strength (0.1 to 1.0): drag up or down, or scroll"
                            strength := mod.widgets.FabBrushDial{ width: 38 sculpt: true strength: true }
                        }
                    }
                    // Arrange: the room it keeps between parts, and the button that arranges them all.
                    arrange_bar := ToolRow{
                        visible: false
                        flow: Right
                        align: Align{y: 0.5}
                        padding: Inset{left: 8 right: 4 top: 3 bottom: 3}
                        spacing: 6
                        a11y: {label: "Arrange"}
                        FabTip{ text: "The gap Arrange keeps between parts"
                            arrange_gap := mod.widgets.ValueInput{width: 110 height: 26 label: "Gap" a11y: {label: "Arrange gap"} min: 0.0 max: 0.0 step: 0.5 precision: 1 suffix: " mm"}
                        }
                        FabTip{ text: "Arrange every part on the plate with this gap"
                            arrange_go := mod.widgets.FabButtonAccent{ text: "Arrange" height: 26 padding: Inset{left: 12 right: 12} }
                        }
                        VSep{}
                        FabTip{ text: "Done arranging"
                            arrange_done := ViewButton{ a11y: {label: "Done arranging"} draw_icon +: { svg: crate_resource("self://resources/icons/close.svg") } }
                        }
                    }
                    // Measure Fit: turn the fitted part a quarter, or clear the fit.
                    fit_bar := ToolRow{
                        visible: false
                        flow: Right
                        align: Align{y: 0.5}
                        padding: 3
                        spacing: 1
                        a11y: {label: "Measure Fit tools"}
                        FabTip{ text: #(keymap::tooltip(Key::FitTurn))
                            fit_turn := Tool{ independent: true a11y: {label: "Turn 90°"} draw_icon +: { svg: crate_resource("self://resources/icons/rotate.svg") } }
                        }
                        FabTip{ text: #(keymap::tooltip(Key::FitClear))
                            fit_clear := Tool{ independent: true a11y: {label: "Clear fit"} draw_icon +: { svg: crate_resource("self://resources/icons/close.svg") } }
                        }
                    }
                    // The Force tool's kinds: what a click on the studied part adds. Fab reads the
                    // click as a face or a hole's wall by itself; Alt takes the other reading.
                    force_bar := ToolRow{
                        visible: false
                        flow: Right
                        align: Align{y: 0.5}
                        padding: Inset{left: 8 right: 8 top: 3 bottom: 3}
                        spacing: 1
                        a11y: {role: "radio_group" label: "Force kinds"}
                        FabTip{ text: #(format!("{}. {} reads a hole's wall as a face", keymap::tooltip(Key::ForceFixed), keymap::shortcut(Key::ForceFlip)))
                            f_fixed := Tool{ a11y: {label: "Fixed"} draw_icon +: { svg: crate_resource("self://resources/icons/force_fixed.svg") } }
                        }
                        FabTip{ text: #(format!("{}. {} reads a hole's wall as a face", keymap::tooltip(Key::ForceLoad), keymap::shortcut(Key::ForceFlip)))
                            f_force := Tool{ a11y: {label: "Force"} draw_icon +: { svg: crate_resource("self://resources/icons/force_arrow.svg") } }
                        }
                        FabTip{ text: #(format!("{}. {} reads a hole's wall as a face", keymap::tooltip(Key::ForcePressure), keymap::shortcut(Key::ForceFlip)))
                            f_pressure := Tool{ a11y: {label: "Pressure"} draw_icon +: { svg: crate_resource("self://resources/icons/force_pressure.svg") } }
                        }
                        FabTip{ text: #(format!("{}. {} reads a hole's wall as a face", keymap::tooltip(Key::ForceTorque), keymap::shortcut(Key::ForceFlip)))
                            f_torque := Tool{ a11y: {label: "Torque"} draw_icon +: { svg: crate_resource("self://resources/icons/force_torque.svg") } }
                        }
                        VSep{}
                        FabTip{ text: "Size: of the next load (N, MPa or N mm), or of the load picked. Drag up or down, or scroll"
                            force_size := mod.widgets.FabBrushDial{ width: 38 force: true }
                        }
                        VSep{}
                        FabTip{ text: #(format!("{}: the load picked (click it, or in the feature tree)", keymap::tooltip(Key::ForceRemove)))
                            f_remove := Tool{ independent: true a11y: {label: "Remove load"} draw_icon +: { svg: crate_resource("self://resources/icons/trash.svg") } }
                        }
                    }
            }
            bottom := View{
                width: Fill
                height: Fill
                flow: Down
                Filler{}
                // Why the plate does not slice, in every workspace with the model.
                slice_failed := Float{
                    visible: false
                    width: Fit
                    height: Fit
                    margin: Inset{left: 10 bottom: 6}
                    padding: Inset{left: 12 right: 12 top: 8 bottom: 8}
                    slice_failed_text := mod.widgets.FabLabel{ text: "" a11y: {label: "Why the plate does not slice"} }
                }
                // The part shows as it was at a step of its build (the Steps editor's marker).
                step_shown := Float{
                    visible: false
                    width: Fit
                    height: Fit
                    flow: Right
                    spacing: 10
                    align: Align{y: 0.5}
                    margin: Inset{left: 10 bottom: 6}
                    padding: Inset{left: 12 right: 8 top: 4 bottom: 4}
                    step_shown_text := mod.widgets.FabLabel{ text: "" a11y: {label: "The step the view shows"} }
                    step_back := mod.widgets.FabButton{ text: "Back to the end" a11y: {label: "Back to the end"} }
                    step_rollback := mod.widgets.FabButton{ text: "Roll back to here" }
                }
            }
        }

        // ---- Mechanics: tools (each tool's help is its tooltip) ----
        mechanics_ui := View{
            visible: false width: Fill height: Fill flow: Overlay
            left := View{
                // under the header strip (`MECH_TOOLS_GAP` below it)
                width: Fit height: Fill flow: Down padding: Inset{left: 10 top: 40}
                tools := Float{
                    flow: Down padding: 3 spacing: 1
                    a11y: {role: "radio_group" label: "Mechanics tools"}
                    FabTip{ place: TipPlace.Right wrap_width: 360 text: #(format!("{}: drag a part to hold and move it. {} moves it nearer or farther. {} to orbit (bare drag off a part).", keymap::tooltip(Key::MechGrab), keymap::shortcut(Key::MechDepth), keymap::shortcut(Key::MechOrbit)))
                        t_grab := Tool{ a11y: {label: "Grab"} draw_icon +: { svg: crate_resource("self://resources/icons/grab.svg") } }
                    }
                    FabTip{ place: TipPlace.Right wrap_width: 360 text: #(format!("{}: drag a part, let go to throw it. {} moves it nearer or farther. {} to orbit (bare drag off a part).", keymap::tooltip(Key::MechThrow), keymap::shortcut(Key::MechDepth), keymap::shortcut(Key::MechOrbit)))
                        t_throw := Tool{ a11y: {label: "Throw"} draw_icon +: { svg: crate_resource("self://resources/icons/throw.svg") } }
                    }
                    FabTip{ place: TipPlace.Right wrap_width: 360 text: #(format!("{}: click a part to pin it at that point; click a pinned part to unpin it. {} to orbit (bare drag off a part).", keymap::tooltip(Key::MechPin), keymap::shortcut(Key::MechOrbit)))
                        t_pin := Tool{ a11y: {label: "Pin"} draw_icon +: { svg: crate_resource("self://resources/icons/pin.svg") } }
                    }
                    FabTip{ place: TipPlace.Right wrap_width: 360 text: #(format!("{}: drag a part to hang it from the pointer and let it swing. {} moves it nearer or farther. {} to orbit (bare drag off a part).", keymap::tooltip(Key::MechDangle), keymap::shortcut(Key::MechDepth), keymap::shortcut(Key::MechOrbit)))
                        t_dangle := Tool{ a11y: {label: "Dangle"} draw_icon +: { svg: crate_resource("self://resources/icons/dangle.svg") } }
                    }
                    FabTip{ place: TipPlace.Right wrap_width: 360 text: #(format!("{}: click a part to push it away from you. {} to orbit (bare drag off a part).", keymap::tooltip(Key::MechPoke), keymap::shortcut(Key::MechOrbit)))
                        t_poke := Tool{ a11y: {label: "Poke"} draw_icon +: { svg: crate_resource("self://resources/icons/poke.svg") } }
                    }
                }
            }
            actions := mod.widgets.FabMechActions{}
        }

        // ---- slice view ----
        slice_ui := View{
            visible: false
            width: Fill
            height: Fill
            flow: Overlay
            // a resin printer's slice: the shown layer flat, beside the playback card (whose pad
            // walks the stack)
            resin_ui := View{
                visible: false
                width: Fill
                height: Fill
                flow: Down
                Filler{}
                resin_row := View{
                    width: Fill
                    height: Fit
                    flow: Right
                    align: Align{y: 1.0}
                    spacing: 10
                    padding: Inset{left: #(56.0 + PAD_CARD_W + 10.0) bottom: 12}
                    resin_panel := mod.widgets.FabResinPanel{}
                    FabTip{ text: "Follow the printer again: the view shows the layer it cured last"
                        resin_live := mod.widgets.FabButton{ visible: false text: "Back to live" }
                    }
                }
            }
            // The playback, in a floating card at the view's bottom left: the pad (a Kaoss pad's
            // square: up it the layers, across it the moves within the layer), the transport and
            // the time under it, then the layer and what prints there.
            bottom := View{
                width: Fill
                height: Fill
                flow: Down
                align: Align{x: 0.0}
                Filler{}
                bar_row := View{
                    width: Fill height: Fit flow: Right
                    // Clear of the tool strip that stays over Design's slice.
                    legend_gap := View{ width: 44 height: 1 }
                    bar := Float{
                        width: #(PAD_CARD_W)
                        flow: Down
                        margin: Inset{left: 12 bottom: 12}
                        padding: Inset{left: 8 right: 8 top: 8 bottom: 8}
                        spacing: 6
                        follow_row := View{
                            visible: false
                            width: Fill height: Fit flow: Down spacing: 6 padding: Inset{left: 2 top: 2}
                            FabTip{ text: "Follow the printer again: the view shows the layer it prints, the head where Fab's slice timing puts it"
                                back_live := mod.widgets.FabButton{ visible: false text: "Back to live" }
                            }
                            follow_text := mod.widgets.FabLabelSmall{ width: Fill text: "" }
                        }
                        job_box := View{
                            visible: false
                            width: Fill height: Fit padding: Inset{left: 2 right: 2 bottom: 4}
                            job_bar := mod.widgets.FabJobTimeline{}
                        }
                        row := View{
                            width: Fill height: Fit flow: Down spacing: 6
                            FabTip{ text: "Drag on the pad: up it the layers, across it the moves within the layer"
                                time_scrub := mod.widgets.FabSlicePad{}
                            }
                            // to the start of the layer, play, to its end, the speed; the time at
                            // the right end
                            transport := View{
                                width: Fill height: Fit flow: Right align: Align{y: 0.5} spacing: 2
                                FabTip{ text: "Back to the start of the layer"
                                    to_start := mod.widgets.FabIconButton{ a11y: {label: "Start of layer"} width: 28 height: 28 padding: Inset{left: 7 right: 7 top: 7 bottom: 7} draw_icon +: { svg: crate_resource("self://resources/icons/skip_back.svg") } }
                                }
                                FabTip{ text: "Play the print"
                                    print_play := mod.widgets.FabIconButton{ a11y: {label: "Play"} width: 32 height: 32 padding: Inset{left: 8 right: 8 top: 8 bottom: 8} draw_icon +: { svg: crate_resource("self://resources/icons/play.svg") } }
                                }
                                FabTip{ text: "Pause"
                                    print_pause := mod.widgets.FabIconButton{ a11y: {label: "Pause"} visible: false width: 32 height: 32 padding: Inset{left: 8 right: 8 top: 8 bottom: 8} draw_icon +: { svg: crate_resource("self://resources/icons/pause.svg") } }
                                }
                                FabTip{ text: "To the end of the layer"
                                    to_end := mod.widgets.FabIconButton{ a11y: {label: "End of layer"} width: 28 height: 28 padding: Inset{left: 7 right: 7 top: 7 bottom: 7} draw_icon +: { svg: crate_resource("self://resources/icons/skip_fwd.svg") } }
                                }
                                FabTip{ text: "Playback speed"
                                    speed := mod.widgets.FabDropdownButton{ tag: @fab_stage_speed owner: @fab_stage_speed label +: { text: "60×" } }
                                }
                                Filler{}
                                time_label := mod.widgets.FabLabelSmall{ width: Fit text: "" margin: Inset{right: 2} }
                            }
                            // the layer, then what prints there (it wraps)
                            meta := View{
                                width: Fill height: Fit flow: Down padding: Inset{left: 2 right: 2}
                                info := View{
                                    width: Fill height: Fit flow: Down spacing: 3
                                    layer_label := mod.widgets.FabLabel{ width: Fill text: "" }
                                    now := mod.widgets.FabLabelSmall{ width: Fill text: "" draw_text +: { color: fab.color_text_muted } }
                                }
                            }
                        }
                    }
                }
            }
            // The slice's state over the view: never a modal, the view
            // stays live (an out-of-date slice shows faded under it).
            chip_layer := View{
                // (shown with something to say: `slice_status`)
                visible: false
                width: Fill
                height: Fit
                flow: Down
                align: Align{x: 0.5}
                padding: Inset{top: 46}
                chip := Float{
                    flow: Right
                    align: Align{y: 0.5}
                    spacing: 10
                    padding: Inset{left: 14 right: 14 top: 7 bottom: 7}
                    chip_text := mod.widgets.FabLabel{ text: "" }
                    chip_bar := mod.widgets.FabProgress{ width: 90 height: 4 visible: false }
                }
            }
        }

        // ---- Monitor: a job Fab has no slice of ----
        monitor_ui := View{
            visible: false
            width: Fill
            height: Fill
            flow: Down
            align: Align{x: 0.5 y: 0.5}
            unknown := Float{
                width: 420
                flow: Down
                spacing: 8
                padding: Inset{left: 20 right: 20 top: 16 bottom: 16}
                unknown_title := mod.widgets.FabTitleLabel{ text: "" }
                unknown_why := mod.widgets.FabLabelDim{ width: Fill text: "" }
                unknown_bar := mod.widgets.FabJobTimeline{}
            }
        }

        // The sent job, at the top of the view.
        monitor_job := View{
            visible: false
            width: Fill
            height: Fit
            flow: Down
            padding: Inset{left: 12 right: 112 top: 44}
            card := mod.widgets.FabJobCard{ width: 620 }
            jobs := mod.widgets.FabJobList{ width: 620 }
        }

        // ---- always ----
        render_row := View{
            width: Fill
            height: Fit
            flow: Down
            // The plate view's toolbar, as every editor's: the area's editor selector (the notch
            // layer draws it at the bar's left end), what makes and changes parts, Test prints'
            // group; at its right end what the view shows: the plate's mode, the render mode and
            // the view's options, a rule before the area's close.
            // A held job keeps only the mode (it has no other look); Mechanics has its own bar.
            modes_row := mod.widgets.FabEditorBar{
                // (a solid bar over the view, as every editor's toolbar)
                // What makes and changes parts: Add (a new part) and Modify (the selected part's
                // features and shape edits). Handled for the app (`main`).
                create_card := View{
                    width: Fit height: Fit flow: Right align: Align{y: 0.5} spacing: 1
                    FabTip{ text: "Add a shape or import a model"
                        add := mod.widgets.FabDropdownButton{ a11y: {label: "Add"}
                            tag: @fab_parts_add
                            owner: @fab_parts_add
                            height: 24
                            label +: { text: "Add" }
                        }
                    }
                    FabTip{ text: "Change the selected part: add a feature, cut away or join a primitive, cut it flat or along a plane, split it"
                        modify := mod.widgets.FabDropdownButton{ a11y: {label: "Modify"}
                            tag: @fab_modify
                            owner: @fab_modify
                            height: 24
                            label +: { text: "Modify" }
                        }
                    }
                    // the selected part down onto the bed
                    FabTip{ text: "Drop to plate: sit the selected part on the bed"
                        drop_part := ViewButton{ a11y: {label: "Drop to plate"} draw_icon +: { svg: crate_resource("self://resources/icons/drop.svg") } }
                    }
                }
                // Test prints: a cube on the picked part, and the tests' sidebar
                tests_rule := mod.widgets.FabVRule{ visible: false }
                tests_group := View{
                    visible: false
                    width: Fit height: Fit flow: Right align: Align{y: 0.5} spacing: 1
                    add_test_tip := FabTip{ text: #(crate::ui::test_prints::ADD_TIP)
                        add_test := ViewButton{ a11y: {label: "Add test cube"} draw_icon +: { svg: crate_resource("self://resources/icons/add_box.svg") } }
                    }
                    FabTip{ text: "Tests: the test prints and their properties, in a sidebar at the view's right"
                        tests_toggle := ViewToggle{
                            width: Fit
                            padding: Inset{left: 6 right: 8 top: 4 bottom: 4}
                            text: "Tests"
                            label_walk: Walk{width: Fit height: Fit margin: Inset{left: 5}}
                            a11y: {label: "Tests"}
                            draw_icon +: { svg: crate_resource("self://resources/icons/cut_box.svg") }
                            draw_text +: {
                                color: fab.color_text
                                color_active: fab.color_text_on_accent
                                text_style: theme.font_regular{font_size: fab.font_size_ui}
                            }
                        }
                    }
                }
                // The view's options (what it shows, its framing, the filament colours) in their
                // flyout, at the bar's right end before the area's close, a rule between
                Filler{}
                // Design's Part sidebar at the view's right, shown or put away
                part_tip := FabTip{ text: "Part: the picked part's editor in a sidebar at the view's right"
                    part_toggle := ViewToggle{ visible: false a11y: {label: "Part"} draw_icon +: { svg: crate_resource("self://resources/icons/sidebar.svg") } }
                }
                // why the colours shown are not what will print (a missing spool's colour, ...):
                // a warning mark, the reason in its tooltip
                colour_notice_tip := FabTip{
                    colour_notice := View{
                        visible: false width: 24 height: 24 align: Align{x: 0.5 y: 0.5}
                        cursor: MouseCursor.Hand
                        FabIconSmall{ icon_walk: Walk{width: 14 height: 14} draw_icon +: { color: fab.color_warning svg: crate_resource("self://resources/icons/warning.svg") } }
                    }
                }
                // what the view shows: the plate's mode, its render mode (Normal, Sliced, Force) and
                // the view's options, together at the bar's right end
                mode_card := View{
                    width: Fit height: Fit flow: Right align: Align{y: 0.5}
                    FabTip{ text: "What the plate is for: Design edits it, Mechanics runs it in a physics sim, Monitor shows the printer's job. It stays as it is across workspaces."
                        plate_mode := mod.widgets.FabDropdownButton{
                            height: 24
                            a11y: {label: "Plate mode"}
                            tag: @fab_plate_mode
                            owner: @fab_plate_mode
                            label +: { text: "Design" }
                        }
                    }
                }
                render_tip := FabTip{ text: #(format!("How the plate shows: Normal, Sliced (the print over the plate, reslicing live as you edit) or Force (the selected part's stress) ({})", keymap::shortcut(Key::SliceView)))
                    render_mode := mod.widgets.FabDropdownButton{
                        height: 24
                        a11y: {label: "Render mode"}
                        tag: @fab_render_mode
                        owner: @fab_render_mode
                        label +: { text: "Normal" }
                    }
                }
                FabTip{ text: "View options: the tool rails, the projection, framing, the filament colours and what the slice shows"
                    view_options := mod.widgets.FabDropdownButton{
                        a11y: {label: "View options"}
                        tag: @fab_view_options
                        owner: @fab_view_options
                        height: 24
                        label +: { text: "View options" }
                    }
                }
                view_rule := mod.widgets.FabVRule{}
            }
            // View › See-Through's opacity and glow, while it is on
            see_row := Float{
                visible: false
                flow: Right
                align: Align{y: 0.5}
                spacing: 20
                margin: Inset{top: 4}
                padding: Inset{left: 10 right: 10 top: 3 bottom: 3}
                // Each label sits right before its own slider, the value at the slider's end
                View{
                    width: Fit
                    height: Fit
                    flow: Right
                    align: Align{y: 0.5}
                    spacing: 6
                    FabTip{ text: "How much the part hides what lies behind it: low shows the paint inside and every surface, high shows the outside"
                        mod.widgets.FabLabelSmall{ text: "Opacity" }
                    }
                    see_opacity := mod.widgets.Slider{ width: 150 height: 18 text: "" min: 0.0 max: 1.0 step: 0.01 default: 0.35 precision: 2 }
                }
                View{
                    width: Fit
                    height: Fit
                    flow: Right
                    align: Align{y: 0.5}
                    spacing: 6
                    FabTip{ text: "How brightly the paint glows outside the part: the pattern the shape cuts through, around it"
                        mod.widgets.FabLabelSmall{ text: "Glow" }
                    }
                    see_glow := mod.widgets.Slider{ width: 110 height: 18 text: "" min: 0.0 max: 1.0 step: 0.01 default: 0.35 precision: 2 }
                }
            }
        }
        nav := View{
            width: Fill
            height: Fit
            flow: Right
            // `NAV_TOP`, under Mechanics' header strip
            padding: Inset{top: 6 right: 8}
            Filler{}
            nav_col := View{
                width: 96
                height: Fit
                flow: Down
                align: Align{x: 0.5}
                spacing: 4
                gizmo := FabNavGizmo{ width: 96 }
            }
        }
        // The view pie (Q held): the views round the pointer, each where the view looks from (Top
        // up, Right right, …), Frame selected low right, Frame all low left; releasing Q there
        // takes it (`FabStage::pie`)
        pie_layer := View{
            visible: false
            width: Fill height: Fill flow: Overlay
            pie_hub := View{
                width: #(PIE_HUB) height: #(PIE_HUB)
                show_bg: true
                draw_bg +: {
                    pixel: fn() {
                        let sdf = Sdf2d.viewport(self.pos * self.rect_size)
                        let r = self.rect_size.x * 0.5
                        sdf.circle(r, r, r - 1.0)
                        sdf.fill_keep(fab.color_float)
                        sdf.stroke(fab.color_float_border, 1.0)
                        sdf.circle(r, r, 2.5)
                        sdf.fill(fab.color_text)
                        return sdf.result
                    }
                }
            }
            pie0 := PiePill{ label +: { text: "Top" } }
            pie1 := PiePill{ label +: { text: "Back" } }
            pie2 := PiePill{ label +: { text: "Right" } }
            pie3 := PiePill{ label +: { text: "Frame selected" } }
            pie4 := PiePill{ label +: { text: "Bottom" } }
            pie5 := PiePill{ label +: { text: "Frame all" } }
            pie6 := PiePill{ label +: { text: "Left" } }
            pie7 := PiePill{ label +: { text: "Front" } }
        }
        // View options' flyout: a card under its button at the toolbar's right end, a grid of
        // tiles (icon and name, lit while on); what the slice shows and the board only when they
        // apply. A press outside it, Escape or the button again closes it.
        view_flyout := View{
            visible: false
            width: Fill height: Fill flow: Right
            align: Align{x: 1.0 y: 0.0}
            padding: Inset{top: #(crate::ui::area_notch::TOOLBAR_H + 4.0) right: 44}
            options_card := Float{
                width: 284 height: Fit flow: Down
                padding: 6 spacing: 6
                grid := View{
                    width: Fill height: Fit flow: Right{wrap: true} spacing: 2
                        FabTip{ text: "The tool rails at the view's left"
                            o_rails := OptionTile{ text: "Tool rails" a11y: {label: "Tool rails"} draw_icon +: { svg: crate_resource("self://resources/icons/select.svg") } }
                        }
                        FabTip{ text: #(keymap::tooltip(Key::Ortho))
                            o_ortho := OptionTile{ text: "Orthographic" a11y: {label: "Orthographic"} draw_icon +: { svg: crate_resource("self://resources/icons/ortho.svg") } }
                        }
                        FabTip{ text: #(keymap::tooltip(Key::FrameAll))
                            o_frame_all := OptionTile{ text: "Frame plate" a11y: {label: "Frame plate"} draw_icon +: { svg: crate_resource("self://resources/icons/fit.svg") } }
                        }
                        FabTip{ text: #(keymap::tooltip(Key::FrameSelected))
                            o_frame_sel := OptionTile{ text: "Frame selected" a11y: {label: "Frame selected"} draw_icon +: { svg: crate_resource("self://resources/icons/frame_sel.svg") } }
                        }
                        FabTip{ text: "Each part in the colour of the filament it prints with (off: the model's own colours)"
                            o_colours := OptionTile{ text: "Filament colours" a11y: {label: "Filament colours"} draw_icon +: { svg: crate_resource("self://resources/icons/palette.svg") } }
                        }
                        FabTip{ text: "Show the circuit board in place: it is never printed or sliced"
                            o_board := OptionTile{ text: "Circuit board" a11y: {label: "Circuit board"} draw_icon +: { svg: crate_resource("self://resources/icons/board.svg") } }
                        }
                }
                slice_title := mod.widgets.FabLabelSmall{ visible: false margin: Inset{left: 4 top: 2} text: "SLICE" }
                slice_grid := View{
                    visible: false
                    width: Fill height: Fit flow: Right{wrap: true} spacing: 2
                        FabTip{ text: #(keymap::tooltip(Key::ShowSupports))
                            o_supports := OptionTile{ text: "Supports" a11y: {label: "Supports"} draw_icon +: { svg: crate_resource("self://resources/icons/supports.svg") } }
                        }
                        FabTip{ text: "Show travel"
                            o_travel := OptionTile{ text: "Travel" a11y: {label: "Travel"} draw_icon +: { svg: crate_resource("self://resources/icons/travel.svg") } }
                        }
                        FabTip{ text: "Show the parts under the toolpaths"
                            o_parts := OptionTile{ text: "Parts" a11y: {label: "Parts"} draw_icon +: { svg: crate_resource("self://resources/icons/cube.svg") } }
                        }
                }
                notice := mod.widgets.FabLabelSmall{ visible: false width: Fill margin: Inset{left: 4 right: 4} text: "" draw_text +: { color: fab.color_warning } }
            }
        }
        // Beside the primary tool rail, still in the pane's top-left corner.
        pane_label := PaneLabel{ margin: Inset{left: #(PANE_LABEL_LEFT) top: 10} cursor: MouseCursor.Hand }
        // A job the person waits on (another plate on its way to the bed), over the view
        // (frontmost: its Cancel takes the press).
        progress_card := mod.widgets.FabProgressCard{}
    }

    mod.widgets.FabStageBase = #(FabStage::register_widget(vm))
    mod.widgets.FabStage = set_type_default() do mod.widgets.FabStageBase{
        width: Fill height: Fill flow: Right
        event_order: EventOrder.Up
        primary := Primary{}
    }
}

#[derive(Default)]
struct StageDrawKeys {
    /// Index hint for reading the current workspace's shape without searching the row each draw.
    workspace_index: usize,
    /// Plate and render mode resolution, which can search the workspace's editor tree.
    modes: Option<(ModeKey, Option<PlateMode>, RenderMode)>,
    /// The Monitor job and resin selectors, which can search the saved job list.
    print_chrome: Option<(PrintChromeKey, PrintChrome)>,
    /// Whether the chip shows (an Underside, Synced or traced view).
    pane_layout: Option<bool>,
    /// The five selections in the visible Mechanics tool rail.
    mechanics_tool: Option<MechTool>,
    /// Visibility of the model bottom, render and slice rows, resin, camera and progress overlays.
    chrome: Option<ChromeKey>,
    /// The three slice tips and toggles and this pane's orthographic toggle.
    /// The plate's mode label, Test prints' toolbar group and its sidebar (the mode, the
    /// sidebar open in the lead pane, a part picked, the print's revision for the totals).
    tests: Option<(Option<PlateMode>, bool, bool, bool, u64)>,
    /// Design's Part sidebar: offered in this view, a part picked, opened, over a slice.
    part_side: Option<(bool, bool, bool, bool)>,
    /// The colours' warning mark beside View options (why they differ from the print's).
    colours: Option<(fab_plate::ColourMode, (usize, bool), bool, usize, bool, bool, crate::print::ColorBy, bool)>,
    /// The three Design tool buttons and the paint, shape, sculpt and force strips and their controls.
    tools: Option<ToolBarsKey>,
    /// Visibility of the see-through row.
    see_row: Option<bool>,
    /// Frame selected's and Drop's enabled states (a part picked; and the plate editable).
    part_actions: Option<(bool, bool)>,
    /// The active-part lookup for the existing tool rail key.
    has_part: Option<((u64, u64, u64), bool)>,
    /// The layer and height lookups for the slice's layer label.
    layer: Option<(u64, f64, u32)>,
    /// Preview selection for the time, layer and timeline readouts.
    playback: Option<PlaybackKey>,
}

#[derive(Clone, Copy, PartialEq)]
struct ModeKey {
    workspace: crate::workspaces::WorkspaceId,
    shape: u64,
    plate_mode: PlateMode,
    force: bool,
    slice: bool,
    look: PartLook,
}

#[derive(Clone, Copy, PartialEq)]
struct PlaybackKey {
    revision: u64,
    held: bool,
    resin: bool,
    playing: bool,
    play_time: f64,
    layer_hi: u32,
    live: Option<f64>,
}

#[derive(Clone, Copy, PartialEq)]
struct PrintChromeKey {
    // Printer reports, job selection and the resin job all advance the print revision.
    revision: u64,
    monitor: bool,
    following: bool,
    slice_active: bool,
    // The resin stack can arrive or go away before a print revision.
    stack: Option<(u64, u32)>,
}

#[derive(Clone, Copy, PartialEq)]
struct PrintChrome {
    held: bool,
    resin: bool,
    resin_live: bool,
}

impl StageDrawKeys {
    fn modes(&mut self, state: &AppState) -> (Option<PlateMode>, RenderMode) {
        let ws = &state.ui.workspaces;
        let def = if let Some(def) = ws.list.get(self.workspace_index).filter(|d| d.id == ws.current) {
            def
        } else if let Some(temp) = ws.temp.as_ref().filter(|t| t.def.id == ws.current) {
            &temp.def
        } else {
            self.workspace_index = ws.position(ws.current).unwrap_or(0);
            &ws.list[self.workspace_index]
        };
        let key = ModeKey {
            workspace: def.id, shape: def.shape, plate_mode: state.ui.plate_mode,
            force: state.force.active, slice: state.print.play.active, look: state.view.look,
        };
        if let Some((old, plate, render)) = self.modes {
            if key == old {
                return (plate, render);
            }
        }
        let plate = state.ui.mode();
        let render = state.render_mode();
        self.modes = Some((key, plate, render));
        (plate, render)
    }

    fn print_chrome(&mut self, p: &crate::print::PrintState) -> PrintChrome {
        let key = PrintChromeKey {
            revision: p.revision, monitor: p.follow.monitor, following: p.follow.on,
            slice_active: p.play.active, stack: p.resin.as_ref().map(|r| (r.stack, r.count)),
        };
        if let Some((old, chrome)) = self.print_chrome {
            if key == old {
                return chrome;
            }
        }
        let chrome = PrintChrome {
            held: p.monitor_holds(),
            resin: p.resin_shown().is_some(),
            resin_live: !p.follow.on && p.resin_followed().is_some(),
        };
        self.print_chrome = Some((key, chrome));
        chrome
    }
}

#[derive(Clone, Copy, PartialEq)]
struct ChromeKey {
    slice: bool,
    mechanics: bool,
    lead: bool,
    print: PrintChrome,
    camera_pip: bool,
}


#[derive(Clone, Copy, PartialEq)]
struct ToolBarsKey {
    design: bool,
    paint: Option<PaintBarKey>,
    /// Arrange's row (its gap as last shown).
    arrange: Option<u64>,
    /// Measure Fit's Turn and Clear.
    fit: bool,
    /// Edit shape: Lay flat's toggle and the pick kind filter.
    lay_flat: Option<(bool, crate::tools::picks::PickKind)>,
    sculpt: Option<StrokeOp>,
    force: Option<ForceKind>,
}

#[derive(Clone, Copy, PartialEq)]
struct PaintBarKey {
    mode: PaintMode,
    erasing: bool,
    target: PaintTarget,
    scaled: bool,
}

#[derive(Script, Widget)]
pub struct FabStage {
    #[deref]
    view: View,
    /// The Part sidebar's left edge held: where the press was and the width then.
    #[rust]
    part_drag: Option<(f64, f64)>,
    /// The Part sidebar's width as last set on its card and the slider's room.
    #[rust]
    part_w_shown: Option<f64>,
    /// The no-trees area the context menu was opened on (`instance/solid`).
    #[rust]
    menu_area: Option<String>,
    /// The part under the context menu's click (its object).
    #[rust]
    menu_part: Option<String>,
    /// The feature under the menu's click (`instance/solid`).
    menu_feature: Option<String>,
    #[rust]
    draw_keys: StageDrawKeys,
    #[rust]
    synced: Option<(Tool, GizmoMode, bool, bool, bool, bool, bool, RenderMode, bool, bool, bool)>,
    #[rust]
    workspace_shown: Option<(Option<PlateMode>, bool)>,
    #[rust]
    print_revision: Option<(u64, u64, Option<PlateMode>, bool, Option<(u32, u32)>, bool)>,
    /// Arrange's row shows (the rail's Arrange pressed): the gap, and the button that arranges.
    #[rust]
    arranging: bool,
    /// View options' flyout is open.
    #[rust]
    options_open: bool,
    /// The view pie while Q is held: its centre (window points) and the slot pointed at.
    #[rust]
    pie: Option<(DVec2, Option<u8>)>,
    /// The pointer's last place over the window, where the pie opens.
    #[rust]
    pointer: DVec2,
    /// The step line over the view as last set: the step and the part's step count.
    #[rust]
    step_line: Option<(usize, usize)>,
    #[rust]
    time_shown: Option<(u64, u64, bool)>,
    #[rust]
    layer_shown: Option<(u64, u32, u32, f32)>,
    #[rust]
    now_shown: Option<(u64, f64)>,
    #[rust]
    pane_label_shown: Option<(Option<LinkHow>, Option<u32>, PaneMode, TraceProgress)>,
    #[rust]
    pane_label_text: String,
    /// The chip's short caption and the number shown beside its icon.
    #[rust]
    pane_label_compact: [String; 2],
    /// Whole points last fitted for the current caption.
    #[rust]
    pane_label_room: Option<f64>,
    #[rust]
    pane_label_svg: Option<&'static str>,
    /// Its viewport's pane: Plate 0's until the stage binds its own.
    #[rust(PaneId::PLATE_ZERO)]
    pane: PaneId,
    /// The Plate instance it shows: whether the dock shows it decides its events.
    #[rust(crate::ui::areas::Instance::first(Editor::Viewport))]
    instance: crate::ui::areas::Instance,
    /// A later Plate view's own pane (Plate 0 shows pane 0 and leases none).
    #[rust]
    lease: Option<PaneLease>,
    /// The shell bound it ([`FabStage::ensure_bound`]): until then, and after its release, it
    /// hears no event and draws nothing.
    #[rust]
    bound: bool,
}

const OWN_VIEWPORT: &[LiveId] = ids!(viewport);

/// The view pie's pills (points), the hub's side, how far the pills stand from the centre, and
/// how far the pointer goes from it before a direction counts.
const PIE_PILL: DVec2 = DVec2 { x: 112.0, y: 26.0 };
const PIE_HUB: f64 = 26.0;
const PIE_RADIUS: f64 = 92.0;
const PIE_DEAD: f64 = 16.0;

/// The view pie's slot the pointer points at from `centre`: none in the dead zone, else the
/// nearest of the eight compass directions (0 north, clockwise).
fn pie_hot(centre: DVec2, pointer: DVec2) -> Option<u8> {
    let d = pointer - centre;
    if d.length() < PIE_DEAD {
        return None;
    }
    Some(((d.x.atan2(-d.y) / std::f64::consts::FRAC_PI_4).round() as i32).rem_euclid(8) as u8)
}

/// What the view pie's slot does: the view from where it stands (Top up, Right right, …), Front
/// and Back the upper diagonals, the framings the lower ones.
fn pie_action(slot: u8) -> ShellAction {
    match slot {
        0 => ShellAction::PresetView(PresetView::Top),
        1 => ShellAction::PresetView(PresetView::Back),
        2 => ShellAction::PresetView(PresetView::Right),
        3 => ShellAction::FrameSelected,
        4 => ShellAction::PresetView(PresetView::Bottom),
        5 => ShellAction::FrameAll,
        6 => ShellAction::PresetView(PresetView::Left),
        _ => ShellAction::PresetView(PresetView::Front),
    }
}

/// View options' Tool rails: this Plate view's rails on or off (the shell keeps them per view).
#[derive(Clone, Copy, Debug)]
pub struct ToolRailsAsked(pub crate::ui::areas::Instance);

/// A stage's pane and its viewport: one, its own (an Underside or Traced view is a Plate view
/// of its own).
fn bound_panes(own: PaneId) -> impl Iterator<Item = (PaneId, &'static [LiveId])> {
    std::iter::once((own, OWN_VIEWPORT))
}

impl FabStage {
    /// Binds the stage to its Plate view once; the shell calls it after each layout load,
    /// before the body hears an event or draws. Plate 0 shows pane 0; a later Plate leases a
    /// pane of its own, at the camera the plate's view file kept for it, else at the lead's.
    /// O(1) once bound.
    pub fn ensure_bound(&mut self, cx: &mut Cx, state: &mut AppState, instance: crate::ui::areas::Instance) {
        if self.bound && self.instance == instance {
            return;
        }
        let mut restored = false;
        let pane = if instance.n == 0 {
            PaneId::PLATE_ZERO
        } else {
            let lease = state.lease_pane(PaneRole::Plate);
            let pane = lease.id();
            let lead = state.view.lead_plate();
            let saved = state.view.saved_plates.iter().find(|(n, ..)| *n == instance.n).map(|&(_, camera, preset)| (camera, preset));
            restored = saved.is_some();
            let (camera, preset) = saved.unwrap_or_else(|| (*state.view.camera(lead), state.view.pane(lead).preset));
            let view = state.view.pane_mut(pane);
            (view.camera, view.preset) = (camera, preset);
            self.lease = Some(lease);
            pane
        };
        self.bind(cx, state, pane, instance);
        // a camera the plate's view file kept is checked against the scene at its first frame
        if restored {
            if let Some(mut viewport) = self.view.widget(cx, OWN_VIEWPORT).borrow_mut::<crate::viewport::FabViewport>() {
                viewport.check_camera();
            }
        }
    }

    /// No workspace holds its Plate view any more: its viewport lets go of its passes and its
    /// target, its pane goes back to the pool, and it does nothing until it binds again (a later
    /// Plate view of the same number starts afresh). Plate 0 stays bound.
    pub fn release(&mut self, cx: &mut Cx, state: &mut AppState) {
        let Some(lease) = self.lease.take() else { return };
        let n = self.instance.n;
        state.view.saved_plates.retain(|(m, ..)| *m != n);
        if let Some(mut viewport) = self.view.widget(cx, OWN_VIEWPORT).borrow_mut::<crate::viewport::FabViewport>() {
            viewport.park(cx, state);
        }
        drop(lease);
        self.bound = false;
    }

    /// Its Plate view's instance and pane, once bound.
    pub fn bound_plate(&self) -> Option<(crate::ui::areas::Instance, PaneId)> {
        self.bound.then_some((self.instance, self.pane))
    }

    /// Show `instance` through `pane`: the viewport, the tool overlays, the mechanics grab and
    /// the navigation gizmo draw and act through it. The panes it leaves let go of the scene
    /// host: no pane holds it that no Plate draws.
    pub fn bind(&mut self, cx: &mut Cx, state: &mut AppState, pane: PaneId, instance: crate::ui::areas::Instance) {
        if pane != self.pane {
            for (left, _) in bound_panes(self.pane) {
                state.let_go_of_host(left);
            }
        }
        self.pane = pane;
        self.instance = instance;
        self.bound = true;
        self.draw_keys = StageDrawKeys::default();
        self.pane_label_shown = Default::default();
        self.pane_label_room = None;
        self.pane_label_svg = None;
        let v = &self.view;
        if let Some(mut w) = v.widget(cx, OWN_VIEWPORT).borrow_mut::<crate::viewport::FabViewport>() { w.bind_pane(pane, PaneRole::Plate); }
        if let Some(mut w) = v.widget(cx, ids!(design_overlay.tool_overlay)).borrow_mut::<crate::tools::overlay::FabToolOverlay>() { w.pane = pane; }
        if let Some(mut w) = v.widget(cx, ids!(design_overlay.part_gizmo)).borrow_mut::<crate::tools::transform::FabPartGizmo>() { w.pane = pane; }
        if let Some(mut w) = v.widget(cx, ids!(design_overlay.shape_handles)).borrow_mut::<crate::tools::handles::FabShapeHandles>() { w.pane = pane; }
        if let Some(mut w) = v.widget(cx, ids!(design_overlay.test_cube)).borrow_mut::<crate::tools::test_cube::FabTestCube>() { w.pane = pane; }
        if let Some(mut w) = v.widget(cx, ids!(design_overlay.force_editor)).borrow_mut::<crate::tools::force_edit::FabForceEditor>() { w.pane = pane; }
        if let Some(mut w) = v.widget(cx, ids!(mech_grab)).borrow_mut::<crate::tools::mech_grab::FabMechGrab>() { w.pane = pane; }
        if let Some(mut w) = v.widget(cx, ids!(nav_col.gizmo)).borrow_mut::<crate::nav::gizmo::FabNavGizmo>() { w.pane = pane; }
        self.view.redraw(cx);
    }

    /// The Plate it shows and that Plate's pane.
    pub fn bound(&self) -> (crate::ui::areas::Instance, PaneId) {
        (self.instance, self.pane)
    }

    /// The dock holds this Plate but does not show it: nothing in it can be pressed and nothing
    /// it shows needs a redraw, so its rails, overlays, gizmos and grab hear nothing. Its
    /// viewports hear only what they need to come back whole
    /// ([`crate::viewport::FabViewport::handle_hidden`]); a lost focus still ends a stroke. Per
    /// event: a viewport lookup per bound pane and a match.
    fn handle_hidden(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        if matches!(event, Event::WindowLostFocus(_) | Event::Pause | Event::Background) {
            crate::tools::paint::cancel();
            crate::tools::sculpt::cancel();
            if let Some(state) = scope.data.get_mut::<AppState>() {
                for (id, _) in bound_panes(self.pane) {
                    if let Some(pane) = state.view.panes.get_mut(id.0 as usize).and_then(Option::as_mut) { pane.pointer_held = false; }
                }
            }
        }
        for (id, path) in bound_panes(self.pane) {
            if id != self.pane && scope.data.get::<AppState>().is_some_and(|state| state.view.panes.get(id.0 as usize).is_none_or(Option::is_none)) {
                continue;
            }
            if let Some(mut viewport) = self.view.widget(cx, path).borrow_mut::<crate::viewport::FabViewport>() {
                viewport.handle_hidden(cx, event, scope);
            }
        }
    }

    /// The view pie's events: Q pressed over this view opens it round the pointer (kept inside
    /// the view), the pointer aims it, Q released takes the slot aimed at (none: nothing), Escape
    /// or a press closes it. Whether the event was the pie's alone (a press while it is open).
    fn pie_event(&mut self, cx: &mut Cx, event: &Event) -> bool {
        match event {
            Event::MouseMove(e) => {
                self.pointer = e.abs;
                if let Some((centre, hot)) = &mut self.pie {
                    let aimed = pie_hot(*centre, e.abs);
                    if aimed != *hot {
                        *hot = aimed;
                        self.view.redraw(cx);
                    }
                }
                false
            }
            Event::Actions(actions) if actions.iter().any(|a| matches!(a.downcast_ref::<StageAction>(), Some(StageAction::ViewPie))) => {
                let view = self.view.area().rect(cx);
                if self.pie.is_none() && view.contains(self.pointer) {
                    // round the pointer, the ring kept inside the view
                    let reach = dvec2(PIE_RADIUS + PIE_PILL.x * 0.5 + 6.0, PIE_RADIUS + PIE_PILL.y * 0.5 + 6.0);
                    let lo = view.pos + reach;
                    let hi = view.pos + view.size - reach;
                    let centre = dvec2(self.pointer.x.clamp(lo.x, hi.x.max(lo.x)), self.pointer.y.clamp(lo.y, hi.y.max(lo.y)));
                    self.pie = Some((centre, pie_hot(centre, self.pointer)));
                    self.view.redraw(cx);
                }
                false
            }
            Event::KeyUp(ke) if ke.key_code == KeyCode::KeyQ => {
                if let Some((_, hot)) = self.pie.take() {
                    if let Some(slot) = hot {
                        cx.action(pie_action(slot));
                    }
                    self.view.redraw(cx);
                }
                false
            }
            Event::KeyDown(ke) if ke.key_code == KeyCode::Escape && self.pie.is_some() => {
                self.pie = None;
                self.view.redraw(cx);
                true
            }
            Event::MouseDown(_) if self.pie.is_some() => {
                self.pie = None;
                self.view.redraw(cx);
                true
            }
            _ => false,
        }
    }

    /// The view pie's layer as it draws: shown while open, its pills round its centre, the one
    /// aimed at lit. Ten lookups while it is open.
    fn sync_pie(&mut self, cx: &mut Cx) {
        let layer = self.view.view(cx, ids!(pie_layer));
        if layer.visible() != self.pie.is_some() {
            layer.set_visible(cx, self.pie.is_some());
        }
        let Some((centre, hot)) = self.pie else { return };
        if let Some(mut hub) = self.view.widget(cx, ids!(pie_layer.pie_hub)).borrow_mut::<View>() {
            hub.walk.abs_pos = Some(centre - DVec2::all(PIE_HUB * 0.5));
        }
        let pills = [ids!(pie_layer.pie0), ids!(pie_layer.pie1), ids!(pie_layer.pie2), ids!(pie_layer.pie3),
            ids!(pie_layer.pie4), ids!(pie_layer.pie5), ids!(pie_layer.pie6), ids!(pie_layer.pie7)];
        for (k, path) in pills.into_iter().enumerate() {
            let angle = k as f64 * std::f64::consts::FRAC_PI_4;
            let at = centre + dvec2(angle.sin(), -angle.cos()) * PIE_RADIUS - PIE_PILL * 0.5;
            let lit = if hot == Some(k as u8) { 1.0 } else { 0.0 };
            let pill = self.view.widget(cx, path);
            if let Some(mut view) = pill.borrow_mut::<View>() {
                view.walk.abs_pos = Some(at);
                view.draw_bg.set_dyn_instance(cx, live_id!(hot), &[lit]);
            }
            if let Some(mut label) = pill.label(cx, ids!(label)).borrow_mut() {
                label.draw_text.set_dyn_instance(cx, live_id!(hot), &[lit]);
            }
        }
    }

    /// View options' tiles as the view has them: on or off, offered or not (a slice's options in
    /// Sliced, the board's on a plate with one), and why the colours shown differ from the print's.
    /// A dozen lookups while the flyout is open.
    fn sync_options(cx: &mut Cx, view: &View, pane: PaneId, state: &AppState) {
        let p = &state.print;
        let card = view.view(cx, ids!(view_flyout.options_card));
        let set = |cx: &mut Cx, path: &[LiveId], on: bool, works: bool| {
            let tile = card.radio_button(cx, path);
            if tile.active(cx) != on { tile.set_active(cx, on, Animate::No); }
            if tile.disabled(cx) == works { tile.set_disabled(cx, !works); }
        };
        set(cx, ids!(o_rails), view.view(cx, ids!(model_ui.left)).visible(), true);
        set(cx, ids!(o_ortho), state.view.camera(pane).ortho, true);
        set(cx, ids!(o_frame_all), false, true);
        set(cx, ids!(o_frame_sel), false, active_part(state).is_some());
        set(cx, ids!(o_colours), state.view.colours == fab_plate::ColourMode::Material, true);
        // what the slice shows (the printed surface draws neither travel nor the parts under it;
        // a resin printer's layers are no toolpaths)
        let sliced = state.render_mode() == RenderMode::Slice;
        card.widget(cx, ids!(slice_title)).set_visible(cx, sliced);
        card.widget(cx, ids!(slice_grid)).set_visible(cx, sliced);
        if sliced {
            let resin = p.resin_shown().is_some();
            set(cx, ids!(o_supports), p.play.supports, !resin);
            set(cx, ids!(o_travel), p.play.travel, !resin && !p.finish);
            set(cx, ids!(o_parts), p.play.show_model, !resin && !p.finish);
        }
        let board = state.board_shown();
        card.widget(cx, ids!(o_board)).set_visible(cx, board.is_some());
        set(cx, ids!(o_board), board == Some(true), true);
        let notice = state.colour_notice();
        card.label(cx, ids!(notice)).set_visible(cx, notice.is_some());
        card.label(cx, ids!(notice)).set_text(cx, notice.as_deref().unwrap_or(""));
    }

    /// A tile pressed in View options' flyout (`instance`: this Plate view).
    fn view_option(cx: &mut Cx, instance: crate::ui::areas::Instance, state: &AppState, pick: LiveId) {
        let p = &state.print;
        match pick {
            id if id == live_id!(vo_rails) => cx.action(ToolRailsAsked(instance)),
            id if id == live_id!(vo_ortho) => cx.action(ShellAction::ToggleOrtho),
            id if id == live_id!(vo_frame_all) => cx.action(ShellAction::FrameAll),
            id if id == live_id!(vo_frame_sel) => cx.action(ShellAction::FrameSelected),
            id if id == live_id!(vo_colours) => {
                let on = state.view.colours == fab_plate::ColourMode::Material;
                cx.action(ShellAction::SetColourMode(if on { fab_plate::ColourMode::Model } else { fab_plate::ColourMode::Material }));
            }
            id if id == live_id!(vo_supports) => cx.action(PrintAction::ShowSupports(!p.play.supports)),
            id if id == live_id!(vo_travel) => cx.action(PrintAction::ShowTravel(!p.play.travel)),
            id if id == live_id!(vo_parts) => cx.action(PrintAction::ShowModel(!p.play.show_model)),
            id if id == live_id!(vo_board) => {
                if let Some(shown) = state.board_shown() {
                    cx.action(ShellAction::ShowBoard(!shown));
                }
            }
            _ => {}
        }
    }

    /// The two rail containers (and the chosen tool's row at the top) keep their visibility as
    /// the plate mode switches their parents; the toolbar's Tools toggle says which.
    pub fn set_tools_shown(&mut self, cx: &mut Cx, shown: bool) {
        self.view.view(cx, ids!(model_ui.left)).set_visible(cx, shown);
        self.view.view(cx, ids!(model_ui.context)).set_visible(cx, shown);
        self.view.view(cx, ids!(mechanics_ui.left)).set_visible(cx, shown);
        let toggle = self.view.radio_button(cx, ids!(mechanics_ui.mech_tools));
        if toggle.active(cx) != shown {
            toggle.set_active(cx, shown, Animate::No);
        }
    }

    /// Its Plate left the screen: its panes let go of the scene host, so the Plate on screen
    /// takes it at its next draw with the studio and environment work all panes' pictures share.
    /// The panes keep their tiles and passes for the Plate's return.
    pub fn hidden(&self, state: &mut AppState) {
        for (pane, _) in bound_panes(self.pane) {
            state.let_go_of_host(pane);
        }
    }

    /// Its Plate is on screen again: what changed while it was hidden draws, as its hidden
    /// viewports skipped the redraws ([`crate::viewport::FabViewport::handle_hidden`]).
    pub fn shown_again(&mut self, cx: &mut Cx) {
        self.draw_keys = StageDrawKeys::default();
        self.view.redraw(cx);
        for (_, path) in bound_panes(self.pane) {
            self.view.widget(cx, path).redraw(cx);
        }
    }

    /// Called before the dock hides the stage on a workspace switch.
    pub fn park(&mut self, cx: &mut Cx, state: &mut AppState) {
        self.draw_keys = StageDrawKeys::default();
        self.workspace_shown = None;
        for (_, path) in bound_panes(self.pane) {
            if let Some(mut viewport) = self.view.widget(cx, path).borrow_mut::<crate::viewport::FabViewport>() {
                viewport.park(cx, state);
            }
        }
    }

    /// Returns whether the Plate view's chip needs a draw to fit new text.
    fn sync_panes(&mut self, cx: &mut Cx, state: &mut AppState) -> bool {
        // the chip on every Plate view says what the view is when it is no plain one: an
        // Underside view, a Synced or a traced view
        let own = state.view.pane(self.pane);
        let label = own.mode == PaneMode::Traced || own.link.source().is_some();
        if self.draw_keys.pane_layout != Some(label) {
            self.draw_keys.pane_layout = Some(label);
            self.pane_label_shown = None;
            self.pane_label_room = None;
            self.view.view(cx, ids!(pane_label)).set_visible(cx, label);
        }
        let mut refit = false;
        for (id, path) in bound_panes(self.pane) {
            let Some(pane) = state.view.panes.get(id.0 as usize).and_then(Option::as_ref) else { continue };
            // Raster mode releases any live tracer the viewport still owns.
            let viewport = self.view.widget(cx, path);
            if let Some(mut viewport) = viewport.borrow_mut::<crate::viewport::FabViewport>() {
                viewport.set_pane_mode(cx, pane.mode);
            }
            let how = pane.link.how();
            // the Plate view it follows, by its number
            let source = pane.link.source().and_then(|s| state.view.plate_number(s));
            let key = (how, source, pane.mode, pane.trace);
            if self.pane_label_shown != Some(key) {
                self.pane_label_shown = Some(key);
                use std::fmt::Write;
                let text = &mut self.pane_label_text;
                text.clear();
                if pane.mode == PaneMode::Traced {
                    let _ = write!(text, "Traced · {} / {}", pane.trace.samples, pane.trace.target);
                    if pane.trace.stale { text.push_str(" · stale"); }
                    if pane.trace.fields_not_traced { text.push_str(" · fields not traced"); }
                    if pane.trace.bodies_not_traced { text.push_str(" · bodies not traced"); }
                    if pane.trace.unavailable { text.push_str(" · unavailable"); }
                } else if let Some(how) = how {
                    text.push_str(match how {
                        LinkHow::Flipped | LinkHow::Through => "Underside of Plate",
                        LinkHow::Synced => "Synced with Plate",
                    });
                    if let Some(n) = source.filter(|n| *n > 0) {
                        let _ = write!(text, " {}", u64::from(n) + 1);
                    }
                    if how == LinkHow::Through { text.push_str(" · see through"); }
                }
                // the next draw fits it to the room it has
                self.pane_label_room = None;
                refit = label;
            }
        }
        refit
    }

    fn fit_pane_label(&mut self, cx: &mut Cx2d, walk: Walk) {
        if self.draw_keys.pane_layout != Some(true) { return; }
        // Leave ten points clear at the area's right edge.
        let room = (cx.peek_walk_turtle(walk).size.x - PANE_LABEL_LEFT - 2.0 * PANE_LABEL_PAD - 10.0).round();
        if self.pane_label_room == Some(room) { return; }
        let Some((how, source, mode, trace)) = self.pane_label_shown else { return };
        let [short, number] = &mut self.pane_label_compact;
        if self.pane_label_room.is_none() {
            use std::fmt::Write;
            short.clear();
            number.clear();
            if mode == PaneMode::Traced {
                let _ = write!(number, "{}", trace.samples);
                let _ = write!(short, "{number} / {}", trace.target);
            } else if let Some(how) = how {
                let _ = write!(number, "{}", u64::from(source.unwrap_or(0)) + 1);
                short.push_str(if how == LinkHow::Synced { "Sync " } else { "Under " });
                short.push_str(number);
                if how == LinkHow::Through { short.push_str(" · thru"); }
            }
        }

        let label = self.view.label(cx, ids!(pane_label.label));
        let Some(mut label) = label.borrow_mut() else { return };
        let full = &self.pane_label_text;
        let (caption, show_icon) = if family_api::measure(&label.draw_text, cx, full) <= room {
            (full.as_str(), false)
        } else if family_api::measure(&label.draw_text, cx, short) <= room {
            (short.as_str(), false)
        } else {
            (number.as_str(), true)
        };
        // a view too narrow for even the icon and its number shows no chip, not a sliver of one
        let fits = !show_icon || family_api::measure(&label.draw_text, cx, number) + PANE_LABEL_ICON <= room;
        label.set_text(cx, caption);
        drop(label);
        self.view.view(cx, ids!(pane_label)).set_visible(cx, fits);
        let mut icon = self.view.widget(cx, ids!(pane_label.icon));
        if show_icon {
            let svg = match (mode, how) {
                (PaneMode::Traced, _) => "self://resources/icons/spark.svg",
                (_, Some(LinkHow::Synced)) => "self://resources/icons/sync.svg",
                _ => "self://resources/icons/swap_sides.svg",
            };
            if self.pane_label_svg != Some(svg) {
                script_apply_eval!(cx, icon, { draw_icon +: { svg: mod.res.crate_resource(#(svg)) } });
                self.pane_label_svg = Some(svg);
            }
        }
        icon.set_visible(cx, show_icon);
        self.pane_label_room = Some(room);
    }

    fn sync_trace_interaction(&self, cx: &mut Cx, state: &mut AppState) {
        if !state.view.panes.iter().flatten().any(|pane| !pane.parked && pane.mode == PaneMode::Traced) { return; }
        for (id, path) in bound_panes(self.pane) {
            let Some(pane) = state.view.panes.get_mut(id.0 as usize).and_then(Option::as_mut) else { continue };
            let viewport = self.view.widget(cx, path);
            pane.interacting = pane.pointer_held || viewport.borrow::<crate::viewport::FabViewport>()
                .is_some_and(|viewport| viewport.trace_interacting());
        }
    }

    fn set_mode(cx: &mut Cx, state: &AppState, mode: GizmoMode) {
        if state.tool != Tool::Select {
            cx.action(ShellAction::SetTool(Tool::Select));
        }
        cx.action(StageAction::Gizmo(mode));
    }

    /// The Force tool: its strip opens, and the Force render mode comes on with it (leaving the
    /// tool keeps the mode; Normal or Sliced ends both).
    fn force_tool(cx: &mut Cx, state: &AppState) {
        Self::set_mode(cx, state, GizmoMode::Force);
        if !state.force.active {
            cx.action(ShellAction::SetRenderMode(RenderMode::Force));
        }
    }

    fn selected_objects(state: &AppState) -> Vec<String> {
        state.scene_state.selection.set.iter().filter_map(|e| state.plate.part_of(*e)).map(|i| state.plate.parts[i].object.clone()).collect()
    }

    fn context_menu(state: &AppState, branch: bool, area: Option<AreaKind>, over_part: bool, feature: Option<&str>) -> Vec<MenuItem> {
        let has = active_part(state).is_some();
        // A branch of the Sliced view's supports under the click (or picked): Delete takes it out.
        let tree = (branch || state.print.picked_branch().is_some())
            .then(|| MenuItem::new(live_id!(ctx_delete_branch), "Delete branch").key(&keymap::shortcut(Key::Delete)));
        // A marked area under the click: its statement goes, the trees replan or the plate reslices.
        let what = match area {
            Some(AreaKind::NoTrees) => Some("Delete no-trees area"),
            Some(AreaKind::Settings) => Some("Delete settings volume"),
            None => None,
        };
        let tree = tree.or_else(|| what.map(|w| MenuItem::new(live_id!(ctx_delete_area), w).key(&keymap::shortcut(Key::Delete))));
        // a volume over the part under the click (or picked; none, it is off); from a slice
        // another workspace shows, it goes back to Design's to make it
        let settings = MenuItem::new(live_id!(ctx_settings_here), crate::ui::code::SETTINGS_HERE).enabled_if(over_part || has);
        let test_cube = MenuItem::new(live_id!(ctx_test_cube), "Test cube").enabled_if(over_part || has);
        // the feature under the click: a volume that follows it
        let override_feature = feature.map(|f| MenuItem::new(live_id!(ctx_override_feature), &format!("Override settings: {}", f.split_once('/').map_or(f, |(_, n)| n))));
        if state.slice_blocks_edits() {
            // Keep Delete discoverable; the app explains that editing needs Design.
            let part = if tree.is_some() { "Delete part" } else { "Delete" };
            return tree.into_iter().chain([settings, test_cube, MenuItem::new(live_id!(part_delete), part).key(&keymap::shortcut(Key::Delete)).enabled_if(has)]).collect();
        }
        let mut items: Vec<MenuItem> = tree.into_iter().collect();
        items.push(MenuItem::new(live_id!(ctx_drop), "Drop to plate").enabled_if(has));
        items.extend(override_feature);
        items.push(settings);
        items.push(test_cube);
        items.extend(crate::ui::parts::part_menu(state, &Self::selected_objects(state), has));
        items.extend([
            MenuItem::sep(),
            MenuItem::new(live_id!(ctx_frame_sel), "Frame selection").key(&keymap::shortcut(Key::FrameSelected)).enabled_if(has),
            MenuItem::new(live_id!(ctx_frame_all), "Frame plate").key(&keymap::shortcut(Key::FrameAll)),
            MenuItem::sep(),
            MenuItem::new(live_id!(ctx_arrange), "Arrange plate"),
            MenuItem::new(live_id!(ctx_add_box), "Add box"),
            MenuItem::new(live_id!(ctx_add_cyl), "Add cylinder"),
        ]);
        items
    }
}

/// What the box tool and Add box say while no part is picked: a box goes on a part, the one
/// picked, so they wait for one.
pub const SELECT_A_PART: &str = "Select a part first";

/// The box tool's tip while a part is picked.
pub const SETTINGS_HERE_TIP: &str = "Print settings here: drag across the picked part (in the Sliced view, on the shown layer) for a box with its own infill and wall loops (m.settings in its script). Alt draws a sphere";

/// The shapes a print settings volume offers beside following a feature: its action's name, and
/// the menu's words.
const VOLUME_SHAPES: [(&str, &str); 4] = [("box", "Box"), ("sphere", "Sphere"), ("cylinder", "Cylinder"), ("capsule", "Capsule")];

/// The menu's id of following the picked feature; the shapes' are their place in [`VOLUME_SHAPES`].
const VOLUME_FOLLOWS: LiveId = LiveId(u64::MAX);

/// The feature a new print settings volume goes on: the Features tree's row, or the view's second
/// click on the model (`instance/solid`). A marked area is none, nor is the part's own body.
pub fn picked_feature(state: &AppState) -> Option<String> {
    let row = state.source.selected.as_ref().map(|id| format!("{}/{id}", state.source.instance));
    row.or_else(|| state.params.focus.clone()).filter(|f| {
        !state.params.areas.iter().any(|a| a.object == *f) && !state.plate.parts.iter().any(|p| p.object == *f)
    })
}

/// "+ Settings volume": following the picked feature first, or shapes round it; with no feature, a
/// box first over the picked part, as it always was.
pub fn volume_menu(state: &AppState) -> Vec<MenuItem> {
    let shapes = VOLUME_SHAPES.iter().enumerate();
    match picked_feature(state) {
        Some(feature) => {
            let name = feature.split_once('/').map_or(feature.as_str(), |(_, n)| n);
            let mut items = vec![MenuItem::new(VOLUME_FOLLOWS, &format!("Follows {name}")), MenuItem::sep()];
            items.extend(shapes.map(|(i, (_, label))| MenuItem::new(LiveId(i as u64), &format!("{label} round {name}"))));
            items
        }
        None => shapes.map(|(i, (_, label))| MenuItem::new(LiveId(i as u64), label)).collect(),
    }
}

/// A shape picked from [`volume_menu`]: the volume, its panel to the front.
pub fn volume_picked(cx: &mut Cx, state: &AppState, pick: LiveId) {
    let feature = picked_feature(state);
    let shape = if pick == VOLUME_FOLLOWS { "follows" } else { VOLUME_SHAPES.get(pick.0 as usize).map_or("box", |(name, _)| *name) };
    let object = active_part(state).map(|i| state.plate.parts[i].object.clone());
    if object.is_none() && feature.is_none() {
        return cx.action(ShellAction::Notice { view: NoticeView::Plate, text: format!("Print settings: {SELECT_A_PART}, the volume goes on it") });
    }
    add_volume(cx, state, object, feature, shape);
}

fn add_volume(cx: &mut Cx, state: &AppState, object: Option<String>, feature: Option<String>, shape: &'static str) {
    if !state.ui.in_mode(PlateMode::Design) {
        cx.action(ShellAction::SetWorkspace(Workspace::Design));
    }
    cx.action(PartAction::SettingsVolume { object, feature, shape });
    cx.action(ShowPanel(Editor::Overrides));
}

/// "Print settings here" from a menu or Add box: on the part under the click (`under`) or else
/// the picked one, a volume over its bounds ([`PartAction::SettingsVolume`]); with none, nothing
/// (the entries are off then) but the status saying why. The Overrides editor, which holds the
/// volumes, comes forward: the new one is picked there. The view stays as it is (Normal or
/// Sliced): the person switches.
pub fn settings_here(cx: &mut Cx, state: &AppState, under: Option<String>) {
    match under.or_else(|| active_part(state).map(|i| state.plate.parts[i].object.clone())) {
        Some(object) => add_volume(cx, state, Some(object), None, "box"),
        None => cx.action(ShellAction::Notice { view: NoticeView::Plate, text: format!("Print settings here: {SELECT_A_PART}, the box goes on it") }),
    }
}

/// The tool that draws a print settings volume on the picked part, from anywhere in Design: the
/// tool armed, the Overrides editor forward; the view stays (the Force mode's study, which takes no
/// volume, goes back to the Normal view). With no part picked, the status says to pick one.
pub fn arm_settings_here(cx: &mut Cx, state: &AppState) {
    if active_part(state).is_none() {
        cx.action(ShellAction::Notice { view: NoticeView::Plate, text: format!("Print settings here: {SELECT_A_PART}, the box goes on it") });
        return;
    }
    if !state.ui.in_mode(PlateMode::Design) {
        cx.action(ShellAction::SetWorkspace(Workspace::Design));
    }
    if state.render_mode() == RenderMode::Force {
        cx.action(ShellAction::SetRenderMode(state.normal_mode()));
    }
    cx.action(ShellAction::SetTool(Tool::SettingsHere));
    cx.action(ShowPanel(Editor::Overrides));
}

/// The stage's own UI state changes (kept in `AppState::ui`).
#[derive(Clone, Copy, Debug)]
pub enum StageAction {
    Gizmo(GizmoMode),
    LayFlat(bool),
    Zones(bool),
    CameraPip(bool),
    /// The Test prints mode's sidebar of tests, open or closed.
    TestsSidebar(bool),
    /// How the 3D views take the mouse (the caption's Settings), kept per machine.
    Nav(NavScheme),
    /// The wheel zooms the other way (Settings), kept per machine.
    InvertWheel(bool),
    /// Design's Part sidebar in the Plate view, shown or put away.
    PartSidebar(bool),
    /// Q pressed: the view pie opens round the pointer in the Plate view under it.
    ViewPie,
    /// The brush radius (mm).
    PaintRadius(f64),
    /// Erase on or off (Alt flips it while held).
    PaintErase(bool),
    /// Paint in the print colour at this index of `PlateInfo::colors`.
    PaintColor(usize),
    /// Paint a support channel, or the colour again.
    PaintTarget(PaintTarget),
    PaintMode(PaintMode),
    /// The smooth fill's largest angle between neighbouring normals (degrees).
    PaintAngle(f64),
    /// What a Pattern fill writes.
    PaintPattern(PaintPattern),
    SculptOp(StrokeOp),
    SculptRadius(f64),
    SculptStrength(f64),
    /// What a click of the Force tool adds.
    ForceKind(ForceKind),
    /// The size a new load of this kind takes.
    ForceSize(ForceKind, f64),
}

/// The Edit shape row's pick kinds.
const PICK_KINDS: [(&[LiveId], crate::tools::picks::PickKind); 5] = [
    (ids!(k_any), crate::tools::picks::PickKind::Any),
    (ids!(k_face), crate::tools::picks::PickKind::Face),
    (ids!(k_edge), crate::tools::picks::PickKind::Edge),
    (ids!(k_feature), crate::tools::picks::PickKind::Feature),
    (ids!(k_body), crate::tools::picks::PickKind::Body),
];

/// The Force strip's kinds.
const FORCE_KINDS: [(&[LiveId], ForceKind); 4] = [
    (ids!(f_fixed), ForceKind::Fixed),
    (ids!(f_force), ForceKind::Force),
    (ids!(f_pressure), ForceKind::Pressure),
    (ids!(f_torque), ForceKind::Torque),
];

/// Remove the load the feature tree picked (the Force strip's bin, Delete in the Force tool).
pub fn remove_focused_load(cx: &mut Cx, state: &AppState) {
    match state.force.focus {
        Some(i) => cx.action(ForceAction::Remove(i)),
        None => cx.action(ShellAction::Notice { view: NoticeView::Plate, text: "Pick a load first (click it with the Force tool, or in the feature tree), then remove it.".into() }),
    }
}

/// Apply a [`StageAction`]; the app calls this from its dispatch.
pub fn apply(state: &mut AppState, action: &StageAction) {
    match action {
        StageAction::Gizmo(m) => {
            state.ui.gizmo = *m;
            if *m != GizmoMode::Shape {
                state.ui.lay_flat = false;
            }
            crate::tools::paint::cancel();
            crate::tools::sculpt::cancel();
        }
        StageAction::LayFlat(on) => state.ui.lay_flat = *on,
        StageAction::Zones(on) => state.ui.show_zones = *on,
        StageAction::CameraPip(on) => state.ui.camera_pip = *on,
        StageAction::TestsSidebar(on) => state.ui.tests_sidebar = *on,
        StageAction::Nav(scheme) => {
            state.ui.nav = *scheme;
            crate::view_prefs::save_nav(*scheme);
        }
        StageAction::InvertWheel(on) => {
            state.ui.invert_wheel = *on;
            crate::view_prefs::save_invert_wheel(*on);
        }
        StageAction::PartSidebar(on) => state.ui.part_sidebar = *on,
        // (the Plate view under the pointer opens its pie: `FabStage::pie_event`)
        StageAction::ViewPie => {}
        StageAction::PaintRadius(r) => state.ui.paint_radius = r.clamp(crate::tools::paint::MIN_RADIUS, crate::tools::paint::MAX_RADIUS),
        StageAction::PaintColor(i) => {
            // Picking a material paints with it.
            if let Some(c) = state.plate.colors.get(*i) {
                let shown = state.print.colors.row(&c.name).map(|r| r.shown).unwrap_or([c.fallback[0], c.fallback[1], c.fallback[2], 1.0]);
                state.ui.paint_color = [shown[0], shown[1], shown[2]];
                state.ui.paint_name = Some(c.name.clone());
            }
            state.ui.paint_erase = false;
            state.ui.paint_target = PaintTarget::Color;
        }
        StageAction::PaintTarget(t) => {
            state.ui.paint_target = *t;
            state.ui.paint_erase = false;
            crate::tools::paint::cancel();
        }
        StageAction::PaintErase(on) => state.ui.paint_erase = *on,
        StageAction::PaintAngle(a) => state.ui.paint_angle = a.clamp(1.0, 180.0),
        StageAction::PaintPattern(p) => state.ui.paint_pattern = p.held(),
        StageAction::PaintMode(m) => {
            state.ui.paint_mode = *m;
            crate::tools::paint::cancel();
        }
        StageAction::SculptOp(op) => {
            state.ui.sculpt_op = *op;
            crate::tools::sculpt::cancel();
        }
        StageAction::SculptRadius(r) => state.ui.sculpt_radius = r.clamp(crate::tools::paint::MIN_RADIUS, crate::tools::paint::MAX_RADIUS),
        StageAction::SculptStrength(s) => state.ui.sculpt_strength = s.clamp(0.1, 1.0),
        StageAction::ForceKind(k) => state.ui.force_kind = *k,
        StageAction::ForceSize(k, v) => {
            if let Some((lo, hi)) = k.sizes() {
                state.ui.force_size[*k as usize] = v.clamp(lo, hi);
            }
        }
    }
}

impl ScriptHook for FabStage {
    fn on_after_apply(&mut self, _vm: &mut ScriptVm, apply: &Apply, _scope: &mut Scope, _value: ScriptValue) {
        if !apply.is_eval() {
            self.draw_keys = StageDrawKeys::default();
            self.synced = None;
            self.workspace_shown = None;
            self.print_revision = None;
            self.step_line = None;
            self.time_shown = None;
            self.layer_shown = None;
            self.now_shown = None;
            self.pane_label_shown = None;
            self.pane_label_room = None;
            self.pane_label_svg = None;
        }
    }
}

impl Widget for FabStage {
    fn semantics(&self, _cx: &Cx, node: &mut a11y_tree::Node) {
        node.role = a11y_tree::Role::Group;
        makepad_platform::a11y::set_string(&mut node.name, "Plate view");
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        if !self.bound {
            return;
        }
        if scope.data.get::<AppState>().is_some_and(|state| !state.ui.plate_shown(self.instance)) {
            self.handle_hidden(cx, event, scope);
            return;
        }
        // the view pie: open on Q over this view, aim with the pointer, release Q to take it
        if self.pie_event(cx, event) {
            return;
        }
        // View options' flyout closes on a press outside it (its button toggles it) and on Escape
        if self.options_open {
            let close = match event {
                Event::MouseDown(e) => {
                    let card = self.view.view(cx, ids!(view_flyout.options_card)).area().rect(cx);
                    let button = self.view.widget(cx, ids!(render_row.view_options)).area().rect(cx);
                    !card.contains(e.abs) && !button.contains(e.abs)
                }
                Event::KeyDown(k) => k.key_code == KeyCode::Escape,
                _ => false,
            };
            if close {
                self.options_open = false;
                self.view.redraw(cx);
            }
        }
        // the Part sidebar's left edge: a drag sets its width (kept when it lets go)
        let grip = self.view.view(cx, ids!(model_ui.part_side.part_col.part_room.part_card.part_grip));
        let lit = |cx: &mut Cx, on: f32| {
            if let Some(mut g) = grip.borrow_mut() { g.draw_bg.set_dyn_instance(cx, id!(hover), &[on]); }
            grip.redraw(cx);
        };
        if grip.visible() {
            match event.hits(cx, grip.area()) {
                Hit::FingerHoverIn(_) => {
                    cx.set_cursor(MouseCursor::EwResize);
                    lit(cx, 1.0);
                }
                Hit::FingerHoverOut(_) if self.part_drag.is_none() => lit(cx, 0.0),
                Hit::FingerDown(fe) if fe.device.is_primary_hit() => {
                    if let Some(state) = scope.data.get::<AppState>() {
                        self.part_drag = Some((fe.abs.x, state.ui.part_side_w));
                    }
                }
                Hit::FingerMove(fe) => {
                    if let (Some((x0, w0)), Some(state)) = (self.part_drag, scope.data.get_mut::<AppState>()) {
                        state.ui.part_side_w = (w0 + x0 - fe.abs.x).clamp(PART_SIDE_MIN_W, PART_SIDE_MAX_W);
                        self.view.redraw(cx);
                    }
                }
                Hit::FingerUp(_) => {
                    self.part_drag = None;
                    lit(cx, 0.0);
                    if let Some(state) = scope.data.get::<AppState>() {
                        crate::view_prefs::save_part_side_w(state.ui.part_side_w);
                    }
                    self.view.redraw(cx);
                }
                _ => {}
            }
        }
        // the tests sidebar's sections: their hover, and a swipe across them folding them all
        if matches!(event, Event::MouseMove(_) | Event::MouseDown(_) | Event::MouseUp(_) | Event::MouseLeave(_) | Event::ClearHover)
            && self.view.view(cx, ids!(model_ui.tests_side)).visible()
        {
            let heads = self.view.widget(cx, ids!(tests_side.tests)).borrow::<crate::ui::test_prints::FabTestSections>().map(|t| t.headers()).unwrap_or_default();
            crate::ui::widgets::section_headers(cx, event, &heads);
        }
        if let Some(state) = scope.data.get_mut::<AppState>() {
            match event {
                Event::MouseDown(event) => {
                    // A press anywhere in this view (its toolbar too) makes it the Plate that
                    // Frame all, Ortho and the presets act on.
                    if self.view.area().rect(cx).contains(event.abs) && state.view.plate_number(self.pane).is_some() {
                        state.view.pressed_plate = self.pane;
                    }
                    for (id, path) in bound_panes(self.pane) {
                        let Some(pane) = state.view.panes.get_mut(id.0 as usize).and_then(Option::as_mut) else { continue };
                        let viewport = self.view.widget(cx, path);
                        pane.pointer_held |= viewport.area().rect(cx).contains(event.abs);
                    }
                }
                Event::MouseUp(_) | Event::WindowLostFocus(_) | Event::Pause | Event::Background => {
                    for (id, _) in bound_panes(self.pane) {
                        if let Some(pane) = state.view.panes.get_mut(id.0 as usize).and_then(Option::as_mut) { pane.pointer_held = false; }
                    }
                }
                Event::TouchUpdate(event) => {
                    for (id, path) in bound_panes(self.pane) {
                        let Some(pane) = state.view.panes.get_mut(id.0 as usize).and_then(Option::as_mut) else { continue };
                        let viewport = self.view.widget(cx, path);
                        let rect = viewport.area().rect(cx);
                        let held = event.touches.iter().any(|touch| !matches!(touch.state, TouchState::Stop | TouchState::Cancel));
                        pane.pointer_held = held && (pane.pointer_held || event.touches.iter()
                            .any(|touch| touch.state == TouchState::Start && rect.contains(touch.abs)));
                    }
                }
                _ => {}
            }
            if self.sync_panes(cx, state) {
                self.view.widget(cx, ids!(pane_label.label)).redraw(cx);
            }
        }
        // The viewport's generic FingerScroll path respects handled_x/y.
        // A held part owns the wheel: dispatch it once, without also zooming.
        if let Event::Scroll(scroll) = event {
            let grab = self.view.widget(cx, ids!(mech_grab));
            let holding = grab.borrow::<crate::tools::mech_grab::FabMechGrab>().is_some_and(|g| g.is_holding());
            if holding && self.view.widget(cx, ids!(viewport)).area().rect(cx).contains(scroll.abs) {
                grab.handle_event(cx, event, scope);
                return;
            }
        }
        if matches!(event, Event::WindowLostFocus(_) | Event::Pause | Event::Background) {
            crate::tools::paint::cancel();
            crate::tools::sculpt::cancel();
            self.view.redraw(cx);
        }
        self.view.handle_event(cx, event, scope);
        if let Some(state) = scope.data.get_mut::<AppState>() {
            self.sync_trace_interaction(cx, state);
        }
        if matches!(event, Event::MouseDown(_) | Event::MouseMove(_) | Event::MouseUp(_) | Event::TouchUpdate(_)) {
            let viewport = self.view.widget(cx, ids!(viewport));
            let input = viewport.borrow_mut::<crate::viewport::FabViewport>().and_then(|mut v| v.take_pointer_input());
            if let (Some(input), Some(state)) = (input, scope.data.get_mut::<AppState>()) {
                if !state.ui.in_mode(PlateMode::Mechanics) {
                    if let Some(mut overlay) = self.view.widget(cx, ids!(design_overlay.tool_overlay)).borrow_mut::<crate::tools::overlay::FabToolOverlay>() {
                        overlay.handle_viewport_input(cx, &input, state);
                    }
                    if let Some(mut gizmo) = self.view.widget(cx, ids!(design_overlay.part_gizmo)).borrow_mut::<crate::tools::transform::FabPartGizmo>() {
                        gizmo.handle_viewport_input(cx, &input, state);
                    }
                    if let Some(mut handles) = self.view.widget(cx, ids!(design_overlay.shape_handles)).borrow_mut::<crate::tools::handles::FabShapeHandles>() {
                        handles.handle_viewport_input(cx, &input, state);
                    }
                    if let Some(mut cube) = self.view.widget(cx, ids!(design_overlay.test_cube)).borrow_mut::<crate::tools::test_cube::FabTestCube>() {
                        cube.handle_viewport_input(cx, &input, state);
                    }
                }
            }
        }
        let Event::Actions(actions) = event else { return };
        let Some(state) = scope.data.get::<AppState>() else { return };
        let v = &self.view;
        // a press on an Underside view's own chip turns See through on or off
        if v.view(cx, ids!(pane_label)).finger_up(actions).is_some_and(|e|
            e.is_over && !e.cancelled && !e.is_sweep && e.device.mouse_button().is_none_or(|b| b.is_primary())
        ) {
            let link = state.view.pane(self.pane).link;
            let next = match link.how() {
                Some(LinkHow::Flipped) => Some(LinkHow::Through),
                Some(LinkHow::Through) => Some(LinkHow::Flipped),
                _ => None,
            };
            if let (Some(how), Some(to)) = (next, link.source()) {
                cx.action(ShellAction::SetPaneLink { pane: self.pane, link: PaneLink::Linked { how, to } });
            }
        }
        for action in actions.filter_widget_actions(v.widget(cx, ids!(viewport)).widget_uid()) {
            let Some(click) = action.action.downcast_ref::<crate::viewport::ViewportContextMenu>() else { continue };
            if state.ui.in_mode(PlateMode::Mechanics) {
                if let Some(menu) = v.widget(cx, ids!(mechanics_ui.actions)).borrow::<crate::ui::mech_actions::FabMechActions>() {
                    menu.open_context(cx, state, self.pane, click.at);
                }
            } else {
                self.menu_area = click.area.clone();
                self.menu_part = click.part.clone();
                self.menu_feature = click.feature.clone();
                let area = click.area.as_ref().and_then(|o| state.params.areas.iter().find(|a| a.object == *o)).map(|a| a.kind);
                open_menu(cx, CONTEXT_MENU, Self::context_menu(state, click.branch.is_some(), area, click.part.is_some(), click.feature.as_deref()), Rect { pos: click.at, size: dvec2(0.0, 0.0) }, MenuPlace::At);
            }
        }
        if state.ui.in_mode(PlateMode::Mechanics) {
            if actions.iter().any(|a| matches!(a.downcast_ref::<MechAction>(), Some(MechAction::Tool(_)))) {
                self.draw_keys.mechanics_tool = None;
            }
            let tools = v.view(cx, ids!(mechanics_ui.left.tools));
            crate::ui::mechanics::handle_tools(cx, &tools, actions);
        }
        // View options: its button opens and closes the flyout; a tile acts as its option
        if dropdown_clicked(actions, live_id!(fab_view_options)).is_some() {
            self.options_open = !self.options_open;
            cx.redraw_area(v.area());
        }
        let card = v.view(cx, ids!(view_flyout.options_card));
        for (path, pick) in [
            (ids!(o_rails), live_id!(vo_rails)), (ids!(o_ortho), live_id!(vo_ortho)), (ids!(o_frame_all), live_id!(vo_frame_all)),
            (ids!(o_frame_sel), live_id!(vo_frame_sel)), (ids!(o_colours), live_id!(vo_colours)), (ids!(o_supports), live_id!(vo_supports)),
            (ids!(o_travel), live_id!(vo_travel)), (ids!(o_parts), live_id!(vo_parts)), (ids!(o_board), live_id!(vo_board)),
        ] {
            let tile = card.radio_button(cx, path);
            if tile.clicked(actions) {
                // (a framing tile is an action: it does not stay lit)
                if matches!(pick, p if p == live_id!(vo_frame_all) || p == live_id!(vo_frame_sel)) {
                    tile.set_active(cx, false, Animate::No);
                }
                Self::view_option(cx, self.instance, state, pick);
            }
        }
        if v.button(cx, ids!(render_row.drop_part)).clicked(actions) {
            if let Some(i) = active_part(state).filter(|_| !state.slice_blocks_edits()) {
                cx.action(PartAction::Drop { object: state.plate.parts[i].object.clone() });
            }
        }
        let tools = ids!(model_ui.left.tools);
        let tv = v.view(cx, tools);
        if tv.radio_button(cx, ids!(t_select)).clicked(actions) {
            Self::set_mode(cx, state, GizmoMode::Off);
        }
        if tv.radio_button(cx, ids!(t_move)).clicked(actions) {
            Self::set_mode(cx, state, GizmoMode::Move);
        }
        if tv.radio_button(cx, ids!(t_rotate)).clicked(actions) {
            Self::set_mode(cx, state, GizmoMode::Rotate);
        }
        if tv.radio_button(cx, ids!(t_scale)).clicked(actions) {
            Self::set_mode(cx, state, GizmoMode::Scale);
        }
        if tv.radio_button(cx, ids!(t_shape)).clicked(actions) {
            Self::set_mode(cx, state, GizmoMode::Shape);
        }
        if tv.radio_button(cx, ids!(t_paint)).clicked(actions) {
            Self::set_mode(cx, state, GizmoMode::Paint);
            cx.redraw_area(v.area());
        }
        if tv.radio_button(cx, ids!(t_sculpt)).clicked(actions) {
            Self::set_mode(cx, state, GizmoMode::Sculpt);
            cx.redraw_area(v.area());
        }
        // Arrange: the rail's button opens its row (and closes it); the row's Arrange arranges
        if tv.radio_button(cx, ids!(arrange_all)).clicked(actions) {
            self.arranging = !self.arranging;
            self.draw_keys.tools = None;
            cx.redraw_area(v.area());
        }
        let arrange = v.widget(cx, ids!(model_ui.context.arrange_bar));
        if arrange.button(cx, ids!(arrange_go)).clicked(actions) && !state.slice_blocks_edits() {
            cx.action(PartAction::Arrange);
        }
        if arrange.button(cx, ids!(arrange_done)).clicked(actions) {
            self.arranging = false;
            self.draw_keys.tools = None;
            cx.redraw_area(v.area());
        }
        // its gap: a finite length, none below zero, and only a change
        if let Some(gap) = arrange.value_input(cx, ids!(arrange_gap)).ended(actions) {
            let now = state.plate.arrange_gap.unwrap_or(8.0);
            if gap.is_finite() && gap >= 0.0 && (gap - now).abs() > 1e-9 && !state.slice_blocks_edits() {
                cx.action(PartAction::SetArrangeGap(gap));
            }
        }
        if tv.radio_button(cx, ids!(t_force)).clicked(actions) {
            Self::force_tool(cx, state);
            cx.redraw_area(v.area());
        }
        // The paint strip beside the rail.
        let pb = v.widget(cx, ids!(model_ui.context.paint_bar));
        if pb.radio_button(cx, ids!(p_brush)).clicked(actions) {
            self.draw_keys.tools = None;
            cx.action(StageAction::PaintErase(false));
            cx.action(StageAction::PaintMode(PaintMode::Brush));
        }
        if pb.radio_button(cx, ids!(p_erase)).clicked(actions) {
            self.draw_keys.tools = None;
            cx.action(StageAction::PaintErase(!state.ui.paint_erase));
        }
        for (id, mode) in [(ids!(p_feature), PaintMode::Feature), (ids!(p_part), PaintMode::Part), (ids!(p_smooth), PaintMode::Flood), (ids!(p_pattern), PaintMode::Pattern)] {
            if pb.radio_button(cx, id).clicked(actions) {
                self.draw_keys.tools = None;
                cx.action(StageAction::PaintMode(mode));
            }
        }
        // A support channel toggles: a second click paints the colour again.
        for (id, target) in [(ids!(p_block), PaintTarget::SupportBlock), (ids!(p_enforce), PaintTarget::SupportEnforce)] {
            if pb.radio_button(cx, id).clicked(actions) {
                self.draw_keys.tools = None;
                cx.action(StageAction::PaintTarget(if state.ui.paint_target == target { PaintTarget::Color } else { target }));
            }
        }
        let sb = v.widget(cx, ids!(model_ui.context.shape_bar));
        for (id, op) in [
            (ids!(s_cut_box), Some(ShapeOp::Subtract(ShapePrim::Box))),
            (ids!(s_cut_cyl), Some(ShapeOp::Subtract(ShapePrim::Cylinder))),
            (ids!(s_cut_sphere), Some(ShapeOp::Subtract(ShapePrim::Sphere))),
            (ids!(s_add_box), Some(ShapeOp::Add(ShapePrim::Box))),
            (ids!(s_add_cyl), Some(ShapeOp::Add(ShapePrim::Cylinder))),
            (ids!(s_add_sphere), Some(ShapeOp::Add(ShapePrim::Sphere))),
            (ids!(s_cut_bottom), Some(ShapeOp::CutBottom)),
            (ids!(s_cut_plane), Some(ShapeOp::CutPlane)),
            (ids!(s_split), Some(ShapeOp::Split)),
            (ids!(s_place), None),
        ] {
            if sb.radio_button(cx, id).clicked(actions) {
                self.draw_keys.tools = None;
                if let Some(object) = active_part(state).map(|i| state.plate.parts[i].object.clone()) {
                    cx.action(match op {
                        Some(op) => PartAction::Shape { object, op },
                        None => PartAction::Drop { object },
                    });
                } else {
                    cx.action(ShellAction::Notice { view: NoticeView::Plate, text: "Select a part first.".into() });
                }
                cx.redraw_area(v.area());
            }
        }
        for (id, kind) in PICK_KINDS {
            if sb.radio_button(cx, id).clicked(actions) {
                self.draw_keys.tools = None;
                cx.action(crate::tools::picks::PickAction::Filter(kind));
            }
        }
        if sb.radio_button(cx, ids!(s_lay_flat)).clicked(actions) {
            self.draw_keys.tools = None;
            cx.action(StageAction::LayFlat(!state.ui.lay_flat));
        }
        let fit = v.widget(cx, ids!(model_ui.context.fit_bar));
        for (id, action) in [(ids!(fit_turn), ShellAction::TurnFit), (ids!(fit_clear), ShellAction::ClearFit)] {
            let button = fit.radio_button(cx, id);
            if button.clicked(actions) {
                button.set_active(cx, false, Animate::No);
                cx.action(action);
            }
        }
        let sb = v.widget(cx, ids!(model_ui.context.sculpt_bar));
        for (id, op) in [(ids!(s_raise), StrokeOp::Raise), (ids!(s_lower), StrokeOp::Lower), (ids!(s_crease), StrokeOp::Crease)] {
            if sb.radio_button(cx, id).clicked(actions) {
                self.draw_keys.tools = None;
                cx.action(StageAction::SculptOp(op));
            }
        }
        let fb = v.widget(cx, ids!(model_ui.context.force_bar));
        for (id, kind) in FORCE_KINDS {
            if fb.radio_button(cx, id).clicked(actions) {
                self.draw_keys.tools = None;
                cx.action(StageAction::ForceKind(kind));
            }
        }
        if fb.radio_button(cx, ids!(f_remove)).clicked(actions) {
            self.draw_keys.tools = None;
            remove_focused_load(cx, state);
            cx.redraw_area(v.area());
        }
        if tv.radio_button(cx, ids!(t_clip)).clicked(actions) {
            cx.action(ShellAction::SetTool(if state.tool == Tool::Section { Tool::Select } else { Tool::Section }));
        }
        if tv.radio_button(cx, ids!(t_measure)).clicked(actions) {
            let m = Tool::Measure(MeasureKind::Distance);
            cx.action(ShellAction::SetTool(if matches!(state.tool, Tool::Measure(_) | Tool::MeasureFit) { Tool::Select } else { m }));
        }
        if tv.radio_button(cx, ids!(t_no_trees)).clicked(actions) {
            cx.action(ShellAction::SetTool(if state.tool == Tool::NoTrees { Tool::Select } else { Tool::NoTrees }));
        }
        if tv.radio_button(cx, ids!(t_settings_here)).clicked(actions) {
            if state.tool == Tool::SettingsHere {
                cx.action(ShellAction::SetTool(Tool::Select));
            } else {
                arm_settings_here(cx, state);
            }
        }
        if let Some(o) = v.slider(cx, ids!(render_row.see_row.see_opacity)).slided(actions) {
            cx.action(ShellAction::SetSeeOpacity(o as f32));
        }
        if let Some(g) = v.slider(cx, ids!(render_row.see_row.see_glow)).slided(actions) {
            cx.action(ShellAction::SetSeeGlow(g as f32));
        }
        // the plate's mode: a menu of the three, the one in force ticked
        if let Some(anchor) = dropdown_clicked(actions, live_id!(fab_plate_mode)) {
            // Design shows the whole plate; Test prints is Design with only the ticked tests
            let tests = state.ui.in_mode(PlateMode::Design) && state.print.test_mode;
            let row = |mode: PlateMode, label: &str| MenuItem::new(LiveId(live_id!(fab_plate_mode).0 ^ (mode as u64 + 1)), label).radio(state.ui.in_mode(mode) && !(tests && mode == PlateMode::Design));
            let items = vec![
                row(PlateMode::Design, "Design"),
                MenuItem::new(live_id!(fab_plate_mode_tests), "Test prints").radio(tests),
                row(PlateMode::Mechanics, "Mechanics"),
                row(PlateMode::Monitor, "Monitor"),
            ];
            open_menu(cx, live_id!(fab_plate_mode), items, anchor, MenuPlace::Below);
        }
        if let Some(pick) = menu_picked(actions, live_id!(fab_plate_mode)) {
            if pick == live_id!(fab_plate_mode_tests) {
                if !state.ui.in_mode(PlateMode::Design) {
                    cx.action(ShellAction::SetPlateMode(PlateMode::Design));
                }
                if !state.print.test_mode {
                    cx.action(PrintAction::TestMode(true));
                }
            } else if let Some(mode) = [PlateMode::Design, PlateMode::Mechanics, PlateMode::Monitor].into_iter().find(|m| pick.0 == live_id!(fab_plate_mode).0 ^ (*m as u64 + 1)) {
                cx.action(ShellAction::SetPlateMode(mode));
                // every other mode prints the whole plate
                if state.print.test_mode {
                    cx.action(PrintAction::TestMode(false));
                }
            }
        }
        // Test prints: a cube on the picked part, and the tests' sidebar
        if v.button(cx, ids!(render_row.add_test)).clicked(actions) || v.button(cx, ids!(tests_side.side_add)).clicked(actions) {
            crate::ui::test_prints::add(cx, state, None);
        }
        if v.radio_button(cx, ids!(render_row.tests_toggle)).clicked(actions) {
            cx.action(StageAction::TestsSidebar(!state.ui.tests_sidebar));
        }
        if v.button(cx, ids!(tests_side.tests_close)).clicked(actions) {
            cx.action(StageAction::TestsSidebar(false));
        }
        // Design's Part sidebar: the toolbar's Part, and the sidebar's own close
        if v.radio_button(cx, ids!(render_row.part_toggle)).clicked(actions) {
            cx.action(StageAction::PartSidebar(!state.ui.part_sidebar));
        }
        if v.button(cx, ids!(part_side.part_col.part_room.part_card.part.editor_bar.side_close)).clicked(actions) {
            cx.action(StageAction::PartSidebar(false));
        }
        // the render mode's flyout: Normal, Sliced, and Force on Design's plate
        if let Some(anchor) = dropdown_clicked(actions, live_id!(fab_render_mode)) {
            let mode = state.render_mode();
            let normal = !matches!(mode, RenderMode::Slice | RenderMode::Force);
            let mut items = vec![
                MenuItem::new(live_id!(rm_normal), "Normal").radio(normal).key(&keymap::shortcut(Key::SliceView)),
                MenuItem::new(live_id!(rm_slice), "Sliced").radio(mode == RenderMode::Slice).key(&keymap::shortcut(Key::SliceView)),
            ];
            if state.ui.in_mode(PlateMode::Design) {
                items.push(MenuItem::new(live_id!(rm_force), "Force").radio(mode == RenderMode::Force));
            }
            open_menu(cx, live_id!(fab_render_mode), items, anchor, MenuPlace::Below);
        }
        match menu_picked(actions, live_id!(fab_render_mode)) {
            Some(id) if id == live_id!(rm_normal) => cx.action(ShellAction::SetRenderMode(state.normal_mode())),
            Some(id) if id == live_id!(rm_slice) => cx.action(ShellAction::SetRenderMode(RenderMode::Slice)),
            Some(id) if id == live_id!(rm_force) => cx.action(ShellAction::SetRenderMode(RenderMode::Force)),
            _ => {}
        }
        // ---- slice view ----
        let sv = v.view(cx, ids!(slice_ui));
        if let Some(anchor) = dropdown_clicked(actions, SPEED_MENU) {
            let items = speed_menu(state.print.play.speed).into_iter().filter(|item| speed_in_menu(item.id, SpeedMenuKind::Print)).collect();
            open_menu(cx, SPEED_MENU, items, anchor, MenuPlace::Below);
        }
        if let Some(id) = menu_picked(actions, SPEED_MENU) {
            if let Some(speed) = speed_pick(id) {
                cx.action(PrintAction::SetSpeed(speed));
            }
        }
        let row = sv.view(cx, ids!(bottom.bar_row.bar.row));
        if row.button(cx, ids!(print_play)).clicked(actions) {
            cx.action(PrintAction::Play(true));
        }
        if row.button(cx, ids!(print_pause)).clicked(actions) {
            cx.action(PrintAction::Play(false));
        }
        if let Some(pv) = state.print.showing() {
            let (a, b) = layer_span(pv, shown_top_layer(&state.print, pv));
            if row.button(cx, ids!(to_start)).clicked(actions) {
                cx.action(PrintAction::Scrub(a));
            }
            if row.button(cx, ids!(to_end)).clicked(actions) {
                cx.action(PrintAction::Scrub(b));
            }
        }
        if v.button(cx, ids!(model_ui.bottom.step_shown.step_back)).clicked(actions) {
            cx.action(crate::ui::steps::StepsAction::Show(None));
        }
        if v.button(cx, ids!(model_ui.bottom.step_shown.step_rollback)).clicked(actions) {
            if let Some(i) = state.steps.shown.filter(|i| i + 1 < state.steps.chips.len()) {
                cx.action(crate::ui::steps::StepsAction::RollBack(i));
            }
        }
        if sv.button(cx, ids!(bottom.bar_row.bar.follow_row.back_live)).clicked(actions) || sv.button(cx, ids!(resin_ui.resin_row.resin_live)).clicked(actions) {
            cx.action(PrintAction::FollowPrinter(true));
        }
        // ---- the part menu ----
        if let Some(pick) = menu_picked(actions, CONTEXT_MENU) {
            let objects = Self::selected_objects(state);
            crate::ui::parts::part_menu_action(cx, state, &objects, pick);
            match pick {
                id if id == live_id!(ctx_drop) => objects.into_iter().for_each(|object| cx.action(PartAction::Drop { object })),
                id if id == live_id!(ctx_frame_sel) => cx.action(ShellAction::FrameSelected),
                id if id == live_id!(ctx_frame_all) => cx.action(ShellAction::FrameAll),
                id if id == live_id!(ctx_arrange) => cx.action(PartAction::Arrange),
                id if id == live_id!(ctx_add_box) => cx.action(PartAction::Add { shape: "box".into() }),
                id if id == live_id!(ctx_add_cyl) => cx.action(PartAction::Add { shape: "cylinder".into() }),
                id if id == live_id!(ctx_settings_here) => settings_here(cx, state, self.menu_part.take()),
                id if id == live_id!(ctx_test_cube) => crate::ui::test_prints::add(cx, state, self.menu_part.take()),
                id if id == live_id!(ctx_override_feature) => {
                    if let Some(feature) = self.menu_feature.take() {
                        add_volume(cx, state, self.menu_part.take(), Some(feature), "follows");
                    }
                }
                id if id == live_id!(ctx_delete_area) => {
                    if let Some(object) = self.menu_area.take() {
                        cx.action(PartAction::DeleteArea { object });
                    }
                }
                id if id == live_id!(ctx_delete_branch) => {
                    if let (Some(_), Some((object, tips))) = (state.print.picked_branch(), state.print.picked_branch.clone()) {
                        cx.action(PrintAction::PickBranch(None));
                        cx.action(PartAction::DeleteTree { object, tips });
                    }
                }
                _ => {}
            }
        }
        // a playhead step redraws only what shows the playhead (the app asks for that)
        if actions.iter().any(|a| a.downcast_ref::<PrintAction>().is_some_and(|a| !a.moves_playhead()) || a.downcast_ref::<StageAction>().is_some()) {
            self.view.redraw(cx);
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        if !self.bound {
            return DrawStep::done();
        }
        if let Some(state) = scope.data.get_mut::<AppState>() {
            self.sync_trace_interaction(cx.cx, state);
            self.sync_panes(cx, state);
            self.fit_pane_label(cx, walk);
            // the one-per-window chrome (the view's render row, the slice bars, the job and
            // loading cards, the camera picture) shows in the lead Plate alone
            let lead = self.pane == state.view.lead_plate();
            let p = &state.print;
            let print = self.draw_keys.print_chrome(p);
            let PrintChrome { held, resin, resin_live } = print;
            let slice = p.play.active || held;
            let v = &self.view;
            let (plate_mode, mode) = self.draw_keys.modes(state);
            let design = plate_mode == Some(PlateMode::Design);
            let mechanics = plate_mode == Some(PlateMode::Mechanics);
            let workspace = (plate_mode, slice);
            if self.workspace_shown != Some(workspace) {
                self.workspace_shown = Some(workspace);
                // which of the rail's tools show (one hidden since the start was never drawn,
                // and its own redraw reaches no list)
                v.view(cx, ids!(model_ui.left.tools)).redraw(cx);
                // The tools stay over Design's slice: the plate is edited under it.
                v.view(cx, ids!(model_ui)).set_visible(cx, !mechanics && (!slice || design));
                // Mechanics keeps the tool overlay for its joint check's marks, without Design's handles.
                for id in [ids!(design_overlay.part_gizmo), ids!(design_overlay.shape_handles), ids!(design_overlay.test_cube), ids!(design_overlay.force_editor)] {
                    v.widget(cx, id).set_visible(cx, !mechanics);
                }
                v.view(cx, ids!(mechanics_ui)).set_visible(cx, mechanics);
            }
            let mech_tool = mechanics.then_some(state.mech.tool);
            if self.draw_keys.mechanics_tool != mech_tool {
                self.draw_keys.mechanics_tool = mech_tool;
                if let Some(tool) = mech_tool {
                    let tools = v.view(cx, ids!(mechanics_ui.left.tools));
                    crate::ui::mechanics::sync_tools(cx, &tools, tool);
                }
            }
            // The gizmo and Mechanics' tools sit under Mechanics' header strip, as tall as the
            // strip last drew (a narrow view wraps it); a height that changed draws once more.
            // Per draw: three lookups.
            let strip = if mechanics {
                let actions = v.widget(cx, ids!(mechanics_ui.actions));
                let h = actions.borrow::<crate::ui::mech_actions::FabMechActions>().map_or(0.0, |a| a.strip_height());
                h
            } else {
                0.0
            };
            // and under the top toolbar while it shows, so the gizmo takes none of its width (its
            // height is fixed: an area of the last frame reads empty here, mid-draw)
            let toolbar = if !mechanics && v.view(cx, ids!(render_row)).visible() { RENDER_ROW_TOP + RENDER_ROW_H } else { 0.0 };
            // and under the chosen tool's row across the top (Paint, Edit shape, Sculpt, Force),
            // while it shows: the rails shown, the tool one with a row
            let row_tool = match state.ui.gizmo {
                GizmoMode::Paint | GizmoMode::Shape | GizmoMode::Sculpt => true,
                GizmoMode::Force => mode == RenderMode::Force,
                _ => false,
            };
            let context = !mechanics && design && ((state.tool == Tool::Select && row_tool) || state.tool == Tool::MeasureFit || self.arranging) && v.view(cx, ids!(model_ui.context)).visible();
            let nav_top = NAV_TOP + strip + toolbar + if context { CONTEXT_ROW } else { 0.0 };
            let nav = v.widget(cx, ids!(primary.nav));
            let moved = nav.borrow_mut::<View>().is_some_and(|mut nav| {
                let moved = (nav.layout.padding.top - nav_top).abs() > 0.25;
                if moved {
                    nav.layout.padding.top = nav_top;
                }
                moved
            });
            if moved {
                nav.redraw(cx);
            }
            if mechanics {
                let tools_top = strip.max(MECH_STRIP_ROW) + MECH_TOOLS_GAP;
                let left = v.widget(cx, ids!(mechanics_ui.left));
                let moved = left.borrow_mut::<View>().is_some_and(|mut left| {
                    let moved = (left.layout.padding.top - tools_top).abs() > 0.25;
                    if moved {
                        left.layout.padding.top = tools_top;
                    }
                    moved
                });
                if moved {
                    left.redraw(cx);
                }
            }
            let looks = !held;
            let chrome = ChromeKey {
                slice, mechanics, lead, print, camera_pip: state.ui.camera_pip,
            };
            if self.draw_keys.chrome != Some(chrome) {
                self.draw_keys.chrome = Some(chrome);
                v.view(cx, ids!(model_ui.bottom)).set_visible(cx, !slice);
                // Every Plate view has its toolbar; the sim's view has none (its bar holds the mode).
                v.view(cx, ids!(render_row)).set_visible(cx, !mechanics);
                v.widget(cx, ids!(render_row.render_tip)).set_visible(cx, looks);
                v.view(cx, ids!(render_row.create_card)).set_visible(cx, looks);
                // a group's rule goes with it (a held job's bar holds only the mode)
                for path in [ids!(render_row.view_options)] {
                    v.widget(cx, path).set_visible(cx, looks);
                }
                v.view(cx, ids!(slice_ui)).set_visible(cx, slice && lead);
                v.view(cx, ids!(slice_ui.resin_ui)).set_visible(cx, resin);
                // Monitor stopped following the resin job when the layers moved by hand
                v.button(cx, ids!(slice_ui.resin_ui.resin_row.resin_live)).set_visible(cx, resin_live);
                v.widget(cx, ids!(progress_card)).set_visible(cx, lead);
            }
            // The plate's mode in its flyout (Test prints: Design with the ticked tests alone), and
            // Test prints' toolbar group and sidebar of tests
            let tests_mode = design && p.test_mode;
            let tests_key = (plate_mode, tests_mode, state.ui.tests_sidebar && lead, active_part(state).is_some(), p.revision);
            if self.draw_keys.tests != Some(tests_key) {
                self.draw_keys.tests = Some(tests_key);
                let name = match plate_mode {
                    Some(PlateMode::Mechanics) => "Mechanics",
                    Some(PlateMode::Monitor) => "Monitor",
                    _ if tests_mode => "Test prints",
                    _ => "Design",
                };
                v.widget(cx, ids!(render_row.plate_mode.label)).set_text(cx, name);
                v.widget(cx, ids!(render_row.tests_rule)).set_visible(cx, tests_mode);
                v.widget(cx, ids!(render_row.tests_group)).set_visible(cx, tests_mode);
                v.radio_button(cx, ids!(render_row.tests_toggle)).set_active(cx, tests_key.2, Animate::No);
                let has_part = tests_key.3;
                v.widget(cx, ids!(render_row.add_test)).set_disabled(cx, !has_part);
                v.widget(cx, ids!(tests_side.side_add)).set_disabled(cx, !has_part);
                for tip in [ids!(render_row.add_test_tip), ids!(tests_side.side_add_tip)] {
                    if let Some(mut tip) = v.widget(cx, tip).borrow_mut::<makepad_widgets_core::tip::Tip>() {
                        tip.text = if has_part { crate::ui::test_prints::ADD_TIP } else { SELECT_A_PART }.to_string();
                    }
                }
                v.view(cx, ids!(model_ui.tests_side)).set_visible(cx, tests_mode && tests_key.2);
            }
            // Design's Part sidebar: in the lead view of Design (not its test prints, whose tests
            // take that side) while a part is picked and the view is wide enough, left of a
            // slice's layer slider; the toolbar's Part shows or puts it away
            let wide = cx.peek_walk_turtle(walk).size.x >= PART_SIDE_MIN_VIEW;
            let offered = lead && design && !tests_mode && wide;
            let picked = offered && active_part(state).is_some();
            let part_key = (offered, picked, state.ui.part_sidebar, slice);
            if self.draw_keys.part_side != Some(part_key) {
                self.draw_keys.part_side = Some(part_key);
                let (offered, picked, on, _) = part_key;
                v.widget(cx, ids!(render_row.part_toggle)).set_visible(cx, offered);
                v.widget(cx, ids!(render_row.part_toggle)).set_disabled(cx, !picked);
                v.radio_button(cx, ids!(render_row.part_toggle)).set_active(cx, on && picked, Animate::No);
                if let Some(mut tip) = v.widget(cx, ids!(render_row.part_tip)).borrow_mut::<makepad_widgets_core::tip::Tip>() {
                    tip.text = if picked { "Part: the picked part's editor in a sidebar at the view's right" } else { "Part: pick a part to edit it here" }.to_string();
                }
                v.view(cx, ids!(model_ui.part_side)).set_visible(cx, picked && on);
            }
            if lead {
                state.ui.part_side_shown = part_key.1 && part_key.2;
            }
            // its width, as dragged: the card's, and the room a slice's layer slider keeps beside it
            let part_w = state.ui.part_side_w;
            if self.part_w_shown != Some(part_w) {
                self.part_w_shown = Some(part_w);
                let paths: [(&[LiveId], f64); 1] = [(ids!(model_ui.part_side.part_col.part_room.part_card), part_w)];
                for (path, w) in paths {
                    let view = v.view(cx, path);
                    if let Some(mut inner) = view.borrow_mut() {
                        inner.walk.width = Size::Fixed(w);
                    }
                    view.redraw(cx);
                }
            }
            // View options' flyout, while open: each tile as the view has it
            let open = self.options_open && lead && !mechanics;
            let flyout = v.view(cx, ids!(view_flyout));
            if flyout.visible() != open {
                flyout.set_visible(cx, open);
            }
            if open {
                Self::sync_options(cx, &self.view, self.pane, state);
                let totals = crate::ui::test_prints::totals(state);
                v.label(cx, ids!(tests_side.tests_totals)).set_visible(cx, totals.is_some());
                v.label(cx, ids!(tests_side.tests_totals)).set_text(cx, totals.as_deref().unwrap_or(""));
            }
            // Filament colours' toggle and the notice beside it.
            let colours = state.view.colours;
            let key = (colours, state.colour_notice_tools(), p.target_printer().is_some(), state.view.missing_spool_colours, slice, held, p.play.color, p.finish);
            if self.draw_keys.colours != Some(key) {
                self.draw_keys.colours = Some(key);
                let notice = state.colour_notice();
                v.widget(cx, ids!(colour_notice)).set_visible(cx, notice.is_some());
                if let Some(mut tip) = v.widget(cx, ids!(colour_notice_tip)).borrow_mut::<makepad_widgets_core::tip::Tip>() {
                    tip.text = notice.unwrap_or_default();
                }
            }
            // the tool overlay's lines start under the gizmo
            let overlay = v.widget(cx, ids!(design_overlay.tool_overlay));
            let moved = overlay.borrow_mut::<crate::tools::overlay::FabToolOverlay>().is_some_and(|mut o| {
                let moved = (o.nav_top - nav_top).abs() > 0.25;
                o.nav_top = nav_top;
                moved
            });
            if moved {
                overlay.redraw(cx);
            }
            // Drop and Frame selected act on the selected part: off without one (Drop also while
            // the slice holds the plate)
            let picked = active_part(state).is_some();
            let part_actions = (picked, picked && !state.slice_blocks_edits());
            if self.draw_keys.part_actions != Some(part_actions) {
                self.draw_keys.part_actions = Some(part_actions);
                v.button(cx, ids!(render_row.drop_part)).set_enabled(cx, part_actions.1);
            }
            let gizmo = (design && state.tool == Tool::Select).then_some(state.ui.gizmo);
            let sculpt_on = gizmo == Some(GizmoMode::Sculpt);
            let tools = ToolBarsKey {
                design,
                paint: (gizmo == Some(GizmoMode::Paint)).then(|| PaintBarKey {
                    mode: state.ui.paint_mode,
                    erasing: crate::tools::paint::erasing(state),
                    target: state.ui.paint_target,
                    scaled: state.ui.paint_pattern.kind.scaled(),
                }),
                arrange: (self.arranging && design).then(|| state.plate.arrange_gap.unwrap_or(8.0).to_bits()),
                fit: state.tool == Tool::MeasureFit,
                lay_flat: (gizmo == Some(GizmoMode::Shape)).then_some((state.ui.lay_flat, state.picks.filter)),
                sculpt: sculpt_on.then(|| crate::tools::sculpt::operation(state)),
                force: (gizmo == Some(GizmoMode::Force) && mode == RenderMode::Force).then_some(state.ui.force_kind),
            };
            if self.draw_keys.tools != Some(tools) {
                self.draw_keys.tools = Some(tools);
                v.widget(cx, ids!(model_ui.left.tools.t_paint)).set_visible(cx, design);
                v.widget(cx, ids!(model_ui.left.tools.t_sculpt)).set_visible(cx, design);
                v.widget(cx, ids!(model_ui.left.tools.t_force)).set_visible(cx, design);
                // The paint strip stays for as long as Paint is the tool, and goes when another is chosen.
                let pb = v.widget(cx, ids!(model_ui.context.paint_bar));
                pb.set_visible(cx, tools.paint.is_some());
                if let Some(PaintBarKey { mode, erasing, target, scaled }) = tools.paint {
                    for (id, on) in [
                        (ids!(p_brush), mode == PaintMode::Brush && !erasing),
                        (ids!(p_erase), erasing),
                        (ids!(p_feature), mode == PaintMode::Feature),
                        (ids!(p_part), mode == PaintMode::Part),
                        (ids!(p_smooth), mode == PaintMode::Flood),
                        (ids!(p_pattern), mode == PaintMode::Pattern),
                        (ids!(p_block), target == PaintTarget::SupportBlock),
                        (ids!(p_enforce), target == PaintTarget::SupportEnforce),
                    ] {
                        pb.radio_button(cx, id).set_active(cx, on, Animate::No);
                    }
                    pb.widget(cx, ids!(diameter)).set_visible(cx, mode == PaintMode::Brush);
                    pb.widget(cx, ids!(angle)).set_visible(cx, mode == PaintMode::Flood);
                    for (id, on) in [(ids!(pattern_kind), true), (ids!(pattern_scale), scaled), (ids!(pattern_seed), scaled), (ids!(pattern_sharp), true), (ids!(pattern_balance), true)] {
                        pb.widget(cx, id).set_visible(cx, mode == PaintMode::Pattern && on);
                    }
                }
                v.widget(cx, ids!(model_ui.context.fit_bar)).set_visible(cx, tools.fit);
                let arrange = v.widget(cx, ids!(model_ui.context.arrange_bar));
                arrange.set_visible(cx, tools.arrange.is_some());
                v.radio_button(cx, ids!(arrange_all)).set_active(cx, tools.arrange.is_some(), Animate::No);
                if let Some(gap) = tools.arrange {
                    let field = arrange.value_input(cx, ids!(arrange_gap));
                    if !field.borrow().is_some_and(|f| f.is_interacting()) {
                        field.set_value(cx, f64::from_bits(gap));
                    }
                }
                let sb = v.widget(cx, ids!(model_ui.context.shape_bar));
                sb.set_visible(cx, tools.lay_flat.is_some());
                if let Some((lay_flat, filter)) = tools.lay_flat {
                    for (id, kind) in PICK_KINDS {
                        sb.radio_button(cx, id).set_active(cx, filter == kind, Animate::No);
                    }
                    for id in [ids!(s_cut_box), ids!(s_cut_cyl), ids!(s_cut_sphere), ids!(s_add_box), ids!(s_add_cyl), ids!(s_add_sphere), ids!(s_cut_bottom), ids!(s_cut_plane), ids!(s_split), ids!(s_place)] {
                        sb.radio_button(cx, id).set_active(cx, false, Animate::No);
                    }
                    sb.radio_button(cx, ids!(s_lay_flat)).set_active(cx, lay_flat, Animate::No);
                }
                let sb = v.widget(cx, ids!(model_ui.context.sculpt_bar));
                sb.set_visible(cx, sculpt_on);
                if let Some(op) = tools.sculpt {
                    for (id, brush) in [(ids!(s_raise), StrokeOp::Raise), (ids!(s_lower), StrokeOp::Lower), (ids!(s_crease), StrokeOp::Crease)] {
                        sb.radio_button(cx, id).set_active(cx, op == brush, Animate::No);
                    }
                }
                let fb = v.widget(cx, ids!(model_ui.context.force_bar));
                fb.set_visible(cx, tools.force.is_some());
                if let Some(selected) = tools.force {
                    for (id, kind) in FORCE_KINDS {
                        fb.radio_button(cx, id).set_active(cx, selected == kind, Animate::No);
                    }
                    fb.radio_button(cx, ids!(f_remove)).set_active(cx, false, Animate::No);
                }
            }

            let see_on = state.view.see.on && !matches!(mode, RenderMode::Slice | RenderMode::Force);
            let see_row = see_on && looks;
            if self.draw_keys.see_row != Some(see_row) {
                self.draw_keys.see_row = Some(see_row);
                v.view(cx, ids!(render_row.see_row)).set_visible(cx, see_row);
            }
            if see_on {
                for (path, value) in [(ids!(render_row.see_row.see_opacity), state.view.see.opacity), (ids!(render_row.see_row.see_glow), state.view.see.glow)] {
                    let slider = v.slider(cx, path);
                    if !slider.has_text_focus(cx) && (slider.value().unwrap_or(-1.0) - value as f64).abs() > 1e-4 {
                        slider.set_value(cx, value as f64);
                    }
                }
            }
            let selection = (state.plate.revision, state.scene_revision, state.scene_state.revision);
            let has_part = match self.draw_keys.has_part {
                Some((key, has_part)) if key == selection => has_part,
                _ => {
                    let has_part = active_part(state).is_some();
                    self.draw_keys.has_part = Some((selection, has_part));
                    has_part
                }
            };
            let key = (state.tool, state.ui.gizmo, state.ui.show_zones, p.overhangs, p.play.travel, p.play.show_model, design, mode, !state.scene_state.selection.set.is_empty(), state.plate.parts.len() > 1, has_part);
            if self.synced != Some(key) {
                self.synced = Some(key);
                let tv = v.view(cx, ids!(model_ui.left.tools));
                tv.redraw(cx);
                tv.tip(cx, ids!(rotate_tip)).set_text(&if sculpt_on { keymap::entry(Key::Rotate).action.into() } else { keymap::tooltip(Key::Rotate) });
                tv.tip(cx, ids!(section_tip)).set_text(&if sculpt_on { keymap::entry(Key::Section).action.into() } else { keymap::tooltip(Key::Section) });
                tv.widget(cx, ids!(t_shape)).set_visible(cx, design);
                // The area tools draw on the shown slice: where its trees stand, on its layers.
                tv.widget(cx, ids!(no_trees_tip)).set_visible(cx, design && mode == RenderMode::Slice);
                // (a print settings volume draws in the Normal view and the Sliced one alike)
                tv.widget(cx, ids!(settings_here_tip)).set_visible(cx, design);
                // a box goes on the picked part: none picked, the tool waits for one
                tv.widget(cx, ids!(t_settings_here)).set_disabled(cx, !has_part);
                tv.tip(cx, ids!(settings_here_tip)).set_text(if has_part { SETTINGS_HERE_TIP } else { SELECT_A_PART });
                // the render mode in its flyout's button
                let shown = match mode { RenderMode::Slice => "Sliced", RenderMode::Force => "Force", _ => "Normal" };
                v.widget(cx, ids!(render_row.render_mode.label)).set_text(cx, shown);
                let select = state.tool == Tool::Select;
                for (id, on) in [
                    (ids!(t_select), select && state.ui.gizmo == GizmoMode::Off),
                    (ids!(t_move), select && state.ui.gizmo == GizmoMode::Move),
                    (ids!(t_rotate), select && state.ui.gizmo == GizmoMode::Rotate),
                    (ids!(t_scale), select && state.ui.gizmo == GizmoMode::Scale),
                    (ids!(t_shape), select && state.ui.gizmo == GizmoMode::Shape),
                    (ids!(t_paint), select && state.ui.gizmo == GizmoMode::Paint),
                    (ids!(t_sculpt), select && state.ui.gizmo == GizmoMode::Sculpt),
                    (ids!(t_force), state.force_tool()),
                    (ids!(t_clip), state.tool == Tool::Section),
                    (ids!(t_measure), matches!(state.tool, Tool::Measure(_) | Tool::MeasureFit)),
                    (ids!(t_no_trees), state.tool == Tool::NoTrees),
                    (ids!(t_settings_here), state.tool == Tool::SettingsHere),
                ] {
                    tv.radio_button(cx, id).set_active(cx, on, Animate::No);
                }
            }
            // the line over the view that says a step shows, with the way back
            let step = state.steps.shown.filter(|_| lead).map(|i| (i, state.steps.chips.len()));
            if self.step_line != step {
                self.step_line = step;
                let line = v.view(cx, ids!(model_ui.bottom.step_shown));
                line.set_visible(cx, step.is_some());
                if let Some((i, of)) = step {
                    line.label(cx, ids!(step_shown_text)).set_text(cx, &format!("Step {} of {of} · the part still prints all {of} · Esc: back to the end", i + 1));
                    let rollback = line.widget(cx, ids!(step_rollback));
                    rollback.set_visible(cx, i + 1 < of);
                    if i + 1 < of {
                        crate::ui::print_settings::name_control(cx, &rollback, &format!("Roll back to step {} of {of}", i + 1), "button");
                    }
                }
            }
            let checking = p.streamed.as_ref().and_then(|s| s.checking());
            let print_key = (p.revision, state.plate.revision, plate_mode, slice, checking, lead);
            if self.print_revision != Some(print_key) {
                self.print_revision = Some(print_key);
                let failed = v.view(cx, ids!(model_ui.bottom.slice_failed));
                failed.set_visible(cx, lead && matches!(p.status, SliceStatus::Failed(_)));
                if matches!(p.status, SliceStatus::Failed(_)) {
                    failed.label(cx, ids!(slice_failed_text)).set_text(cx, &slice_status(p).0);
                }

                let has_job_card = crate::print::job_card::job_view(p, Cx::time_now()).is_some();
                v.view(cx, ids!(monitor_job)).set_visible(cx, lead && state.ui.in_mode(PlateMode::Monitor) && (has_job_card || !p.follow.jobs.is_empty()));
                v.widget(cx, ids!(monitor_job.card)).set_visible(cx, has_job_card);
                v.widget(cx, ids!(monitor_job.jobs)).set_visible(cx, true);
                // Monitor: the printer's job, followed or said.
                let job = p.follow.shown_printing().filter(|_| state.ui.in_mode(PlateMode::Monitor));
                let unknown = job.filter(|j| j.preview.is_none());
                let mon = v.view(cx, ids!(monitor_ui));
                mon.set_visible(cx, lead && unknown.is_some());
                if let Some(j) = unknown {
                    let name = j.name.as_deref().unwrap_or("a job");
                    mon.label(cx, ids!(unknown.unknown_title)).set_text(cx, &format!("The printer is printing {name}"));
                    mon.label(cx, ids!(unknown.unknown_why)).set_text(cx, j.unmatched.as_deref().unwrap_or(""));
                }

                if slice {
                    let sv = v.view(cx, ids!(slice_ui));
                    let showing = p.showing().cloned();
                    let has = showing.is_some();
                    sv.view(cx, ids!(bottom.bar_row.legend_gap)).set_visible(cx, design);
                    // the pad walks a slice's layers, or a resin printer's (its stack alone: no
                    // transport, no moves)
                    let resin_stack = p.resin_shown().is_some();
                    sv.view(cx, ids!(bottom)).set_visible(cx, has || resin_stack);
                    sv.view(cx, ids!(bottom.bar_row.bar.row.transport)).set_visible(cx, !resin_stack);
                    let bar = sv.view(cx, ids!(bottom.bar_row.bar));
                    let job_view = p.monitor_preview();
                    bar.view(cx, ids!(follow_row)).set_visible(cx, job_view.is_some());
                    bar.view(cx, ids!(job_box)).set_visible(cx, job.is_some_and(|j| j.preview.is_some()));
                    if let Some(pv) = job_view {
                        let back = bar.button(cx, ids!(follow_row.back_live));
                        back.set_visible(cx, p.following().is_none());
                        back.set_text(cx, if job.is_some() { "Back to live" } else { "Whole job" });
                        bar.label(cx, ids!(follow_row.follow_text)).set_text(cx, &crate::print::follow::view_text(p, pv));
                    }
                    let row = sv.view(cx, ids!(bottom.bar_row.bar.row));
                    row.view(cx, ids!(speed)).label(cx, ids!(label)).set_text(cx, &speed_label(p.play.speed));
                    // The print time ahead of its playing preview: no transport until the preview comes.
                    let playable = !p.making_preview();
                    row.button(cx, ids!(print_play)).set_visible(cx, playable && !p.play.playing);
                    row.button(cx, ids!(print_pause)).set_visible(cx, playable && p.play.playing);
                    row.button(cx, ids!(to_start)).set_visible(cx, playable);
                    row.button(cx, ids!(to_end)).set_visible(cx, playable);
                    // The slice's state, as a chip over the view.
                    let (status, progress) = slice_status(p);
                    let running = matches!(p.status, SliceStatus::Running { .. });
                    let mut checking: Option<f32> = None;
                    let chip = if held {
                        // The plate's slice is not this view's business.
                        None
                    } else if let Some(r) = p.resin_shown() {
                        // a resin stack being cut, or why it can't be
                        (!r.note.is_empty()).then(|| r.note.clone())
                    } else if running {
                        Some(match (&p.status, progress) {
                            (SliceStatus::Running { stage, .. }, _) if stage.starts_with(crate::print::GROWING) => format!("{stage}…"),
                            (SliceStatus::Running { stage, .. }, Some(f)) if stage == crate::print::PLANNING_SUPPORTS => format!("{stage}… {:.0}%", f * 100.0),
                            (_, Some(f)) => format!("Slicing… {:.0}%", f * 100.0),
                            (_, None) => "Slicing…".to_string(),
                        })
                    } else if matches!(p.status, SliceStatus::Failed(_)) {
                        Some(status)
                    } else if state.plate.parts.is_empty() && p.preview.is_none() {
                        // Empty plates have neither model parts nor a slice.
                        Some("There is nothing on the plate yet.".to_string())
                    } else if p.preview.is_none() && state.plate.parts.iter().all(|part| part.is_board()) {
                        // A board alone: it is never printed, so no slice comes.
                        Some("Nothing to print yet: a board is not printed.".to_string())
                    } else if p.preview.is_none() || p.stale {
                        Some("Waiting to slice…".to_string())
                    } else if let Some((done, total)) = p.streamed.as_ref().and_then(|s| s.checking()) {
                        // The sliced printability streams in after the slice: a small note, the
                        // preview stays usable.
                        checking = Some(done as f32 / total.max(1) as f32);
                        Some(format!("Checking printability… {done}/{total} layers"))
                    } else {
                        // A part that won't fit the printer sliced anyway: the slice says so.
                        p.fit_warning.clone()
                    };
                    sv.view(cx, ids!(chip_layer)).set_visible(cx, chip.is_some());
                    if let Some(text) = chip {
                        let c = sv.view(cx, ids!(chip_layer.chip));
                        c.label(cx, ids!(chip_text)).set_text(cx, &text);
                        let bar = c.view(cx, ids!(chip_bar));
                        bar.set_visible(cx, running || checking.is_some());
                        if let Some(f) = progress.or(checking) {
                            if let Some(mut bar) = bar.borrow_mut() {
                                bar.draw_bg.set_dyn_instance(cx, live_id!(progress), &[f]);
                            }
                        }
                    }
                }
            }
            let playback = slice.then_some(PlaybackKey {
                revision: p.revision, held, resin, playing: p.play.playing,
                play_time: p.play.time, layer_hi: p.play.layer_hi,
                live: (held && p.follow.on).then_some(p.follow.live),
            });
            if self.draw_keys.playback != playback {
                self.draw_keys.playback = playback;
                if let Some(r) = playback.and_then(|_| p.resin_shown()) {
                    // a resin printer's stack: its layer and height (no time, nothing printing)
                    let top = p.play.layer_hi.min(r.count.saturating_sub(1));
                    let info = v.view(cx, ids!(slice_ui.bottom.bar_row.bar.info));
                    info.label(cx, ids!(layer_label)).set_text(cx, &format!("Layer {} / {}  ·  {:.2} mm", top + 1, r.count, r.top_of(top)));
                    info.label(cx, ids!(now)).set_text(cx, "");
                    self.layer_shown = None;
                    self.time_shown = None;
                } else if let Some(pv) = playback.and_then(|_| p.showing()) {
                    // Following a job: where the head is, also while the whole job shows.
                    let at = if p.following().is_some() { p.follow.live } else { p.play.time };
                    let time = (at.max(0.0).round() as u64, pv.total.max(0.0).round() as u64, p.making_preview());
                    if self.time_shown != Some(time) {
                        self.time_shown = Some(time);
                        let text = if time.2 { "Making the preview to play…".to_string() } else { format!("{} / {}", format_time(at), format_time(pv.total)) };
                        v.view(cx, ids!(slice_ui.bottom.bar_row.bar.row)).label(cx, ids!(time_label)).set_text(cx, &text);
                    }
                    let layer_inputs = (pv.id, p.play.time, p.play.layer_hi);
                    if self.draw_keys.layer != Some(layer_inputs) {
                        self.draw_keys.layer = Some(layer_inputs);
                        let top = shown_top_layer(p, pv);
                        let z = pv.layers.iter().find(|l| l.layer == top).map(|l| l.z).unwrap_or(0.0);
                        let max_layer = pv.max_layer();
                        let layer = (pv.id, top, max_layer, z);
                        if self.layer_shown != Some(layer) {
                            self.layer_shown = Some(layer);
                            v.view(cx, ids!(slice_ui.bottom.bar_row.bar.info)).label(cx, ids!(layer_label)).set_text(cx, &format!("Layer {} / {}  ·  {:.2} mm", top + 1, max_layer + 1, z));
                        }
                    }
                    let now_key = (p.revision, p.play.time);
                    if self.now_shown != Some(now_key) {
                        self.now_shown = Some(now_key);
                        let now = if p.play.time > 0.0 && p.play.time < pv.total {
                            let n = pv.timeline.now(p.play.time);
                            format!("{}  ·  filament {}  ·  {:.0} mm/s  ·  {}  ·  bed {} °C  ·  fan {} %", n.doing_text(), n.filament, n.speed, n.temps_text(), n.bed_temp, n.fan)
                        } else { String::new() };
                        v.view(cx, ids!(slice_ui.bottom.bar_row.bar.info)).label(cx, ids!(now)).set_text(cx, &now);
                    }
                }
            }
        }
        self.sync_pie(cx);
        while self.view.draw_walk(cx, scope, walk).is_step() {}
        DrawStep::done()
    }
}

