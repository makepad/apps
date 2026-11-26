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

/// What a playhead step ([`PrintAction::moves_playhead`]) changes: the slice in the view, the
/// layer slider, the transport with its time and layer readouts, a resin printer's layer and
/// the headset's view. Everything else in the stage and the panels stays as it was drawn.
pub const PLAYHEAD: &[&[LiveId]] = &[ids!(viewport), ids!(slice_ui.right_col), ids!(slice_ui.bottom.bar_row.bar), ids!(slice_ui.resin_ui), ids!(xr)];

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
        draw_bg +: {
            pixel: fn() {
                let sdf = Sdf2d.viewport(self.pos * self.rect_size)
                sdf.box(0.5, 0.5, self.rect_size.x - 1.0, self.rect_size.y - 1.0, fab.radius_lg)
                sdf.fill_keep(fab.color_float)
                sdf.stroke(fab.color_float_border, 1.0)
                return sdf.result
            }
        }
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
    // A render mode, styled as the panels' tabs: no face, its name half-lit at rest and lit
    // under the pointer, the chosen one in the full ink over an accent underline.
    let RenderSegment = mod.widgets.FabSegmentTab{
        height: 24
        padding: Inset{left: 10 right: 10 top: 0 bottom: 0}
        draw_bg +: {
            pixel: fn() {
                let sdf = Sdf2d.viewport(self.pos * self.rect_size)
                sdf.box(6.0, self.rect_size.y - 2.0, self.rect_size.x - 12.0, 2.0, 0.5)
                sdf.fill(vec4(fab.color_accent.xyz, fab.color_accent.w * self.active))
                return sdf.result
            }
        }
        draw_text +: {
            color: mix(fab.color_text_dim, fab.color_text, 0.5)
            color_hover: fab.color_text
            color_down: fab.color_text
            color_active: fab.color_text_active
            // (the one just clicked keeps the keyboard: it reads as chosen, not as rest)
            color_focus: fab.color_text_active
        }
    }
    // A toggle of the view bar, as tall as the render switch's segments.
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
                    // an action, not a tool: a button the size of the tools
                    FabTip{ text: "Arrange every part on the plate"
                        arrange_all := mod.widgets.FabIconButton{
                            width: 32 height: 32
                            align: Align{x: 0.5 y: 0.5}
                            padding: Inset{left: 8 right: 8 top: 8 bottom: 8}
                            icon_walk: Walk{width: 16 height: 16}
                            a11y: {label: "Arrange plate"}
                            draw_icon +: { svg: crate_resource("self://resources/icons/arrange.svg") }
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
                        padding: 3
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
                        padding: 3
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
                        padding: 3
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
                    // The Force tool's kinds: what a click on the studied part adds. Fab reads the
                    // click as a face or a hole's wall by itself; Alt takes the other reading.
                    force_bar := ToolRow{
                        visible: false
                        flow: Right
                        align: Align{y: 0.5}
                        padding: 3
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
            right_col := View{
                width: Fill
                height: Fill
                flow: Right
                clip_x: false
                clip_y: false
                Filler{}
                layer_col := View{
                    width: Fit
                    height: Fill
                    flow: Down
                    clip_x: false
                    clip_y: false
                    padding: Inset{right: 16 top: #(SLIDER_TOP) bottom: 100}
                    layers := mod.widgets.FabLayerSlider{}
                }
            }
            // a resin printer's slice: the shown layer flat, in place of the print's bar
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
                    padding: Inset{left: 56 bottom: 12}
                    resin_panel := mod.widgets.FabResinPanel{}
                    FabTip{ text: "Follow the printer again: the view shows the layer it cured last"
                        resin_live := mod.widgets.FabButton{ visible: false text: "Back to live" }
                    }
                }
            }
            bottom := View{
                width: Fill
                height: Fill
                flow: Down
                align: Align{x: 0.5}
                Filler{}
                bar_row := View{
                    width: Fill height: Fit flow: Right
                    // Clear of the tool strip that stays over Design's slice.
                    legend_gap := View{ width: 44 height: 1 }
                    bar := Float{
                        width: Fill
                        flow: Down
                        margin: Inset{left: 12 right: 80 bottom: 12}
                        padding: Inset{left: 10 right: 14 top: 8 bottom: 8}
                        spacing: 4
                        follow_row := View{
                            visible: false
                            width: Fill height: Fit flow: Right align: Align{y: 0.5} spacing: 10 padding: Inset{left: 4 top: 2}
                            FabTip{ text: "Follow the printer again: the view shows the layer it prints, the head where Fab's slice timing puts it"
                                back_live := mod.widgets.FabButton{ visible: false text: "Back to live" }
                            }
                            follow_text := mod.widgets.FabLabelSmall{ width: Fill text: "" }
                        }
                        job_box := View{
                            visible: false
                            width: Fill height: Fit padding: Inset{left: 4 right: 2 bottom: 4}
                            job_bar := mod.widgets.FabJobTimeline{}
                        }
                        // Two rows: the transport and its scrubber (to the start of the layer,
                        // play, to its end, the speed, then the scrubber taking the rest), then
                        // the time and what is printing there.
                        row := View{
                            width: Fill height: Fit flow: Down spacing: 4
                            transport := View{
                                width: Fill height: Fit flow: Right align: Align{y: 0.5} spacing: 4
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
                                time_scrub := mod.widgets.FabMoveSlider{ width: Fill margin: Inset{left: 6 right: 6} }
                            }
                            // what prints there (it wraps on a narrow view), then, apart by a rule at
                            // the right end, the time
                            meta := View{
                                width: Fill height: Fit flow: Right align: Align{y: 0.5} spacing: 12 padding: Inset{left: 4 right: 6}
                                info := View{
                                    width: Fill height: Fit flow: Right{wrap: true} align: Align{y: 0.5} spacing: 12
                                    layer_label := mod.widgets.FabLabel{ width: Fit text: "" }
                                    now := mod.widgets.FabLabelSmall{ width: Fit text: "" }
                                }
                                time_rule := mod.widgets.FabVRule{ height: 14 }
                                time_label := mod.widgets.FabLabelSmall{ width: Fit text: "" }
                            }
                        }
                    }
                }
            }
            // The slice's state over the view: never a modal, the view
            // stays live (an out-of-date slice shows faded under it).
            chip_layer := View{
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
            // layer draws it at the bar's left end), then the plate's mode with the render mode
            // beside it (as tabs), what makes and changes parts, and what the view shows, in
            // groups apart by rules.
            // A held job keeps only the mode (it has no other look); Mechanics has its own bar.
            modes_row := mod.widgets.FabEditorBar{
                // (a solid bar over the view, as every editor's toolbar)
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
                render_modes := View{
                    a11y: {role: "radio_group" label: "Render mode"}
                    width: Fit height: Fit flow: Right align: Align{y: 0.5} spacing: 1
                    normal_tip := FabTip{ text: #(format!("The parts, in the look View › Shading sets ({})", keymap::shortcut(Key::SliceView)))
                        rm_normal := RenderSegment{ text: "Normal" }
                    }
                    slice_tip := FabTip{ text: #(format!("The sliced print over the plate: edit and it reslices live ({})", keymap::shortcut(Key::SliceView)))
                        rm_slice := RenderSegment{ text: "Sliced" }
                    }
                    force_tip := FabTip{ text: "The selected part's stress as it will print, under the fixtures and loads you put on it: edit and it is studied again"
                        rm_force := RenderSegment{ text: "Force" }
                    }
                }
                create_rule := mod.widgets.FabVRule{}
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
                }
                view_rule := mod.widgets.FabVRule{}
                // What the view shows in this mode: the View menu's toggles, with its keys.
                view_bar := View{
                    a11y: {role: "toolbar" label: "View"}
                    width: Fit height: Fit flow: Right align: Align{y: 0.5} spacing: 1
                    // the view's tool rails, on or off (its area menu's Tools)
                    FabTip{ text: "Show the tool rails at the view's left"
                        show_tools := ViewToggle{ a11y: {label: "Tools"} draw_icon +: { svg: crate_resource("self://resources/icons/select.svg") } }
                    }
                    // the projection, in every mode (on: orthographic)
                    FabTip{ text: #(keymap::tooltip(Key::Ortho))
                        ortho := ViewToggle{ a11y: {label: "Orthographic"} draw_icon +: { svg: crate_resource("self://resources/icons/ortho.svg") } }
                    }
                    // the whole plate back in view, in every mode
                    FabTip{ text: #(keymap::tooltip(Key::FrameAll))
                        frame_all := ViewButton{ a11y: {label: "Frame plate"} draw_icon +: { svg: crate_resource("self://resources/icons/fit.svg") } }
                    }
                    // Sliced: what the slice shows
                    supports_tip := FabTip{ text: #(keymap::tooltip(Key::ShowSupports))
                        show_supports := ViewToggle{ a11y: {label: "Show supports"} draw_icon +: { svg: crate_resource("self://resources/icons/supports.svg") } }
                    }
                    travel_tip := FabTip{ text: "Show travel"
                        show_travel := ViewToggle{ a11y: {label: "Show travel"} draw_icon +: { svg: crate_resource("self://resources/icons/travel.svg") } }
                    }
                    parts_tip := FabTip{ text: "Show parts under the toolpaths"
                        show_parts := ViewToggle{ a11y: {label: "Show parts"} draw_icon +: { svg: crate_resource("self://resources/icons/cube.svg") } }
                    }
                    // a project with a board: the board in place, never printed
                    board_tip := FabTip{ text: "Show the circuit board in place: it is never printed or sliced"
                        show_board := ViewToggle{ a11y: {label: "Show board"} draw_icon +: { svg: crate_resource("self://resources/icons/board.svg") } }
                    }
                }
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
    view_bar: Option<ViewBarKey>,
    /// The board tip and toggle, including the scan for boards and their visibility.
    board_revision: Option<u64>,
    /// The three Design tool buttons and the paint, shape, sculpt and force strips and their controls.
    tools: Option<ToolBarsKey>,
    /// Visibility of the see-through row.
    see_row: Option<bool>,
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
struct ViewBarKey {
    sliced: bool,
    resin: bool,
    finish: bool,
    supports: bool,
    travel: bool,
    parts: bool,
    ortho: bool,
}

#[derive(Clone, Copy, PartialEq)]
struct ToolBarsKey {
    design: bool,
    paint: Option<PaintBarKey>,
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
    print_revision: Option<(u64, u64, Option<PlateMode>, bool, Option<(u32, u32)>, bool, bool)>,
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

    /// The two rail containers (and the chosen tool's row at the top) keep their visibility as
    /// the plate mode switches their parents; the toolbar's Tools toggle says which.
    pub fn set_tools_shown(&mut self, cx: &mut Cx, shown: bool) {
        self.view.view(cx, ids!(model_ui.left)).set_visible(cx, shown);
        self.view.view(cx, ids!(model_ui.context)).set_visible(cx, shown);
        self.view.view(cx, ids!(mechanics_ui.left)).set_visible(cx, shown);
        let toggle = self.view.radio_button(cx, ids!(render_row.view_bar.show_tools));
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
        // A clicked radio has already changed its own active state. Sync its group on
        // the next draw even if the action leaves the model as it was (e.g. Remove).
        let bar = v.view(cx, ids!(render_row.view_bar));
        if bar.radio_button(cx, ids!(ortho)).clicked(actions) {
            self.draw_keys.view_bar = None;
            cx.action(ShellAction::ToggleOrtho);
        }
        if bar.button(cx, ids!(frame_all)).clicked(actions) {
            cx.action(ShellAction::FrameAll);
        }
        if bar.radio_button(cx, ids!(show_supports)).clicked(actions) {
            self.draw_keys.view_bar = None;
            cx.action(PrintAction::ShowSupports(!state.print.play.supports));
        }
        if bar.radio_button(cx, ids!(show_travel)).clicked(actions) {
            self.draw_keys.view_bar = None;
            cx.action(PrintAction::ShowTravel(!state.print.play.travel));
        }
        if bar.radio_button(cx, ids!(show_parts)).clicked(actions) {
            self.draw_keys.view_bar = None;
            cx.action(PrintAction::ShowModel(!state.print.play.show_model));
        }
        if bar.radio_button(cx, ids!(show_board)).clicked(actions) {
            self.draw_keys.board_revision = None;
            if let Some(shown) = state.board_shown() {
                cx.action(ShellAction::ShowBoard(!shown));
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
        if tv.button(cx, ids!(arrange_all)).clicked(actions) {
            cx.action(PartAction::Arrange);
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
            cx.action(ShellAction::SetTool(if matches!(state.tool, Tool::Measure(_)) { Tool::Select } else { m }));
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
            let row = |mode: PlateMode, label: &str| MenuItem::new(LiveId(live_id!(fab_plate_mode).0 ^ (mode as u64 + 1)), label).radio(state.ui.in_mode(mode));
            let items = vec![row(PlateMode::Design, "Design"), row(PlateMode::Mechanics, "Mechanics"), row(PlateMode::Monitor, "Monitor")];
            open_menu(cx, live_id!(fab_plate_mode), items, anchor, MenuPlace::Below);
        }
        if let Some(pick) = menu_picked(actions, live_id!(fab_plate_mode)) {
            if let Some(mode) = [PlateMode::Design, PlateMode::Mechanics, PlateMode::Monitor].into_iter().find(|m| pick.0 == live_id!(fab_plate_mode).0 ^ (*m as u64 + 1)) {
                cx.action(ShellAction::SetPlateMode(mode));
            }
        }
        let modes = v.view(cx, ids!(render_row.render_modes));
        if modes.radio_button(cx, ids!(rm_normal)).clicked(actions) {
            cx.action(ShellAction::SetRenderMode(state.normal_mode()));
        }
        if modes.radio_button(cx, ids!(rm_slice)).clicked(actions) {
            cx.action(ShellAction::SetRenderMode(RenderMode::Slice));
        }
        if modes.radio_button(cx, ids!(rm_force)).clicked(actions) {
            cx.action(ShellAction::SetRenderMode(RenderMode::Force));
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
                let name = match plate_mode {
                    Some(PlateMode::Mechanics) => "Mechanics",
                    Some(PlateMode::Monitor) => "Monitor",
                    _ => "Design",
                };
                v.widget(cx, ids!(render_row.plate_mode.label)).set_text(cx, name);
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
            let context = !mechanics && design && state.tool == Tool::Select && row_tool && v.view(cx, ids!(model_ui.context)).visible();
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
                v.view(cx, ids!(render_row.render_modes)).set_visible(cx, looks);
                v.view(cx, ids!(render_row.create_card)).set_visible(cx, looks);
                v.view(cx, ids!(render_row.view_bar)).set_visible(cx, looks);
                // a group's rule goes with it (a held job's bar holds only the mode)
                for rule in [ids!(render_row.create_rule), ids!(render_row.view_rule)] {
                    v.widget(cx, rule).set_visible(cx, looks);
                }
                v.view(cx, ids!(slice_ui)).set_visible(cx, slice && lead);
                v.view(cx, ids!(slice_ui.resin_ui)).set_visible(cx, resin);
                // Monitor stopped following the resin job when the layers moved by hand
                v.button(cx, ids!(slice_ui.resin_ui.resin_row.resin_live)).set_visible(cx, resin_live);
                v.widget(cx, ids!(progress_card)).set_visible(cx, lead);
            }
            // The view bar: its toggles by the mode alone, each as the view has it.
            let view_bar = ViewBarKey {
                sliced: mode == RenderMode::Slice, resin, finish: p.finish,
                supports: p.play.supports, travel: p.play.travel, parts: p.play.show_model,
                ortho: state.view.camera(self.pane).ortho,
            };
            if self.draw_keys.view_bar != Some(view_bar) {
                self.draw_keys.view_bar = Some(view_bar);
                let bar = v.view(cx, ids!(render_row.view_bar));
                // The printed surface draws neither travel nor the parts under it; a resin printer's
                // layers are no toolpaths.
                for (tip, toggle, on, works) in [
                    (ids!(supports_tip), ids!(show_supports), view_bar.supports, !resin),
                    (ids!(travel_tip), ids!(show_travel), view_bar.travel, !resin && !view_bar.finish),
                    (ids!(parts_tip), ids!(show_parts), view_bar.parts, !resin && !view_bar.finish),
                ] {
                    bar.widget(cx, tip).set_visible(cx, view_bar.sliced);
                    let toggle = bar.radio_button(cx, toggle);
                    if toggle.active(cx) != on {
                        toggle.set_active(cx, on, Animate::No);
                    }
                    if toggle.disabled(cx) == works {
                        toggle.set_disabled(cx, !works);
                    }
                }
                let ortho = bar.radio_button(cx, ids!(ortho));
                if ortho.active(cx) != view_bar.ortho {
                    ortho.set_active(cx, view_bar.ortho, Animate::No);
                }
            }
            // the board's toggle, in every mode of a plate with a board
            if self.draw_keys.board_revision != Some(state.plate.revision) {
                self.draw_keys.board_revision = Some(state.plate.revision);
                let bar = v.view(cx, ids!(render_row.view_bar));
                let board = state.board_shown();
                bar.widget(cx, ids!(board_tip)).set_visible(cx, board.is_some());
                let show_board = bar.radio_button(cx, ids!(show_board));
                if show_board.active(cx) != (board == Some(true)) {
                    show_board.set_active(cx, board == Some(true), Animate::No);
                }
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
                let modes = v.view(cx, ids!(render_row.render_modes));
                let slice_key = if sculpt_on { String::new() } else { format!(" ({})", keymap::shortcut(Key::SliceView)) };
                modes.tip(cx, ids!(normal_tip)).set_text(&format!("The parts, in the look View › Shading sets{slice_key}"));
                modes.tip(cx, ids!(slice_tip)).set_text(&format!("The sliced print over the plate: edit and it reslices live{slice_key}"));
                tv.widget(cx, ids!(t_shape)).set_visible(cx, design);
                // The area tools draw on the shown slice: where its trees stand, on its layers.
                tv.widget(cx, ids!(no_trees_tip)).set_visible(cx, design && mode == RenderMode::Slice);
                // (a print settings volume draws in the Normal view and the Sliced one alike)
                tv.widget(cx, ids!(settings_here_tip)).set_visible(cx, design);
                // a box goes on the picked part: none picked, the tool waits for one
                tv.widget(cx, ids!(t_settings_here)).set_disabled(cx, !has_part);
                tv.tip(cx, ids!(settings_here_tip)).set_text(if has_part { SETTINGS_HERE_TIP } else { SELECT_A_PART });
                modes.radio_button(cx, ids!(rm_normal)).set_active(cx, !matches!(mode, RenderMode::Slice | RenderMode::Force), Animate::No);
                modes.radio_button(cx, ids!(rm_slice)).set_active(cx, mode == RenderMode::Slice, Animate::No);
                modes.radio_button(cx, ids!(rm_force)).set_active(cx, mode == RenderMode::Force, Animate::No);
                // Force studies a part of Design's plate
                modes.widget(cx, ids!(force_tip)).set_visible(cx, design);
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
                    (ids!(t_measure), matches!(state.tool, Tool::Measure(_))),
                    (ids!(t_no_trees), state.tool == Tool::NoTrees),
                    (ids!(t_settings_here), state.tool == Tool::SettingsHere),
                ] {
                    tv.radio_button(cx, id).set_active(cx, on, Animate::No);
                }
            }
            let checking = p.streamed.as_ref().and_then(|s| s.checking());
            let jobs_area = state.ui.on_screen(Editor::Jobs);
            let print_key = (p.revision, state.plate.revision, plate_mode, slice, checking, jobs_area, lead);
            if self.print_revision != Some(print_key) {
                self.print_revision = Some(print_key);
                let failed = v.view(cx, ids!(model_ui.bottom.slice_failed));
                failed.set_visible(cx, lead && matches!(p.status, SliceStatus::Failed(_)));
                if matches!(p.status, SliceStatus::Failed(_)) {
                    failed.label(cx, ids!(slice_failed_text)).set_text(cx, &slice_status(p).0);
                }

                let has_job_card = crate::print::job_card::job_view(p, Cx::time_now()).is_some();
                v.view(cx, ids!(monitor_job)).set_visible(cx, lead && state.ui.in_mode(PlateMode::Monitor) && (has_job_card || (!p.follow.jobs.is_empty() && !jobs_area)));
                v.widget(cx, ids!(monitor_job.card)).set_visible(cx, has_job_card);
                v.widget(cx, ids!(monitor_job.jobs)).set_visible(cx, !jobs_area);
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
                    // the layer slider walks a slice's layers, or a resin printer's
                    sv.view(cx, ids!(right_col)).set_visible(cx, has || p.resin_shown().is_some());
                    sv.view(cx, ids!(bottom)).set_visible(cx, has);
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
                if let Some(pv) = playback.and_then(|_| p.showing()) {
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
        while self.view.draw_walk(cx, scope, walk).is_step() {}
        DrawStep::done()
    }
}

