//! A complete slot linked for either physical slot, and the 6c flash image.
use fab_elec_mcu::{compile_kernel, Compiled};
use fab_elec_mcu_emit32::control::{steps, Step};
use fab_elec_mcu_island::{abi_hash, import_map::parse_roots, Header, IslandBinding, SegmentKind};
use fab_elec_mcu_rt::{bundle::{self, BundleFile, Kind, IMAGE_RGB565}, format::OP_PORT, module::{buffer_dimensions, DeclArg, Elem, Module}, ports, watch, SETUP};
use makepad_core_util::sha256::sha256;
use crate::{Audio, I2cBus, Motion, MotionSram, Touch, TouchSram};
use crate::wiring::{audio_clock, AudioClock};

use crate::{boot_stage, build_handlers, chip, emit, panel, radio, runtime, wdt_hold, BoardTime, Layout, Options, Panel, PanelSram, Record, Refusal, SourceMap, Where, SLOT_BASE, SLOT_DATA_BASE};

/// Automatic radio retry intervals and the bring-up hold for a captured fault.
#[derive(Clone, Copy, Debug)]
pub struct RestartPolicy {
    pub base_ms: u32,
    pub ceiling_ms: u32,
    pub stable_ms: u32,
    pub hold_after_fault: bool,
}

impl RestartPolicy {
    /// Provisional retry and stability intervals; R9 measurements will set the board values.
    pub const PROVISIONAL: Self = Self {
        base_ms: 1_000, ceiling_ms: 300_000, stable_ms: 60_000, hold_after_fault: false,
    };
}

/// The radio profile (fable-answers.md Q5 §3). None leaves every image byte unchanged.
#[derive(Clone, Debug)]
pub struct Radio {
    pub island: IslandBinding,
    pub restart: RestartPolicy,
    pub dispatcher_priority: u32,
    /// Multiple of 16, at least radio::MIN_STACK_BYTES.
    pub irq_stack_bytes: u32,
    /// Holds dispatcher, idle, explicit boot tasks and the optional station start task.
    pub task_capacity: u32,
    pub isr_capacity: u32,
    /// Kernel heap, a multiple of eight, including its control tables.
    pub heap_bytes: u32,
    /// Timers covered by the allocation floor. R9 sizes the provisional reserve of 64.
    pub timer_reserve: u32,
    /// Zero disables tracing; otherwise a power of two in 2..=1024.
    pub trace_entries: u32,
    /// Longest island stretch, in instructions, that can keep the dispatcher from running;
    /// added to the RWDT gap. R9 measures it on the chip.
    pub burst_insns: u64,
    pub boot_tasks: Vec<BootTask>,
    pub start: Option<StartTask>,
}

/// The profile chooses both values. IDF's main-task defaults are not pinned.
#[derive(Clone, Copy, Debug)]
pub struct StartTask {
    /// Kernel priority, i.e. IDF priority + 1 as in osi_task_create. Idle uses zero.
    pub priority: u32,
    pub stack_bytes: u32,
}

#[derive(Clone, Debug)]
pub struct BootTask {
    pub entry: u32,
    pub abi: radio::TaskAbi,
    pub priority: u32,
    pub stack_bytes: u32,
    pub args: [u32; 6],
}

impl Radio {
    /// Planned, provisional station profile for the voicebox. R9 measures these values on the board.
    pub fn voicebox(island: IslandBinding) -> Self {
        Self {
            island, restart: RestartPolicy::PROVISIONAL,
            dispatcher_priority: 1, irq_stack_bytes: radio::MIN_STACK_BYTES,
            task_capacity: 16, isr_capacity: radio::PACK_ISR_LINES,
            heap_bytes: 128 * 1024, timer_reserve: 64,
            trace_entries: 0, burst_insns: 0, boot_tasks: Vec::new(),
            start: Some(StartTask { priority: 3, stack_bytes: 4096 }),
        }
    }

    fn boot_count(&self) -> usize { self.boot_tasks.len() + usize::from(self.start.is_some()) }

    fn stack_sizes(&self) -> impl Iterator<Item = u32> + '_ {
        self.boot_tasks.iter().map(|task| task.stack_bytes).chain(self.start.map(|task| task.stack_bytes))
    }

    fn check_island_layout(&self, board: &Layout) -> Result<(), Refusal> {
        let island = &self.island;
        let h = &island.header;
        let page = board.island_mmu_page().map_err(refusal)?;
        if h.mmu_page != page {
            return Err(refusal(format!("island mmu_page {}, board requires {page}", h.mmu_page)));
        }
        let flash_bytes = board.island_pages * crate::PAGE;
        if h.image_size > flash_bytes {
            return Err(refusal(format!("island image is {} bytes, flash reservation is {flash_bytes} bytes", h.image_size)));
        }
        Header::decode(&h.encode()).map_err(|e| refusal(e.to_string()))?;
        if h.gate_table != crate::SRAM_START {
            return Err(refusal(format!("island window starts at {:#010x}, board requires {:#010x}", h.gate_table, crate::SRAM_START)));
        }
        if island.segments.len() != h.segment_count as usize || island.gate_names.len() != h.gate_count as usize {
            return Err(refusal(format!("island binding has {} segments and {} gates, header declares {} and {}",
                island.segments.len(), island.gate_names.len(), h.segment_count, h.gate_count)));
        }
        // The linker aligns each segment group and the final guard to 16 bytes.
        let mut end = u64::from(h.gate_table) + 4 * u64::from(h.gate_count);
        for s in &island.segments {
            let start = match s.kind {
                SegmentKind::Iram => end.next_multiple_of(16) - u64::from(crate::SRAM_START) + 0x4037_8000,
                SegmentKind::Data | SegmentKind::Bss => end.next_multiple_of(16),
                _ => continue,
            };
            if u64::from(s.address) != start {
                return Err(refusal(format!("island {} starts at {:#010x}, window requires {start:#010x}", s.kind.name(), s.address)));
            }
            end = end.next_multiple_of(16) + u64::from(s.bytes);
        }
        let guard = end.next_multiple_of(16);
        if u64::from(island.guard) != guard || u64::from(island.window_end) != guard + 4 {
            return Err(refusal(format!("island guard {:#010x} and window_end {:#010x}, linked extents require {guard:#010x} and {:#010x}",
                island.guard, island.window_end, guard + 4)));
        }
        let required = parse_roots(include_str!("../../island/packs/esp32s3-idf-v6.1.1-sta.roots"))
            .expect("station root manifest");
        for name in &required {
            if !island.entries.iter().any(|(root, _)| root == name) {
                return Err(refusal(format!("island is missing required root {name}")));
            }
        }
        if island.entries.len() != h.entry_count as usize {
            return Err(refusal(format!("island binding has {} named entries, header declares {}", island.entries.len(), h.entry_count)));
        }
        match island.objects.get(radio::PHY_FUNS_OBJECT) {
            None => return Err(refusal(format!("island is missing required object {}", radio::PHY_FUNS_OBJECT))),
            Some(object) if object.bytes != 4 => return Err(refusal(format!("island object {} is {} bytes, expected 4",
                radio::PHY_FUNS_OBJECT, object.bytes))),
            Some(_) => {}
        }
        for (_, row) in radio::adapter::OSI_FIELDS {
            if let radio::adapter::Entry::Glue(name) = row {
                if !island.glue.contains_key(name) {
                    return Err(refusal(format!("island is missing OSI glue {name}")));
                }
            }
        }
        if self.start.is_some() {
            let entry = island.glue.get("radio_start").ok_or_else(|| refusal("island is missing radio_start glue"))?;
            if *entry >> 30 != SLOT_BASE >> 30 {
                return Err(refusal("radio windowed boot entry must share task_exit's 1-GB region"));
            }
        }
        Ok(())
    }

    /// Validate against the exports of the same compiled kernel the runtime will use.
    fn check(&self, board: &Layout, layout: &radio::kernel::KernelLayout) -> Result<(Compiled, u32), Refusal> {
        self.check_island_layout(board)?;
        if self.restart.base_ms == 0 { return Err(refusal("radio restart base_ms must be at least 1")); }
        if self.restart.ceiling_ms < self.restart.base_ms {
            return Err(refusal("radio restart ceiling_ms must be at least base_ms"));
        }
        if self.heap_bytes & 7 != 0 { return Err(refusal("radio heap_bytes must be a multiple of eight")); }
        let control = radio::kernel::heap_control_bytes(self.heap_bytes);
        let floor = radio::kernel::timer_floor_bytes(self.timer_reserve);
        let minimum = radio::kernel::HEAP_MIN_BLOCK;
        if u64::from(self.heap_bytes) < u64::from(control) + floor + u64::from(minimum) {
            return Err(refusal(format!("radio heap of {} bytes needs {control} control bytes + {floor} timer floor bytes + {minimum} minimum block bytes", self.heap_bytes)));
        }
        if self.isr_capacity < radio::PACK_ISR_LINES {
            return Err(refusal("radio isr_capacity must hold PACK_ISR_LINES"));
        }
        if self.trace_entries != 0 && (!(2..=1024).contains(&self.trace_entries) || !self.trace_entries.is_power_of_two()) {
            return Err(refusal("radio trace_entries must be zero or a power of two in 2..=1024"));
        }
        if u64::from(self.task_capacity) < 2 + self.boot_count() as u64 {
            return Err(refusal("radio task_capacity must hold dispatcher, idle and every boot task"));
        }
        // Idle has priority zero and never blocks. A READY dispatcher must
        // outrank it, otherwise the first yield could leave idle asleep forever.
        if self.dispatcher_priority == 0 {
            return Err(refusal("radio dispatcher_priority must exceed idle's priority zero"));
        }
        for bytes in std::iter::once(self.irq_stack_bytes).chain(self.stack_sizes()) {
            if bytes & 15 != 0 || bytes < radio::MIN_STACK_BYTES {
                return Err(refusal(format!("radio stacks must be 16-aligned and at least {} bytes for initial_frame", radio::MIN_STACK_BYTES)));
            }
        }
        if self.boot_tasks.iter().any(|t| t.priority == 0) || self.start.is_some_and(|t| t.priority == 0) {
            return Err(refusal("idle alone has priority zero"));
        }
        for task in &self.boot_tasks {
            if matches!(task.abi, radio::TaskAbi::Windowed) && task.entry >> 30 != SLOT_BASE >> 30 {
                return Err(refusal("radio windowed boot entry must share task_exit's 1-GB region"));
            }
        }
        let compiled = compile_kernel(radio::kernel::OBJECTS, &chip::profile(), &crate::RADIO_KERNEL_LINES,
            &radio::kernel::kernel_consts(layout)).map_err(|errors| refusal(format!("radio kernel: {errors:?}")))?;
        let kernel = fab_elec_mcu_rt::module::load_kernel(&compiled.module, &chip::profile(), &crate::RADIO_KERNEL_LINES)
            .expect("compiled radio kernel");
        let island = &self.island;
        let gates: Vec<_> = island.gate_names.iter().map(String::as_str).collect();
        for (index, &name) in gates.iter().enumerate() {
            if kernel.entry(name).is_none() {
                return Err(refusal(format!("island gate {index} ({name}) has no kernel export")));
            }
        }
        let roots: Vec<_> = island.entries.iter().map(|(name, _)| name.clone()).collect();
        let hash = abi_hash(&roots, &gates);
        if island.header.abi_hash != hash {
            return Err(refusal(format!("island root or gate order differs from the linked header: gates {gates:?}, \
                ordered names hash {hash:#018x}, header {:#018x}", island.header.abi_hash)));
        }
        let state_words = kernel.state_words();
        Ok((compiled, state_words))
    }
}

