//! Digital GPIO/IO_MUX and USB-Serial-JTAG's serial EP1 (link 0).
//! Citations use facts.md and facts-machine.md under electronics/emcu/s3,
//! ESP32-S3 TRM v1.8, and the approved ESP-IDF v6.1.1 soc headers.
//! Profiles specify ROM handoff state; hardware reset diagrams alone do
//! not establish what the ROM leaves behind. Unspecified state is seeded junk.

use std::collections::VecDeque;

use fab_elec_mcu_emu::{Drive, LinkOut, Pull};

use super::{Access, ResetRequest, ResetScope};
use crate::Junk;

// facts.md §4, TRM p.408 Table 4.3-3 and §4e.
const GPIO: u32 = 0x6000_4000;
const IO_MUX: u32 = 0x6000_9000;
const USB: u32 = 0x6003_8000;
// facts-machine.md §6, TRM pp.472,478: GPIO22–25 and GPIO49+ do not exist.
const VALID: [u32; 2] = [0xfc3f_ffff, 0x0001_ffff];
// facts-machine.md §6, TRM pp.507–508; gpio_reg.h:277–319,7789–7813.
const PIN_FIELDS: u32 = 0x0003_ff9f;
const OUT_FIELDS: u32 = 0xfff;
const GPIO_SIGNAL: u32 = 256; // gpio_sig_map.h:449, SIG_GPIO_OUT_IDX.
// facts-machine.md §6, TRM pp.514–515; io_mux_reg.h:10–70,140.
const FUN_PD: u32 = 1 << 7;
const FUN_PU: u32 = 1 << 8;
const FUN_IE: u32 = 1 << 9;
const GPIO_FUNCTION: u32 = 1 << 12;
// facts-machine.md §7, TRM pp.1248–1249,1259–1262: serial packets and IRQs.
const PACKET: usize = 64;
const RX_PACKET: u32 = 1 << 2;
const TX_EMPTY: u32 = 1 << 3;
// usb_serial_jtag_reg.h:69; TRM p.1259 §33.6: IN token for IN endpoint 1.
const IN_TOKEN_REC_IN_EP1: u32 = 1 << 8;
const USB_IRQS: u32 = 0xfff;

