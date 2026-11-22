use fab_elec_mcu_rt::watch::*;

#[test]
fn kernel_faults_round_trip_with_exact_lengths() {
    use fab_elec_mcu_rt::{kernel::KernelFault, sched::Fault};
    for (fault, code, arg) in [
        (Fault::Kernel { kind: KernelFault::Misaligned, site: 321 }, 64, 0),
        (Fault::Kernel { kind: KernelFault::OutOfRegion, site: 321 }, 65, 0),
        (Fault::Kernel { kind: KernelFault::CopyBound, site: 321 }, 66, 0),
        (Fault::Kernel { kind: KernelFault::FillBound, site: 321 }, 67, 0),
        (Fault::KernelReport { code: FAULT_KERNEL_TIMER_REFUSED, site: 321, arg: 7 }, 96, 7),
        (Fault::KernelReport { code: 127, site: 321, arg: i32::MIN }, 127, i32::MIN),
        (Fault::Kernel { kind: KernelFault::BlockInCritical { export: 5 }, site: 321 }, 68, 5),
        (Fault::KernelReport { code: FAULT_KERNEL_CRIT_MISMATCH, site: 321, arg: 0x4037_8a10 }, 97, 0x4037_8a10),
        (Fault::KernelReport { code: FAULT_KERNEL_CRIT_SPAN, site: 321, arg: 912 }, 98, 912),
        (Fault::KernelReport { code: FAULT_KERNEL_ISR_DEPTH, site: 321, arg: 2 }, 99, 2),
        (Fault::KernelReport { code: FAULT_RADIO_PHY_INIT_FAILED, site: 321, arg: -3 }, 100, -3),
        (Fault::KernelReport { code: FAULT_RADIO_PHY_CAL_STORE_FAILED, site: 321, arg: 1 }, 101, 1),
        (Fault::KernelReport { code: FAULT_KERNEL_IRQ_LINE_REFUSED, site: 321, arg: 31 }, 102, 31),
        (Fault::KernelReport { code: FAULT_KERNEL_IRQ_SOURCE_REFUSED, site: 321, arg: 98 }, 103, 98),
        (Fault::KernelReport { code: FAULT_KERNEL_ISR_TABLE_FULL, site: 321, arg: 9 }, 104, 9),
        (Fault::KernelReport { code: FAULT_KERNEL_ISR_ENTRY_REFUSED, site: 321, arg: 0x4038_0000 }, 105, 0x4038_0000),
        (Fault::KernelReport { code: FAULT_RADIO_ISLAND_MISMATCH, site: 321, arg: 0x9e3d_51c7_u32 as i32 }, 106, 0x9e3d_51c7_u32 as i32),
    ] {
        let report = matches!(fault, Fault::KernelReport { .. });
        assert_eq!(FAULT_KERNEL_REPORT.contains(&code), report, "{fault:?}: report block");
        assert_eq!(FAULT_KERNEL_FATAL.contains(&code), !report, "{fault:?}: fatal block");
        let event = Event::from_fault(fault);
        assert_eq!(event, Event::Fault { code, site: 321, arg }, "{fault:?}: reserved kernel code and argument");
        if !report && code != FAULT_KERNEL_BLOCK_IN_CRITICAL {
            assert_eq!(Event::Fault { code, site: 321, arg: 1 }.kernel_fault(), None, "{fault:?}: fatal argument must remain zero");
        }
        let frame = Frame { kind: FAULT, seq: 1, tick: 123, event };
        let wire = packet(&frame);
        let mut body = [0; MAX_BODY];
        let n = cobs_decode(&wire[..wire.len() - 1], &mut body).expect("valid encoded fault");
        let decoded = decode(&body[..n]).expect("kernel fault decodes");
        assert_eq!(decoded, frame, "{fault:?}: ordinary fault frame round trip");
        assert_eq!(decoded.event.kernel_fault(), Some(fault), "{fault:?}: named fault round trip");
        for delta in [-1isize, 1] {
            let end = (n as isize + delta) as usize;
            let mut bad = body;
            bad[end - 1] = crc8(&bad[..end - 1]);
            assert_eq!(decode(&bad[..end]), Err(Bad::Length), "{fault:?}: reject payload length delta {delta}");
        }
    }
    assert_eq!(Event::Fault { code: FAULT_KERNEL_BLOCK_IN_CRITICAL, site: 321, arg: -1 }.kernel_fault(), None,
        "a blocked export's index is never negative");
    let cap = Fault::LoopCap { site: 42 };
    assert_eq!(Event::from_fault(cap).kernel_fault(), Some(cap), "kernel cap keeps the existing app fault code");
}

