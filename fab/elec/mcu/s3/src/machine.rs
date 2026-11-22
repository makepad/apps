//! Board-facing machine and ROM handoff. Time counts retired CPU cycles on
//! a 480 MHz tick grid; interrupt/exception entry itself costs no ticks.

use std::any::Any;
use std::sync::Arc;

use fab_elec_mcu_emu::board::{Audio, Board, BusEvent, NetInfo, NoBoard};
use fab_elec_mcu_emu::{ChipState, Drive, DriveEdge, Halt, HaltKind, LinkOut, Machine, Note, Ran, Status, Tier, Until, Wake};
use fab_elec_mcu_emu::bus::{Bus, Kind, Width};
use fab_elec_mcu_xtensa::insn::{Insn, Op};
use fab_elec_mcu_xtensa::sim::{cause, lane, Cpu, Decoded, IntType, Stop, TrapMode};

use crate::bus::{Registers, S3Bus, FLASH_INSN, SRAM_DATA, SRAM_LEN};
use crate::input::InputSchedule;
use crate::foreign::ForeignCode;
use crate::periph::{Periph, ResetRequest};
use crate::{config::s3_config, Junk, S3Profile};

#[cfg(feature = "jit")]
mod native;
mod spin;

pub use crate::bus::StopDetail;

// facts.md §3, TRM p.530: common integer grid for 40 MHz XTAL divisors
// and the documented 480/320 MHz PLL CPU selections.
const TICK_HZ: u64 = 480_000_000;
// facts-machine.md §6; TRM p.472 §6.1, p.478 §6.5.3: no GPIO22–25.
const PINS: [&str; 45] = [
    "GPIO0", "GPIO1", "GPIO2", "GPIO3", "GPIO4", "GPIO5", "GPIO6", "GPIO7",
    "GPIO8", "GPIO9", "GPIO10", "GPIO11", "GPIO12", "GPIO13", "GPIO14", "GPIO15",
    "GPIO16", "GPIO17", "GPIO18", "GPIO19", "GPIO20", "GPIO21",
    "GPIO26", "GPIO27", "GPIO28", "GPIO29", "GPIO30", "GPIO31", "GPIO32", "GPIO33",
    "GPIO34", "GPIO35", "GPIO36", "GPIO37", "GPIO38", "GPIO39", "GPIO40", "GPIO41",
    "GPIO42", "GPIO43", "GPIO44", "GPIO45", "GPIO46", "GPIO47", "GPIO48",
];
const AR_NAMES: [&str; 16] = [
    "a0", "a1", "a2", "a3", "a4", "a5", "a6", "a7",
    "a8", "a9", "a10", "a11", "a12", "a13", "a14", "a15",
];

#[derive(Clone)]
struct Saved {
    cpu: Cpu,
    regs: Registers,
    periph: Periph,
    tick: u64,
    board_t0: u64,
    remainder: u64,
    halt: Option<Halt>,
    resets: u64,
    last_reset: Option<ResetRequest>,
    drive_at: u64,
    drives: Vec<DriveEdge>,
    lens: (usize, usize),
    input_cursor: usize,
}

/// One CPU0, with immutable shared flash. Clone is a whole-machine snapshot;
/// frequent mark/rewind instead copies only small state and journals SRAM.
#[derive(Clone)]
pub struct S3Machine<B: Board = NoBoard> {
    cpu: Cpu,
    bus: S3Bus,
    foreign: Option<Arc<dyn ForeignCode>>,
    board: B,
    // Board time at the start of this chip reset epoch.
    board_t0: u64,
    pad_inputs: Vec<(u16, bool)>,
    // The schedule survives rewind; Saved holds only the cursor.
    inputs: InputSchedule<B::In>,
    profile: S3Profile,
    tick: u64,
    // Elapsed ticks since the last complete CPU cycle, during WAITI only.
    remainder: u64,
    halt: Option<Halt>,
    resets: u64,
    last_reset: Option<ResetRequest>,
    // Start tick of the instruction whose write last changed a pin's drive.
    drive_at: u64,
    // Untaken drive changes, and each pin's drive as last logged.
    drives: Vec<DriveEdge>,
    levels: Vec<Drive>,
    trace: Vec<(u64, u32)>,
    out: Vec<LinkOut>,
    saved: Option<Box<Saved>>,
    // How code runs, and the decoded instructions of the flash windows' code. Neither is
    // machine state: results never depend on them, and mark/rewind leave them alone.
    tier: Tier,
    code: Code,
    /// The first chip, board or input deadline, as `events` found it when nothing was due: the
    /// lane entered right after it reads it (nothing changes in between).
    horizon: u64,
    /// The board and input part of `horizon`. A register access reaches neither the board nor
    /// the inputs, so it holds until the lane returns.
    outside: u64,
    /// The spin skip's hints and counter (`machine/spin.rs`): not machine state either.
    spin: spin::Spin,
}

/// A decoded entry not decoded yet.
const UNDECODED: u8 = 0x80;
const BLANK: Decoded = Decoded {
    insn: Insn { op: Op::Nop, r: 0, s: 0, t: 0, imm: 0, bits: 0 }, len: 0, class: UNDECODED, group: 0,
};
/// Code pages of the flash instruction window, 4 KiB each.
const CODE_PAGES: usize = 0x0200_0000 >> 12;

/// Decoded instructions of the flash instruction window by start address, decoded on first
/// execution, under the machine's current MMU and foreign table. Changing either
/// discards this cache. A clone shares each decoded page until one decodes more.
#[derive(Clone)]
struct Code {
    gen: u64,
    pages: Vec<Option<Arc<[Decoded]>>>,
    /// Native units by page (`Tier::Native`), made on first use; they go with the decoded pages.
    #[cfg(feature = "jit")]
    jit: Vec<native::PageJit>,
}

