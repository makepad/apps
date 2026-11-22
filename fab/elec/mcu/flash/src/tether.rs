//! Pictures over the USB tether: a [`Peer`] that streams RGB565 pictures to firmware on the frame
//! channel, and the frame format that firmware reads (the handheld's pictures firmware,
//! `fixtures/handheld_pictures.fw.splash`). Each Ethernet II frame carries two panel rows.

use crate::watch::Peer;
use fab_elec_mcu_netgw::Link;
use fab_elec_mcu_rt::watch::{Event, Frame, ETH_MAX};
use std::{io::{self, Write}, time::Duration};

/// IEEE 802's local experimental EtherType: these frames are for this program only.
pub const ETHERTYPE: u16 = 0x88b5;
/// Destination and source of the handheld fixture frames, in wire order.
pub(crate) const ADDRESSES: [u8; 12] = [2, 0, 0, 0, 0, 1, 2, 0, 0, 0, 0, 2];
/// The frame kind for picture rows ('P').
pub const KIND_ROWS: u8 = b'P';
/// Panel rows per frame.
pub const ROWS: usize = 2;
/// Destination, source, EtherType, kind, serial, first row (u16, little-endian), two zero bytes.
pub const HEADER: usize = 20;
/// The handheld's panel, in pixels.
pub const PANEL: (usize, usize) = (368, 448);
/// Frames queued ahead of the device's credits: enough to keep its eight receive buffers busy,
/// few enough that a device reset drops little.
pub(crate) const AHEAD: usize = 16;

/// The frames for one picture of `width`-pixel rows: each carries `ROWS` rows of big-endian RGB565
/// after the header, in row order. `pixels` holds whole frames' rows and a frame fits `ETH_MAX`.
pub fn picture_frames(serial: u8, width: usize, pixels: &[u16]) -> Vec<Vec<u8>> {
    let band = width * ROWS;
    assert!(HEADER + band * 2 <= ETH_MAX, "{ROWS} rows of {width} pixels exceed an Ethernet frame");
    assert!(band > 0 && pixels.len() % band == 0, "a picture is whole bands of {ROWS} rows");
    pixels.chunks_exact(band).enumerate().map(|(n, rows)| {
        let y = u16::try_from(n * ROWS).expect("a panel row fits u16");
        let mut frame = Vec::with_capacity(HEADER + band * 2);
        frame.extend_from_slice(&ADDRESSES);
        frame.extend_from_slice(&ETHERTYPE.to_be_bytes());
        frame.extend_from_slice(&[KIND_ROWS, serial]);
        frame.extend_from_slice(&y.to_le_bytes());
        frame.extend_from_slice(&[0, 0]);
        for pixel in rows { frame.extend_from_slice(&pixel.to_be_bytes()); }
        frame
    }).collect()
}

/// `rgba` (`w` x `h`) centred on a `width` x `height` panel as RGB565, each channel truncated as
/// firmware images are: cropped where it overflows, black where it does not reach. Alpha is ignored.
pub fn centred(rgba: &[u8], w: usize, h: usize, width: usize, height: usize) -> Vec<u16> {
    assert_eq!(rgba.len(), w * h * 4, "RGBA holds w x h pixels");
    let mut pixels = vec![0u16; width * height];
    for y in 0..height {
        let Some(sy) = (y + h / 2).checked_sub(height / 2).filter(|&sy| sy < h) else { continue };
        for x in 0..width {
            let Some(sx) = (x + w / 2).checked_sub(width / 2).filter(|&sx| sx < w) else { continue };
            let at = (sy * w + sx) * 4;
            pixels[y * width + x] = ((rgba[at] as u16 >> 3) << 11) | ((rgba[at + 1] as u16 >> 2) << 5) | (rgba[at + 2] as u16 >> 3);
        }
    }
    pixels
}

/// Streams pictures in turn, each `hold` on the panel before the next starts, round and round.
/// A device reset (a new Hello) starts the current picture again on the cleared panel.
pub struct Pictures {
    pictures: Vec<Vec<Vec<u8>>>,
    hold: Duration,
    current: usize,
    next: usize,
    live: bool,
    held_from: Option<Duration>,
    shown: u64,
    frames: u64,
    restarts: u64,
}

impl Pictures {
    /// `pictures` are panel-sized RGB565 pictures; their serials are their places in the list.
    pub fn new(pictures: &[Vec<u16>], hold: Duration) -> Self {
        assert!(!pictures.is_empty(), "at least one picture");
        let pictures = pictures.iter().enumerate()
            .map(|(n, pixels)| picture_frames(n as u8, PANEL.0, pixels)).collect();
        Self { pictures, hold, current: 0, next: 0, live: false, held_from: None, shown: 0, frames: 0, restarts: 0 }
    }
}

impl Peer for Pictures {
    fn turn(&mut self, link: &mut Link, now: Duration, frame: Option<&Frame>) {
        if let Some(Frame { event: Event::Hello { .. }, .. }) = frame {
            if self.live { self.restarts += 1; }
            self.live = true;
            self.next = 0;
            self.held_from = None;
        }
        if !self.live { return; }
        let frames = &self.pictures[self.current];
        while self.next < frames.len() && link.stats().tether.queue_len < AHEAD {
            link.queue_frame(&frames[self.next]).expect("picture frames have Ethernet lengths");
            self.next += 1;
            self.frames += 1;
        }
        if self.next < frames.len() || link.stats().tether.queue_len > 0 { return; }
        let held_from = *self.held_from.get_or_insert_with(|| { self.shown += 1; now });
        if now.saturating_sub(held_from) >= self.hold {
            self.current = (self.current + 1) % self.pictures.len();
            self.next = 0;
            self.held_from = None;
        }
    }

    fn summary(&self, output: &mut dyn Write) -> io::Result<()> {
        writeln!(output, "tether pictures_shown={} frames_queued={} restarts={}", self.shown, self.frames, self.restarts)
    }
}
