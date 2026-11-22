//! The capture clock and the run clock are driven here, one callback at a time.

use fab_elec_fw::mic::{MicInput, MicPort, MicStatus, MicWriter};
use fab_elec_fw::sound_track;
use fab_elec_mcu_parts::SoundTrack;
use fab_elec_sim::input::{InputSource, Inputs};
use fab_elec_sim::live::{LiveCmd, ValuePlan};
use fab_elec_sim::livesim::{LiveSim, LiveSpec};
use fab_elec_sim::{SimCircuit, Sink, Source};
use std::sync::{atomic::AtomicU64, Arc};

const DEVICE: u32 = 48_000;
const CALLBACK: usize = 193;

// A mono float WAV gives the callback the very same f32 values the file converter decodes.
fn wav(rate: u32, frames: usize) -> Vec<u8> {
    let size = frames as u32 * 4;
    let mut bytes = Vec::from(*b"RIFF");
    bytes.extend((36 + size).to_le_bytes());
    bytes.extend(b"WAVEfmt ");
    bytes.extend(16_u32.to_le_bytes());
    bytes.extend(3_u16.to_le_bytes());
    bytes.extend(1_u16.to_le_bytes());
    bytes.extend(rate.to_le_bytes());
    bytes.extend((rate * 4).to_le_bytes());
    bytes.extend(4_u16.to_le_bytes());
    bytes.extend(32_u16.to_le_bytes());
    bytes.extend(b"data");
    bytes.extend(size.to_le_bytes());
    for i in 0..frames {
        let sample = ((i * 89 % 1021) as f32 - 510.0) / 491.0;
        bytes.extend(sample.to_le_bytes());
    }
    bytes
}

fn samples(wav: &[u8]) -> Vec<f32> {
    wav[44..].chunks_exact(4).map(|b| f32::from_le_bytes(b.try_into().unwrap())).collect()
}

fn same_track(actual: &SoundTrack, expected: &SoundTrack) {
    assert_eq!(actual.len(), expected.len());
    assert_eq!(actual.period_s(), expected.period_s());
    assert_eq!(actual.horizon_s(), expected.horizon_s());
    for i in 0..expected.len() {
        assert_eq!(actual.sample(i), expected.sample(i), "PCM sample {i}");
    }
}

struct Discard;
impl Sink for Discard {}

fn capture(device: u32, rate: u32, db: f64, origin: u64) -> (MicWriter, Arc<MicInput>) {
    let port = MicPort::new(device).unwrap();
    let writer = port.writer(device).unwrap();
    let source = MicInput::new(port, device, rate, db, origin, "U1".into()).unwrap();
    (writer, source)
}

fn paced(wav: &[u8], ticks_per_callback: usize) -> MicStatus {
    let rate = 16_000;
    let db = -9.0;
    let (mut writer, source) = capture(DEVICE, rate, db, 0);
    let mut circuit = SimCircuit::new();
    circuit.v("v1", "in", "0", Source::dc(1.0));
    circuit.r("r1", "in", "0", 1e3);
    let spec = LiveSpec { playing: false, max_step: 0.00025, trace: false, ..LiveSpec::default() };
    let mut sim = LiveSim::new(circuit, ValuePlan::default(), 1, spec).unwrap();
    assert!(sim.apply(LiveCmd::Inputs(Inputs {
        sources: vec![source.clone()], want: Arc::new(AtomicU64::new(0)),
    })));
    sim.values(1.0);

    let audio = samples(wav);
    let expected = sound_track(wav, rate, db, 0).unwrap();
    let mut compared = 0;
    for (i, chunk) in audio.chunks(CALLBACK).enumerate() {
        writer.push(chunk);
        sim.input_starved();
        let fed = i * CALLBACK + chunk.len();
        let complete = fed as u64 * u64::from(rate) / u64::from(DEVICE);
        assert_eq!(source.input.track.len(), complete, "complete codec periods after callback {i}");
        for k in compared..complete {
            assert_eq!(source.input.track.sample(k), expected.sample(k), "open track sample {k}");
        }
        compared = complete;
        for offset in 0..ticks_per_callback {
            let tick = i * ticks_per_callback + offset;
            // Start with a callback in hand. At half speed the run catches the capture.
            let target = (tick.saturating_sub(1) * CALLBACK) as f64 / f64::from(DEVICE);
            while sim.t() < target {
                let before = sim.t();
                sim.advance(target, None, &mut Discard).unwrap();
                if sim.input_starved() {
                    let status = source.status();
                    let short = sim.t() + sim.step_cap() + source.period() - source.horizon().unwrap();
                    assert!((short - status.behind_s).abs() < 1e-12 && short > 0.0);
                    let held = sim.t();
                    sim.advance(target, None, &mut Discard).unwrap();
                    assert_eq!(sim.t(), held);
                    assert_eq!(source.status().holds, status.holds, "one continuous hold");
                    break;
                }
                assert!(sim.t() > before);
            }
        }
    }
    assert!(!source.input.track.is_closed());
    drop(writer);
    assert!(!sim.input_starved(), "the last callback is drained and the run is released");
    assert!(source.input.track.is_closed());
    same_track(&source.input.track, &expected);
    source.status()
}

