use fab_elec_mcu_flash::{
    esp_rom::{self, Entry, EspRegion, EspWrite, Exit, Grant, Layout, ResetReport, Waiver},
    identify::{identify, Board},
    md5,
    stk500::{self, *},
};
use makepad_core_util::sha256;
use makepad_serial::line::Line;
use std::{collections::VecDeque, fs, io, time::{Duration, Instant, SystemTime, UNIX_EPOCH}};

#[derive(Clone, Copy)]
struct BootloaderTiming {
    start: Duration,
    ready: Duration,
    silent_reset: bool,
}

struct FakeArduino {
    baud: u32,
    sw_major: u8,
    signature: [u8; 3],
    flash: Vec<u8>,
    app_output: Vec<u8>,
    last_command: Option<Duration>,
    pending: Vec<u8>,
    address: usize,
    programming: bool,
    commands: Vec<u8>,
    timing: Option<BootloaderTiming>,
    booting_at: Option<Duration>,
    buffered: Vec<u8>,
    resets: usize,
}

impl FakeArduino {
    fn new(baud: u32, sw_major: u8) -> Self {
        Self {
            baud,
            sw_major,
            signature: [0x1e, 0x95, 0x0f],
            flash: vec![0xff; 32768],
            app_output: b"hello\r\n".to_vec(),
            last_command: None,
            pending: Vec::new(),
            address: 0,
            programming: false,
            commands: Vec::new(),
            timing: None,
            booting_at: None,
            buffered: Vec::new(),
            resets: 0,
        }
    }

    fn reset(&mut self, now: Duration) {
        self.resets += 1;
        self.booting_at = self.timing.map(|_| now);
        self.buffered.clear();
        self.last_command = Some(now);
        self.pending.clear();
        self.address = 0;
        self.programming = false;
    }

    fn ready_at(&self) -> Option<Duration> {
        Some(self.booting_at? + self.timing?.ready)
    }

    fn advance(&mut self, now: Duration, input: &mut VecDeque<u8>) {
        if let Some(ready_at) = self.ready_at().filter(|&ready_at| ready_at <= now) {
            self.booting_at = None;
            let bytes = std::mem::take(&mut self.buffered);
            self.receive(ready_at, &bytes, input);
        }
    }

    fn write(&mut self, now: Duration, baud: u32, bytes: &[u8], input: &mut VecDeque<u8>) {
        if let Some(timing) = self.timing {
            if self.last_command.is_none() { return; }
            if let Some(reset) = self.booting_at {
                if now >= reset + timing.start && baud == self.baud {
                    let space = 3 - self.buffered.len();
                    self.buffered.extend(bytes.iter().take(space).copied());
                }
                return;
            }
        }
        if baud != self.baud {
            input.extend(bytes.iter().map(|_| 0x00));
            return;
        }
        self.receive(now, bytes, input);
    }

    fn receive(&mut self, now: Duration, bytes: &[u8], input: &mut VecDeque<u8>) {
        if !self.last_command.is_some_and(|last| now - last < Duration::from_secs(1)) {
            self.last_command = None;
            self.pending.clear();
            return;
        }
        for &byte in bytes {
            self.pending.push(byte);
            let expected = match self.pending[0] {
                GET_SYNC | ENTER_PROGMODE | LEAVE_PROGMODE | READ_SIGN => 2,
                GET_PARAMETER => 3,
                LOAD_ADDRESS => 4,
                READ_PAGE => 5,
                PROG_PAGE => {
                    if self.pending.len() < 4 { continue; }
                    5 + u16::from_be_bytes([self.pending[1], self.pending[2]]) as usize
                }
                _ => {
                    input.push_back(STK_NOSYNC);
                    self.pending.clear();
                    continue;
                }
            };
            if self.pending.len() == expected {
                let packet = std::mem::take(&mut self.pending);
                if packet.last() != Some(&CRC_EOP) {
                    if self.timing.is_some_and(|timing| timing.silent_reset) {
                        self.last_command = None;
                        self.programming = false;
                        return;
                    }
                    input.push_back(STK_NOSYNC);
                    continue;
                }
                self.execute(now, &packet, input);
            }
        }
    }

    fn execute(&mut self, now: Duration, packet: &[u8], input: &mut VecDeque<u8>) {
        let mut payload = Vec::new();
        let mut status = STK_OK;
        match packet[0] {
            GET_SYNC => {}
            GET_PARAMETER => {
                let value = match packet[1] {
                    HW_VER => 3,
                    SW_MAJOR => self.sw_major,
                    SW_MINOR => 0,
                    _ => { input.push_back(STK_NOSYNC); return; }
                };
                payload.push(value);
            }
            READ_SIGN => payload.extend_from_slice(&self.signature),
            ENTER_PROGMODE => self.programming = true,
            LEAVE_PROGMODE => self.programming = false,
            LOAD_ADDRESS => {
                self.address = u16::from_le_bytes([packet[1], packet[2]]) as usize * 2;
            }
            PROG_PAGE | READ_PAGE => {
                if packet[3] != b'F' {
                    input.push_back(STK_NOSYNC);
                    return;
                }
                let len = u16::from_be_bytes([packet[1], packet[2]]) as usize;
                if !self.programming || self.address + len > self.flash.len() {
                    status = STK_FAILED;
                } else {
                    let flash = &mut self.flash[self.address..self.address + len];
                    if packet[0] == PROG_PAGE {
                        flash.copy_from_slice(&packet[4..packet.len() - 1]);
                    } else {
                        payload.extend_from_slice(flash);
                    }
                    self.address += len;
                }
            }
            _ => { input.push_back(STK_NOSYNC); return; }
        }
        self.commands.push(packet[0]);
        self.last_command = if packet[0] == LEAVE_PROGMODE { None } else { Some(now) };
        input.push_back(STK_INSYNC);
        input.extend(payload);
        input.push_back(status);
    }
}

struct FakeEsp {
    magic: u32,
    download_mode: bool,
    reset_at: Option<Duration>,
    in_frame: bool,
    escaped: bool,
    pending: Vec<u8>,
    commands: Vec<u8>,
}

impl FakeEsp {
    fn new(magic: u32) -> Self {
        Self {
            magic,
            download_mode: false,
            reset_at: None,
            in_frame: false,
            escaped: false,
            pending: Vec::new(),
            commands: Vec::new(),
        }
    }

    fn rts(&mut self, now: Duration, was: bool, on: bool, dtr: bool, input: &mut VecDeque<u8>) {
        if on {
            self.download_mode = false;
            self.reset_at = if !dtr { Some(now) } else { None };
            self.in_frame = false;
            self.escaped = false;
            self.pending.clear();
        } else if was {
            self.download_mode = dtr && self.reset_at.is_some_and(|start| {
                now - start >= Duration::from_millis(100)
            });
            self.reset_at = None;
            input.extend(b"boot\r\n");
        }
    }

    fn write(&mut self, baud: u32, bytes: &[u8], input: &mut VecDeque<u8>) {
        if !self.download_mode || baud != 115200 { return; }
        for &byte in bytes {
            if byte == 0xc0 {
                if self.in_frame && !self.pending.is_empty() {
                    assert!(!self.escaped, "ESP request must not end inside a SLIP escape");
                    let packet = std::mem::take(&mut self.pending);
                    self.execute(&packet, input);
                }
                self.in_frame = true;
                self.escaped = false;
            } else if self.in_frame {
                if self.escaped {
                    self.pending.push(match byte {
                        0xdc => 0xc0,
                        0xdd => 0xdb,
                        _ => panic!("ESP request must use valid SLIP escapes"),
                    });
                    self.escaped = false;
                } else if byte == 0xdb {
                    self.escaped = true;
                } else {
                    self.pending.push(byte);
                }
            }
        }
    }

    fn execute(&mut self, packet: &[u8], input: &mut VecDeque<u8>) {
        assert!(packet.len() >= 8, "ESP request must contain its full header");
        assert_eq!(packet[0], 0, "ESP request direction must be host to ROM");
        let size = u16::from_le_bytes([packet[2], packet[3]]) as usize;
        assert_eq!(packet.len(), size + 8, "ESP request size must match its payload");
        assert_eq!(&packet[4..8], &[0; 4], "read-only ESP commands have a zero checksum");
        self.commands.push(packet[1]);
        match packet[1] {
            0x08 => {
                let mut sync = [0x55; 36];
                sync[..4].copy_from_slice(&[7, 7, 0x12, 0x20]);
                assert_eq!(&packet[8..], &sync, "ESP SYNC must use the ROM sync payload");
                for _ in 0..8 {
                    Self::reply(input, 0x08, 0);
                }
            }
            0x0a => {
                assert_eq!(&packet[8..], &0x40001000u32.to_le_bytes(), "ESP probe must read the family id register");
                // A late SYNC response exercises ignoring a different reply command.
                Self::reply(input, 0x08, 0);
                Self::reply(input, 0x0a, self.magic);
            }
            _ => panic!("ESP identification must send only SYNC and READ_REG"),
        }
    }

    fn reply(input: &mut VecDeque<u8>, command: u8, value: u32) {
        let mut packet = vec![1, command, 4, 0];
        packet.extend_from_slice(&value.to_le_bytes());
        packet.extend_from_slice(&[0; 4]);
        input.push_back(0xc0);
        for byte in packet {
            match byte {
                0xc0 => input.extend([0xdb, 0xdc]),
                0xdb => input.extend([0xdb, 0xdd]),
                _ => input.push_back(byte),
            }
        }
        input.push_back(0xc0);
    }
}

enum FakeBoard {
    Arduino(FakeArduino),
    Esp(FakeEsp),
}

struct FakeLine {
    now: Duration,
    write_latency: Duration,
    baud: u32,
    dtr: bool,
    rts: bool,
    input: VecDeque<u8>,
    board: FakeBoard,
}

