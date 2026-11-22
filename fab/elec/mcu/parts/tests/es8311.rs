use std::sync::Arc;

use fab_elec_mcu_emu::board::{Audio, AudioRun, Board, BusEvent, I2cAns, I2cOp, I2sFrame, I2sStandard, NetKind, Res};
use fab_elec_mcu_emu::{Drive, Pull};
use fab_elec_mcu_parts::{model, BoardDesc, NetDesc, Netlist, PartDesc, PinRef, SoundTrack, Stimulus, TrackRef};

// CE low (es8311-datasheet.txt:303-307; es8311-user-guide.txt:969-972).
const ADDR: u8 = 0x18;
const PADS: [&str; 8] = ["control_clk", "control_data", "bit_clk", "frame_clk", "speaker", "mic", "master_clk", "amp_enable"];
const AMP_EN: u16 = 7;

/// The codec (part 0) with its output on a speaker amplifier (part 1, on at EN high, 0 dB,
/// starting 1 ms after EN rises: sixteen frames).
fn board() -> Netlist {
    // Chip pad order deliberately differs from net order. es8311-datasheet.txt:231-243.
    let mut nets: Vec<NetDesc> = [(4, "DSDIN"), (5, "ASDOUT"), (6, "MCLK"), (3, "LRCK"),
                (0, "SCL"), (2, "SCLK"), (1, "SDA")].into_iter().map(|(pad, pin)| NetDesc {
        name: pin.into(), rise_ps: None, pull: if pad < 2 { Pull::Up } else { Pull::None },
        pins: vec![PinRef::Chip(PADS[pad].into()), PinRef::Part { part: 0, pin: pin.into() }],
    }).collect();
    for (out, input) in [("OUTP", "INP"), ("OUTN", "INN")] {
        nets.push(NetDesc { name: out.into(), rise_ps: None, pull: Pull::None,
            pins: vec![PinRef::Part { part: 0, pin: out.into() }, PinRef::Part { part: 1, pin: input.into() }] });
    }
    nets.push(NetDesc { name: "PA_CTRL".into(), rise_ps: None, pull: Pull::None,
        pins: vec![PinRef::Chip(PADS[usize::from(AMP_EN)].into()), PinRef::Part { part: 1, pin: "EN".into() }] });
    let kind = model("ES8311", &[("addr".into(), f64::from(ADDR))]).expect("CE-low codec must resolve");
    let amp = model("AMP", &[("enable".into(), 1.0), ("gain_db".into(), 0.0), ("start_ms".into(), 1.0)])
        .expect("a unity amplifier must resolve");
    let desc = BoardDesc { seed: 0, parts: vec![PartDesc { refdes: "U1".into(), kind }, PartDesc { refdes: "U2".into(), kind: amp }], nets };
    let mut board = Netlist::new(desc).expect("codec clock, data and output nets must resolve");
    board.bind(&PADS, 16_000_000).expect("board must bind at 1000 ticks per 16 kHz sample");
    board
}

/// The sound played since the last take, as (run, samples) pairs.
fn played(board: &mut Netlist) -> Vec<(AudioRun, Vec<i16>)> {
    let mut audio = Audio::default();
    board.take_audio(&mut audio);
    audio.each().map(|(run, samples)| (*run, samples.to_vec())).collect()
}

// 5 ms in reset exemplifies the guide's several ms (es8311-user-guide.txt:1371-1373).
const START: u64 = 80_000;

