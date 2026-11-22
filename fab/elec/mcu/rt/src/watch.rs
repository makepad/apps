//! Watch streams are `COBS(kind, seq, tick_le32, payload, crc8) 0x00`.
//! Hello identifies the protocol, image, clock, reset state and current radio generation.
//! Write and Read report pin activity and its source site. Duty reports an eight-bit PWM level.
//! Shift reports a value shifted MSB first on data and clock outputs;
//! EepromRead and EepromWrite report a byte at an EEPROM address and call site;
//! Rx reports a byte at its reception sample, which may predate prior frames;
//! Fault carries runtime failures, kernel reports and radio start markers;
//! RadioRecord carries four words of an S3 radio fault record to the host.
//! Time supplies the high tick bits and Probe reports a sampled value. Queue
//! overflow follows "keep the oldest, count the newest": Lost says how many
//! events the device could not queue, between its payload's first tick and
//! the frame's tick (the last lost event). All multibyte fields are little-endian.
//!
//! The link also carries network frames, both ways and in the same framing: Eth
//! holds one Ethernet II frame verbatim (ETH_MIN..=ETH_MAX bytes, no FCS), and
//! EthCredit (device to host) says how many receive buffers the device freed
//! since its previous credit. From host to device the host numbers its own
//! frames and echoes the latest Hello tick as its link epoch. RADIO_RESTART uses
//! that same numbered stream and epoch echo to request a radio restart. Link reports
//! one cumulative u64 link counter. Eth's body is larger than any event's, so the
//! event constants (MAX_BODY, MAX_WIRE: the devices' own frame buffers) stay
//! small; ETH_BODY and ETH_WIRE size the host's encoder and the Reader.

use core::ops::RangeInclusive;

pub const PROTO: u8 = 4;

/// Observation carried by an image. The discriminants are the Hello wire values.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(u8)]
pub enum Level {
    Off = 0,
    #[default]
    Faults = 1,
    Ports = 2,
}

impl Level {
    pub fn name(self) -> &'static str {
        match self { Self::Off => "off", Self::Faults => "faults", Self::Ports => "ports" }
    }
}
pub const HELLO: u8 = 0;
pub const WRITE: u8 = 1;
pub const READ: u8 = 2;
pub const FAULT: u8 = 3;
pub const LOST: u8 = 4;
pub const TIME: u8 = 5;
pub const PROBE: u8 = 6;
pub const RX: u8 = 7;
pub const DUTY: u8 = 8;
pub const ETH: u8 = 9;
pub const ETH_CREDIT: u8 = 10;
pub const LINK: u8 = 11;
pub const RADIO_RECORD: u8 = 12;
pub const SHIFT: u8 = 13;
pub const EEPROM_READ: u8 = 14;
pub const EEPROM_WRITE: u8 = 15;
/// Host-to-device command with an empty payload. The tick echoes the host's link epoch as
/// in a host Eth frame. The device restarts its radio island when the tick matches its latest Hello.
pub const RADIO_RESTART: u8 = 16;
/// The networking messages between a device's ports layer and the host end of the tether, from
/// PROTO 4 on; `net::NetMsg` is their one codec (payloads and directions there). Host frames echo
/// the latest Hello tick as Eth's do, and a device Hello ends every connection.
pub const NET_OPEN: u8 = 17;
pub const NET_CLOSE: u8 = 18;
pub const NET_DATA: u8 = 19;
pub const NET_CREDIT: u8 = 20;
pub const NET_STATE: u8 = 21;
pub const NET_LINK: u8 = 22;

/// A Fault's site when the radio's scheduler, not a port call, reports it (FAULT_KERNEL_CRIT_SPAN,
/// FAULT_KERNEL_ISR_DEPTH): no source line. 0xFFFF is the serial byte's "no site" (sched's
/// u32::MAX in 16 bits); `module::load` refuses a module whose sites would reach either.
pub const SITE_RADIO_SCHEDULER: u16 = u16::MAX - 1;

