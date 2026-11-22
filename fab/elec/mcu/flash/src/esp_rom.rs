//! ESP ROM probing and ESP32-S3 ROM flash operations, without a stub loader.
//! The legacy [`probe`] remains read-only (SYNC and READ_REG).
//! S3 commands follow "Serial Protocol", sections "Command Packet",
//! "Response Packet", "Status Bytes" and "Supported by Stub Loader and ROM Loader".
//! SPI1 backups use only ROM register commands and MD5, restore controller
//! configuration and stay in the loader; no code is uploaded to the chip.

use crate::md5;
use makepad_serial::line::{Line, Step, CONNECT_WINDOW};
use makepad_core_util::sha256::{self, Sha256};
use std::{fmt, fs, io::{self, Write}, path::Path, time::{Duration, Instant}};

const END: u8 = 0xc0;
const ESC: u8 = 0xdb;
const ESC_END: u8 = 0xdc;
const ESC_ESC: u8 = 0xdd;
const SYNC: u8 = 0x08;
const WRITE_REG: u8 = 0x09;
const READ_REG: u8 = 0x0a;
const MAGIC_ADDRESS: u32 = 0x40001000;
const BYTE_TIMEOUT: Duration = Duration::from_millis(100);

#[derive(Debug)]
pub enum Error {
    Io(io::Error),
    Timeout,
    NoSync,
    /// The port took no data after the download reset: the chip is still in its own firmware,
    /// whose USB device ignores the reset lines (a USB-OTG device rather than USB-Serial-JTAG).
    NotLoader,
    Protocol(&'static str),
    Spi1(&'static str),
    Restore { operation: Box<Error>, restore: Box<Error> },
    BackupBlock { block: u32, offset: u32, want: [u8; 16], got: [u8; 16] },
    Rom { code: u8, meaning: &'static str },
    WrongChip { want: u32, got: u32 },
    /// A register read back after WRITE_REG does not hold the written value.
    Register { address: u32, wrote: u32, got: u32 },
    Plan(String),
    Verify { offset: u32, size: u32, want: [u8; 16], got: [u8; 16] },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(err) => write!(f, "I/O: {err}"),
            Self::Timeout => f.write_str("ESP ROM reply timed out"),
            Self::NoSync => f.write_str("no ESP ROM sync"),
            Self::NotLoader => f.write_str("the device took no data after the download reset: it is not in the ESP ROM loader \
                (its USB device ignores the DTR/RTS reset; hold the BOOT strap through a reset to enter download mode)"),
            Self::Protocol(message) => write!(f, "ESP ROM protocol: {message}"),
            Self::Spi1(message) => write!(f, "ESP SPI1: {message}"),
            Self::Restore { operation, restore } => write!(f, "{operation}; SPI1 restoration also failed: {restore}"),
            Self::BackupBlock { block, offset, want, got } => write!(f,
                "ESP backup block {block} at 0x{offset:08x}: systematic error (two identical reads), ROM MD5 {}, read MD5 {}",
                md5::hex(want), md5::hex(got)),
            Self::Rom { code, meaning } => write!(f, "ESP ROM error 0x{code:02x}: {meaning}"),
            Self::WrongChip { want, got } => write!(f, "ESP ROM chip id: expected 0x{want:08x}, got 0x{got:08x}"),
            Self::Register { address, wrote, got } => write!(f,
                "ESP register 0x{address:08x}: wrote 0x{wrote:08x}, reads back 0x{got:08x}"),
            Self::Plan(message) => write!(f, "ESP flash plan: {message}"),
            Self::Verify { offset, size, want, got } => write!(f,
                "ESP ROM verify failed for region 0x{offset:08x} ({size} bytes): expected {}, got {}",
                md5::hex(want), md5::hex(got)),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(err) => Some(err),
            _ => None,
        }
    }
}

impl From<io::Error> for Error {
    fn from(err: io::Error) -> Self { Self::Io(err) }
}

#[derive(Debug)]
pub struct EspRom {
    pub magic: u32,
}

fn request<L: Line>(line: &mut L, command: u8, data: &[u8]) -> Result<(), Error> {
    request_checksum(line, command, data, 0)
}

/// "Serial Protocol", "Low Level Protocol", "Command Packet" and "Checksum".
fn request_checksum<L: Line>(line: &mut L, command: u8, data: &[u8], checksum: u32) -> Result<(), Error> {
    let size = u16::try_from(data.len()).map_err(|_| Error::Protocol("request is too long"))?;
    let mut packet = Vec::with_capacity(8 + data.len());
    packet.extend_from_slice(&[0, command]);
    packet.extend_from_slice(&size.to_le_bytes());
    packet.extend_from_slice(&checksum.to_le_bytes());
    packet.extend_from_slice(data);
    let mut frame = Vec::with_capacity(packet.len() + 2);
    frame.push(END);
    for byte in packet {
        match byte {
            END => frame.extend_from_slice(&[ESC, ESC_END]),
            ESC => frame.extend_from_slice(&[ESC, ESC_ESC]),
            _ => frame.push(byte),
        }
    }
    frame.push(END);
    line.write_all(&frame)?;
    Ok(())
}

fn read_frame<L: Line>(line: &mut L) -> Result<Vec<u8>, Error> {
    let mut frame = Vec::new();
    let mut started = false;
    let mut escaped = false;
    loop {
        let byte = line.read_byte(BYTE_TIMEOUT)?.ok_or(Error::Timeout)?;
        if !started {
            started = byte == END;
            continue;
        }
        if escaped {
            frame.push(match byte {
                ESC_END => END,
                ESC_ESC => ESC,
                _ => return Err(Error::Protocol("invalid SLIP escape")),
            });
            escaped = false;
        } else {
            match byte {
                END if !frame.is_empty() => return Ok(frame),
                END => continue,
                ESC => { escaped = true; continue; }
                _ => frame.push(byte),
            }
        }
        if frame.len() > 4096 {
            return Err(Error::Protocol("reply frame exceeds 4096 bytes"));
        }
    }
}

fn reply<L: Line>(line: &mut L, command: u8) -> Result<u32, Error> {
    loop {
        let frame = read_frame(line)?;
        if frame.len() < 8 || frame[0] != 1 {
            return Err(Error::Protocol("invalid reply header"));
        }
        if frame[1] != command {
            continue;
        }
        let size = u16::from_le_bytes([frame[2], frame[3]]) as usize;
        if size != frame.len() - 8 {
            return Err(Error::Protocol("reply size does not match its header"));
        }
        // The legacy probe's SYNC/READ_REG replies have no payload before
        // status ("Serial Protocol", "Status Bytes" and the command table).
        if size != 0 && frame[8] != 0 {
            return Err(Error::Protocol("ROM command reported failure"));
        }
        return Ok(u32::from_le_bytes([frame[4], frame[5], frame[6], frame[7]]));
    }
}

fn reset_loader<L: Line>(line: &mut L) -> Result<(), Error> {
    use Step::*;
    line.sequence(&[
        Dtr(false), Rts(true), Wait(Duration::from_millis(100)),
        Dtr(true), Rts(false), Wait(Duration::from_millis(50)),
        Dtr(false), Drain,
    ]).map_err(|(_, err)| err)?;
    Ok(())
}

fn run_firmware<L: Line>(line: &mut L) -> Result<(), Error> {
    use Step::*;
    line.sequence(&[Dtr(false), Rts(true), Wait(Duration::from_millis(100)), Rts(false)])
        .map_err(|(_, err)| err)?;
    Ok(())
}

fn identify_rom<L: Line>(line: &mut L) -> Result<EspRom, Error> {
    let mut data = [0x55; 36];
    data[..4].copy_from_slice(&[0x07, 0x07, 0x12, 0x20]);
    let mut synced = false;
    for _ in 0..7 {
        request(line, SYNC, &data)?;
        match reply(line, SYNC) {
            Ok(_) => { synced = true; break; }
            Err(Error::Timeout) => {}
            Err(err) => return Err(err),
        }
    }
    if !synced {
        return Err(Error::NoSync);
    }
    // The ROM sends several replies for a single SYNC.
    line.sleep(Duration::from_millis(100));
    line.drain_input()?;
    request(line, READ_REG, &MAGIC_ADDRESS.to_le_bytes())?;
    Ok(EspRom { magic: reply(line, READ_REG)? })
}

/// Reset into the ROM, sync, read the family id word, then run the firmware.
/// Firmware reset is also attempted if probing fails.
pub fn probe<L: Line>(line: &mut L) -> Result<EspRom, Error> {
    line.set_baud(115200)?;
    let result = reset_loader(line).and_then(|()| identify_rom(line));
    let resumed = run_firmware(line);
    let rom = result?;
    resumed?;
    Ok(rom)
}

// "Serial Protocol", "Supported by Stub Loader and ROM Loader".
const FLASH_BEGIN: u8 = 0x02;
const FLASH_DATA: u8 = 0x03;
const SPI_SET_PARAMS: u8 = 0x0b;
const SPI_ATTACH: u8 = 0x0d;
const SPI_FLASH_MD5: u8 = 0x13;
const GET_SECURITY_INFO: u8 = 0x14;

/// W25Q128JV, section 2 "Features" and section 1 "General Descriptions":
/// 256-byte pages, 4 KiB sectors, 64 KiB blocks (128 Mbit / 16 MiB chip).
const PAGE_SIZE: u32 = 256;
const SECTOR_SIZE: u32 = 4096;
const BLOCK_SIZE: u32 = 65536;
/// Lead choice: 1024-byte packets; "Serial Protocol", "Writing Data" only
/// requires the block to fit the device's RAM.
const PACKET_SIZE: u32 = 1024;
/// W25Q128JV, section 9.6 "AC Electrical Characteristics": maximum tPP and tSE.
const PAGE_PROGRAM_MAX: Duration = Duration::from_millis(3);
const SECTOR_ERASE_MAX: Duration = Duration::from_millis(400);
/// Lead choice: one second for transport overhead in addition to flash time.
const TRANSPORT_ALLOWANCE: Duration = Duration::from_secs(1);
/// Lead choice: report unbounded MD5 progress every five seconds of line time.
const MD5_PROGRESS: Duration = Duration::from_secs(5);

/// "Serial Protocol", "ROM Loader Errors" (the entire ROM table).
fn rom_error(code: u8) -> Error {
    let meaning = match code {
        0x00 => "Undefined errors",
        0x01 => "The input parameter is invalid",
        0x02 => "Failed to malloc memory from system",
        0x03 => "Failed to send out message",
        0x04 => "Failed to receive message",
        0x05 => "The format of the received message is invalid",
        0x06 => "Message is ok, but the running result is wrong",
        0x07 => "Checksum error",
        0x08 => "Flash write error",
        0x09 => "Flash read error",
        0x0a => "Flash read length error",
        0x0b => "Deflate failed error",
        0x0c => "Deflate Adler32 error",
        0x0d => "Deflate parameter error",
        0x0e => "Invalid RAM binary size",
        0x0f => "Invalid RAM binary address",
        0x64 => "Invalid parameter",
        0x65 => "Invalid format",
        0x66 => "Description too long",
        0x67 => "Bad encoding description",
        0x69 => "Insufficient storage",
        _ => "Unknown ROM loader error",
    };
    Error::Rom { code, meaning }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecurityInfo {
    pub flags: u32,
    pub flash_crypt_cnt: u8,
    pub key_purposes: [u8; 7],
    pub chip_id: u32,
    pub eco_version: u32,
}

impl fmt::Display for SecurityInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "flags=0x{:08x} flash_crypt_cnt={} key_purposes={:?} chip_id=0x{:08x} eco_version={}",
            self.flags, self.flash_crypt_cnt, self.key_purposes, self.chip_id, self.eco_version)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResetReport {
    PortStayed,
    /// One-based step number in TRM Table 33.4-3 or 33.4-4.
    PortDropped { step: usize },
    /// Every step was sent, then the port went away before the first SYNC reply.
    PortDroppedLater,
    /// No reset was sent: the chip was already in the ROM loader ([`Entry::NoReset`]).
    Skipped,
}

impl fmt::Display for ResetReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PortStayed => f.write_str("port stayed"),
            Self::PortDropped { step } => write!(f, "port dropped at step {step}"),
            Self::PortDroppedLater => f.write_str("port dropped after the sequence"),
            Self::Skipped => f.write_str("not sent"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectionReport {
    pub download: ResetReport,
    /// None when the command stayed in the ROM loader.
    pub boot: Option<ResetReport>,
}

/// How a read-only command ends: boot the flash (TRM 33.4.2 Table 33.4-4), or stay in the ROM
/// loader. Lead choice: staying lets the next command start without leaving the loader, which
/// matters for a board whose own firmware cannot be reset back into it (a USB-OTG device that
/// ignores the reset lines: only the BOOT strap brings it back).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Exit { Boot, Stay }

/// How a command reaches the ROM loader: the download reset (TRM 33.4.2 Table 33.4-3), or none
/// for a chip already in the loader (entered by the BOOT strap or by its own firmware). Lead
/// choice: a loader that is not on the USB-Serial-JTAG port may not take the reset lines as the
/// table does, so a chip already there is not reset again.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Entry { Reset, NoReset }

fn exit<L: Line>(line: &mut L, exit: Exit) -> Result<Option<ResetReport>, Error> {
    match exit {
        Exit::Boot => usb_reset(line, false).map(Some),
        Exit::Stay => {
            println!("esp-rom: stayed in the ROM loader");
            Ok(None)
        }
    }
}

/// Leave the ROM loader by the boot reset alone (TRM 33.4.2 Table 33.4-4), without a connection.
pub fn esp_boot<L: Line>(line: &mut L) -> Result<ResetReport, Error> {
    usb_reset(line, false)
}

#[derive(Debug, Clone)]
pub struct Grant {
    pub device: String,
    pub said_by: String,
}

/// Explicit reason for writing without a verified flash backup.
#[derive(Debug, Clone)]
pub struct Waiver {
    pub reason: String,
}

#[derive(Debug, Clone)]
pub struct EspRegion {
    pub offset: u32,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct EspWrite {
    pub regions: Vec<EspRegion>,
    pub flash_size: u32,
    pub expect_chip_id: u32,
    pub grant: Grant,
    pub waiver: Waiver,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegionMd5 {
    pub offset: u32,
    pub size: u32,
    pub md5: [u8; 16],
}

#[derive(Debug)]
pub struct EspReport {
    pub security: SecurityInfo,
    pub before: [u8; 16],
    pub regions: Vec<RegionMd5>,
    /// The offsets of the regions the chip already held, so they were not written.
    pub unchanged: Vec<u32>,
    pub connection: ConnectionReport,
}

#[derive(Debug)]
pub struct EspInfoReport {
    pub security: SecurityInfo,
    pub connection: ConnectionReport,
}

#[derive(Debug)]
pub struct EspRegReport {
    pub security: SecurityInfo,
    pub download: ResetReport,
    /// One value per requested address, in the order asked.
    pub values: Vec<u32>,
}

#[derive(Debug)]
pub struct ForceDownloadReport {
    pub security: SecurityInfo,
    pub download: ResetReport,
    /// RTC_CNTL_OPTION1 before and after; no write is sent when the bit was already clear.
    pub before: u32,
    pub after: u32,
}

#[derive(Debug)]
pub struct EspMd5Report {
    pub security: SecurityInfo,
    pub region: RegionMd5,
    pub connection: ConnectionReport,
}

/// ESP32-S3 TRM, 33.3.2 Table 33.3-2 and 33.4.2 Tables 33.4-3 / 33.4-4.
/// `true` asserts the CDC control-line bit. No inter-step sleeps are documented.
/// Lead choice: on I/O failure at or after reset, stop the sequence and reopen.
fn usb_reset<L: Line>(line: &mut L, download: bool) -> Result<ResetReport, Error> {
    use Step::{Dtr, Rts};
    let download_steps = [Dtr(false), Rts(false), Dtr(true), Rts(false), Rts(true), Dtr(false), Rts(true), Rts(false)];
    let boot_steps = [Dtr(false), Rts(false), Rts(true), Rts(false)];
    let (steps, reset_step): (&[Step], usize) = if download {
        (&download_steps, 6)
    } else {
        (&boot_steps, 3)
    };
    let mut report = ResetReport::PortStayed;
    if let Err((index, err)) = line.sequence(steps) {
        if index + 1 < reset_step { return Err(err.into()); }
        line.reopen()?;
        report = ResetReport::PortDropped { step: index + 1 };
    }
    println!("esp-rom: {} reset: {report}", if download { "download" } else { "boot" });
    Ok(report)
}

enum ReplyWait {
    Byte { deadline: Option<Instant> },
    Until(Instant),
    Md5 { bytes: u32, start: Instant, next_message: Instant },
}

impl ReplyWait {
    fn byte<L: Line>(&mut self, line: &mut L) -> Result<u8, Error> {
        loop {
            let now = line.now();
            let deadline = match self {
                Self::Byte { deadline } => *deadline,
                Self::Until(deadline) => Some(*deadline),
                Self::Md5 { bytes, start, next_message } => {
                    if now >= *next_message {
                        eprintln!("esp-rom: waiting for MD5 of {bytes} bytes ({} s)",
                            now.duration_since(*start).as_secs());
                        *next_message = now + MD5_PROGRESS;
                    }
                    None
                }
            };
            let timeout = match deadline {
                Some(deadline) if now >= deadline => return Err(Error::Timeout),
                Some(deadline) => BYTE_TIMEOUT.min(deadline.duration_since(now)),
                None => BYTE_TIMEOUT,
            };
            if let Some(byte) = line.read_byte(timeout)? { return Ok(byte); }
            if matches!(self, Self::Byte { .. }) { return Err(Error::Timeout); }
        }
    }
}

/// "Serial Protocol", "Low Level Protocol" and "Response Packet".
fn s3_frame<L: Line>(line: &mut L, wait: &mut ReplyWait) -> Result<Vec<u8>, Error> {
    let mut frame = Vec::new();
    let mut started = false;
    let mut escaped = false;
    loop {
        let byte = wait.byte(line)?;
        if !started { started = byte == END; continue; }
        if escaped {
            frame.push(match byte {
                ESC_END => END,
                ESC_ESC => ESC,
                _ => return Err(Error::Protocol("invalid SLIP escape")),
            });
            escaped = false;
        } else {
            match byte {
                END if !frame.is_empty() => return Ok(frame),
                END => continue,
                ESC => { escaped = true; continue; }
                _ => frame.push(byte),
            }
        }
        // Header plus the entire range of the documented u16 Size field.
        if frame.len() > 8 + u16::MAX as usize {
            return Err(Error::Protocol("reply exceeds the Size field's range"));
        }
    }
}

/// "Serial Protocol", "Response Packet", "Status Bytes" and the instruction
/// after "Stub Loader Status & Error" to keep reading until Command matches.
fn s3_reply<L: Line>(line: &mut L, command: u8, mut wait: ReplyWait) -> Result<(u32, Vec<u8>), Error> {
    s3_reply_inner(line, command, &mut wait, false)
}

fn s3_reply_inner<L: Line>(line: &mut L, command: u8, wait: &mut ReplyWait, ordered: bool) -> Result<(u32, Vec<u8>), Error> {
    loop {
        let frame = s3_frame(line, wait)?;
        if frame.len() < 8 || frame[0] != 1 {
            return Err(Error::Protocol("invalid reply header"));
        }
        let size = u16::from_le_bytes([frame[2], frame[3]]) as usize;
        if size != frame.len() - 8 || size < 4 {
            return Err(Error::Protocol("S3 reply must contain its declared data and four status bytes"));
        }
        if frame[1] != command {
            if ordered { return Err(Error::Protocol("SPI1 batch reply command is out of order")); }
            continue;
        }
        let status = frame.len() - 4;
        match frame[status] {
            0 => {}
            1 => return Err(rom_error(frame[status + 1])),
            _ => return Err(Error::Protocol("invalid ROM status flag")),
        }
        // The final two reserved bytes are not interpreted as status.
        let value = u32::from_le_bytes([frame[4], frame[5], frame[6], frame[7]]);
        return Ok((value, frame[8..status].to_vec()));
    }
}

/// One SYNC attempt: drain stale input first when asked, send, wait for its reply. A frame before
/// that reply that is no reply at all is not the loader's: the USB Serial/JTAG port stays through
/// the reset, so what the firmware sent just before it can still come in after the drain (a board
/// reporting over the watch link did, 11-18). It is skipped and said, and the wait goes on to the
/// deadline.
fn sync_once<L: Line>(line: &mut L, data: &[u8; 36], deadline: Instant, drain: bool) -> Result<(), Error> {
    if drain { line.drain_input()?; }
    request(line, SYNC, data)?;
    loop {
        match s3_reply(line, SYNC, ReplyWait::Byte { deadline: Some(deadline) }) {
            Err(Error::Protocol(why)) => println!("esp-rom: sync: skipped a frame that is not the loader's ({why})"),
            reply => return reply.map(drop),
        }
    }
}

fn words(values: &[u32]) -> Vec<u8> {
    values.iter().flat_map(|value| value.to_le_bytes()).collect()
}

/// "Serial Protocol", "Supported by Stub Loader and ROM Loader", GET_SECURITY_INFO: exactly 20
/// bytes before the four status bytes.
fn read_security<L: Line>(line: &mut L) -> Result<SecurityInfo, Error> {
    request(line, GET_SECURITY_INFO, &[])?;
    let (_, data) = s3_reply(line, GET_SECURITY_INFO, ReplyWait::Byte { deadline: None })?;
    if data.len() != 20 {
        return Err(Error::Protocol("GET_SECURITY_INFO must return exactly 20 data bytes"));
    }
    let mut key_purposes = [0; 7];
    key_purposes.copy_from_slice(&data[5..12]);
    let info = SecurityInfo {
        flags: u32::from_le_bytes(data[0..4].try_into().unwrap()),
        flash_crypt_cnt: data[4],
        key_purposes,
        chip_id: u32::from_le_bytes(data[12..16].try_into().unwrap()),
        eco_version: u32::from_le_bytes(data[16..20].try_into().unwrap()),
    };
    println!("esp-rom: {info}");
    Ok(info)
}

/// Right after the sync reply: the chip's security info and, on an S3, its RTC and super watchdogs
/// stopped. A watchdog the board's firmware armed lives in the RTC domain through the download
/// reset (a Core Reset, TRM 7.1.3) and resets the ROM loader once its hold runs out, about 100 ms
/// for our boot stage's (seen on the voicebox S3, 2026-11-18: `rst:0x9 (RTCWDT_SYS_RST)` again and
/// again in download mode, its flash commands timing out). So the first batch unlocks and feeds
/// the RTC watchdog (its count starts again at once) and reads both configs; the second clears
/// the RTC watchdog's enable and flash-boot mode, relocks it, and disables the super watchdog.
/// Both stay stopped until the next power-on or until firmware arms them again.
fn identify_and_quiet<L: Line>(line: &mut L) -> Result<SecurityInfo, Error> {
    let security = read_security(line)?;
    if security.chip_id != S3_CHIP_ID {
        return Ok(security);
    }
    let write = |address: u32, value: u32| (WRITE_REG, words(&[address, value, u32::MAX, 0]));
    let read = |address: u32| (READ_REG, words(&[address]));
    let got = batch(line, &[
        write(RWDT_PROTECT, WDT_KEY), write(RWDT_FEED, RWDT_FEED_BIT), read(RWDT_CONFIG),
        write(SWD_PROTECT, SWD_KEY), read(SWD_CONFIG),
    ])?;
    let (rwdt, swd) = (got[2], got[4]);
    batch(line, &[write(RWDT_CONFIG, rwdt & !(RWDT_EN | RWDT_FLASHBOOT)), write(RWDT_PROTECT, 0), write(SWD_CONFIG, swd | SWD_DISABLE)])?;
    println!("esp-rom: watchdogs stopped: RTC 0x{rwdt:08x} (enabled {}), super 0x{swd:08x} (disabled {})",
        rwdt & RWDT_EN != 0, swd & SWD_DISABLE != 0);
    Ok(security)
}

/// Sends every request, then consumes every reply in order: one round trip for the lot.
fn batch<L: Line>(line: &mut L, requests: &[(u8, Vec<u8>)]) -> Result<Vec<u32>, Error> {
    let mut sent = 0;
    let mut failure = None;
    for (command, data) in requests {
        match request(line, *command, data) {
            Ok(()) => sent += 1,
            Err(err) => { failure = Some(err); break; }
        }
    }
    let mut values = Vec::with_capacity(sent);
    // Consume every sent request's reply, including after a ROM failure,
    // before restoration can issue any further commands. Unlike the
    // single-command path, a batch must never skip an unexpected command.
    for (command, _) in &requests[..sent] {
        match s3_reply_inner(line, *command, &mut ReplyWait::Byte { deadline: None }, true) {
            Ok((value, _)) => values.push(value),
            Err(err) => { if failure.is_none() { failure = Some(err); } }
        }
    }
    match failure {
        Some(err) => Err(err),
        None => Ok(values),
    }
}
struct S3Client<'a, L: Line> {
    line: &'a mut L,
    /// GET_SECURITY_INFO as the chip answered it at connect.
    security: SecurityInfo,
}

impl<'a, L: Line> S3Client<'a, L> {
    /// "Serial Protocol", "Initialization" and "Tracing Esptool Serial
    /// Communications": SYNC payload and multiple replies. No baud command:
    /// TRM 33.3.2 Table 33.3-1 says USB Serial/JTAG ignores line coding.
    fn connect(line: &'a mut L, entry: Entry) -> Result<(Self, ResetReport), Error> {
        let mut download = match entry {
            Entry::Reset => usb_reset(line, true)?,
            Entry::NoReset => {
                println!("esp-rom: download reset: {}", ResetReport::Skipped);
                ResetReport::Skipped
            }
        };
        let deadline = line.now() + CONNECT_WINDOW;
        let mut data = [0x55; 36];
        data[..4].copy_from_slice(&[0x07, 0x07, 0x12, 0x20]);
        let mut drain = true;
        // the error of the last exchange a reset cut short, if the window ends after it
        let mut cut: Option<Error> = None;
        let security = loop {
            if line.now() >= deadline { return Err(cut.unwrap_or(Error::NoSync)); }
            match sync_once(line, &data, deadline, drain) {
                Ok(()) => match identify_and_quiet(line) {
                    Ok(security) => break security,
                    // the chip reset under the exchange (a watchdog its firmware armed): sync again
                    Err(Error::Timeout) => {
                        cut = Some(Error::Timeout);
                        drain = true;
                    }
                    Err(err) => return Err(err),
                },
                Err(Error::Timeout) => drain = false,
                // A stalled write is a device that does not read, not a port that left.
                Err(Error::Io(err)) if err.kind() == io::ErrorKind::TimedOut => return Err(Error::NotLoader),
                // Lead choice: the USB device may leave a moment after the reset step instead of
                // at it, so every step succeeds and the next I/O fails. Reopen once, then sync.
                Err(Error::Io(_)) if download == ResetReport::PortStayed => {
                    line.reopen()?;
                    download = ResetReport::PortDroppedLater;
                    println!("esp-rom: download reset: {download}");
                    drain = true;
                }
                Err(err) => return Err(err),
            }
        };
        // Consume anything still arriving until the existing byte timeout is quiet.
        while line.read_byte(BYTE_TIMEOUT)?.is_some() {}
        line.drain_input()?;
        Ok((Self { line, security }, download))
    }

    fn command(&mut self, command: u8, data: &[u8]) -> Result<Vec<u8>, Error> {
        request(self.line, command, data)?;
        let (_, data) = s3_reply(self.line, command, ReplyWait::Byte { deadline: None })?;
        Ok(data)
    }

    /// serial-protocol.txt, "Supported by Stub Loader and ROM Loader", READ_REG:
    /// one address word; the register value is in the response header.
    fn read_reg(&mut self, address: u32) -> Result<u32, Error> {
        request(self.line, READ_REG, &words(&[address]))?;
        let (value, _) = s3_reply(self.line, READ_REG, ReplyWait::Byte { deadline: None })?;
        Ok(value)
    }

    /// serial-protocol.txt, same table, WRITE_REG: address, value, mask, delay.
    /// Lead choice: full mask and zero delay; the next command is a separate
    /// round trip, except in the explicitly pipelined SPI1 read below.
    fn write_reg(&mut self, address: u32, value: u32) -> Result<(), Error> {
        self.command(WRITE_REG, &words(&[address, value, u32::MAX, 0]))?;
        Ok(())
    }

    fn register_batch(&mut self, requests: &[(u8, Vec<u8>)]) -> Result<Vec<u32>, Error> {
        batch(self.line, requests)
    }


    /// "Serial Protocol", "SPI Configuration Commands" and the command table.
    fn attach(&mut self, flash_size: u32) -> Result<(), Error> {
        self.command(SPI_ATTACH, &words(&[0, 0]))?;
        // Lead choice: id 0 and status mask 0xFFFF; accepted only if the ROM's
        // status reply succeeds. The document says only total size is variable
        // and most of these parameters are unused. Geometry: W25Q128JV §1/§2.
        self.command(SPI_SET_PARAMS, &words(&[0, flash_size, BLOCK_SIZE, SECTOR_SIZE, PAGE_SIZE, 0xffff]))?;
        Ok(())
    }

    /// "Serial Protocol", "Verifying Uploaded Data" and SPI_FLASH_MD5.
    /// No document bounds this operation: keep waiting while the line is alive.
    fn flash_md5(&mut self, offset: u32, size: u32) -> Result<[u8; 16], Error> {
        request(self.line, SPI_FLASH_MD5, &words(&[offset, size, 0, 0]))?;
        let start = self.line.now();
        let (_, data) = s3_reply(self.line, SPI_FLASH_MD5, ReplyWait::Md5 {
            bytes: size, start, next_message: start + MD5_PROGRESS,
        })?;
        if data.len() != 32 {
            return Err(Error::Protocol("SPI_FLASH_MD5 must return 32 hex ASCII characters"));
        }
        let nibble = |byte| match byte {
            b'0'..=b'9' => Ok(byte - b'0'),
            b'a'..=b'f' => Ok(byte - b'a' + 10),
            b'A'..=b'F' => Ok(byte - b'A' + 10),
            _ => Err(Error::Protocol("SPI_FLASH_MD5 contains non-hex ASCII")),
        };
        let mut digest = [0; 16];
        for (out, pair) in digest.iter_mut().zip(data.chunks_exact(2)) {
            *out = (nibble(pair[0])? << 4) | nibble(pair[1])?;
        }
        Ok(digest)
    }

    /// "Serial Protocol", "Writing Data", "Checksum" and FLASH_BEGIN/DATA.
    /// W25Q128JV §9.6 bounds erase and program waits; transport is a lead choice.
    /// `sent(done, size)` after each packet's reply.
    fn write_region(&mut self, region: &EspRegion, sent: &mut dyn FnMut(u32, u32)) -> Result<RegionMd5, Error> {
        let size = u32::try_from(region.bytes.len()).map_err(|_| Error::Plan("region exceeds u32 size".into()))?;
        let erase = erase_size(size)?;
        let packets = size.div_ceil(PACKET_SIZE);
        request(self.line, FLASH_BEGIN, &words(&[erase, packets, PACKET_SIZE, region.offset, 0]))?;
        let deadline = self.line.now() + SECTOR_ERASE_MAX * (erase / SECTOR_SIZE) + TRANSPORT_ALLOWANCE;
        s3_reply(self.line, FLASH_BEGIN, ReplyWait::Until(deadline))?;
        for (sequence, chunk) in region.bytes.chunks(PACKET_SIZE as usize).enumerate() {
            let mut data = words(&[PACKET_SIZE, sequence as u32, 0, 0]);
            data.extend_from_slice(chunk);
            data.resize(16 + PACKET_SIZE as usize, 0xff);
            let checksum = data[16..].iter().fold(0xefu8, |checksum, byte| checksum ^ byte);
            request_checksum(self.line, FLASH_DATA, &data, u32::from(checksum))?;
            let deadline = self.line.now() + PAGE_PROGRAM_MAX * (PACKET_SIZE / PAGE_SIZE) + TRANSPORT_ALLOWANCE;
            s3_reply(self.line, FLASH_DATA, ReplyWait::Until(deadline))?;
            sent((sequence as u32 * PACKET_SIZE + chunk.len() as u32).min(size), size);
        }
        let want = md5::digest(&region.bytes);
        let got = self.flash_md5(region.offset, size)?;
        if want != got {
            return Err(Error::Verify { offset: region.offset, size, want, got });
        }
        println!("esp-rom: region 0x{:08x} ({size} bytes) verified MD5 {}", region.offset, md5::hex(&got));
        Ok(RegionMd5 { offset: region.offset, size, md5: got })
    }
}

fn erase_size(size: u32) -> Result<u32, Error> {
    size.div_ceil(SECTOR_SIZE).checked_mul(SECTOR_SIZE)
        .ok_or_else(|| Error::Plan("rounded erase size exceeds u32".into()))
}

fn check_range(offset: u32, size: u32, flash_size: u32) -> Result<(), Error> {
    if flash_size == 0 || offset.checked_add(size).is_none_or(|end| end > flash_size) {
        return Err(Error::Plan(format!("region 0x{offset:08x} ({size} bytes) ends past flash size {flash_size}")));
    }
    Ok(())
}

fn check_plan(plan: &EspWrite) -> Result<(), Error> {
    if plan.grant.device.trim().is_empty() || plan.grant.said_by.trim().is_empty() || plan.waiver.reason.trim().is_empty() {
        return Err(Error::Plan("a device, grant author and no-backup reason are required".into()));
    }
    if plan.regions.is_empty() { return Err(Error::Plan("no regions supplied".into())); }
    let mut ranges = Vec::with_capacity(plan.regions.len());
    for region in &plan.regions {
        if region.offset % SECTOR_SIZE != 0 {
            return Err(Error::Plan(format!("region 0x{:08x} is not 4 KiB aligned", region.offset)));
        }
        let size = u32::try_from(region.bytes.len()).map_err(|_| Error::Plan("region exceeds u32 size".into()))?;
        if size == 0 { return Err(Error::Plan(format!("region 0x{:08x} is empty", region.offset))); }
        // Check the whole erase footprint too, so rounding cannot erase beyond
        // flash_size or another region. Packet padding fits within that footprint.
        let erase = erase_size(size)?;
        check_range(region.offset, erase, plan.flash_size)?;
        ranges.push((region.offset, region.offset + erase));
    }
    ranges.sort_unstable();
    for pair in ranges.windows(2) {
        if pair[0].1 > pair[1].0 {
            return Err(Error::Plan(format!("regions 0x{:08x} and 0x{:08x} overlap", pair[0].0, pair[1].0)));
        }
    }
    Ok(())
}

/// Identify the S3 via "Serial Protocol", "Initialization - Chip Type Detection",
/// then reset to flash boot per ESP32-S3 TRM 33.4.2 Table 33.4-4. Writes no flash (on an S3 it stops
/// the watchdogs, `identify_and_quiet`).
pub fn esp_info<L: Line>(line: &mut L, entry: Entry, leave: Exit) -> Result<EspInfoReport, Error> {
    let (client, download) = S3Client::connect(line, entry)?;
    let security = client.security.clone();
    let boot = exit(client.line, leave)?;
    Ok(EspInfoReport { security, connection: ConnectionReport { download, boot } })
}

/// Flash fingerprint, writing no flash (on an S3 it stops the watchdogs, `identify_and_quiet`):
/// "Serial Protocol", "SPI Configuration Commands"
/// and "Verifying Uploaded Data"; boot reset: ESP32-S3 TRM 33.4.2 Table 33.4-4.
pub fn esp_md5<L: Line>(line: &mut L, entry: Entry, offset: u32, size: u32, flash_size: u32, leave: Exit) -> Result<EspMd5Report, Error> {
    let (mut client, download) = S3Client::connect(line, entry)?;
    let security = client.security.clone();
    check_range(offset, size, flash_size)?;
    client.attach(flash_size)?;
    let md5 = client.flash_md5(offset, size)?;
    println!("esp-rom: region 0x{offset:08x} ({size} bytes) MD5 {}", md5::hex(&md5));
    let boot = exit(client.line, leave)?;
    Ok(EspMd5Report { security, region: RegionMd5 { offset, size, md5 }, connection: ConnectionReport { download, boot } })
}

/// Read registers through the ROM loader ("Serial Protocol", READ_REG) and stay in it. Writes no
/// flash (on an S3 it stops the watchdogs first, `identify_and_quiet`).
/// Lead choice: the caller names plain status and configuration registers. A FIFO register pops
/// a value when read, so this is not a memory dump; addresses must be word aligned.
pub fn esp_read_reg<L: Line>(line: &mut L, entry: Entry, addresses: &[u32]) -> Result<EspRegReport, Error> {
    if let Some(address) = addresses.iter().find(|address| *address % 4 != 0) {
        return Err(Error::Plan(format!("register 0x{address:08x} is not word aligned")));
    }
    if addresses.is_empty() { return Err(Error::Plan("no registers named".into())); }
    let (mut client, download) = S3Client::connect(line, entry)?;
    let security = client.security.clone();
    let mut values = Vec::with_capacity(addresses.len());
    for &address in addresses {
        let value = client.read_reg(address)?;
        println!("esp-rom: register 0x{address:08x} = 0x{value:08x}");
        values.push(value);
    }
    exit(client.line, Exit::Stay)?;
    Ok(EspRegReport { security, download, values })
}

/// The S3's chip id in GET_SECURITY_INFO, as the voicebox S3's ROM loader answered (2026-11-17:
/// chip_id 9). The watchdog addresses below are the S3's, so they are written on it only.
const S3_CHIP_ID: u32 = 9;
// The RTC watchdog: IDF register/soc/rtc_cntl_reg.h:1988–2115 (WDTCONFIG0, WDTWPROTECT, their
// fields); unlock procedure facts.md:115–131. The same writes Fab's S3 boot stage makes
// (emit_s3/src/lib.rs, `RWDT_*`): unlock, feed, clear enable and flash-boot mode, relock.
const RWDT_CONFIG: u32 = 0x6000_8098;
const RWDT_FEED: u32 = 0x6000_80ac;
const RWDT_FEED_BIT: u32 = 1 << 31; // rtc_cntl_reg.h:2101–2107.
const RWDT_PROTECT: u32 = 0x6000_80b0;
const WDT_KEY: u32 = 0x50d8_3aa1;
const RWDT_EN: u32 = 1 << 31;
const RWDT_FLASHBOOT: u32 = 1 << 12;
// The super watchdog: rtc_cntl_reg.h:2117–2173; facts.md:132: unlock, disable, no assumed relock
// (as the boot stage's `SWD_*`).
const SWD_CONFIG: u32 = 0x6000_80b4;
const SWD_PROTECT: u32 = 0x6000_80b8;
const SWD_KEY: u32 = 0x8f1d_312a;
const SWD_DISABLE: u32 = 1 << 30;

// ESP-IDF v6.1.1 esp32s3/register/soc/reg_base.h:14 DR_REG_RTCCNTL_BASE and rtc_cntl_reg.h:3209-3212
// RTC_CNTL_OPTION1_REG, RTC_CNTL_FORCE_DOWNLOAD_BOOT (bit 0, the register's only field).
const RTC_CNTL_OPTION1: u32 = 0x6000812c;
const FORCE_DOWNLOAD_BOOT: u32 = 1;

/// Clear RTC_CNTL_FORCE_DOWNLOAD_BOOT, read it back, and stay in the ROM loader. TRM 8.2: while
/// it is set, a CPU reset overwrites GPIO_STRAPPING[3:2] with 00 (Joint Download Boot), and a
/// Core Reset such as the USB-Serial-JTAG one (TRM Table 7.1-1, 0x15) leaves RTC registers as they
/// are (TRM 7.1.3), so no reset clears it. The chip id is checked before the write, since the
/// address is the S3's. Bits 31:1 are reserved (TRM Register 10.58) and are written back as read.
pub fn esp_clear_force_download<L: Line>(line: &mut L, entry: Entry, expect_chip_id: u32) -> Result<ForceDownloadReport, Error> {
    let (mut client, download) = S3Client::connect(line, entry)?;
    let security = client.security.clone();
    if security.chip_id != expect_chip_id {
        return Err(Error::WrongChip { want: expect_chip_id, got: security.chip_id });
    }
    let before = client.read_reg(RTC_CNTL_OPTION1)?;
    println!("esp-rom: RTC_CNTL_OPTION1 0x{before:08x} (FORCE_DOWNLOAD_BOOT {})", before & FORCE_DOWNLOAD_BOOT);
    let mut after = before;
    if before & FORCE_DOWNLOAD_BOOT != 0 {
        let wrote = before & !FORCE_DOWNLOAD_BOOT;
        client.write_reg(RTC_CNTL_OPTION1, wrote)?;
        after = client.read_reg(RTC_CNTL_OPTION1)?;
        println!("esp-rom: RTC_CNTL_OPTION1 now 0x{after:08x}");
        if after != wrote { return Err(Error::Register { address: RTC_CNTL_OPTION1, wrote, got: after }); }
    }
    exit(client.line, Exit::Stay)?;
    Ok(ForceDownloadReport { security, download, before, after })
}

/// What a write is doing, for a person watching it ([`esp_write`]'s `watch`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WriteStep {
    /// In the ROM loader, the chip id checked and the flash attached.
    Connected,
    /// The whole flash's MD5 from before the write is taken (seconds on a 16 MB chip).
    Fingerprinted,
    /// Region `index` of `of`: `done` of its `size` bytes sent.
    Writing { index: usize, of: usize, done: u32, size: u32 },
    /// Region `index`'s flash MD5 matches the image.
    Verified { index: usize },
    /// Region `index` already held the image's bytes (its flash MD5 matched), so it was not
    /// erased or written.
    Unchanged { index: usize },
    /// Out of the loader: the image boots.
    Booting,
}

/// Verify chip id and all regions before erasing; fingerprint the whole flash,
/// write and verify each region the chip does not already hold, then boot, telling `watch` each step. "Serial Protocol", "Writing Data"
/// and "Verifying Uploaded Data". Lead choices: grant and backup waiver gates,
/// 4 KiB aligned offsets, sector-rounded erase sizes and 1024-byte packets.
/// FLASH_END is unnecessary while staying in the loader; exit uses TRM 33.4.2
/// Table 33.4-4. A failed operation returns immediately without booting a partial image.
pub fn esp_write<L: Line>(line: &mut L, plan: &EspWrite, watch: &mut dyn FnMut(WriteStep)) -> Result<EspReport, Error> {
    let (mut client, download) = S3Client::connect(line, Entry::Reset)?;
    let security = client.security.clone();
    if security.chip_id != plan.expect_chip_id {
        return Err(Error::WrongChip { want: plan.expect_chip_id, got: security.chip_id });
    }
    check_plan(plan)?;
    client.attach(plan.flash_size)?;
    watch(WriteStep::Connected);
    let before = client.flash_md5(0, plan.flash_size)?;
    println!("esp-rom: before MD5 {} ({} bytes)", md5::hex(&before), plan.flash_size);
    io::stdout().flush()?;
    watch(WriteStep::Fingerprinted);
    let mut regions = Vec::with_capacity(plan.regions.len());
    let mut unchanged = Vec::new();
    for (index, region) in plan.regions.iter().enumerate() {
        // A region the chip already holds costs its MD5 only: an image whose file bundle did not
        // change rewrites the slot alone. check_plan bounds every size to u32.
        let size = region.bytes.len() as u32;
        let want = md5::digest(&region.bytes);
        if client.flash_md5(region.offset, size)? == want {
            println!("esp-rom: region 0x{:08x} ({size} bytes) unchanged, MD5 {}", region.offset, md5::hex(&want));
            regions.push(RegionMd5 { offset: region.offset, size, md5: want });
            unchanged.push(region.offset);
            watch(WriteStep::Unchanged { index });
            continue;
        }
        regions.push(client.write_region(region, &mut |done, size| watch(WriteStep::Writing { index, of: plan.regions.len(), done, size }))?);
        watch(WriteStep::Verified { index });
    }
    watch(WriteStep::Booting);
    let boot = Some(usb_reset(client.line, false)?);
    Ok(EspReport { security, before, regions, unchanged, connection: ConnectionReport { download, boot } })
}

// ESP-IDF v6.1.1 esp32s3/register/soc/reg_base.h:7, DR_REG_SPI1_BASE.
const SPI1_BASE: u32 = 0x60002000;
// spi_mem_reg.h:15,126,135,336,364,460,474,496,583,886..1013.
const SPI1_CMD: u32 = SPI1_BASE;
const SPI1_ADDR: u32 = SPI1_BASE + 0x04;
const SPI1_CTRL: u32 = SPI1_BASE + 0x08;
const SPI1_CLOCK: u32 = SPI1_BASE + 0x14;
const SPI1_USER: u32 = SPI1_BASE + 0x18;
const SPI1_USER1: u32 = SPI1_BASE + 0x1c;
const SPI1_USER2: u32 = SPI1_BASE + 0x20;
const SPI1_MISO_DLEN: u32 = SPI1_BASE + 0x28;
const SPI1_CACHE_FCTRL: u32 = SPI1_BASE + 0x3c;
const SPI1_W0: u32 = SPI1_BASE + 0x58;
// spi_mem_reg.h:110..116: trigger on set, cleared once the operation is done.
const SPI_MEM_USR: u32 = 1 << 18;
// spi_mem_reg.h:365..394, USR_COMMAND / ADDR / DUMMY / MISO / MOSI.
const READ_PHASES: u32 = (1 << 31) | (1 << 30) | (1 << 28);
const UNUSED_READ_PHASES: u32 = (1 << 29) | (1 << 27);
// spi_mem_reg.h:217..246, FCMD_OCT/QUAD/DUAL, FADDR_OCT, FDIN_OCT.
const CTRL_LINE_MODES: u32 = (1 << 9) | (1 << 8) | (1 << 7) | (1 << 6) | (1 << 5);
// spi_mem_reg.h:584..625, FADDR_QUAD/DUAL and FDIN_QUAD/DUAL in CACHE_FCTRL.
// FDOUT_* only affect the disabled MOSI phase (same header:247..252,591..597,612..618).
const CACHE_LINE_MODES: u32 = (1 << 8) | (1 << 6) | (1 << 5) | (1 << 3);
// spi_mem_reg.h:461..486: lengths are bit counts minus one.
const ADDR_BITLEN_MASK: u32 = 0x3f << 26;
const COMMAND_FIELDS: u32 = (0xf << 28) | 0xffff;
// Lead choice: use all sixteen W registers, 64 bytes per SPI transaction.
const SPI1_READ_SIZE: u32 = 64;
// Lead choice: nonzero offsets distinguish the address layouts, spread across
// different flash contents. Offset zero alone cannot distinguish them.
const PROBE_OFFSETS: [u32; 4] = [0x1000, 0x8000, 0x10000, 0x20000];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Layout { Low, High }

impl Layout {
    fn address(self, offset: u32) -> u32 {
        // spi_mem_reg.h:127..133 says only "In user mode, it is the memory
        // address". Lead choice: test low bits and high bits (the latter as
        // GP-SPI2 Table 30.5-10 does for 24 bits); ROM MD5 decides on the chip.
        match self { Self::Low => offset, Self::High => offset << 8 }
    }
}

impl fmt::Display for Layout {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self { Self::Low => "low", Self::High => "high" })
    }
}

