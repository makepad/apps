//! Ordered sample delivery, exact period averages, level, frequency and worker backpressure; the
//! mix of voices, its controls and meters; the player's crossfades and lead; a run below real
//! time heard as its recording, made at its run's rate and played at the device's.

use fab_elec_sim::listen::{clock_now, rate_change, soft_limit, AudioTap, ListenBus, Player, SampleRing, TapMix, TapSpec, TapVoice, FADE_SECONDS, LIMIT_KNEE, METER_EVERY, RATE_HALF};
use fab_elec_sim::recording::{links, settle_tapes, window_frames, write_wav, Delivery, RunContent, RunSlot, Shape, Take, Tape, TapeShelf, CHUNK};
use fab_elec_sim::stamp::{UnkKind, UnknownInfo};
use fab_elec_sim::Sink;
use std::f64::consts::TAU;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

const RATE: f64 = 48_000.0;

fn unknowns() -> [UnknownInfo; 2] {
    [
        UnknownInfo { name: "0".into(), kind: UnkKind::Voltage },
        UnknownInfo { name: "signal".into(), kind: UnkKind::Voltage },
    ]
}

fn spec(gain: f32, dc_block_hz: f64) -> TapSpec {
    TapSpec { voices: vec![TapVoice { net: "signal".into(), reference: None, gain }], latency: 0.01, dc_block_hz, mix: TapMix::new(1) }
}

// A deadline closes the ring on failure so a broken producer cannot strand this proof in a wait.
// The completion flag is read before pop: once seen, that pop includes every published sample.
fn consumer<const BLOCK: usize>(ring: Arc<SampleRing>, done: Arc<AtomicBool>) -> JoinHandle<(Vec<f32>, usize, bool)> {
    thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(1);
        let mut samples = Vec::new();
        let mut block = [0.0; BLOCK];
        let mut max_fill = 0;
        loop {
            thread::sleep(Duration::from_millis(1));
            let finished = done.load(Ordering::Acquire);
            max_fill = max_fill.max(ring.fill());
            let count = ring.pop(&mut block);
            samples.extend_from_slice(&block[..count]);
            if finished && count == 0 {
                return (samples, max_fill, true);
            }
            if Instant::now() >= deadline {
                ring.set_open(false);
                return (samples, max_fill, false);
            }
        }
    })
}

#[test]
fn ring_wrap_order_and_claim() {
    assert_eq!(SampleRing::new(0).capacity(), 2, "the minimum capacity is two");
    let ring = SampleRing::new(5);
    assert_eq!(ring.capacity(), 8, "capacity rounds up to a power of two");
    assert_eq!(ring.fill(), 0, "a new ring is empty");
    assert!(!ring.is_open(), "a new ring starts closed");
    assert_eq!(ring.device_rate(), 0, "a new ring has no known device rate");
    ring.set_device_rate(48_000);
    assert_eq!(ring.device_rate(), 48_000, "the consumer publishes its device rate");
    ring.set_open(true);
    assert!(ring.is_open(), "the consumer opens the ring");
    let mut producer = ring.claim().expect("an unclaimed ring grants the producer");
    assert!(ring.claim().is_none(), "the producer claim is exclusive");
    ring.note_underrun();
    assert_eq!(ring.underruns(), 0, "stopped rings do not count underruns");
    producer.set_running(true);
    assert!(ring.running(), "the producer marks samples flowing");
    ring.note_underrun();
    assert_eq!(ring.underruns(), 1, "running rings count underruns");

    let mut written = 0;
    let mut read = 0;
    for _ in 0..12 {
        while producer.free() != 0 {
            assert!(producer.push(written as f32), "a free slot accepts the next sample");
            written += 1;
            assert_eq!(ring.fill(), written - read, "fill counts queued samples");
            assert_eq!(ring.fill() + producer.free(), ring.capacity(), "fill and free partition capacity");
        }
        assert!(!producer.push(-1.0), "a full ring refuses a sample");
        let mut block = [0.0; 3];
        assert_eq!(ring.pop(&mut block), 3, "a partial read returns three samples");
        for sample in block {
            assert_eq!(sample, read as f32, "wrap-around preserves sample order");
            read += 1;
        }
        assert_eq!(ring.fill(), written - read, "a partial read updates fill");
        assert_eq!(producer.free(), 3, "a partial read releases exactly its slots");
    }
    let mut large = [-1.0; 16];
    let count = ring.pop(&mut large);
    assert_eq!(count, written - read, "a large output returns only available samples");
    for &sample in &large[..count] {
        assert_eq!(sample, read as f32, "the final drain preserves order");
        read += 1;
    }
    assert!(large[count..].iter().all(|&x| x == -1.0), "pop leaves the unused output unchanged");
    assert_eq!(ring.fill(), 0, "the final drain empties the ring");
    assert_eq!(producer.free(), ring.capacity(), "all drained slots are free");
    assert_eq!(ring.pop(&mut large), 0, "an empty ring returns no samples");
    drop(producer);
    assert!(!ring.running(), "dropping the producer stops the ring");
    ring.note_underrun();
    assert_eq!(ring.underruns(), 1, "an unclaimed ring does not count underruns");
    assert!(ring.claim().is_some(), "dropping the producer releases its claim");
}

#[test]
fn tap_rate_level_and_frequency() {
    let ring = SampleRing::new(32_768);
    ring.set_device_rate(48_000);
    ring.set_open(true);
    let mut tap = AudioTap::new(spec(0.5, 5.0), Arc::clone(&ring));
    tap.begin(&unknowns());
    let done = Arc::new(AtomicBool::new(false));
    let drain = consumer::<512>(Arc::clone(&ring), Arc::clone(&done));
    let mut micros = 0;
    let mut step = 0;
    loop {
        let t = f64::from(micros) * 1e-6;
        tap.point(t, None, &[0.0, 2.5 + (TAU * 1000.0 * t).sin()]);
        if micros == 500_000 {
            break;
        }
        micros += if step % 2 == 0 { 7 } else { 13 };
        step += 1;
    }
    tap.idle();
    done.store(true, Ordering::Release);
    let (samples, max_fill, completed) = drain.join().expect("the level proof's consumer finishes");
    assert!(completed, "the level proof drains without reaching its failure deadline");
    assert!(!ring.running(), "idle marks the producer stopped");
    assert!(max_fill <= ring.capacity(), "the level proof never exceeds ring capacity");
    assert!(samples.len().abs_diff(24_000) <= 1, "half a second produces 24 000 samples, got {}", samples.len());
    assert_eq!(tap.stats().samples as usize, samples.len(), "the sample count agrees with the consumer");
    assert_eq!(tap.stats().dropped_closed, 0, "an open ring loses no tone samples");
    let settled = &samples[9600..];
    let rms = (settled.iter().map(|&x| f64::from(x).powi(2)).sum::<f64>() / settled.len() as f64).sqrt();
    let expected_rms = 0.5 * 0.7071;
    assert!((rms / expected_rms - 1.0).abs() < 0.01, "DC rejection and gain preserve the tone RMS: {rms}");
    let crossings: Vec<f64> = settled.windows(2).enumerate().filter_map(|(i, pair)| {
        let a = f64::from(pair[0]);
        let b = f64::from(pair[1]);
        (a <= 0.0 && b > 0.0).then(|| i as f64 - a / (b - a))
    }).collect();
    assert!(crossings.len() >= 2, "the tone has enough rising zero crossings");
    let hz = (crossings.len() - 1) as f64 * RATE / (crossings[crossings.len() - 1] - crossings[0]);
    assert!((hz / 1000.0 - 1.0).abs() < 0.001, "zero crossings retain 1000 Hz: {hz}");
}

#[test]
fn tap_backpressure_preserves_every_average() {
    let ring = SampleRing::new(1024);
    ring.set_device_rate(48_000);
    ring.set_open(true);
    let mut tap = AudioTap::new(spec(1.0, 0.0), Arc::clone(&ring));
    tap.begin(&unknowns());
    let done = Arc::new(AtomicBool::new(false));
    let drain = consumer::<128>(Arc::clone(&ring), Arc::clone(&done));
    let mut max_fill = 0;
    for k in 0..=4800 {
        let t = f64::from(k) / RATE;
        tap.point(t, None, &[0.0, t]);
        max_fill = max_fill.max(ring.fill());
    }
    tap.steady(0.1, &[0.0, 0.1]);
    done.store(true, Ordering::Release);
    let (samples, consumer_fill, completed) = drain.join().expect("the ramp proof's consumer finishes");
    assert!(completed, "the ramp drains without reaching its failure deadline");
    assert!(!ring.running(), "steady marks the producer stopped");
    assert_eq!(samples.len(), 4800, "the consumer receives every ramp sample");
    assert_eq!(tap.stats().samples, 4800, "the tap queues exactly 4800 ramp samples");
    assert_eq!(tap.stats().dropped_closed, 0, "backpressure drops no samples");
    assert!(tap.stats().waits > 0, "a slower listener makes the worker wait");
    assert!(max_fill.max(consumer_fill) <= ring.capacity(), "fill never exceeds capacity");
    assert_eq!(ring.fill(), 0, "the consumer drains the entire ramp");
    for (k, &sample) in samples.iter().enumerate() {
        let expected = (k as f64 + 0.5) / RATE;
        assert!((f64::from(sample) - expected).abs() < 1e-6, "ramp sample {k} is its period average in order: {sample} vs {expected}");
    }
}

#[test]
fn tap_closed_ring_never_waits() {
    let ring = SampleRing::new(64);
    ring.set_open(false);
    let mut tap = AudioTap::new(spec(1.0, 0.0), Arc::clone(&ring));
    tap.begin(&unknowns());
    tap.point(0.0, None, &[0.0, 0.0]);
    tap.point(0.1, None, &[0.0, 0.1]);
    assert_eq!(tap.stats().waits, 0, "a closed ring never waits for a consumer");
    assert_eq!(tap.stats().samples, 64, "a closed ring fills its available slots");
    assert_eq!(tap.stats().dropped_closed, 4800 - 64, "every sample beyond the closed capacity is counted");
    assert_eq!(ring.fill(), ring.capacity(), "closed delivery never exceeds capacity");
    assert!(ring.running(), "accepted points mark the tap running");
    drop(tap);
    assert!(!ring.running(), "dropping the tap marks its producer stopped");
    assert!(ring.claim().is_some(), "dropping the tap releases the producer");
}

