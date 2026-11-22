// The analog check follows ESP-IDF v6.1.1 components/esp_hal_regi2c/esp32s2/regi2c_impl.c
// (Apache-2.0) and esp-hal's esp32s3 regi2c.rs (MIT OR Apache-2.0, taken under MIT). See fab/CREDITS.md.
// SPDX-FileCopyrightText: 2019-2026 Espressif Systems (Shanghai) CO LTD
// SPDX-FileCopyrightText: 2021 esp-rs
// SPDX-License-Identifier: MIT AND Apache-2.0
//! 6b1 cooperative runtime. Hardware values cite electronics/emcu/s3/facts.md
//! and facts-machine.md; opcode additions cite the Xtensa ISA Summary.
//! Without radio, no runtime helper uses the stack or a12..a15. Dispatcher-owned
//! a12..a15 survive CALL0 handlers and ports, and exceptions only return from WAITI.
//! With radio, the dispatcher is a preemptible CALL0 task; the radio kernel
//! saves its context and runs pick on a separate interrupt stack.
//!
//! Radio-off static retired-instruction counts (not elapsed chip cycles): stamp is 25
//! with one VALID poll, +3 per further poll. A scanned timer costs 6 when
//! stopped, 12 for the first armed row, and 11..14 thereafter. With one due
//! repeating timer, one VALID poll, an empty TX ring and no overrun, a turn
//! is 128 instructions (127 when the deadline has another high word),
//! including CALL0/feed and excluding the handler. Each coalesced firing
//! adds 18..19 plus its Fault (and a feed when motion or touch is declared).
//! Sensor service stretches are derived beside i2c_transfer below.
//! Without a link, capped arm through WAITI, seven vector
//! instructions and the jump back costs at most 124 (121 idle) with an empty
//! drain; pending/blocked TX makes that 133 (130 idle). Add drain byte work
//! when that path sends data. The eight-instruction feed precedes WAITI.
//! A link adds one feed per sleep before LINK_IDLE: 8 instructions, or 10 with radio.
//!
//! Enqueue, excluding its nested drain: 80 fixed; CRC 10/body byte; COBS
//! 15/zero, 16/nonzero, 23/nonfinal 254-byte split. A full final block costs
//! 15 and omits two fixed instructions. Each CRC/COBS feed adds 7 over its
//! ordinary branch path, every 64 input bytes and at the end (except a full
//! final COBS block). Thus a short frame has 94 fixed including both feeds.
//! Refusal costs 36 plus drain, with no CRC/COBS or sequence advance.
//! Drain: 12/byte, +9 per automatic 64-byte flush, plus 25 fixed if empty
//! or 32 if blocked. Sleep flushes a partial packet in 9 more. A host read racing
//! the blocked clear/recheck adds 10 before retrying. No blocked polling.
//! Event packing is at most 160 including LOST and Time, excluding enqueues.
//! F3/F4 static upper counts (forward conditional branches charged as two):
//! With Ethernet, RX_BYTE <=58 per non-delimiter and <=119 per delimiter
//! including slot publication/reset. In a mixed image the restart delimiter
//! takes <=87 even on a stale-epoch carry. Radio alone costs <=54/78: the
//! longest code byte inserts an implicit zero, and a stale delimiter checks
//! both header and epoch, increments both counter words and clears five words.
//! RX adds <=13 per invocation and <=19 per wire byte (21 with a radio feed).
//! With an 8-instruction feed, copy C(n) <=9*n + 8*ceil(n/128) + 16.
//! With a radio feed use 10*ceil(n/128) + 20 for the feed terms. A valid send
//! costs <=60+C(n)+enqueue(n+6); recv <=60+C(n), no frame <=39. Credit
//! costs <=30+enqueue(8), or 9 with nothing owed. Enqueue counts above exclude
//! its nested drain; for B body bytes a uniform bound is
//! 80+10*B+16*(B+1)+7*floor((B+1)/254)+7*(ceil(B/64)+ceil((B+1)/64)).
//! Radio adds two instructions per feed at entry and each CRC/COBS boundary.
//! F4: STALL <=62 instructions, including a high-word counter carry; the
//! empty-ring/disarmed path is <=16. LINK_STATS <=133 with no changes, or
//! <=133+50*k+k*enqueue(15) for k changed counters (k<=5). Excluding its
//! nested drain, enqueue(15)<=500 (506 with radio). REHELLO costs <=19 when
//! full and <=9 when idle. Hello's 28 body bytes and CRC give it an enqueue
//! bound of 94+10*28+16*29=838, or 844 with the three longer radio feeds.
//! With one VALID poll and N Ethernet slot clears,
//! success costs <=275+12*N+838+318 = 1431+12*N, plus the Hello and credit
//! drains. In a mixed image the generation copy costs four instructions and
//! the radio feeds cost two more each: <=279+14*N+844+324 = 1447+14*N.
//! Radio alone clears seven decoder words and sends Hello for <=186+844=1030
//! plus one drain. Each extra VALID poll adds 3. These counts
//! include RET_N and exclude the caller's two instructions for CALLX0 setup
//! and invocation.
//! Runtime tests print independent instret deltas and latency.
use fab_elec_mcu::Compiled;
use fab_elec_mcu_island::{IslandBinding, SegmentKind};
use fab_elec_mcu_emit32::{asm::Pass, xtensa::{assemble, cc, ccz, enc, Asm, Reg}};
use fab_elec_mcu_rt::{kernel::RegionKind, module::{Elem, Module}, ports, sched::DrawRefusal, watch, KernelLines, SETUP};

use crate::{image::{EthSram, LinkSram, Pin, RuntimeRoutine, SpeakerSram, SramLayout}, panel, Panel, PanelSram, SLOT_BASE, SLOT_DATA_BASE};
use crate::{radio, Radio};
use crate::{I2cBus, Motion, MotionSram, Touch, TouchSram};
use crate::i2c::Timing;

mod speaker;
mod mic;
pub(crate) mod net;

/// Conservative fallback bound for a port call's work between feeds. Handler
/// bodies use final emitted counts instead of this allowance per AIR op.
/// USB drains, display loops and buffer copy/fill loops
/// feed themselves; their individual stretches are bounded beside those loops.
///
/// Excluding its self-feeding drain, a Fault's 13-byte body costs at most
/// `94 + 13*10 + 14*16 = 448`, Time `94 + 8*10 + 9*16 = 318`, and LOST
/// `94 + 14*10 + 15*16 = 474`. All three plus 160 for event packing and
/// 256 for port validation, emitter operand/call traffic and bookkeeping
/// total 1656 instructions, rounded generously to 2048. Larger ETH
/// bodies feed during CRC and COBS every 64 input bytes; even a COBS block
/// split within that span stays below `64*16 + 7 + 32 < 2048`. A drain's
/// 64-byte stretch, including an adverse clear/read race, is below 832.
///
/// Arithmetic is smaller: `float.rs` and `tests/codegen.rs` bound normal f32
/// div/sqrt at 91/121. Each subnormal operand adds at most `4*23 - 2 = 90`
/// normalization instructions. `emit::Body::{load,store}` use at most three
/// instructions per operand/result, giving div `91 + 2*90 + 3*3 = 280`
/// and sqrt `121 + 90 + 2*3 = 217`, including operand loads and result store.
/// Buffer element accesses take at most 16 instructions: address literal,
/// offset load (3), limit literal, clamp branch (2) and move, scaling (2),
/// element access and value load/result store (3), rounded up. Buffer port
/// setup, range clamps and once-per-site bookkeeping fit the 256 reserve
/// above; their element loops feed every 128 elements and are excluded.
/// Present's pixel loop, including the RGB565 byte swap, feeds every 64
/// pixels: at most 64 * 22 + 128 setup/finish instructions < 2048.
/// Ethernet send's staging copy and recv's slot copy feed at entry, exit and
/// every 128 bytes: <= 9*128 + 3*8 = 1176 instructions. Send then uses F2's
/// CRC/COBS feeds above. With radio the copy stretch is <= 9*128 + 3*10.
/// RX feeds before each wire byte. Including FIFO/loop/call/feed, a
/// non-delimiter/delimiter costs <=77/138 with Ethernet alone, <=79/140
/// with both features, or <=75/99 with radio alone. Credit adds <=30 to an
/// eight-byte enqueue (318, or 324 with radio, plus its self-feeding drain).
/// This is an instruction bound, not rt::Profile's nominal AIR timing.
pub const MAX_OP_INSNS: u64 = 2048;

/// due:u64, period:u64, armed:u32, repeat:u32, address:u32, handler id:u32.
pub const TIMER_BYTES: u32 = 32;
/// Holds one maximum ETH frame (watch::ETH_WIRE = 1528) plus an event burst,
/// about 125 ms at the model's 64-byte/ms USB service rate. One byte stays empty
/// to distinguish full from empty; refused frames are counted by their caller.
pub const TX_RING_BYTES: u32 = 8192;
const TX_RING_BITS: u8 = TX_RING_BYTES.ilog2() as u8;
/// u16 payload length plus Ethernet bytes, rounded to a word.
pub const RX_SLOT_BYTES: u32 = (2 + watch::ETH_MAX as u32 + 3) & !3;
/// Flash buffer row: address, len, elem, element bytes, width, reserved[3].
pub(crate) const BUFFER_ROW_WORDS: usize = 8;
const BUFFER_ROW_SHIFT: u32 = (BUFFER_ROW_WORDS * 4).ilog2();
/// Caps each sleep at 100 ms at 16 MHz SYSTIMER (facts-machine.md §1).
pub const SLEEP_CAP_TICKS: u32 = 1_600_000;
// Link deadlines use the same 16 MHz SYSTIMER as the dispatcher (§1).
const STALL_TICKS: u32 = 4_000_000;
const ANSWER_TICKS: u32 = 32_000_000;
const ANSWER_MAX_TICKS: u32 = 480_000_000;
/// Allows a fixed number of runtime instructions between feeds. Even a
/// coalesced firing with pending LOST and Time reaches enqueue's entry feed
/// within 128 (turn) + 25 (stamp) + 133 (arm/wake) + 19 (coalescing) + 160
/// (packing) + 832 (the turn's drain, which feeds only after a full packet)
/// = 1297 instructions. Sleep's feeds run at INTLEVEL 15 and preserve interrupt
/// state. Through sleep's DRAIN and feed, retain the full 1297 and both sensor
/// checks (40). Ethernet adds <=64 for F3 (15+24+14+11 for RX, selection,
/// invocation and credit) and 2+62 for TURN's STALL. Even charging a final RX
/// byte on top, Ethernet costs <=1297+40+64+64+138+8=1611, mixed
/// <=1297+40+64+64+140+10=1615, and radio alone <=1297+40+10=1347.
///
/// From that feed, calling LINK_IDLE and saving the return costs 4. With one
/// VALID poll, STAMP and STALL cost 2+25 and 2+62. Ethernet's answer check
/// costs <=26, so the prefix is 4+27+64+26=121 for Ethernet and mixed.
/// Radio alone does RX here: 4+15+27+64=110 on an empty FIFO. With input,
/// allow another 99 from the final byte's feed on every radio-only path below.
/// RX feeds per byte in all three layouts.
///
/// Successful REHELLO with Ethernet reaches its first slot feed in another
/// 2 (call) + 16 (space check) + 27 (STAMP) + 2 (epoch) + 4 (slot setup)
/// + 3 (clear/advance) + 8 or 10 (feed): <=183 from LINK_IDLE's boundary,
/// or 185 mixed. Further slots feed every 12 or 14 instructions. The last
/// slot reaches Hello's WIRE feed within 12+275+12=299 or 14+279+14=307,
/// retaining the full fixed REHELLO allowance. Radio alone reaches Hello's
/// WIRE feed in <=110+2+186+14=312. WIRE's entry costs 4 plus the feed.
///
/// If REHELLO is idle or full, allow 2+19 for it. An owed credit reaches WIRE
/// in <=121+21+2+30+12=186 (188 mixed). Otherwise the credit check costs 11
/// with Ethernet, zero without it. The first changed LINK row reaches WIRE
/// within 121+21+11+2+133+50+12=350 (352 mixed), or 110+21+2+133+50+14=330
/// with radio alone. A quiet scan returns in another 3 after LINK_STATS.
/// Through the next radio feed allow 121+21+11+2+133+3+10=301 mixed or
/// 110+21+2+133+3+10=279 radio alone. Ethernet alone continues to the final
/// sleep feed within 121+21+11+2+133+3+133+446=870, retaining the arm/wake
/// and F4 allowances for the flush, STALL and deadline comparisons.
/// After WIRE's final encoder feed, allow 32+832+5=869 for tail, drain and
/// return. Even another changed row reaches its feed within 869+446+50+14=1379.
/// A quiet suffix reaches the radio feed within 869+446+10=1325 or the final
/// Ethernet sleep feed within 869+446+133=1448.
///
/// From sleep's radio feed, RADIO_RECORD reaches WIRE within 53, or returns within
/// 23 for an absent, deferred or exhausted record. RADIO_REPORTS reaches WIRE within
/// 200 including event packing, so the first report costs <=23+200=223 to its feed. Each of
/// the eight reports enters WIRE even on refusal. If both senders are empty,
/// allow 23+64 (including bulk LOSS) +64 (PHY) +7 (empty event check) +24 (restart)
/// +133+446+16 (arm, link and restart deadline checks) = 777 through sleep's feed. Between record frames
/// the WIRE tail, drain, return and next sender feed cost <=32+832+5+44=913.
/// Between WIRE feeds allow <=64 for EVENT's loss/return suffix (including
/// both LOSS calls if Time is refused): 32+832+5+64+200=1133.
/// After the last EVENT, head/empty checks, bulk LOSS
/// and return cost <=64; through sleep's feed the suffix is bounded by
/// 32+832+5+64+64+133+446=1576 before the restart check. Allow 24 more for
/// its caller and the RUNNING return or first policy feed. A recognized stop
/// feeds on entry and on deferral. Its policy interval is bounded by 512+2=514,
/// including one STAMP, at most 31 six-instruction delay doublings and the
/// additional MOVI/compare for a failed start. RUNNING bypasses that comparison.
/// The PHY snapshot check adds 6 instructions once saved or refused, 10 while
/// awaiting calibration (including CALL0 setup), or at most 64 on refusal
/// (48 plus <=11 to exclude a failed PHY start and five for port 78's non-stopping publication).
/// This final EVENT-to-sleep stretch is the largest fixed dispatcher bound
/// for the three layouts: 1576+24+64+7+16=1687, leaving 361 of 2048. The 164-word copy costs
/// 1515 instructions: 162 ordinary trips at 9, two at 8, four feeds at 10
/// (entry, 128 and zero remaining, exit), and the entry zero check.
/// RADIO_EVENTS on an empty list costs 7 including its call. Nonempty entry/return
/// costs 15 including switching stacks and back. A station item costs 50 plus
/// its take, EVENT and free: 3 feeds at 10, two gate calls at 2, one result branch,
/// save/load node at 1 each, 7 to classify base/id, 5 to call EVENT and 1 backedge.
/// A synthetic island without WIFI_EVENT uses 1 for classification. The final empty
/// take adds 24 (two feeds, CALL0 and the taken branch) plus the gate itself.
/// At INTLEVEL 15 no poster can grow the list. Between feeds allow take.max_insns+12
/// and free.max_insns+14 (load node, CALL0, backedge, feed); both fixed additions
/// fit in RUNTIME_GAP_INSNS alongside the image's largest gate allowance.
/// After take's feed allow 14 through the EVENT call, <=67 through its first WIRE
/// call, then 4 stores/setup + <=20 through WIRE's due-checked feed.
/// RADIO_START feeds every 128 absolute stores. Its bracketed restart stretch is
/// at most 128*3 + 10 (feed) + 2 (call) + 4 (READY check) + 12 (interrupt/PS
/// tail and return) = 412 instructions.
/// RADIO_SWITCH_FIXED_INSNS still covers only irq_entry through RFE.
/// Add TIMER_SCAN_INSNS per declared timer and the largest Handler::max_insns:
/// its final emitted body count with loop trips, larger IF arms, inline helper
/// repetitions and port allowances, plus wrapper/frame clear. build_image also
/// adds any configured radio burst/switch/masked-gate allowance, then converts at CPU_HZ.
pub const RUNTIME_GAP_INSNS: u64 = 2048;
/// Allows the worst scan cost per declared timer, from the header's 6..14 instructions.
pub const TIMER_SCAN_INSNS: u64 = 14;
/// Dispatcher-owned CPU lines: TARGET0 and optional GDMA OUT1 on 1 (facts-machine.md §2), and
/// USB-Serial-JTAG source 96 on 2, level 1, level-triggered
/// (S3 TRM v1.8 Table 9.3-2 p. 544). Island lines must never use 1 or 2.
/// A radio-free image also waits for SPI2 on SPI_LINE (spi_send).
pub const DISPATCHER_LINES: u32 = (1 << 1) | (1 << 2);
const YIELD: u32 = 1 << radio::sched::YIELD_LINE;
/// The kernel preserves dispatcher/yield enables and may raise only yield.
pub const RADIO_KERNEL_LINES: KernelLines = KernelLines { protected: DISPATCHER_LINES | YIELD, raise: YIELD };
/// The normal path adds four instructions for the idle comparison: vector 8
/// + save/switch (133+4) + restore 100 = 245, still below R3-3's 256 allowance.
/// Add <=90 spill/retry attempts plus sched.rs's fixed 64. This bounds irq_entry
/// through RFE, even retaining the vector allowance. Add 20 per task and
/// radio::MARKED_ISR_SLOT_INSNS per ISR slot, plus the production report policy:
/// each of the two possible scheduler reports (98/99) adds five instructions.
/// Radio::burst_insns covers the matching shim/ISR bodies.
pub const RADIO_SWITCH_FIXED_INSNS: u64 = 256 + 90 + 64;
/// Pure dispatcher wake from idle, user vector through restore's RFE:
/// vector 8 + entry 8 (ADDMI, two stores, three loads, expanded taken branch)
/// + IDLE_FAST 19 (pending check 7, acknowledge/mask 5, READY/cursor/current
/// stores 6, jump 1) + restore 100 = 135 retired instructions. EXCM stays set;
/// there are no spills, picker calls or task/ISR scans. Excludes block's return
/// and subsequent dispatcher work; the radio/off latency difference is bounded
/// by this full wake cost, retaining the radio-off vector's allowance.
pub const RADIO_IDLE_WAKE_INSNS: u64 = 8 + 8 + 19 + 100;
/// Switch allowance at the profile's maximum table occupancy, excluding the
/// island execution already charged to burst_insns. Instructions, not chip cycles.
pub fn radio_switch_insns(profile: &Radio) -> u64 {
    RADIO_SWITCH_FIXED_INSNS + 20 * u64::from(profile.task_capacity)
        + (radio::MARKED_ISR_SLOT_INSNS + 2 * 5) * u64::from(profile.isr_capacity)
}
pub const KERNEL_VECTOR: u32 = SLOT_BASE + 0x300;
pub const USER_VECTOR: u32 = SLOT_BASE + 0x340;
pub const DOUBLE_VECTOR: u32 = SLOT_BASE + 0x3c0;
pub const INIT: u32 = SLOT_BASE + 0x400;

// ISA Summary pp. 295–296,301,305,312,317.
const EXCSAVE1: u32 = 209;
const EXCCAUSE: u32 = 232;
const VECBASE: u32 = 231;
const CPENABLE: u32 = 224;
const INTENABLE: u32 = 228;
const FCR: u32 = 232;
// facts-machine.md §1 / systimer_reg.h:16–520, reg_base.h:37.
// rtc_cntl_reg.h:3679-3694: DATE_REG[18:13] are the LDO slave power-downs.
const RTC_DATE: u32 = 0x6000_81fc;
const LDO_SLAVES: u32 = 0x3f << 13;
// facts.md section 3, TRM p.830 Register 17.3 and p.840 Register 17.20.
const CPU_PER_CONF: u32 = 0x600c_0010;
const SYSCLK_CONF: u32 = 0x600c_0060;
// TRM p.530 Table 7.2-2: the 480 MHz PLL (PLL_FREQ_SEL) with CPUPERIOD_SEL 1 is 160 MHz, init's first clock.
const CPU_PERIOD_160: u32 = (1 << 2) | 1;
// TRM p.830 Register 17.3, SYSTEM_CPU_WAIT_MODE_FORCE_ON: set, WAITI leaves the CPU's clock on, so
// CCOUNT counts through the wait (the feed's clock).
const CPU_WAIT_MODE_FORCE_ON: u32 = 1 << 3;
// The feed's quarter at most, so CCOUNT minus the due word stays inside the signed range.
const RWDT_QUARTER_CAP: u32 = 1 << 30;
const _: () = assert!(crate::CPU_HZ == 240_000_000, "init's raise ends at CPU_PERIOD_240 with the LDO slaves 0x7 >> 3");
const SYSTIMER: u32 = 0x6002_3000;
const UNIT0: u32 = (1 << 31) | (1 << 30); // register clock, unit-0 work
const TARGET0: u32 = 1 << 24;
const UPDATE: u32 = 1 << 30;
// §2 / interrupt_core0_reg.h:473–480, reg_base.h:61; CPU line 1 is level 1.
const TARGET0_MAP: u32 = 0x600c_20e4;
// §4: rtc_cntl_reg.h:564–569; reset_reasons.h:45.
const RWDT_RESET_CAUSE: u32 = 9;
// §6 / facts.md §4a. IO_MUX function 1, OUT_SEL 256, OEN_SEL bit 10.
const GPIO: u32 = 0x6000_4000;
const IO_MUX: u32 = 0x6000_9000;
// §7 / usb_serial_jtag_reg.h:14–46; free bit 1, WR_DONE bit 0.
const USB_FREE_BIT: u8 = 1;
// facts-machine.md §7 / usb_serial_jtag_reg.h:254–259,328–333: IN_EMPTY bit 3,
// INT_ENA +0x10, INT_CLR +0x14. TRM pp.1248–1249: 64-byte auto-flush.
const USB_EMPTY: u32 = 1 << 3;
// usb_serial_jtag_reg.h:27–32,260–266,334–340: OUT data available bit 2,
// OUT_RECV_PKT interrupt bit 2. Clear before draining, never after it.
const USB_RX: u32 = 1 << 2;
// facts-machine.md §7, TRM v1.8 pp.1259–1262 Registers 33.6–33.9;
// usb_serial_jtag_reg.h:69,148,224,298: IN_TOKEN_REC_IN_EP1 is bit 8
// in RAW (+8), ST (+0x0c), ENA (+0x10) and CLR (+0x14).
const USB_IN_TOKEN: u32 = 1 << 8;
// §2 / interrupt_core0_reg.h:785–791: source 96, map +0x180.
// TRM v1.8 Table 9.3-2 p.544: CPU line 2 is level-1, level-triggered.
const USB_MAP: u32 = 0x600c_2180;
// facts-machine.md:135 / TRM v1.8 p.541 §9.3.1 Table 9.3: SPI2's source 21
// (SPI_INTR_2), map +0x054. facts-machine.md:242 / TRM p.544 Table 9.3-2: CPU
// line 3 is level-1, level-triggered. Radio-free images only: a radio image
// leaves the line to the island's routing (ROUTABLE_LINES) and polls.
const SPI2_MAP: u32 = 0x600c_2054;
const SPI_LINE: u32 = 3;
/// Board-unverified (Symbols::board_unverified) until a person has seen Sand on a real board's
/// screen. Sand's check image ran these sends on a real S3 report for report with its twin (11-18,
/// fixtures/examples/board_check.rs), but the panel stayed dark, so what they put on the glass is unseen.
const SPI_WAITI: &str = "SPI2 send waits for its transfer's end in WAITI on CPU line 3";
// facts-machine.md:156 / TRM v1.8 p.542 §9.3.1 Table 9.3-1: I2C0's source 42
// (I2C_EXT0_INTR), map +0x0A8. facts-machine.md:243 / TRM p.544 Table 9.3-2: CPU
// line 4 is level-1, level-triggered. Radio-free images only, as SPI_LINE.
const I2C0_MAP: u32 = 0x600c_20a8;
const I2C_LINE: u32 = 4;
/// Board-unverified (Symbols::board_unverified) with SPI_WAITI, until a person has seen Sand on a real
/// board's screen. The check image's accelerometer reads gave live values (11-18: about 0.04 g, -0.08 g,
/// -1.08 g lying still) and its touch polls found no finger; how the reads follow a tilt needs a hand
/// on the board.
const I2C_WAITI: &str = "I2C0 transfer waits for its STOP or deadline in WAITI on CPU lines 4 and 1";
// ISA Summary RSR p.668; CCOUNT is special register 234, the cycle counter
// (core-isa.h:354 XCHAL_HAVE_CCOUNT).
const CCOUNT: u32 = 234;
// ISA pp.279-280: LCOUNT is special register 2; a WSR to it takes an ISYNC before any loop use.
const LCOUNT: u32 = 2;
// IDF soc/esp32s3/include/soc/regi2c_defs.h:12,17,21,25: I2C_MST_ANA_CONF0_REG,
// ANA_CONFIG_REG with I2C_BBPLL_M (bit 17), ANA_CONFIG2_REG.
const ANA_CONF0: u32 = 0x6000_E040;
const ANA_CONFIG: u32 = 0x6000_E044;
const ANA_CONFIG2: u32 = 0x6000_E048;
const ANA_BBPLL_MASK: u32 = 1 << 17;
// The ESP32-S2's command layout (esp_hal_regi2c/esp32s2/regi2c_impl.c:54-79, :109-118) found no
// slave through 0x6000E000 (the first analog check) and read data 0x00 for every register through
// host 1's word at 0x6000E004 (session 16).
// b2: the chip's own analog I2C read, rom_i2c_readReg(block, host_id, reg_add) -> u8 (esp-hal 4059aa58
// esp-hal/src/soc/esp32s3/regi2c.rs:135-142, its esp_rom_regi2c_read; esp-rom-sys/ld/esp32s3/rom/
// esp32s3.rom.api.ld:66 names it rom_i2c_readReg, esp32s3.rom.ld:1247 places it at 0x40005d48),
// called through the windowed shim. It shares the slot's 1-GB region, as RETW needs.
const ROM_I2C_READ_REG: u32 = 0x4000_5d48;
// rom_i2c_writeReg(block, host_id, reg_add, data) (esp-hal 4059aa58 esp-hal/src/soc/esp32s3/regi2c.rs:144-151,
// its regi2c_write; esp32s3.rom.api.ld:68 names it, esp32s3.rom.ld:1249 places it), through the same shim.
const ROM_I2C_WRITE_REG: u32 = 0x4000_5d60;
// regi2c_bbpll.h, regi2c_dig_reg.h (and esp-hal's REGI2C_BBPLL(0x66, 1)): both blocks are host 1.
const ANALOG_HOST: u32 = 1;
// regi2c_dig_reg.h: DIG_REG (0x6D) register 4 holds EXT_RTC_DREG and register 6 EXT_DIG_DREG, each in [4:0].
const DIG_REG: u32 = 0x6d;
const DREG_FIELD: u32 = 0x1f;
// efuse_reg.h:775-1017 (reg_base.h:13): RD_MAC_SPI_SYS_3 (BLK_VERSION_MINOR [26:24]), _4 (K_RTC_LDO [19:13],
// K_DIG_LDO [26:20], V_RTC_DBIAS20's low five bits [31:27]), _5 (its high three [2:0], V_DIG_DBIAS20 [10:3],
// DIG_DBIAS_HVT [15:11]) and RD_SYS_PART1_DATA4 (BLK_VERSION_MAJOR [1:0]). V_RTC_DBIAS20 is one eight-bit
// value across the word boundary, its low bits first, as BLOCK1's bits run (INFERRED: IDF's getter is not on
// disk; both halves are "BLOCK1 voltage of rtc dbias20", and rtc_init.c:337 reads bit 7 as its sign).
const EFUSE_SYS_3: u32 = 0x6000_7050;
const EFUSE_SYS_4: u32 = 0x6000_7054;
const EFUSE_SYS_5: u32 = 0x6000_7058;
const EFUSE_PART1_4: u32 = 0x6000_706c;
// IDF esp_hw_support/port/esp32s3/include/soc/rtc.h:139-142.
const K_RTC_MID: u32 = 198;
const K_DIG_MID: u32 = 211;
const V_RTC_MID: u32 = 10181;
const V_DIG_MID: u32 = 10841;
// rtc_init.c:41-42: the 240 MHz settings of a chip whose eFuse carries no PVT calibration.
const DBIAS_240_DEFAULT: u32 = 28;
// IDF rtc_clk.c:241's esp_rom_delay_us(40) after the regulator writes, in CCOUNT cycles at 160 MHz: the ROM's
// delay counts CPU ticks per us, which IDF sets for the new clock only after the switch (:251).
const RAISE_SETTLE_CYCLES: u32 = 40 * 160;
// TRM p.530 Table 7.2-2: the 480 MHz PLL with CPUPERIOD_SEL 2 is 240 MHz (clk_tree_ll.h:467-483).
const CPU_PERIOD_240: u32 = (1 << 2) | 2;
// The interval the check times CCOUNT over: 100 us of the 16 MHz systimer, 16000 cycles at 160 MHz, 24000 at 240;
// its last round twice that.
const CLOCK_CHECK_TICKS: u32 = 1600;
/// The analog I2C registers `SlotOptions::analog_check` reads, as (slave id, register): BBPLL (0x66)
/// registers 2-6 and 9, whose IDF 480 MHz-from-40 MHz values (esp_hal_clock/esp32s3/include/hal/
/// clk_tree_ll.h:310-385) tell which PLL configuration the ROM left, then DIG_REG (0x6D) registers 4-7
/// and 13 (regi2c_dig_reg.h), the regulators as init's raise left them.
pub const ANALOG_READS: [(u8, u8); 11] = [
    (0x66, 2), (0x66, 3), (0x66, 4), (0x66, 5), (0x66, 6), (0x66, 9),
    (0x6d, 4), (0x6d, 5), (0x6d, 6), (0x6d, 7), (0x6d, 13),
];
/// Probe id of `ANALOG_READS[k]`'s answer is this plus 2k, its poll's id the next one.
pub const ANALOG_READ_PROBES: u8 = 16;
/// The analog check's probes after its reads, ids from this one: the settings init's raise wrote, EXT_RTC_DREG |
/// EXT_DIG_DREG << 8 (`dbias_240` again), then the clock check at CPU_HZ (nine probes, `clock_check`). The DIG_REG 4
/// and 6 answers among ANALOG_READS are the registers as the raise left them.
pub const RAISE_PROBES: u8 = ANALOG_READ_PROBES + 2 * ANALOG_READS.len() as u8;
/// A panel report's probe ids (Panel::report), in init's order: the expander's Input, Output and
/// Config as found (E02-E05); the time after its reset pulse and wait; Input, Output and Config again;
/// the times after SWRESET's wait, after SLPOUT's wait and after DISPON. A time is STAMP's ticks
/// since t0 over 1,024 (64 us at the 16 MHz systimer).
pub const PANEL_PROBES: u8 = 64;

