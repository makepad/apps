//! SYSCON Wi-Fi clock and reset registers. The modem is unmodelled, so these
//! words are storage. No modelled block depends on these clocks or resets.

use super::{Access, ResetScope};

// ESP-IDF v6.1.1 ESP32-S3 reg_base.h:40 and syscon_reg.h:145–155.
const WIFI_CLK_EN: u32 = 0x6002_6014;
const WIFI_RST_EN: u32 = 0x6002_6018;

#[derive(Clone, PartialEq, Eq)]
pub(super) struct Syscon {
    wifi_clk_en: u32,
    wifi_rst_en: u32,
}

impl Syscon {
    pub fn new() -> Self {
        Self { wifi_clk_en: 0xfffc_e030, wifi_rst_en: 0 }
    }

    pub fn reset(&mut self, scope: ResetScope) {
        // INFERRED retention by domain, as for the interrupt matrix. CPU-only
        // retains the digital registers. Core and System restore their defaults.
        if scope != ResetScope::Cpu { *self = Self::new(); }
    }

    pub fn read(&self, addr: u32) -> Access<u32> {
        Some(Ok(match addr {
            WIFI_CLK_EN => self.wifi_clk_en,
            WIFI_RST_EN => self.wifi_rst_en,
            _ => return None,
        }))
    }

    pub fn write(&mut self, addr: u32, value: u32) -> Access<()> {
        match addr {
            WIFI_CLK_EN => self.wifi_clk_en = value,
            WIFI_RST_EN => self.wifi_rst_en = value,
            _ => return None,
        }
        Some(Ok(()))
    }
}