#[derive(Clone, Copy)]
struct Spi1Saved {
    ctrl: u32,
    clock: u32,
    user: u32,
    user1: u32,
    user2: u32,
    miso_dlen: u32,
    addr: u32,
    cache_fctrl: u32,
}

impl Spi1Saved {
    fn writable(self) -> [(u32, u32); 5] {
        [(SPI1_USER, self.user), (SPI1_USER1, self.user1), (SPI1_USER2, self.user2),
            (SPI1_MISO_DLEN, self.miso_dlen), (SPI1_ADDR, self.addr)]
    }
}

impl fmt::Display for Spi1Saved {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SPI1 saved: CTRL=0x{:08x} CLOCK=0x{:08x} USER=0x{:08x} USER1=0x{:08x} USER2=0x{:08x} MISO_DLEN=0x{:08x} ADDR=0x{:08x} CACHE_FCTRL=0x{:08x}",
            self.ctrl, self.clock, self.user, self.user1, self.user2, self.miso_dlen, self.addr, self.cache_fctrl)
    }
}

/// A reader borrows an attached, configured client. Every caller runs its work
/// inside a result-producing closure and passes that result to `finish`, so
/// `end` runs before either success or an error escapes. `begin` does the same
/// if configuration fails part way through.
struct Spi1Reader<'a, 'b, L: Line> {
    client: &'a mut S3Client<'b, L>,
    layout: Layout,
    saved: Spi1Saved,
    changed: usize,
}

