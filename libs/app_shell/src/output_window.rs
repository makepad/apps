//! The output window: a second window (a projector, the audience's screen)
//! showing one picture the product already renders, with no second
//! renderer: the product hands it a frame source ([`OutputWindow::set_source`])
//! and the window draws that source's frames as they come.
//!
//! The window has no caption on any host: it drags from anywhere and a
//! double click makes it fullscreen on its screen (and restores it). Over
//! the picture sits `mod.widgets.OutputOverlay` (nothing by default; a
//! product that draws over its picture, like Story's DECK in another
//! language, defines it after [`script_mod`] and reaches it through
//! [`OutputWindow::overlay`]).
//!
//! Main-loop cost: one frame stamp compare per event while the window is
//! open; nothing while it is closed.

use makepad_live_frame::{FrameBindError, FrameBinding, FrameDemand, FrameSource, LiveFrameViewWidgetRefExt};
use makepad_widgets::*;
use std::rc::Rc;

/// The overlay's default: nothing over the picture.
pub fn script_mod(vm: &mut ScriptVm) {
    script_eval!(vm, {
        use mod.prelude.widgets.*
        mod.widgets.OutputOverlay = View{visible: false width: 0 height: 0}
        true
    });
}

struct Picture {
    source: Rc<dyn FrameSource>,
    binding: FrameBinding,
    _demand: FrameDemand,
    stamp: Option<u64>,
}

#[derive(Default)]
pub struct OutputWindow {
    window: Option<WidgetRef>,
    picture: Option<Picture>,
    closing: bool,
    unavailable: bool,
    /// What the window shows (None: it says so and waits).
    source: Option<(Rc<dyn FrameSource>, FrameBinding)>,
}

impl OutputWindow {
    pub fn window_id(&self) -> Option<WindowId> {
        self.window.as_ref()?.as_window().window_id()
    }

    pub fn is_open(&self) -> bool {
        self.window.is_some() && !self.closing
    }

    /// The window's overlay widget (`mod.widgets.OutputOverlay`), while open.
    pub fn overlay(&self, cx: &mut Cx) -> Option<WidgetRef> {
        let window = self.window.as_ref()?;
        Some(window.widget(cx, ids!(output_overlay)))
    }

    /// Show `source` (None: nothing). The window, when open, binds it at its
    /// next sync; the old picture's texture is let go first.
    pub fn set_source(&mut self, cx: &mut Cx, source: Option<(Rc<dyn FrameSource>, FrameBinding)>) {
        self.source = source;
        if self.window.is_some() && !self.closing {
            self.detach(cx);
        }
    }

    pub fn has_source(&self) -> bool {
        self.source.is_some()
    }