/// Powers the codec up for 16 kHz duplex Philips 16-bit frames in a 32-bit slot with a 256x MCLK,
/// by manual power-up, at `START`.
fn power_up(board: &mut Netlist) {
    write(board, 0, 0x00, 0x1f).expect("CSM off and digital blocks in reset");
    for (reg, value) in [
        (0x01, 0x3f), // es8311-datasheet.txt:679-714 pin MCLK and all path/serial clocks on.
        (0x02, 0), (0x05, 0), // es8311-datasheet.txt:715-723,764-771 unity clock dividers and multiplier.
        (0x03, 16), (0x04, 16), // es8311-user-guide.txt:1238-1242 worked 256x single-speed OSR pair.
        (0x06, 9), (0x08, 3), // es8311-user-guide.txt:1255-1258,1307-1310 master dividers inactive.
        (0x09, 0x0c), (0x0a, 0x0c), // es8311-datasheet.txt:833-841,853-861 Philips, 16-bit words.
        (0x0b, 0), (0x0c, 0), // es8311-datasheet.txt:862-885 shortest A/B/C stages.
        (0x0d, 0x06), (0x0e, 0), // es8311-datasheet.txt:886-915 references, normal VMID, PGA/modulator on.
        (0x12, 1), (0x14, 0x10), // es8311-datasheet.txt:959-964,973-981 DAC with reference, MIC1 selected.
        (0x00, 0xa4), // es8311-datasheet.txt:654-678 CSM on, explicit manual power-up, only master block in reset.
    ] {
        write(board, START, reg, value).unwrap_or_else(|why| panic!("power/clock setup {reg:#04x}: {why}"));
    }
}

fn frame(t: u64, tx: [i32; 2]) -> I2sFrame {
    I2sFrame {
        t, mclk: Some(6), bclk: 2, ws: 3, dout: Some(4), din: Some(5),
        mclk_hz: Some((4_096_000, 1)), bclk_hz: (1_024_000, 1),
        standard: I2sStandard::Philips, slot_bits: 32, data_bits: 16, tx,
    }
}

// Single byte write and repeated-START read. es8311-datasheet.txt:310-326.
fn select(board: &mut Netlist, t: u64, reg: u8) -> Res {
    board.i2c(t, 0, 1, I2cOp::Start)?;
    assert_eq!(board.i2c(t, 0, 1, I2cOp::Write(ADDR << 1))?, I2cAns::Ack(true), "strapped address must ACK");
    assert_eq!(board.i2c(t, 0, 1, I2cOp::Write(reg))?, I2cAns::Ack(true), "register {reg:#04x} must ACK");
    Ok(())
}

fn write(board: &mut Netlist, t: u64, reg: u8, value: u8) -> Res {
    select(board, t, reg)?;
    let reply = board.i2c(t, 0, 1, I2cOp::Write(value));
    board.i2c(t, 0, 1, I2cOp::Stop)?;
    assert_eq!(reply?, I2cAns::Ack(true), "accepted data for {reg:#04x} must ACK");
    Ok(())
}

fn read(board: &mut Netlist, t: u64, reg: u8) -> Res<u8> {
    select(board, t, reg)?;
    board.i2c(t, 0, 1, I2cOp::Start)?;
    assert_eq!(board.i2c(t, 0, 1, I2cOp::Write(ADDR << 1 | 1))?, I2cAns::Ack(true), "codec read address must ACK");
    let reply = board.i2c(t, 0, 1, I2cOp::Read { ack: false })?;
    board.i2c(t, 0, 1, I2cOp::Stop)?;
    let I2cAns::Byte(byte) = reply else { panic!("register {reg:#04x} must return a byte") };
    Ok(byte)
}