impl Code {
    fn new() -> Self {
        Code { gen: 0, pages: vec![None; CODE_PAGES],
            #[cfg(feature = "jit")] jit: Vec::new() }
    }

    /// Counts a lane instruction at `pc`, run under `marks`, towards its page's native unit.
    #[cfg(feature = "jit")]
    #[inline]
    fn count<B: Board>(&mut self, pc: u32, bus: &S3Bus, marks: &fab_elec_mcu_emu::PcSet,
        foreign: &Option<Arc<dyn ForeignCode>>) {
        let p = (pc.wrapping_sub(FLASH_INSN) >> 12) as usize;
        let base = FLASH_INSN + ((p as u32) << 12);
        if self.jit.is_empty() { self.jit.resize_with(CODE_PAGES, Default::default); }
        // Count, compile and consult the shared cache only on entirely owned
        // pages. A unit from another machine could have different SLOW PCs.
        if let Some(f) = foreign {
            if !self.jit[p].owned(base, &**f) { return; }
        }
        if let Some(page) = &self.pages[p] {
            self.jit[p].count::<B>(base, (pc.wrapping_sub(FLASH_INSN) & 4095) as usize, page, bus, marks);
        }
    }

    /// The decoded instruction at `pc`, decoding it now if it is new: None when `pc` is outside
    /// the flash instruction window or a byte of it would not fetch (the reference step then
    /// fetches it and takes the fault).
    #[inline]
    fn get(&mut self, cpu: &Cpu, bus: &S3Bus, pc: u32, foreign: &Option<Arc<dyn ForeignCode>>) -> Option<Decoded> {
        let offset = pc.wrapping_sub(FLASH_INSN);
        let page = self.pages.get((offset >> 12) as usize)?;
        if let Some(page) = page {
            let d = page[(offset & 4095) as usize];
            if d.class != UNDECODED { return Some(d); }
        }
        self.decode(cpu, bus, pc, foreign.as_deref())
    }

    #[cold]
    fn decode(&mut self, cpu: &Cpu, bus: &S3Bus, pc: u32, foreign: Option<&dyn ForeignCode>) -> Option<Decoded> {
        let slow = Decoded { class: lane::SLOW, ..BLANK };
        let d = if foreign.is_some_and(|f| f.contains(pc)) {
            slow
        } else {
            // Table 198 p. 661: the first nibble gives the length, as `step` fetches.
            let first = u32::from(bus.peek_fetch(pc)?);
            let length = if first & 8 == 0 { 3 } else { 2 };
            if foreign.is_some_and(|f| (pc + 1..pc + length).any(|at| f.contains(at))) {
                slow
            } else {
                let mut bits = first;
                for byte in 1..length {
                    bits |= u32::from(bus.peek_fetch(pc + byte)?) << (byte * 8);
                }
                let decoded = cpu.decode_lane(bits).unwrap_or(slow);
                // The compiler peeks L32R literals of ordinary decoded entries.
                // Leave a load overlapping foreign bytes to the reference data bus.
                if decoded.insn.op == Op::L32r && foreign.is_some_and(|f| {
                    let at = fab_elec_mcu_xtensa::insn::target(&decoded.insn, pc).unwrap();
                    (0..4).any(|i| f.contains(at.wrapping_add(i)))
                }) { slow } else { decoded }
            }
        };
        let offset = pc.wrapping_sub(FLASH_INSN);
        let page = self.pages[(offset >> 12) as usize].get_or_insert_with(|| Arc::from(vec![BLANK; 4096]));
        Arc::make_mut(page)[(offset & 4095) as usize] = d;
        Some(d)
    }
}

// Only used for a reference PC one or two bytes before a foreign range.
struct BoundaryFetch<'a> {
    bus: &'a mut S3Bus,
    foreign: &'a dyn ForeignCode,
    crossed: bool,
}

impl Bus for BoundaryFetch<'_> {
    fn read(&mut self, addr: u32, width: Width, kind: Kind, now: u64) -> Result<u32, ()> {
        if kind == Kind::Fetch && self.foreign.contains(addr) {
            self.crossed = true;
            return Err(());
        }
        self.bus.read(addr, width, kind, now)
    }

    fn write(&mut self, addr: u32, width: Width, value: u32, now: u64) -> Result<(), ()> {
        self.bus.write(addr, width, value, now)
    }
}

/// The bus a lane's instruction runs on: the machine's own, which notes any access that is not
/// plain memory (a register, which may change a pin, an event, an interrupt line or the clock),
/// made at the instruction's own tick as the reference makes it.
struct LaneBus<'a, N: spin::Notes> {
    bus: &'a mut S3Bus,
    tick: u64,
    /// The register access: 0 for none, `WROTE` for a write (or a read of address 0 or 1), else
    /// the address read, which the spin skip's trigger looks at.
    slow: u32,
    /// What a probe of the spin skip needs to know of the instruction's accesses.
    notes: N,
}

/// `LaneBus::slow` after a register write.
const WROTE: u32 = 1;

impl<N: spin::Notes> Bus for LaneBus<'_, N> {
    #[inline]
    fn read(&mut self, addr: u32, width: Width, kind: Kind, now: u64) -> Result<u32, ()> {
        if kind == Kind::Load {
            if let Some(value) = self.bus.sram_load(addr, width) { return Ok(value); }
        }
        if !S3Bus::plain(addr) {
            self.slow = addr.max(WROTE);
            self.bus.tick = self.tick;
            self.notes.read(addr);
        }
        self.bus.read(addr, width, kind, now)
    }
    #[inline]
    fn write(&mut self, addr: u32, width: Width, value: u32, now: u64) -> Result<(), ()> {
        if let Some(result) = self.bus.sram_store(addr, width, value) {
            self.notes.store();
            return result;
        }
        if !S3Bus::plain(addr) {
            self.slow = WROTE;
            self.bus.tick = self.tick;
            self.notes.write(addr, value);
        }
        self.bus.write(addr, width, value, now)
    }
}

/// What the lane carries from one instruction to the next.
#[derive(Clone, Copy)]
struct LaneRun {
    /// Instructions the lane may still start before the horizon or the first comparator match.
    budget: u64,
    /// The retired count when the lane last counted its cycles in.
    start: u64,
    /// Ticks per CPU cycle.
    period: u64,
}

/// How the lane goes on after `lane_step`.
#[derive(Clone, Copy, PartialEq, Eq)]
enum LaneStep {
    /// The next instruction.
    Next,
    /// A stop at this PC: the lane ends here, before the instruction.
    Stop,
    /// The instruction got the reference loop's handling, which makes the next decisions.
    Return,
    /// The next instruction, and a probe of the spin skip has started: the lane runs it in
    /// `lane_probe`.
    Probe,
}

/// What the reference loop reads before an instruction, for its handling after it.
#[derive(Clone, Copy)]
struct Before {
    pc: u32,
    period: u64,
    retired: u64,
    writes: u64,
    drive: u64,
}

/// The CPU at the ROM's jump: entry PC, junk in every register the ROM
/// leaves undocumented, interrupts off.
fn handoff_cpu(profile: &S3Profile, junk: &mut Junk) -> Cpu {
    let mut cpu = Cpu::new(s3_config(), profile.entry);
    cpu.traps = TrapMode::Take;
    for a in &mut cpu.ar { *a = junk.u32(); }
    for f in &mut cpu.fr { *f = junk.u32(); }
    // Legal masks follow xtensa/src/sim.rs write_sr, PS_MASK and the
    // FCR/FSR user-register writes (ISA pp.281–284,295,312,317).
    cpu.sar = junk.u32() & 63;
    cpu.ps = junk.u32() & 0x0007_0fff;
    cpu.window_base = junk.u32();
    cpu.window_start = junk.u32();
    cpu.epc1 = junk.u32();
    cpu.excsave1 = junk.u32();
    cpu.exccause = junk.u32() & 63;
    cpu.excvaddr = junk.u32();
    cpu.vecbase = junk.u32() & !1;
    cpu.ccount = junk.u32();
    for compare in &mut cpu.ccompare { *compare = junk.u32(); }
    cpu.cpenable = junk.u32() & 255;
    cpu.br = junk.u32() as u16;
    cpu.fcr = junk.u32() & 0x7f;
    cpu.fsr = junk.u32() & 0xf80;
    // M: an interrupt before the image's first instruction would be ROM
    // behavior, which is never simulated. The chip probe must confirm.
    cpu.intenable = 0;
    cpu.interrupt = 0;
    cpu
}

