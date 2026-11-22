//! Masked kernel exports, called with the handler's CALL0 ABI. The radio thunk
//! owns the RSIL-15 bracket; neither an export nor a port changes PS.
//!
//! Port bounds come from the final relaxed bytes. Count the whole routine
//! once, subtract every recorded loop span, and add one CALLX0. All branch
//! arms remain in that fixed count, so it may exceed any single path. A
//! routine that emitted a jump to kernel_fault also charges that stub's
//! measured span once, regardless of how many fault branches it contains.
//! Loops sum by default; explicit alternative groups cost their longest member.
//! A missed alternative mark only loosens the bound and never makes it unsound.
//! Copy/fill multiply by max (copy's two directions are alternatives).
//! now64 adds one VALID poll; each further poll costs
//! its recorded span, currently three instructions, exposed as per_unit.
pub mod gates;
mod ports;
pub(crate) use ports::{scalar_access, ScalarAccess};

use fab_elec_mcu_rt::{kernel::RegionKind, module::Module, watch};
use crate::{emit::{self, Domain}, EntryReport, Options, Refusal, SourceMap};

pub const OBJECTS: &str = concat!(include_str!("objects.splash"), "\n", include_str!("adapter.splash"));

/// Owner, depth, recursive flag and waiter head, all initially zero.
pub const MUTEX_BYTES: u32 = 16;

// TLSF control layout, shared with objects.splash through kernel_consts.
const TIMER_NODE_BYTES: u32 = 36; // T_KEY through T_TAG in objects.splash.
/// The TLSF bitmaps follow the last header word, H_TIMER_UNSET at offset 64.
pub const HEAP_HEADER_BYTES: u32 = 68;
pub const HEAP_MIN_BLOCK: u32 = 32;
const HEAP_SECOND_LEVELS: u32 = 8;
// TLSF adds a 16-byte header and aligns the whole block to eight bytes.
fn block_bytes(payload: u32) -> u32 { ((payload + 16 + 7) & !7).max(HEAP_MIN_BLOCK) }
fn timer_nodes_max(bytes: u32) -> u32 { bytes / block_bytes(TIMER_NODE_BYTES) }
/// Margin in bytes for nodes and slot chunks. Fragmentation can still prevent allocation.
pub fn timer_floor_bytes(reserve: u32) -> u64 {
    u64::from(reserve) * u64::from(block_bytes(TIMER_NODE_BYTES))
        + u64::from(reserve.div_ceil(64)) * u64::from(block_bytes(64 * 4))
}
fn timer_dir_words(bytes: u32) -> u32 { timer_nodes_max(bytes).div_ceil(64).max(1) }
fn heap_first_levels(bytes: u32) -> u32 { bytes.max(1).ilog2().saturating_sub(4).max(1) }
fn heap_tables_bytes(bytes: u32) -> u32 {
    (HEAP_HEADER_BYTES + heap_first_levels(bytes) * (4 + HEAP_SECOND_LEVELS * 4) + 7) & !7
}
pub fn heap_control_bytes(bytes: u32) -> u32 {
    (heap_tables_bytes(bytes) + 4 * timer_dir_words(bytes) + 7) & !7
}

/// Stated assumption: IDF default CONFIG_FREERTOS_HZ=100. Verify in R9.
pub const BLOB_TICK_US: u32 = 10_000;

/// Island ISR level (emit_s3/src/radio/sched.rs handler contract), shared
/// by every pack critical section. Entry at a higher level stays there.
pub const CRIT_MASK: u32 = 3;

/// SYSTIMER ticks per microsecond, derived from runtime's 100 ms sleep cap.
pub const TICKS_PER_US: u32 = crate::runtime::SLEEP_CAP_TICKS / 100_000;