#[test]
fn radio_start_decodes_the_packed_argument() {
    assert_eq!(radio_start(0xff6e_1234u32 as i32), RadioStart { generation: 0x1234, cause: 110, consecutive: 255 });
}

fn bitwise_crc(bytes: &[u8]) -> u8 {
    let mut crc = 0u8;
    for &byte in bytes {
        crc ^= byte;
        for _ in 0..8 {
            let high = crc & 0x80 != 0;
            crc = crc.wrapping_mul(2);
            if high {
                crc ^= 7;
            }
        }
    }
    crc
}

fn packet(frame: &Frame) -> Vec<u8> {
    let mut wire = [0; MAX_WIRE];
    let len = encode(frame, &mut wire);
    wire[..len].to_vec()
}

fn feed(reader: &mut Reader, bytes: &[u8]) -> Vec<Result<Frame, Bad>> {
    bytes.iter().filter_map(|&byte| reader.push(byte)).collect()
}

#[test]
fn crc_and_cobs_boundaries() {
    assert_eq!(crc8(b"123456789"), 0xf4, "CRC must match the SMBUS check value");
    for (byte, &entry) in CRC8_TABLE.iter().enumerate() {
        assert_eq!(entry, bitwise_crc(&[byte as u8]), "CRC table entry {byte} must match polynomial division");
    }
    let cases = [
        vec![], b"no zeros".to_vec(), vec![0], vec![0; 2], vec![0; 253],
        vec![0x55; 254], vec![0xaa; 255], vec![0, 1, 2, 3, 0],
        [vec![0x55; 254], vec![0]].concat(),
        [vec![0], vec![0x55; 254], vec![0]].concat(),
        vec![0x55; 508],
    ];
    let mut encoded = [0; 512];
    let mut decoded = [0; 512];
    for (case, source) in cases.iter().enumerate() {
        let len = cobs_encode(source, &mut encoded).expect("COBS output buffer must fit every case");
        assert!(!encoded[..len].contains(&0), "COBS case {case} must have no embedded zeros");
        let got = cobs_decode(&encoded[..len], &mut decoded).expect("encoded COBS must decode");
        assert_eq!(&decoded[..got], source.as_slice(), "COBS round trip case {case}");
        assert_eq!(cobs_encode(source, &mut vec![0; len]), Some(len), "exact encoded capacity must fit case {case}");
        assert_eq!(cobs_encode(source, &mut vec![0; len - 1]), None, "short encoded buffer must fail case {case}");
        assert_eq!(cobs_decode(&encoded[..len], &mut vec![0; source.len()]), Some(source.len()),
            "exact decoded capacity must fit case {case}");
        if !source.is_empty() {
            assert_eq!(cobs_decode(&encoded[..len], &mut vec![0; source.len() - 1]), None,
                "short decoded buffer must fail case {case}");
        }
    }
    let len = cobs_encode(&[0x55; 254], &mut encoded).expect("one full COBS block must fit");
    assert_eq!(len, 255, "a full final block must need only its code byte");
    assert_eq!(encoded[0], 255, "254-byte block code must be FF");
    assert_eq!(&encoded[1..len], &[0x55; 254], "full block must preserve every byte");
    for malformed in [&[][..], &[0], &[3, 1], &[2, 0], &[1, 0], &[255, 1]] {
        assert_eq!(cobs_decode(malformed, &mut decoded), None, "malformed COBS {malformed:?} must fail");
    }
}

