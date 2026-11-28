//! Props list templates and row bindings from the in-app tweaker's sidebar.
use super::{inset_leg_rank, level_color, EditorEvent, PropertySets, Props, RowBinding, RowKind, SectionKind, StructKind, VisKind};
use makepad_widgets::{fab_controls::{FabColorPick, FabColorPickAction, FabValueInput, FabValueInputAction}, *};
use super::super::{theme::ThemePalette, RequestId};
use super::live::{LiveSets, Phase};
use super::layout::{LayoutEvent, LayoutKind, LayoutValues, view::LayoutControls};
use std::{collections::{BTreeMap, HashMap}, rc::Rc};

pub(in crate::tweak) fn sidebar_value(vm: &mut ScriptVm, row: ScriptValue) -> ScriptValue {
    script_eval!(vm, {
        use mod.prelude.workspace.*
        use mod.widgets.*
        let fab = mod.fab
        let TweakRow = #(row)
        let PanelFont = theme.font_regular{
            font_family: theme.font_regular.font_family{
                latin := mod.text.FontMember{
                    res: crate_resource("makepad_widgets:resources/IBMPlexSans-Text.ttf")
                    asc: -0.1 desc: 0.0
                }
            }
            line_spacing: 1.2
        }
        let PanelLabelDim = Label {
            width: Fit height: Fit padding: 0
            draw_text +: { ink_centered: true color: #xb4b4b4 text_style: PanelFont{font_size: 8.5} }
        }
        let PanelLabelSmall = PanelLabelDim { draw_text.text_style.font_size: 7.5 }
        let FabHeaderLabel = PanelLabelDim { draw_text +: {color: #xd0d0d0 text_style +: {font_size: 9.0}} }

        let FabSection = View{
            width: Fill
            height: 22
            flow: Right
            align: Align{x: 0.0 y: 0.5}
            padding: Inset{left: 6 right: 6 top: 0 bottom: 0}
            spacing: 4
            cursor: MouseCursor.Hand
            show_bg: true
            draw_bg +: {
                hover: instance(0.0)
                pixel: fn() {
                    let sdf = Sdf2d.viewport(self.pos * self.rect_size)
                    sdf.box(0.5, 0.5, self.rect_size.x - 1.0, self.rect_size.y - 1.0, 2.0)
                    sdf.fill(#x3d3d3d.mix(#x656565, self.hover * 0.5))
                    return sdf.result
                }
            }
            title := FabHeaderLabel{ text: "Section" }
        }

        let PanelButton = Button {
            min_height: 0
            padding: Inset{left: 6 right: 6 top: 3 bottom: 3}
            margin: Inset{left: 0 right: 0 top: 3 bottom: 3}
            spacing: 6
            draw_bg +: {
                border_size: 1.0
                border_radius: 2.0
                color_dither: 0.0
                color: fab.color_button
                color_hover: fab.color_button_hover
                color_down: fab.color_button_down
                color_focus: fab.color_button
                color_disabled: #x1d1d1d
                // Flat, in every state. The stock Button's face and
                // bevel are gradients, and the shader decides on the
                // REST stop alone whether to run them: with that one
                // left on, the other states' -1 stops are mixed in
                // as colours, and the off state grows a bright rim.
                color_2: vec4(-1.0, -1.0, -1.0, -1.0)
                color_2_hover: vec4(-1.0, -1.0, -1.0, -1.0)
                color_2_down: vec4(-1.0, -1.0, -1.0, -1.0)
                color_2_focus: vec4(-1.0, -1.0, -1.0, -1.0)
                color_2_disabled: vec4(-1.0, -1.0, -1.0, -1.0)
                border_color: fab.color_border
                border_color_hover: fab.color_border_light
                border_color_down: fab.color_border
                border_color_focus: fab.color_focus_ring
                border_color_disabled: fab.color_border
                border_color_2: vec4(-1.0, -1.0, -1.0, -1.0)
                border_color_2_hover: vec4(-1.0, -1.0, -1.0, -1.0)
                border_color_2_down: vec4(-1.0, -1.0, -1.0, -1.0)
                border_color_2_focus: vec4(-1.0, -1.0, -1.0, -1.0)
                border_color_2_disabled: vec4(-1.0, -1.0, -1.0, -1.0)
                // The panel's own face, so a sheet that replaces the
                // stock one replaces something this button does not
                // use. Flat fill, one hairline, the same state mix
                // order the stock face has. The wait spinner is left
                // out on purpose: no button on this panel is ever put
                // in the loading state, and the arc is the one part
                // of the stock face that is not just a box.
                pixel: fn() {
                    let sdf = Sdf2d.viewport(self.pos * self.rect_size)
                    sdf.box(
                        self.border_size
                        self.border_size
                        self.rect_size.x - self.border_size * 2.
                        self.rect_size.y - self.border_size * 2.
                        self.border_radius
                    )
                    sdf.fill_keep(
                        self.color
                            .mix(self.color_focus, self.focus)
                            .mix(self.color_hover, self.hover)
                            .mix(self.color_down, self.down)
                            .mix(self.color_disabled, self.disabled)
                    )
                    sdf.stroke(
                        self.border_color
                            .mix(self.border_color_focus, self.focus)
                            .mix(self.border_color_hover, self.hover)
                            .mix(self.border_color_down, self.down)
                            .mix(self.border_color_disabled, self.disabled)
                        self.border_size
                    )
                    return sdf.result
                }
            }
            draw_text +: {
                color: fab.color_text
                color_hover: fab.color_text_active
                color_down: fab.color_text_active
                color_focus: fab.color_text
                color_disabled: #x8c8c8c
            }
        }

        let PanelInput = TextInput {
            min_height: 0
            padding: Inset{left: 6 right: 6 top: 3 bottom: 3}
            margin: Inset{left: 0 right: 0 top: 3 bottom: 3}
            draw_bg +: {
                border_size: 1.0
                border_radius: 2.0
                color_dither: 0.0
                color: #x1d1d1d
                color_hover: #x232323
                color_focus: #x161616
                color_down: #x161616
                color_empty: #x1d1d1d
                color_disabled: #x1d1d1d
                border_color: #x161616
                border_color_hover: #x4a4a4a
                border_color_focus: #x7aa2e8
                border_color_down: #x161616
                border_color_empty: #x161616
                border_color_disabled: #x161616
                // The panel's own field face. The `empty` state is
                // mixed first, exactly as the stock face mixes it, or
                // the placeholder row would lose its ground.
                pixel: fn() {
                    let sdf = Sdf2d.viewport(self.pos * self.rect_size)
                    sdf.box(
                        self.border_size
                        self.border_size
                        self.rect_size.x - self.border_size * 2.
                        self.rect_size.y - self.border_size * 2.
                        self.border_radius
                    )
                    sdf.fill_keep(
                        self.color
                            .mix(self.color_empty, self.empty)
                            .mix(self.color_focus, self.focus)
                            .mix(self.color_hover.mix(self.color_down, self.down), self.hover)
                            .mix(self.color_disabled, self.disabled)
                    )
                    sdf.stroke(
                        self.border_color
                            .mix(self.border_color_empty, self.empty)
                            .mix(self.border_color_focus, self.focus)
                            .mix(self.border_color_hover.mix(self.border_color_down, self.down), self.hover)
                            .mix(self.border_color_disabled, self.disabled)
                        self.border_size
                    )
                    return sdf.result
                }
            }
            draw_text +: {
                color: #xe6e6e6
                color_hover: #xffffff
                color_focus: #xffffff
                color_down: #xffffff
                color_disabled: #x8c8c8c
                color_empty: #x8c8c8c
                color_empty_hover: #xb4b4b4
                color_empty_focus: #xb4b4b4
            }
            draw_selection +: {
                color: #x334d80
                color_hover: #x334d80
                color_focus: #x334d80
                color_down: #x334d80
                color_empty: #x334d80
                color_disabled: #x334d80
            }
            draw_cursor +: {
                color: #xffffff
            }
        }

        let PanelCheckBox = CheckBox {
            draw_bg +: {
                // Flat, whatever material the app's theme raises.
                material: 0.0
                color: fab.color_input
                color_hover: fab.color_input_hover
                color_down: fab.color_input_active
                color_active: fab.color_input
                color_focus: fab.color_input
                color_disabled: fab.color_input
                border_color: fab.color_border
                border_color_hover: fab.color_border_light
                border_color_down: fab.color_border
                border_color_active: fab.color_border
                border_color_focus: fab.color_focus_ring
                border_color_disabled: fab.color_border
                mark_color_active: fab.color_text_active
                mark_color_active_hover: fab.color_text_active
                mark_color_mixed: fab.color_text_active
                mark_color_disabled: #x8c8c8c
            }
            draw_text +: {
                color: fab.color_text
                color_hover: fab.color_text_active
                color_down: fab.color_text_active
                color_focus: fab.color_text
                color_active: fab.color_text
                color_disabled: #x8c8c8c
            }
        }

        let SectionRowT = FabSection {
            count := PanelLabelSmall { width: Fit margin: Inset{left: 4 top: 1 right: 0 bottom: 0} text: "" }
        }
        let InfoRowT = FabPropRow {
            value := PanelLabelSmall {width: Fill}
            loc := PanelLabelSmall {width: Fit}
        }
        let CascadeRowT = View {
            width: Fill
            height: Fit
            flow: Down
            spacing: 2
            padding: Inset{left: 8 right: 8 top: 4 bottom: 4}
            head := View {
                width: Fill
                height: Fit
                flow: Right
                spacing: 6
                align: Align{x: 0.0 y: 0.5}
                ic_app := View { width: Fit height: Fit visible: false
                    i := Icon { icon_walk: Walk{width: 12 height: Fit} draw_icon +: { color: #xbbbbbb svg: crate_resource("makepad_widgets:resources/icons/icon_file.svg") } }
                }
                ic_lib := View { width: Fit height: Fit visible: false
                    i := Icon { icon_walk: Walk{width: 12 height: Fit} draw_icon +: { color: #xbbbbbb svg: crate_resource("makepad_widgets:resources/icons/icon_widget.svg") } }
                }
                ic_theme := View { width: Fit height: Fit visible: false
                    i := Icon { icon_walk: Walk{width: 12 height: Fit} draw_icon +: { color: #xbbbbbb svg: crate_resource("makepad_widgets:resources/icons/icon_draw.svg") } }
                }
                ic_native := View { width: Fit height: Fit visible: false
                    i := Icon { icon_walk: Walk{width: 12 height: Fit} draw_icon +: { color: #xbbbbbb svg: crate_resource("makepad_widgets:resources/icons/icon_layout.svg") } }
                }
                chip := RoundedView {
                    width: Fit height: Fit
                    padding: Inset{left: 5 right: 5 top: 1 bottom: 1}
                    draw_bg +: { color: #x555555 border_radius: 3. }
                    lbl := PanelLabelSmall { width: Fit text: "L0" draw_text +: { color: #x151515 } }
                }
                loc := PanelLabelDim { width: Fill text: "" max_lines: 1 text_overflow: TextOverflow.Ellipsis }
            }
        }

        let NumRowT = View {
            width: Fill
            height: 24
            flow: Right
            align: Align{x: 0.0 y: 0.5}
            // Tight on the right: every point between the swatch and
            // the panel edge is a point the NAME does not get, and
            // the name is the thing that was being truncated.
            padding: Inset{left: 8 right: 2 top: 0 bottom: 0}
            spacing: 4
            name := PanelLabelDim {
                width: Fill
                text: ""
                max_lines: 1
                text_overflow: TextOverflow.Ellipsis
            }
            // Sized to the value column, not to the value: the
            // widest thing that has to fit anywhere in it is a
            // `#00000000`, and every point past that is a point
            // stolen from the name, which is what gets truncated.
            value := FabValueInput {
                width: 106
                height: 18
            }
            origin := PanelLabelSmall { width: 8 margin: Inset{left: 0 top: 2 right: 0 bottom: 0} text: "" }
        }
        let BoolRowT = View {
            width: Fill
            height: 24
            flow: Right
            align: Align{x: 0.0 y: 0.5}
            // Tight on the right: every point between the swatch and
            // the panel edge is a point the NAME does not get, and
            // the name is the thing that was being truncated.
            padding: Inset{left: 8 right: 2 top: 0 bottom: 0}
            spacing: 4
            name := PanelLabelDim {
                width: Fill
                text: ""
                max_lines: 1
                text_overflow: TextOverflow.Ellipsis
            }
            value := PanelCheckBox {
                width: Fit
                height: Fit
                text: ""
            }
            origin := PanelLabelSmall { width: 8 margin: Inset{left: 0 top: 2 right: 0 bottom: 0} text: "" }
        }

        let TextRowT = View {
            width: Fill
            height: 24
            flow: Right
            align: Align{x: 0.0 y: 0.5}
            // Tight on the right: every point between the swatch and
            // the panel edge is a point the NAME does not get, and
            // the name is the thing that was being truncated.
            padding: Inset{left: 8 right: 2 top: 0 bottom: 0}
            spacing: 4
            name := PanelLabelDim {
                width: Fill
                text: ""
                max_lines: 1
                text_overflow: TextOverflow.Ellipsis
            }
            value := PanelInput {
                width: 106
                height: 18
                empty_text: ""
                submit_on_enter: true
                draw_bg +: {
                    border_radius: 2.0
                }
                draw_text +: {
                    ink_centered: true
                    text_style +: {
                        font_size: 8.5
                    }
                }
            }
            origin := PanelLabelSmall { width: 8 margin: Inset{left: 0 top: 2 right: 0 bottom: 0} text: "" }
        }

        let ColorRowT = View {
            width: Fill
            height: 24
            flow: Right
            align: Align{x: 0.0 y: 0.5}
            // Tight on the right: every point between the swatch and
            // the panel edge is a point the NAME does not get, and
            // the name is the thing that was being truncated.
            padding: Inset{left: 8 right: 2 top: 0 bottom: 0}
            spacing: 4
            name := PanelLabelDim {
                width: Fill
                text: ""
                max_lines: 1
                text_overflow: TextOverflow.Ellipsis
            }
            // 80 + spacing + the swatch is exactly the 106 the number
            // rows use, so both columns end on the same edge — and 80
            // is what `#00000000` actually needs.
            value := PanelInput {
                width: 80
                height: 18
                empty_text: "#rrggbbaa"
                submit_on_enter: true
                draw_bg +: {
                    border_radius: 2.0
                }
                draw_text +: {
                    ink_centered: true
                    text_style +: {
                        font_size: 8.5
                    }
                }
            }
            swatch := FabColorPick {
                popover +: { hex_row +: { pick +: { visible: false } } }
                width: 22
                height: 16
            }
            origin := PanelLabelSmall { width: 8 margin: Inset{left: 0 top: 2 right: 0 bottom: 0} text: "" }
        }

        let VecRowT = View {
            width: Fill height: 24 flow: Right align: Align{x: 0.0 y: 0.5}
            padding: Inset{left: 8 right: 6 top: 0 bottom: 0} spacing: 4
            name := PanelLabelDim { width: Fill text: "" max_lines: 1 text_overflow: TextOverflow.Ellipsis }
            vx := FabValueInput { width: 46 height: 18 }
            vy := FabValueInput { width: 46 height: 18 }
            vz_wrap := View { width: Fit height: Fit visible: false
                vz := FabValueInput { width: 46 height: 18 }
            }
            vw_wrap := View { width: Fit height: Fit visible: false
                vw := FabValueInput { width: 46 height: 18 }
            }
        }
        let InsetRowT = View {
            width: Fill height: 24 flow: Right align: Align{x: 0.0 y: 0.5}
            padding: Inset{left: 8 right: 6 top: 0 bottom: 0} spacing: 3
            name := PanelLabelDim { width: Fill text: "" max_lines: 1 text_overflow: TextOverflow.Ellipsis }
            il := FabValueInput { width: 40 height: 18 }
            it := FabValueInput { width: 40 height: 18 }
            ir := FabValueInput { width: 40 height: 18 }
            ib := FabValueInput { width: 40 height: 18 }
        }
        let MetricsRowT = View {
            width: Fill height: 24 flow: Right align: Align{x: 0.0 y: 0.5}
            padding: Inset{left: 8 right: 6 top: 0 bottom: 0} spacing: 4
            name := PanelLabelDim { width: Fill text: "" max_lines: 1 text_overflow: TextOverflow.Ellipsis }
            m0 := FabValueInput { width: 44 height: 18 }
            m1 := FabValueInput { width: 44 height: 18 }
            m2 := FabValueInput { width: 44 height: 18 }
        }
        let NoEditorRowT = View {
            width: Fill height: 24 flow: Right align: Align{x: 0.0 y: 0.5}
            padding: Inset{left: 8 right: 6 top: 0 bottom: 0} spacing: 6
            name := PanelLabelDim { width: Fill text: "" max_lines: 1 text_overflow: TextOverflow.Ellipsis }
            ne := PanelLabelSmall { width: Fit text: "no editor yet" }
        }

        let MeasuredRowT = View {
            width: Fill
            height: Fit
            flow: Right
            padding: Inset{left: 8 right: 8 top: 1 bottom: 3}
            measured := PanelLabelSmall {
                width: Fill
                text: ""
                flow: Right{wrap: true}
            }
        }

        // One size field, in the person's own words: a number, a
        // percentage, an expression.
        let SizeInputT = PanelInput {
            submit_on_enter: true
            height: 18
            empty_text: ""
            label_align: Align{x: 0.5 y: 0.5}
            draw_bg +: {
                border_radius: 2.0
            }
            draw_text +: {
                ink_centered: true
                text_style +: { font_size: 8.5 }
            }
        }
        let SizeRowT = FabPropRow {
            height: Fit
            size_col := View {
                width: Fill
                height: Fit
                flow: Down
                spacing: 2
                w_row := View {
                    width: Fill
                    height: Fit
                    flow: Right
                    spacing: 4
                    align: Align{x: 0.0 y: 0.5}
                    w_axis := PanelLabelSmall { width: 12 text: "W" }
                    w_seg := View { width: Fit height: Fit flow: Right spacing: 1
                        w_fill := PanelButton { width: Fit height: Fit padding: Inset{left: 4 right: 4 top: 1 bottom: 1} margin: Inset{left:0 right:0 top:0 bottom:0} text: "" draw_text +: { text_style +: { font_size: 7.0 } } }
                        w_fit := PanelButton { width: Fit height: Fit padding: Inset{left: 4 right: 4 top: 1 bottom: 1} margin: Inset{left:0 right:0 top:0 bottom:0} text: "" draw_text +: { text_style +: { font_size: 7.0 } } }
                        w_fix := PanelButton { width: Fit height: Fit padding: Inset{left: 4 right: 4 top: 1 bottom: 1} margin: Inset{left:0 right:0 top:0 bottom:0} text: "" draw_text +: { text_style +: { font_size: 7.0 } } }
                    }
                    w_input := SizeInputT { width: Fill }
                }
                // The content-box clamps, under the axis they bound.
                w_clamp := View {
                    width: Fill
                    height: Fit
                    flow: Right{wrap: true}
                    spacing: 4
                    align: Align{x: 0.0 y: 0.5}
                    w_min_label := PanelLabelSmall { width: Fit text: "min" }
                    w_min := SizeInputT { width: 42 empty_text: "\u{2013}" }
                    w_max_label := PanelLabelSmall { width: Fit margin: Inset{left: 4 top: 0 right: 0 bottom: 0} text: "max" }
                    w_max := SizeInputT { width: 42 empty_text: "\u{2013}" }
                }
                // A Fill's own fields; the lines are not there otherwise.
                w_grow := View {
                    width: Fill
                    height: Fit
                    flow: Right{wrap: true}
                    spacing: 4
                    align: Align{x: 0.0 y: 0.5}
                    w_grow_label := PanelLabelSmall { width: Fit text: "grow" }
                    w_weight := FabValueInput { width: 42 height: 18 precision: 1 padding: Inset{left: 4 right: 4 top: 0 bottom: 0} }
                    w_shrink_label := PanelLabelSmall { width: Fit margin: Inset{left: 4 top: 0 right: 0 bottom: 0} text: "shrink" }
                    w_shrink := FabValueInput { width: 42 height: 18 precision: 1 padding: Inset{left: 4 right: 4 top: 0 bottom: 0} }
                }
                w_basis := View {
                    width: Fill
                    height: Fit
                    flow: Right
                    spacing: 4
                    align: Align{x: 0.0 y: 0.5}
                    w_basis_label := PanelLabelSmall { width: Fit text: "basis" }
                    w_basis_in := SizeInputT { width: Fill }
                }
                h_row := View {
                    width: Fill
                    height: Fit
                    flow: Right
                    spacing: 4
                    align: Align{x: 0.0 y: 0.5}
                    h_axis := PanelLabelSmall { width: 12 text: "H" }
                    h_seg := View { width: Fit height: Fit flow: Right spacing: 1
                        h_fill := PanelButton { width: Fit height: Fit padding: Inset{left: 4 right: 4 top: 1 bottom: 1} margin: Inset{left:0 right:0 top:0 bottom:0} text: "" draw_text +: { text_style +: { font_size: 7.0 } } }
                        h_fit := PanelButton { width: Fit height: Fit padding: Inset{left: 4 right: 4 top: 1 bottom: 1} margin: Inset{left:0 right:0 top:0 bottom:0} text: "" draw_text +: { text_style +: { font_size: 7.0 } } }
                        h_fix := PanelButton { width: Fit height: Fit padding: Inset{left: 4 right: 4 top: 1 bottom: 1} margin: Inset{left:0 right:0 top:0 bottom:0} text: "" draw_text +: { text_style +: { font_size: 7.0 } } }
                    }
                    h_input := SizeInputT { width: Fill }
                }
                // The content-box clamps, under the axis they bound.
                h_clamp := View {
                    width: Fill
                    height: Fit
                    flow: Right{wrap: true}
                    spacing: 4
                    align: Align{x: 0.0 y: 0.5}
                    h_min_label := PanelLabelSmall { width: Fit text: "min" }
                    h_min := SizeInputT { width: 42 empty_text: "\u{2013}" }
                    h_max_label := PanelLabelSmall { width: Fit margin: Inset{left: 4 top: 0 right: 0 bottom: 0} text: "max" }
                    h_max := SizeInputT { width: 42 empty_text: "\u{2013}" }
                }
                // A Fill's own fields; the lines are not there otherwise.
                h_grow := View {
                    width: Fill
                    height: Fit
                    flow: Right{wrap: true}
                    spacing: 4
                    align: Align{x: 0.0 y: 0.5}
                    h_grow_label := PanelLabelSmall { width: Fit text: "grow" }
                    h_weight := FabValueInput { width: 42 height: 18 precision: 1 padding: Inset{left: 4 right: 4 top: 0 bottom: 0} }
                    h_shrink_label := PanelLabelSmall { width: Fit margin: Inset{left: 4 top: 0 right: 0 bottom: 0} text: "shrink" }
                    h_shrink := FabValueInput { width: 42 height: 18 precision: 1 padding: Inset{left: 4 right: 4 top: 0 bottom: 0} }
                }
                h_basis := View {
                    width: Fill
                    height: Fit
                    flow: Right
                    spacing: 4
                    align: Align{x: 0.0 y: 0.5}
                    h_basis_label := PanelLabelSmall { width: Fit text: "basis" }
                    h_basis_in := SizeInputT { width: Fill }
                }
                aspect_row := View {
                    width: Fill
                    height: Fit
                    flow: Right{wrap: true}
                    spacing: 4
                    align: Align{x: 0.0 y: 0.5}
                    aspect_label := PanelLabelSmall { width: Fit text: "aspect" }
                    aspect_in := SizeInputT { width: 42 empty_text: "\u{2013}" }
                    aspect_hint := PanelLabelSmall { width: Fit text: "width : height" }
                }
            }
        }
        let IdentityRowT = View {
            width: Fill
            height: Fit
            flow: Right
            spacing: 6
            align: Align{x: 0.0 y: 0.5}
            padding: Inset{left: 8 right: 8 top: 3 bottom: 5}
            name_field := PanelInput {
                is_read_only: true submit_on_enter: true
                // 70 / 30 against the type beside it: a name is
                // usually short, and a type that gets ellipsised is
                // no use at all.
                width: Fill{weight: 70.0}
                height: 20
                empty_text: "unnamed"
                draw_bg +: {
                    border_radius: 2.0
                }
                draw_text +: {
                    text_style +: { font_size: 8.5 }
                }
            }
            type_label := PanelLabelDim {
                width: Fill{weight: 30.0}
                text: ""
                max_lines: 1
                text_overflow: TextOverflow.Ellipsis
            }
        }
        let BoxLeg = FabValueInput {
            width: Fill{weight: 100.0 max: 64}
            height: 16
            padding: Inset{left: 4 right: 4 top: 0 bottom: 0}
        }
        let BoxRowT = FabPropRow {
            height: Fit
            margin: Inset{left: 0 right: 0 top: 3 bottom: 9}
            box_col := View {
                width: Fill
                height: Fit
                flow: Down
                spacing: 2
                top_row := View {
                    width: Fill
                    height: Fit
                    align: Align{x: 0.5 y: 0.5}
                    leg_top := BoxLeg {}
                }
                mid_row := View {
                    width: Fill
                    height: Fit
                    flow: Right
                    spacing: 4
                    align: Align{x: 0.5 y: 0.5}
                    leg_left := BoxLeg {}
                    frame := View {
                        width: Fill{weight: 40.0}
                        height: 20
                        show_bg: true
                        draw_bg +: {
                            pixel: fn() {
                                let sdf = Sdf2d.viewport(self.pos * self.rect_size)
                                sdf.box(1.0, 1.0, self.rect_size.x - 2.0, self.rect_size.y - 2.0, 3.0)
                                sdf.fill_keep(#x26262600)
                                sdf.stroke(#x5a5a5a, 1.0)
                                return sdf.result
                            }
                        }
                    }
                    leg_right := BoxLeg {}
                }
                bot_row := View {
                    width: Fill
                    height: Fit
                    align: Align{x: 0.5 y: 0.5}
                    leg_bottom := BoxLeg {}
                }
            }
            link := PanelCheckBox {
                visible: false
                width: Fit
                height: Fit
                text: ""
            }
        }
        // The container's flow on two lines: the direction, the
        // wrap and how a wrapped row lines up; then the gaps.
        let FlowRowT = FabPropRow {
            height: Fit
            flow_col := View {
                width: Fill
                height: Fit
                flow: Down
                spacing: 2
                dir_row := View {
                    width: Fill
                    height: Fit
                    flow: Right{wrap: true}
                    spacing: 4
                    align: Align{x: 0.0 y: 0.5}
                    flow_seg := View { width: Fit height: Fit flow: Right spacing: 1
                        f_right := PanelButton { width: Fit height: Fit padding: Inset{left: 4 right: 4 top: 1 bottom: 1} margin: Inset{left:0 right:0 top:0 bottom:0} text: "" draw_text +: { text_style +: { font_size: 7.0 } } }
                        f_down := PanelButton { width: Fit height: Fit padding: Inset{left: 4 right: 4 top: 1 bottom: 1} margin: Inset{left:0 right:0 top:0 bottom:0} text: "" draw_text +: { text_style +: { font_size: 7.0 } } }
                        f_over := PanelButton { width: Fit height: Fit padding: Inset{left: 4 right: 4 top: 1 bottom: 1} margin: Inset{left:0 right:0 top:0 bottom:0} text: "" draw_text +: { text_style +: { font_size: 7.0 } } }
                    }
                    f_wrap := PanelButton { width: Fit height: Fit padding: Inset{left: 4 right: 4 top: 1 bottom: 1} margin: Inset{left:0 right:0 top:0 bottom:0} text: "" draw_text +: { text_style +: { font_size: 7.0 } } }
                    ra_seg := View { width: Fit height: Fit flow: Right spacing: 1 margin: Inset{left: 4 right: 0 top: 0 bottom: 0}
                        ra_top := PanelButton { width: Fit height: Fit padding: Inset{left: 4 right: 4 top: 1 bottom: 1} margin: Inset{left:0 right:0 top:0 bottom:0} text: "" draw_text +: { text_style +: { font_size: 7.0 } } }
                        ra_mid := PanelButton { width: Fit height: Fit padding: Inset{left: 4 right: 4 top: 1 bottom: 1} margin: Inset{left:0 right:0 top:0 bottom:0} text: "" draw_text +: { text_style +: { font_size: 7.0 } } }
                        ra_bottom := PanelButton { width: Fit height: Fit padding: Inset{left: 4 right: 4 top: 1 bottom: 1} margin: Inset{left:0 right:0 top:0 bottom:0} text: "" draw_text +: { text_style +: { font_size: 7.0 } } }
                    }
                }
                gap_row := View {
                    width: Fill
                    height: Fit
                    flow: Right{wrap: true}
                    spacing: 4
                    align: Align{x: 0.0 y: 0.5}
                    gap_label := PanelLabelSmall { width: Fit text: "gap" }
                    spacing_input := FabValueInput { width: 56 height: 18 }
                    wrap_box := View { width: Fit height: Fit flow: Right spacing: 4 align: Align{x: 0.0 y: 0.5}
                        wrap_label := PanelLabelSmall { width: Fit margin: Inset{left: 4 top: 0 right: 0 bottom: 0} text: "rows" }
                        wrap_input := FabValueInput { width: 56 height: 18 }
                    }
                }
            }
        }
        // The children along the flow and across it. Which is x
        // and which is y follows the direction, so the labels say.
        let AlignRowT = FabPropRow {
            height: Fit
            align_col := View {
                width: Fill
                height: Fit
                flow: Down
                spacing: 2
                just_row := View { width: Fill height: Fit flow: Right{wrap: true} spacing: 4 align: Align{x: 0.0 y: 0.5}
                    just_label := PanelLabelSmall { width: 34 text: "justify" }
                    just_seg := View { width: Fit height: Fit flow: Right spacing: 1
                        j_stretch := PanelButton { width: Fit height: Fit padding: Inset{left: 3 right: 3 top: 1 bottom: 1} margin: Inset{left:0 right:0 top:0 bottom:0} text: "" draw_text +: { text_style +: { font_size: 7.0 } } }
                        j_start := PanelButton { width: Fit height: Fit padding: Inset{left: 3 right: 3 top: 1 bottom: 1} margin: Inset{left:0 right:0 top:0 bottom:0} text: "" draw_text +: { text_style +: { font_size: 7.0 } } }
                        j_mid := PanelButton { width: Fit height: Fit padding: Inset{left: 3 right: 3 top: 1 bottom: 1} margin: Inset{left:0 right:0 top:0 bottom:0} text: "" draw_text +: { text_style +: { font_size: 7.0 } } }
                        j_end := PanelButton { width: Fit height: Fit padding: Inset{left: 3 right: 3 top: 1 bottom: 1} margin: Inset{left:0 right:0 top:0 bottom:0} text: "" draw_text +: { text_style +: { font_size: 7.0 } } }
                    }
                    just_axis := PanelLabelSmall { width: Fit text: "" }
                }
                space_row := View { width: Fill height: Fit flow: Right{wrap: true} spacing: 4 align: Align{x: 0.0 y: 0.5}
                    space_label := PanelLabelSmall { width: 34 text: "space" }
                    space_seg := View { width: Fit height: Fit flow: Right spacing: 1
                        s_between := PanelButton { width: Fit height: Fit padding: Inset{left: 3 right: 3 top: 1 bottom: 1} margin: Inset{left:0 right:0 top:0 bottom:0} text: "" draw_text +: { text_style +: { font_size: 7.0 } } }
                        s_around := PanelButton { width: Fit height: Fit padding: Inset{left: 3 right: 3 top: 1 bottom: 1} margin: Inset{left:0 right:0 top:0 bottom:0} text: "" draw_text +: { text_style +: { font_size: 7.0 } } }
                        s_evenly := PanelButton { width: Fit height: Fit padding: Inset{left: 3 right: 3 top: 1 bottom: 1} margin: Inset{left:0 right:0 top:0 bottom:0} text: "" draw_text +: { text_style +: { font_size: 7.0 } } }
                    }
                }
                cross_row := View { width: Fill height: Fit flow: Right{wrap: true} spacing: 4 align: Align{x: 0.0 y: 0.5}
                    cross_label := PanelLabelSmall { width: 34 text: "align" }
                    cross_seg := View { width: Fit height: Fit flow: Right spacing: 1
                        c_stretch := PanelButton { width: Fit height: Fit padding: Inset{left: 3 right: 3 top: 1 bottom: 1} margin: Inset{left:0 right:0 top:0 bottom:0} text: "" draw_text +: { text_style +: { font_size: 7.0 } } }
                        c_start := PanelButton { width: Fit height: Fit padding: Inset{left: 3 right: 3 top: 1 bottom: 1} margin: Inset{left:0 right:0 top:0 bottom:0} text: "" draw_text +: { text_style +: { font_size: 7.0 } } }
                        c_mid := PanelButton { width: Fit height: Fit padding: Inset{left: 3 right: 3 top: 1 bottom: 1} margin: Inset{left:0 right:0 top:0 bottom:0} text: "" draw_text +: { text_style +: { font_size: 7.0 } } }
                        c_end := PanelButton { width: Fit height: Fit padding: Inset{left: 3 right: 3 top: 1 bottom: 1} margin: Inset{left:0 right:0 top:0 bottom:0} text: "" draw_text +: { text_style +: { font_size: 7.0 } } }
                    }
                    cross_axis := PanelLabelSmall { width: Fit text: "" }
                }
            }
        }
        // A heading inside the Layout section: the rows under it are
        // about the selection in its parent, or about its children.
        let GroupRowT = View {
            width: Fill
            height: 20
            flow: Right
            align: Align{x: 0.0 y: 1.0}
            padding: Inset{left: 8 right: 6 top: 0 bottom: 2}
            title := PanelLabelSmall { width: Fill text: "" }
        }
        // What kind of container it is, the ask to become the other
        // kind, and the name its children can size against.
        let ContainerRowT = FabPropRow {
            height: Fit
            ctr_col := View {
                width: Fill
                height: Fit
                flow: Down
                spacing: 2
                mode_row := View { width: Fill height: Fit flow: Right{wrap: true} spacing: 6 align: Align{x: 0.0 y: 0.5}
                    mode_label := PanelLabelSmall { width: Fit text: "" }
                    convert := PanelButton { width: Fit height: Fit padding: Inset{left: 4 right: 4 top: 1 bottom: 1} margin: Inset{left:0 right:0 top:0 bottom:0} text: "" draw_text +: { text_style +: { font_size: 7.0 } } }
                }
                // The ask to put it in a Dock as its one tab. Beside
                // the grid/flex ask, not instead of it: that one
                // changes the container's type, this one wraps it in
                // its parent, and both can stand.
                dock_row := View { width: Fill height: Fit flow: Right{wrap: true} spacing: 6 align: Align{x: 0.0 y: 0.5}
                    dock_check := PanelCheckBox { width: Fit height: Fit text: "" }
                    dock_label := PanelLabelSmall { width: Fit text: "dockable" }
                }
                // What a standing ask means, on a line of its own: the
                // column is too narrow for it beside the box, and a
                // Fill label wraps where a Fit one runs off the panel.
                dock_hint := PanelLabelSmall { width: Fill text: "" }
                name_row := View { width: Fill height: Fit flow: Right spacing: 4 align: Align{x: 0.0 y: 0.5}
                    ctr_label := PanelLabelSmall { width: Fit text: "named" }
                    ctr_name := SizeInputT { width: Fill empty_text: "\u{2013}" label_align: Align{x: 0.0 y: 0.5} draw_text +: { ink_centered: false } }
                }
            }
        }
        // Out of the flow: a checkbox, and the place in the parent
        // once it is.
        let AbsRowT = FabPropRow {
            height: Fit
            abs_col := View {
                width: Fill
                height: Fit
                flow: Right{wrap: true}
                spacing: 6
                align: Align{x: 0.0 y: 0.5}
                abs_check := PanelCheckBox { width: Fit height: Fit text: "" }
                abs_label := PanelLabelSmall { width: Fit text: "absolute" }
                abs_xy := View { width: Fit height: Fit flow: Right spacing: 4 align: Align{x: 0.0 y: 0.5}
                    x_label := PanelLabelSmall { width: Fit text: "x" }
                    abs_x := FabValueInput { width: 44 height: 18 }
                    y_label := PanelLabelSmall { width: Fit text: "y" }
                    abs_y := FabValueInput { width: 44 height: 18 }
                }
            }
        }
        // A Grid's own layout: the tracks as CSS, the gaps, which way
        // cells without a place are filled in, and the named areas.
        let GridRowT = FabPropRow {
            height: Fit
            grid_col := View {
                width: Fill
                height: Fit
                flow: Down
                spacing: 2
                cols_row := View { width: Fill height: Fit flow: Right spacing: 4 align: Align{x: 0.0 y: 0.5}
                    cols_label := PanelLabelSmall { width: 40 text: "columns" }
                    cols_in := SizeInputT { width: Fill label_align: Align{x: 0.0 y: 0.5} draw_text +: { ink_centered: false } }
                }
                rows_row := View { width: Fill height: Fit flow: Right spacing: 4 align: Align{x: 0.0 y: 0.5}
                    rows_label := PanelLabelSmall { width: 40 text: "rows" }
                    rows_in := SizeInputT { width: Fill label_align: Align{x: 0.0 y: 0.5} draw_text +: { ink_centered: false } }
                }
                gaps_row := View { width: Fill height: Fit flow: Right{wrap: true} spacing: 4 align: Align{x: 0.0 y: 0.5}
                    gap_label := PanelLabelSmall { width: 34 text: "gap" }
                    gap_x := PanelLabelSmall { width: Fit text: "\u{2194}" }
                    gap_col := FabValueInput { width: 40 height: 18 }
                    gap_y := PanelLabelSmall { width: Fit margin: Inset{left: 4 top: 0 right: 0 bottom: 0} text: "\u{2195}" }
                    gap_row_in := FabValueInput { width: 40 height: 18 }
                }
                fill_row := View { width: Fill height: Fit flow: Right{wrap: true} spacing: 4 align: Align{x: 0.0 y: 0.5}
                    fill_label := PanelLabelSmall { width: 40 text: "fill" }
                    fill_seg := View { width: Fit height: Fit flow: Right spacing: 1
                        f_rows := PanelButton { width: Fit height: Fit padding: Inset{left: 3 right: 3 top: 1 bottom: 1} margin: Inset{left:0 right:0 top:0 bottom:0} text: "" draw_text +: { text_style +: { font_size: 7.0 } } }
                        f_cols := PanelButton { width: Fit height: Fit padding: Inset{left: 3 right: 3 top: 1 bottom: 1} margin: Inset{left:0 right:0 top:0 bottom:0} text: "" draw_text +: { text_style +: { font_size: 7.0 } } }
                    }
                }
                areas_row := View { width: Fill height: Fit flow: Right spacing: 4 align: Align{x: 0.0 y: 0.5}
                    areas_label := PanelLabelSmall { width: 40 text: "areas" }
                    areas_in := SizeInputT { width: Fill label_align: Align{x: 0.0 y: 0.5} draw_text +: { ink_centered: false } }
                }
            }
        }
        // Where a child of a Grid sits: a column and row (0 is wherever
        // the fill order puts it), how many of each it spans, or an
        // area by name.
        let CellRowT = FabPropRow {
            height: Fit
            cell_col := View {
                width: Fill
                height: Fit
                flow: Down
                spacing: 2
                place_row := View { width: Fill height: Fit flow: Right{wrap: true} spacing: 4 align: Align{x: 0.0 y: 0.5}
                    col_label := PanelLabelSmall { width: Fit text: "col" }
                    cell_c := FabValueInput { width: 40 height: 18 min: 0.0 step: 1.0 precision: 0 }
                    row_label := PanelLabelSmall { width: Fit margin: Inset{left: 4 top: 0 right: 0 bottom: 0} text: "row" }
                    cell_r := FabValueInput { width: 40 height: 18 min: 0.0 step: 1.0 precision: 0 }
                }
                span_row := View { width: Fill height: Fit flow: Right{wrap: true} spacing: 4 align: Align{x: 0.0 y: 0.5}
                    span_label := PanelLabelSmall { width: Fit text: "span" }
                    cell_cs := FabValueInput { width: 40 height: 18 min: 0.0 step: 1.0 precision: 0 }
                    by_label := PanelLabelSmall { width: Fit text: "\u{00d7}" }
                    cell_rs := FabValueInput { width: 40 height: 18 min: 0.0 step: 1.0 precision: 0 }
                }
                area_row := View { width: Fill height: Fit flow: Right spacing: 4 align: Align{x: 0.0 y: 0.5}
                    area_label := PanelLabelSmall { width: Fit text: "area" }
                    cell_area := SizeInputT { width: Fill empty_text: "\u{2013}" label_align: Align{x: 0.0 y: 0.5} draw_text +: { ink_centered: false } }
                }
            }
        }

        let PropRow = TweakRow{width: Fill height: Fit flow: Down cursor: MouseCursor.Default grab_key_focus: false}
        let Refusal = PanelLabelSmall{visible: false width: Fill flow: Right{wrap: true}
            margin: Inset{left: 8 right: 4 bottom: 2} draw_text.color: #xe0a020}
        PortalList {
            width: Fill height: Fill
            margin: Inset{top: 2}
            drag_scrolling: false
            InfoRow := TweakRow{width: Fill height: Fit InfoRowT{}}
            SectionRow := TweakRow{width: Fill height: Fit header := SectionRowT{}}
            ColorRow := PropRow{
                editor := ColorRowT{}
                tname_wrap := View {
                    width: Fill height: Fit visible: false flow: Right
                    align: Align{x: 1.0 y: 0.5}
                    padding: Inset{left: 8 right: 14 top: 0 bottom: 3}
                    tname := PanelButton {
                        width: Fit{max: FitBound.Rel{base: Base.Full factor: 1.0}}
                        height: 16 margin: Inset{left: 0 right: 0 top: 0 bottom: 0}
                        padding: Inset{left: 4 right: 4 top: 1 bottom: 1}
                        text: ""
                        draw_text +: {max_lines: 1 text_overflow: TextOverflow.Ellipsis text_style +: {font_size: 7.0}}
                    }
                }
                refusal := Refusal{}
            }
            TextRow := PropRow{editor := TextRowT{} refusal := Refusal{}}
            NumRow := PropRow{editor := NumRowT{} refusal := Refusal{}}
            BoolRow := PropRow{editor := BoolRowT{} refusal := Refusal{}}
            VecRow := PropRow{editor := VecRowT{} refusal := Refusal{}}
            InsetRow := PropRow{editor := InsetRowT{} refusal := Refusal{}}
            MetricsRow := PropRow{editor := MetricsRowT{} refusal := Refusal{}}
            NoEditorRow := PropRow{editor := NoEditorRowT{} refusal := Refusal{}}
            IdentityRow := PropRow{editor := IdentityRowT{} refusal := Refusal{}
                rename_confirm := View{visible: false width: Fill height: Fit flow: Right{wrap: true} spacing: 3
                    rename_anyway := PanelButton{text: "Rename anyway"}
                    rename_cancel := PanelButton{text: "Cancel"}
                }
            }
            SizeRow := PropRow{editor := SizeRowT{} refusal := Refusal{}}
            BoxRow := PropRow{editor := BoxRowT{} refusal := Refusal{}}
            FlowRow := PropRow{editor := FlowRowT{} refusal := Refusal{}}
            AlignRow := PropRow{editor := AlignRowT{} refusal := Refusal{}}
            GroupRow := PropRow{editor := GroupRowT{} refusal := Refusal{}}
            ContainerRow := PropRow{editor := ContainerRowT{} refusal := Refusal{}}
            AbsRow := PropRow{editor := AbsRowT{} refusal := Refusal{}}
            GridRow := PropRow{editor := GridRowT{} refusal := Refusal{}}
            CellRow := PropRow{editor := CellRowT{} refusal := Refusal{}}
            MeasuredRow := TweakRow{width: Fill height: Fit MeasuredRowT{}}
            CascadeRow := TweakRow{width: Fill height: Fit CascadeRowT{}}
        }
    })
}

fn component_fields(kind: StructKind) -> &'static [&'static [LiveId]] {
    match kind {
        StructKind::Vec2 => &[ids!(vx), ids!(vy)],
        StructKind::Vec3 => &[ids!(vx), ids!(vy), ids!(vz)],
        StructKind::Vec4 => &[ids!(vx), ids!(vy), ids!(vz), ids!(vw)],
        StructKind::Inset => &[ids!(il), ids!(it), ids!(ir), ids!(ib)],
        StructKind::Metrics => &[ids!(m0), ids!(m1), ids!(m2)],
        StructKind::None | StructKind::NoEditor => &[],
    }
}

fn cascade_icon_kind(file: &str) -> usize {
    if file.is_empty() {
        3
    } else if file.rsplit('/').next().unwrap_or("").contains("theme") {
        2
    } else if file.starts_with("widgets/") || file.contains("/widgets/") {
        1
    } else {
        0
    }
}

fn fmt_measure(v: f64) -> String {
    let rounded = (v * 10.0).round() / 10.0;
    if (rounded - rounded.round()).abs() < f64::EPSILON {
        format!("{}", rounded.round() as i64)
    } else {
        format!("{rounded:.1}")
    }
}


#[derive(Clone, Copy, PartialEq)]
pub(in crate::tweak) enum HeaderId { ShaderConstants, Theme(u8) }

#[derive(Clone, PartialEq)]
pub(in crate::tweak) enum PropLine {
    Section(SectionKind, usize, bool),
    More(SectionKind, usize),
    Header { title: String, count: String, open: Option<(HeaderId, bool)> },
    Info { name: String, value: String, loc: String },
    Identity { path: Rc<str>, ty: Rc<str>, field: RenameField },
    Measured(String),
    Layout { kind: LayoutKind, path: Rc<str>, values: Rc<LayoutValues>, refusal: String },
    Prop { path: Rc<str>, row: RowBinding, theme_match: Option<Rc<str>>, tag: Option<&'static str>, refusal: Option<String> },
    CascadeLevel { level: usize, loc: String, icon: usize, color: Vec4f },
}

impl PropLine {
    pub(in crate::tweak) fn same_binding(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Identity { path, .. }, Self::Identity { path: other, .. }) => path == other,
            (Self::Layout { path, kind, values, .. }, Self::Layout { path: p, kind: k, values: v, .. }) =>
                path == p && kind == k && values.is_grid == v.is_grid,
            (Self::Prop { path, row, .. }, Self::Prop { path: other_path, row: other_row, .. }) =>
                path == other_path && row.prop == other_row.prop && row.kind == other_row.kind
                    && row.struct_kind == other_row.struct_kind && row.quoted == other_row.quoted,
            _ => self == other,
        }
    }

    pub(in crate::tweak) fn template(&self) -> LiveId {
        match self {
            Self::Section(..) | Self::More(..) | Self::Header { .. } => id!(SectionRow),
            Self::Info { .. } => id!(InfoRow),
            Self::Measured(_) => id!(MeasuredRow),
            Self::Identity { .. } => id!(IdentityRow),
            Self::Layout { kind, .. } => kind.template(),
            Self::Prop { row, .. } => row.template(),
            Self::CascadeLevel { .. } => id!(CascadeRow),
        }
    }

    pub(in crate::tweak) fn draw_sidebar(&self, cx: &mut Cx, item: &View, previous: Option<&Self>, controls: &mut LayoutControls) {
        match self {
            Self::Section(section, count, open) => {
                item.label(cx, ids!(title)).set_text(cx, section.title());
                item.label(cx, ids!(count)).set_text(cx, &format!("{count}{}", if *open { "" } else { "  +" }));
            }
            Self::Header { title, count, open } => {
                item.label(cx, ids!(title)).set_text(cx, title);
                item.label(cx, ids!(count)).set_text(cx, &format!("{count}{}", if matches!(open, Some((_, false))) { "  +" } else { "" }));
            }
            Self::Info { name, value, loc } => {
                item.label(cx, ids!(name)).set_text(cx, name);
                item.label(cx, ids!(value)).set_text(cx, value);
                item.label(cx, ids!(loc)).set_text(cx, loc);
            }
            Self::More(_, count) => {
                item.label(cx, ids!(title)).set_text(cx, &format!("+ show all ({count})"));
                item.label(cx, ids!(count)).set_text(cx, "");
            }
            Self::Identity { path, ty, field } => {
                field.bind(cx, item, path, ty, previous.is_some_and(|p| !self.same_binding(p)));
            }
            Self::Layout { kind, values, refusal, .. } => {
                let editor = item.widget(cx, ids!(editor));
                let new_binding = previous.is_some_and(|p| !self.same_binding(p));
                values.draw_sidebar(cx, &editor, *kind, new_binding, controls);
                let error = item.label(cx, ids!(refusal));
                error.set_visible(cx, !refusal.is_empty());
                error.set_text(cx, refusal);
            }
            Self::Measured(text) => item.label(cx, ids!(measured)).set_text(cx, text),
            Self::CascadeLevel { level, loc, icon, color } => {
                for (k, id) in [ids!(ic_app), ids!(ic_lib), ids!(ic_theme), ids!(ic_native)].into_iter().enumerate() {
                    item.widget(cx, id).set_visible(cx, k == *icon);
                }
                let chip = item.widget(cx, ids!(chip));
                chip.child(id!(lbl)).set_text(cx, &format!("L{level}"));
                if let Some(mut chip) = chip.borrow_mut::<View>() {
                    chip.draw_bg.set_dyn_instance(cx, id!(color), &[color.x, color.y, color.z, color.w]);
                }
                item.label(cx, ids!(loc)).set_text(cx, loc);
            }
            Self::Prop { path, row, theme_match, tag, refusal } => {
                let name = item.label(cx, ids!(name));
                if let Some(tag) = tag {
                    name.set_text(cx, &format!("{} · {tag}", row.prop));
                } else {
                    name.set_text(cx, &row.prop);
                }
                name.set_text_color(cx, if row.changed {
                    vec4(1.0, 0.78, 0.42, 1.0)
                } else if row.set {
                    vec4(0.72, 0.72, 0.72, 1.0)
                } else {
                    vec4(0.48, 0.48, 0.48, 1.0)
                });
                let origin = item.label(cx, ids!(origin));
                if let Some(level) = row.level {
                    origin.set_text(cx, "●");
                    origin.set_text_color(cx, level_color(level));
                } else {
                    origin.set_text(cx, "");
                }
                let same_prop = matches!(previous, Some(Self::Prop {path: p, row: r, ..})
                    if p == path && r.prop == row.prop);
                let value_changed = !same_prop || !matches!(previous, Some(Self::Prop { row: r, .. }) if r.value == row.value);
                let bind_number = |cx: &mut Cx, field: &[LiveId], value: f64| {
                    if let Some(mut input) = item.widget(cx, field).borrow_mut::<FabValueInput>() {
                        if previous.is_some() && !same_prop {
                            // A recycled scrubber must drop the old property's drag or open text edit.
                            input.set_enabled(cx, false);
                            input.set_enabled(cx, true);
                        }
                        input.set_value(cx, value);
                    }
                };
                match row.struct_kind {
                    StructKind::Vec2 | StructKind::Vec3 | StructKind::Vec4 | StructKind::Inset | StructKind::Metrics => {
                        if matches!(row.struct_kind, StructKind::Vec2 | StructKind::Vec3 | StructKind::Vec4) {
                            item.widget(cx, ids!(vz_wrap)).set_visible(cx, row.comp_vals.len() >= 3);
                            item.widget(cx, ids!(vw_wrap)).set_visible(cx, row.comp_vals.len() == 4);
                        }
                        for (field, value) in component_fields(row.struct_kind).iter().zip(&row.comp_vals) {
                            bind_number(cx, field, *value);
                        }
                    }
                    StructKind::NoEditor => {}
                    StructKind::None => match row.kind {
                        RowKind::Num => {
                            if let Ok(v) = row.value.parse::<f64>() {
                                bind_number(cx, ids!(value), v);
                            }
                        }
                        RowKind::Bool => item.check_box(cx, ids!(value)).set_active(cx, row.value == "true", Animate::No),
                        RowKind::Text | RowKind::Color | RowKind::Info => {
                            let field = item.text_input(cx, ids!(value));
                            if !same_prop && field.area().is_valid(cx) && field.area() == cx.key_focus() {
                                cx.set_key_focus(Area::Empty);
                            }
                            if value_changed && (field.area() == Area::Empty || !cx.has_key_focus(field.area())) {
                                field.set_text(cx, &row.value);
                            }
                            if row.kind == RowKind::Color {
                                let wrap = item.widget(cx, ids!(tname_wrap));
                                wrap.set_visible(cx, theme_match.is_some());
                                if let Some(token) = theme_match {
                                    wrap.child(id!(tname)).set_text(cx, &format!("≈ theme.{token}"));
                                }
                                if let Some(mut pick) = item.widget(cx, ids!(swatch)).borrow_mut::<FabColorPick>() {
                                    if !pick.is_open() {
                                        if let Some((rgba, _)) = super::parse_hex(&row.value) { pick.set_rgba(cx, rgba); }
                                    }
                                }
                            }
                        }
                    },
                }
                let error = item.label(cx, ids!(refusal));
                error.set_visible(cx, refusal.is_some());
                error.set_text(cx, refusal.as_deref().unwrap_or(""));
            }
        }
    }
}

#[derive(Clone, Default, PartialEq)]
pub(in crate::tweak) struct RenameField {
    pub(in crate::tweak) editable: bool,
    pub(in crate::tweak) draft: Option<String>,
    pub(in crate::tweak) message: String,
    pub(in crate::tweak) confirm: bool,
    pub(in crate::tweak) busy: bool,
}

impl RenameField {
    fn edit(&mut self, text: &str) {
        if self.draft.as_deref() == Some(text) { return }
        self.draft = Some(text.into());
        self.confirm = false;
        self.message = if text.len() <= crate::tweak::wire::MAX_DESIGN_NAME_LEN
            && makepad_widgets::makepad_script::syntax::write::key(text).is_ok_and(|key| key == text) {
            String::new()
        } else { "Enter a valid Splash identifier.".into() };
    }
}

#[derive(Default)]
pub(in crate::tweak) struct Sidebar {
    pub(in crate::tweak) rename: RenameField,
    pub(in crate::tweak) located: bool,
    pub(in crate::tweak) location_checked: bool,
    pub(in crate::tweak) props: Props,
    pub(in crate::tweak) path: Rc<str>,
    pub(in crate::tweak) ty: String,
    pub(in crate::tweak) rect: Option<super::super::Rect>,
    pub(in crate::tweak) collapsed: [bool; 6],
    pub(in crate::tweak) expanded: [bool; 6],
    pub(in crate::tweak) refusals: HashMap<String, String>,
    // Local Sets still ahead of the last Rows snapshot for this selection.
    pub(in crate::tweak) layout_edits: BTreeMap<RequestId, (String, String)>,
    pub(in crate::tweak) components: BTreeMap<RequestId, (String, StructKind, Vec<f64>)>,
    pub(in crate::tweak) last_label_click: Option<(f64, String)>,
}

pub(in crate::tweak) enum SidebarAction<'a> {
    Rename { name: String, force: bool },
    Redraw,
    ClearRefusal(Rc<str>, String),
    PaletteOpened(WidgetUid),
    Sets { path: Rc<str>, edits: PropertySets<'a>, phase: Phase },
}

impl Phase {
    fn number(action: &FabValueInputAction) -> Self {
        match action {
            FabValueInputAction::Changed(_) => Self::Step,
            FabValueInputAction::Ended(_) => Self::End,
            _ => Self::Off,
        }
    }
}

impl Sidebar {
    fn label_double_click(&mut self, prop: &str, time: f64) -> bool {
        if self.last_label_click.as_ref().is_some_and(|(t, p)| p == prop && time - t < 0.4) {
            self.last_label_click = None;
            return true;
        }
        self.last_label_click = Some((time, prop.to_string()));
        false
    }

    pub(in crate::tweak) fn bound_row(&self, row: &RowBinding) -> RowBinding {
        let mut row = row.clone();
        if !row.comp_vals.is_empty() {
            if let Some((_, _, values)) = self.components.values().rev()
                .find(|(prop, kind, _)| *prop == row.prop && *kind == row.struct_kind) {
                row.comp_vals.clone_from(values);
            }
        }
        row
    }

    pub(in crate::tweak) fn handle_sidebar_actions<'a>(&mut self, cx: &mut Cx, item: &WidgetRef,
        line: &'a PropLine, actions: &'a Actions, controls: &'a LayoutControls, live: &LiveSets) -> Option<SidebarAction<'a>> {
        match line {
            PropLine::Section(section, ..) | PropLine::More(section, _) => {
                let tap = item.view(cx, ids!(header)).finger_up(actions)?;
                if !tap.is_over || !tap.was_tap() { return None; }
                if matches!(line, PropLine::More(..)) {
                    self.expanded[section.index()] = true;
                } else {
                    let folded = &mut self.collapsed[section.index()];
                    *folded = !*folded;
                }
                Some(SidebarAction::Redraw)
            }
            PropLine::Identity { path, .. } if self.rename.editable && !self.rename.busy && self.path == *path => {
                let input = item.text_input(cx, ids!(name_field));
                if item.button(cx, ids!(rename_cancel)).clicked(actions) || input.escaped(actions) {
                    self.rename = RenameField { editable: true, ..Default::default() };
                    input.set_text(cx, crate::tweak::widget_name(path));
                    return Some(SidebarAction::Redraw);
                }
                if let Some(text) = input.changed(actions) {
                    self.rename.edit(&text);
                    return Some(SidebarAction::Redraw);
                }
                let force = self.rename.confirm && item.button(cx, ids!(rename_anyway)).clicked(actions);
                if force {
                    return Some(SidebarAction::Rename { name: self.rename.draft.clone()?, force });
                }
                let text = input.returned(actions).map(|(text, _)| text)
                    .or_else(|| input.key_focus_lost(actions).then(|| input.text()))?;
                if self.rename.confirm || text == crate::tweak::widget_name(path) { return None }
                self.rename.edit(&text);
                if self.rename.message.is_empty() { Some(SidebarAction::Rename { name: text, force }) }
                else { Some(SidebarAction::Redraw) }
            }
            PropLine::Layout { kind, path, values, .. } => {
                let action = actions.first()?.as_widget_action()?;
                let event = if action.widget_uid == item.widget_uid() {
                    let ViewAction::FingerDown(e) = action.cast() else { return None };
                    let prop = values.reset_label(cx, &item.widget(cx, ids!(editor)), *kind, e.abs)?;
                    if !self.label_double_click(prop, e.time) { return None; }
                    LayoutEvent::Reset(prop)
                } else {
                    controls.event(action)?
                };
                let phase = match event { LayoutEvent::Number(_, n) => Phase::number(n), _ => Phase::Off };
                // A second Fill field or position axis can move before the
                // first one's final value has reached the app or been painted.
                let pending = match event {
                    LayoutEvent::Number(field, _) | LayoutEvent::Returned(field, _) => field.split_once('#'),
                    _ => None,
                }.and_then(|(prop, _)| live.waiting(path, prop).or_else(||
                    self.layout_edits.values().rev().find(|(p, _)| p == prop).map(|(_, v)| v.as_str())));
                let edits = values.decision(event, pending)?;
                Some(SidebarAction::Sets { path: path.clone(), edits, phase })
            }
            PropLine::Prop { path, row, theme_match, .. } if row.kind != RowKind::Info => {
                let action = actions.first()?.as_widget_action()?;
                let commit = |binding: &RowBinding, event, phase| binding.set_value(event).map(|value|
                    SidebarAction::Sets { path: path.clone(), edits: PropertySets::one(row.prop.as_str(), value), phase });
                if action.widget_uid == item.widget_uid() {
                    if let ViewAction::FingerDown(e) = action.cast() {
                        // The reset affordance is a double press on this row's label.
                        let rect = item.widget(cx, ids!(editor)).area().rect(cx);
                        let label_zone = Rect { pos: rect.pos, size: dvec2(104.0_f64.min(rect.size.x * 0.5), rect.size.y) };
                        if label_zone.contains(e.abs) && self.label_double_click(&row.prop, e.time) {
                            return commit(row, EditorEvent::Reset, Phase::Off);
                        }
                    }
                    return None;
                }
                if let Some(number) = action.action.downcast_ref::<FabValueInputAction>() {
                    let fields = component_fields(row.struct_kind);
                    let leg = if fields.is_empty() {
                        if item.widget(cx, ids!(value)).widget_uid() != action.widget_uid { return None; }
                        None
                    } else {
                        Some(fields.iter().position(|f| item.widget(cx, f).widget_uid() == action.widget_uid)?)
                    };
                    let event = EditorEvent::Number { leg, action: number.clone() };
                    let phase = Phase::number(number);
                    if leg.is_some() && phase != Phase::Off {
                        // A different leg may have moved since the last send or paint.
                        let mut binding = self.bound_row(row);
                        if let Some(value) = live.waiting(path, &row.prop) {
                            binding.comp_vals = super::parse_struct(value).1;
                        }
                        return commit(&binding, event, phase);
                    }
                    return commit(row, event, phase);
                }
                let bind_theme = |name: &str| Some(SidebarAction::Sets { path: path.clone(),
                    edits: PropertySets::one(row.prop.as_str(), format!("theme.{name}")), phase: Phase::Off });
                if let Some(name) = theme_match {
                    if item.button(cx, ids!(tname)).clicked(actions) { return bind_theme(name) }
                }
                if row.kind == RowKind::Color && action.widget_uid == item.widget(cx, ids!(swatch)).widget_uid() {
                    let (v, phase) = match action.action.downcast_ref::<FabColorPickAction>()? {
                        FabColorPickAction::Opened => return Some(SidebarAction::PaletteOpened(action.widget_uid)),
                        FabColorPickAction::PalettePick(name) => return bind_theme(name),
                        FabColorPickAction::Changed(v) => (*v, Phase::Step),
                        FabColorPickAction::Ended(v) => (*v, Phase::End),
                        _ => return None,
                    };
                    return commit(row, EditorEvent::Color(v), phase);
                }
                if action.widget_uid != item.widget(cx, ids!(value)).widget_uid() { return None; }
                if let Some(CheckBoxAction::Change(on)) = action.action.downcast_ref::<CheckBoxAction>() {
                    return commit(row, EditorEvent::Toggle(*on), Phase::Off);
                }
                match action.action.downcast_ref::<TextInputAction>()? {
                    TextInputAction::Escaped => {
                        item.text_input(cx, ids!(value)).set_text(cx, &row.value);
                        Some(SidebarAction::ClearRefusal(path.clone(), row.prop.clone()))
                    }
                    TextInputAction::Returned(text, _) => {
                        let set = commit(row, EditorEvent::Text(text), Phase::Off);
                        if set.is_none() && row.kind == RowKind::Color {
                            if path.as_ref() == "theme" && !text.trim().is_empty() {
                                return Some(SidebarAction::Sets { path: path.clone(),
                                    edits: PropertySets::one(row.prop.as_str(), text.trim().into()), phase: Phase::Off })
                            }
                            item.text_input(cx, ids!(value)).set_text(cx, &row.value);
                        }
                        set
                    }
                    _ => None,
                }
            }
            _ => None,
        }
    }

    fn measured(&self) -> String {
        let mut parts = Vec::new();
        if !self.ty.is_empty() { parts.push(self.ty.clone()); }
        if let Some(r) = self.rect {
            parts.push(format!("measured {} × {} px", fmt_measure(r.w), fmt_measure(r.h)));
        }
        // The peer's Picked notice carries the rect. Rows also supplies the current
        // box model, including changes another seat made after the pick.
        for name in ["margin", "padding"] {
            if let Some(row) = self.props.rows.iter().find(|r| r.prop == name) {
                parts.push(format!("{name} {}", row.value));
                continue;
            }
            let legs: Vec<_> = self.props.rows.iter().filter_map(|r| {
                let (prop, leg) = r.prop.split_once('.')?;
                (prop == name && inset_leg_rank(leg) < 4).then_some((leg, r.value.as_str()))
            }).collect();
            if !legs.is_empty() {
                if legs.len() == 4 && legs.iter().all(|(_, v)| *v == legs[0].1) {
                    parts.push(format!("{name} {}", legs[0].1));
                } else {
                    parts.push(format!("{name} {}", legs.iter().map(|(k, v)| format!("{k} {v}")).collect::<Vec<_>>().join(", ")));
                }
            }
        }
        parts.join(" · ")
    }

    pub(in crate::tweak) fn lines(&self, palette: &ThemePalette) -> Vec<PropLine> {
        if self.path.is_empty() { return Vec::new(); }
        let entries = self.props.visible_rows(&self.collapsed, &self.expanded, &self.ty);
        let mut values = self.props.layout_values(&self.ty);
        for (prop, value) in self.layout_edits.values() {
            values.values.insert(prop.clone(), value.clone());
        }
        let values = Rc::new(values);
        let ty: Rc<str> = Rc::from(self.ty.as_str());
        entries.into_iter().map(|entry| match entry {
            VisKind::Layout(LayoutKind::Identity) => PropLine::Identity { path: self.path.clone(), ty: ty.clone(), field: self.rename.clone() },
            VisKind::Layout(kind) => {
                let mut failures: Vec<_> = self.refusals.iter().filter(|(prop, _)| kind.owns(prop))
                    .map(|(prop, why)| format!("{prop}: {why}")).collect();
                failures.sort();
                PropLine::Layout { kind, path: self.path.clone(), values: values.clone(), refusal: failures.join("\n") }
            }
            VisKind::Section(section, count, open) => PropLine::Section(section, count, open),
            VisKind::More(section, count) => PropLine::More(section, count),
            VisKind::Measured => PropLine::Measured(self.measured()),
            VisKind::Prop(index) => {
                let row = &self.props.rows[index];
                PropLine::Prop { path: self.path.clone(), row: self.bound_row(row), theme_match: (row.kind == RowKind::Color).then(|| palette.match_hex(&row.value)).flatten(), tag: None, refusal: self.refusals.get(&row.prop).cloned() }
            }
            VisKind::CascadeLevel(level) => {
                let lvl = &self.props.cascade[level];
                PropLine::CascadeLevel { level, loc: if lvl.file.is_empty() { "native".into() } else { lvl.loc.clone() },
                    icon: cascade_icon_kind(&lvl.file), color: lvl.color }
            }
        }).collect()
    }
}
