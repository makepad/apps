//! Whole-image capture through the codec, the GDMA ring and the microsplash mic ports.

use std::sync::Arc;

use fab_elec_mcu_emu::{board::BusEvent, Machine, Tier};
use fab_elec_mcu_parts::{SoundTrack, Stimulus, TrackRef};
use fab_elec_mcu_rt::watch;

use super::Rig;

fn first_adc_frame(r: &Rig) -> u64 {
    r.bus.iter().find_map(|e| match *e {
        BusEvent::Note { t, what: "ES8311: ADC returns silence without a sound track" } => Some(t),
        _ => None,
    }).expect("the codec saw its first RX frame")
}

fn run_until(r: &mut Rig, tick: u64) {
    let now = r.machine.board_time();
    assert!(now < tick, "capture observation at {tick} fell behind board time {now}");
    r.run(tick - now, None);
}

#[test]
fn f10_and_f15_record_from_the_first_whole_frame_and_continue_in_silence() {
    for (rate, speaker) in [(16_000_u32, "let out = speaker(16000, 20, fn() {})"), (22_050, "")] {
        let frames = rate / 50;
        let source = format!(r#"
{speaker}
let pcm = samples({size})
var blocks: i32 = 0
var before: i32 = 0
var got: i32 = 0
var left: i32 = 0
let capture = mic({rate}, 40, fn() {{
    fill(pcm, 0, len(pcm), 12345)
    before = mic_ready()
    got = record(pcm, 1, {frames})
    left = mic_ready()
    blocks += 1
}})
"#, size = frames + 2);
        let mut r = Rig::new(&source, |_| {});
        r.boot();
        let hz = r.machine.tick_hz();
        r.run(hz / 1000, None);
        let first = first_adc_frame(&r);
        let block = hz / 50;
        let quarter_frame = hz / (4 * u64::from(rate));

        // Attach halfway through the last frame of the silent first block. The
        // codec samples on the first edge; IN publishes at the frame's end
        // (twin I2S event, TRM 37594-37599, 37856-37857). The next whole frame
        // starts block two, whose EOF is exactly 20 ms later, at both rates.
        run_until(&mut r, first + block - 2 * quarter_frame);
        let origin = (u128::from(r.machine.board_time()) * 1_000_000_000_000 / u128::from(hz)) as u64;
        let track = Arc::new(SoundTrack::new(origin, (rate, 1)));
        let mut expected = vec![0_i16; 4 * frames as usize];
        for (i, sample) in expected[frames as usize..(2 * frames + 3) as usize].iter_mut().enumerate() {
            *sample = ((i as i32 - frames as i32 / 2) * 127) as i16;
            track.push(*sample);
        }
        track.close();
        assert!(r.machine.host_input(&Stimulus::Sound { part: 0, track: TrackRef(Arc::clone(&track)) }),
            "{rate} Hz: the codec accepted the closed track");

        let pcm = r.image.slot.layout.buffers.iter().find(|b| b.name == "pcm").expect("the program's record buffer");
        let address = pcm.address;
        let mut bytes = vec![0; pcm.bytes as usize];
        for (k, want) in expected.chunks_exact(frames as usize).enumerate() {
            let n = k as u32 + 1;
            let eof = first + u64::from(n) * block;
            run_until(&mut r, eof - quarter_frame);
            assert_eq!(r.var("blocks"), n - 1, "{rate} Hz block {n}: no callback before the last frame ends");
            run_until(&mut r, eof + hz / 1000);
            assert_eq!(r.var("blocks"), n, "{rate} Hz block {n}: the handler finished after EOF");
            assert_eq!((r.var("before"), r.var("got"), r.var("left")), (frames, frames, 0),
                "{rate} Hz block {n}: record consumed exactly one mono block");
            assert!(r.machine.read_data(address, &mut bytes), "{rate} Hz block {n}: readable destination");
            let recorded: Vec<_> = bytes.chunks_exact(2).map(|v| i16::from_le_bytes([v[0], v[1]])).collect();
            assert_eq!((recorded[0], recorded[frames as usize + 1]), (12345, 12345),
                "{rate} Hz block {n}: record respected its offset and length");
            let phase = ["before attach", "first whole input block", "track tail and silence", "closed-track silence"][k];
            for (i, &sample) in want.iter().enumerate() {
                assert_eq!(recorded[1 + i], sample, "{rate} Hz {phase}, frame {i} at 0 dB");
            }
        }
        assert_eq!(track.short_reads(), 0, "{rate} Hz: reading beyond the closed track produced no short read");
        assert!(r.faults.is_empty(), "{rate} Hz: capture faulted: {:?}", r.faults);
    }
}

#[test]
fn a_gap_reports_the_exact_overwrite_only_during_a_take() {
    let mut r = Rig::new(r#"
let pcm = samples(320)
var calls: i32 = 0
var moved: i32 = 0
var consumed: i32 = 0
var unread: i32 = 0
var sum: i32 = 0
let capture = mic(16000, 20, fn() {
    calls += 1
    if calls == 1 || calls == 4 {
        if calls == 1 {
            consumed = record(pcm, 0, 313)
        } else {
            consumed = record(pcm, 0, 320)
            moved = mic_rewind(7)
        }
        unread = mic_ready()
        for i in 0..2000000 { sum = (sum + i) & 65535 }
    }
    if calls == 3 || calls == 6 {
        consumed = record(pcm, 0, 320)
        unread = mic_ready()
    }
})
"#, |_| {});
    r.machine.set_tier(Tier::Native);
    r.boot();
    let hz = r.machine.tick_hz();
    r.run(hz / 1000, None);
    let first = first_adc_frame(&r);
    let block = hz / 50;
    let returned = r.image.slot.symbols.handler_return;
    let site = r.image.slot.layout.mic.as_ref().unwrap().entry as u16;
    let mut idle_blocks = 0;
    for call in [1, 4] {
        let start = (r.machine.board_time() - first) / block + 1;
        r.run(hz / 2, Some(returned));
        assert_eq!(r.var("calls"), call, "the long mic handler completed");
        let taking = call == 4;
        assert_eq!(r.var("consumed"), if taking { 320 } else { 313 }, "read before the gap");
        assert_eq!(r.var("moved"), if taking { 7 } else { 0 }, "rewind only for the take");
        assert_eq!(r.var("unread"), 7, "both gaps start with the same unread tail");

        let end = (r.machine.board_time() - first) / block;
        let finished = end - start;
        assert!(finished >= 2, "the handler spanned at least two RX completions");

        // All those completions leave one pending callback. It skips its read,
        // so one more block overwrites the cursor before the next handler drains it.
        r.run(block, Some(returned));
        assert_eq!(r.var("calls"), call + 1, "a coalesced block ran after the gap");
        assert_eq!((r.machine.board_time() - first) / block, end,
            "the coalesced handler and loss report preceded the next block");
        r.run(2 * block, Some(returned));
        assert_eq!((r.machine.board_time() - first) / block, end + 1, "the next callback waited for a fresh EOF");
        assert_eq!((r.var("calls"), r.var("consumed"), r.var("unread")), (call + 2, 320, 0),
            "reading the newest block caught up to capture");
        r.run(hz / 500, None); // Let the watch frame reach the board log.
        if taking {
            assert_eq!(finished, idle_blocks, "both pauses covered the same number of blocks");
            // First EOF drops the seven unread samples; each further EOF before
            // dispatch drops a whole block. The later unread block is the same episode.
            let lost = 7 + (finished as i32 - 1) * 320;
            assert_eq!(r.faults, [(watch::FAULT_MIC_LOST, site, lost)], "one loss with its sample count");
        } else {
            idle_blocks = finished;
            assert!(r.faults.is_empty(), "history advanced outside a take: {:?}", r.faults);
        }
    }
}
