//! Original Stage program mix and the audio-effects rack.
//! The host owns the graph. Only DJ A, DJ B and the combined Ironfish bus
//! have gain endpoints; absent sources and dynamics stay disabled. The rack
//! draws the host's effects ([`MixRackRow`]) and reports what is asked of
//! it ([`MixSurfaceAction`]); the host plays them (product/mix_rack.rs).
//! Geometry and materials are extracted from donor main.rs / fx_rack.rs.

use makepad_widgets::*;

script_mod! {
    use mod.prelude.widgets_internal.*
    use mod.widgets.*
    let stage = mod.stage

    let Tick = StageTick{}
    let PanelLabel = Label{draw_text.color: stage.secondary draw_text.text_style.font_size: 10}
    let ChromeButton = StageButton{
        height: 22 margin: 0 padding: 0 align: Align{x: 0.5 y: 0.5}
        draw_bg +: {color_disabled: stage.ghost_face border_color_disabled: stage.line_faint}
        draw_text +: {color_disabled: stage.dim_lo}
    }
    let ApcFader = StageFader{margin: 0 label_walk: Walk{width: 0 height: 0 margin: 0}}
    let ApcHSlider = StageHSlider{margin: 0 label_walk: Walk{width: 0 height: 0 margin: 0}}
    let UnavailableButton = ChromeButton{animator +: {disabled +: {default: @on}}}
    let UnavailableFader = ApcFader{
        default: 0
        animator +: {disabled +: {default: @on}}
        draw_bg +: {fill_color: stage.border cap_color: stage.dim cap_shadow: stage.dim_lo}
    }
    let UnavailableHSlider = ApcHSlider{draw_bg.inert: 1 animator +: {disabled +: {default: @on}}}
    let UnavailableKnob = StageKnob{
        margin: 0 label_walk: Walk{width: 0 height: 0 margin: 0}
        draw_bg.inert: 1 animator +: {disabled +: {default: @on}}
    }

    let PillButton = Button{
        grab_key_focus: false
        height: 22 margin: 0 padding: Inset{left: 6 right: 6 top: 0 bottom: 0}
        align: Align{x: 0.5 y: 0.5}
        draw_bg +: {
            color: stage.raised
            color_hover: stage.control_top
            color_down: stage.row
            border_color: stage.border
            border_radius: 2.5
            border_size: 1.0
        }
        draw_text +: {
            color: stage.text_soft
            color_hover: stage.accent
            text_style: theme.font_bold{font_size: 10}
        }
    }

    let StemChip = PillButton{
        width: 12
        height: Fit
        padding: Inset{left: 1.0, right: 1.0, top: 2.0, bottom: 2.0}
        margin: Inset{left: 0.0, right: 0.0, top: 0.0, bottom: 0.0}
        draw_text +: {text_style: theme.font_bold{font_size: 9}}
    }

    let FaderCol = View{
        width: 44
        height: Fit
        flow: Down
        spacing: 3
        align: Align{x: 0.5, y: 0.0}
    }

    let FxRoute = View{
        width: 256
        height: Fit
        flow: Right
        spacing: 10
        align: Align{x: 0.0, y: 0.5}
    }

    let FxName = View{
        width: 92
        height: Fit
        flow: Down
        spacing: 2
        align: Align{x: 0.0, y: 0.5}
    }

    let FxSpare = View{
        width: Fill{min: 120. max: 420.}
        height: Fit
        align: Align{x: 0.0, y: 0.5}
    }

    let FxParam = FxSpare{
        flow: Down
        spacing: 2
    }

    let FxParamLabel = Label{
        width: Fill
        max_lines: 1
        text_overflow: TextOverflow.Ellipsis
        align: Align{x: 0.0, y: 0.5}
        padding: Inset{top: 0., bottom: 0.}
        draw_text +: {
            color: theme.color_label_outer
            text_style: theme.font_regular{font_size: theme.font_size_p}
        }
    }

    let FxParamValue = Label{
        width: Fit
        align: Align{x: 1.0, y: 0.5}
        padding: Inset{top: 0., bottom: 0.}
        draw_text +: {
            color: theme.color_text_val
            text_style: theme.font_regular{font_size: theme.font_size_p}
        }
    }

    let FxParamHead = View{
        width: Fill
        height: Fit
        flow: Right
        spacing: 6
        align: Align{x: 0.0, y: 0.5}
        fx_label := FxParamLabel{text: ""}
        fx_value := FxParamValue{text: ""}
    }

    let FxSlider = Slider{
        width: Fill
        height: 22
        scroll_step: 0.025
        margin: 0.
        align: Align{x: 0.0, y: 0.5}
        label_walk: Walk{width: Fill height: Fit margin: 0.}
        text_input: TextInput{width: 0 height: 0}
        draw_bg +: {
            // The quad is the box: a style sheet grows its slider family's quad for shadows, and this pixel draws to the box, so under a sheet it would scale up.
            vertex: fn() {self.vertex_pos = self.clip_and_transform_vertex(self.rect_pos, self.rect_size)}
            label_size: 0.
            pixel: fn() {
                let sdf = Sdf2d.viewport(self.pos * self.rect_size)
                let track_w = self.rect_size.x - self.label_size
                let handle_sz = self.handle_size

                let offset_px = vec2(0., 0.)

                let offset_uv = vec2(
                offset_px.x / track_w
                offset_px.y / self.rect_size.y
                )

                let border_sz_uv = vec2(
                self.border_size / track_w
                self.border_size / self.rect_size.y
                )

                let sz_px = vec2(
                track_w
                self.rect_size.y - offset_px.y
                )

                let scale_factor_border = vec2(
                track_w / sz_px.x
                self.rect_size.y / sz_px.y
                )

                let sz_inner_px = vec2(
                track_w - self.border_size * 2.
                self.rect_size.y - self.border_size * 2. - offset_px.y
                )

                let scale_factor_fill = vec2(
                track_w / sz_inner_px.x
                self.rect_size.y / sz_inner_px.y
                )

                let slider_top = offset_px.y + self.border_size
                let slider_width = track_w - self.border_size * 2.
                let slider_bottom = self.rect_size.y - offset_px.y - self.border_size * 2.
                let slider_height = (self.rect_size.y - offset_px.y) * 0.5 - self.val_padding

                let mut color_fill = self.color
                let mut color_fill_hover = self.color_hover
                let mut color_fill_focus = self.color_focus
                let mut color_fill_drag = self.color_drag
                let mut color_fill_disabled = self.color_disabled

                let pos_y_adj = self.pos.y - offset_uv.y

                if self.color_2.x > -0.5 {
                    let dither = Math.random_2d(self.pos.xy) * 0.04 * self.color_dither
                    let gfx = self.pos.x * scale_factor_fill.x - border_sz_uv.x * 2. + dither
                    let gfy = pos_y_adj * scale_factor_fill.y - border_sz_uv.y * 2. + dither
                    let gradient_fill = vec2(gfx, gfy)
                    let dir = if self.gradient_fill_horizontal > 0.5 gradient_fill.x else gradient_fill.y
                    color_fill = mix(self.color, self.color_2, dir)
                    color_fill_hover = mix(self.color_hover, self.color_2_hover, dir)
                    color_fill_focus = mix(self.color_focus, self.color_2_focus, dir)
                    color_fill_drag = mix(self.color_drag, self.color_2_drag, dir)
                    color_fill_disabled = mix(self.color_disabled, self.color_2_disabled, dir)
                }

                let mut color_stroke = self.border_color
                let mut color_stroke_hover = self.border_color_hover
                let mut color_stroke_focus = self.border_color_focus
                let mut color_stroke_drag = self.border_color_drag
                let mut color_stroke_disabled = self.border_color_disabled

                let mut border_color_2 = self.border_color
                let mut border_color_2_hover = self.border_color_hover
                let mut border_color_2_focus = self.border_color_focus
                let mut border_color_2_drag = self.border_color_drag
                let mut border_color_2_disabled = self.border_color_disabled

                if self.border_color_2.x > -0.5 {
                    let dither = Math.random_2d(self.pos.xy) * 0.04 * self.color_dither
                    let gbx = self.pos.x * scale_factor_border.x + dither
                    let gby = pos_y_adj * scale_factor_border.y + dither
                    let gradient_border = vec2(gbx, gby)
                    let dir = if self.gradient_border_horizontal > 0.5 gradient_border.x else gradient_border.y
                    color_stroke = mix(self.border_color, self.border_color_2, dir)
                    color_stroke_hover = mix(self.border_color_hover, self.border_color_2_hover, dir)
                    color_stroke_focus = mix(self.border_color_focus, self.border_color_2_focus, dir)
                    color_stroke_drag = mix(self.border_color_drag, self.border_color_2_drag, dir)
                    color_stroke_disabled = mix(self.border_color_disabled, self.border_color_2_disabled, dir)
                    border_color_2 = self.border_color_2
                    border_color_2_hover = self.border_color_2_hover
                    border_color_2_focus = self.border_color_2_focus
                    border_color_2_drag = self.border_color_2_drag
                    border_color_2_disabled = self.border_color_2_disabled
                }

                let mut handle_fill = self.handle_color
                let mut handle_fill_hover = self.handle_color_hover
                let mut handle_fill_focus = self.handle_color_focus
                let mut handle_fill_drag = self.handle_color_drag
                let mut handle_fill_disabled = self.handle_color_disabled

                let mut handle_stroke = self.border_color
                let mut handle_stroke_hover = self.border_color_hover
                let mut handle_stroke_drag = self.border_color_drag
                let mut handle_stroke_disabled = self.border_color_disabled

                if self.handle_color_2.x > -0.5 {
                    let dither = Math.random_2d(self.pos.xy) * 0.04 * self.color_dither
                    let gfx = self.pos.x * scale_factor_fill.x - border_sz_uv.x * 2. + dither
                    let gfy = pos_y_adj * scale_factor_fill.y - border_sz_uv.y * 2. + dither
                    let gradient_fill = vec2(gfx, gfy)
                    let dir = if self.gradient_fill_horizontal > 0.5 gradient_fill.x else gradient_fill.y
                    handle_fill = mix(self.handle_color, self.handle_color_2, dir)
                    handle_fill_hover = mix(self.handle_color_hover, self.handle_color_2_hover, dir)
                    handle_fill_focus = mix(self.handle_color_focus, self.handle_color_2_focus, dir)
                    handle_fill_drag = mix(self.handle_color_drag, self.handle_color_2_drag, dir)
                    handle_fill_disabled = mix(self.handle_color_disabled, self.handle_color_2_disabled, dir)
                }

                if self.border_color_2.x > -0.5 {
                    let dither = Math.random_2d(self.pos.xy) * 0.04 * self.color_dither
                    let gbx = self.pos.x * scale_factor_border.x + dither
                    let gby = pos_y_adj * scale_factor_border.y + dither
                    let gradient_border = vec2(gbx, gby)
                    let dir = if self.gradient_border_horizontal > 0.5 gradient_border.x else gradient_border.y
                    handle_stroke = mix(self.border_color_2, self.border_color, dir)
                    handle_stroke_hover = mix(self.border_color_2_hover, self.border_color_hover, dir)
                    handle_stroke_drag = mix(self.border_color_2_drag, self.border_color_drag, dir)
                    handle_stroke_disabled = mix(self.border_color_2_disabled, self.border_color_disabled, dir)
                }

                sdf.box(
                self.border_size
                slider_top
                slider_width
                slider_bottom
                self.border_radius
                )

                let fill = color_fill
                .mix(color_fill_focus, self.focus)
                .mix(color_fill_hover.mix(color_fill_drag, self.drag), self.hover)
                .mix(color_fill_disabled, self.disabled)

                sdf.fill_keep(fill)

                let stroke = color_stroke
                .mix(color_stroke_focus.mix(color_stroke_hover.mix(color_stroke_drag, self.drag), self.hover), self.focus)
                .mix(color_stroke_disabled, self.disabled)

                sdf.stroke(stroke, self.border_size)

                let offset_sides = self.border_size + 6.
                sdf.rect(
                self.border_size + offset_sides
                offset_px.y + (self.rect_size.y - offset_px.y) * 0.5 - self.border_size - 0.5
                track_w - 2. * offset_sides - self.border_size * 2.
                self.border_size * 2. + 1.
                )

                sdf.fill(
                self.border_color
                .mix(self.border_color_focus.mix(self.border_color_hover.mix(self.border_color_drag, self.drag), self.hover), self.focus)
                .mix(self.border_color_disabled, self.disabled)
                )

                sdf.rect(
                self.border_size + offset_sides
                offset_px.y + (self.rect_size.y - offset_px.y) * 0.5
                track_w - 2. * offset_sides - self.border_size * 2. + 0.5
                self.border_size * 2.
                )

                sdf.fill(
                border_color_2
                .mix(border_color_2_hover, self.hover)
                .mix(border_color_2_hover.mix(border_color_2_drag, self.drag), self.hover)
                .mix(border_color_2_disabled, self.disabled)
                )

                let track_length = track_w - offset_sides * 4.
                let val_x = self.slide_pos * track_length + offset_sides * 2.
                let origin_x = self.origin_pos * track_length + offset_sides * 2.
                let offset_top = self.rect_size.y - (self.rect_size.y - offset_px.y) * 0.5
                let move_x = mix(offset_sides, origin_x, self.arc_origin)

                sdf.move_to(move_x, offset_top)
                sdf.line_to(val_x, offset_top)

                sdf.stroke(
                self.val_color
                .mix(self.val_color_hover, self.hover)
                .mix(self.val_color_focus.mix(self.val_color_hover.mix(self.val_color_drag, self.drag), self.hover), self.focus)
                .mix(self.val_color_disabled, self.disabled),
                slider_height
                )

                let ctrl_height = self.rect_size.y - offset_px.y
                let handle_x = self.slide_pos * (track_w - handle_sz - offset_sides) - 3
                let handle_padding = 1.5
                sdf.box(
                handle_x + offset_sides + self.border_size
                offset_px.y + self.border_size + handle_padding
                self.handle_size - self.border_size * 2.
                ctrl_height - self.border_size * 2. - handle_padding * 2.
                self.border_radius
                )

                let hfill = handle_fill
                .mix(handle_fill_hover, self.hover)
                .mix(handle_fill_focus.mix(handle_fill_hover.mix(handle_fill_drag, self.drag), self.hover), self.focus)
                .mix(handle_fill_disabled, self.disabled)

                sdf.fill_keep(hfill)

                let hstroke = handle_stroke
                .mix(handle_stroke_hover.mix(handle_stroke_drag, self.drag), self.hover)
                .mix(handle_stroke_disabled, self.disabled)

                sdf.stroke(hstroke, self.border_size)

                return sdf.result
            }
        }
    }

    let FxGrip = View{
        width: 18
        height: 22
        align: Align{x: 0.5, y: 0.5}
        Label{
            text: "≡"
            draw_text +: {
                color: stage.dim
                text_style: theme.font_bold{font_size: 11}
            }
        }
    }

    let UnavailablePill = PillButton{
        animator +: {disabled +: {default: @on}}
        draw_bg +: {color_disabled: stage.ghost_face border_color_disabled: stage.line_faint}
        draw_text +: {color_disabled: stage.dim_lo}
    }
    let IconButton = StageIconButton{margin: 0 animator +: {disabled +: {default: @on}}}
    let UnavailablePicker = DropDown{height: 22 labels: ["—"] animator +: {disabled +: {default: @on}}}
    // The scalar cell is the donor's label/value-above-track arrangement.
    // Stepped controls, learn targets and response graphs need a rack provider.
    let FxCell = FxParam{
        visible: false
        fx_head := FxParamHead{}
        fx_slider_cell := View{width: Fill height: Fit align: Align{x: 0 y: 0.5}
            fx_slider_tip := Tip{text: "Audio effect parameters are unavailable"
                fx_slider := FxSlider{animator +: {disabled +: {default: @on}}}
            }
        }
    }
    // Parsed, reusable donor row; the empty rack never invents an instance.
    mod.widgets.StageMixFxRow = RoundedView{
        width: Fill
        height: Fit
        flow: Right
        spacing: 8
        align: Align{x: 0.0, y: 0.5}
        padding: Inset{left: 0.0 right: 6.0 top: 3.0 bottom: 3.0}
        margin: Inset{left: 0.0 right: 10.0 top: 2.0 bottom: 2.0}
        draw_bg +: {
            color: stage.row
            border_color: stage.line
            border_size: 1.0
            border_radius: 3.0
        }
        Tip{ text: "Drag to move this effect up or down the chain; the order is the order it plays in"
            fx_grip := FxGrip{}
        }
        fx_head_col := View{
            width: 430
            height: Fit
            flow: Down
            spacing: 4
            fx_head_line := View{
                width: Fill
                height: Fit
                flow: Right
                spacing: 8
                align: Align{x: 0.0, y: 0.5}
                fx_route := FxRoute{
                    fx_master_col := View{
                        width: 64 height: Fit flow: Down spacing: 2
                        Tip{ text: "Route this effect to the final bus"
                            fx_chip_0 := UnavailablePill{width: 64 text: "MASTER"}
                        }
                        View{
                            width: Fill height: Fit flow: Right spacing: 2
                            align: Align{x: 0.5, y: 0.0}
                            Tip{ text: "Route this effect to the clip decks' sound"
                                fx_vj := StemChip{animator +: {disabled +: {default: @on}} width: 31 text: "VJ"}
                            }
                            Tip{ text: "Route this effect to both DJ decks together, after the crossfader"
                                fx_dj := StemChip{animator +: {disabled +: {default: @on}} width: 31 text: "DJ"}
                            }
                        }
                    }
                    fx_stem_col_a := View{
                        width: 54 height: Fit flow: Down spacing: 2
                        Tip{ text: "Route this effect to the whole of deck A"
                            fx_chip_1 := UnavailablePill{width: 54 text: "A"}
                        }
                        View{
                            width: Fill height: Fit flow: Right{wrap: true} spacing: 2
                            Tip{ text: "Route this effect to deck A's vocals alone"
                                fx_stem_a0 := StemChip{animator +: {disabled +: {default: @on}} text: "v"}
                            }
                            Tip{ text: "Route this effect to deck A's drums alone"
                                fx_stem_a1 := StemChip{animator +: {disabled +: {default: @on}} text: "d"}
                            }
                            Tip{ text: "Route this effect to deck A's bass alone"
                                fx_stem_a2 := StemChip{animator +: {disabled +: {default: @on}} text: "b"}
                            }
                            Tip{ text: "Route this effect to what is left of deck A"
                                fx_stem_a3 := StemChip{animator +: {disabled +: {default: @on}} text: "o"}
                            }
                        }
                    }
                    fx_stem_col_b := View{
                        width: 54 height: Fit flow: Down spacing: 2
                        Tip{ text: "Route this effect to the whole of deck B"
                            fx_chip_2 := UnavailablePill{width: 54 text: "B"}
                        }
                        View{
                            width: Fill height: Fit flow: Right{wrap: true} spacing: 2
                            Tip{ text: "Route this effect to deck B's vocals alone"
                                fx_stem_b0 := StemChip{animator +: {disabled +: {default: @on}} text: "v"}
                            }
                            Tip{ text: "Route this effect to deck B's drums alone"
                                fx_stem_b1 := StemChip{animator +: {disabled +: {default: @on}} text: "d"}
                            }
                            Tip{ text: "Route this effect to deck B's bass alone"
                                fx_stem_b2 := StemChip{animator +: {disabled +: {default: @on}} text: "b"}
                            }
                            Tip{ text: "Route this effect to what is left of deck B"
                                fx_stem_b3 := StemChip{animator +: {disabled +: {default: @on}} text: "o"}
                            }
                        }
                    }
                    fx_score_col := View{
                        width: 54 height: Fit flow: Down spacing: 2
                        Tip{ text: "Route this effect to all four instrument strips at once"
                            fx_chip_3 := UnavailablePill{width: 54 text: "SCORE"}
                        }
                        View{
                            width: Fill height: Fit flow: Right{wrap: true} spacing: 2
                            Tip{ text: "Route this effect to the SFX strip alone: the pads"
                                fx_score_0 := StemChip{animator +: {disabled +: {default: @on}} text: "s"}
                            }
                            Tip{ text: "Route this effect to the PIANO strip alone"
                                fx_score_1 := StemChip{animator +: {disabled +: {default: @on}} text: "p"}
                            }
                            Tip{ text: "Route this effect to the IRON strip alone"
                                fx_score_2 := StemChip{animator +: {disabled +: {default: @on}} text: "i"}
                            }
                            Tip{ text: "Route this effect to the DRUMS strip alone"
                                fx_score_3 := StemChip{animator +: {disabled +: {default: @on}} text: "d"}
                            }
                        }
                    }
                }
                View{
                    width: 36
                    height: Fit
                    align: Align{x: 0.5, y: 0.5}
                    fx_mix_learn := View{width: Fill height: Fit
                        Tip{ text: "How much of this effect is heard: dry to wet"
                            fx_mix := UnavailableKnob{width: 36 height: 36 min: 0.0 max: 1.0 default: 1.0 scroll_step: 0.025}
                        }
                    }
                }
                FxName{
                    fx_name_tip := Tip{ text: ""
                        fx_name := UnavailablePill{width: 92 text: ""}
                    }
                    fx_seat_cell := View{
                        visible: false
                        width: 92
                        height: Fit
                        align: Align{x: 0.0, y: 0.5}
                        Tip{ text: "Where this effect sits on its route: before or after the fader it plays through. After it, a tail rings out when the fader goes down"
                            fx_seat := UnavailablePicker{width: 92}
                        }
                    }
                }
                View{
                    width: 22
                    height: Fit
                    align: Align{x: 0.0, y: 0.5}
                    Tip{ text: "Hear this effect's result in the headphones: the chain it plays on, taken right after it; if the effect sits after the fader, raise the fader to hear it"
                        fx_listen := IconButton{
                            width: 22
                            align: Align{x: 0.5, y: 0.5}
                            draw_icon +: { svg: crate_resource("self:resources/icons/headphones.svg") }
                        }
                    }
                }
            }
            fx_graph_cell := View{
                width: Fill
                height: Fill
                visible: false
                fx_eq_graph := View{width: Fill height: Fill}
            }
        }
        View{width: 8 height: 1}
        fx_load_cell := View{
            width: 54
            height: Fit
            visible: false
            align: Align{x: 0.0, y: 0.5}
            Tip{ text: "Load a wav file as an impulse response: the room this effect plays through"
                fx_load := UnavailablePill{width: 54 text: "LOAD…"}
            }
        }
        fx_cells := View{
            width: Fill
            height: Fit
            flow: Right{wrap: true}
            spacing: 8
            fx_p0 := FxCell{} fx_p1 := FxCell{} fx_p2 := FxCell{} fx_p3 := FxCell{}
            fx_p4 := FxCell{} fx_p5 := FxCell{} fx_p6 := FxCell{} fx_p7 := FxCell{}
            fx_p8 := FxCell{} fx_p9 := FxCell{} fx_p10 := FxCell{} fx_p11 := FxCell{}
            fx_p12 := FxCell{} fx_p13 := FxCell{} fx_p14 := FxCell{} fx_p15 := FxCell{}
        }
        View{
            width: 22
            height: Fill
            flow: Down
            spacing: 2
            align: Align{x: 1.0, y: 0.0}
            padding: Inset{top: 1., bottom: 1.}
            Tip{ text: "Take this effect out of the rack: switched off, back in CHOOSE, its channels kept"
                fx_remove := UnavailablePill{width: 22 height: 22 margin: 0. text: "×"}
            }
            Tip{ text: "Put every knob on this row back to the value the table gives it"
                fx_reset := IconButton{width: 22 height: 22 margin: 0. icon_walk: Walk{width: 9 height: Fit} draw_icon +: { svg: crate_resource("self:resources/icons/undo.svg") }}
            }
        }
    }

    mod.widgets.MixSurfaceBase = #(MixSurface::register_widget(vm))
    mod.widgets.MixSurface = set_type_default() do mod.widgets.MixSurfaceBase{
        width: Fill height: Fill flow: Down spacing: 6
        View{
            width: Fill height: Fit flow: Right spacing: 8
            Tick{width: Fit text: "PROGRAM MIX"}
            mix_solo_status := Label{
                width: Fill text: "DJ A · DJ B · IRON"
                draw_text.color: stage.muted
                draw_text.text_style.font_size: 9
            }
            mix_master_meter := Label{
                width: Fit text: "PEAK — · GR —"
                draw_text.color: stage.text
                draw_text.text_style.font_size: 9
            }
        }
        mix_strip_row := View{
            width: Fill height: Fit flow: Right spacing: 10
            new_batch: true
            mix_strips := View{
                width: 438 height: Fit flow: Right spacing: 10
                mix_video_col := FaderCol{width: 54 mix_video_name := Tick{text: "VIDEO"} mix_video_meter := Tick{text: "—"} Tip{ text: "Video audio is unavailable" mix_video_gain := UnavailableFader{max: 1.5} } Tip{ text: "Channel mute/solo is unavailable" mix_video_mute := UnavailableButton{width: 54 text: "MUTE"} } Tip{ text: "Channel mute/solo is unavailable" mix_video_solo := UnavailableButton{width: 54 text: "SOLO"} } Tip{ text: "Browser audio is unavailable" mix_web_lamp := Tick{text: "WEB —"} } Tip{ text: "Browser audio is unavailable" mix_web_gain := UnavailableFader{max: 1.5 height: 64} }}
                mix_dja_col := FaderCol{width: 54 mix_dja_name := Tick{text: "DJ A"} mix_dja_meter := Tick{text: "—"} Tip{ text: "DJ A level into the final bus, up to 1.5x" mix_dja_gain := ApcFader{max: 1.5 min: 0 default: 1 bind := "deck_a_gain.gain"} } Tip{ text: "Channel mute/solo is unavailable" mix_dja_mute := UnavailableButton{width: 54 text: "MUTE"} } Tip{ text: "Channel mute/solo is unavailable" mix_dja_solo := UnavailableButton{width: 54 text: "SOLO"} }}
                mix_djb_col := FaderCol{width: 54 mix_djb_name := Tick{text: "DJ B"} mix_djb_meter := Tick{text: "—"} Tip{ text: "DJ B level into the final bus, up to 1.5x" mix_djb_gain := ApcFader{max: 1.5 min: 0 default: 1 bind := "deck_b_gain.gain"} } Tip{ text: "Channel mute/solo is unavailable" mix_djb_mute := UnavailableButton{width: 54 text: "MUTE"} } Tip{ text: "Channel mute/solo is unavailable" mix_djb_solo := UnavailableButton{width: 54 text: "SOLO"} }}
                mix_sfx_col := FaderCol{width: 54 mix_sfx_name := Tick{text: "SFX"} mix_sfx_meter := Tick{text: "—"} Tip{ text: "Sample pads are unavailable" mix_sfx_gain := UnavailableFader{max: 1.5} } Tip{ text: "Channel mute/solo is unavailable" mix_sfx_mute := UnavailableButton{width: 54 text: "MUTE"} } Tip{ text: "Channel mute/solo is unavailable" mix_sfx_solo := UnavailableButton{width: 54 text: "SOLO"} }}
                mix_piano_col := FaderCol{width: 54 mix_piano_name := Tick{text: "PIANO"} mix_piano_meter := Tick{text: "—"} Tip{ text: "Piano instrument is unavailable" mix_piano_gain := UnavailableFader{max: 1.5} } Tip{ text: "Channel mute/solo is unavailable" mix_piano_mute := UnavailableButton{width: 54 text: "MUTE"} } Tip{ text: "Channel mute/solo is unavailable" mix_piano_solo := UnavailableButton{width: 54 text: "SOLO"} }}
                mix_score_col := FaderCol{width: 54 mix_score_name := Tick{text: "SCORE"} mix_score_meter := Tick{text: "—"} Tip{ text: "The SCORE page's score into the final bus, up to 1.5x" mix_score_gain := ApcFader{max: 1.5 min: 0 default: 1 bind := "score_gain.gain"} } Tip{ text: "Channel mute/solo is unavailable" mix_score_mute := UnavailableButton{width: 54 text: "MUTE"} } Tip{ text: "Channel mute/solo is unavailable" mix_score_solo := UnavailableButton{width: 54 text: "SOLO"} }}
                mix_drums_col := FaderCol{width: 54 mix_drums_name := Tick{text: "DRUMS"} mix_drums_meter := Tick{text: "—"} Tip{ text: "Drum instrument is unavailable" mix_drums_gain := UnavailableFader{max: 1.5} } Tip{ text: "Channel mute/solo is unavailable" mix_drums_mute := UnavailableButton{width: 54 text: "MUTE"} } Tip{ text: "Channel mute/solo is unavailable" mix_drums_solo := UnavailableButton{width: 54 text: "SOLO"} }}
            }
            RoundedView{
                width: Fill height: Fit flow: Down spacing: 3 padding: 8
                draw_bg +: {color: stage.panel_score border_color: stage.line border_size: 1.0 border_radius: 3.0}
                View{width: Fill height: Fit flow: Right
                    Tick{width: Fill text: "FINAL BUS · COMPRESSOR / LIMITER"}
                    Tip{ text: "Final-bus compressor is unavailable"
                        mix_master_bypass := UnavailableButton{width: 66 text: "BYPASS"}
                    }
                }
                View{width: Fill height: Fit flow: Right spacing: 5 Tick{width: 72 text: "THRESHOLD"} Tip{ text: "Final-bus compressor is unavailable" mix_comp_threshold := UnavailableHSlider{scroll_step: 0.025} } Tick{width: 52 text: "RATIO"} Tip{ text: "Final-bus compressor is unavailable" mix_comp_ratio := UnavailableHSlider{scroll_step: 0.025} }}
                View{width: Fill height: Fit flow: Right spacing: 5 Tick{width: 72 text: "ATTACK"} Tip{ text: "Final-bus compressor is unavailable" mix_comp_attack := UnavailableHSlider{scroll_step: 0.025} } Tick{width: 52 text: "RELEASE"} Tip{ text: "Final-bus compressor is unavailable" mix_comp_release := UnavailableHSlider{scroll_step: 0.025} }}
                View{width: Fill height: Fit flow: Right spacing: 5 Tick{width: 72 text: "MAKEUP"} Tip{ text: "Final-bus compressor is unavailable" mix_comp_makeup := UnavailableHSlider{scroll_step: 0.025} } Tick{width: 52 text: "CEILING"} Tip{ text: "Final-bus limiter is unavailable" mix_limiter_ceiling := UnavailableHSlider{scroll_step: 0.025} }}
                Label{
                    width: Fill text: "DJ A, DJ B and the Ironfish voices feed the program bus. Compressor and limiter are unavailable."
                    draw_text.color: stage.dim_lo
                    draw_text.text_style.font_size: 9
                }
            }
        }

        mix_rack := View{
            width: Fill height: Fill flow: Right spacing: 8 new_batch: true
            fx_active_pane := View{
                width: Fill height: Fill flow: Down spacing: 4
                View{
                    width: Fill height: Fit flow: Right spacing: 8
                    padding: Inset{left: 0 right: 16 top: 0 bottom: 0}
                    align: Align{x: 0 y: 0.5}
                    fx_route_head := FxRoute{PanelLabel{text: "fx on"}}
                    View{width: Fill height: 1}
                    Tip{text: "Open the effect loudness settings"
                        sfx_fx_levels := PillButton{width: 70 text: "LEVELS"}
                    }
                }
                fx_active := View{
                    width: Fill height: Fill flow: Overlay
                    list := ReorderList{
                        width: Fill height: Fill drag_scrolling: false
                        drag_handle: @fx_grip
                        draw_indicator +: {color: stage.accent}
                        FxActiveRow := mod.widgets.StageMixFxRow{}
                    }
                    fx_empty := View{width: Fill height: 40 align: Align{x: 0.5 y: 0.5}
                        Label{text: "no effects in the rack" draw_text.color: stage.muted}
                    }
                }
            }
            fx_choose_pane := View{
                width: 240 height: Fill flow: Down spacing: 4
                PanelLabel{text: "CHOOSE"}
                fx_choose := DataGrid{
                    width: Fill
                    height: Fill
                    rows: 0
                    cols: 2
                    selection: GridSelectMode.Off
                    grab_key_focus: false
                    row_drag: true
                    row_drag_scroll: false
                    drag_threshold: 5.0
                    show_row_headers: false
                    allow_row_resize: false
                    allow_col_resize: true
                    allow_col_reorder: false
                    sortable: false
                    header_edges: true
                    zebra_stripes: true
                    default_row_height: 23.0
                    col_header_height: 22.0
                    min_col_width: 16.0
                    cell_pad_x: 6.0
                    header_align: 0.0
                    cell_cursor: MouseCursor.Hand
                    color_bg: stage.bg
                    color_cell: stage.row
                    color_cell_alt: stage.row_alt
                    color_text: stage.label
                    color_header: stage.bg
                    color_header_active: stage.raised
                    color_header_text: stage.secondary
                    color_header_sorted: stage.text_hi
                    color_selection: #x00000000
                    color_selection_border: #x00000000
                    color_drag_marker: stage.accent
                    color_header_edge: stage.edge
                    draw_cell +: {border_color: uniform(stage.bg)}
                    draw_text +: {
                        text_style: theme.font_regular{font_size: 9}
                        color: stage.label
                    }
                    draw_text_bold +: {
                        text_style: theme.font_bold{font_size: 9}
                        color: stage.label
                    }
                    draw_text_header +: {text_style: theme.font_bold{font_size: 8}}
                }
                Label{width: Fill text: "Click an effect to put it on the final bus; its MASTER / A / B chips move it"
                    draw_text.color: stage.dim_lo draw_text.text_style.font_size: 9}
            }
        }
        sfx_levels_modal := Modal{
            can_dismiss: true
            content +: {
                width: Fit height: Fit
                RoundedView{
                    width: Fit height: Fit flow: Down spacing: 4 padding: 10
                    draw_bg +: {color: stage.panel border_color: stage.separator border_size: 1 border_radius: 5}
                    align: Align{x: 0 y: 0}
                    View{width: Fit height: Fit flow: Right spacing: 8 align: Align{x: 0 y: 0.5}
                        PanelLabel{text: "FX LEVELS"}
                        PanelLabel{text: "FOL follows ALL · OFF leaves it alone · MTCH matches the input · CAP holds under the cap"}
                    }
                    View{width: Fit height: Fit flow: Right spacing: 8 align: Align{x: 0 y: 0.5}
                        PanelLabel{width: 84 text: "ALL"}
                        Tip{text: "Effect loudness rules are unavailable"
                            sfx_lvl_default := UnavailablePicker{width: 48}
                        }
                        PanelLabel{text: "what every effect on FOL does"}
                    }
                    fx_levels := View{width: 402 height: 320 flow: Down
                        View{width: Fill height: 40 align: Align{x: 0.5 y: 0.5}
                            Label{text: "no effects in the rack" draw_text.color: stage.muted}
                        }
                    }
                    Tip{text: "Close the levels dialog"
                        sfx_levels_close := PillButton{width: 60 text: "CLOSE"}
                    }
                }
            }
        }
    }
}

