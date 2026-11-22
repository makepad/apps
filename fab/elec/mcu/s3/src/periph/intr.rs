//! CORE0 interrupt matrix. Source levels are supplied by the peripheral blocks;
//! CPU edge latches remain in Cpu (and are cloned with it by mark/rewind).

use fab_elec_mcu_xtensa::sim::{Config, IntType};

use super::{Access, ResetScope};

// facts-machine.md:104,108,114–225 (§2): CORE0 starts here and its last
// documented word is DATE at +0x7fc. No CPU1 bank address is given there.
const BASE: u32 = 0x600c_2000;
const RESERVED: &str = "reserved interrupt-matrix field is undocumented";

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct Intr {
    map: [u8; 99],
    clock_gate: u32,
    date: u32,
}

impl Intr {
    pub fn new() -> Self {
        // facts-machine.md:108,224–225: documented register resets.
        // M: use these at ROM handoff too; the ROM's actual map/gate/date
        // handoff values remain a chip-probe item.
        Self { map: [16; 99], clock_gate: 1, date: 0x0201_2300 }
    }

    pub fn reset(&mut self, scope: ResetScope) {
        // INFERRED from facts-machine.md:356 (§4): Core resets digital
        // peripherals except RTC; System includes RTC; CPU-only keeps them.
        // Cause rows: :363 (Core SW), :369 (CPU0 SW), :372 (System RWDT).
        if scope != ResetScope::Cpu { *self = Self::new(); }
    }

    fn offset(addr: u32) -> Option<u32> {
        addr.checked_sub(BASE).filter(|&off| off < 0x800)
    }

    pub fn read(&self, addr: u32, sources: u128) -> Access<u32> {
        let off = Self::offset(addr)?;
        Some(match off {
            0..=0x188 => Ok(u32::from(self.map[(off / 4) as usize])),
            // INFERRED indexing, facts-machine.md:220–227: live source
            // N occupies word N/32, bit N%32, independently of its map.
            0x18c..=0x198 => Ok((sources >> (((off - 0x18c) / 4) * 32)) as u32),
            0x19c => Ok(self.clock_gate),
            0x7fc => Ok(self.date),
            _ => Err("undecoded interrupt-matrix register"),
        })
    }

    pub fn write(&mut self, addr: u32, value: u32) -> Access<()> {
        let off = Self::offset(addr)?;
        Some((|| {
            // Validate before mutation: every refused write preserves the
            // whole bank, as required by the peripheral access contract.
            match off {
                0..=0x188 => {
                    if value & !31 != 0 { return Err(RESERVED); }
                    // facts-machine.md:253, Q6: CPU NMI entry is outside
                    // this model, even when the mapped source is low.
                    if value == 14 { return Err("routing a source to the NMI interrupt is not modelled"); }
                    self.map[(off / 4) as usize] = value as u8;
                }
                0x18c..=0x198 => return Err("write to read-only interrupt-matrix register"),
                0x19c => {
                    if value & !1 != 0 { return Err(RESERVED); }
                    // facts-machine.md:224,227: gating effect NOT FOUND.
                    if value == 0 { return Err("interrupt-matrix clock gating is undocumented"); }
                    self.clock_gate = value;
                }
                0x7fc => {
                    // facts-machine.md:225: R/W [27:0], reset 0x2012300.
                    if value & !0x0fff_ffff != 0 { return Err(RESERVED); }
                    self.date = value;
                }
                _ => return Err("undecoded interrupt-matrix register"),
            }
            Ok(())
        })())
    }

    pub fn destination(&self, source: usize, config: &Config) -> Option<u32> {
        let line = self.map[source] as usize;
        // facts-machine.md:108,239–270; config.rs transcribes core-isa.h.
        // Internal lines 6/7/11/15/16/29 disconnect; 14 is refused above.
        matches!(config.int_type[line], IntType::Level | IntType::Edge).then_some(line as u32)
    }

    pub fn lines(&self, mut sources: u128, config: &Config) -> u32 {
        let mut lines = 0;
        while sources != 0 {
            let source = sources.trailing_zeros() as usize;
            sources &= sources - 1;
            // INFERRED OR, facts-machine.md:229: overlapping sources do
            // not manufacture another rising edge on an already-high line.
            if let Some(line) = self.destination(source, config) { lines |= 1 << line; }
        }
        lines
    }

    pub fn source_name(&self, line: u32, sources: u128) -> &'static str {
        // A latched CPU edge can outlive its source and even its mapping.
        // Prefer a currently high contributor, then a still-mapped source.
        for active_only in [true, false] {
            for source in [16, 21, 42, 43, 57, 58, 59, 71, 72, 73, 74, 75, 96] {
                if u32::from(self.map[source]) == line && (!active_only || sources & (1 << source) != 0) {
                    return Self::name(source);
                }
            }
        }
        "peripheral interrupt"
    }

    pub fn name(source: usize) -> &'static str {
        // facts-machine.md:130,171–173,210 (SYSTIMER suffix shortened).
        match source {
            16 => "GPIO_INTERRUPT_CPU",
            21 => "SPI_INTR_2", // facts-io.md S15: level, not source 44.
            // facts-io.md I19/D-I4: symbolic instance names, not reversed comments.
            42 => "I2C_EXT0_INTR",
            43 => "I2C_EXT1_INTR",
            57 => "SYSTIMER_TARGET0",
            58 => "SYSTIMER_TARGET1",
            59 => "SYSTIMER_TARGET2",
            // facts-io.md G12: independent level source per OUT channel.
            71 => "DMA_OUT_CH0_INTR",
            72 => "DMA_OUT_CH1_INTR",
            73 => "DMA_OUT_CH2_INTR",
            74 => "DMA_OUT_CH3_INTR",
            75 => "DMA_OUT_CH4_INTR",
            96 => "USB_DEVICE_INT",
            _ => "peripheral event",
        }
    }
}
