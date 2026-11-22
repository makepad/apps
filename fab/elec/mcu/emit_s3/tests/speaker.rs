//! Microsplash audio ports through a booted S3 and the shared ES8311 board.
use std::sync::Arc;

use fab_elec_mcu::{compile, Compiled};
use fab_elec_mcu_emit_s3::{build_image, wiring, Board, BoardTime, Image, SLOT_BASE, VOICEBOX};
use fab_elec_mcu_emu::{board::{Audio, BusEvent}, Machine, PcSet, Pull, Ran, Until};
use fab_elec_mcu_parts::{NetDesc, Netlist, PinRef};
use fab_elec_mcu_rt::{module, watch};
use fab_elec_mcu_s3::{CpuClock, PinsProfile, S3Machine, S3Profile, TimeProfile};

#[path = "support/image_fixture.rs"]
mod image_fixture;
#[path = "speaker/mic.rs"]
mod mic;
#[path = "../../s3/tests/support/speaker_board.rs"]
mod speaker_board;

fn profile() -> S3Profile {
    S3Profile {
        mac: [2, 0, 0, 0, 0, 1], pvt: fab_elec_mcu_s3::NO_PVT, flash_bytes: VOICEBOX.flash_bytes as usize,
        handoff_cpu: CpuClock::Xtal { div: 2 }, handoff_mapped_pages: 64,
        entry: 0x4200_0408, seed: 0x8311,
        time: TimeProfile {
            systimer_first_long: true, rc_slow_hz: (136_000, 1), wdt_delay_sel: 0,
            swd_timeout_ticks: 432_000_000, swd_request_lead_ticks: 48_000_000,
            wdt_sync_cycles: 2, mwdt_rephase_on_feed: false, mwdt_rephase_on_config: true,
            wdt_restart_on_enable: true,
        },
        pins: PinsProfile {
            power_on_gpio0_high: true, usb_service_ticks: 480_000,
            usb_frame_phase_ticks: 0, input_sync_delay_ticks: 0,
            handoff_digital_pads_unheld: true, handoff_out: [0; 2], handoff_enable: [0; 2],
            handoff_pin: [0; 45], handoff_io_mux: [1 << 12; 45], handoff_out_sel: [256; 45],
            handoff_usb_tx: Vec::new(), handoff_usb_tx_flushed: false,
            handoff_usb_rx: Vec::new(), handoff_usb_int_raw: 8, handoff_usb_int_ena: 0,
        },
    }
}

struct Rig {
    program: Compiled,
    image: Image,
    machine: S3Machine<Netlist>,
    marker: u16,
    bus: Vec<BusEvent>,
    audio: Audio,
    reader: watch::Reader,
    faults: Vec<(u8, u16, i32)>,
}

impl Rig {
    fn new(source: &str, prepare: impl FnOnce(&mut Image)) -> Self {
        let mut desc = speaker_board::board();
        let marker = desc.nets.len() as u16;
        desc.nets.push(NetDesc { name: "DRAINED".into(), rise_ps: None, pull: Pull::Down, pins: vec![PinRef::Chip("GPIO2".into())] });
        let wiring = wiring(&desc).unwrap(); let language = wiring.profile();
        let program = compile(source, &language, &[("D2", 2)], &[]).unwrap_or_else(|e| panic!("{e:?}"));
        let module = module::load(&program.module, &language).unwrap();
        let p = profile();
        let board = Board { layout: VOICEBOX, i2c: wiring.i2c, motion: None, panel: None, touch: None, radio: None,
            time: BoardTime { rc_slow_hz: p.time.rc_slow_hz, wdt_delay_sel: p.time.wdt_delay_sel } };
        let mut image = build_image(&module, &board, &wiring.options()).unwrap();
        prepare(&mut image);
        let machine = S3Machine::with_board(Arc::new(image.flash.clone()), &p, Netlist::new(desc).unwrap()).unwrap();
        Self { program, image, machine, marker, bus: Vec::new(), audio: Audio::default(), reader: watch::Reader::default(), faults: Vec::new() }
    }

    fn run(&mut self, ticks: u64, stop: Option<u32>) {
        let end = self.machine.now() + ticks;
        let stops: Vec<_> = stop.into_iter().collect();
        let mut marks = PcSet::new(SLOT_BASE, self.image.slot.bytes.len() as u32);
        for &pc in &stops { marks.insert(pc); }
        let mut barrier = None;
        loop {
            let ran = self.machine.run(&Until { tick: end, marks: &marks, stops: &stops,
                stops_after: self.machine.now(), on_drive: false });
            assert!(self.machine.last_reset().is_none(), "the audio image reset");
            self.machine.take_audio(&mut self.audio); self.machine.take_bus(&mut self.bus);
            let mut bytes = Vec::new(); self.machine.take_out(&mut bytes);
            for b in bytes {
                if let Some(frame) = self.reader.push(b.byte) {
                    if let watch::Event::Fault { code, site, arg } = frame.unwrap().event { self.faults.push((code, site, arg)); }
                }
            }
            match ran {
                Ran::Barrier => {
                    let now = self.machine.now();
                    assert_ne!(barrier.replace(now), Some(now), "audio board repeated a barrier without advancing");
                    self.machine.mark();
                }
                Ran::Pc if stop == Some(self.machine.pc()) => return,
                Ran::Tick if stop.is_none() => return,
                other => panic!("audio image missed its target: {other:?}, {:?}", self.machine.status()),
            }
        }
    }

