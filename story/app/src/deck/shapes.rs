//! The rounded, flat-coloured rectangle the play bar draws its parts with.

use makepad_widgets::*;

/// The gap between the picture and the bar under it.
pub const STRIP_GAP: f64 = 6.0;

#[derive(Script, ScriptHook)]
#[repr(C)]
pub struct DrawDeckUi {
    #[deref]
    draw_super: DrawQuad,
    #[live(vec4(1.0, 1.0, 1.0, 1.0))]
    pub color: Vec4f,
    #[live(4.0)]
    pub radius: f32,
}

impl DrawDeckUi {
    /// A rounded rectangle.
    pub fn fill(&mut self, cx: &mut Cx2d, rect: Rect, color: Vec4f, radius: f64) {
        self.color = color;
        self.radius = radius as f32;
        self.draw_abs(cx, rect);
    }
}

script_mod! {
    use mod.prelude.widgets_internal.*
    use mod.widgets.*

    // A rounded, flat-coloured rectangle (the play bar's parts).
    set_type_default() do #(DrawDeckUi::script_shader(vm)){
        ..mod.draw.DrawQuad
        pixel: fn() {
            let sdf = Sdf2d.viewport(self.pos * self.rect_size)
            sdf.box(0.0, 0.0, self.rect_size.x, self.rect_size.y, max(self.radius, 0.5))
            sdf.fill(vec4(self.color.xyz * self.color.w, self.color.w))
            return sdf.result
        }
    }
}
