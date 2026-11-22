// The command register's layout and read sequence are derived from ESP-IDF v6.1.1
// components/esp_hal_regi2c/esp32s2/regi2c_impl.c (see fab/CREDITS.md).
// SPDX-FileCopyrightText: 2019-2026 Espressif Systems (Shanghai) CO LTD
// SPDX-License-Identifier: Apache-2.0
//! ESP-IDF v6.1.1 (Espressif Systems, Apache-2.0):
//! components/soc/esp32s3/include/soc/regi2c_defs.h (ANA_CONFIG_REG 0x6000E044: I2C_BBPLL_M bit 17, I2C_SAR_M bit 18;
//! ANA_CONFIG2_REG 0x6000E048; I2C_MST_ANA_CONF0_REG 0x6000E040, BBPLL_CAL_DONE bit 24);
//! components/esp_hal_regi2c/esp32s2/regi2c_impl.c (the ESP32-S2's command register 0x6000E000: slave id [7:0],
//! register [15:8], data [23:16], write bit 24, busy bit 25); components/soc/esp32s3/include/soc/regi2c_bbpll.h and
//! regi2c_dig_reg.h (blocks 0x66 and 0x6D, both host id 1); components/esp_hal_clock/esp32s3/include/hal/
//! clk_tree_ll.h:310-385 (the BBPLL values for 480 MHz from a 40 MHz crystal).
//!
//! What the board showed (its first analog check, then register reads over the ROM loader): after the ROM, ANA_CONF0 = 0x2100e408, ANA_CONFIG = 0x2d,
//! ANA_CONFIG2 = 0x4, and both 0x6000E000 and 0x6000E004 read 0x04000000. S2-layout reads of the BBPLL and DIG_REG
//! through 0x6000E000 end (BUSY clears) with data 0xff: no slave answers there. Through 0x6000E004 every register
//! read 0x00 (session 16): it is no command register in the S2 layout, and the model refuses commands there. The
//! ROM's rom_i2c_readReg read the analog registers in `OBSERVED` (session 17); `regi2c` serves those, and the ROM
//! table's stand-in (`crate::rom`) the call. rom_i2c_writeReg changes only the two core regulators' EXT_*_DREG
//! fields (DIG_REG 4 and 6, bits [4:0]), as the 240 MHz raise does (IDF esp_hw_support/port/esp32s3/rtc_clk.c:233-240,
//! its REGI2C_WRITE_MASK; regi2c_dig_reg.h), the other bits as read.

use super::Access;

const BASE: u32 = 0x6000_E000;
const BBPLL_MASK: u32 = 1 << 17;
const WRITE: u32 = 1 << 24;
const BUSY: u32 = 1 << 25;
/// Bit 26 of both command words as the board found them after the ROM; no source names it.
const FOUND: u32 = 1 << 26;
// Assumed (M): no source gives the transaction time; the board check measures it and this value follows the chip.
const BUSY_TICKS: u64 = 9_600;

const NOTED_HOST0: u8 = 1;

/// The blocks, both behind host 1 (regi2c_bbpll.h, regi2c_dig_reg.h).
const BBPLL: u8 = 0x66;
const DIG_REG: u8 = 0x6D;
const HOST: u8 = 1;
/// DIG_REG's EXT_RTC_DREG (register 4) and EXT_DIG_DREG (register 6), bits [4:0] (regi2c_dig_reg.h).
const DREGS: [u8; 2] = [4, 6];
const DREG_FIELD: u8 = 0x1f;
/// (block, register, byte) as the board's ROM read them (session 17): the BBPLL in IDF's 480 MHz-from-40 MHz
/// configuration but DHREF_SEL 2 and VCO_DBIAS 2 (IDF writes 3, clk_tree_ll.h:310-388), the regulators at 24 under
/// external control.
const OBSERVED: [(u8, u8, u8); 11] = [
    (BBPLL, 2, 0x50), (BBPLL, 3, 0x08), (BBPLL, 4, 0x6B), (BBPLL, 5, 0x80), (BBPLL, 6, 0x63), (BBPLL, 9, 0x92),
    (DIG_REG, 4, 0x98), (DIG_REG, 5, 0x18), (DIG_REG, 6, 0x98), (DIG_REG, 7, 0x18), (DIG_REG, 13, 0x4E),
];