impl<'a, 'b, L: Line> Spi1Reader<'a, 'b, L> {
    fn begin(client: &'a mut S3Client<'b, L>, layout: Layout) -> Result<Self, Error> {
        let saved = Spi1Saved {
            user: client.read_reg(SPI1_USER)?,
            user1: client.read_reg(SPI1_USER1)?,
            user2: client.read_reg(SPI1_USER2)?,
            miso_dlen: client.read_reg(SPI1_MISO_DLEN)?,
            addr: client.read_reg(SPI1_ADDR)?,
            ctrl: client.read_reg(SPI1_CTRL)?,
            clock: client.read_reg(SPI1_CLOCK)?,
            cache_fctrl: client.read_reg(SPI1_CACHE_FCTRL)?,
        };
        // W25Q128JV §8.2.6: 03h is Standard SPI only. Lead choice: refuse
        // multiple-line command/address/input modes rather than changing them.
        if saved.ctrl & CTRL_LINE_MODES != 0 || saved.cache_fctrl & CACHE_LINE_MODES != 0 {
            return Err(Error::Spi1("0x03 requires Standard SPI; CMD, ADDR or DIN line-mode bits are set"));
        }
        let mut reader = Self { client, layout, saved, changed: 0 };
        // W25Q128JV §8.2.6: command 03h, 24 address bits, then data input.
        // spi_mem_reg.h USR_* fields: keep unrelated USER/USER1/USER2 bits;
        // MISO_DLEN.USR_MISO_DBITLEN (497..502) is 64 * 8 - 1.
        for (address, value) in [
            (SPI1_USER, (saved.user | READ_PHASES) & !UNUSED_READ_PHASES),
            (SPI1_USER1, (saved.user1 & !ADDR_BITLEN_MASK) | (23 << 26)),
            (SPI1_USER2, (saved.user2 & !COMMAND_FIELDS) | (7 << 28) | 0x03),
            (SPI1_MISO_DLEN, SPI1_READ_SIZE * 8 - 1),
        ] {
            // Even a failed reply may follow a write that reached the chip.
            reader.changed += 1;
            if let Err(err) = reader.client.write_reg(address, value) {
                return reader.finish(Err(err));
            }
        }
        Ok(reader)
    }