const TURN: usize = 0;
const STAMP: usize = 1;
const EVENT: usize = 2;
const WIRE: usize = 3;
const PORT: usize = 4; // reserve the entire port table, including unsupported ports
const CRC: usize = PORT + ports::PORTS.len();
const DRAIN: usize = CRC + 1;
const LOSS: usize = DRAIN + 1;
const RADIO_START: usize = LOSS + 1;
const RADIO_BLOCK: usize = LOSS + 2;
const RX: usize = RADIO_BLOCK + 1;
const RX_BYTE: usize = RX + 1;
const CREDIT: usize = RX_BYTE + 1;
const STALL: usize = CREDIT + 1;
const REHELLO: usize = STALL + 1;
const LINK_IDLE: usize = REHELLO + 1;
const LINK_STATS: usize = LINK_IDLE + 1;
const RADIO_RECORD: usize = LINK_STATS + 1;
const RADIO_REPORTS: usize = RADIO_RECORD + 1;
const LOSS_N: usize = RADIO_REPORTS + 1;
const ISLAND_START: usize = LOSS_N + 1;
const RADIO_BEGIN: usize = ISLAND_START + 1;
const RADIO_RESTART: usize = RADIO_BEGIN + 1;
const RADIO_STOP: usize = RADIO_RESTART + 1;
const RADIO_PHY_TABLE: usize = RADIO_STOP + 1;
const RADIO_EVENTS: usize = RADIO_PHY_TABLE + 1;
const RADIO_EVENT_TAKE: usize = RADIO_EVENTS + 1;
const RADIO_EVENT_FREE: usize = RADIO_EVENT_TAKE + 1;
const SPEAKER_SERVICE: usize = RADIO_EVENT_FREE + 1;
const SPEAKER_CLOCK: usize = SPEAKER_SERVICE + 1;
const MIC_SERVICE: usize = SPEAKER_CLOCK + 1;
const MIC_IRQ: usize = MIC_SERVICE + 1;
const RADIO_IRQ: usize = MIC_IRQ + 1;
const NET_FLUSH: usize = RADIO_IRQ + 1;

/// Slot DROM addresses used by the island loader.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct IslandTables {
    /// The island header's sixteen little-endian words.
    pub header: u32,
    /// One windowed thunk address per island gate index.
    pub gates: u32,
    /// RAM segments in binding order. IRAM/DATA carry (source offset, destination, bytes), BSS (destination, bytes).
    pub segments: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RadioSymbols {
    /// All radio blobs, including alignment padding, in emission order.
    pub blobs: Vec<RuntimeRoutine>,
    pub osi_table: u32,
    /// Distinct OSI targets in thunk order; also the trace gate indices.
    pub thunks: Vec<(String, u32)>,
    /// Placed by the slot linker after the runtime and handlers.
    pub island_tables: IslandTables,
    pub island_status: u32,
    /// Windowed entries in the island import map's gate-index order.
    pub island_thunks: Vec<(String, u32)>,
    /// Checked ranges in probe order, with their write permission and region kind.
    pub kernel_regions: Vec<(std::ops::Range<u32>, bool, RegionKind)>,
    pub gate_reports: Vec<radio::kernel::gates::GateReport>,
    pub kernel_map: crate::SourceMap,
    pub kernel_symbols: Vec<(String, u32, u32)>,
    pub adapter_symbols: Vec<(String, u32, u32)>,
    pub isr_start: u32,
    pub isr_max_span: u32,
    pub isr_max_entry: u32,
    pub crit_depth: u32,
    pub phy_calibrated: u32,
    /// Entry address of the kernel's port-78 report routine.
    pub report: u32,
    /// The kernel's crit_span_report initial value: the largest gate's max_insns, at least one
    /// cycle per instruction. A report means a pack critical section outlasted the emitted
    /// kernel's worst primitive.
    pub crit_span_report_cycles: u32,
    /// The kernel state word holding that threshold after init.
    pub crit_span_report: u32,
    /// Kernel state word holding the windowed return link to task_exit.
    pub task_exit_link: u32,
    /// Kernel state word holding the DROM address of the PHY init-data table.
    pub phy_init_data: u32,
    /// Largest kernel gate plus its thunk's masked instructions, charged to RWDT.
    pub masked_gate_insns: u64,
    pub irq_entry: u32,
    pub restore: u32,
    pub stack_fault: u32,
    pub fault_entry: u32,
    pub radio_stop: u32,
    /// Masked dispatcher entry, a2 points to RadioSram::request.
    pub restart: u32,
    /// First PC after port 78 publishes the start marker, still masked.
    pub started: u32,
    /// Retired instructions from restart entry through the masked return.
    pub restart_max_insns: u64,
    pub fatal: u32,
    pub trace_record: u32,
    pub pick: u32,
    pub block: u32,
    pub idle: u32,
    pub no_task: u32,
    pub shim: u32,
    pub start: u32,
    pub task_exit: u32,
    /// CALLX0 block PCs: future deadline, then idle dispatcher.
    pub block_calls: [u32; 2],
}

#[derive(Clone, Debug, Default)]
pub struct Symbols {
    /// Runtime paths in this image that no board has shown working yet ("board-unverified"), by name.
    /// The board check that first proves one on a chip removes its entry.
    pub board_unverified: Vec<&'static str>,
    pub t0_ready: u32,
    /// After device init, a2/a3 hold the common first-deadline origin in SYSTIMER ticks since t0.
    pub timer_start: u32,
    pub dispatch: u32,
    pub stamp: u32,
    pub stamp_end: u32,
    pub call_handler: u32,
    pub handler_return: u32,
    /// WAITI normally; CALLX0 block under radio (also RadioSymbols::block_calls[0]).
    pub waiti: u32,
    pub arm: u32,
    /// WAITI normally; CALLX0 block under radio (also RadioSymbols::block_calls[1]).
    pub idle_waiti: u32,
    pub drain: u32,
    pub drain_end: u32,
    pub setup_call: u32,
    /// SPI_ADDR store PCs, for correlating executed commands with board bus events.
    pub panel_commands: Vec<u32>,
    /// Sample transfer boundaries for retired-instruction measurements.
    pub motion_start: u32,
    pub motion_end: u32,
    /// Touch poll boundaries, including unchanged and failed reports.
    pub touch_start: u32,
    pub touch_end: u32,
    pub radio: Option<RadioSymbols>,
}

#[derive(Default)]
pub(crate) struct Link {
    pub state_init: u32,
    pub shared: u32,
    pub timer_init: u32,
    pub roles: u32,
    pub crc8: u32,
    /// `CRC32_TABLE` in the slot, read through DROM.
    pub crc32: u32,
    pub buffers: u32,
    pub slot_bytes: u32,
    pub setup: u32,
    pub eth: u32,
    pub net_config: u32,
    pub speaker: u32,
    pub mic: u32,
    pub radio_state_init: u32,
    pub radio_shared: u32,
    pub radio_table_init: u32,
    pub island_tables: IslandTables,
    pub island_flash_page: u32,
    pub island_pages: u32,
    pub phy_init_data: u32,
    pub radio_report: u32,
    pub radio_crit_depth: u32,
    pub radio_phy_calibrated: u32,
    pub radio_crit_span_report_cycles: u32,
    /// SlotOptions::rwdt_quarter_per_hold: init multiplies the programmed RWDT hold by it.
    pub rwdt_quarter_per_hold: u32,
}

pub(crate) struct RadioData {
    pub state_init: Vec<u32>,
    pub shared: Vec<u32>,
    pub table: Vec<u32>,
    pub island_gates: Vec<u32>,
}

pub(crate) struct Runtime {
    pub bytes: Vec<u8>,
    pub ports: Vec<(u16, u32)>,
    pub routines: Vec<RuntimeRoutine>,
    pub symbols: Symbols,
    pub radio_data: Option<RadioData>,
}

struct Code<'a> {
    a: &'a mut Asm,
    next: usize,
    routines: Vec<RuntimeRoutine>,
    symbols: Symbols,
    radio: bool,
    mic: bool,
    /// The SRAM address of t0, the SYSTIMER count stamps are relative to.
    t0: u32,
    /// SramLayout::rwdt_due: the CCOUNT the next feed is due at, then the quarter.
    rwdt_due: u32,
}

impl Code<'_> {
    fn label(&mut self) -> usize { let label = self.next; self.next += 1; label }
    fn bind(&mut self, label: usize) { self.a.base.label_assign(label); }
    fn jump(&mut self, label: usize) { self.a.j_label(label); }
    fn branch(&mut self, cond: u8, x: Reg, y: Reg, label: usize) { self.a.bcc_reg_reg_label(cond, x, y, label); }
    fn zero(&mut self, cond: u8, r: Reg, label: usize) { self.a.bccz_reg_label(cond, r, label); }
    fn here(&self) -> u32 { SLOT_BASE + self.a.base.offset() }
    fn imm(&mut self, r: Reg, value: u32) { self.a.mov_reg_i32_optimised(r, value); }
    // Relocations always occupy one literal and one L32R, even when zero in
    // the provisional image. This makes final linking independent of values.
    fn address(&mut self, r: Reg, value: u32) { self.a.mov_reg_i32(r, value); }
    fn load(&mut self, r: Reg, address: u32) { self.address(11, address); self.a.l32i(r, 11, 0); }
    fn store(&mut self, r: Reg, address: u32) { self.address(11, address); self.a.s32i(r, 11, 0); }
    fn byte(&mut self, r: Reg, address: u32) { self.address(11, address); self.a.s8i(r, 11, 0); }
    fn set(&mut self, address: u32, value: u32) { self.imm(10, value); self.store(10, address); }
    fn shr(&mut self, dst: Reg, src: Reg, n: u32) { self.imm(11, n); self.a.ssr(11); self.a.srl(dst, src); }
    fn shl(&mut self, dst: Reg, src: Reg, n: u32) { self.imm(11, n); self.a.ssl(11); self.a.sll(dst, src); }
    fn call(&mut self, label: usize) {
        self.address(11, SLOT_BASE + self.a.base.label_offset(label).unwrap_or(0)); self.a.callx0(11);
    }
    fn routine(&mut self, label: usize, name: &'static str) {
        if let Some(last) = self.routines.last_mut() { last.end = SLOT_BASE + self.a.base.offset(); }
        self.a.base.align(4);
        self.bind(label);
        self.routines.push(RuntimeRoutine { name, start: self.here(), end: 0 });
    }
    /// The RWDT feed once it is due: when CCOUNT has reached the due word (SramLayout::rwdt_due), the
    /// due word moves a quarter of the RWDT's timeout on (the word after it) and the feed follows;
    /// a feed point before that costs a CCOUNT read and a compare instead of three RTC_CNTL writes
    /// (wdt_hold's doc has the bound). CCOUNT counts through WAITI (init sets
    /// CPU_WAIT_MODE_FORCE_ON). The signed compare holds while the quarter and a feed point's
    /// lateness stay under 2^31 cycles: init caps the quarter at 2^30. With the radio the due
    /// word and the feed are one masked section, so a task switched out between them cannot hold
    /// the others' feeds back. Only a9..a11.
    fn feed(&mut self) {
        let skip = self.label();
        let due = self.rwdt_due;
        // RSIL needs no SYNC; restoring PS needs RSYNC (ISA pp. 582–583,289).
        if self.radio || self.mic { self.a.op24(enc::rrr(0, 0, 0, 6, 15, 11)); }
        let now = if self.radio || self.mic { 10 } else { 11 };
        self.a.op24(enc::rsr(0, 3, 0, CCOUNT, now));
        self.address(9, due); self.a.l32i(9, 9, 0); self.a.sub(9, now, 9);
        self.zero(ccz::LT, 9, skip);
        if self.radio || self.mic {
            // a11 holds PS: the quarter goes through a9, which then takes the address again.
            self.address(9, due); self.a.l32i(9, 9, 4); self.a.add_n(10, 10, 9);
            self.address(9, due); self.a.s32i(10, 9, 0);
            self.feed_masked();
            self.bind(skip);
            self.a.wsr(11, 230); self.a.rsync();
            return;
        }
        self.address(9, due); self.a.l32i(10, 9, 4); self.a.add_n(11, 11, 10); self.a.s32i(11, 9, 0);
        self.feed_now();
        self.bind(skip);
    }
    /// The RWDT feed now, whatever the due word says: init's clears (before the due word holds
    /// anything) and the one-off paths (setup's return, radio start and restart, power-on, the
    /// installs and copies), so their feeds stay where the power and install sequences place
    /// them. The repeating paths use `feed`. facts-machine.md §5: unlock, FEED bit 31, relock.
    /// Only a9..a11.
    fn feed_now(&mut self) {
        if self.radio || self.mic {
            // libphy also touches RTC_CNTL. Keep unlock/feed/relock indivisible.
            self.a.op24(enc::rrr(0, 0, 0, 6, 15, 11));
            self.feed_masked();
            self.a.wsr(11, 230); self.a.rsync();
            return;
        }
        self.address(9, crate::RWDT_PROTECT); self.address(10, crate::WDT_KEY);
        self.a.s32i(10, 9, 0);
        self.address(11, crate::RWDT_FEED); self.imm(10, crate::RWDT_FEED_BIT);
        self.a.s32i(10, 11, 0); self.imm(10, 0); self.a.s32i(10, 9, 0);
    }
    /// Unlock, feed and relock with PS saved in a11 (the radio's masked feed): a9 and a10 only.
    fn feed_masked(&mut self) {
        self.address(9, crate::RWDT_FEED); self.address(10, crate::WDT_KEY);
        self.a.s32i(10, 9, crate::RWDT_PROTECT - crate::RWDT_FEED);
        self.imm(10, crate::RWDT_FEED_BIT); self.a.s32i(10, 9, 0);
        self.imm(10, 0); self.a.s32i(10, 9, crate::RWDT_PROTECT - crate::RWDT_FEED);
    }
    fn copy(&mut self, src: u32, dst: u32, words: u32) {
        self.address(2, src); self.address(3, dst); self.imm(4, words);
        let done = self.label(); let again = self.label();
        self.zero(ccz::EQ, 4, done); self.bind(again);
        self.a.l32i(5, 2, 0); self.a.s32i(5, 3, 0);
        self.a.addi(2, 2, 4); self.a.addi(3, 3, 4); self.a.addi(4, 4, -1);
        self.zero(ccz::NE, 4, again); self.bind(done);
    }
    // Radio state can be larger than one watchdog interval. Feed on entry,
    // exit and every 128 words: <=128*9 loop instructions plus feed/setup.
    // Keep the original copy path unchanged for radio-off image bytes.
    fn copy_fed(&mut self, src: u32, dst: u32, words: u32) {
        self.address(2, src); self.address(3, dst); self.imm(4, words);
        self.copy_fed_words();
    }
    // a2/a3 advance to the first trailing byte. a6 survives for the tail count.
    fn copy_fed_words(&mut self) {
        self.feed_now();
        let done = self.label(); let again = self.label();
        self.zero(ccz::EQ, 4, done); self.bind(again);
        self.a.l32i(5, 2, 0); self.a.s32i(5, 3, 0);
        self.a.addi(2, 2, 4); self.a.addi(3, 3, 4); self.a.addi(4, 4, -1);
        let next = self.label(); self.a.extui(5, 4, 0, 7);
        self.zero(ccz::NE, 5, next); self.feed_now(); self.bind(next);
        self.zero(ccz::NE, 4, again); self.bind(done); self.feed_now();
    }
    // Saturating u64 add (lo,hi) += (x,y); scratch must be distinct.
    fn add64(&mut self, lo: Reg, hi: Reg, x: Reg, y: Reg, tmp: Reg, carry: Reg) {
        let overflow = self.label(); let done = self.label();
        self.a.mov_n(tmp, lo); self.a.add_n(lo, lo, x);
        self.imm(carry, 0); self.branch(cc::GEU, lo, tmp, done);
        self.imm(carry, 1); self.bind(done);
        self.a.mov_n(tmp, hi); self.a.add_n(hi, hi, y);
        self.branch(cc::LTU, hi, tmp, overflow);
        self.a.mov_n(tmp, hi); self.a.add_n(hi, hi, carry);
        let finished = self.label(); self.branch(cc::GEU, hi, tmp, finished);
        self.bind(overflow); self.imm(lo, u32::MAX); self.imm(hi, u32::MAX); self.bind(finished);
    }
}

pub(crate) fn build(m: &Module<'_>, l: &SramLayout, pins: &[Pin], panel: Option<&Panel>, i2c: Option<&I2cBus>,
    motion: Option<(Motion, u8)>, touch: Option<(Touch, u8)>, profile: Option<&Radio>, radio_kernel: Option<&Compiled>,
    analog_check: bool, link: &Link) -> Runtime {
    let mut routines = Vec::new(); let mut symbols = Symbols::default(); let mut port_addresses = Vec::new();
    let mut radio_data = None;
    let draw_ports: Vec<_> = [ports::PRESENT, ports::FILL_RECT].into_iter()
        .filter(|&port| crate::emit::uses_port(m, port)).collect();
    let buffer_ports: Vec<_> = [ports::COPY, ports::FILL].into_iter()
        .filter(|&port| crate::emit::uses_port(m, port)).collect();
    let eth_ports: Vec<_> = [ports::ETH_SEND, ports::ETH_RECV, ports::ETH_MAC].into_iter()
        .filter(|&port| crate::emit::uses_port(m, port)).collect();
    let net_ports: Vec<_> = (ports::NET_LINK..=ports::NET_KIND)
        .filter(|&port| crate::emit::uses_port(m, port)).collect();
    let motion_ports: Vec<_> = [ports::ACCEL, ports::GYRO].into_iter()
        .filter(|&port| crate::emit::uses_port(m, port)).collect();
    let touch_ports: Vec<_> = [ports::TOUCH_X, ports::TOUCH_Y, ports::TOUCH_DOWN].into_iter()
        .filter(|&port| crate::emit::uses_port(m, port)).collect();
    let speaker_ports: Vec<_> = (ports::PLAY..=ports::SPEAKER_ON)
        .filter(|&port| crate::emit::uses_port(m, port)).collect();
    let mic_ports: Vec<_> = (ports::RECORD..=ports::MIC_REWIND)
        .filter(|&port| crate::emit::uses_port(m, port)).collect();
    let probe_ports: Vec<_> = [ports::PROBE].into_iter()
        .filter(|&port| crate::emit::uses_port(m, port)).collect();
    let timing = i2c.map(|bus| bus.timing().expect("verified I2C timing"));
    let kernel_module = radio_kernel.map(|compiled|
        fab_elec_mcu_rt::module::load_kernel(&compiled.module, &crate::chip::profile(), &RADIO_KERNEL_LINES)
            .expect("compiled radio kernel"));
    let mut radio_sizes = None;
    let capture = l.mic.is_some();
    let vector_lines = if capture { mic::LINE_BIT } else { 0 };
    let wake_lines = DISPATCHER_LINES & !vector_lines;
    let labels = (if capture { 2048 } else if l.speaker.is_some() { 1536 } else { 1024 })
        + if l.net.is_some() { 512 } else { 0 };
    let bytes = assemble(&mut Asm::new(labels, 11), |a| {
        // core-isa.h:572–580. The boot stage enters at INIT; 0..0x300 is the
        // window-vector block (zero padding: the runtime runs with WOE clear and
        // CALL0 only), then these three vectors; all other bytes are padding.
        a.base.reserve(0x300);
        a.wsr(0, EXCSAVE1);
        a.op24(enc::rsr(0, 3, 0, EXCCAUSE, 0)); // RSR: ISA Table 221 p.668.
        // BNEI a0,4,spin: Table 15 p.64, Table 252 p.680.
        // MOVI, WSR, RSR, RFE = 12 bytes after BNEI; PC bias = 4.
        if capture {
            a.op24(enc::bri8(6, 4, 0, 1, 2, 2)); // BNEI skips the 3-byte J to the spin.
            a.j_label(MIC_IRQ);
        } else {
            a.op24(enc::bri8(6, 4, 0, 1, 2, 11));
            a.movi(0, 0); a.wsr(0, INTENABLE);
            a.op24(enc::rsr(0, 3, 0, EXCSAVE1, 0));
            a.op24(enc::rrr(0, 0, 0, 3, 0, 0)); // RFE: Tables 201,207,211 pp.662–665.
        }
        a.j(-4);
        a.base.reserve(0x340 - a.base.offset()); a.j(-4);
        a.base.reserve(0x3c0 - a.base.offset()); a.j(-4);
        a.base.reserve(0x400 - a.base.offset());
        let mut c = Code { a, next: if l.net.is_some() { NET_FLUSH + 1 } else if capture { RADIO_IRQ + 1 } else if l.speaker.is_some() { SPEAKER_CLOCK + 1 } else if profile.is_some() { RADIO_EVENT_FREE + 1 } else { LINK_STATS + 1 },
            radio: profile.is_some(), mic: capture, t0: l.t0, rwdt_due: l.rwdt_due, symbols: Symbols::default(), routines: vec![
            RuntimeRoutine { name: "kernel_vector", start: KERNEL_VECTOR, end: KERNEL_VECTOR + if capture { 15 } else { 24 } },
            RuntimeRoutine { name: "user_vector", start: USER_VECTOR, end: USER_VECTOR + 3 },
            RuntimeRoutine { name: "double_vector", start: DOUBLE_VECTOR, end: DOUBLE_VECTOR + 3 },
            RuntimeRoutine { name: "init", start: INIT, end: 0 },
        ] };
        c.a.literal_table();
        // init's raise and the analog check call the ROM's windowed code through one shim (placed after the routines)
        let rom_shim = c.label();
        init(&mut c, m, l, pins, panel, i2c, timing.as_ref(), rom_shim, analog_check, link);
        dispatch(&mut c, l, motion, touch, timing.as_ref(), link);
        port_routines(&mut c, l, link);
        for &port in &draw_ports { draw_port(&mut c, l, panel, link, port); }
        for &port in &buffer_ports { buffer_port(&mut c, l, link, port); }
        for &port in &motion_ports { motion_port(&mut c, l, port); }
        for &port in &touch_ports { touch_port(&mut c, l, port); }
        if let Some(s) = &l.speaker {
            c.routine(SPEAKER_CLOCK, "speaker_clock"); sample_clock(&mut c, l, s.now);
            speaker::service(&mut c, s, l.mic.as_ref());
            for &port in &speaker_ports { speaker::port(&mut c, l, s, link, timing.as_ref().expect("speaker I2C timing"), port); }
        }
        if let Some(mic) = &l.mic {
            mic::irq(&mut c, l, mic);
            mic::service(&mut c, l, mic);
            for &port in &mic_ports { mic::port(&mut c, l, mic, link, port); }
        }
        for &port in &probe_ports { probe_port(&mut c, l, port); }
        if let Some(e) = &l.link {
            if let Some(n) = &l.net {
                net::flush(&mut c, l, n, e, link);
                for &port in &net_ports { net::port(&mut c, l, n, link, port); }
            }
            if let Some(eth) = &l.eth {
                for &port in &eth_ports { eth_port(&mut c, l, eth, e, link, port); }
            }
            receive(&mut c, e);
            receive_byte(&mut c, l, e, link);
            if let Some(eth) = &l.eth { credit(&mut c, l, eth); }
            stall(&mut c, l, e);
            rehello(&mut c, l, e);
            link_idle(&mut c, l, e);
            link_stats(&mut c, l, e);
        }
        let mut started = 0;
        if let Some(r) = &l.radio {
            let profile = profile.expect("radio profile");
            radio_record(&mut c, l, r); radio_reports(&mut c, r);
            radio_events(&mut c, r, kernel_module.as_ref().unwrap(), &profile.island);
            island_start(&mut c, &profile.island, r, link);
            started = radio_begin(&mut c, r, link);
            radio_phy_table(&mut c, r, &profile.island, link);
            radio_restart(&mut c, r, link, profile.restart);
        }
        stamp(&mut c, l);
        event(&mut c, l);
        wire(&mut c, l, link);
        drain(&mut c, l);
        loss(&mut c, l);
        if c.radio { loss_n(&mut c, l); }
        crc32(&mut c, link);
        let end = c.here(); c.routines.last_mut().unwrap().end = end;
        c.a.base.align(4);
        // The ROM's I2C calls are windowed code: every image carries the radio's CALL0-to-windowed
        // shim and its window-vector block at VECBASE+0, both as the radio images place them.
        let code = radio::shim_code(c.here());
        radio_blob(&mut c, "rom_shim", &code.bytes, &[(rom_shim, code.call)]);
        c.a.base.patch(0, &radio::window_block());
        if let Some(profile) = profile {
            let r = l.radio.as_ref().expect("radio SRAM");
            let shim_base = c.here();
            let shim = radio::shim_code(c.here());
            radio_blob(&mut c, "radio_shim", &shim.bytes, &[]);
            let sched = radio::sched_code(c.here(), &radio::SchedLayout {
                current: r.current, cursor: r.cursor, task_count: r.task_count, tasks: r.tasks,
                isr_count: r.isr_count, isrs: r.isrs, stray: r.stray,
                dispatcher: r.dispatcher, wake_lines, vector_lines, shim: shim.call,
                // The first build uses zeros for the three forward references. mov_reg_i32
                // always emits one L32R and one literal, and only the final build ships.
                marks: Some(radio::SchedMarks {
                    isr_start: r.isr_start, isr_max_span: r.isr_max_span, isr_max_entry: r.isr_max_entry,
                    crit_depth: link.radio_crit_depth, report: link.radio_report, threshold: link.radio_crit_span_report_cycles,
                }),
            });
            radio_blob(&mut c, "radio_sched", &sched.bytes, &[(RADIO_BLOCK, sched.block)]);
            let kernel = radio::kernel_code(c.here(), SLOT_BASE, &radio::RadioLayout {
                current: r.current, irq_stack_top: r.irq_stack.top, pick: sched.pick,
                irq_stack_base: r.irq_stack.base, tasks: r.tasks, task_count: r.task_count,
                dispatcher: r.dispatcher, idle: r.idle, isr_count: r.isr_count,
                wake_lines, vector_lines, shim_start: shim_base, shim_end: shim_base + shim.bytes.len() as u32,
                radio_state: r.radio_state, island_guard: r.island_guard, island_status: r.island_status,
                fault: r.fault, trace_head: r.trace_head, trace: r.trace, trace_entries: profile.trace_entries,
            });
            let mut kernel_entries = vec![(RADIO_STOP, kernel.radio_stop)];
            if capture { kernel_entries.push((RADIO_IRQ, kernel.irq_entry)); }
            kernel_entries.sort_unstable_by_key(|&(_, at)| at);
            radio_blob(&mut c, "radio_kernel", &kernel.bytes, &kernel_entries);
            let boot = radio::boot_code(c.here(), l, profile, sched.idle, kernel.task_exit);
            radio_blob(&mut c, "radio_boot", &boot.bytes, &[(RADIO_START, boot.start)]);
            // The kernel compiled against CODE_REGION = SLOT_BASE >>> 30 (image.rs); RETW keeps the
            // caller's PC bits 31:30, so task_exit and the ISR shim must live in that region too.
            assert!(kernel.task_exit >> 30 == SLOT_BASE >> 30 && shim.call >> 30 == SLOT_BASE >> 30,
                "task_exit and the ISR shim share the kernel's CODE_REGION");
            let built = radio_objects(c.here(), l, &profile.island, kernel_module.as_ref().unwrap(), kernel.task_exit, link);
            radio_blob(&mut c, "radio_objects", &built.objects.bytes, &[]);
            // At INTLEVEL 15 the dispatcher has no live IRQ frame. Both sequential
            // CALL0 gates can use the aligned IRQ stack above its guard.
            for gate in built.gates.gates.iter().filter(|g| matches!(g.name.as_str(), "free" | "event_take")) {
                assert!(gate.stack_bytes <= r.irq_stack.top - r.irq_stack.base - 4,
                    "{} needs {} bytes above the IRQ stack guard", gate.name, gate.stack_bytes);
            }
            let mut drain_gates = [
                (RADIO_EVENT_FREE, built.gate("free").call), (RADIO_EVENT_TAKE, built.gate("event_take").call),
            ];
            drain_gates.sort_unstable_by_key(|&(_, address)| address);
            radio_blob(&mut c, "radio_gates", &built.gates.bytes, &drain_gates);
            let report = built.objects.symbols.iter().find(|s| s.0 == "report").expect("kernel report").1;
            let crit_depth = built.objects.crit_depth.expect("kernel crit_depth");
            let phy_calibrated = built.objects.phy_calibrated.expect("kernel phy_calibrated");
            // The assembler's counting traversal still has a provisional objects base.
            if link.slot_bytes != 0 && c.a.base.pass() == Pass::Emit {
                assert_eq!((link.radio_report, link.radio_crit_depth, link.radio_phy_calibrated, link.radio_crit_span_report_cycles),
                    (report, crit_depth, phy_calibrated, built.threshold), "linked scheduler marks must match the emitted kernel");
            }
            use radio::adapter::{adapter, OSI_FIELDS};
            use radio::kernel::gates::EXIT_LEVEL_EXPORTS;
            let mut imports = Vec::new();
            for &name in OSI_FIELDS.iter().flat_map(|(_, row)| row.kernel_exports()) {
                if imports.iter().any(|g: &radio::Gate<'_>| g.name == name) { continue; }
                imports.push(built.gate(name));
            }
            // Each emitted gate is charged with its own longest masked thunk path.
            let masked_gate_insns = built.gates.gates.iter().map(|g| {
                g.max_insns.checked_add(radio::thunk::masked_insns(g.blocks,
                    EXIT_LEVEL_EXPORTS.contains(&g.name.as_str()), profile.trace_entries != 0))
                    .expect("masked gate bound overflow")
            }).max().unwrap_or(0);
            let trace = (profile.trace_entries != 0).then_some(kernel.trace_record);
            let blocking = Some(radio::Blocking {
                current: r.current, block: sched.block, crit_depth: built.objects.crit_depth,
                kernel_fault: built.objects.symbols.iter().find(|s| s.0 == "kernel_fault").unwrap().1,
            });
            let thunks = radio::exports(c.here(), &[], &imports, trace, blocking);
            radio_blob(&mut c, "radio_thunks", &thunks.bytes, &[]);
            let adapter = adapter(c.here(), |name| {
                thunks.thunks[imports.iter().position(|g| g.name == name).expect("OSI thunk")]
            }, |name| profile.island.glue[name]);
            radio_blob(&mut c, "radio_adapter", &adapter.bytes, &[]);
            // The slot copies the optional config with the OSI table in a single fed loop.
            let mut table = adapter.table.to_vec();
            if r.wifi_config.is_some() { table.extend(radio::start_glue::config(r.osi_table, &adapter.crypto)); }
            let island = &profile.island;
            let gates: Vec<_> = island.gate_names.iter().map(|name| built.gate(name)).collect();
            let roots: Vec<_> = island.entries.iter().map(|(name, _)| name.as_str()).collect();
            let island_exports = radio::exports(c.here(), &roots, &gates, trace, blocking);
            assert_eq!(island_exports.abi_hash, island.header.abi_hash, "island thunk ABI");
            radio_blob(&mut c, "radio_island_thunks", &island_exports.bytes, &[]);
            let sizes = [shim.bytes.len(), sched.bytes.len(), kernel.bytes.len(), boot.bytes.len(),
                built.objects.bytes.len(), built.gates.bytes.len(), thunks.bytes.len(), adapter.bytes.len(), island_exports.bytes.len()];
            if let Some(previous) = radio_sizes { assert_eq!(sizes, previous, "radio blob sizes must not depend on base"); }
            radio_sizes = Some(sizes);
            c.a.base.patch(0, &radio::window_block());
            let irq = if capture { SLOT_BASE + c.a.base.label_offset(MIC_IRQ).unwrap_or(0) } else { kernel.irq_entry };
            let user = radio::user_vector(SLOT_BASE, irq, kernel.fault_entry);
            c.a.base.patch(0x340, &user);
            c.routines.iter_mut().find(|r| r.name == "user_vector").unwrap().end = USER_VECTOR + user.len() as u32;
            c.routines.insert(0, RuntimeRoutine { name: "window_vectors", start: SLOT_BASE, end: KERNEL_VECTOR });
            c.symbols.radio = Some(RadioSymbols {
                blobs: c.routines.iter().filter(|r| r.start >= shim_base).cloned().collect(),
                osi_table: r.osi_table,
                thunks: imports.iter().zip(&thunks.thunks).map(|(g, &at)| (g.name.to_owned(), at)).collect(),
                island_tables: IslandTables::default(),
                island_status: r.island_status,
                island_thunks: island.gate_names.iter().cloned().zip(island_exports.thunks.iter().copied()).collect(),
                kernel_regions: built.regions.into_iter().map(|r| (r.base..r.base + r.len, r.writable, r.kind)).collect(),
                gate_reports: built.gates.gates, kernel_map: built.objects.map,
                kernel_symbols: built.objects.symbols, adapter_symbols: adapter.symbols,
                isr_start: r.isr_start, isr_max_span: r.isr_max_span, isr_max_entry: r.isr_max_entry,
                crit_depth, phy_calibrated, report,
                crit_span_report_cycles: built.threshold, masked_gate_insns,
                crit_span_report: built.objects.crit_span_report.expect("kernel crit_span_report"),
                task_exit_link: built.objects.task_exit_link.expect("kernel task_exit_link"),
                phy_init_data: built.objects.phy_init_data.expect("kernel phy_init_data"),
                irq_entry: kernel.irq_entry, restore: kernel.restore,
                stack_fault: kernel.stack_fault, pick: sched.pick, block: sched.block, idle: sched.idle,
                fault_entry: kernel.fault_entry, radio_stop: kernel.radio_stop, fatal: kernel.fatal, trace_record: kernel.trace_record,
                restart: SLOT_BASE + c.a.base.label_offset(RADIO_RESTART).unwrap(), started,
                restart_max_insns: restart_bound(r, profile, link.island_pages, boot.feeds),
                no_task: sched.no_task, shim: shim.call, start: boot.start, task_exit: kernel.task_exit,
                block_calls: [c.symbols.waiti, c.symbols.idle_waiti] });
            // Keep the aggregate for image layout, and give Fab an exact PC for each named
            // crypto ILL in the source map. These labels add no bytes to the image.
            let blob = c.routines.iter().position(|b| b.name == "radio_adapter").unwrap();
            c.routines[blob].end = adapter.crypto[0];
            c.routines.splice(blob + 1..blob + 1, radio::adapter::CRYPTO_STUB_NAMES.into_iter()
                .zip(adapter.crypto).map(|(name, start)| RuntimeRoutine { name, start, end: start + 9 }));
            radio_data = Some(RadioData { state_init: built.state_init, shared: built.shared,
                table, island_gates: island_exports.thunks });
        }
        port_addresses = (0..8).map(|p| (p as u16, SLOT_BASE + c.a.base.label_offset(PORT + p).unwrap())).collect();
        port_addresses.extend(draw_ports.iter().chain(&buffer_ports).chain(&eth_ports).chain(&net_ports).chain(&motion_ports).chain(&touch_ports).chain(&speaker_ports).chain(&mic_ports).chain(&probe_ports).map(|&p|
            (p, SLOT_BASE + c.a.base.label_offset(PORT + p as usize).unwrap())));
        symbols = c.symbols; routines = c.routines;
    });
    Runtime { bytes, ports: port_addresses, routines, symbols, radio_data }
}

struct RadioObjects {
    objects: radio::kernel::KernelCode,
    gates: radio::kernel::gates::Gates,
    regions: Vec<radio::kernel::KernelRegion>,
    state_init: Vec<u32>,
    shared: Vec<u32>,
    threshold: u32,
}

impl RadioObjects {
    fn gate<'a>(&self, name: &'a str) -> radio::Gate<'a> {
        let export = self.objects.entries.iter().position(|e| e.name == name).expect("kernel export");
        let crate::EntryKind::Export { params, .. } = &self.objects.entries[export].kind else { unreachable!() };
        let gate = self.gates.gates.iter().find(|g| g.name == name).expect("kernel gate");
        radio::Gate { name, call: gate.address, blocks: gate.blocks,
            exit_level: radio::kernel::gates::EXIT_LEVEL_EXPORTS.contains(&name).then(||
                u8::try_from(params.len().checked_sub(3).expect("three hidden gate arguments")).unwrap()),
            export: export.try_into().expect("kernel export index") }
    }
}