/// What the ROM does with the image at a handoff: jump to it, or a path the
/// model never simulates.
fn boot_halt(flash: &[u8], profile: &S3Profile, strap_flash: bool) -> Option<Halt> {
    let unmodeled = |what| Some(Halt { tick: 0, pc: profile.entry, kind: HaltKind::Unmodeled, what });
    // facts.md §1: GPIO0 low at reset selects the ROM's download boot.
    if !strap_flash {
        return unmodeled("GPIO0 low at reset selects download boot, which is not modelled");
    }
    // emcu-s3.md:27–30 "Boot facts" supersedes Q6 §1.2's old offset 0:
    // two little-endian 0xaedb041d words at flash 0x400 and 0x404.
    let magic = 0xaedb_041du32.to_le_bytes();
    let direct = flash.get(0x400..0x404) == Some(magic.as_slice())
        && flash.get(0x404..0x408) == Some(magic.as_slice());
    if !direct {
        return unmodeled("no Direct Boot magic at flash 0x400: the ROM would run the second-stage loader, which is never simulated");
    }
    None
}

impl S3Machine<NoBoard> {
    /// Models the ROM's jump, never the ROM's execution: the ROM functions the model stands in for
    /// (`rom::table`) are installed. Invalid profile bounds panic; unsupported image boot paths
    /// produce a latched halt.
    pub fn new(flash: Arc<Vec<u8>>, profile: &S3Profile) -> Self {
        Self::with_board(flash, profile, NoBoard).expect("NoBoard construction cannot fail")
    }
}

impl<B: Board> S3Machine<B> {
    /// Models the ROM's jump, installs the ROM's stand-ins (every image's init calls the ROM), then
    /// binds the board and resolves the initial pad levels.
    pub fn with_board(flash: Arc<Vec<u8>>, profile: &S3Profile, board: B) -> Result<Self, String> {
        let mut junk = Junk(profile.seed);
        let mut cpu = handoff_cpu(profile, &mut junk);
        let bus = S3Bus::new(flash, profile, &mut junk);
        cpu.set_lines(bus.periph.interrupt_lines(&cpu.config));
        let halt = boot_halt(bus.flash(), profile, bus.periph.pins.boot_strap_flash());
        let levels = (0..PINS.len()).map(|pin| bus.periph.drive(pin)).collect();
        let mut machine = Self {
            cpu, bus, foreign: Some(crate::rom::table()), profile: profile.clone(), tick: 0, remainder: 0, halt, resets: 0,
            board, board_t0: 0, pad_inputs: Vec::new(),
            inputs: InputSchedule { entries: Vec::new(), cursor: 0 },
            last_reset: None, drive_at: 0, drives: Vec::new(), levels, trace: Vec::new(), out: Vec::new(),
            saved: None, tier: Tier::default(), code: Code::new(), horizon: 0, outside: 0,
            spin: spin::Spin::default(),
        };
        machine.board.bind(&PINS, TICK_HZ)?;
        for (pin, &drive) in machine.levels.iter().enumerate() {
            machine.board.pad_drive(0, pin as u16, drive)
                .map_err(|what| format!("initial drive of {}: {what}", PINS[pin]))?;
        }
        machine.feed_pad_inputs(0);
        Ok(machine)
    }

    /// The parts and nets around the chip, as of the machine's tick.
    pub fn board(&self) -> &B { &self.board }

    /// Install a part's table of immutable code beside the ROM's stand-ins (`rom::Beside`): it
    /// replaces any earlier part's table, never the ROM's, which every image's init calls. Discard
    /// all decode and native decisions made with an earlier table, including shared units already
    /// collected.
    pub fn with_foreign(mut self, foreign: Arc<dyn ForeignCode>) -> Self {
        self.foreign = Some(Arc::new(crate::rom::Beside { part: foreign, rom: crate::rom::table() }));
        self.code = Code { gen: self.bus.code_gen, ..Code::new() };
        self.spin.abandon();
        self
    }

    pub fn explain_foreign(&self) -> Option<String> {
        let halt = self.halt?;
        if halt.kind != HaltKind::Unmodeled || self.cpu.pc != halt.pc { return None; }
        self.foreign.as_ref()?.explain(halt.pc, &self.cpu, &self.bus)
    }

    /// True when the page has native accounting and can hold a compiled unit.
    pub fn native_page_tracked(&self, pc: u32) -> bool {
        #[cfg(feature = "jit")]
        {
            let p = (pc.wrapping_sub(FLASH_INSN) >> 12) as usize;
            self.code.jit.get(p).is_some_and(|jit| self.foreign.is_none() || jit.owned == Some(true))
        }
        #[cfg(not(feature = "jit"))]
        { let _ = pc; false }
    }

    /// Whether a completed unit accepts this entry under these marks. Probe a copy
    /// so inspecting another set of breakpoints cannot evict the running unit.
    pub fn has_native_entry(&self, pc: u32, marks: &fab_elec_mcu_emu::PcSet) -> bool {
        #[cfg(feature = "jit")]
        {
            let page = (pc.wrapping_sub(FLASH_INSN) >> 12) as usize;
            self.code.jit.get(page).is_some_and(|jit| jit.clone().unit(pc, marks, pc & !4095).is_some())
        }
        #[cfg(not(feature = "jit"))]
        { let _ = (pc, marks); false }
    }

    #[inline]
    fn reference_step(&mut self, pc: u32) -> Result<(), Stop> {
        // Keep the table's paths (step_supplied, the boundary guard) out of run's loop.
        if self.foreign.is_some() { return self.foreign_step(pc); }
        self.cpu.step(&mut self.bus)
    }

    #[inline(never)]
    fn foreign_step(&mut self, pc: u32) -> Result<(), Stop> {
        if let Some(foreign) = self.foreign.as_deref() {
            if foreign.contains(pc) {
                return self.cpu.step_supplied(&mut self.bus, |cpu, bus| foreign.at(pc, cpu, bus));
            }
            if foreign.contains(pc.wrapping_add(1)) || foreign.contains(pc.wrapping_add(2)) {
                let before = self.cpu.clone();
                let mut guard = BoundaryFetch { bus: &mut self.bus, foreign, crossed: false };
                let result = self.cpu.step(&mut guard);
                if guard.crossed {
                    // Replace the internal fetch error/vector entry with a latched
                    // part-boundary stop at the unchanged offending instruction.
                    self.cpu = before;
                    return Err(Stop::Unmodeled { pc, what: "instruction crosses foreign code boundary" });
                }
                return result;
            }
        }
        self.cpu.step(&mut self.bus)
    }

    /// Monotonic board time, including elapsed chip reset epochs.
    pub fn board_time(&self) -> u64 { self.board_t0 + self.tick }