    fn read(&mut self, offset: u32) -> Result<[u8; 64], Error> {
        let deadline = self.client.line.now() + CONNECT_WINDOW;
        self.changed = 5;
        // Lead choice: pipeline these nineteen requests before reading replies.
        // The ROM processes one command at a time; USB flow control retains
        // unread requests. Every reply must match its request's position.
        let mut requests = vec![
            (WRITE_REG, words(&[SPI1_ADDR, self.layout.address(offset), u32::MAX, 0])),
            (WRITE_REG, words(&[SPI1_CMD, SPI_MEM_USR, u32::MAX, 0])),
            (READ_REG, words(&[SPI1_CMD])),
        ];
        for word in 0..16 { requests.push((READ_REG, words(&[SPI1_W0 + word * 4]))); }
        let values = self.client.register_batch(&requests)?;
        let data = if values[2] & SPI_MEM_USR != 0 {
            // Discard all of this batch's W values if CMD was still busy.
            self.wait_idle(deadline)?;
            let requests: Vec<_> = (0..16).map(|word| (READ_REG, words(&[SPI1_W0 + word * 4]))).collect();
            self.client.register_batch(&requests)?
        } else {
            values[3..].to_vec()
        };
        let mut bytes = [0; 64];
        // Lead choice: W0..W15 in order, each word's little-endian bytes.
        // The probe checks this interpretation against the ROM's flash MD5.
        for (chunk, value) in bytes.chunks_exact_mut(4).zip(data) {
            chunk.copy_from_slice(&value.to_le_bytes());
        }
        Ok(bytes)
    }