fn radio_var(m: &Module<'_>, r: &crate::RadioSram, name: &str) -> u32 {
    let var = m.vars().iter().find(|v| m.string(v.name) == name).expect(name);
    r.kernel_state + 4 * var.offset
}

fn radio_objects(base: u32, l: &SramLayout, island: &IslandBinding, m: &Module<'_>, task_exit: u32, link: &Link) -> RadioObjects {
    use radio::kernel::{self, gates::{gates, EntryRegs}, KernelOptions, KernelRegion};
    let r = l.radio.as_ref().unwrap();
    let mut regions = vec![
        // Kernel-created objects/stacks and boot task stacks are all here.
        KernelRegion { base: r.start, len: r.end - r.start, writable: true, kind: RegionKind::Kernel },
        KernelRegion { base: kernel::SYSCON_WIFI_CLK_EN, len: kernel::SYSCON_WIFI_RST_EN - kernel::SYSCON_WIFI_CLK_EN + 4, writable: true, kind: RegionKind::Device },
        KernelRegion { base: kernel::INTR_MATRIX, len: 4 * kernel::WIFI_SOURCES, writable: true, kind: RegionKind::Device },
        KernelRegion { base: kernel::EFUSE_MAC_0, len: kernel::EFUSE_MAC_1 - kernel::EFUSE_MAC_0 + 4, writable: false, kind: RegionKind::Device },
        KernelRegion { base: kernel::RTC_STORE1, len: 4, writable: false, kind: RegionKind::Device },
        KernelRegion { base: kernel::RNG_DATA, len: 4, writable: false, kind: RegionKind::Device },
    ];
    // P ports can also read and write island DATA through BSS and read the flash image's DROM mapping.
    let mut ram = island.segments.iter().filter(|s| matches!(s.kind, SegmentKind::Data | SegmentKind::Bss));
    if let Some(first) = ram.next() {
        let last = ram.next_back().unwrap_or(first);
        regions.push(KernelRegion { base: first.address, len: last.address - first.address + last.bytes, writable: true, kind: RegionKind::Pack });
    }
    regions.push(KernelRegion { base: 0x3c00_0000 + island.header.mmu_page * crate::PAGE,
        len: island.header.image_size, writable: false, kind: RegionKind::Pack });
    let options = KernelOptions { base, state: r.kernel_state, t0: l.t0, reports: Some(r.reports), stop_pending: Some(r.stop_pending), regions };
    // K reaches only radio SRAM, P also reaches the island ranges, and D reaches only the five MMIO ranges.
    // now64 checks its destination as K. Its fixed SYSTIMER access and the special-register ports bypass the walk.
    assert_eq!(m.state_words(), r.kernel_state_words, "compiled kernel state must match its SRAM allocation");
    let objects = kernel::build_kernel(m, &options).expect("radio kernel emits");
    let routines = gates((base + objects.bytes.len() as u32 + 3) & !3, &objects,
        EntryRegs { state: r.kernel_state, shared: link.radio_shared });
    let largest = routines.gates.iter().map(|g| g.max_insns).max().expect("kernel exports");
    let threshold = u32::try_from(largest).expect("critical span report threshold exceeds u32 cycles");
    let mut state_init = m.state_init().to_vec();
    for (slot, value) in [
        (objects.crit_span_report.expect("kernel crit_span_report"), threshold),
        (objects.task_exit_link.expect("kernel task_exit_link"), radio::windowed_exit_link(task_exit)),
        (objects.phy_init_data.expect("kernel phy_init_data"), link.phy_init_data),
    ] {
        state_init[((slot - r.kernel_state) / 4) as usize] = value;
    }
    RadioObjects { objects, gates: routines, regions: options.regions, state_init, shared: m.shared().to_vec(), threshold }
}

// Each producer is assembled at c.here(), including its own literal table.
// Bind entries in address order so forward CALL0 sites can reach the blob.
fn radio_blob(c: &mut Code<'_>, name: &'static str, bytes: &[u8], entries: &[(usize, u32)]) {
    let start = c.here();
    let offset = c.a.base.offset();
    for &(label, address) in entries {
        let gap = address - c.here();
        c.a.base.reserve(gap); c.bind(label);
    }
    let remaining = bytes.len() as u32 - (c.here() - start);
    c.a.base.reserve(remaining);
    c.a.base.patch(offset, bytes);
    c.a.base.align(4);
    c.routines.push(RuntimeRoutine { name, start, end: c.here() });
}

/// ERADIO-owned fault transport, kept beside the radio wiring because it calls
/// the runtime's own WIRE. Refusals remain pending and never contribute to LOST.
fn radio_record(c: &mut Code<'_>, l: &SramLayout, r: &crate::image::RadioSram) {
    assert_eq!(l.body & 3, 0, "radio record body is word aligned");
    c.routine(RADIO_RECORD, "radio_record"); c.store(0, r.report_return);
    let done = c.label(); c.load(2, r.fault); c.zero(ccz::EQ, 2, done);
    // Earlier reports, including this run's start marker, and pending ring loss drain first.
    // Sleep holds INTLEVEL 15, so one check orders both frames against every producer.
    c.address(6, r.reports); c.a.l32i(2, 6, 0); c.a.l32i(3, 6, 4);
    c.branch(cc::NE, 2, 3, done); c.a.l32i(2, 6, 8); c.zero(ccz::NE, 2, done);
    // Unroll the two-frame budget: WIRE clobbers a2..a11, so reload the index
    // after it returns. No loop counter or dispatcher register needs saving.
    for _ in 0..2 {
        c.load(2, r.report_next); c.imm(3, radio::FAULT_FRAMES); c.branch(cc::GEU, 2, 3, done);
        c.address(3, l.body); c.a.s16i(2, 3, 6);
        c.imm(4, u32::from(watch::RADIO_RECORD)); c.a.s8i(4, 3, 0);
        c.a.op24(enc::slli(5, 2, 4)); c.address(6, r.fault); c.a.add_n(5, 6, 5);
        for word in 0..4 { c.a.l32i(6, 5, word * 4); c.a.s32i(6, 3, 8 + word * 4); }
        c.imm(2, 24); c.load(4, l.stamp); c.call(WIRE);
        c.zero(ccz::EQ, 2, done);
        c.load(2, r.report_next); c.a.addi(2, 2, 1); c.store(2, r.report_next);
    }
    c.bind(done); c.load(0, r.report_return); c.a.ret_n();
}

// CALL0 from sleep at INTLEVEL 15. No producer can append during the drain.
fn radio_reports(c: &mut Code<'_>, r: &crate::image::RadioSram) {
    c.routine(RADIO_REPORTS, "radio_reports"); c.store(0, r.reports_return);
    let losses = c.label(); let done = c.label();
    // EVENT clobbers a2..a11. Unroll the fixed bound and reload head/base
    // after each call, leaving the dispatcher's a12..a15 untouched.
    for _ in 0..radio::REPORT_RING_ENTRIES {
        c.address(6, r.reports); c.a.l32i(2, 6, 0); c.a.l32i(3, 6, 4);
        c.branch(cc::EQ, 2, 3, losses);
        c.a.extui(7, 2, 0, radio::REPORT_RING_ENTRIES.ilog2() as u8);
        c.a.addx2(7, 7, 7); c.a.addx4(6, 7, 6);
        c.a.l32i(3, 6, 12); c.a.l32i(4, 6, 16); c.a.l32i(5, 6, 20);
        c.imm(2, u32::from(watch::FAULT)); c.call(EVENT);
        // EVENT counts its own refused frames as lost; consume this entry
        // whether it was accepted or refused, without counting it twice.
        c.load(2, r.reports); c.a.addi(2, 2, 1); c.store(2, r.reports);
    }
    c.bind(losses); c.load(5, r.reports + 8); c.zero(ccz::EQ, 5, done);
    c.call(LOSS_N); c.set(r.reports + 8, 0);
    c.bind(done); c.load(0, r.reports_return); c.a.ret_n();
}

// Only sleep calls this routine. INTLEVEL 15 serializes the list with event_commit
// and leaves the IRQ stack available until the dispatcher restores its own SP.
fn radio_events(c: &mut Code<'_>, r: &crate::RadioSram, m: &Module<'_>, island: &IslandBinding) {
    c.routine(RADIO_EVENTS, "radio_events");
    let done = c.label(); let restore = c.label(); let next = c.label();
    c.load(2, radio_var(m, r, "event_head")); c.zero(ccz::EQ, 2, done);
    c.a.addi(1, 1, -16); c.a.s32i(0, 1, 0); c.a.s32i(12, 1, 4); c.a.mov_n(12, 1);
    c.address(1, r.irq_stack.top);
    c.bind(next); c.feed_now(); c.call(RADIO_EVENT_TAKE); c.feed_now();
    c.zero(ccz::EQ, 2, restore); c.a.s32i(2, 12, 8);
    if let Some(&word) = island.glue.get("WIFI_EVENT") {
        // The C object WIFI_EVENT contains the relocated base pointer.
        c.a.l32i(5, 2, 8); c.a.l32i(7, 2, 4); c.load(8, word);
        c.a.xor(7, 7, 8); c.imm(8, u32::MAX); c.a.movnez(5, 8, 7);
    } else { c.imm(5, u32::MAX); }
    c.imm(2, u32::from(watch::FAULT)); c.imm(3, u32::from(watch::FAULT_RADIO_EVENT));
    c.imm(4, u32::from(watch::SITE_RADIO_SCHEDULER)); c.call(EVENT);
    c.feed_now(); c.a.l32i(2, 12, 8); c.call(RADIO_EVENT_FREE); c.jump(next);
    c.bind(restore); c.a.mov_n(1, 12);
    c.a.l32i(0, 1, 0); c.a.l32i(12, 1, 4); c.a.addi(1, 1, 16);
    c.bind(done); c.a.ret_n();
}

// Only slot descriptors supply copy offsets, SRAM destinations and lengths.
fn load_island_segment(c: &mut Code<'_>, descriptor: u32, drom: u32, bss: bool) {
    c.address(8, descriptor);
    if bss {
        c.a.l32i(3, 8, 0); c.a.l32i(4, 8, 4);
    } else {
        c.a.l32i(2, 8, 0); c.address(5, drom); c.a.add_n(2, 2, 5);
        c.a.l32i(3, 8, 4); c.a.l32i(4, 8, 8);
    }
    c.a.extui(6, 4, 0, 2); c.a.op24(enc::srli(4, 4, 2));
    if bss {
        c.feed_now(); c.imm(5, 0);
        let tail = c.label(); let clear = c.label();
        c.zero(ccz::EQ, 4, tail); c.bind(clear);
        c.a.s32i(5, 3, 0); c.a.addi(3, 3, 4); c.a.addi(4, 4, -1);
        c.a.extui(7, 4, 0, 8); c.zero(ccz::NE, 7, clear);
        c.feed_now(); c.zero(ccz::NE, 4, clear); c.bind(tail);
    } else {
        c.copy_fed_words();
    }
    let done = c.label(); let byte = c.label();
    c.zero(ccz::EQ, 6, done); c.bind(byte);
    if !bss { c.a.l8ui(5, 2, 0); c.a.addi(2, 2, 1); }
    c.a.s8i(5, 3, 0); c.a.addi(3, 3, 1); c.a.addi(6, 6, -1);
    c.zero(ccz::NE, 6, byte); c.bind(done);
}

/// The power-on wait (phy_init.c's 10 us) in cycles of the image's CPU clock.
const POWER_ON_WAIT: u32 = crate::CPU_HZ / 100_000;
/// The longest stretch of that wait between two feeds: the whole wait at 160 MHz, which with its
/// setup, loop exit and the stores up to the next feed fits MAX_OP_INSNS.
const POWER_ON_STRETCH: u32 = 1600;
/// The wait in equal stretches, each after a feed: (stretches, cycles each). One stretch of
/// 1,600 at 160 MHz, two of 1,200 at 240 MHz.
const POWER_ON_STRETCHES: (u32, u32) = {
    let stretches = POWER_ON_WAIT.div_ceil(POWER_ON_STRETCH);
    (stretches, POWER_ON_WAIT.div_ceil(stretches))
};
/// Each stretch after the first adds its masked feed (10 instructions), the wait's setup (2)
/// and its last turn (3).
const POWER_ON_STRETCH_EXTRA: u64 = 15;

// ESP-IDF v6.1.1 components/esp_phy/src/phy_init.c:427–447, esp_wifi_bt_power_domain_on.
fn radio_power_on(c: &mut Code<'_>) {
    use radio::kernel::{CLK_COMMON, MODEM_RESET_FIELD, RTC_CNTL_DIG_ISO, RTC_CNTL_DIG_PWC,
        SYSCON_WIFI_CLK_EN, SYSCON_WIFI_RST_EN, WIFI_FORCE_ISO, WIFI_FORCE_PD};

    bits(c, RTC_CNTL_DIG_PWC, WIFI_FORCE_PD, 0);
    // The spin retires at most one instruction per cycle, fewer when the branch stalls, so
    // no stretch plus its setup and loop exit passes MAX_OP_INSNS at any CPU_HZ.
    let (stretches, cycles) = POWER_ON_STRETCHES;
    for _ in 0..stretches {
        c.feed_now();
        c.imm(4, cycles);
        c.a.op24(enc::rsr(0, 3, 0, CCOUNT, 2));
        let wait = c.label(); c.bind(wait);
        c.a.op24(enc::rsr(0, 3, 0, CCOUNT, 3)); c.a.sub(3, 3, 2);
        c.branch(cc::LTU, 3, 4, wait);
    }

    bits(c, SYSCON_WIFI_CLK_EN, 0, CLK_COMMON);
    bits(c, SYSCON_WIFI_RST_EN, 0, MODEM_RESET_FIELD);
    bits(c, SYSCON_WIFI_RST_EN, MODEM_RESET_FIELD, 0);
    bits(c, RTC_CNTL_DIG_ISO, WIFI_FORCE_ISO, 0);
    bits(c, SYSCON_WIFI_CLK_EN, CLK_COMMON, 0);
}

// CALL0. The caller holds INTLEVEL 15. Feeds the watchdog itself, preserves PS and
// a1, clobbers a2..a11, and returns through a0. The report port and feeds preserve
// a8, which holds the return address across the report call.
fn island_start(c: &mut Code<'_>, island: &IslandBinding, r: &crate::RadioSram, link: &Link) {
    c.routine(ISLAND_START, "island_start");
    c.copy_fed(link.radio_state_init, r.kernel_state, r.kernel_state_words);
    c.copy_fed(link.radio_table_init, r.osi_table, r.osi_config_words());
    let h = &island.header;
    let drom = 0x3c00_0000 + h.mmu_page * crate::PAGE;
    let pages = c.label();
    c.address(6, crate::MMU_TABLE + 4 * h.mmu_page);
    c.imm(7, link.island_flash_page); c.imm(9, link.island_pages);
    crate::map_flash_pages(c.a, pages);

    let absent = c.label(); let mismatch = c.label(); let report = c.label();
    c.address(2, drom); c.address(3, link.island_tables.header);
    c.a.l32i(4, 2, 0); c.a.l32i(5, 3, 0); c.branch(cc::NE, 4, 5, absent);
    c.imm(6, fab_elec_mcu_island::Header::SIZE / 4 - 1);
    let compare = c.label(); c.bind(compare);
    c.a.l32i(4, 2, 4); c.a.l32i(5, 3, 4); c.branch(cc::NE, 4, 5, mismatch);
    c.a.addi(2, 2, 4); c.a.addi(3, 3, 4); c.a.addi(6, 6, -1);
    c.zero(ccz::NE, 6, compare);

    let mut descriptor = link.island_tables.segments;
    for s in &island.segments {
        match s.kind {
            SegmentKind::Iram | SegmentKind::Data => { load_island_segment(c, descriptor, drom, false); descriptor += 12; }
            SegmentKind::Bss => { load_island_segment(c, descriptor, drom, true); descriptor += 8; }
            SegmentKind::Text | SegmentKind::Rodata => {}
        }
    }
    c.copy_fed(link.island_tables.gates, h.gate_table, h.gate_count);
    c.set(r.island_guard, radio::GUARD); c.a.memw(); c.a.isync();
    radio_power_on(c);
    c.set(r.island_status, radio::ISLAND_READY); c.a.ret_n();

    c.bind(absent); c.imm(4, radio::ISLAND_ABSENT); c.jump(report);
    c.bind(mismatch); c.imm(4, radio::ISLAND_MISMATCH);
    c.bind(report); c.store(4, r.island_status);
    // Port 78 appends (code, site, arg) to the report ring under the caller's mask.
    c.imm(2, u32::from(watch::FAULT_RADIO_ISLAND_MISMATCH)); c.load(3, drom + 60);
    c.a.mov_n(8, 0);
    c.imm(10, 0); c.address(11, link.radio_report); c.a.callx0(11);
    c.a.mov_n(0, 8); c.a.ret_n();
}

// CALL0 at INTLEVEL 15, a2 = cause. Keep the packed argument across the drain.
fn radio_begin(c: &mut Code<'_>, r: &crate::RadioSram, link: &Link) -> u32 {
    c.routine(RADIO_BEGIN, "radio_begin");
    let done = c.label(); let pack = c.label();
    c.load(3, r.island_status); c.imm(4, radio::ISLAND_READY); c.branch(cc::NE, 3, 4, done);
    c.store(0, r.restart_return);
    c.load(3, r.generation); c.a.addi(3, 3, 1); c.store(3, r.generation);
    c.load(4, r.consecutive);
    c.imm(5, 255); c.a.minu(4, 4, 5);
    c.zero(ccz::EQ, 2, pack);
    c.a.addi(4, 4, 1); c.a.minu(4, 4, 5); c.store(4, r.consecutive);
    c.bind(pack);
    c.a.extui(3, 3, 0, 16); c.a.extui(2, 2, 0, 8);
    c.a.op24(enc::slli(2, 2, 16)); c.a.op24(enc::slli(4, 4, 24));
    c.a.or(3, 3, 2); c.a.or(3, 3, 4);
    c.a.addi(1, 1, -16); c.a.s32i(3, 1, 0);
    c.call(RADIO_REPORTS);
    // Decision 3: timestamp publication, after every older report has been drained.
    c.call(STAMP); c.store(2, r.started_at); c.store(3, r.started_at + 4);
    c.a.l32i(3, 1, 0); c.a.addi(1, 1, 16);
    c.imm(2, u32::from(watch::FAULT_RADIO_STARTED)); c.imm(10, u32::from(watch::SITE_RADIO_SCHEDULER));
    c.address(11, link.radio_report); c.a.callx0(11);
    let started = c.here();
    c.load(0, r.restart_return); c.bind(done); c.a.ret_n();
    started
}

// CALL0 at INTLEVEL 15. Preserve a0, a1, a12..a15 and PS. Only the first
// completed PHY init supplies the table; both terminal states survive restart.
fn radio_phy_table(c: &mut Code<'_>, r: &crate::RadioSram, island: &IslandBinding, link: &Link) {
    c.routine(RADIO_PHY_TABLE, "radio_phy_table");
    let pending = c.label(); let done = c.label();
    c.load(2, r.phy_table_state); c.zero(ccz::EQ, 2, pending);
    c.bind(done); c.a.ret_n();
    c.bind(pending);
    c.load(2, link.radio_phy_calibrated); c.zero(ccz::EQ, 2, done);
    // phy_init_done sets calibrated before it reports 100. Leave the once-only
    // snapshot pending so the next successful PHY start can supply the table.
    let calibrated = c.label();
    c.load(2, r.radio_state); c.imm(3, radio::RADIO_STOPPED); c.branch(cc::NE, 2, 3, calibrated);
    c.load(2, r.stop_cause); c.imm(3, u32::from(watch::FAULT_RADIO_PHY_INIT_FAILED)); c.branch(cc::EQ, 2, 3, done);
    c.bind(calibrated);
    c.load(3, island.objects[radio::PHY_FUNS_OBJECT].start);
    transfer_phy_table(c, r, link, true);
    c.a.ret_n();
}

// a3 holds the observed ROM pointer for validation and for report 112. Test
// the largest valid start so a high pointer cannot wrap past the upper bound.
fn transfer_phy_table(c: &mut Code<'_>, r: &crate::RadioSram, link: &Link, save: bool) {
    use crate::RadioSram;
    let refused = c.label(); let done = c.label();
    c.a.extui(4, 3, 0, 2); c.zero(ccz::NE, 4, refused);
    c.address(4, RadioSram::ROM_DATA_START); c.branch(cc::LTU, 3, 4, refused);
    c.address(4, RadioSram::ROM_DATA_END - 4 * RadioSram::PHY_TABLE_WORDS); c.branch(cc::LTU, 4, 3, refused);
    if save {
        c.a.mov_n(6, 3); c.a.mov_n(2, 3); c.address(3, r.phy_table);
    } else {
        c.address(2, r.phy_table);
    }
    c.imm(4, RadioSram::PHY_TABLE_WORDS); c.copy_fed_words();
    if save { c.store(6, r.phy_table_at); c.set(r.phy_table_state, 1); }
    c.jump(done);
    c.bind(refused);
    c.a.mov_n(8, 0); // Port 78 preserves a8 across CALLX0.
    c.imm(2, u32::from(watch::FAULT_RADIO_PHY_TABLE_REFUSED)); c.imm(10, u32::from(watch::SITE_RADIO_SCHEDULER));
    c.address(11, link.radio_report); c.a.callx0(11);
    c.set(r.phy_table_state, 2); c.a.mov_n(0, 8);
    c.bind(done);
}

// Convert policy intervals in the emitter, keeping the full SYSTIMER width.
fn restart_ticks(ms: u32) -> u64 { u64::from(ms) * 1000 * u64::from(radio::kernel::TICKS_PER_US) }

// a2:a3 = recognition time. Decide stability once, even if a record takes
// many turns to drain or a bring-up image spends hours waiting for a request.
fn recognize_radio_stop(c: &mut Code<'_>, r: &crate::RadioSram, stable_ms: u32) {
    let seen = c.label(); let no_borrow = c.label(); let stable = c.label(); let remember = c.label();
    c.load(4, r.next_start); c.load(5, r.next_start + 4); c.a.or(6, 4, 5); c.zero(ccz::NE, 6, seen);
    c.load(4, r.started_at); c.load(5, r.started_at + 4); c.a.sub(5, 3, 5);
    c.branch(cc::GEU, 2, 4, no_borrow); c.a.addi(5, 5, -1); c.bind(no_borrow); c.a.sub(4, 2, 4);
    let interval = restart_ticks(stable_ms);
    c.imm(6, (interval >> 32) as u32); c.branch(cc::LTU, 5, 6, remember); c.branch(cc::NE, 5, 6, stable);
    c.imm(6, interval as u32); c.branch(cc::LTU, 4, 6, remember);
    c.bind(stable); c.set(r.consecutive, 0);
    // MAX is beyond any SYSTIMER deadline. It means recognized but not scheduled.
    c.bind(remember); c.imm(4, u32::MAX); c.store(4, r.next_start); c.store(4, r.next_start + 4);
    c.bind(seen);
}

