//! An attached device's sound through its own ring: the device tap's rate conversion stays exact
//! over a long run (F5), a run that goes back, its sound changed by an edit, is heard once (F8),
//! and a run slower than real time is heard in stretches, each sample once (F7), the player
//! following the speed both ways without a click (F9).

use fab_elec_sim::cosim::{CoAudio, CoAudioRun};
use fab_elec_sim::device_audio::{DevicePlayer, DeviceTap};
use fab_elec_sim::listen::{SampleRing, DEVICE_RING_SECONDS, FADE_SECONDS, KEEPS_UP};
use fab_elec_sim::Sink;
use std::f64::consts::TAU;
use std::sync::Arc;

const IN_HZ: u64 = 16_000;

/// The kernel's half length: an output needs the input samples this far past its position.
const HALF: u64 = 16;

/// A ring the producer never waits on in these single-threaded proofs (its lead is the ring),
/// open at `out_hz`.
fn ring(out_hz: u32) -> Arc<SampleRing> {
    let ring = SampleRing::new(1 << 17);
    ring.set_device_rate(out_hz);
    ring.set_open(true);
    ring.set_ahead(ring.capacity() / 2);
    ring
}

/// Samples `from..to` of a tone at `hz`, as the device's I2S frames give them.
fn tone(hz: f64, from: u64, to: u64) -> Vec<i16> {
    (from..to).map(|n| (12_000.0 * (TAU * hz * n as f64 / IN_HZ as f64).sin()).round() as i16).collect()
}

/// Gives the tap `samples` (the device's sound from sample `first` on) as committed runs of the
/// lengths in `cuts` (round), each followed by the run's point at its end.
fn feed(tap: &mut DeviceTap, samples: &[i16], first: u64, cuts: &[usize], mut after: impl FnMut(u64)) {
    let mut audio = CoAudio::default();
    let (mut at, mut i) = (0, 0);
    while at < samples.len() {
        let len = cuts[i % cuts.len()].min(samples.len() - at);
        let n = first + at as u64;
        audio.runs.push(CoAudioRun { t0: n as f64 / IN_HZ as f64, rate: (IN_HZ, 1), len });
        audio.samples.extend_from_slice(&samples[at..at + len]);
        tap.co_audio(&audio);
        audio.clear();
        at += len;
        tap.point((first + at as u64) as f64 / IN_HZ as f64, None, &[]);
        after(first + at as u64);
        i += 1;
    }
}

/// Everything the ring holds.
fn drain(ring: &SampleRing, out: &mut Vec<f32>) {
    let mut buf = [0.0f32; 1024];
    loop {
        let n = ring.pop(&mut buf);
        if n == 0 {
            return;
        }
        out.extend_from_slice(&buf[..n]);
    }
}