/// One host's command word and its transaction.
#[derive(Clone, PartialEq, Eq)]
struct Host {
    cmd: u32,
    data: u8,
    busy_until: u64,
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct Ana {
    conf0: u32,
    config: u32,
    config2: u32,
    /// Host 0's command word (0x6000E000) and host 1's (0x6000E004).
    hosts: [Host; 2],
    /// (block, register, byte) the image wrote through rom_i2c_writeReg, read before `OBSERVED`.
    written: Vec<(u8, u8, u8)>,
    noted: u8,
    notes: Vec<(u64, &'static str)>,
}

impl Ana {
    pub fn new() -> Self {
        let host = Host { cmd: FOUND, data: 0, busy_until: 0 };
        Self { conf0: 0x2100_E408, config: 0x2D, config2: 0x4, hosts: [host.clone(), host], written: Vec::new(), noted: 0, notes: Vec::new() }
    }

    /// Register `reg` of `block` through `host`, as rom_i2c_readReg reads it.
    pub fn regi2c(&self, block: u8, host: u8, reg: u8) -> Result<u8, &'static str> {
        if host != HOST {
            return Err("analog I2C host other than 1 is not modelled (the BBPLL and DIG_REG are host 1's)");
        }
        if block == BBPLL && self.config & BBPLL_MASK != 0 {
            return Err("BBPLL read while ANA_CONFIG masks it is not documented");
        }
        self.written.iter().chain(&OBSERVED).find(|&&(b, r, _)| b == block && r == reg).map(|&(_, _, byte)| byte)
            .ok_or("analog I2C register the board has not shown is not modelled (BBPLL 2-6 and 9, DIG_REG 4-7 and 13 are)")
    }

    /// Whether rom_i2c_writeReg may write `data` to register `reg` of `block` through `host`: a core
    /// regulator's EXT_*_DREG field, the register's other bits as they read now.
    pub fn regi2c_writable(&self, block: u8, host: u8, reg: u8, data: u8) -> Result<(), &'static str> {
        let now = self.regi2c(block, host, reg)?;
        if block != DIG_REG || !DREGS.contains(&reg) {
            return Err("analog I2C writes other than the core regulators' EXT_*_DREG (DIG_REG 4 and 6) are not modelled");
        }
        if (now ^ data) & !DREG_FIELD != 0 {
            return Err("an analog I2C write that changes DIG_REG bits outside EXT_*_DREG [4:0] is not modelled");
        }
        Ok(())
    }

    /// The write `regi2c_writable` allowed: from now on the register reads `data`.
    pub fn regi2c_set(&mut self, block: u8, reg: u8, data: u8) {
        self.written.retain(|&(b, r, _)| (b, r) != (block, reg));
        self.written.push((block, reg, data));
    }

    /// Whether the image wrote both core regulators, as the 240 MHz raise does before the CPU goes there.
    pub fn regulators_written(&self) -> bool {
        DREGS.iter().all(|&reg| self.written.iter().any(|&(b, r, _)| (b, r) == (DIG_REG, reg)))
    }

    fn note_once(&mut self, bit: u8, tick: u64, what: &'static str) {
        if self.noted & bit == 0 {
            self.notes.push((tick, what));
            self.noted |= bit;
        }
    }

    pub fn read(&mut self, addr: u32, tick: u64) -> Access<u32> {
        if !(BASE..BASE + 0x1000).contains(&addr) { return None; }
        let value = match addr - BASE {
            offset @ (0x000 | 0x004) => {
                let host = &self.hosts[(offset / 4) as usize];
                let mut word = host.cmd & (FOUND | 0x00FF_FFFF);
                if tick < host.busy_until {
                    word |= BUSY;
                } else if host.cmd & WRITE == 0 {
                    word = (word & !0x00FF_0000) | (u32::from(host.data) << 16);
                }
                word
            }
            0x014 => return Some(Err("a read of 0x6000E014 stalled the board's bus (the ROM loader stopped answering); it is not modelled")),
            0x040 => self.conf0,
            0x044 => self.config,
            0x048 => self.config2,
            _ => return Some(Err("analog I2C master register outside 0x000/0x004/0x040/0x044/0x048 is not modelled")),
        };
        Some(Ok(value))
    }

    fn command(&mut self, host: usize, value: u32, tick: u64) -> Result<(), &'static str> {
        if self.hosts.iter().any(|h| tick < h.busy_until) {
            return Err("analog I2C command while a command is busy is not documented");
        }
        if value & WRITE != 0 {
            return Err("analog I2C writes through the command words are not documented (rom_i2c_writeReg writes the regulators)");
        }
        if value >> 25 != 0 {
            return Err("analog I2C command with reserved bits set is not documented");
        }
        if host == 1 {
            return Err("0x6000E004 is no analog I2C command register: the board read 0x00 for every register there (session 16); rom_i2c_readReg reads them");
        }
        let data = match value as u8 {
            BBPLL | DIG_REG => {
                self.note_once(NOTED_HOST0, tick, "analog I2C master: host 0 (0x6000E000) reads the BBPLL and DIG_REG as 0xff, as the board did, BUSY for an Assumed 20 us");
                0xff
            }
            _ => return Err("analog I2C slave outside the BBPLL and DIG_REG is not modelled"),
        };
        let h = &mut self.hosts[host];
        h.cmd = (h.cmd & FOUND) | value;
        h.data = data;
        h.busy_until = tick + BUSY_TICKS;
        Ok(())
    }

    pub fn write(&mut self, addr: u32, value: u32, tick: u64) -> Access<()> {
        if !(BASE..BASE + 0x1000).contains(&addr) { return None; }
        Some(match addr - BASE {
            offset @ (0x000 | 0x004) => self.command((offset / 4) as usize, value, tick),
            0x044 if value == (self.config & !BBPLL_MASK) => {
                self.config = value;
                Ok(())
            }
            0x044 => Err("ANA_CONFIG writes other than clearing the BBPLL mask are not modelled"),
            _ => Err("analog I2C master register writes other than the commands and the BBPLL unmask are not modelled"),
        })
    }

    pub fn take_notes(&mut self, out: &mut Vec<(u64, &'static str)>) { out.append(&mut self.notes); }
}
