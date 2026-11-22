//! Mono capture from a sample ring to the board microphone, on the run clock.

use crate::{mic_gain, mic_sample, TrackInput};
use fab_elec_mcu::convert::{float_to_i16, Resampler};
use fab_elec_mcu_parts::SoundTrack;
use fab_elec_sim::input::InputSource;
use fab_elec_sim::listen::{RingProducer, SampleRing};
use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, Mutex, TryLockError};

/// The callback owns this end. Dropping it lets the worker finish the last sample and close.
pub struct MicWriter {
    producer: RingProducer,
    port: Arc<MicPort>,
    limit: usize,
    overflowing: bool,
}

/// Allocated off the callback. A device can start writing here before reporting its rate.
pub struct MicPort {
    ring: Arc<SampleRing>,
    prefix: Option<Arc<MicPort>>,
    overflows: Arc<AtomicU64>,
    state: AtomicU8,
}

const COPYING: u8 = 1;
const STOPPED: u8 = 2;

impl MicPort {
    pub fn new(rate: u32) -> Result<Arc<Self>, &'static str> {
        Ok(Arc::new(Self { ring: SampleRing::new(four_seconds(rate)?), prefix: None,
            overflows: Arc::default(), state: AtomicU8::new(0) }))
    }

    /// Takes the sole writer without allocating or waiting.
    pub fn writer(self: &Arc<Self>, rate: u32) -> Option<MicWriter> {
        let limit = four_seconds(rate).ok()?;
        if self.state.load(Ordering::Acquire) != 0 { return None; }
        let producer = self.ring.claim()?;
        Some(MicWriter { producer, port: self.clone(), limit, overflowing: false })
    }

    pub fn stop(&self) { self.state.fetch_or(STOPPED, Ordering::Release); }

    pub fn overflows(&self) -> u64 { self.overflows.load(Ordering::Relaxed) }
}

fn four_seconds(rate: u32) -> Result<usize, &'static str> {
    if rate == 0 { return Err("the microphone input rate must be non-zero"); }
    let count = usize::try_from(u64::from(rate) * 4).map_err(|_| "microphone ring size exceeds addressable space")?;
    if count.checked_next_power_of_two().is_none_or(|n| n > isize::MAX as usize / std::mem::size_of::<f32>()) {
        return Err("microphone ring size exceeds addressable space");
    }
    Ok(count)
}

impl Drop for MicWriter {
    fn drop(&mut self) { self.port.stop(); }
}

impl MicWriter {
    /// Continues on a larger ring once it is ready for the negotiated rate.
    pub fn move_to(&mut self, mut next: MicWriter) {
        next.overflowing = self.overflowing;
        *self = next;
    }

