//! The ROM functions the model stands in for: the ROM itself never runs (Q6 §1.2). Each entry is a
//! foreign-table contract (`VirtualFns`) from its sources, installed on every machine (`table`, as
//! `S3Machine::with_board` does) and kept beside a part's table (`Beside`, `S3Machine::with_foreign`):
//! every image's init calls the ROM.

use std::sync::Arc;

use fab_elec_mcu_xtensa::insn::Insn;
use fab_elec_mcu_xtensa::sim::{Cpu, Stop};

use crate::foreign::{Ctx, ForeignBus, ForeignCode, Next, VirtualFn, VirtualFns, MIN_FRAME};

/// The ROM's instruction range: ESP-IDF v6.1.1 soc/esp32s3/include/soc/soc.h:175-176
/// (SOC_IROM_MASK_LOW, SOC_IROM_MASK_HIGH); TRM p.403 Table 4.3-1.
const ROM: std::ops::Range<u32> = 0x4000_0000..0x4006_0000;
/// rom_i2c_readReg(block, host_id, reg_add) -> u8: esp-hal 4059aa58 esp-hal/src/soc/esp32s3/regi2c.rs:135-142
/// (its esp_rom_regi2c_read); esp-rom-sys/ld/esp32s3/rom/esp32s3.rom.api.ld:66 names it, esp32s3.rom.ld:1247
/// places it.
pub const I2C_READ_REG: u32 = 0x4000_5d48;
/// rom_i2c_writeReg(block, host_id, reg_add, data): esp-hal 4059aa58 esp-hal/src/soc/esp32s3/regi2c.rs:144-151
/// (its regi2c_write); esp32s3.rom.api.ld:68 names it, esp32s3.rom.ld:1249 places it.
pub const I2C_WRITE_REG: u32 = 0x4000_5d60;
const BBPLL: u8 = 0x66;
/// The cycles each stand-in call burns: the slowest the board showed (CCOUNT around the call at 160 MHz;
/// readReg sessions 17 and 19-21, writeReg sessions 19-21).
pub const READ_BBPLL_CYCLES: u32 = 2574;
pub const READ_DIG_REG_CYCLES: u32 = 2573;
pub const WRITE_CYCLES: u32 = 1971;
const COST: &str = "rom_i2c_readReg: each call takes the slowest the board showed for its block (sessions 17 and 19-21, CCOUNT \
    around the call at 160 MHz: BBPLL 939-2574 cycles, DIG_REG 767-2573; the first call 7302-9182, cold), as cycles at any \
    CPU clock (Assumed)";
const WRITE_COST: &str = "rom_i2c_writeReg: each call takes the slowest the board showed (sessions 19-21, CCOUNT around the call \
    at 160 MHz: 767-1971 cycles for DIG_REG 4 and 6), as cycles at any CPU clock; the register changes at the call's end (Assumed)";

/// The ROM's range and the entries the model serves, for a machine table that also holds another part's rows.
pub fn rows() -> (std::ops::Range<u32>, Vec<VirtualFn>) {
    let read = VirtualFn { name: "rom_i2c_readReg".into(), address: I2C_READ_REG, bytes: 15, frame: MIN_FRAME, body: read_reg };
    let write = VirtualFn { name: "rom_i2c_writeReg".into(), address: I2C_WRITE_REG, bytes: 15, frame: MIN_FRAME, body: write_reg };
    (ROM, vec![read, write])
}

/// A part's table beside the ROM's stand-ins, as `S3Machine::with_foreign` installs it: a PC the part's
/// table holds is the part's (a table that took `rows` serves the ROM's entries itself), any other the ROM's.
pub(crate) struct Beside { pub(crate) part: Arc<dyn ForeignCode>, pub(crate) rom: Arc<VirtualFns> }

impl ForeignCode for Beside {
    fn contains(&self, pc: u32) -> bool { self.part.contains(pc) || self.rom.contains(pc) }

    fn at(&self, pc: u32, cpu: &mut Cpu, bus: &mut dyn ForeignBus) -> Result<(Insn, u32), Stop> {
        if self.part.contains(pc) { self.part.at(pc, cpu, bus) } else { self.rom.at(pc, cpu, bus) }
    }

    fn explain(&self, pc: u32, cpu: &Cpu, bus: &dyn ForeignBus) -> Option<String> {
        if self.part.contains(pc) { self.part.explain(pc, cpu, bus) } else { self.rom.explain(pc, cpu, bus) }
    }
}

/// The ROM entries the model serves.
pub fn table() -> Arc<VirtualFns> {
    let (range, functions) = rows();
    Arc::new(VirtualFns::new(vec![range], functions).expect("the ROM table's entries lie in the ROM, apart"))
}

/// The register's byte from the analog model (`ForeignBus::regi2c`) after the call's cost; a register
/// the model does not serve stops the call before it starts.
fn read_reg(ctx: &Ctx, bus: &dyn ForeignBus) -> Next {
    let [block, host, reg] = [0, 1, 2].map(|k| ctx.args[k] as u8);
    match (ctx.step, bus.regi2c(block, host, reg)) {
        (_, Err(why)) => Next::Stop(why),
        (0, Ok(_)) => Next::Effect(|_, bus| bus.note(COST)),
        (1, Ok(_)) => Next::Burn(if block == BBPLL { READ_BBPLL_CYCLES } else { READ_DIG_REG_CYCLES }),
        (_, Ok(byte)) => Next::Return(byte.into()),
    }
}

/// The call's cost, then the write (`ForeignBus::regi2c_set`); a write the analog model does not take
/// stops the call before it starts.
fn write_reg(ctx: &Ctx, bus: &dyn ForeignBus) -> Next {
    let [block, host, reg, data] = [0, 1, 2, 3].map(|k| ctx.args[k] as u8);
    match (ctx.step, bus.regi2c_writable(block, host, reg, data)) {
        (_, Err(why)) => Next::Stop(why),
        (0, Ok(())) => Next::Effect(|_, bus| bus.note(WRITE_COST)),
        (1, Ok(())) => Next::Burn(WRITE_CYCLES),
        (2, Ok(())) => Next::Effect(|ctx, bus| bus.regi2c_set(ctx.args[0] as u8, ctx.args[2] as u8, ctx.args[3] as u8)),
        (_, Ok(())) => Next::Return(0),
    }
}