/// A consumer that stops draining an open ring (a device gone away) holds the worker for one
/// stall watch, never longer: the tap then drops what does not fit until the consumer takes
/// samples again, and waits normally once it does.
#[test]
fn a_stalled_consumer_never_holds_the_worker() {
    let ring = SampleRing::new(64);
    ring.set_device_rate(48_000);
    ring.set_open(true);
    let mut tap = AudioTap::new(spec(1.0, 0.0), Arc::clone(&ring));
    tap.begin(&unknowns());
    let start = Instant::now();
    tap.point(0.0, None, &[0.0, 0.0]);
    tap.point(0.1, None, &[0.0, 0.1]);
    let took = start.elapsed();
    assert!(took < Duration::from_millis(800), "one stall watch, not one per sample: {took:?}");
    assert_eq!((tap.stats().waits, tap.stats().stalls), (1, 1), "one wait, taken for a stall");
    assert_eq!(tap.stats().samples + tap.stats().dropped_stalled, 4800, "every sample is queued or counted as dropped");
    // the consumer comes back: the tap waits on it again instead of dropping
    let dropped = tap.stats().dropped_stalled;
    let mut block = [0.0f32; 64];
    ring.pop(&mut block);
    let done = Arc::new(AtomicBool::new(false));
    let drain = consumer::<128>(Arc::clone(&ring), Arc::clone(&done));
    tap.point(0.11, None, &[0.0, 0.11]);
    done.store(true, Ordering::Release);
    drain.join().expect("the consumer ends");
    assert_eq!(tap.stats().stalls, 1, "a consumer that drains again is not taken for stalled");
    assert_eq!(tap.stats().dropped_stalled, dropped, "once the consumer drains again nothing more is dropped");
    assert_eq!(tap.stats().samples + tap.stats().dropped_stalled, 5280, "every sample is queued or counted as dropped");
}

/// A net resting at a few volts is heard as silence from its first sample: the DC block starts
/// at rest on the first value rather than stepping from zero.
#[test]
fn listening_to_a_biased_net_starts_silent() {
    let ring = SampleRing::new(1024);
    ring.set_device_rate(48_000);
    let mut tap = AudioTap::new(spec(0.4, 5.0), Arc::clone(&ring));
    tap.begin(&unknowns());
    tap.point(0.0, None, &[0.0, 2.5]);
    tap.point(0.01, None, &[0.0, 2.5]);
    let mut out = [1.0f32; 480];
    let got = ring.pop(&mut out);
    assert_eq!(got, 480, "10 ms of samples");
    assert!(out.iter().all(|s| s.abs() < 1e-6), "a steady 2.5 V is silent from the start: first {}", out[0]);
}

fn voice(net: &str, gain: f32) -> TapVoice {
    TapVoice { net: net.into(), reference: None, gain }
}

fn three() -> [UnknownInfo; 3] {
    [
        UnknownInfo { name: "0".into(), kind: UnkKind::Voltage },
        UnknownInfo { name: "a".into(), kind: UnkKind::Voltage },
        UnknownInfo { name: "b".into(), kind: UnkKind::Voltage },
    ]
}

/// Runs `tap` over `[t0, t1]` in 10 us points of `at(t)` and pops what it queued.
fn run(tap: &mut AudioTap, ring: &SampleRing, t0: f64, t1: f64, at: impl Fn(f64) -> [f64; 3]) -> Vec<f32> {
    let steps = ((t1 - t0) / 1e-5).round() as usize;
    for k in 0..=steps {
        let t = t0 + k as f64 * 1e-5;
        tap.point(t, None, &at(t));
    }
    let mut out = vec![0.0f32; ring.fill()];
    let got = ring.pop(&mut out);
    out.truncate(got);
    out
}

/// A live run that goes back to a snapshot (`Sink::save`, `Sink::rewind`) and steps the same
/// stretch again is heard as the run that never went back: a tap on two voices (DC blocked),
/// recording at 96 kHz, stepped in 10 us points to 50 ms, once straight and once going back from
/// 35 ms to its state at 20 ms. Live play goes on from where it was heard to, sample for sample
/// the straight run's (the stretch stepped again is not heard twice, nor piled into the sample
/// where play stopped); the take goes on (no new one), its tape the straight run's by bits, cut
/// back at the return and written again; and the run's clean stretch ends at the return.
#[test]
fn a_tap_that_goes_back_plays_on_and_keeps_its_take() {
    let at = |k: u32| {
        let t = f64::from(k) * 1e-5;
        (t, [0.0, 0.3 * (TAU * 440.0 * t).sin() + 0.1, 0.2 * (TAU * 3100.0 * t).sin()])
    };
    let unknowns = three();
    let make = || {
        let bus = ListenBus::new(1 << 14, Arc::default());
        let content = Arc::new(RunContent::new(1, None, 96_000.0));
        bus.runs.set(Some(content.clone()));
        bus.ring.set_device_rate(48_000);
        bus.ring.set_open(true);
        // a lead the ring never fills here: nobody plays it, and the tap never waits
        let spec = TapSpec { voices: vec![voice("a", 1.0), voice("b", 1.0)], latency: 1.0, dc_block_hz: 20.0, mix: TapMix::new(2) };
        let mut tap = AudioTap::new(spec, bus.ring.clone()).recording(bus.record(1, 1, 0.25));
        tap.begin(&unknowns);
        (bus, tap, content)
    };
    let step = |tap: &mut AudioTap, ks: std::ops::RangeInclusive<u32>| {
        for k in ks {
            let (t, x) = at(k);
            tap.point(t, None, &x);
        }
    };
    let heard = |bus: &ListenBus| {
        settle_tapes();
        let mut out = vec![0.0f32; bus.ring.fill()];
        let n = bus.ring.pop(&mut out);
        out.truncate(n);
        let tape = bus.tapes.current(1).expect("the take is taped");
        let mut taped = vec![0.0f32; 2 * tape.frames() as usize];
        assert_eq!(tape.read(0, &mut taped).expect("the tape reads"), tape.frames() as usize);
        (out, tape.take.take, taped)
    };
    let (bus, mut tap, straight_content) = make();
    step(&mut tap, 0..=5000);
    let (live, take, taped) = heard(&bus);
    let (bus, mut back, content) = make();
    step(&mut back, 0..=2000);
    let saved = back.save().expect("a tap that heard points saves its state");
    step(&mut back, 2001..=3500);
    settle_tapes();
    let before = bus.tapes.current(1).expect("the take is taped").frames();
    back.rewind(at(2000).0, Some(&*saved), &unknowns);
    settle_tapes();
    let cut = bus.tapes.current(1).expect("the take is taped").frames();
    step(&mut back, 2001..=5000);
    let (back_live, back_take, back_taped) = heard(&bus);
    assert!(live.len() >= 2399, "50 ms at 48 kHz: {}", live.len());
    let parted = back_live.iter().zip(&live).position(|(a, b)| a.to_bits() != b.to_bits());
    assert!(back_live.len() == live.len() && parted.is_none(), "live play goes on as the straight run's: {} samples against {}, parting at {parted:?}", back_live.len(), live.len());
    assert_eq!(back_take, take, "the take goes on: no new one");
    assert!(before == 6 * CHUNK as u64 && cut == 3 * CHUNK as u64, "the tape is cut back to the chunks made by 20 ms: {before} frames to {cut}");
    let parted = back_taped.iter().zip(&taped).position(|(a, b)| a.to_bits() != b.to_bits());
    assert!(taped.len() == 2 * 9 * CHUNK && back_taped.len() == taped.len() && parted.is_none(), "the take is the straight run's by bits: {} frames against {}, parting at {parted:?}", back_taped.len() / 2, taped.len() / 2);
    assert!(straight_content.clean_until().is_infinite() && content.clean_until() == at(2000).0, "the clean stretch ends at the return: {}", content.clean_until());
}

/// A tap mixes its voices: each at its gain and level, summed, at the master level, soft limited
/// above the knee and never past full scale; a mute queues silence and the samples keep coming;
/// a voice that names no unknown is silent while the others play; the controls take effect on a
/// running tap; and each voice is AC coupled on its own, so a biased one adds nothing.
#[test]
fn a_mix_sums_its_voices_at_their_levels_and_limits_softly() {
    assert_eq!((soft_limit(0.5), soft_limit(-LIMIT_KNEE)), (0.5, -LIMIT_KNEE), "the limiter passes the mix below its knee");
    let slope = (soft_limit(LIMIT_KNEE + 1e-6) - LIMIT_KNEE) / 1e-6;
    assert!((slope - 1.0).abs() < 1e-3, "the shoulder meets the line with its slope: {slope}");
    assert!(soft_limit(1e6) <= 1.0 && soft_limit(1.2) > soft_limit(1.1), "the shoulder rises and never passes full scale");

    let ring = SampleRing::new(1 << 16);
    ring.set_device_rate(48_000);
    let mix = TapMix::new(3);
    let spec = TapSpec { voices: vec![voice("a", 0.5), voice("b", 0.25), voice("missing", 1.0)], latency: 0.01, dc_block_hz: 0.0, mix: mix.clone() };
    let mut tap = AudioTap::new(spec, ring.clone());
    tap.begin(&three());
    mix.set_level(1, 0.5);
    let steady = |_: f64| [0.0, 0.4, 0.8];
    let first = run(&mut tap, &ring, 0.0, 0.01, steady);
    assert!(first.len().abs_diff(480) <= 1, "10 ms at 48 kHz: {}", first.len());
    assert!(first.iter().all(|&s| (s - 0.3).abs() < 1e-6), "0.4 V x 0.5 + 0.8 V x 0.25 x 0.5 = 0.3, the missing voice silent: {}", first[0]);
    mix.set_master(4.0);
    let loud = run(&mut tap, &ring, 0.01, 0.02, steady);
    let limited = soft_limit(1.2) as f32;
    assert!(loud.iter().all(|&s| (s - limited).abs() < 1e-6 && s < 1.0), "a master that takes the mix to 1.2 is limited to {limited}: {}", loud[0]);
    mix.set_muted(true);
    let before = tap.stats().samples;
    let muted = run(&mut tap, &ring, 0.02, 0.03, steady);
    assert!(muted.len().abs_diff(480) <= 1 && muted.iter().all(|&s| s == 0.0), "muted, the tap queues silence");
    assert!((tap.stats().samples - before).abs_diff(480) <= 1, "and the samples keep coming");

    let ring = SampleRing::new(1 << 16);
    ring.set_device_rate(48_000);
    let spec = TapSpec { voices: vec![voice("a", 1.0), voice("b", 0.5)], latency: 0.01, dc_block_hz: 5.0, mix: TapMix::new(2) };
    let mut tap = AudioTap::new(spec, ring.clone());
    tap.begin(&three());
    let out = run(&mut tap, &ring, 0.0, 0.5, |t| [0.0, 3.0, 2.5 + (TAU * 1000.0 * t).sin()]);
    let settled = &out[9600..];
    let rms = (settled.iter().map(|&x| f64::from(x).powi(2)).sum::<f64>() / settled.len() as f64).sqrt();
    assert!((rms / (0.5 * 0.7071) - 1.0).abs() < 0.01, "a biased voice adds nothing and the tone's voice keeps its RMS: {rms}");
}

