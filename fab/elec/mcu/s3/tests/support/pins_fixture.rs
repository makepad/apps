//! Pin/link handoff profile and small Xtensa firmware helpers.

use fab_elec_mcu_emit32::xtensa::{ccz, enc};
use fab_elec_mcu_emu::{Machine, PcSet, Ran, Until};
use fab_elec_mcu_s3::{PinsProfile, S3Machine};

use super::Program;

// facts.md §4a, TRM pp.408,501–511,514; facts-machine.md §§6–7.
pub const GPIO_BASE: u32 = 0x6000_4000;
pub const MUX_BASE: u32 = 0x6000_9000;
pub const USB_BASE: u32 = 0x6003_8000;
pub const GPIO_OUT: u32 = GPIO_BASE + 4;
pub const GPIO_ENABLE: u32 = GPIO_BASE + 0x20;
pub const GPIO_STATUS: u32 = GPIO_BASE + 0x44;
pub const GPIO_IN: u32 = GPIO_BASE + 0x3c;
pub const USB_EP1_CONF: u32 = USB_BASE + 4;
pub const USB_RAW: u32 = USB_BASE + 8;
pub const USB_ST: u32 = USB_BASE + 0x0c;
pub const USB_ENA: u32 = USB_BASE + 0x10;
pub const USB_CLR: u32 = USB_BASE + 0x14;
// facts-machine.md §6, io_mux_reg.h:41–65,140.
pub const MUX_GPIO: u32 = 1 << 12;
pub const MUX_IE: u32 = 1 << 9;
pub const MUX_UP: u32 = 1 << 8;
pub const MUX_DOWN: u32 = 1 << 7;
// M: Fable Q6 USB row, fable-answers.md:1635; machine's 480 MHz grid.
pub const USB_FRAME: u64 = 480_000;

pub fn mux_reg(gpio: u32) -> u32 { MUX_BASE + 4 + 4 * gpio }
pub fn pin_reg(gpio: u32) -> u32 { GPIO_BASE + 0x74 + 4 * gpio }
pub fn out_sel_reg(gpio: u32) -> u32 { GPIO_BASE + 0x554 + 4 * gpio }

/// Every M field is set here; none is inherited from an implicit default.
pub fn pins_profile() -> PinsProfile {
    let handoff_io_mux = std::array::from_fn(|p| {
        let gpio = if p < 22 { p } else { p + 4 };
        // facts-machine.md §6 table / TRM pp.493–494 Table 6.12-1:
        // RST 0 disables IE; 1 enables IE; 2 adds WPD; 3 adds WPU;
        // 4 enables output plus WPU (GPIO43). These are pin states, not
        // complete register words, and not MCU_SEL function numbers.
        let rst = match gpio {
            0 | 26..=32 | 44 => 3,
            4..=8 | 15 | 16 | 19..=21 => 0,
            43 => 4,
            45 | 46 => 2,
            _ => 1,
        };
        let drv = if gpio == 19 || gpio == 20 { 3 } else { 2 };
        // M: the fixture's handoff_io_mux chooses GPIO function on all pads,
        // with omitted sleep/filter flags zero, and the table's IE/pulls/DRV.
        // MTCK uses the table's EFUSE_DIS_PAD_JTAG=0 pull-up case. GPIO43's
        // UART reset OE is deliberately replaced by the handoff_enable field.
        MUX_GPIO | (drv << 10)
            | if matches!(rst, 1..=3) { MUX_IE } else { 0 }
            | if rst == 2 { MUX_DOWN } else { 0 }
            | if matches!(rst, 3 | 4) || gpio == 39 { MUX_UP } else { 0 }
    });
    PinsProfile {
        power_on_gpio0_high: true,
        usb_service_ticks: 480_000,
        usb_frame_phase_ticks: 0,
        input_sync_delay_ticks: 0,
        handoff_digital_pads_unheld: true,
        handoff_out: [0, 0],
        handoff_enable: [0, 0],
        handoff_pin: [0; 45],
        handoff_io_mux,
        handoff_out_sel: [256; 45],
        handoff_usb_tx: Vec::new(),
        handoff_usb_tx_flushed: false,
        handoff_usb_rx: Vec::new(),
        // facts-machine.md §7 / TRM pp.1259–1262: RAW reset is 0x8,
        // ENA reset zero. M: fixture chooses these at the ROM handoff too.
        handoff_usb_int_raw: 8,
        handoff_usb_int_ena: 0,
    }
}