/// Describes the board's flash layout and CPU/watchdog timing.
pub struct Board {
    /// Specifies the board's flash allocation (boot-stage.md, Flash layout).
    pub layout: Layout,
    /// Supplies the measured RTC_SLOW clock and programmed eFuse delay selection.
    pub time: BoardTime,
    /// Wiring and visible geometry for the board's optional CO5300 panel.
    pub panel: Option<Panel>,
    /// Shared I2C0 wiring and clock for the panel expander and sensors.
    pub i2c: Option<I2cBus>,
    /// QMI8658C address on that bus.
    pub motion: Option<Motion>,
    /// CST820 address on the same bus.
    pub touch: Option<Touch>,
    pub radio: Option<Radio>,
}

/// Holds a confirmed first image, from flash offset zero through the end of slot 0.
pub struct Image {
    /// Unrounded instruction allowance used to derive the RWDT hold.
    pub gap_insns: u128,
    /// Contains the boot stage, erased padding and R1 sector, R0 record, and slot 0.
    pub flash: Vec<u8>,
    /// Describes the confirmed sequence-one record written into R0.
    pub record: Record,
    /// Holds the linked program and its runtime/source maps.
    pub slot: Slot,
}

/// APB with the CPU on the PLL at CPU_HZ (TRM p.530 Table 7.2-2).
const APB_HZ: u64 = 80_000_000;

pub const FUNCTIONS_UNSUPPORTED: &str = "SharedFunctionsUnsupported: S3 emission does not support shared functions yet";

fn uses_shared_i2c(m: &Module<'_>) -> bool {
    m.display().is_some() || m.motion().is_some() || m.touch()
        || m.decls().iter().any(|d| matches!(d.form, ports::FORM_SPEAKER | ports::FORM_MIC))
}

/// Builds a confirmed slot-0 image using the board's layout and measured clocks, for CPU_HZ.
/// Uses the largest bound from final emitted handler instructions (zero if
/// there are no handlers), including loop trips, the larger IF arms, port
/// allowances, wrapper and frame clear. Adds RUNTIME_GAP_INSNS, timer_count *
/// TIMER_SCAN_INSNS and any configured radio burst/switch/masked-gate allowance. Rounds
/// the conversion at CPU_HZ upward (the model retires one instruction per CPU cycle; chip
/// cache-miss cost remains an M1b probe item). Self-feeding USB drains,
/// display and buffer copy/fill loops are excluded from handler bounds;
/// their individual feed gaps remain at most MAX_OP_INSNS.
pub fn build_image(m: &Module<'_>, board: &Board, o: &SlotOptions) -> Result<Image, Refusal> {
    if !m.funcs().is_empty() { return Err(refusal(FUNCTIONS_UNSUPPORTED)); }
    let layout = &board.layout;
    layout.check().map_err(refusal)?;
    let mut options = o.clone();
    options.flash_limit_bytes = layout.slot_pages * crate::PAGE;
    options.panel = board.panel;
    options.i2c = board.i2c;
    options.motion = board.motion;
    options.touch = board.touch;
    // facts-io I13: 3*SCLK <= (SDA_HOLD_TIME-4)*APB. Check the
    // programmed divider/hold values, rather than imposing a CPU-rate policy.
    // facts.md section 3 / TRM 7.2.4: APB=80 MHz with the CPU on the PLL (CPU_HZ).
    if uses_shared_i2c(m) {
        if let Some(bus) = board.i2c {
            let t = bus.timing()?;
            if u64::from(t.hold - 4) * u64::from(t.divider) * APB_HZ < 3 * 40_000_000 {
                return Err(refusal("shared I2C SDA hold timing violates the TRM APB/main-clock constraint (I13)"));
            }
        }
    }
    options.radio = board.radio.clone();
    options.rwdt_quarter_per_hold = crate::feed_quarter_per_hold(&board.time);
    let slot = build_slot_for_layout(m, &options, layout)?;
    let handler_insns = slot.handlers.iter().map(|h| h.max_insns).max().unwrap_or(0);
    let mut gap_insns = u128::from(handler_insns)
        + u128::from(runtime::RUNTIME_GAP_INSNS)
        + u128::from(slot.layout.timer_count) * u128::from(runtime::TIMER_SCAN_INSNS);
    if slot.layout.mic.is_some() {
        // An EOF can interrupt the longest foreground stretch. The ISR feeds
        // on entry to each block and every 64 samples through the copy.
        gap_insns += u128::from(runtime::MAX_OP_INSNS);
    }
    if let Some(radio) = &options.radio {
        // Island execution (including ISR bodies/shims) is the measured burst;
        // charge kernel save/pick/restore separately on both sides of it.
        gap_insns += u128::from(radio.burst_insns) + 2 * u128::from(runtime::radio_switch_insns(radio))
            + u128::from(slot.symbols.radio.as_ref().unwrap().masked_gate_insns);
    }
    let gap_ns = (gap_insns * 1_000_000_000).div_ceil(u128::from(crate::CPU_HZ));
    let gap_ns = u64::try_from(gap_ns).map_err(|_| refusal("RWDT feed gap exceeds the u64 nanosecond limit"))?;
    // boot-stage.md, Record: confirmed first image, with all three tries bits.
    let bundle_pages = u16::try_from(slot.bundle.len().div_ceil(crate::PAGE as usize))
        .map_err(|_| refusal("the file bundle's pages exceed the record's 16 bits"))?;
    let record = Record { seq: 1, slot: 0, prev: 0xff, wdt: wdt_hold(gap_ns, &board.time)?,
        length: slot.bytes.len() as u32, bundle_pages, sha256: sha256(&slot.bytes), tries: 7, confirmed: true };
    let end = layout.slots[0] as usize + slot.bytes.len();
    if layout.records.iter().any(|&at| at as usize + crate::SECTOR as usize > end) {
        return Err(refusal("record sectors must fit before the end of the slot-0 flash image"));
    }
    let mut flash = boot_stage(layout);
    flash.resize(end, 0xff);
    let at = layout.records[0] as usize;
    let bytes = record.bytes();
    flash[at..at + bytes.len()].copy_from_slice(&bytes);
    flash[layout.slots[0] as usize..].copy_from_slice(&slot.bytes);
    if !slot.bundle.is_empty() {
        let at = layout.bundles[0] as usize;
        flash.resize(at, 0xff);
        flash.extend_from_slice(&slot.bundle);
    }
    Ok(Image { flash, record, slot, gap_insns })
}

#[derive(Clone, Debug)]
pub struct SlotOptions {
    pub sites: Vec<(u32, u32)>,
    pub marks: Vec<(u32, u32)>,
    pub flash_limit_bytes: u32,
    pub panel: Option<Panel>,
    pub i2c: Option<I2cBus>,
    pub motion: Option<Motion>,
    pub touch: Option<Touch>,
    pub audio: Option<Audio>,
    pub radio: Option<Radio>,
    /// Report the analog I2C master's state and the BBPLL and DIG_REG registers as Probe frames
    /// before setup (`runtime::ANALOG_READS`), with the regulators as init's raise left them; then
    /// the settings it wrote and a clock check at CPU_HZ (`runtime::RAISE_PROBES`).
    pub analog_check: bool,
    /// The board's [`crate::feed_quarter_per_hold`]; zero feeds the RWDT at every feed point.
    pub rwdt_quarter_per_hold: u32,
    /// The module's image names in its order (`fab_elec_mcu::Compiled::images`): the slot's file
    /// bundle files each image under its name.
    pub image_names: Vec<String>,
}

impl Default for SlotOptions {
    fn default() -> Self {
        Self { sites: Vec::new(), marks: Vec::new(), flash_limit_bytes: crate::VOICEBOX.slot_pages * 65536, panel: None, i2c: None, motion: None, touch: None, audio: None, radio: None, analog_check: false, rwdt_quarter_per_hold: 0, image_names: Vec::new() }
    }
}