#[test]
fn the_same_wav_at_normal_and_half_speed() {
    let wav = wav(DEVICE, CALLBACK * 100 + 1);
    assert_eq!(paced(&wav, 1), MicStatus::default());
    let slow = paced(&wav, 2);
    assert!(slow.holds > 1, "the run held and resumed as more sound arrived");
    assert_eq!((slow.overflows, slow.behind_s), (0, 0.0));
}

#[test]
fn fractional_rates_keep_phase_across_callbacks_and_finish_the_last_sample() {
    let origin = 38_000_000_000;
    for (device, rate) in [(DEVICE, 22_050), (44_100, 48_000), (DEVICE, DEVICE)] {
        let wav = wav(device, 709);
        let (mut writer, source) = capture(device, rate, 0.0, origin);
        for chunk in samples(&wav).chunks(7) {
            writer.push(chunk);
            assert!(source.poll_horizon(0.0).is_some());
        }
        drop(writer);
        assert_eq!(source.poll_horizon(0.0), None);
        same_track(&source.input.track, &sound_track(&wav, rate, 0.0, origin).unwrap());
        assert_eq!(source.status(), MicStatus::default());
    }
}

#[test]
fn the_first_device_buffers_survive_rate_discovery_and_a_ring_change() {
    let wav = wav(DEVICE, 2 * CALLBACK + 1);
    let audio = samples(&wav);
    let expected = sound_track(&wav, 16_000, 0.0, 0).unwrap();
    for reserve in [DEVICE, 8_000] {
        let first = MicPort::new(reserve).unwrap();
        let mut writer = first.writer(DEVICE).unwrap();
        writer.push(&audio[..CALLBACK]);
        let source = MicInput::new(first.clone(), DEVICE, 16_000, 0.0, 0, "U1".into()).unwrap();
        assert!(source.poll_horizon(0.0).is_some());
        assert_eq!(source.input.track.len(), CALLBACK as u64 / 3);
        if !Arc::ptr_eq(&first, source.port()) {
            writer.move_to(source.port().writer(DEVICE).unwrap());
        }
        writer.push(&audio[CALLBACK..]);
        source.finish();
        same_track(&source.input.track, &expected);
        assert_eq!(source.status().overflows, 0);
    }
}

#[test]
fn a_level_change_applies_from_the_next_drain() {
    let (mut writer, source) = capture(DEVICE, DEVICE, 0.0, 0);
    writer.push(&[1.0]);
    assert!(source.poll_horizon(0.0).is_some());
    source.set_level_db(-6.0206).unwrap();
    writer.push(&[1.0]);
    drop(writer);
    assert_eq!(source.poll_horizon(0.0), None);
    assert_eq!(source.input.track.sample(0), Some(32767));
    assert_eq!(source.input.track.sample(1), Some(16383));
}

#[test]
fn pausing_without_another_run_poll_finishes_the_admitted_buffers() {
    let wav = wav(DEVICE, CALLBACK + 1);
    let (mut writer, source) = capture(DEVICE, 16_000, -12.0, 0);
    writer.push(&samples(&wav));
    source.stop();
    writer.push(&[1.0, 1.0, 1.0]);
    source.finish();
    assert_eq!(source.horizon(), None);
    same_track(&source.input.track, &sound_track(&wav, 16_000, -12.0, 0).unwrap());
}

#[test]
fn four_seconds_without_a_drain_counts_each_overflow_episode_once() {
    let capacity = 4 * DEVICE as usize;
    let wav = wav(DEVICE, capacity + CALLBACK);
    let audio = samples(&wav);
    let (mut writer, source) = capture(DEVICE, DEVICE, 0.0, 0);
    // Stop the run clock for four seconds of callbacks. The very next frame is the first loss.
    for chunk in audio[..capacity].chunks(CALLBACK) { writer.push(chunk); }
    assert_eq!(source.status().overflows, 0);
    writer.push(&audio[capacity..capacity + 1]);
    assert_eq!(source.status().overflows, 1);
    writer.push(&audio[capacity + 1..]);
    assert_eq!(source.status().overflows, 1);
    assert!(source.poll_horizon(0.0).is_some());
    assert_eq!(source.input.track.len(), capacity as u64);

    // Fill it again across the ring's wrap. One callback straddles the four-second limit.
    for chunk in audio.chunks(CALLBACK) { writer.push(chunk); }
    assert_eq!(source.status().overflows, 2);
    drop(writer);
    assert_eq!(source.poll_horizon(0.0), None);
    let expected = sound_track(&wav, DEVICE, 0.0, 0).unwrap();
    assert_eq!(source.input.track.len(), (2 * capacity) as u64);
    for i in 0..(2 * capacity) as u64 {
        assert_eq!(source.input.track.sample(i), expected.sample(i % capacity as u64));
    }
}