#[test]
fn codec_control_duplex_clocks_and_capture() {
    let mut board = board();
    // es8311-datasheet.txt:654,679,772,808,886,1405,1417-1425.
    for (reg, reset) in [(0x00, 0x1f), (0x01, 0), (0x06, 3), (0x08, 0xff), (0x0d, 0xfc),
                         (0xfc, 0), (0xfd, 0x83), (0xfe, 0x11), (0xff, 0)] {
        assert_eq!(read(&mut board, 0, reg), Ok(reset), "register {reg:#04x} reset/identity byte");
    }
    // es8311-datasheet.txt:842-861. Bit 7 is unassigned on the ADC serial port.
    let why = write(&mut board, 0, 0x0a, 0x80).expect_err("reserved bit 7 must stop the write");
    assert!(why.contains("0x0a") && why.contains("reserved"), "refusal must identify register and reason: {why}");
    assert_eq!(read(&mut board, 0, 0x0a), Ok(0), "rejected write must preserve the reset byte");
    // es8311-datasheet.txt:654-678 and es8311-user-guide.txt:1360-1373.
    write(&mut board, 0, 0x00, 0x1f).expect("CSM off and digital blocks in reset");
    let mut frame = frame(0, [-32768, 32767]);
    assert!(board.i2s(&frame).expect_err("CSM held in reset must refuse audio").contains("CSM"), "power refusal must identify CSM");
    power_up(&mut board);
    frame.t = START + 4_000;
    assert_eq!(board.i2s(&frame), Ok([0; 2]), "16 kHz duplex, 256x MCLK and 64x BCLK must yield silent ADC");
    let first = AudioRun { part: 0, t0: frame.t, rate: (16_000, 1), len: 1 };
    frame.t += 1_000;
    frame.tx = [123, -456];
    let why = board.i2s(&I2sFrame { standard: I2sStandard::Pcm, ..frame }).expect_err("PCM must mismatch Philips");
    assert!(why.contains("standard mismatch"), "halt must name the serial standard mismatch: {why}");
    let detail = board.refusal_detail().expect("standard mismatch must supply halt detail");
    assert!(detail.contains("Philips") && detail.contains("Pcm"), "detail must identify codec and frame standards: {detail}");
    board.mark();
    assert_eq!(board.refusal_detail(), None, "mark must clear the previous refusal detail");
    let why = board.i2s(&I2sFrame { data_bits: 24, ..frame }).expect_err("24 bits must mismatch 16");
    assert!(why.contains("word length mismatch"), "halt must name the word length mismatch: {why}");
    let detail = board.refusal_detail().expect("word length mismatch must supply halt detail");
    assert!(detail.contains("16") && detail.contains("24"), "detail must identify both word widths: {detail}");
    let why = board.i2s(&I2sFrame { mclk_hz: Some((2_048_000, 1)), ..frame }).expect_err("128x clock must fail");
    assert!(why.contains("clock/LRCK ratio mismatch"), "halt must name the converter clock ratio mismatch: {why}");
    let detail = board.refusal_detail().expect("converter clock ratio mismatch must supply halt detail");
    assert!(detail.contains("240") && detail.contains("actual 128"), "detail must give the bypassed-EQ minimum and actual ratio: {detail}");
    let why = board.i2s(&I2sFrame { mclk_hz: Some((8_192_001, 2)), ..frame }).expect_err("half-Hz error must fail exact ratio");
    assert!(why.contains("clock/LRCK ratio mismatch"), "fractional ratio must halt with a clock mismatch: {why}");
    let detail = board.refusal_detail().expect("fractional clock ratio mismatch must supply halt detail");
    assert!(detail.contains("8192001/32000"), "fractional ratio must survive without rounding: {detail}");
    let why = board.i2s(&I2sFrame { dout: Some(5), ..frame }).expect_err("miswired DOUT must fail");
    assert!(why.contains("DSDIN"), "wiring error must identify the codec input: {why}");
    assert_eq!(board.i2s(&frame), Ok([0; 2]), "valid frame after refusals must succeed");
    board.rewind();
    assert_eq!(board.refusal_detail(), None, "rewind must clear refusal detail from later frames");
    // the amplifier is off (EN low): the speaker plays silence, frame for frame
    assert_eq!(played(&mut board), [(first, vec![0])], "rewind keeps exactly the frames played before the mark");
    assert!(played(&mut board).is_empty(), "a take leaves nothing to take again");
    assert_eq!(board.i2s(&frame), Ok([0; 2]), "the frame plays again after the rewind");
    assert_eq!(played(&mut board), [(AudioRun { t0: frame.t, ..first }, vec![0])], "a run starts after a take");

    // es8311-datasheet.txt:679-692,715-723,959-964. BCLK * 4 is 256x fs; only ADC is used.
    let switch = frame.t;
    write(&mut board, switch, 0x01, 0x9a).expect("BCLK source must not require pin MCLK or DAC clocks");
    write(&mut board, switch, 0x02, 0x10).expect("clock multiplier code 2 is x4");
    write(&mut board, switch, 0x12, 2).expect("power down the unused DAC");
    frame.t += 4_000;
    frame.mclk = None;
    frame.mclk_hz = None;
    assert!(board.i2s(&frame).expect_err("duplex must check the DAC independently").contains("DAC"), "powered-down playback must name DAC");
    frame.dout = None;
    frame.tx = [0; 2];
    assert_eq!(board.i2s(&frame), Ok([0; 2]), "ADC-only stream must work on BCLK with the DAC off");
    assert!(played(&mut board).is_empty(), "an ADC-only frame plays nothing");
    let mut events = Vec::new();
    board.take_bus(&mut events);
    let silence = events.iter().filter(|e| matches!(e, BusEvent::Note { what, .. } if what.contains("ADC returns silence"))).count();
    assert_eq!(silence, 1, "microphone modelling decision must be reported once across accepted frames");

    // es8311-datasheet.txt:654-678,862-885. Automatic minimum stages at 48 kHz are 21 us each.
    let restart = frame.t;
    write(&mut board, restart, 0x00, 0x1f).expect("codec can return to reset before automatic startup");
    write(&mut board, restart + START, 0x00, 0x84).expect("automatic startup after reset interval");
    frame.bclk_hz = (3_072_000, 1);
    frame.t = restart + START + 63 * 16 - 1;
    assert!(board.i2s(&frame).expect_err("all three power-up delays must elapse").contains("power-up"), "early frame must identify the sequencer");
    frame.t += 1;
    assert_eq!(board.i2s(&frame), Ok([0; 2]), "automatic power-up must complete at the documented three-stage boundary");
    frame.t += 1_000;
    frame.bclk_hz = (1_024_000, 1);
    assert_eq!(board.i2s(&frame), Ok([0; 2]), "changing LRCK after automatic power-up must preserve readiness");
}