// Decision 2: a2:a3 = now, a7 = cause. An existing deadline is immutable.
// For k >= 2 use base * 2^(k-2). Compare the exponent with the first
// ceiling exponent before shifting; at most 31 doublings are possible.
fn automatic_start_due(c: &mut Code<'_>, r: &crate::RadioSram, policy: crate::image::RestartPolicy,
    start: usize, waiting: usize) {
    let scheduled = c.label(); let ceiling = c.label(); let add = c.label(); let shift = c.label();
    c.load(4, r.next_start); c.load(5, r.next_start + 4);
    c.imm(6, u32::MAX); c.branch(cc::NE, 5, 6, scheduled); c.branch(cc::NE, 4, 6, scheduled);
    c.load(6, r.consecutive); c.zero(ccz::EQ, 6, start); c.a.addi(6, 6, -1);
    let exponent = u32::BITS - (policy.ceiling_ms.div_ceil(policy.base_ms) - 1).leading_zeros();
    c.imm(9, exponent); c.branch(cc::GEU, 6, 9, ceiling);
    let base = restart_ticks(policy.base_ms);
    c.imm(4, base as u32); c.imm(5, (base >> 32) as u32); c.zero(ccz::EQ, 6, add);
    c.bind(shift); c.a.extui(9, 4, 31, 1); c.a.add_n(4, 4, 4); c.a.add_n(5, 5, 5); c.a.or(5, 5, 9);
    c.a.addi(6, 6, -1); c.zero(ccz::NE, 6, shift); c.jump(add);
    let cap = restart_ticks(policy.ceiling_ms);
    c.bind(ceiling); c.imm(4, cap as u32); c.imm(5, (cap >> 32) as u32);
    c.bind(add); c.add64(4, 5, 2, 3, 6, 9);
    c.store(4, r.next_start); c.store(5, r.next_start + 4); c.jump(waiting);
    c.bind(scheduled); c.branch(cc::LTU, 3, 5, waiting); c.branch(cc::NE, 3, 5, start);
    c.branch(cc::LTU, 2, 4, waiting);
}

// CALL0 at INTLEVEL 15 after the masked report drain and PHY snapshot. a2 addresses
// the request. RUNNING with no request returns after two loads, L32R, OR and branch.
// Preserves a1, a12..a15 and PS (UM already set); clobbers a2..a11 and feeds itself.
fn radio_restart(c: &mut Code<'_>, r: &crate::RadioSram, link: &Link, policy: crate::image::RestartPolicy) {
    use radio::kernel::{MODEM_RESET_FIELD, RTC_CNTL_DIG_ISO, RTC_CNTL_DIG_PWC,
        SYSCON_WIFI_CLK_EN, SYSCON_WIFI_RST_EN, WIFI_FORCE_ISO, WIFI_FORCE_PD};
    c.routine(RADIO_RESTART, "radio_restart");
    let active = c.label(); let clear_request = c.label(); let waiting = c.label();
    let classified = c.label(); let request = c.label(); let stopped = c.label();
    let automatic = c.label(); let start = c.label(); let power_off = c.label();
    c.a.l32i(3, 2, 0); c.load(4, r.radio_state); c.a.or(3, 3, 4); c.zero(ccz::NE, 3, active); c.a.ret_n();
    c.bind(active); c.feed_now();
    c.load(3, r.island_status); c.imm(4, radio::ISLAND_READY); c.branch(cc::NE, 3, 4, clear_request);
    c.a.mov_n(8, 0); c.call(STAMP); c.a.mov_n(0, 8); // STAMP preserves a8.
    recognize_radio_stop(c, r, policy.stable_ms);
    c.load(4, r.fault); c.zero(ccz::EQ, 4, classified);
    c.load(4, r.report_next); c.imm(5, radio::FAULT_FRAMES); c.branch(cc::LTU, 4, 5, waiting);
    c.bind(classified);
    // Decision 1: a host request takes precedence over any delay or hold.
    c.load(4, r.request); c.zero(ccz::NE, 4, request);
    c.load(4, r.radio_state); c.imm(5, radio::RADIO_STOPPED); c.branch(cc::EQ, 4, 5, stopped);
    // Decisions 1 and 4: fault recovery starts only after transport, unless held for bring-up.
    if policy.hold_after_fault { c.jump(waiting); } else {
        c.imm(7, u32::from(watch::RADIO_START_CAUSE_FAULT)); c.jump(automatic);
    }
    c.bind(stopped); c.load(7, r.stop_cause);
    // IDF's ESP_ERROR_CHECK on failed Wi-Fi initialization reboots the chip. Here the same
    // failure restarts the radio under its backoff policy. Port 78 only publishes this code.
    for code in std::iter::once(watch::FAULT_RADIO_PHY_INIT_FAILED).chain(watch::FAULT_KERNEL_STOPPING)
        .chain([watch::FAULT_RADIO_START_FAILED]) {
        c.imm(5, u32::from(code)); c.branch(cc::EQ, 7, 5, automatic);
    }
    c.jump(waiting);
    c.bind(request); c.imm(7, u32::from(watch::RADIO_START_CAUSE_REQUEST)); c.jump(start);
    c.bind(automatic); automatic_start_due(c, r, policy, start, waiting);
    c.bind(start); c.store(0, r.restart_return); c.a.addi(1, 1, -16); c.a.s32i(7, 1, 4); c.feed_now();
    c.set(r.next_start, 0); c.set(r.next_start + 4, 0);
    c.load(3, r.radio_state); c.zero(ccz::NE, 3, power_off);
    c.imm(2, 1); c.call(RADIO_STOP);
    c.bind(power_off);
    // ESP-IDF v6.1.1 components/esp_phy/src/phy_init.c, esp_wifi_bt_power_domain_off.
    bits(c, RTC_CNTL_DIG_ISO, 0, WIFI_FORCE_ISO);
    bits(c, RTC_CNTL_DIG_PWC, 0, WIFI_FORCE_PD);
    c.load(2, r.boot_wifi_clk); c.store(2, SYSCON_WIFI_CLK_EN);
    c.load(2, r.boot_wifi_rst); c.imm(3, MODEM_RESET_FIELD); c.a.or(2, 2, 3); c.store(2, SYSCON_WIFI_RST_EN);
    c.copy_fed(r.boot_phy_ram, crate::RadioSram::PHY_ROM_RAM, crate::RadioSram::PHY_ROM_WORDS);
    // Restore the table after the ROM span so the later, post-init snapshot wins on overlap.
    let clear = c.label();
    c.load(3, r.phy_table_state); c.imm(4, 1); c.branch(cc::NE, 3, 4, clear);
    c.load(3, r.phy_table_at); transfer_phy_table(c, r, link, false);
    c.bind(clear);
    for span in r.clear_spans() {
        if span.start < span.end { clear_words(c, span.start, (span.end - span.start) / 4); }
    }
    // End the power-on spin's feed interval before installing task frames.
    c.call(ISLAND_START); c.feed_now(); c.call(RADIO_START); c.feed_now();
    // BEGIN owns the same survivor return slot while it drains and appends.
    c.load(3, r.restart_return); c.a.s32i(3, 1, 0);
    c.a.l32i(2, 1, 4); c.call(RADIO_BEGIN);
    c.a.l32i(0, 1, 0); c.a.addi(1, 1, 16);
    c.feed_now();
    c.bind(clear_request); c.set(r.request, 0); c.a.ret_n();
    c.bind(waiting); c.feed_now(); c.a.ret_n();
}

// From RADIO_RESTART entry to return. Only a PHY table refusal can enter the
// ring between sleep's drain and BEGIN. Each clear/copy/BSS word costs at most 7/9/6
// instructions respectively before feeds; one extra per word covers the periodic
// ten-instruction feed (partial blocks charged below). Boot installs at most
// eighteen nonzero words per boot task, three instructions each, plus ten per
// complete group of 128 stores across the common and optional lists. The linked
// station pack's planned 128 KiB heap / 16-slot profile installs only dispatcher
// and idle (20 stores, zero feeds). From E1's 418,556, the failure export's arg and
// result each add 8 clear + 10 copy, two island gates add 2*10 copy, and classifying
// cause 114 adds MOVI + branch (2). With 34,069 clear, 12,396 copy and 2,428 BSS
// words this gives 418,614. Opting in adds 40 config copy words, the clear words
// for its TCB, stack, config and alignment, and 54 for its frame plus any install feed.
// With a 4 KiB start stack in that plan: 418,614 + 1,076*8 + 40*10 + 54 = 427,676.
// These sums are at 160 MHz. At 240 MHz the power-on wait is two 1,200-cycle stretches,
// adding 800 + 15 = 815.
// The masked RWDT budget is three quarters of
// the timeout minus one turn, at least 50 ms (8M cycles at 160 MHz).
fn restart_bound(r: &crate::RadioSram, profile: &Radio, pages: u32, install_feeds: u64) -> u64 {
    let clear: u64 = r.clear_spans().iter().map(|s| u64::from((s.end - s.start) / 4)).sum();
    let mut copy = u64::from(crate::RadioSram::PHY_ROM_WORDS + crate::RadioSram::PHY_TABLE_WORDS) + u64::from(r.kernel_state_words)
        + u64::from(r.osi_config_words()) + u64::from(profile.island.header.gate_count);
    let mut bss = 0; let mut segments = 0;
    for s in &profile.island.segments {
        match s.kind {
            SegmentKind::Iram | SegmentKind::Data => copy += u64::from(s.bytes / 4),
            SegmentKind::Bss => bss += u64::from(s.bytes / 4),
            _ => continue,
        }
        segments += 1;
    }
    // The power-on wait's stretches, then 2496 for 16 header comparisons, boot's
    // common and idle stores, quiesce's fixed 40, power, fixed copies, feeds,
    // pointer checks, report publication and marker (4096 with the 160 MHz wait).
    // Each RAM segment also gets 96 for setup, boundary feeds and up to three tail bytes.
    // Another 512 covers both STAMP calls, recognition, scheduling (<=31 six-instruction
    // shifts), deadline/transport checks, publication, stack traffic and policy feeds.
    // A nonmatching cause costs one MOVI and one inverted branch before testing 114.
    let (stretches, cycles) = POWER_ON_STRETCHES;
    let power_on = u64::from(stretches * cycles) + POWER_ON_STRETCH_EXTRA * u64::from(stretches - 1);
    power_on + 2496 + 512 + 2 + 8 * clear + 10 * copy + 7 * bss + 96 * segments + 5 * u64::from(pages)
        + 8 * u64::from(profile.task_capacity) + 54 * r.boot_tasks.len() as u64 + 10 * install_feeds
}

fn clear_words(c: &mut Code<'_>, start: u32, words: u32) {
    c.address(2, start); c.imm(3, words); c.imm(4, 0);
    let clear = c.label(); c.bind(clear);
    c.a.s32i(4, 2, 0); c.a.addi(2, 2, 4); c.a.addi(3, 3, -1);
    // At most 256 * 7 loop instructions plus setup/feed < MAX_OP_INSNS. Init clears the due word
    // here, so these feed now.
    let next = c.label(); c.a.extui(5, 3, 0, 8); c.zero(ccz::NE, 5, next); c.feed_now(); c.bind(next);
    c.zero(ccz::NE, 3, clear);
}

fn init(c: &mut Code<'_>, m: &Module<'_>, l: &SramLayout, pins: &[Pin], panel: Option<&Panel>, i2c: Option<&I2cBus>, timing: Option<&Timing>,
    rom_shim: usize, analog_check: bool, link: &Link) {
    c.address(11, SLOT_BASE); c.a.wsr(11, VECBASE); c.a.rsync(); c.a.isync();
    c.imm(11, 1); c.a.wsr(11, CPENABLE); c.a.rsync();
    c.imm(11, 0); c.a.op24(enc::rsr(0, 3, 15, FCR, 11)); // Table 221 p.668, WUR.FCR pp.317,651.
    c.a.mov_n(12, 2); c.a.mov_n(13, 3); // incoming RESET_STATE / STORE0
    // Once a2/a3 are saved, the CPU onto the running PLL at 160 MHz in esp-hal's order
    // (configure_cpu_clk_impl with ensure_voltage_raised): the LDO slaves for the new rate
    // (0x7 >> (160 MHz / 80 MHz)), the PLL frequency and CPU period, then the source, PRE_DIV 0;
    // then at once to CPU_HZ as IDF raises it (raise_240).
    bits(c, RTC_DATE, LDO_SLAVES, (0x7 >> (160 / 80)) << 13);
    bits(c, CPU_PER_CONF, 0xf, CPU_PERIOD_160 | CPU_WAIT_MODE_FORCE_ON);
    bits(c, SYSCLK_CONF, 0xfff, 1 << 10);
    raise_240(c, rom_shim);
    if let Some(audio) = l.speaker.as_ref().map(|s| &s.audio).or_else(|| l.mic.as_ref().map(|m| &m.audio)) {
        speaker::amps_off(c, audio);
        c.feed_now(); // Before the first SRAM clear, even on a board with many amplifier enables.
    }
    // Clear runtime data (with the range-fault bitmap), then each region's
    // buffers with their padding. Initialized state is restored below.
    let spans = std::iter::once((l.t0, l.runtime_bytes / 4)).chain(l.regions.iter()
        .filter(|r| r.used > r.start).map(|r| (r.start as u32, ((r.used - r.start) / 4) as u32)));
    for (start, words) in spans { clear_words(c, start, words); }
    c.a.extui(12, 12, 0, 6); c.store(12, l.reset_cause); c.store(13, l.previous_handler);
    // The feeds' quarter: the stage-0 hold the boot stage programmed (HOLD registers read back,
    // facts-machine.md §5) times the board's quarter per hold, capped; then the first feed point
    // is due now.
    c.load(4, crate::RWDT_HOLD); c.address(5, link.rwdt_quarter_per_hold);
    c.a.mull(6, 4, 5); c.a.muluh(7, 4, 5); c.address(5, RWDT_QUARTER_CAP);
    c.a.movnez(6, 5, 7); c.a.minu(6, 6, 5); c.store(6, l.rwdt_due + 4);
    c.a.op24(enc::rsr(0, 3, 0, CCOUNT, 6)); c.store(6, l.rwdt_due);
    c.copy(link.state_init, l.state, l.state_words);
    c.copy(link.timer_init, l.timers, l.timer_count * (TIMER_BYTES / 4));
    if let Some(r) = &l.radio {
        c.copy_fed(crate::RadioSram::PHY_ROM_RAM, r.boot_phy_ram, crate::RadioSram::PHY_ROM_WORDS);
        c.load(2, radio::kernel::SYSCON_WIFI_CLK_EN); c.store(2, r.boot_wifi_clk);
        c.load(2, radio::kernel::SYSCON_WIFI_RST_EN); c.store(2, r.boot_wifi_rst);
        c.call(ISLAND_START);
    }
    for pin in pins {
        let mux = IO_MUX + 4 + pin.number * 4;
        c.load(2, mux); c.imm(3, !((7 << 12) | (1 << 1) | (3 << 7)));
        c.a.and(2, 2, 3);
        // Enable the input path for outputs too: sched permits reading them.
        let pull = if pin.output { 0 } else { match pin.pull { ports::PULL_UP => 1 << 8,
            ports::PULL_DOWN => 1 << 7, _ => 0 } };
        c.imm(3, (1 << 12) | (1 << 9) | pull); c.a.or(2, 2, 3); c.store(2, mux);
        let bank = u32::from(pin.number >= 32);
        let bit = 1 << (pin.number & 31);
        if pin.output {
            c.load(2, GPIO + 0x74 + pin.number * 4); c.imm(3, !(1 << 2));
            c.a.and(2, 2, 3); c.store(2, GPIO + 0x74 + pin.number * 4);
            c.set(GPIO + 0x554 + pin.number * 4, 256 | (1 << 10));
            c.set(GPIO + 0x0c + bank * 12, bit); // start low before enabling
            c.set(GPIO + 0x24 + bank * 12, bit);
        } else { c.set(GPIO + 0x28 + bank * 12, bit); }
    }
    // CRC runs before t0, through DROM, over the final slot including all
    // literals. No CRC field is embedded in the bytes being identified.
    c.call(CRC); c.a.mov_n(14, 2);
    c.set(SYSTIMER, UNIT0); c.set(SYSTIMER + 0x34, 0); c.set(SYSTIMER + 0x64, 0);
    c.set(TARGET0_MAP, 1); c.set(USB_MAP, if c.mic { 1 } else { 2 });
    bits(c, crate::USB_EP1 + 0x10, USB_EMPTY | if l.link.is_some() { USB_RX | USB_IN_TOKEN } else { 0 }, 0);
    c.imm(2, 0); c.a.wsr(2, INTENABLE); c.a.rsync();
    c.call(STAMP); c.store(2, l.t0); c.store(3, l.t0 + 4);
    c.set(l.stamp, 0); c.set(l.stamp + 4, 0);
    c.symbols.t0_ready = c.here();
    if let Some(e) = &l.link {
        c.store(14, e.image_crc); c.set(e.rehello, 1); c.call(REHELLO);
        bits(c, crate::USB_EP1 + 0x10, 0, USB_RX);
    } else {
        let n = hello_template(c, l);
        c.store(14, l.body + 8); c.load(2, l.reset_cause); c.byte(2, l.body + 21);
        c.imm(2, n); c.address(3, l.body); c.load(4, l.stamp); c.call(WIRE);
    }
    let healthy = c.label(); c.load(5, l.reset_cause); c.imm(2, RWDT_RESET_CAUSE);
    c.branch(cc::NE, 5, 2, healthy);
    c.load(4, l.previous_handler); c.imm(3, u32::from(watch::FAULT_WATCHDOG));
    c.imm(2, u32::from(watch::FAULT)); c.call(EVENT); c.bind(healthy);
    if analog_check { analog(c, rom_shim); }
    if let Some(bus) = i2c { i2c_init(c, bus, timing.expect("I2C timing")); }
    if let Some(panel) = panel { panel_init(c, l, panel, timing.expect("panel I2C timing")); }
    if l.speaker.is_some() || c.mic { speaker::audio_init(c, l, timing.expect("audio I2C timing")); }
    if c.mic {
        c.call(STAMP); c.call(MIC_SERVICE);
        c.imm(2, mic::LINE_BIT); c.a.wsr(2, INTENABLE); c.a.rsync();
        c.a.op24(enc::rrr(0, 0, 0, 6, 0, 2));
    }
    if l.timer_count != 0 { start_timers(c, l); }
    if let Some(k) = m.entry(SETUP) {
        c.call(STAMP); c.set(l.ordinal, 1); c.set(crate::STORE0, k as u32);
        c.address(2, l.state); c.address(3, link.shared); c.address(11, link.setup);
        c.symbols.setup_call = c.here(); c.a.callx0(11); c.feed_now();
    }
    // As in radio_restart, a feed ends RADIO_START's install stretch.
    if c.radio {
        if c.mic { c.a.op24(enc::rrr(0, 0, 0, 6, 15, 2)); }
        c.call(RADIO_START); c.feed_now(); c.imm(2, 0); c.call(RADIO_BEGIN);
    }
    c.jump(TURN);
}

// The 240 MHz port's analog check (SlotOptions::analog_check), before setup, with the CPU at CPU_HZ. Probe frames:
// ids 0-5 the low and high halves of ANA_CONF0, ANA_CONFIG and ANA_CONFIG2 as found; ids 6-7
// ANA_CONFIG after the BBPLL unmask, which clears bit 17 only when it is set
// (esp_hal_regi2c/esp32s3/include/hal/regi2c_ctrl_ll.h:29-32, regi2c_ctrl_ll_i2c_bbpll_enable);
// then per ANALOG_READS entry its `register << 8 | data` from the ROM's read (b2) and the call's
// CCOUNT cycles (a call over the probe's 16-bit range reads 0xffff); then RAISE_PROBES. Each call runs with
// interrupts masked through the shim at `shim` (`rom_call`). Registers kept across the shim and EVENT calls:
// a12 the call's start, a13 the probe word, a14 the cycles, a15 PS (the shim keeps a12..a15; EVENT and its
// callees use a2..a11; feed a9..a11).
fn analog(c: &mut Code<'_>, shim: usize) {
    c.call(STAMP);
    for (n, address) in [ANA_CONF0, ANA_CONFIG, ANA_CONFIG2].into_iter().enumerate() {
        c.load(12, address); probe_halves(c, 2 * n as u8, 12);
    }
    let unmasked = c.label();
    c.load(12, ANA_CONFIG); c.a.extui(13, 12, 17, 1); c.zero(ccz::EQ, 13, unmasked);
    c.imm(13, !ANA_BBPLL_MASK); c.a.and(12, 12, 13); c.store(12, ANA_CONFIG);
    c.bind(unmasked);
    c.load(12, ANA_CONFIG); probe_halves(c, 6, 12);
    for (k, &(slave, register)) in ANALOG_READS.iter().enumerate() {
        let id = ANALOG_READ_PROBES + 2 * k as u8;
        c.feed();
        rom_call(c, shim, ROM_I2C_READ_REG, true, |c| {
            c.imm(3, u32::from(slave)); c.imm(4, ANALOG_HOST); c.imm(5, u32::from(register));
        });
        c.a.extui(13, 2, 0, 8); c.imm(12, u32::from(register) << 8); c.a.or(13, 13, 12);
        probe(c, id, 13); probe(c, id + 1, 14);
    }
    dbias_240(c, 13); probe(c, RAISE_PROBES, 13);
    clock_check(c, RAISE_PROBES + 1);
}

// One ROM call through the shim with interrupts masked (RSIL a15, 15; PS back after): a2 `entry`, `args` sets a3
// onwards; after it a2 holds the result. Timed, a14 holds the call's CCOUNT cycles (over the probe's 16-bit range
// 0xffff) and a12 is used; untimed, the shim keeps a12..a14. The windowed callee may leave the loop registers
// clobbered (the shim's contract), so LCOUNT goes back to zero before interrupts do, as the radio scheduler does
// after its shim: the runtime never loops in hardware, and a stray LCOUNT would loop back at LEND. a3.
fn rom_call(c: &mut Code<'_>, shim: usize, entry: u32, timed: bool, args: impl FnOnce(&mut Code<'_>)) {
    c.a.op24(enc::rrr(0, 0, 0, 6, 15, 15)); // RSIL a15, 15
    c.address(2, entry); args(c);
    if timed { c.a.op24(enc::rsr(0, 3, 0, CCOUNT, 12)); }
    c.call(shim);
    if timed { c.a.op24(enc::rsr(0, 3, 0, CCOUNT, 14)); }
    c.imm(3, 0); c.a.wsr(3, LCOUNT); c.a.isync();
    c.a.wsr(15, 230); c.a.rsync();
    if timed { c.a.sub(14, 14, 12); c.imm(12, 0xffff); c.a.minu(14, 14, 12); }
}

// init's raise to CPU_HZ, its last step before it touches SRAM: IDF's rtc_clk_cpu_freq_to_pll_mhz(240) from
// 160 MHz (esp_hw_support/port/esp32s3/rtc_clk.c:221-251): EXT_RTC_DREG and EXT_DIG_DREG to this chip's 240 MHz
// settings (`dbias_240`), 40 us, RTC_CNTL_DATE SLAVE_PD = DEFAULT_LDO_SLAVE 0x7 (rtc.h:147) >> 240/80, then
// CPUPERIOD_SEL 2. The PLL already runs at 480 MHz (init), so no bbpll_configure: IDF runs it only when the CPU
// comes from another source, and esp-hal 4059aa58 (soc/esp32s3/clocks.rs) runs none. A regulator write is the
// register as rom_i2c_readReg reads it with [4:0] replaced (esp-hal's write_field; IDF's REGI2C_WRITE_MASK).
// DIG_REG has no ANA_CONFIG mask (regi2c_defs.h names the BBPLL's and the SAR's only). No feed: the feed's due
// word is not set yet, and the boot stage's hold covers init. The ROM's frames lie below the boot stage's a1.
// a14 holds the settings; a12 and a13 (the incoming RESET_STATE and STORE0) are kept. The board ran this
// sequence after the analog check's reads (sessions 19-21).
fn raise_240(c: &mut Code<'_>, shim: usize) {
    dbias_240(c, 14);
    for (k, register) in [4u8, 6].into_iter().enumerate() {
        let at = |c: &mut Code<'_>| { c.imm(3, DIG_REG); c.imm(4, ANALOG_HOST); c.imm(5, u32::from(register)); };
        rom_call(c, shim, ROM_I2C_READ_REG, false, at);
        c.a.extui(6, 14, 8 * k as u8, 5); c.imm(3, !DREG_FIELD & 0xff); c.a.and(2, 2, 3); c.a.or(6, 6, 2);
        rom_call(c, shim, ROM_I2C_WRITE_REG, false, at);
    }
    let settle = c.label();
    c.imm(3, RAISE_SETTLE_CYCLES); c.a.op24(enc::rsr(0, 3, 0, CCOUNT, 2));
    c.bind(settle); c.a.op24(enc::rsr(0, 3, 0, CCOUNT, 4)); c.a.sub(4, 4, 2); c.branch(cc::LTU, 4, 3, settle);
    bits(c, RTC_DATE, LDO_SLAVES, (0x7 >> (240 / 80)) << 13);
    bits(c, CPU_PER_CONF, 0x7, CPU_PERIOD_240);
}

// This chip's 240 MHz core-regulator settings into `d`, EXT_RTC_DREG | EXT_DIG_DREG << 8: IDF's
// rtc_set_stored_dbias (esp_hw_support/port/esp32s3/rtc_init.c:376-409) from the eFuse at run time, never
// baked into the image. With PVT (block version 1.1, or 1.2 and later, :391) and a nonzero DIG_DBIAS_HVT, dig is
// min(dig1v3, DIG_DBIAS_HVT + 3) (:355-374, :400) and rtc the first setting whose voltage reaches dig's less
// 25 mV (:327-353); otherwise both stay 28. K_* and V_* are sign-magnitude, bits 6 and 7 the signs (:335-338);
// the sums wrap as IDF's unsigned ones do and the searches compare unsigned. a2..a11.
fn dbias_240(c: &mut Code<'_>, d: Reg) {
    let (pvt, done) = (c.label(), c.label());
    c.imm(d, DBIAS_240_DEFAULT | DBIAS_240_DEFAULT << 8);
    c.load(2, EFUSE_SYS_3); c.a.extui(2, 2, 24, 3);
    c.load(3, EFUSE_PART1_4); c.a.extui(3, 3, 0, 2);
    c.imm(4, 2); c.branch(cc::GEU, 3, 4, pvt);
    c.imm(4, 1); c.branch(cc::EQ, 2, 4, pvt);
    c.branch(cc::NE, 3, 4, done);
    c.imm(4, 2); c.branch(cc::LTU, 2, 4, done);
    c.bind(pvt);
    c.load(5, EFUSE_SYS_5); c.a.extui(6, 5, 11, 5);
    c.zero(ccz::EQ, 6, done);
    c.load(4, EFUSE_SYS_4);
    // a7 the digital voltage at setting 20 and a8 its slope, in 0.1 mV (rtc_init.c:363-364).
    c.a.extui(7, 5, 3, 8); sign_magnitude(c, 7, 8);
    c.imm(2, 20); c.a.mull(7, 7, 2); c.imm(2, V_DIG_MID); c.a.add_n(7, 7, 2);
    c.a.extui(8, 4, 20, 7); sign_magnitude(c, 8, 7);
    c.imm(2, K_DIG_MID); c.a.add_n(8, 8, 2);
    c.imm(9, 13000); search(c, 3, 7, 8, 9);
    c.a.addi(6, 6, 3); c.a.minu(6, 3, 6);
    // a9 the digital voltage at dig less 25 mV (rtc_init.c:344, :348).
    c.a.addi(9, 6, -20); c.a.mull(9, 9, 8); c.a.add_n(9, 9, 7); c.imm(2, 250); c.a.sub(9, 9, 2);
    // a7 and a8 the RTC voltage at 20 and its slope (rtc_init.c:340, :342).
    c.a.extui(7, 4, 27, 5); c.a.extui(2, 5, 0, 3); c.a.slli(2, 2, 5); c.a.or(7, 7, 2); sign_magnitude(c, 7, 8);
    c.imm(2, 20); c.a.mull(7, 7, 2); c.imm(2, V_RTC_MID); c.a.add_n(7, 7, 2);
    c.a.extui(8, 4, 13, 7); sign_magnitude(c, 8, 7);
    c.imm(2, K_RTC_MID); c.a.add_n(8, 8, 2);
    search(c, 3, 7, 8, 9);
    c.a.slli(6, 6, 8); c.a.or(d, 3, 6);
    c.bind(done);
}

// IDF's sign-magnitude eFuse field of `bits` bits in `r`: the top bit the sign (rtc_init.c:335-338). a10, a11.
fn sign_magnitude(c: &mut Code<'_>, r: Reg, bits: u8) {
    c.a.extui(10, r, bits - 1, 1); c.a.extui(r, r, 0, bits - 1); c.a.neg(11, r); c.a.movnez(r, 11, 10);
}

// IDF's dbias search (rtc_init.c:345-351, :366-372): into `d` the first setting in 15..=30 at which
// `base + slope * (d - 20)` reaches `target`, unsigned, else 31. a10, a11.
fn search(c: &mut Code<'_>, d: Reg, base: Reg, slope: Reg, target: Reg) {
    let (next, found) = (c.label(), c.label());
    c.imm(d, 15);
    c.imm(10, 5); c.a.mull(10, 10, slope); c.a.sub(10, base, 10);
    c.imm(11, 31);
    c.bind(next);
    c.branch(cc::GEU, 10, target, found);
    c.a.addi(d, d, 1); c.a.add_n(10, 10, slope);
    c.branch(cc::LTU, d, 11, next);
    c.bind(found);
}

// Until the 16 MHz systimer has run a3 ticks from now: unit 0's low word (UPDATE, then VALUE_VALID, as STAMP),
// differences wrapping; a9 counts the reads after the first, a10 their VALUE_VALID polls. The systimer counts from
// init on, so the wait ends. a4..a10.
fn systimer_wait(c: &mut Code<'_>) {
    let (first, next, valid) = (c.label(), c.label(), c.label());
    c.address(4, SYSTIMER); c.imm(5, UPDATE); c.a.s32i(5, 4, 4);
    c.bind(first); c.a.l32i(5, 4, 4); c.a.extui(5, 5, 29, 1); c.zero(ccz::EQ, 5, first);
    c.a.l32i(6, 4, 0x44); c.imm(9, 0); c.imm(10, 0);
    c.bind(next);
    c.imm(5, UPDATE); c.a.s32i(5, 4, 4);
    c.bind(valid); c.a.addi(10, 10, 1); c.a.l32i(5, 4, 4); c.a.extui(5, 5, 29, 1); c.zero(ccz::EQ, 5, valid);
    c.a.addi(9, 9, 1);
    c.a.l32i(7, 4, 0x44); c.a.sub(7, 7, 6); c.branch(cc::LTU, 7, 3, next);
}