/// One buffer's allocation, including its trailing word padding.
#[derive(Clone, Debug)]
pub struct BufferPlace {
    pub name: String,
    pub elem: Elem,
    /// Index into `SramLayout::regions`; None for read-only slot data in flash.
    pub region: Option<usize>,
    pub address: u32,
    /// Element bytes, excluding trailing word padding.
    pub bytes: u64,
    pub allocated_bytes: u64,
}

/// A memory span buffers fill, in placement order: internal SRAM between
/// module state and the largest handler stack; P5 appends PSRAM.
#[derive(Clone, Debug)]
pub struct BufferRegion {
    pub name: &'static str,
    pub start: u64,
    /// Exclusive. Below `start` when state and the stack alone overflow.
    pub end: u64,
    /// End of the buffers placed here; boot clears `start..used`.
    pub used: u64,
}

/// Watch receive, lifetime and LINK report state shared by Ethernet and radio images.
#[derive(Clone, Debug)]
pub struct LinkSram {
    pub rx_bytes: u32,
    pub rx_left: u32,
    pub rx_zero: u32,
    /// 0 idle, 1 decoding, 2 refused, 3 malformed (discard through delimiter).
    pub rx_mode: u32,
    pub rx_crc: u32,
    /// Kind/sequence then the unaligned little-endian tick, word padded.
    pub rx_header: u32,
    /// Five consecutive little-endian u64 pairs in LINK counter order.
    pub in_edges: u32,
    /// Host frames of any kind refused for a stale epoch.
    pub eth_stale: u32,
    pub tx_dropped: u32,
    pub rx_bad: u32,
    pub rx_refused: u32,
    /// Last successfully enqueued values, in the same order as the counters.
    pub reported: u32,
    pub report_index: u32,
    pub report_return: u32,
    /// 0 disarmed, 1 observing the 250 ms window, 2 waiting for an IN token.
    pub stall_state: u32,
    pub stall_due: u32,
    /// 0 none, 1 host-open edge (also boot), 2 no-answer fallback.
    pub rehello: u32,
    pub hello_return: u32,
    pub image_crc: u32,
    pub idle_return: u32,
    pub epoch: u32,
    pub rx_return: u32,
}

/// Ethernet slots, grants, answer tracking and writable TX body.
#[derive(Clone, Debug)]
pub struct EthSram {
    pub count: u32,
    pub entry: usize,
    /// Slots contain a u16 length followed by ETH_MAX bytes, word padded.
    /// Zero length means free; all count slots can therefore be occupied.
    pub slots: u32,
    pub head: u32,
    pub tail: u32,
    pub rx_slot: u32,
    pub received: u32,
    pub seen: u32,
    pub owed: u32,
    /// 0 no ETH sent this lifetime, 1 awaiting an answer, 2 answered.
    pub answer_state: u32,
    pub answer_wait: u32,
    pub answer_due: u32,
    pub due: u32,
    pub credit_return: u32,
    /// Writable body for F2's atomic enqueue (header, payload, CRC).
    pub tx_body: u32,
}

/// Per-connection storage. A slot starts with its length and part flags.
#[derive(Clone, Debug)]
pub struct NetStorage {
    pub entry: usize,
    pub slots: u32,
    pub slot_count: u32,
    pub send: u32,
    pub send_bytes: u32,
    /// Three packed bits per queued byte suffice even for one-byte messages.
    pub boundaries: u32,
}

#[derive(Clone, Debug)]
pub(crate) struct NetTx {
    pub body: u32,
    pub part_bytes: u32,
}

#[derive(Clone, Debug)]
pub struct NetSram {
    pub connections: Vec<NetStorage>,
    pub(crate) rows: u32,
    pub(crate) link_state: u32,
    pub(crate) rx_payload: u32,
    pub(crate) tx: Option<NetTx>,
    pub(crate) tickets: u32,
    pub(crate) ticket_count: u32,
    pub(crate) ticket_head: u32,
    pub(crate) ticket_tail: u32,
    pub(crate) flush_return: u32,
}

/// I2S0 OUT ring in internal SRAM. Each sample occupies one stereo I16 frame.
#[derive(Clone, Debug)]
pub struct SpeakerSram {
    pub(crate) audio: Audio,
    pub(crate) clock: AudioClock,
    pub(crate) rounding_note: Option<String>,
    pub entry: usize,
    pub count: u32,
    /// SRAM stride in stereo frames. GDMA skips the padding in shorter slots.
    pub slot_frames: u32,
    pub descriptors: u32,
    pub buffer: u32,
    /// Set only after all codec setup writes succeeded.
    pub available: u32,
    /// Tracks TX_UPDATE, the first EOF and running DMA across service calls.
    pub(crate) phase: u32,
    pub(crate) stalled: u32,
    /// A short play can retry from the refill handler on the next observed EOF.
    pub(crate) retry_refill: u32,
    /// 0 after W1C, 1 for a serviced latch, 2 while checking an ambiguous lap.
    pub(crate) raw_seen: u32,
    /// After a delayed anchor, bracket an actual edge before writing its successor.
    pub(crate) uncertain: u32,
    pub(crate) previous_poll: u32,
    /// Actual observation used for the stall deadline, independent of the playback grid.
    pub(crate) progress_at: u32,
    /// Index of the first descriptor whose EOF has yet to be serviced.
    pub head: u32,
    /// Next physical slot offset in frames from head, including the in-flight slot and padding.
    pub write: u32,
    pub queued: u32,
    /// Per descriptor: queued prefix length, negated when played and awaiting clearing.
    pub lengths: u32,
    /// Number of nonzero lengths, including outstanding zero-fills.
    pub dirty: u32,
    /// Fault argument awaiting publication after the head and queue are consistent.
    pub lost: u32,
    /// A full tail drained with a refill pending. Confirm the first silent descriptor at EOF.
    pub(crate) confirm: u32,
    /// CALL0 return, PS and a12..a15 across speaker service.
    pub(crate) saved: u32,
    /// Private hardware clock sample. Ports preserve the running handler's entry stamp.
    pub(crate) now: u32,
    /// Two-word tick of the last serviced descriptor boundary.
    pub last_eof: u32,
    /// First pending refill's two-word tick and its generation counters.
    pub due: u32,
    pub received: u32,
    pub seen: u32,
}

impl SpeakerSram {
    pub(crate) fn capacity_frames(&self) -> u32 { self.count * self.clock.hz / 200 }
}

impl Audio {
    fn check_pins(&self, pins: &[Pin], bus: I2cBus, panel: Option<Panel>, capture: bool) -> Result<(), Refusal> {
        let mut used: Vec<_> = pins.iter().map(|p| p.number).collect();
        used.extend([bus.scl, bus.sda]);
        if let Some(p) = panel { used.extend([p.cs, p.sck, p.mosi]); }
        if let Some(pin) = self.asdout {
            if capture && (!matches!(pin, 0..=21 | 26..=48) || used.contains(&pin)) {
                return Err(refusal(format!("mic input GPIO{pin} is missing or already in use")));
            }
            used.push(pin);
        }
        let mut reserve = |pin| {
            if !matches!(pin, 0..=21 | 26..=48) || used.contains(&pin) {
                return Err(refusal(format!("audio output GPIO{pin} is missing or already in use")));
            }
            used.push(pin);
            Ok(())
        };
        for pin in [self.mclk, self.sclk, self.lrck, self.dsdin] { reserve(pin)?; }
        for amp in self.amp_enables() { reserve(amp.gpio)?; }
        Ok(())
    }
}

/// IN2 receives stereo blocks. The interrupt publishes mono history after copying a whole block.
#[derive(Clone, Debug)]
pub struct MicSram {
    pub(crate) audio: Audio,
    pub(crate) clock: AudioClock,
    pub entry: usize,
    /// Stereo frames in one 20 ms DMA block, and mono samples held across all history blocks.
    pub frames: u32,
    pub capacity: u32,
    pub descriptors: u32,
    pub buffer: u32,
    pub history: u32,
    /// Next descriptor to retire, and next mono sample position to overwrite.
    pub(crate) next: u32,
    pub(crate) write: u32,
    /// Held and unread samples. The read cursor is (write - ready) modulo capacity.
    pub(crate) held: u32,
    pub ready: u32,
    pub(crate) taking: u32,
    pub(crate) lost: u32,
    pub(crate) reported: u32,
    pub(crate) phase: u32,
    pub(crate) stalled: u32,
    pub(crate) progress: u32,
    /// SYSTIMER ticks paired with CPU cycles for timestamping an interrupt.
    pub(crate) edge: u32,
    pub(crate) cycle_ref: u32,
    pub(crate) stamp_cycle: u32,
    pub(crate) irq_save: u32,
    pub(crate) call_return: u32,
    pub(crate) port_args: u32,
    /// First pending block's two-word tick and its generation counters.
    pub due: u32,
    pub received: u32,
    pub seen: u32,
}

impl MicSram {
    // The current and prefetched successor headers belong to GDMA (ring-facts §3).
    // Four slots leave two retired headers available even when two EOFs coalesce.
    pub const DESCRIPTORS: u32 = 4;
}

/// Guard at base, exclusive top; both ends are 16-aligned.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RadioStack { pub base: u32, pub top: u32 }