/// ESP-IDF v6.1.1 components/soc/esp32s3/register/soc/reg_base.h:40 and
/// components/soc/esp32s3/register/soc/syscon_reg.h:145: SYSCON base + 0x14.
pub const SYSCON_WIFI_CLK_EN: u32 = 0x6002_6014;
/// ESP-IDF v6.1.1 components/soc/esp32s3/register/soc/syscon_reg.h:180.
pub const CLK_COMMON: u32 = 0x78078f;
/// ESP-IDF v6.1.1 components/soc/esp32s3/register/soc/syscon_reg.h:185.
/// Unverified module-bit meaning: together with CLK_RNG this is
/// PHY_INIT_MODEM_CLOCK_REQUIRED_BITS (components/esp_phy/esp32s3/include/
/// phy_init_deps.h:9) minus CLK_COMMON. phy_module_enable's body is not pinned;
/// a board register read must confirm it.
pub const CLK_BT_BASEBAND: u32 = 1 << 11;
/// ESP-IDF v6.1.1 components/soc/esp32s3/register/soc/syscon_reg.h:194.
/// Module-bit meaning is unverified as above. Never cleared: the RNG gate
/// needs it (components/esp_phy/src/phy_init.c:421).
pub const CLK_RNG: u32 = 1 << 15;
/// ESP-IDF v6.1.1 components/esp_phy/include/esp_phy_init.h:38.
pub const PHY_MODEM_WIFI: u32 = 1;
/// ESP-IDF v6.1.1 components/esp_phy/include/esp_phy_init.h:60.
pub const PHY_RF_CAL_FULL: u32 = 2;
/// ESP-IDF v6.1.1 components/esp_phy/include/esp_private/phy.h:17.
pub const ESP_CAL_DATA_CHECK_FAIL: u32 = 1;
/// ESP-IDF v6.1.1 components/esp_phy/Kconfig:181-183 defaults to 1000 ms;
/// components/esp_phy/src/phy_common.c:64 converts it to microseconds.
pub const PHY_TRACK_PLL_PERIOD_US: u32 = 1_000_000;

// Island glue/kernel protocol, not IDF numeric codes. The named states split
// ESP-IDF v6.1.1 components/esp_phy/src/phy_init.c:282-425 around libphy calls.
/// No mutex ownership; glue returns without any further calls. A caller
/// bypassing phy_lock, still waiting for it, or calling after phy_leave can
/// reach this state. The fixed PHY mutex never needs allocation or retry.
pub const PHY_SKIP: u32 = 0;
/// A modem is already enabled (ESP-IDF v6.1.1 components/esp_phy/src/phy_init.c:285).
pub const PHY_ON: u32 = 1;
/// First calibration (ESP-IDF v6.1.1 components/esp_phy/src/phy_init.c:295-300).
pub const PHY_FIRST: u32 = 2;
/// Calibrated wake without a saved register block
/// (ESP-IDF v6.1.1 components/esp_phy/src/phy_init.c:336-340).
pub const PHY_WAKE: u32 = 3;
/// Calibrated wake with a saved register block
/// (ESP-IDF v6.1.1 components/esp_phy/src/phy_init.c:262-266,339-340).
pub const PHY_WAKE_RESTORE: u32 = 4;
/// Last modem stops (ESP-IDF v6.1.1 components/esp_phy/src/phy_init.c:381-422).
pub const PHY_LAST: u32 = 5;
/// Removing Wi-Fi does not stop the last modem
/// (ESP-IDF v6.1.1 components/esp_phy/src/phy_init.c:381-383).
pub const PHY_NOT_LAST: u32 = 6;
/// Glue operation selecting esp_phy_enable
/// (ESP-IDF v6.1.1 components/esp_phy/src/phy_init.c:282).
pub const PHY_OP_ENABLE: u32 = 1;
/// Glue operation selecting esp_phy_disable
/// (ESP-IDF v6.1.1 components/esp_phy/src/phy_init.c:375).
pub const PHY_OP_DISABLE: u32 = 2;

