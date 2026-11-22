use fab_elec_mcu_flash::watch::{watch, watch_until, Reset};
use fab_elec_mcu_rt::watch::*;
use makepad_serial::line::Line;
use std::{collections::VecDeque, io, time::{Duration, Instant}};

struct FakeLine {
    origin: Instant,
    now: Duration,
    input: VecDeque<(Duration, u8)>,
    /// Each control change: when, which line ('D' DTR, 'R' RTS), and its state.
    controls: Vec<(Duration, char, bool)>,
    drained: usize,
    close_at: Option<Duration>,
}

impl FakeLine {
    fn new() -> Self {
        Self {
            origin: Instant::now(), now: Duration::ZERO, input: VecDeque::new(),
            controls: Vec::new(), drained: 0, close_at: None,
        }
    }

    fn bytes(&mut self, at_ms: u64, bytes: &[u8]) {
        self.input.extend(bytes.iter().enumerate().map(|(i, &byte)| {
            (Duration::from_millis(at_ms + i as u64), byte)
        }));
    }

    fn frame(&mut self, at_ms: u64, frame: Frame) {
        let mut wire = [0; MAX_WIRE];
        let len = encode(&frame, &mut wire);
        self.bytes(at_ms, &wire[..len]);
    }
}

impl Line for FakeLine {
    fn write_all(&mut self, _bytes: &[u8]) -> io::Result<()> {
        panic!("watch must never write serial bytes");
    }

    fn read_byte(&mut self, timeout: Duration) -> io::Result<Option<u8>> {
        let deadline = self.now + timeout;
        if let Some(&(at, byte)) = self.input.front() {
            if at <= deadline {
                self.now = self.now.max(at);
                self.input.pop_front();
                return Ok(Some(byte));
            }
        }
        if let Some(at) = self.close_at {
            if at <= deadline {
                self.now = self.now.max(at);
                return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "fake port closed"));
            }
        }
        self.now = deadline;
        Ok(None)
    }

    fn set_dtr(&mut self, on: bool) -> io::Result<()> {
        self.controls.push((self.now, 'D', on));
        Ok(())
    }

    fn set_rts(&mut self, on: bool) -> io::Result<()> {
        self.controls.push((self.now, 'R', on));
        Ok(())
    }

    fn set_baud(&mut self, _baud: u32) -> io::Result<()> {
        panic!("watch must use the baud already selected at open");
    }

    fn drain_input(&mut self) -> io::Result<()> {
        while self.input.front().is_some_and(|&(at, _)| at <= self.now) {
            self.input.pop_front();
            self.drained += 1;
        }
        Ok(())
    }

    fn sleep(&mut self, duration: Duration) { self.now += duration; }
    fn reopen(&mut self) -> io::Result<()> { Ok(()) }
    fn now(&self) -> Instant { self.origin + self.now }
}

