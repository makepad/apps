//! GP-SPI2/GDMA firmware, facts-io.md S01–S18, G01–G16, Appendices B/C.

use fab_elec_mcu_emit32::xtensa::{ccz, enc};

use super::{mux_reg, out_sel_reg, pin_reg, Program, CPU_PER, GPIO_BASE, MUX_GPIO, MUX_IE, SYSCLK};

pub const SPI2: u32 = 0x6002_4000;
pub const SPI_RAW: u32 = SPI2 + 0x3c;
pub const SPI_CLR: u32 = SPI2 + 0x38;
pub const SPI_DONE: u32 = 1 << 12;
pub const GDMA: u32 = 0x6003_f000;
pub const STRIP: u32 = 0x3fc9_0000;
pub const DESCRIPTORS: u32 = 0x3fc8_9000;
pub const HALF_BYTES: usize = 368 * 32 * 2;
pub const DESCRIPTORS_PER_HALF: usize = (HALF_BYTES + 4094) / 4095;

impl Program {
    pub fn spi_setup(&mut self) {
        // The model accepts PLL480 period 0: CPU 80 MHz, APB 80 MHz.
        self.store(CPU_PER, 4);
        self.time_bits(SYSCLK, 3 << 10, 1 << 10);
        // S17/G12/D-G3: hold resets, enable clocks, release resets.
        let spi = (1 << 6) | (1 << 22);
        self.time_bits(0x600c_0020, 0, spi);
        self.time_bits(0x600c_0024, 0, 1 << 6);
        self.time_bits(0x600c_0018, 0, spi);
        self.time_bits(0x600c_001c, 0, 1 << 6);
        self.time_bits(0x600c_0020, spi, 0);
        self.time_bits(0x600c_0024, 1 << 6, 0);
        // S16: D/Q/WP/HD in lane order, CLK and CS0 through the matrix.
        for (pad, signal) in [(4, 103), (5, 102), (6, 105), (7, 104), (11, 101), (12, 110)] {
            self.store(pin_reg(pad), 0);
            self.store(mux_reg(pad), MUX_GPIO | MUX_IE);
            self.store(out_sel_reg(pad), signal);
        }
        self.store(pin_reg(8), 0);
        self.store(mux_reg(8), MUX_GPIO | MUX_IE);
        self.store(out_sel_reg(8), 256);
        self.store(GPIO_BASE + 0x08, 1 << 8); // OUT_W1TS before enabling RESX.
        self.store(GPIO_BASE + 0x24, (0xf << 4) | (1 << 8) | (1 << 11) | (1 << 12));
        // S08: PLL_F80M / (N+1) = 40 MHz, L=N=1, H=0, PRE=0.
        self.store(SPI2 + 0x0c, (1 << 12) | 1);
        self.store(SPI2 + 0x08, 0x003c_0000); // Preserve reset idle polarities.
        self.store(SPI2 + 0x14, (23 << 27) | (1 << 22) | (1 << 16) | 7);
        self.store(SPI2 + 0x20, 0x3e); // CS0 only, mode 0, no KEEP_ACTIVE.
        self.store(SPI2 + 0xe8, 6); // MST_CLK_ACTIVE and PLL_F80M selection.
        self.store(GDMA + 0x60, 12); // OUT_EOF_MODE=1, AUTO_WRBACK=1 (G06/G10).
        self.store(GDMA + 0x64, 1 << 12); // OUT_CHECK_OWNER.
        self.store(GDMA + 0xa8, 0); // Only OUT0 selects SPI2 (G01).
    }

    pub fn spi_delay_us(&mut self, us: u32) {
        // 80 MHz CPU, one instruction per cycle in this model. Every wait
        // uses counted loops; round up and include their setup/branch costs.
        if us <= 50 {
            self.counted_loop((us * 40) as i32);
        } else {
            // counted_loop(2000) is 4001 instructions; outer ADDI/BNEZ add 2.
            self.load(7, (us * 80 + 4002) / 4003);
            let start = self.pc();
            self.counted_loop(2000);
            self.emit(enc::addi(7, 7, -1));
            self.emit(enc::bccz(ccz::NE, 7, (i64::from(start) - i64::from(self.pc()) - 4) as i32));
        }
    }