#[test]
fn microphone_gain_volume_and_slots_follow_the_frame_clock() {
    let mut board = board();
    power_up(&mut board);
    let origin = START + 1_000;
    // 62,500 ps per board tick. Read a quarter period into each recorded sample.
    let track = Arc::new(SoundTrack::new(origin * 62_500, (32_000, 2)));
    let ramp = [-30_000i16, -20_000, -10_000, 0, 10_000, 20_000, 30_000];
    for _ in 0..4 {
        for x in ramp { track.push(x); }
    }
    track.close();
    board.input(START, Stimulus::Sound { part: 0, track: TrackRef(track.clone()) }).expect("attach the recorded ramp");
    let rx = |t| I2sFrame { dout: None, ..frame(t, [0; 2]) };
    assert_eq!(board.i2s(&rx(origin - 1)), Ok([0; 2]), "silence before the recording starts");
    let mut t = origin + 250;
    // es8311-datasheet.txt:980-991,1038-1046,1375-1379. Independently rounded PCM
    // at unity, +3 dB PGA (including both clip limits), +0.5 dB and the listed -90.5 dB.
    for (pga, volume, route, words) in [
        (0x10, 0xbf, 0x00, ramp.map(i32::from)),
        (0x11, 0xbf, 0x10, [-32768, -28251, -14125, 0, 14125, 28251, 32767]),
        (0x10, 0xc0, 0x20, [-31778, -21185, -10593, 0, 10593, 21185, 31778]),
        (0x10, 0x01, 0x00, [-1, -1, 0, 0, 0, 1, 1]),
    ] {
        write(&mut board, t, 0x14, pga).expect("PGA gain");
        write(&mut board, t, 0x17, volume).expect("ADC volume");
        write(&mut board, t, 0x44, route).expect("ASDOUT slot selection");
        for word in words {
            let slots = match route { 0x10 => [word, 0], 0x20 => [0, word], _ => [word; 2] };
            assert_eq!(board.i2s(&rx(t)), Ok(slots), "ASDOUT at tick {t}");
            t += 1_000;
        }
    }
    assert_eq!(board.i2s(&rx(t)), Ok([0; 2]), "silence after the nonzero last sample");
    assert_eq!(track.short_reads(), 0, "a closed recording cannot run short");
}