/// The meters read each voice after its level and the mix after the limiter: the RMS of a tone and
/// its peak; reading a meter takes its peak.
#[test]
fn the_meters_read_each_voice_and_the_mix() {
    let ring = SampleRing::new(1 << 16);
    ring.set_device_rate(48_000);
    let mix = TapMix::new(2);
    let spec = TapSpec { voices: vec![voice("a", 0.5), voice("b", 1.0)], latency: 0.01, dc_block_hz: 0.0, mix: mix.clone() };
    let mut tap = AudioTap::new(spec, ring.clone());
    tap.begin(&three());
    let out = run(&mut tap, &ring, 0.0, 0.3, |t| [0.0, (TAU * 1000.0 * t).sin(), 0.0]);
    assert!(out.len() >= 4 * METER_EVERY as usize, "enough samples for several meter writes");
    let (tone, quiet, sum) = (mix.meter(0), mix.meter(1), mix.meter(2));
    assert!((f64::from(tone.rms) / (0.5 * 0.7071) - 1.0).abs() < 0.02, "the tone's voice reads its RMS: {}", tone.rms);
    assert!(tone.peak > 0.49 && tone.peak <= 0.5, "and its peak: {}", tone.peak);
    assert_eq!((quiet.rms, quiet.peak), (0.0, 0.0), "a silent voice reads nothing");
    assert!((sum.rms - tone.rms).abs() < 1e-3, "the mix reads its one sounding voice: {} vs {}", sum.rms, tone.rms);
    assert_eq!(mix.meter(0).peak, 0.0, "reading a meter takes its peak");
}

/// A tap that records into a player, stepped by hand in lockstep with the player's callbacks: a
/// ramp ([`RAMP`]) heard at level 0.5, so every sample out names the frame it plays.
struct Rig {
    ring: Arc<SampleRing>,
    bus: ListenBus,
    mix: Arc<TapMix>,
    tapes: Arc<TapeShelf>,
    tap: AudioTap,
    player: Player,
    t: f64,
    /// The device's rate, Hz, and its 10 ms buffer.
    rate: f64,
    out: Vec<f32>,
    heard: Vec<f32>,
    holds: usize,
}

/// Volts a second: slow enough that the loudest sample heard stays below the limiter's knee.
const RAMP: f64 = 0.05;

impl Rig {
    fn new(window_s: f64) -> Rig {
        Rig::keyed(window_s, Arc::default())
    }

    /// A rig whose runs' content goes up in `runs`.
    fn keyed(window_s: f64, runs: Arc<RunSlot>) -> Rig {
        let bus = ListenBus::new(1 << 16, runs);
        let (ring, tapes) = (bus.ring.clone(), bus.tapes.clone());
        ring.set_device_rate(48_000);
        ring.set_open(true);
        let links = bus.take_player().expect("a new bus has its player's ends");
        assert!(bus.take_player().is_none(), "one output takes the bus's player ends");
        let mix = TapMix::new(1);
        mix.set_level(0, 0.5);
        bus.links.shapes.try_send(Box::new(Shape::new(1, window_s, 1, mix.clone()))).ok().expect("the shape goes");
        let record = bus.record(1, 1, window_s);
        // a lead longer than the ring ever holds here: the tap never waits on this one thread
        let spec = TapSpec { voices: vec![voice("signal", 1.0)], latency: 0.05, dc_block_hz: 0.0, mix: mix.clone() };
        let mut tap = AudioTap::new(spec, ring.clone()).recording(record);
        tap.begin(&unknowns());
        Rig { ring, bus, mix, tapes, tap, player: Player::new(Some(links)), t: 0.0, rate: 48_000.0, out: vec![0.0; 480], heard: Vec::new(), holds: 0 }
    }

    /// The device changes its rate.
    fn set_device(&mut self, hz: u32) {
        self.ring.set_device_rate(hz);
        self.rate = f64::from(hz);
        self.out = vec![0.0; hz as usize / 100];
    }

    /// `fills` 10 ms callbacks, the run making `made` of real time a callback (unless the tap
    /// holds it) and the tap's speed reading `speed`.
    fn run(&mut self, made: f64, speed: f32, fills: usize) {
        for _ in 0..fills {
            if self.tap.holds() {
                self.holds += 1;
            } else {
                let end = self.t + made * 0.01;
                while self.t < end {
                    self.tap.point(self.t, None, &[0.0, RAMP * self.t]);
                    self.t += 1.0 / 96_000.0;
                }
            }
            self.ring.set_tap_speed(speed);
            self.player.fill(&self.ring, &mut self.out, self.rate);
            self.heard.extend_from_slice(&self.out);
            self.bus.links.spent.drain();
        }
        // the tapes have what the tap queued, as on a disk that keeps up
        settle_tapes();
    }
}

/// The device frame (at `rate` Hz) a sample out plays, at `level`.
fn frame_of(s: f32, level: f64, rate: f64) -> f64 {
    f64::from(s) / (level * RAMP) * rate - 0.5
}

/// Runs of consecutive device frames (at `rate` Hz) in what was heard: (first frame, length), each
/// at least 1000 long (crossfades between them are not runs).
fn runs(heard: &[f32], level: f64, rate: f64) -> Vec<(f64, usize)> {
    let mut runs: Vec<(f64, usize)> = Vec::new();
    let mut run = (0.0, 0usize);
    for w in heard.windows(2) {
        let (a, b) = (frame_of(w[0], level, rate), frame_of(w[1], level, rate));
        // f32 keeps a frame to a few hundredths over these seconds; a jump or a crossfade is far off
        if w[0] != 0.0 && (b - a - 1.0).abs() < 0.25 {
            run = if run.1 == 0 { (a, 2) } else { (run.0, run.1 + 1) };
        } else {
            if run.1 >= 1000 {
                runs.push(run);
            }
            run = (0.0, 0);
        }
    }
    if run.1 >= 1000 {
        runs.push(run);
    }
    runs
}

