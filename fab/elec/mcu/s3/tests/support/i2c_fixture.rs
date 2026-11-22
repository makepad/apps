//! I2C0 firmware register setup; facts-io.md I01–I20 and Appendix A.

use fab_elec_mcu_emit32::xtensa::{ccz, enc};

use super::{mux_reg, out_sel_reg, pin_reg, Program, GPIO_BASE, MUX_GPIO, MUX_IE};

pub const I2C0: u32 = 0x6001_3000;
pub const I2C_CTR: u32 = I2C0 + 0x04;
pub const I2C_DATA: u32 = I2C0 + 0x1c;
pub const I2C_CLR: u32 = I2C0 + 0x24;
pub const I2C_ENA: u32 = I2C0 + 0x28;
pub const I2C_STATUS: u32 = I2C0 + 0x2c;
pub const I2C_COMPLETE: u32 = 1 << 7;
pub const I2C_NACK: u32 = 1 << 10;
pub const I2C_MASTER: u32 = 1 << 4;
pub const I2C_RSTART: u32 = 6 << 11;
pub const I2C_STOP: u32 = 2 << 11;
pub const I2C_READ_NACK: u32 = (3 << 11) | (1 << 10) | 1;
pub const I2C_DIV: u32 = 0;
pub const I2C_LOW: u32 = 199;
pub const I2C_HIGH: u32 = 199;
pub const I2C_WAIT_HIGH: u32 = 0;
pub const I2C_START_HOLD: u32 = 100;
pub const I2C_RSTART_SETUP: u32 = 100;
pub const I2C_STOP_HOLD: u32 = 100;
pub const I2C_STOP_SETUP: u32 = 100;

pub fn i2c_write(count: u8) -> u32 { (1 << 11) | (1 << 8) | u32::from(count) }

impl Program {
    pub fn i2c_setup(&mut self) {
        // I20: preserve all other gates, explicitly hold I2C0 reset, clock it,
        // then release reset. Hardware reset defaults do not describe ROM state.
        self.time_bits(0x600c_0020, 0, 1 << 7);
        self.time_bits(0x600c_0018, 0, 1 << 7);
        self.time_bits(0x600c_0020, 1 << 7, 0);
        for (pad, signal) in [(14, 89), (15, 90)] {
            self.store(pin_reg(pad), 1 << 2);
            self.store(mux_reg(pad), MUX_GPIO | MUX_IE);
            self.store(out_sel_reg(pad), signal);
            self.store(GPIO_BASE + 0x154 + 4 * signal, (1 << 7) | pad);
        }
        self.store(I2C_CTR, I2C_MASTER);
        self.store(I2C0 + 0x54, (1 << 21) | I2C_DIV);
        // I11/I13: SCLK=40 MHz, APB=20 MHz, SCLK/SCL=401 (>20).
        // HOLD=20 gives 3*40 <= (20-4)*20 MHz; 20+100 > 0+3;
        // 0 < SAMPLE=80 < 199 and 80 < 0+100+100; PROTECT=200
        // gives 200+20 > LOW=199. I12 then gives about 99.75 kHz SCL.
        for (off, value) in [
            (0, I2C_LOW), (0x30, 20), (0x34, 80),
            (0x38, I2C_HIGH | (I2C_WAIT_HIGH << 9)),
            (0x40, I2C_START_HOLD), (0x44, I2C_RSTART_SETUP),
            (0x48, I2C_STOP_HOLD), (0x4c, I2C_STOP_SETUP),
            (0x50, 0), (0x84, 200),
        ] { self.store(I2C0 + off, value); }
        self.store(I2C_CTR, I2C_MASTER | (1 << 11));
        // I07: FIFO reset bits are R/W, so release them explicitly. Keep
        // protection and the Appendix A thresholds (TX 4, RX 11).
        self.store(I2C0 + 0x18, 0x408b | (3 << 12));
        self.store(I2C0 + 0x18, 0x408b);
        // I16: RAW includes unknown idle FSM timeouts; poll only these known
        // sources through STATUS. CPU INTENABLE remains zero at handoff.
        self.store(I2C_ENA, I2C_COMPLETE | I2C_NACK);
    }

    pub fn i2c_list(&mut self, tx: &[u8], commands: &[u32]) -> u32 {
        for &byte in tx { self.store(I2C_DATA, u32::from(byte)); }
        // I02: software clears DONE when replacing commands, including slots
        // a previous longer list used and a NACK-generated STOP will skip.
        for n in 0..8 {
            self.store(I2C0 + 0x58 + 4 * n, commands.get(n as usize).copied().unwrap_or(0));
        }
        self.store(I2C_CTR, I2C_MASTER | (1 << 5))
    }

    pub fn i2c_wait(&mut self) {
        self.load(1, I2C_STATUS);
        self.emit(enc::movi(2, I2C_COMPLETE as i32));
        let poll = self.emit(enc::l32i(5, 1, 0));
        self.emit(enc::and(6, 5, 2));
        self.emit(enc::bccz(ccz::EQ, 6, (i64::from(poll) - i64::from(self.pc()) - 4) as i32));
    }
}
