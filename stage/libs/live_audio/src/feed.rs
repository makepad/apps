//! A deck source fed from outside the audio thread: a mixer that cannot run
//! on the device's thread (it allocates, it evaluates a document: an edit's
//! whole mix) writes 48 kHz stereo frames a little ahead into a
//! [`FeedRing`], and a deck plays them ([`DeckCommand::LoadFeed`]). The
//! deck brings what every source has: its dip around starts and stops, its
//! resampling to the device's rate and its observations.
//!
//! The writer restarts the ring for a new start (a seek, a play from
//! elsewhere): the deck dips out of what it played, if anything sounds,
//! and begins exactly where the new start begins. The deck publishes how
//! far it has played, the sound's clock ([`FeedRing::played`]).
//!
//! [`DeckCommand::LoadFeed`]: crate::deck::DeckCommand::LoadFeed

use relax_attr::relax;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;

/// The rate a feed is written at.
pub const FEED_RATE: u32 = 48_000;

/// The ring between a feed's writer and the deck that plays it.
pub struct FeedRing {
    /// Interleaved stereo f32 bits.
    samples: Box<[AtomicU32]>,
    frames: usize,
    /// Absolute frame the writer writes next.
    write: AtomicU64,
    /// The current start's generation and its first frame (`start` is
    /// stored before `generation`).
    generation: AtomicU64,
    start: AtomicU64,
    /// Where the deck reads (absolute frames, the lowest it may still read),
    /// and frames of the current start played (the sound's clock).
    read: AtomicU64,
    played: AtomicU64,
    /// Frames the deck played as silence because they were not written yet
    /// (an underrun: the writer fell behind), since the ring was made.
    starved: AtomicU64,
}

#[relax(O3, always)]
impl FeedRing {
    /// A ring of `frames` stereo frames (at [`FEED_RATE`]).
    pub fn new(frames: usize) -> Arc<Self> {
        Arc::new(Self {
            samples: (0..frames.max(2) * 2).map(|_| AtomicU32::new(0)).collect(),
            frames: frames.max(2),
            write: AtomicU64::new(0),
            generation: AtomicU64::new(0),
            start: AtomicU64::new(0),
            read: AtomicU64::new(0),
            played: AtomicU64::new(0),
            starved: AtomicU64::new(0),
        })
    }

    pub fn capacity(&self) -> usize {
        self.frames
    }

    /// A new start: what the deck plays next begins at the write head.
    pub fn restart(&self) -> u64 {
        let start = self.write.load(Ordering::Relaxed);
        self.start.store(start, Ordering::Release);
        // `read` stays where the deck is until it takes the new start: the
        // frames it still dips through are not written over.
        self.played.store(0, Ordering::Release);
        self.generation.fetch_add(1, Ordering::AcqRel) + 1
    }

    /// Frames written and not yet read (the writer keeps this ahead; after
    /// a restart, until the deck takes it, the old start's unread rest
    /// counts too).
    pub fn queued(&self) -> u64 {
        self.write.load(Ordering::Acquire).saturating_sub(self.read.load(Ordering::Acquire))
    }

    /// Room for more frames.
    pub fn room(&self) -> u64 {
        (self.frames as u64).saturating_sub(self.queued() + 1)
    }

    /// Append interleaved stereo frames (at most [`Self::room`]).
    pub fn push(&self, interleaved: &[f32]) {
        let write = self.write.load(Ordering::Relaxed);
        let n = (interleaved.len() / 2).min(self.room() as usize);
        for (i, pair) in interleaved.chunks_exact(2).take(n).enumerate() {
            let at = ((write + i as u64) as usize % self.frames) * 2;
            self.samples[at].store(pair[0].to_bits(), Ordering::Relaxed);
            self.samples[at + 1].store(pair[1].to_bits(), Ordering::Relaxed);
        }
        self.write.store(write + n as u64, Ordering::Release);
    }

    /// Frames of the current start played, at [`FEED_RATE`].
    pub fn played(&self) -> u64 {
        self.played.load(Ordering::Acquire)
    }

    /// Frames played as silence while playing because the writer had not
    /// written them yet (underruns), in all.
    pub fn starved(&self) -> u64 {
        self.starved.load(Ordering::Relaxed)
    }

    fn frame(&self, frame: u64) -> [f32; 2] {
        let at = (frame as usize % self.frames) * 2;
        [f32::from_bits(self.samples[at].load(Ordering::Relaxed)), f32::from_bits(self.samples[at + 1].load(Ordering::Relaxed))]
    }
}

/// A deck's feed: the ring and where the deck is in it.
pub struct FeedSource {
    ring: Arc<FeedRing>,
    generation: u64,
    /// Absolute fractional frame read next.
    position: f64,
    start: u64,
    /// Starts and stops dip (live); without, the frames pass as written
    /// (an offline render that must equal the mix).
    ramp: bool,
}

#[relax(O3, always)]
impl FeedSource {
    pub fn new(ring: Arc<FeedRing>, ramp: bool) -> Self {
        let generation = ring.generation.load(Ordering::Acquire);
        let start = ring.start.load(Ordering::Acquire);
        Self { ring, generation, position: start as f64, start, ramp }
    }

    pub fn ring(&self) -> &Arc<FeedRing> {
        &self.ring
    }