/// A run below real time is heard as its recording, whole and never as crackle. Its one underrun
/// switches the player to the recording; the first window starts before it is whole, once the
/// rest renders in time at the measured speed; each window plays from its start to its end and
/// round until the next can play to its end, which takes over at the seam. A run that gets two
/// windows ahead of the one heard is held until the player moves on, so no window is skipped. A
/// level change is heard at once (the recording is mixed as it plays), and a run faster than real
/// time is heard live again.
#[test]
fn a_run_below_real_time_plays_its_recording() {
    let window_s = 0.25;
    let w = window_frames(window_s, 48_000.0) as f64;
    let mut rig = Rig::new(window_s);
    // live until the ring runs short at 0.4x: one underrun, then the recording
    for _ in 0..50 {
        rig.run(0.4, 0.4, 1);
        if rig.ring.play_status().rendered {
            break;
        }
    }
    assert!(rig.ring.play_status().rendered && rig.ring.underruns() == 1, "one underrun shows the run is behind");
    let switched = rig.heard.len();
    rig.run(0.4, 0.4, 1);
    let (voice, mix) = (rig.mix.meter(0), rig.mix.meter(1));
    assert!(voice.rms == 0.0 && mix.rms == 0.0, "the meters read silence while the first window renders: {} {}", voice.rms, mix.rms);
    let mut started = None;
    for fill in 0..150 {
        rig.run(0.4, 0.4, 1);
        if started.is_none() && rig.out.iter().any(|&s| s != 0.0) {
            let st = rig.ring.play_status();
            started = Some((fill, st));
        }
    }
    let (_, st) = started.expect("the recording plays");
    assert!(st.rendered && st.ready_s < st.window_s, "the first window starts before it is whole: {st:?}");
    // what live play heard fades out at the switch: the recording starts after the silence
    let quiet = switched + rig.heard[switched..].iter().position(|&s| s == 0.0).unwrap();
    let silent_until = quiet + rig.heard[quiet..].iter().position(|&s| s != 0.0).unwrap();
    // round and on at 0.4x: the next window is heard once it can play to its end
    rig.run(0.4, 0.4, 250);
    let slow = runs(&rig.heard[silent_until..], 0.5, 48_000.0);
    let fade = 48_000.0 * FADE_SECONDS;
    let windows: Vec<f64> = slow.iter().map(|&(first, _)| ((first - fade) / w).round()).collect();
    for &(first, len) in &slow[..slow.len() - 1] {
        let at = first - ((first - fade) / w).round() * w;
        assert!((at - fade).abs() < 2.0 && (len as f64 - (w - fade)).abs() < 2.0, "a window plays from its start to its end: {first} for {len}");
    }
    assert!(windows.windows(2).all(|p| p[1] >= p[0]) && windows.first() == Some(&0.0) && windows.contains(&1.0), "window 0 round, then window 1: {windows:?}");
    // the ring says what the device plays now: the run's time of the callback's first sample (the
    // play head less the buffer), where an edit made now is heard from
    let mut told = 0;
    for _ in 0..40 {
        rig.run(0.4, 0.4, 1);
        let frames: Vec<f64> = rig.out.iter().map(|&s| frame_of(s, 0.5, 48_000.0)).collect();
        if rig.out[0] != 0.0 && frames.windows(2).all(|p| (p[1] - p[0] - 1.0).abs() < 0.25) {
            let at = rig.ring.heard_at().expect("while the recording plays, the time heard is known");
            assert!((at * 48_000.0 - frames[0]).abs() < 0.5, "the time heard is the buffer's first sample's: {at} s, frame {}", frames[0]);
            told += 1;
        }
    }
    assert!(told >= 30, "most callbacks play inside a window: {told} of 40");
    // The heard record says where the sound is at any instant until the next callback, round a
    // loop's wrap and a move to the next window inside its buffer too: asked right after a
    // callback for an instant up to a buffer on, it names the run's time of the frame the device
    // plays then (a buffer behind the newest handed out).
    let start = rig.heard.len();
    let mut asked: Vec<(usize, f64)> = Vec::new();
    for _ in 0..80 {
        rig.run(0.4, 0.4, 1);
        let Some(stamped) = rig.ring.heard_now(clock_now()).map(|h| h.stamped) else { continue };
        let end = rig.heard.len() - rig.out.len();
        for d in [0usize, 160, 320, 479] {
            if let Some(h) = rig.ring.heard_now(stamped + d as f64 / 48_000.0) {
                assert!(h.moving, "the sound moves while the recording plays");
                asked.push((end + d, h.t));
            }
        }
    }
    let frames: Vec<f64> = (0..rig.heard.len()).map(|k| frame_of(rig.heard[k], 0.5, 48_000.0)).collect();
    // a sample inside a run of the ramp (not silence, a fade or a crossfade) has a known time
    let known = |k: usize| k > 0 && k + 1 < frames.len() && rig.heard[k] != 0.0 && (frames[k] - frames[k - 1] - 1.0).abs() < 0.25 && (frames[k + 1] - frames[k] - 1.0).abs() < 0.25;
    let checked: Vec<f64> = asked.iter().filter(|&&(k, _)| k >= start && known(k)).map(|&(k, t)| (t * 48_000.0 - frames[k]).abs()).collect();
    let worst = checked.iter().copied().fold(0.0, f64::max);
    let wraps = runs(&rig.heard[start..], 0.5, 48_000.0).windows(2).filter(|p| p[1].0 < p[0].0).count();
    assert!(checked.len() > 250 && wraps > 0 && worst < 0.5, "the heard record names the frame playing at each instant asked, across {wraps} wrap(s): {} checked, worst {worst:.2} frames off", checked.len());
    // Stop or Pause: silent within the fade (the first callback after it starts fading), until
    // Play brings the recording back, faded in (the click check below covers both)
    rig.ring.set_hushed(true);
    let from = rig.heard.len();
    rig.run(0.0, 0.4, 20);
    let after_fade = from + fade as usize + 1;
    assert!(rig.heard[from..after_fade].iter().any(|&s| s != 0.0) && rig.heard[after_fade..].iter().all(|&s| s == 0.0), "the loop fades out at a stop and stays silent");
    // stopped, the record holds where the sound stopped: it does not move on
    let (a, b) = (rig.ring.heard_now(clock_now()), rig.ring.heard_now(clock_now() + 0.5));
    assert!(a.is_some_and(|a| !a.moving) && a.map(|a| a.t) == b.map(|b| b.t), "a stop holds the time heard: {a:?} then {b:?}");
    rig.ring.set_hushed(false);
    let from = rig.heard.len();
    rig.run(0.0, 0.4, 20);
    assert!(rig.heard[from..].iter().filter(|&&s| s != 0.0).count() > 400, "Play brings the recording back");
    // the run gets ahead: held a window past the one heard, and no window is skipped
    let from = rig.heard.len();
    rig.run(3.0, 0.4, 300);
    assert!(rig.holds > 0, "the run waits for the player");
    let fast: Vec<f64> = runs(&rig.heard[from..], 0.5, 48_000.0).iter().map(|&(first, _)| ((first - fade) / w).round()).collect();
    assert!(fast.windows(2).all(|p| p[1] - p[0] <= 1.0) && fast.len() > 1, "each window after the last: {fast:?}");
    // the run starts over (an edit restarts it) and renders ahead fast: the last take plays round
    // until the new one's window 0 can play, which is heard from its start, then its window 1;
    // the new take's later windows never take the table window 0 waits in
    let from = rig.heard.len();
    rig.t = 0.0;
    rig.run(3.0, 0.4, 300);
    let again: Vec<f64> = runs(&rig.heard[from..], 0.5, 48_000.0).iter().map(|&(first, _)| ((first - fade) / w).round()).collect();
    let zero = again.iter().position(|&w| w == 0.0).unwrap_or_else(|| panic!("the new take's window 0 is heard: {again:?}"));
    assert!(zero > 0 && again[..zero].iter().all(|&w| w > 1.0), "the last take's windows play until then: {again:?}");
    assert!(again[zero..].windows(2).all(|p| p[1] >= p[0] && p[1] - p[0] <= 1.0) && again[zero..].contains(&1.0), "the new take from its window 0, then each next: {again:?}");
    let peak = rig.heard.iter().fold(0.0f32, |m, s| m.max(s.abs()));
    let steepest = rig.heard.windows(2).map(|p| (p[1] - p[0]).abs()).fold(0.0f32, f32::max);
    assert!(peak < LIMIT_KNEE as f32 && steepest <= 1.05 * peak / (48_000.0 * FADE_SECONDS) as f32, "no click at a start, a seam or a window's change: {steepest} at peak {peak}");
    // a level is heard at once
    rig.mix.set_level(0, 0.25);
    let from = rig.heard.len();
    rig.run(0.0, 0.4, 20);
    assert!(!runs(&rig.heard[from..], 0.25, 48_000.0).is_empty(), "the recording plays at the new level without a new render");
    // faster than real time: live again
    rig.run(1.0, 2.0, 150);
    assert!(!rig.ring.play_status().rendered, "a run faster than real time is heard live again: {:?}", rig.ring.play_status());
    assert_eq!(rig.ring.heard_at(), None, "live, no recorded time is heard: an edit goes in at the run's front");
    assert_eq!(rig.ring.heard_now(clock_now()), None, "live, the views show the run's front");
    // the whole take is on its tape, every frame before its level, and saves as a float WAV at
    // the levels now
    let tape = rig.tapes.current(1).expect("the take is taped");
    let frames = tape.frames();
    assert!(frames as f64 >= 48_000.0 * (rig.t - 0.02) && frames % CHUNK as u64 == 0, "every whole chunk of the run: {frames} frames for {} s", rig.t);
    let mut back = vec![0.0f32; frames as usize];
    assert_eq!(tape.read(0, &mut back).expect("the tape reads back"), frames as usize);
    let off = back.iter().enumerate().map(|(k, &x)| (f64::from(x) - RAMP * (k as f64 + 0.5) / 48_000.0).abs()).fold(0.0, f64::max);
    assert!(off < 1e-6, "the tape holds the run's samples from its start: off by {off}");
    let mut wav = Vec::new();
    assert_eq!(write_wav(&tape, &rig.mix, &mut wav).expect("the WAV writes"), frames);
    let word = |at: usize| u32::from_le_bytes(wav[at..at + 4].try_into().unwrap());
    assert!(&wav[..4] == b"RIFF" && &wav[8..16] == b"WAVEfmt " && &wav[50..54] == b"data", "a RIFF WAVE with fmt, fact and data");
    assert_eq!((word(4) as usize, u16::from_le_bytes([wav[20], wav[21]]), word(24), word(54) as usize), (wav.len() - 8, 3, 48_000, 4 * frames as usize), "float mono at the run's rate, sized right");
    let sample = |k: usize| f32::from_le_bytes(wav[58 + 4 * k..62 + 4 * k].try_into().unwrap());
    assert!((0..frames as usize).step_by(997).all(|k| sample(k) == (f64::from(back[k]) * 0.25) as f32), "each sample at the level now (0.25)");
}

/// A take of content the shelf kept replays it at once: the run starts over with the same
/// content (a key in the run slot) and the new take's window 0 plays at the last window's seam
/// rather than after a render, its windows back to back from the kept take while the render goes
/// on behind it, every rendered chunk equal to the kept one. A clean stretch that ended early
/// replays only that far; another content renders.
#[test]
fn a_take_of_kept_content_replays_at_once() {
    let window_s = 0.25;
    let w = window_frames(window_s, 48_000.0) as f64;
    let fade = 48_000.0 * FADE_SECONDS;
    let runs_slot = Arc::new(RunSlot::default());
    let content = Arc::new(RunContent::new(7, Some("R7 = 22k".into()), 0.0));
    runs_slot.set(Some(content.clone()));
    let mut rig = Rig::keyed(window_s, runs_slot.clone());
    // the first take renders slowly: 0.1x, three windows heard
    rig.run(0.1, 0.1, 1200);
    let first = rig.tapes.current(1).expect("the take is taped");
    assert!(first.content.as_ref().is_some_and(|c| Arc::ptr_eq(c, &content)), "a take from its run's start carries the run's content");
    assert!(first.clean_frames() as f64 >= 2.0 * w, "two windows rendered and clean: {}", first.clean_frames());
    // the run starts over with the same content: the kept take plays from its window 0's start at
    // the last window's seam, and on through its windows back to back: in 1 s past two windows,
    // where a render at 0.1x makes 0.1 s
    let from = rig.heard.len();
    rig.t = 0.0;
    rig.run(0.1, 0.1, 100);
    let again = runs(&rig.heard[from..], 0.5, 48_000.0);
    let zero = again.iter().position(|&(first, _)| (first - fade).abs() < 2.0).unwrap_or_else(|| panic!("the kept take plays from its window 0's start: {again:?}"));
    let reach = again[zero..].iter().map(|&(first, len)| first + len as f64).fold(0.0, f64::max);
    assert!(reach >= 2.0 * w, "the kept windows play back to back: frame {reach} reached, a window is {w}: {again:?}");
    assert!(rig.tapes.replayed.load(Ordering::Relaxed) > 0, "chunks came from the kept take");
    rig.run(0.1, 0.1, 400);
    assert_eq!((rig.tapes.differed.load(Ordering::Relaxed), rig.tapes.worst.load(Ordering::Relaxed)), (0, 0), "every chunk the render made again equals the kept one");
    // a clean stretch that ended at 0.3 s: replayed that far only
    let now = rig.tapes.current(1).expect("the take is taped");
    assert!(!Arc::ptr_eq(&now, &first) && now.same_as(&first), "the new take records the same content");
    content.taint(0.3);
    let served_before = rig.tapes.replayed.load(Ordering::Relaxed);
    rig.t = 0.0;
    rig.run(0.1, 0.1, 30);
    let served = rig.tapes.replayed.load(Ordering::Relaxed) - served_before;
    assert!(served as f64 * CHUNK as f64 <= 0.3 * 48_000.0, "no kept chunk past the clean stretch: {served} chunks");
    // another content renders: nothing kept is replayed
    runs_slot.set(Some(Arc::new(RunContent::new(8, None, 0.0))));
    let served_before = rig.tapes.replayed.load(Ordering::Relaxed);
    rig.t = 0.0;
    rig.run(0.1, 0.1, 100);
    assert_eq!(rig.tapes.replayed.load(Ordering::Relaxed), served_before, "another content plays nothing kept");
}