/// Radio allocations inside the runtime's zero-cleared span.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RadioSram {
    /// Last word of the linked island window, below the runtime clear.
    pub island_guard: u32,
    pub island_status: u32,
    /// Whole allocation, including alignment padding, checked by raw ports.
    pub start: u32,
    pub end: u32,
    pub kernel_state: u32,
    pub kernel_state_words: u32,
    pub heap: u32,
    pub phy_cal_data: u32,
    /// Internal SRAM for the PHY backup DMA.
    pub phy_dig_regs: u32,
    pub phy_mutex: u32,
    pub osi_table: u32,
    /// wifi_init_config_t follows the OSI words in the same restart copy.
    pub wifi_config: Option<u32>,
    pub current: u32,
    pub cursor: u32,
    pub task_count: u32,
    pub isr_count: u32,
    pub stray: u32,
    pub isr_start: u32,
    pub isr_max_span: u32,
    pub isr_max_entry: u32,
    pub radio_state: u32,
    pub fault: u32,
    pub stop_cause: u32,
    pub stop_pending: u32,
    /// Next four-word fault frame; advances only after WIRE accepts it.
    pub report_next: u32,
    pub report_return: u32,
    /// Word-aligned head, tail, saturating lost, then eight {code, site, arg}
    /// word triples. Head/tail are free-running; slots use index & (N - 1).
    pub reports: u32,
    pub generation: u32,
    pub consecutive: u32,
    pub request: u32,
    pub restart_return: u32,
    pub boot_wifi_clk: u32,
    pub boot_wifi_rst: u32,
    pub boot_phy_ram: u32,
    /// Snapshot state: 0 pending, 1 saved, 2 refused.
    pub phy_table_state: u32,
    pub phy_table_at: u32,
    pub phy_table: u32,
    /// Relative SYSTIMER tick of the last start marker (two words).
    pub started_at: u32,
    /// Two-word restart deadline. Zero while running, MAX when the stop has been
    /// recognized and recovery is waiting for a record or a request.
    pub next_start: u32,
    pub reports_return: u32,
    pub trace_head: u32,
    pub trace: u32,
    pub tasks: u32,
    pub isrs: u32,
    pub dispatcher: u32,
    pub idle: u32,
    pub boot_tasks: Vec<u32>,
    pub idle_stack: RadioStack,
    pub boot_stacks: Vec<RadioStack>,
    pub irq_stack: RadioStack,
}

impl RadioSram {
    pub(crate) fn osi_config_words(&self) -> u32 {
        let config = if self.wifi_config.is_some() { radio::start_glue::CONFIG_WORDS } else { 0 };
        (radio::adapter::OSI_FIELDS.len() + config) as u32
    }

    /// PHY globals in ROM-owned SRAM, ESP-IDF v6.1.1 esp32s3.rom.ld.
    pub const PHY_ROM_RAM: u32 = 0x3fce_f81c;
    pub const PHY_ROM_WORDS: u32 = (0x3fce_f958 - Self::PHY_ROM_RAM) / 4;
    /// 656 bytes: phy_get_romfunc_addr in ESP-IDF v6.1.1 components/esp_phy/lib/esp32s3/libphy.a
    /// (phy_init.o) stores at offsets 8..652.
    pub const PHY_TABLE_WORDS: u32 = 164;
    /// Start of ROM-owned RAM above STACK_TOP.
    pub const ROM_DATA_START: u32 = 0x3fce_f000;
    /// Exclusive end of ROM-owned RAM above STACK_TOP.
    pub const ROM_DATA_END: u32 = 0x3fcf_0000;

    /// Word ranges to rebuild, interleaved with the cause, ring and boot survivors.
    pub fn clear_spans(&self) -> [std::ops::Range<u32>; 3] {
        let ring_end = self.reports + 12 + radio::REPORT_RING_ENTRIES * 12;
        assert_eq!(ring_end, self.generation);
        assert_eq!(self.boot_phy_ram + Self::PHY_ROM_WORDS * 4, self.phy_table_state);
        assert_eq!(self.phy_table_state + 4, self.phy_table_at);
        assert_eq!(self.phy_table_at + 4, self.phy_table);
        assert_eq!(self.phy_table + Self::PHY_TABLE_WORDS * 4, self.started_at);
        assert_eq!(self.started_at + 8, self.next_start);
        assert_eq!(self.next_start + 8, self.reports_return);
        let ends = [self.start, self.stop_cause, self.stop_cause + 4, self.reports,
            ring_end, self.reports_return, self.end];
        assert!(ends.windows(2).all(|p| p[0] <= p[1]) && ends.iter().all(|a| a & 3 == 0));
        [ends[0]..ends[1], ends[2]..ends[3], ends[5]..ends[6]]
    }

    fn new(start: u64, profile: &Radio, board: &Layout, usage: &mut SramUsage) -> Result<(Self, Compiled), Refusal> {
        let mut next = start;
        let mut take = |bytes: u64, align: u64| -> u32 {
            next = (next + align - 1) & !(align - 1);
            let at = next;
            next += bytes;
            // An unplaceable allocation has no device address. Keep measuring it
            // in u64 so the refusal includes the rest of the profile as well.
            if next <= u64::from(crate::STACK_TOP) { at as u32 } else { 0 }
        };
        // Keep the cursor beside current for irq_entry's direct idle wake.
        let current = take(u64::from(radio::CURSOR_OFFSET) + 4, 4);
        let cursor = current + radio::CURSOR_OFFSET;
        let task_count = take(4, 4); let isr_count = take(4, 4); let stray = take(4, 4);
        // Init clears these three scheduler words with stray in the t0..state span.
        let isr_start = take(4, 4); let isr_max_span = take(4, 4); let isr_max_entry = take(4, 4);
        let radio_state = take(4, 4);
        let island_status = take(4, 4);
        let fault = take(u64::from(radio::STOP_PENDING_OFFSET) + 4, 4);
        let stop_cause = fault + radio::STOP_CAUSE_OFFSET;
        let stop_pending = fault + radio::STOP_PENDING_OFFSET;
        let report_next = take(4, 4); let report_return = take(4, 4);
        let reports = take(12 + u64::from(radio::REPORT_RING_ENTRIES) * 12, 4);
        let generation = take(4, 4); let consecutive = take(4, 4); let request = take(4, 4);
        let restart_return = take(4, 4); let boot_wifi_clk = take(4, 4); let boot_wifi_rst = take(4, 4);
        let boot_phy_ram = take(u64::from(Self::PHY_ROM_WORDS) * 4, 4);
        let phy_table_state = take(4, 4);
        let phy_table_at = take(4, 4);
        let phy_table = take(u64::from(Self::PHY_TABLE_WORDS) * 4, 4);
        let started_at = take(8, 4); let next_start = take(8, 4);
        let reports_return = take(4, 4);
        let trace_head = take(4, 4);
        let trace = take(u64::from(profile.trace_entries) * u64::from(radio::TRACE_ENTRY_WORDS) * 4, 4);
        let tasks = take(u64::from(profile.task_capacity) * 4, 4);
        let isrs = take(u64::from(profile.isr_capacity) * u64::from(radio::ISR_SLOT), 4);
        let dispatcher = take(radio::TCB_SIZE as u64, 4);
        let idle = take(radio::TCB_SIZE as u64, 4);
        let boot_tasks = (0..profile.boot_count()).map(|_| take(radio::TCB_SIZE as u64, 4))
            .collect();
        let heap = take(u64::from(profile.heap_bytes), 8);
        let phy_cal_data = take(1904, 4);
        let phy_dig_regs = take(84, 4);
        let phy_mutex = take(u64::from(radio::kernel::MUTEX_BYTES), 4);
        let osi_bytes = radio::adapter::OSI_FIELDS.len() as u64 * 4;
        let (osi_table, wifi_config) = if profile.start.is_some() {
            // The inline u64 needs 8-byte alignment. The OSI table occupies 480 bytes.
            assert_eq!(osi_bytes % 8, 0);
            let osi = take(osi_bytes + 4 * radio::start_glue::CONFIG_WORDS as u64, 8);
            (osi, Some(osi + osi_bytes as u32))
        } else { (take(osi_bytes, 4), None) };
        let mut stack = |bytes: u32| -> RadioStack {
            let base = take(u64::from(bytes), 16);
            RadioStack { base, top: base + bytes }
        };
        let idle_stack = stack(radio::IDLE_STACK_BYTES);
        let boot_stacks = profile.stack_sizes().map(&mut stack).collect();
        let irq_stack = stack(profile.irq_stack_bytes);
        let mut r = Self { start: start as u32, end: irq_stack.top, island_guard: profile.island.guard, island_status,
            kernel_state: 0, kernel_state_words: 0,
            heap, phy_cal_data, phy_dig_regs, phy_mutex, osi_table, wifi_config,
            current, cursor, task_count, isr_count, stray, isr_start, isr_max_span, isr_max_entry,
            radio_state, fault, stop_cause, stop_pending, report_next, report_return, reports, reports_return, trace_head, trace, tasks, isrs,
            generation, consecutive, request, restart_return, boot_wifi_clk, boot_wifi_rst, boot_phy_ram,
            phy_table_state, phy_table_at, phy_table, started_at, next_start,
            dispatcher, idle, boot_tasks, idle_stack, boot_stacks, irq_stack };
        let kernel_start = next;
        let mut layout = r.kernel_layout(profile);
        if kernel_start > u64::from(crate::STACK_TOP) {
            // Only size the kernel state for this refusal. State consists of declared
            // scalar variables and export slots, independent of addresses and loop bounds.
            // Unplaceable heap sizes and capacities need not be representable as kernel i32s.
            layout.heap_size = radio::kernel::heap_control_bytes(profile.heap_bytes);
            layout.max_tasks = 2;
            layout.timer_reserve = 0;
            layout.isr_capacity = radio::PACK_ISR_LINES;
        }
        let (compiled, state_words) = profile.check(board, &layout)?;
        let end = kernel_start + u64::from(state_words) * 4;
        usage.radio = end - start;
        if end > u64::from(crate::STACK_TOP) {
            return Err(usage.refusal(u64::from(radio::DISPATCHER_HEADROOM)));
        }
        r.kernel_state = kernel_start as u32;
        r.kernel_state_words = state_words;
        r.end = end as u32;
        Ok((r, compiled))
    }