/// IDF v6.1.1 components/soc/esp32s3/register/soc/{reg_base,syscon_reg}.h:
/// DR_REG_SYSCON_BASE + 0x18, SYSCON_WIFI_RST_EN_REG.
pub const SYSCON_WIFI_RST_EN: u32 = 0x6002_6018;
/// Runtime start only. ESP-IDF v6.1.1 ESP32-S3 reg_base.h:14 and rtc_cntl_reg.h:1774.
pub const RTC_CNTL_DIG_PWC: u32 = 0x6000_8090;
/// Runtime start only. ESP-IDF v6.1.1 ESP32-S3 reg_base.h:14 and rtc_cntl_reg.h:1872.
pub const RTC_CNTL_DIG_ISO: u32 = 0x6000_8094;
/// Only runtime start writes this power-down override. ESP-IDF v6.1.1 ESP32-S3 rtc_cntl_reg.h:1829–1834.
pub const WIFI_FORCE_PD: u32 = 1 << 17;
/// Only runtime start writes this isolation override. ESP-IDF v6.1.1 ESP32-S3 rtc_cntl_reg.h:1891–1896.
pub const WIFI_FORCE_ISO: u32 = 1 << 28;
/// Only runtime start pulses this modem reset field. ESP-IDF v6.1.1 ESP32-S3 syscon_reg.h:205–226.
pub const MODEM_RESET_FIELD: u32 = 0x0000_2a1f;
/// IDF v6.1.1 components/soc/esp32s3/register/soc/syscon_reg.h: SYSTEM_WIFIMAC_RST.
pub const WIFIMAC_RST: u32 = 1 << 2;
/// IDF v6.1.1 components/soc/esp32s3/register/soc/{reg_base,efuse_reg}.h:
/// DR_REG_EFUSE_BASE + 0x44, EFUSE_RD_MAC_SPI_SYS_0_REG (low 32 MAC bits).
pub const EFUSE_MAC_0: u32 = 0x6000_7044;
/// IDF v6.1.1 components/soc/esp32s3/register/soc/{reg_base,efuse_reg}.h:
/// DR_REG_EFUSE_BASE + 0x48, EFUSE_RD_MAC_SPI_SYS_1_REG (low 16 bits are MAC high).
pub const EFUSE_MAC_1: u32 = 0x6000_7048;
/// IDF v6.1.1 components/soc/esp32s3/register/soc/{reg_base,rtc_cntl_reg}.h:
/// DR_REG_RTCCNTL_BASE + 0x54, RTC_CNTL_STORE1_REG.
pub const RTC_STORE1: u32 = 0x6000_8054;
/// ESP32-S3 TRM v1.8 §25.5: RNG_DATA_REG, read only.
pub const RNG_DATA: u32 = 0x6003_507C;
/// ESP32-S3 TRM v1.8 §25.3: 500 kHz read rate with the slower entropy source.
pub const RNG_SPACING_US: u32 = 2;
/// A poll is roughly 150 Xtensa instructions (SYSTIMER snapshot, checked
/// loads, XOR and branch). At the S3's maximum 240 MHz (ESP32-S3 TRM v1.8
/// §7.2.4.1), 2 us is 480 cycles: about four polls at an ideal one cycle per
/// instruction; MMIO makes them slower. This is an estimate, not a measurement.
/// 64 leaves margin and bounds a stalled clock; reaching it still returns a
/// word (ESP32-S3 TRM v1.8 §25.3 sets the spacing).
pub const RNG_POLL_CAP: u32 = 64;

/// IDF v6.1.1 components/soc/esp32s3/register/soc/reg_base.h:61 and
/// components/soc/esp32s3/register/soc/interrupt_core0_reg.h:15-33:
/// CPU 0 map words, source s at base + 4*s.
pub const INTR_MATRIX: u32 = 0x600C_2000;
/// IDF v6.1.1 components/soc/esp32s3/include/soc/interrupts.h:18-21:
/// WIFI_MAC, WIFI_MAC_NMI, WIFI_PWR and WIFI_BB are sources 0..=3.
pub const WIFI_SOURCES: u32 = 4;
/// IDF v6.1.1 components/xtensa/esp32s3/include/xtensa/config/core-isa.h:383-452:
/// level 1 and EXTERN_LEVEL/EXTERN_EDGE: 0..=5, 8..=10, 12, 13, 17, 18.
pub const LEVEL1_EXTERNAL_LINES: u32 = 0x0006_373f;
/// emit_s3/src/runtime.rs:159 reserves the dispatcher and yield lines.
pub const ROUTABLE_LINES: u32 = LEVEL1_EXTERNAL_LINES & !crate::RADIO_KERNEL_LINES.protected;