impl FakeLine {
    fn new(board: FakeBoard) -> Self {
        let input = match &board {
            FakeBoard::Arduino(board) => board.app_output.clone().into(),
            FakeBoard::Esp(_) => b"firmware\r\n".iter().copied().collect(),
        };
        Self {
            now: Duration::ZERO,
            write_latency: Duration::ZERO,
            baud: 115200,
            dtr: false,
            rts: false,
            input,
            board,
        }
    }

    fn advance_to(&mut self, now: Duration) {
        if let FakeBoard::Arduino(board) = &mut self.board {
            board.advance(now, &mut self.input);
        }
        self.now = now;
    }

    fn arduino(&self) -> &FakeArduino {
        match &self.board {
            FakeBoard::Arduino(board) => board,
            _ => panic!("this scenario requires an Arduino"),
        }
    }

    fn arduino_mut(&mut self) -> &mut FakeArduino {
        match &mut self.board {
            FakeBoard::Arduino(board) => board,
            _ => panic!("this scenario requires an Arduino"),
        }
    }
}

impl Line for FakeLine {
    fn write_all(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.advance_to(self.now + self.write_latency
            + Duration::from_micros((bytes.len() as u64 * 10_000_000).div_ceil(self.baud as u64)));
        match &mut self.board {
            FakeBoard::Arduino(board) => board.write(self.now, self.baud, bytes, &mut self.input),
            FakeBoard::Esp(board) => board.write(self.baud, bytes, &mut self.input),
        }
        Ok(())
    }

    fn read_byte(&mut self, timeout: Duration) -> io::Result<Option<u8>> {
        let deadline = self.now + timeout;
        if self.input.is_empty() {
            let ready_at = match &self.board {
                FakeBoard::Arduino(board) => board.ready_at(),
                FakeBoard::Esp(_) => None,
            };
            if let Some(ready_at) = ready_at.filter(|&ready_at| ready_at <= deadline) {
                self.advance_to(ready_at);
            }
        }
        if let Some(byte) = self.input.pop_front() {
            self.advance_to(self.now + Duration::from_micros(10_000_000u64.div_ceil(self.baud as u64)));
            Ok(Some(byte))
        } else {
            self.advance_to(deadline);
            Ok(None)
        }
    }

    fn set_dtr(&mut self, on: bool) -> io::Result<()> {
        if on && !self.dtr {
            if let FakeBoard::Arduino(board) = &mut self.board { board.reset(self.now); }
        }
        self.dtr = on;
        Ok(())
    }

    fn set_rts(&mut self, on: bool) -> io::Result<()> {
        if let FakeBoard::Esp(board) = &mut self.board {
            board.rts(self.now, self.rts, on, self.dtr, &mut self.input);
        }
        self.rts = on;
        Ok(())
    }

    fn set_baud(&mut self, baud: u32) -> io::Result<()> { self.baud = baud; Ok(()) }
    fn drain_input(&mut self) -> io::Result<()> { self.input.clear(); Ok(()) }
    fn reopen(&mut self) -> io::Result<()> { Ok(()) }
    fn sleep(&mut self, d: Duration) { self.advance_to(self.now + d); }
}

#[test]
fn optiboot_delayed_uart_reads() {
    // optiboot 4.4 turns its UART on about 66 ms after reset and reads it after 375 ms of LED
    // flashes; the old 300 ms sync retries overflow the chip's three-byte receive buffer and fail
    // this model. A bootloader whose UART comes on after the first settle (start 200 ms) loses
    // the first sync and needs exactly one more, longer settled, reset.
    for (start_ms, latencies, expected_resets) in [(66, 0..=40, 1), (200, 0..=0, 2)] {
        for latency_ms in latencies.step_by(5) {
            let mut arduino = FakeArduino::new(115200, 4);
            arduino.timing = Some(BootloaderTiming {
                start: Duration::from_millis(start_ms),
                ready: Duration::from_millis(start_ms + 375),
                silent_reset: true,
            });
            let mut line = FakeLine::new(FakeBoard::Arduino(arduino));
            line.write_latency = Duration::from_millis(latency_ms);
            let board = identify(&mut line);
            let resets = line.arduino().resets;
            let case = format!("start {start_ms} ms, latency {latency_ms} ms, {resets} resets");
            let boot = match board {
                Board::Stk500(boot) => boot,
                other => panic!("optiboot must identify ({case}): {other}"),
            };
            assert_eq!(boot.signature, [0x1e, 0x95, 0x0f], "chip signature must survive delayed UART reads ({case})");
            assert_eq!(boot.baud, 115200, "optiboot must identify at its baud ({case})");
            assert_eq!(resets, expected_resets, "sync must reset only when its sync was lost ({case})");
        }
    }
}

#[test]
fn arduino_identify_flash_verify() {
    let mut line = FakeLine::new(FakeBoard::Arduino(FakeArduino::new(115200, 8)));
    let original = line.arduino().flash.clone();
    let want = [0x1e, 0x95, 0x0f];
    let boot = match identify(&mut line) {
        Board::Stk500(boot) => boot,
        board => panic!("optiboot must be identified despite queued application output: {board}"),
    };
    assert_eq!(boot.baud, 115200, "optiboot must be found at its baud");
    assert_eq!(boot.signature, want, "identification must report the chip signature");
    assert_eq!(boot.chip.unwrap().name, "ATmega328P", "signature must resolve to ATmega328P");
    assert_eq!(boot.app_bytes(), Some(32256), "optiboot must reserve 512 boot bytes");
    assert!(!line.arduino().commands.contains(&PROG_PAGE), "identification must never program a page");
    assert_eq!(line.arduino().flash, original, "identification must leave all flash unchanged");

    let mut state = 0x12345678u32;
    let image: Vec<u8> = (0..1000).map(|_| {
        state ^= state << 13;
        state ^= state >> 17;
        state ^= state << 5;
        state as u8
    }).collect();
    let pages = write_flash(&mut line, 115200, want, &image).unwrap();
    assert_eq!(pages, 8, "1000 bytes must use eight 128-byte pages");
    let mut padded = image.clone();
    padded.resize(1024, 0xff);
    assert_eq!(&line.arduino().flash[..1024], &padded, "written pages must contain the image and erased padding");
    assert_eq!(&line.arduino().flash[1024..], &original[1024..], "bytes outside programmed pages must be untouched");
    assert_eq!(line.arduino().commands.iter().filter(|&&cmd| cmd == READ_PAGE).count(), 8,
        "flashing must read back every programmed page");
    assert!(!line.arduino().programming, "successful flashing must leave programming mode");

    line.arduino_mut().commands.clear();
    let readback = stk500::read_flash(&mut line, 115200, 1003).unwrap();
    assert_eq!(readback, padded[..1003], "read_flash must return an exact, possibly odd byte count");
    assert!(!line.arduino().commands.contains(&PROG_PAGE), "reading flash must not program it");

    line.arduino_mut().commands.clear();
    let err = write_flash(&mut line, 115200, [0x1e, 0x95, 0x14], &image).unwrap_err();
    assert!(matches!(err, Error::WrongChip { want: [0x1e, 0x95, 0x14], got } if got == want),
        "a different expected signature must be refused as WrongChip");
    assert!(!line.arduino().commands.contains(&PROG_PAGE), "wrong-chip refusal must occur before programming");

    line.arduino_mut().commands.clear();
    let oversized = vec![0x5a; boot.app_bytes().unwrap() + 1];
    let err = write_flash(&mut line, 115200, want, &oversized).unwrap_err();
    assert!(matches!(err, Error::ImageTooLarge { bytes: 32257, max: 32256 }),
        "an image reaching the boot section must be refused as ImageTooLarge");
    assert!(!line.arduino().commands.contains(&PROG_PAGE), "oversize refusal must occur before programming");
    assert_eq!(&line.arduino().flash[..1024], &padded, "refusals must preserve the previous application");
    assert_eq!(&line.arduino().flash[1024..], &original[1024..], "refusals must preserve all remaining flash");

    let mut old = FakeLine::new(FakeBoard::Arduino(FakeArduino::new(57600, 1)));
    let boot = match identify(&mut old) {
        Board::Stk500(boot) => boot,
        board => panic!("the old bootloader must be found after trying the wrong baud: {board}"),
    };
    assert_eq!(boot.baud, 57600, "identification must try the second baud");
    assert_eq!(boot.app_bytes(), Some(30720), "old bootloaders must reserve 2048 boot bytes");
    assert!(!old.arduino().commands.contains(&PROG_PAGE), "old bootloader identification must also be read-only");
}

#[test]
fn esp_rom_probe() {
    // The raw id deliberately contains both bytes that SLIP must escape.
    let magic = 0xc0db506f;
    let mut line = FakeLine::new(FakeBoard::Esp(FakeEsp::new(magic)));
    match identify(&mut line) {
        Board::EspRom(rom) => assert_eq!(rom.magic, magic, "ESP probe must return the raw family id through SLIP escapes"),
        board => panic!("ESP ROM must be identified after both STK500 probes fail: {board}"),
    }
    let FakeBoard::Esp(board) = &line.board else { panic!("ESP scenario must keep its board"); };
    assert_eq!(board.commands, [0x08, 0x0a], "ESP probe must execute only SYNC and READ_REG");
    assert!(!board.download_mode, "ESP probe must leave the chip running its firmware");
    assert!(line.now >= Duration::from_secs(6), "failed STK500 attempts must consume virtual timeout time");
}