#[test]
fn events_resync_sequences_and_ticks() {
    let hello = Event::Hello {
        proto: PROTO, ports_version: 2, image: 0x1234_5678, clock_hz: 16_000_000,
        prescale: 8, ring: 64, tick_hi: 9, mcusr: 2, wdtcsr: 8, level: Level::Ports, generation: 0x89ab_cdef,
    };
    let events = [
        (HELLO, hello),
        (WRITE, Event::Write { pin: 13, level: true, site: 0x1234 }),
        (READ, Event::Read { pin: 127, level: false, site: 0xabcd }),
        (FAULT, Event::Fault { code: FAULT_PIN, site: 8, arg: -123456 }),
        (LOST, Event::Lost { count: 0x7fff_ffff, saturated: true, first_tick: 0x1122_0000 }),
        (TIME, Event::Time { tick_hi: 10 }),
        (PROBE, Event::Probe { id: 255, value: 0xbeef }),
        (DUTY, Event::Duty { pin: 11, level: 254, site: 0xabcd }),
        (SHIFT, Event::Shift { data: 11, clock: 13, value: 0x89ab_cdef, bits: 32, site: 0xabcd }),
        (FAULT, Event::Fault { code: FAULT_SHIFT_BITS, site: 0xabcd, arg: 33 }),
        (FAULT, Event::Fault { code: FAULT_I2C_NACK, site: 0x15, arg: 0xfe }),
        (FAULT, Event::Fault { code: FAULT_I2C_TIMEOUT, site: 0x6b, arg: 0x35 }),
        (EEPROM_READ, Event::EepromRead { addr: 1023, value: 0xff, site: 0xabcd }),
        (EEPROM_WRITE, Event::EepromWrite { addr: 1023, value: 0x34, site: 0xabcd }),
        (FAULT, Event::Fault { code: FAULT_EEPROM_ADDR, site: 0xabcd, arg: -1 }),
        (FAULT, Event::Fault { code: FAULT_EEPROM_ADDR, site: 0x1234, arg: 1024 }),
        (RX, Event::Rx { byte: 0 }),
        (RX, Event::Rx { byte: 255 }),
        (FAULT, Event::Fault { code: FAULT_SERIAL_LOST, site: 0, arg: 3 }),
        (FAULT, Event::Fault { code: FAULT_SERIAL_LOST_AT_LEAST, site: 0, arg: 1 }),
    ];
    let mut reader = Reader::new();
    for (i, &(kind, event)) in events.iter().enumerate() {
        let frame = Frame { kind, seq: 253u8.wrapping_add(i as u8), tick: 0x1122_3344 + i as u32, event };
        let bytes = packet(&frame);
        assert!(bytes.len() <= MAX_WIRE, "every event must fit MAX_WIRE");
        assert_eq!(bytes.last(), Some(&0), "frame must end in a delimiter");
        assert_eq!(feed(&mut reader, &bytes), [Ok(frame)], "event {event:?} must survive the wire exactly");
        if kind == SHIFT {
            let mut body = [0; MAX_BODY];
            let len = cobs_decode(&bytes[..bytes.len() - 1], &mut body).expect("valid shift COBS");
            assert_eq!(len, 16, "shift body is six header bytes, nine payload bytes and CRC");
            assert_eq!(&body[6..len - 1], &[11, 13, 32, 0xef, 0xcd, 0xab, 0x89, 0xcd, 0xab], "shift wire field order and little-endian value/site");
        }
        if kind == EEPROM_READ || kind == EEPROM_WRITE {
            let mut body = [0; MAX_BODY];
            let len = cobs_decode(&bytes[..bytes.len() - 1], &mut body).expect("valid EEPROM COBS");
            let value = if kind == EEPROM_READ { 0xff } else { 0x34 };
            assert_eq!(len, 12, "EEPROM body is six header bytes, five payload bytes and CRC");
            assert_eq!(&body[6..len - 1], &[0xff, 3, value, 0xcd, 0xab], "EEPROM wire order is address LE16, value, site LE16");
            body[len - 2] = crc8(&body[..len - 2]);
            assert_eq!(decode(&body[..len - 1]), Err(Bad::Length), "short EEPROM payload is refused");
        }
    }
    assert_eq!(reader.frames, events.len() as u64, "every event must count as a frame");
    assert_eq!(reader.bad, BadCounts::default(), "valid events must cause no errors");
    assert_eq!(reader.gaps, 0, "sequence 255 to 0 must not be a gap");

    let hello_frame = Frame { kind: HELLO, seq: 10, tick: 100, event: hello };
    for level in [Level::Off, Level::Faults, Level::Ports] {
        let mut frame = hello_frame;
        if let Event::Hello { level: encoded, .. } = &mut frame.event { *encoded = level; }
        assert_eq!(feed(&mut Reader::new(), &packet(&frame)), [Ok(frame)], "Hello level round trip");
    }
    let wire = packet(&hello_frame);
    let mut hello_body = [0; MAX_BODY];
    let len = cobs_decode(&wire[..wire.len() - 1], &mut hello_body).unwrap();
    for (offset, bad) in [(6, 1), (23, 3)] {
        let mut body = hello_body;
        body[offset] = bad;
        body[len - 1] = crc8(&body[..len - 1]);
        assert_eq!(decode(&body[..len]), Err(Bad::Kind), "unsupported protocol or level is refused");
    }
    hello_body[6] = 2;
    let old_crc = len - 1 - 4;
    hello_body[old_crc] = crc8(&hello_body[..old_crc]);
    assert_eq!(decode(&hello_body[..=old_crc]), Err(Bad::Length), "PROTO 2's shorter Hello is refused");
    let write = Frame { kind: WRITE, seq: 11, tick: 200, event: events[1].1 };
    reader = Reader::new();
    assert_eq!(feed(&mut reader, b"boot\0"), [Err(Bad::Cobs)], "boot junk must be rejected at its delimiter");
    assert!(feed(&mut reader, &[0, 0]).is_empty(), "empty frames must be skipped");
    assert_eq!(feed(&mut reader, &packet(&hello_frame)), [Ok(hello_frame)], "Hello must recover after boot junk");
    let cut = Frame { kind: PROBE, seq: 11, tick: 0x1122_3344, event: Event::Probe { id: 3, value: 0x7788 } };
    let mut short = packet(&cut)[..2].to_vec();
    short.push(0);
    assert_eq!(feed(&mut reader, &short), [Err(Bad::Cobs)], "cut COBS block must be rejected");
    assert_eq!(reader.bad.cobs, 2, "both malformed packets must be counted");

    let wire = packet(&write);
    let mut body = [0; MAX_BODY];
    let len = cobs_decode(&wire[..wire.len() - 1], &mut body).expect("write body must decode");
    assert_eq!(&body[..len - 1], &[WRITE, 11, 200, 0, 0, 0, 0x8d, 0x34, 0x12],
        "wire layout must put the packed pin after the little-endian tick and before the little-endian site");
    assert_eq!(body[len - 1], bitwise_crc(&body[..len - 1]),
        "frame CRC must cover the complete header and payload");
    body[6] ^= 1;
    let mut flipped = [0; MAX_WIRE];
    let end = cobs_encode(&body[..len], &mut flipped).expect("corrupted body must still fit COBS");
    assert_eq!(feed(&mut reader, &flipped[..end + 1]), [Err(Bad::Crc)], "payload bit flip must fail CRC");
    assert_eq!(reader.bad.crc, 1, "CRC failure must be counted");
    assert_eq!(feed(&mut reader, &wire), [Ok(write)], "bad packets must not advance the sequence");
    assert_eq!(reader.gaps, 0, "bad packets must leave the last good sequence intact");

    let overlong = vec![0x33; ETH_WIRE + 10];
    assert_eq!(feed(&mut reader, &overlong), [Err(Bad::Length)], "an overlong run must emit only one error");
    assert!(feed(&mut reader, &[0, 0]).is_empty(), "overflow delimiter must only resynchronize");
    assert_eq!(reader.bad.length, 1, "overlong run must count one length error");
    assert_eq!(reader.junk, overlong.len() as u64, "all bytes of discarded overlong run must count as junk");
    let next = Frame { seq: 12, ..write };
    assert_eq!(feed(&mut reader, &packet(&next)), [Ok(next)], "next good frame must survive overflow resync");
    let jump = Frame { seq: 15, ..write };
    assert_eq!(feed(&mut reader, &packet(&jump)), [Ok(jump)], "sequence jump must still deliver the frame");
    assert_eq!(reader.gaps, 1, "one sequence jump must count one gap");
    let restart = Frame { seq: 200, ..hello_frame };
    assert_eq!(feed(&mut reader, &packet(&restart)), [Ok(restart)], "Hello must establish a fresh sequence");
    assert_eq!(reader.gaps, 0, "Hello must reset gaps and never count as one");
    let next = Frame { seq: 201, ..write };
    assert_eq!(feed(&mut reader, &packet(&next)), [Ok(next)], "sequence must continue from Hello");
    assert_eq!(reader.gaps, 0, "new epoch must have no inherited sequence gap");

    // Valid CRCs isolate length/kind checking from corruption detection.
    for (kind, expected) in [(HELLO, Bad::Length), (DUTY, Bad::Length), (RX, Bad::Length), (255, Bad::Kind)] {
        let mut body = [kind, 1, 0, 0, 0, 0, 0];
        body[6] = crc8(&body[..6]);
        assert_eq!(decode(&body), Err(expected), "kind {kind} must enforce its payload shape");
        let mut wire = [0; MAX_WIRE];
        let len = cobs_encode(&body, &mut wire).expect("malformed frame body must fit COBS");
        assert_eq!(feed(&mut reader, &wire[..len + 1]), [Err(expected)], "reader must report {expected:?}");
    }
    assert_eq!(reader.bad.length, 4, "all payload length failures must also be counted");
    assert_eq!(reader.bad.kind, 1, "unknown kind must be counted");
    for len in 0..7 {
        assert_eq!(decode(&[0; 7][..len]), Err(Bad::Length), "short body of length {len} must be rejected safely");
    }
    assert_eq!(decode(&[0; ETH_BODY + 1]), Err(Bad::Length), "oversized body must be rejected safely");

    let mut ticks = Unwrap::new();
    let before = Frame { tick: u32::MAX - 1, ..hello_frame };
    assert_eq!(ticks.tick(&before), (9u64 << 32) | u64::from(u32::MAX - 1), "Hello must establish high tick bits");
    let after = Frame { tick: 3, ..write };
    assert_eq!(ticks.tick(&after), (10u64 << 32) | 3, "low tick wrap must increment the high word");
    assert_eq!(ticks.tick(&after), (10u64 << 32) | 3, "equal ticks must not increment the high word");
    let rx = Frame { kind: RX, seq: 12, tick: u32::MAX, event: Event::Rx { byte: 255 } };
    assert_eq!(ticks.tick(&rx), (9u64 << 32) | u64::from(u32::MAX), "queued Rx before a wrap must retain its reception epoch");
    let rx = Frame { tick: 1, ..rx };
    assert_eq!(ticks.tick(&rx), (10u64 << 32) | 1, "queued Rx after a wrap must not create another epoch");
    assert_eq!(ticks.tick(&after), (10u64 << 32) | 3, "Rx must not move the chronological anchor backward");
    let time = Frame { kind: TIME, seq: 12, tick: 1, event: Event::Time { tick_hi: 42 } };
    assert_eq!(ticks.tick(&time), (42u64 << 32) | 1, "Time must set its high word without an extra wrap increment");
    assert_eq!(ticks.tick(&after), (42u64 << 32) | 3, "frames after Time must use its high word");
    assert_eq!(ticks.tick(&hello_frame), (9u64 << 32) | 100, "Hello must reset the tick epoch");
}