/// Device addresses, never host pointers. Initialize the heap to zero once
/// before the first call; serialize exports under the masked thunk contract.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KernelLayout {
    pub current: u32,
    pub tasks: u32,
    pub task_count: u32,
    pub max_tasks: u32,
    pub heap_base: u32,
    pub heap_size: u32,
    pub timer_reserve: u32,
    /// Bits 31:30 shared by windowed task entries, task_exit and the ISR shim.
    pub code_region: u32,
    /// R3 slots {mask, entry, arg}: emit_s3/src/radio/sched.rs:44-45.
    pub isrs: u32,
    /// Live count word: emit_s3/src/radio/sched.rs:42-43.
    pub isr_count: u32,
    /// Slot allocation; production uses radio::PACK_ISR_LINES.
    pub isr_capacity: u32,
    /// R4c-3 allocates 1904 calibration bytes here (ESP-IDF v6.1.1
    /// components/esp_phy/include/esp_phy_init.h:47-51).
    pub phy_cal_data: u32,
    /// R4c-3 allocates 84 backup bytes here (ESP-IDF v6.1.1
    /// components/soc/esp32s3/include/soc/soc_caps.h:445).
    pub phy_dig_regs: u32,
    /// Fixed MUTEX_BYTES slot, zeroed before the first PHY call.
    pub phy_mutex: u32,
}