    /// Install the complete replay before execution or the first mark. Equal ticks retain
    /// their supplied order. An existing nonempty schedule cannot be replaced.
    pub fn install_input_schedule(&mut self, mut entries: Vec<(u64, B::In)>) -> Result<(), Vec<(u64, B::In)>> {
        if !self.inputs.entries.is_empty() || self.tick != 0 || self.resets != 0 || self.saved.is_some() {
            return Err(entries);
        }
        entries.sort_by_key(|entry| entry.0);
        self.inputs = InputSchedule { entries, cursor: 0 };
        Ok(())
    }

    /// Every scheduled input in board ticks, including entries not yet applied. Rewind
    /// retains these entries; a replay can install the complete schedule before execution.
    pub fn inputs(&self) -> &[(u64, B::In)] {
        &self.inputs.entries
    }

    /// Seed internal SRAM before the first instruction or mark. This is
    /// handoff setup, not a CPU or DMA access; subsequent writes use journals.
    pub fn seed_data(&mut self, addr: u32, bytes: &[u8]) -> bool {
        if self.tick != 0 || self.resets != 0 || self.saved.is_some() { return false; }
        let Some(offset) = addr.checked_sub(SRAM_DATA).map(|n| n as usize) else { return false; };
        if offset >= SRAM_LEN || bytes.len() > SRAM_LEN - offset { return false; }
        self.bus.sram[offset..offset + bytes.len()].copy_from_slice(bytes);
        true
    }

    /// Outside stimulus (a key, a finger) reaching the board at the machine's tick; a board
    /// that cannot model it latches an Unmodeled halt.
    pub fn board_in(&mut self, ev: B::In) {
        if let Err(what) = self.board.input(self.board_t0 + self.tick, ev) {
            self.latch(self.cpu.pc, HaltKind::Unmodeled, what);
        }
        self.feed_pad_inputs(self.tick);
    }

    /// Open or close the host's USB serial port at the current tick. The host
    /// listens by default; its listening state and queued bytes survive every
    /// chip reset scope. Reopening resumes service on the next host frame.
    pub fn set_usb_host_listening(&mut self, listening: bool) {
        self.bus.periph.pins.set_usb_host_listening(listening, self.tick);
    }

    fn feed_pad_inputs(&mut self, tick: u64) {
        self.board.pad_inputs(&mut self.pad_inputs);
        if self.pad_inputs.is_empty() { return; }
        // M: board nets reach the chip input at zero delay; the pins block
        // documents its own input-sync choice.
        for (pad, high) in self.pad_inputs.drain(..) {
            self.bus.periph.set_input(pad as usize, high, tick);
        }
        self.update_lines();
    }