    /// Whether a render would play something: frames written ahead of the
    /// read position, or a new start to take. Nothing else makes a sound.
    pub fn pending(&self) -> bool {
        self.ring.generation.load(Ordering::Acquire) != self.generation
            || self.position.floor() as u64 + 1 < self.ring.write.load(Ordering::Acquire)
    }

    /// Seconds played of the current start.
    pub fn seconds(&self) -> f64 {
        (self.position - self.start as f64).max(0.0) / FEED_RATE as f64
    }

    /// One pass: `play`ing, the dip `gain`, at the device's `rate`.
    pub(crate) fn render(&mut self, gain: &mut f32, play: bool, rate: f32, frames: usize, mut write: impl FnMut(usize, [f32; 2])) {
        let ramp = self.ramp;
        let step = 1.0 / crate::deck::DECK_DIP_FRAMES as f32;
        let generation = self.ring.generation.load(Ordering::Acquire);
        let fresh = generation != self.generation;
        // A new start: dipped out of the old one first, unless silent.
        if fresh && (*gain <= 0.0 || !ramp) {
            self.generation = generation;
            self.start = self.ring.start.load(Ordering::Acquire);
            self.position = self.start as f64;
        }
        let mut switching = generation != self.generation;
        let written = self.ring.write.load(Ordering::Acquire);
        let advance = FEED_RATE as f64 / rate.max(1.0) as f64;
        let mut starved = 0;
        for frame in 0..frames {
            // Dipped out of the old start: take the new one here.
            if switching && *gain <= 0.0 {
                switching = false;
                self.generation = generation;
                self.start = self.ring.start.load(Ordering::Acquire);
                self.position = self.start as f64;
            }
            let target = if play && !switching { 1.0 } else { 0.0 };
            *gain = if !ramp { target } else if *gain < target { (*gain + step).min(target) } else { (*gain - step).max(target) };
            let at = self.position.floor();
            let index = at as u64;
            // Not written yet: silence, and the clock waits.
            if *gain <= 0.0 || index + 1 >= written {
                if *gain > 0.0 && play && !switching {
                    starved += 1;
                }
                write(frame, [0.0; 2]);
                continue;
            }
            let f = (self.position - at) as f32;
            let (a, b) = (self.ring.frame(index), self.ring.frame(index + 1));
            let value = [(a[0] + (b[0] - a[0]) * f) * *gain, (a[1] + (b[1] - a[1]) * f) * *gain];
            write(frame, value);
            self.position += advance;
        }
        if starved > 0 {
            self.ring.starved.fetch_add(starved, Ordering::Relaxed);
        }
        // While dipping out of an old start the old frames stay read; once
        // switched, from the new start on.
        self.ring.read.store(self.position.floor() as u64, Ordering::Release);
        if !switching {
            self.ring.played.store((self.position - self.start as f64).max(0.0) as u64, Ordering::Release);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ramp_ring(frames: usize) -> Arc<FeedRing> {
        let ring = FeedRing::new(frames * 4);
        ring.restart();
        let samples: Vec<f32> = (0..frames).flat_map(|i| [i as f32 / 1e4, -(i as f32) / 1e4]).collect();
        ring.push(&samples);
        ring
    }

    #[test]
    fn at_the_feed_rate_without_a_ramp_the_frames_pass_exactly() {
        let ring = ramp_ring(4096);
        let mut source = FeedSource::new(ring.clone(), false);
        let mut gain = 0.0;
        let mut out = Vec::new();
        source.render(&mut gain, true, 48_000.0, 1024, |_, v| out.push(v));
        for (i, v) in out.iter().enumerate() {
            assert_eq!(v[0].to_bits(), (i as f32 / 1e4).to_bits(), "frame {i}");
        }
        assert_eq!(ring.played(), 1024);
    }

    #[test]
    fn a_new_start_dips_out_and_begins_where_it_begins() {
        let ring = ramp_ring(2048);
        let mut source = FeedSource::new(ring.clone(), true);
        let mut gain = 0.0;
        let mut last = 0.0f32;
        let mut worst = 0.0f32;
        let mut collect = |v: [f32; 2]| { worst = worst.max((v[0] - last).abs()); last = v[0]; };
        source.render(&mut gain, true, 48_000.0, 512, |_, v| collect(v));
        // The writer restarts at its write head (frame 2048) with a level jump.
        ring.restart();
        ring.push(&vec![0.5; 2 * 1024]);
        source.render(&mut gain, true, 48_000.0, 600, |_, v| collect(v));
        assert!(worst < 0.5 / crate::deck::DECK_DIP_FRAMES as f32 * 1.5 + 1e-3, "a click: {worst}");
        assert_eq!(last, 0.5);
        assert!(ring.played() > 0);
    }

    #[test]
    fn what_is_not_written_waits_in_silence() {
        let ring = ramp_ring(100);
        let mut source = FeedSource::new(ring.clone(), false);
        let mut gain = 0.0;
        let mut out = Vec::new();
        source.render(&mut gain, true, 48_000.0, 256, |_, v| out.push(v));
        assert!(out[200..].iter().all(|v| *v == [0.0; 2]));
        assert_eq!(ring.played(), 99);
    }
}