#[test]
fn the_device_tap_keeps_its_count_exact_at_44_1_khz() {
    // F5: 20 s of 16 kHz sound in committed runs of uneven lengths, played at 44.1 kHz by a
    // consumer that pops a 10 ms buffer whenever two are queued. After every run the samples
    // pushed are exactly those whose positions the input reaches (an output at input position
    // k * 160 / 441 needs the input up to HALF samples past it): an exact identity at every step,
    // so the count cannot drift however long the run goes on.
    const CD_HZ: u32 = 44_100;
    const CD_BUFFER: usize = 441;
    let ring = ring(CD_HZ);
    let mut tap = DeviceTap::new(ring.clone(), 0.02);
    let mut player = DevicePlayer::new();
    let mut buf = vec![0.0f32; CD_BUFFER];
    let mut consumed = 0u64;
    tap.begin(&[]);
    let n = 20 * IN_HZ;
    let samples = tone(440.0, 0, n);
    let expected = |taken: u64| (taken.saturating_sub(HALF) * u64::from(CD_HZ)).div_ceil(IN_HZ);
    let mut worst = 0i64;
    feed(&mut tap, &samples, 0, &[3, 4, 3, 5, 17, 2, 4, 3], |taken| {
        worst = worst.max((ring.produced() as i64 - expected(taken) as i64).abs());
        // the run keeps up (the tap's own reading of a loaded box could say otherwise)
        ring.set_tap_speed(4.0);
        while ring.fill() >= 2 * CD_BUFFER {
            player.fill(&ring, &mut buf, f64::from(CD_HZ));
            consumed += CD_BUFFER as u64;
        }
    });
    assert_eq!(worst, 0, "the samples pushed after each run are those its input reaches");
    // the stream ends once the run is past its last sample: the kernel's tail is pushed
    tap.point(n as f64 / IN_HZ as f64 + 0.05, None, &[]);
    tap.idle();
    assert_eq!(ring.produced(), (n * u64::from(CD_HZ)).div_ceil(IN_HZ) as usize, "20 s at 16 kHz is 882 000 samples at 44.1 kHz");
    while ring.fill() > 0 {
        player.fill(&ring, &mut buf, f64::from(CD_HZ));
        consumed += CD_BUFFER as u64;
    }
    assert!(consumed >= ring.produced() as u64 && consumed < ring.produced() as u64 + CD_BUFFER as u64, "the consumer plays all of it: {consumed}");
    assert_eq!(ring.underruns(), 0, "the consumer never ran dry while the run went on");
    assert_eq!((tap.stats().dropped_closed, tap.stats().dropped_stalled), (0, 0), "nothing dropped");
}

#[test]
fn a_run_that_goes_back_is_heard_once() {
    // F8: the device plays 440 Hz; at 0.5 s an edit sends the run back to 0.3 s, and from there
    // the device plays 660 Hz. What was pushed up to 0.5 s stays as it was; the new sound is heard
    // from where the pushed sound ends. So the output is bit for bit that of a run that played
    // 440 Hz to 0.5 s and 660 Hz after, with no sample twice.
    let (back, front, end) = (3 * IN_HZ / 10, IN_HZ / 2, IN_HZ);
    let cuts = [3, 4, 3, 5, 17, 2, 4, 3];
    let low = tone(440.0, 0, front);
    let high = tone(660.0, back, end);

    let reference = ring(OUT_HZ);
    let mut tap = DeviceTap::new(reference.clone(), 0.02);
    tap.begin(&[]);
    let mut expected = Vec::new();
    let mut joined = low.clone();
    joined.extend_from_slice(&high[(front - back) as usize..]);
    feed(&mut tap, &joined, 0, &cuts, |_| drain(&reference, &mut expected));
    tap.point(end as f64 / IN_HZ as f64 + 0.05, None, &[]);
    drain(&reference, &mut expected);

    let edited = ring(OUT_HZ);
    let mut tap = DeviceTap::new(edited.clone(), 0.02);
    tap.begin(&[]);
    let mut heard = Vec::new();
    feed(&mut tap, &low, 0, &cuts, |_| drain(&edited, &mut heard));
    let before = edited.produced();
    tap.rewind(back as f64 / IN_HZ as f64, None, &[]);
    // the stretch run again up to the front gives nothing new
    feed(&mut tap, &high[..(front - back) as usize], back, &[7, 2, 5], |_| drain(&edited, &mut heard));
    assert_eq!(edited.produced(), before, "nothing at or before the pushed time is pushed again");
    feed(&mut tap, &high[(front - back) as usize..], front, &[7, 2, 5], |_| drain(&edited, &mut heard));
    tap.point(end as f64 / IN_HZ as f64 + 0.05, None, &[]);
    drain(&edited, &mut heard);

    assert_eq!(heard.len(), expected.len(), "as many samples as the run without the edit's repeat");
    let first = heard.iter().zip(&expected).position(|(a, b)| a.to_bits() != b.to_bits());
    assert_eq!(first, None, "every sample is the joined run's");
}