    pub fn spi_panel_reset(&mut self) {
        self.store(GPIO_BASE + 0x0c, 1 << 8);
        self.spi_delay_us(20);
        self.store(GPIO_BASE + 0x08, 1 << 8);
        self.spi_delay_us(120_000); // P06: 120 ms after RESX before SLPOUT.
    }

    pub fn spi_transfer(&mut self, command: u8, bytes: &[u8], dma_half: Option<usize>) -> u32 {
        let len = if dma_half.is_some() { HALF_BYTES } else { bytes.len() };
        // Table 30.5-10: a 24-bit address is ADDR_VALUE[31:8]. The panel's
        // wire address is {0, CMD, 0}, so the register holds CMD << 16.
        self.store(SPI2 + 4, u32::from(command) << 16);
        self.store(SPI2 + 0x18, (7 << 28) | (1 << 27) | if dma_half.is_some() { 0x32 } else { 0x02 });
        self.store(SPI2 + 0x1c, if len == 0 { 0 } else { len as u32 * 8 - 1 });
        let user = (1 << 31) | (1 << 30) | if len != 0 { 1 << 27 } else { 0 }
            | if dma_half.is_some() { 1 << 13 } else { 0 };
        // CS_SETUP/HOLD=0 gives half a 40 MHz clock (6 ticks) on each side.
        self.store(SPI2 + 0x10, user);
        self.store(SPI2 + 0x30, if dma_half.is_some() { 1 << 28 } else { 0 });
        if let Some(half) = dma_half {
            let address = DESCRIPTORS + (half * DESCRIPTORS_PER_HALF * 12) as u32;
            self.store(GDMA + 0x80, (address & 0xfffff) | (1 << 21));
        } else if !bytes.is_empty() {
            assert!(bytes.len() <= 4, "CPU panel command parameters fit W0");
            let mut word = [0; 4];
            word[..bytes.len()].copy_from_slice(bytes);
            self.store(SPI2 + 0x98, u32::from_le_bytes(word));
        }
        self.store(SPI2, 1 << 23); // S06: latch every configuration write.
        self.store(SPI2, 1 << 24)
    }

    pub fn spi_wait(&mut self) {
        self.load(1, SPI_RAW);
        self.load(2, SPI_DONE);
        let poll = self.emit(enc::l32i(5, 1, 0));
        self.emit(enc::and(6, 5, 2));
        self.emit(enc::bccz(ccz::EQ, 6, (i64::from(poll) - i64::from(self.pc()) - 4) as i32));
    }

    pub fn spi_result(&mut self, addr: u32) {
        // a6 is the final poll's masked TRANS_DONE value, not a constant.
        self.load(3, addr);
        self.emit(enc::s32i(6, 3, 0));
        self.store(SPI_CLR, SPI_DONE);
    }
}

pub fn descriptor_bytes() -> Vec<u8> {
    let mut bytes = Vec::new();
    for half in 0..2 {
        let mut remaining = HALF_BYTES;
        let mut buffer = STRIP + (half * HALF_BYTES) as u32;
        for n in 0..DESCRIPTORS_PER_HALF {
            let len = remaining.min(4095);
            let final_descriptor = n + 1 == DESCRIPTORS_PER_HALF;
            let dw0 = (1 << 31) | if final_descriptor { 1 << 30 } else { 0 }
                | (len as u32) << 12 | len as u32;
            let next = if final_descriptor { 0 }
                else { DESCRIPTORS + ((half * DESCRIPTORS_PER_HALF + n + 1) * 12) as u32 };
            for word in [dw0, buffer, next] { bytes.extend_from_slice(&word.to_le_bytes()); }
            remaining -= len;
            buffer += len as u32;
        }
    }
    bytes
}