// CCOUNT against the systimer, interrupts masked, in three rounds through one copy of the code:
// CLOCK_CHECK_TICKS with the code cold, the same warm, then twice it warm. Per round three probes from `id`: the
// cycles (capped at 0xffff), the systimer reads the wait made and their VALUE_VALID polls (session 20: a warm
// read takes 0.42-0.44 us at both clocks; the polls tell the VALID latency from the accesses' cost). The warm rounds' difference is the clock against
// the systimer alone; the first warm round's excess over that the reads' and the loop's cost; the cold round's
// excess over the warm one the first pass's instruction fetches.
fn clock_check(c: &mut Code<'_>, id: u8) {
    let again = c.label();
    c.imm(13, 0);
    c.bind(again);
    c.a.srli(2, 13, 1); c.a.ssl(2); c.imm(3, CLOCK_CHECK_TICKS); c.a.sll(3, 3);
    c.a.op24(enc::rrr(0, 0, 0, 6, 15, 15)); // RSIL a15, 15
    c.a.op24(enc::rsr(0, 3, 0, CCOUNT, 12));
    systimer_wait(c);
    c.a.op24(enc::rsr(0, 3, 0, CCOUNT, 14));
    c.a.wsr(15, 230); c.a.rsync();
    c.a.sub(14, 14, 12); c.imm(12, 0xffff); c.a.minu(14, 14, 12);
    // a12 the reads in its low half, the polls in its high half; EVENT keeps a12..a15.
    c.a.extui(9, 9, 0, 16); c.a.slli(10, 10, 16); c.a.or(12, 9, 10);
    // Probe id + 3 * round the cycles, the next ones the reads and the polls.
    for offset in 0..3 {
        c.a.addx2(3, 13, 13); c.a.addi(3, 3, i32::from(id) + offset);
        match offset { 0 => c.a.extui(4, 14, 0, 16), 1 => c.a.extui(4, 12, 0, 16), _ => c.a.extui(4, 12, 16, 16) }
        c.imm(2, u32::from(watch::PROBE)); c.call(EVENT);
    }
    c.a.addi(13, 13, 1); c.imm(2, 3); c.branch(cc::LTU, 13, 2, again);
}

// watch::Event::Probe { id, value }: EVENT's a3 byte and a4 low half.
fn probe(c: &mut Code<'_>, id: u8, value: Reg) {
    c.a.extui(4, value, 0, 16); c.imm(3, u32::from(id)); c.imm(2, u32::from(watch::PROBE)); c.call(EVENT);
}

// `probe(id, value)` (ports::PROBE): a2 id, a3 value; EVENT keeps the id's low byte and the
// value's low half, so the frame is the program's own word for word.
fn probe_port(c: &mut Code<'_>, l: &SramLayout, port: u16) {
    c.routine(PORT + port as usize, ports::PORTS[port as usize].name);
    c.store(0, l.port_return);
    c.a.mov_n(4, 3); c.a.mov_n(3, 2); c.imm(2, u32::from(watch::PROBE)); c.call(EVENT);
    c.load(0, l.port_return); c.a.ret_n();
}

fn probe_halves(c: &mut Code<'_>, id: u8, word: Reg) {
    probe(c, id, word); c.shr(13, word, 16); probe(c, id + 1, 13);
}

fn stamp(c: &mut Code<'_>, l: &SramLayout) {
    c.routine(STAMP, "stamp"); c.symbols.stamp = c.here();
    if let Some(m) = &l.mic {
        // Pair the dispatcher stamp with CCOUNT. The capture ISR reads this
        // origin without issuing a competing SYSTIMER snapshot.
        read_clock(c, l, Some(m.stamp_cycle));
        c.a.op24(enc::rrr(0, 0, 0, 6, 15, 4));
        c.load(5, m.stamp_cycle); c.store(5, m.cycle_ref);
        c.store(2, m.edge); c.store(3, m.edge + 4);
        c.a.wsr(4, 230); c.a.rsync();
        c.store(2, l.stamp); c.store(3, l.stamp + 4); c.a.ret_n();
    } else { sample_clock(c, l, l.stamp); }
    c.symbols.stamp_end = c.here();
}

fn sample_clock(c: &mut Code<'_>, l: &SramLayout, dest: u32) {
    read_clock(c, l, None);
    c.store(2, dest); c.store(3, dest + 4); c.a.ret_n();
}

fn read_clock(c: &mut Code<'_>, l: &SramLayout, cycle: Option<u32>) {
    c.address(4, SYSTIMER); c.imm(5, UPDATE);
    if cycle.is_some() { c.a.op24(enc::rrr(0, 0, 0, 6, 15, 6)); }
    c.a.s32i(5, 4, 4);
    if let Some(at) = cycle {
        // Pair the two time bases at the snapshot request. Let capture run
        // while waiting for VALUE_VALID, using the previous published pair.
        c.a.op24(enc::rsr(0, 3, 0, CCOUNT, 5)); c.store(5, at);
        c.a.wsr(6, 230); c.a.rsync();
    }
    let poll = c.label(); c.bind(poll);
    c.a.l32i(5, 4, 4); c.a.extui(5, 5, 29, 1); c.zero(ccz::EQ, 5, poll);
    c.a.l32i(2, 4, 0x44); c.a.l32i(3, 4, 0x40);
    c.imm(5, 0x000f_ffff); c.a.and(3, 3, 5); // VALUE_HI exposes only 20 bits
    c.load(4, l.t0); c.load(5, l.t0 + 4);
    c.imm(6, 0); let no_borrow = c.label(); c.branch(cc::GEU, 2, 4, no_borrow);
    c.imm(6, 1); c.bind(no_borrow);
    c.a.sub(2, 2, 4); c.a.sub(3, 3, 5); c.a.sub(3, 3, 6);
}

fn start_timers(c: &mut Code<'_>, l: &SramLayout) {
    // The flash table supplied first delays with every row disarmed. Capture
    // once so equal delays stay tied. Setup runs against this same grid.
    read_clock(c, l, None); c.symbols.timer_start = c.here();
    c.address(8, l.timers); c.imm(12, l.timer_count); c.imm(13, 1);
    let row = c.label(); c.bind(row); c.feed();
    c.a.l32i(4, 8, 0); c.a.l32i(5, 8, 4); c.add64(4, 5, 2, 3, 6, 7);
    c.a.s32i(4, 8, 0); c.a.s32i(5, 8, 4); c.a.s32i(13, 8, 16);
    c.a.addi(8, 8, TIMER_BYTES as i32); c.a.addi(12, 12, -1); c.zero(ccz::NE, 12, row);
}

fn dispatch(c: &mut Code<'_>, l: &SramLayout, motion: Option<(Motion, u8)>, touch: Option<(Touch, u8)>, timing: Option<&Timing>, link: &Link) {
    c.routine(TURN, "dispatcher"); c.symbols.dispatch = c.here();
    // RSIL a2,15 (a2,0 with radio): ISA Table 201 p.662; next instruction sees the new
    // INTLEVEL without a SYNC (pp.582–583).
    c.a.op24(enc::rrr(0, 0, 0, 6, if c.radio || c.mic { 0 } else { 15 }, 2));
    // Radio-only images take host bytes in sleep, so the wake-to-handler path (RADIO_IDLE_WAKE_INSNS) is unchanged.
    if l.eth.is_some() || l.net.is_some() {
        // Consume an armed IN edge before draining: the token may also have
        // freed the FIFO, and a drain can refill it before we observe FREE.
        c.call(STAMP); c.call(STALL); c.call(DRAIN); c.call(RX);
    } else { c.call(DRAIN); c.call(STAMP); }
    if l.speaker.is_some() { c.call(SPEAKER_SERVICE); }
    if c.mic { c.call(MIC_SERVICE); }
    if l.net.is_some() { c.call(NET_FLUSH); }
    c.address(2, l.timers); c.imm(3, l.timer_count); c.imm(13, 0);
    let scan = c.label(); let skip = c.label(); let choose = c.label(); let scanned = c.label();
    c.zero(ccz::EQ, 3, scanned); c.bind(scan);
    c.a.l32i(4, 2, 16); c.zero(ccz::EQ, 4, skip);
    c.a.l32i(4, 2, 0); c.a.l32i(5, 2, 4);
    c.zero(ccz::EQ, 13, choose);
    c.branch(cc::LTU, 5, 15, choose); c.branch(cc::NE, 5, 15, skip);
    c.branch(cc::GEU, 4, 14, skip); // equals retain the lower timer number
    c.bind(choose); c.a.mov_n(13, 2); c.a.mov_n(14, 4); c.a.mov_n(15, 5);
    c.bind(skip); c.a.addi(2, 2, TIMER_BYTES as i32); c.a.addi(3, 3, -1);
    c.zero(ccz::NE, 3, scan); c.bind(scanned);
    let events: Vec<_> = l.eth.iter().map(|e| (e.received, e.seen, e.due, e.entry, link.eth))
        .chain(l.speaker.iter().map(|s| (s.received, s.seen, s.due, s.entry, link.speaker)))
        .chain(l.mic.iter().map(|m| (m.received, m.seen, m.due, m.entry, link.mic)))
        .map(|row| (row, c.label())).collect();
    for &((received, seen, at, _, _), _) in &events {
        let skip = c.label(); let choose = c.label();
        c.load(2, received); c.load(3, seen); c.branch(cc::EQ, 2, 3, skip);
        c.load(4, at); c.load(5, at + 4);
        c.zero(ccz::EQ, 13, choose);
        c.branch(cc::LTU, 5, 15, choose); c.branch(cc::NE, 5, 15, skip);
        c.branch(cc::GEU, 4, 14, skip); // timers win equal due stamps
        c.bind(choose); c.address(13, at); c.a.mov_n(14, 4); c.a.mov_n(15, 5);
        c.bind(skip);
    }
    let net_handler = l.net.as_ref().map(|n| { net::scan(c, n); c.label() });
    let idle = c.label(); let future = c.label(); let due = c.label(); let invoke = c.label();
    c.zero(ccz::EQ, 13, idle);
    c.load(6, l.stamp); c.load(7, l.stamp + 4);
    c.branch(cc::LTU, 7, 15, future); c.branch(cc::NE, 7, 15, due);
    c.branch(cc::LTU, 6, 14, future); c.bind(due);
    for &((_, _, at, _, _), start) in &events { c.address(2, at); c.branch(cc::EQ, 13, 2, start); }
    if let (Some(n), Some(handler)) = (&l.net, net_handler) { net::select(c, l, n, link, handler); }
    if let Some((sensor, odr)) = motion {
        let s = l.motion.as_ref().expect("motion SRAM");
        let ready = c.label(); let configure = c.label();
        c.address(2, s.timer); c.branch(cc::NE, 13, 2, ready);
        c.load(2, s.initialized); c.imm(3, MOTION_RUNNING); c.branch(cc::EQ, 2, 3, ready);
        c.zero(ccz::NE, 2, configure);
        motion_reset(c, s, sensor, l.i2c_pending.expect("shared I2C owner"), timing.expect("motion I2C timing"));
        c.bind(configure);
        motion_init(c, s, sensor, odr, l.i2c_pending.expect("shared I2C owner"), timing.expect("motion I2C timing"));
        c.bind(ready);
    }
    if let Some((sensor, scan)) = touch {
        let s = l.touch.as_ref().expect("touch SRAM");
        let ready = c.label();
        c.address(2, s.timer); c.branch(cc::NE, 13, 2, ready);
        c.load(2, s.initialized); c.zero(ccz::NE, 2, ready);
        touch_init(c, s, sensor, scan, l.i2c_pending.expect("shared I2C owner"), timing.expect("touch I2C timing"));
        c.bind(ready);
    }
    c.a.l32i(2, 13, 20);
    let repeat = c.label(); c.zero(ccz::NE, 2, repeat);
    c.imm(2, 0); c.a.s32i(2, 13, 16); c.jump(invoke);
    c.bind(repeat); c.a.l32i(4, 13, 8); c.a.l32i(5, 13, 12);
    c.imm(8, 0); c.add64(14, 15, 4, 5, 2, 3);
    let advance = c.label(); let advanced = c.label(); let missed = c.label();
    c.a.or(2, 4, 5); c.zero(ccz::EQ, 2, advanced); // period(0) matches sched
    c.bind(advance);
    c.branch(cc::LTU, 15, 7, missed); c.branch(cc::NE, 15, 7, advanced);
    c.branch(cc::LTU, 6, 14, advanced);
    c.bind(missed);
    c.imm(2, u32::MAX); let saturated = c.label(); c.branch(cc::EQ, 8, 2, saturated);
    c.a.addi(8, 8, 1); c.bind(saturated);
    c.add64(14, 15, 4, 5, 2, 3);
    // A bounded I2C transfer can span many periods on a slower shared bus.
    // Keep the existing timer-grid/coalescing rule, feeding between advances.
    if motion.is_some() || touch.is_some() || l.speaker.is_some() || c.mic { c.feed(); }
    c.jump(advance);
    c.bind(advanced); c.a.s32i(14, 13, 0); c.a.s32i(15, 13, 4);
    c.zero(ccz::EQ, 8, invoke);
    c.a.mov_n(5, 8); c.address(4, l.timers); c.a.sub(4, 13, 4); c.shr(4, 4, 5);
    c.imm(3, u32::from(watch::FAULT_OVERRUN)); c.imm(2, u32::from(watch::FAULT)); c.call(EVENT);
    c.bind(invoke);
    if let Some((sensor, _)) = motion {
        let s = l.motion.as_ref().expect("motion SRAM");
        let other = c.label(); c.address(2, s.timer); c.branch(cc::NE, 13, 2, other);
        motion_sample(c, s, sensor, l.i2c_pending.expect("shared I2C owner"), timing.expect("motion I2C timing"));
        c.bind(other);
    }
    if let Some((sensor, _)) = touch {
        let s = l.touch.as_ref().expect("touch SRAM");
        let other = c.label(); c.address(2, s.timer); c.branch(cc::NE, 13, 2, other);
        touch_sample(c, s, sensor, l.i2c_pending.expect("shared I2C owner"), timing.expect("touch I2C timing"));
        c.bind(other);
    }
    let device_events = !events.is_empty() || net_handler.is_some();
    if device_events {
        let enter = c.label();
        c.a.l32i(12, 13, 24); c.a.l32i(2, 13, 28); c.jump(enter);
        for (i, &((received, seen, _, entry, address), start)) in events.iter().enumerate() {
            c.bind(start);
            // Acknowledge at entry so an arrival during the handler stays pending.
            c.load(2, received); c.store(2, seen);
            c.address(12, address); c.imm(2, entry as u32);
            if i + 1 < events.len() { c.jump(enter); }
        }
        if let Some(handler) = net_handler { c.bind(handler); }
        c.bind(enter); c.store(2, crate::STORE0);
    }
    c.load(2, l.ordinal); c.a.addi(2, 2, 1); c.store(2, l.ordinal);
    if !device_events { c.a.l32i(2, 13, 28); c.store(2, crate::STORE0); }
    c.address(2, l.state); c.address(3, link.shared);
    if !device_events { c.a.l32i(11, 13, 24); }
    c.symbols.call_handler = c.here(); c.a.callx0(if device_events { 12 } else { 11 });
    c.symbols.handler_return = c.here(); c.feed(); c.jump(TURN);

    for (label, armed) in [(future, true), (idle, false)] {
        c.bind(label);
        if armed { c.symbols.arm = c.here(); }
        if c.radio || c.mic { c.a.op24(enc::rrr(0, 0, 0, 6, 15, 2)); }
        sleep(c, l, armed, l.speaker.as_ref());
        if let Some(r) = &l.radio {
            c.address(2, r.dispatcher);
            c.address(11, SLOT_BASE + c.a.base.label_offset(RADIO_BLOCK).unwrap_or(0));
        }
        if armed { c.symbols.waiti = c.here(); } else { c.symbols.idle_waiti = c.here(); }
        if c.radio { c.a.callx0(11); } else { c.a.waiti(0); }
        c.jump(TURN);
    }
}

// Arms TARGET0 at the stamp lo/hi (relative to t0), using x, y, tmp, carry, a10 and a11.
// Stop comparison while loading/clearing, then enable it. A deadline passed during this
// path still fires (facts-machine.md §1, Table 11.4-2). One alarm, owned by the runtime:
// sleep re-arms it before every dispatcher WAITI or block.
fn arm(c: &mut Code<'_>, lo: Reg, hi: Reg, x: Reg, y: Reg, tmp: Reg, carry: Reg) {
    c.set(SYSTIMER, UNIT0); c.set(SYSTIMER + 0x64, 0);
    let t0 = c.t0; c.load(x, t0); c.load(y, t0 + 4);
    c.add64(lo, hi, x, y, tmp, carry);
    c.imm(tmp, 0x000f_ffff); c.a.and(hi, hi, tmp); // 52-bit target register
    c.store(hi, SYSTIMER + 0x1c); c.store(lo, SYSTIMER + 0x20);
    c.set(SYSTIMER + 0x50, 1); c.set(SYSTIMER + 0x6c, 1);
    c.set(SYSTIMER + 0x64, 1); c.set(SYSTIMER, UNIT0 | TARGET0);
}

// Shared by dispatch and peripheral boot waits. a14/a15 is the relative deadline;
// cap it, add t0, arm TARGET0 and feed, keeping INTLEVEL 15 until WAITI/block.
fn sleep(c: &mut Code<'_>, l: &SramLayout, armed: bool, speaker: Option<&SpeakerSram>) {
    c.call(DRAIN);
    // Sleep feeds after its drain and before each idle service, so each service's stretch starts at a feed.
    if l.link.is_some() { c.feed(); c.call(LINK_IDLE); }
    if let Some(r) = &l.radio {
        c.feed();
        c.call(RADIO_RECORD); c.call(RADIO_REPORTS); c.call(RADIO_PHY_TABLE); c.call(RADIO_EVENTS);
        c.address(2, r.request); c.call(RADIO_RESTART);
    }
    // The idle point flushes the turn's partial packet; WR_DONE only with FREE=1.
    let flushed = c.label();
    c.load(2, l.tx_packet); c.zero(ccz::EQ, 2, flushed);
    c.address(5, crate::USB_EP1); c.a.l32i(3, 5, 4); c.a.extui(3, 3, USB_FREE_BIT, 1); c.zero(ccz::EQ, 3, flushed);
    c.imm(3, 1); c.a.s32i(3, 5, 4); c.imm(2, 0); c.store(2, l.tx_packet);
    c.bind(flushed);
    if l.link.is_some() { c.call(STALL); }
    c.load(6, l.stamp); c.load(7, l.stamp + 4);
    c.imm(4, SLEEP_CAP_TICKS); c.imm(5, 0); c.add64(6, 7, 4, 5, 2, 3);
    if armed {
        let capped = c.label(); let chosen = c.label();
        c.branch(cc::LTU, 7, 15, capped); c.branch(cc::NE, 7, 15, chosen);
        c.branch(cc::GEU, 6, 14, chosen);
        c.bind(capped); c.a.mov_n(14, 6); c.a.mov_n(15, 7); c.bind(chosen);
    } else { c.a.mov_n(14, 6); c.a.mov_n(15, 7); }
    if let Some(r) = &l.radio {
        // Decision 6: a scheduled restart competes with the cap and app/link timers.
        // Zero is unscheduled. MAX (waiting for a record or a request) loses to the cap.
        let skip = c.label(); let choose = c.label();
        c.load(4, r.next_start); c.load(5, r.next_start + 4); c.a.or(2, 4, 5); c.zero(ccz::EQ, 2, skip);
        c.branch(cc::LTU, 5, 15, choose); c.branch(cc::NE, 5, 15, skip); c.branch(cc::GEU, 4, 14, skip);
        c.bind(choose); c.a.mov_n(14, 4); c.a.mov_n(15, 5); c.bind(skip);
    }
    if let Some(e) = &l.link {
        // Both link clocks compete with timers and the watchdog sleep cap.
        // A pending re-Hello waits for TX space, not an already-expired alarm.
        for (state, deadline, fallback) in std::iter::once((e.stall_state, e.stall_due, false))
            .chain(l.eth.as_ref().map(|eth| (eth.answer_state, eth.answer_due, true))) {
            let skip = c.label(); let choose = c.label();
            if fallback { c.load(2, e.rehello); c.zero(ccz::NE, 2, skip); }
            c.load(2, state); c.imm(3, 1); c.branch(cc::NE, 2, 3, skip);
            c.load(4, deadline); c.load(5, deadline + 4);
            c.branch(cc::LTU, 5, 15, choose); c.branch(cc::NE, 5, 15, skip);
            c.branch(cc::GEU, 4, 14, skip);
            c.bind(choose); c.a.mov_n(14, 4); c.a.mov_n(15, 5); c.bind(skip);
        }
    }
    // Stop comparison while loading/clearing, then enable it. A deadline
    // passed during this path still fires (facts-machine.md §1, Table 11.4-2).
    // One alarm, owned exclusively by the runtime, including under radio.
    // CONF readback after a one-shot is undocumented (facts-machine.md §1).
    if let Some(s) = speaker { speaker::arm_eof(c, s); }
    if let Some(m) = &l.mic { mic::arm(c, l, m); }
    // LINK_IDLE can have started a new Hello lifetime since the dispatch scan.
    if let Some(n) = &l.net { c.imm(13, 1); net::scan(c, n); }
    arm(c, 14, 15, 4, 5, 2, 3);
    // Only pending bytes need IN_EMPTY. Never clear an empty FIFO's RAW
    // here: it may reassert immediately (pins.rs's adverse M: choice).
    c.load(3, crate::USB_EP1 + 0x10); c.imm(4, !USB_EMPTY); c.a.and(3, 3, 4);
    c.imm(2, (1 << 1) | if l.link.is_some() { 1 << 2 } else { 0 });
    c.load(4, l.tx_head); c.load(5, l.tx_tail);
    let empty = c.label(); c.branch(cc::EQ, 4, 5, empty);
    c.imm(4, USB_EMPTY); c.a.or(3, 3, 4); c.imm(2, (1 << 1) | (1 << 2));
    c.bind(empty); c.store(3, crate::USB_EP1 + 0x10);
    if c.radio {
        // Radio: INTENABLE also holds the yield and island lines. At INTLEVEL 15,
        // replace only the dispatcher's lines with the ones this sleep waits for.
        c.a.op24(enc::rsr(0, 3, 0, INTENABLE, 3));
        c.imm(4, !DISPATCHER_LINES); c.a.and(3, 3, 4); c.a.or(2, 2, 3);
    }
    if c.mic { c.imm(3, mic::LINE_BIT); c.a.or(2, 2, 3); }
    c.a.wsr(2, INTENABLE); c.a.rsync();
    // INTLEVEL remains 15 throughout the feed; WAITI/block unmasks it.
    c.feed();
}

// Register values below follow s3/tests/support/{spi_fixture,i2c_fixture}.rs,
// facts-io S01–S18/G01–G12/I01–I20, and facts.md's SYSTEM offsets.
const SPI2: u32 = 0x6002_4000;
const GDMA: u32 = 0x6003_f000;
const I2C0: u32 = 0x6001_3000;
const SPI_DONE: u32 = 1 << 12;
const I2C_COMPLETE: u32 = 1 << 7;
const I2C_NACK: u32 = 1 << 10;

fn bits(c: &mut Code<'_>, address: u32, clear: u32, set: u32) {
    c.load(2, address);
    if clear != 0 { c.imm(3, !clear); c.a.and(2, 2, 3); }
    if set != 0 { c.imm(3, set); c.a.or(2, 2, 3); }
    c.store(2, address);
}

// Boot-only: a14/a15 may be used before entering setup/dispatch. The true
// deadline lives in scratch because sleep caps and converts its register pair.
// A boot wait takes no host bytes, so with a link its RX cause stays disabled
// until the wait ends: the host's first packet (its answer to the Hello) would
// otherwise hold the USB line and wake each WAITI at once, and the wait would
// spin through its whole time (a panel's 360 ms: 167k wakes).
fn boot_delay(c: &mut Code<'_>, l: &SramLayout, scratch: u32, us: u32) {
    if l.link.is_some() { bits(c, crate::USB_EP1 + 0x10, USB_RX, 0); }
    c.call(STAMP); c.imm(4, us * 16); c.imm(5, 0); c.add64(2, 3, 4, 5, 6, 7);
    c.store(2, scratch); c.store(3, scratch + 4);
    let again = c.label(); let wait = c.label(); let done = c.label();
    c.bind(again);
    // RSIL after a wake restores the dispatcher's masked execution convention.
    c.a.op24(enc::rrr(0, 0, 0, 6, 15, 2)); c.feed(); c.call(STAMP);
    c.load(14, scratch); c.load(15, scratch + 4);
    c.branch(cc::LTU, 3, 15, wait); c.branch(cc::NE, 3, 15, done);
    c.branch(cc::GEU, 2, 14, done);
    c.bind(wait); sleep(c, l, true, None); c.a.waiti(0); c.jump(again); c.bind(done);
    // a2/a3 (the last stamp) and a11 are free here: no caller reads them after the wait.
    if l.link.is_some() { bits(c, crate::USB_EP1 + 0x10, 0, USB_RX); }
}

// I20/I15: one controller owner and one routing/configuration for every
// board client. No panel-local reset can change a sensor transfer's timing.
fn i2c_init(c: &mut Code<'_>, bus: &I2cBus, t: &Timing) {
    bits(c, 0x600c_0020, 0, 1 << 7);
    bits(c, 0x600c_0018, 0, 1 << 7);
    bits(c, 0x600c_0020, 1 << 7, 0);
    for (pin, signal) in [(bus.scl, 89), (bus.sda, 90)] {
        c.set(GPIO + 0x74 + pin * 4, 1 << 2);
        c.set(IO_MUX + 4 + pin * 4, (1 << 12) | (1 << 9));
        c.set(GPIO + 0x554 + pin * 4, signal);
        c.set(GPIO + 0x154 + signal * 4, (1 << 7) | pin);
    }
    i2c_configure(c, t);
    // i2c_wait's WAITI: COMPLETE/NACK (enabled by i2c_configure) reach I2C_LINE.
    if !c.radio { c.set(I2C0_MAP, I2C_LINE); }
}

fn i2c_configure(c: &mut Code<'_>, t: &Timing) {
    // I11: divided XTAL, active; I12/I13 phase derivation is in i2c.rs.
    c.set(I2C0 + 0x04, 1 << 4); c.set(I2C0 + 0x54, (1 << 21) | (t.divider - 1));
    for (offset, value) in [(0, t.low), (0x30, t.hold), (0x34, t.sample),
        (0x38, t.high | (t.wait << 9)), (0x40, t.start), (0x44, t.start),
        (0x48, t.stop), (0x4c, t.start), (0x50, 0), (0x84, t.low + 1)]
    { c.set(I2C0 + offset, value); }
    c.set(I2C0 + 0x04, (1 << 4) | (1 << 11)); // I09: CONF_UPGATE.
    c.set(I2C0 + 0x28, I2C_COMPLETE | I2C_NACK);
}

fn i2c_list(c: &mut Code<'_>, tx: &[u8], commands: &[u32]) {
    // I07/Appendix A: RX watermark 31 leaves room for twelve axis bytes;
    // reset both FIFOs before a new transaction, including after failure.
    c.set(I2C0 + 0x18, 0x409f | (3 << 12)); c.set(I2C0 + 0x18, 0x409f);
    c.set(I2C0 + 0x24, I2C_COMPLETE | I2C_NACK);
    for &byte in tx { c.set(I2C0 + 0x1c, u32::from(byte)); }
    // I02/I02a: replace every command slot, clearing old DONE bits too.
    for n in 0..8 { c.set(I2C0 + 0x58 + n * 4, commands.get(n as usize).copied().unwrap_or(0)); }
}

fn i2c_wait(c: &mut Code<'_>, pending_transfer: Option<u32>, deadline: u32, nack: usize, timeout: usize, clock: usize) {
    c.call(clock); c.imm(4, deadline); c.imm(5, 0); c.add64(2, 3, 4, 5, 6, 7);
    c.a.mov_n(7, 2); c.a.mov_n(8, 3);
    if let Some(at) = pending_transfer { c.set(at, 1); }
    // Radio-free, a wait inside the RWDT hold's sleep cap sleeps in WAITI between
    // checks: woken by COMPLETE/NACK on I2C_LINE or by TARGET0, armed here at the
    // deadline (sleep re-arms it for the dispatcher). Otherwise it polls, fed each turn.
    let wake = !c.radio && !c.mic && deadline <= SLEEP_CAP_TICKS;
    if wake {
        if !c.symbols.board_unverified.contains(&I2C_WAITI) { c.symbols.board_unverified.push(I2C_WAITI); }
        c.a.mov_n(4, 7); c.a.mov_n(5, 8); arm(c, 4, 5, 2, 3, 6, 9);
    }
    c.set(I2C0 + 0x04, (1 << 4) | (1 << 5));
    let poll = c.label(); let pending = c.label(); let done = c.label(); c.bind(poll);
    // RSIL a2,15 after a wake restores the masked convention (ISA Table 201 p.662).
    if wake { c.a.op24(enc::rrr(0, 0, 0, 6, 15, 2)); }
    // Feed every poll or wake. STAMP uses a2..a6, leaving deadline a7/a8 intact.
    // I16/I18: read enabled COMPLETE/NACK through STATUS, never unknown RAW.
    c.feed(); c.call(clock);
    c.branch(cc::LTU, 3, 8, pending); c.branch(cc::NE, 3, 8, timeout);
    c.branch(cc::LTU, 7, 2, timeout); c.bind(pending);
    c.load(2, I2C0 + 0x2c); c.imm(3, I2C_COMPLETE); c.a.and(3, 2, 3);
    c.zero(ccz::NE, 3, done);
    if wake {
        // Both lines are level-triggered and held until cleared, so a STOP or the
        // deadline between this check and WAITI fires at WAITI. A NACK holds
        // I2C_LINE until its STOP (I04, bounded), so that STOP is polled through.
        // The kernel vector clears INTENABLE and returns past WAITI.
        c.imm(3, (1 << 1) | (1 << I2C_LINE)); c.a.wsr(3, INTENABLE); c.a.rsync();
        c.a.waiti(0);
    }
    c.jump(poll);
    // I04: a NACK generates STOP. Let that bounded STOP finish before
    // reconfiguring the controller; its NACK bit remains latched (I18).
    c.bind(done);
    if let Some(at) = pending_transfer { c.set(at, 0); }
    c.imm(3, I2C_NACK); c.a.and(3, 2, 3); c.zero(ccz::NE, 3, nack);
    c.set(I2C0 + 0x24, I2C_COMPLETE | I2C_NACK);
}