#[test]
fn eth_frames_cross_the_link_whole() {
    // The longest payload has nonzero runs longer than a COBS block and zeros, so both block kinds occur.
    let full: Vec<u8> = (0..ETH_MAX).map(|i| if i % 600 == 599 { 0 } else { (i % 255 + 1) as u8 }).collect();
    let header: Vec<u8> = (0..ETH_MIN as u8).map(|i| i * 3).collect();
    let mut reader = Reader::new();
    let mut wire = [0; ETH_WIRE];
    for (seq, payload) in [(7u8, &header[..]), (8, &full[..])] {
        let len = encode_eth(seq, 0, payload, &mut wire).expect("ETH_MIN and ETH_MAX payloads encode");
        assert!(wire[..len - 1].iter().all(|&b| b != 0) && wire[len - 1] == 0, "an Eth frame is one COBS packet and its delimiter");
        let frame = Frame { kind: ETH, seq, tick: 0, event: Event::Eth { len: payload.len() as u16 } };
        assert_eq!(feed(&mut reader, &wire[..len]), [Ok(frame)], "a {}-byte Eth frame decodes with the host's seq and tick 0", payload.len());
        assert_eq!(reader.eth_body(), payload, "the reader lends the {}-byte frame's bytes unchanged", payload.len());
    }
    let credit = Frame { kind: ETH_CREDIT, seq: 9, tick: 0x0102_0304, event: Event::EthCredit { count: 0xbeef } };
    assert_eq!(feed(&mut reader, &packet(&credit)), [Ok(credit)], "a credit round-trips its count");
    assert!(reader.eth_body().is_empty(), "the lent bytes end at the next frame");
    assert_eq!(reader.gaps, 0, "Eth and credit frames continue one sequence");

    let worst = [0xffu8; ETH_MAX];
    let len = encode_eth(0xff, u32::MAX, &worst, &mut wire).expect("the worst-case expansion fits ETH_WIRE");
    assert_eq!(feed(&mut reader, &wire[..len]).len(), 1, "the worst-case frame reaches the reader whole");
    assert_eq!(reader.eth_body(), &worst[..], "the worst-case frame's bytes arrive unchanged");

    for short_or_long in [ETH_MIN - 1, ETH_MAX + 1] {
        let payload = vec![0x5a; short_or_long];
        assert_eq!(encode_eth(1, 0, &payload, &mut wire), None, "encode_eth refuses a {short_or_long}-byte payload");
        let mut body = vec![ETH, 1, 0, 0, 0, 0];
        body.extend_from_slice(&payload);
        body.push(crc8(&body));
        assert_eq!(decode(&body), Err(Bad::Length), "a {short_or_long}-byte Eth payload with a good CRC is a length error");
        let mut packed = vec![0; body.len() + body.len() / 254 + 2];
        let n = cobs_encode(&body, &mut packed).expect("a malformed body still COBS-encodes");
        packed.truncate(n);
        packed.push(0);
        let lengths = reader.bad.length;
        assert_eq!(feed(&mut reader, &packed), [Err(Bad::Length)], "the reader reports a {short_or_long}-byte Eth payload once, as Length");
        assert_eq!(reader.bad.length, lengths + 1, "the {short_or_long}-byte payload counts one length error");
        assert!(reader.eth_body().is_empty(), "a refused frame lends no bytes");
    }
}