    fn wait_idle(&mut self, deadline: Instant) -> Result<(), Error> {
        // spi_mem_reg.h SPI_MEM_USR: "cleared once the operation done".
        // Lead choice: CONNECT_WINDOW of line time bounds a stuck controller;
        // each poll is a round trip, with no arbitrary poll-count limit.
        loop {
            if self.client.line.now() >= deadline {
                return Err(Error::Spi1("controller stuck: SPI_MEM_USR did not clear within CONNECT_WINDOW"));
            }
            request(self.client.line, READ_REG, &words(&[SPI1_CMD]))?;
            let result = s3_reply(self.client.line, READ_REG, ReplyWait::Until(deadline));
            let (value, _) = match result {
                Err(Error::Timeout) => return Err(Error::Spi1("controller stuck: SPI_MEM_USR did not clear within CONNECT_WINDOW")),
                other => other?,
            };
            if value & SPI_MEM_USR == 0 { return Ok(()); }
        }
    }

    fn end(&mut self) -> Result<(), Error> {
        let mut failure = None;
        // Restore in reverse write order, attempting every saved register even
        // if an earlier restore fails. CMD is a self-clearing trigger, not saved
        // configuration; replaying it could trigger another operation.
        for (address, value) in self.saved.writable()[..self.changed].iter().rev() {
            if let Err(err) = self.client.write_reg(*address, *value) {
                if failure.is_none() { failure = Some(err); }
            }
        }
        match failure { Some(err) => Err(err), None => Ok(()) }
    }