fn panel_i2c_write(c: &mut Code<'_>, p: &Panel, t: &Timing, pending: Option<u32>, register: u8, value: u8, failed: usize) {
    c.feed();
    // I03/I04: RSTART, WRITE three ACK-checked bytes, STOP.
    i2c_list(c, &[p.expander << 1, register, value], &[6 << 11, (1 << 11) | (1 << 8) | 3, 2 << 11]);
    i2c_wait(c, pending, t.deadline(3, 1), failed, failed, STAMP);
}

// Panel::report: the expander's Input, Output and Config (E02-E05), one probe each from `id`.
// E07/I03/I04: pointer write, repeated START, address+R, one byte read with NACK, STOP; the byte
// waits in the RX FIFO. a13 holds it across EVENT, which uses a2..a11.
fn panel_registers(c: &mut Code<'_>, p: &Panel, t: &Timing, pending: Option<u32>, id: u8, failed: usize) {
    for (k, register) in [0x00, 0x01, 0x03].into_iter().enumerate() {
        c.feed();
        i2c_list(c, &[p.expander << 1, register, (p.expander << 1) | 1],
            &[6 << 11, (1 << 11) | (1 << 8) | 2, 6 << 11, (1 << 11) | (1 << 8) | 1, (3 << 11) | (1 << 10) | 1, 2 << 11]);
        i2c_wait(c, pending, t.deadline(4, 2), failed, failed, STAMP);
        c.load(13, I2C0 + 0x1c); c.a.extui(13, 13, 0, 8); probe(c, id + k as u8, 13);
    }
}

// Panel::report: STAMP's ticks since t0 over 1,024 as probe `id` (PANEL_PROBES).
fn panel_stage(c: &mut Code<'_>, id: u8) {
    c.call(STAMP); c.shr(13, 2, 10); probe(c, id, 13);
}

// Each transaction feeds before setup and every poll. Setup is at most
// 4 FIFO/flag writes + 3 TX writes + 8 command writes (3 instructions each),
// STAMP (25 + 3 per extra VALID poll), CALLX0 (2), and <=32 deadline/start
// instructions: <=128 including the pending check, excluding extra VALID polls.
// A poll adds <=24 outside STAMP, a wake <=29 (RSIL, INTENABLE, WAITI) and the
// kernel vector's seven; arming TARGET0 at a wait's start adds <=48 to its setup
// (six SYSTIMER writes, t0, a saturating add, two stores);
// motion RX unpack/commit is <=6*21+24=150.
// Touch's four-byte unpack, compare and latch take <=64 before the next feed;
// its idle/release paths are shorter. Touch ports are three-instruction leaves.
// Coalescing feeds per skipped period (8 instructions, 10 under radio).
// These stretches fit MAX_OP_INSNS;
// the transfer's full polling duration is deliberately not a handler allowance,
// just as the self-feeding Ethernet service is not. ACCEL/GYRO use the existing
// per-port MAX_OP_INSNS bound. Motion and touch dispatcher checks together add
// <=40 to the dispatcher prefix charged in RUNTIME_GAP_INSNS.
// Each sensor row is already included in TIMER_SCAN_INSNS.
fn i2c_transfer(c: &mut Code<'_>, pending: u32, addr: u8, t: &Timing, register: u8, write: Option<u8>, count: u8, failed: usize) {
    let list = |c: &mut Code<'_>| {
        if let Some(value) = write {
            i2c_list(c, &[addr << 1, register, value], &[6 << 11, (1 << 11) | (1 << 8) | 3, 2 << 11]);
            return (3, 1);
        }
        // I03/I04: address+W, pointer, repeated START, address+R, READ
        // ACK for all but last, READ NACK for last, STOP. Seven commands.
        let mut commands = vec![6 << 11, (1 << 11) | (1 << 8) | 2, 6 << 11, (1 << 11) | (1 << 8) | 1];
        if count > 1 { commands.push((3 << 11) | u32::from(count - 1)); }
        commands.extend([(3 << 11) | (1 << 10) | 1, 2 << 11]);
        i2c_list(c, &[addr << 1, register, (addr << 1) | 1], &commands);
        (3 + u32::from(count), 2)
    };
    i2c_transaction(c, pending, addr, t, failed, STAMP, |c| c.imm(5, u32::from(register)), list);
}

// Keep ownership after a timeout until STOP (facts-io I09/Q01). Only then may
// the client load its command list. On failure put its register in a5 for EVENT.
fn i2c_transaction(c: &mut Code<'_>, pending: u32, addr: u8, t: &Timing, failed: usize, clock: usize,
    fault_register: impl Fn(&mut Code<'_>), list: impl FnOnce(&mut Code<'_>) -> (u32, u32)) {
    c.feed();
    let nack = c.label(); let timeout = c.label(); let done = c.label();
    let idle = c.label(); c.load(2, pending); c.zero(ccz::EQ, 2, idle);
    c.load(2, I2C0 + 0x2c); c.imm(3, I2C_COMPLETE); c.a.and(2, 2, 3);
    c.zero(ccz::EQ, 2, timeout); c.set(pending, 0); c.bind(idle);
    let (bytes, starts) = list(c);
    i2c_wait(c, Some(pending), t.deadline(bytes, starts), nack, timeout, clock);
    c.jump(done);
    for (label, code) in [(nack, watch::FAULT_I2C_NACK), (timeout, watch::FAULT_I2C_TIMEOUT)] {
        c.bind(label);
        c.imm(2, u32::from(watch::FAULT)); c.imm(3, u32::from(code));
        c.imm(4, u32::from(addr)); fault_register(c); c.call(EVENT); c.jump(failed);
    }
    c.bind(done);
}

/// `MotionSram::initialized` once the sensors run; 0 before the reset, MOTION_RESETTING after it.
const MOTION_RUNNING: u32 = 1;
const MOTION_RESETTING: u32 = 2;

/// The QMI8658C keeps its power, and so its state, across an ESP-only reset: reset it first.
/// Rev A (qmi8658c-reva.txt) Table 27, lines 1314–1323: 0xB0 to RESET from any mode (its 7.4
/// text, line 1741, says 0x0B; the board's chip took 0xB0 and answered RESET_DONE). Then about
/// 200 us startup and at most 15 ms initialization with no access (3.3.1 lines 402–409, 7.4
/// line 1745): Note, 16 ms covers both with 0.8 ms for the "about". The timer row waits it out.
fn motion_reset(c: &mut Code<'_>, s: &MotionSram, sensor: Motion, pending: u32, t: &Timing) {
    let failed = c.label();
    i2c_transfer(c, pending, sensor.addr, t, 0x60, Some(0xb0), 0, failed);
    c.set(s.initialized, MOTION_RESETTING);
    let wait_ticks: u32 = 16_000 * 16;
    c.call(STAMP); c.imm(4, wait_ticks); c.imm(5, 0); c.add64(2, 3, 4, 5, 6, 7);
    c.store(2, s.timer); c.store(3, s.timer + 4); c.feed(); c.jump(TURN);
    c.bind(failed); c.set(s.timer + 16, 0); c.feed(); c.jump(TURN);
}

fn motion_init(c: &mut Code<'_>, s: &MotionSram, sensor: Motion, odr: u8, pending: u32, t: &Timing) {
    let failed = c.label();
    // QMI8658C Rev 0.6: CTRL1 0x02 AI bit6 (1109–1114), BE=0
    // (1115–1116; the model's cited lead decision applies byte order to I2C), first, so
    // the reset flag below reads as stored either way.
    i2c_transfer(c, pending, sensor.addr, t, 0x02, Some(0x40), 0, failed);
    // Rev A 7.4 (lines 1749–1752): 0x4D reads 0x80 after a successful reset, until the
    // sensors are enabled. Anything else disarms motion.
    i2c_transfer(c, pending, sensor.addr, t, 0x4d, None, 1, failed);
    c.load(2, I2C0 + 0x1c); c.a.extui(2, 2, 0, 8); c.imm(3, 0x80); c.branch(cc::NE, 2, 3, failed);
    // CTRL2/3 ranges and ODR (1120–1141,1156–1180), CTRL7 syncSmpl
    // bit7 and gyro/accel enables (1259–1262,1277–1280).
    for (register, value) in [(0x03, (crate::motion::ACCEL_FS << 4) | odr),
        (0x04, (crate::motion::GYRO_FS << 4) | odr), (0x08, 0x83)]
    { i2c_transfer(c, pending, sensor.addr, t, register, Some(value), 0, failed); }
    c.set(s.initialized, MOTION_RUNNING);
    // Wait via the same timer row: gyro's 60 ms + 3/ODR settling exceeds
    // accel's 3 ms + 3/ODR (2110–2119). First sample follows one period.
    let settle_ticks = (60_000 + 3 * (125u64 << odr)) * 16;
    c.call(STAMP); c.imm(4, settle_ticks as u32); c.imm(5, (settle_ticks >> 32) as u32);
    c.add64(2, 3, 4, 5, 6, 7);
    c.load(4, s.timer + 8); c.load(5, s.timer + 12); c.add64(2, 3, 4, 5, 6, 7);
    c.store(2, s.timer); c.store(3, s.timer + 4); c.feed(); c.jump(TURN);
    c.bind(failed); c.set(s.timer + 16, 0); c.feed(); c.jump(TURN);
}

fn motion_sample(c: &mut Code<'_>, s: &MotionSram, sensor: Motion, pending: u32, t: &Timing) {
    c.symbols.motion_start = c.here();
    // STATUS0 is 0x2e (map 934–938); bits 0/1 are aDA/gDA
    // (1397–1404). Reading it locks output (1887–1896). No documented
    // combined status/data burst: use two transactions, then all six axes.
    i2c_transfer(c, pending, sensor.addr, t, 0x2e, None, 1, TURN);
    c.load(2, I2C0 + 0x1c); c.a.extui(2, 2, 0, 2); c.store(2, s.status);
    // AX_L..GZ_H (0x35..0x40), map 948–969. CTRL1 AI increments
    // between bytes (1113–1114); reading GZ_H releases the lock (1897–1900).
    // Always finish the read, even with no new data, so the lock is released.
    i2c_transfer(c, pending, sensor.addr, t, 0x35, None, 12, TURN);
    c.load(4, s.status);
    for axis in 0..6 {
        c.load(2, I2C0 + 0x1c); c.load(3, I2C0 + 0x1c);
        c.a.extui(2, 2, 0, 8); c.a.extui(3, 3, 0, 8);
        c.shl(3, 3, 8); c.a.or(2, 2, 3);
        // Sign-extend the raw 16-bit count with Xtensa shift instructions.
        c.shl(2, 2, 16); c.imm(11, 16); c.a.ssr(11); c.a.sra(2, 2);
        let stale = c.label(); c.zero(ccz::EQ, 4, stale);
        c.store(2, s.sample + axis * 4); c.bind(stale);
    }
    c.feed(); c.call(STAMP); // handler clock includes the bounded transfer.
    c.symbols.motion_end = c.here();
}

fn motion_port(c: &mut Code<'_>, l: &SramLayout, port: u16) {
    let s = l.motion.as_ref().expect("verified motion port declaration");
    c.routine(PORT + port as usize, ports::PORTS[port as usize].name);
    let bad = c.label(); c.imm(3, 3); c.branch(cc::GEU, 2, 3, bad);
    c.shl(2, 2, 2); c.address(3, s.sample + if port == ports::GYRO { 12 } else { 0 });
    c.a.add_n(2, 2, 3); c.a.l32i(2, 2, 0); c.a.ret_n();
    c.bind(bad); c.store(0, l.port_return);
    c.a.mov_n(5, 2); c.a.mov_n(4, 10); c.imm(3, u32::from(watch::FAULT_RANGE));
    c.imm(2, u32::from(watch::FAULT)); c.call(EVENT);
    c.imm(2, 0); c.load(0, l.port_return); c.a.ret_n();
}

fn touch_init(c: &mut Code<'_>, s: &TouchSram, sensor: Touch, scan: u8, pending: u32, t: &Timing) {
    let failed = c.label();
    // facts-parts T09/T17: disable autosleep before polls; the adverse model
    // refuses I2C in standby. T12: NorScanPer in 10 ms units, rounded up in
    // image.rs. T14: LongPressTime (default 10 s) and AutoReset (default not
    // documented) restart the controller on their own, into the 100 ms
    // reinitialization of T18 that a poll cannot see coming; zero disables
    // each. T01/T02/O04: address/framing are Assumed CST816-family evidence.
    for (register, value) in [(0xfe, 1), (0xee, scan), (0xfc, 0), (0xfb, 0)] {
        i2c_transfer(c, pending, sensor.addr, t, register, Some(value), 0, failed);
    }
    c.set(s.initialized, 1);
    c.call(STAMP); c.load(4, s.timer + 8); c.load(5, s.timer + 12);
    c.add64(2, 3, 4, 5, 6, 7);
    c.store(2, s.timer); c.store(3, s.timer + 4); c.feed(); c.jump(TURN);
    // As with motion, an init failure disarms this device. An unfinished
    // command still owns the shared pending word until COMPLETE (I09/I18).
    c.bind(failed); c.set(s.timer + 16, 0); c.feed(); c.jump(TURN);
}

fn touch_sample(c: &mut Code<'_>, s: &TouchSram, sensor: Touch, pending: u32, t: &Timing) {
    c.symbols.touch_start = c.here();
    let release = c.label(); let changed = c.label(); let unchanged = c.label(); let done = c.label();
    // T03: FingerNum is the full byte, zero means no finger. Do not read
    // coordinates before a sampled touch: T04-T07 give no reset values.
    i2c_transfer(c, pending, sensor.addr, t, 0x02, None, 1, unchanged);
    c.load(2, I2C0 + 0x1c); c.a.extui(2, 2, 0, 8); c.zero(ccz::EQ, 2, release);
    // T04-T07/T15: exactly four bytes XH/XL/YH/YL; mask the documented
    // 12-bit fields, never read the undocumented 0x07. T02/O04: consecutive
    // increment is Assumed. T15 gives no atomic snapshot: adverse choice is
    // to use the bytes actually read, without inventing a controller latch.
    i2c_transfer(c, pending, sensor.addr, t, 0x03, None, 4, unchanged);
    for dst in [4, 5] {
        c.load(2, I2C0 + 0x1c); c.load(3, I2C0 + 0x1c);
        c.a.extui(2, 2, 0, 4); c.a.extui(3, 3, 0, 8);
        c.shl(2, 2, 8); c.a.or(2, 2, 3); c.a.mov_n(dst, 2);
    }
    c.imm(6, 1);
    c.load(2, s.report + 8); c.zero(ccz::EQ, 2, changed);
    c.load(2, s.report); c.branch(cc::NE, 2, 4, changed);
    c.load(2, s.report + 4); c.branch(cc::NE, 2, 5, changed); c.jump(unchanged);
    c.bind(release);
    c.load(2, s.report + 8); c.zero(ccz::EQ, 2, unchanged);
    c.load(4, s.report); c.load(5, s.report + 4); c.imm(6, 0);
    c.bind(changed);
    c.store(4, s.report); c.store(5, s.report + 4); c.store(6, s.report + 8);
    c.imm(8, 1); c.jump(done);
    c.bind(unchanged); c.imm(8, 0);
    c.bind(done); c.feed(); c.call(STAMP); // preserves a8, includes transfer time.
    c.symbols.touch_end = c.here();
    c.zero(ccz::EQ, 8, TURN);
}

fn touch_port(c: &mut Code<'_>, l: &SramLayout, port: u16) {
    let s = l.touch.as_ref().expect("verified touch port declaration");
    c.routine(PORT + port as usize, ports::PORTS[port as usize].name);
    c.load(2, s.report + u32::from(port - ports::TOUCH_X) * 4); c.a.ret_n();
}

fn panel_init(c: &mut Code<'_>, l: &SramLayout, p: &Panel, t: &Timing) {
    let memory = l.panel.as_ref().expect("declared panel SRAM");
    let failed = c.label(); let done = c.label();
    // Brief steps 1, 2, 5 precede 3, 4, 6: even a missing expander leaves
    // all routed SPI outputs backed by a configured controller.
    let gates = (1 << 6) | (1 << 22);
    bits(c, 0x600c_0020, 0, gates); bits(c, 0x600c_0024, 0, 1 << 6);
    bits(c, 0x600c_0018, 0, gates); bits(c, 0x600c_001c, 0, 1 << 6);
    bits(c, 0x600c_0020, gates, 0); bits(c, 0x600c_0024, 1 << 6, 0);
    for (pin, signal) in [(p.sck, 101), (p.mosi, 103), (p.cs, 110)]
    {
        c.set(GPIO + 0x74 + pin * 4, 0);
        c.set(IO_MUX + 4 + pin * 4, (1 << 12) | (1 << 9));
        c.set(GPIO + 0x554 + pin * 4, signal);
        c.set(GPIO + 0x24 + u32::from(pin >= 32) * 12, 1 << (pin & 31));
    }
    // S08: use XTAL at 40 MHz, divider bypass; mode 0 and CS0 only (S09/S10).
    // S11/D-S2..D-S4: WR_BIT_ORDER=0, eight-bit command, one lane per phase.
    c.set(SPI2 + 0x0c, 1 << 31); c.set(SPI2 + 0x08, 0x003c_0000);
    c.set(SPI2 + 0x14, (23 << 27) | (1 << 22) | (1 << 16) | 7);
    c.set(SPI2 + 0x20, 0x3e); c.set(SPI2 + 0xe8, 1 << 1);
    c.set(GDMA + 0x60, 12); c.set(GDMA + 0x64, 1 << 12); c.set(GDMA + 0xa8, 0);
    c.set(SPI2 + 0x38, SPI_DONE);
    // A transfer's end wakes its WAITI (spi_send): TRANS_DONE (bit 12) enabled once in
    // SPI_DMA_INT_ENA_REG +0x34 (facts-io.md:733,745), routed to SPI_LINE. INTENABLE
    // holds the line only while a send waits.
    if !c.radio { c.set(SPI2 + 0x34, SPI_DONE); c.set(SPI2_MAP, SPI_LINE); }

    // E03/E05/R02: set the latch before enabling EXIO0's low output.
    // Lead choice: every other expander pin remains a power-on input (R07),
    // including TP_RESET on EXIO2 (R02); touch uses its T18 power-on startup.
    let low = !(1u8 << p.reset_bit);
    if p.report { panel_registers(c, p, t, l.i2c_pending, PANEL_PROBES, failed); }
    panel_i2c_write(c, p, t, l.i2c_pending, 0x01, low, failed);
    panel_i2c_write(c, p, t, l.i2c_pending, 0x03, low, failed);
    boot_delay(c, l, memory.scratch, 20); // P06: strictly more than 10 us.
    panel_i2c_write(c, p, t, l.i2c_pending, 0x01, 0xff, failed);
    boot_delay(c, l, memory.scratch, 120_000); // P06, hardware reset.
    if p.report {
        panel_stage(c, PANEL_PROBES + 3);
        panel_registers(c, p, t, l.i2c_pending, PANEL_PROBES + 4, failed);
    }
    spi_command(c, 0x01, &[]); // P09, SWRESET from unknown sleep state.
    boot_delay(c, l, memory.scratch, 120_000);
    if p.report { panel_stage(c, PANEL_PROBES + 7); }
    spi_command(c, 0x11, &[]);
    // Lead choice: 120 ms covers P10's 5 ms and P07's 60 ms to DISPON.
    boot_delay(c, l, memory.scratch, 120_000);
    if p.report { panel_stage(c, PANEL_PROBES + 8); }
    // P23: SPI_WRAM lets SPI writes reach frame RAM; every reset clears it. P24: BCTRL makes DBV
    // take effect; its reset default is in conflict (D-P6), and DD (dimming, untimed) stays off.
    for (command, data) in [(0xc4, &[0x80][..]), (0x3a, &[0x55][..]), (0x53, &[0x20][..]),
        (0x51, &[p.brightness][..]), (0x20, &[][..]), (0x29, &[][..])]
    { spi_command(c, command, data); }
    if p.report { panel_stage(c, PANEL_PROBES + 9); }
    c.set(memory.available, 1); c.jump(done);
    c.bind(failed);
    // The expander's writes, and a report's reads, belong to bring-up step 4.
    c.imm(2, u32::from(watch::FAULT)); c.imm(3, u32::from(watch::FAULT_PANEL));
    c.imm(4, 0); c.imm(5, 4); c.call(EVENT);
    c.bind(done);
}

fn spi_command(c: &mut Code<'_>, command: u8, data: &[u8]) {
    let mut word = [0; 4]; word[..data.len()].copy_from_slice(data);
    if !data.is_empty() { c.set(SPI2 + 0x98, u32::from_le_bytes(word)); }
    c.imm(2, data.len() as u32); spi_send(c, command, false);
}

// a2 is the exact byte count. W0 was filled for a CPU transfer, or OUT0 was
// started over the exact descriptor list for DMA. No transfer is outstanding
// when this returns, so the next draw may safely rewrite its SRAM (S05/S14).
fn spi_send(c: &mut Code<'_>, command: u8, dma: bool) {
    c.set(SPI2 + 4, u32::from(command) << 16); // Table 30.5-10: ADDR_VALUE[31:8].
    c.symbols.panel_commands.push(c.here() - 3);
    c.set(SPI2 + 0x18, (7 << 28) | (1 << 27) | 0x02);
    c.imm(3, (1 << 31) | (1 << 30));
    let no_data = c.label(); let configured = c.label(); c.zero(ccz::EQ, 2, no_data);
    c.shl(2, 2, 3); c.a.addi(2, 2, -1);
    c.imm(4, 1 << 27); c.a.or(3, 3, 4); c.jump(configured);
    c.bind(no_data); c.imm(2, 0); c.bind(configured);
    c.store(2, SPI2 + 0x1c); c.store(3, SPI2 + 0x10);
    c.set(SPI2 + 0x30, if dma { 1 << 28 } else { 0 });
    c.set(SPI2, 1 << 23); c.set(SPI2, 1 << 24); // S06 UPDATE then USR.
    let wait = c.label(); let done = c.label();
    if c.mic && !c.radio {
        // A draw may run in boot or an interruptible handler. Save its PS and
        // enabled lines before borrowing WAITI, keeping capture live throughout.
        c.a.op24(enc::rrr(0, 0, 0, 6, 15, 5));
        c.a.op24(enc::rsr(0, 3, 0, INTENABLE, 4));
    }
    c.bind(wait);
    if c.radio {
        // Under radio the dispatcher blocks only through the radio kernel at the
        // sites the image names (RadioSymbols::block_calls), and SPI_LINE is the
        // island's to route; a send polls, fed each turn: at most 15 instructions
        // a poll, the eight-instruction feed included.
        c.feed(); c.load(2, SPI2 + 0x3c); c.imm(3, SPI_DONE); c.a.and(2, 2, 3);
        c.zero(ccz::EQ, 2, wait);
    } else {
        // RSIL a2,15 after a wake restores the masked convention (ISA Table 201
        // p.662). The transfer's end is level-triggered and held until its clear,
        // so an end between this check and WAITI fires at WAITI: none is missed.
        // The kernel vector clears INTENABLE and returns past WAITI. One feed a
        // wake: the longest transfer SPI_MS_DATA_BITLEN allows (2^18 bits, one
        // lane at the 40 MHz clock of S08) is 6.6 ms, inside the RWDT hold's
        // 100 ms sleep cap (wdt.rs).
        if !c.symbols.board_unverified.contains(&SPI_WAITI) { c.symbols.board_unverified.push(SPI_WAITI); }
        c.a.op24(enc::rrr(0, 0, 0, 6, 15, 2));
        c.load(2, SPI2 + 0x3c); c.imm(3, SPI_DONE); c.a.and(2, 2, 3);
        c.zero(ccz::NE, 2, done);
        c.imm(2, (1 << SPI_LINE) | if c.mic { mic::LINE_BIT } else { 0 }); c.a.wsr(2, INTENABLE); c.a.rsync();
        c.feed(); c.a.waiti(0); c.jump(wait);
    }
    // Completion is observed before any buffer or descriptor can be changed.
    c.bind(done); c.set(SPI2 + 0x38, SPI_DONE);
    if c.mic && !c.radio {
        c.a.wsr(4, INTENABLE); c.a.rsync(); c.a.wsr(5, 230); c.a.rsync();
    }
}

// Display scratch word offsets. Boot uses only the first two for its deadline.
const DRAW_SITE: u32 = 0;
const DRAW_X: u32 = 4;
const DRAW_Y: u32 = 8;
const DRAW_W: u32 = 12;
const DRAW_H: u32 = 16;
const DRAW_SOURCE: u32 = 20;
const DRAW_STRIDE: u32 = 24;
const DRAW_COLOR: u32 = 28;
const DRAW_ROWS: u32 = 32;
const DRAW_BYTES: u32 = 36;
const DRAW_DX: u32 = 40;
const DRAW_DY: u32 = 44;
const DRAW_MAX_ROWS: u32 = 48;

fn draw_port(c: &mut Code<'_>, l: &SramLayout, panel: Option<&Panel>, link: &Link, port: u16) {
    c.routine(PORT + port as usize, ports::PORTS[port as usize].name);
    c.store(0, l.port_return);
    let Some(panel) = panel else {
        // sched::Io checks the undeclared display before inspecting any argument.
        c.a.mov_n(4, 10); c.imm(5, DrawRefusal::Handle as u32);
        c.imm(3, u32::from(watch::FAULT_DRAW)); c.imm(2, u32::from(watch::FAULT)); c.call(EVENT);
        c.load(0, l.port_return); c.a.ret_n(); return;
    };
    let p = l.panel.as_ref().expect("declared panel SRAM"); let s = p.scratch;
    let handle = c.label(); let outside = c.label(); let odd = c.label();
    let report = c.label(); let done = c.label();
    c.store(10, s + DRAW_SITE); c.zero(ccz::NE, 2, handle);
    let present = port == ports::PRESENT;
    if present {
        c.store(8, s + DRAW_X); c.store(9, s + DRAW_Y);
        c.store(6, s + DRAW_W); c.store(7, s + DRAW_H);
        // The verifier proves a constant U16 buffer source with a 2-D extent.
        c.shl(3, 3, BUFFER_ROW_SHIFT); c.address(2, link.buffers); c.a.add_n(3, 3, 2);
        c.a.l32i(2, 3, 0); c.a.l32i(8, 3, 16); c.a.l32i(9, 3, 4); c.a.quou(9, 9, 8);
        for r in [4, 5, 6, 7] { c.zero(ccz::LT, r, outside); }
        // Nonnegative source operands: subtract from the extent first, so
        // sx+w and sy+h never overflow, even for full-range i32 arguments.
        c.branch(cc::LTU, 8, 4, outside); c.branch(cc::LTU, 9, 5, outside);
        c.a.sub(3, 8, 4); c.branch(cc::LTU, 3, 6, outside);
        c.a.sub(3, 9, 5); c.branch(cc::LTU, 3, 7, outside);
        c.shl(8, 8, 1); c.store(8, s + DRAW_STRIDE);
        c.a.mull(5, 5, 8); c.a.addx2(4, 4, 2); c.a.add_n(4, 4, 5);
        c.store(4, s + DRAW_SOURCE);
    } else {
        c.store(3, s + DRAW_X); c.store(4, s + DRAW_Y);
        c.store(5, s + DRAW_W); c.store(6, s + DRAW_H);
        c.zero(ccz::LT, 5, outside); c.zero(ccz::LT, 6, outside);
        // Little-endian S16I must place the RGB565 high byte first on the wire.
        c.a.extui(2, 7, 8, 8); c.a.extui(3, 7, 0, 8); c.shl(3, 3, 8);
        c.a.or(2, 2, 3); c.store(2, s + DRAW_COLOR);
    }
    clip_axis(c, s + DRAW_X, s + DRAW_W, s + DRAW_DX, panel.width, done);
    clip_axis(c, s + DRAW_Y, s + DRAW_H, s + DRAW_DY, panel.height, done);
    c.load(2, s + DRAW_X);
    for field in [DRAW_Y, DRAW_W, DRAW_H] { c.load(3, s + field); c.a.or(2, 2, 3); }
    c.a.extui(2, 2, 0, 1); c.zero(ccz::NE, 2, odd);
    // Lead choice: a failed panel still performs every reference validation.
    c.load(2, p.available); c.zero(ccz::EQ, 2, done); c.feed();
    if present {
        c.load(2, s + DRAW_SOURCE); c.load(3, s + DRAW_DX); c.a.addx2(2, 3, 2);
        c.load(3, s + DRAW_DY); c.load(4, s + DRAW_STRIDE); c.a.mull(3, 3, 4);
        c.a.add_n(2, 2, 3); c.store(2, s + DRAW_SOURCE);
    }
    // S03 strip limit, rounded down to even rows. P19 limits width to 480
    // (voicebox 368), hence this is always at least two for a nonempty window.
    c.imm(2, panel::BUFFER_BYTES / 2); c.load(3, s + DRAW_W); c.a.quou(2, 2, 3);
    c.imm(3, !1); c.a.and(2, 2, 3); c.load(3, s + DRAW_H);
    let rows_fit = c.label(); c.branch(cc::LTU, 2, 3, rows_fit); c.a.mov_n(2, 3); c.bind(rows_fit);
    c.store(2, s + DRAW_MAX_ROWS);
    if !present {
        c.load(3, s + DRAW_W); c.a.mull(4, 2, 3);
        fill_pixels(c, p); // Fill the first (largest) strip just once.
    }
    let strip = c.label(); c.bind(strip); c.feed();
    c.load(2, s + DRAW_MAX_ROWS); c.load(3, s + DRAW_H);
    let remaining = c.label(); c.branch(cc::LTU, 2, 3, remaining); c.a.mov_n(2, 3); c.bind(remaining);
    c.store(2, s + DRAW_ROWS); c.load(3, s + DRAW_W); c.a.mull(3, 2, 3);
    c.shl(3, 3, 1); c.store(3, s + DRAW_BYTES);
    if present { copy_pixels(c, p); }
    c.feed();
    for (command, origin, size, offset) in [(0x2a, DRAW_X, DRAW_W, panel.column_offset),
        (0x2b, DRAW_Y, DRAW_ROWS, panel.row_offset)]
    {
        c.load(2, s + origin); c.imm(3, offset); c.a.add_n(2, 2, 3); c.load(3, s + size);
        spi_window(c); c.imm(2, 4); spi_send(c, command, false);
    }
    descriptors(c, p);
    c.set(GDMA + 0x80, (p.descriptors & 0xfffff) | (1 << 21)); // G07, OUT0 START.
    c.load(2, s + DRAW_BYTES); spi_send(c, 0x2c, true);
    c.load(2, s + DRAW_ROWS); c.load(3, s + DRAW_Y); c.a.add_n(3, 3, 2); c.store(3, s + DRAW_Y);
    c.load(3, s + DRAW_H); c.a.sub(3, 3, 2); c.store(3, s + DRAW_H);
    c.zero(ccz::NE, 3, strip); c.jump(done);
    c.bind(handle); c.imm(5, DrawRefusal::Handle as u32); c.jump(report);
    c.bind(outside); c.imm(5, DrawRefusal::SourceOutside as u32); c.jump(report);
    c.bind(odd); c.imm(5, DrawRefusal::OddWindow as u32);
    c.bind(report); c.load(4, s + DRAW_SITE);
    c.imm(3, u32::from(watch::FAULT_DRAW)); c.imm(2, u32::from(watch::FAULT)); c.call(EVENT);
    c.bind(done); c.load(0, l.port_return); c.a.ret_n();
}