#[test]
fn net_frames_carry_device_messages_up_and_host_messages_down() {
    use fab_elec_mcu_rt::net::{code, Dir, NetMsg, NET_PART, PART_BINARY, PART_FIRST, PART_LAST, READY};
    // the device's kinds, as its ports layer frames them on the tether, the longest part included
    let long: Vec<u8> = (0..NET_PART).map(|i| (i % 251) as u8).collect();
    let up = [
        NetMsg::Open { c: 3 }, NetMsg::Close { c: 3 }, NetMsg::Freed { c: 3, slots: 2 },
        NetMsg::Data { c: 3, flags: PART_BINARY | PART_FIRST | PART_LAST, bytes: long },
    ];
    let mut reader = Reader::new();
    for (seq, msg) in up.iter().enumerate() {
        let mut body = vec![msg.kind(), seq as u8, 9, 0, 0, 0];
        msg.encode(&mut body);
        body.push(crc8(&body));
        let mut wire = vec![0; ETH_WIRE];
        let n = cobs_encode(&body, &mut wire).expect("a device message COBS-encodes");
        wire.truncate(n);
        wire.push(0);
        let len = (body.len() - 7) as u16;
        assert_eq!(feed(&mut reader, &wire), [Ok(Frame { kind: msg.kind(), seq: seq as u8, tick: 9, event: Event::Net { len } })],
            "{msg:?} decodes as a Net frame with its tick");
        assert_eq!(NetMsg::decode(msg.kind(), Dir::Up, reader.net_body()).as_ref(), Some(msg), "the reader lends {msg:?}'s payload");
        assert!(reader.eth_body().is_empty(), "a Net frame lends no Eth bytes");
    }
    assert_eq!(reader.gaps, 0, "Net frames continue the device's sequence");
    let probe = Frame { kind: PROBE, seq: 4, tick: 10, event: Event::Probe { id: 1, value: 2 } };
    assert_eq!(feed(&mut reader, &packet(&probe)), [Ok(probe)]);
    assert!(reader.net_body().is_empty(), "the lent payload ends at the next frame");

    // the host's own kinds never come from a device; a malformed device message is a length error
    let host = NetMsg::State { c: 3, state: READY, code: code::NONE, detail: 0 };
    for (kind, payload, bad) in [(NET_STATE, host_payload(&host), Bad::Kind), (NET_LINK, vec![2, 0, 0, 0, 0, 0], Bad::Kind),
        (NET_OPEN, vec![3, 0], Bad::Length), (NET_CREDIT, vec![3, 0, 0], Bad::Length), (NET_DATA, vec![3, 8, 1], Bad::Length)] {
        let mut body = vec![kind, 0, 0, 0, 0, 0];
        body.extend_from_slice(&payload);
        body.push(crc8(&body));
        assert_eq!(decode(&body), Err(bad), "kind {kind} with payload {payload:?}");
    }

    // the host's messages echo the device's Hello tick and decode downward
    let down = [
        host, NetMsg::Link { state: READY, code: code::NONE, detail: 7 }, NetMsg::Granted { c: 3, bytes: 16384 },
        NetMsg::Data { c: 3, flags: PART_FIRST | PART_LAST, bytes: vec![0; NET_PART] },
    ];
    let mut wire = [0; ETH_WIRE];
    for (seq, msg) in down.iter().enumerate() {
        let len = encode_net(seq as u8, 0x0102_0304, msg, &mut wire);
        let packet = wire[..len].strip_suffix(&[0]).expect("a host message ends in a delimiter");
        assert!(!packet.contains(&0), "{msg:?} is one COBS packet");
        let mut body = [0; ETH_BODY];
        let n = cobs_decode(packet, &mut body).expect("the host's packet decodes");
        assert_eq!(&body[..6], &[msg.kind(), seq as u8, 4, 3, 2, 1], "kind, host sequence and the echoed tick");
        assert_eq!(body[n - 1], crc8(&body[..n - 1]), "the host message's CRC");
        assert_eq!(NetMsg::decode(msg.kind(), Dir::Down, &body[6..n - 1]).as_ref(), Some(msg), "{msg:?} crosses unchanged");
    }
}