    fn kernel_layout(&self, profile: &Radio) -> radio::kernel::KernelLayout {
        radio::kernel::KernelLayout { current: self.current, tasks: self.tasks, task_count: self.task_count,
            max_tasks: profile.task_capacity, timer_reserve: profile.timer_reserve,
            heap_base: self.heap, heap_size: profile.heap_bytes, code_region: SLOT_BASE >> 30,
            isrs: self.isrs, isr_count: self.isr_count, isr_capacity: profile.isr_capacity,
            phy_cal_data: self.phy_cal_data,
            phy_dig_regs: self.phy_dig_regs, phy_mutex: self.phy_mutex }
    }
}

/// Absolute data addresses. Runtime scratch is static: dispatcher ports cannot
/// nest. Radio tasks own separate stacks and must not re-enter these ports.
#[derive(Clone, Debug)]
pub struct SramLayout {
    pub t0: u32,
    pub timers: u32,
    pub timer_count: u32,
    pub time_sent: u32,
    pub seq: u32,
    pub ordinal: u32,
    pub stamp: u32,
    pub reset_cause: u32,
    pub previous_handler: u32,
    /// The CCOUNT the next RWDT feed is due at, then the feeds' quarter in CPU cycles.
    pub rwdt_due: u32,
    pub event: u32,
    pub port_return: u32,
    pub event_return: u32,
    pub wire_return: u32,
    pub wire_body: u32,
    pub wire_written: u32,
    pub read_value: u32,
    pub body: u32,
    pub tx_head: u32,
    pub tx_tail: u32,
    pub tx_packet: u32,
    pub tx_ring: u32,
    pub lost_count: u32,
    pub lost_saturated: u32,
    pub lost_first: u32,
    pub lost_last: u32,
    pub link: Option<LinkSram>,
    pub eth: Option<EthSram>,
    pub net: Option<NetSram>,
    pub panel: Option<PanelSram>,
    pub motion: Option<MotionSram>,
    pub touch: Option<TouchSram>,
    pub speaker: Option<SpeakerSram>,
    pub mic: Option<MicSram>,
    /// One shared owner: timeout retains I2C until COMPLETE for all clients.
    pub i2c_pending: Option<u32>,
    pub radio: Option<RadioSram>,
    pub buffer_scratch: u32,
    pub range_faults: u32,
    pub range_fault_words: u32,
    /// Indexed by module buffer number; RAM addresses follow size order.
    /// Read-only addresses are linked to slot data after handler sizing.
    pub buffers: Vec<BufferPlace>,
    pub regions: Vec<BufferRegion>,
    pub state: u32,
    pub state_words: u32,
    pub runtime_bytes: u32,
    pub stack_top: u32,
    pub stack_bytes: u32,
}

struct SramUsage {
    window_end: u32,
    runtime: u64,
    radio: u64,
    state: u64,
    buffers: u64,
}

impl SramUsage {
    fn t0(&self) -> u64 { u64::from(self.window_end).next_multiple_of(16) }

    fn end(&self) -> u64 {
        (self.t0() + u64::from(chip::profile().ram_words) * 4).min(u64::from(crate::STACK_TOP))
    }

    fn refusal(&self, stack: u64) -> Refusal {
        let base = crate::SRAM_START;
        let t0 = self.t0();
        let end = self.end();
        let required = self.runtime + self.radio + self.state + self.buffers + stack;
        refusal(format!("internal SRAM: island window {base:#010x}..{:#010x} ({} bytes), alignment padding {} bytes, \
            runtime {} bytes, radio {} bytes, state {} bytes, padded buffers {} bytes, stack reservation {stack} bytes \
            (includes largest handler stack once sized), available {} bytes, deficit {} bytes",
            self.window_end, self.window_end - base, t0 - u64::from(self.window_end), self.runtime,
            self.radio, self.state, self.buffers, end.saturating_sub(t0), t0 + required - end))
    }
}

