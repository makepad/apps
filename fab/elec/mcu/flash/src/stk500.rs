//! STK500v1 commands, board identification and verified application flashing.

use makepad_serial::line::{Line, Step};
use std::{fmt, io, time::Duration};

pub const STK_OK: u8 = 0x10;
pub const STK_FAILED: u8 = 0x11;
pub const STK_INSYNC: u8 = 0x14;
pub const STK_NOSYNC: u8 = 0x15;
pub const CRC_EOP: u8 = 0x20;
pub const GET_SYNC: u8 = 0x30;
pub const GET_PARAMETER: u8 = 0x41;
pub const ENTER_PROGMODE: u8 = 0x50;
pub const LEAVE_PROGMODE: u8 = 0x51;
pub const LOAD_ADDRESS: u8 = 0x55;
pub const PROG_PAGE: u8 = 0x64;
pub const READ_PAGE: u8 = 0x74;
pub const READ_SIGN: u8 = 0x75;
pub const HW_VER: u8 = 0x80;
pub const SW_MAJOR: u8 = 0x81;
pub const SW_MINOR: u8 = 0x82;

/// Wait after the reset edge before the first sync: past optiboot's start-up (about 66 ms), so the
/// chip's UART is on and holds the sync until the bootloader's loop reads it.
const RESET_SETTLE: Duration = Duration::from_millis(100);
/// Each further reset waits this much longer, for a bootloader whose UART comes on later.
const RESET_SETTLE_STEP: Duration = Duration::from_millis(200);
/// Wait for the first sync's answer: past optiboot's LED flashes (about 375 ms), inside its 1 s
/// watchdog window.
const SYNC_WAIT: Duration = Duration::from_millis(800);
/// Resets one sync takes at most, the caller's included.
const SYNC_RESETS: u32 = 3;

#[derive(Debug)]
pub enum Error {
    Io(io::Error),
    Timeout,
    NoSync,
    Failed,
    Protocol { expected: u8, got: u8 },
    NotSynced,
    UnknownChip([u8; 3]),
    WrongChip { want: [u8; 3], got: [u8; 3] },
    ImageTooLarge { bytes: usize, max: usize },
    EmptyImage,
    VerifyMismatch { offset: usize, want: u8, got: u8 },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(err) => write!(f, "serial I/O: {err}"),
            Self::Timeout => f.write_str("STK500 reply timed out"),
            Self::NoSync => f.write_str("no STK500 sync"),
            Self::Failed => f.write_str("STK500 command failed"),
            Self::Protocol { expected, got } => {
                write!(f, "STK500 expected 0x{expected:02x}, got 0x{got:02x}")
            }
            Self::NotSynced => f.write_str("STK500 has not synced"),
            Self::UnknownChip(sig) => {
                write!(f, "unknown chip {:02x}{:02x}{:02x}", sig[0], sig[1], sig[2])
            }
            Self::WrongChip { want, got } => write!(
                f,
                "wrong chip: wanted {:02x}{:02x}{:02x}, got {:02x}{:02x}{:02x}",
                want[0], want[1], want[2], got[0], got[1], got[2]
            ),
            Self::ImageTooLarge { bytes, max } => {
                write!(f, "image has {bytes} bytes, maximum is {max}")
            }
            Self::EmptyImage => f.write_str("image is empty"),
            Self::VerifyMismatch { offset, want, got } => {
                write!(f, "verify mismatch at byte {offset}: wanted 0x{want:02x}, got 0x{got:02x}")
            }
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
    fn from(err: io::Error) -> Self {
        Self::Io(err)
    }
}

pub struct Stk500<'a, L: Line> {
    line: &'a mut L,
    synced: bool,
}

impl<'a, L: Line> Stk500<'a, L> {
    pub fn new(line: &'a mut L) -> Self {
        Self { line, synced: false }
    }

    /// Reset into the bootloader using the asserting DTR/RTS edge.
    pub fn reset(&mut self) -> Result<(), Error> {
        self.reset_settle(RESET_SETTLE)
    }

    fn reset_settle(&mut self, settle: Duration) -> Result<(), Error> {
        use Step::*;
        self.synced = false;
        self.line.sequence(&[
            Dtr(false), Rts(false), Wait(Duration::from_millis(250)),
            Dtr(true), Rts(true), Wait(settle), Drain,
        ]).map_err(|(_, err)| err)?;
        Ok(())
    }

