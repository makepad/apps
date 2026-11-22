//! Screen-space caption and magnitude ramp for the board's heat field.

use super::theme::BoardTheme;
use fab_elec_sim::heat::HeatField;
use makepad_widgets_core::*;
use std::fmt::Write as _;

script_mod! {
    use mod.prelude.widgets_internal.*
    use mod.elec_live.*

    mod.draw.DrawHeatRamp = set_type_default() do #(DrawHeatRamp::script_shader(vm)) {
        ..mod.draw.DrawQuad
        depth_write: false
        light: 0.0
        alpha: 1.0
        pixel: fn() {
            let sdf = Sdf2d.viewport(self.pos * self.rect_size)
            sdf.box(0. 0. self.rect_size.x self.rect_size.y 2.)
            let color = Live.mag(self.pos.x, self.light)
            sdf.fill_premul(vec4(color * self.alpha, self.alpha))
            return sdf.result
        }
    }

    mod.widgets.BoardHeatKey = set_type_default() do #(HeatKey::script_api(vm)) {
        draw_ramp: mod.draw.DrawHeatRamp {}
        draw_plate: mod.draw.DrawColor {
            pixel: fn() {
                let sdf = Sdf2d.viewport(self.pos * self.rect_size)
                sdf.box(0. 0. self.rect_size.x self.rect_size.y 3.)
                sdf.fill(self.color)
                return sdf.result
            }
        }
        draw_label: mod.draw.DrawText {
            text_style: theme.font_regular {font_size: 8.5}
        }
    }
}

/// A rounded magnitude ramp; one quad with per-instance light and fade.
#[derive(Script, ScriptHook)]
#[repr(C)]
pub struct DrawHeatRamp {
    #[deref]
    pub draw_super: DrawQuad,
    #[live]
    pub light: f32,
    #[live]
    pub alpha: f32,
}

/// A pane's heat legend draw helper; constant work with respect to the field's grid size.
#[derive(Script, ScriptHook)]
pub struct HeatKey {
    #[live]
    draw_ramp: DrawHeatRamp,
    #[live]
    draw_plate: DrawColor,
    #[live]
    draw_label: DrawText,
}

impl HeatKey {
    /// Draw the fading lower-left key in O(1) grid work, allocating only its two number strings.
    pub fn draw(&mut self, cx: &mut Cx2d, pane: Rect, field: &HeatField, mix: f32, light: bool) {
        if mix <= 0.004 || pane.size.x <= 0.0 || pane.size.y <= 0.0 {
            return;
        }
        let lo = number(field.range[0], &field.unit);
        let hi = number(field.range[1], &field.unit);
        let [caption, low, high] = [field.label.as_str(), lo.as_str(), hi.as_str()].map(|text| {
            let laid = self.draw_label.layout(cx, 0.0, 0.0, None, false, Align::default(), text);
            dvec2(
                f64::from(laid.size_in_lpxs.width * self.draw_label.font_scale),
                f64::from(laid.size_in_lpxs.height * self.draw_label.font_scale),
            )
        });
        let pad = dvec2(8.0, 8.0);
        let gap = 6.0;
        let bar = dvec2(112.0, 8.0);
        let row_height = low.y.max(high.y).max(bar.y);
        let row_width = low.x + gap + bar.x + gap + high.x;
        let size = dvec2(caption.x.max(row_width), caption.y + gap + row_height) + pad * 2.0;
        let rect = Rect {
            pos: pane.pos + dvec2(12.0, pane.size.y - 12.0 - size.y),
            size,
        };
        let start = rect.pos + pad;
        let row_y = start.y + caption.y + gap + row_height * 0.5;
        let theme = BoardTheme::for_light(light);
        let ink = theme.label;
        let plate = theme.marks[3];
        self.draw_plate.color = vec4(plate[0], plate[1], plate[2], plate[3] * mix);
        self.draw_label.color = vec4(ink[0], ink[1], ink[2], ink[3] * mix);
        self.draw_ramp.light = if light { 1.0 } else { 0.0 };
        self.draw_ramp.alpha = mix;

        cx.push_clip_rect(pane);
        self.draw_plate.draw_abs(cx, rect);
        self.draw_label.draw_abs(cx, start, &field.label);
        self.draw_label.draw_abs(cx, dvec2(start.x, row_y - low.y * 0.5), &lo);
        let bar_x = start.x + low.x + gap;
        self.draw_ramp.draw_abs(cx, Rect { pos: dvec2(bar_x, row_y - bar.y * 0.5), size: bar });
        self.draw_label.draw_abs(cx, dvec2(bar_x + bar.x + gap, row_y - high.y * 0.5), &hi);
        cx.pop_clip_rect();
    }
}

// Three significant digits, trimmed in place; scientific notation keeps extreme ranges short.
fn number(value: f32, unit: &str) -> String {
    let mut text = String::with_capacity(24 + unit.len());
    if value == 0.0 {
        text.push('0');
    } else if value.is_finite() {
        let value = f64::from(value);
        let mut exponent = value.abs().log10().floor() as i32;
        let mut digits = (value * 10.0_f64.powi(2 - exponent)).round();
        if digits.abs() >= 1000.0 {
            digits /= 10.0;
            exponent += 1;
        }
        let scientific = !(-3..3).contains(&exponent);
        let shown = digits * 10.0_f64.powi(if scientific { -2 } else { exponent - 2 });
        let decimals = if scientific { 2 } else { (2 - exponent).max(0) as usize };
        let _ = write!(text, "{shown:.decimals$}");
        if decimals > 0 {
            let end = text.trim_end_matches('0').trim_end_matches('.').len();
            text.truncate(end);
        }
        if scientific {
            let _ = write!(text, "e{exponent}");
        }
    } else {
        let _ = write!(text, "{value}");
    }
    if !unit.is_empty() {
        text.push(' ');
        text.push_str(unit);
    }
    text
}
