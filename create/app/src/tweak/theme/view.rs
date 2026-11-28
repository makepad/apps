//! The live theme picker and member store controls above the shared property list.
use super::builder::*;
use super::{BuildAction, lab::ThemeMix, MixAction, ThemeAction, ThemeChoice, ThemeTab};
use makepad_widgets::{fab_controls::{FabColorPick, FabColorPickAction, FabPaletteCarousel, FabPaletteCarouselAction, FabSliderWidgetRefExt}, theme_tokens::{Appearance, BlendTheme, WeightMode}};
use makepad_widgets::*;

/// FlatList draws through its parent in catalogue order. Keep the row widgets
/// across weight moves so focused sliders and active drags retain their identity.
#[derive(Script, ScriptHook, Widget)]
pub struct EqRows {
    #[source] source: ScriptObjectRef,
    #[deref] view: View,
    #[rust] sliders: Vec<(BlendTheme, WidgetRef)>,
}

impl EqRows {
    fn bind(&mut self, cx: &mut Cx, lab: &ThemeMix) {
        if self.sliders.iter().map(|(t, _)| t).ne(lab.rows().iter().map(|r| &r.theme)) {
            let list = self.view.flat_list(cx, ids!(eq_rows));
            let Some(mut list) = list.borrow_mut() else { return };
            self.sliders.clear();
            for row in lab.rows() {
                let Some(slider) = list.item(cx, LiveId::from_str(&row.theme.name()), id!(Row)) else { continue };
                slider.as_fab_slider().set_label(cx, &row.label);
                self.sliders.push((row.theme, slider));
            }
            list.items.retain_visible();
            list.redraw(cx);
        }
        for ((row, share), (_, slider)) in lab.rows().iter().zip(lab.displayed_shares()).zip(&self.sliders) {
            slider.as_fab_slider().set_value_and_readout(cx, row.weight, share);
        }
    }

    fn action(&self, actions: &Actions) -> Option<MixAction> {
        for (index, (_, widget)) in self.sliders.iter().enumerate() {
            let slider = widget.as_fab_slider();
            if slider.was_reset(actions) { return Some(MixAction::Reset(index)) }
            if let Some(value) = slider.ended(actions) { return Some(MixAction::Weight { index, value, ended: true }) }
            if let Some(value) = slider.changed(actions) { return Some(MixAction::Weight { index, value, ended: false }) }
        }
        None
    }
}

impl Widget for EqRows {
    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        while let Some(step) = self.view.draw_walk(cx, scope, walk).step() {
            if step.borrow::<FlatList>().is_none() { return Err(step) }
            for (_, slider) in &self.sliders { slider.draw_all(cx, scope); }
        }
        DrawStep::done()
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope);
    }
}