    /// Sync after reset, then confirm alignment with a second answered sync.
    /// Optiboot reads its UART late, and the chip buffers only three bytes:
    /// a second command in flight overflows that buffer and corrupts the stream.
    /// Retry only after resetting, each reset settling longer, at most three including the caller's.
    pub fn sync(&mut self) -> Result<(), Error> {
        self.synced = false;
        let reply_timeout = Duration::from_millis(300);
        'reset: for attempt in 0..SYNC_RESETS {
            if attempt != 0 {
                self.reset_settle(RESET_SETTLE + RESET_SETTLE_STEP * attempt)?;
            }
            self.line.drain_input()?;
            for timeout in [SYNC_WAIT, reply_timeout] {
                self.line.write_all(&[GET_SYNC, CRC_EOP])?;
                if self.line.read_byte(timeout)? != Some(STK_INSYNC)
                    || self.line.read_byte(reply_timeout)? != Some(STK_OK)
                {
                    continue 'reset;
                }
            }
            self.synced = true;
            return Ok(());
        }
        Err(Error::NoSync)
    }

    fn require_sync(&self) -> Result<(), Error> {
        if self.synced { Ok(()) } else { Err(Error::NotSynced) }
    }

    fn byte(&mut self) -> Result<u8, Error> {
        self.line.read_byte(Duration::from_secs(1))?.ok_or(Error::Timeout)
    }

    fn marker(&mut self, expected: u8) -> Result<(), Error> {
        let got = self.byte()?;
        if got == expected {
            Ok(())
        } else if got == STK_NOSYNC {
            self.synced = false;
            Err(Error::NoSync)
        } else if expected == STK_OK && got == STK_FAILED {
            Err(Error::Failed)
        } else {
            Err(Error::Protocol { expected, got })
        }
    }

    fn command(&mut self, bytes: &[u8], len: usize) -> Result<Vec<u8>, Error> {
        self.require_sync()?;
        let mut packet = Vec::with_capacity(bytes.len() + 1);
        packet.extend_from_slice(bytes);
        packet.push(CRC_EOP);
        self.line.write_all(&packet)?;
        self.marker(STK_INSYNC)?;
        let mut payload = Vec::with_capacity(len);
        for _ in 0..len {
            // Payload bytes may themselves equal STK_NOSYNC or STK_FAILED.
            payload.push(self.byte()?);
        }
        self.marker(STK_OK)?;
        Ok(payload)
    }

    pub fn get_parameter(&mut self, p: u8) -> Result<u8, Error> {
        let reply = self.command(&[GET_PARAMETER, p], 1)?;
        Ok(reply[0])
    }

    pub fn read_signature(&mut self) -> Result<[u8; 3], Error> {
        let reply = self.command(&[READ_SIGN], 3)?;
        Ok([reply[0], reply[1], reply[2]])
    }

    pub fn enter_progmode(&mut self) -> Result<(), Error> {
        self.command(&[ENTER_PROGMODE], 0)?;
        Ok(())
    }

    pub fn leave_progmode(&mut self) -> Result<(), Error> {
        self.command(&[LEAVE_PROGMODE], 0)?;
        self.synced = false;
        Ok(())
    }

    /// Load a flash WORD address (byte offset / 2), little endian on the wire.
    pub fn load_address(&mut self, word: u16) -> Result<(), Error> {
        let [lo, hi] = word.to_le_bytes();
        self.command(&[LOAD_ADDRESS, lo, hi], 0)?;
        Ok(())
    }

    pub fn program_page(&mut self, data: &[u8]) -> Result<(), Error> {
        self.require_sync()?;
        let len = wire_len(data.len())?.to_be_bytes();
        let mut bytes = Vec::with_capacity(data.len() + 4);
        bytes.extend_from_slice(&[PROG_PAGE, len[0], len[1], b'F']);
        bytes.extend_from_slice(data);
        self.command(&bytes, 0)?;
        Ok(())
    }

    pub fn read_page(&mut self, len: usize) -> Result<Vec<u8>, Error> {
        self.require_sync()?;
        let [hi, lo] = wire_len(len)?.to_be_bytes();
        self.command(&[READ_PAGE, hi, lo, b'F'], len)
    }
}

fn wire_len(bytes: usize) -> Result<u16, Error> {
    u16::try_from(bytes).map_err(|_| Error::ImageTooLarge { bytes, max: u16::MAX as usize })
}

#[derive(Debug)]
pub struct Chip {
    pub name: &'static str,
    pub signature: [u8; 3],
    pub flash_bytes: usize,
    pub page_bytes: usize,
}

static CHIPS: [Chip; 4] = [
    Chip { name: "ATmega328P", signature: [0x1e, 0x95, 0x0f], flash_bytes: 32768, page_bytes: 128 },
    Chip { name: "ATmega328", signature: [0x1e, 0x95, 0x14], flash_bytes: 32768, page_bytes: 128 },
    Chip { name: "ATmega168", signature: [0x1e, 0x94, 0x06], flash_bytes: 16384, page_bytes: 128 },
    Chip { name: "ATmega168PA", signature: [0x1e, 0x94, 0x0b], flash_bytes: 16384, page_bytes: 128 },
];

