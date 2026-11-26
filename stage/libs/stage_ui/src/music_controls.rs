//! Original Stage DJ control geometry, extracted from donor music_view.rs.
//! These are Slider/Button/View templates only; the host owns deck state.

use makepad_widgets::*;

script_mod! {
    use mod.prelude.widgets_internal.*
    use mod.widgets.*
    let stage = mod.stage

    let MusicLabel = Label{
        draw_text.color: stage.secondary
        draw_text.text_style: theme.font_bold{font_size: 8}
    }

    let MusicValue = Label{
        flow: Flow.Right{wrap: false}
        max_lines: 1
        draw_text.color: stage.text
        draw_text.text_style.font_size: 11
    }

    let MusicButton = Button{
        draw_bg +: {
            color: stage.control
            color_focus: stage.control
            color_hover: stage.control_hover
            color_down: stage.control_down
            border_color: stage.border_hi
            border_radius: 6.0
            border_size: 1.0
        }
        draw_text +: {
            color: stage.label
            color_focus: stage.label
            color_hover: stage.text_hi
            text_style: theme.font_bold{font_size: 9}
        }
    }

    let MusicIconButton = ButtonIcon{
        width: 30
        height: 24
        icon_walk: Walk{width: 12 height: Fit}
        draw_bg +: {
            color: stage.control
            color_focus: stage.control
            color_hover: stage.control_hover
            color_down: stage.control_down
            border_color: stage.border
            border_radius: 5.0
            border_size: 1.0
        }
        draw_icon +: {
            color: stage.label
        }
    }

    let MusicTransportButton = MusicButton{
        height: 38
        padding: Inset{left: 8.0 right: 8.0 top: 0.0 bottom: 0.0}
        align: Align{x: 0.5, y: 0.5}
        draw_bg +: {border_radius: 7.0}
        draw_text +: {text_style: theme.font_bold{font_size: 11}}
    }

    let MusicTransportIconButton = MusicIconButton{
        width: 40
        height: 38
        icon_walk: Walk{width: 16 height: Fit}
        draw_bg +: {border_radius: 7.0}
    }

    let MSRow = View{
        width: Fill
        height: Fit
        flow: Right
        spacing: 2
    }

    let MusicKnob = Rotary{
        width: 42
        height: 42
        min: 0.0
        max: 2.0
        default: 1.0
        scroll_step: 0.025
        taper: Audio
        arc_from_origin: true
        text: ""
        flow: Down
        text_input: TextInput{width: 0 height: 0}
        draw_bg +: {
            // The quad is the box: a style sheet grows its slider family's quad for shadows, and this pixel draws to the box, so under a sheet it would scale up.
            vertex: fn() {self.vertex_pos = self.clip_and_transform_vertex(self.rect_pos, self.rect_size)}
            body_color: uniform(stage.groove)
            body_color_hover: uniform(stage.track)
            rim_color: uniform(stage.border_strong)
            ring_color: uniform(stage.control_top)
            val_color: uniform(stage.accent)
            pointer_color: uniform(stage.text_bright)
            pixel: fn() {
                let sdf = Sdf2d.viewport(self.pos * self.rect_size)
                let c = self.rect_size * 0.5
                let r = min(self.rect_size.x, self.rect_size.y) * 0.5
                let start = PI * 0.25
                let sweep = PI * 1.5
                sdf.arc_round_caps(c.x, c.y, r - 2.5, start, start + sweep, 2.5)
                sdf.fill(self.ring_color)
                // The lit arc runs from the resting point to the pointer
                // when the knob asks for it, else from the stop as a
                // fader would.
                let lo = min(self.origin_pos, self.slide_pos)
                let hi = max(self.origin_pos, self.slide_pos)
                let from = mix(0.0, lo, self.arc_origin)
                let to = mix(max(self.slide_pos, 0.01), max(hi, lo + 0.01), self.arc_origin)
                sdf.arc_round_caps(c.x, c.y, r - 2.5, start + sweep * from, start + sweep * to, 2.5)
                sdf.fill(self.val_color)
                // A tick at the resting point, so a knob at rest -- an arc
                // of no length -- still shows where home is.
                let o = start + sweep * self.origin_pos
                let od = vec2(-sin(o), cos(o))
                let t0 = c + od * (r - 6.0)
                let t1 = c + od * r
                sdf.move_to(t0.x, t0.y)
                sdf.line_to(t1.x, t1.y)
                sdf.stroke(self.rim_color * self.arc_origin, 1.0)
                sdf.circle(c.x, c.y, r - 7.5)
                sdf.fill_keep(self.body_color.mix(self.body_color_hover, max(self.hover, self.drag)))
                sdf.stroke(self.rim_color, 1.0)
                let a = start + sweep * self.slide_pos
                let d = vec2(-sin(a), cos(a))
                let p0 = c + d * (r - 13.0)
                let p1 = c + d * (r - 8.0)
                sdf.move_to(p0.x, p0.y)
                sdf.line_to(p1.x, p1.y)
                sdf.stroke(self.pointer_color, 2.0)
                return sdf.result
            }
        }
    }

    let KnobLabel = Button{
        width: Fill
        height: Fit
        padding: 0
        margin: 0
        align: Align{x: 0.5, y: 0.0}
        draw_bg +: {
            color: #x00000000
            color_focus: #x00000000
            color_hover: #x00000000
            color_down: #x00000000
            border_size: 0.0
            border_radius: 0.0
        }
        draw_text +: {
            color: stage.secondary
            color_focus: stage.secondary
            color_hover: stage.label
            color_down: stage.muted
            text_style: theme.font_bold{font_size: 7}
        }
    }

    let KnobStack = View{
        width: 46
        height: Fit
        flow: Down
        spacing: 2
        align: Align{x: 0.5, y: 0.0}
    }

    let StemStack = KnobStack{width: 44}

    let StemKnob = MusicKnob{width: 40 height: 40 default: 1.0}

    let MusicFader = Slider{
        axis: DragAxis.Vertical
        scroll_step: 0.025
        width: 40
        height: Fill
        text: ""
        flow: Down
        text_input: TextInput{width: 0 height: 0}
        draw_bg +: {
            // The quad is the box: a style sheet grows its slider family's quad for shadows, and this pixel draws to the box, so under a sheet it would scale up.
            vertex: fn() {self.vertex_pos = self.clip_and_transform_vertex(self.rect_pos, self.rect_size)}
            body_color: uniform(stage.groove)
            track_color: uniform(stage.track)
            fill_color: uniform(stage.accent)
            cap_color: uniform(stage.text)
            cap_shadow: uniform(stage.muted)
            pixel: fn() {
                let sdf = Sdf2d.viewport(self.pos * self.rect_size)
                sdf.box(3., 2., self.rect_size.x - 6., self.rect_size.y - 4., 6.)
                sdf.fill(self.body_color)
                let top = 10.
                let h = self.rect_size.y - top - 10.
                let track_w = 7.
                let track_x = (self.rect_size.x - track_w) * 0.5
                sdf.box(track_x, top, track_w, h, 3.)
                sdf.fill(self.track_color)
                // From the bottom, or out of the resting point when the
                // fader asks for it: a pitch fader pulled down should
                // show a bar hanging BELOW centre, not a shorter bar
                // still growing from the floor.
                let lo = min(self.origin_pos, self.slide_pos)
                let hi = max(self.origin_pos, self.slide_pos)
                let from = mix(0., lo, self.arc_origin)
                let to = mix(self.slide_pos, hi, self.arc_origin)
                let fill_h = max(1., h * (to - from))
                let fill_bottom = top + h - h * from
                sdf.box(track_x + 1.5, fill_bottom - fill_h + 1.5, track_w - 3., max(1., fill_h - 3.), 2.)
                sdf.fill(self.fill_color)
                let cap_h = 13.
                // The cap follows the value, independently of an origin-centred fill.
                let cap_y = top + h * (1.0 - self.slide_pos) - cap_h * 0.5
                sdf.box(5., cap_y + 1.5, self.rect_size.x - 10., cap_h, 4.)
                sdf.fill(self.cap_shadow)
                sdf.box(4., cap_y, self.rect_size.x - 8., cap_h, 4.)
                sdf.fill(self.cap_color)
                return sdf.result
            }
        }
    }

    let CrossFader = Slider{
        width: Fill
        height: 40
        min: 0.0
        max: 1.0
        default: 0.5
        scroll_step: 0.025
        text: ""
        text_input: TextInput{width: 0 height: 0}
        draw_bg +: {
            // The quad is the box: a style sheet grows its slider family's quad for shadows, and this pixel draws to the box, so under a sheet it would scale up.
            vertex: fn() {self.vertex_pos = self.clip_and_transform_vertex(self.rect_pos, self.rect_size)}
            body_color: uniform(stage.groove)
            track_color: uniform(stage.track)
            fill_color: uniform(stage.accent)
            cap_color: uniform(stage.text)
            cap_shadow: uniform(stage.muted)
            pixel: fn() {
                let sdf = Sdf2d.viewport(self.pos * self.rect_size)
                sdf.box(2., 6., self.rect_size.x - 4., self.rect_size.y - 12., 8.)
                sdf.fill(self.body_color)
                let left = 10.
                let w = self.rect_size.x - left - 10.
                let track_h = 9.
                let track_y = (self.rect_size.y - track_h) * 0.5
                sdf.box(left, track_y, w, track_h, 4.)
                sdf.fill(self.track_color)
                let fill_w = max(1., w * self.slide_pos)
                sdf.box(left + 1.5, track_y + 1.5, max(1., fill_w - 3.), track_h - 3., 3.)
                sdf.fill(self.fill_color)
                let cap_w = 20.
                let cap_x = left + fill_w - cap_w * 0.5
                sdf.box(cap_x + 1.5, 8., cap_w, self.rect_size.y - 16., 6.)
                sdf.fill(self.cap_shadow)
                sdf.box(cap_x, 6., cap_w, self.rect_size.y - 14., 6.)
                sdf.fill(self.cap_color)
                return sdf.result
            }
        }
    }

    let DeckMeter = SolidView{
        width: 9
        height: Fill
        draw_bg +: {
            level: uniform(0.0)
            hold: uniform(0.0)
            color: uniform(stage.groove)
            color_lit: uniform(stage.accent)
            color_hot: uniform(stage.hot)
            // Its own colour, and a pale one deliberately: the mark has to
            // read against the dark trough AND against the lit bar it sits
            // on top of, and anything from the bar's own range disappears
            // into the bar exactly when the level is high.
            color_mark: uniform(stage.mark)
            pixel: fn() {
                let sdf = Sdf2d.viewport(self.pos * self.rect_size)
                sdf.box(0.0, 0.0, self.rect_size.x, self.rect_size.y, 3.0)
                sdf.fill(self.color)
                let y = 1.0 - self.pos.y
                let on = step(y, self.level)
                let hot = smoothstep(0.72, 0.95, y)
                let seg = step(0.35, fract(self.pos.y * self.rect_size.y / 4.0))
                let c = self.color_lit.mix(self.color_hot, hot)
                let h = max(1.0, self.rect_size.y * self.level)
                sdf.box(1.5, self.rect_size.y - h, self.rect_size.x - 3.0, h, 2.0)
                sdf.fill(vec4(c.x, c.y, c.z, c.w * on * seg))
                // Held inside the column rather than centred on the level:
                // at full scale a centred mark loses half its width off the
                // top edge, which is the one reading it exists to report.
                // Two whole pixels, on a whole pixel. At 1.5 it landed
                // across two rows at half weight in each and came and went
                // as the level moved it a fraction up or down -- the same
                // reason the marks on the wave lane snap.
                let mark_h = 2.0
                let mark_y = floor(clamp(
                    (1.0 - self.hold) * self.rect_size.y - mark_h * 0.5,
                    0.0,
                    self.rect_size.y - mark_h
                ))
                sdf.box(1.5, mark_y, self.rect_size.x - 3.0, mark_h, 0.5)
                // Nothing held, no mark -- otherwise it parks on the floor
                // and reads as a level that is not there.
                sdf.fill(vec4(
                    self.color_mark.x,
                    self.color_mark.y,
                    self.color_mark.z,
                    self.color_mark.w * step(0.002, self.hold)
                ))
                return sdf.result
            }
        }
    }

    let DeckWell = RoundedView{
        width: Fill
        height: Fill
        padding: 1
        draw_bg +: {
            color: stage.video
            border_color: stage.border
            border_size: 1.0
            border_radius: 8.0
        }
    }

    mod.widgets.StageMusicLabel = MusicLabel
    mod.widgets.StageMusicValue = MusicValue
    mod.widgets.StageMusicButton = MusicButton
    mod.widgets.StageMusicIconButton = MusicIconButton
    mod.widgets.StageMusicTransportButton = MusicTransportButton
    mod.widgets.StageMusicTransportIconButton = MusicTransportIconButton
    mod.widgets.StageMusicMSRow = MSRow
    mod.widgets.StageMusicKnob = MusicKnob
    mod.widgets.StageMusicKnobLabel = KnobLabel
    mod.widgets.StageMusicKnobStack = KnobStack
    mod.widgets.StageMusicStemStack = StemStack
    mod.widgets.StageMusicStemKnob = StemKnob
    mod.widgets.StageMusicFader = MusicFader
    mod.widgets.StageMusicCrossFader = CrossFader
    mod.widgets.StageMusicDeckMeter = DeckMeter
    mod.widgets.StageMusicDeckWell = DeckWell
    true
}