pub const FAULT_PIN: u8 = 0;
pub const FAULT_TIMER: u8 = 1;
pub const FAULT_OVERRUN: u8 = 2;
pub const FAULT_LOOP_CAP: u8 = 3;
pub const FAULT_SERIAL: u8 = 4;
/// Fault.site is the link, Fault.arg holds the u32 count's bits.
/// Counts FIFO drops only, saturating at u32::MAX; no hardware overrun.
pub const FAULT_SERIAL_LOST: u8 = 5;
/// Same link/count fields, but at least this many frames were lost: the
/// hardware overran and cannot say how many. Adds one per DOR0-tagged frame
/// to the FIFO-drop count, saturating at u32::MAX.
pub const FAULT_SERIAL_LOST_AT_LEAST: u8 = 6;
/// the previous run ended in a watchdog reset; site = the handler id the runtime had stored, arg = the reset cause
pub const FAULT_WATCHDOG: u8 = 7;
/// A refused draw was skipped; site = the port call's site,
/// arg = the `sched::DrawRefusal` number (0 odd window, 1 source outside, 2 handle).
pub const FAULT_DRAW: u8 = 8;
/// Panel bring-up failed; site = 0, arg = the bring-up step that failed.
pub const FAULT_PANEL: u8 = 9;
/// A copy/fill range was clamped, or a motion axis or audio argument was out of range.
/// Site is the call. Arg is the requested count, axis or speaker level.
pub const FAULT_RANGE: u8 = 10;
/// An Ethernet send length is outside ETH_MIN..=ETH_MAX; nothing was sent.
/// Site is the port call; arg is the requested length.
pub const FAULT_ETH_LENGTH: u8 = 11;
/// An Ethernet port's buffer range is outside its buffer; nothing was sent
/// or written, and a receive frame stays queued. Site is the call; arg is off.
pub const FAULT_ETH_RANGE: u8 = 12;
/// `shift_out`'s bit count is outside 1..=32; nothing was driven. Site is the
/// call; arg is the requested count.
pub const FAULT_SHIFT_BITS: u8 = 13;
/// Shared I2C transaction NACK; site is the seven-bit device address,
/// arg is the register. Used by motion and touch.
pub const FAULT_I2C_NACK: u8 = 14;
/// Shared I2C transaction missed its bus-derived deadline; same arguments.
/// The transfer retains bus ownership until COMPLETE is observed.
pub const FAULT_I2C_TIMEOUT: u8 = 15;
/// An EEPROM address is outside the chip's capacity; nothing was written,
/// and a read returned 0. Site is the call; arg is the requested address.
pub const FAULT_EEPROM_ADDR: u8 = 16;
/// A networking port named no declared connection; it did nothing and returned 0. Site is the
/// call; arg is the value given as the connection.
pub const FAULT_NET_HANDLE: u8 = 17;
/// A `net_send` or `net_recv` count was negative or its range left the buffer; nothing was taken
/// or read, and the port returned 0. Site is the call; arg is off.
pub const FAULT_NET_RANGE: u8 = 18;
/// The reference HAL has no device for this audio port. Site is the call, arg is the port.
pub const FAULT_AUDIO_UNAVAILABLE: u8 = 19;
/// Playback ran dry while a requested refill handler was still waiting to run.
/// Site is the speaker entry. Arg counts silent or stale descriptors at recovery, saturated at i32::MAX.
pub const FAULT_SPEAKER_LOST: u8 = 20;
/// A speaker wait exceeded its hardware budget. Site is the speaker entry.
/// Arg is 1 for TX_UPDATE, 2 for the first EOF, or 3 for a running ring without a new EOF.
pub const FAULT_SPEAKER_STALL: u8 = 21;
/// Mic history overtook the read cursor during a take. Site is the mic entry.
/// Arg counts unread samples overwritten before the report, saturated at i32::MAX.
pub const FAULT_MIC_LOST: u8 = 22;
/// A capture wait exceeded its hardware budget. Site is the mic entry.
/// Arg is 1 for RX_UPDATE or 2 for a running receiver without a completed block.
pub const FAULT_MIC_STALL: u8 = 23;