/// One effect in the rack, as the host shows it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MixRackRow {
    pub name: String,
    /// What the effect does, for the name's tip.
    pub tip: String,
    /// Which route it plays on: 0 the final bus (MASTER), 1 deck A, 2 deck B.
    pub route: usize,
    pub params: Vec<MixRackParam>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct MixRackParam {
    pub label: String,
    /// The value as read: "1.2 kHz", "40 %".
    pub text: String,
    /// The slider's place, 0..1.
    pub norm: f64,
}

/// What the rack asks the host for. Rows are indices into the rows the host
/// handed over; the surface changes nothing itself.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum MixSurfaceAction {
    #[default]
    None,
    /// The chooser's effect at this index, into the rack.
    Add(usize),
    Remove(usize),
    /// Every knob of the row back to its default.
    Reset(usize),
    /// The row dragged from one place to another.
    Move { from: usize, to: usize },
    /// The row moved to another route (0 MASTER, 1 A, 2 B).
    Route { row: usize, route: usize },
    /// A knob turned, to this place 0..1.
    Param { row: usize, param: usize, norm: f64 },
    /// A knob let go: the host keeps the value.
    ParamEnd { row: usize },
}

const ROUTE_CHIPS: [&[LiveId]; 3] = [ids!(fx_chip_0), ids!(fx_chip_1), ids!(fx_chip_2)];
const PARAM_CELLS: [&[LiveId]; 16] = [
    ids!(fx_p0), ids!(fx_p1), ids!(fx_p2), ids!(fx_p3), ids!(fx_p4), ids!(fx_p5), ids!(fx_p6), ids!(fx_p7),
    ids!(fx_p8), ids!(fx_p9), ids!(fx_p10), ids!(fx_p11), ids!(fx_p12), ids!(fx_p13), ids!(fx_p14), ids!(fx_p15),
];