    /// Logs each pin whose drive changed since the last log, at `tick`.
    fn log_drives(&mut self, tick: u64) -> Result<(), &'static str> {
        let mut result = Ok(());
        for pin in 0..PINS.len() {
            let drive = self.bus.periph.drive(pin);
            if drive != self.levels[pin] {
                self.levels[pin] = drive;
                self.drives.push(DriveEdge { tick, pin: pin as u16, drive });
                let changed = self.board.pad_drive(self.board_t0 + tick, pin as u16, drive);
                result = result.and(changed);
            }
        }
        self.feed_pad_inputs(tick);
        result
    }

    fn relevel(&mut self) {
        for pin in 0..PINS.len() {
            self.levels[pin] = self.bus.periph.drive(pin);
        }
    }

    pub fn cpu(&self) -> &Cpu { &self.cpu }

    /// Polling cycles the spin skip counted in without running them (`machine/spin.rs`).
    pub fn spin_skipped(&self) -> u64 { self.spin.skipped }

    fn update_lines(&mut self) {
        // M: facts-machine.md:227 leaves propagation latency unspecified.
        // Observe writes/events/host input at their machine boundary; no
        // invented matrix pipeline and no routing work per idle instruction.
        self.cpu.set_lines(self.bus.periph.interrupt_lines(&self.cpu.config));
    }

    /// The last reset a block performed since power-on, in its own epoch.
    pub fn last_reset(&self) -> Option<ResetRequest> { self.last_reset }

    /// Performs a block's reset: the ROM's next handoff, with SRAM kept and
    /// fresh junk for everything the ROM leaves undocumented.
    fn reset(&mut self, reset: ResetRequest) -> Ran {
        self.board_t0 += reset.tick;
        self.resets += 1;
        let mut junk = Junk(self.profile.seed.rotate_left(17) ^ self.resets.wrapping_mul(0xd6e8_feb8_6659_fd93));
        self.cpu = handoff_cpu(&self.profile, &mut junk);
        self.bus.rehandoff(&self.profile, reset.scope, &mut junk);
        let apb = self.bus.regs.apb_ticks();
        self.bus.periph.reset(reset, &self.profile.time, &self.profile.pins, apb, self.bus.regs.gates(), &mut junk);
        self.update_lines();
        self.tick = 0;
        self.remainder = 0;
        self.last_reset = Some(reset);
        self.halt = boot_halt(self.bus.flash(), &self.profile, self.bus.periph.pins.boot_strap_flash());
        if let Err(what) = self.board.chip_reset(self.board_t0) {
            self.latch(self.cpu.pc, HaltKind::Unmodeled, what);
        }
        self.feed_pad_inputs(0);
        if let Err(what) = self.log_drives(0) {
            self.latch(self.cpu.pc, HaltKind::Unmodeled, what);
        }
        // A reset discards untaken changes; `drive` gives the reset state.
        self.drives.clear();
        Ran::Reset { at: reset.tick }
    }

    fn board_next(&self) -> Option<u64> {
        self.board.next_event().map(|due| {
            due.checked_sub(self.board_t0).expect("board event cannot precede the current chip epoch")
        })
    }

    fn input_next(&self) -> Option<u64> {
        self.inputs.next().map(|due| {
            due.checked_sub(self.board_t0).expect("input cannot precede the current chip epoch")
        })
    }

    /// At equal ticks: chip blocks, then board events, then stimuli in insertion order.
    /// Recheck after each event, so zero-delay events it creates precede the next stimulus.
    /// A reset returns its `Ran` and ends this epoch's events.
    fn events(&mut self) -> Option<Ran> {
        loop {
            let due = self.board_next();
            let input = self.input_next();
            let limit = due.into_iter().chain(input).fold(self.tick, u64::min);
            let next = self.bus.periph.next_event();
            if let Some(at) = next.filter(|&due| due <= limit) {
                let generation = self.bus.periph.drive_gen();
                let result = self.bus.periph.events(at, &mut self.out, &mut self.board, self.board_t0,
                    &mut self.bus.sram, &mut self.bus.journal);
                self.feed_pad_inputs(at);
                match result {
                    Ok(Some(reset)) => return Some(self.reset(reset)),
                    Ok(None) => {
                        self.update_lines();
                        if self.bus.periph.drive_gen() != generation {
                            // `Ran::Drive` reports the tick of the change, here the event's.
                            self.drive_at = at;
                            if let Err(what) = self.log_drives(at) {
                                self.latch(self.cpu.pc, HaltKind::Unmodeled, what);
                                return Some(Ran::Halted);
                            }
                        }
                    }
                    Err(what) => {
                        self.latch(self.cpu.pc, HaltKind::Unmodeled, what);
                        return Some(Ran::Halted);
                    }
                }
                // Pad drives can change the board's next deadline.
                continue;
            }
            let (at, result) = if let Some(due) = due.filter(|&due| {
                due <= self.tick && input.is_none_or(|input| due <= input)
            }) {
                (due, self.board.event(self.board_t0 + due))
            } else if let Some(due) = input.filter(|&due| due <= self.tick) {
                let (t, ev) = self.inputs.pop();
                (due, self.board.input(t, ev))
            } else {
                // Nothing is due: the first deadline ahead, for the lane entered next.
                self.outside = due.into_iter().chain(input).fold(u64::MAX, u64::min);
                self.horizon = next.map_or(self.outside, |next| next.min(self.outside));
                return None;
            };
            self.feed_pad_inputs(at);
            if let Err(what) = result {
                self.latch(self.cpu.pc, HaltKind::Unmodeled, what);
                return Some(Ran::Halted);
            }
        }
    }

    /// None for non-bus stops, including a rejected ROM handoff or PRID read.
    pub fn stop_detail(&self) -> Option<StopDetail> { self.bus.regs.stop.map(|s| s.detail) }

    fn latch(&mut self, pc: u32, kind: HaltKind, what: &'static str) {
        self.halt = Some(Halt { tick: self.tick, pc, kind, what });
    }

    fn wake(&self) -> Option<Wake> {
        if !self.cpu.sleeping() { return None; }
        let period = self.bus.regs.ticks_per_cycle();
        let level = self.cpu.interrupt_level();
        let sources = self.bus.periph.interrupt_sources();
        let cpu_wake = self.cpu.config.int_type.iter().enumerate().filter_map(|(line, ty)| {
            // A masked match latches but does not end the WAITI.
            if self.cpu.intenable & (1 << line) == 0 || self.cpu.config.int_level[line] <= level { return None; }
            let pending = self.cpu.interrupt & (1 << line) != 0;
            let (tick, source) = match *ty {
                IntType::Timer(timer) => {
                    let tick = if pending { self.tick } else {
                        let d = self.cpu.ccompare[timer as usize].wrapping_sub(self.cpu.ccount);
                        let cycles = if d == 0 { 1u64 << 32 } else { u64::from(d) };
                        self.tick.checked_add(cycles * period - self.remainder)?
                    };
                    (tick, ["CCOMPARE0", "CCOMPARE1", "CCOMPARE2"][timer as usize])
                }
                IntType::Level | IntType::Edge if pending =>
                    (self.tick, self.bus.periph.intr.source_name(line as u32, sources)),
                IntType::Software if pending => (self.tick, "software interrupt"),
                _ => return None,
            };
            Some(Wake { tick, source, vector: line as u32 })
        });
        // A conservative bound, including events that may only reset the
        // machine or raise a masked source. Re-evaluate after that event;
        // never advertise a later wake than the real peripheral or board request.
        // u32::MAX means this bound has no routed CPU interrupt number.
        let event_wake = self.bus.periph.next_event().map(|tick| {
            let source = self.bus.periph.event_source(tick);
            Wake {
                tick: tick.max(self.tick),
                source: source.map_or("peripheral event", crate::periph::intr::Intr::name),
                vector: source.and_then(|s| self.bus.periph.intr.destination(s, &self.cpu.config)).unwrap_or(u32::MAX),
            }
        });
        let board_wake = self.board_next().map(|due| Wake {
            tick: due.max(self.tick), source: "board event", vector: u32::MAX,
        });
        let input_wake = self.input_next().map(|due| Wake {
            tick: due.max(self.tick), source: "board input", vector: u32::MAX,
        });
        cpu_wake.chain(event_wake).chain(board_wake).chain(input_wake)
            .min_by_key(|wake| (wake.tick, wake.vector))
    }

    /// The reference loop's handling after an instruction (or an interrupt entry, or a sleep):
    /// interrupt lines and pin drives a register write changed, the tick, latched stops, and a
    /// WAITI's sleep up to the next event or `deadline`.
    #[inline]
    fn after_step(&mut self, b: Before, result: Result<(), Stop>, deadline: u64) {
        if self.bus.periph.write_gen() != b.writes { self.update_lines(); }
        let drives = if self.bus.periph.drive_gen() != b.drive {
            self.drive_at = self.tick;
            self.log_drives(self.tick)
        } else { Ok(()) };
        if self.cpu.instret != b.retired {
            self.tick = self.tick.checked_add(b.period).expect("S3 tick overflow");
        }
        if let Some(stop) = self.bus.regs.stop {
            // CPU exception entry after Err(()) is not meaningful here.
            // Preserve the PC from before the faulting instruction.
            self.latch(b.pc, HaltKind::Unmodeled, stop.what);
            return;
        }
        if let Err(what) = drives {
            self.latch(b.pc, HaltKind::Unmodeled, what);
            return;
        }
        match result {
            Ok(()) => {}
            Err(Stop::Waiti { .. }) => {
                // A newly retired WAITI may itself reach CCOMPARE. Give
                // the CPU an entry step before considering any time jump.
                // Sleep is cut at the next chip or board event, handled above.
                let deadline = self.bus.periph.next_event().into_iter().chain(self.board_next()).chain(self.input_next())
                    .fold(deadline, u64::min);
                if self.cpu.instret == b.retired && self.tick < deadline {
                    self.sleep_to(deadline);
                }
            }
            Err(Stop::Break { pc }) => self.latch(pc, HaltKind::Break, "BREAK"),
            Err(Stop::Unmodeled { pc, what }) => self.latch(pc, HaltKind::Unmodeled, what),
            Err(Stop::Exception { pc, cause, vaddr }) => {
                let (kind, what) = match cause {
                    cause::ILLEGAL => (HaltKind::BadInsn, "instruction encoding"),
                    cause::INSTR_FETCH_ERROR | cause::LOAD_STORE_ERROR | cause::LOAD_STORE_ALIGNMENT =>
                        (HaltKind::BadAddress { addr: vaddr }, "address"),
                    _ => (HaltKind::Unmodeled, "CPU exception stopped"),
                };
                self.latch(pc, kind, what);
            }
            Err(Stop::Limit) => unreachable!("single steps have no retirement limit"),
        }
    }

    /// The decoded lane (`Tier::Decoded`), entered where the reference loop has found no halt, no
    /// due event, no drive change to return and no barrier. It runs instructions while every
    /// check the reference loop makes before an instruction is known to come out clear: before
    /// the horizon (the first event, input or board deadline, the run's deadline, the first
    /// comparator match), with no interrupt pending or sleep, until a stop, a barrier, an
    /// instruction the reference must run (`lane::SLOW`, code outside the flash window), or an
    /// instruction after which a check may change: a register access, a PS write, a vector
    /// entry or a stop. That last instruction gets the reference loop's own handling. After a
    /// register access that leaves every check clear, which the lane tests as the reference
    /// loop would, it goes on. It never returns a `Ran`: the reference loop makes every decision
    /// at the next boundary. Returns whether it ran anything.
    fn lane(&mut self, until: &Until) -> bool {
        // A probe the last call left unfinished failed: its cycle left the lane.
        self.spin.abandon();
        if !self.cpu.lane_ready() || self.bus.regs.stop.is_some() { return false; }
        if self.code.gen != self.bus.code_gen {
            self.code = Code { gen: self.bus.code_gen, ..Code::new() };
        }
        // Code the lane cannot run costs the reference no more than this lookup.
        match self.code.get(&self.cpu, &self.bus, self.cpu.pc, &self.foreign) {
            Some(d) if d.class & lane::SLOW == 0 => {}
            _ => return false,
        }
        let horizon = self.horizon.min(until.tick);
        if self.tick >= horizon { return false; }
        let period = self.bus.regs.ticks_per_cycle();
        // Instructions start at tick, tick + period, ...: each before the horizon runs here.
        // At most 2^32 - 1, so the cycle count always fits `Cpu::advance`.
        let budget = (horizon - self.tick).div_ceil(period).min(self.cpu.timer_budget()).min(u64::from(u32::MAX));
        let mut run = LaneRun { budget, start: self.cpu.instret, period };
        let mut ran = false;
        while run.budget > 0 {
            let pc = self.cpu.pc;
            #[cfg(feature = "jit")]
            if self.tier == Tier::Native {
                match self.native(&mut run, until, pc, &mut ran) {
                    // No unit here, or one left before an instruction it does not run, or with
                    // less budget than its next run needs, or a spin probe started in its helper:
                    // the lane runs on (and ends when the budget is spent).
                    None | Some(native::EXIT_MISS | native::EXIT_BUDGET) => {}
                    Some(native::EXIT_RETURN) => return true,
                    // A stop at a PC.
                    Some(_) => break,
                }
                if self.spin.probing() {
                    match self.lane_probe(&mut run, until) {
                        LaneStep::Next | LaneStep::Probe => continue,
                        LaneStep::Stop => break,
                        LaneStep::Return => return true,
                    }
                }
                if run.budget == 0 || self.cpu.pc != pc { continue; }
            }
            let Some(d) = self.code.get(&self.cpu, &self.bus, pc, &self.foreign) else { break };
            if d.class & lane::SLOW != 0 { break; }
            #[cfg(feature = "jit")]
            if self.tier == Tier::Native { self.code.count::<B>(pc, &self.bus, until.marks, &self.foreign); }
            match self.lane_step::<spin::Plain>(&mut run, until, pc, &d) {
                LaneStep::Next => ran = true,
                LaneStep::Probe => {
                    ran = true;
                    match self.lane_probe(&mut run, until) {
                        LaneStep::Next | LaneStep::Probe => {}
                        LaneStep::Stop => break,
                        LaneStep::Return => return true,
                    }
                }
                LaneStep::Stop => break,
                LaneStep::Return => return true,
            }
        }
        // The budget holds the run below the first comparator match.
        self.cpu.advance((self.cpu.instret - run.start) as u32);
        ran
    }

    /// Runs the native unit of `pc`'s page from `pc`, when there is one that holds it: its exit
    /// code, with the lane's run state and `ran` as it left them.
    #[cfg(feature = "jit")]
    fn native(&mut self, run: &mut LaneRun, until: &Until, pc: u32, ran: &mut bool) -> Option<u32> {
        // Units run with the window gate off; see `native`'s module doc.
        if self.cpu.windows_checked() { return None; }
        let page = (pc.wrapping_sub(FLASH_INSN) >> 12) as usize;
        let jit = self.code.jit.get_mut(page)?;
        if self.foreign.is_some() && jit.owned != Some(true) { return None; }
        // Held for the call: the code is never dropped while it runs.
        let base = FLASH_INSN + ((page as u32) << 12);
        let native = jit.unit(pc, until.marks, base)?;
        let memory = self.memory();
        let mut ctx = native::LaneCtx::<B> {
            next: 0, ran: 0, sram: memory.sram, flags: memory.flags, dma: memory.dma,
            machine: self, until: until as *const Until as *const Until<'static>, run: *run, panic: None,
        };
        // SAFETY: the unit was built for `LaneCtx<B>` and holds `pc`; the context outlives the call.
        let exit = unsafe { native.unit.run(&mut ctx as *mut native::LaneCtx<B> as *mut u8, pc) };
        *run = ctx.run;
        if ctx.ran != 0 { *ran = true; }
        if let Some(payload) = ctx.panic.take() { std::panic::resume_unwind(payload); }
        Some(exit)
    }

    /// One lane instruction at `pc`, decoded as `d` (not `lane::SLOW`), with budget left.
    /// `N` is what its bus notes for the spin skip: nothing in the ordinary lane (`spin::Plain`;
    /// the trigger reads the address in `LaneBus::slow`), every access while a probe runs
    /// (`spin::Probing`).
    #[inline(always)]
    fn lane_step<N: spin::Notes>(&mut self, run: &mut LaneRun, until: &Until, pc: u32, d: &Decoded) -> LaneStep {
        if until.marks.contains(pc) {
            // A marked PC traces each run: a cycle through it is never skipped.
            if N::PROBE { self.spin.abandon(); }
            if self.tick > until.stops_after && until.stops.binary_search(&pc).is_ok() { return LaneStep::Stop; }
            self.trace.push((self.tick, pc));
        }
        let before = Before {
            pc, period: run.period, retired: self.cpu.instret,
            writes: self.bus.periph.write_gen(), drive: self.bus.periph.drive_gen(),
        };
        let mut bus = LaneBus { bus: &mut self.bus, tick: self.tick, slow: 0, notes: N::default() };
        let result = self.cpu.execute_decoded(d, &mut bus);
        let (slow, notes) = (bus.slow, bus.notes);
        if matches!(result, Ok(true)) && slow == 0 && d.class & lane::ENDS == 0 {
            self.tick += run.period;
            run.budget -= 1;
            if N::PROBE { self.spin_plain(notes.stored()); }
            return LaneStep::Next;
        }
        // A retired register access with no PS change may let the lane go on (below).
        let access = matches!(result, Ok(true)) && d.class & lane::ENDS == 0;
        // Count every retired cycle in before the reference's own handling, as `step` has
        // advanced CCOUNT before its machine sees the instruction.
        self.cpu.advance((self.cpu.instret - run.start) as u32);
        let result = match result {
            Ok(_) if self.cpu.sleeping() => Err(Stop::Waiti { pc: self.cpu.pc }),
            Ok(_) => Ok(()),
            Err(stop) => Err(stop),
        };
        self.after_step(before, result, until.tick);
        // The reference loop's checks at the next boundary, in its order: no halt; `events`
        // finds nothing due (the board and inputs are as it left them, so `outside` holds; a
        // drive change, which may move the board's deadline, returns); no drive to return;
        // no barrier (the board is untouched); then the lane's own entry conditions, and the
        // decoded code still current (an MMU write changes it).
        if !access || self.halt.is_some() || self.bus.periph.drive_gen() != before.drive
            || self.bus.regs.stop.is_some() || !self.cpu.lane_ready()
            || self.code.gen != self.bus.code_gen { return LaneStep::Return; }
        let horizon = self.bus.periph.next_event().map_or(self.outside, |next| next.min(self.outside)).min(until.tick);
        if self.tick >= horizon { return LaneStep::Return; }
        run.period = self.bus.regs.ticks_per_cycle();
        run.budget = (horizon - self.tick).div_ceil(run.period).min(self.cpu.timer_budget()).min(u64::from(u32::MAX));
        run.start = self.cpu.instret;
        if N::PROBE {
            if notes.stored() { self.spin.abandon(); } else { self.spin_probe(run, pc, notes.access()); }
        } else if slow != WROTE && self.spin_trigger(run, pc, slow) {
            return LaneStep::Probe;
        }
        LaneStep::Next
    }

    /// The decoded lane while a probe of the spin skip runs (`machine/spin.rs`), each
    /// instruction noted, until the probe ends: `Next` when the lane goes on, else as
    /// `lane_step`. A probe the lane cannot finish here failed.
    #[inline(never)]
    fn lane_probe(&mut self, run: &mut LaneRun, until: &Until) -> LaneStep {
        let mut step = LaneStep::Next;
        while self.spin.probing() && run.budget > 0 {
            let pc = self.cpu.pc;
            let d = match self.code.get(&self.cpu, &self.bus, pc, &self.foreign) {
                Some(d) if d.class & lane::SLOW == 0 => d,
                _ => break,
            };
            #[cfg(feature = "jit")]
            if self.tier == Tier::Native { self.code.count::<B>(pc, &self.bus, until.marks, &self.foreign); }
            step = self.lane_step::<spin::Probing>(run, until, pc, &d);
            if matches!(step, LaneStep::Stop | LaneStep::Return) { break; }
        }
        self.spin.abandon();
        step
    }

    fn sleep_to(&mut self, deadline: u64) {
        let period = self.bus.regs.ticks_per_cycle();
        // next_timer ignores PS masking, as required by its contract. Even
        // a masked match latches the request; the next step decides entry.
        let to_match = self.cpu.next_timer().map(|distance| {
            let cycles = if distance == 0 { 1u64 << 32 } else { u64::from(distance) };
            cycles * period - self.remainder
        });
        let ticks = to_match.map_or(deadline - self.tick, |dt| dt.min(deadline - self.tick));
        // Quotient/remainder form avoids overflow at a near-u64::MAX deadline.
        let partial = self.remainder + ticks % period;
        let cycles = ticks / period + partial / period;
        self.remainder = partial % period;
        // An arbitrarily long sleep without enabled timers still updates all
        // comparator latches. One full wrap reaches every comparator; extra
        // full wraps change nothing. Split per Cpu::next_timer's contract.
        if cycles >= 1u64 << 32 {
            self.cpu.advance(u32::MAX);
            self.cpu.advance(1);
        }
        self.cpu.advance(cycles as u32);
        self.tick += ticks;
    }
}

