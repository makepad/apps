//! Read-only factory MAC and core-regulator calibration (PVT) shadow registers. ESP32-S3 TRM v1.8
//! §5.3.3 p.422 specifies automatic loading at boot; Registers 5.18/5.19 p.436 give fields.
//! ESP-IDF v6.1.1 (Espressif Systems, Apache-2.0 OR MIT)
//! reg_base.h:13 and efuse_reg.h:674–720 (MAC), :775–1020 (BLK_VERSION_MINOR, K_RTC_LDO, K_DIG_LDO,
//! V_RTC_DBIAS20, V_DIG_DBIAS20, DIG_DBIAS_HVT, BLK_VERSION_MAJOR). No programming or command engine.

use super::Access;
use crate::Junk;

const BASE: u32 = 0x6000_7000;
/// The PVT words' offsets (RD_MAC_SPI_SYS_3, _4, _5, RD_SYS_PART1_DATA4) and the bits of each that hold
/// the PVT fields (efuse_reg.h:775–1020); a profile supplies those bits, the rest stay seeded junk.
const PVT_WORDS: [u32; 4] = [0x50, 0x54, 0x58, 0x6c];
const PVT_MASKS: [u32; 4] = [0x0700_0000, 0xffff_e000, 0x0000_ffff, 0x0000_0003];

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct Efuse {
    mac: [u32; 2],
    pvt: [u32; 4],
    noted: bool,
    notes: Vec<(u64, &'static str)>,
}

impl Efuse {
    pub fn new(mac: [u8; 6], pvt: [u32; 4], junk: &mut Junk) -> Self {
        // MAC_0 holds the low 32 bits, MAC_1[15:0] the high 16 bits.
        // The profile supplies bytes in network/display order.
        // M: the other half of MAC_SPI_SYS_1 describes SPI pad fuses, not
        // MAC. They are outside this model and deliberately remain seeded
        // junk, so a consumer must mask/extract the documented MAC field.
        let mac = [u32::from_be_bytes(mac[2..].try_into().unwrap()),
            (junk.u32() & 0xffff_0000) | u32::from(u16::from_be_bytes([mac[0], mac[1]]))];
        let pvt = std::array::from_fn(|k| (pvt[k] & PVT_MASKS[k]) | (junk.u32() & !PVT_MASKS[k]));
        Self { mac, pvt, noted: false, notes: Vec::new() }
    }

    pub fn read(&mut self, addr: u32, tick: u64) -> Access<u32> {
        if !(BASE..BASE + 0x1000).contains(&addr) { return None; }
        let value = match addr - BASE {
            0x44 => self.mac[0],
            0x48 => self.mac[1],
            offset => match PVT_WORDS.iter().position(|&o| o == offset) {
                Some(k) => self.pvt[k],
                None => return Some(Err("eFuse register outside the factory MAC and the PVT words is not modelled")),
            },
        };
        if !self.noted {
            self.notes.push((tick, "eFuse fields beside the factory MAC and the PVT fields are outside this model; modelled as seeded junk and must be masked"));
            self.noted = true;
        }
        Some(Ok(value))
    }

    pub fn write(&self, addr: u32) -> Access<()> {
        (BASE..BASE + 0x1000).contains(&addr)
            .then_some(Err("eFuse writes are not modelled; factory MAC registers are read-only"))
    }

    pub fn take_notes(&mut self, out: &mut Vec<(u64, &'static str)>) { out.append(&mut self.notes); }
}