    fn boot(&mut self) {
        self.run(self.machine.tick_hz() / 2, Some(self.image.slot.symbols.dispatch));
    }

    fn var(&self, name: &str) -> u32 {
        let offset = self.program.lowered.state_vars.iter().find(|v| v.name == name).unwrap().offset;
        self.word(self.image.slot.layout.state + offset * 4)
    }

    fn word(&self, at: u32) -> u32 {
        let mut bytes = [0; 4];
        assert!(self.machine.read_data(at, &mut bytes));
        u32::from_le_bytes(bytes)
    }

    fn samples(&self, rate: u64) -> Vec<(u64, i16)> {
        let hz = self.machine.tick_hz();
        let start = self.audio.runs.first().expect("the board played audio").t0;
        let mut frames = Vec::new();
        for (run, samples) in self.audio.each() {
            assert_eq!(run.rate, (rate, 1));
            // The twin rounds absolute I2S edges up to board ticks. Differences
            // from the first rounded edge can fall on either adjacent tick.
            let elapsed = frames.len() as u64 * hz;
            assert!((start + elapsed / rate..=start + elapsed.div_ceil(rate)).contains(&run.t0));
            frames.extend(samples.iter().enumerate().map(|(i, &v)| (run.t0 + i as u64 * hz / rate, v)));
        }
        frames
    }
}