fn host_payload(msg: &fab_elec_mcu_rt::net::NetMsg) -> Vec<u8> {
    let mut payload = Vec::new();
    msg.encode(&mut payload);
    payload
}

#[test]
fn radio_restart_is_an_empty_host_command() {
    let mut wire = [0; MAX_WIRE];
    for (seq, tick) in [(0, 0), (3, 0x1234_5678)] {
        let len = encode_radio_restart(seq, tick, &mut wire);
        let packet = wire[..len].strip_suffix(&[0]).expect("restart ends in a delimiter");
        assert!(!packet.contains(&0), "no embedded zero for seq {seq}");
        let mut body = [0; 7];
        assert_eq!(cobs_decode(packet, &mut body), Some(7), "six header bytes and CRC for seq {seq}");
        assert_eq!(&body[..2], &[16, seq], "restart kind and host sequence");
        assert_eq!(&body[2..6], &tick.to_le_bytes(), "epoch echoed in little endian");
        assert_eq!(body[6], bitwise_crc(&body[..6]), "valid restart CRC for seq {seq}");
        assert_eq!(decode(&body), Err(Bad::Kind), "a restart is host-to-device only");
    }
}

#[test]
fn link_counters_keep_all_64_bits_and_require_exact_length() {
    let mut reader = Reader::new();
    for (counter, value) in [0, u32::MAX as u64, 1 << 32, 0x0123_4567_89ab_cdef, u64::MAX].into_iter().enumerate() {
        let frame = Frame { kind: LINK, seq: counter as u8, tick: 0x1020_3040,
            event: Event::Link { counter: counter as u8, value } };
        let wire = packet(&frame);
        assert_eq!(wire.len(), 18, "LINK fits the event wire buffer");
        assert_eq!(feed(&mut reader, &wire), [Ok(frame)], "every counter retains its full u64");
        let mut body = [0; MAX_BODY];
        let n = cobs_decode(&wire[..wire.len() - 1], &mut body).unwrap();
        assert_eq!(n, 16, "six header bytes, nine payload bytes, CRC");
        assert_eq!(&body[7..15], &value.to_le_bytes(), "LINK value is little endian");
        for len in [15, 17] {
            let mut malformed = body;
            malformed[len - 1] = crc8(&malformed[..len - 1]);
            assert_eq!(decode(&malformed[..len]), Err(Bad::Length), "LINK rejects body length {len}");
        }
    }
    assert_eq!(reader.gaps, 0);
}