/// Fatal kernel faults stop the export. They use the ordinary seven-byte
/// Fault payload, separate from the growing app fault table; arg is 0 unless
/// the code defines one.
pub const FAULT_KERNEL_FATAL: RangeInclusive<u8> = 64..=95;
pub const FAULT_KERNEL_MISALIGNED: u8 = 64;
pub const FAULT_KERNEL_OUT_OF_REGION: u8 = 65;
pub const FAULT_KERNEL_COPY_BOUND: u8 = 66;
pub const FAULT_KERNEL_FILL_BOUND: u8 = 67;
/// A blocking export would wait inside a critical section (depth > 0);
/// arg = the export's index.
pub const FAULT_KERNEL_BLOCK_IN_CRITICAL: u8 = 68;
/// Non-fatal kernel reports use the ordinary Fault payload without stopping
/// the export. Each code defines its argument.
pub const FAULT_KERNEL_REPORT: RangeInclusive<u8> = 96..=127;
/// The radio heap had no room for a timer's node or slot chunk when the timer was set; the timer
/// stays pending and its arm tries again. arg = the live timer count.
pub const FAULT_KERNEL_TIMER_REFUSED: u8 = 96;
/// A critical section's restore did not match its token, or came at depth 0;
/// arg = the caller's return address.
pub const FAULT_KERNEL_CRIT_MISMATCH: u8 = 97;
/// A new longest masked span, above the kernel's worst primitive; arg = the
/// span in CPU cycles.
pub const FAULT_KERNEL_CRIT_SPAN: u8 = 98;
/// An island ISR handler returned inside a critical section; arg = its depth.
pub const FAULT_KERNEL_ISR_DEPTH: u8 = 99;
/// The radio PHY's initialisation failed. The radio stops, and restarts with the next start
/// marker. arg = its result.
pub const FAULT_RADIO_PHY_INIT_FAILED: u8 = 100;
/// Storing the radio PHY's calibration failed; arg = the flash error.
pub const FAULT_RADIO_PHY_CAL_STORE_FAILED: u8 = 101;
/// The radio asked for an interrupt line it may not use (runtime-owned, not
/// level 1, or out of range); arg = the line.
pub const FAULT_KERNEL_IRQ_LINE_REFUSED: u8 = 102;
/// The radio asked to route an interrupt source it may not use; arg = the source.
pub const FAULT_KERNEL_IRQ_SOURCE_REFUSED: u8 = 103;
/// No island ISR slot is left for an interrupt line; arg = the line.
pub const FAULT_KERNEL_ISR_TABLE_FULL: u8 = 104;
/// An island ISR entry lies outside the island's code region; arg = the entry address.
pub const FAULT_KERNEL_ISR_ENTRY_REFUSED: u8 = 105;
/// The image's radio island is absent from flash or is not the one this firmware was built with;
/// the radio stays off and the app and USB run on. arg = the island header's CRC as read from flash.
pub const FAULT_RADIO_ISLAND_MISMATCH: u8 = 106;
/// A pending timer still had no memory when it was armed and will not fire. The radio stops,
/// and restarts with the next start marker. arg = the ETSTimer's address.
pub const FAULT_KERNEL_TIMER_LOST: u8 = 107;
/// A timer was armed before it was ever set; the arm is ignored. arg = the ETSTimer's address.
pub const FAULT_KERNEL_TIMER_UNSET: u8 = 108;
/// The radio heap refused an allocation (other than a timer's, which is FAULT_KERNEL_TIMER_REFUSED).
/// arg = the bytes asked for, saturated at 0xffff_ffff when the request does not fit in 32 bits.
pub const FAULT_KERNEL_HEAP_EXHAUSTED: u8 = 109;
/// A timer carries the kernel's tag but its node word fails validation: the radio's code wrote over
/// the kernel's words. The radio stops, and restarts with the next start marker.
/// arg = the ETSTimer's address.
pub const FAULT_KERNEL_TIMER_HANDLE_INVALID: u8 = 110;
/// Every radio start, including boot. The argument holds generation in bits 0..=15, cause in
/// bits 16..=23 (0 for boot, RADIO_START_CAUSE_REQUEST, RADIO_START_CAUSE_FAULT, 96..=127 for a
/// stop report), and consecutive restarts in bits 24..=31, saturating at 255. All stopping
/// reports for the previous start precede this marker in the stream.
pub const FAULT_RADIO_STARTED: u8 = 111;
/// A `FAULT_RADIO_STARTED` cause: the host asked for the restart.
pub const RADIO_START_CAUSE_REQUEST: u8 = 1;
/// A `FAULT_RADIO_STARTED` cause: an automatic start after a radio fault; the fault's record
/// precedes the marker.
pub const RADIO_START_CAUSE_FAULT: u8 = 2;
/// The pack's PHY function table pointer (`g_phyFuns`) is not word-aligned, or the table's
/// 656 bytes are not inside the ROM data page 0x3fcef000..0x3fcf0000. The runtime neither
/// saves nor restores the table, and restarts go on without it. arg = the pointer.
pub const FAULT_RADIO_PHY_TABLE_REFUSED: u8 = 112;
/// A Wi-Fi event the radio posted, in post order. arg = the event id (WIFI_EVENT base), or
/// 0xFFFF_FFFF for another base. Not a fault: the radio runs on.
pub const FAULT_RADIO_EVENT: u8 = 113;
/// Wi-Fi start failed. arg holds the step (1 init, 2 set mode, 3 start) in the high
/// byte and the low 24 bits of the returned error. The task then explicitly stops the radio.
pub const FAULT_RADIO_START_FAILED: u8 = 114;
/// The kernel reports these at failure counts 1, 2, 4, 8 and so on (its counters are exact), so a
/// host that tallied n reports has seen from 2^(n-1) to 2^n - 1 failures.
pub const FAULT_KERNEL_DOUBLING: [u8; 5] = [FAULT_KERNEL_TIMER_REFUSED, FAULT_KERNEL_TIMER_LOST, FAULT_KERNEL_TIMER_UNSET,
    FAULT_KERNEL_HEAP_EXHAUSTED, FAULT_KERNEL_TIMER_HANDLE_INVALID];
/// On these reports the radio stops, and restarts with the next start marker.
pub const FAULT_KERNEL_STOPPING: [u8; 2] = [FAULT_KERNEL_TIMER_LOST, FAULT_KERNEL_TIMER_HANDLE_INVALID];

/// The fields of a `FAULT_RADIO_STARTED` argument.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RadioStart {
    pub generation: u16,
    pub cause: u8,
    pub consecutive: u8,
}

pub fn radio_start(arg: i32) -> RadioStart {
    let [lo, hi, cause, consecutive] = arg.to_le_bytes();
    RadioStart { generation: u16::from_le_bytes([lo, hi]), cause, consecutive }
}

/// Hello is the largest watch event, with a six-byte header, 22 payload bytes and a CRC byte.
pub const MAX_BODY: usize = 29;
/// One COBS code byte and the trailing zero delimiter frame the event body.
pub const MAX_WIRE: usize = MAX_BODY + 2;

/// An Eth frame's shortest and longest payload: an Ethernet II header alone,
/// and a full 1500-byte MTU frame with its header (no FCS).
pub const ETH_MIN: usize = 14;
pub const ETH_MAX: usize = 1514;
/// The longest body: kind, seq, tick, the largest payload, crc.
pub const ETH_BODY: usize = 6 + ETH_MAX + 1;
/// The longest wire frame: COBS adds at most one code byte per 254 body bytes
/// plus one, then the delimiter.
pub const ETH_WIRE: usize = ETH_BODY + ETH_BODY / 254 + 1 + 1;

