//! The twin's speaker through the S3 model: firmware sets an ES8311 up over I2C, raises its
//! amplifier's enable and streams one second of a 1 kHz tone at 16 kHz from a GDMA descriptor
//! chain to I2S0. What the co-simulated device commits is the tone, frame for frame, however a
//! hit cuts its stretches; sound adds no commit and no analog landing; and the device tap turns it
//! into 48 kHz for a consumer with the tone's pitch and no images.

use std::sync::Arc;

use fab_elec_mcu_cosim::{CoMachine, Config};
use fab_elec_mcu_emit32::xtensa::{ccz, enc};
use fab_elec_mcu_parts::Netlist;
use fab_elec_mcu_s3::S3Machine;
use fab_elec_sim::cosim::{Bus, CoAudio, CoSim, CoSimPin};
use fab_elec_sim::device_audio::{DevicePlayer, DeviceTap};
use fab_elec_sim::fft::{spectrum, Window};
use fab_elec_sim::listen::SampleRing;
use fab_elec_sim::{PinEdge, PinMode, SimDiag, Sink};

mod support;
#[path = "support/speaker_board.rs"]
mod speaker_board;
use speaker_board::{AMP_EN, CODEC};
use support::i2c_fixture::{i2c_write, I2C_CLR, I2C_COMPLETE, I2C_RSTART, I2C_STOP};
use support::{mux_reg, out_sel_reg, pin_reg, profile, Program, GPIO_ENABLE, GPIO_OUT, MUX_GPIO, RAM};

// trm.txt:13407, 13450.
const I2S: u32 = 0x6000_f000;
const DMA: u32 = 0x6003_f000;
// Fixture allocations in SRAM: sixteen descriptors of 1000 frames (4000 bytes) each, linked in
// order, the last ending the list.
const DESC: u32 = RAM + 0x1000;
const BUF: u32 = RAM + 0x2000;
const FRAMES_PER_DESC: u32 = 1000;
const DESCS: u32 = 16;
const FRAMES: usize = (FRAMES_PER_DESC * DESCS) as usize;
/// The input pad a scripted step hits (not in the board, read by nobody).
const HIT_PIN: &str = "GPIO2";
const VDD: f64 = 3.3;
/// The S3's live stretch: 0.2 ms on its 480 MHz tick grid (`fab_elec_fw`).
const STRETCH: u64 = 96_000;
const OUT_HZ: u32 = 48_000;
const BUFFER: usize = 480;

/// The tone, left slot: 1 kHz at 16 kHz is sixteen samples a period, at half scale.
fn tone(k: usize) -> i16 {
    (16384.0 * (std::f64::consts::TAU * k as f64 / 16.0).sin()).round() as i16
}

fn delay(p: &mut Program, n: u32) {
    p.load(4, n);
    let start = p.emit(enc::addi(4, 4, -1));
    p.emit(enc::bccz(ccz::NE, 4, (i64::from(start) - i64::from(p.pc()) - 4) as i32));
}

fn codec_write(p: &mut Program, reg: u8, value: u8) {
    p.i2c_list(&[CODEC << 1, reg, value], &[I2C_RSTART, i2c_write(3), I2C_STOP]);
    p.i2c_wait();
    p.store(I2C_CLR, I2C_COMPLETE);
}

fn poll_clear(p: &mut Program, addr: u32, mask: u32) {
    p.load(1, addr);
    p.load(2, mask);
    let again = p.emit(enc::l32i(3, 1, 0));
    p.emit(enc::and(3, 3, 2));
    p.emit(enc::bccz(ccz::NE, 3, (i64::from(again) - i64::from(p.pc()) - 4) as i32));
}