impl<B: Board> Machine for S3Machine<B> {
    fn tick_hz(&self) -> u64 { TICK_HZ }
    fn now(&self) -> u64 { self.tick }
    fn pc(&self) -> u32 { self.cpu.pc }

    fn run(&mut self, until: &Until) -> Ran {
        let generation = self.bus.periph.drive_gen();
        loop {
            if self.halt.is_some() { return Ran::Halted; }
            // Block events at their own ticks, processed at the first
            // instruction boundary at or after them; a reset ends the call.
            if let Some(ran) = self.events() { return ran; }
            if until.on_drive && self.bus.periph.drive_gen() != generation {
                return Ran::Drive { at: self.drive_at };
            }
            // A part's journal asking for a mark precedes Pc and Tick even when the
            // write that filled it ended exactly at the deadline. A mark, not a log,
            // resumes the run.
            if self.board.barrier() { return Ran::Barrier; }
            // One lane, inlined here with one call site for each lane step. Two copies
            // (one per table state) left Cpu::execute_decoded out of line, a call per
            // decoded instruction (5.3% on plain machines). The table checks are
            // per-page branches in count and native, Native tier only.
            if self.tier != Tier::Ref && self.lane(until) { continue; }
            let pc = self.cpu.pc;
            let marked = until.marks.contains(pc) && self.cpu.will_execute();
            if marked && self.tick > until.stops_after && until.stops.binary_search(&pc).is_ok() {
                return Ran::Pc;
            }
            if self.tick >= until.tick { return Ran::Tick; }
            if marked { self.trace.push((self.tick, pc)); }
            let retired = self.cpu.instret;
            // A clock write takes effect on the following instruction.
            let period = self.bus.regs.ticks_per_cycle();
            // Only a peripheral write can change a source level or a map
            // within an instruction (reads schedule events, handled above),
            // so instructions without one do no routing work.
            let writes = self.bus.periph.write_gen();
            let drive = self.bus.periph.drive_gen();
            self.bus.tick = self.tick;
            let result = self.reference_step(pc);
            self.after_step(Before { pc, period, retired, writes, drive }, result, until.tick);
        }
    }