const fn crc8_table() -> [u8; 256] {
    let mut table = [0; 256];
    let mut i = 0;
    while i < table.len() {
        let mut crc = i as u8;
        let mut bit = 0;
        while bit < 8 {
            crc = if crc & 0x80 != 0 { (crc << 1) ^ 0x07 } else { crc << 1 };
            bit += 1;
        }
        table[i] = crc;
        i += 1;
    }
    table
}

/// CRC-8/SMBUS: polynomial 0x07, initial value and xorout zero, unreflected.
pub const CRC8_TABLE: [u8; 256] = crc8_table();

pub fn crc8(bytes: &[u8]) -> u8 {
    let mut crc = 0;
    for &byte in bytes {
        crc = CRC8_TABLE[(crc ^ byte) as usize];
    }
    crc
}

/// Encode without a delimiter. On failure, `dst` may contain a partial result.
pub fn cobs_encode(src: &[u8], dst: &mut [u8]) -> Option<usize> {
    *dst.get_mut(0)? = 0;
    let (mut code_at, mut written, mut code) = (0, 1, 1u8);
    for (i, &byte) in src.iter().enumerate() {
        if byte == 0 {
            *dst.get_mut(code_at)? = code;
            code_at = written;
            *dst.get_mut(written)? = 0;
            written += 1;
            code = 1;
        } else {
            *dst.get_mut(written)? = byte;
            written += 1;
            code += 1;
            if code == 0xff {
                *dst.get_mut(code_at)? = code;
                // A full final block needs no empty block after it.
                if i + 1 == src.len() {
                    return Some(written);
                }
                code_at = written;
                *dst.get_mut(written)? = 0;
                written += 1;
                code = 1;
            }
        }
    }
    *dst.get_mut(code_at)? = code;
    Some(written)
}

/// Decode a packet without its delimiter; reject embedded zeros and short blocks.
/// On failure, `dst` may contain a partial result.
pub fn cobs_decode(src: &[u8], dst: &mut [u8]) -> Option<usize> {
    if src.is_empty() {
        return None;
    }
    let (mut read, mut written) = (0usize, 0usize);
    while read < src.len() {
        let code = src[read];
        if code == 0 {
            return None;
        }
        read += 1;
        let end = read.checked_add(usize::from(code) - 1)?;
        for &byte in src.get(read..end)? {
            if byte == 0 {
                return None;
            }
            *dst.get_mut(written)? = byte;
            written += 1;
        }
        read = end;
        if code != 0xff && read < src.len() {
            *dst.get_mut(written)? = 0;
            written += 1;
        }
    }
    Some(written)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    Hello {
        proto: u8,
        ports_version: u8,
        image: u32,
        clock_hz: u32,
        prescale: u8,
        ring: u16,
        tick_hi: u16,
        mcusr: u8,
        wdtcsr: u8,
        level: Level,
        /// number of radio island starts so far; 0 without a radio or before its first start
        generation: u32,
    },
    Write { pin: u8, level: bool, site: u16 },
    Duty { pin: u8, level: u8, site: u16 },
    Shift { data: u8, clock: u8, value: u32, bits: u8, site: u16 },
    EepromRead { addr: u16, value: u8, site: u16 },
    EepromWrite { addr: u16, value: u8, site: u16 },
    Read { pin: u8, level: bool, site: u16 },
    Fault { code: u8, site: u16, arg: i32 },
    Lost { count: u32, saturated: bool, first_tick: u32 },
    Time { tick_hi: u16 },
    Probe { id: u8, value: u16 },
    /// Tick is the ISR's reception sample, even when dispatch was delayed.
    Rx { byte: u8 },
    /// One network frame of `len` bytes; [`Reader::eth_body`] lends its bytes
    /// until the next push, and [`encode_eth`] sends one.
    Eth { len: u16 },
    /// Receive buffers freed since the previous credit.
    EthCredit { count: u16 },
    /// One of the device's network messages (kind `NET_OPEN..=NET_LINK`, its `len`-byte payload a
    /// well-formed device-to-host `net::NetMsg`); [`Reader::net_body`] lends the payload until the
    /// next push, and [`encode_net`] sends the host's.
    Net { len: u16 },
    /// Cumulative link statistics: 0 IN edges, 1 stale ETH, 2 TX drops,
    /// 3 bad RX frames, 4 RX frames refused for lack of a slot.
    Link { counter: u8, value: u64 },
    /// S3 only (never AVR): frame i carries record words 4i..4i+4, with zero
    /// padding in the last frame. Word 0 is the record length in words.
    /// The index and four words occupy 18 little-endian payload bytes, giving a 25-byte body.
    RadioRecord { index: u16, words: [u32; 4] },
}