/// A rising sweep from 200 Hz to `top` Hz over `n` samples at 16 kHz, starting off zero, so each
/// stretch of it is its own.
fn sweep(n: u64, top: f64) -> Vec<i16> {
    let span = n as f64 / IN_HZ as f64;
    (0..n).map(|k| {
        let t = k as f64 / IN_HZ as f64;
        let phase = 0.4 + TAU * (200.0 * t + (top - 200.0) * t * t / (2.0 * span));
        (12_000.0 * phase.sin()).round() as i16
    }).collect()
}

/// What the tap pushes for `samples` with nothing holding it: the sound a slower run must play.
fn pushed(samples: &[i16]) -> Vec<f32> {
    let ring = ring(OUT_HZ);
    let mut tap = DeviceTap::new(ring.clone(), 0.02);
    tap.begin(&[]);
    let mut all = Vec::new();
    feed(&mut tap, samples, 0, &[80], |_| drain(&ring, &mut all));
    tap.point(samples.len() as f64 / IN_HZ as f64 + 0.05, None, &[]);
    drain(&ring, &mut all);
    all
}

const OUT_HZ: u32 = 48_000;
/// The player's device buffer: 10 ms.
const BUFFER: usize = 480;

/// What a player at 48 kHz on a simulated clock hears of `samples` from a run whose speed is
/// `speeds` (each the device sample it lasts to and the speed): for each 10 ms buffer the run
/// gives `speed` x 10 ms of sound in runs of 5 ms, never past the ring's lead (the paced wait,
/// which one thread cannot sit in), its speed reading `speed`. The ring is the device ring's size.
/// The output, per buffer whether the run was below KEEPS_UP and whether the player played in
/// stretches, and the tap's drops (closed, stalled).
fn heard_at(samples: &[i16], speeds: &[(u64, f32)]) -> (Vec<f32>, Vec<(bool, bool)>, (u64, u64)) {
    let ring = SampleRing::new((DEVICE_RING_SECONDS * f64::from(OUT_HZ)) as usize);
    ring.set_device_rate(OUT_HZ);
    ring.set_open(true);
    let mut tap = DeviceTap::new(ring.clone(), 0.02);
    let mut player = DevicePlayer::new();
    tap.begin(&[]);
    let n = samples.len() as u64;
    let speed_at = |k: u64| speeds.iter().find(|(end, _)| k < *end).map_or(speeds[speeds.len() - 1].1, |s| s.1);
    let mut audio = CoAudio::default();
    let (mut at, mut owed, mut ended) = (0u64, 0.0f64, false);
    let (mut out, mut states) = (Vec::new(), Vec::new());
    let mut buf = [0.0f32; BUFFER];
    ring.set_tap_speed(speed_at(0));
    loop {
        player.fill(&ring, &mut buf, f64::from(OUT_HZ));
        out.extend_from_slice(&buf);
        let speed = speed_at(at);
        states.push((speed < KEEPS_UP, player.stretches()));
        if ended && ring.fill() == 0 && buf.iter().all(|&s| s == 0.0) {
            break;
        }
        owed += f64::from(speed) * BUFFER as f64 * IN_HZ as f64 / f64::from(OUT_HZ);
        while at < n && owed >= 80.0 {
            let len = 80.min(n - at) as usize;
            // the run steps on only while the ring has room for what the step gives
            if ring.fill() + 3 * len + 3 > ring.ahead() {
                owed = 80.0;
                break;
            }
            audio.runs.push(CoAudioRun { t0: at as f64 / IN_HZ as f64, rate: (IN_HZ, 1), len });
            audio.samples.extend_from_slice(&samples[at as usize..at as usize + len]);
            tap.co_audio(&audio);
            audio.clear();
            at += len as u64;
            owed -= len as f64;
            tap.point(at as f64 / IN_HZ as f64, None, &[]);
            ring.set_tap_speed(speed);
        }
        if at == n && !ended {
            tap.point(n as f64 / IN_HZ as f64 + 0.05, None, &[]);
            tap.idle();
            ended = true;
        }
    }
    (out, states, (tap.stats().dropped_closed, tap.stats().dropped_stalled))
}