/// Board and ROM-handoff assumptions. Pin arrays use Machine::pins() order,
/// GPIO0–21 then GPIO26–48. Undocumented ROM state has no implicit default.
#[derive(Clone, Debug)]
pub struct PinsProfile {
    /// M: board pull-up / BOOT button on GPIO0 at power-on; facts.md §1,
    /// TRM pp.534–535. Later handoffs sample the current external input.
    pub power_on_gpio0_high: bool,
    /// M: adverse host-service assumption: at most one 64-byte EP1 packet
    /// each way per service. Nonzero; 480000 ticks on the 480 MHz model grid
    /// means 1 ms (64000 B/s each way before framing). Replace this period
    /// with the first board run's measured service rate.
    pub usb_service_ticks: u64,
    /// M: an attached, listening host services EP1 on this phase of each
    /// service period, in each reset epoch (less than usb_service_ticks).
    /// Fable Q6 USB row, fable-answers.md:1635; facts-machine.md §7.
    pub usb_frame_phase_ticks: u64,
    /// M: zero input synchronizer delay; facts-machine.md §6 / TRM
    /// §§6.4.2–6.4.3 give no observation latency. Only zero is supported.
    pub input_sync_delay_ticks: u64,
    /// M: pads use the digital mux without retained holds at ROM handoff;
    /// facts.md §4a / TRM §§6.7.2,6.9. Only true is supported.
    pub handoff_digital_pads_unheld: bool,
    /// M: OUT/OUT1 valid bits at ROM handoff; facts-machine.md §6 documents
    /// hardware reset zero (TRM pp.501–503), not the ROM's final latches.
    pub handoff_out: [u32; 2],
    /// M: ENABLE/ENABLE1 valid bits at ROM handoff; facts-machine.md §6,
    /// TRM pp.504–505 document reset zero, not the ROM's final enables.
    pub handoff_enable: [u32; 2],
    /// M: PINn fields at handoff; facts-machine.md §6 / TRM p.507 document
    /// reset zero, not ROM writes. Reserved bits remain seeded junk.
    pub handoff_pin: [u32; 45],
    /// M: IO_MUX fields at handoff; facts-machine.md §6 / TRM pp.493–494,
    /// 514–515 give per-pin RST states but no complete per-pin reset words
    /// or final ROM state. GPIO function is 1 (io_mux_reg.h:140).
    pub handoff_io_mux: [u32; 45],
    /// M: output matrix fields at handoff; facts-machine.md §6 / TRM p.508
    /// document reset selector 256, not the ROM's final routing.
    pub handoff_out_sel: [u32; 45],
    /// M: bytes left in the serial IN packet at handoff, at most 64;
    /// facts-machine.md §7 / TRM §33.3.3 do not specify ROM FIFO contents.
    pub handoff_usb_tx: Vec<u8>,
    /// M: whether the handoff IN packet is flushed; facts-machine.md §7,
    /// TRM §33.3.3. A full 64-byte packet must already be flushed.
    pub handoff_usb_tx_flushed: bool,
    /// M: unread serial OUT packet at handoff, at most 64 bytes;
    /// facts-machine.md §7 / TRM §33.3.3 do not specify ROM FIFO contents.
    pub handoff_usb_rx: Vec<u8>,
    /// M: USB RAW at handoff; facts-machine.md §7 / TRM pp.1259–1262
    /// give hardware reset 0x8, but no complete ROM handoff state.
    pub handoff_usb_int_raw: u32,
    /// M: USB ENA at handoff; facts-machine.md §7 / TRM pp.1259–1262
    /// give hardware reset zero, but no complete ROM handoff state.
    pub handoff_usb_int_ena: u32,
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct Pins {
    out: [u32; 2],
    enable: [u32; 2],
    status: [u32; 2],
    gpio_edge_at: [Option<u64>; 45],
    input: [u32; 2],
    input_junk: [u32; 2],
    pin: [u32; 45],
    mux: [u32; 45],
    out_sel: [u32; 45],
    in_sel: [u32; 256],
    strap: u32,
    clock_gate: u32,
    generation: u64,
    spi_cs: Option<u16>,
    spi_idle_high: bool,
    tx: Vec<u8>,
    flushed: bool,
    rx: VecDeque<u8>,
    // External host state: cloned for rewind and retained across every reset.
    host: VecDeque<u8>,
    host_listening: bool,
    usb_raw: u32,
    usb_ena: u32,
    usb_conf0: u32,
    usb_junk: [u32; 3],
    usb_set_at: Option<u64>,
    usb_set_bits: u32,
    usb_service_ticks: u64,
    frame_phase: u64,
    due: Option<u64>,
    // Modelled timing assumptions, cloned with the rest of the rewind state.
    notes: Vec<(u64, &'static str)>,
}

fn gpio_number(pin: usize) -> usize {
    assert!(pin < 45, "S3 pin index is outside Machine::pins()");
    if pin < 22 { pin } else { pin + 4 }
}

fn pin_index(number: usize) -> Result<usize, &'static str> {
    match number {
        0..=21 => Ok(number),
        26..=48 => Ok(number - 4),
        _ => Err("GPIO register names an unimplemented pin"),
    }
}

fn bank_bit(pin: usize) -> (usize, u32) {
    let n = gpio_number(pin);
    (n / 32, 1 << (n % 32))
}

fn fields(old: u32, mask: u32, value: u32) -> u32 { (old & !mask) | (value & mask) }

// facts.md §4a and facts-machine.md §6, TRM pp.501–511: OUT, ENABLE,
// STATUS, their two banks and write-one aliases.
fn bank_register(offset: u32) -> Option<(usize, usize, u32)> {
    for (kind, base) in [0x04, 0x20, 0x44].into_iter().enumerate() {
        if (base..base + 24).contains(&offset) && (offset - base) % 4 == 0 {
            let word = (offset - base) / 4;
            return Some((kind, (word / 3) as usize, word % 3));
        }
    }
    None
}

impl Pins {
    pub fn new(profile: &PinsProfile, junk: &mut Junk) -> Self {
        assert!(profile.usb_service_ticks != 0, "USB service period must be nonzero");
        assert!(profile.usb_frame_phase_ticks < profile.usb_service_ticks,
            "USB frame phase must be below the service period");
        assert_eq!(profile.input_sync_delay_ticks, 0, "nonzero GPIO synchronizer delay is not modelled");
        assert!(profile.handoff_digital_pads_unheld, "RTC pad routing and retained holds are not modelled");
        assert!(profile.handoff_usb_tx.len() <= PACKET && profile.handoff_usb_rx.len() <= PACKET,
            "handoff serial EP1 packets must fit 64 bytes");
        assert!(profile.handoff_usb_tx.len() < PACKET || profile.handoff_usb_tx_flushed,
            "a full handoff serial IN packet must be flushed");
        let mut this = Self {
            out: std::array::from_fn(|b| fields(junk.u32(), VALID[b], profile.handoff_out[b])),
            enable: std::array::from_fn(|b| fields(junk.u32(), VALID[b], profile.handoff_enable[b])),
            status: std::array::from_fn(|_| junk.u32()),
            gpio_edge_at: [None; 45],
            input: std::array::from_fn(|_| junk.u32()),
            input_junk: std::array::from_fn(|_| junk.u32()),
            pin: std::array::from_fn(|p| fields(junk.u32(), PIN_FIELDS, profile.handoff_pin[p])),
            mux: std::array::from_fn(|p| fields(junk.u32(), 0xffff, profile.handoff_io_mux[p])),
            out_sel: std::array::from_fn(|p| fields(junk.u32(), OUT_FIELDS, profile.handoff_out_sel[p])),
            in_sel: std::array::from_fn(|_| junk.u32()),
            strap: junk.u32(), clock_gate: junk.u32(), generation: 0,
            spi_cs: None, spi_idle_high: false,
            tx: profile.handoff_usb_tx.clone(), flushed: profile.handoff_usb_tx_flushed,
            rx: profile.handoff_usb_rx.iter().copied().collect(), host: VecDeque::new(),
            host_listening: true,
            usb_raw: fields(junk.u32(), USB_IRQS, profile.handoff_usb_int_raw),
            usb_ena: fields(junk.u32(), USB_IRQS, profile.handoff_usb_int_ena),
            usb_conf0: junk.u32(), usb_junk: std::array::from_fn(|_| junk.u32()),
            usb_set_at: None, usb_set_bits: 0,
            usb_service_ticks: profile.usb_service_ticks,
            frame_phase: profile.usb_frame_phase_ticks, due: None,
            notes: Vec::new(),
        };
        this.input[0] = fields(this.input[0], 1, u32::from(profile.power_on_gpio0_high));
        for p in 0..45 {
            Self::check_pin(this.pin[p]).expect("unsupported PINn handoff profile");
            Self::check_selector(this.out_sel[p]).expect("unsupported output selector handoff profile");
            this.check_mux(p, this.mux[p], this.enable, this.out_sel[p])
                .expect("unsupported IO_MUX handoff profile");
        }
        this.latch_straps();
        this.latch_levels();
        this.schedule(0);
        this
    }

    /// Append pending model assumptions to the machine's visible run events.
    pub fn take_notes(&mut self, out: &mut Vec<(u64, &'static str)>) {
        out.append(&mut self.notes);
    }

    fn check_pin(value: u32) -> Result<(), &'static str> {
        // facts-machine.md §6, TRM p.507: INT_TYPE 0–5 and ENA bits 13/14.
        if (value >> 7) & 7 >= 6 { return Err("GPIO INT_TYPE 6 or 7 is undocumented"); }
        if value & (7 << 15) != 0 { return Err("GPIO INT_ENA bits 17:15 are undocumented"); }
        Ok(())
    }

    fn check_selector(value: u32) -> Result<(), &'static str> {
        // facts-machine.md §6, TRM pp.477–478,508; gpio_sig_map.h:449.
        match value & 0x1ff {
            22..=27 if value & ((1 << 9) | (1 << 11)) != 0 =>
                Err("I2S inverted output data or output enable is unsupported"), // trm.txt:17230–17239.
            22..=27 => Ok(()), // I2S0 outputs, trm.txt:16185–16187, 16205–16207.
            GPIO_SIGNAL | 89..=92 | 101..=105 | 110 => Ok(()), // I20, S16.
            257..=511 => Err("GPIO output selector above 256 is undocumented"),
            _ => Err("GPIO output selector names an unmodelled peripheral signal"),
        }
    }