/// The firmware: the codec up by manual power-up for 16 kHz Philips 16-bit frames with a 256x
/// MCLK, its DAC at 0 dB and unmuted, the amplifier on, then I2S0 TX from the descriptor chain
/// (`sound`; otherwise the same writes with TX left stopped), then WAITI.
fn image(sound: bool) -> Arc<Vec<u8>> {
    let mut p = Program::new();
    p.disable_watchdogs();
    // trm.txt:13455, 28796-28892: SYSTEM I2S0 and DMA clocks out of reset.
    for (clk, rst, bit) in [(0x600c_0018, 0x600c_0020, 4), (0x600c_001c, 0x600c_0024, 6)] {
        p.time_bits(rst, 0, 1 << bit);
        p.time_bits(clk, 0, 1 << bit);
        p.time_bits(rst, 1 << bit, 0);
    }
    p.i2c_setup();
    // es8311-user-guide.txt:1324-1375: CSM off with the digital blocks in reset, several ms, then
    // the clocks, format, analog and DAC settings, CSM on last with manual power-up (SEQ_DIS).
    codec_write(&mut p, 0x00, 0x1f);
    delay(&mut p, 60_000);
    for (reg, value) in [(0x01, 0x3f), (0x02, 0), (0x05, 0), (0x03, 16), (0x04, 16), (0x06, 9), (0x08, 3),
        (0x09, 0x0c), (0x0a, 0x0c), (0x0b, 0), (0x0c, 0), (0x0d, 0x06), (0x0e, 0), (0x12, 1), (0x14, 0x10),
        (0x00, 0xa4), (0x31, 0), (0x32, 0xbf)] {
        codec_write(&mut p, reg, value);
    }
    p.gpio_setup(AMP_EN, 0);
    p.store(GPIO_OUT + 4, 1 << AMP_EN);
    p.store(GPIO_ENABLE + 4, 1 << AMP_EN);
    // trm.txt:37989-38032: XTAL/(9+49/64) to MCLK 4.096 MHz, BCK MCLK/8 (as tests/i2s.rs).
    for (off, extra) in [(0x30, 0), (0x34, 1 << 29)] {
        p.store(I2S + off, extra | (1 << 26) | 9);
        p.store(I2S + off + 8, (1 << 27) | (3 << 18) | (4 << 9) | 15);
    }
    for (gpio, signal) in [(4, 23), (5, 22), (6, 24), (7, 25)] {
        p.store(pin_reg(gpio), 0);
        p.store(mux_reg(gpio), MUX_GPIO);
        p.store(out_sel_reg(gpio), signal);
    }
    // trm.txt:38304-38339, 38421-38476: Philips 16-bit data in 16-bit slots, channels 0 and 1.
    p.store(I2S + 0x2c, (1 << 30) | (1 << 29) | (15 << 24) | (16 << 18) | (15 << 13) | (7 << 7) | 15);
    p.store(I2S + 0x54, 0x10003);
    // trm.txt:38609-38650: TX stops on empty.
    let tx = (1 << 27) | (1 << 19) | (1 << 15) | (1 << 13) | (1 << 12);
    p.store(I2S + 0x24, tx | (1 << 8));
    poll_clear(&mut p, I2S + 0x24, 1 << 8);
    p.store(I2S + 0x24, tx | 3);
    // trm.txt:11692-11710, 12156-12367: OUT channel 0 reset, EOF at the last byte out (mode 1),
    // no write-back or owner check, peripheral I2S0, the chain started.
    let out = DMA + 0x60;
    p.store(out, 8 | 1);
    p.store(out, 8);
    p.store(out + 4, 0);
    p.store(out + 0x48, 3);
    p.store(out + 0x20, (DESC & 0xfffff) | (1 << 21));
    p.store(I2S + 0x24, if sound { tx | 4 } else { tx });
    p.sr(228, 0);
    p.emit(enc::waiti(0));
    p.halt();
    p.finish()
}

/// The device over the codec board, its tone and descriptors seeded, with the hit pad in the pin
/// table.
fn device(sound: bool) -> CoMachine<S3Machine<Netlist>> {
    let board = Netlist::new(speaker_board::board()).expect("the codec and amplifier board resolves");
    let mut m = S3Machine::with_board(image(sound), &profile(0x8311), board).expect("the codec board binds");
    let samples: Vec<u8> = (0..FRAMES).flat_map(|k| [tone(k), 0]).flat_map(i16::to_le_bytes).collect();
    assert!(m.seed_data(BUF, &samples), "a second of frames fits in SRAM");
    // trm.txt:11414-11455: owner, length and size, EOF on the last, linked in order.
    let bytes = FRAMES_PER_DESC * 4;
    let words: Vec<u8> = (0..DESCS).flat_map(|i| {
        let last = i + 1 == DESCS;
        [(1 << 31) | (u32::from(last) << 30) | (bytes << 12) | bytes, BUF + i * bytes, if last { 0 } else { DESC + 12 * (i + 1) }]
    }).flat_map(u32::to_le_bytes).collect();
    assert!(m.seed_data(DESC, &words), "the descriptor chain fits in SRAM");
    let pin = CoSimPin { name: HIT_PIN.into(), gpio: "g".into(), pad: "p".into(), vss: "0".into(), vdd: "vdd".into(), vih: 0.7, vil: 0.3 };
    CoMachine::new(m, Config { stretch: STRETCH, debug: None }, &[pin]).expect("the device attaches")
}