    fn mark(&mut self) {
        // Into the slot of the last mark, so the drive log keeps its room.
        if let Some(slot) = &mut self.saved {
            let Saved { cpu, regs, periph, tick, board_t0, remainder, halt, resets, last_reset, drive_at, drives, lens, input_cursor } = &mut **slot;
            cpu.clone_from(&self.cpu);
            regs.clone_from(&self.bus.regs);
            periph.clone_from(&self.bus.periph);
            drives.clone_from(&self.drives);
            (*tick, *board_t0, *remainder, *halt, *resets, *last_reset, *drive_at, *lens, *input_cursor) = (self.tick, self.board_t0,
                self.remainder, self.halt, self.resets, self.last_reset, self.drive_at, (self.trace.len(), self.out.len()), self.inputs.cursor);
        } else {
            self.saved = Some(Box::new(Saved {
                cpu: self.cpu.clone(), regs: self.bus.regs.clone(), periph: self.bus.periph.clone(),
                tick: self.tick, remainder: self.remainder, halt: self.halt, resets: self.resets,
                board_t0: self.board_t0,
                last_reset: self.last_reset, drive_at: self.drive_at, drives: self.drives.clone(),
                lens: (self.trace.len(), self.out.len()),
                input_cursor: self.inputs.cursor,
            }));
        }
        self.bus.mark();
        self.board.mark();
    }

    fn host_input(&mut self, input: &dyn Any) -> bool {
        let Some(input) = input.downcast_ref::<B::In>() else { return false; };
        // After every entry due by now, so a live input never reorders an installed replay;
        // entries before the cursor are all due, so the cursor stays valid.
        let now = self.board_time();
        let at = self.inputs.entries.partition_point(|entry| entry.0 <= now);
        self.inputs.entries.insert(at, (now, input.clone()));
        if self.halt.is_none() {
            // Live inputs and replay use the same chip/board/input event ordering.
            let _ = self.events();
        }
        true
    }