/// S3 ROM model from "Serial Protocol", "Low Level Protocol", "Command Packet",
/// "Status Bytes", "Checksum", "Writing Data" and the ROM command table.
/// Reset state follows ESP32-S3 TRM 33.3.2 Table 33.3-2.
struct FakeEspRom {
    origin: Instant,
    now: Duration,
    input: VecDeque<(Duration, u8)>,
    flash: Vec<u8>,
    controls: Vec<(char, bool)>,
    commands: Vec<u8>,
    md5_ranges: Vec<(u32, u32)>,
    checksum_matches: Vec<bool>,
    dtr: bool,
    rts: bool,
    download_flag: bool,
    download_mode: bool,
    connected: bool,
    drop_on_reset: bool,
    drop_after_reset: bool,
    late_drop_due: bool,
    not_reading: bool,
    reopened: usize,
    attached: bool,
    configured: bool,
    write_offset: usize,
    packet_count: u32,
    sequence: u32,
    corrupt_write: bool,
    inject_bad_checksum: bool,
    miss_sync: bool,
    /// Bytes the firmware sent before the download reset that reach the host only after it, with
    /// the first SYNC: the USB Serial/JTAG port stays through the reset.
    firmware_tail: Vec<u8>,
    md5_delay: Duration,
    security_size: usize,
    md5_reply: Option<Vec<u8>>,
    close_on_md5: bool,
    layout: Layout,
    spi1: [u32; 38],
    spi_polls: usize,
    spi_polls_left: usize,
    spi_pending: Option<[u32; 16]>,
    spi_offsets: Vec<u32>,
    spi_writes: Vec<(u32, u32)>,
    corrupt_spi_offset: Option<u32>,
    reject_spi_write: Option<u32>,
    wrong_spi_reply: Option<u32>,
    register_requests: usize,
    register_batches: Vec<usize>,
    /// Registers outside SPI1: GPIO_STRAP, GPIO_IN, IO_MUX_GPIO0 and RTC_CNTL_OPTION1.
    registers: Vec<(u32, u32)>,
    register_writes: Vec<(u32, u32)>,
    option1_stuck: bool,
    /// The chip id GET_SECURITY_INFO answers.
    chip_id: u32,
    /// When the armed RTC watchdog's count last started (its last feed), and the reset period
    /// (`life`) the last SYNC was answered in.
    watchdog_origin: Duration,
    synced_life: u128,
}

impl FakeEspRom {
    // Fixture values, deliberately containing SLIP escapes and nonzero payload
    // bytes where a parser that assumes status-first would fail.
    const CHIP_ID: u32 = 0xc0db_0009;

    fn new() -> Self {
        Self {
            origin: Instant::now(), now: Duration::ZERO, input: VecDeque::new(),
            flash: (0..16384).map(|i| (i * 37 + 11) as u8).collect(),
            controls: Vec::new(), commands: Vec::new(), md5_ranges: Vec::new(),
            checksum_matches: Vec::new(), dtr: false, rts: false,
            download_flag: false, download_mode: false, connected: true,
            drop_on_reset: false, drop_after_reset: false, late_drop_due: false, not_reading: false, reopened: 0, attached: false, configured: false,
            write_offset: 0, packet_count: 0, sequence: 0, corrupt_write: false,
            inject_bad_checksum: false, miss_sync: true, firmware_tail: Vec::new(),
            md5_delay: Duration::from_millis(10100), security_size: 20,
            md5_reply: None, close_on_md5: false,
            layout: Layout::Low, spi1: [0; 38], spi_polls: 0, spi_polls_left: 0,
            spi_pending: None, spi_offsets: Vec::new(), spi_writes: Vec::new(),
            corrupt_spi_offset: None, reject_spi_write: None, wrong_spi_reply: None,
            register_requests: 0, register_batches: Vec::new(),
            // Fixture values: the session-3 strap (boot:0x23), GPIO0 high with input enabled, and
            // FORCE_DOWNLOAD_BOOT set (TRM Registers 6.15, 6.16, 6.36 and 10.58).
            registers: vec![(0x60004038, 0x23), (0x6000403c, 0x0000_0001), (0x60009004, 0x0000_0b00), (0x6000812c, 1)],
            register_writes: Vec::new(), option1_stuck: false, chip_id: Self::CHIP_ID,
            watchdog_origin: Duration::ZERO, synced_life: 0,
        }
    }

    /// An S3 whose firmware left its RTC watchdog armed through the download reset, as the
    /// voicebox S3 did on 2026-11-18 (`rst:0x9 (RTCWDT_SYS_RST)` again and again in download
    /// mode). Fixture values: rtc_cntl_reg.h WDTCONFIG0 (enable 31, stage-0 action 30:28,
    /// flash-boot mode 12), WDTFEED, WDTWPROTECT, SWD_CONF and SWD_WPROTECT, both protects locked.
    fn s3_with_armed_watchdog() -> Self {
        let mut rom = Self::new();
        rom.chip_id = 9;
        rom.miss_sync = false;
        rom.registers.extend([(0x6000_80b0, 0), (0x6000_80ac, 0), (0x6000_8098, (1 << 31) | (3 << 28) | (1 << 12) | 0x0f), (0x6000_80b8, 0), (0x6000_80b4, 0x0000_0040)]);
        rom
    }

    /// About the RTC watchdog hold Fab's S3 boot stage arms (13602 RTC_SLOW cycles).
    const WATCHDOG_HOLD: Duration = Duration::from_millis(100);

    /// Which reset period of the armed watchdog it is now: the loader comes back after each.
    fn life(&self) -> u128 {
        self.now.saturating_sub(self.watchdog_origin).as_micros() / Self::WATCHDOG_HOLD.as_micros()
    }

    /// Whether the RTC watchdog is armed (the chip resets before a long command answers).
    fn watchdog_armed(&self) -> bool {
        self.registers.iter().any(|&(address, value)| address == 0x6000_8098 && value & (1 << 31) != 0)
    }

    fn live(&self) -> io::Result<()> {
        if self.connected { Ok(()) } else {
            Err(io::Error::new(io::ErrorKind::NotConnected, "fake USB port dropped"))
        }
    }

    fn control(&mut self, which: char, on: bool) -> io::Result<()> {
        self.live()?;
        self.controls.push((which, on));
        match which {
            'D' => self.dtr = on,
            'R' => self.rts = on,
            _ => unreachable!("fixture control must be DTR or RTS"),
        }
        match (self.rts, self.dtr) {
            (false, false) => self.download_flag = false,
            (false, true) => self.download_flag = true,
            (true, true) => {}
            (true, false) => {
                self.download_mode = self.download_flag;
                self.input.clear();
                self.attached = false;
                self.configured = false;
                self.packet_count = 0;
                self.sequence = 0;
                if self.drop_on_reset {
                    self.connected = false;
                    return self.live();
                }
                // The port leaves after the download sequence: its next data I/O fails.
                if self.drop_after_reset && self.download_flag { self.late_drop_due = true; }
            }
        }
        Ok(())
    }

    fn word(data: &[u8], at: usize) -> u32 {
        u32::from_le_bytes(data[at..at + 4].try_into().unwrap())
    }

    fn reply(&mut self, command: u8, data: &[u8], error: Option<u8>, delay: Duration) {
        self.reply_value(command, 0, data, error, delay);
    }

    fn reply_value(&mut self, command: u8, value: u32, data: &[u8], error: Option<u8>, delay: Duration) {
        let mut packet = vec![1, command];
        packet.extend_from_slice(&((data.len() + 4) as u16).to_le_bytes());
        packet.extend_from_slice(&value.to_le_bytes());
        packet.extend_from_slice(data);
        // Status is LAST; reserved bytes have no status meaning.
        packet.extend_from_slice(&[u8::from(error.is_some()), error.unwrap_or(0), 0xc0, 0xdb]);
        let at = self.now + delay;
        self.input.push_back((at, 0xc0));
        for byte in packet {
            match byte {
                0xc0 => self.input.extend([(at, 0xdb), (at, 0xdc)]),
                0xdb => self.input.extend([(at, 0xdb), (at, 0xdd)]),
                _ => self.input.push_back((at, byte)),
            }
        }
        self.input.push_back((at, 0xc0));
    }

    /// gpio_reg.h GPIO_STRAP_REG/GPIO_IN_REG, io_mux_reg.h IO_MUX_GPIO0_REG and rtc_cntl_reg.h
    /// RTC_CNTL_OPTION1_REG. Only OPTION1 may be written, and only with the full mask.
    fn plain_register(&mut self, command: u8, data: &[u8]) {
        let address = Self::word(data, 0);
        let at = self.registers.iter().position(|(known, _)| *known == address)
            .unwrap_or_else(|| panic!("register 0x{address:08x} is not one the fixture names"));
        if command == 0x09 {
            // RTC_CNTL_OPTION1, and on an S3 the RTC and super watchdogs' config and protect
            assert!([0x6000812c, 0x6000_80b0, 0x6000_80ac, 0x6000_8098, 0x6000_80b8, 0x6000_80b4].contains(&address),
                "registers written outside SPI1 are RTC_CNTL_OPTION1 and the S3's watchdogs, not 0x{address:08x}");
            assert!(address == 0x6000812c || self.chip_id == 9, "the watchdog addresses are written on an S3 only");
            assert_eq!(Self::word(data, 8), u32::MAX, "WRITE_REG uses a full mask");
            assert_eq!(Self::word(data, 12), 0, "WRITE_REG uses the lead's zero delay");
            let value = Self::word(data, 4);
            self.register_writes.push((address, value));
            // TRM: a watchdog's config takes a write only while its protect register holds the key
            let protect = |rom: &Self, at: u32| rom.registers.iter().find(|(known, _)| *known == at).map(|&(_, value)| value);
            let locked = match address {
                0x6000_8098 | 0x6000_80ac => protect(self, 0x6000_80b0) != Some(0x50d8_3aa1),
                0x6000_80b4 => protect(self, 0x6000_80b8) != Some(0x8f1d_312a),
                _ => false,
            };
            if !self.option1_stuck && !locked { self.registers[at].1 = value; }
            if address == 0x6000_80ac && value & (1 << 31) != 0 && !locked {
                // a feed: the count starts again now
                self.watchdog_origin = self.now;
                self.synced_life = 0;
            }
            self.reply(command, &[], None, Duration::ZERO);
        } else {
            self.reply_value(command, self.registers[at].1, &[], None, Duration::ZERO);
        }
    }