#[test]
fn watch_output_reset_deadline_and_disconnect() {
    let mut line = FakeLine::new();
    line.bytes(0, &[1, 2]);
    line.bytes(60, &[255, 1, 0]);
    let write = Frame {
        kind: WRITE, seq: 11, tick: u32::MAX - 1,
        event: Event::Write { pin: 13, level: true, site: 4 },
    };
    line.frame(70, Frame { seq: 90, ..write });
    let hello = Frame {
        kind: HELLO, seq: 10, tick: u32::MAX - 4,
        event: Event::Hello {
            proto: PROTO, ports_version: fab_elec_mcu_rt::ports::PORT_TABLE_VERSION as u8, image: 42, clock_hz: 16_000_000,
            prescale: 8, ring: 64, tick_hi: 2, mcusr: 2, wdtcsr: 0, level: Level::Ports, generation: 0,
        },
    };
    let mut wire = [0; MAX_WIRE];
    let len = encode(&hello, &mut wire);
    let mut body = [0; MAX_BODY];
    let body_len = cobs_decode(&wire[..len - 1], &mut body).expect("Hello body must decode");
    body[body_len - 1] ^= 1;
    let len = cobs_encode(&body[..body_len], &mut wire).expect("bad CRC Hello must still fit");
    wire[len] = 0;
    line.bytes(90, &wire[..len + 1]);
    line.frame(140, hello);
    line.frame(180, write);
    line.frame(200, Frame {
        kind: READ, seq: 12, tick: 3, event: Event::Read { pin: 7, level: false, site: 5 },
    });
    line.frame(220, Frame {
        kind: FAULT, seq: 13, tick: 4, event: Event::Fault { code: FAULT_OVERRUN, site: 6, arg: 3 },
    });
    line.frame(240, Frame {
        kind: LOST, seq: 14, tick: 5,
        event: Event::Lost { count: 17, saturated: true, first_tick: u32::MAX - 1 },
    });
    line.frame(270, Frame { kind: TIME, seq: 15, tick: 6, event: Event::Time { tick_hi: 7 } });
    line.frame(290, Frame { kind: PROBE, seq: 16, tick: 7, event: Event::Probe { id: 2, value: 99 } });
    line.frame(310, Frame { kind: RX, seq: 17, tick: 5, event: Event::Rx { byte: 255 } });
    line.frame(330, Frame {
        kind: FAULT, seq: 18, tick: 8, event: Event::Fault { code: FAULT_SERIAL_LOST, site: 0, arg: 3 },
    });
    line.frame(360, Frame {
        kind: FAULT, seq: 19, tick: 9, event: Event::Fault { code: FAULT_SERIAL_LOST_AT_LEAST, site: 0, arg: 1 },
    });

    let mut output = Vec::new();
    watch(&mut line, Reset::Dtr, Some(Duration::from_millis(350)), &mut output)
        .expect("watch must complete under virtual time");
    // The Hello line prints the protocol the image speaks, whichever is current.
    let expected = format!(concat!(
        "10 12884901883 hello proto={} ports_version=15 image=42 clock_hz=16000000 prescale=8 ring=64 tick_hi=2 mcusr=2 wdtcsr=0 level=ports generation=0 reset_ms=140\n",
        "11 12884901886 write pin=13 level=1 site=4\n",
        "12 12884901891 read pin=7 level=0 site=5\n",
        "13 12884901892 fault overrun code=2 arg=3 site=6\n",
        "14 12884901893 lost count=17 first=12884901886 last=12884901893 saturated=1\n",
        "15 30064771078 time tick_hi=7\n",
        "16 30064771079 probe id=2 value=99\n",
        "17 30064771077 rx byte=255\n",
        "18 30064771080 fault serial_lost code=5 arg=3 site=0\n",
        "19 30064771081 fault serial_lost_at_least code=6 arg=1 site=0\n",
        "summary frames=11 bad_cobs=1 bad_crc=1 bad_length=0 bad_kind=0 junk=0 gaps=0 lost=17 lost_saturated=1\n",
    ), PROTO);
    assert_eq!(String::from_utf8(output).expect("watch output must be UTF-8"), expected,
        "watch must gate on a CRC-valid Hello, timestamp its first byte and print every field and counter exactly");
    assert_eq!(line.controls, [(Duration::ZERO, 'D', true), (Duration::from_millis(50), 'D', false)],
        "DTR must assert for exactly 50 ms before release, and RTS stays alone");
    assert_eq!(line.drained, 2, "DTR reset must drain only already-arrived input");
    assert_eq!(line.now, Duration::from_millis(400), "--ms must bound reception after the reset pulse");

    let mut line = FakeLine::new();
    line.close_at = Some(Duration::from_secs(3));
    line.frame(2000, hello);
    line.frame(2100, Frame { kind: FAULT, seq: 11, tick: 4,
        event: Event::Fault { code: FAULT_I2C_NACK, site: 0x15, arg: 0xfe } });
    line.frame(2200, Frame { kind: FAULT, seq: 12, tick: 5,
        event: Event::Fault { code: FAULT_I2C_TIMEOUT, site: 0x6b, arg: 0x35 } });
    let mut output = Vec::new();
    watch(&mut line, Reset::None, None, &mut output).expect("port closure must end an unlimited watch cleanly");
    assert!(line.controls.is_empty(), "reset none must not touch DTR or RTS");
    assert_eq!(line.drained, 0, "reset none must preserve queued input");
    assert_eq!(line.now, Duration::from_secs(3), "idle read timeouts must not end an unlimited watch");
    assert_eq!(String::from_utf8(output).expect("watch output must be UTF-8"), format!(concat!(
        "10 12884901883 hello proto={} ports_version=15 image=42 clock_hz=16000000 prescale=8 ring=64 tick_hi=2 mcusr=2 wdtcsr=0 level=ports generation=0 reset_ms=2000\n",
        "11 12884901892 fault i2c_nack code=14 arg=254 site=21\n",
        "12 12884901893 fault i2c_timeout code=15 arg=53 site=107\n",
        "summary frames=3 bad_cobs=0 bad_crc=0 bad_length=0 bad_kind=0 junk=0 gaps=0 lost=0 lost_saturated=0\n",
    ), PROTO), "disconnect must print the summary and reset none must time from entry");

    let mut line = FakeLine::new();
    line.bytes(0, b"ESP-ROM\r\nentry 1");
    line.frame(300, hello);
    let mut output = Vec::new();
    watch(&mut line, Reset::Usj, Some(Duration::from_millis(400)), &mut output).expect("a USJ reset watch must complete");
    let (at, released) = (Duration::ZERO, Duration::from_millis(100));
    assert_eq!(line.controls, [(at, 'D', false), (at, 'R', false), (at, 'R', true), (released, 'R', false)],
        "a USJ flash-boot reset must follow TRM Table 33.4-4: DTR off, then an RTS write (clears the download flag), then a 100 ms RTS pulse");
    assert_eq!(line.drained, 0, "a USJ reset must keep every byte after it");
    assert_eq!(String::from_utf8(output).expect("watch output must be UTF-8"), format!(concat!(
        "rom ESP-ROM\n",
        "rom entry 1\n",
        "10 12884901883 hello proto={} ports_version=15 image=42 clock_hz=16000000 prescale=8 ring=64 tick_hi=2 mcusr=2 wdtcsr=0 level=ports generation=0 reset_ms=300\n",
        "summary frames=1 bad_cobs=0 bad_crc=0 bad_length=0 bad_kind=0 junk=0 gaps=0 lost=0 lost_saturated=0\n",
    ), PROTO), "the ROM's lines must print as rom text, a Hello right after the last one must still be found, and reset_ms must time from the reset");

    let mut line = FakeLine::new();
    let mut output = Vec::new();
    let mut checks = 0;
    watch_until(&mut line, Reset::None, None, &mut output, || {
        checks += 1;
        checks > 1
    }).expect("stop request must end watch cleanly");
    assert_eq!(line.now, Duration::from_secs(1), "stop callback must be checked after an idle read");
    assert_eq!(String::from_utf8(output).expect("summary must be UTF-8"),
        "summary frames=0 bad_cobs=0 bad_crc=0 bad_length=0 bad_kind=0 junk=0 gaps=0 lost=0 lost_saturated=0\n",
        "stop request must print a final summary");
}