pub fn chip(sig: [u8; 3]) -> Option<&'static Chip> {
    CHIPS.iter().find(|chip| chip.signature == sig)
}

#[derive(Debug)]
pub struct Bootloader {
    pub baud: u32,
    pub hw: u8,
    pub sw_major: u8,
    pub sw_minor: u8,
    pub signature: [u8; 3],
    pub chip: Option<&'static Chip>,
}

impl Bootloader {
    /// Flash minus a 512-byte boot section for software major >= 4, otherwise
    /// minus the conservative 2048-byte boot section.
    pub fn app_bytes(&self) -> Option<usize> {
        self.chip?.flash_bytes.checked_sub(if self.sw_major >= 4 { 512 } else { 2048 })
    }
}

fn probe_client<L: Line>(client: &mut Stk500<'_, L>, baud: u32) -> Result<Bootloader, Error> {
    client.line.set_baud(baud)?;
    client.reset()?;
    client.sync()?;
    let hw = client.get_parameter(HW_VER)?;
    let sw_major = client.get_parameter(SW_MAJOR)?;
    let sw_minor = client.get_parameter(SW_MINOR)?;
    let signature = client.read_signature()?;
    Ok(Bootloader { baud, hw, sw_major, sw_minor, signature, chip: chip(signature) })
}

/// Reset, sync and read bootloader parameters and signature. Never writes flash.
pub fn probe<L: Line>(line: &mut L, baud: u32) -> Result<Bootloader, Error> {
    probe_client(&mut Stk500::new(line), baud)
}

/// Identify the board, check the expected signature and application size, then
/// program and verify full pages, padding the final page with 0xff.
pub fn write_flash<L: Line>(line: &mut L, baud: u32, want: [u8; 3], image: &[u8]) -> Result<usize, Error> {
    if image.is_empty() {
        return Err(Error::EmptyImage);
    }
    let mut client = Stk500::new(line);
    let boot = probe_client(&mut client, baud)?;
    if boot.signature != want {
        return Err(Error::WrongChip { want, got: boot.signature });
    }
    let chip = boot.chip.ok_or(Error::UnknownChip(boot.signature))?;
    let max = boot.app_bytes().ok_or(Error::UnknownChip(boot.signature))?;
    if image.len() > max {
        return Err(Error::ImageTooLarge { bytes: image.len(), max });
    }
    let pages = image.len().div_ceil(chip.page_bytes);
    client.enter_progmode()?;
    let result = (|| {
        let mut padded = vec![0xff; chip.page_bytes];
        for (index, data) in image.chunks(chip.page_bytes).enumerate() {
            padded.fill(0xff);
            padded[..data.len()].copy_from_slice(data);
            client.load_address((index * chip.page_bytes / 2) as u16)?;
            client.program_page(&padded)?;
        }
        for index in 0..pages {
            let offset = index * chip.page_bytes;
            client.load_address((offset / 2) as u16)?;
            let data = client.read_page(chip.page_bytes)?;
            for (within, got) in data.into_iter().enumerate() {
                let at = offset + within;
                let want = match image.get(at) { Some(byte) => *byte, None => 0xff };
                if got != want {
                    return Err(Error::VerifyMismatch { offset: at, want, got });
                }
            }
        }
        Ok(pages)
    })();
    // Attempt to release the bootloader even when programming or verification failed.
    let leave = client.leave_progmode();
    let pages = result?;
    leave?;
    Ok(pages)
}

/// Identify, enter programming mode, read from address zero, then leave.
/// Reads may include the boot section but cannot exceed the known flash size.
pub fn read_flash<L: Line>(line: &mut L, baud: u32, len: usize) -> Result<Vec<u8>, Error> {
    let mut client = Stk500::new(line);
    let boot = probe_client(&mut client, baud)?;
    let chip = boot.chip.ok_or(Error::UnknownChip(boot.signature))?;
    if len > chip.flash_bytes {
        return Err(Error::ImageTooLarge { bytes: len, max: chip.flash_bytes });
    }
    client.enter_progmode()?;
    let result = (|| {
        let mut bytes = Vec::with_capacity(len);
        while bytes.len() < len {
            let offset = bytes.len();
            let count = chip.page_bytes.min(len - offset);
            client.load_address((offset / 2) as u16)?;
            bytes.extend_from_slice(&client.read_page(count)?);
        }
        Ok::<_, Error>(bytes)
    })();
    let leave = client.leave_progmode();
    let bytes = result?;
    leave?;
    Ok(bytes)
}