    fn spi_register(&mut self, command: u8, data: &[u8]) {
        // serial-protocol.txt, READ_REG/WRITE_REG command table; register map:
        // reg_base.h DR_REG_SPI1_BASE and spi_mem_reg.h CMD, USER*, MISO_DLEN,
        // ADDR and W0..W15. Fixture constants are independent of the reader.
        assert!(self.configured, "SPI1 register access follows attach and set params");
        assert_eq!(data.len(), if command == 0x09 { 16 } else { 4 },
            "register commands carry exactly the documented words");
        self.register_requests += 1;
        self.now += Duration::from_micros(10);
        let address = Self::word(data, 0);
        assert!((0x60002000..=0x60002094).contains(&address) && address % 4 == 0,
            "SPI1 accesses stay inside the documented register file");
        let index = ((address - 0x60002000) / 4) as usize;
        if command == 0x09 {
            let value = Self::word(data, 4);
            assert_eq!(Self::word(data, 8), u32::MAX, "WRITE_REG uses a full mask");
            assert_eq!(Self::word(data, 12), 0, "WRITE_REG uses the lead's zero delay");
            self.spi_writes.push((address, value));
            if self.reject_spi_write == Some(address) {
                self.reject_spi_write = None;
                self.reply(command, &[], Some(0x01), Duration::ZERO);
                return;
            }
            self.spi1[index] = value;
            if index == 0 {
                // W25Q128JV §8.2.6 and spi_mem_reg.h USR_*: exactly the
                // command, address and DIN phases of a 64-byte 03h read.
                let valid = value == 1 << 18
                    && self.spi1[0x18 / 4] & 0xf8000000 == 0xd0000000
                    && self.spi1[0x1c / 4] >> 26 == 23
                    && self.spi1[0x20 / 4] & 0xf000ffff == 0x70000003
                    && self.spi1[0x28 / 4] == 511;
                if !valid {
                    self.reply(command, &[], Some(0x01), Duration::ZERO);
                    return;
                }
                let addr = self.spi1[1];
                let addr = match self.layout { Layout::Low => addr, Layout::High => addr >> 8 };
                // Only A23..A0 reach flash; the fake then wraps at its own size.
                let offset = (addr & 0x00ffffff) % self.flash.len() as u32;
                self.spi_offsets.push(offset);
                let mut bytes = [0; 64];
                for (i, byte) in bytes.iter_mut().enumerate() {
                    *byte = self.flash[(offset as usize + i) % self.flash.len()];
                }
                if self.corrupt_spi_offset == Some(offset) {
                    bytes[0] ^= 1;
                    self.corrupt_spi_offset = None;
                }
                let mut values = [0; 16];
                for (value, chunk) in values.iter_mut().zip(bytes.chunks_exact(4)) {
                    *value = u32::from_le_bytes(chunk.try_into().unwrap());
                }
                self.spi_pending = Some(values);
                self.spi_polls_left = self.spi_polls;
            }
            self.reply(command, &[], None, Duration::ZERO);
        } else {
            if index == 0 && self.spi_pending.is_some() {
                if self.spi_polls_left == 0 {
                    self.spi1[0x58 / 4..=0x94 / 4].copy_from_slice(&self.spi_pending.take().unwrap());
                    self.spi1[0] &= !(1 << 18);
                } else {
                    self.spi_polls_left -= 1;
                }
            }
            let reply_command = if self.wrong_spi_reply == Some(address) {
                self.wrong_spi_reply = None;
                0x09
            } else { command };
            self.reply_value(reply_command, self.spi1[index], &[], None, Duration::ZERO);
        }
    }

    fn execute(&mut self, packet: &[u8]) {
        assert!(self.download_mode, "S3 commands require the table's download-mode reset");
        assert!(packet.len() >= 8, "S3 requests contain the complete eight-byte header");
        assert_eq!(packet[0], 0, "S3 request direction is host to ROM");
        assert_eq!(u16::from_le_bytes([packet[2], packet[3]]) as usize, packet.len() - 8,
            "S3 request Size counts decoded data bytes");
        let command = packet[1];
        self.commands.push(command);
        if self.watchdog_armed() {
            // the loader answers a SYNC whenever it is up; a request sent after the watchdog
            // reset the chip since that SYNC is lost (the port stays, the reply never comes)
            if command == 0x08 { self.synced_life = self.life(); } else if self.life() != self.synced_life { return; }
        }
        let data = &packet[8..];
        if command != 0x03 {
            assert_eq!(Self::word(packet, 4), 0, "non-DATA commands use a zero checksum");
        }
        let mut delay = Duration::ZERO;
        match command {
            0x09 | 0x0a if (0x60002000..0x60003000).contains(&Self::word(data, 0)) => { self.spi_register(command, data); return; }
            0x09 | 0x0a => { self.plain_register(command, data); return; }
            0x08 => {
                let mut expected = [0x55; 36];
                expected[..4].copy_from_slice(&[7, 7, 0x12, 0x20]);
                assert_eq!(data, expected, "S3 SYNC carries the documented 36 bytes");
                for byte in std::mem::take(&mut self.firmware_tail) { self.input.push_back((self.now, byte)); }
                if self.miss_sync { self.miss_sync = false; return; }
                for ms in [0, 30, 60] { self.reply(command, &[], None, Duration::from_millis(ms)); }
                return;
            }
            0x14 => {
                assert!(data.is_empty(), "GET_SECURITY_INFO has no request payload");
                let mut info = 0x20db_c007u32.to_le_bytes().to_vec();
                info.extend_from_slice(&[5, 1, 2, 3, 4, 5, 6, 7]);
                info.extend_from_slice(&self.chip_id.to_le_bytes());
                info.extend_from_slice(&2u32.to_le_bytes());
                info.resize(self.security_size, 0);
                // Also exercise ignoring an unrelated, late SYNC packet.
                self.reply(0x08, &[], None, Duration::ZERO);
                self.reply(command, &info, None, Duration::ZERO);
                return;
            }
            0x0d => {
                assert_eq!(data, &[0; 8], "SPI_ATTACH uses the two-word ROM form");
                self.attached = true;
            }
            0x0b => {
                assert!(self.attached, "SPI_SET_PARAMS follows SPI_ATTACH");
                assert_eq!(data.len(), 24, "SPI_SET_PARAMS has six words");
                let params: Vec<_> = (0..24).step_by(4).map(|at| Self::word(data, at)).collect();
                assert_eq!(params, [0, self.flash.len() as u32, 65536, 4096, 256, 0xffff],
                    "SPI_SET_PARAMS uses board geometry and the lead's id and mask");
                self.configured = true;
            }
            0x02 => {
                assert!(self.configured, "FLASH_BEGIN requires SPI configuration");
                assert_eq!(data.len(), 20, "FLASH_BEGIN has five ROM words");
                let erase = Self::word(data, 0) as usize;
                self.packet_count = Self::word(data, 4);
                assert_eq!(Self::word(data, 8), 1024, "FLASH_BEGIN declares the lead's packet size");
                self.write_offset = Self::word(data, 12) as usize;
                assert_eq!(Self::word(data, 16), 0, "FLASH_BEGIN selects unencrypted writing");
                assert_eq!(erase % 4096, 0, "FLASH_BEGIN erases whole sectors");
                assert_eq!(self.write_offset % 4096, 0, "FLASH_BEGIN offset is sector aligned");
                assert!(self.write_offset + erase <= self.flash.len(), "FLASH_BEGIN stays inside flash");
                self.flash[self.write_offset..self.write_offset + erase].fill(0xff);
                self.sequence = 0;
                // W25Q128JV §9.6 maxima plus transport latency within the lead's
                // 1 s allowance; this exceeds the ordinary 100 ms byte timeout.
                delay = Duration::from_millis((erase / 4096) as u64 * 400 + 900);
            }
            0x03 => {
                assert_eq!(data.len(), 16 + 1024, "FLASH_DATA includes a header and a full padded packet");
                assert_eq!(Self::word(data, 0), 1024, "FLASH_DATA length includes padding");
                assert_eq!(Self::word(data, 4), self.sequence, "FLASH_DATA sequence starts at zero per region");
                assert_eq!(&data[8..16], &[0; 8], "FLASH_DATA's unused words are zero");
                assert!(self.sequence < self.packet_count, "FLASH_DATA respects the declared packet count");
                let checksum = data[16..].iter().fold(0xefu8, |sum, byte| sum ^ byte);
                let matches = Self::word(packet, 4) == u32::from(checksum);
                self.checksum_matches.push(matches);
                if !matches { self.reply(command, &[], Some(0x07), Duration::ZERO); return; }
                let start = self.write_offset + self.sequence as usize * 1024;
                self.flash[start..start + 1024].copy_from_slice(&data[16..]);
                if self.corrupt_write { self.flash[start] ^= 1; self.corrupt_write = false; }
                self.sequence += 1;
                delay = Duration::from_millis(4 * 3 + 900);
            }
            0x13 => {
                assert!(self.configured, "SPI_FLASH_MD5 requires SPI configuration");
                assert_eq!(self.sequence, self.packet_count, "verification follows every declared FLASH_DATA packet");
                assert_eq!(data.len(), 16, "SPI_FLASH_MD5 has four words");
                assert_eq!(&data[8..], &[0; 8], "SPI_FLASH_MD5's final two words are zero");
                let offset = Self::word(data, 0);
                let size = Self::word(data, 4);
                self.md5_ranges.push((offset, size));
                // an armed RTC watchdog resets the chip before the hash answers: the port drops
                if self.close_on_md5 || self.watchdog_armed() { self.connected = false; return; }
                let digest = md5::digest(&self.flash[offset as usize..(offset + size) as usize]);
                let data = self.md5_reply.take().unwrap_or_else(|| md5::hex(&digest).into_bytes());
                self.reply(command, &data, None, self.md5_delay);
                return;
            }
            _ => panic!("S3 path must use only the brief's ROM commands, got 0x{command:02x}"),
        }
        self.reply(command, &[], None, delay);
    }
}

