use std::sync::Arc;

use fab_elec_mcu_emu::board::{Board, I2cAns, I2cOp, Res};
use fab_elec_mcu_emu::Pull;
use fab_elec_mcu_parts::{
    BoardDesc, MotionSample, MotionTrack, NetDesc, Netlist, PartDesc, PartKind, PinRef, Stimulus,
    TrackRead,
};

// Deadlines fall between board ticks, and the ramp crosses sample 4096 during the run.
const HZ: u64 = 1_001;
const ENABLE: u64 = 17 * HZ + 1;
const PS_PER_SEC: u128 = 1_000_000_000_000;

fn step(board: &mut Netlist, t: u64, op: I2cOp) -> Res<I2cAns> {
    board.i2c(t, 0, 1, op)
}

fn select(board: &mut Netlist, t: u64, reg: u8) -> Res {
    step(board, t, I2cOp::Start)?;
    assert_eq!(step(board, t, I2cOp::Write(0x6b << 1))?, I2cAns::Ack(true), "IMU address must ACK");
    assert_eq!(step(board, t, I2cOp::Write(reg))?, I2cAns::Ack(true), "register pointer must ACK");
    Ok(())
}

fn write(board: &mut Netlist, t: u64, reg: u8, value: u8) -> Res {
    select(board, t, reg)?;
    assert_eq!(step(board, t, I2cOp::Write(value))?, I2cAns::Ack(true), "register write must ACK");
    step(board, t, I2cOp::Stop)?;
    Ok(())
}

fn word(board: &mut Netlist, t: u64, reg: u8) -> Res<i16> {
    select(board, t, reg)?;
    step(board, t, I2cOp::Start)?;
    assert_eq!(step(board, t, I2cOp::Write(0x6b << 1 | 1))?, I2cAns::Ack(true), "read address must ACK");
    let mut bytes = [0; 2];
    for (i, byte) in bytes.iter_mut().enumerate() {
        let I2cAns::Byte(value) = step(board, t, I2cOp::Read { ack: i == 0 })? else {
            panic!("selected output register must return a byte");
        };
        *byte = value;
    }
    step(board, t, I2cOp::Stop)?;
    Ok(i16::from_le_bytes(bytes))
}

fn instant_ps(ms: u64) -> u128 {
    u128::from(ENABLE) * PS_PER_SEC / u128::from(HZ) + u128::from(ms) * 1_000_000_000
}

fn read_tick(ms: u64) -> u64 {
    ENABLE + (ms * HZ).div_ceil(1_000)
}

fn x_of(sample: MotionSample) -> i16 {
    let product = i64::from(sample.accel_ug[0]) * 16_384;
    ((product + product.signum() * 500_000) / 1_000_000) as i16
}

fn expected_x(track: &MotionTrack, ms: u64) -> i16 {
    let TrackRead::Sample(sample) = track.at(instant_ps(ms)) else {
        panic!("the ramp must cover the publication at {ms} ms");
    };
    x_of(sample)
}

