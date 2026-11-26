//! DECK's picture as a frame source: the texture the player shows now,
//! for any view that binds frames (the output window).

use makepad_live_frame::{FrameBindError, FrameBinding, FrameDemand, FrameProviderId, FrameRead, FrameSource};
use makepad_widgets::Texture;
use std::cell::RefCell;
use std::rc::Rc;

/// The one output a deck has.
struct Bound;

#[derive(Default)]
struct Shown {
    texture: Option<Texture>,
    stamp: u64,
}

/// The player writes the picture it shows; readers bind and read it. One
/// token for a deck: a new document keeps the source and changes
/// the picture.
pub struct DeckFrames {
    provider: FrameProviderId,
    shown: RefCell<Shown>,
    /// The pixel size another window shows the picture at (the output
    /// window): the slides are recorded at least that large.
    reader_pixels: std::cell::Cell<Option<(u32, u32)>>,
}

impl DeckFrames {
    pub fn new() -> Rc<DeckFrames> {
        Rc::new(DeckFrames { provider: FrameProviderId::default(), shown: RefCell::new(Shown::default()), reader_pixels: std::cell::Cell::new(None) })
    }

    /// The picture now showing (None: black, or nothing yet). A changed
    /// picture or `repainted` bumps the stamp readers redraw on.
    pub fn show(&self, texture: Option<Texture>, repainted: bool) {
        let mut shown = self.shown.borrow_mut();
        if repainted || shown.texture != texture {
            shown.stamp += 1;
            // Readers (the output window) look at the stamp on their next
            // event: there is one even when nothing else moves. Without a
            // reader nobody looks (the signal is an app-wide event).
            if self.reader_pixels.get().is_some() {
                makepad_widgets::SignalToUI::set_ui_signal();
            }
        }
        shown.texture = texture;
    }

    /// The pixel size the output window shows the picture at (None: no
    /// such window).
    pub fn set_reader_pixels(&self, pixels: Option<(u32, u32)>) {
        self.reader_pixels.set(pixels);
    }

    pub fn reader_pixels(&self) -> Option<(u32, u32)> {
        self.reader_pixels.get()
    }

    /// The picture now showing, to read back.
    pub fn texture(&self) -> Option<Texture> {
        self.shown.borrow().texture.clone()
    }

    /// The binding to this source's picture.
    pub fn binding(&self) -> FrameBinding {
        FrameBinding::new(&self.provider, Bound)
    }
}

impl FrameSource for DeckFrames {
    fn bind(&self, _instance: &str, _node: &str, _port: &str) -> Result<FrameBinding, FrameBindError> {
        Ok(self.binding())
    }

    fn read(&self, binding: &FrameBinding) -> FrameRead {
        if binding.data::<Bound>(&self.provider).is_none() {
            return FrameRead::Invalid(FrameBindError::StaleBinding);
        }
        match &self.shown.borrow().texture {
            Some(texture) => FrameRead::Ready(texture.clone()),
            None => FrameRead::Pending,
        }
    }

    fn frame_stamp(&self, binding: &FrameBinding) -> Option<u64> {
        binding.data::<Bound>(&self.provider)?;
        Some(self.shown.borrow().stamp)
    }

    fn hold(&self, binding: &FrameBinding) -> Result<FrameDemand, FrameBindError> {
        binding.data::<Bound>(&self.provider).ok_or(FrameBindError::StaleBinding)?;
        Ok(FrameDemand::new(()))
    }
}