/// The analog in the live worker's place: one input pad (row 0) at VDD, stepping to 0 V at `step`.
struct Analog {
    t: f64,
    low: bool,
    step: Option<f64>,
    hits: Vec<(u32, f64)>,
}

impl Bus for Analog {
    fn t(&self) -> f64 { self.t }
    fn v(&self, _pin: u32) -> f64 { if self.low { 0.0 } else { VDD } }
    fn supply(&self, _pin: u32) -> f64 { VDD }
    fn advance(&mut self, t: f64) -> Result<(), SimDiag> {
        self.t = self.t.max(t);
        Ok(())
    }
    fn write(&mut self, _pin: u32, _mode: PinMode) -> bool { true }
    fn hits(&self) -> &[(u32, f64)] { &self.hits }
}

/// Runs the device at its instants (and at the step) until its next instant passes `end`, handing
/// `each` the device and the time after every run.
fn play(dev: &mut CoMachine<S3Machine<Netlist>>, bus: &mut Analog, end: f64, each: &mut dyn FnMut(&mut CoMachine<S3Machine<Netlist>>, f64)) {
    loop {
        let next = dev.next();
        match bus.step {
            Some(ts) if ts < next && ts <= end => {
                bus.step = None;
                bus.low = true;
                bus.hits = dev.watches().iter().filter(|w| w.pin == 0 && matches!(w.edge, PinEdge::Falling)).map(|w| (w.id, ts)).collect();
                assert!(!bus.hits.is_empty(), "the input pad is watched");
                bus.t = ts;
            }
            _ if next <= end => bus.t = next,
            _ => return,
        }
        dev.run(bus).expect("the device runs");
        bus.hits.clear();
        each(dev, bus.t);
    }
}

/// Runs that follow each other (the same rate, the next one starting within half a sample of where
/// the last ended) as one: the sound, not where commits cut it.
fn merged(audio: &CoAudio) -> Vec<(f64, (u64, u64), Vec<i16>)> {
    let mut out: Vec<(f64, (u64, u64), Vec<i16>)> = Vec::new();
    for (run, samples) in audio.each() {
        let period = run.rate.1 as f64 / run.rate.0 as f64;
        match out.last_mut() {
            Some((t0, rate, s)) if *rate == run.rate && (*t0 + s.len() as f64 * period - run.t0).abs() < 0.5 * period => s.extend_from_slice(samples),
            _ => out.push((run.t0, run.rate, samples.to_vec())),
        }
    }
    out
}

const END: f64 = 1.25;

/// The sound a device commits over the whole run, with a hit at `step` or none.
fn committed(step: Option<f64>) -> (CoAudio, fab_elec_mcu_cosim::Stats) {
    let mut dev = device(true);
    let mut bus = Analog { t: 0.0, low: false, step, hits: Vec::new() };
    assert!(dev.run(&mut bus).is_ok(), "the device attaches at 0");
    let mut audio = CoAudio::default();
    play(&mut dev, &mut bus, END, &mut |dev, _| dev.take_audio(&mut audio));
    (audio, dev.stats())
}

#[test]
fn committed_sound_is_the_tone_once_whatever_cuts_the_stretches() {
    let (plain, plain_stats) = committed(None);
    let runs = merged(&plain);
    assert_eq!(runs.len(), 1, "one stream: {:?}", runs.iter().map(|r| (r.0, r.2.len())).collect::<Vec<_>>());
    let (t0, rate, samples) = &runs[0];
    assert_eq!(*rate, (16_000, 1), "the codec's rate is BCK over two 16-bit slots");
    assert_eq!(samples.len(), FRAMES, "every frame of the chain reaches the speaker once");
    assert!(samples.iter().enumerate().all(|(k, &s)| s == tone(k)), "the speaker hears the left slot at 0 dB, sample for sample");
    // F3: a hit mid-stretch (between two of its 0.2 ms deadlines) re-runs the device from its mark
    let at = t0 + 0.4 + 0.37 * STRETCH as f64 / 480e6;
    let (hit, hit_stats) = committed(Some(at));
    assert!(hit_stats.reruns > plain_stats.reruns, "the hit re-ran the device: {hit_stats:?}");
    assert_eq!(merged(&hit), runs, "a re-run stretch commits no frame twice and loses none");
}