    /// Copies one mono buffer. Consecutive losses count once until a sample fits again.
    pub fn push(&mut self, samples: &[f32]) {
        if samples.is_empty() { return; }
        if self.port.state.compare_exchange(0, COPYING, Ordering::Acquire, Ordering::Relaxed).is_err() { return; }
        let prefix = self.port.prefix.as_ref().map_or(0, |p| p.ring.fill());
        let written = self.producer.push_slice(samples, self.limit.saturating_sub(prefix));
        let full = written < samples.len();
        if full && (!self.overflowing || written > 0) {
            self.port.overflows.fetch_add(1, Ordering::Relaxed);
        }
        self.overflowing = full;
        self.port.state.fetch_and(!COPYING, Ordering::Release);
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct MicStatus {
    pub overflows: u64,
    /// How many times the run began holding for this track.
    pub holds: u64,
    /// How much more audio the next step needs, excluding the run's requested lead.
    pub behind_s: f64,
}

/// The run polls this source before each step. Only the worker handles track segments.
pub struct MicInput {
    pub input: TrackInput<i16>,
    port: Arc<MicPort>,
    pump: Mutex<Pump>,
    gain: AtomicU64,
    holds: AtomicU64,
    behind: AtomicU64,
}

impl MicInput {
    /// A fresh open track at the codec rate and four seconds of queue at the input rate.
    pub fn new(
        first: Arc<MicPort>, source_rate: u32, rate: u32, level_db: f64, origin_ps: u64, refdes: String,
    ) -> Result<Arc<Self>, String> {
        if !(8000..=48000).contains(&rate) {
            return Err(format!("output rate must be 8000..=48000 Hz, got {rate}"));
        }
        let gain = mic_gain(level_db)?;
        let capacity = four_seconds(source_rate)?;
        let port = if first.ring.capacity() >= capacity { first } else {
            Arc::new(MicPort { ring: SampleRing::new(capacity), overflows: first.overflows.clone(),
                prefix: Some(first), state: AtomicU8::new(0) })
        };
        let track = Arc::new(SoundTrack::new(origin_ps, (rate, 1)));
        Ok(Arc::new(Self {
            input: TrackInput::new(track, refdes),
            port,
            pump: Mutex::new(Pump {
                resampler: Resampler::new(source_rate, rate),
                pending: Vec::new(), written: 0, prefix_drained: false,
            }),
            gain: AtomicU64::new(gain.to_bits()),
            holds: AtomicU64::new(0), behind: AtomicU64::new(0),
        }))
    }

    pub fn port(&self) -> &Arc<MicPort> { &self.port }

    /// Stops admitting buffers immediately, even while the platform retires the callback.
    pub fn stop(&self) {
        self.port.stop();
        if let Some(prefix) = &self.port.prefix { prefix.stop(); }
    }

    /// Finishes a stopped capture on the task pool if the run paused before its next poll.
    pub fn finish(&self) {
        self.stop();
        let mut pump = self.pump.lock().expect("microphone pump poisoned");
        let gain = f64::from_bits(self.gain.load(Ordering::Relaxed));
        while !self.input.track.is_closed() {
            pump.drain(&self.port, &self.input.track, gain);
            if !self.input.track.is_closed() { std::thread::yield_now(); }
        }
    }

    /// Applies to samples the worker converts from here on.
    pub fn set_level_db(&self, db: f64) -> Result<(), String> {
        self.gain.store(mic_gain(db)?.to_bits(), Ordering::Relaxed);
        Ok(())
    }

    pub fn status(&self) -> MicStatus {
        MicStatus {
            overflows: self.port.overflows(),
            holds: self.holds.load(Ordering::Relaxed),
            behind_s: if self.input.track.is_closed() { 0.0 } else { f64::from_bits(self.behind.load(Ordering::Relaxed)) },
        }
    }
}

impl InputSource for MicInput {
    fn horizon(&self) -> Option<f64> { self.input.horizon() }
    fn period(&self) -> f64 { self.input.period() }
    fn name(&self) -> String { self.input.name() }
    fn ask(&self, want: f64) { self.input.ask(want); }

    fn poll_horizon(&self, required: f64) -> Option<f64> {
        if !self.input.track.is_closed() {
            match self.pump.try_lock() {
                Ok(mut pump) => {
                    let gain = f64::from_bits(self.gain.load(Ordering::Relaxed));
                    pump.drain(&self.port, &self.input.track, gain);
                }
                Err(TryLockError::WouldBlock) => {}
                Err(TryLockError::Poisoned(_)) => panic!("microphone pump poisoned"),
            }
        }
        let horizon = self.horizon();
        let behind = horizon.map_or(0.0, |at| (required - at).max(0.0));
        if self.behind.load(Ordering::Relaxed) != behind.to_bits() {
            let previous = self.behind.swap(behind.to_bits(), Ordering::Relaxed);
            if behind > 0.0 && previous == 0 { self.holds.fetch_add(1, Ordering::Relaxed); }
        }
        horizon
    }
}

struct Pump {
    resampler: Resampler,
    // A reader can have a segment when we try to append. Keep the unpublished suffix in order.
    pending: Vec<i16>,
    written: usize,
    prefix_drained: bool,
}

impl Pump {
    fn publish(&mut self, track: &SoundTrack) -> bool {
        while self.written < self.pending.len() {
            let n = track.try_append(&self.pending[self.written..]);
            if n == 0 { return false; }
            self.written += n;
        }
        self.pending.clear();
        self.written = 0;
        true
    }

    fn drain(&mut self, port: &MicPort, track: &SoundTrack, gain: f64) {
        if let Some(prefix) = port.prefix.as_ref().filter(|_| !self.prefix_drained) {
            self.prefix_drained = self.drain_port(prefix, track, gain);
            if !self.prefix_drained { return; }
        }
        if self.drain_port(port, track, gain) {
            self.resampler.finish(|s| self.pending.push(mic_sample(s, gain)));
            if self.publish(track) { track.close(); }
        }
    }

    /// True once the writer has left this ring and its last complete periods are published.
    fn drain_port(&mut self, port: &MicPort, track: &SoundTrack, gain: f64) -> bool {
        if !self.publish(track) { return false; }
        // The producer's drop follows its last write. Drain at most what was here at entry.
        let ended = port.state.load(Ordering::Acquire) == STOPPED;
        let ring = &port.ring;
        let mut remaining = ring.fill();
        if remaining == 0 { return ended; }
        let mut buffer = [0.0; 512];
        while remaining > 0 {
            let n = remaining.min(buffer.len());
            let popped = ring.pop(&mut buffer[..n]);
            for &sample in &buffer[..popped] {
                self.resampler.push(float_to_i16(sample), |s| self.pending.push(mic_sample(s, gain)));
            }
            remaining -= popped;
            if !self.publish(track) { return false; }
        }
        ended
    }
}