// Equivalent to the reference's i64 clipping without computing x+w: if x is
// negative, its unsigned magnitude must be less than w or the result is empty
// (including i32::MIN). Subtract that magnitude, then clamp to limit-max(x,0).
// With nonnegative w <= i32::MAX, none of these operations can overflow.
fn clip_axis(c: &mut Code<'_>, origin: u32, size: u32, delta: u32, limit: u32, empty: usize) {
    c.load(2, origin); c.load(3, size); c.imm(4, 0);
    let positive = c.label(); let clipped = c.label(); c.zero(ccz::GE, 2, positive);
    c.a.neg(4, 2); c.branch(cc::GEU, 4, 3, empty);
    c.a.sub(3, 3, 4); c.imm(2, 0); c.bind(positive);
    c.imm(5, limit); c.branch(cc::GEU, 2, 5, empty); c.a.sub(5, 5, 2);
    c.branch(cc::LTU, 3, 5, clipped); c.a.mov_n(3, 5); c.bind(clipped);
    c.zero(ccz::EQ, 3, empty); c.store(2, origin); c.store(3, size); c.store(4, delta);
}

// a4 pixels. Four instructions per pixel, at most 256 per feed; chunk setup,
// branches and the feed keep the maximum stretch below 1100 < MAX_OP_INSNS.
fn fill_pixels(c: &mut Code<'_>, p: &PanelSram) {
    c.address(2, p.buffer); c.load(3, p.scratch + DRAW_COLOR);
    let batch = c.label(); let sized = c.label(); let pixel = c.label(); c.bind(batch); c.feed();
    c.imm(5, 256); c.branch(cc::GEU, 4, 5, sized); c.a.mov_n(5, 4); c.bind(sized);
    c.a.sub(4, 4, 5); c.bind(pixel);
    c.a.s16i(3, 2, 0); c.a.addi(2, 2, 2); c.a.addi(5, 5, -1); c.zero(ccz::NE, 5, pixel);
    c.zero(ccz::NE, 4, batch);
}

fn copy_pixels(c: &mut Code<'_>, p: &PanelSram) {
    let s = p.scratch;
    c.load(2, s + DRAW_SOURCE); c.address(3, p.buffer); c.load(4, s + DRAW_W);
    c.load(5, s + DRAW_ROWS); c.load(7, s + DRAW_STRIDE); c.shl(6, 4, 1); c.a.sub(7, 7, 6);
    c.imm(8, 64); c.feed();
    let pixel = c.label(); let fed = c.label(); let done = c.label(); c.bind(pixel);
    // SRAM and DROM both hold little-endian U16 pixels (see load_element).
    // Swap into wire order, including at an odd source x. At most 22
    // instructions per pixel even at a row boundary; each 64 pixels plus
    // <128 setup/finish instructions stay below MAX_OP_INSNS.
    c.a.l16ui(6, 2, 0);
    c.a.extui(9, 6, 8, 8); c.a.extui(6, 6, 0, 8); c.shl(6, 6, 8); c.a.or(6, 6, 9);
    c.a.s16i(6, 3, 0);
    c.a.addi(2, 2, 2); c.a.addi(3, 3, 2); c.a.addi(4, 4, -1); c.a.addi(8, 8, -1);
    c.zero(ccz::NE, 8, fed); c.feed(); c.imm(8, 64); c.bind(fed);
    c.zero(ccz::NE, 4, pixel);
    c.a.add_n(2, 2, 7); c.a.addi(5, 5, -1); c.zero(ccz::EQ, 5, done);
    c.load(4, s + DRAW_W); c.jump(pixel); c.bind(done); c.store(2, s + DRAW_SOURCE);
}

// a2 start, a3 size. P15/P16's four big-endian parameter bytes are packed
// into W0's low-byte-first word (S04); all register accesses remain words.
fn spi_window(c: &mut Code<'_>) {
    c.a.add_n(3, 2, 3); c.a.addi(3, 3, -1);
    c.shr(4, 2, 8); c.a.extui(5, 2, 0, 8); c.shl(5, 5, 8); c.a.or(4, 4, 5);
    c.a.extui(5, 3, 8, 8); c.shl(5, 5, 16); c.a.or(4, 4, 5);
    c.a.extui(5, 3, 0, 8); c.shl(5, 5, 24); c.a.or(4, 4, 5); c.store(4, SPI2 + 0x98);
}

fn descriptors(c: &mut Code<'_>, p: &PanelSram) {
    c.feed(); c.load(2, p.scratch + DRAW_BYTES); c.address(3, p.buffer); c.address(4, p.descriptors);
    let descriptor = c.label(); let sized = c.label(); let more = c.label(); c.bind(descriptor);
    c.imm(5, panel::DESCRIPTOR_BYTES); c.branch(cc::GEU, 2, 5, sized); c.a.mov_n(5, 2); c.bind(sized);
    c.a.sub(2, 2, 5); c.shl(6, 5, 12); c.a.or(6, 6, 5);
    c.imm(8, 1 << 31); c.a.or(6, 6, 8); c.a.addi(7, 4, 12);
    c.zero(ccz::NE, 2, more); c.imm(7, 0); c.imm(8, 1 << 30); c.a.or(6, 6, 8); c.bind(more);
    // G02/G03a lead decision: three little-endian CPU words, owner on each,
    // EOF only on the last, length == size. S05: sum == this strip's bytes.
    // At most nine descriptors * 25 instructions, well below MAX_OP_INSNS.
    c.a.s32i(6, 4, 0); c.a.s32i(3, 4, 4); c.a.s32i(7, 4, 8);
    c.a.add_n(3, 3, 5); c.a.mov_n(4, 7); c.zero(ccz::NE, 2, descriptor);
}

// Non-reentrant buffer ports preserve arguments across a watch Fault frame.
const BUF_DST_HANDLE: u32 = 0;
const BUF_DST_OFF: u32 = 4;
const BUF_SRC_HANDLE: u32 = 8;
const BUF_SRC_OFF: u32 = 12;
const BUF_N: u32 = 16;
const BUF_SITE: u32 = 20;
const BUF_VALUE: u32 = 24;
const BUF_DST: u32 = 28;
const BUF_SRC: u32 = 32;
const BUF_COUNT: u32 = 36;
const BUF_KIND: u32 = 40;
const BUF_STEP: u32 = 44;
const BUF_BAD: u32 = 48;
pub(crate) const BUFFER_SCRATCH_BYTES: u32 = 52;

// rt::sched::buffer_range: clamp start and count independently, remembering
// invalid signed inputs. Compare n to len - start, never form off + n in u32.
// The module verifier proves the handles and COPY's matching element kinds.
fn buffer_range(c: &mut Code<'_>, s: u32, link: &Link, source: bool) {
    c.load(2, s + if source { BUF_SRC_HANDLE } else { BUF_DST_HANDLE });
    c.shl(2, 2, BUFFER_ROW_SHIFT); c.address(3, link.buffers); c.a.add_n(2, 2, 3);
    c.a.l32i(6, 2, 0); c.a.l32i(7, 2, 4); c.a.l32i(8, 2, 12);
    if !source {
        c.a.l32i(5, 2, 8); c.store(5, s + BUF_KIND); c.store(8, s + BUF_STEP);
    }
    c.load(2, s + if source { BUF_SRC_OFF } else { BUF_DST_OFF });
    c.load(3, s + BUF_N); c.load(4, s + BUF_BAD);
    let start_nonnegative = c.label(); let count_nonnegative = c.label();
    c.zero(ccz::GE, 2, start_nonnegative); c.imm(2, 0); c.imm(4, 1); c.bind(start_nonnegative);
    c.zero(ccz::GE, 3, count_nonnegative); c.imm(3, 0); c.imm(4, 1); c.bind(count_nonnegative);
    let start_fits = c.label(); let count_fits = c.label();
    c.branch(cc::GEU, 7, 2, start_fits); c.a.mov_n(2, 7); c.imm(4, 1); c.bind(start_fits);
    c.a.sub(5, 7, 2);
    c.branch(cc::GEU, 5, 3, count_fits); c.a.mov_n(3, 5); c.imm(4, 1); c.bind(count_fits);
    c.a.mull(2, 2, 8); c.a.add_n(2, 2, 6);
    c.store(2, s + if source { BUF_SRC } else { BUF_DST });
    if source {
        c.load(6, s + BUF_COUNT); let smaller = c.label();
        c.branch(cc::GEU, 6, 3, smaller); c.a.mov_n(3, 6); c.bind(smaller);
    }
    c.store(3, s + BUF_COUNT); c.store(4, s + BUF_BAD);
}

fn buffer_port(c: &mut Code<'_>, l: &SramLayout, link: &Link, port: u16) {
    c.routine(PORT + port as usize, ports::PORTS[port as usize].name);
    let s = l.buffer_scratch;
    let copy = port == ports::COPY;
    c.store(0, l.port_return); c.store(10, s + BUF_SITE);
    c.store(2, s + BUF_DST_HANDLE); c.store(3, s + BUF_DST_OFF);
    if copy {
        c.store(4, s + BUF_SRC_HANDLE); c.store(5, s + BUF_SRC_OFF); c.store(6, s + BUF_N);
    } else { c.store(4, s + BUF_N); c.store(5, s + BUF_VALUE); }
    c.set(s + BUF_BAD, 0); c.feed();
    buffer_range(c, s, link, false);
    if copy { buffer_range(c, s, link, true); }
    let reported = c.label();
    c.load(2, s + BUF_BAD); c.zero(ccz::EQ, 2, reported);
    c.load(4, s + BUF_SITE); c.shr(3, 4, 5); c.shl(3, 3, 2);
    c.address(6, l.range_faults); c.a.add_n(6, 6, 3); c.a.l32i(2, 6, 0);
    c.a.extui(7, 4, 0, 5); c.imm(8, 1); c.a.ssl(7); c.a.sll(8, 8);
    c.a.and(7, 2, 8); c.zero(ccz::NE, 7, reported);
    c.a.or(2, 2, 8); c.a.s32i(2, 6, 0);
    c.load(5, s + BUF_N); c.imm(3, u32::from(watch::FAULT_RANGE));
    c.imm(2, u32::from(watch::FAULT)); c.call(EVENT); c.bind(reported);
    let done = c.label();
    c.load(2, s + BUF_DST); c.load(4, s + BUF_COUNT); c.load(5, s + BUF_STEP);
    c.zero(ccz::EQ, 4, done);
    if copy {
        c.load(3, s + BUF_SRC);
        let forward = c.label();
        c.load(6, s + BUF_DST_HANDLE); c.load(7, s + BUF_SRC_HANDLE);
        c.branch(cc::NE, 6, 7, forward); c.branch(cc::GEU, 3, 2, forward);
        c.a.addi(6, 4, -1); c.a.mull(6, 6, 5);
        c.a.add_n(2, 2, 6); c.a.add_n(3, 3, 6); c.a.neg(5, 5); c.bind(forward);
    } else { c.load(3, s + BUF_VALUE); }
    c.load(8, s + BUF_KIND);
    for elem in [Elem::Word, Elem::U8, Elem::U16, Elem::I16] {
        let next = c.label(); let element = c.label();
        c.imm(6, elem as u32); c.branch(cc::NE, 8, 6, next);
        c.bind(element);
        // Feed uses only a9..a11. Feeding every 128 elements keeps a gap
        // within 128 * 9 loop instructions plus the feed, independently of
        // count; the port also feeds on entry and on return.
        let fed = c.label(); c.a.extui(7, 4, 0, 7); c.zero(ccz::NE, 7, fed); c.feed(); c.bind(fed);
        if copy {
            crate::emit::load_element(c.a, elem, 6, 3);
            crate::emit::store_element(c.a, elem, 6, 2); c.a.add_n(3, 3, 5);
        } else { crate::emit::store_element(c.a, elem, 3, 2); }
        c.a.add_n(2, 2, 5); c.a.addi(4, 4, -1); c.zero(ccz::NE, 4, element);
        c.jump(done); c.bind(next);
    }
    c.bind(done); c.feed(); c.load(0, l.port_return); c.a.ret_n();
}

fn port_routines(c: &mut Code<'_>, l: &SramLayout, link: &Link) {
    for read in [false, true] {
        let port = if read { ports::PIN_READ } else { ports::PIN_WRITE };
        c.routine(PORT + port as usize, ports::PORTS[port as usize].name);
        c.store(0, l.port_return);
        let bad = c.label(); let report = c.label();
        c.imm(4, 49); c.branch(cc::GEU, 2, 4, bad);
        c.address(4, link.roles); c.a.add_n(4, 4, 2); c.a.l8ui(4, 4, 0);
        if read { c.zero(ccz::EQ, 4, bad); }
        else { c.imm(5, 2); c.branch(cc::NE, 4, 5, bad); }
        c.a.extui(5, 2, 0, 5); c.imm(6, 1); c.a.ssl(5); c.a.sll(6, 6);
        c.imm(4, GPIO + if read { 0x3c } else { 0x08 });
        let low = c.label(); c.imm(5, 32); c.branch(cc::LTU, 2, 5, low);
        c.a.addi(4, 4, if read { 4 } else { 12 }); c.bind(low);
        if read {
            c.a.l32i(3, 4, 0); c.a.and(3, 3, 6);
        } else {
            let high = c.label(); c.zero(ccz::NE, 3, high);
            c.a.addi(4, 4, 4); c.bind(high); c.a.s32i(6, 4, 0);
        }
        // Normalize before packing; keep the result across EVENT for reads.
        c.imm(6, 0); let zero = c.label(); c.zero(ccz::EQ, 3, zero); c.imm(6, 1); c.bind(zero);
        if read { c.store(6, l.read_value); }
        c.shl(6, 6, 7); c.a.or(3, 2, 6);
        c.a.mov_n(4, 10); c.imm(2, u32::from(if read { watch::READ } else { watch::WRITE }));
        c.jump(report);
        c.bind(bad); c.a.mov_n(5, 2); c.a.mov_n(4, 10);
        c.imm(3, u32::from(watch::FAULT_PIN)); c.imm(2, u32::from(watch::FAULT));
        if read { c.imm(6, 0); c.store(6, l.read_value); }
        c.bind(report); c.call(EVENT);
        if read { c.load(2, l.read_value); }
        c.load(0, l.port_return); c.a.ret_n();
    }
    c.routine(PORT + ports::CLOCK_US as usize, "clock_us");
    c.load(2, l.stamp); c.load(3, l.stamp + 4);
    c.shr(2, 2, 4); c.shl(3, 3, 28); c.a.or(2, 2, 3); c.a.ret_n();
    c.routine(PORT + ports::CLOCK_MS as usize, "clock_ms");
    c.load(2, l.stamp); c.load(3, l.stamp + 4);
    // The u64 stamp over 16000, low word of the quotient: the high word's
    // remainder r < 2^14 leads two exact QUOU/REMU digits of 16 bits each
    // (each dividend below 2^30), and the high word's own quotient only
    // reaches the discarded bits.
    c.imm(6, 16000); c.a.remu(4, 3, 6);
    c.shl(4, 4, 16); c.shr(5, 2, 16); c.a.or(4, 4, 5);
    c.a.quou(3, 4, 6); c.a.remu(4, 4, 6);
    c.shl(4, 4, 16); c.a.extui(5, 2, 0, 16); c.a.or(4, 4, 5);
    c.a.quou(2, 4, 6); c.shl(3, 3, 16); c.a.or(2, 2, 3); c.a.ret_n();
    for port in [ports::TIMER_STOP, ports::TIMER_START, ports::TIMER_PERIOD] {
        c.routine(PORT + port as usize, ports::PORTS[port as usize].name);
        c.store(0, l.port_return);
        let bad = c.label(); c.imm(4, l.timer_count - u32::from(l.motion.is_some()) - u32::from(l.touch.is_some())); c.branch(cc::GEU, 2, 4, bad);
        c.shl(6, 2, 5); c.address(4, l.timers); c.a.add_n(6, 6, 4);
        match port {
            ports::TIMER_STOP => { c.imm(2, 0); c.a.s32i(2, 6, 16); }
            ports::TIMER_START => {
                c.load(2, l.stamp); c.load(3, l.stamp + 4);
                c.a.l32i(4, 6, 8); c.a.l32i(5, 6, 12);
                c.add64(2, 3, 4, 5, 7, 8);
                c.a.s32i(2, 6, 0); c.a.s32i(3, 6, 4); c.imm(2, 1); c.a.s32i(2, 6, 16);
            }
            _ => {
                let positive = c.label(); c.zero(ccz::GE, 3, positive); c.imm(3, 0); c.bind(positive);
                c.shr(4, 3, 28); c.shl(3, 3, 4);
                c.a.s32i(3, 6, 8); c.a.s32i(4, 6, 12);
            }
        }
        c.a.ret_n(); // a0 unchanged on successful leaf path
        c.bind(bad); c.a.mov_n(5, 2); c.a.mov_n(4, 10);
        c.imm(3, u32::from(watch::FAULT_TIMER)); c.imm(2, u32::from(watch::FAULT)); c.call(EVENT);
        c.load(0, l.port_return); c.a.ret_n();
    }
    c.routine(PORT + ports::LOOP_CAP as usize, "loop_cap");
    c.store(0, l.port_return); c.a.mov_n(4, 10); c.imm(5, 0);
    c.imm(3, u32::from(watch::FAULT_LOOP_CAP)); c.imm(2, u32::from(watch::FAULT)); c.call(EVENT);
    c.load(0, l.port_return); c.a.ret_n();
}

// a2 kind, a3 packed pin/level or fault code, a4 site, a5 argument.
// The saved request survives a preceding LOST and Time frame.
fn event(c: &mut Code<'_>, l: &SramLayout) {
    c.routine(EVENT, "watch_event"); c.store(0, l.event_return);
    for r in 2..=5 { c.store(r, l.event + u32::from(r - 2) * 4); }
    let current = c.label(); let counted = c.label(); let refused = c.label();
    let no_loss = c.label();
    c.load(2, l.lost_count); c.zero(ccz::EQ, 2, no_loss);
    // watch::Event::Lost: low 31 count bits, saturation in bit 31,
    // first tick in the payload and last lost tick in the frame header.
    c.imm(3, 0x7fff_ffff); let fits = c.label();
    c.branch(cc::GEU, 3, 2, fits); c.a.mov_n(2, 3); c.bind(fits);
    c.load(3, l.lost_saturated); c.shl(3, 3, 31); c.a.or(2, 2, 3);
    for byte in 0..4 { c.byte(2, l.body + 6 + byte); if byte != 3 { c.shr(2, 2, 8); } }
    c.load(2, l.lost_first);
    for byte in 0..4 { c.byte(2, l.body + 10 + byte); if byte != 3 { c.shr(2, 2, 8); } }
    c.imm(2, u32::from(watch::LOST)); c.byte(2, l.body);
    c.imm(2, 14); c.address(3, l.body); c.load(4, l.lost_last); c.call(WIRE);
    c.zero(ccz::EQ, 2, refused);
    c.set(l.lost_count, 0); c.set(l.lost_saturated, 0);
    c.bind(no_loss);
    c.load(2, l.stamp + 4); c.a.extui(2, 2, 0, 16); c.load(3, l.time_sent);
    c.branch(cc::EQ, 2, 3, current);
    c.byte(2, l.body + 6); c.shr(2, 2, 8); c.byte(2, l.body + 7);
    c.imm(2, u32::from(watch::TIME)); c.byte(2, l.body);
    c.imm(2, 8); c.address(3, l.body); c.load(4, l.stamp); c.call(WIRE);
    let time_written = c.label(); c.zero(ccz::NE, 2, time_written);
    // Count both the failed Time and this event; retry Time next time so
    // later event ticks cannot be interpreted using the old high word.
    c.call(LOSS); c.jump(refused);
    c.bind(time_written); c.load(2, l.stamp + 4); c.a.extui(2, 2, 0, 16); c.store(2, l.time_sent);
    c.bind(current);
    c.load(2, l.event); c.byte(2, l.body);
    c.load(3, l.event + 4); c.byte(3, l.body + 6);
    c.load(4, l.event + 8); c.byte(4, l.body + 7); c.shr(4, 4, 8); c.byte(4, l.body + 8);
    c.imm(3, u32::from(watch::FAULT)); let short = c.label(); let send = c.label();
    c.branch(cc::NE, 2, 3, short);
    c.load(5, l.event + 12);
    for byte in 0..4 { c.byte(5, l.body + 9 + byte); if byte != 3 { c.shr(5, 5, 8); } }
    c.imm(2, 13); c.jump(send); c.bind(short); c.imm(2, 9);
    c.bind(send); c.address(3, l.body); c.load(4, l.stamp); c.call(WIRE);
    c.zero(ccz::NE, 2, counted);
    c.bind(refused); c.call(LOSS);
    c.bind(counted); c.load(0, l.event_return); c.a.ret_n();
}

// Count refused event/Time frames, not failed attempts to report LOST itself.
fn loss(c: &mut Code<'_>, l: &SramLayout) {
    c.routine(LOSS, "watch_loss");
    loss_body(c, l, None);
}

// Radio-only leaf: add a nonzero unsigned count in a5. Sharing the emitter
// keeps LOSS's original instruction sequence and radio-off images unchanged.
fn loss_n(c: &mut Code<'_>, l: &SramLayout) {
    c.routine(LOSS_N, "watch_loss_n");
    loss_body(c, l, Some(5));
}

fn loss_body(c: &mut Code<'_>, l: &SramLayout, count: Option<Reg>) {
    c.load(2, l.lost_count); c.load(3, l.stamp);
    let seen = c.label(); c.zero(ccz::NE, 2, seen); c.store(3, l.lost_first); c.bind(seen);
    c.store(3, l.lost_last);
    if let Some(count) = count {
        c.a.add_n(4, 2, count); let fits = c.label(); c.branch(cc::GEU, 4, 2, fits);
        c.imm(4, u32::MAX); c.bind(fits); c.a.mov_n(2, 4); c.store(2, l.lost_count);
    } else {
        c.imm(4, u32::MAX); let full = c.label(); c.branch(cc::EQ, 2, 4, full);
        c.a.addi(2, 2, 1); c.store(2, l.lost_count); c.bind(full);
    }
    // Keep the full saturating u32 internally. The codec can represent only
    // 31 bits, so flag that loss of precision before the u32 itself saturates.
    c.imm(4, 0x7fff_ffff); let exact = c.label(); c.branch(cc::GEU, 4, 2, exact);
    c.set(l.lost_saturated, 1); c.bind(exact); c.a.ret_n();
}

// Space for a2 body bytes plus CRC, floor(n/254)+1 COBS codes and delimiter.
// One unused ring byte distinguishes head==tail from full.
fn wire_room(c: &mut Code<'_>, l: &SramLayout, refused: usize) {
    c.a.addi(5, 2, 1); c.imm(6, 254); c.a.quou(6, 5, 6);
    c.a.add_n(5, 5, 6); c.a.addi(5, 5, 2);
    c.load(6, l.tx_tail); c.load(7, l.tx_head);
    c.a.sub(6, 6, 7); c.a.addi(6, 6, -1); c.a.extui(6, 6, 0, TX_RING_BITS);
    c.branch(cc::LTU, 6, 5, refused);
}

// rt::watch::{encode,cobs_encode}: a2 body length excluding CRC, a3 writable
// body (including room for CRC), a4 tick. Returns a2=1 iff the whole frame was
// queued. The caller owns loss/retry policy; no byte or sequence is committed
// on refusal. CALL0, QUOU, EXTUI and byte accesses use emit32::xtensa's cited
// ISA encoders (ISA Summary QUOU pp.557–558, EXTUI p.429).
fn wire(c: &mut Code<'_>, l: &SramLayout, link: &Link) {
    c.routine(WIRE, "watch_enqueue"); c.store(0, l.wire_return);
    c.store(3, l.wire_body); c.feed();
    let refused = c.label(); let finish = c.label(); wire_room(c, l, refused);
    c.load(5, l.seq); c.a.s8i(5, 3, 1);
    for byte in 0..4 { c.a.s8i(4, 3, 2 + byte); if byte != 3 { c.shr(4, 4, 8); } }
    c.imm(4, 0); c.a.mov_n(5, 2); c.address(6, link.crc8);
    let crc = c.label(); c.bind(crc);
    c.a.l8ui(7, 3, 0); c.a.xor(4, 4, 7); c.a.add_n(8, 6, 4); c.a.l8ui(4, 8, 0);
    c.a.addi(3, 3, 1); c.a.addi(5, 5, -1);
    let crc_fed = c.label(); c.a.extui(8, 5, 0, 6); c.zero(ccz::NE, 8, crc_fed); c.feed(); c.bind(crc_fed);
    c.zero(ccz::NE, 5, crc);
    c.a.s8i(4, 3, 0); c.a.addi(2, 2, 1);
    c.load(3, l.wire_body); c.load(4, l.tx_head); c.a.mov_n(5, 4);
    c.a.addi(4, 4, 1); c.a.extui(4, 4, 0, TX_RING_BITS);
    c.imm(6, 1); c.address(7, l.tx_ring);
    let encode = c.label(); let zero = c.label(); let next = c.label();
    let block = c.label(); let encoded = c.label(); c.bind(encode);
    c.a.l8ui(8, 3, 0); c.a.addi(3, 3, 1); c.a.addi(2, 2, -1); c.zero(ccz::EQ, 8, zero);
    c.a.add_n(9, 7, 4); c.a.s8i(8, 9, 0);
    c.a.addi(4, 4, 1); c.a.extui(4, 4, 0, TX_RING_BITS); c.a.addi(6, 6, 1);
    c.imm(8, 255); c.branch(cc::NE, 6, 8, next);
    c.a.add_n(9, 7, 5); c.a.s8i(6, 9, 0);
    // A full final block has no trailing empty code, exactly as the codec.
    c.zero(ccz::EQ, 2, encoded); c.jump(block);
    c.bind(zero); c.a.add_n(9, 7, 5); c.a.s8i(6, 9, 0);
    c.bind(block); c.a.mov_n(5, 4); c.a.addi(4, 4, 1);
    c.a.extui(4, 4, 0, TX_RING_BITS); c.imm(6, 1);
    c.bind(next);
    let cobs_fed = c.label(); c.a.extui(8, 2, 0, 6); c.zero(ccz::NE, 8, cobs_fed); c.feed(); c.bind(cobs_fed);
    c.zero(ccz::NE, 2, encode);
    c.a.add_n(9, 7, 5); c.a.s8i(6, 9, 0); c.bind(encoded);
    c.a.add_n(9, 7, 4); c.imm(8, 0); c.a.s8i(8, 9, 0);
    c.a.addi(4, 4, 1); c.a.extui(4, 4, 0, TX_RING_BITS); c.store(4, l.tx_head);
    c.load(3, l.seq); c.a.addi(3, 3, 1); c.store(3, l.seq);
    c.set(l.wire_written, 1); c.jump(finish);
    c.bind(refused); c.set(l.wire_written, 0);
    c.bind(finish); c.call(DRAIN); c.load(2, l.wire_written); c.load(0, l.wire_return); c.a.ret_n();
}