#[test]
fn sound_adds_no_commit_and_no_analog_landing() {
    // the same firmware with TX left stopped: from 0.2 s to 0.8 s both make the same commits and
    // runs (the stretch deadlines), so an I2S frame is no effect on the analog
    let mut counts = Vec::new();
    for sound in [true, false] {
        let mut dev = device(sound);
        let mut bus = Analog { t: 0.0, low: false, step: None, hits: Vec::new() };
        assert!(dev.run(&mut bus).is_ok(), "the device attaches at 0");
        let mut audio = CoAudio::default();
        play(&mut dev, &mut bus, 0.2, &mut |dev, _| dev.take_audio(&mut audio));
        let before = dev.stats();
        play(&mut dev, &mut bus, 0.8, &mut |dev, _| dev.take_audio(&mut audio));
        let after = dev.stats();
        assert_eq!(audio.is_empty(), !sound, "the stopped firmware plays nothing");
        counts.push((after.commits - before.commits, after.runs - before.runs));
    }
    assert_eq!(counts[0], counts[1], "(commits, runs) with sound and without");
}

#[test]
fn the_device_tap_plays_the_tone_at_48_khz_without_images() {
    // F4: the consumer at 48 kHz pops a 10 ms buffer whenever two are queued (so it never runs
    // dry while the run goes on), and what is left once the run rests
    let ring = SampleRing::new(1 << 17);
    ring.set_device_rate(OUT_HZ);
    ring.set_open(true);
    // the producer never waits on this single-threaded consumer: its lead is the ring
    ring.set_ahead(ring.capacity() / 2);
    let mut tap = DeviceTap::new(ring.clone(), 0.02);
    let mut player = DevicePlayer::new();
    tap.begin(&[]);
    let mut dev = device(true);
    let mut bus = Analog { t: 0.0, low: false, step: None, hits: Vec::new() };
    assert!(dev.run(&mut bus).is_ok(), "the device attaches at 0");
    let mut heard = Vec::new();
    let mut buf = vec![0.0f32; BUFFER];
    let mut audio = CoAudio::default();
    play(&mut dev, &mut bus, END, &mut |dev, t| {
        dev.take_audio(&mut audio);
        tap.co_audio(&audio);
        audio.clear();
        tap.point(t, None, &[]);
        // the twin runs faster than real time (the tap's own reading of a loaded box could say
        // otherwise, and the player would gather stretches)
        ring.set_tap_speed(4.0);
        while ring.fill() >= 2 * BUFFER {
            player.fill(&ring, &mut buf, f64::from(OUT_HZ));
            heard.extend_from_slice(&buf);
        }
    });
    tap.idle();
    while ring.fill() > 0 {
        player.fill(&ring, &mut buf, f64::from(OUT_HZ));
        heard.extend_from_slice(&buf);
    }
    assert_eq!(ring.produced(), 3 * FRAMES, "one second at 16 kHz is 48 000 samples at 48 kHz");
    assert_eq!(ring.underruns(), 0, "the consumer never ran dry while the run went on");
    assert_eq!((tap.stats().dropped_closed, tap.stats().dropped_stalled), (0, 0), "nothing dropped");
    // the steady middle of the tone: the peak at 1 kHz, half scale, and nothing above 8 kHz
    // within 60 dB of it
    let first = heard.iter().position(|s| s.abs() > 1e-3).expect("the tone is heard");
    let mid: Vec<f64> = heard[first + 4800..first + 4800 + 38_400].iter().map(|&s| f64::from(s)).collect();
    let sp = spectrum(&mid, 1.0 / f64::from(OUT_HZ), Window::BlackmanHarris);
    let (k, peak) = sp.amp.iter().copied().enumerate().fold((0, 0.0), |m, (k, a)| if a > m.1 { (k, a) } else { m });
    assert!((k as f64 * sp.df - 1000.0).abs() <= sp.df, "the peak is at 1 kHz: {} Hz", k as f64 * sp.df);
    // the level from the signal itself: 800 whole periods, so its RMS is the amplitude over sqrt 2
    let amplitude = (mid.iter().map(|s| s * s).sum::<f64>() / mid.len() as f64).sqrt() * std::f64::consts::SQRT_2;
    assert!((amplitude - 0.5).abs() < 0.005, "the tone keeps its level: {amplitude}");
    let image = sp.amp.iter().enumerate().filter(|(k, _)| *k as f64 * sp.df > 8000.0).map(|(_, &a)| a).fold(0.0, f64::max);
    assert!(image < peak * 1e-3, "images above 8 kHz are 60 dB down: {:.1} dB", 20.0 * (image / peak).log10());
}