/// A run that starts over (an edit) while the window heard still renders is heard from the new
/// take's start once that can play: the old take renders no further, so the play head that
/// reaches its render does not wait there, silent, with the run held behind it.
#[test]
fn a_run_that_starts_over_while_its_window_renders_is_heard_from_the_new_start() {
    let mut rig = Rig::new(0.25);
    // at 0.4x the run underruns and is heard as its recording, silent until its first window
    // starts, before that is whole
    let mut waits = false;
    let started = (0..400).any(|_| {
        rig.run(0.4, 0.4, 1);
        let sounds = rig.out.iter().any(|&s| s != 0.0);
        let starts = waits && sounds;
        waits |= rig.ring.play_status().rendered && !sounds;
        starts
    });
    assert!(started && rig.ring.play_status().rendered, "the recording plays: {:?}", rig.ring.play_status());
    // it starts over at once, the window heard a third rendered short
    let from = rig.heard.len();
    rig.t = 0.0;
    rig.run(0.4, 0.4, 150);
    let again = runs(&rig.heard[from..], 0.5, 48_000.0);
    let fade = 48_000.0 * FADE_SECONDS;
    assert!(again.iter().any(|&(first, _)| (first - fade).abs() < 2.0), "the new take is heard from its start: {again:?}");
    assert!(rig.t > 0.4 && rig.holds == 0, "the run goes on, never held: at {} s, {} holds", rig.t, rig.holds);
}

/// A tap that only records (a board's output jack) tapes its run from the start whether anyone
/// hears it or not, makes no live samples and never holds the run, even while the device is
/// open; a take nobody hears stops taping at its bound, counted once, its start kept with its
/// run's content, and the run goes on; the next take tapes again from its own start.
#[test]
fn a_recorder_tapes_from_the_start_and_stops_an_unheard_take_at_its_bound() {
    let runs = Arc::new(RunSlot::default());
    let content = Arc::new(RunContent::new(3, None, 48_000.0));
    runs.set(Some(content.clone()));
    let bus = ListenBus::new(1 << 16, runs);
    bus.ring.set_device_rate(44_100);
    bus.ring.set_open(true);
    let mut record = bus.record(9, 9, 0.25);
    record.bound = 10 * 4 * CHUNK as u64;
    let spec = TapSpec { voices: vec![voice("signal", 1.0)], latency: 0.05, dc_block_hz: 0.0, mix: TapMix::new(1) };
    let mut tap = AudioTap::recorder(spec, bus.ring.clone(), record);
    tap.begin(&unknowns());
    // 0.2 s of run: 9600 frames at the run's 48 kHz, eighteen whole chunks, the bound ten
    let step = |tap: &mut AudioTap, ks: std::ops::RangeInclusive<usize>| {
        for k in ks {
            let t = k as f64 / 96_000.0;
            tap.point(t, None, &[0.0, RAMP * t]);
            assert!(!tap.holds(), "a recorder never holds the run");
        }
    };
    let taped = |tape: &Tape| {
        settle_tapes();
        let mut out = vec![0.0f32; tape.frames() as usize];
        assert_eq!(tape.read(0, &mut out).expect("the tape reads"), out.len());
        out
    };
    step(&mut tap, 0..=4800);
    let saved = tap.save().expect("a recorder saves its state");
    step(&mut tap, 4801..=19_200);
    settle_tapes();
    let first = bus.tapes.current(9).expect("the take is taped");
    assert_eq!((first.rate, first.frames()), (48_000.0, 10 * CHUNK as u64), "the take is at its run's rate and stops at its bound");
    assert_eq!(bus.tapes.bounded.load(Ordering::Relaxed), 1, "the bound is counted");
    assert!(first.content.as_ref().is_some_and(|c| Arc::ptr_eq(c, &content)) && first.clean_frames() == first.frames(), "its start is kept with its run's content");
    assert_eq!((bus.ring.fill(), bus.ring.tap_speed()), (0, 0.0), "no live samples, no speed measured");
    // the run goes back to 50 ms, before the bound: the take tapes on from there to it again
    let straight = taped(&first);
    tap.rewind(0.05, Some(&*saved), &unknowns());
    settle_tapes();
    assert_eq!(first.frames(), 4 * CHUNK as u64, "the tape is cut back to the chunks made by 50 ms");
    step(&mut tap, 4801..=19_200);
    let again = bus.tapes.current(9).expect("the take is taped");
    let parted = taped(&again).iter().zip(&straight).position(|(a, b)| a.to_bits() != b.to_bits());
    assert!(Arc::ptr_eq(&first, &again) && again.frames() == first.frames() && parted.is_none(), "the same take tapes to its bound again, the straight run's by bits: {} frames, parting at {parted:?}", again.frames());
    assert_eq!(bus.tapes.bounded.load(Ordering::Relaxed), 2, "reaching the bound again counts again");
    // the run starts over: a new take tapes from its own start
    step(&mut tap, 0..=19_200);
    settle_tapes();
    let second = bus.tapes.current(9).expect("the next take is taped");
    assert!(!Arc::ptr_eq(&first, &second) && second.frames() == first.frames(), "the next take tapes again to its bound");
    assert_eq!(bus.tapes.bounded.load(Ordering::Relaxed), 3, "each take nobody hears stops at its bound");
}

#[test]
fn the_recording_follows_the_hand_and_plays_on_from_its_release() {
    const RATE: f64 = 2048.0;
    const TAKE_RATE: f64 = 4096.0;
    const BUFFER: usize = 16;
    let ring = SampleRing::new(256);
    ring.set_device_rate(RATE as u32);
    ring.set_open(true);
    let (ui, ends) = links();
    let mut player = Player::new(Some(ends));
    let mut out = [0.0; BUFFER];
    let fade = (FADE_SECONDS * RATE) as usize;
    let time = |frame: usize| 2.0 + frame as f64 / TAKE_RATE;
    let ramp = |frame: usize| (frame + 1) as f32 / 1024.0;
    let shape = Shape::new(1, CHUNK as f64 / TAKE_RATE, 1, TapMix::new(1));
    assert!(ui.shapes.try_send(Box::new(shape)).is_ok(), "the player receives the ramp's shape");
    let chunk = Delivery { take: Take { tap: 1, take: 1 }, index: 1,
        chunk: (0..CHUNK).map(ramp).collect::<Vec<_>>().into(), rate: TAKE_RATE, t: time(0) };
    assert!(ui.chunks.try_send(chunk).is_ok(), "the complete window arrives at its simulated time");

    ring.set_hushed(true);
    ring.set_scrub(Some(time(48)));
    player.fill(&ring, &mut out, RATE);
    for end in [80, 112, 144] {
        ring.set_scrub(Some(time(end)));
        player.fill(&ring, &mut out, RATE);
        assert!(out[fade..].windows(2).all(|w| w[0] < w[1]), "a forward drag plays rising samples while paused");
        assert_eq!(out[BUFFER - 1], ramp(end - 2), "the stroke's last sample is the take at its instant, a step short of the requested frame");
    }
    for end in [112, 80] {
        ring.set_scrub(Some(time(end)));
        player.fill(&ring, &mut out, RATE);
        assert!(out.windows(2).all(|w| w[0] > w[1]), "a backward drag plays falling samples");
        assert_eq!(out[BUFFER - 1], ramp(end + 2), "reverse reads the take at the head, where its heard clock is");
        let stamp = ring.heard_now(clock_now()).expect("the reverse stroke has a heard clock").stamped;
        let mid = ring.heard_now(stamp + 8.0 / RATE).expect("the reverse stroke has a midpoint");
        assert!(mid.moving && (mid.t - time(end + 16)).abs() < 1e-9, "the heard clock moves backward with the samples");
    }
    player.fill(&ring, &mut out, RATE);
    assert!(out[0] > 0.0 && out[fade..].iter().all(|&s| s == 0.0), "a resting hand fades to silence");
    player.fill(&ring, &mut out, RATE);
    assert_eq!(out, [0.0; BUFFER], "a stationary hand stays silent");

    ring.set_scrub(Some(time(96)));
    player.fill(&ring, &mut out, RATE);
    ring.set_scrub(None);
    player.fill(&ring, &mut out, RATE);
    let held = ring.heard_now(clock_now()).expect("release while paused keeps a heard head");
    assert!(!held.moving && held.t == time(96), "release while paused holds the last scratched frame");
    ring.set_hushed(false);
    player.fill(&ring, &mut out, RATE);
    assert_eq!(out[fade], ramp(96 + 2 * fade), "Play fades back in from the held frame");

    // Play from where the person is: paused, a seek, then Play plays from the instant sought, and
    // the heard clock (the drawn line) is there within one device buffer
    ring.set_hushed(true);
    player.fill(&ring, &mut out, RATE);
    ring.seek(time(200));
    ring.set_hushed(false);
    player.fill(&ring, &mut out, RATE);
    assert_eq!(out[fade], ramp(200 + 2 * fade), "Play goes on from the instant sought");
    let stamp = ring.heard_now(clock_now()).expect("the sought head is heard").stamped;
    let line = ring.heard_now(stamp).expect("the heard clock at its stamp");
    assert!(line.moving && (line.t - time(200)).abs() <= BUFFER as f64 / RATE + 1e-9, "the line is within one device buffer of the sound: {} vs {}", line.t, time(200));

    // a fast throw: eight frames a sample, the read reaching 128 frames each way (checked where
    // that stays inside the window; nearer its end it reads on into the loop's start)
    ring.set_looping(true);
    ring.set_scrub(Some(time(32)));
    player.fill(&ring, &mut out, RATE);
    ring.set_scrub(Some(time(440)));
    for end in [160, 288] {
        player.fill(&ring, &mut out, RATE);
        assert_eq!(out[BUFFER - 1], ramp(end - 8), "a fast throw catches up at four times normal speed");
    }
    player.fill(&ring, &mut out, RATE);
    player.fill(&ring, &mut out, RATE);
    ring.set_scrub(None);
    player.fill(&ring, &mut out, RATE);
    assert_eq!(out[0], ramp(440), "Loop continues at normal speed from the released position");
    ring.set_tap_speed(1.5);
    for _ in 0..2 * RATE as usize / BUFFER {
        player.fill(&ring, &mut out, RATE);
    }
    assert_eq!(out[0], ramp(440), "the resting worker's last speed leaves the released loop in time");
    ring.set_scrub(Some(time(CHUNK)));
    player.fill(&ring, &mut out, RATE);
    assert!(out[fade..].iter().all(|&s| s == 0.0), "a target at the window's exclusive end fades to silence");
    let held = ring.heard_now(clock_now()).expect("an uncovered target keeps the previous heard head");
    assert!(!held.moving && held.t == time(440 + 2 * BUFFER), "an uncovered target holds the last playable position");

    ring.set_looping(false);
    ring.set_scrub(None);
    player.fill(&ring, &mut out, RATE);
    assert!(!ring.plays_recording() && ring.heard_now(clock_now()).is_none(), "release on a playing open run returns to live audio");

    // At twice the take rate this window needs two chunks. Only its first has arrived.
    let partial_rate = 2.0 * TAKE_RATE;
    let partial = Delivery { take: Take { tap: 1, take: 2 }, index: 0,
        chunk: vec![0.5; CHUNK].into(), rate: partial_rate, t: 4.0 };
    assert!(ui.chunks.try_send(partial).is_ok(), "the second window has its first chunk");
    // four frames a sample: the read reaches 64 frames each way, so sample 7 (at frame CHUNK - 65)
    // reads the first chunk alone and sample 8 (four frames on) reads into the missing one
    let reach = 4 * RATE_HALF;
    let from = CHUNK - reach - 1 - 7 * 4;
    ring.set_looping(true);
    ring.set_scrub(Some(4.0 + from as f64 / partial_rate));
    player.fill(&ring, &mut out, RATE);
    ring.set_scrub(Some(4.0 + (from + 16 * 4) as f64 / partial_rate));
    player.fill(&ring, &mut out, RATE);
    assert!(out[7] == 0.5 && out[8] > 0.0 && out[8] < out[7] && out[15] == 0.0, "the scratch fades across a missing chunk");
    ring.set_scrub(None);
    player.fill(&ring, &mut out, RATE);
    assert!(!ring.plays_recording(), "an incomplete scratched window returns to live even when another window is whole");

    ring.set_realtime(true);
    ring.set_scrub(Some(time(48)));
    let mut producer = ring.claim().expect("the live producer claims its ring");
    producer.set_running(true);
    for _ in 0..BUFFER {
        assert!(producer.push(-0.25), "the live samples fit");
    }
    player.fill(&ring, &mut out, RATE);
    assert!(out[fade..].iter().all(|&s| s == -0.25), "Realtime hears live samples while the hand is down");
    assert!(!ring.plays_recording() && ring.heard_now(clock_now()).is_none(), "a Realtime scrub leaves the recorded head inactive");
}

