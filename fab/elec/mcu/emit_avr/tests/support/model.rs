use fab_elec_mcu_rt::{serial, watch, sched::{Fault, Hal}, Profile};

pub const SETTING: serial::SerialSetting = serial::SerialSetting { link: 0, asked: 19200, ubrr: 51, u2x: false };
pub const PROFILE: Profile = fab_elec_mcu_emit_avr::chip::profile(16_000_000);
pub const ECHO_SOURCE: &str = "let out = output(D13)\nlet motor = pwm(D9, hz(490))\nlet link = serial(19200, fn() {\n    let value = byte(link)\n    write(out, if value != 0 { high } else { low })\n    duty(motor, value)\n})\n";
pub const RX_BYTES: [u8; 6] = [0, 1, 255, 128, 7, 0];

pub struct Model {
    pub outputs: Vec<(u64, watch::Event)>,
    pub faults: Vec<Fault>,
    pub events: Vec<watch::Event>,
    pub setting: Option<serial::SerialSetting>,
    pub input_high: Option<bool>,
    pub analog: Vec<(u32, u16)>,
    pub eeprom: [u8; 1024],
}

impl Default for Model {
    fn default() -> Self {
        Self { outputs: Vec::new(), faults: Vec::new(), events: Vec::new(), setting: None,
            input_high: None, analog: Vec::new(), eeprom: [0xff; 1024] }
    }
}

impl Hal for Model {
    fn eeprom_read(&mut self, t: u64, addr: u16, site: u32) -> u8 {
        let value = self.eeprom[addr as usize];
        let event = watch::Event::EepromRead { addr, value, site: site as u16 };
        self.outputs.push((t, event)); self.events.push(event);
        value
    }
    fn eeprom_write(&mut self, t: u64, addr: u16, value: u8, site: u32) {
        self.eeprom[addr as usize] = value;
        let event = watch::Event::EepromWrite { addr, value, site: site as u16 };
        self.outputs.push((t, event)); self.events.push(event);
    }
    fn shift(&mut self, t: u64, data: u32, clock: u32, value: u32, bits: u8, site: u32) {
        let event = watch::Event::Shift { data: data as u8, clock: clock as u8, value, bits, site: site as u16 };
        self.outputs.push((t, event)); self.events.push(event);
    }
    fn configure(&mut self, _: u64, _: u32, _: fab_elec_mcu_rt::sched::Role) {}
    fn serial(&mut self, _: u64, _: u32, setting: serial::SerialSetting) { assert_eq!(setting, self.setting.unwrap_or(SETTING)); }
    fn write(&mut self, t: u64, pin: u32, high: bool, site: u32) {
        let event = watch::Event::Write { pin: pin as u8, level: high, site: site as u16 };
        self.outputs.push((t, event)); self.events.push(event);
    }
    fn duty(&mut self, t: u64, pin: u32, level: u8, site: u32) {
        let event = watch::Event::Duty { pin: pin as u8, level, site: site as u16 };
        self.outputs.push((t, event)); self.events.push(event);
    }
    fn read(&mut self, _: u64, _: u32, _: u32) -> bool { self.input_high.expect("this fixture never reads a pin") }
    fn adc(&mut self, _: u64, pin: u32, _: u32) -> u16 {
        self.analog.iter().find(|&&(p, _)| p == pin).expect("fixture supplies this analog pin").1
    }
    fn eth_send(&mut self, _: u64, _: &[u8], _: u32) -> bool { false }
    fn eth_mac(&mut self, _: u64, _: u32) -> [u8; 6] { [2, 0, 0, 0, 0, 1] }
    fn clock(&mut self, t: u64, _: u32) -> u64 { t }
    fn fault(&mut self, _: u64, fault: Fault) { self.faults.push(fault);
        let (code, site, arg) = match fault {
            Fault::LoopCap { site } => (watch::FAULT_LOOP_CAP, site, 0),
            Fault::Draw { site, refusal, .. } => (watch::FAULT_DRAW, site, refusal as i32),
            Fault::Range { site, n } => (watch::FAULT_RANGE, site, n),
            Fault::ShiftBits { site, bits } => (watch::FAULT_SHIFT_BITS, site, bits),
            Fault::EepromAddr { site, addr } => (watch::FAULT_EEPROM_ADDR, site, addr),
            Fault::EthLength { site, n } => (watch::FAULT_ETH_LENGTH, site, n),
            Fault::EthRange { site, off, .. } => (watch::FAULT_ETH_RANGE, site, off),
            Fault::Pin { pin, site, .. } => (watch::FAULT_PIN, site, pin),
            Fault::Timer { timer, site, .. } => (watch::FAULT_TIMER, site, timer),
            Fault::Serial { link, site, .. } => (watch::FAULT_SERIAL, site, link),
            Fault::Overrun { timer, missed } => (watch::FAULT_OVERRUN, timer, missed as i32),
            Fault::SerialLost { link, count, at_least } =>
                (if at_least { watch::FAULT_SERIAL_LOST_AT_LEAST } else { watch::FAULT_SERIAL_LOST }, link, count as i32),
            Fault::Kernel { .. } | Fault::KernelReport { .. } => panic!("an AVR app module has no kernel ports"),
            Fault::NetHandle { .. } | Fault::NetRange { .. } => panic!("the 328P refuses net declarations"),
            Fault::AudioUnavailable { .. } => panic!("the 328P refuses audio declarations"),
        };
        self.events.push(watch::Event::Fault { code, site: site as u16, arg }); }
}