impl Program {
    pub fn pin_read(&mut self, reg: u8, addr: u32) -> u32 {
        self.load(1, addr);
        self.emit(enc::l32i(reg, 1, 0))
    }

    pub fn gpio_setup(&mut self, gpio: u32, mux_flags: u32) {
        // facts.md §4a output recipe / TRM pp.477–478,507–508,514–515.
        self.store(pin_reg(gpio), 0);
        self.store(out_sel_reg(gpio), 256);
        self.store(mux_reg(gpio), MUX_GPIO | mux_flags);
    }

    /// A real firmware loop writes count consecutive bytes, not host injection.
    pub fn usb_tx_sequence(&mut self, first: u8, count: u8) {
        assert!((1..=64).contains(&count), "fixture writes one nonempty EP1 packet");
        self.load(1, USB_BASE);
        self.emit(enc::movi(2, i32::from(first)));
        self.emit(enc::movi(4, i32::from(count)));
        let start = self.emit(enc::s32i(2, 1, 0));
        self.emit(enc::addi(2, 2, 1));
        self.emit(enc::addi(4, 4, -1));
        self.emit(enc::bccz(ccz::NE, 4, (i64::from(start) - i64::from(self.pc()) - 4) as i32));
    }

    /// Clear RECV_PKT before draining to avoid the modelled pending-packet
    /// ordering assumption (facts-machine.md §7 leaves it open). Save words in SRAM;
    /// only each word's low byte is RDWR_BYTE, usb_serial_jtag_reg.h:14–24.
    pub fn usb_rx_words(&mut self, ram: u32, count: u8) {
        assert!((1..=64).contains(&count), "fixture drains one nonempty EP1 packet");
        self.store(USB_CLR, 4);
        self.load(1, USB_BASE);
        self.load(3, ram);
        self.emit(enc::movi(4, i32::from(count)));
        let start = self.emit(enc::l32i(5, 1, 0));
        self.emit(enc::s32i(5, 3, 0));
        self.emit(enc::addi(3, 3, 4));
        self.emit(enc::addi(4, 4, -1));
        self.emit(enc::bccz(ccz::NE, 4, (i64::from(start) - i64::from(self.pc()) - 4) as i32));
    }

    /// Poll FREE/AVAIL, then branch until the requested bit is set.
    pub fn usb_wait_bit(&mut self, bit: u8) {
        self.load(1, USB_EP1_CONF);
        self.emit(enc::movi(2, 1 << bit));
        let start = self.emit(enc::l32i(5, 1, 0));
        self.emit(enc::and(5, 5, 2));
        self.emit(enc::bccz(ccz::EQ, 5, (i64::from(start) - i64::from(self.pc()) - 4) as i32));
    }

    pub fn pins_spin(&mut self) -> u32 {
        // emit32's checked J displacement is relative to PC+4.
        self.emit(enc::j(-4))
    }
}

pub fn pins_at_pc(machine: &mut impl Machine, pc: u32) {
    let mut marks = PcSet::new(pc, 1);
    marks.insert(pc);
    let after = machine.now();
    assert_eq!(machine.run(&Until {
        tick: after + 10 * USB_FRAME, marks: &marks, stops: &[pc],
        stops_after: after, on_drive: false,
    }), Ran::Pc, "pin/link firmware reaches its requested instruction boundary");
}

pub fn pins_drive(machine: &mut S3Machine) -> Ran {
    machine.run(&Until {
        tick: machine.now() + 100_000, marks: &PcSet::default(), stops: &[],
        stops_after: 0, on_drive: true,
    })
}

pub fn received_words(machine: &impl Machine, ram: u32, count: usize) -> Vec<u8> {
    let mut bytes = vec![0; count * 4];
    assert!(machine.read_data(ram, &mut bytes), "received packet words are inspectable SRAM");
    bytes.chunks_exact(4).map(|word| word[0]).collect()
}