#[test]
fn realtime_keeps_sample_order_across_gaps_and_sizes_its_lead_for_stalls() {
    const HZ: u32 = 2000;
    const BUFFER: usize = 20;
    let rate = f64::from(HZ);
    let ring = SampleRing::new(256);
    ring.set_device_rate(HZ);
    ring.set_open(true);
    let mut producer = ring.claim().expect("the worker claims its ring");
    let (ui, ends) = links();
    let mut player = Player::new(Some(ends));
    let mut out = [0.0; BUFFER];

    // Switch away from an actual recorded loop, still available throughout the proof.
    let shape = Shape::new(1, CHUNK as f64 / rate, 1, TapMix::new(1));
    assert!(ui.shapes.try_send(Box::new(shape)).is_ok(), "the player receives the recording shape");
    let chunk = Delivery { take: Take { tap: 1, take: 1 }, index: 0, chunk: vec![-0.5; CHUNK].into(), rate, t: 0.0 };
    assert!(ui.chunks.try_send(chunk).is_ok(), "the player receives a whole window");
    ring.set_ended(true);
    player.fill(&ring, &mut out, rate);
    player.fill(&ring, &mut out, rate);
    assert!(ring.plays_recording() && out[BUFFER - 1] < 0.0, "the recorded loop is heard before the switch");

    ring.set_realtime(true);
    ring.set_ended(false);
    producer.set_running(true);
    ring.set_tap_speed(0.8);
    let ramp = |i: usize| 0.25 + i as f32 / 65536.0;
    for i in 0..BUFFER {
        assert!(producer.push(ramp(i)), "the first device buffer fits");
    }
    player.fill(&ring, &mut out, rate);
    assert_eq!(ring.consumed(), BUFFER, "Realtime with a speed reading starts on one device buffer");
    let fade = (rate * FADE_SECONDS) as usize;
    assert!(out[0] < 0.0 && out[fade] == ramp(fade), "the switch fades from the recording into the live ramp");

    let mut gap = false;
    for _ in 0..125 {
        let first = ring.produced();
        for i in first..first + BUFFER * 4 / 5 {
            assert!(producer.push(ramp(i)), "the slow worker queues sample {i}");
        }
        let before = ring.consumed();
        let queued = ring.fill();
        player.fill(&ring, &mut out, rate);
        let consumed = ring.consumed() - before;
        assert!(consumed <= BUFFER, "the player never skips ahead in the ring");
        if queued >= BUFFER {
            assert_eq!(consumed, BUFFER, "play resumes as soon as a device buffer is ready");
        }
        for (i, &sample) in out[..consumed].iter().enumerate().skip(fade) {
            assert_eq!(sample, ramp(before + i), "the live ramp stays in order outside the fade at {}", before + i);
        }
        if consumed > fade && consumed < BUFFER {
            assert!(out[consumed] > 0.0 && out[consumed] < out[consumed - 1], "an underrun fades from the last live sample");
        }
        gap |= out[fade..].iter().all(|&s| s == 0.0);
        assert!(!ring.plays_recording(), "a slow callback stays live with a complete recorded window available");
    }
    producer.set_running(false);
    ring.set_ended(true);
    player.fill(&ring, &mut out, rate);
    assert_eq!(ring.consumed(), ring.produced(), "all produced samples are consumed, including the final partial buffer");
    assert!(!ring.plays_recording(), "an ended run also stays out of the recorded loop");
    assert!(gap && ring.underruns() > 0, "the slow run leaves silent gaps and counts underruns");
    let status = ring.play_status();
    assert!(status.behind && status.speed == 0.8, "the status reports the measured shortfall");
    assert_eq!(ring.ahead(), 2 * BUFFER, "a slow run does not inflate the lead");

    producer.set_running(true);
    ring.set_ended(false);
    ring.set_tap_speed(1.05);
    // each callback the worker tops the ring up to the lead (`LEAD`), or queues that many samples
    const LEAD: usize = usize::MAX;
    let mut callback = |feed: usize| {
        let n = if feed == LEAD { ring.ahead().saturating_sub(ring.fill()) } else { feed };
        for _ in 0..n {
            assert!(producer.push(0.5), "the measured lead fits in the ring");
        }
        player.fill(&ring, &mut out, rate);
    };
    for _ in 0..100 { callback(LEAD); }
    let status = ring.play_status();
    assert!(!status.behind && status.speed == 1.05, "the recovered speed clears the behind indicator");
    assert!((status.buffer_ms - 10.0).abs() < 1e-4 && (status.lead_ms - 20.0).abs() < 1e-4, "the status separates the device buffer and live lead");
    let underruns = ring.underruns();
    for _ in 0..3 { callback(0); }
    assert_eq!(ring.underruns(), underruns + 1, "a worker stall counts as one underrun");
    assert_eq!(ring.ahead(), 4 * BUFFER, "one stall doubles the two-buffer lead");
    // a run held to its clock steps a buffer a callback: Realtime resumes on one, and the status
    // reports what the ring holds and a buffer, not the doubled lead
    for _ in 0..10 { callback(BUFFER); }
    let status = ring.play_status();
    assert!((status.latency_s - (2 * BUFFER) as f32 / rate as f32).abs() < 1e-6 && ring.underruns() == underruns + 1, "the latency is what the ring holds and a buffer, not the doubled lead: {status:?}");
    for _ in 0..2000 { callback(LEAD); }
    assert_eq!(ring.ahead(), 2 * BUFFER, "quiet playback returns to its two-device-buffer floor");
    // a run that starts over has no speed until its own first reading: the last run's slow one
    // leaves the status at the next callback, not at the player's next reading
    ring.set_tap_speed(0.8);
    for _ in 0..100 { callback(LEAD); }
    assert!(ring.play_status().behind, "a reading below real time reads behind");
    let mut tap = AudioTap::new(spec(1.0, 0.0), ring.clone());
    tap.begin(&unknowns());
    assert_eq!(ring.tap_speed(), 0.0, "a new run's tap has no reading yet");
    callback(LEAD);
    let status = ring.play_status();
    assert!(!status.behind && status.speed == 0.0, "a new run reads no speed, not the last run's: {}", status.speed);
    // a new run starts on two device buffers: its first rounds can step a little under real
    // time, and a start on one runs dry at once, a click; the lead stays as it was
    producer.set_running(false);
    while ring.fill() > 0 {
        player.fill(&ring, &mut out, rate);
    }
    player.fill(&ring, &mut out, rate);
    producer.set_running(true);
    let (before, ahead, underruns) = (ring.consumed(), ring.ahead(), ring.underruns());
    for _ in 0..BUFFER {
        assert!(producer.push(0.5), "the new run's first buffer fits");
    }
    player.fill(&ring, &mut out, rate);
    assert_eq!(ring.consumed(), before, "a new run's start waits for a second device buffer");
    for _ in 0..BUFFER {
        assert!(producer.push(0.5), "the new run's second buffer fits");
    }
    player.fill(&ring, &mut out, rate);
    assert_eq!(ring.consumed(), before + BUFFER, "a new run plays once two device buffers are ready");
    assert_eq!((ring.ahead(), ring.underruns()), (ahead, underruns), "the two-buffer start keeps the lead and counts no underrun");
}