    /// Open the window titled `title` (an open one stays as it is).
    pub fn open(&mut self, cx: &mut Cx, title: &str) {
        if self.window.is_some() {
            return;
        }
        let title = title.to_string();
        // A second Root would replace the global widget-tree root. This
        // independent Window uses Root's draw dispatch without registering a
        // new application root or moving the main UI's inspection identity.
        self.window = Some(cx.with_vm(|vm| {
            let value = script_eval!(vm, {
                use mod.prelude.widgets.*
                use mod.widgets.*
                Window{
                    window.title: #(title)
                    // The picture and nothing else: no caption on any host, in any state.
                    chrome: WindowChrome.None
                    // No caption to hold: it drags from anywhere (not while fullscreen,
                    // presenting). A double click makes it fullscreen on its screen (over
                    // bars and dock: the presenter's output) and again restores it exactly.
                    drag: WindowDrag.Window
                    double_click: WindowDoubleClick.Fullscreen
                    window.inner_size: vec2(960, 540)
                    pass +: {clear_color: #000}
                    body +: {
                        width: Fill height: Fill flow: Overlay
                        output_frame := LiveFrameView{
                            width: Fill height: Fill fit: FrameFit.Contain
                            draw_frame +: {color: #000}
                            draw_status +: {color: #c2ccd8}
                            status_align: Align{x: 0.5 y: 0.5}
                        }
                        output_overlay := mod.widgets.OutputOverlay{}
                    }
                }
            });
            WidgetRef::script_from_value(vm, value)
        }));
        self.closing = false;
        self.unavailable = false;
        self.sync_picture(cx);
        if let Some(window) = &self.window {
            window.redraw(cx);
        }
    }

    fn detach(&mut self, cx: &mut Cx) {
        if let Some(window) = &self.window {
            window.live_frame_view(cx, ids!(output_frame)).clear_frame(cx);
        }
        // Texture references are cleared before the provider lease is dropped.
        self.picture = None;
        self.unavailable = false;
    }

    pub fn close(&mut self, cx: &mut Cx) {
        if self.window.is_none() || self.closing {
            return;
        }
        self.closing = true;
        self.detach(cx);
        if let Some(window) = &self.window {
            window.as_window().close(cx);
        }
        // Keep the widget until its WindowClosed acknowledgement so a queued
        // close cannot outlive the Rust owner.
    }

    /// Binds the window's picture and says whether it moved since the last
    /// look (the window then redraws).
    fn sync_picture(&mut self, cx: &mut Cx) -> bool {
        if self.window.is_none() || self.closing {
            return false;
        }
        let window = self.window.as_ref().expect("window checked");
        let view = window.live_frame_view(cx, ids!(output_frame));
        if self.picture.is_none() {
            if let Some((source, binding)) = self.source.clone() {
                match source.hold(&binding) {
                    Ok(demand) => {
                        view.set_frame(cx, source.clone(), binding.clone());
                        self.picture = Some(Picture { source, binding, _demand: demand, stamp: None });
                        self.unavailable = false;
                    }
                    Err(error) => {
                        if !self.unavailable {
                            view.set_bind_error(cx, error);
                            self.unavailable = true;
                        }
                    }
                }
            }
            if self.picture.is_none() && !self.unavailable {
                view.set_bind_error(cx, FrameBindError::NoSession);
                self.unavailable = true;
            }
        }
        if let Some(picture) = &mut self.picture {
            let stamp = picture.source.frame_stamp(&picture.binding);
            if picture.stamp != stamp {
                picture.stamp = stamp;
                window.redraw(cx);
                return true;
            }
        }
        false
    }

    /// Call after the product's pump, so a new source retires the old
    /// texture before this window draws. `moved`: something over the
    /// picture (the overlay) changed and the window draws whole. The main
    /// window's close policy compares WindowId; closing this one stops
    /// nothing the product runs.
    pub fn handle_event(&mut self, cx: &mut Cx, event: &Event, moved: bool) {
        let Some(id) = self.window_id() else { return };
        if let Event::WindowClosed(closed) = event {
            if closed.window_id == id {
                self.detach(cx);
                self.window = None;
                self.closing = false;
                return;
            }
        }
        let moved = self.sync_picture(cx) || moved;
        if let Some(window) = &self.window {
            if let Event::Draw(draw) = event {
                // The picture changes inside the main window's draw (the
                // product records it there), and a redraw asked for during a
                // draw is dropped: the window draws its new picture in this
                // same draw, so it follows every frame of the picture.
                let whole;
                let draw = if moved {
                    whole = DrawEvent { redraw_all: true, ..draw.clone() };
                    &whole
                } else {
                    draw
                };
                let mut cx_draw = CxDraw::new(cx, draw);
                window.draw_all(&mut Cx2d::new(&mut cx_draw), &mut Scope::empty());
            } else {
                window.handle_event(cx, event, &mut Scope::empty());
            }
        }
        if let Event::WindowCloseRequested(request) = event {
            if request.window_id == id && request.accept_close.get() {
                self.closing = true;
                self.detach(cx);
            }
        }
    }
}