impl Line for FakeEspRom {
    fn write_all(&mut self, bytes: &[u8]) -> io::Result<()> {
        // TtyLine's answer when the device stops reading (WRITE_STALL).
        if self.not_reading { return Err(io::Error::new(io::ErrorKind::TimedOut, "took no data")); }
        if std::mem::take(&mut self.late_drop_due) { self.connected = false; }
        self.live()?;
        assert_eq!(bytes.first(), Some(&0xc0), "S3 request begins with SLIP END");
        assert_eq!(bytes.last(), Some(&0xc0), "S3 request ends with SLIP END");
        let mut packet = Vec::new();
        let mut escaped = false;
        for &byte in &bytes[1..bytes.len() - 1] {
            if escaped {
                packet.push(match byte {
                    0xdc => 0xc0,
                    0xdd => 0xdb,
                    _ => panic!("S3 request uses valid SLIP escapes"),
                });
                escaped = false;
            } else if byte == 0xdb {
                escaped = true;
            } else {
                assert_ne!(byte, 0xc0, "embedded END bytes must be escaped");
                packet.push(byte);
            }
        }
        assert!(!escaped, "S3 request cannot end in an incomplete escape");
        if self.inject_bad_checksum && packet[1] == 0x03 {
            packet[4] ^= 1;
            self.inject_bad_checksum = false;
        }
        self.execute(&packet);
        Ok(())
    }

    fn read_byte(&mut self, timeout: Duration) -> io::Result<Option<u8>> {
        self.live()?;
        if self.register_requests != 0 {
            self.register_batches.push(std::mem::take(&mut self.register_requests));
        }
        let deadline = self.now + timeout;
        if let Some(&(at, byte)) = self.input.front() {
            if at <= deadline {
                self.now = self.now.max(at);
                self.input.pop_front();
                return Ok(Some(byte));
            }
        }
        self.now = deadline;
        Ok(None)
    }

    fn set_dtr(&mut self, on: bool) -> io::Result<()> { self.control('D', on) }
    fn set_rts(&mut self, on: bool) -> io::Result<()> { self.control('R', on) }
    fn set_baud(&mut self, _baud: u32) -> io::Result<()> {
        panic!("USB Serial/JTAG path leaves the transport's line coding alone");
    }
    fn drain_input(&mut self) -> io::Result<()> {
        if std::mem::take(&mut self.late_drop_due) { self.connected = false; }
        self.live()?;
        while self.input.front().is_some_and(|&(at, _)| at <= self.now) { self.input.pop_front(); }
        Ok(())
    }
    fn reopen(&mut self) -> io::Result<()> {
        assert!(!self.connected, "reopen is exercised after a reset dropped the port");
        self.reopened += 1;
        self.now += Duration::from_millis(100);
        self.connected = true;
        // Model a returned USB port with released host control bits. The SoC's
        // already-selected boot mode survives the port's re-enumeration.
        self.dtr = false;
        self.rts = false;
        self.download_flag = false;
        Ok(())
    }
    fn sleep(&mut self, _duration: Duration) {
        panic!("TRM S3 reset sequences contain no sleeps");
    }
    fn now(&self) -> Instant { self.origin + self.now }
}