#[derive(Script, ScriptHook, Widget)]
pub struct MixSurface {
    #[deref]
    view: View,
    #[rust]
    chooser_initialized: bool,
    #[rust]
    rack: Vec<MixRackRow>,
    /// The chooser's rows: (name, category).
    #[rust]
    choices: Vec<(String, String)>,
}

impl MixSurface {
    pub fn set_rack(&mut self, cx: &mut Cx, rows: &[MixRackRow]) {
        if self.rack != rows {
            self.rack = rows.to_vec();
            self.view.view(cx, ids!(fx_empty)).set_visible(cx, self.rack.is_empty());
            self.view.redraw(cx);
        }
    }

    pub fn set_choices(&mut self, cx: &mut Cx, choices: &[(String, String)]) {
        if self.choices != choices {
            self.choices = choices.to_vec();
            self.chooser_initialized = false;
            self.view.redraw(cx);
        }
    }

    /// One rack row's widgets from its model.
    fn fill_row(cx: &mut Cx, item: &WidgetRef, row: &MixRackRow) {
        let palette = makepad_look::palette::get(cx);
        let name = item.button(cx, ids!(fx_name));
        name.set_text(cx, &row.name);
        item.widget(cx, ids!(fx_name)).set_disabled(cx, false);
        for (route, id) in ROUTE_CHIPS.iter().enumerate() {
            let chip = item.button(cx, id);
            item.widget(cx, id).set_disabled(cx, false);
            let lit = route == row.route;
            let (bg, ink) = if lit { (palette.accent, palette.on_accent) } else { (palette.raised, palette.text_soft) };
            let (bg, ink) = (Vec4f::from_u32(bg), Vec4f::from_u32(ink));
            // Set straight in: this runs in the draw, where an apply would
            // redraw the chip and so ask for the next frame for good.
            if let Some(mut chip) = chip.borrow_mut() {
                chip.draw_bg.set_uniform(cx, live_id!(color), &[bg.x, bg.y, bg.z, bg.w]);
                chip.draw_text.color = ink;
            };
        }
        item.widget(cx, ids!(fx_remove)).set_disabled(cx, false);
        for (index, cell_id) in PARAM_CELLS.iter().enumerate() {
            let cell = item.widget(cx, cell_id);
            match row.params.get(index) {
                Some(param) => {
                    cell.set_visible(cx, true);
                    cell.label(cx, ids!(fx_label)).set_text(cx, &param.label);
                    cell.label(cx, ids!(fx_value)).set_text(cx, &param.text);
                    let slider = cell.slider(cx, ids!(fx_slider));
                    cell.widget(cx, ids!(fx_slider)).set_disabled(cx, false);
                    if !slider.has_text_focus(cx) {
                        slider.set_value(cx, param.norm);
                    }
                }
                None => cell.set_visible(cx, false),
            }
        }
    }