/// No switch clicks, and the lead follows the worker: a steady 0.5 played through a start, a
/// stall, a slow stretch and a fast one never steps by more than two crossfades' slopes. A stall
/// of a run that keeps up is one short fade with the lead doubled, not rendering; the lead starts
/// on two device buffers and quiet play sizes it back down there, and live play primes to it, so
/// a paced run holds it; a run measured below real time renders.
#[test]
fn every_switch_crossfades_and_the_lead_follows_the_worker() {
    let rate = 48_000.0;
    let ring = SampleRing::new(1 << 17);
    ring.set_device_rate(48_000);
    ring.set_open(true);
    // the lead a tap asks before the device's first buffer (60 ms)
    ring.set_ahead(2880);
    let mut producer = ring.claim().expect("the producer");
    producer.set_running(true);
    let mut player = Player::new(None);
    let mut out = vec![0.0f32; 480];
    let (mut last, mut steepest) = (0.0f32, 0.0f32);
    ring.set_tap_speed(1.0);
    // a worker that keeps up tops the ring up to the lead, as the tap's backpressure does
    const KEEPS_UP: usize = usize::MAX;
    let mut play = |player: &mut Player, producer: &mut fab_elec_sim::listen::RingProducer, produce: usize, fills: usize| {
        for _ in 0..fills {
            let n = if produce == KEEPS_UP { ring.ahead().saturating_sub(ring.fill()) } else { produce };
            for _ in 0..n {
                producer.push(0.5);
            }
            player.fill(&ring, &mut out, rate);
            for &s in &out {
                steepest = steepest.max((s - last).abs());
                last = s;
            }
        }
    };
    // a start, then real time long enough to measure it
    play(&mut player, &mut producer, KEEPS_UP, 120);
    assert_eq!((ring.underruns(), ring.play_status().rendered), (0, false), "a run at real time plays live");
    assert_eq!(ring.ahead(), 960, "the lead starts on two device buffers, whatever the tap asked, as in Realtime");
    // the worker stalls for three buffers, then steps paced, a buffer a callback, as a run held
    // to its clock does: the ring holds what play primed again, the doubled lead
    play(&mut player, &mut producer, 0, 3);
    play(&mut player, &mut producer, 480, 10);
    assert_eq!(ring.underruns(), 1, "the stall is one underrun");
    assert!(!ring.play_status().rendered && ring.ahead() == 1920, "one stall doubles the two-buffer lead, heard live again: {:?} ahead {}", ring.play_status(), ring.ahead());
    let st = ring.play_status();
    assert!((st.latency_s - (1920 + 480) as f32 / 48_000.0).abs() < 1e-6, "play primes to the doubled lead: a change waits behind it and a buffer: {st:?}");
    // quiet play brings the lead back down to two buffers, and what is heard waits behind that
    play(&mut player, &mut producer, KEEPS_UP, 2000);
    assert_eq!(ring.ahead(), 960, "quiet playback returns to its two-device-buffer floor");
    let st = ring.play_status();
    assert!((st.latency_s - (ring.ahead() + 480) as f32 / 48_000.0).abs() < 1e-6, "topped up to its lead, the latency is the lead and a buffer: {st:?}");
    // below real time its recording plays (none here: silence), and faster it is live again
    ring.set_tap_speed(0.4);
    play(&mut player, &mut producer, 192, 600);
    assert!(ring.play_status().rendered, "a slow run plays its recording: {:?}", ring.play_status());
    ring.set_tap_speed(2.0);
    play(&mut player, &mut producer, 960, 200);
    assert!(!ring.play_status().rendered, "a fast run is live again: {:?}", ring.play_status());
    let fade = 0.5 / (rate * fab_elec_sim::listen::FADE_SECONDS) as f32;
    assert!(steepest <= 2.0 * fade + 1e-6, "no step is steeper than two crossfades' slopes: {steepest} (a crossfade is {fade} a sample)");
}

/// The windows (by number, `w` device frames each) whose runs a stretch of what was heard plays,
/// each played from its start to its end but the last, which may still be playing, and the first
/// when the stretch begins `mid` window.
fn whole_windows(heard: &[f32], rate: f64, w: f64, mid: bool, what: &str) -> Vec<f64> {
    let fade = rate * FADE_SECONDS;
    let played = runs(heard, 0.5, rate);
    assert!(played.len() >= 3, "{what}: windows are heard: {played:?}");
    for &(first, len) in &played[usize::from(mid)..played.len() - 1] {
        let at = first - ((first - fade) / w).round() * w;
        assert!((at - fade).abs() < 2.0 && (len as f64 - (w - fade)).abs() < 2.0, "{what}: a window plays from its start to its end: {first} for {len}, a window is {w}");
    }
    played.iter().map(|&(first, _)| ((first - fade) / w).round()).collect()
}

/// A take is made at its run's sample rate whatever the device plays at, and played at the
/// device's: a 96 kHz take heard at 48 kHz plays each window from its start to its end, every
/// sample the two take frames it covers, so the ramp is heard frame by frame as at 48 kHz. A
/// device that changes its rate makes no new take: the take goes on at its rate and is heard at
/// the new one, window by window; once kept, it replays there when the run starts over, every
/// chunk the render makes again equal to the kept one. A run that starts over while the window
/// heard still renders is heard from the new take's start: that render ended, so its play head
/// does not wait for it.
#[test]
fn a_take_is_made_at_its_runs_rate_and_played_at_the_devices() {
    let window_s = 0.25;
    let take_w = window_frames(window_s, 96_000.0) as f64;
    let runs_slot = Arc::new(RunSlot::default());
    runs_slot.set(Some(Arc::new(RunContent::new(9, None, 96_000.0))));
    let mut rig = Rig::keyed(window_s, runs_slot);
    for _ in 0..50 {
        rig.run(0.4, 0.4, 1);
        if rig.ring.play_status().rendered {
            break;
        }
    }
    assert!(rig.ring.play_status().rendered, "the run below real time plays its recording");
    let from = rig.heard.len();
    rig.run(0.4, 0.4, 400);
    let tape = rig.tapes.current(1).expect("the take is taped");
    assert_eq!((tape.rate, tape.take.take), (96_000.0, 0), "the take is at its run's rate, not the device's");
    let at48 = whole_windows(&rig.heard[from..], 48_000.0, take_w / 2.0, false, "a 96 kHz take at 48 kHz");
    assert!(at48.windows(2).all(|p| p[1] >= p[0]) && at48.first() == Some(&0.0) && at48.contains(&1.0), "window 0 round, then window 1: {at48:?}");
    // the device goes to 44.1 kHz mid window: the same take goes on, heard at the new rate from
    // there, and window by window after
    let frames = tape.frames();
    rig.set_device(44_100);
    let from = rig.heard.len();
    rig.run(0.4, 0.4, 400);
    let now = rig.tapes.current(1).expect("the take is taped");
    assert!(Arc::ptr_eq(&now, &tape) && now.frames() > frames, "the device's change makes no new take; the take goes on");
    let at44 = whole_windows(&rig.heard[from..], 44_100.0, take_w * 44_100.0 / 96_000.0, true, "the take at 44.1 kHz");
    assert!(at44.windows(2).all(|p| p[1] >= p[0] && p[1] - p[0] <= 1.0), "each window after the last: {at44:?}");
    // the run starts over with the same content while the window heard still renders: the kept
    // take replays at 44.1 kHz from its start, not the old window waiting for a render that ended
    let from = rig.heard.len();
    rig.t = 0.0;
    rig.run(0.4, 0.4, 300);
    assert!(rig.tapes.replayed.load(Ordering::Relaxed) > 0, "the kept take replays on the new device");
    assert_eq!(rig.tapes.differed.load(Ordering::Relaxed), 0, "every chunk rendered again equals the kept one");
    let again = runs(&rig.heard[from..], 0.5, 44_100.0);
    assert!(again.iter().any(|&(first, _)| (first - 44_100.0 * FADE_SECONDS).abs() < 2.0), "the kept take is heard from its start: {again:?}");
}

/// A tap's take of `signal` (volts at a time, a point every 1/192 000 s for `seconds`, every step
/// free) when its run's grid is at `rate` Hz, as its tape holds it: the run's read at each instant.
fn take_of(rate: f64, seconds: f64, signal: impl Fn(f64) -> f64) -> Vec<f32> {
    let runs = Arc::new(RunSlot::default());
    runs.set(Some(Arc::new(RunContent::new(1, None, rate))));
    let bus = ListenBus::new(1 << 16, runs);
    // a closed ring: the live side never waits on it
    bus.ring.set_device_rate(48_000);
    let spec = TapSpec { voices: vec![voice("signal", 1.0)], latency: 0.01, dc_block_hz: 0.0, mix: TapMix::new(1) };
    let mut tap = AudioTap::new(spec, bus.ring.clone()).recording(bus.record(1, 1, 0.25));
    tap.begin(&unknowns());
    tap.sample_period(1.0 / rate);
    for k in 0..=(seconds * 192_000.0) as usize {
        let t = k as f64 / 192_000.0;
        tap.point(t, None, &[0.0, signal(t)]);
        if k > 0 {
            tap.chosen(t);
        }
    }
    settle_tapes();
    let tape = bus.tapes.current(1).expect("the take is taped");
    assert_eq!(tape.rate, rate, "the take is at its run's rate");
    let mut out = vec![0.0f32; tape.frames() as usize];
    assert_eq!(tape.read(0, &mut out).expect("the tape reads back"), out.len());
    out
}