#[test]
fn esp_rom_s3_write_and_verify() {
    let mut line = FakeEspRom::new();
    let original = line.flash.clone();
    let download = [('D', false), ('R', false), ('D', true), ('R', false), ('R', true), ('D', false), ('R', true), ('R', false)];
    let boot = [('D', false), ('R', false), ('R', true), ('R', false)];
    let info = esp_rom::esp_info(&mut line, Entry::Reset, Exit::Boot).expect("read-only S3 info connects and boots");
    assert_eq!(line.controls, [download.as_slice(), boot.as_slice()].concat(),
        "download and boot reset step order matches TRM Tables 33.4-3 and 33.4-4");
    assert_eq!(info.connection.download, ResetReport::PortStayed, "download reports a port that stayed");
    assert_eq!(info.connection.boot, Some(ResetReport::PortStayed), "boot reports a port that stayed");
    assert_eq!(info.security.flags, 0x20db_c007, "security flags precede status and survive SLIP escaping");
    assert_eq!(info.security.flash_crypt_cnt, 5, "security crypt count is one byte");
    assert_eq!(info.security.key_purposes, [1, 2, 3, 4, 5, 6, 7], "security key purposes are exactly seven bytes");
    assert_eq!(info.security.chip_id, FakeEspRom::CHIP_ID, "security chip id is the fourth word");
    assert_eq!(info.security.eco_version, 2, "security ECO version is the fifth word");
    assert_eq!(line.commands, [0x08, 0x08, 0x14], "SYNC retries in virtual time and info uses no flash commands");
    assert!(!line.download_mode, "info finishes booting from flash");
    assert_eq!(line.flash, original, "info preserves the entire flash");

    line.controls.clear();
    let stayed = esp_rom::esp_info(&mut line, Entry::Reset, Exit::Stay).expect("read-only S3 info can stay in the loader");
    assert_eq!(line.controls, download, "staying sends the download reset and no boot reset");
    assert_eq!(stayed.connection.boot, None, "a stay reports no boot reset");
    assert!(line.download_mode, "a stay leaves the chip in the ROM loader");
    line.controls.clear();
    let again = esp_rom::esp_info(&mut line, Entry::NoReset, Exit::Stay).expect("a chip already in the loader connects without a reset");
    assert_eq!((line.controls.is_empty(), again.connection.download), (true, ResetReport::Skipped),
        "--no-reset sends no reset line change and reports the reset as not sent");
    assert_eq!(again.security, stayed.security, "the no-reset connection reads the same security info");
    assert!(line.download_mode, "a no-reset stay leaves the chip in the ROM loader");
    assert_eq!(esp_rom::esp_boot(&mut line).expect("the boot reset alone needs no connection"), ResetReport::PortStayed,
        "esp-boot reports the boot reset");
    assert_eq!(line.controls, boot, "esp-boot sends exactly the boot reset (Table 33.4-4)");
    assert!(!line.download_mode, "esp-boot leaves the ROM loader");

    line.drop_on_reset = true;
    line.controls.clear();
    line.commands.clear();
    let read = esp_rom::esp_md5(&mut line, Entry::Reset, 3, 149, original.len() as u32, Exit::Boot)
        .expect("read-only MD5 survives reset disconnects");
    assert_eq!(line.controls, [&download[..6], &boot[..3]].concat(),
        "a dropped port stops the remaining reset steps in both sequences");
    assert_eq!(read.connection.download, ResetReport::PortDropped { step: 6 },
        "connection report names the download reset drop step");
    assert_eq!(read.connection.boot, Some(ResetReport::PortDropped { step: 3 }),
        "connection report names the boot reset drop step");
    assert_eq!(line.reopened, 2, "both reset disconnects reopen the port");
    assert_eq!(read.region.md5, md5::digest(&original[3..152]), "read-only MD5 hashes the exact arbitrary byte range");
    assert_eq!(read.security, info.security, "MD5 also returns the full security info");
    assert!(!line.commands.contains(&0x02), "read-only MD5 never erases");
    assert_eq!(line.flash, original, "read-only MD5 preserves flash");

    line.drop_on_reset = false;
    line.drop_after_reset = true;
    line.reopened = 0;
    let late = esp_rom::esp_info(&mut line, Entry::Reset, Exit::Boot).expect("info survives a port that drops after the reset sequence");
    assert_eq!(late.connection.download, ResetReport::PortDroppedLater,
        "a port that leaves after every download step is reported as a later drop");
    assert_eq!(line.reopened, 1, "a later drop reopens the port once");
    assert_eq!(late.security, info.security, "info after a later drop reads the same security info");
    line.drop_after_reset = false;
    line.not_reading = true;
    line.reopened = 0;
    assert!(matches!(esp_rom::esp_info(&mut line, Entry::Reset, Exit::Boot), Err(esp_rom::Error::NotLoader)),
        "a device that takes no data after the download reset is reported as not in the ROM loader");
    assert_eq!(line.reopened, 0, "a stalled write is not a dropped port");
    line.not_reading = false;
    line.drop_on_reset = true;

    let mut plan = EspWrite {
        regions: vec![
            EspRegion { offset: 0, bytes: (0..1301).map(|i| (i * 19 + 3) as u8).collect() },
            EspRegion { offset: 8192, bytes: (0..270).map(|i| (i * 13 + 7) as u8).collect() },
        ],
        flash_size: original.len() as u32,
        expect_chip_id: FakeEspRom::CHIP_ID + 1,
        grant: Grant { device: "fake S3".into(), said_by: "test operator".into() },
        waiver: Waiver { reason: "virtual flash fixture has its original bytes in memory".into() },
    };
    line.commands.clear();
    let err = esp_rom::esp_write(&mut line, &plan, &mut |_| {}).expect_err("wrong expected chip id is refused");
    assert!(matches!(err, esp_rom::Error::WrongChip { want, got }
        if want == FakeEspRom::CHIP_ID + 1 && got == FakeEspRom::CHIP_ID),
        "chip-id refusal carries both expected and actual ids");
    assert!(err.to_string().contains("c0db000a") && err.to_string().contains("c0db0009"),
        "chip-id error prints both ids");
    assert!(!line.commands.contains(&0x02), "wrong chip is refused before any FLASH_BEGIN");
    plan.expect_chip_id = FakeEspRom::CHIP_ID;
    for (offset, property) in [(1, "aligned"), (0, "overlap"), (16384, "past flash")] {
        line.commands.clear();
        plan.regions[1].offset = offset;
        let err = esp_rom::esp_write(&mut line, &plan, &mut |_| {}).expect_err("invalid region is refused");
        assert!(err.to_string().contains(property), "invalid region reports its {property} property");
        assert!(!line.commands.contains(&0x02), "{property} refusal occurs before erase");
        assert_eq!(line.flash, original, "{property} refusal preserves flash");
    }
    plan.regions[1].offset = 8192;

    line.commands.clear();
    line.controls.clear();
    line.md5_ranges.clear();
    let before_time = line.now;
    let mut steps = Vec::new();
    let report = esp_rom::esp_write(&mut line, &plan, &mut |step| steps.push(step)).expect("two-region S3 write verifies");
    {
        use esp_rom::WriteStep::*;
        assert_eq!(steps, [Connected, Fingerprinted,
            Writing { index: 0, of: 2, done: 1024, size: 1301 }, Writing { index: 0, of: 2, done: 1301, size: 1301 }, Verified { index: 0 },
            Writing { index: 1, of: 2, done: 270, size: 270 }, Verified { index: 1 }, Booting],
            "the watcher hears each step in order, each packet as its reply comes, and each region's verify");
        assert!(report.unchanged.is_empty(), "regions the chip did not hold are all written");
    }
    assert_eq!(report.before, md5::digest(&original), "before fingerprint hashes the flash before any erase");
    assert_eq!(report.security, info.security, "write report carries security info");
    assert_eq!(report.connection.download, ResetReport::PortDropped { step: 6 }, "write reports the download drop");
    assert_eq!(report.connection.boot, Some(ResetReport::PortDropped { step: 3 }), "write reports the boot drop");
    assert_eq!(line.controls, [&download[..6], &boot[..3]].concat(), "write uses both documented reset prefixes on drop");
    assert_eq!(line.commands, [0x08, 0x14, 0x0d, 0x0b, 0x13, 0x13, 0x02, 0x03, 0x03, 0x13, 0x13, 0x02, 0x03, 0x13],
        "write orders security, attach, params, before MD5, then per region its MD5, write and verify, without FLASH_END");
    assert_eq!(line.md5_ranges, [(0, plan.flash_size), (0, 1301), (0, 1301), (8192, 270), (8192, 270)],
        "before MD5 covers the whole flash; each region's check and verify exclude padding");
    assert_eq!(line.checksum_matches, [true, true, true], "every FLASH_DATA checksum matches its data-only XOR");
    assert!(line.now - before_time >= line.md5_delay * 3, "MD5 waits remain alive beyond the connect window in virtual time");
    let mut expected_flash = original.clone();
    assert_eq!(report.regions.len(), plan.regions.len(), "report has an MD5 for every region");
    for (region, verified) in plan.regions.iter().zip(&report.regions) {
        let start = region.offset as usize;
        let end = start + region.bytes.len();
        expected_flash[start..start + 4096].fill(0xff);
        expected_flash[start..end].copy_from_slice(&region.bytes);
        assert_eq!(&line.flash[start..end], &region.bytes, "region 0x{start:08x} lands exact bytes");
        assert!(line.flash[end..start + region.bytes.len().div_ceil(1024) * 1024].iter().all(|&byte| byte == 0xff),
            "region 0x{start:08x} pads only after the data");
        assert_eq!(verified.offset, region.offset, "verify names its region offset");
        assert_eq!(verified.size, region.bytes.len() as u32, "verify reports the unpadded size");
        assert_eq!(verified.md5, md5::digest(&region.bytes), "region verify equals the host MD5");
    }
    assert_eq!(line.flash, expected_flash, "write preserves bytes outside erased sectors and fills erased tails with FF");
    assert!(!line.download_mode, "successful write boots from flash");

    // The same image again: the chip holds every region already, so nothing is erased or written.
    line.commands.clear();
    let mut steps = Vec::new();
    let again = esp_rom::esp_write(&mut line, &plan, &mut |step| steps.push(step)).expect("an unchanged image verifies");
    {
        use esp_rom::WriteStep::*;
        assert_eq!(steps, [Connected, Fingerprinted, Unchanged { index: 0 }, Unchanged { index: 1 }, Booting],
            "each region the chip holds is said unchanged");
    }
    assert!(!line.commands.contains(&0x02), "an unchanged region is never erased");
    assert_eq!((again.unchanged, again.regions), (vec![0, 8192], report.regions),
        "the report names the unchanged regions, with the same MD5s as written");
    assert_eq!(line.flash, expected_flash, "an unchanged write leaves the flash as it was");

    plan.regions.remove(0);
    for byte in &mut plan.regions[0].bytes { *byte ^= 0x5a; }
    line.corrupt_write = true;
    let err = esp_rom::esp_write(&mut line, &plan, &mut |_| {}).expect_err("a corrupted byte fails MD5 verification");
    assert!(matches!(err, esp_rom::Error::Verify { offset: 8192, size: 270, .. }),
        "verify failure carries the corrupted region and its exact size");
    assert!(err.to_string().contains("region 0x00002000"), "verify error names the corrupted region");

    line.inject_bad_checksum = true;
    let err = esp_rom::esp_write(&mut line, &plan, &mut |_| {}).expect_err("an injected bad checksum is rejected by the ROM");
    assert!(matches!(err, esp_rom::Error::Rom { code: 0x07, meaning: "Checksum error" }),
        "ROM status 1 error 07 surfaces the document's Checksum error");
    assert!(err.to_string().contains("Checksum error"), "ROM error display includes its meaning");
    assert_eq!(line.checksum_matches.last(), Some(&false), "checksum fault reached the fake ROM's checksum check");

    for size in [19, 21] {
        line.security_size = size;
        let err = esp_rom::esp_info(&mut line, Entry::Reset, Exit::Boot).expect_err("security payload must have exactly 20 bytes");
        assert!(matches!(err, esp_rom::Error::Protocol(_)), "wrong security length is a protocol error");
    }
    line.security_size = 20;
    for invalid in [vec![b'0'; 16], vec![b'g'; 32], vec![b'0'; 33]] {
        line.md5_reply = Some(invalid);
        let err = esp_rom::esp_md5(&mut line, Entry::Reset, 0, 16, plan.flash_size, Exit::Boot).expect_err("MD5 payload must be exactly 32 hex ASCII bytes");
        assert!(matches!(err, esp_rom::Error::Protocol(_)), "malformed MD5 is a protocol error");
    }
    line.close_on_md5 = true;
    let err = esp_rom::esp_md5(&mut line, Entry::Reset, 0, 16, plan.flash_size, Exit::Boot).expect_err("unbounded MD5 wait ends when the line dies");
    assert!(matches!(err, esp_rom::Error::Io(_)), "MD5 propagates a dead line as I/O failure");
}

/// The fake board behind a node's `serve`: the session's port is this one chip, and a reopen
/// finds it again after the reset dropped its USB port.
#[cfg(unix)]
struct Shared(std::sync::Arc<std::sync::Mutex<FakeEspRom>>);

#[cfg(unix)]
impl Line for Shared {
    fn write_all(&mut self, bytes: &[u8]) -> io::Result<()> { self.0.lock().unwrap().write_all(bytes) }
    fn read_byte(&mut self, timeout: Duration) -> io::Result<Option<u8>> { self.0.lock().unwrap().read_byte(timeout) }
    fn set_dtr(&mut self, on: bool) -> io::Result<()> { self.0.lock().unwrap().set_dtr(on) }
    fn set_rts(&mut self, on: bool) -> io::Result<()> { self.0.lock().unwrap().set_rts(on) }
    fn set_baud(&mut self, baud: u32) -> io::Result<()> { self.0.lock().unwrap().set_baud(baud) }
    fn drain_input(&mut self) -> io::Result<()> { self.0.lock().unwrap().drain_input() }
    fn reopen(&mut self) -> io::Result<()> { unreachable!("the node reopens through its opener") }
    fn sleep(&mut self, d: Duration) { self.0.lock().unwrap().sleep(d) }
    fn now(&self) -> Instant { self.0.lock().unwrap().now() }
}

#[cfg(unix)]
impl makepad_serial::remote::Opener for Shared {
    fn open(&mut self, _: makepad_serial::remote::Mode, _: u32) -> io::Result<Box<dyn Line + Send>> {
        Ok(Box::new(Shared(self.0.clone())))
    }
    fn reopen(&mut self, _: makepad_serial::remote::Mode, _: u32, _: Duration) -> io::Result<Box<dyn Line + Send>> {
        self.0.lock().unwrap().reopen()?;
        Ok(Box::new(Shared(self.0.clone())))
    }
}

/// The S3 write through a remote line (a node's `serve` over a socket) reaches the chip exactly
/// as the direct write does: the same reset steps and port drops, commands, flash bytes and
/// report. The fake's clock is its own, so only what reaches the chip is compared.
#[cfg(unix)]
#[test]
fn esp_rom_s3_write_through_a_remote_line_matches_the_direct_write() {
    use makepad_serial::remote::{serve, Mode, RemoteLine};
    let fresh = || { let mut line = FakeEspRom::new(); line.drop_on_reset = true; line };
    let mut direct = fresh();
    let plan = EspWrite {
        regions: vec![
            EspRegion { offset: 0, bytes: (0..1301).map(|i| (i * 19 + 3) as u8).collect() },
            EspRegion { offset: 8192, bytes: (0..270).map(|i| (i * 13 + 7) as u8).collect() },
        ],
        flash_size: direct.flash.len() as u32,
        expect_chip_id: FakeEspRom::CHIP_ID,
        grant: Grant { device: "fake S3".into(), said_by: "test operator".into() },
        waiver: Waiver { reason: "virtual flash fixture has its original bytes in memory".into() },
    };
    let mut steps = Vec::new();
    let report = esp_rom::esp_write(&mut direct, &plan, &mut |step| steps.push(step)).expect("the direct write verifies");

    let chip = std::sync::Arc::new(std::sync::Mutex::new(fresh()));
    let (client, node) = std::os::unix::net::UnixStream::pair().expect("a socket pair");
    let mut opener = Shared(chip.clone());
    let serving = std::thread::spawn(move || serve(node.try_clone()?, node, &mut opener));
    let mut line = RemoteLine::open(client, "the bench PC", Mode::Serial, 115_200).expect("the session opens");
    let mut remote_steps = Vec::new();
    let remote = esp_rom::esp_write(&mut line, &plan, &mut |step| remote_steps.push(step)).expect("the remote write verifies");
    drop(line);
    serving.join().expect("the node thread ends").expect("the session ends cleanly");

    let chip = chip.lock().unwrap();
    assert_eq!(chip.flash, direct.flash, "the remote write leaves the same flash bytes");
    assert_eq!(chip.commands, direct.commands, "the ROM sees the same commands in the same order");
    assert_eq!(chip.controls, direct.controls, "the reset steps reach DTR and RTS alike, the port drops included");
    assert_eq!((chip.reopened, chip.md5_ranges.clone()), (direct.reopened, direct.md5_ranges.clone()),
        "the port is found again as often, and the same ranges are fingerprinted");
    assert_eq!((remote.before, remote.regions, remote.connection, remote.security), (report.before, report.regions, report.connection, report.security),
        "the report is the direct write's");
    assert_eq!(remote_steps, steps, "the watcher hears the same steps");
}