    fn finish<T>(mut self, result: Result<T, Error>) -> Result<T, Error> {
        match (result, self.end()) {
            (Ok(value), Ok(())) => Ok(value),
            (Err(err), Ok(())) | (Ok(_), Err(err)) => Err(err),
            (Err(operation), Err(restore)) => Err(Error::Restore {
                operation: Box::new(operation), restore: Box::new(restore),
            }),
        }
    }
}

fn check_read_size(flash_size: u32) -> Result<(), Error> {
    // W25Q128JV §8.2.6's 24-bit address phase cannot address beyond 16 MiB.
    if flash_size == 0 || flash_size > 1 << 24 {
        return Err(Error::Plan("0x03 requires a nonzero flash size within its 24-bit address range".into()));
    }
    Ok(())
}

/// Compare both SPI1 address layouts to ROM MD5 at four nonzero offsets.
/// Always stay in the loader. `output` carries the probe's reviewable results.
pub fn esp_spi1_probe<L: Line, W: Write>(line: &mut L, entry: Entry, flash_size: u32, output: &mut W) -> Result<Layout, Error> {
    check_read_size(flash_size)?;
    for offset in PROBE_OFFSETS { check_range(offset, SPI1_READ_SIZE, flash_size)?; }
    let (mut client, _) = S3Client::connect(line, entry)?;
    client.attach(flash_size)?;
    // ROM flash operations precede begin so they cannot alter the configured
    // SPI1 reader between reads. Each layout gets exactly one reader.
    let mut expected = Vec::new();
    for offset in PROBE_OFFSETS { expected.push(client.flash_md5(offset, SPI1_READ_SIZE)?); }
    let mut fits = [true; 2];
    for (index, layout) in [Layout::Low, Layout::High].into_iter().enumerate() {
        let mut reader = Spi1Reader::begin(&mut client, layout)?;
        let result = (|| {
            if index == 0 { writeln!(output, "{}", reader.saved)?; }
            for (offset, want) in PROBE_OFFSETS.into_iter().zip(&expected) {
                let got = md5::digest(&reader.read(offset)?);
                let matches = got == *want;
                fits[index] &= matches;
                writeln!(output, "0x{offset:08x} {layout}: MD5 {} {}", md5::hex(&got),
                    if matches { "match" } else { "differs" })?;
            }
            Ok(())
        })();
        reader.finish(result)?;
    }
    match fits {
        [true, false] => { writeln!(output, "layout: low")?; Ok(Layout::Low) }
        [false, true] => { writeln!(output, "layout: high")?; Ok(Layout::High) }
        [true, true] => { writeln!(output, "both layouts fit")?; Err(Error::Spi1("both layouts fit")) }
        [false, false] => { writeln!(output, "no layout fits")?; Err(Error::Spi1("no layout fits")) }
    }
}

