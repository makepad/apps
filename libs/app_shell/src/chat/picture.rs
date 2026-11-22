//! The picture that goes with the person's line: what the window shows
//! (the frame at the playhead, the shown slide, a desk's picture), read
//! back from the texture that shows it.
//!
//! Pictures go to the model as PNG, at most 1024 px a side, and only when
//! the model answering takes images; else the turn goes with its text
//! context.
//!
//! Main-loop cost: nothing while no turn waits; a turn's readback is a
//! ticket polled per pump, its pixels scaled and encoded once.

use makepad_widgets::*;

/// A picture's longest side for the model (the agent's image seam takes at
/// most 1024 px a side).
pub const EDGE: usize = 1024;
/// How long a turn waits for the frame before it goes without it.
pub const GRAB_WAIT: f64 = 1.5;

/// Pixels: tight top-down RGBA, opaque.
#[derive(Clone)]
pub struct Pixels {
    pub width: usize,
    pub height: usize,
    pub rgba: Vec<u8>,
}

impl Pixels {
    /// At most `edge` a side (area-averaged); itself when it fits.
    pub fn fit(&self, edge: usize) -> Pixels {
        if self.width <= edge && self.height <= edge {
            return self.clone();
        }
        let scale = edge as f64 / self.width.max(self.height) as f64;
        let (w, h) = (((self.width as f64 * scale).round() as usize).max(1), ((self.height as f64 * scale).round() as usize).max(1));
        let mut rgba = vec![0u8; w * h * 4];
        for y in 0..h {
            let (y0, y1) = (y * self.height / h, ((y + 1) * self.height / h).max(y * self.height / h + 1).min(self.height));
            for x in 0..w {
                let (x0, x1) = (x * self.width / w, ((x + 1) * self.width / w).max(x * self.width / w + 1).min(self.width));
                let mut sum = [0u32; 4];
                for sy in y0..y1 {
                    let row = sy * self.width * 4;
                    for sx in x0..x1 {
                        let p = &self.rgba[row + sx * 4..row + sx * 4 + 4];
                        for c in 0..4 {
                            sum[c] += p[c] as u32;
                        }
                    }
                }
                let n = ((y1 - y0) * (x1 - x0)).max(1) as u32;
                let at = (y * w + x) * 4;
                for c in 0..4 {
                    rgba[at + c] = (sum[c] / n) as u8;
                }
            }
        }
        Pixels { width: w, height: h, rgba }
    }

    pub fn png(&self) -> Result<Vec<u8>, String> {
        Cx::encode_rgba_as_png(self.width as u32, self.height as u32, &self.rgba)
    }

}

/// Ask for `texture`'s pixels (the viewer's picture as it shows).
fn grab(cx: &mut Cx, texture: &Texture) -> Result<ReadbackTicket, ReadbackError> {
    texture.read_back(cx, ReadbackRequest::default())
}

/// The pixels of `ticket`, once they are back.
pub fn grabbed(cx: &mut Cx, ticket: ReadbackTicket) -> Option<Result<Pixels, String>> {
    let result = cx.try_take_texture_readbacks_for(&[ticket]).into_iter().next()?;
    Some(result.data.map_err(|error| format!("{error:?}")).map(|data| {
        let mut rgba = data[..result.width * result.height * 4].to_vec();
        for px in rgba.chunks_exact_mut(4) {
            px[3] = 255;
        }
        Pixels { width: result.width, height: result.height, rgba }
    }))
}

/// A picture being read back: the readback (none: the viewer had no
/// picture) and since when.
pub struct Waiting {
    pub ticket: Option<ReadbackTicket>,
    asking: Option<Texture>,
    wait_logged: bool,
    pub since: f64,
    pub failed: Option<String>,
}

impl Waiting {
    pub fn new(cx: &mut Cx, texture: Option<Texture>) -> Waiting {
        let failed = texture.is_none().then(|| "the viewer shows no picture yet".into());
        let mut waiting = Waiting { ticket: None, asking: texture, wait_logged: false, since: Cx::time_now(), failed };
        waiting.ask(cx);
        waiting
    }

    fn ask(&mut self, cx: &mut Cx) {
        let Some(texture) = &self.asking else { return };
        match grab(cx, texture) {
            Ok(ticket) => {
                self.wait_logged = false;
                self.asking = None;
                self.ticket = Some(ticket);
                self.since = Cx::time_now();
            }
            Err(ReadbackError::Backpressure) => {
                if !self.wait_logged {
                    log!("chat: context readback request limit reached; picture for the turn waits");
                    self.wait_logged = true;
                }
                // The recurring window pump retries Waiting; requesting a
                // frame keeps that pump at its active cadence while we wait.
                cx.new_next_frame();
            }
            Err(error) => {
                self.asking = None;
                self.failed = Some(format!("the picture could not be read ({error:?})"));
            }
        }
    }

    /// The frame, once back (or `Err` why there is none); `None` while it
    /// is on its way.
    pub fn poll(&mut self, cx: &mut Cx) -> Option<Result<Pixels, String>> {
        self.ask(cx);
        if let Some(why) = &self.failed {
            return Some(Err(why.clone()));
        }
        let ticket = self.ticket?;
        if let Some(result) = grabbed(cx, ticket) {
            self.ticket = None;
            return Some(result);
        }
        if Cx::time_now() - self.since > GRAB_WAIT {
            cx.cancel_texture_readback(ticket);
            self.ticket = None;
            return Some(Err("the picture took too long to read".into()));
        }
        cx.new_next_frame();
        None
    }
}