/// Numeric bindings for the kernel-only prelude. Refuse unrepresentable
/// layout values rather than wrapping a device address into a negative i32.
pub fn kernel_consts(l: &KernelLayout) -> Vec<(&'static str, i32)> {
    use super::{sched::*, task::*};
    const _: () = assert!(TICKS_PER_US > 1 && TICKS_PER_US.is_power_of_two(),
        "kernel timeout rate must be a power of two greater than one");
    const TICKS_HIGH_SHIFT: u32 = 32 - TICKS_PER_US.trailing_zeros();
    assert!(l.code_region <= 3, "windowed code region must fit two bits");
    assert_eq!(l.heap_base & 7, 0, "kernel heap base requires eight-byte alignment");
    assert_eq!(l.heap_size & 7, 0, "kernel heap size requires eight-byte alignment");
    assert!(l.heap_size >= heap_control_bytes(l.heap_size), "kernel heap must hold its control words");
    assert!(u64::from(l.heap_base) + u64::from(l.heap_size) <= i32::MAX as u64,
        "kernel heap end does not fit i32");
    let checked_values = [
        ("TCB_STATE", TCB_STATE), ("TCB_PRIORITY", TCB_PRIORITY),
        ("TCB_DEADLINE", TCB_DEADLINE), ("TCB_RESULT", TCB_RESULT),
        ("TCB_WAIT_NEXT", TCB_WAIT_NEXT), ("TCB_WAIT_PREV", TCB_WAIT_PREV), ("TCB_WAIT_OBJ", TCB_WAIT_OBJ),
        ("TCB_WAIT_ARG", TCB_WAIT_ARG), ("READY", READY), ("BLOCKED", BLOCKED),
        ("YIELD_LINE", YIELD_LINE), ("TICKS_PER_US", TICKS_PER_US),
        ("TICKS_HIGH_SHIFT", TICKS_HIGH_SHIFT),
        ("BLOB_TICK_US", BLOB_TICK_US),
        ("CRIT_MASK", CRIT_MASK),
        ("FAULT_KERNEL_CRIT_MISMATCH", watch::FAULT_KERNEL_CRIT_MISMATCH as u32),
        ("FAULT_KERNEL_CRIT_SPAN", watch::FAULT_KERNEL_CRIT_SPAN as u32),
        ("FAULT_KERNEL_TIMER_REFUSED", watch::FAULT_KERNEL_TIMER_REFUSED as u32),
        ("FAULT_KERNEL_TIMER_LOST", watch::FAULT_KERNEL_TIMER_LOST as u32),
        ("FAULT_KERNEL_TIMER_UNSET", watch::FAULT_KERNEL_TIMER_UNSET as u32),
        ("FAULT_KERNEL_HEAP_EXHAUSTED", watch::FAULT_KERNEL_HEAP_EXHAUSTED as u32),
        ("FAULT_KERNEL_TIMER_HANDLE_INVALID", watch::FAULT_KERNEL_TIMER_HANDLE_INVALID as u32),
        ("BLOB_MAX_PRIORITY", super::adapter::BLOB_MAX_PRIORITY as u32),
        ("SYSCON_WIFI_RST_EN", SYSCON_WIFI_RST_EN), ("WIFIMAC_RST", WIFIMAC_RST),
        ("EFUSE_MAC_0", EFUSE_MAC_0), ("EFUSE_MAC_1", EFUSE_MAC_1),
        ("RTC_STORE1", RTC_STORE1), ("RNG_DATA", RNG_DATA),
        ("RNG_SPACING_US", RNG_SPACING_US), ("RNG_POLL_CAP", RNG_POLL_CAP),
        ("INTR_MATRIX", INTR_MATRIX), // reg_base.h:61; see INTR_MATRIX above.
        ("WIFI_SOURCES", WIFI_SOURCES), // interrupts.h:18-21; see WIFI_SOURCES above.
        ("ROUTABLE_LINES", ROUTABLE_LINES), // core-isa.h:383-452 minus runtime-owned lines.
        ("FAULT_KERNEL_IRQ_LINE_REFUSED", watch::FAULT_KERNEL_IRQ_LINE_REFUSED as u32), // rt/src/watch.rs:129.
        ("FAULT_KERNEL_IRQ_SOURCE_REFUSED", watch::FAULT_KERNEL_IRQ_SOURCE_REFUSED as u32), // rt/src/watch.rs:131.
        ("FAULT_KERNEL_ISR_TABLE_FULL", watch::FAULT_KERNEL_ISR_TABLE_FULL as u32), // rt/src/watch.rs:133.
        ("FAULT_KERNEL_ISR_ENTRY_REFUSED", watch::FAULT_KERNEL_ISR_ENTRY_REFUSED as u32), // rt/src/watch.rs:135.
        ("ISRS", l.isrs), // emit_s3/src/radio/sched.rs:44-45.
        ("ISR_COUNT", l.isr_count), // emit_s3/src/radio/sched.rs:42-43.
        ("ISR_CAPACITY", l.isr_capacity), // radio::PACK_ISR_LINES; allocation bound, not a live count.
        ("ISR_SLOT", ISR_SLOT), // emit_s3/src/radio/sched.rs:15, twelve-byte slots.
        ("CODE_REGION", l.code_region),
        ("CURRENT", l.current), ("TASKS", l.tasks), ("TASK_COUNT", l.task_count),
        ("MAX_TASKS", l.max_tasks), ("HEAP_BASE", l.heap_base), ("HEAP_SIZE", l.heap_size),
        ("TIMER_FLOOR", u32::try_from(timer_floor_bytes(l.timer_reserve)).expect("timer floor exceeds u32")),
        ("H_SL", HEAP_HEADER_BYTES), // TLSF second-level bitmaps follow H_TIMER_UNSET.
        ("H_TABLES", heap_tables_bytes(l.heap_size)), // End of the TLSF tables, zero before heap_init.
        ("H_DIR", heap_tables_bytes(l.heap_size)), // First timer chunk pointer follows the aligned tables.
        ("TIMER_DIR_WORDS", timer_dir_words(l.heap_size)), // One pointer per 64 possible timer nodes.
        ("TIMER_SIFT_CAP", timer_nodes_max(l.heap_size).max(1).ilog2() + 1), // Levels in the largest possible timer heap.
        ("T_BYTES", TIMER_NODE_BYTES), // Timer node payload, including its final tag word.
        ("H_FL_COUNT", heap_first_levels(l.heap_size)), ("H_SL_COUNT", HEAP_SECOND_LEVELS),
        ("H_FIXED", heap_control_bytes(l.heap_size)), ("M_BYTES", MUTEX_BYTES),
        ("CURSOR", l.current.checked_add(CURSOR_OFFSET).expect("kernel cursor address overflows")),
        ("TCB_FRAME", TCB_FRAME), ("TCB_STACK_BASE", TCB_STACK_BASE), ("TCB_STACK_TOP", TCB_STACK_TOP),
        ("TCB_NAME", TCB_NAME), ("TCB_SIZE", TCB_SIZE as u32), ("DEAD", DEAD),
        ("TCB_THREAD_SEM", TCB_THREAD_SEM),
        ("CONTEXT_SIZE", CONTEXT_SIZE as u32), ("CONTEXT_PC", CONTEXT_PC), ("CONTEXT_PS", CONTEXT_PS),
        ("CONTEXT_AR", CONTEXT_AR), ("CONTEXT_CPENABLE", CONTEXT_CPENABLE),
        ("MIN_STACK_BYTES", super::boot::MIN_STACK_BYTES),
        ("SYSCON_WIFI_CLK_EN", SYSCON_WIFI_CLK_EN), ("CLK_COMMON", CLK_COMMON),
        ("CLK_BT_BASEBAND", CLK_BT_BASEBAND), ("CLK_RNG", CLK_RNG),
        ("PHY_MODEM_WIFI", PHY_MODEM_WIFI), ("PHY_RF_CAL_FULL", PHY_RF_CAL_FULL),
        ("ESP_CAL_DATA_CHECK_FAIL", ESP_CAL_DATA_CHECK_FAIL),
        ("PHY_TRACK_PLL_PERIOD_US", PHY_TRACK_PLL_PERIOD_US),
        ("PHY_SKIP", PHY_SKIP), ("PHY_ON", PHY_ON), ("PHY_FIRST", PHY_FIRST),
        ("PHY_WAKE", PHY_WAKE), ("PHY_WAKE_RESTORE", PHY_WAKE_RESTORE),
        ("PHY_LAST", PHY_LAST), ("PHY_NOT_LAST", PHY_NOT_LAST),
        ("PHY_OP_ENABLE", PHY_OP_ENABLE), ("PHY_OP_DISABLE", PHY_OP_DISABLE),
        ("PHY_CAL_DATA", l.phy_cal_data),
        ("PHY_DIG_REGS", l.phy_dig_regs),
        ("PHY_MUTEX", l.phy_mutex),
        // rt/src/watch.rs:124 names code 100 FAULT_RADIO_PHY_INIT_FAILED.
        ("FAULT_KERNEL_PHY_INIT_FAILED", watch::FAULT_RADIO_PHY_INIT_FAILED as u32),
        ("FAULT_RADIO_START_FAILED", watch::FAULT_RADIO_START_FAILED as u32),
    ];
    // These are register and sentinel bit patterns,
    // not addresses. Preserve bit 31 instead of applying address refusal.
    let bit_patterns = [
        ("GUARD", GUARD), ("WINDOWED_PS", WINDOWED_PS),
    ];
    checked_values.into_iter().map(|(name, value)|
        (name, i32::try_from(value).unwrap_or_else(|_| panic!("kernel constant {name} does not fit i32"))))
        .chain(bit_patterns.into_iter().map(|(name, bits)| (name, bits as i32)))
        .collect()
}

