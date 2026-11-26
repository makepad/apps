//! Story's output window (the shell's [`OutputWindow`]): DECK's picture on
//! a second window (a projector, the audience's screen), with no second
//! renderer: the deck's own frame source feeds it, and DECK's render in
//! another language draws over it (the window's overlay is
//! `DeckOutputView`).

use makepad_app_shell::output_window::OutputWindow;
use makepad_live_frame::{FrameBinding, FrameSource};
use makepad_widgets::*;
use std::rc::Rc;

/// The output window's overlay in Story: DECK in another language.
pub(crate) fn script_mod(vm: &mut ScriptVm) {
    makepad_app_shell::output_window::script_mod(vm);
    script_eval!(vm, {
        mod.widgets.OutputOverlay = mod.widgets.DeckOutputView{}
        true
    });
}

#[derive(Default)]
pub(crate) struct StoryOutput {
    window: OutputWindow,
    /// The deck's picture, while DECK is the page; none: the window waits.
    feed: Option<Rc<dyn FrameSource>>,
    /// DECK's render in another language, drawn over the fed picture.
    deck_lane: Option<crate::deck::output_lane::SharedOutput>,
    /// The deck lane's request serial the window last saw.
    lane_seen: u64,
}

impl StoryOutput {
    /// Show `feed` in the window (None: nothing until a page feeds it). A
    /// window already open switches at its next sync.
    pub(crate) fn set_feed(&mut self, cx: &mut Cx, feed: Option<(Rc<dyn FrameSource>, FrameBinding)>) {
        let same = match (&self.feed, &feed) {
            (None, None) => true,
            (Some(old), Some((new, _))) => Rc::ptr_eq(old, new),
            _ => false,
        };
        if same {
            return;
        }
        self.feed = feed.as_ref().map(|(source, _)| source.clone());
        self.window.set_source(cx, feed);
    }

    /// DECK's render in another language (None: the picture as fed).
    pub(crate) fn set_deck_lane(&mut self, cx: &mut Cx, lane: Option<crate::deck::output_lane::SharedOutput>) {
        self.deck_lane = lane;
        self.attach_deck_lane(cx);
    }

    fn attach_deck_lane(&mut self, cx: &mut Cx) {
        let Some(view) = self.window.overlay(cx) else { return };
        let lane = self.deck_lane.clone();
        if let Some(mut view) = view.borrow_mut::<crate::deck::output_lane::DeckOutputView>() {
            view.set_shared(cx, lane);
        };
    }

    pub(crate) fn window_id(&self) -> Option<WindowId> {
        self.window.window_id()
    }

    pub(crate) fn is_open(&self) -> bool {
        self.window.is_open()
    }

    pub(crate) fn toggle(&mut self, cx: &mut Cx) -> Result<(), String> {
        if self.window.window_id().is_some() {
            self.window.close(cx);
            return Ok(());
        }
        if !self.window.has_source() {
            return Err("The output window shows the deck: open DECK first".into());
        }
        self.window.open(cx, "Makepad Story — Output");
        self.attach_deck_lane(cx);
        Ok(())
    }

    pub(crate) fn close(&mut self, cx: &mut Cx) {
        self.window.close(cx);
    }

    /// DECK's render in another language has a new request since the
    /// window last drew.
    fn lane_moved(&mut self) -> bool {
        let Some(lane) = &self.deck_lane else { return false };
        let serial = lane.borrow().serial;
        serial != std::mem::replace(&mut self.lane_seen, serial)
    }

    pub(crate) fn handle_event(&mut self, cx: &mut Cx, event: &Event) {
        if self.window.window_id().is_none() {
            return;
        }
        let moved = self.lane_moved();
        self.window.handle_event(cx, event, moved);
    }
}