    fn enabled(pin: usize, enable: [u32; 2], selector: u32) -> bool {
        let (bank, bit) = bank_bit(pin);
        // Selector 256 uses GPIO_ENABLE for either OEN_SEL value, followed
        // by OEN_INV_SEL[11]. facts-machine.md §6, TRM pp.477–478,508.
        (enable[bank] & bit != 0) ^ (selector & (1 << 11) != 0)
    }

    fn check_mux(&self, pin: usize, mux: u32, enable: [u32; 2], selector: u32) -> Result<(), &'static str> {
        // facts-machine.md §6 leaves opposed pulls and filter observation
        // unspecified. Sleep fields are stored; WAITI is not SoC Light-sleep.
        if mux & (FUN_PU | FUN_PD) == (FUN_PU | FUN_PD) || mux & 12 == 12 {
            return Err("simultaneous IO_MUX pull-up and pull-down is undocumented");
        }
        if mux & FUN_IE != 0 && mux & (1 << 15) != 0 {
            return Err("IO_MUX input filter timing and observation point are undocumented");
        }
        if Self::enabled(pin, enable, selector) && mux & (7 << 12) != GPIO_FUNCTION {
            return Err("IO_MUX non-GPIO function on a driven pin is not modelled");
        }
        Ok(())
    }

    fn active_levels(&self) -> [u32; 2] {
        let mut active = [0; 2];
        for p in 0..45 {
            if self.mux[p] & FUN_IE == 0 { continue; }
            let (bank, bit) = bank_bit(p);
            let high = self.input[bank] & bit != 0;
            // facts-machine.md §6, TRM p.507: INT_TYPE 4 low, 5 high.
            if matches!((self.pin[p] >> 7 & 7, high), (4, false) | (5, true)) { active[bank] |= bit; }
        }
        active
    }

    fn latch_levels(&mut self) {
        let active = self.active_levels();
        for (status, active) in self.status.iter_mut().zip(active) { *status |= active; }
    }

    fn latch_straps(&mut self) {
        // facts.md §1, TRM p.506: STRAP bits 2/3/4/5 = GPIO46/0/45/3.
        // The machine handoff contract samples every reset, including Cpu;
        // this is a ROM-handoff model rather than chip-reset timing.
        for (number, bit) in [(46, 2), (0, 3), (45, 4), (3, 5)] {
            let high = self.input[number / 32] >> (number % 32) & 1;
            self.strap = fields(self.strap, 1 << bit, high << bit);
        }
    }

    pub fn boot_strap_flash(&self) -> bool {
        // facts.md §1, TRM p.535 Table 8.2-1: GPIO0 high selects SPI boot.
        self.strap & (1 << 3) != 0
    }

    fn input_word(&self, bank: usize) -> u32 {
        let mut word = self.input_junk[bank];
        for p in 0..45 {
            let (b, bit) = bank_bit(p);
            // facts.md §4a, TRM §§6.4.3–6.4.4: IN requires FUN_IE.
            // Disabled/absent inputs have no sourced readback: retain junk.
            if b == bank && self.mux[p] & FUN_IE != 0 { word = fields(word, bit, self.input[b]); }
        }
        word
    }

    fn qualified_status(&self, bank: usize, ena_bit: u32) -> u32 {
        let mut word = self.status[bank] & !VALID[bank];
        // Each valid status bit set, kept when its pin's enable bit is set.
        let mut set = self.status[bank] & VALID[bank];
        while set != 0 {
            let n = set.trailing_zeros();
            set &= set - 1;
            if pin_index(bank * 32 + n as usize).is_ok_and(|p| self.pin[p] & (1 << ena_bit) != 0) { word |= 1 << n; }
        }
        word
    }

    pub fn interrupt_sources(&self) -> u128 {
        // facts-machine.md:613,623,625,629 (§6): normal source 16 is the
        // OR of valid status bits qualified by each pin's enable bit 13.
        // Reserved/absent pin junk must never become an interrupt source.
        // A bank with no valid status bit qualifies none: skip its pin loop.
        let gpio = (0..2).fold(0, |gpio, bank| {
            if self.status[bank] & VALID[bank] == 0 { gpio } else { gpio | (self.qualified_status(bank, 13) & VALID[bank]) }
        });
        // facts-machine.md:658,677 (§7): RAW AND ENA holds source 96;
        // only the twelve defined causes participate, not reserved junk.
        let usb = self.usb_raw & self.usb_ena & USB_IRQS;
        (u128::from(gpio != 0) << 16) | (u128::from(usb != 0) << 96)
    }

    pub fn read(&mut self, addr: u32, tick: u64) -> Access<u32> {
        if (USB..USB + 0x80).contains(&addr) { return Some(self.usb_read(addr - USB, tick)); }
        if (IO_MUX + 4..=IO_MUX + 4 + 48 * 4).contains(&addr) {
            return Some(pin_index(((addr - IO_MUX - 4) / 4) as usize).map(|p| self.mux[p]));
        }
        let off = addr.checked_sub(GPIO)?;
        if let Some((kind, bank, alias)) = bank_register(off) {
            // facts-machine.md §6 explicitly leaves WO readback unspecified.
            return Some(if alias != 0 { Err("GPIO write-only alias readback is undocumented") }
                else { Ok(match kind { 0 => self.out[bank], 1 => self.enable[bank], _ => self.status[bank] }) });
        }
        Some(match off {
            0x38 => Ok(self.strap),
            0x3c | 0x40 => Ok(self.input_word(((off - 0x3c) / 4) as usize)),
            // facts-machine.md §6, TRM pp.509–510: normal / NMI qualification.
            0x5c => Ok(self.qualified_status(0, 13)),
            0x60 => Ok(self.qualified_status(0, 14)),
            0x68 => Ok(self.qualified_status(1, 13)),
            0x6c => Ok(self.qualified_status(1, 14)),
            0x64 | 0x70 => Err("GPIO CPUSDIO status behavior is undocumented"),
            0x74..=0x134 => pin_index(((off - 0x74) / 4) as usize).map(|p| self.pin[p]),
            // facts-machine.md §6 / TRM p.507: 256 matrix input words.
            0x154..=0x550 => Ok(self.in_sel[((off - 0x154) / 4) as usize]),
            0x554..=0x614 => pin_index(((off - 0x554) / 4) as usize).map(|p| self.out_sel[p]),
            0x62c => Ok(self.clock_gate), // TRM p.508, gpio_reg.h:9193–9199.
            _ => return None,
        })
    }

    pub fn write(&mut self, addr: u32, value: u32, tick: u64) -> Access<()> {
        if (USB..USB + 0x80).contains(&addr) { return Some(self.usb_write(addr - USB, value, tick)); }
        if (IO_MUX + 4..=IO_MUX + 4 + 48 * 4).contains(&addr) {
            return Some((|| {
                let p = pin_index(((addr - IO_MUX - 4) / 4) as usize)?;
                self.check_mux(p, value, self.enable, self.out_sel[p])?;
                self.mux[p] = fields(self.mux[p], 0xffff, value);
                self.latch_levels();
                self.generation = self.generation.wrapping_add(1);
                Ok(())
            })());
        }
        let off = addr.checked_sub(GPIO)?;
        if let Some((kind, bank, alias)) = bank_register(off) { return Some(self.write_bank(kind, bank, alias, value, tick)); }
        Some(match off {
            0x38 | 0x3c | 0x40 | 0x5c | 0x60 | 0x68 | 0x6c => Err("write to read-only GPIO register"),
            0x64 | 0x70 => Err("GPIO CPUSDIO status behavior is undocumented"),
            0x74..=0x134 => (|| {
                let p = pin_index(((off - 0x74) / 4) as usize)?;
                Self::check_pin(value)?;
                self.pin[p] = fields(self.pin[p], PIN_FIELDS, value);
                self.latch_levels();
                self.generation = self.generation.wrapping_add(1);
                Ok(())
            })(),
            0x154..=0x550 => {
                // facts-machine.md §6 / TRM p.507. I2C resolves its inputs
                // from this same matrix at TRANS_START (facts-io.md I20).
                let word = &mut self.in_sel[((off - 0x154) / 4) as usize];
                *word = fields(*word, 0xff, value);
                Ok(())
            }
            0x554..=0x614 => (|| {
                let p = pin_index(((off - 0x554) / 4) as usize)?;
                Self::check_selector(value)?;
                self.check_mux(p, self.mux[p], self.enable, value)?;
                self.out_sel[p] = fields(self.out_sel[p], OUT_FIELDS, value);
                self.generation = self.generation.wrapping_add(1);
                Ok(())
            })(),
            0x62c => {
                // TRM p.508: CLK_EN[0] forces the clock free-running. It is
                // not SYSTEM's peripheral gate and changes no logical state.
                self.clock_gate = fields(self.clock_gate, 1, value);
                Ok(())
            }
            _ => return None,
        })
    }

    fn write_bank(&mut self, kind: usize, bank: usize, alias: u32, value: u32, tick: u64) -> Result<(), &'static str> {
        // facts-machine.md §6: invalid-bit storage is NOT FOUND. Reads keep
        // junk in those bits; writes naming them stop instead of discarding.
        if value & !VALID[bank] != 0 { return Err("GPIO bank write names an unimplemented pin"); }
        let old = match kind { 0 => self.out[bank], 1 => self.enable[bank], _ => self.status[bank] };
        let mut next = match alias { 0 => fields(old, VALID[bank], value), 1 => old | value, _ => old & !value };
        if kind == 1 {
            let mut enable = self.enable;
            enable[bank] = next;
            for p in 0..45 { self.check_mux(p, self.mux[p], enable, self.out_sel[p])?; }
        }
        if kind == 2 {
            let clearing = if alias == 2 { value } else if alias == 0 { !value & VALID[bank] } else { 0 };
            let active = self.active_levels()[bank];
            if clearing & active != 0 {
                // M: facts-machine.md §6 leaves reassertion timing open.
                // A still-active source immediately undoes its clear.
                next |= active;
                self.notes.push((tick, "GPIO level interrupt cleared while its level is active; hardware timing undocumented; modelled as re-asserted at once"));
            }
            for p in 0..45 {
                let (b, bit) = bank_bit(p);
                if b == bank && clearing & bit != 0 && self.gpio_edge_at[p] == Some(tick) {
                    // M: facts-machine.md §6 leaves detection/clear priority
                    // open. The clear commits after the edge, losing it.
                    self.notes.push((tick, "GPIO interrupt cleared in the tick of a new edge; hardware priority undocumented; modelled as the edge lost"));
                    break;
                }
            }
        }
        match kind { 0 => self.out[bank] = next, 1 => self.enable[bank] = next, _ => self.status[bank] = next }
        if kind != 2 { self.generation = self.generation.wrapping_add(1); }
        Ok(())
    }

    pub fn drive(&self, pin: usize) -> Drive {
        let (bank, bit) = bank_bit(pin);
        let pull = if self.mux[pin] & FUN_PU != 0 { Pull::Up }
            else if self.mux[pin] & FUN_PD != 0 { Pull::Down } else { Pull::None };
        if (89..=92).contains(&(self.out_sel[pin] & 0x1ff)) {
            // M: I2C travels byte by byte through Board::i2c; per-bit SCL/SDA
            // edges are not pad drive changes. I15 requires open drain;
            // TRANS_START checks PAD_DRIVER independently of FORCE_OUT (D-I1).
            return Drive::OpenDrain { low: false, pull };
        }
        let signal = self.out_sel[pin] & 0x1ff;
        if (22..=27).contains(&signal) {
            // M: edges travel in Board::i2s frames. Park enabled audio pads low between records.
            // trm.txt:16185–16187, 16205–16207: peripheral output enable is constant one.
            return if self.out_sel[pin] & (1 << 10) == 0 || Self::enabled(pin, self.enable, self.out_sel[pin]) {
                Drive::Low
            } else { Drive::Input(pull) };
        }
        if matches!(signal, 101..=105 | 110) {
            if self.out_sel[pin] & (1 << 10) != 0 && !Self::enabled(pin, self.enable, self.out_sel[pin]) {
                return Drive::Input(pull);
            }
            // S09/S11: keep only idle clock/data levels; their edges travel
            // through Board::spi. CS uses the ordinary pad-drive path (S10).
            let high = match signal {
                101 => self.spi_idle_high,
                110 => self.spi_cs != Some(pin as u16),
                _ => true, // Appendix B reset D/Q/WP/HOLD_POL, required by SPI.
            };
            return if high { Drive::High } else { Drive::Low };
        }
        if !Self::enabled(pin, self.enable, self.out_sel[pin]) { return Drive::Input(pull); }
        // Active selectors/functions were validated before any state changed.
        // facts-machine.md §6 / TRM pp.477–478,507–508: OUT_INV_SEL[9],
        // PAD_DRIVER[2], and selector 256 with its GPIO enable.
        let high = (self.out[bank] & bit != 0) ^ (self.out_sel[pin] & (1 << 9) != 0);
        if self.pin[pin] & 4 != 0 { Drive::OpenDrain { low: !high, pull } }
        else if high { Drive::High } else { Drive::Low }
    }

    pub fn drive_gen(&self) -> u64 { self.generation }

    pub fn spi_select(&mut self, pad: u16, idle_high: bool) {
        self.spi_cs = Some(pad);
        self.spi_idle_high = idle_high;
        self.generation = self.generation.wrapping_add(1);
    }

    pub fn spi_release(&mut self) {
        self.spi_cs = None;
        self.generation = self.generation.wrapping_add(1);
    }

    /// I20/S16: a peripheral signal must have one enabled matrix output.
    /// I2C adds its open-drain and matching input-route requirements below.
    pub fn output_pad(&self, signal: usize) -> Result<u16, &'static str> {
        self.matrix_output(signal)?.ok_or("peripheral signal has no output-enabled GPIO matrix pad (I20, S16)")
    }

    fn matrix_output(&self, signal: usize) -> Result<Option<u16>, &'static str> {
        let mut selected = self.out_sel.iter().enumerate().filter(|(p, value)| {
            **value & 0x1ff == signal as u32
                && (**value & (1 << 10) == 0 || Self::enabled(*p, self.enable, **value))
        });
        let Some((p, out)) = selected.next() else { return Ok(None); };
        if selected.next().is_some() { return Err("peripheral signal has more than one output-enabled pad (I20, S16)"); }
        if self.mux[p] & (7 << 12) != GPIO_FUNCTION { return Err("peripheral pad is not routed through the GPIO matrix in IO_MUX"); }
        if out & ((1 << 9) | (1 << 11)) != 0 { return Err("peripheral inverted output or output enable is not modelled"); }
        if matches!(signal, 101..=105 | 110) && self.pin[p] & 4 != 0 {
            return Err("SPI open-drain pad drive is outside the modelled subset (S16)");
        }
        Ok(Some(p as u16))
    }

    pub fn i2s_output(&self, signal: usize) -> Result<Option<u16>, &'static str> {
        // trm.txt:15930–15948, 17230–17239. Frame pads describe non-inverted push-pull signals.
        let pad = self.matrix_output(signal)?;
        if pad.is_some_and(|p| self.pin[usize::from(p)] & 4 != 0) { return Err("I2S open-drain output is unsupported"); }
        Ok(pad)
    }

    pub fn i2s_clocks(&self, tx: bool, shared: bool) -> Result<(u16, u16), &'static str> {
        // trm.txt:16185–16187, 16205–16207, 38649–38650. Loopback shares both wires.
        let pair = |tx| -> Result<_, &'static str> {
            match (self.i2s_output(if tx { 22 } else { 26 })?, self.i2s_output(if tx { 24 } else { 27 })?) {
                (Some(bck), Some(ws)) => Ok(Some((bck, ws))),
                (None, None) => Ok(None),
                _ => Err("I2S BCK and WS must be routed as one pair"),
            }
        };
        let own = pair(tx)?;
        if shared && pair(!tx)?.is_some() {
            return Err("I2S loopback requires only the first-started unit's BCK/WS pair on the pads");
        }
        own.ok_or("I2S master BCK/WS pair is not routed")
    }

    pub fn i2s_input(&self) -> Result<u16, &'static str> {
        // trm.txt:16205, 15868–15888, 17188–17207.
        let sel = self.in_sel[25];
        if sel & 0xc0 != 0x80 { return Err("I2S data input needs a non-inverted GPIO matrix route"); }
        let p = pin_index((sel & 63) as usize)?;
        if self.mux[p] & ((7 << 12) | FUN_IE) != GPIO_FUNCTION | FUN_IE {
            return Err("I2S data input needs GPIO function and input enable");
        }
        if !matches!(self.drive(p), Drive::Input(_)) { return Err("I2S data input pad is driven by the chip"); }
        Ok(p as u16)
    }

    pub fn i2s_route_write(&self, addr: u32, value: u32) -> bool {
        // M: a frame holds its routes for a WS period. Reject mid-frame changes to these
        // pads while other GPIOs keep working (trm.txt:15868–15875, 15930–15948).
        let audio = |v: u32| (22..=27).contains(&(v & 511)); // trm.txt:16185–16187, 16205–16207.
        let din = self.in_sel[25] & 255;
        let used = |p: usize| audio(self.out_sel[p]) || din == (0x80 | gpio_number(p) as u32);
        let word_pin = |off: u32| pin_index((off / 4) as usize).ok();
        if let Some(off) = addr.checked_sub(IO_MUX + 4).filter(|&v| v <= 48 * 4) {
            return word_pin(off).is_some_and(|p| used(p) && (value ^ self.mux[p]) & 0xffff != 0);
        }
        let Some(off) = addr.checked_sub(GPIO) else { return false; };
        match off {
            // trm.txt:16764–16775, 17188–17239: DIN selector and per-pad output selectors.
            x if x == 0x154 + 25 * 4 => value & 255 != din,
            0x554..=0x614 => word_pin(off - 0x554).is_some_and(|p| {
                (used(p) || audio(value)) && (value ^ self.out_sel[p]) & OUT_FIELDS != 0
            }),
            0x74..=0x134 => word_pin(off - 0x74).is_some_and(|p| used(p) && (value ^ self.pin[p]) & 4 != 0),
            // trm.txt:15931–15942: software-controlled output enables and their set/clear aliases.
            0x20..=0x34 => {
                let (_, bank, alias) = bank_register(off).expect("GPIO ENABLE word");
                let changed = match alias { 0 => self.enable[bank] ^ value, 1 => !self.enable[bank] & value,
                    _ => self.enable[bank] & value };
                self.out_sel.iter().enumerate().any(|(p, &s)| {
                    let (b, bit) = bank_bit(p);
                    b == bank && changed & bit != 0
                        && ((audio(s) && s & (1 << 10) != 0) || din == (0x80 | gpio_number(p) as u32))
                })
            }
            _ => false,
        }
    }

    pub fn i2c_pads(&self, controller: usize) -> Result<[u16; 2], &'static str> {
        let mut pads = [0; 2];
        for (line, pad) in pads.iter_mut().enumerate() {
            let signal = 89 + 2 * controller + line; // facts-io.md I20.
            let p = self.output_pad(signal)? as usize;
            if self.pin[p] & 4 == 0 { return Err("I2C pad requires GPIO PAD_DRIVER open drain (I15, D-I1)"); }
            if self.mux[p] & FUN_IE == 0 { return Err("I2C pad input is disabled in IO_MUX"); }
            // facts-machine.md §6: IN_SEL names the GPIO number, whereas
            // Board takes the compact Machine::pins() index (GPIO22–25 absent).
            let input = self.in_sel[signal];
            if input & 0x80 == 0 { return Err("I2C input must select the GPIO matrix"); }
            if input & 0x40 != 0 { return Err("I2C matrix input must not be inverted"); }
            if input & 0x3f != gpio_number(p) as u32 { return Err("I2C input and output select different pads"); }
            *pad = p as u16;
        }
        Ok(pads)
    }

    pub fn set_input(&mut self, pin: usize, high: bool, tick: u64) {
        let (bank, bit) = bank_bit(pin);
        let old = self.input[bank] & bit != 0;
        self.input[bank] = fields(self.input[bank], bit, if high { bit } else { 0 });
        // M: profile.input_sync_delay_ticks is zero. Input/status change at
        // this call's tick, with no invented APB pipeline latency.
        if self.mux[pin] & FUN_IE != 0 {
            // facts-machine.md §6, TRM p.507: rising/falling/either types.
            let edge = match self.pin[pin] >> 7 & 7 {
                1 => !old && high, 2 => old && !high, 3 => old != high, _ => false,
            };
            if edge { self.status[bank] |= bit; self.gpio_edge_at[pin] = Some(tick); }
        }
        self.latch_levels();
    }

    fn usb_read(&mut self, off: u32, tick: u64) -> Result<u32, &'static str> {
        // facts-machine.md §7 / TRM §§33.3.3,33.6; usb_serial_jtag_reg.h:14–46.
        match off {
            0 => {
                if self.rx.is_empty() { return Err("USB EP1 read with no received byte is undocumented"); }
                if self.rx.len() == 1 && self.usb_raw & RX_PACKET != 0 {
                    // M: facts-machine.md §7 leaves drain/RECV_PKT ordering
                    // open. Return the old packet's last byte first; queued
                    // host bytes follow on the existing service grid.
                    self.notes.push((tick, "USB EP1 final byte read while a new packet is pending; hardware ordering undocumented; modelled as the new packet following"));
                }
                let byte = self.rx.pop_front().unwrap();
                self.schedule(tick);
                Ok(fields(self.usb_junk[0], 0xff, u32::from(byte)))
            }
            4 => Ok(fields(self.usb_junk[1], 6, (u32::from(!self.flushed) << 1) | (u32::from(!self.rx.is_empty()) << 2))),
            8 => Ok(self.usb_raw),
            0x0c => Ok(fields(self.usb_junk[2], USB_IRQS, self.usb_raw & self.usb_ena)),
            0x10 => Ok(self.usb_ena),
            0x14 => Err("USB INT_CLR write-only readback is undocumented"),
            0x18 => Ok(self.usb_conf0),
            0x2c | 0x3c => Err("USB EP1 pointer and state encodings are undocumented"),
            _ => Err("USB-Serial-JTAG register is not modelled"),
        }
    }

    fn usb_clear(&mut self, value: u32, tick: u64) -> Result<(), &'static str> {
        // facts-machine.md §7: RAW is R/WTC/SS, CLR is WT. M: the clear
        // wins a simultaneous event; a still-empty FIFO reasserts at once.
        // Neither hardware priority nor reassertion timing is documented.
        if self.usb_set_at == Some(tick) && value & self.usb_set_bits != 0 {
            self.notes.push((tick, "USB interrupt cleared in the tick of a new event; hardware priority undocumented; modelled as the event lost"));
        }
        self.usb_raw &= !(value & USB_IRQS);
        if value & TX_EMPTY != 0 && self.tx.is_empty() && !self.flushed {
            self.usb_raw |= TX_EMPTY;
            self.notes.push((tick, "USB SERIAL_IN_EMPTY cleared while its FIFO is empty; hardware timing undocumented; modelled as re-asserted at once"));
        }
        Ok(())
    }

    fn usb_write(&mut self, off: u32, value: u32, tick: u64) -> Result<(), &'static str> {
        match off {
            0 => {
                if self.flushed { return Err("USB EP1 write while SERIAL_IN_EP_DATA_FREE is 0 is undocumented"); }
                self.tx.push(value as u8);
                // facts-machine.md §7 / TRM pp.1248–1249: 64th byte flushes.
                if self.tx.len() == PACKET { self.flushed = true; self.schedule(tick); }
                Ok(())
            }
            4 => {
                if value & 1 != 0 {
                    if self.flushed { return Err("USB WR_DONE while SERIAL_IN_EP_DATA_FREE is 0 is undocumented"); }
                    self.flushed = true; // Includes a documented 0-byte packet.
                    self.schedule(tick);
                }
                Ok(())
            }
            8 | 0x14 => self.usb_clear(value, tick),
            0x0c | 0x2c | 0x3c => Err("write to read-only USB register"),
            0x10 => { self.usb_ena = fields(self.usb_ena, USB_IRQS, value); Ok(()) }
            // Stored only: no enumeration, PHY or JTAG model. §7;
            // usb_serial_jtag_reg.h:353–445.
            0x18 => { self.usb_conf0 = fields(self.usb_conf0, 0x1ffff, value); Ok(()) }
            _ => Err("USB-Serial-JTAG register is not modelled"),
        }
    }

    fn schedule(&mut self, tick: u64) {
        // A listening host polls every frame, including empty or blocked
        // packets. A closed port has no scheduled host service.
        if self.host_listening && self.due.is_none() {
            let delta = if tick < self.frame_phase { self.frame_phase - tick }
                else { self.usb_service_ticks - (tick - self.frame_phase) % self.usb_service_ticks };
            self.due = Some(tick.checked_add(delta).expect("USB frame tick overflow"));
        }
    }

    pub fn next_event(&self) -> Option<u64> { self.due }

    pub fn event(&mut self, tick: u64, out: &mut Vec<LinkOut>) -> Result<(), &'static str> {
        if self.due != Some(tick) { return Ok(()); }
        self.due = None;
        // M: polls occur only while listening; see set_usb_host_listening.
        // An IN token arrives even when there is no flushed packet to read.
        let mut set = IN_TOKEN_REC_IN_EP1;
        // M: the profile's host grid. Hardware FIFO/interrupt transitions:
        // facts-machine.md §7 / TRM pp.1248–1249,1259.
        if self.flushed {
            for byte in self.tx.drain(..) { out.push(LinkOut { link: 0, tick, byte }); }
            self.flushed = false;
            set |= TX_EMPTY;
        }
        if self.rx.is_empty() && !self.host.is_empty() {
            for _ in 0..PACKET {
                let Some(byte) = self.host.pop_front() else { break; };
                self.rx.push_back(byte);
            }
            set |= RX_PACKET;
        }
        self.usb_raw |= set;
        self.usb_set_at = Some(tick);
        self.usb_set_bits = set;
        self.schedule(tick);
        Ok(())
    }

    pub fn set_usb_host_listening(&mut self, listening: bool, tick: u64) {
        if self.host_listening == listening { return; }
        self.host_listening = listening;
        // M: TRM §33.3.3 permits an unread IN packet to stay busy, but does
        // not say whether a host with its port closed still sends IN tokens.
        // Model no polls or OUT delivery while closed; a board check must
        // confirm or refute the IN-token assumption: fc1-seam.md, "Board check"
        // (macOS and Linux, booked through main). Note transitions only,
        // rather than allocating a note on every frame.
        self.notes.push((tick, "USB IN tokens modelled only while the host port is listening; whether a closed port polls EP1 is undocumented; pending fc1-seam.md Board check on macOS and Linux"));
        self.due = None;
        self.schedule(tick);
    }

    pub fn link_in(&mut self, link: u8, byte: u8, tick: u64) {
        assert_eq!(link, 0, "S3 USB-Serial-JTAG only provides link 0");
        // Unbounded host input survives every reset. Only the firmware-facing
        // packet is bounded by TRM §33.3.3's 64-byte limit.
        self.host.push_back(byte);
        self.schedule(tick);
    }

    pub fn reset(&mut self, reset: ResetRequest, profile: &PinsProfile, junk: &mut Junk) {
        assert!(profile.usb_service_ticks != 0, "USB service period must be nonzero");
        assert!(profile.usb_frame_phase_ticks < profile.usb_service_ticks,
            "USB frame phase must be below the service period");
        // TRM pp.526–527 §7.1: these blocks belong to the digital system
        // (Core scope); Cpu reset preserves their registers and packets.
        if reset.scope != ResetScope::Cpu {
            let host = std::mem::take(&mut self.host);
            let host_listening = self.host_listening;
            let input = self.input;
            let generation = self.generation.wrapping_add(1);
            *self = Self::new(profile, junk);
            self.host = host;
            self.host_listening = host_listening;
            self.input = input;
            self.generation = generation;
            self.latch_levels();
        }
        self.latch_straps();
        self.gpio_edge_at = [None; 45];
        self.usb_service_ticks = profile.usb_service_ticks;
        self.frame_phase = profile.usb_frame_phase_ticks;
        self.due = None;
        self.usb_set_at = None;
        self.usb_set_bits = 0;
        self.schedule(0);
    }

    pub fn apb_changed(&mut self, _apb_ticks: u64, _tick: u64) -> Result<(), &'static str> {
        // Immediate GPIO observations are profiled; host frames use the
        // independent tick grid. facts.md §4e requires >=40 MHz APB for PHY
        // compliance but documents no below-limit FIFO behavior. This byte
        // pipe makes no USB PHY compliance claim.
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FRAME: u64 = 480_000;

    fn pins() -> Pins {
        // M: isolated register fixture, with digital GPIO inputs enabled,
        // no pulls/outputs/interrupts, and empty EP1 packets at handoff.
        // Fields/selector: facts-machine.md §6, TRM pp.507–508,514–515.
        // USB RAW/ENA reset choice: §7, TRM pp.1259–1262.
        Pins::new(&PinsProfile {
            power_on_gpio0_high: true, usb_service_ticks: 480_000, usb_frame_phase_ticks: 0,
            input_sync_delay_ticks: 0, handoff_digital_pads_unheld: true,
            handoff_out: [0; 2], handoff_enable: [0; 2], handoff_pin: [0; 45],
            handoff_io_mux: [GPIO_FUNCTION | FUN_IE; 45], handoff_out_sel: [GPIO_SIGNAL; 45],
            handoff_usb_tx: Vec::new(), handoff_usb_tx_flushed: false,
            handoff_usb_rx: Vec::new(), handoff_usb_int_raw: TX_EMPTY, handoff_usb_int_ena: 0,
        }, &mut Junk(1))
    }

    fn assert_notes(pins: &mut Pins, expected: &[(u64, &'static str)]) {
        let mut out = vec![(0, "earlier run event")];
        let expected: Vec<_> = out.iter().chain(expected).copied().collect();
        pins.take_notes(&mut out);
        assert_eq!(out, expected, "assumptions append the access tick and source gap to existing events");
        pins.take_notes(&mut out);
        assert_eq!(out, expected, "taking notes drains them and preserves existing output");
    }

    #[test]
    fn gpio_clear_loses_a_same_tick_edge() {
        // facts-machine.md §6: rising-edge type 1; STATUS and W1TC in both banks.
        for (pin, gpio, status, bit) in [(4, 4, GPIO + 0x44, 16), (44, 48, GPIO + 0x50, 1 << 16)] {
            for alias in [0, 8] {
                let mut p = pins();
                assert_eq!(p.write(GPIO + 0x74 + 4 * gpio, 1 << 7, 0), Some(Ok(())));
                p.set_input(pin, false, 1);
                assert_eq!(p.write(status, 0, 2), Some(Ok(())));
                p.set_input(pin, true, 10);
                assert_eq!(p.read(status, 10).unwrap().unwrap() & bit, bit, "edge latches before the clear");
                assert_eq!(p.write(status + alias, if alias == 0 { 0 } else { bit }, 10), Some(Ok(())));
                assert_eq!(p.read(status, 10).unwrap().unwrap() & bit, 0, "clear after the edge loses its interrupt");
                assert_notes(&mut p, &[(10, "GPIO interrupt cleared in the tick of a new edge; hardware priority undocumented; modelled as the edge lost")]);
                p.set_input(pin, false, 11);
                p.set_input(pin, true, 11);
                assert_eq!(p.write(status + alias, if alias == 0 { 0 } else { bit }, 12), Some(Ok(())));
                assert_eq!(p.read(status, 12).unwrap().unwrap() & bit, 0, "an older edge clears normally");
                assert_notes(&mut p, &[]);
            }
        }
    }

    #[test]
    fn gpio_active_level_reasserts_and_notes_restore_with_the_snapshot() {
        // facts-machine.md §6 / TRM p.507: 4 low level, 5 high level.
        for (kind, high) in [(4, false), (5, true)] {
            for alias in [0, 8] {
                let mut p = pins();
                assert_eq!(p.write(GPIO + 0x84, kind << 7, 0), Some(Ok(())));
                p.set_input(4, high, 1);
                let before = p.clone();
                let clear = if alias == 0 { 0 } else { 16 };
                assert_eq!(p.write(GPIO + 0x44 + alias, clear, 10), Some(Ok(())));
                assert_eq!(p.read(GPIO + 0x44, 10).unwrap().unwrap() & 16, 16, "active level reasserts in the clear tick");
                let pending = p.clone();
                let expected = [(10, "GPIO level interrupt cleared while its level is active; hardware timing undocumented; modelled as re-asserted at once")];
                assert_notes(&mut p, &expected);
                // Machine mark/rewind clones Pins through Periph.
                p.clone_from(&pending);
                assert_notes(&mut p, &expected);
                p.clone_from(&before);
                assert_notes(&mut p, &[]);
                assert_eq!(p.write(GPIO + 0x44 + alias, clear, 10), Some(Ok(())));
                assert_notes(&mut p, &expected);
                p.set_input(4, !high, 11);
                assert_eq!(p.write(GPIO + 0x44 + alias, clear, 12), Some(Ok(())));
                assert_eq!(p.read(GPIO + 0x44, 12).unwrap().unwrap() & 16, 0, "inactive level can be cleared");
                assert_notes(&mut p, &[]);
            }
        }
    }

    #[test]
    fn usb_clear_loses_a_same_tick_event_without_losing_bytes() {
        // facts-machine.md §7: RAW is WTC; CLR uses the same interrupt bits.
        for clear in [USB + 8, USB + 0x14] {
            let mut p = pins();
            p.link_in(0, 7, 0);
            let mut out = Vec::new();
            assert_eq!(p.event(FRAME, &mut out), Ok(()));
            assert_eq!(p.read(USB + 8, FRAME).unwrap().unwrap() & RX_PACKET, RX_PACKET, "host reception sets RAW");
            assert_eq!(p.write(clear, RX_PACKET, FRAME), Some(Ok(())));
            assert_eq!(p.read(USB + 8, FRAME).unwrap().unwrap() & RX_PACKET, 0, "same-tick clear loses the new interrupt");
            assert_eq!(p.read(USB, FRAME).unwrap().unwrap() & 255, 7, "clearing an interrupt does not discard its byte");
            assert_notes(&mut p, &[(FRAME, "USB interrupt cleared in the tick of a new event; hardware priority undocumented; modelled as the event lost")]);
        }
    }

    #[test]
    fn usb_empty_interrupt_reasserts_until_the_fifo_is_nonempty() {
        for clear in [USB + 8, USB + 0x14] {
            let mut p = pins();
            assert_eq!(p.write(USB + 0x10, TX_EMPTY, 0), Some(Ok(())));
            assert_eq!(p.write(clear, TX_EMPTY, 10), Some(Ok(())));
            for status in [USB + 8, USB + 0x0c] {
                assert_eq!(p.read(status, 10).unwrap().unwrap() & TX_EMPTY, TX_EMPTY, "empty source immediately reasserts RAW and enabled ST");
            }
            assert_notes(&mut p, &[(10, "USB SERIAL_IN_EMPTY cleared while its FIFO is empty; hardware timing undocumented; modelled as re-asserted at once")]);
            assert_eq!(p.write(USB, 1, 11), Some(Ok(())));
            assert_eq!(p.write(clear, TX_EMPTY, 12), Some(Ok(())));
            assert_eq!(p.read(USB + 8, 12).unwrap().unwrap() & TX_EMPTY, 0, "nonempty FIFO allows a lasting clear");
            assert_notes(&mut p, &[]);
            // Both assumptions apply when host consumption empties the FIFO
            // in the clear tick: clear the event, then reassert the level.
            assert_eq!(p.write(USB + 4, 1, 13), Some(Ok(())));
            assert_eq!(p.event(FRAME, &mut Vec::new()), Ok(()));
            assert_eq!(p.write(clear, TX_EMPTY, FRAME), Some(Ok(())));
            assert_eq!(p.read(USB + 8, FRAME).unwrap().unwrap() & TX_EMPTY, TX_EMPTY, "empty source reasserts even when its new event is cleared");
            assert_notes(&mut p, &[
                (FRAME, "USB interrupt cleared in the tick of a new event; hardware priority undocumented; modelled as the event lost"),
                (FRAME, "USB SERIAL_IN_EMPTY cleared while its FIFO is empty; hardware timing undocumented; modelled as re-asserted at once"),
            ]);
        }
    }

    #[test]
    fn usb_pending_packet_follows_the_current_packets_final_byte() {
        let mut p = pins();
        let mut out = Vec::new();
        p.link_in(0, 1, 0);
        assert_eq!(p.event(FRAME, &mut out), Ok(()));
        for byte in [2, 3, 4] { p.link_in(0, byte, FRAME + 1); }
        assert_eq!(p.next_event(), Some(2 * FRAME), "unread OUT packet leaves IN polling scheduled");
        assert_eq!(p.read(USB, FRAME + 2).unwrap().unwrap() & 255, 1, "pending receive interrupt does not stop the old packet's final read");
        assert_notes(&mut p, &[(FRAME + 2, "USB EP1 final byte read while a new packet is pending; hardware ordering undocumented; modelled as the new packet following")]);
        assert_eq!(p.next_event(), Some(2 * FRAME), "final read keeps delivery on the existing host grid");
        assert_eq!(p.event(2 * FRAME, &mut out), Ok(()));
        assert_eq!(p.write(USB + 0x14, RX_PACKET, 2 * FRAME + 1), Some(Ok(())));
        for byte in [2, 3, 4] {
            assert_eq!(p.read(USB, 2 * FRAME + 2).unwrap().unwrap() & 255, byte, "new packet bytes follow in arrival order");
        }
        assert_eq!(p.read(USB + 4, 2 * FRAME + 2).unwrap().unwrap() & 4, 0, "all queued bytes were consumed exactly once");
        assert_notes(&mut p, &[]);
    }
}