/// A take played at `step` of its frames a sample (the read's rate change), `n` samples.
fn played(tape: &[f32], step: f64, n: usize) -> Vec<f64> {
    (0..n)
        .map(|i| {
            let mut sample = 0.0;
            let sum = rate_change(i as f64 * step, step, 0, tape.len() as i64 - 1, |k, w| {
                sample += w * f64::from(tape[k as usize]);
                true
            });
            sample / sum.expect("a take's frames are all there")
        })
        .collect()
}

/// The output samples a comparison of takes leaves out at each end: the read's own start and end
/// (its kernel's reach at the take's rate) and the rate change's reach past them.
const ENDS: usize = 2 * RATE_HALF + 1;

fn rms(x: impl Iterator<Item = f64>) -> f64 {
    let (sum, n) = x.fold((0.0, 0usize), |(s, n), v| (s + v * v, n + 1));
    (sum / n.max(1) as f64).sqrt()
}

/// A take's level played at `step` of its frames a sample against `at`, the take made at the
/// device's rate, in dB: away from the two ends ([`ENDS`]).
fn played_db(made: &[f32], at: &[f32], step: f64) -> f64 {
    let n = at.len().min(((made.len() - 1) as f64 / step) as usize);
    let heard = played(made, step, n);
    20.0 * (rms(heard[ENDS..n - ENDS].iter().copied()) / rms(at[ENDS..n - ENDS].iter().map(|&x| f64::from(x)))).log10()
}

/// A take played at another rate is the take made at it, away from the takes' ends ([`ENDS`]): a
/// 96 kHz take at 48 kHz (each sample the take at its instant through the read's rate change) is
/// the 48 kHz take within the read's kernel, 80 dB under the tone, at 1, 10 and 20 kHz; at 44.1
/// kHz a 20 kHz tone, the top third octave, is within 0.5 dB of the 44.1 kHz take's.
#[test]
fn a_take_played_at_another_rate_is_the_take_made_at_it() {
    let tone = |hz: f64| move |t: f64| 0.3 * (TAU * hz * t).sin();
    for hz in [1000.0, 10_000.0, 20_000.0] {
        let (t96, t48) = (take_of(96_000.0, 0.05, tone(hz)), take_of(48_000.0, 0.05, tone(hz)));
        let n = t48.len().min(t96.len() / 2);
        let half = played(&t96, 2.0, n);
        let off = half[ENDS..n - ENDS].iter().zip(&t48[ENDS..n - ENDS]).map(|(a, &b)| (a - f64::from(b)).abs()).fold(0.0, f64::max);
        assert!(off < 1e-4 * 0.3, "a 96 kHz take at 48 kHz is the 48 kHz take at {hz} Hz: off by {off}");
    }
    let (t96, t44) = (take_of(96_000.0, 0.05, tone(20_000.0)), take_of(44_100.0, 0.05, tone(20_000.0)));
    let db = played_db(&t96, &t44, 96_000.0 / 44_100.0);
    assert!(db.abs() < 0.5, "a 96 kHz take at 44.1 kHz, 20 kHz: {db} dB from the 44.1 kHz take");
}


/// A take whose run went back renders again from a chunk on, and the player takes the newer
/// render in as it comes without a click (Fable's test 4: the click measure of Follow-up 3).
/// The player alone, one voice, windows of 8 chunks at 48 kHz played at 48 kHz (a device sample a
/// frame); an older and a newer render that agree up to a known frame in chunk 2 and then part: a
/// hat-like pair of tones in both, and in the newer a low shift and tone growing in. Chunks of the
/// newer render come before the play head reaches them, under it (in the chunk it plays) and
/// behind it; the play head reaches the newer render's front (the older render after it) on each
/// pass; then the next window comes in the newer render and the player moves to it.
/// - at every swap, front, loop and move, the largest second difference of what is heard over a
///   fade is under 1.5 times the largest elsewhere;
/// - the newer render is heard from the slot it replaced, and the older plays on past its front;
/// - the next window, rendered before the return, waits until its newer render can play;
/// - every chunk replaced comes back to the UI, the front's once the slot after it is replaced.
#[test]
fn a_render_swapped_in_under_the_play_head_never_clicks() {
    const WINDOW: usize = 8 * CHUNK;
    const PART: usize = 2 * CHUNK + 100;
    let tone = |hz: f64, n: usize| (TAU * hz * n as f64 / RATE).sin();
    let older = |n: usize| 0.2 * tone(150.0, n) + 0.015 * tone(7000.0, n) + 0.01 * (TAU * 9300.0 * n as f64 / RATE + 1.0).sin();
    let newer = |n: usize| {
        let grown = if n < PART { 0.0 } else { 1.0 - (-((n - PART) as f64) / 300.0).exp() };
        older(n) + grown * (0.25 + 0.08 * (TAU * 210.0 * n as f64 / RATE + 0.5).sin())
    };
    let chunk = |index: usize, render: &dyn Fn(usize) -> f64| -> Arc<[f32]> { (0..CHUNK).map(|k| render(index * CHUNK + k) as f32).collect() };
    let take = Take { tap: 1, take: 1 };
    let (ui, ends) = links();
    let send = |index: usize, c: &Arc<[f32]>| assert!(ui.chunks.try_send(Delivery { take, index: index as u64, chunk: c.clone(), rate: RATE, t: (index * CHUNK) as f64 / RATE }).is_ok(), "chunk {index} goes");
    let ring = SampleRing::new(1 << 12);
    ring.set_device_rate(48_000);
    ring.set_open(true);
    let mut producer = ring.claim().expect("the producer");
    producer.set_running(true);
    ring.set_tap_speed(0.4);
    let mut player = Player::new(Some(ends));
    assert!(ui.shapes.try_send(Box::new(Shape::new(1, WINDOW as f64 / RATE, 1, TapMix::new(1)))).is_ok(), "the shape goes");
    let old: Vec<Arc<[f32]>> = (0..16).map(|i| chunk(i, &older)).collect();
    let new: Vec<Arc<[f32]>> = (0..16).map(|i| chunk(i, &newer)).collect();
    for (i, c) in old[..8].iter().enumerate() {
        send(i, c);
    }
    // live until the ring runs dry below real time: then the recording plays, window 0 first
    for _ in 0..1024 {
        producer.push(0.0);
    }
    let mut out = [0.0f32; 4];
    while !ring.play_status().rendered {
        player.fill(&ring, &mut out, RATE);
    }
    // the play head's sample `i` of window 0 is frame i (mod the window) until the move
    let mut heard: Vec<f32> = Vec::new();
    let mut play_to = |player: &mut Player, to: usize| {
        while heard.len() < to {
            player.fill(&ring, &mut out, RATE);
            heard.extend_from_slice(&out);
            ui.spent.drain();
        }
    };
    play_to(&mut player, 100);
    // window 1 as rendered before the edit; then the run goes back to chunk 2
    for i in 8..16 {
        send(i, &old[i]);
    }
    play_to(&mut player, 300);
    send(2, &new[2]);
    play_to(&mut player, 1736);
    send(3, &new[3]);
    play_to(&mut player, 2600);
    send(4, &new[4]);
    send(5, &new[5]);
    play_to(&mut player, 2604);
    let refs = |c: &Arc<[f32]>| Arc::strong_count(c);
    assert!(refs(&old[4]) == 1 && refs(&old[5]) == 2, "the front's replaced chunk is kept (5: {}), the one before it back (4: {})", refs(&old[5]), refs(&old[4]));
    play_to(&mut player, WINDOW + 3500);
    send(6, &new[6]);
    send(7, &new[7]);
    for i in 8..16 {
        send(i, &new[i]);
    }
    play_to(&mut player, 2 * WINDOW + 1200);
    // what was heard where: the newer render from the slot it replaced, the older on past its
    // front, window 0 again while window 1 waited for its newer render, then window 1's
    let at = |i: usize, render: &dyn Fn(usize) -> f64| soft_limit(f64::from(render(i) as f32)) as f32;
    let same = |from: usize, to: usize, frame: &dyn Fn(usize) -> usize, render: &dyn Fn(usize) -> f64| (from..to).all(|i| heard[i] == at(frame(i), render));
    let w0 = |i: usize| i % WINDOW;
    assert!(same(WINDOW + 200, WINDOW + 3000, &w0, &newer), "the second pass plays window 0 again, the newer render from chunk 2 to its front");
    assert!(same(WINDOW + 3300, WINDOW + 3500, &w0, &older), "the older render plays on past the front");
    assert!(same(2 * WINDOW + 400, 2 * WINDOW + 1200, &|i| i - WINDOW, &newer), "window 1 plays in the newer render once it can");
    // the click measure: each boundary's fade against everywhere else (the start's fade aside)
    let fade = (RATE * FADE_SECONDS) as usize;
    let d2: Vec<f32> = (2..heard.len()).map(|i| (heard[i] - 2.0 * heard[i - 1] + heard[i - 2]).abs()).collect();
    let bounds = [("the front at chunk 3", 3 * CHUNK), ("a swap under the head", 1736), ("the front at chunk 4", 4 * CHUNK), ("a swap under the head", 2600), ("the front at chunk 6", 6 * CHUNK), ("the loop", WINDOW), ("the front at chunk 6, next pass", WINDOW + 6 * CHUNK), ("a swap under the head, next pass", WINDOW + 3500), ("the move to window 1", 2 * WINDOW)];
    let near = |i: usize, b: usize| i + 4 >= b && i <= b + fade + 4;
    let elsewhere = (fade + 4..d2.len()).filter(|&i| !bounds.iter().any(|&(_, b)| near(i + 2, b))).map(|i| d2[i]).fold(0.0f32, f32::max);
    for (what, b) in bounds {
        let worst = (b - 6..=b + fade + 2).map(|i| d2[i]).fold(0.0f32, f32::max);
        assert!(worst < 1.5 * elsewhere, "no click at {what} (sample {b}): second difference {worst} against {elsewhere} elsewhere");
    }
    // every replaced chunk came back: the front's too, once the slot after it was replaced
    let held: Vec<usize> = (2..16).filter(|&i| refs(&old[i]) != 1).collect();
    assert!(held.is_empty() && (2..16).all(|i| refs(&new[i]) == 2), "every replaced chunk comes back, every newer one is in its slot: held {held:?}");
}