impl SramLayout {
    fn new(m: &Module<'_>, timers: usize, options: &SlotOptions, board: &Layout) -> Result<(Self, Option<Compiled>, SramUsage), Refusal> {
        let profile = options.radio.as_ref();
        // facts.md §2: SRAM1 data alias; boot-stage.md: a1 at handoff.
        let mut usage = SramUsage {
            window_end: profile.map_or(crate::SRAM_START, |p| p.island.window_end), runtime: 0, radio: 0,
            state: u64::from(m.state_words()) * 4,
            buffers: m.buffers().iter().filter(|b| b.writable).map(|b| b.words() as u64 * 4).sum(),
        };
        let stack_bytes = if profile.is_some() { radio::DISPATCHER_HEADROOM } else { 0 };
        // Init clears words from t0 to state. Round past the guard to retain the
        // original 16-byte alignment of the runtime base and the two-word clock.
        let app_start = usage.t0();
        let mut next = app_start;
        let mut take = |bytes: u64| {
            let at = next;
            next += bytes;
            if next <= u64::from(crate::STACK_TOP) { at as u32 } else { 0 }
        };
        let t0 = take(8);
        let table = take(timers as u64 * u64::from(runtime::TIMER_BYTES));
        let time_sent = take(4);
        let seq = take(4);
        let ordinal = take(4);
        let stamp = take(8);
        let reset_cause = take(4);
        let previous_handler = take(4);
        let rwdt_due = take(8);
        let event = take(16);
        let port_return = take(4);
        let event_return = take(4);
        let wire_return = take(4);
        let wire_body = take(4);
        let wire_written = take(4);
        let read_value = take(4);
        let body = take((watch::MAX_BODY as u64 + 3) & !3);
        let tx_head = take(4);
        let tx_tail = take(4);
        let tx_packet = take(4);
        let lost_count = take(4);
        let lost_saturated = take(4);
        let lost_first = take(4);
        let lost_last = take(4);
        let tx_ring = take(u64::from(runtime::TX_RING_BYTES));
        let eth_decl = m.decls().iter().find(|d| d.form == ports::FORM_ETH);
        let declarations = m.net();
        let link = (eth_decl.is_some() || profile.is_some() || !declarations.is_empty()).then(|| LinkSram {
            rx_bytes: take(4), rx_left: take(4), rx_zero: take(4), rx_mode: take(4), rx_crc: take(4),
            rx_header: take(8),
            in_edges: take(8), eth_stale: take(8), tx_dropped: take(8), rx_bad: take(8), rx_refused: take(8),
            reported: take(40), report_index: take(4), report_return: take(4),
            stall_state: take(4), stall_due: take(8), rehello: take(4), hello_return: take(4), image_crc: take(4),
            idle_return: take(4), epoch: take(4), rx_return: take(4),
        });
        let eth = if let Some(d) = eth_decl {
            let [DeclArg::Int(count), DeclArg::Entry(entry)] = d.args.as_slice() else {
                unreachable!("verified eth declaration");
            };
            let bytes = *count as u64 * u64::from(runtime::RX_SLOT_BYTES);
            Some(EthSram {
                count: *count as u32, entry: *entry as usize, slots: take(bytes),
                head: take(4), tail: take(4), rx_slot: take(4), received: take(4), seen: take(4), owed: take(4),
                answer_state: take(4), answer_wait: take(4), answer_due: take(8), due: take(8), credit_return: take(4),
                tx_body: take((watch::ETH_BODY as u64 + 3) & !3),
            })
        } else { None };
        let net = (!declarations.is_empty()).then(|| {
            let count = declarations.len() as u64;
            let sends = emit::uses_port(m, ports::NET_SEND);
            let receives = emit::uses_port(m, ports::NET_RECV);
            let send_bytes: u64 = declarations.iter().map(|d| u64::from(d.send_bytes)).sum();
            let largest = declarations.iter().map(|d| u64::from(d.send_bytes)).max().unwrap();
            // Adjacent Data for one connection shares a ticket. Each separate run
            // of the largest sender needs a byte from another sender or a control
            // between runs. Three Open/Close plus one summed Freed per reader.
            let controls = (3 + u64::from(receives)) * count;
            let runs = if sends { send_bytes.min(2 * (send_bytes - largest) + controls + 1) } else { 0 };
            let ticket_count = controls + runs + 1;
            let connections = declarations.iter().map(|d| NetStorage {
                entry: usize::from(d.entry), slots: take(u64::from(d.slots) * u64::from(runtime::net::SLOT_BYTES)),
                slot_count: d.slots, send: take(u64::from(d.send_bytes).next_multiple_of(4)), send_bytes: d.send_bytes,
                boundaries: take(3 * u64::from(d.send_bytes).div_ceil(32) * 4),
            }).collect();
            NetSram { connections, rows: take(count * u64::from(runtime::net::ROW_BYTES)),
                link_state: take(12), rx_payload: take((watch::ETH_MAX as u64 + 1).next_multiple_of(4)),
                tx: sends.then(|| NetTx {
                    body: eth.as_ref().map_or_else(|| take((watch::ETH_BODY as u64).next_multiple_of(4)), |e| e.tx_body),
                    part_bytes: take(4),
                }),
                tickets: take(4 * ticket_count), ticket_count: u32::try_from(ticket_count).unwrap_or(u32::MAX),
                ticket_head: take(4), ticket_tail: take(4), flush_return: take(4) }
        });
        // G03/G03a: word-aligned descriptors and buffers in internal SRAM.
        // Lead choice: a zero availability word suppresses traffic after failed bring-up.
        let panel = m.display().map(|_| PanelSram {
            available: take(4), scratch: take(u64::from(panel::SCRATCH_BYTES)),
            buffer: take(u64::from(panel::BUFFER_BYTES)),
            descriptors: take(u64::from(panel::DESCRIPTORS) * 12),
        });
        let motion = m.motion().map(|_| MotionSram {
            timer: table + (timers as u32 - 1 - u32::from(m.touch())) * runtime::TIMER_BYTES,
            initialized: take(4), sample: take(24), status: take(4),
        });
        let touch = m.touch().then(|| TouchSram {
            timer: table + (timers as u32 - 1) * runtime::TIMER_BYTES,
            initialized: take(4), report: take(12),
        });
        let speaker = if let Some(d) = m.decls().iter().find(|d| d.form == ports::FORM_SPEAKER) {
            let [DeclArg::Int(hz), DeclArg::Int(ms), DeclArg::Entry(entry)] = d.args.as_slice() else {
                unreachable!("verified speaker declaration");
            };
            let audio = options.audio.as_ref().ok_or_else(|| refusal("the board has no speaker codec"))?;
            if !matches!(audio.addr, 0x18 | 0x19) { return Err(refusal("ES8311 address must be 0x18 or 0x19")); }
            let clock = audio_clock(*hz as u32)?;
            let queue_ms = *ms as u32;
            let step = 5 * clock.descriptor_cycle();
            // Every supported step divides the verified 200 ms ceiling.
            let actual_ms = queue_ms.next_multiple_of(step);
            let rounding_note = (actual_ms != queue_ms).then(|| format!(
                "speaker queue {queue_ms} ms at {hz} Hz rounds up to {actual_ms} ms (whole {step} ms steps)"));
            let frames = clock.hz.div_ceil(200);
            let count = actual_ms / 5;
            Some(SpeakerSram {
                audio: audio.clone(), clock, rounding_note, entry: *entry as usize, count, slot_frames: frames,
                descriptors: take(u64::from(count) * 12), buffer: take(u64::from(count) * u64::from(frames) * 4),
                available: take(4), head: take(4), write: take(4), queued: take(4), lost: take(4), confirm: take(4),
                phase: take(4), stalled: take(4), retry_refill: take(4), raw_seen: take(4), uncertain: take(4),
                previous_poll: take(8), progress_at: take(8),
                lengths: take(u64::from(count) * 4), dirty: take(4), saved: take(24), now: take(8),
                last_eof: take(8), due: take(8), received: take(4), seen: take(4),
            })
        } else { None };
        let mic = if let Some(d) = m.decls().iter().find(|d| d.form == ports::FORM_MIC) {
            let [DeclArg::Int(hz), DeclArg::Int(ms), DeclArg::Entry(entry)] = d.args.as_slice() else {
                unreachable!("verified mic declaration");
            };
            let audio = options.audio.as_ref().filter(|a| a.asdout.is_some())
                .ok_or_else(|| refusal("the board has no codec with a mic input"))?;
            if !matches!(audio.addr, 0x18 | 0x19) { return Err(refusal("ES8311 address must be 0x18 or 0x19")); }
            let clock = audio_clock(*hz as u32)?;
            let frames = clock.hz / 50;
            let capacity = frames * (*ms as u32 / 20);
            Some(MicSram {
                audio: audio.clone(), clock, entry: *entry as usize, frames, capacity,
                descriptors: take(u64::from(MicSram::DESCRIPTORS * 12)),
                buffer: take(u64::from(MicSram::DESCRIPTORS * frames * 4)),
                history: take(u64::from(capacity * 2).next_multiple_of(4)),
                next: take(4), write: take(4), held: take(4), ready: take(4), taking: take(4),
                lost: take(4), reported: take(4), phase: take(4), stalled: take(4), progress: take(4),
                edge: take(8), cycle_ref: take(4), stamp_cycle: take(4),
                irq_save: take(40), call_return: take(4), port_args: take(16),
                due: take(8), received: take(4), seen: take(4),
            })
        } else { None };
        let i2c_pending = (m.motion().is_some() || m.touch() || speaker.is_some() || mic.is_some()).then(|| take(4));
        let buffer_scratch = take(if m.buffers().is_empty() { 0 } else { u64::from(runtime::BUFFER_SCRATCH_BYTES) });
        let range_fault_words = if m.buffers().is_empty() { 0 } else { m.n_sites().div_ceil(32) };
        let range_faults = take(u64::from(range_fault_words) * 4);
        usage.runtime = next - app_start;
        let (radio, compiled) = if let Some(profile) = profile {
            let (radio, compiled) = RadioSram::new(next, profile, board, &mut usage)?;
            next = u64::from(radio.end);
            (Some(radio), Some(compiled))
        } else { (None, None) };
        if next > u64::from(crate::STACK_TOP) { return Err(usage.refusal(u64::from(stack_bytes))); }
        let state = next as u32;
        let buffers = m.buffers().iter().enumerate().map(|(k, b)| BufferPlace {
            name: m.string(m.decls()[b.decl as usize].name).to_owned(), elem: b.elem,
            // Distinct provisional flash addresses keep every relocation large.
            // The assembler currently gives each use its own literal (no dedup).
            region: b.writable.then_some(0), address: if b.writable { 0 } else { SLOT_DATA_BASE + k as u32 * 4 },
            bytes: u64::from(b.len) * u64::from(b.elem.bytes()),
            allocated_bytes: b.words() as u64 * 4,
        }).collect();
        let mut layout = Self { t0, timers: table, timer_count: timers as u32, time_sent, seq, ordinal,
            stamp, reset_cause, previous_handler, rwdt_due, event, port_return, event_return, wire_return,
            wire_body, wire_written, read_value, body, tx_head, tx_tail, tx_packet, tx_ring,
            lost_count, lost_saturated, lost_first, lost_last,
            link, eth, net, panel, motion, touch, speaker, mic, i2c_pending, radio, buffer_scratch, range_faults, range_fault_words, buffers,
            regions: Vec::new(), state, state_words: m.state_words(), runtime_bytes: state - t0,
            stack_top: crate::STACK_TOP, stack_bytes };
        layout.place_buffers(&usage)?;
        Ok((layout, compiled, usage))
    }

    /// The regions buffers may fill, in placement order. Use u64 even for
    /// modules verified with a profile larger than the board's pool.
    fn buffer_regions(&self, usage: &SramUsage) -> Vec<BufferRegion> {
        let end = usage.end();
        let start = u64::from(self.state) + u64::from(self.state_words) * 4;
        vec![BufferRegion { name: "internal SRAM", start, end: end.saturating_sub(u64::from(self.stack_bytes)), used: start }]
    }

    /// Recheck once the actual largest handler frame is known.
    fn place_buffers(&mut self, usage: &SramUsage) -> Result<(), Refusal> {
        let mut regions = self.buffer_regions(usage);
        let sram = &regions[0];
        if sram.start > sram.end {
            return Err(usage.refusal(u64::from(self.stack_bytes)));
        }
        place(&mut self.buffers, &mut regions).map_err(|map| {
            let mut error = usage.refusal(u64::from(self.stack_bytes));
            error.message.push_str(&map);
            error
        })?;
        self.regions = regions;
        Ok(())
    }
}

/// The placement rule, in one place: smallest writable buffer first, ties by number,
/// each into the first region with room left, so a buffer that does not fit
/// one region spills to the next. Allocations are word multiples from word
/// aligned starts. When any buffer fits nowhere, returns the whole map.
fn place(buffers: &mut [BufferPlace], regions: &mut [BufferRegion]) -> Result<(), String> {
    let free: Vec<_> = regions.iter().map(|r| r.end.saturating_sub(r.start)).collect();
    let mut order: Vec<_> = (0..buffers.len()).collect();
    order.sort_by_key(|&k| (buffers[k].bytes, k));
    let mut buffer_map = String::new();
    let mut fits = true;
    for k in order {
        let b = &mut buffers[k];
        let at = if b.region.is_none() {
            "flash (read-only slot data)".to_owned()
        } else {
            match regions.iter().position(|r| r.used + b.allocated_bytes <= r.end) {
                Some(r) => {
                    b.region = Some(r); b.address = regions[r].used as u32; regions[r].used += b.allocated_bytes;
                    format!("{:#010x} in {}", b.address, regions[r].name)
                }
                None => { fits = false; "does not fit".to_owned() }
            }
        };
        buffer_map.push_str(&format!("\n  buffer {k} `{}` {:?}: {} bytes ({} allocated), {at}",
            b.name, b.elem, b.bytes, b.allocated_bytes));
    }
    if fits { return Ok(()); }
    let mut map = String::new();
    for (r, free) in regions.iter().zip(free) {
        map.push_str(&format!("\n  region {} {:#010x}..{:#010x}: {free} free bytes before buffers, {} free bytes after",
            r.name, r.start, r.end, r.end.saturating_sub(r.used)));
    }
    Err(map + &buffer_map)
}

