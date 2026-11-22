//! Read-only board identification, trying Arduino bootloaders before ESP ROM.

use crate::{esp_rom, stk500};
use makepad_serial::line::Line;
use std::fmt;

#[derive(Debug)]
pub enum Board {
    Stk500(stk500::Bootloader),
    EspRom(esp_rom::EspRom),
    Unknown,
}

pub fn identify<L: Line>(line: &mut L) -> Board {
    for baud in [115200, 57600] {
        if let Ok(boot) = stk500::probe(line, baud) {
            return Board::Stk500(boot);
        }
    }
    match esp_rom::probe(line) {
        Ok(rom) => Board::EspRom(rom),
        Err(_) => Board::Unknown,
    }
}

impl fmt::Display for Board {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Stk500(boot) => {
                write!(
                    f,
                    "stk500 baud={} hw={} sw={}.{} signature={:02x}{:02x}{:02x} chip=",
                    boot.baud, boot.hw, boot.sw_major, boot.sw_minor,
                    boot.signature[0], boot.signature[1], boot.signature[2]
                )?;
                match boot.chip {
                    Some(chip) => f.write_str(chip.name)?,
                    None => f.write_str("unknown")?,
                }
                match boot.app_bytes() {
                    Some(bytes) => write!(f, " app_bytes={bytes}"),
                    None => f.write_str(" app_bytes=unknown"),
                }
            }
            Self::EspRom(rom) => write!(f, "esp-rom magic=0x{:08x}", rom.magic),
            Self::Unknown => f.write_str("unknown (no stk500 sync at 115200 or 57600, no esp rom sync)"),
        }
    }
}