    fn publish(&mut self, t: f64) {
        self.board.publish(t);
    }

    fn take_feeds(&mut self) -> Option<Box<dyn Any + Send>> { self.board.take_feeds() }
    fn put_feeds(&mut self, feeds: Box<dyn Any + Send>) { self.board.put_feeds(feeds); }

    fn rewind(&mut self) {
        let saved = self.saved.as_ref().expect("rewind requires a mark");
        if !saved.regs.same_mmu(&self.bus.regs) { self.bus.code_gen += 1; }
        self.bus.rewind();
        self.board.rewind();
        self.inputs.cursor = saved.input_cursor;
        // Restore the CPU's external input history as well as its pending
        // edge latches, together with the cloned matrix and source blocks.
        // Reconstructing history from levels could invent a rising edge.
        self.cpu.clone_from(&saved.cpu);
        self.bus.regs.clone_from(&saved.regs);
        self.bus.periph.clone_from(&saved.periph);
        self.tick = saved.tick;
        self.board_t0 = saved.board_t0;
        self.remainder = saved.remainder;
        self.halt = saved.halt;
        self.resets = saved.resets;
        self.last_reset = saved.last_reset;
        self.drive_at = saved.drive_at;
        self.drives.clone_from(&saved.drives);
        self.trace.truncate(saved.lens.0);
        self.out.truncate(saved.lens.1);
        self.relevel();
    }

    fn pins(&self) -> &[&'static str] { &PINS }
    fn drive(&self, pin: usize) -> Drive {
        assert!(pin < PINS.len(), "S3 GPIO index outside pin list");
        self.bus.periph.drive(pin)
    }
    fn drive_gen(&self) -> u64 { self.bus.periph.drive_gen() }
    fn take_drives(&mut self, out: &mut Vec<DriveEdge>) { out.append(&mut self.drives); }
    fn set_input(&mut self, pin: usize, high: bool) {
        assert!(pin < PINS.len(), "S3 GPIO index outside pin list");
        self.bus.periph.set_input(pin, high, self.tick);
        self.update_lines();
    }
    fn link_in(&mut self, link: u8, byte: u8) {
        self.bus.periph.link_in(link, byte, self.tick);
        self.update_lines();
    }
    fn take_out(&mut self, out: &mut Vec<LinkOut>) { out.append(&mut self.out); }
    fn take_trace(&mut self, out: &mut Vec<(u64, u32)>) { out.append(&mut self.trace); }
    fn take_notes(&mut self, out: &mut Vec<Note>) { self.bus.periph.take_notes(out); }
    fn nets(&self) -> &[NetInfo] { self.board.nets() }
    fn take_bus(&mut self, out: &mut Vec<BusEvent>) { self.board.take_bus(out); }
    fn take_audio(&mut self, out: &mut Audio) { self.board.take_audio(out); }

    fn status(&self) -> Status {
        if let Some(halt) = self.halt { Status::Halted(halt) }
        else if self.cpu.sleeping() { Status::Sleeping { wake: self.wake().map(|wake| wake.tick) } }
        else { Status::Running }
    }

    fn halt_detail(&self) -> Option<String> { self.explain_foreign().or_else(|| self.board.refusal_detail()) }

    fn regs(&self, out: &mut Vec<(&'static str, u32)>) {
        let cpu = &self.cpu;
        out.clear();
        out.extend(AR_NAMES.into_iter().zip(cpu.ar));
        out.extend([
            ("pc", cpu.pc), ("sar", cpu.sar), ("ps", cpu.ps),
            ("windowbase", cpu.window_base), ("windowstart", cpu.window_start),
            ("epc1", cpu.epc1), ("excsave1", cpu.excsave1),
            ("exccause", cpu.exccause), ("excvaddr", cpu.excvaddr),
            ("vecbase", cpu.vecbase), ("intenable", cpu.intenable), ("interrupt", cpu.interrupt),
            ("ccount", cpu.ccount), ("ccompare0", cpu.ccompare[0]),
            ("ccompare1", cpu.ccompare[1]), ("ccompare2", cpu.ccompare[2]),
            ("cpenable", cpu.cpenable), ("br", cpu.br as u32), ("fcr", cpu.fcr), ("fsr", cpu.fsr),
        ]);
    }

    fn read_data(&self, addr: u32, out: &mut [u8]) -> bool {
        let Some(n) = addr.checked_sub(SRAM_DATA).map(|n| n as usize) else { return false; };
        if n >= SRAM_LEN || out.len() > SRAM_LEN - n { return false; }
        out.copy_from_slice(&self.bus.sram[n..n + out.len()]);
        true
    }

    fn chip(&self, out: &mut ChipState) {
        out.timers.clear();
        out.pending.clear();
        out.wake = if self.halt.is_none() { self.wake() } else { None };
    }

    fn set_tier(&mut self, tier: Tier) { self.tier = tier; }

    fn same_state(&self, other: &Self) -> Option<&'static str> {
        // Exhaustive so new state cannot silently escape the differential proof.
        let S3Machine {
            cpu, bus, foreign: _, board, board_t0, pad_inputs: _, inputs, profile: _, tick, remainder, halt, resets,
            last_reset, drive_at, drives, levels, trace, out, saved, tier: _, code: _, horizon: _, outside: _, spin: _,
        } = self;
        let o = other;
        if let Some(field) = cpu.same_state(&o.cpu) { return Some(field); }
        if (*tick, *remainder, *board_t0) != (o.tick, o.remainder, o.board_t0) { return Some("tick"); }
        if *halt != o.halt { return Some("halt"); }
        if (*resets, *last_reset) != (o.resets, o.last_reset) { return Some("resets"); }
        if *drive_at != o.drive_at || *drives != o.drives || *levels != o.levels { return Some("drives"); }
        if *trace != o.trace { return Some("trace"); }
        if *out != o.out { return Some("link output"); }
        if inputs.cursor != o.inputs.cursor || inputs.entries.len() != o.inputs.entries.len() { return Some("inputs"); }
        if let Some(field) = bus.same_state(&o.bus) { return Some(field); }
        if let Some(field) = board.same_state(&o.board) { return Some(field); }
        match (saved, &o.saved) {
            (None, None) => None,
            (Some(a), Some(b)) => {
                let Saved { cpu, regs, periph, tick, board_t0, remainder, halt, resets, last_reset, drive_at, drives, lens, input_cursor } = &**a;
                if let Some(field) = cpu.same_state(&b.cpu) { return Some(if field == "cpu.pc" { "mark: cpu.pc" } else { "mark: cpu" }); }
                if *regs != b.regs { return Some("mark: regs"); }
                if periph.same_state(&b.periph).is_some() { return Some("mark: periph"); }
                if (*tick, *board_t0, *remainder, *halt, *resets, *last_reset, *drive_at, lens, *input_cursor)
                    != (b.tick, b.board_t0, b.remainder, b.halt, b.resets, b.last_reset, b.drive_at, &b.lens, b.input_cursor)
                    || *drives != b.drives
                {
                    return Some("mark");
                }
                None
            }
            _ => Some("mark"),
        }
    }
}