#[derive(Clone, Debug)]
pub struct Handler {
    /// Module entry number, also the STORE0 and watchdog Fault.site id.
    pub id: u32,
    pub name: String,
    pub address: u32,
    pub stack_bytes: u32,
    /// Bounds retired instructions for one call, from the first instruction at
    /// `address` through RET_N inclusive, excluding self-feeding USB drains
    /// and display/buffer loops. Counts final relaxed body instructions with
    /// loop trip bounds, larger IF arms and bounded inline helper repetitions;
    /// adds each port's allowance, ENTRY_FIXED_INSNS for the wrapper, and
    /// `stack_bytes - 16` for clearing this handler's own frame.
    pub max_insns: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuntimeRoutine {
    pub name: &'static str,
    pub start: u32,
    pub end: u32,
}

#[derive(Clone, Debug, Default)]
pub struct SlotMap {
    pub handlers: SourceMap,
    pub runtime: Vec<RuntimeRoutine>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SlotWhere {
    Handler(Where),
    Runtime(&'static str),
    Outside,
}

impl SlotMap {
    pub fn find(&self, pc: u32) -> SlotWhere {
        let handler = self.handlers.find(pc);
        if handler != Where::Outside { return SlotWhere::Handler(handler); }
        self.runtime.iter().find(|r| (r.start..r.end).contains(&pc))
            .map_or(SlotWhere::Outside, |r| SlotWhere::Runtime(r.name))
    }
}

pub struct Slot {
    pub bytes: Vec<u8>,
    /// The slot's file bundle (`fab_elec_mcu_rt::bundle`; empty: the program has no files), which
    /// the stage maps after the slot's pages.
    pub bundle: Vec<u8>,
    pub map: SlotMap,
    pub layout: SramLayout,
    pub handlers: Vec<Handler>,
    pub symbols: runtime::Symbols,
    pub shared: u32,
    pub image_id: u32,
    /// Byte address where handler code and its literal pools begin, after the runtime.
    pub code_base: u32,
}

#[derive(Clone, Copy)]
pub(crate) struct Pin { pub number: u32, pub output: bool, pub pull: i32 }
#[derive(Clone, Copy)]
pub(crate) struct Timer { pub entry: usize, pub period: u64, pub repeat: bool }

pub(crate) fn refusal(message: impl Into<String>) -> Refusal {
    Refusal { entry: 0, code: 0, message: message.into() }
}

/// The module's images packed into the slot's file bundle, and each image's address through the
/// bundle's mapping. Unused images are kept too: their declarations remain valid. The pixels are
/// read in place from flash, so the bundle region bounds them, not the slot.
fn file_bundle(m: &Module<'_>, names: &[String], board: &Layout) -> Result<(Vec<u8>, Vec<u32>), Refusal> {
    if names.len() != m.images().len() {
        return Err(refusal(format!("{} image names for the module's {} images", names.len(), m.images().len())));
    }
    if names.is_empty() { return Ok((Vec::new(), Vec::new())); }
    let files: Vec<BundleFile> = m.images().iter().zip(names).map(|(image, name)| {
        let mut pixels = Vec::with_capacity(image.width as usize * image.height as usize * 2);
        for y in 0..image.height {
            for x in 0..image.width {
                pixels.extend_from_slice(&image.pixel(x, y).expect("verified image extent").to_le_bytes());
            }
        }
        BundleFile { name: name.clone(), kind: Kind::Image, format: IMAGE_RGB565, a: image.width, b: image.height, bytes: pixels }
    }).collect();
    let packed = bundle::pack(&files).map_err(|e| refusal(format!("file bundle: {e}")))?;
    let capacity = board.bundle_pages as usize * crate::PAGE as usize;
    if packed.len() > capacity {
        return Err(refusal(format!("the program's files take {} bytes; its bundle region holds {capacity}", packed.len())));
    }
    let read = bundle::read(&packed).expect("a packed bundle reads back");
    let base = board.bundle_data_base();
    let addresses = names.iter().map(|name| base + read.find(name).expect("every packed image is found").offset).collect();
    Ok((packed, addresses))
}

/// Module declarations determine timer number/order, exactly as rt::sched.
pub fn build_slot(m: &Module<'_>, o: &SlotOptions) -> Result<Slot, Refusal> {
    build_slot_for_layout(m, o, &crate::VOICEBOX)
}

fn build_slot_for_layout(m: &Module<'_>, o: &SlotOptions, board: &Layout) -> Result<Slot, Refusal> {
    if !m.funcs().is_empty() { return Err(refusal(FUNCTIONS_UNSUPPORTED)); }
    let page_end = if o.radio.is_some() { board.island_mmu_page().map_err(refusal)? } else { crate::MMU_ENTRIES };
    let limit = o.flash_limit_bytes.min((page_end - crate::ROM_PAGES) * crate::PAGE);
    let motion = if let Some(profile) = m.motion() {
        let sensor = o.motion.ok_or_else(|| refusal("the board has no motion input"))?;
        if !matches!(sensor.addr, 0x6a | 0x6b) { return Err(refusal("QMI8658C address must be 0x6a or 0x6b (lines 2571–2579)")); }
        if profile != crate::MOTION_PROFILE { return Err(refusal("module motion scales differ from the configured QMI8658C full scales")); }
        Some(sensor)
    } else { None };
    let touch = if m.touch() {
        let sensor = o.touch.ok_or_else(|| refusal("the board has no touch input"))?;
        if sensor.addr > 0x7f { return Err(refusal("CST820 address must be seven-bit (T01, O04)")); }
        Some(sensor)
    } else { None };
    let i2c = if uses_shared_i2c(m) {
        let bus = o.i2c.ok_or_else(|| refusal("the board has no shared I2C bus"))?;
        let pins = [bus.sda, bus.scl];
        if pins.iter().any(|pin| !(0..=21).contains(pin) && !(26..=48).contains(pin)) || bus.sda == bus.scl {
            return Err(refusal("invalid shared I2C GPIO wiring"));
        }
        bus.timing()?;
        Some(bus)
    } else { None };
    let panel = match m.display() {
        None => None,
        Some(display) => {
            let p = o.panel.ok_or_else(|| refusal("the board has no panel"))?;
            if (display.width, display.height) != (p.width, p.height) {
                return Err(refusal("the module display dimensions differ from the board panel"));
            }
            // P15/P16/P19: even windows must fit the controller's 480x480 RAM.
            if p.column_offset & 1 != 0 || p.row_offset & 1 != 0
                || u64::from(p.column_offset) + u64::from(p.width) > 480
                || u64::from(p.row_offset) + u64::from(p.height) > 480
            { return Err(refusal("the panel's even visible area must fit CO5300 memory")); }
            let bus = i2c.expect("display requires shared I2C");
            let pins = [p.cs, p.sck, p.mosi, bus.sda, bus.scl];
            if pins.iter().enumerate().any(|(i, pin)|
                (!(0..=21).contains(pin) && !(26..=48).contains(pin)) || pins[..i].contains(pin))
                || !(0x20..=0x27).contains(&p.expander) || p.reset_bit >= 8
            { return Err(refusal("invalid panel GPIO or TCA9554 wiring")); }
            Some(p)
        }
    };
    let mut pins = Vec::new();
    let mut timers = Vec::new();
    for d in m.decls() {
        match (d.form, d.args.as_slice()) {
            (ports::FORM_OUTPUT, [DeclArg::Int(pin)]) |
            (ports::FORM_INPUT, [DeclArg::Int(pin), _]) => {
                // facts-machine.md §6: holes 22..25, last physical pin 48.
                if !(0..=21).contains(pin) && !(26..=48).contains(pin) {
                    return Err(refusal(format!("declaration `{}`: GPIO{pin} does not exist on the S3", m.string(d.name))));
                }
                let pull = if let Some(DeclArg::Int(pull)) = d.args.get(1) { *pull } else { ports::PULL_NONE };
                pins.push(Pin { number: *pin as u32, output: d.form == ports::FORM_OUTPUT, pull });
            }
            (ports::FORM_EVERY | ports::FORM_AFTER, [DeclArg::Int(us), DeclArg::Entry(entry)]) => {
                timers.push(Timer { entry: *entry as usize, period: u64::from((*us).max(0) as u32) * 16,
                    repeat: d.form == ports::FORM_EVERY });
            }
            (ports::FORM_PWM, _) => return Err(refusal("no CALL0 routine supplied for `pwm_duty`: S3 PWM is not available yet")),
            (ports::FORM_SERIAL, _) => return Err(refusal("no CALL0 routine supplied for `serial_byte`: S3 serial is not available yet")),
            _ => {}
        }
    }
    for k in 0..m.entries().len() {
        for step in steps(m, k) {
            if let Step::Op(i) = step {
                if i.op == OP_PORT && i.args[0] == u32::from(ports::SHIFT_OUT) {
                    return Err(Refusal { entry: k, code: i.at, message: "`shift_out` is not available on the S3 yet".into() });
                }
            }
        }
    }
    let mut motion_odr = None;
    if let Some(d) = m.decls().iter().find(|d| d.form == ports::FORM_MOTION) {
        let [DeclArg::Int(us), DeclArg::Entry(entry)] = d.args.as_slice() else { unreachable!("verified motion") };
        motion_odr = Some(crate::motion::odr(*us as u32)?);
        timers.push(Timer { entry: *entry as usize, period: *us as u64 * 16, repeat: true });
    }
    let mut touch_scan = None;
    if let Some(d) = m.decls().iter().find(|d| d.form == ports::FORM_TOUCH) {
        let [DeclArg::Int(us), DeclArg::Entry(entry)] = d.args.as_slice() else { unreachable!("verified touch") };
        // T12: NorScanPer uses 10 ms units, 1..=30. Round up so a scan
        // lasts at least one poll period. On the non-overloaded timer grid,
        // every scan is polled, including a tap's consecutive down/up scans.
        // Overruns still coalesce; firmware cannot recover an unread scan.
        touch_scan = Some((*us as u32).div_ceil(10_000) as u8);
        timers.push(Timer { entry: *entry as usize, period: *us as u64 * 16, repeat: true });
    }
    let (mut layout, radio_kernel, usage) = SramLayout::new(m, timers.len(), o, board)?;
    if let Some(audio) = layout.speaker.as_ref().map(|s| &s.audio).or_else(|| layout.mic.as_ref().map(|m| &m.audio)) {
        audio.check_pins(&pins, i2c.expect("audio requires shared I2C"), panel, layout.mic.is_some())?;
    }
    let mut link = runtime::Link { island_flash_page: board.island / crate::PAGE, island_pages: board.island_pages,
        rwdt_quarter_per_hold: o.rwdt_quarter_per_hold, ..runtime::Link::default() };
    let first = runtime::build(m, &layout, &pins, panel.as_ref(), i2c.as_ref(), motion.zip(motion_odr),
        touch.zip(touch_scan), o.radio.as_ref(), radio_kernel.as_ref(), o.analog_check, &link);
    let mut options = Options { base: SLOT_BASE + first.bytes.len() as u32,
        ports: first.ports.clone(), sites: o.sites.clone(), marks: o.marks.clone(),
        buffers: layout.buffers.iter().map(|b| b.address).collect() };
    let stack = u64::from(layout.stack_bytes) + u64::from(emit::max_stack_bytes(m, &options)?);
    layout.stack_bytes = u32::try_from(stack).map_err(|_| usage.refusal(stack))?;
    layout.place_buffers(&usage)?;
    options.buffers = layout.buffers.iter().map(|b| b.address).collect();
    let code = build_handlers(m, &options)?;
    let handlers: Vec<_> = code.entries.iter().enumerate().map(|(id, e)|
        Handler { id: id as u32, name: e.name.clone(), address: code.base + e.offset,
            stack_bytes: e.stack_bytes, max_insns: e.max_insns }
    ).collect();
    let mut bytes = first.bytes;
    bytes.extend_from_slice(&code.bytes);
    while bytes.len() & 3 != 0 { bytes.push(0); }
    let mut words = |data: &[u32]| {
        let address = SLOT_DATA_BASE + bytes.len() as u32;
        for word in data { bytes.extend_from_slice(&word.to_le_bytes()); }
        address
    };
    link.state_init = words(m.state_init());
    link.shared = words(m.shared());
    if let Some(net) = &layout.net {
        let rows: Vec<_> = net.connections.iter().flat_map(|s| [handlers[s.entry].address, s.entry as u32,
            s.slots, s.slot_count, s.send, s.send_bytes, s.boundaries, 0]).collect();
        link.net_config = words(&rows);
    }
    if let Some(data) = &first.radio_data {
        let symbols = first.symbols.radio.as_ref().expect("radio symbols");
        link.radio_report = symbols.report;
        link.radio_crit_depth = symbols.crit_depth;
        link.radio_phy_calibrated = symbols.phy_calibrated;
        link.radio_crit_span_report_cycles = symbols.crit_span_report_cycles;
        link.radio_state_init = words(&data.state_init);
        link.radio_shared = words(&data.shared);
        link.radio_table_init = words(&data.table);
        // ESP-IDF v6.1.1 components/esp_phy/esp32s3/phy_init_data.c also
        // keeps this 128-byte table in flash rodata.
        let phy: Vec<_> = radio::phy_init_data::PHY_INIT_DATA.chunks_exact(4)
            .map(|b| u32::from_le_bytes(b.try_into().unwrap())).collect();
        link.phy_init_data = words(&phy);
        let island = &o.radio.as_ref().expect("radio profile").island;
        let header: Vec<_> = island.header.encode().chunks_exact(4)
            .map(|b| u32::from_le_bytes(b.try_into().unwrap())).collect();
        let mut segments = Vec::new();
        for s in &island.segments {
            match s.kind {
                SegmentKind::Iram => segments.extend([s.file_offset, crate::SRAM_START + (s.address - 0x4037_8000), s.bytes]),
                SegmentKind::Data => segments.extend([s.file_offset, s.address, s.bytes]),
                SegmentKind::Bss => segments.extend([s.address, s.bytes]),
                SegmentKind::Text | SegmentKind::Rodata => {}
            }
        }
        link.island_tables = runtime::IslandTables { header: words(&header), gates: words(&data.island_gates),
            segments: words(&segments) };
    }
    // Eight words per row: address, len, elem, element bytes, 2-D width,
    // three reserved zeros. The power-of-two stride keeps runtime indexing a shift.
    link.buffers = words(&vec![0; m.buffers().len() * runtime::BUFFER_ROW_WORDS]);
    let mut table = Vec::with_capacity(timers.len() * 8);
    for (index, timer) in timers.iter().enumerate() {
        // First delays are relative to the end of device init. The sensor
        // power-on minima are conservative since power precedes this point.
        let row = layout.timers + index as u32 * runtime::TIMER_BYTES;
        let due = if layout.motion.as_ref().is_some_and(|s| s.timer == row) {
            1_750_000 * 16 // QMI8658C power-on (2099–2109)
        } else if layout.touch.as_ref().is_some_and(|s| s.timer == row) {
            // T18: no CST816-family access before 100 ms power-on startup.
            100_000 * 16
        } else { timer.period };
        table.extend_from_slice(&[due as u32, (due >> 32) as u32,
            timer.period as u32, (timer.period >> 32) as u32, 0, u32::from(timer.repeat),
            handlers[timer.entry].address, handlers[timer.entry].id]);
    }
    link.timer_init = words(&table);
    drop(words);
    link.roles = SLOT_DATA_BASE + bytes.len() as u32;
    let mut roles = [0u8; 52]; // rounded to a word; only indices 0..48 may be read.
    for pin in &pins { roles[pin.number as usize] = if pin.output { 2 } else { 1 }; }
    bytes.extend_from_slice(&roles);
    link.crc8 = SLOT_DATA_BASE + bytes.len() as u32;
    bytes.extend_from_slice(&watch::CRC8_TABLE);
    while bytes.len() & 3 != 0 { bytes.push(0); }
    link.crc32 = SLOT_DATA_BASE + bytes.len() as u32;
    for word in crate::CRC32_TABLE { bytes.extend_from_slice(&word.to_le_bytes()); }
    let (bundle, image_addresses) = file_bundle(m, &o.image_names, board)?;
    for (k, (buffer, placed)) in m.buffers().iter().zip(&mut layout.buffers).enumerate() {
        let decl = &m.decls()[buffer.decl as usize];
        if placed.region.is_none() {
            let (ports::FORM_IMAGE, [DeclArg::Int(index), ..]) = (decl.form, decl.args.as_slice()) else {
                unreachable!("the loader proves every read-only buffer is an image");
            };
            placed.address = image_addresses[*index as usize];
        }
        let width = buffer_dimensions(decl).map_or(0, |(width, _)| width);
        let row = [placed.address, buffer.len, buffer.elem as u32, buffer.elem.bytes(), width, 0, 0, 0];
        let at = (link.buffers - SLOT_DATA_BASE) as usize + k * runtime::BUFFER_ROW_WORDS * 4;
        for (dst, word) in bytes[at..at + runtime::BUFFER_ROW_WORDS * 4].chunks_exact_mut(4).zip(row) {
            dst.copy_from_slice(&word.to_le_bytes());
        }
    }
    link.slot_bytes = u32::try_from(bytes.len()).map_err(|_| refusal("slot size exceeds 32 bits"))?;
    if link.slot_bytes > limit { return Err(refusal(format!("slot is {} bytes; flash limit is {limit} bytes", link.slot_bytes))); }
    options.buffers = layout.buffers.iter().map(|b| b.address).collect();
    let final_code = build_handlers(m, &options)?;
    assert_eq!(final_code.bytes.len(), code.bytes.len(), "linking flash buffers must not change handler code size");
    for (linked, provisional) in final_code.entries.iter().zip(&code.entries) {
        assert_eq!(linked.offset, provisional.offset, "linking flash buffers must not move handlers");
    }
    let code_at = (code.base - SLOT_BASE) as usize;
    bytes[code_at..code_at + final_code.bytes.len()].copy_from_slice(&final_code.bytes);
    link.setup = m.entry(SETUP).map_or(0, |k| handlers[k].address);
    link.eth = layout.eth.as_ref().map_or(0, |e| handlers[e.entry].address);
    link.speaker = layout.speaker.as_ref().map_or(0, |s| handlers[s.entry].address);
    link.mic = layout.mic.as_ref().map_or(0, |s| handlers[s.entry].address);
    let mut final_runtime = runtime::build(m, &layout, &pins, panel.as_ref(), i2c.as_ref(), motion.zip(motion_odr),
        touch.zip(touch_scan), o.radio.as_ref(), radio_kernel.as_ref(), o.analog_check, &link);
    assert_eq!(final_runtime.ports, first.ports, "linking must not move runtime routines");
    assert_eq!(final_runtime.bytes.len() as u32, code.base - SLOT_BASE, "linking must not change runtime size");
    if o.radio.is_some() {
        assert_eq!(final_runtime.symbols.radio, first.symbols.radio, "linking must not move radio entries");
        assert_eq!(final_runtime.routines, first.routines, "linking must not resize radio blobs");
        let first_data = first.radio_data.as_ref().unwrap();
        let data = final_runtime.radio_data.as_ref().unwrap();
        for (address, initial, linked) in [
            (link.radio_state_init, first_data.state_init.as_slice(), data.state_init.as_slice()),
            (link.radio_shared, first_data.shared.as_slice(), data.shared.as_slice()),
            (link.radio_table_init, first_data.table.as_slice(), data.table.as_slice()),
            (link.island_tables.gates, first_data.island_gates.as_slice(), data.island_gates.as_slice()),
        ] {
            assert_eq!(initial.len(), linked.len(), "linking must not resize radio DROM data");
            let at = (address - SLOT_DATA_BASE) as usize;
            for (dst, word) in bytes[at..at + linked.len() * 4].chunks_exact_mut(4).zip(linked) {
                dst.copy_from_slice(&word.to_le_bytes());
            }
        }
        // Both code passes have agreed. The slot owns the DROM symbols and placed these tables once.
        final_runtime.symbols.radio.as_mut().unwrap().island_tables = link.island_tables;
    }
    bytes[..final_runtime.bytes.len()].copy_from_slice(&final_runtime.bytes);
    let image_id = crate::crc32(&bytes);
    Ok(Slot { bytes, bundle, image_id, layout, handlers, shared: link.shared, symbols: final_runtime.symbols, code_base: code.base,
        map: SlotMap { handlers: final_code.map, runtime: final_runtime.routines } })
}