#[test]
fn restarting_a_recording_preserves_the_first_play_on_rewind() {
    let mut board = board();
    power_up(&mut board);
    let start = START + 1_000;
    write(&mut board, start, 0x17, 0xbf).expect("unity ADC volume");
    let wav = SoundTrack::new(0, (16_000, 1));
    for word in 1..=4100 { wav.push(word); }
    wav.close();
    let play = |t: u64| Stimulus::Sound { part: 0, track: TrackRef(Arc::new(wav.replay_at(t * 62_500))) };
    let rx = |t| I2sFrame { dout: None, ..frame(t, [0; 2]) };

    board.input(start, play(start)).unwrap();
    assert_eq!(board.i2s(&rx(start)), Ok([1; 2]));
    board.mark();
    let again = start + 4096 * 1_000;
    board.input(again, play(again)).unwrap();
    assert_eq!(board.i2s(&rx(again)), Ok([1; 2]));
    board.rewind();
    assert_eq!(board.i2s(&rx(again)), Ok([4097; 2]));
    assert_eq!(board.i2s(&rx(again + 4_000)), Ok([0; 2]));
}

#[test]
fn microphone_rate_mismatch_and_open_short_read() {
    let mut board = board();
    power_up(&mut board);
    let t = START + 1_000;
    write(&mut board, t, 0x17, 0xbf).expect("unity ADC volume (es8311-datasheet.txt:1043)");
    let track = Arc::new(SoundTrack::new(t * 62_500, (16_000, 1)));
    track.push(-1234);
    board.input(t, Stimulus::Sound { part: 0, track: TrackRef(track.clone()) }).expect("attach the open track");
    let rx = |t| I2sFrame { dout: None, ..frame(t, [0; 2]) };
    assert_eq!(board.i2s(&rx(t)), Ok([-1234; 2]));
    let next = t + 1_000;
    assert_eq!(board.i2s(&rx(next)), Ok([0; 2]));
    assert_eq!(track.short_reads(), 1, "an open recording counts a missing sample");
    track.push(1234);
    track.push(2345);
    track.close();
    board.mark();
    // Half a hertz off, with nonzero data covering both reads.
    let wrong = Arc::new(SoundTrack::new(next * 62_500, (32_001, 2)));
    wrong.push(9000);
    wrong.push(9001);
    wrong.close();
    board.input(next, Stimulus::Sound { part: 0, track: TrackRef(wrong) }).expect("replace the recording");
    for at in [next, next + 1_000] { assert_eq!(board.i2s(&rx(at)), Ok([0; 2])); }
    let mut events = Vec::new();
    board.take_bus(&mut events);
    assert_eq!(events.iter().filter(|e| matches!(e, BusEvent::Note { what, .. } if what.contains("rate differs from LRCK"))).count(), 1);
    board.rewind();
    assert_eq!(board.i2s(&rx(next)), Ok([1234; 2]), "rewind restores the original recording at this tick");
    assert_eq!(board.i2s(&rx(next + 1_000)), Ok([2345; 2]));
    assert_eq!(track.short_reads(), 1);
}