#[test]
fn motion_at_sensor_publications() {
    let wire = |name: &'static str| NetDesc {
        name: name.into(),
        pull: Pull::Up,
        rise_ps: None,
        pins: vec![PinRef::Chip(name.into()), PinRef::Part { part: 0, pin: name.into() }],
    };
    let mut slow = Netlist::new(BoardDesc {
        seed: 0,
        parts: vec![PartDesc { refdes: "U5".into(), kind: PartKind::Qmi8658c { addr: 0x6b } }],
        nets: vec![wire("SCL"), wire("SDA")],
    }).expect("IMU board must wire");
    slow.bind(&["SCL", "SDA"], HZ).expect("IMU board must bind");
    write(&mut slow, ENABLE, 2, 0x40).expect("CTRL1 must enable little-endian bursts");

    let mut delayed = slow.clone();
    let later = Arc::new(MotionTrack::new(18_000_000_000_000, (240, 1)));
    later.push(MotionSample { accel_ug: [500_000, 0, 0], gyro_mdps: [0; 3] });
    assert_eq!(later.at(instant_ps(27)), TrackRead::Before, "an instant before sample zero precedes the track");
    delayed.attach_motion_track(0, later).expect("delayed track must attach");

    let track = Arc::new(MotionTrack::new(0, (240, 1)));
    let ramp = |i: i32| MotionSample { accel_ug: [(i - 4096) * 25_000, 0, 0], gyro_mdps: [0; 3] };
    for i in 0..4088 {
        track.push(ramp(i));
    }
    slow.attach_motion_track(0, Arc::clone(&track)).expect("ramp must attach");
    let mut fast = slow.clone();
    for (board, ctrl2) in [(&mut slow, 6), (&mut delayed, 6), (&mut fast, 4)] {
        write(board, ENABLE, 3, ctrl2).expect("CTRL2 must select the accel ODR");
        write(board, ENABLE, 8, 1).expect("CTRL7 must enable accel");
    }

    // The newest input wins: a motion stimulus ends the attached track from its tick on.
    let mut held = slow.clone();
    held.input(ENABLE, Stimulus::Motion { part: 0, accel_ug: [250_000, 0, 0], gyro_mdps: [0; 3] })
        .expect("a motion stimulus replaces an attached track, never fails the run");
    assert_eq!(word(&mut held, read_tick(27), 0x35), Ok(4_096), "after the stimulus X reads its 0.25 g, not the track");
    for ms in [27, 35] {
        assert_eq!(word(&mut delayed, read_tick(ms), 0x35), Ok(0), "pre-origin X must stay level");
        assert_eq!(word(&mut delayed, read_tick(ms), 0x39), Ok(16_384), "pre-origin Z must publish +1g");
    }

    // At 125 Hz the first publication is 3 ms + 3 periods after enable.
    assert_eq!(word(&mut slow, read_tick(27), 0x35), Ok(expected_x(&track, 27)), "first 125 Hz sample must use its deadline");
    let mut saved = slow.clone();
    // A read past the open track's end is a live run's hold bug: the sensor keeps the last written
    // sample rather than fail, and the track counts the read.
    assert_eq!(word(&mut slow, read_tick(35), 0x35), Ok(x_of(ramp(4087))), "a missing interpolation endpoint keeps the last sample");
    assert_eq!(track.short_reads(), 1, "the track counts the short read");
    for i in 4088..=4116 {
        track.push(ramp(i));
    }
    assert_eq!(track.at(17_065_000_000_000), TrackRead::Sample(MotionSample { accel_ug: [-10_000, 0, 0], gyro_mdps: [0; 3] }),
        "the ramp must interpolate across the segment boundary from sample 4095 to 4096");
    for ms in [35, 43, 51, 59, 67] {
        let x = word(&mut saved, read_tick(ms), 0x35);
        assert_eq!(x, Ok(expected_x(&track, ms)), "125 Hz accel X must match the track at {ms} ms: the clone from before the short read sees later appends");
        if ms > 35 {
            assert_eq!(word(&mut slow, read_tick(ms), 0x35), x, "after a short read the sensor follows the track again at {ms} ms");
        }
    }
    assert_eq!(track.short_reads(), 1, "reads within the track count none");
    // At 500 Hz several publications interpolate inside the same motion interval.
    for ms in [9, 11, 13, 15, 17, 19, 21, 23, 67] {
        assert_eq!(word(&mut fast, read_tick(ms), 0x35), Ok(expected_x(&track, ms)),
            "500 Hz accel X must match the last publication at {ms} ms");
    }

    let last = TrackRead::Sample(ramp(4116));
    assert_eq!(track.at(17_150_000_000_000), last, "the exact last sample must be readable while open");
    assert_eq!(track.at(u128::MAX), TrackRead::Ahead, "the largest instant must be ahead of the open track");
    track.close();
    assert_eq!(track.at(17_150_000_000_001), last, "the closed track must hold immediately past its end");
    assert_eq!(track.at(u128::MAX), last, "the closed track must hold through the largest instant");

    let reserved = MotionTrack::new(0, (240, 1));
    for k in 0..3 * 4096 + 5 {
        assert!(!reserved.push(ramp(k)), "reserved push at index {k} must append without allocating");
        reserved.reserve();
    }
    for k in 0..3 * 4096 + 5 {
        assert_eq!(reserved.sample(k as u64), Some(ramp(k)), "the reserved track must preserve sample {k}");
    }

    let unreserved = MotionTrack::new(0, (240, 1));
    for k in 0..2 * 4096 + 5 {
        assert_eq!(unreserved.push(ramp(k)), k == 8191,
            "push {k} must report allocation only at 8191 after the initial spare was used at 4095");
    }
    for k in 0..2 * 4096 + 5 {
        assert_eq!(unreserved.sample(k as u64), Some(ramp(k)), "the track must preserve sample {k} without reserve calls");
    }
}