// Leaf: only a2..a11; preserves dispatcher registers and never waits for USB.
fn drain(c: &mut Code<'_>, l: &SramLayout) {
    c.routine(DRAIN, "watch_drain"); c.symbols.drain = c.here();
    // A drain writes at most one packet: the 64th byte auto-flushes and FREE
    // stays 0 until the host reads it. It feeds only after a full packet, so
    // an empty drain leaves the dispatcher's WAITI feed as the last one. A
    // partial packet is flushed only at the idle point (sleep), so the frames
    // of one turn share packets: the model's host reads one per frame.
    c.load(2, l.tx_tail); c.load(3, l.tx_head);
    c.address(4, l.tx_ring); c.address(5, crate::USB_EP1); c.load(6, l.tx_packet);
    let again = c.label(); let blocked = c.label(); let done = c.label();
    c.bind(again); c.branch(cc::EQ, 2, 3, done);
    c.a.l32i(7, 5, 4); c.a.extui(7, 7, USB_FREE_BIT, 1); c.zero(ccz::EQ, 7, blocked);
    c.a.add_n(7, 4, 2); c.a.l8ui(8, 7, 0); c.a.s32i(8, 5, 0);
    c.a.addi(2, 2, 1); c.a.extui(2, 2, 0, TX_RING_BITS);
    c.a.addi(6, 6, 1); c.a.extui(6, 6, 0, 6); // 64th byte auto-flushes.
    c.zero(ccz::NE, 6, again); c.feed(); c.jump(again);
    c.bind(blocked);
    // M: clearing RAW can lose a simultaneous event, and an empty FIFO may
    // immediately reassert it (facts-machine.md §7; pins.rs). Clear while
    // blocked, then recheck FREE: a host read racing the clear cannot strand
    // bytes. Never rely on a clear suppressing a still-empty FIFO.
    c.imm(7, USB_EMPTY); c.a.s32i(7, 5, 0x14);
    c.a.l32i(7, 5, 4); c.a.extui(7, 7, USB_FREE_BIT, 1); c.zero(ccz::NE, 7, again); c.jump(done);
    c.bind(done); c.store(2, l.tx_tail); c.store(6, l.tx_packet);
    c.a.ret_n(); c.symbols.drain_end = c.here();
}

// CRC-32/ISO-HDLC over the slot through DROM, one table step per byte (`crate::crc32`'s result
// bit for bit): crc = table[(crc ^ byte) & 0xff] ^ (crc >> 8). Twelve instructions a byte and a
// feed point every 64 bytes, so a feed gap stays under 800 instructions, within MAX_OP_INSNS.
fn crc32(c: &mut Code<'_>, link: &Link) {
    c.routine(CRC, "image_crc32");
    c.address(2, SLOT_DATA_BASE); c.address(3, link.slot_bytes);
    c.imm(4, u32::MAX); c.address(5, link.crc32);
    let byte = c.label(); let fed = c.label(); let done = c.label();
    c.zero(ccz::EQ, 3, done); c.bind(byte);
    c.a.l8ui(6, 2, 0); c.a.xor(6, 6, 4); c.a.extui(6, 6, 0, 8);
    c.a.addx4(6, 6, 5); c.a.l32i(6, 6, 0);
    c.a.srli(4, 4, 8); c.a.xor(4, 4, 6);
    c.a.addi(2, 2, 1); c.a.addi(3, 3, -1);
    c.a.extui(7, 3, 0, 6); c.zero(ccz::NE, 7, fed); c.feed(); c.bind(fed);
    c.zero(ccz::NE, 3, byte);
    c.bind(done); c.imm(2, u32::MAX); c.a.xor(2, 2, 4); c.a.ret_n();
}

// Clear before the first FIFO read. A packet racing the clear remains readable;
// one arriving after the final availability check leaves RX_PACKET asserted.
// Only one 64-byte OUT packet is exposed per USB frame (pins.rs). Each byte
// feeds independently, including discard paths, so malformed input cannot
// extend a watchdog stretch. There is no host wait or firmware packet-size cap.
// Shared by Ethernet and radio images; the eth_ name is kept for map and test stability.
fn receive(c: &mut Code<'_>, e: &LinkSram) {
    c.routine(RX, "eth_rx"); c.store(0, e.rx_return);
    c.set(crate::USB_EP1 + 0x14, USB_RX);
    let again = c.label(); let done = c.label(); c.bind(again);
    c.load(2, crate::USB_EP1 + 4); c.a.extui(2, 2, 2, 1); c.zero(ccz::EQ, 2, done);
    c.feed(); c.load(2, crate::USB_EP1); c.a.extui(2, 2, 0, 8); c.call(RX_BYTE); c.jump(again);
    c.bind(done); c.load(0, e.rx_return); c.a.ret_n();
}

// Append decoded a2, preserving a8 (the next COBS code). Ethernet alone
// streams into its reserved slot. A connection image stages the payload
// until it knows the kind, CRC and epoch. Published lengths exclude the CRC.
fn rx_put(c: &mut Code<'_>, e: &LinkSram, l: &SramLayout, link: &Link, bad: usize) {
    let max = if l.eth.is_some() || l.net.is_some() { watch::ETH_BODY as u32 } else { 7 };
    c.load(3, e.rx_bytes); c.imm(4, max); c.branch(cc::GEU, 3, 4, bad);
    c.load(4, e.rx_crc); c.a.xor(4, 4, 2); c.address(5, link.crc8);
    c.a.add_n(5, 5, 4); c.a.l8ui(4, 5, 0); c.store(4, e.rx_crc);
    let payload = c.label(); let counted = c.label();
    c.imm(4, 6); c.branch(cc::GEU, 3, 4, payload);
    c.address(5, e.rx_header); c.a.add_n(5, 5, 3); c.a.s8i(2, 5, 0); c.jump(counted);
    c.bind(payload);
    if let Some(n) = &l.net {
        // The kind shares one OUT pipe with ETH. Stage through CRC and epoch
        // validation even when the Ethernet ring has no free slot.
        c.address(5, n.rx_payload - 6); c.a.add_n(5, 5, 3); c.a.s8i(2, 5, 0);
    } else if let Some(eth) = &l.eth {
        c.imm(4, (watch::ETH_BODY - 1) as u32); c.branch(cc::EQ, 3, 4, counted);
        c.load(5, eth.rx_slot); c.a.add_n(5, 5, 3); c.a.addi(5, 5, -4); c.a.s8i(2, 5, 0);
    }
    c.bind(counted); c.a.addi(3, 3, 1); c.store(3, e.rx_bytes);
}

fn increment(c: &mut Code<'_>, address: u32) {
    c.load(2, address); c.a.addi(2, 2, 1); c.store(2, address);
}

// Even one refusal per ~100 instructions at 240 MHz takes more than 10^5
// years to reach 2^64 events. Word-pair carry suffices; no saturation or
// overflow path is needed. Keep these counters across link lifetimes.
fn increment64(c: &mut Code<'_>, address: u32) {
    increment(c, address);
    let done = c.label(); c.zero(ccz::NE, 2, done); increment(c, address + 4); c.bind(done);
}

// Store stamp + a4 (a u32 tick interval) as a full deadline. The stamp is
// derived from a 52-bit hardware counter; adding at most 30 s fits in u64.
fn link_deadline(c: &mut Code<'_>, l: &SramLayout, address: u32) {
    c.load(2, l.stamp); c.load(3, l.stamp + 4); c.a.mov_n(5, 2); c.a.add_n(2, 2, 4);
    let carry = c.label(); c.branch(cc::GEU, 2, 5, carry); c.a.addi(3, 3, 1); c.bind(carry);
    c.store(2, address); c.store(3, address + 4);
}

// Branch while stamp < deadline, preserving a12..a15 like every link helper.
fn link_future(c: &mut Code<'_>, l: &SramLayout, address: u32, future: usize) {
    c.load(2, l.stamp); c.load(3, l.stamp + 4); c.load(5, address); c.load(6, address + 4);
    let due = c.label();
    c.branch(cc::LTU, 3, 6, future); c.branch(cc::NE, 3, 6, due);
    c.branch(cc::LTU, 2, 5, future); c.bind(due);
}

// Leaf. M: the model emits IN tokens only while the host port is listening.
// pins.rs emits a Note at each transition, pointing at fc1-seam.md's pending
// "Board check" on macOS and Linux. The TRM specifies the token interrupt,
// not host-driver polling while a virtual port is closed.
// Shared by Ethernet and radio images; the eth_ name is kept for map and test stability.
fn stall(c: &mut Code<'_>, l: &SramLayout, e: &LinkSram) {
    c.routine(STALL, "eth_stall");
    let check = c.label(); let disarm = c.label(); let clear = c.label();
    let edge = c.label(); let restart = c.label(); let done = c.label();
    c.load(4, e.stall_state); c.imm(5, 2); c.branch(cc::NE, 4, 5, check);
    // An armed token wins over FREE: that very token can have freed EP1.
    c.load(2, crate::USB_EP1 + 8); c.a.extui(2, 2, 8, 1); c.zero(ccz::NE, 2, edge);
    c.bind(check);
    c.load(2, l.tx_head); c.load(3, l.tx_tail); c.branch(cc::EQ, 2, 3, disarm);
    c.load(2, crate::USB_EP1 + 4); c.a.extui(2, 2, USB_FREE_BIT, 1); c.zero(ccz::NE, 2, disarm);
    c.zero(ccz::EQ, 4, restart); c.imm(5, 2); c.branch(cc::EQ, 4, 5, done);
    link_future(c, l, e.stall_due, done);
    c.load(2, crate::USB_EP1 + 8); c.a.extui(2, 2, 8, 1); c.zero(ccz::NE, 2, restart);
    // Do not clear after the observation: a token racing the ENA write must
    // remain raw and wake line 2. The model's simultaneous clear loses it.
    bits(c, crate::USB_EP1 + 0x10, 0, USB_IN_TOKEN); c.set(e.stall_state, 2); c.jump(done);
    c.bind(restart);
    c.set(crate::USB_EP1 + 0x14, USB_IN_TOKEN);
    c.imm(4, STALL_TICKS); link_deadline(c, l, e.stall_due); c.set(e.stall_state, 1); c.jump(done);
    c.bind(disarm);
    // The usual non-stalled path performs no peripheral write.
    c.load(2, e.stall_state); c.zero(ccz::EQ, 2, done);
    c.imm(3, 2); c.branch(cc::NE, 2, 3, clear);
    // A token can race the first RAW read and the FREE observation above.
    // Re-read RAW before disabling an armed wake, so that edge is not lost.
    c.load(2, crate::USB_EP1 + 8); c.a.extui(2, 2, 8, 1); c.zero(ccz::EQ, 2, clear);
    c.bind(edge); increment64(c, e.in_edges); c.set(e.rehello, 1);
    c.bind(clear);
    bits(c, crate::USB_EP1 + 0x10, USB_IN_TOKEN, 0); c.set(e.stall_state, 0);
    c.bind(done); c.a.ret_n();
}

fn hello_template(c: &mut Code<'_>, l: &SramLayout) -> u32 {
    // Derive the Hello template from rt::watch::encode, like emit_avr.
    let hello = watch::Frame { kind: watch::HELLO, seq: 0, tick: 0,
        event: watch::Event::Hello { proto: watch::PROTO, ports_version: ports::PORT_TABLE_VERSION as u8,
            image: 0, clock_hz: 16_000_000, prescale: 1, ring: 0, tick_hi: 0, mcusr: 0,
            wdtcsr: 0, level: watch::Level::Ports, generation: 0 } };
    let mut wire = [0; watch::MAX_WIRE]; let n = watch::encode(&hello, &mut wire);
    let mut body = [0; watch::MAX_BODY];
    let n = watch::cobs_decode(&wire[..n - 1], &mut body).expect("Hello body");
    for (k, &byte) in body[..n - 1].iter().enumerate() { c.imm(2, u32::from(byte)); c.byte(2, l.body + k as u32); }
    if let Some(r) = &l.radio {
        c.load(2, r.generation); c.store(2, l.body + 24);
    }
    (n - 1) as u32
}

// Shared by Ethernet and radio images; the eth_ name is kept for map and test stability.
fn rehello(c: &mut Code<'_>, l: &SramLayout, e: &LinkSram) {
    c.routine(REHELLO, "eth_rehello"); c.store(0, e.hello_return);
    let done = c.label(); c.load(2, e.rehello); c.zero(ccz::EQ, 2, done);
    // Hello takes exactly 31 wire bytes and Ethernet's credit 11, each including its delimiter.
    // No other producer can run here; nested drains only increase free space.
    // Refusal changes neither the lifetime nor a single queued byte.
    c.load(2, l.tx_tail); c.load(3, l.tx_head);
    c.a.sub(2, 2, 3); c.a.addi(2, 2, -1); c.a.extui(2, 2, 0, TX_RING_BITS);
    let credit_bytes = if l.eth.is_some() { 11 } else { 0 };
    c.imm(3, watch::MAX_WIRE as u32 + credit_bytes); c.branch(cc::LTU, 2, 3, done);
    c.call(STAMP); c.store(2, e.epoch);
    if let Some(n) = &l.net { net::hello(c, l, n); }
    if let Some(eth) = &l.eth {
        // Free only the slot lengths; payload bytes need no destructive clear.
        c.address(2, eth.slots); c.imm(3, eth.count); c.imm(4, 0); c.imm(5, RX_SLOT_BYTES);
        let slot = c.label(); c.bind(slot);
        c.a.s16i(4, 2, 0); c.a.add_n(2, 2, 5); c.a.addi(3, 3, -1); c.feed(); c.zero(ccz::NE, 3, slot);
        for address in [eth.head, eth.tail, eth.rx_slot, eth.received, eth.seen, eth.owed,
            eth.due, eth.due + 4, eth.answer_state] { c.set(address, 0); }
        let backoff = c.label(); let wait_ready = c.label(); let capped = c.label();
        c.load(2, e.rehello); c.imm(3, 2); c.branch(cc::EQ, 2, 3, backoff);
        c.imm(4, ANSWER_TICKS); c.jump(wait_ready);
        c.bind(backoff); c.load(4, eth.answer_wait); c.a.add_n(4, 4, 4); c.imm(5, ANSWER_MAX_TICKS);
        c.branch(cc::LTU, 4, 5, capped); c.a.mov_n(4, 5); c.bind(capped);
        c.bind(wait_ready); c.store(4, eth.answer_wait); link_deadline(c, l, eth.answer_due);
    }
    for address in [e.rx_bytes, e.rx_left, e.rx_zero, e.rx_mode, e.rx_crc, e.rx_header, e.rx_header + 4] {
        c.set(address, 0);
    }
    let n = hello_template(c, l);
    c.load(2, e.image_crc); c.store(2, l.body + 8);
    c.load(2, l.reset_cause); c.byte(2, l.body + 21);
    c.load(2, l.stamp + 4); c.a.extui(2, 2, 0, 16); c.store(2, l.time_sent);
    c.byte(2, l.body + 19); c.shr(2, 2, 8); c.byte(2, l.body + 20);
    c.imm(2, n); c.address(3, l.body); c.load(4, l.stamp); c.call(WIRE);
    if let Some(eth) = &l.eth { c.set(eth.owed, eth.count); c.call(CREDIT); }
    c.set(e.rehello, 0);
    c.bind(done); c.load(0, e.hello_return); c.a.ret_n();
}

// Shared by Ethernet and radio images; the eth_ name is kept for map and test stability.
fn link_idle(c: &mut Code<'_>, l: &SramLayout, e: &LinkSram) {
    c.routine(LINK_IDLE, "eth_idle"); c.store(0, e.idle_return);
    // Radio-only images take host bytes in sleep, so the wake-to-handler path (RADIO_IDLE_WAKE_INSNS) is unchanged.
    if l.eth.is_none() && l.net.is_none() { c.call(RX); }
    // A handler may have run since TURN's sample; start windows at idle's
    // running tick, never at a stale handler-start tick.
    c.call(STAMP); c.call(STALL);
    if let Some(eth) = &l.eth {
        let pending = c.label();
        c.load(2, e.rehello); c.zero(ccz::NE, 2, pending);
        c.load(2, eth.answer_state); c.imm(3, 1); c.branch(cc::NE, 2, 3, pending);
        link_future(c, l, eth.answer_due, pending); c.set(e.rehello, 2);
        c.bind(pending);
    }
    c.call(REHELLO);
    if l.eth.is_some() { c.call(CREDIT); }
    c.call(LINK_STATS);
    c.load(0, e.idle_return); c.a.ret_n();
}

// Shared by Ethernet and radio images; the eth_ name is kept for map and test stability.
fn link_stats(c: &mut Code<'_>, l: &SramLayout, e: &LinkSram) {
    c.routine(LINK_STATS, "eth_link_stats"); c.store(0, e.report_return); c.set(e.report_index, 0);
    let scan = c.label(); let changed = c.label(); let next = c.label();
    c.bind(scan); c.load(2, e.report_index); c.shl(3, 2, 3);
    c.address(4, e.in_edges); c.a.add_n(4, 4, 3); c.address(5, e.reported); c.a.add_n(5, 5, 3);
    c.a.l32i(6, 4, 0); c.a.l32i(7, 5, 0); c.branch(cc::NE, 6, 7, changed);
    c.a.l32i(6, 4, 4); c.a.l32i(7, 5, 4); c.branch(cc::EQ, 6, 7, next);
    c.bind(changed); c.byte(2, l.body + 6);
    c.imm(2, u32::from(watch::LINK)); c.byte(2, l.body);
    // The u64 is unaligned in the wire body, so copy bytes from its SRAM pair.
    for byte in 0..8 { c.a.l8ui(2, 4, byte); c.byte(2, l.body + 7 + byte); }
    c.imm(2, 15); c.address(3, l.body); c.load(4, l.stamp); c.call(WIRE);
    // Retain this snapshot on refusal. WIRE's nested drain may make space for
    // another counter in this scan; reporting never contributes to LOST.
    c.zero(ccz::EQ, 2, next);
    c.load(2, e.report_index); c.shl(3, 2, 3);
    c.address(4, e.in_edges); c.a.add_n(4, 4, 3); c.address(5, e.reported); c.a.add_n(5, 5, 3);
    for offset in [0, 4] { c.a.l32i(6, 4, offset); c.a.s32i(6, 5, offset); }
    c.bind(next); increment(c, e.report_index); c.imm(3, 5); c.branch(cc::LTU, 2, 3, scan);
    c.load(0, e.report_return); c.a.ret_n();
}

fn next_slot(c: &mut Code<'_>, e: &EthSram, index: u32) {
    c.load(2, index); c.a.addi(2, 2, 1); c.imm(3, e.count);
    let next = c.label(); c.branch(cc::LTU, 2, 3, next); c.imm(2, 0); c.bind(next); c.store(2, index);
}

// An accepted ETH or the host's NET_LINK confirms that it saw this Hello.
// Cancel only the no-answer retry. A host-open edge still begins a new lifetime.
fn link_answered(c: &mut Code<'_>, eth: &EthSram, e: &LinkSram) {
    c.set(eth.answer_state, 2); c.set(eth.answer_wait, ANSWER_TICKS);
    c.load(2, e.rehello); c.imm(3, 2); let answered = c.label();
    c.branch(cc::NE, 2, 3, answered); c.set(e.rehello, 0); c.bind(answered);
}

// Shared by Ethernet and radio images; the eth_ name is kept for map and test stability.
fn receive_byte(c: &mut Code<'_>, l: &SramLayout, e: &LinkSram, link: &Link) {
    c.routine(RX_BYTE, "eth_rx_byte");
    let delimiter = c.label(); let active = c.label(); let data = c.label();
    let bad = c.label(); let reset = c.label(); let bad_frame = c.label();
    let refused = c.label(); let stale = c.label(); let done = c.label();
    c.load(3, e.rx_mode); c.zero(ccz::EQ, 2, delimiter);
    c.imm(4, 2); c.branch(cc::GEU, 3, 4, done); c.zero(ccz::NE, 3, active);
    if let Some(eth) = &l.eth {
        // Reserve on the first encoded byte. With NET staging, a full Ethernet
        // ring leaves the slot pointer zero and connection frames can still pass.
        c.load(3, eth.head); c.imm(4, RX_SLOT_BYTES); c.a.mull(3, 3, 4);
        c.address(4, eth.slots); c.a.add_n(3, 3, 4); c.a.l16ui(4, 3, 0);
        let free = c.label(); c.zero(ccz::EQ, 4, free);
        if l.net.is_none() { c.set(e.rx_mode, 2); c.jump(done); }
        else { c.imm(3, 0); }
        c.bind(free); c.store(3, eth.rx_slot);
    }
    c.set(e.rx_mode, 1);
    c.bind(active); c.load(3, e.rx_left); c.zero(ccz::NE, 3, data);
    c.a.mov_n(8, 2); c.load(3, e.rx_zero);
    let code = c.label(); c.zero(ccz::EQ, 3, code);
    c.imm(2, 0); rx_put(c, e, l, link, bad);
    c.bind(code); c.a.addi(3, 8, -1); c.store(3, e.rx_left);
    c.imm(3, 1); c.imm(4, 255); let zero = c.label();
    c.branch(cc::NE, 8, 4, zero); c.imm(3, 0); c.bind(zero); c.store(3, e.rx_zero); c.jump(done);
    c.bind(data); c.a.addi(3, 3, -1); c.store(3, e.rx_left); rx_put(c, e, l, link, bad); c.jump(done);
    c.bind(bad); c.set(e.rx_mode, 3); c.jump(done);

    c.bind(delimiter); c.zero(ccz::EQ, 3, done); // empty delimiters resynchronise, as Reader does
    if l.eth.is_some() { c.imm(4, 2); c.branch(cc::EQ, 3, 4, refused); }
    c.imm(4, 3); c.branch(cc::EQ, 3, 4, bad_frame);
    c.load(3, e.rx_left); c.zero(ccz::NE, 3, bad_frame);
    // CRC residue is zero after including the CRC byte (watch::CRC8_TABLE).
    c.load(3, e.rx_crc); c.zero(ccz::NE, 3, bad_frame);
    c.address(4, e.rx_header); c.a.l8ui(3, 4, 0);
    if let Some(n) = &l.net { net::tether::receive(c, l, n, e, link, bad_frame, refused, stale, reset); }
    let other = if l.radio.is_some() { c.label() } else { bad_frame };
    if let Some(eth) = &l.eth {
        c.imm(5, u32::from(watch::ETH));
        c.branch(cc::NE, 3, 5, other);
        c.load(6, e.rx_bytes); c.imm(5, (watch::ETH_MIN + 7) as u32); c.branch(cc::LTU, 6, 5, bad_frame);
        c.imm(5, watch::ETH_BODY as u32); c.branch(cc::LTU, 5, 6, bad_frame);
        rx_epoch(c, e, stale);
        c.a.addi(6, 6, -7); c.load(3, eth.rx_slot);
        if let Some(n) = &l.net {
            c.zero(ccz::EQ, 3, refused); c.address(2, n.rx_payload); c.a.addi(3, 3, 2);
            c.a.mov_n(4, 6); net::copy_bytes(c); c.load(3, eth.rx_slot);
        }
        c.a.s16i(6, 3, 0);
        link_answered(c, eth, e);
        next_slot(c, eth, eth.head);
        c.load(2, eth.received); c.load(3, eth.seen);
        let pending = c.label(); c.branch(cc::NE, 2, 3, pending);
        c.load(4, l.stamp); c.store(4, eth.due); c.load(4, l.stamp + 4); c.store(4, eth.due + 4);
        c.bind(pending); c.a.addi(2, 2, 1); c.store(2, eth.received); c.jump(reset);
    }
    if let Some(r) = &l.radio {
        c.bind(other); c.imm(5, u32::from(watch::RADIO_RESTART)); c.branch(cc::NE, 3, 5, bad_frame);
        c.load(6, e.rx_bytes); c.imm(5, 7); c.branch(cc::NE, 6, 5, bad_frame);
        rx_epoch(c, e, stale); c.set(r.request, 1); c.jump(reset);
    } else if l.eth.is_none() { c.jump(bad_frame); }
    c.bind(stale); increment64(c, e.eth_stale); c.jump(reset);
    if l.eth.is_some() || l.net.is_some() { c.bind(refused); increment64(c, e.rx_refused); c.jump(reset); }
    c.bind(bad_frame); increment64(c, e.rx_bad);
    c.bind(reset);
    for address in [e.rx_mode, e.rx_bytes, e.rx_left, e.rx_zero, e.rx_crc] { c.set(address, 0); }
    if let Some(eth) = &l.eth { c.set(eth.rx_slot, 0); }
    c.bind(done); c.a.ret_n();
}

// a4 points to the six-byte header. Its epoch starts at unaligned byte two.
fn rx_epoch(c: &mut Code<'_>, e: &LinkSram, stale: usize) {
    c.imm(3, 0);
    for byte in 0..4 {
        c.a.l8ui(5, 4, 2 + byte); if byte != 0 { c.shl(5, 5, byte * 8); } c.a.or(3, 3, 5);
    }
    c.load(5, e.epoch); c.branch(cc::NE, 3, 5, stale);
}

// A failed credit stays owed, never LOST. The SRAM pool limits count (and thus
// outstanding credits) to less than u16::MAX: each slot costs 1516 bytes.
fn credit(c: &mut Code<'_>, l: &SramLayout, e: &EthSram) {
    c.routine(CREDIT, "eth_credit"); c.store(0, e.credit_return);
    let done = c.label(); c.load(2, e.owed); c.zero(ccz::EQ, 2, done);
    c.byte(2, l.body + 6); c.shr(2, 2, 8); c.byte(2, l.body + 7);
    c.imm(2, u32::from(watch::ETH_CREDIT)); c.byte(2, l.body);
    c.imm(2, 8); c.address(3, l.body); c.load(4, l.stamp); c.call(WIRE);
    c.zero(ccz::EQ, 2, done); c.set(e.owed, 0);
    c.bind(done); c.load(0, e.credit_return); c.a.ret_n();
}

fn eth_port(c: &mut Code<'_>, l: &SramLayout, e: &EthSram, shared: &LinkSram, link: &Link, port: u16) {
    c.routine(PORT + port as usize, ports::PORTS[port as usize].name);
    c.store(0, l.port_return);
    // The verifier proves a U8 buffer handle and writability for recv/mac.
    // Keep site/off/n in buffer scratch across fault packing and enqueue.
    let s = l.buffer_scratch;
    c.store(10, s + BUF_SITE); c.store(3, s + BUF_DST_OFF);
    if port != ports::ETH_SEND { c.imm(4, if port == ports::ETH_RECV { watch::ETH_MAX as u32 } else { 6 }); }
    c.store(4, s + BUF_N);
    let length = c.label(); let range = c.label(); let returned = c.label();
    if port == ports::ETH_SEND {
        c.imm(5, watch::ETH_MIN as u32); c.branch(cc::LTU, 4, 5, length);
        c.imm(5, watch::ETH_MAX as u32); c.branch(cc::LTU, 5, 4, length);
    }
    c.shl(2, 2, BUFFER_ROW_SHIFT); c.address(5, link.buffers); c.a.add_n(2, 2, 5);
    c.a.l32i(5, 2, 4); c.branch(cc::LTU, 5, 3, range); // also rejects negative off
    c.a.sub(5, 5, 3); c.branch(cc::LTU, 5, 4, range);
    c.a.l32i(2, 2, 0); c.a.add_n(2, 2, 3);
    if port == ports::ETH_SEND {
        c.a.mov_n(3, 2); c.address(2, e.tx_body + 6);
        eth_copy(c);
        c.imm(2, u32::from(watch::ETH)); c.byte(2, e.tx_body);
        c.load(2, s + BUF_N); c.a.addi(2, 2, 6); c.address(3, e.tx_body); c.load(4, l.stamp); c.call(WIRE);
        let sent = c.label(); c.zero(ccz::NE, 2, sent);
        increment64(c, shared.tx_dropped); c.imm(2, 0); c.jump(returned);
        c.bind(sent);
        c.load(3, e.answer_state); c.zero(ccz::NE, 3, returned); c.set(e.answer_state, 1); c.jump(returned);
    } else if port == ports::ETH_RECV {
        c.load(3, e.tail); c.imm(4, RX_SLOT_BYTES); c.a.mull(3, 3, 4);
        c.address(4, e.slots); c.a.add_n(3, 3, 4); c.a.l16ui(4, 3, 0);
        c.store(4, l.read_value);
        let empty = c.label(); c.zero(ccz::EQ, 4, empty);
        c.store(3, s + BUF_SRC); c.a.addi(3, 3, 2); eth_copy(c);
        c.load(3, s + BUF_SRC); c.imm(2, 0); c.a.s16i(2, 3, 0);
        next_slot(c, e, e.tail); increment(c, e.owed);
        c.bind(empty); c.load(2, l.read_value); c.jump(returned);
    } else {
        // TRM v1.8 §5.5, Registers 5.18/5.19 p.436; ESP-IDF v6.1.1
        // reg_base.h:13, efuse_reg.h:674–696. Low 32 bits then high 16;
        // a MAC's displayed/network order is most-significant byte first.
        c.load(3, 0x6000_7048); c.load(4, 0x6000_7044);
        for (dst, reg, shift) in [(0, 3, 8), (1, 3, 0), (2, 4, 24), (3, 4, 16), (4, 4, 8), (5, 4, 0)] {
            if shift == 0 { c.a.mov_n(5, reg); } else { c.shr(5, reg, shift); }
            c.a.s8i(5, 2, dst);
        }
        c.jump(returned);
    }
    if port == ports::ETH_SEND {
        c.bind(length); c.load(5, s + BUF_N); c.imm(3, u32::from(watch::FAULT_ETH_LENGTH));
        let report = c.label(); c.jump(report);
        c.bind(range); c.load(5, s + BUF_DST_OFF); c.imm(3, u32::from(watch::FAULT_ETH_RANGE));
        c.bind(report);
    } else { c.bind(range); c.load(5, s + BUF_DST_OFF); c.imm(3, u32::from(watch::FAULT_ETH_RANGE)); }
    c.load(4, s + BUF_SITE); c.imm(2, u32::from(watch::FAULT)); c.call(EVENT); c.imm(2, 0);
    c.bind(returned); c.load(0, l.port_return); c.a.ret_n();
}

// a2 destination, a3 source, a4 positive byte count. Entry/exit and each
// 128-byte boundary feed; <=9 instructions per byte plus 8 per feed (10 with radio). Both the
// send staging copy and recv's direct slot copy stay below MAX_OP_INSNS.
fn eth_copy(c: &mut Code<'_>) {
    c.feed(); let again = c.label(); c.bind(again);
    c.a.l8ui(5, 3, 0); c.a.s8i(5, 2, 0); c.a.addi(2, 2, 1); c.a.addi(3, 3, 1); c.a.addi(4, 4, -1);
    let fed = c.label(); c.a.extui(5, 4, 0, 7); c.zero(ccz::NE, 5, fed); c.feed(); c.bind(fed);
    c.zero(ccz::NE, 4, again); c.feed();
}