#[test]
fn radio_record_fills_the_event_body_and_requires_exact_length() {
    let frame = Frame { kind: RADIO_RECORD, seq: 255, tick: 0x1020_3040,
        event: Event::RadioRecord { index: 0x1234, words: [164, 0, 0x89ab_cdef, u32::MAX] } };
    let wire = packet(&frame);
    // RadioRecord is shorter than Hello, the largest watch event.
    assert_eq!(wire.len(), 27, "RadioRecord wire length includes the COBS code and delimiter");
    assert_eq!(feed(&mut Reader::new(), &wire), [Ok(frame)], "RadioRecord round-trips every field through the reader");
    let mut body = [0; MAX_BODY + 1];
    let n = cobs_decode(&wire[..wire.len() - 1], &mut body).expect("RadioRecord COBS decodes");
    assert_eq!(n, 25, "RadioRecord body is six header bytes, eighteen payload bytes and CRC");
    assert_eq!(&body[6..8], &0x1234u16.to_le_bytes(), "frame index is little endian");
    for (chunk, word) in body[8..24].chunks_exact(4).zip([164u32, 0, 0x89ab_cdef, u32::MAX]) {
        assert_eq!(chunk, &word.to_le_bytes(), "each record word is little endian");
    }
    for len in [24, 26] {
        let mut malformed = body;
        malformed[len - 1] = crc8(&malformed[..len - 1]);
        assert_eq!(decode(&malformed[..len]), Err(Bad::Length), "RadioRecord rejects body length {len} even with valid CRC");
        let mut encoded = [0; MAX_WIRE + 1];
        let n = cobs_encode(&malformed[..len], &mut encoded).expect("malformed frame still COBS encodes");
        encoded[n] = 0;
        assert_eq!(feed(&mut Reader::new(), &encoded[..n + 1]), [Err(Bad::Length)],
            "the reader refuses a RadioRecord one byte short or long");
    }
}