pub(in crate::tweak) fn wrap_value(vm: &mut ScriptVm, rows: ScriptValue) -> ScriptValue {
    script_eval!(vm, {
        use mod.prelude.workspace.*
        use mod.widgets.*
        let EqRows = #(rows)
        let fab = mod.fab
        let PanelFont = mod.text.TextStyle{
            font_family: mod.text.FontFamily{
                latin := mod.text.FontMember{
                    res: crate_resource("makepad_widgets:resources/IBMPlexSans-Text.ttf")
                    asc: -0.1
                    desc: 0.0
                }
            }
            line_spacing: 1.2
            font_size: 8.0
        }
        // One row of the open list. Its ground, its hover and the
        // row in force, all the panel's own.
        let PanelMenuItem = PopupMenuItem {
            width: Fill
            height: Fit
            align: Align{x: 0.0 y: 0.5}
            padding: Inset{left: 7 right: 7 top: 3 bottom: 3}
            draw_text +: {
                color: fab.color_text
                color_hover: fab.color_text_active
                color_active: fab.color_text_active
                color_disabled: #x8c8c8c
                text_style: PanelFont{}
            }
            draw_icon +: { color: fab.color_text }
            draw_bg +: {
                border_size: 0.0
                border_radius: 2.0
                color: fab.color_popover
                color_hover: fab.color_row_hover
                color_active: fab.color_selection_bg
                color_disabled: fab.color_popover
                color_2: vec4(-1.0, -1.0, -1.0, -1.0)
                color_2_hover: vec4(-1.0, -1.0, -1.0, -1.0)
                color_2_active: vec4(-1.0, -1.0, -1.0, -1.0)
                color_2_disabled: vec4(-1.0, -1.0, -1.0, -1.0)
                border_color: vec4(0.0, 0.0, 0.0, 0.0)
                border_color_hover: vec4(0.0, 0.0, 0.0, 0.0)
                border_color_active: vec4(0.0, 0.0, 0.0, 0.0)
                border_color_disabled: vec4(0.0, 0.0, 0.0, 0.0)
                border_color_2: vec4(-1.0, -1.0, -1.0, -1.0)
                mark_color: vec4(0.0, 0.0, 0.0, 0.0)
                mark_color_active: vec4(0.0, 0.0, 0.0, 0.0)
                mark_color_disabled: vec4(0.0, 0.0, 0.0, 0.0)
                pixel: fn() {
                    let sdf = Sdf2d.viewport(self.pos * self.rect_size)
                    sdf.box(0.0, 0.0, self.rect_size.x, self.rect_size.y, self.border_radius)
                    sdf.fill(
                        self.color
                            .mix(self.color_active, self.active)
                            .mix(self.color_hover, self.hover)
                            .mix(self.color_disabled, self.disabled)
                    )
                    return sdf.result
                }
            }
        }
        // The open list's own plate: the popover tone, one hairline,
        // and no theme token anywhere in it.
        let PanelPopupMenu = PopupMenu {
            width: 224.
            height: Fit
            flow: Down
            padding: Inset{left: 3 right: 3 top: 3 bottom: 3}
            menu_item: PanelMenuItem{}
            draw_bg +: {
                border_size: 1.0
                border_radius: 3.0
                color: fab.color_popover
                color_2: vec4(-1.0, -1.0, -1.0, -1.0)
                border_color: fab.color_popover_border
                border_color_2: vec4(-1.0, -1.0, -1.0, -1.0)
                pixel: fn() {
                    let sdf = Sdf2d.viewport(self.pos * self.rect_size)
                    sdf.box(
                        0.5
                        0.5
                        self.rect_size.x - 1.0
                        self.rect_size.y - 1.0
                        self.border_radius
                    )
                    sdf.fill_keep(self.color)
                    sdf.stroke(self.border_color, self.border_size)
                    return sdf.result
                }
            }
        }
        // The closed face. Every metric a sheet could reach is
        // written out here -- `min_height`, the padding, the margin,
        // the bevel and the corner -- and the shader is this
        // panel's, not the theme's.
        let PanelDropDown = DropDown {
            width: Fill
            height: 20
            min_height: 0
            align: Align{x: 0.0 y: 0.5}
            padding: Inset{left: 6 right: 18 top: 2 bottom: 2}
            margin: Inset{left: 0 right: 0 top: 0 bottom: 0}
            popup_menu: PanelPopupMenu{}
            draw_text +: {
                color: fab.color_text
                color_hover: fab.color_text_active
                color_focus: fab.color_text_active
                color_down: fab.color_text_active
                color_disabled: #x8c8c8c
                text_style: PanelFont{}
            }
            draw_icon +: { color: fab.color_text }
            draw_bg +: {
                border_size: 1.0
                border_radius: 2.0
                color_dither: 0.0
                color: fab.color_input
                color_hover: fab.color_input_hover
                color_focus: fab.color_input
                color_down: fab.color_input_active
                color_disabled: #x1d1d1d
                // Flat in every state, like PanelButton: with a
                // gradient stop left on, the other states' -1 stops
                // are mixed in as colours.
                color_2: vec4(-1.0, -1.0, -1.0, -1.0)
                color_2_hover: vec4(-1.0, -1.0, -1.0, -1.0)
                color_2_focus: vec4(-1.0, -1.0, -1.0, -1.0)
                color_2_down: vec4(-1.0, -1.0, -1.0, -1.0)
                color_2_disabled: vec4(-1.0, -1.0, -1.0, -1.0)
                border_color: fab.color_border
                border_color_hover: fab.color_border_light
                border_color_focus: fab.color_focus_ring
                border_color_down: fab.color_border
                border_color_disabled: fab.color_border
                border_color_2: vec4(-1.0, -1.0, -1.0, -1.0)
                border_color_2_hover: vec4(-1.0, -1.0, -1.0, -1.0)
                border_color_2_focus: vec4(-1.0, -1.0, -1.0, -1.0)
                border_color_2_down: vec4(-1.0, -1.0, -1.0, -1.0)
                border_color_2_disabled: vec4(-1.0, -1.0, -1.0, -1.0)
                arrow_color: #xb4b4b4
                arrow_color_hover: fab.color_text_active
                arrow_color_focus: fab.color_text_active
                arrow_color_down: fab.color_text_active
                arrow_color_disabled: #x8c8c8c
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
                            .mix(self.color_down, self.down * self.hover)
                            .mix(self.color_disabled, self.disabled)
                    )
                    sdf.stroke(
                        self.border_color
                            .mix(self.border_color_focus, self.focus)
                            .mix(self.border_color_hover, self.hover)
                            .mix(self.border_color_down, self.down * self.hover)
                            .mix(self.border_color_disabled, self.disabled)
                        self.border_size
                    )
                    // The arrow goes on AFTER the face, over it.
                    // Laid first, the face's fill painted straight
                    // over it and no picker in the panel showed one.
                    let c = vec2(self.rect_size.x - 10.0, self.rect_size.y * 0.5)
                    let sz = 2.5
                    sdf.move_to(c.x - sz, c.y - sz + 1.0)
                    sdf.line_to(c.x + sz, c.y - sz + 1.0)
                    sdf.line_to(c.x, c.y + sz * 0.25 + 1.0)
                    sdf.close_path()
                    sdf.fill(
                        self.arrow_color
                            .mix(self.arrow_color_focus, self.focus)
                            .mix(self.arrow_color_hover, self.hover)
                            .mix(self.arrow_color_down, self.down)
                            .mix(self.arrow_color_disabled, self.disabled)
                    )
                    return sdf.result
                }
            }
        }
        let PanelLabelSmall = Label{width: Fit height: Fit padding: 0
            draw_text +: {ink_centered: true color: #xb4b4b4 text_style: PanelFont{font_size: 7.5}}}
        let ThemeButton = PanelButton{width: Fit height: 20 min_height: 0
            padding: Inset{left: 6 right: 6 top: 2 bottom: 2}
            draw_text +: {text_style: PanelFont{font_size: 7.5}}}
        let TbColorT = View {
            width: Fill
            height: Fit
            flow: Down
            spacing: 1
            clip_x: false
            tb_color_name := PanelLabelSmall {
                width: Fit
                text: ""
                max_lines: 1
            }
            // Carried onto another square, it trades colours with
            // that one; into a gap, it moves there and the others
            // close up. The names are the roles and stay put.
            tb_color := FabColorPick {
                width: Fill
                height: 18
                draggable: true
                // A theme colour has no transparency: the A row is
                // one more thing to read past on the way to the
                // three that matter, and an alpha that slipped in
                // from a drag or a pasted hex would ride into the
                // built theme where nothing can see it.
                with_alpha: false
                popover +: {hex_row +: {pick +: {visible: false}}}
            }
        }
        let TbLockT = View {
            width: Fit
            height: Fit
            flow: Right
            spacing: 0
            align: Align{x: 0.5 y: 0.5}
            margin: Inset{left: 0 right: 2 top: 0 bottom: 0}
            lock_open := PanelButton {
                width: 14
                height: 14
                padding: Inset{left: 0 right: 0 top: 0 bottom: 0}
                margin: Inset{left: 0 right: 0 top: 0 bottom: 0}
                spacing: 0
                align: Align{x: 0.5 y: 0.5}
                text: ""
                icon_walk: Walk{width: 10 height: 10}
                draw_icon +: {
                    color: #x8a8a8a
                    svg: crate_resource("makepad_widgets:resources/icons/icon_lock_open.svg")
                }
            }
            lock_shut := PanelButton {
                width: 14
                height: 14
                padding: Inset{left: 0 right: 0 top: 0 bottom: 0}
                margin: Inset{left: 0 right: 0 top: 0 bottom: 0}
                spacing: 0
                align: Align{x: 0.5 y: 0.5}
                visible: false
                text: ""
                icon_walk: Walk{width: 10 height: 10}
                draw_icon +: {
                    color: #xffffff
                    svg: crate_resource("makepad_widgets:resources/icons/icon_lock_shut.svg")
                }
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
        View{visible: false width: Fill height: Fill{weight: 75.0} flow: Down spacing: 3
            theme_head_scroll := ScrollYView{width: Fill height: Fit{max: FitBound.Rel{base: Base.Full factor: 0.5}}
                flow: Down spacing: 3 padding: Inset{left: 4 right: 4 top: 0 bottom: 3}
                theme_pick_row := View{width: Fill height: Fit flow: Right{wrap: true} spacing: 3 align: Align{y: 0.5}
                    theme_pick := PanelDropDown{}
                    theme_bake := ThemeButton{text: "Bake to source"}
                    eq_fold := ThemeButton{text: "+ mix" visible: false}
                    tb_fold := ThemeButton{text: "+ build" visible: false}
                }
                eq_body := View{visible: false width: Fill height: Fit flow: Down spacing: 3
                    eq_appearance_row := View{width: Fill height: Fit flow: Right{wrap: true} spacing: 3
                        eq_dark := ThemeButton{text: "dark"}
                        eq_light := ThemeButton{text: "light"}
                        eq_absolute := ThemeButton{text: "absolute" margin: Inset{left: 6}}
                        eq_relative := ThemeButton{text: "relative"}
                        eq_random := ThemeButton{text: "surprise"}
                    }
                    eq_controls := EqRows{width: Fill height: Fit flow: Down
                        eq_rows := FlatList{width: Fill height: Fit flow: Down spacing: 1
                            scroll_bars +: {show_scroll_y: false}
                            Row := FabSlider{height: fab.row_height_sm}
                        }
                    }
                    eq_read := PanelLabelSmall{width: Fill max_lines: 2 flow: Right{wrap: true}}
                }
                tb_body := View {
                    visible: false
                    width: Fill
                    height: Fit
                    flow: Down
                    spacing: 3
                    padding: Inset{left: 0 right: 0 top: 2 bottom: 0}
                    tb_count_row := View {
                        width: Fill
                        height: Fit
                        flow: Right{wrap: true}
                        spacing: 2
                        align: Align{x: 0.0 y: 0.5}
                        tb_count_name := PanelLabelSmall {
                            width: Fit
                            margin: Inset{right: 4}
                            text: "Colours"
                            max_lines: 1
                        }
                        tb_count_1 := ThemeButton {
                            width: Fit
                            height: 20
                            padding: Inset{left: 6 right: 6 top: 2 bottom: 2}
                            text: "1"
                            draw_text +: { text_style +: { font_size: 7.5 } }
                        }
                        tb_count_2 := ThemeButton {
                            width: Fit
                            height: 20
                            padding: Inset{left: 6 right: 6 top: 2 bottom: 2}
                            text: "2"
                            draw_text +: { text_style +: { font_size: 7.5 } }
                        }
                        tb_count_3 := ThemeButton {
                            width: Fit
                            height: 20
                            padding: Inset{left: 6 right: 6 top: 2 bottom: 2}
                            text: "3"
                            draw_text +: { text_style +: { font_size: 7.5 } }
                        }
                        tb_count_4 := ThemeButton {
                            width: Fit
                            height: 20
                            padding: Inset{left: 6 right: 6 top: 2 bottom: 2}
                            text: "4"
                            draw_text +: { text_style +: { font_size: 7.5 } }
                        }
                        tb_adjust_name := PanelLabelSmall {
                            width: Fit
                            margin: Inset{left: 4 right: 2}
                            text: "Adjust"
                            max_lines: 1
                        }
                        tb_adjust := PanelCheckBox { width: Fit height: Fit text: "" }
                        tb_roll_name := PanelLabelSmall {
                            width: Fit
                            margin: Inset{left: 4 right: 2}
                            text: "Roll"
                            max_lines: 1
                        }
                        tb_random := ThemeButton {
                            width: 18
                            height: 18
                            padding: Inset{left: 0 right: 0 top: 0 bottom: 0}
                            margin: Inset{left: 0 right: 0 top: 0 bottom: 0}
                            spacing: 0
                            align: Align{x: 0.5 y: 0.5}
                            text: ""
                            icon_walk: Walk{width: 12 height: 12}
                            draw_icon +: {
                                color: #xd8d8d8
                                svg: crate_resource("makepad_widgets:resources/icons/icon_dice.svg")
                            }
                        }
                    }
                    tb_seed_row := View {
                        width: Fill
                        height: Fit
                        flow: Right
                        spacing: 3
                        align: Align{x: 0.0 y: 1.0}
                        lock := TbLockT {}
                        tb_color_0 := TbColorT {}
                        tb_color_1 := TbColorT {}
                        tb_color_2 := TbColorT {}
                        tb_color_3 := TbColorT {}
                    }
                    tb_strip := View {
                        width: Fill
                        height: Fit
                        flow: Right
                        spacing: 2
                        align: Align{x: 0.0 y: 0.5}
                        tb_strip_prev := ThemeButton {
                            width: 18
                            height: 22
                            padding: Inset{left: 4 right: 4 top: 1 bottom: 3}
                            margin: Inset{left: 0 right: 0 top: 0 bottom: 0}
                            align: Align{x: 0.5 y: 0.5}
                            text: "\u{2039}"
                            draw_text +: { text_style +: { font_size: 10.0 } }
                        }
                        tb_carousel := FabPaletteCarousel {
                            width: Fill
                            height: 88
                        }
                        tb_strip_next := ThemeButton {
                            width: 18
                            height: 22
                            padding: Inset{left: 4 right: 4 top: 1 bottom: 3}
                            margin: Inset{left: 0 right: 0 top: 0 bottom: 0}
                            align: Align{x: 0.5 y: 0.5}
                            text: "\u{203a}"
                            draw_text +: { text_style +: { font_size: 10.0 } }
                        }
                    }
                    tb_rows := View {
                        width: Fill
                        height: Fit
                        flow: Down
                        spacing: 1
                        PanelLabelSmall {
                            width: Fill
                            margin: Inset{top: 3}
                            text: "Surface"
                        }
                        tb_saturation_row := View {
                            width: Fill
                            height: Fit
                            flow: Right
                            spacing: 0
                            align: Align{x: 0.0 y: 0.5}
                            lock := TbLockT {}
                            tb_saturation := FabSlider {
                                height: fab.row_height_sm
                                label: "Saturation"
                                min: 0.0
                                max: 100.0
                                step: 1.0
                            }
                        }
                        tb_lightness_row := View {
                            width: Fill
                            height: Fit
                            flow: Right
                            spacing: 0
                            align: Align{x: 0.0 y: 0.5}
                            lock := TbLockT {}
                            tb_lightness := FabSlider {
                                height: fab.row_height_sm
                                label: "Lightness"
                                min: 0.0
                                max: 100.0
                                step: 1.0
                            }
                        }
                        PanelLabelSmall {
                            width: Fill
                            margin: Inset{top: 3}
                            text: "Text"
                        }
                        tb_font_size_row := View {
                            width: Fill
                            height: Fit
                            flow: Right
                            spacing: 0
                            align: Align{x: 0.0 y: 0.5}
                            lock := TbLockT {}
                            tb_font_size := FabSlider {
                                height: fab.row_height_sm
                                label: "Text size"
                                min: 6.0
                                max: 30.0
                                step: 0.5
                                big_step: 2.0
                                precision: 1
                                unit: ""
                            }
                        }
                        tb_font_contrast_row := View {
                            width: Fill
                            height: Fit
                            flow: Right
                            spacing: 0
                            align: Align{x: 0.0 y: 0.5}
                            lock := TbLockT {}
                            tb_font_contrast := FabSlider {
                                height: fab.row_height_sm
                                label: "Text variation"
                                min: 0.0
                                max: 8.0
                                step: 0.5
                                big_step: 1.0
                                precision: 1
                                unit: ""
                            }
                        }
                        tb_text_contrast_row := View {
                            width: Fill
                            height: Fit
                            flow: Right
                            spacing: 0
                            align: Align{x: 0.0 y: 0.5}
                            lock := TbLockT {}
                            tb_text_contrast := FabSlider {
                                height: fab.row_height_sm
                                label: "Text contrast"
                                min: 4.5
                                max: 21.0
                                step: 0.1
                                big_step: 1.0
                                precision: 1
                                unit: ""
                            }
                        }
                        tb_text_color_row := View {
                            width: Fill
                            height: fab.row_height_sm
                            flow: Right
                            align: Align{x: 0.0 y: 0.5}
                            padding: Inset{left: 0 right: 6 top: 0 bottom: 0}
                            lock := TbLockT {}
                            tb_text_color_name := PanelLabelSmall {
                                width: fab.prop_label_width
                                margin: Inset{left: 8 right: 0 top: 0 bottom: 0}
                                text: "Text colour"
                                max_lines: 1
                            }
                            tb_text_color := PanelDropDown {
                                width: Fill
                                height: 18
                                padding: Inset{left: 5 right: 16 top: 1 bottom: 1}
                                popup_menu: PanelPopupMenu{width: 72.}
                            }
                        }
                        tb_text_tint_row := View {
                            width: Fill
                            height: Fit
                            flow: Right
                            spacing: 0
                            align: Align{x: 0.0 y: 0.5}
                            lock := TbLockT {}
                            tb_text_tint := FabSlider {
                                height: fab.row_height_sm
                                label: "Text tint"
                                min: 0.0
                                max: 100.0
                                step: 1.0
                            }
                        }
                        PanelLabelSmall {
                            width: Fill
                            margin: Inset{top: 3}
                            text: "Shape"
                        }
                        tb_spacing_row := View {
                            width: Fill
                            height: Fit
                            flow: Right
                            spacing: 0
                            align: Align{x: 0.0 y: 0.5}
                            lock := TbLockT {}
                            tb_spacing := FabSlider {
                                height: fab.row_height_sm
                                label: "Spacing"
                                min: 2.0
                                max: 20.0
                                step: 0.5
                                big_step: 2.0
                                precision: 1
                                unit: ""
                            }
                        }
                        tb_roundness_row := View {
                            width: Fill
                            height: Fit
                            flow: Right
                            spacing: 0
                            align: Align{x: 0.0 y: 0.5}
                            lock := TbLockT {}
                            tb_roundness := FabSlider {
                                height: fab.row_height_sm
                                label: "Roundness"
                                min: 0.0
                                max: 20.0
                                step: 0.5
                                big_step: 2.0
                                precision: 1
                                unit: ""
                            }
                        }
                    }
                    tb_read := PanelLabelSmall {
                        width: Fill
                        text: ""
                        max_lines: 2
                    }
                }
                theme_save_row := View{width: Fill height: Fit flow: Right spacing: 3 align: Align{y: 0.5}
                    theme_name := PanelInput{width: Fill height: 20 empty_text: "name this theme"
                        padding: Inset{left: 6 right: 6 top: 2 bottom: 2}
                        draw_bg +: {border_radius: 2.0} draw_text +: {text_style: PanelFont{font_size: 8.0}}}
                    theme_save := ThemeButton{text: "save as"}
                    theme_delete := ThemeButton{visible: false text: "delete"}
                }
                theme_msg := PanelLabelSmall{visible: false width: Fill max_lines: 2 flow: Right{wrap: true}}
            }
            theme_filter := FabSearch{margin: Inset{left: 4 right: 4} input +: {empty_text: "Filter tokens"}}
            theme_status := BodyNote{visible: false width: Fill margin: Inset{left: 4 right: 4} flow: Right{wrap: true}}
        }
    })
}

impl ThemeTab {
    pub fn header_action(&mut self, cx: &mut Cx, root: &WidgetRef, actions: &Actions) -> Option<ThemeAction> {
        if root.button(cx, ids!(theme_bake)).clicked(actions) { return Some(ThemeAction::Bake) }
        if root.button(cx, ids!(tb_fold)).clicked(actions) { return Some(ThemeAction::Build(BuildAction::Fold)) }
        if self.tb_open {
            if let Some(action) = self.builder_action(cx, root, actions) { return Some(ThemeAction::Build(action)) }
        }
        if root.button(cx, ids!(eq_fold)).clicked(actions) { return Some(ThemeAction::Mix(MixAction::Fold)) }
        if root.widget(cx, ids!(eq_body)).visible() {
            for (id, action) in [(ids!(eq_dark), MixAction::Appearance(Appearance::Dark)),
                (ids!(eq_light), MixAction::Appearance(Appearance::Light)),
                (ids!(eq_absolute), MixAction::Mode(WeightMode::Absolute)),
                (ids!(eq_relative), MixAction::Mode(WeightMode::Relative)), (ids!(eq_random), MixAction::Surprise)] {
                if root.button(cx, id).clicked(actions) { return Some(ThemeAction::Mix(action)) }
            }
            if let Some(action) = root.widget(cx, ids!(eq_controls)).borrow::<EqRows>().and_then(|r| r.action(actions)) {
                return Some(ThemeAction::Mix(action))
            }
        }
        let name = root.text_input(cx, ids!(theme_name));
        if root.button(cx, ids!(theme_save)).clicked(actions) { return Some(ThemeAction::SaveAs(name.text())) }
        if root.button(cx, ids!(theme_delete)).clicked(actions) { return Some(ThemeAction::DeleteSaved) }
        if let Some(i) = root.drop_down(cx, ids!(theme_pick)).selected(actions) { return Some(ThemeAction::PickChoice(i)) }
        if let Some(text) = name.changed(actions) { return Some(ThemeAction::Name(text)) }
        let filter = root.text_input(cx, ids!(theme_filter.input));
        if filter.escaped(actions) {
            filter.set_text(cx, "");
            return Some(ThemeAction::Filter(String::new()))
        }
        filter.changed(actions).map(ThemeAction::Filter)
    }

    pub fn sync_weights(&self, cx: &mut Cx, root: &WidgetRef) {
        if let (Some(lab), Some(mut rows)) = (&self.eq_lab, root.widget(cx, ids!(eq_controls)).borrow_mut::<EqRows>()) {
            rows.bind(cx, lab);
        }
    }

    pub fn sync_mixer(&self, cx: &mut Cx, root: &WidgetRef) {
        let fold = root.button(cx, ids!(eq_fold));
        fold.set_visible(cx, self.snapshot.is_some());
        fold.set_text(cx, if self.eq_open { "- mix" } else { "+ mix" });
        root.widget(cx, ids!(eq_body)).set_visible(cx, self.eq_open);
        root.label(cx, ids!(eq_read)).set_text(cx, &self.eq_reading);
        if !self.eq_open { return }
        root.widget(cx, ids!(eq_appearance_row)).set_visible(cx, self.eq_lab.is_some());
        root.widget(cx, ids!(eq_controls)).set_visible(cx, self.eq_lab.is_some());
        if let Some(lab) = &self.eq_lab {
            for (id, selected) in [(ids!(eq_dark), lab.appearance() == Appearance::Dark),
                (ids!(eq_light), lab.appearance() == Appearance::Light), (ids!(eq_absolute), lab.mode() == WeightMode::Absolute),
                (ids!(eq_relative), lab.mode() == WeightMode::Relative)] {
                crate::tweak::panel::set_tab_fill(cx, root.widget(cx, id), selected);
            }
            self.sync_weights(cx, root);
        }
    }

    pub fn sync_header(&mut self, cx: &mut Cx, root: &WidgetRef) {
        self.sync_mixer(cx, root);
        self.sync_builder(cx, root);
        let pick = root.drop_down(cx, ids!(theme_pick));
        let entries = self.choice_list();
        if self.entries != entries {
            pick.set_labels(cx, entries.iter().map(|e| e.label().into()).collect());
            self.entries = entries;
        }
        let index = self.selected_choice_index();
        if pick.selected_item() != index { pick.set_selected_item(cx, index); }
        let saved = matches!(self.entries.get(index), Some(ThemeChoice::Saved(_)));
        if !saved { self.last_preset_idx = index; }
        root.widget(cx, ids!(theme_delete)).set_visible(cx, saved);
        let name = root.text_input(cx, ids!(theme_name));
        if name.text() != self.name_text { name.set_text(cx, &self.name_text); }
        for (id, text) in [(ids!(theme_msg), &self.msg), (ids!(theme_status), &self.status)] {
            let label = root.label(cx, id);
            label.set_visible(cx, !text.is_empty());
            label.set_text(cx, text);
        }
    }
}

impl ThemeTab {
    pub fn close_build_popovers(&mut self, cx: &mut Cx, root: &WidgetRef) {
        if !self.tb_open { return }
        let row = root.widget(cx, ids!(tb_seed_row));
        for id in TB_COLOR_IDS {
            if let Some(mut pick) = row.child(id).child(live_id!(tb_color)).borrow_mut::<FabColorPick>() {
                pick.set_drop_target(cx, false);
                pick.set_insert_bar(cx, None);
                if pick.is_open() { cx.capture_actions(|cx| pick.close_popover(cx, false)); }
            }
        }
        self.tb_color_carried = None;
        self.tb_color_opened = None;
        self.tb_color_target = None;
        self.tb_color_insert = None;
    }

    fn mark_build_drop(&mut self, cx: &mut Cx, body: &WidgetRef, from: usize, at: Option<DVec2>) -> Option<TbDrop> {
        let shown = tb_shown_slots(self.tb_state.as_ref()?.params().color_count);
        let row = body.child(live_id!(tb_seed_row));
        let controls = TB_COLOR_IDS.map(|id| row.child(id).child(live_id!(tb_color)));
        let squares: Vec<_> = shown.iter().map(|slot| controls[*slot].area().rect(cx)).collect();
        let top = shown.iter().map(|slot| row.child(TB_COLOR_IDS[*slot]).child(live_id!(tb_color_name)).area().rect(cx))
            .filter(|r| r.size.y > 0.).map(|r| r.pos.y).fold(squares[0].pos.y, f64::min);
        let rect = row.area().rect(cx);
        let drop = at.and_then(|at| tb_drop_zone(&squares, top, rect.pos.x + rect.size.x,
            shown.iter().position(|s| *s == from)?, at));
        self.tb_color_target = if let Some(TbDrop::Swap(i)) = drop { Some(shown[i]) } else { None };
        self.tb_color_insert = if let Some(TbDrop::Insert(i)) = drop { Some(i) } else { None };
        let bar = self.tb_color_insert.map(|i| tb_insert_bar(&squares, i));
        for (i, widget) in controls.iter().enumerate() {
            if let Some(mut pick) = widget.borrow_mut::<FabColorPick>() {
                pick.set_drop_target(cx, self.tb_color_target == Some(i));
                pick.set_insert_bar(cx, if self.tb_color_carried == Some(i) { bar } else { None });
            }
        }
        drop
    }

    fn builder_action(&mut self, cx: &mut Cx, root: &WidgetRef, actions: &Actions) -> Option<BuildAction> {
        let body = root.widget(cx, ids!(tb_body));
        if self.tb_reanchor_pending { return None }
        for (i, id) in TB_COUNT_IDS.iter().enumerate() {
            if body.child(live_id!(tb_count_row)).child(*id).as_button().clicked(actions) { return Some(BuildAction::Count(COLOR_COUNTS[i])) }
        }
        if root.button(cx, ids!(tb_random)).clicked(actions) { return Some(BuildAction::Roll) }
        if let Some(on) = root.check_box(cx, ids!(tb_adjust)).changed(actions) { return Some(BuildAction::Adjust(on)) }
        if let Some(i) = root.drop_down(cx, ids!(tb_text_color)).selected(actions) { return Some(BuildAction::Text(i)) }
        for (i, lock) in BuildLock::ALL.iter().enumerate() {
            if lock.lock_in(&body).child(TB_LOCK_FACES[usize::from(self.tb_locks[i])]).as_button().clicked(actions) {
                return Some(BuildAction::Lock(i))
            }
        }
        for row in BuildRow::ALL {
            let slider = body.child(live_id!(tb_rows)).child(row.line()).child(row.slot()).as_fab_slider();
            if slider.was_reset(actions) { return Some(BuildAction::Reset(row)) }
            if let Some(value) = slider.ended(actions) { return Some(BuildAction::Slider { row, value, ended: true }) }
            if let Some(value) = slider.changed(actions) { return Some(BuildAction::Slider { row, value, ended: false }) }
        }
        let strip = body.child(live_id!(tb_strip));
        let carousel = strip.child(live_id!(tb_carousel));
        if let Some(row) = carousel.borrow::<FabPaletteCarousel>() { sync_arrows(cx, &strip, &row); }
        for (path, forward) in [(ids!(tb_strip_prev), false), (ids!(tb_strip_next), true)] {
            if root.button(cx, path).clicked(actions) {
                if let Some(mut row) = carousel.borrow_mut::<FabPaletteCarousel>() {
                    row.scroll_page(cx, forward);
                    sync_arrows(cx, &strip, &row);
                }
                return None
            }
        }
        let colors = TB_COLOR_IDS.map(|id| body.child(live_id!(tb_seed_row)).child(id).child(live_id!(tb_color)).widget_uid());
        let mut color_action = None;
        for a in actions.iter().filter_map(|a| a.as_widget_action()) {
            if a.widget_uid == carousel.widget_uid() {
                if let FabPaletteCarouselAction::Pick(i) = a.cast() { return Some(BuildAction::Chip(i)) }
            }
            let Some(slot) = colors.iter().position(|id| *id == a.widget_uid) else { continue };
            let action = a.cast::<FabColorPickAction>();
            match action {
                FabColorPickAction::Ended(v) | FabColorPickAction::Changed(v) => {
                    let ended = matches!(action, FabColorPickAction::Ended(_));
                    let rgba = u32::from_be_bytes([v.x, v.y, v.z, 1.0].map(|n| (255. * n.clamp(0., 1.)).round() as u8));
                    if ended { return Some(BuildAction::Color { slot, rgba, ended }) }
                    color_action = Some(BuildAction::Color { slot, rgba, ended });
                }
                FabColorPickAction::Opened => color_action = Some(BuildAction::Touch(slot)),
                FabColorPickAction::DragStarted => {
                    self.tb_color_carried = Some(slot);
                    self.tb_color_opened = None;
                    self.mark_build_drop(cx, &body, slot, None);
                }
                FabColorPickAction::DragMoved(at) => { self.mark_build_drop(cx, &body, slot, Some(at)); }
                FabColorPickAction::DragDropped(at) => {
                    let drop = self.mark_build_drop(cx, &body, slot, Some(at));
                    self.tb_color_carried = None;
                    self.mark_build_drop(cx, &body, slot, None);
                    color_action = drop.map(|d| BuildAction::Drop(slot, d));
                }
                FabColorPickAction::DragCancelled => {
                    self.tb_color_carried = None;
                    self.mark_build_drop(cx, &body, slot, None);
                }
                _ => {}
            }
        }
        color_action
    }

    pub fn sync_build_controls(&mut self, cx: &mut Cx, root: &WidgetRef) {
        let Some(state) = &self.tb_state else { return };
        let params = state.params();
        let body = root.widget(cx, ids!(tb_body));
        let colors = params.palette();
        for (slot, id) in TB_COLOR_IDS.iter().enumerate() {
            let column = body.child(live_id!(tb_seed_row)).child(*id);
            let visible = SeedSlot::ALL[slot].chosen_at(params.color_count);
            column.set_visible(cx, visible);
            let label = column.child(live_id!(tb_color_name));
            label.set_text(cx, ["Prim", "Sec", "Tert", "Surf"][slot]);
            if let Some(mut name) = label.borrow_mut::<Label>() {
                let shade = if params.color_count > 1 && self.tb_seed_slot.index() == slot { 0.706 } else { 0.549 };
                name.draw_text.color = vec4(shade, shade, shade, 1.);
            }
            if let Some(mut pick) = column.child(live_id!(tb_color)).borrow_mut::<FabColorPick>() {
                if pick.is_open() && (!visible || self.tb_color_opened.is_some_and(|i| i != slot)) {
                    cx.capture_actions(|cx| pick.close_popover(cx, false));
                }
                if !pick.is_open() { pick.set_rgba(cx, colors[slot].to_be_bytes().map(|c| f32::from(c) / 255.)); }
            }
        }
        for row in BuildRow::ALL {
            let slider = body.child(live_id!(tb_rows)).child(row.line()).child(row.slot()).as_fab_slider();
            if let Some((min, max)) = row.range(&params) { slider.set_range(cx, min, max); }
            slider.set_value(cx, row.shown(&params));
            slider.set_enabled(cx, row.enabled(&params));
        }
    }

    pub fn sync_builder(&mut self, cx: &mut Cx, root: &WidgetRef) {
        let fold = root.button(cx, ids!(tb_fold));
        fold.set_visible(cx, self.snapshot.is_some());
        fold.set_text(cx, if self.tb_open { "- build" } else { "+ build" });
        let body = root.widget(cx, ids!(tb_body));
        body.set_visible(cx, self.tb_open && self.tb_state.is_some());
        body.child(live_id!(tb_read)).set_text(cx, &self.tb_reading);
        if !self.tb_open { return }
        let Some(state) = &self.tb_state else { return };
        let params = state.params();
        let count = params.color_count;
        self.sync_build_controls(cx, root);
        for (i, id) in TB_COUNT_IDS.iter().enumerate() {
            crate::tweak::panel::set_tab_fill(cx, body.child(live_id!(tb_count_row)).child(*id), count == COLOR_COUNTS[i]);
        }
        root.check_box(cx, ids!(tb_adjust)).set_active(cx, self.tb_adjust, Animate::No);
        let text = root.drop_down(cx, ids!(tb_text_color));
        if self.tb_text_labels_for != count { text.set_labels(cx, tb_text_labels(count)); self.tb_text_labels_for = count; }
        text.set_selected_item(cx, tb_text_entry_at(params.text_color, count));
        for (i, lock) in BuildLock::ALL.iter().enumerate() {
            for (at, face) in TB_LOCK_FACES.iter().enumerate() {
                let button = lock.lock_in(&body).child(*face);
                button.set_visible(cx, (at == 1) == self.tb_locks[i]);
            }
        }
        let strip = body.child(live_id!(tb_strip));
        strip.set_visible(cx, count > 1);
        if let Some(mut row) = strip.child(live_id!(tb_carousel)).borrow_mut::<FabPaletteCarousel>() {
            row.set_bands(cx, count);
            if std::mem::take(&mut self.tb_strip_rewind) { row.rewind(cx); }
            if std::mem::take(&mut self.tb_chips_changed) {
                let chips: Vec<_> = self.tb_suggestions.iter().map(|offer| {
                    let shown = offer.shown();
                    std::array::from_fn(|i| {
                        Vec4f::from_u32(shown.get(i).copied().unwrap_or(0xff))
                    })
                }).collect();
                let chosen = self.tb_chosen.as_ref().filter(|s| s.worn_by(&params))
                    .and_then(|s| self.tb_suggestions.iter().position(|offer| offer == s));
                row.set_chips(cx, &chips, chosen);
            }
            sync_arrows(cx, &strip, &row);
        }
    }
}


fn sync_arrows(cx: &mut Cx, strip: &WidgetRef, row: &FabPaletteCarousel) {
    for (id, enabled) in [(live_id!(tb_strip_prev), !row.at_start()), (live_id!(tb_strip_next), !row.at_end())] {
        let button = strip.child(id);
        button.as_button().set_enabled(cx, enabled);
        if button.disabled(cx) == enabled { button.set_disabled(cx, !enabled); }
    }
}