fn playback(rate: u64, sound: usize, refill: &str, other_handlers: &str) -> Rig {
    let source = format!(r#"
let pcm = samples({sound})
let drained = output(D2)
var sent: i32 = 0
var writes: i32 = 0
var declined: i32 = 0
let out = speaker({rate}, 20, fn() {{
    let before = sent
    {refill}
    if sent > before {{ writes += 1 }}
    if sent > 0 {{
        if speaker_queued() == 0 {{
            write(drained, high)
        }} else {{ if sent == before {{ declined += 1 }} }}
    }}
}})
fn setup() {{
    for i in 0..{sound} {{ pcm[i] = (i % 31 - 15) * 997 + 13 }}
    speaker_on(1)
}}
{other_handlers}
"#);
    let mut r = Rig::new(&source, |_| {}); r.boot();
    r.run(r.machine.tick_hz() / 8, None); r
}

fn sample(i: usize) -> i16 { ((i as i32 % 31 - 15) * 997 + 13) as i16 }

#[test]
fn f11_refills_play_the_sound_and_queued_reaches_zero_within_one_descriptor() {
    for rate in [16_000, 44_100] {
        let sound = (rate / 25) as usize + 37; // two full ring laps and a partial tail
        let r = playback(rate, sound, r#"if sent < len(pcm) {
            let left = len(pcm) - sent
            let room = speaker_free()
            let n = if left < room { left } else { room }
            sent += play(pcm, sent, n)
        }"#, "");
        assert_eq!(r.var("sent"), sound as u32); assert!(r.var("writes") > 1);
        let ring = r.image.slot.layout.speaker.as_ref().unwrap();
        let stride = (rate as u32).div_ceil(200) * 4;
        for i in 0..ring.count {
            let at = ring.descriptors + 12 * i;
            let header = r.word(at);
            let frames = (i + 1) * rate as u32 / 200 - i * rate as u32 / 200;
            assert_eq!((header & 4095, (header >> 12) & 4095), (stride, 4 * frames));
            assert_eq!(r.word(at + 4), ring.buffer + i * stride);
        }
        let audio = r.samples(rate); let first = audio.iter().position(|&(_, v)| v != 0).unwrap();
        for (i, &(_, value)) in audio[first..first + sound].iter().enumerate() {
            assert_eq!(value, sample(i), "{rate} Hz frame {i} at unity gain");
        }
        let last = audio[first + sound - 1].0;
        let drained = r.bus.iter().find_map(|e| match *e {
            BusEvent::Level { t, net, high: true } if net == r.marker => Some(t), _ => None,
        }).expect("the refill handler observed an empty queue");
        let longest = (rate.div_ceil(200) * r.machine.tick_hz()).div_ceil(rate);
        assert!((last..=last + longest).contains(&drained));
        assert!(audio[first + sound..].iter().all(|&(_, v)| v == 0));
        assert!(r.faults.is_empty(), "the refill ran and the sound ended: {:?}", r.faults);
    }
}

fn sound_then_silence(n: usize, other_handlers: &str) -> Rig {
    let r = playback(16_000, 677, &format!("if sent == 0 {{ sent = play(pcm, 0, {n}) }}"), other_handlers);
    let audio = r.samples(16_000); let first = audio.iter().position(|&(_, v)| v != 0).unwrap();
    assert_eq!(r.var("sent"), n as u32);
    assert!(audio[first..first + n].iter().enumerate().all(|(i, &(_, v))| v == sample(i)));
    let tail = &audio[first + n..];
    assert!(tail.len() >= 2 * 320 && tail.iter().all(|&(_, v)| v == 0), "no replay on later laps");
    r
}

#[test]
fn f12_a_refill_held_past_the_drain_reports_one_loss() {
    // The timer services DMA while it holds the dispatcher. The EOF of the
    // first 80 samples raises a refill that cannot run before the tail drains.
    let busy = r#"
var waited: i32 = 0
let busy = after(ms(1), fn() {
    if speaker_queued() > 0 {
        for i in 0..65536 {
            if speaker_queued() == 0 { waited = 1; break }
        }
    }
})
"#;
    for n in [143, 160] {
        let r = sound_then_silence(n, busy);
        assert_eq!(r.var("waited"), 1);
        assert_eq!(r.var("declined"), 0);
        let site = r.image.slot.layout.speaker.as_ref().unwrap().entry as u16;
        assert_eq!(r.faults, [(watch::FAULT_SPEAKER_LOST, site, 1)], "{n} samples");
    }
}

#[test]
fn an_answered_refill_can_let_the_sound_end() {
    for n in [143, 160] {
        let r = sound_then_silence(n, "");
        assert!(r.var("declined") > 0, "the refill ran before the last sample drained");
        assert!(r.faults.is_empty(), "{n} samples: {:?}", r.faults);
    }
}

#[test]
fn speaker_init_leaves_the_first_five_ms_timer_period_for_the_program() {
    let mut r = Rig::new(r#"
var ticks: i32 = 0
let out = speaker(16000, 20, fn() {})
let tick = every(ms(5), fn() { ticks += 1 })
"#, |_| {});
    r.boot();
    let ms = r.machine.tick_hz() / 1000;
    r.run(3 * ms, None); assert_eq!(r.var("ticks"), 0);
    r.run(9 * ms, None); assert_eq!(r.var("ticks"), 2);
    assert!(r.faults.is_empty(), "device init caused a boot fault: {:?}", r.faults);
}

// Clear TX_START after setup has queued sound. The twin finishes the
// active frame and stops requesting DMA (TRM 37558-37565).
fn stop_tx(image: &mut Image) {
    image_fixture::before_dispatch(image, |a, _| {
        a.mov_reg_i32(11, 0x6000_f024);
        a.l32i(2, 11, 0); a.movi(3, -5); a.and(2, 2, 3); a.s32i(2, 11, 0);
    });
}

#[test]
fn stopped_tx_reports_once_and_play_keeps_returning() {
    let mut r = Rig::new(r#"
let pcm = samples(320)
var accepted: i32 = 0
var queued: i32 = 0
var room: i32 = 0
var polls: i32 = 0
let out = speaker(16000, 20, fn() {})
fn setup() { accepted = play(pcm, 0, 320) }
let tick = every(5000, fn() {
    accepted += play(pcm, 0, 320)
    queued = speaker_queued()
    room = speaker_free()
    polls += 1
})
"#, stop_tx);
    r.boot(); let hz = r.machine.tick_hz(); r.run(hz / 10, None);
    let count = r.var("polls"); let accepted = r.var("accepted");
    let frames = r.audio.samples.len(); assert!(frames > 0, "the ring ran before TX_START was cleared");
    assert!(count > 0 && accepted > 0); assert_eq!(r.var("queued"), accepted);
    r.run(hz / 10, None);
    assert_eq!(r.audio.samples.len(), frames, "the stopped transmitter requested no more frames");
    assert!(r.var("polls") >= count + 19); assert_eq!(r.var("accepted"), accepted);
    assert_eq!((r.var("queued"), r.var("room")), (accepted, 0));
    let site = r.image.slot.layout.speaker.as_ref().unwrap().entry as u16;
    let speaker_faults: Vec<_> = r.faults.iter().copied().filter(|&(code, _, _)|
        matches!(code, watch::FAULT_SPEAKER_STALL | watch::FAULT_SPEAKER_LOST)).collect();
    assert_eq!(speaker_faults, [(watch::FAULT_SPEAKER_STALL, site, 3)]);
}