#[test]
fn esp_rom_s3_stops_the_watchdogs_its_firmware_armed_before_the_flash_commands() {
    let mut line = FakeEspRom::s3_with_armed_watchdog();
    let size = line.flash.len() as u32;
    let read = esp_rom::esp_md5(&mut line, Entry::Reset, 0, size, size, Exit::Boot)
        .expect("the MD5 answers once the watchdogs are stopped");
    assert_eq!(read.region.md5, md5::digest(&line.flash), "the hash is the whole flash's");
    assert_eq!(line.register_writes, [
        (0x6000_80b0, 0x50d8_3aa1), (0x6000_80ac, 1 << 31), (0x6000_80b8, 0x8f1d_312a),
        (0x6000_8098, (3 << 28) | 0x0f), (0x6000_80b0, 0), (0x6000_80b4, 0x0000_0040 | (1 << 30)),
    ], "unlock and feed the RTC watchdog and unlock the super one at once, then clear enable and \
        flash-boot mode keeping the rest, relock, and disable the super watchdog");
    assert!(!line.watchdog_armed(), "the RTC watchdog is stopped");
    assert_eq!(line.flash, (0..16384).map(|i| (i * 37 + 11) as u8).collect::<Vec<u8>>(), "no flash is written");

    let mut other = FakeEspRom::new();
    esp_rom::esp_md5(&mut other, Entry::Reset, 0, 16, size, Exit::Boot).expect("another chip's MD5 answers");
    assert!(other.register_writes.is_empty(), "a chip that is not an S3 gets no write to the S3's watchdog addresses");
}

#[test]
fn esp_rom_s3_syncs_past_the_firmware_output_that_reaches_the_host_after_the_reset() {
    // A board running a program that reports over the watch link (11-18): its last frames came in
    // after the download reset, ahead of the SYNC reply, and the sync stopped at the first of them.
    // Fixture bytes in the watch link's shape (zero-terminated), holding a SLIP END that opens a
    // short frame and an END followed by a bad escape.
    let mut line = FakeEspRom::new();
    line.miss_sync = false;
    line.firmware_tail = vec![0x07, 0x06, 0x02, 0xc0, 0x31, 0x10, 0x00, 0x05, 0xc0, 0x04, 0xc0, 0xdb, 0x42, 0x00];
    let size = line.flash.len() as u32;
    let read = esp_rom::esp_md5(&mut line, Entry::Reset, 0, 16, size, Exit::Boot)
        .expect("the sync passes the firmware's last frames");
    assert_eq!(read.region.md5, md5::digest(&line.flash[..16]), "the MD5 is the region's");
    assert_eq!(line.commands[..2], [0x08, 0x14], "the first SYNC's reply is taken, past the frames ahead of it");
}

#[test]
fn esp_rom_s3_registers() {
    let mut line = FakeEspRom::new();
    line.download_mode = true;
    let named = [0x6000403c, 0x60004038, 0x60009004, 0x6000812c];
    let read = esp_rom::esp_read_reg(&mut line, Entry::NoReset, &named).expect("a chip in the loader reads named registers");
    assert_eq!(read.values, [1, 0x23, 0x0b00, 1], "values come back in the order asked");
    assert_eq!((line.controls.is_empty(), read.download), (true, ResetReport::Skipped), "--no-reset sends no line change");
    assert_eq!(read.security.chip_id, FakeEspRom::CHIP_ID, "the read names the chip");
    assert_eq!(line.commands, [0x08, 0x08, 0x14, 0x0a, 0x0a, 0x0a, 0x0a], "reads use only SYNC, GET_SECURITY_INFO and READ_REG");
    assert!(line.register_writes.is_empty() && line.download_mode, "reads write nothing and stay in the loader");
    for bad in [&[0x6000403e][..], &[]] {
        line.commands.clear();
        let err = esp_rom::esp_read_reg(&mut line, Entry::NoReset, bad).expect_err("unaligned or empty register lists are refused");
        assert!(matches!(err, esp_rom::Error::Plan(_)) && line.commands.is_empty(), "a refused list sends nothing");
    }

    let err = esp_rom::esp_clear_force_download(&mut line, Entry::NoReset, 9).expect_err("another chip id is refused");
    assert!(matches!(err, esp_rom::Error::WrongChip { want: 9, got: FakeEspRom::CHIP_ID }), "the refusal names both ids");
    assert!(line.register_writes.is_empty(), "a wrong chip gets no write");
    let cleared = esp_rom::esp_clear_force_download(&mut line, Entry::NoReset, FakeEspRom::CHIP_ID).expect("the bit clears");
    assert_eq!((cleared.before, cleared.after), (1, 0), "the report shows the bit before and after");
    assert_eq!(line.register_writes, [(0x6000812c, 0)], "one full-mask write clears bit 0 and keeps the reserved bits");
    assert!(line.controls.is_empty() && line.download_mode, "the clear sends no reset and stays in the loader");
    let again = esp_rom::esp_clear_force_download(&mut line, Entry::NoReset, FakeEspRom::CHIP_ID).expect("a clear bit is fine");
    assert_eq!((again.before, again.after, line.register_writes.len()), (0, 0, 1), "a clear bit is not written again");

    line.registers[3].1 = 1;
    line.option1_stuck = true;
    let err = esp_rom::esp_clear_force_download(&mut line, Entry::NoReset, FakeEspRom::CHIP_ID).expect_err("a bit that stays set fails");
    assert!(matches!(err, esp_rom::Error::Register { address: 0x6000812c, wrote: 0, got: 1 }), "the failure names the read-back");
}

