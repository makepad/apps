//! Serial identification, flashing and watch streams for microcontroller boards.
//!
//! The flasher never writes flash on a board it has not identified. The
//! [`stk500::write_flash`] entry point checks the requested signature, supported
//! chip and application size before programming, then verifies every written
//! page. [`esp_rom::esp_write`] checks the requested chip id and region bounds
//! before erasing, requires a grant and backup waiver, and verifies each region
//! with MD5. [`esp_rom::esp_spi1_probe`] selects an SPI1 address layout using
//! nonzero offsets checked against the ROM's MD5. [`esp_rom::esp_read`] backs up
//! flash through ROM READ_REG/WRITE_REG and the SPI1 controller, with no stub or
//! flash writes. It verifies every 64 KiB block and the whole image before
//! saving flash.bin, flash.sha256 and report.txt, restores the saved SPI1
//! configuration, and stays in the loader. Both SPI1 commands always stay;
//! esp-info and esp-md5 can also stay with --stay. [`esp_rom::esp_read_reg`] reads
//! named registers and [`esp_rom::esp_clear_force_download`] clears one RTC bit
//! after a chip id check; both stay in the loader. Identification, ESP ROM
//! probing, info, MD5 and backups are read-only with respect to flash. The low-level
//! [`stk500::Stk500`] client exposes individual bootloader commands for callers
//! that manage their own identification and programming sequence.
//!
//! [`watch::tether`] adds a host peer on the device's frame channel; [`tether::Pictures`] streams
//! pictures to the handheld's pictures firmware through it.
//! [`rate::Rate`] measures throughput with the handheld's tether rate firmware.
//!
//! All protocol waits go through [`makepad_serial::line::Line`], allowing virtual-time tests.

pub mod stk500;
pub mod esp_rom;
pub mod md5;
pub mod identify;
pub mod watch;
pub mod tether;
pub mod rate;
