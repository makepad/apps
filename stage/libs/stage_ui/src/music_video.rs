//! The DJ music video: a deck's attached video behind the optional karaoke,
//! or a themed fallback when the music has no picture.
//!
//! The picture is a `LiveFrameView` (Contain: the whole frame, aspect kept),
//! so it inherits that view's texture safeguards: a cleared or hidden view
//! lets its frame texture go, and a released one keeps its binding for the
//! next draw. The host binds the frame; this widget has no clock, capture,
//! audio or status text of its own.
//!
//! Registered by `makepad_stage_ui::script_mod`, which the host calls after
//! `makepad_live_frame::script_mod`: the template composes
//! `LiveFrameView`.

use makepad_live_frame::{FrameBinding, FrameSource, LiveFrameViewWidgetRefExt};
use makepad_widgets::*;
use std::rc::Rc;

script_mod! {
    use mod.prelude.widgets_internal.*
    use mod.widgets.*
    let stage = mod.stage

    mod.widgets.StageMusicVideo = set_type_default() do #(StageMusicVideo::register_widget(vm)) {
        width: Fill height: Fill flow: Overlay
        // The themed fallback: shown behind the picture's letterbox, and alone
        // for music without video.
        fallback := View{
            width: Fill height: Fill show_bg: true
            draw_bg +: {
                color: stage.wave_bg
                pixel: fn() {
                    let centre = self.pos - vec2(0.5, 0.45)
                    let glow = 1.0 - clamp(length(centre * vec2(1.0, 1.4)) * 1.6, 0.0, 1.0)
                    return vec4(self.color.rgb * (0.55 + 0.45 * glow), self.color.a)
                }
            }
        }
        picture := View{
            width: Fill height: Fill visible: false
            frame := LiveFrameView{
                width: Fill height: Fill
                fit: FrameFit.Contain
                // Letterbox and not-yet-ready are transparent: the fallback
                // shows through. The view's own status text is not shown.
                draw_frame +: {color: #0000}
                draw_status +: {color: #0000}
            }
        }
        // The host's optional karaoke, over the picture.
        lyrics := View{width: Fill height: Fill}
    }
}

#[derive(Script, ScriptHook, Widget)]
pub struct StageMusicVideo {
    #[deref]
    view: View,
    /// A frame binding is held (shown, or hidden with its texture released).
    #[rust]
    bound: bool,
}

impl Widget for StageMusicVideo {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope);
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        self.view.draw_walk(cx, scope, walk)
    }
}

impl StageMusicVideo {
    fn frame(&self, cx: &Cx) -> makepad_live_frame::LiveFrameViewRef {
        self.view.widget(cx, ids!(frame)).as_live_frame_view()
    }
}

impl StageMusicVideoRef {
    /// Show `binding` of `source`: the picture replaces the fallback.
    pub fn set_frame(&self, cx: &mut Cx, source: Rc<dyn FrameSource>, binding: FrameBinding) {
        let Some(mut inner) = self.borrow_mut() else { return };
        inner.frame(cx).set_frame(cx, source, binding);
        inner.view.widget(cx, ids!(picture)).set_visible(cx, true);
        inner.bound = true;
        inner.view.redraw(cx);
    }

    /// No picture: the frame texture and binding go, the fallback shows.
    pub fn clear_frame(&self, cx: &mut Cx) {
        let Some(mut inner) = self.borrow_mut() else { return };
        inner.frame(cx).clear_frame(cx);
        inner.view.widget(cx, ids!(picture)).set_visible(cx, false);
        inner.bound = false;
        inner.view.redraw(cx);
    }

    /// The widget is hidden: let the frame texture go at once, keeping the
    /// binding, so a reshow draws the provider's current picture afresh.
    pub fn release_frame_texture(&self, cx: &mut Cx) {
        let Some(inner) = self.borrow() else { return };
        inner.frame(cx).release_frame_texture(cx);
    }

    /// Whether a frame binding is held.
    pub fn has_frame(&self) -> bool {
        self.borrow().is_some_and(|inner| inner.bound)
    }

    /// The overlay slot for the host's optional karaoke.
    pub fn lyrics(&self, cx: &Cx) -> WidgetRef {
        self.borrow().map_or_else(WidgetRef::empty, |inner| inner.view.widget(cx, ids!(lyrics)))
    }
}
