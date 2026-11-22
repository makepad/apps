//! A device's own sound in a live run: what an attached process plays on its speaker
//! (`CoSim::take_audio`, given to the sinks as `Sink::co_audio`), resampled to the output device's
//! rate and pushed into a ring of its own ([`DeviceTap`]), and the ring's consumer for the audio
//! callback ([`DevicePlayer`]). The net tap's ring, grid and recording are not involved: the
//! callback sums the two players.
//!
//! The sound is committed before it is heard: a device runs ahead of the analog privately and may
//! run a stretch again, so what reaches the tap is never taken back. A run that goes back gives the
//! sound of the stretch it runs again once more; the tap skips everything at or before the time it
//! has pushed to, so no sample is heard twice.

use crate::cosim::{CoAudio, CoAudioRun};
use crate::listen::{soft_limit, PacedProducer, SampleRing, TapMix, TapStats, FADE_SECONDS, KEEPS_UP};
use crate::stamp::UnknownInfo;
use crate::Sink;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Kernel taps of the resampler: input samples each output sample weighs (half before, half after
/// its position).
const TAPS: usize = 32;

/// Phases of the kernel's table; an output between two phases interpolates their rows.
const PHASES: usize = 256;

/// The kernel's passband edge as a share of the lower of the two rates' Nyquist frequencies: a
/// 32-tap Kaiser (beta 8) kernel then holds everything above the lower Nyquist under -60 dB.
const CUTOFF: f64 = 0.85;
const KAISER_BETA: f64 = 8.0;

/// A stream counts as ended once a point of the run is this far past its last sample with nothing
/// new: its kernel's tail is pushed (the last samples heard), and sound after it starts a new
/// stream. Committed sound trails the analog by one of the device's stretches, far less than this;
/// a stream that went on after all starts afresh at its next sample, losing nothing.
const END_SECONDS: f64 = 0.02;

/// Sound gathered before it plays while the run is slower than real time: the player is silent
/// until the ring holds this much (or the run stopped), then plays until the ring is dry. At 0.5x
/// that is about 1 s of sound and 1 s of silence; at 0.9x, 5 s and 0.6 s. A taste constant for the
/// ear.
pub const STRETCH_SECONDS: f64 = 0.5;

/// How often the player takes the run's speed reading, in seconds of its output.
const SPEED_SECONDS: f64 = 0.5;

/// How long the first sound waits for the consumer to say its rate (a device just opened) before
/// it plays at 48 kHz; on the worker, as the net tap waits.
const RATE_WAIT: Duration = Duration::from_millis(250);

/// A windowed-sinc resampler from an exact rational input rate (`num / den` Hz) to an integer
/// output rate, its read position exact integer arithmetic (an input index and a remainder in
/// units of `1 / (out_hz * den)` input samples), never an accumulating float. Per output sample
/// O(TAPS); no allocation after `new`.
#[derive(Clone)]
struct Resampler {
    /// The input step per output sample, `num / (out_hz * den)`, as `step / unit`.
    step: u64,
    unit: u64,
    /// `(PHASES + 1) * TAPS` weights: row `p` is the kernel at fractional offset `p / PHASES`.
    table: Vec<f32>,
    /// The last `TAPS` input samples, round, and how many came in all.
    hist: [f32; TAPS],
    taken: u64,
    /// The next output's position: input index `at` plus `rem / unit`.
    at: u64,
    rem: u64,
}

impl Resampler {
    fn new(rate: (u64, u64), out_hz: u32) -> Resampler {
        let (num, den) = rate;
        let unit = u64::from(out_hz) * den;
        let (step, unit) = reduce(num, unit);
        let in_hz = num as f64 / den as f64;
        // the cutoff as a share of the input's Nyquist
        let c = CUTOFF * (f64::from(out_hz) / in_hz).min(1.0);
        let i0b = bessel_i0(KAISER_BETA);
        let half = TAPS as f64 / 2.0;
        let mut table = Vec::with_capacity((PHASES + 1) * TAPS);
        for p in 0..=PHASES {
            let frac = p as f64 / PHASES as f64;
            for j in 0..TAPS {
                // tap j weighs input `at - (TAPS/2 - 1) + j`: its distance from the position
                let x = j as f64 - (half - 1.0) - frac;
                let r = x / half;
                let w = if r.abs() < 1.0 { bessel_i0(KAISER_BETA * (1.0 - r * r).sqrt()) / i0b } else { 0.0 };
                table.push((c * sinc(c * x) * w) as f32);
            }
        }
        Resampler { step, unit, table, hist: [0.0; TAPS], taken: 0, at: 0, rem: 0 }
    }

    /// Takes one input sample and appends every output it completes (an output needs the samples
    /// up to `TAPS / 2` after its position).
    fn push(&mut self, x: f32, out: &mut impl FnMut(f32)) {
        self.hist[(self.taken % TAPS as u64) as usize] = x;
        self.taken += 1;
        while self.at + TAPS as u64 / 2 < self.taken {
            out(self.output());
            self.advance();
        }
    }

    /// The outputs whose positions lie before the input's end, the samples after it taken as
    /// silence: the stream's last sound. O(TAPS) per output.
    fn finish(&mut self, out: &mut impl FnMut(f32)) {
        let end = self.taken;
        while self.at < end {
            let pad = self.at + TAPS as u64 / 2 + 1;
            while self.taken < pad {
                self.hist[(self.taken % TAPS as u64) as usize] = 0.0;
                self.taken += 1;
            }
            out(self.output());
            self.advance();
        }
    }

    fn advance(&mut self) {
        self.rem += self.step;
        self.at += self.rem / self.unit;
        self.rem %= self.unit;
    }

    /// The output at the current position from the newest `TAPS` inputs (those before the stream
    /// count as silence).
    fn output(&self) -> f32 {
        let pos = self.rem as f64 / self.unit as f64 * PHASES as f64;
        let p = (pos as usize).min(PHASES - 1);
        let w = (pos - p as f64) as f32;
        let (a, b) = (&self.table[p * TAPS..(p + 1) * TAPS], &self.table[(p + 1) * TAPS..(p + 2) * TAPS]);
        // the first tap's input index
        let first = self.at as i64 - (TAPS as i64 / 2 - 1);
        let mut y = 0.0f32;
        for j in 0..TAPS {
            let k = first + j as i64;
            if k < 0 {
                continue;
            }
            let x = self.hist[(k as u64 % TAPS as u64) as usize];
            y += x * (a[j] + w * (b[j] - a[j]));
        }
        y
    }
}

fn reduce(a: u64, b: u64) -> (u64, u64) {
    let (mut x, mut y) = (a, b);
    while y != 0 {
        (x, y) = (y, x % y);
    }
    (a / x, b / x)
}

fn sinc(x: f64) -> f64 {
    if x == 0.0 {
        1.0
    } else {
        let px = std::f64::consts::PI * x;
        px.sin() / px
    }
}

/// The modified Bessel function of the first kind, order zero, by its power series.
fn bessel_i0(x: f64) -> f64 {
    let (mut sum, mut term, q) = (1.0, 1.0, x * x / 4.0);
    let mut k = 1.0;
    while term > 1e-12 * sum {
        term *= q / (k * k);
        sum += term;
        k += 1.0;
    }
    sum
}

/// One stream of sound being resampled: its rate, the run time of its first sample and the time
/// of the sample after its last.
struct Stream {
    rate: (u64, u64),
    out_hz: u32,
    t0: f64,
    samples: u64,
    resampler: Resampler,
}

impl Stream {
    fn period(&self) -> f64 {
        self.rate.1 as f64 / self.rate.0 as f64
    }

    /// The run time of sample `k` of the stream.
    fn time(&self, k: u64) -> f64 {
        self.t0 + k as f64 * self.period()
    }
}

/// The sink that hears an attached process's speaker: installed through Listen under an id of its
/// own, beside or without the net tap. Each committed run is resampled to the consumer's rate and
/// pushed through the paced wait, so the worker runs no faster than the consumer plays, as with
/// the net tap. Runs that follow each other (the same rate, the next sample's time within half a
/// sample) are one stream; a gap, a new rate, the consumer's new rate or a run that starts over
/// ends the stream (its kernel's tail is pushed) and the next sound starts a new one. A run that
/// went back gives sound it already pushed: what lies at or before the pushed time is skipped.
/// Per input sample O(1), per output O(TAPS); no allocation but a stream's kernel table.
pub struct DeviceTap {
    ring: Arc<SampleRing>,
    paced: PacedProducer,
    /// Seconds kept ahead of the consumer before waiting, until the player sets the lead.
    latency: f64,
    stream: Option<Stream>,
    /// The consumer's rate was known (or waited for) at the first sound.
    rate_known: bool,
    /// The last point's time while the run steps.
    last_t: Option<f64>,
}

impl DeviceTap {
    pub fn new(ring: Arc<SampleRing>, latency: f64) -> DeviceTap {
        DeviceTap { paced: PacedProducer::new(ring.clone()), ring, latency, stream: None, rate_known: false, last_t: None }
    }

    /// The cumulative sample and waiting counts; constant time.
    pub fn stats(&self) -> &TapStats {
        self.paced.stats()
    }

    fn out_hz(&mut self) -> u32 {
        if !self.rate_known {
            let since = Instant::now();
            while self.ring.device_rate() == 0 && self.ring.is_open() && since.elapsed() < RATE_WAIT {
                std::thread::sleep(Duration::from_millis(1));
            }
            self.rate_known = true;
        }
        match self.ring.device_rate() {
            0 => 48_000,
            hz => hz,
        }
    }

    fn lead(&self, out_hz: u32) -> usize {
        (self.latency * f64::from(out_hz)).ceil().max(1.0).min(self.ring.capacity() as f64) as usize
    }

    /// Pushes the stream's last sound and drops it.
    fn end_stream(&mut self) {
        let Some(mut s) = self.stream.take() else { return };
        let lead = self.lead(s.out_hz);
        let paced = &mut self.paced;
        if paced.claim() {
            s.resampler.finish(&mut |y| paced.queue(y, lead));
        }
    }

    /// One run's samples into the stream: those at or before what was pushed skipped, a gap or a
    /// rate change starting a new stream.
    fn take_run(&mut self, run: &CoAudioRun, samples: &[i16]) {
        let out_hz = self.out_hz();
        let lead = self.lead(out_hz);
        // follows on (or overlaps what was pushed: a run that went back) or not (a gap, a new rate)
        let heard = match &self.stream {
            Some(s) if s.rate == run.rate && s.out_hz == out_hz && run.t0 <= s.time(s.samples) + 0.5 * s.period() => {
                Some(((s.time(s.samples) - run.t0) / s.period()).round().max(0.0) as usize)
            }
            Some(_) => None,
            None => Some(0),
        };
        let skip = match heard {
            Some(n) => n.min(samples.len()),
            None => {
                self.end_stream();
                0
            }
        };
        let samples = &samples[skip..];
        if samples.is_empty() || !self.paced.claim() {
            return;
        }
        let stream = self.stream.get_or_insert_with(|| Stream { rate: run.rate, out_hz, t0: run.t0, samples: 0, resampler: Resampler::new(run.rate, out_hz) });
        stream.samples += samples.len() as u64;
        let paced = &mut self.paced;
        paced.set_running(true);
        for &x in samples {
            stream.resampler.push(f32::from(x) / 32768.0, &mut |y| paced.queue(y, lead));
        }
    }
}

impl Sink for DeviceTap {
    fn begin(&mut self, _unknowns: &[UnknownInfo]) {
        // a run that starts over: its times start over, and what was pushed of the last is done
        self.end_stream();
        self.idle();
        self.paced.restart_speed();
        self.last_t = None;
        self.paced.claim();
    }

    fn point(&mut self, t: f64, _outer: Option<f64>, _x: &[f64]) {
        if self.last_t.is_some_and(|last| t < last) {
            self.last_t = None;
        }
        if let Some(last) = self.last_t {
            self.paced.stepped(t - last);
        }
        self.last_t = Some(t);
        self.paced.point();
        if self.stream.as_ref().is_some_and(|s| t > s.time(s.samples) + END_SECONDS) {
            self.end_stream();
        }
    }

    fn co_audio(&mut self, audio: &CoAudio) {
        for (run, samples) in audio.each() {
            self.take_run(run, samples);
        }
    }

    fn started(&mut self) {
        self.paced.started();
    }

    fn clock_wait(&mut self, wall: std::time::Duration) {
        self.paced.clock_wait(wall);
    }

    fn listened(&self) -> bool {
        self.paced.listened()
    }

    fn idle(&mut self) {
        self.paced.rest();
        self.paced.set_running(false);
    }

    fn steady(&mut self, _at: f64, _x: &[f64]) {
        self.idle();
    }

    /// The run went back: its stream goes on, and the sound it gives again up to what was pushed
    /// is skipped.
    fn rewind(&mut self, _at: f64, _saved: Option<&dyn crate::SinkState>, _unknowns: &[UnknownInfo]) {
        self.last_t = None;
    }

    /// The speaker is heard as the device plays: a take's recording holds none of it.
    fn live_only(&self) -> bool {
        true
    }
}

/// The consumer of a device ring, for the audio callback beside the net [`crate::listen::Player`].
/// Two states on the run's measured speed (the ring's, taken every [`SPEED_SECONDS`] of output):
///
/// - Live, at or above [`KEEPS_UP`]: a lead of two device buffers (before the first reading) or
///   one, a fade into a gap ([`FADE_SECONDS`]), and play again as soon as a buffer is ready, over
///   a fade in. The ring's lead is two device buffers.
/// - Stretches, below it: silent until the ring holds [`STRETCH_SECONDS`] (or the run stopped),
///   then play from a fade in until the ring is dry and fade. Every sample the device played is
///   heard once, in order, at its pitch, later than the screen shows it. The ring's lead is its
///   capacity less a buffer, so a run slower than real time never waits on a player that holds (a
///   hold the paced wait would take for a gone consumer).
///
/// `fill` is O(samples), with no lock, wait or allocation.
pub struct DevicePlayer {
    primed: bool,
    /// The last sample heard and the samples its fade has left.
    last: f32,
    fade_left: usize,
    /// The device's largest buffer so far.
    frames: usize,
    /// The speed reading in use (0: the run has none yet) and the output since it was taken.
    speed: f32,
    window: usize,
    /// The ring's lead is the one Stretches set.
    stretched: bool,
    /// The last buffer was heard to its end, and the samples a fade in has left.
    heard: bool,
    fade_in: usize,
}

impl Default for DevicePlayer {
    fn default() -> Self {
        Self::new()
    }
}

impl DevicePlayer {
    pub fn new() -> DevicePlayer {
        DevicePlayer { primed: false, last: 0.0, fade_left: 0, frames: 0, speed: 0.0, window: 0, stretched: false, heard: false, fade_in: 0 }
    }

    /// Whether it plays in stretches: the reading in use is below [`KEEPS_UP`].
    pub fn stretches(&self) -> bool {
        self.speed > 0.0 && self.speed < KEEPS_UP
    }

    /// Fills `out` (one channel at `rate` Hz) from `ring`.
    pub fn fill(&mut self, ring: &SampleRing, out: &mut [f32], rate: f64) {
        let fade = ((rate * FADE_SECONDS) as usize).max(1);
        if !ring.is_open() {
            // closed: what it still holds is drained unheard, so a later open starts fresh
            while ring.pop(out) > 0 {}
            out.fill(0.0);
            *self = DevicePlayer::new();
            return;
        }
        self.frames = self.frames.max(out.len());
        // the speed: a run that started over has none until its own first reading
        let reading = ring.tap_speed();
        self.window += out.len();
        if reading == 0.0 || self.speed == 0.0 || self.window as f64 >= SPEED_SECONDS * rate {
            (self.speed, self.window) = (reading, 0);
        }
        let stretches = self.stretches();
        let floor = 2 * self.frames;
        if stretches {
            let lead = ring.capacity().saturating_sub(self.frames).max(floor);
            if ring.ahead() != lead {
                ring.set_ahead(lead);
            }
        } else if self.stretched || ring.ahead() < floor {
            ring.set_ahead(floor);
        }
        self.stretched = stretches;
        let fill = ring.fill();
        let running = ring.running();
        self.primed &= running;
        // Stretches gather their length (below half the lead, so the run never waits on the
        // hold); Live waits for two buffers at a run's start (no reading yet), else one; a stopped
        // producer can have a last partial buffer waiting
        let buffers = if self.speed == 0.0 { 2 } else { 1 };
        let need = if stretches {
            ((STRETCH_SECONDS * rate) as usize).min(ring.ahead() / 2)
        } else {
            (buffers * out.len()).min(ring.ahead())
        };
        self.primed |= fill > 0 && (!running || fill >= need);
        let got = if self.primed { ring.pop(out) } else { 0 };
        if got > 0 {
            // sound after silence (a stretch, play after a gap) comes in over a fade, not a step
            if !self.heard {
                self.fade_in = fade;
            }
            for s in &mut out[..got] {
                if self.fade_in == 0 {
                    break;
                }
                *s *= (fade - self.fade_in + 1) as f32 / fade as f32;
                self.fade_in -= 1;
            }
            (self.last, self.fade_left) = (out[got - 1], fade);
        }
        self.heard = got == out.len();
        if got < out.len() {
            if self.primed && running {
                // a stretch's end is its plan, a live gap an underrun
                if !stretches {
                    ring.note_underrun();
                }
                self.primed = false;
            }
            for s in &mut out[got..] {
                *s = if self.fade_left > 0 {
                    self.fade_left -= 1;
                    self.last * self.fade_left as f32 / fade as f32
                } else {
                    0.0
                };
            }
        }
    }
}

impl DevicePlayer {
    /// The output's sum: `out` (the net player's buffer) plus the device ring's sound at its level
    /// and master, soft limited, while the device ring is open and not muted; `out` untouched
    /// otherwise (a closed ring is drained unheard). `scratch` is the caller's buffer, made at
    /// start: `out` is filled in pieces of its length. O(samples), no lock, wait or allocation.
    pub fn mix_into(&mut self, ring: &SampleRing, mix: &TapMix, out: &mut [f32], scratch: &mut [f32], rate: f64) {
        if scratch.is_empty() {
            return;
        }
        if !ring.is_open() {
            let n = out.len().min(scratch.len());
            self.fill(ring, &mut scratch[..n], rate);
            return;
        }
        let gain = if mix.muted() { 0.0 } else { mix.level(0) * mix.master() };
        for piece in out.chunks_mut(scratch.len()) {
            let device = &mut scratch[..piece.len()];
            self.fill(ring, device, rate);
            for (o, &d) in piece.iter_mut().zip(device.iter()) {
                *o = soft_limit(f64::from(*o) + f64::from(d * gain)) as f32;
            }
        }
    }
}