#[derive(Clone, Debug)]
pub struct KernelRegion { pub base: u32, pub len: u32, pub writable: bool, pub kind: RegionKind }

/// An inline scalar memory instruction whose whole access was checked at build time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProvenAccess {
    pub pc: u32,
    pub address: u32,
    pub width: u32,
    pub class: fab_elec_mcu_rt::kernel::AccessClass,
    pub write: bool,
}

#[derive(Clone, Debug)]
pub struct KernelOptions {
    pub base: u32,
    pub state: u32,
    pub t0: u32,
    /// Word-aligned report ring in writable SRAM, required by port 78.
    /// Exports append under the masked thunk contract; the dispatcher drains it.
    pub reports: Option<u32>,
    /// Word-aligned stop word in writable SRAM. Required by port 86; when present,
    /// port 78 also stops the island for PHY init failure and FAULT_KERNEL_STOPPING.
    pub stop_pending: Option<u32>,
    /// Checked in this order. Regions must be nonempty, disjoint and must
    /// not cross the end of the 32-bit address space.
    pub regions: Vec<KernelRegion>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LoopUnit { Byte, Poll }

/// Instruction counts derived from a port's final assembly spans.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PortCost {
    /// Every instruction outside the loop spans, plus CALLX0 and, when the
    /// routine can fault, one traversal of the shared kernel_fault stub.
    pub fixed: u64,
    /// Instructions in the region walks within the fixed cost of one call.
    pub walk: u64,
    /// Sum of loop groups, each costing its longest alternative including
    /// the backward branch. A byte for raw copy/fill, a VALID poll for now64,
    /// or zero for a routine without loops.
    pub per_unit: u64,
    /// The routine's own non-loop instructions, excluding CALLX0 and the stub.
    pub non_loop: u64,
    pub(crate) unit: Option<LoopUnit>,
}