impl Event {
    /// Convert a scheduler/reference fault to its existing watch envelope.
    /// Sites have the same low-16-bit representation as app fault sites.
    pub fn from_fault(fault: crate::sched::Fault) -> Self {
        use crate::{kernel::KernelFault, sched::Fault};
        let (code, site, arg) = match fault {
            Fault::Pin { site, pin, .. } => (FAULT_PIN, site, pin),
            Fault::Timer { site, timer, .. } => (FAULT_TIMER, site, timer),
            Fault::Serial { site, link, .. } => (FAULT_SERIAL, site, link),
            Fault::SerialLost { link, count, at_least } => (if at_least { FAULT_SERIAL_LOST_AT_LEAST } else { FAULT_SERIAL_LOST }, link, count as i32),
            Fault::Overrun { timer, missed } => (FAULT_OVERRUN, timer, missed as i32),
            Fault::LoopCap { site } => (FAULT_LOOP_CAP, site, 0),
            Fault::Draw { site, refusal, .. } => (FAULT_DRAW, site, refusal as i32),
            Fault::Range { site, n } => (FAULT_RANGE, site, n),
            Fault::EthLength { site, n } => (FAULT_ETH_LENGTH, site, n),
            Fault::EthRange { site, off, .. } => (FAULT_ETH_RANGE, site, off),
            Fault::ShiftBits { site, bits } => (FAULT_SHIFT_BITS, site, bits),
            Fault::EepromAddr { site, addr } => (FAULT_EEPROM_ADDR, site, addr),
            Fault::NetHandle { site, c, .. } => (FAULT_NET_HANDLE, site, c),
            Fault::NetRange { site, off, .. } => (FAULT_NET_RANGE, site, off),
            Fault::AudioUnavailable { port, site, .. } => (FAULT_AUDIO_UNAVAILABLE, site, i32::from(port)),
            Fault::Kernel { kind, site } => {
                let (code, arg) = match kind {
                    KernelFault::Misaligned => (FAULT_KERNEL_MISALIGNED, 0),
                    KernelFault::OutOfRegion => (FAULT_KERNEL_OUT_OF_REGION, 0),
                    KernelFault::CopyBound => (FAULT_KERNEL_COPY_BOUND, 0),
                    KernelFault::FillBound => (FAULT_KERNEL_FILL_BOUND, 0),
                    KernelFault::BlockInCritical { export } => (FAULT_KERNEL_BLOCK_IN_CRITICAL, i32::from(export)),
                };
                (code, site, arg)
            }
            Fault::KernelReport { code, site, arg } => (code, site, arg),
        };
        Self::Fault { code, site: site as u16, arg }
    }