fn backup_absent(directory: &Path) -> Result<(), Error> {
    match fs::symlink_metadata(directory.join("flash.bin")) {
        Ok(_) => Err(Error::Plan("output directory already holds flash.bin".into())),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err.into()),
    }
}

/// How a read-back goes, for the caller's progress line.
#[derive(Clone, Copy, Debug)]
pub enum ReadStep {
    /// The ROM's MD5 of every 64 KiB block is known: `blocks` of them.
    Fingerprinted { blocks: u32 },
    /// `done` of `size` bytes read and verified, at `rate` bytes per second so far.
    Read { done: u32, size: u32, rate: f64 },
    /// A block read differed from the ROM's MD5 and is read again (the error is transient while
    /// two failed reads differ).
    Retry { offset: u32 },
}

/// A whole flash read back and verified: the bytes and how they were read.
pub struct ReadBack {
    pub security: SecurityInfo,
    pub flash_size: u32,
    pub whole_md5: [u8; 16],
    pub sha256: String,
    pub image: Vec<u8>,
    pub blocks: Vec<RegionMd5>,
    pub layout: Layout,
    /// The SPI1 registers saved before the read and restored after it, as text.
    pub saved: String,
    pub seconds: f64,
}

/// Read flash through SPI1 without a stub or a flash write, verifying every block and the whole
/// image against SPI_FLASH_MD5, then leave the loader as `leave` says (also on errors once
/// connected). `stop` is asked between blocks; true ends the read with `Error::Plan`.
pub fn esp_read_back<L: Line>(
    line: &mut L, entry: Entry, flash_size: u32, layout: Layout, leave: Exit,
    progress: &mut dyn FnMut(ReadStep), stop: &dyn Fn() -> bool,
) -> Result<ReadBack, Error> {
    check_read_size(flash_size)?;
    let start = Instant::now();
    let (mut client, _) = S3Client::connect(line, entry)?;
    let security = client.security.clone();
    let read = read_verified(&mut client, flash_size, layout, start, progress, stop);
    let left = exit(client.line, leave);
    let (image, whole_md5, sha256, blocks, saved) = read?;
    left?;
    Ok(ReadBack { security, flash_size, whole_md5, sha256, image, blocks, layout, saved,
        seconds: start.elapsed().as_secs_f64() })
}