impl PortCost {
    pub(crate) fn bound(self, max: u32) -> u64 {
        let units = match self.unit { Some(LoopUnit::Byte) => u64::from(max), Some(LoopUnit::Poll) => 1, None => 0 };
        // Routine spans are bounded by the assembler's 120 KiB reach check;
        // even u32::MAX units cannot overflow this u64 calculation.
        self.fixed + self.per_unit * units
    }
}

#[derive(Clone, Debug)]
pub struct KernelCode {
    pub base: u32,
    pub bytes: Vec<u8>,
    pub entries: Vec<EntryReport>,
    /// (name, absolute address, byte size), excluding literals and padding.
    pub symbols: Vec<(String, u32, u32)>,
    pub map: SourceMap,
    /// One derived cost per emitted port, in port-number order.
    pub costs: Vec<(u16, PortCost)>,
    /// Optional OSI 64-bit result's high word, resolved by module variable name.
    pub osi_time_high: Option<u32>,
    /// Critical-section nesting word, resolved by module variable name.
    pub crit_depth: Option<u32>,
    /// Set by the pack's first completed PHY calibration.
    pub phy_calibrated: Option<u32>,
    /// Exit INTLEVEL returned in a3 by the critical-section gate routines.
    pub osi_crit_level: Option<u32>,
    /// Longest task critical section in wrapping CCOUNT cycles; R4c-3 reads it.
    pub crit_max_span: Option<u32>,
    /// Report threshold word, resolved by module variable name. The image sets its initial value
    /// from the compiled kernel's largest masked gate max_insns (at least one cycle per
    /// instruction). A report means a pack section outlasted the kernel's worst primitive; this
    /// is no execution limit.
    pub crit_span_report: Option<u32>,
    /// Island return address of the disable that opened that longest section.
    pub crit_max_ra: Option<u32>,
    /// State word holding the initial windowed a0 of a new task.
    pub task_exit_link: Option<u32>,
    /// State word holding the address of the image's PHY init-data table.
    pub phy_init_data: Option<u32>,
}

impl KernelCode {
    /// CALLX0 through RET.N or ILL. max is the call's constant raw-copy/fill
    /// bound; other ports ignore it. now64 counts one VALID poll, with
    /// +per_unit per further poll (+3 for the current emitted poll body).
    /// None means the port has no routine in this blob.
    pub fn port_cost(&self, port: u16, max: u32) -> Option<u64> {
        self.costs.iter().find(|c| c.0 == port).map(|c| c.1.bound(max))
    }
}

