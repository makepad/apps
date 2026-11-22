use fab_elec_mcu_rt as rt;

/// The voicebox board's panel wiring and geometry. Pins: facts-parts R01–R05;
/// column offset: app.md's working 368x448 area starts at controller column 16.
/// The visible rows start at zero (lead choice: app.md gives no row offset).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Panel {
    pub width: u32,
    pub height: u32,
    pub column_offset: u32,
    pub row_offset: u32,
    pub cs: u32,
    pub sck: u32,
    pub mosi: u32,
    pub expander: u8,
    pub reset_bit: u8,
    pub brightness: u8,
    /// Board checks only: init reports its stages and the expander's registers as probes
    /// (runtime::PANEL_PROBES). Circuit wiring leaves it off.
    pub report: bool,
}

pub const VOICEBOX_PANEL: Panel = Panel {
    width: 368, height: 448, column_offset: 16, row_offset: 0,
    cs: 12, sck: 11, mosi: 4,
    expander: 0x20, reset_bit: 0,
    brightness: 0xff, // Lead choice: maximum brightness (P12).
    report: false,
};

impl Panel {
    pub fn profile(&self) -> rt::Profile {
        rt::Profile { display: Some(rt::DisplayProfile { width: self.width, height: self.height }),
            ..crate::chip::profile() }
    }
}

// S03: at most 2^18 data bits per SPI user transfer. Lead choice: 4092 is
// the largest word multiple below G04's 4095-byte descriptor limit.
pub(crate) const BUFFER_BYTES: u32 = 32768;
pub(crate) const DESCRIPTOR_BYTES: u32 = 4092;
pub(crate) const DESCRIPTORS: u32 = BUFFER_BYTES.div_ceil(DESCRIPTOR_BYTES);

/// Static scratch belongs to the non-reentrant display ports and boot code.
#[derive(Clone, Debug)]
pub struct PanelSram {
    pub available: u32,
    pub scratch: u32,
    pub buffer: u32,
    pub descriptors: u32,
}

pub(crate) const SCRATCH_BYTES: u32 = 64;