#[test]
fn esp_rom_s3_spi1_backup() {
    let mut line = FakeEspRom::new();
    let flash_size = 256 * 1024;
    line.flash = (0..flash_size).map(|i: u32| (i * 37 ^ (i >> 8) * 71 ^ (i >> 16) * 113) as u8).collect();
    let original = line.flash.clone();
    line.md5_delay = Duration::ZERO;
    // Non-default saved fields make restoration and preservation observable.
    // spi_mem_reg.h: CTRL, CLOCK, USER, USER1, USER2, MISO_DLEN, ADDR,
    // CACHE_FCTRL. FDOUT_QUAD is harmless with the MOSI phase disabled.
    for (offset, value) in [
        (0x08, 0x00240000), (0x14, 0x00030103), (0x18, 0xa8000040),
        (0x1c, 0x7c000005), (0x20, 0x90ab00db), (0x28, 32),
        (0x04, 0x00db00c0), (0x3c, 1 << 7),
    ] { line.spi1[offset / 4] = value; }
    let saved = line.spi1;
    let restored = |line: &FakeEspRom| {
        for offset in [0x18, 0x1c, 0x20, 0x28, 0x04, 0x08, 0x14, 0x3c] {
            assert_eq!(line.spi1[offset / 4], saved[offset / 4],
                "SPI1 register at offset 0x{offset:02x} retains its saved value after reading");
        }
        assert_eq!(&line.spi_writes[line.spi_writes.len() - 5..], &[
            (0x60002004, saved[1]), (0x60002028, saved[0x28 / 4]),
            (0x60002020, saved[0x20 / 4]), (0x6000201c, saved[0x1c / 4]),
            (0x60002018, saved[0x18 / 4]),
        ], "SPI1 restoration follows reverse write order");
    };
    let download = [('D', false), ('R', false), ('D', true), ('R', false), ('R', true), ('D', false), ('R', true), ('R', false)];
    for layout in [Layout::Low, Layout::High] {
        line.layout = layout;
        // First probe exercises busy polling and discarded stale W values;
        // second exercises the nineteen-reply fast path.
        line.spi_polls = if layout == Layout::Low { 2 } else { 0 };
        line.controls.clear();
        line.md5_ranges.clear();
        let mut output = Vec::new();
        let selected = esp_rom::esp_spi1_probe(&mut line, Entry::Reset, flash_size, &mut output)
            .expect("nonzero-offset SPI1 probe succeeds for each fake layout");
        let output = String::from_utf8(output).expect("probe output is UTF-8 text");
        assert_eq!(selected, layout, "probe selects the fake's {layout} address layout");
        for offset in [0x1000, 0x8000, 0x10000, 0x20000] {
            let digest = md5::hex(&md5::digest(&original[offset..offset + 64]));
            assert!(output.contains(&format!("0x{offset:08x} {layout}: MD5 {digest} match")),
                "probe reports match with the ROM's digest for {layout} at 0x{offset:08x}");
        }
        let other = if layout == Layout::Low { Layout::High } else { Layout::Low };
        assert!(output.lines().any(|row| row.contains(&format!(" {other}:")) && row.ends_with("differs")),
            "probe reports differs for the other layout at least once");
        assert!(output.ends_with(&format!("layout: {layout}\n")), "probe ends with its unambiguous layout");
        assert_eq!(output.matches("SPI1 saved:").count(), 1, "probe reports saved registers once");
        for name in ["CTRL=", "CLOCK=", "USER=", "USER1=", "USER2=", "MISO_DLEN=", "ADDR="] {
            assert!(output.contains(name), "probe reports saved {name}");
        }
        assert_eq!(line.md5_ranges, [(0x1000, 64), (0x8000, 64), (0x10000, 64), (0x20000, 64)],
            "probe asks the ROM about each nonzero 64-byte region once");
        assert_eq!(line.controls, download, "probe sends no boot reset for either layout");
        restored(&line);
    }
    assert!(line.register_batches.contains(&19), "SPI1 read sends nineteen requests before reading replies");
    assert!(line.register_batches.contains(&16), "busy CMD causes fresh W0..W15 reads after polling");
    assert!(line.spi_writes.contains(&(0x60002018, 0xd0000040)), "USER keeps unrelated bits while selecting read phases");
    assert!(line.spi_writes.contains(&(0x6000201c, 0x5c000005)), "USER1 keeps unrelated bits with a 24-bit address");
    assert!(line.spi_writes.contains(&(0x60002020, 0x70ab0003)), "USER2 keeps unrelated bits with an 8-bit 03h command");

    let unique = SystemTime::now().duration_since(UNIX_EPOCH).expect("temporary directory timestamp is available").as_nanos();
    let directory = std::env::temp_dir().join(format!("esp-spi1-backup-{}-{unique}", std::process::id()));
    fs::create_dir(&directory).expect("unique SPI1 test directory is created");
    let good = directory.join("good");
    line.controls.clear();
    line.md5_ranges.clear();
    line.spi_offsets.clear();
    esp_rom::esp_read(&mut line, Entry::Reset, flash_size, Layout::High, &good).expect("whole-flash SPI1 backup verifies");
    assert_eq!(fs::read(good.join("flash.bin")).expect("verified backup file exists"), original,
        "flash.bin equals every byte of the fake's flash");
    let digest = sha256::sha256_hex(&original);
    assert_eq!(fs::read_to_string(good.join("flash.sha256")).expect("SHA-256 sidecar exists"), format!("{digest}  flash.bin\n"),
        "flash.sha256 matches makepad-core-util's digest and checksum-file format");
    assert!(!good.join("flash.bin.partial").exists(), "successful backup renames the partial file");
    let report = fs::read_to_string(good.join("report.txt")).expect("backup report exists");
    assert!(report.contains("layout: high"), "backup report names its address layout");
    assert!(report.contains(&format!("ROM MD5: {}", md5::hex(&md5::digest(&original)))),
        "backup report records the ROM's whole-flash MD5");
    assert!(report.contains(&format!("sha256: {digest}")), "backup report records the host SHA-256");
    for field in ["chip_id=0xc0db0009", "flags=0x20dbc007", "flash_crypt_cnt=5",
        "key_purposes=[1, 2, 3, 4, 5, 6, 7]", "eco_version=2", "flash_size: 262144",
        "SPI1 saved:", "bytes: 262144", "wall_time_seconds:", "rate_KB_per_second:"] {
        assert!(report.contains(field), "backup report contains {field}");
    }
    for (block, bytes) in original.chunks(65536).enumerate() {
        assert!(report.contains(&format!("block {block} at 0x{:08x} (65536 bytes) MD5 {}", block * 65536, md5::hex(&md5::digest(bytes)))),
            "backup report names block {block} and its verified MD5");
    }
    assert!(!report.contains(&directory.to_string_lossy().to_string()), "backup report contains no output path");
    assert_eq!(line.md5_ranges, [(0, flash_size), (0, 65536), (65536, 65536), (131072, 65536), (196608, 65536)],
        "backup asks for the whole ROM MD5 followed by every block MD5");
    assert_eq!(line.spi_offsets.len(), original.len() / 64, "clean backup reads every 64-byte chunk once");
    assert_eq!(line.controls, download, "backup sends no boot reset");
    assert!(line.download_mode, "backup stays in the ROM loader");
    restored(&line);
    let commands = line.commands.len();
    let err = esp_rom::esp_read(&mut line, Entry::Reset, flash_size, Layout::High, &good).expect_err("existing backup is refused");
    assert!(err.to_string().contains("already holds flash.bin"), "existing-backup refusal explains the collision");
    assert_eq!(line.commands.len(), commands, "existing backup is refused before chip access");
    assert_eq!(fs::read(good.join("flash.bin")).expect("original backup remains readable"), original,
        "existing-backup refusal preserves the verified file");

    let transient = directory.join("transient");
    line.corrupt_spi_offset = Some(65536 + 128);
    line.spi_offsets.clear();
    esp_rom::esp_read(&mut line, Entry::Reset, flash_size, Layout::High, &transient).expect("transient read corruption is retried successfully");
    assert_eq!(fs::read(transient.join("flash.bin")).expect("retried backup exists"), original,
        "retried backup still equals the original flash");
    assert_eq!(line.spi_offsets.iter().filter(|&&offset| offset == 65536).count(), 2,
        "one corrupted read causes the affected block to be read twice");
    assert_eq!(line.spi_offsets.iter().filter(|&&offset| offset == 0).count(), 1,
        "transient retry keeps previously verified blocks");
    restored(&line);

    let wrong = directory.join("wrong");
    line.spi_offsets.clear();
    let err = esp_rom::esp_read(&mut line, Entry::Reset, flash_size, Layout::Low, &wrong).expect_err("wrong address layout is systematic");
    assert!(matches!(err, esp_rom::Error::BackupBlock { block: 0, offset: 0, .. }),
        "wrong-layout failure identifies block 0");
    assert!(err.to_string().contains("ROM MD5") && err.to_string().contains("read MD5") && err.to_string().contains("systematic"),
        "systematic failure names the expected and actual MD5s");
    assert_eq!(line.spi_offsets.len(), 2 * 65536 / 64, "two identical failed reads stop without an arbitrary retry limit");
    assert!(!wrong.exists(), "systematic failure writes neither flash.bin nor any other backup file");
    restored(&line);

    // A command mismatch must not consume a later same-command reply as if it
    // were the missing position; all outstanding replies precede restoration.
    line.wrong_spi_reply = Some(0x60002058);
    let mut output = Vec::new();
    let err = esp_rom::esp_spi1_probe(&mut line, Entry::Reset, flash_size, &mut output).expect_err("out-of-order batch reply is refused");
    assert!(err.to_string().contains("out of order"), "batch command mismatch is reported");
    assert!(line.input.is_empty(), "failed batch consumes its remaining replies before restoring");
    restored(&line);
    line.reject_spi_write = Some(0x60002020);
    let err = esp_rom::esp_spi1_probe(&mut line, Entry::Reset, flash_size, &mut output).expect_err("failed begin is refused");
    assert!(matches!(err, esp_rom::Error::Rom { code: 0x01, .. }), "begin propagates ROM status failure");
    for offset in [0x18, 0x1c, 0x20, 0x28, 0x04] {
        assert_eq!(line.spi1[offset / 4], saved[offset / 4], "failed begin restores configuration at 0x{offset:02x}");
    }

    line.spi1[0x08 / 4] |= 1 << 8; // spi_mem_reg.h FCMD_QUAD in CTRL.
    line.spi_writes.clear();
    let err = esp_rom::esp_spi1_probe(&mut line, Entry::Reset, flash_size, &mut output).expect_err("quad command mode is refused");
    assert!(matches!(err, esp_rom::Error::Spi1(_)) && err.to_string().contains("Standard SPI"),
        "quad line-mode refusal explains the Standard SPI requirement");
    assert!(line.spi_writes.is_empty(), "quad mode is refused before any SPI1 write");
    line.spi1[0x08 / 4] = saved[0x08 / 4];
    line.spi1[0x3c / 4] |= 1 << 6; // FDIN_QUAD is in CACHE_FCTRL, not CTRL.
    let err = esp_rom::esp_spi1_probe(&mut line, Entry::Reset, flash_size, &mut output).expect_err("quad DIN mode is refused");
    assert!(matches!(err, esp_rom::Error::Spi1(_)), "CACHE_FCTRL line modes are also checked");
    assert!(line.spi_writes.is_empty(), "quad DIN mode is refused before any SPI1 write");
    assert!(line.commands.iter().all(|command| matches!(command, 0x08 | 0x09 | 0x0a | 0x0b | 0x0d | 0x13 | 0x14)),
        "backup and probe never issue FLASH_* erase/write commands or stub commands");
    assert!(line.controls.chunks_exact(download.len()).all(|controls| controls == download)
        && line.controls.len() % download.len() == 0, "success and error paths send only download resets");
    assert!(line.download_mode, "all backup error paths stay in the loader");
    assert_eq!(line.flash, original, "all probe, read and error paths preserve flash contents");
    fs::remove_dir_all(&directory).expect("SPI1 test removes its unique temporary directory");
}

#[test]
fn md5_reference_values() {
    // Reference values supplied by the lead, from the system md5 tool.
    for (input, expected) in [
        ("", "d41d8cd98f00b204e9800998ecf8427e"),
        ("abc", "900150983cd24fb0d6963f7d28e17f72"),
        ("The quick brown fox jumps over the lazy dog", "9e107d9d372bb6826bd81d3542a419d6"),
    ] {
        assert_eq!(md5::hex(&md5::digest(input.as_bytes())), expected, "MD5 matches the reference for {input:?}");
        let mut incremental = md5::Md5::new();
        for byte in input.bytes() { incremental.update(&[byte]); }
        assert_eq!(md5::hex(&incremental.finish()), expected, "incremental MD5 matches the reference for {input:?}");
    }
}