    /// Recover a kernel fault, report or shared loop-cap fault after decode.
    /// The frame decoder checks the exact ordinary Fault payload length.
    pub fn kernel_fault(self) -> Option<crate::sched::Fault> {
        use crate::{kernel::KernelFault, sched::Fault};
        let Self::Fault { code, site, arg } = self else { return None; };
        let site = u32::from(site);
        if FAULT_KERNEL_REPORT.contains(&code) {
            return Some(Fault::KernelReport { code, site, arg });
        }
        if code == FAULT_KERNEL_BLOCK_IN_CRITICAL {
            let export = u16::try_from(arg).ok()?;
            return Some(Fault::Kernel { kind: KernelFault::BlockInCritical { export }, site });
        }
        if arg != 0 { return None; }
        let kind = match code {
            FAULT_KERNEL_MISALIGNED => KernelFault::Misaligned,
            FAULT_KERNEL_OUT_OF_REGION => KernelFault::OutOfRegion,
            FAULT_KERNEL_COPY_BOUND => KernelFault::CopyBound,
            FAULT_KERNEL_FILL_BOUND => KernelFault::FillBound,
            FAULT_LOOP_CAP => return Some(Fault::LoopCap { site }),
            _ => return None,
        };
        Some(Fault::Kernel { kind, site })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Frame {
    pub kind: u8,
    pub seq: u8,
    pub tick: u32,
    pub event: Event,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Bad {
    Cobs,
    Crc,
    Length,
    Kind,
}

/// Encode including the delimiter. `kind` must agree with `event`; it is sent
/// verbatim. Write/Read pins use their low seven bits; Lost counts use their low 31 bits.
/// An Eth frame carries a body: it is sent with [`encode_eth`], and passing one
/// here is a programming error (a panic).
pub fn encode(frame: &Frame, wire: &mut [u8; MAX_WIRE]) -> usize {
    let mut body = [0; MAX_BODY];
    body[0] = frame.kind;
    body[1] = frame.seq;
    body[2..6].copy_from_slice(&frame.tick.to_le_bytes());
    let p = &mut body[6..];
    let payload_len = match frame.event {
        Event::Hello { proto, ports_version, image, clock_hz, prescale, ring, tick_hi, mcusr, wdtcsr, level, generation } => {
            p[0] = proto;
            p[1] = ports_version;
            p[2..6].copy_from_slice(&image.to_le_bytes());
            p[6..10].copy_from_slice(&clock_hz.to_le_bytes());
            p[10] = prescale;
            p[11..13].copy_from_slice(&ring.to_le_bytes());
            p[13..15].copy_from_slice(&tick_hi.to_le_bytes());
            p[15] = mcusr;
            p[16] = wdtcsr;
            p[17] = level as u8;
            p[18..22].copy_from_slice(&generation.to_le_bytes());
            22
        }
        Event::Write { pin, level, site } | Event::Read { pin, level, site } => {
            p[0] = (pin & 0x7f) | (u8::from(level) << 7);
            p[1..3].copy_from_slice(&site.to_le_bytes());
            3
        }
        Event::Duty { pin, level, site } => {
            p[0] = pin;
            p[1] = level;
            p[2..4].copy_from_slice(&site.to_le_bytes());
            4
        }
        Event::Shift { data, clock, value, bits, site } => {
            p[0] = data;
            p[1] = clock;
            p[2] = bits;
            p[3..7].copy_from_slice(&value.to_le_bytes());
            p[7..9].copy_from_slice(&site.to_le_bytes());
            9
        }
        Event::EepromRead { addr, value, site } | Event::EepromWrite { addr, value, site } => {
            p[..2].copy_from_slice(&addr.to_le_bytes());
            p[2] = value;
            p[3..5].copy_from_slice(&site.to_le_bytes());
            5
        }
        Event::Fault { code, site, arg } => {
            p[0] = code;
            p[1..3].copy_from_slice(&site.to_le_bytes());
            p[3..7].copy_from_slice(&arg.to_le_bytes());
            7
        }
        Event::Lost { count, saturated, first_tick } => {
            let count = (count & 0x7fff_ffff) | (u32::from(saturated) << 31);
            p[..4].copy_from_slice(&count.to_le_bytes());
            p[4..8].copy_from_slice(&first_tick.to_le_bytes());
            8
        }
        Event::Time { tick_hi } => {
            p[..2].copy_from_slice(&tick_hi.to_le_bytes());
            2
        }
        Event::Probe { id, value } => {
            p[0] = id;
            p[1..3].copy_from_slice(&value.to_le_bytes());
            3
        }
        Event::Rx { byte } => { p[0] = byte; 1 }
        Event::EthCredit { count } => {
            p[..2].copy_from_slice(&count.to_le_bytes());
            2
        }
        Event::RadioRecord { index, words } => {
            p[..2].copy_from_slice(&index.to_le_bytes());
            for (dst, word) in p[2..18].chunks_exact_mut(4).zip(words) {
                dst.copy_from_slice(&word.to_le_bytes());
            }
            18
        }
        Event::Link { counter, value } => {
            p[0] = counter;
            p[1..9].copy_from_slice(&value.to_le_bytes());
            9
        }
        Event::Eth { .. } => panic!("an Eth frame carries a body: send it with encode_eth"),
        Event::Net { .. } => panic!("a Net frame carries a message: send it with encode_net"),
    };
    let crc_at = 6 + payload_len;
    body[crc_at] = crc8(&body[..crc_at]);
    // MAX_BODY is below one COBS block; MAX_WIRE always has room for
    // the code byte and delimiter, so this encoding cannot fail.
    let len = cobs_encode(&body[..crc_at + 1], wire).unwrap_or(0);
    wire[len] = 0;
    len + 1
}

/// Encode one Eth frame including the delimiter; None when `payload` is not
/// ETH_MIN..=ETH_MAX bytes (nothing is written then).
pub fn encode_eth(seq: u8, tick: u32, payload: &[u8], wire: &mut [u8; ETH_WIRE]) -> Option<usize> {
    if !(ETH_MIN..=ETH_MAX).contains(&payload.len()) {
        return None;
    }
    let mut body = [0; ETH_BODY];
    body[0] = ETH;
    body[1] = seq;
    body[2..6].copy_from_slice(&tick.to_le_bytes());
    let crc_at = 6 + payload.len();
    body[6..crc_at].copy_from_slice(payload);
    body[crc_at] = crc8(&body[..crc_at]);
    // ETH_WIRE holds the worst-case COBS expansion and the delimiter.
    let len = cobs_encode(&body[..crc_at + 1], wire)?;
    wire[len] = 0;
    Some(len + 1)
}

/// Encode one of the host's network messages, including the delimiter. `tick` echoes the device's
/// latest Hello, as a host Eth frame's does. Every well-formed message fits ETH_WIRE (a part is at
/// most `net::NET_PART` bytes); a longer part is a programming error (a panic).
pub fn encode_net(seq: u8, tick: u32, msg: &crate::net::NetMsg, wire: &mut [u8; ETH_WIRE]) -> usize {
    let mut body = alloc::vec::Vec::with_capacity(ETH_BODY);
    body.push(msg.kind());
    body.push(seq);
    body.extend_from_slice(&tick.to_le_bytes());
    msg.encode(&mut body);
    assert!(body.len() < ETH_BODY, "a network message of {} payload bytes is longer than a part", body.len() - 6);
    body.push(crc8(&body));
    let len = cobs_encode(&body, wire).expect("a network message fits ETH_WIRE");
    wire[len] = 0;
    len + 1
}

/// Encode a host radio restart request, including its delimiter.
pub fn encode_radio_restart(seq: u8, tick: u32, wire: &mut [u8; MAX_WIRE]) -> usize {
    let mut body = [0; 7];
    body[0] = RADIO_RESTART;
    body[1] = seq;
    body[2..6].copy_from_slice(&tick.to_le_bytes());
    body[6] = crc8(&body[..6]);
    let len = cobs_encode(&body, wire).expect("a seven-byte body fits MAX_WIRE");
    wire[len] = 0;
    len + 1
}

/// Decode one body. An Eth frame's bytes stay in `body` at `6..6 + len`.
pub fn decode(body: &[u8]) -> Result<Frame, Bad> {
    if !(7..=ETH_BODY).contains(&body.len()) {
        return Err(Bad::Length);
    }
    let crc_at = body.len() - 1;
    if crc8(&body[..crc_at]) != body[crc_at] {
        return Err(Bad::Crc);
    }
    let payload = &body[6..crc_at];
    let event = match (body[0], payload) {
        (HELLO, &[proto, ports_version, i0, i1, i2, i3, c0, c1, c2, c3, prescale, r0, r1, h0, h1,
            mcusr, wdtcsr, level, g0, g1, g2, g3]) => {
            if proto != PROTO { return Err(Bad::Kind); }
            let level = match level {
                0 => Level::Off, 1 => Level::Faults, 2 => Level::Ports,
                _ => return Err(Bad::Kind),
            };
            Event::Hello {
                proto, ports_version,
                image: u32::from_le_bytes([i0, i1, i2, i3]),
                clock_hz: u32::from_le_bytes([c0, c1, c2, c3]),
                prescale, ring: u16::from_le_bytes([r0, r1]),
                tick_hi: u16::from_le_bytes([h0, h1]), mcusr, wdtcsr, level,
                generation: u32::from_le_bytes([g0, g1, g2, g3]),
            }
        }
        (kind @ (WRITE | READ), &[packed, s0, s1]) => {
            let (pin, level, site) = (packed & 0x7f, packed & 0x80 != 0, u16::from_le_bytes([s0, s1]));
            if kind == WRITE { Event::Write { pin, level, site } } else { Event::Read { pin, level, site } }
        }
        (DUTY, &[pin, level, s0, s1]) => {
            Event::Duty { pin, level, site: u16::from_le_bytes([s0, s1]) }
        }
        (SHIFT, &[data, clock, bits, v0, v1, v2, v3, s0, s1]) => Event::Shift {
            data, clock, value: u32::from_le_bytes([v0, v1, v2, v3]), bits,
            site: u16::from_le_bytes([s0, s1]),
        },
        (kind @ (EEPROM_READ | EEPROM_WRITE), &[a0, a1, value, s0, s1]) => {
            let (addr, site) = (u16::from_le_bytes([a0, a1]), u16::from_le_bytes([s0, s1]));
            if kind == EEPROM_READ { Event::EepromRead { addr, value, site } }
            else { Event::EepromWrite { addr, value, site } }
        }
        (FAULT, &[code, s0, s1, a0, a1, a2, a3]) => {
            Event::Fault { code, site: u16::from_le_bytes([s0, s1]), arg: i32::from_le_bytes([a0, a1, a2, a3]) }
        }
        (LOST, &[c0, c1, c2, c3, t0, t1, t2, t3]) => {
            let packed = u32::from_le_bytes([c0, c1, c2, c3]);
            Event::Lost {
                count: packed & 0x7fff_ffff, saturated: packed & 0x8000_0000 != 0,
                first_tick: u32::from_le_bytes([t0, t1, t2, t3]),
            }
        }
        (TIME, &[h0, h1]) => Event::Time { tick_hi: u16::from_le_bytes([h0, h1]) },
        (PROBE, &[id, v0, v1]) => Event::Probe { id, value: u16::from_le_bytes([v0, v1]) },
        (ETH, frame) if (ETH_MIN..=ETH_MAX).contains(&frame.len()) => Event::Eth { len: frame.len() as u16 },
        (ETH_CREDIT, &[c0, c1]) => Event::EthCredit { count: u16::from_le_bytes([c0, c1]) },
        (RADIO_RECORD, &[i0, i1, a0, a1, a2, a3, b0, b1, b2, b3, c0, c1, c2, c3, d0, d1, d2, d3]) => Event::RadioRecord {
            index: u16::from_le_bytes([i0, i1]),
            words: [u32::from_le_bytes([a0, a1, a2, a3]), u32::from_le_bytes([b0, b1, b2, b3]),
                u32::from_le_bytes([c0, c1, c2, c3]), u32::from_le_bytes([d0, d1, d2, d3])],
        },
        (LINK, &[counter, v0, v1, v2, v3, v4, v5, v6, v7]) => Event::Link {
            counter, value: u64::from_le_bytes([v0, v1, v2, v3, v4, v5, v6, v7]),
        },
        (RX, &[byte]) => Event::Rx { byte },
        // the host's own kinds never come from a device
        (NET_STATE | NET_LINK, _) => return Err(Bad::Kind),
        (kind @ NET_OPEN..=NET_CREDIT, payload) => match crate::net::NetMsg::decode(kind, crate::net::Dir::Up, payload) {
            Some(_) => Event::Net { len: payload.len() as u16 },
            None => return Err(Bad::Length),
        },
        (HELLO | WRITE | READ | DUTY | SHIFT | EEPROM_READ | EEPROM_WRITE | FAULT | LOST | TIME | PROBE | RX | ETH | ETH_CREDIT | LINK | RADIO_RECORD, _) => return Err(Bad::Length),
        _ => return Err(Bad::Kind),
    };
    Ok(Frame {
        kind: body[0], seq: body[1],
        tick: u32::from_le_bytes([body[2], body[3], body[4], body[5]]), event,
    })
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BadCounts {
    pub cobs: u64,
    pub crc: u64,
    pub length: u64,
    pub kind: u64,
}

impl BadCounts {
    fn record(&mut self, bad: Bad) {
        let count = match bad {
            Bad::Cobs => &mut self.cobs,
            Bad::Crc => &mut self.crc,
            Bad::Length => &mut self.length,
            Bad::Kind => &mut self.kind,
        };
        *count = count.saturating_add(1);
    }
}

#[derive(Clone)]
pub struct Reader {
    wire: [u8; ETH_WIRE],
    len: usize,
    /// The last decoded body; an Eth frame's bytes and a Net frame's payload are lent from here.
    body: [u8; ETH_WIRE],
    eth: Option<(usize, usize)>,
    net: Option<(usize, usize)>,
    resync: bool,
    previous_seq: Option<u8>,
    /// Successfully decoded frames, including Hello.
    pub frames: u64,
    pub bad: BadCounts,
    /// Nonzero bytes discarded in overlong runs, including the buffered prefix.
    pub junk: u64,
    /// Sequence discontinuities since the most recent Hello.
    pub gaps: u64,
}

impl Default for Reader {
    fn default() -> Self {
        Reader {
            wire: [0; ETH_WIRE], len: 0, body: [0; ETH_WIRE], eth: None, net: None, resync: false,
            previous_seq: None, frames: 0, bad: BadCounts::default(), junk: 0, gaps: 0,
        }
    }
}

impl Reader {
    pub fn new() -> Self {
        Self::default()
    }

    /// The bytes of the Eth frame the latest push returned; empty after any
    /// other push.
    pub fn eth_body(&self) -> &[u8] {
        match self.eth {
            Some((start, end)) => &self.body[start..end],
            None => &[],
        }
    }

    /// The payload of the Net frame the latest push returned (`net::NetMsg::decode` with
    /// `Dir::Up` reads it); empty after any other push.
    pub fn net_body(&self) -> &[u8] {
        match self.net {
            Some((start, end)) => &self.body[start..end],
            None => &[],
        }
    }

    /// Emit an overlength error once, at the first byte beyond ETH_WIRE (the
    /// longest frame of any kind); discard the remainder through the next
    /// delimiter. Empty packets vanish.
    pub fn push(&mut self, byte: u8) -> Option<Result<Frame, Bad>> {
        self.eth = None;
        self.net = None;
        if self.resync {
            if byte == 0 {
                self.resync = false;
            } else {
                self.junk = self.junk.saturating_add(1);
            }
            return None;
        }
        if byte != 0 {
            if self.len == self.wire.len() {
                self.junk = self.junk.saturating_add(self.len as u64 + 1);
                self.len = 0;
                self.resync = true;
                self.bad.record(Bad::Length);
                return Some(Err(Bad::Length));
            }
            self.wire[self.len] = byte;
            self.len += 1;
            return None;
        }
        if self.len == 0 {
            return None;
        }
        // An ETH_WIRE-sized encoded input may decode to ETH_WIRE - 1 bytes.
        // Keep room here so an oversized body is Length, not a COBS error.
        let result = match cobs_decode(&self.wire[..self.len], &mut self.body) {
            Some(len) => decode(&self.body[..len]),
            None => Err(Bad::Cobs),
        };
        match result {
            Ok(Frame { event: Event::Eth { len }, .. }) => self.eth = Some((6, 6 + usize::from(len))),
            Ok(Frame { event: Event::Net { len }, .. }) => self.net = Some((6, 6 + usize::from(len))),
            _ => {}
        }
        self.len = 0;
        match result {
            Ok(frame) => {
                self.frames = self.frames.saturating_add(1);
                if matches!(frame.event, Event::Hello { .. }) {
                    self.gaps = 0;
                } else if self.previous_seq.is_some_and(|seq| frame.seq != seq.wrapping_add(1)) {
                    self.gaps = self.gaps.saturating_add(1);
                }
                self.previous_seq = Some(frame.seq);
            }
            Err(bad) => self.bad.record(bad),
        }
        Some(result)
    }
}

/// Reconstruct a 48-bit clock. Rx may precede the most recently transmitted
/// tick: its reception sample predates dispatch. Resolve it within half a
/// low-word epoch of the current anchor without moving that anchor backward.
/// The device emits Time every 2^30 ticks so a gap never spans a wrap.
/// Hello starts a new epoch; Time explicitly supplies its own high tick bits.
#[derive(Default)]
pub struct Unwrap {
    hi: u16,
    previous: Option<u32>,
}

impl Unwrap {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn tick(&mut self, frame: &Frame) -> u64 {
        if matches!(frame.event, Event::Rx { .. }) {
            if let Some(previous) = self.previous {
                let anchor = (u64::from(self.hi) << 32) | u64::from(previous);
                let delta = i64::from(frame.tick.wrapping_sub(previous) as i32);
                let tick = anchor.wrapping_add_signed(delta) & ((1u64 << 48) - 1);
                if delta >= 0 {
                    self.hi = (tick >> 32) as u16;
                    self.previous = Some(frame.tick);
                }
                return tick;
            }
        }
        match frame.event {
            Event::Hello { tick_hi, .. } | Event::Time { tick_hi } => self.hi = tick_hi,
            _ if self.previous.is_some_and(|low| frame.tick < low) => self.hi = self.hi.wrapping_add(1),
            _ => {}
        }
        self.previous = Some(frame.tick);
        (u64::from(self.hi) << 32) | u64::from(frame.tick)
    }
}