    fn handle_rack_actions(&mut self, cx: &mut Cx, actions: &Actions) {
        let uid = self.widget_uid();
        let list = self.view.reorder_list(cx, ids!(list));
        let (moved, items) = match list.borrow() {
            Some(list) => (list.reordered(actions), list.items_with_actions(actions)),
            None => (None, Vec::new()),
        };
        if let Some((from, to)) = moved {
            cx.widget_action(uid, MixSurfaceAction::Move { from, to });
        }
        for (row, item) in items {
            if row >= self.rack.len() {
                continue;
            }
            for (route, id) in ROUTE_CHIPS.iter().enumerate() {
                if item.button(cx, id).clicked(actions) {
                    cx.widget_action(uid, MixSurfaceAction::Route { row, route });
                }
            }
            if item.button(cx, ids!(fx_remove)).clicked(actions) {
                cx.widget_action(uid, MixSurfaceAction::Remove(row));
            }
            if item.button(cx, ids!(fx_reset)).clicked(actions) {
                cx.widget_action(uid, MixSurfaceAction::Reset(row));
            }
            for (param, cell_id) in PARAM_CELLS.iter().enumerate() {
                let slider = item.widget(cx, cell_id).slider(cx, ids!(fx_slider));
                if let Some(norm) = slider.slided(actions) {
                    cx.widget_action(uid, MixSurfaceAction::Param { row, param, norm });
                }
                if slider.end_slide(actions).is_some() {
                    cx.widget_action(uid, MixSurfaceAction::ParamEnd { row });
                }
            }
        }
        let grid = self.view.data_grid(cx, ids!(fx_choose));
        for action in grid.actions(actions) {
            if let DataGridAction::CellReleased { row, .. } = action {
                if row < self.choices.len() {
                    cx.widget_action(uid, MixSurfaceAction::Add(row));
                }
            }
        }
    }
}