/// The output's stretches of sound (split at exact silence), each as the samples it played: its
/// fade out (the last sample's `fade - 1` steps down) taken off.
fn stretches(out: &[f32], fade: usize) -> Vec<&[f32]> {
    out.split(|&s| s == 0.0).filter(|run| !run.is_empty()).map(|run| &run[..run.len().saturating_sub(fade - 1)]).collect()
}

#[test]
fn a_slow_run_is_heard_in_stretches_each_sample_once() {
    // F7: 3 s of a sweep from a run at 0.5x. The player gathers 0.5 s, plays from a fade in until
    // the ring is dry, fades out and gathers again: what it played is every pushed sample once, in
    // order (the fade ins scaled), nothing dropped, and no stretch but the last under 0.4 s.
    let fade = (f64::from(OUT_HZ) * FADE_SECONDS) as usize;
    let samples = sweep(3 * IN_HZ, 3000.0);
    let reference = pushed(&samples);
    let (out, states, dropped) = heard_at(&samples, &[(3 * IN_HZ, 0.5)]);
    assert_eq!(dropped, (0, 0), "nothing dropped while the player holds");
    assert!(states.iter().all(|&(_, stretching)| stretching), "below KEEPS_UP the player plays in stretches");
    let played = stretches(&out, fade);
    assert!(played.len() >= 3, "a slow run is heard in stretches: {}", played.len());
    let mut at = 0;
    for (i, stretch) in played.iter().enumerate() {
        let want = &reference[at..at + stretch.len()];
        for (k, (&got, &was)) in stretch.iter().zip(want).enumerate() {
            let ramped = if k < fade { was * ((k + 1) as f32 / fade as f32) } else { was };
            assert_eq!(got.to_bits(), ramped.to_bits(), "stretch {i} sample {k} is the pushed sample {}", at + k);
        }
        if i + 1 < played.len() {
            assert!(stretch.len() as f64 >= 0.4 * f64::from(OUT_HZ), "stretch {i} lasts {} samples", stretch.len());
        }
        at += stretch.len();
    }
    assert_eq!(at, reference.len(), "every pushed sample is played once");
}

#[test]
fn the_player_follows_the_speed_both_ways_without_a_click() {
    // F9: 1 s of a sweep at 0.5x, 1.5 s at 2x (the ring's lead paces it), 1 s at 0.5x: the player
    // changes state within a second of each crossing of KEEPS_UP, and no output sample steps from
    // the last by more than the sweep itself does plus a fade's step.
    let samples = sweep(7 * IN_HZ / 2, 800.0);
    let reference = pushed(&samples);
    let slope = reference.windows(2).map(|w| (w[1] - w[0]).abs()).fold(0.0f32, f32::max);
    let fade = (f64::from(OUT_HZ) * FADE_SECONDS) as f32;
    let (out, states, dropped) = heard_at(&samples, &[(IN_HZ, 0.5), (5 * IN_HZ / 2, 2.0), (7 * IN_HZ / 2, 0.5)]);
    assert_eq!(dropped, (0, 0), "nothing dropped");
    let crossings: Vec<usize> = (1..states.len()).filter(|&i| states[i].0 != states[i - 1].0).collect();
    assert_eq!(crossings.len(), 2, "the run crosses KEEPS_UP twice");
    for &i in &crossings {
        let follows = (i..states.len()).find(|&j| states[j].1 == states[i].0).expect("the player follows");
        assert!(follows - i <= 100, "the player follows within a second: {} ms", (follows - i) * 10);
    }
    let step = out.windows(2).map(|w| (w[1] - w[0]).abs()).fold(0.0f32, f32::max);
    assert!(step <= slope + 0.5 / fade, "no click: the largest step {step} against the sweep's {slope}");
}