fn refusal(message: &str) -> Refusal { Refusal { entry: 0, code: 0, message: message.into() } }

pub fn build_kernel(m: &Module<'_>, o: &KernelOptions) -> Result<KernelCode, Refusal> {
    emit_kernel(m, o, true)
}

/// Gate bounds for the same layout with every scalar access emitted as a checked call.
/// Used to compare gate budgets, including register allocation and branch relaxation.
pub fn checked_gate_bounds(m: &Module<'_>, o: &KernelOptions, shared: u32) -> Result<Vec<(String, u64)>, Refusal> {
    let code = emit_kernel(m, o, false)?;
    let base = (u64::from(o.base) + code.bytes.len() as u64).next_multiple_of(4);
    let base = u32::try_from(base).map_err(|_| refusal("kernel gate address overflows"))?;
    let wrappers = gates::gates(base, &code, gates::EntryRegs { state: o.state, shared });
    Ok(wrappers.gates.into_iter().map(|gate| (gate.name, gate.max_insns)).collect())
}

fn emit_kernel(m: &Module<'_>, o: &KernelOptions, inline_constants: bool) -> Result<KernelCode, Refusal> {
    if !m.funcs().is_empty() { return Err(refusal(crate::image::FUNCTIONS_UNSUPPORTED)); }
    let lines = m.kernel_lines().ok_or_else(|| refusal("build_kernel requires a kernel module"))?;
    if o.base & 3 != 0 { return Err(refusal("kernel code base must be word aligned")); }
    if o.state & 3 != 0 { return Err(refusal("kernel state must be word aligned")); }
    if u64::from(o.state) + u64::from(m.state_words()) * 4 > 1 << 32 {
        return Err(refusal("kernel state address overflows"));
    }
    for (i, r) in o.regions.iter().enumerate() {
        let end = u64::from(r.base) + u64::from(r.len);
        if r.len == 0 || end > 1 << 32 { return Err(refusal("kernel region is empty or wraps")); }
        if o.regions[..i].iter().any(|p| u64::from(r.base) < u64::from(p.base) + u64::from(p.len) && u64::from(p.base) < end) {
            return Err(refusal("kernel regions overlap"));
        }
    }
    let routines = ports::build(m, o, lines)?;
    // Put the routines first so every OP_PORT has its final address before
    // planning handlers. App build_handlers continues down its original path.
    let prefix = routines.bytes.len() as u32;
    let base = o.base.checked_add(prefix).ok_or_else(|| refusal("kernel code address overflows"))?;
    let code = emit::build_domain(m, &Options { base, ports: routines.targets, ..Options::default() },
        Domain::Kernel { state: o.state, costs: &routines.costs, regions: &o.regions, inline_constants })?;
    let mut entries = code.entries;
    for entry in &mut entries { entry.offset += prefix; }
    let mut bytes = routines.bytes;
    bytes.extend(code.bytes);
    let state_slot = |name: &str| m.vars().iter().find(|v| m.string(v.name) == name)
        .map(|v| v.offset.checked_mul(4).and_then(|offset| o.state.checked_add(offset))
            .ok_or_else(|| refusal(&format!("{name} slot address overflows")))).transpose();
    Ok(KernelCode { base: o.base, bytes, entries, symbols: routines.symbols, map: code.map,
        costs: routines.costs, osi_time_high: state_slot("osi_time_high")?, crit_depth: state_slot("crit_depth")?,
        phy_calibrated: state_slot("phy_calibrated")?,
        osi_crit_level: state_slot("osi_crit_level")?, crit_max_span: state_slot("crit_max_span")?,
        crit_span_report: state_slot("crit_span_report")?, crit_max_ra: state_slot("crit_max_ra")?,
        task_exit_link: state_slot("task_exit_link")?, phy_init_data: state_slot("phy_init_data")? })
}