#[test]
fn speaker_hears_the_selected_slot_through_mutes_volume_and_amplifier() {
    let mut board = board();
    assert_eq!(board.nets().iter().filter(|n| n.kind == NetKind::Audio).count(), 2, "the codec output nets carry sound");
    power_up(&mut board);
    // A ramp in the left slot and its mirror in the right, eight frames from `t`.
    let ramp = |k: i32| [k * 4_000 - 16_000, 16_000 - k * 3_000];
    let mut t = START + 4_000;
    let play = |board: &mut Netlist, t: &mut u64| -> Vec<(AudioRun, Vec<i16>)> {
        for k in 0..8 {
            assert_eq!(board.i2s(&frame(*t, ramp(k))), Ok([0; 2]), "frame {k} is accepted");
            *t += 1_000;
        }
        played(board)
    };
    let left: Vec<i16> = (0..8).map(|k| ramp(k)[0] as i16).collect();
    let right: Vec<i16> = (0..8).map(|k| ramp(k)[1] as i16).collect();
    let silence = vec![0i16; 8];
    let one = |t0: u64, samples: Vec<i16>| vec![(AudioRun { part: 0, t0, rate: (16_000, 1), len: 8 }, samples)];

    // F2: the amplifier off (EN low at power-on) silences the speaker.
    let t0 = t;
    assert_eq!(play(&mut board, &mut t), one(t0, silence.clone()), "the amplifier off plays silence");
    board.pad_drive(t, AMP_EN, Drive::High).expect("EN rises");
    let t0 = t;
    assert_eq!(play(&mut board, &mut t), one(t0, silence.clone()), "the amplifier plays nothing while it starts");
    // F2: the volume's floor, 0x00, is -95.5 dB: at most one LSB at full scale (from 1 ms on).
    t += 8_000;
    let floor = play(&mut board, &mut t);
    assert!(floor[0].1.iter().all(|s| s.abs() <= 1), "volume 0x00 is within an LSB of silence: {floor:?}");
    // F1: 0 dB (0xBF), unmuted, amplifier on: the left slot sample for sample, from the first frame's tick.
    write(&mut board, t, 0x32, 0xbf).expect("0 dB");
    let t0 = t;
    assert_eq!(play(&mut board, &mut t), one(t0, left.clone()), "0 dB plays the left slot exactly");
    // es8311-datasheet.txt:819-823: SDP_IN_SEL 1 selects the right slot.
    write(&mut board, t, 0x09, 0x8c).expect("right slot");
    let t0 = t;
    assert_eq!(play(&mut board, &mut t), one(t0, right.clone()), "SDP_IN_SEL plays the right slot");
    write(&mut board, t, 0x09, 0x0c).expect("left slot");
    // F2: -6 dB (0xB3, twelve 0.5 dB steps under 0xBF) scales by 10^(-6/20), rounded.
    write(&mut board, t, 0x32, 0xb3).expect("-6 dB");
    let t0 = t;
    let scaled: Vec<i16> = left.iter().map(|&x| (f64::from(x) * 10f64.powf(-0.3)).round() as i16).collect();
    assert_eq!(play(&mut board, &mut t), one(t0, scaled), "-6 dB scales every sample by the documented gain");
    write(&mut board, t, 0x32, 0xbf).expect("0 dB");
    // DAC_INV turns the phase (es8311-datasheet.txt:1227-1229).
    write(&mut board, t, 0x31, 0x10).expect("inverted");
    let inverted: Vec<i16> = left.iter().map(|&x| x.saturating_neg()).collect();
    let t0 = t;
    assert_eq!(play(&mut board, &mut t), one(t0, inverted), "DAC_INV negates every sample");
    // F2: each mute silences: DSM, DEM (0x31 bits 6, 5) and the serial port's (0x09 bit 6).
    for (reg, value, unmuted) in [(0x31, 0x40, 0x00), (0x31, 0x20, 0x00), (0x09, 0x4c, 0x0c)] {
        write(&mut board, t, reg, value).expect("mute");
        let t0 = t;
        assert_eq!(play(&mut board, &mut t), one(t0, silence.clone()), "mute {reg:#04x}={value:#04x} silences");
        write(&mut board, t, reg, unmuted).expect("unmute");
    }
    // EN low again: silence, though the codec plays.
    board.pad_drive(t, AMP_EN, Drive::Low).expect("EN falls");
    let t0 = t;
    assert_eq!(play(&mut board, &mut t), one(t0, silence), "the amplifier off silences the speaker again");
    board.pad_drive(t, AMP_EN, Drive::High).expect("EN rises");
    t += 16_000;

    // A frame two periods on starts a new run; a frame within a tick of one period goes on.
    for (dt, sample) in [(0, 100), (2_000, 200), (1_001, 300)] {
        t += dt;
        assert_eq!(board.i2s(&frame(t, [sample, 0])), Ok([0; 2]), "frame accepted");
    }
    let runs: Vec<_> = played(&mut board).into_iter().map(|(run, s)| (run.t0, s)).collect();
    assert_eq!(runs, [(t - 3_001, vec![100]), (t - 1_001, vec![200, 300])], "runs split where a frame is missing");

    // EQ enabled (0x37 bit 3 clear) is not modelled, and says so once.
    write(&mut board, t, 0x37, 0x00).expect("EQ on");
    assert_eq!(board.i2s(&frame(t + 1_000, [1, 0])), Ok([0; 2]), "frame accepted");
    let mut events = Vec::new();
    board.take_bus(&mut events);
    let count = |what: &str| events.iter().filter(|e| matches!(e, BusEvent::Note { what: w, .. } if w.contains(what))).count();
    assert_eq!(count("the speaker hears the DAC's digital path"), 1, "the speaker model's choice is reported once");
    assert_eq!(count("EQ are not modelled"), 1, "unmodelled DAC processing is reported");
}