type Verified = (Vec<u8>, [u8; 16], String, Vec<RegionMd5>, String);

fn read_verified<L: Line>(
    client: &mut S3Client<'_, L>, flash_size: u32, layout: Layout, start: Instant,
    progress: &mut dyn FnMut(ReadStep), stop: &dyn Fn() -> bool,
) -> Result<Verified, Error> {
    client.attach(flash_size)?;
    let whole_md5 = client.flash_md5(0, flash_size)?;
    let mut blocks = Vec::new();
    for offset in (0..flash_size).step_by(BLOCK_SIZE as usize) {
        let size = BLOCK_SIZE.min(flash_size - offset);
        blocks.push(RegionMd5 { offset, size, md5: client.flash_md5(offset, size)? });
    }
    progress(ReadStep::Fingerprinted { blocks: blocks.len() as u32 });
    let mut reader = Spi1Reader::begin(client, layout)?;
    let saved = reader.saved.to_string();
    let result = (|| {
        let mut image = Vec::with_capacity(flash_size as usize);
        let mut whole = md5::Md5::new();
        let mut sha = Sha256::new();
        for block in &blocks {
            if stop() {
                return Err(Error::Plan("the read was stopped".into()));
            }
            let mut previous: Option<Vec<u8>> = None;
            loop {
                let mut bytes = Vec::with_capacity(block.size as usize);
                for relative in (0..block.size).step_by(SPI1_READ_SIZE as usize) {
                    let chunk = reader.read(block.offset + relative)?;
                    let count = SPI1_READ_SIZE.min(block.size - relative) as usize;
                    bytes.extend_from_slice(&chunk[..count]);
                }
                let got = md5::digest(&bytes);
                if got == block.md5 {
                    whole.update(&bytes);
                    sha.update(&bytes);
                    image.extend_from_slice(&bytes);
                    break;
                }
                if previous.as_ref() == Some(&bytes) {
                    return Err(Error::BackupBlock {
                        block: block.offset / BLOCK_SIZE, offset: block.offset, want: block.md5, got,
                    });
                }
                progress(ReadStep::Retry { offset: block.offset });
                // Lead choice: retry while consecutive failed reads differ;
                // there is no arbitrary retry limit on transient corruption.
                previous = Some(bytes);
            }
            let done = image.len() as u32;
            progress(ReadStep::Read { done, size: flash_size, rate: f64::from(done) / start.elapsed().as_secs_f64() });
        }
        let got = whole.finish();
        if got != whole_md5 { return Err(Error::Verify { offset: 0, size: flash_size, want: whole_md5, got }); }
        Ok((image, sha256::to_hex(&sha.finish())))
    })();
    let (image, sha) = reader.finish(result)?;
    Ok((image, whole_md5, sha, blocks, saved))
}

/// Write a read-back into `directory` (made when missing): `flash.bin` (renamed into place last,
/// so a present one is whole), `flash.sha256` and `report.txt`. Refuses a directory that already
/// holds a `flash.bin`.
pub fn write_backup(directory: &Path, back: &ReadBack) -> Result<(), Error> {
    backup_absent(directory)?;
    let mut report = format!("security: {}\nflash_size: {}\nROM MD5: {}\nsha256: {}\nlayout: {}\n{}\n",
        back.security, back.flash_size, md5::hex(&back.whole_md5), back.sha256, back.layout, back.saved);
    for block in &back.blocks {
        report.push_str(&format!("block {} at 0x{:08x} ({} bytes) MD5 {}\n",
            block.offset / BLOCK_SIZE, block.offset, block.size, md5::hex(&block.md5)));
    }
    report.push_str(&format!("bytes: {}\nwall_time_seconds: {:.3}\nrate_KB_per_second: {:.2}\n",
        back.image.len(), back.seconds, back.image.len() as f64 / back.seconds / 1024.0));
    fs::create_dir_all(directory)?;
    backup_absent(directory)?;
    let partial = directory.join("flash.bin.partial");
    let mut file = fs::OpenOptions::new().write(true).create_new(true).open(&partial)?;
    file.write_all(&back.image)?;
    file.sync_all()?;
    drop(file);
    fs::write(directory.join("flash.sha256"), format!("{}  flash.bin\n", back.sha256))?;
    fs::write(directory.join("report.txt"), report)?;
    backup_absent(directory)?;
    fs::rename(partial, directory.join("flash.bin"))?;
    Ok(())
}

/// The CLI's backup: [`esp_read_back`] staying in the ROM loader, its progress on stderr, then
/// [`write_backup`].
pub fn esp_read<L: Line>(line: &mut L, entry: Entry, flash_size: u32, layout: Layout, directory: &Path) -> Result<(), Error> {
    backup_absent(directory)?;
    let back = esp_read_back(line, entry, flash_size, layout, Exit::Stay, &mut |step| match step {
        ReadStep::Fingerprinted { blocks } => eprintln!("esp-read: the ROM's MD5 of {blocks} blocks known"),
        ReadStep::Retry { offset } => eprintln!("esp-read: retry block {} at 0x{offset:08x}", offset / BLOCK_SIZE),
        ReadStep::Read { done, size, rate } => eprintln!("esp-read: offset 0x{done:08x}/{size:#010x}, {:.2} KB/s, {:.1} s remaining",
            rate / 1024.0, f64::from(size - done) / rate),
    }, &|| false)?;
    println!("esp-rom: whole flash MD5 {} ({flash_size} bytes)", md5::hex(&back.whole_md5));
    write_backup(directory, &back)?;
    println!("esp-read: verified {flash_size} bytes, SHA-256 {}; stayed in the ROM loader", back.sha256);
    Ok(())
}