impl MixSurfaceRef {
    pub fn set_rack(&self, cx: &mut Cx, rows: &[MixRackRow]) {
        if let Some(mut inner) = self.borrow_mut() {
            inner.set_rack(cx, rows);
        }
    }

    pub fn set_choices(&self, cx: &mut Cx, choices: &[(String, String)]) {
        if let Some(mut inner) = self.borrow_mut() {
            inner.set_choices(cx, choices);
        }
    }

    /// The rack's asks in `actions`, in order.
    pub fn actions(&self, actions: &Actions) -> Vec<MixSurfaceAction> {
        actions.filter_widget_actions_cast::<MixSurfaceAction>(self.widget_uid()).filter(|action| *action != MixSurfaceAction::None).collect()
    }
}

impl Widget for MixSurface {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        if matches!(event, Event::ScriptReapply) {
            self.chooser_initialized = false;
        }
        self.view.handle_event(cx, event, scope);
        if let Event::Actions(actions) = event {
            if self.button(cx, ids!(sfx_fx_levels)).clicked(actions) {
                self.modal(cx, ids!(sfx_levels_modal)).open(cx);
            }
            if self.button(cx, ids!(sfx_levels_close)).clicked(actions) {
                self.modal(cx, ids!(sfx_levels_modal)).close(cx);
            }
            self.handle_rack_actions(cx, actions);
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        while let Some(step) = self.view.draw_walk(cx, scope, walk).step() {
            if let Some(mut list) = step.borrow_mut::<ReorderList>() {
                list.set_item_range(cx, 0, self.rack.len());
                while let Some(index) = list.next_visible_item(cx) {
                    let Some(row) = self.rack.get(index) else { continue };
                    let item = list.item(cx, index, live_id!(FxActiveRow));
                    Self::fill_row(cx, &item, row);
                    item.draw_all(cx, scope);
                }
            }
            if let Some(mut grid) = step.borrow_mut::<DataGrid>() {
                if !self.chooser_initialized {
                    grid.set_col_labels(vec!["NAME".into(), "CATEGORY".into()]);
                    grid.set_col_widths(cx, &[124.0, 100.0]);
                    self.chooser_initialized = true;
                }
                grid.set_grid_size(self.choices.len(), 2);
                while let Some(cell) = grid.next_cell(cx) {
                    let Some((name, category)) = self.choices.get(cell.row) else { continue };
                    grid.cell_text(cx, &cell, if cell.col == 0 { name } else { category });
                }
            }
        }
        DrawStep::done()
    }
}