#[test]
fn audio_nets_join_only_codec_outputs_and_amplifier_inputs() {
    let net = |name: &str, pins: Vec<PinRef>| NetDesc { name: name.into(), rise_ps: None, pull: Pull::None, pins };
    let part = |part: u16, pin: &str| PinRef::Part { part, pin: pin.into() };
    let codec = model("ES8311", &[("addr".into(), f64::from(ADDR))]).expect("codec");
    let amp = model("AMP", &[("enable".into(), 0.0), ("gain_db".into(), -6.0), ("start_ms".into(), 120.0)])
        .expect("an enable-low amplifier at -6 dB");
    let parts = vec![PartDesc { refdes: "U1".into(), kind: codec }, PartDesc { refdes: "U2".into(), kind: amp }];
    let build = |audio: Vec<NetDesc>| {
        let mut nets = vec![net("SCL", vec![PinRef::Chip("scl".into()), part(0, "SCL")]),
                            net("SDA", vec![PinRef::Chip("sda".into()), part(0, "SDA")])];
        nets.extend(audio);
        Netlist::new(BoardDesc { seed: 0, parts: parts.clone(), nets })
    };
    let why = build(vec![net("OUT", vec![part(0, "OUTP"), part(1, "INP"), PinRef::Chip("gpio".into())])])
        .expect_err("a chip pad on an audio net is refused");
    assert!(why.contains("audio net"), "the refusal names the audio net: {why}");
    let why = build(vec![net("OUT", vec![part(0, "OUTP")]), net("IN", vec![part(1, "INP")])])
        .expect_err("an amplifier off every codec output is refused");
    assert!(why.contains("reaches no codec output"), "the refusal names the missing source: {why}");
    let why = build(vec![net("OUT", vec![part(0, "OUTP"), part(1, "INP")]), net("N", vec![part(1, "INN")])])
        .expect_err("INN away from the codec's OUTN is refused");
    assert!(why.contains("INN"), "the refusal names INN: {why}");
    assert!(model("AMP", &[("enable".into(), 2.0), ("gain_db".into(), 0.0), ("start_ms".into(), 0.0)]).is_err(),
        "enable is a level, 0 or 1");
    assert!(model("AMP", &[("enable".into(), 1.0), ("gain_db".into(), 0.0)]).is_err(), "the start time is explicit");
}
