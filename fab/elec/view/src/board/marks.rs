//! Screen-space finding markers and labels. Item tinting belongs to the host's item-state
//! texture; this overlay draws no board geometry. Unconnected findings draw nothing here: the
//! ratsnest line is that finding.

use super::checks::{marks_in, CheckMark};
use crate::canvas::Camera2d;
use fab_elec_circuit::CheckKind;
use fab_elec_core::{BBox, ItemId, Nm, Severity, P};
use makepad_widgets_core::*;

script_mod! {
    use mod.prelude.widgets_internal.*

    mod.draw.DrawBoardMark = set_type_default() do #(DrawBoardMark::script_shader(vm)) {
        ..mod.draw.DrawQuad
        depth_write: false
        color: #fff
        centre: 1.0
        pixel: fn() {
            let p = (self.pos - vec2(0.5, 0.5)) * self.rect_size
            let r = length(p)
            // Leave half a logical pixel outside the ring for its antialiasing fringe.
            let radius = min(self.rect_size.x, self.rect_size.y) * 0.5 - 1.25
            let ring = abs(r - radius) - 0.75
            let dot = r - 1.5
            let coverage = clamp(0.5 - mix(ring, min(ring, dot), self.centre) * self.draw_pass.dpi_factor, 0.0, 1.0)
            let alpha = coverage * self.color.a
            return vec4(self.color.rgb * alpha, alpha)
        }
    }

    mod.widgets.BoardMarkOverlay = set_type_default() do #(MarkOverlay::script_api(vm)) {
        draw_mark: mod.draw.DrawBoardMark {}
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

/// A 1.5 logical-pixel ring with an optional filled centre dot and an instance colour.
#[derive(Script, ScriptHook)]
#[repr(C)]
pub struct DrawBoardMark {
    #[deref]
    pub draw_super: DrawQuad,
    #[live]
    pub color: Vec4f,
    #[live]
    pub centre: f32,
}

/// A pane's `#[live]` draw helper, registered as `mod.widgets.BoardMarkOverlay`.
#[derive(Script, ScriptHook)]
pub struct MarkOverlay {
    #[live]
    draw_mark: DrawBoardMark,
    #[live]
    draw_plate: DrawColor,
    #[live]
    draw_label: DrawText,
}

impl MarkOverlay {
    /// Draw every visible marker at 12 logical pixels (`inks`: info, warning, error, plate). An
    /// error's label is always on; a warning's or an info's shows while the pointer is over one of
    /// its items (`hover`), so the board is not covered in tags. A label sits on a plate of the
    /// board's ground, so it reads over copper and silk, at the first free place right, left, above
    /// or below the marker; with none free it is skipped for this frame and the marker still draws.
    /// There is no marker count limit. Pending marks draw last as rings in `pending_ink`, without
    /// labels, plates or label reservations. O(visible marks) per draw, plus a binary search of a
    /// mark's items for the hovered one.
    pub fn draw(&mut self, cx: &mut Cx2d, marks: &[CheckMark], cam: &Camera2d, inks: [[f32; 4]; 4], pending_ink: [f32; 4], hover: Option<ItemId>) {
        if cam.view.size.x <= 0.0 || cam.view.size.y <= 0.0 {
            return;
        }
        let visible = cam.visible();
        // Camera world units are mm; the inclusive marker bounds are integer nm.
        let view = BBox {
            min: P::new((visible[0] * 1e6).ceil() as Nm, (visible[1] * 1e6).ceil() as Nm),
            max: P::new((visible[2] * 1e6).floor() as Nm, (visible[3] * 1e6).floor() as Nm),
        };
        let plate = inks[3];
        let pad = dvec2(4.0, 2.0);
        let mut taken: Vec<Rect> = Vec::new();
        let overlaps = |a: &Rect, b: &Rect| {
            a.pos.y < b.pos.y + b.size.y && b.pos.y < a.pos.y + a.size.y && a.pos.x < b.pos.x + b.size.x
                && b.pos.x < a.pos.x + a.size.x
        };
        cx.push_clip_rect(cam.view);
        self.draw_mark.centre = 1.0;
        for mark in marks_in(marks, view).filter(|mark| !mark.pending && mark.kind != CheckKind::Unconnected) {
            let at = cam.to_screen([f64::from(mark.at.x) / 1e6, f64::from(mark.at.y) / 1e6]);
            let ink = inks[match mark.severity {
                Severity::Info => 0,
                Severity::Warning => 1,
                Severity::Error => 2,
            }];
            let color = vec4(ink[0], ink[1], ink[2], ink[3]);
            self.draw_mark.color = color;
            let marker = Rect { pos: at - dvec2(6.0, 6.0), size: dvec2(12.0, 12.0) };
            self.draw_mark.draw_abs(cx, marker);
            taken.push(marker);
            let hovered = hover.is_some_and(|item| mark.items.binary_search(&item).is_ok());
            if mark.severity != Severity::Error && !hovered {
                continue;
            }

            let laid = self.draw_label.layout(cx, 0.0, 0.0, None, false, Align::default(), &mark.label);
            let text = dvec2(
                f64::from(laid.size_in_lpxs.width * self.draw_label.font_scale),
                f64::from(laid.size_in_lpxs.height * self.draw_label.font_scale),
            );
            let size = text + pad * 2.0;
            let places = [
                at + dvec2(9.0, -size.y * 0.5),
                at + dvec2(-9.0 - size.x, -size.y * 0.5),
                at + dvec2(-size.x * 0.5, -9.0 - size.y),
                at + dvec2(-size.x * 0.5, 9.0),
            ];
            let Some(rect) = places
                .into_iter()
                .map(|pos| Rect { pos, size })
                .find(|rect| !taken.iter().any(|earlier| overlaps(rect, earlier)))
            else {
                continue;
            };
            self.draw_plate.color = vec4(plate[0], plate[1], plate[2], plate[3]);
            self.draw_plate.draw_abs(cx, rect);
            self.draw_label.color = color;
            self.draw_label.draw_abs(cx, rect.pos + pad, &mark.label);
            taken.push(rect);
        }
        self.draw_mark.centre = 0.0;
        self.draw_mark.color = vec4(pending_ink[0], pending_ink[1], pending_ink[2], pending_ink[3]);
        for mark in marks_in(marks, view).filter(|mark| mark.pending && mark.kind != CheckKind::Unconnected) {
            let at = cam.to_screen([f64::from(mark.at.x) / 1e6, f64::from(mark.at.y) / 1e6]);
            self.draw_mark.draw_abs(cx, Rect { pos: at - dvec2(6.0, 6.0), size: dvec2(12.0, 12.0) });
        }
        cx.pop_clip_rect();
    }
}
