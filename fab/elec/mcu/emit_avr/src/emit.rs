//! E1: verified integer/bool code, SRAM slots, and relaxed entry transfers.
use std::cell::{Cell, RefCell};

use fab_elec_mcu_avr::asm::{Asm, Label, Relocation, Target};
use fab_elec_mcu_avr::insn::{Insn, Insn::*, Ptr};
use fab_elec_mcu_rt::{fmath, format::*, module::{DeclArg, Module}, ports, watch::Level, SETUP};

use crate::bound::{self, Region};
use crate::helpers::Helper;
use crate::image::{self, Image, PinDef, Program, Role, SerialDef, Stubs, TimerDef};
use crate::range::{EntryRanges, Interval};
use crate::runtime::Timebase;

pub struct Options {
    /// CPU clock in whole Hz, at least one.
    pub f_clk: u32,
    pub image_id: u32,
    /// Image observation level; defaults to Faults.
    pub watch: Level,
    pub flash_limit_bytes: u32,
    /// Proof build: treat every interval as full, retaining the original i32 widths.
    pub full_ranges: bool,
    /// (line, column) of each port site (the compiler's `Compiled.sites`),
    /// so a refusal can say where its value is used. Empty: site numbers.
    pub sites: Vec<(u32, u32)>,
    /// (Module code index, statement ID), ascending: the compiler's `Compiled.marks`.
    pub marks: Vec<(u32, u32)>,
    /// (Module code index of a condition's `if`, condition ID), ascending: the compiler's
    /// `Compiled.cond_marks`.
    pub conds: Vec<(u32, u32)>,
}

impl Default for Options {
    fn default() -> Self { Self { f_clk: 16_000_000, image_id: 0, watch: Level::Faults, flash_limit_bytes: 32768, full_ranges: false, sites: Vec::new(), marks: Vec::new(), conds: Vec::new() } }
}

pub struct EntryReport {
    pub name: String,
    /// Worst-case cycles through RET, excluding the caller's CALL and interrupt service.
    pub max_cycles: u64,
    pub flash_words: u32,
    pub slot_bytes: u16,
    pub var_bytes: u16,
    pub frame_bytes: u16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SerialReport {
    pub depth: u32,
    /// Queuing stretch including interrupt service.
    pub stretch_cycles: u64,
    /// Longest entry, in module order (including setup).
    pub entry: usize,
}

pub struct Built {
    pub image: Image,
    /// Intervals used for code generation (all full in a `full_ranges` proof build), state words
    /// in the module's state order. The image relies on the state words' intervals: a store into
    /// a word writes only the bytes its interval needs, and an index proven in range is not
    /// clamped, so anything that writes a state word from outside the image (none does today)
    /// must keep it inside its interval.
    pub ranges: crate::range::Ranges,
    /// Module entry order, including setup. Flash sizes include RET.
    pub entries: Vec<EntryReport>,
    /// The AVR cycle-derived receive depth; absent without serial.
    pub serial: Option<SerialReport>,
    /// Where each flash word came from: a runtime routine, or an entry's module instruction and
    /// its port site.
    pub map: SourceMap,
}

/// Where a program counter is, in the program's terms.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Where {
    /// The runtime's own code, by the routine it falls in (the nearest named symbol before it).
    Runtime(&'static str),
    /// Module entry `entry` (module order, setup included). `code` is the module instruction whose
    /// code holds the pc (None: the entry's own prologue, epilogue or a control transfer), and
    /// `site` the port site of that instruction when it is a port call. `mark` is the
    /// last statement mark at or before the instruction's code index in this entry.
    Entry { entry: usize, code: Option<u32>, site: Option<u32>, mark: Option<u32> },
    /// Data, or no code at all.
    Outside,
}

/// The map from flash word addresses (the AVR's pc) back to the program.
#[derive(Clone, Debug, Default)]
pub struct SourceMap {
    /// Named runtime routines by start, ascending; the runtime ends where the first entry starts.
    pub runtime: Vec<(u32, &'static str)>,
    /// Per module entry: its code's [start, end) word range.
    pub entries: Vec<(u32, u32)>,
    /// Per emitted module instruction, ascending: [start, end), module entry, instruction, port site.
    pub ops: Vec<OpSpan>,
    /// (Word address, statement ID), ascending: first emitted op at or after each
    /// mark in its entry. Statements beginning at one address all appear, outermost first.
    /// A mark with no following emitted op has no address.
    pub stmt_starts: Vec<(u32, u32)>,
    /// (Word address, condition ID), ascending: the conditional branch that tests each source
    /// condition's `if` (`Options::conds`), taken when the `if`'s value is true. A condition whose
    /// `if` the module does not hold has none.
    pub conds: Vec<(u32, u32)>,
    /// (Word address, module entry), ascending: the word after each CALL into an
    /// entry, including setup's reset call and every dispatcher call.
    pub returns: Vec<(u32, usize)>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OpSpan {
    pub start: u32,
    pub end: u32,
    pub entry: usize,
    pub code: u32,
    pub site: Option<u32>,
    /// Last statement mark at or before `code` within this entry; None before its first mark.
    pub mark: Option<u32>,
}

impl SourceMap {
    pub fn find(&self, pc: u32) -> Where {
        if let Some(entry) = self.entries.iter().position(|&(start, end)| (start..end).contains(&pc)) {
            let k = self.ops.partition_point(|o| o.end <= pc);
            let op = self.ops.get(k).filter(|o| o.start <= pc && o.entry == entry);
            return Where::Entry { entry, code: op.map(|o| o.code), site: op.and_then(|o| o.site), mark: op.and_then(|o| o.mark) };
        }
        let code_start = self.entries.iter().map(|e| e.0).min().unwrap_or(u32::MAX);
        match self.runtime.iter().rev().find(|r| r.0 <= pc) {
            Some(&(_, name)) if pc < code_start => Where::Runtime(name),
            _ => Where::Outside,
        }
    }
}

fn runtime_names(s: &image::Symbols) -> Vec<(u32, &'static str)> {
    let mut v = vec![
        (s.t0, "t0"), (s.pass, "pass"), (s.sleep, "sleep"), (s.write, "write"), (s.duty, "duty"),
        (s.read, "read"), (s.record_write, "record_write"), (s.record_read, "record_read"),
        (s.adc, "analog_read"), (s.adc_convert, "adc_convert"),
        (s.clock_us, "clock_us"), (s.clock_ms, "clock_ms"), (s.timer_stop, "timer_stop"),
        (s.timer_start, "timer_start"), (s.timer_period, "timer_period"), (s.serial_byte, "serial_byte"),
        (s.loop_cap, "loop_cap"), (s.push, "push"), (s.fault, "fault_cell"), (s.rx, "rx"), (s.rx_sample, "rx_sample"),
        (s.rx_scan, "rx_scan"), (s.rx_take, "rx_take"), (s.rx_check, "rx_check"), (s.rx_defer, "rx_defer"),
        (s.bookkeep, "bookkeep"), (s.now48, "now48"), (s.delay, "delay"), (s.lock, "lock"), (s.tov, "tov"),
        (s.udre, "udre"), (s.write_effect, "write_effect"), (s.duty_effect, "duty_effect"),
        (s.pad, "pad"),
        (s.shift_spi, "shift_out_spi"),
        (s.mul32, "mul32"),
        (s.udivmod32, "udivmod32"),
        (s.sdiv32, "sdiv32"),
        (s.srem32, "srem32"),
        (s.shl32, "shl32"),
        (s.shr32, "shr32"),
        (s.sar32, "sar32"),
        (s.eeprom_read, "eeprom_read"), (s.eeprom_write, "eeprom_write"),
    ];
    v.extend(s.shift_bitbang.iter().map(|&(_, start, _)| (start, "shift_out_bitbang")));
    v.extend(s.dispatch.iter().map(|&a| (a, "dispatch")));
    // Zero is the reset vector table; a symbol a build does not emit stays zero.
    v.retain(|&(a, _)| a != 0);
    v.push((0, "vectors"));
    v.sort_by_key(|&(a, name)| (a, name));
    v.dedup_by_key(|r| r.0);
    v
}

/// Why the emitter refused a module, and where: the module entry and code index it was at.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Refusal {
    /// Module entry, setup included (module order).
    pub entry: usize,
    /// The module code index.
    pub code: u32,
    /// The reason, naming the entry and code index.
    pub message: String,
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

fn error(m: &Module<'_>, k: usize, at: u32, reason: impl std::fmt::Display) -> Refusal {
    let name = m.entries().get(k).map_or("<module>", |e| m.string(e.name));
    Refusal { entry: k, code: at, message: format!("entry `{name}`, code {at}: {reason}") }
}

pub const FUNCTIONS_UNSUPPORTED: &str = "SharedFunctionsUnsupported: AVR emission does not support shared functions yet";

pub fn build_module(m: &Module<'_>, o: &Options) -> Result<Built, Refusal> {
    if !m.funcs().is_empty() { return Err(error(m, 0, 0, FUNCTIONS_UNSUPPORTED)); }
    if o.f_clk == 0 { return Err(error(m, 0, 0, "CPU clock must be at least one Hz")); }
    if m.motion().is_some() {
        return Err(error(m, 0, 0, "motion input is unavailable on the 328P"));
    }
    if m.touch() {
        return Err(error(m, 0, 0, "touch input is unavailable on the 328P"));
    }
    if m.decls().iter().any(|d| matches!(d.form, ports::FORM_SPEAKER | ports::FORM_MIC)) {
        return Err(error(m, 0, 0, "speaker and mic are unavailable on the 328P"));
    }
    if let Some(net) = m.decls().iter().find(|d| d.form == ports::FORM_NET) {
        return Err(error(m, 0, 0, format!("net declaration `{}`: `net` connections are not available on the 328P yet", m.string(net.name))));
    }
    if let Some(eth) = m.decls().iter().find(|d| d.form == ports::FORM_ETH) {
        return Err(error(m, 0, 0, format!("eth declaration `{}`: Ethernet is unavailable on the 328P", m.string(eth.name))));
    }
    if let Some(buffer) = m.buffers().first() {
        let name = m.string(m.decls()[buffer.decl as usize].name);
        return Err(error(m, 0, 0, format!("buffer declaration `{name}`: buffers, including read-only images, are unavailable on the 328P")));
    }
    let mut ranges = crate::range::analyse(m);
    if o.full_ranges {
        for entry in &mut ranges.entries {
            entry.values.fill(Interval::FULL); entry.vars.fill(Interval::FULL);
        }
        ranges.state.fill(Interval::FULL);
    }
    let tb = Timebase::new(o.f_clk);
    let plans: Vec<_> = (0..m.entries().len()).map(|k| Plan::new(m, k, o, &ranges.entries[k], &ranges.state)).collect::<Result<_, _>>()?;
    let setup = m.entry(SETUP);
    let mut index = vec![None; plans.len()];
    let mut next = 0;
    for (k, slot) in index.iter_mut().enumerate() {
        if Some(k) != setup { *slot = Some(next); next += 1; }
    }
    let context = setup.unwrap_or(0);
    let at = m.entries().get(context).map_or(0, |e| e.body + 1);
    let decl_error = |name: u32, reason: &str| error(m, context, at,
        format!("declaration `{}`: {reason}", m.string(name)));
    let mapped = |entry: u16| index.get(entry as usize).copied().flatten()
        .ok_or_else(|| error(m, entry as usize, m.entries()[entry as usize].body + 1,
            "setup cannot also be a timer or serial handler"));
    let mut p = Program {
        f_clk: tb.f_clk, image_id: o.image_id, watch: o.watch, flash_limit_bytes: o.flash_limit_bytes,
        state_init: m.state_init().iter().flat_map(|w| w.to_le_bytes()).collect(),
        shared: m.shared().iter().flat_map(|w| w.to_le_bytes()).collect(),
        scratch_bytes: plans.iter().map(|p| p.bytes).max().unwrap_or(0),
        ports: 0, records: 0,
        ..Program::default()
    };
    for d in m.decls() {
        match (d.form, d.args.as_slice()) {
            (ports::FORM_OUTPUT, [DeclArg::Int(pin)]) |
            (ports::FORM_INPUT, [DeclArg::Int(pin), _]) => {
                // Watch off leaves PD1 (TXD) to the program, and PD0 (RXD) too without serial.
                let first = if o.watch != Level::Off { 2 } else if m.serial().is_empty() { 0 } else { 1 };
                if !(first..20).contains(pin) {
                    return Err(decl_error(d.name, &format!("output/input pin must be in {first}..=19 on the 328P")));
                }
                let role = if d.form == ports::FORM_OUTPUT { Role::Output } else {
                    let DeclArg::Int(pull) = d.args[1] else { unreachable!("verified input") };
                    if pull == 2 { return Err(decl_error(d.name, "input pull-down is unavailable on the 328P")); }
                    Role::Input { pull: pull == 1 }
                };
                p.pins.push(PinDef { pin: *pin as u8, role });
            }
            (ports::FORM_ANALOG, [DeclArg::Int(pin)]) => {
                if fab_elec_mcu_avr::chip::adc_channel(*pin as u32).is_none() {
                    return Err(decl_error(d.name, "analog pin must be in 14..=21 on the 328P"));
                }
                p.pins.push(PinDef { pin: *pin as u8, role: Role::Analog });
            }
            (ports::FORM_EVERY | ports::FORM_AFTER, [DeclArg::Int(us), DeclArg::Entry(entry)]) => {
                if *us == 0 { return Err(error(m, *entry as usize, m.entries()[*entry as usize].body + 1,
                    "every/after zero delay is unavailable on the 328P")); }
                let (n, denominator) = tb.us;
                let us = *us as u64;
                if us % u64::from(n) != 0 {
                    let form = if d.form == ports::FORM_EVERY { "every" } else { "after" };
                    let lower = us / u64::from(n) * u64::from(n);
                    let upper = lower + u64::from(n);
                    let nearest = if lower == 0 { format!("the nearest period is {upper} µs") }
                        else { format!("the nearest periods are {lower} µs and {upper} µs") };
                    return Err(error(m, *entry as usize, m.entries()[*entry as usize].body + 1,
                        format!("{form} {us} µs is not a whole number of Timer0 ticks at {} Hz (one tick per 8 CPU cycles); {nearest}", tb.f_clk)));
                }
                p.timers.push(TimerDef { entry: mapped(*entry)?, period_ticks: us / u64::from(n) * u64::from(denominator),
                    repeat: d.form == ports::FORM_EVERY });
            }
            _ => {}
        }
    }
    for pin in m.pwm() {
        p.pins.push(PinDef { pin: u8::try_from(pin.pin).map_err(|_| error(m, context, at,
            format!("PWM pin {} does not fit the 328P", pin.pin)))?, role: Role::Pwm(pin.setting) });
    }
    for s in m.serial() {
        if p.serial.is_some() || s.link != 0 {
            return Err(error(m, s.entry as usize, m.entries()[s.entry as usize].body + 1,
                "serial requires the single USART0 link (link 0)"));
        }
        p.serial = Some(SerialDef { entry: mapped(s.entry)?, setting: s.setting, fifo: s.fifo });
    }
    let pins: Vec<_> = p.pins.iter().filter_map(|def| {
        if def.role == Role::Analog {
            return Some(InlinePin { pin: def.pin, port: 0, input: 0, bit: 0, role: def.role });
        }
        let gpio = fab_elec_mcu_avr::chip::by_number(u32::from(def.pin))?;
        // pin_register returns the data address of PINx; bit instructions use I/O offsets.
        let input = fab_elec_mcu_avr::chip::pin_register(gpio.port) as u8 - 0x20;
        Some(InlinePin { pin: def.pin, port: input + 2, input, bit: gpio.bit, role: def.role })
    }).collect();
    for (k, plan) in plans.iter().enumerate() {
        let mut skipped = 0;
        for (step, dead) in plan.steps.iter().zip(&plan.dead) {
            if *dead { continue; }
            if matches!(step, Step::EndLoop) && skipped != 0 { skipped -= 1; }
            if let Step::Op(i) = step {
                if i.op == OP_LOOP && (skipped != 0 || i.args[0] == 0) { skipped += 1; }
                if skipped == 0 {
                    if let Some(helper) = plan.helper(*i) {
                        if !p.helpers.contains(&helper) { p.helpers.push(helper); }
                    }
                }
                if i.op == OP_PORT {
                    if i.args[0] == u32::from(ports::SHIFT_OUT) {
                        let data = plan.konst[i.args[4] as usize];
                        let clock = plan.konst[i.args[5] as usize];
                        let computed = match (data, clock) {
                            (None, None) => Some("data and clock"),
                            (None, _) => Some("data"), (_, None) => Some("clock"), _ => None,
                        };
                        if let Some(computed) = computed {
                            return Err(error(m, k, i.at, format!("`shift_out` on the 328P needs its data and clock pins named by declarations; this call computes its {computed} pin")));
                        }
                        let pair = (data.unwrap() as u8, clock.unwrap() as u8);
                        if !p.shift_pairs.contains(&pair) { p.shift_pairs.push(pair); }
                    } else if inline_pin(plan, &pins, i.args).is_some() {
                        // Analog reads need only the converter; there is no ten-bit watch record.
                        if i.args[0] == u32::from(ports::ADC_READ) { p.adc_convert = true; }
                        else { p.records |= 1 << i.args[0]; }
                    } else { p.ports |= 1 << i.args[0]; }
                }
            }
        }
    }
    // The closures borrow plans only for this build; the returned image owns all bytes.
    let bodies: Vec<_> = plans.iter().map(|plan| {
        let pins = &pins;
        move |a: &mut Asm, s: &Stubs| {
            let start = a.here();
            plan.start.set(start);
            plan.ops.borrow_mut().clear();
            plan.branches.borrow_mut().clear();
            plan.regions.borrow_mut().clear();
            Emitter { a: &mut *a, s, plan, pins, watch: o.watch, acc: None }.emit();
            plan.words.set(a.here() - start + 1);
        }
    }).collect();
    p.entries = bodies.iter().enumerate().filter_map(|(k, body)|
        (Some(k) != setup).then_some(body as image::Entry<'_>)).collect();
    p.setup = setup.map(|k| &bodies[k] as image::Entry<'_>);
    let refusal = |reason: String| {
        let k = if reason.contains("RAM") || reason.contains("ring") {
            plans.iter().enumerate().max_by_key(|(_, p)| p.bytes).map(|(k, _)| k)
        } else { plans.iter().enumerate().max_by_key(|(_, p)| p.words.get()).map(|(k, _)| k) }
            .unwrap_or(context);
        error(m, k, m.entries().get(k).map_or(0, |e| e.body + 1), reason)
    };
    let build = |p: &Program<'_>| -> Result<Image, String> {
        let (image, relocation) = image::build_with_timebase(p, &tb)?;
        for plan in &plans { plan.relocate(&relocation); }
        Ok(image)
    };
    let mut image = build(&p).map_err(refusal)?;
    let entry_bounds = |image: &Image| plans.iter().map(|p|
        bound::entry(image, p.start.get()..p.start.get() + p.words.get(), &p.regions.borrow()))
        .collect::<Vec<_>>();
    let bounds = entry_bounds(&image);
    let serial = if let Some(s) = p.serial {
        let (entry, &longest) = bounds.iter().enumerate().max_by_key(|(_, bound)| *bound)
            .expect("serial has a handler");
        let (depth, stretch_cycles) = bound::stretch(&image, longest, s.setting).map_err(refusal)?;
        if depth != s.fifo {
            p.serial.as_mut().unwrap().fifo = depth;
            let rebuilt = build(&p).map_err(|reason| {
                let name = m.string(m.entries()[entry].name);
                let us = longest as f64 * 1_000_000.0 / f64::from(o.f_clk);
                error(m, entry, m.entries()[entry].body + 1,
                    format!("{reason}; longest handler `{name}`: {longest} cycles ({us:.3} µs), serial {} baud", s.setting.asked))
            })?;
            assert_eq!(rebuilt.words.len(), image.words.len(), "flash size must not depend on RX FIFO depth");
            image = rebuilt;
            assert_eq!(entry_bounds(&image), bounds, "entry bounds must not depend on RX FIFO depth");
        }
        Some(SerialReport { depth, stretch_cycles, entry })
    } else { None };
    let entries = plans.iter().enumerate().map(|(k, p)| EntryReport {
        name: m.string(m.entries()[k].name).to_owned(), flash_words: p.words.get(),
        max_cycles: bounds[k],
        slot_bytes: p.slot_bytes, var_bytes: p.var_bytes, frame_bytes: p.frame_bytes,
    }).collect();
    let mut map = SourceMap { runtime: runtime_names(&image.symbols), ..SourceMap::default() };
    for (k, plan) in plans.iter().enumerate() {
        let start = plan.start.get();
        map.entries.push((start, start + plan.words.get()));
        let code = m.code();
        let entry = &m.entries()[k];
        // Exclude a preceding entry's terminal mark at this entry's header.
        let first = o.marks.partition_point(|&(at, _)| at < entry.body + 1);
        let end = o.marks.partition_point(|&(at, _)| at <= entry.code + entry.len);
        let marks = &o.marks[first..end];
        let ops = plan.ops.borrow();
        for &(start, end, at) in ops.iter() {
            // A port instruction's words: op, port, site, argument count, result count, ...
            let site = (code[at as usize] & OP_MASK == OP_PORT).then(|| code[at as usize + 2]);
            let before = marks.partition_point(|&(code, _)| code <= at);
            let mark = before.checked_sub(1).map(|i| marks[i].1);
            map.ops.push(OpSpan { start, end, entry: k, code: at, site, mark });
        }
        for &(at, id) in marks {
            let next = ops.partition_point(|&(_, _, code)| code < at);
            if let Some(&(start, _, _)) = ops.get(next) {
                map.stmt_starts.push((start, id));
            }
        }
        // Conditions that share an `if` (an outer one folded away) name it by the innermost.
        for &(pc, at) in plan.branches.borrow().iter() {
            let k = o.conds.partition_point(|&(code, _)| code <= at);
            if let Some(&(_, cond)) = k.checked_sub(1).map(|k| &o.conds[k]).filter(|c| c.0 == at) {
                map.conds.push((pc, cond));
            }
        }
        if Some(k) == setup {
            map.returns.push((image.symbols.setup_call + 2, k));
        } else if let Some(dispatch) = index[k] {
            // CALL occupies two flash words on the 328P.
            map.returns.push((image.symbols.dispatch_call[dispatch] + 2, k));
        }
    }
    map.ops.sort_by_key(|o| o.start);
    map.stmt_starts.sort_by_key(|s| s.0);
    map.conds.sort_by_key(|c| c.0);
    map.returns.sort_by_key(|r| r.0);
    Ok(Built { image, entries, serial, map, ranges })
}

#[derive(Clone, Copy)]
pub(crate) struct Instruction<'a> { pub at: u32, pub op: u32, pub sub: u32, pub args: &'a [u32], pub end: u32 }

#[derive(Clone, Copy)]
pub(crate) enum Step<'a> { Op(Instruction<'a>), Else, EndIf, EndLoop }

enum Finish { Root, Then(u32), Else, Loop }

// Walk verified blocks iteratively, retaining their boundaries for both passes.
fn steps<'a>(m: &Module<'a>, k: usize) -> Vec<Step<'a>> {
    let code = m.code();
    let e = &m.entries()[k];
    let mut pc = e.body + 1;
    let mut stack = vec![(e.code + e.len, Finish::Root)];
    let mut out = Vec::new();
    while let Some((end, _)) = stack.last() {
        if pc == *end {
            match stack.pop().unwrap().1 {
                Finish::Root => break,
                Finish::Then(end) => {
                    pc += 1;
                    stack.push((end, Finish::Else));
                    out.push(Step::Else);
                }
                Finish::Else => out.push(Step::EndIf),
                Finish::Loop => out.push(Step::EndLoop),
            }
            continue;
        }
        let at = pc;
        let word = code[pc as usize];
        let op = word & OP_MASK;
        let count = match op {
            OP_CONST | OP_GET | OP_UN | OP_SET => 2,
            OP_CONST_D | OP_BIN | OP_CMP_F | OP_CMP_I | OP_CMP_D | OP_WRAP => 3,
            OP_LOAD | OP_STORE => 4 + u32::from(word >> SUB_SHIFT == REGION_BUF_LONG),
            OP_SEL | OP_FMA => 4,
            OP_PORT => 4 + code[pc as usize + 3] + code[pc as usize + 4],
            OP_IF | OP_LOOP | OP_BREAK | OP_CONTINUE => 1,
            _ => unreachable!("verified opcode"),
        };
        let args = &code[pc as usize + 1..(pc + count + 1) as usize];
        pc += count + 1;
        let mut end = pc;
        if op == OP_IF {
            let otherwise = pc + 1 + code[pc as usize];
            end = otherwise + 1 + code[otherwise as usize];
            stack.push((otherwise, Finish::Then(end)));
            pc += 1;
        } else if op == OP_LOOP {
            end = pc + 1 + code[pc as usize];
            stack.push((end, Finish::Loop));
            pc += 1;
        }
        out.push(Step::Op(Instruction { at, op, sub: word >> SUB_SHIFT, args, end }));
    }
    out
}

// The values an instruction reads at run time. A folded conversion reads
// only its select's condition.
fn reads(i: &Instruction<'_>, folded: &[Option<Folded>]) -> Vec<u32> {
    let a = i.args;
    if pure(i) {
        match folded[a[0] as usize] {
            Some(Folded::Sel { c, .. }) => return vec![c],
            Some(Folded::K(_)) => return vec![],
            None => {}
        }
    }
    match i.op {
        OP_SET | OP_UN | OP_WRAP => vec![a[1]],
        OP_BIN | OP_CMP_I | OP_CMP_F | OP_CMP_D => vec![a[1], a[2]],
        OP_SEL | OP_FMA => a[1..4].to_vec(),
        OP_LOAD => if a[3] != NONE { vec![a[3]] } else { vec![] },
        OP_STORE => if a[2] != NONE { vec![a[2], a[3]] } else { vec![a[3]] },
        OP_IF => vec![a[0]],
        OP_PORT => a[4..4 + a[2] as usize].to_vec(),
        _ => vec![],
    }
}

/// A float that is a constant or a select between two constants: the
/// language's literal joins (`if c { high } else { low }` selects 1.0 or 0.0
/// in f32, then converts to the port's i32). On the i32 side of a
/// conversion the folded value is emitted as an integer constant or select,
/// and the float chain is dead. Leaves are bit patterns of the value's type.
#[derive(Clone, Copy)]
pub(crate) enum Folded { K(u32), Sel { c: u32, t: u32, e: u32 } }

impl Folded {
    fn map(self, f: impl Fn(u32) -> u32) -> Folded {
        match self { Folded::K(k) => Folded::K(f(k)), Folded::Sel { c, t, e } => Folded::Sel { c, t: f(t), e: f(e) } }
    }
}

// Folds per value in code order (SSA: operands precede their uses). Only
// i32 results matter for emission; f32 entries feed them.
fn fold(m: &Module<'_>, k: usize, steps: &[Step<'_>]) -> Vec<Option<Folded>> {
    let n = m.entries()[k].n_vals as usize;
    let mut f: Vec<Option<Folded>> = vec![None; n];
    let fl = |x: u32| f32::from_bits(x);
    for step in steps {
        let Step::Op(i) = step else { continue };
        let a = i.args;
        let ty = |v: u32| m.ty(k, v, false);
        let out = match i.op {
            OP_CONST if ty(a[0]) == TY_F32 => Some(Folded::K(a[1])),
            OP_SEL if ty(a[0]) == TY_F32 => match (f[a[2] as usize], f[a[3] as usize]) {
                (Some(Folded::K(t)), Some(Folded::K(e))) => Some(Folded::Sel { c: a[1], t, e }),
                _ => None,
            },
            OP_UN => f[a[1] as usize].and_then(|x| match i.sub {
                UN_FLOOR_F => Some(x.map(|v| fmath::floor_f32(fl(v)).to_bits())),
                UN_CEIL_F => Some(x.map(|v| fmath::ceil_f32(fl(v)).to_bits())),
                UN_TRUNC_F => Some(x.map(|v| fmath::trunc_f32(fl(v)).to_bits())),
                UN_ROUND_F => Some(x.map(|v| fmath::round_f32(fl(v)).to_bits())),
                UN_ABS_F => Some(x.map(|v| fmath::abs_f32(fl(v)).to_bits())),
                UN_NEG_F => Some(x.map(|v| (-fl(v)).to_bits())),
                UN_F2I => Some(x.map(|v| fl(v) as i32 as u32)),
                UN_BITS_FI => Some(x),
                _ => None,
            }),
            _ => None,
        };
        if pure(i) { f[a[0] as usize] = out; }
    }
    f
}

// CONST..=FMA define one value and have no other effect.
pub(crate) fn pure(i: &Instruction<'_>) -> bool { (OP_CONST..=OP_FMA).contains(&i.op) }

#[derive(Clone, Copy, Default)]
struct Slot { offset: u16, size: u8 }

/// Where a value lives between its definition and its uses.
#[derive(Clone, Copy)]
enum Home {
    /// A constant (i32 or bool): never stored; each use takes its bytes as immediates.
    Imm(u32),
    /// A get whose variable cannot change before any use: read the variable's own slot.
    Var(Slot),
    /// A compare's SREG bit, consumed by the next emitting step's if; set means true.
    Flags { bit: u8, set: bool },
    /// A single use in the next emitting step takes the result directly from r16..r19.
    Acc { size: u8, consumer: usize },
    /// A scratch slot; a narrow slot is one byte, zero-extended on use.
    Slot(Slot),
}

/// One byte of an operand, as an instruction can take it.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ByteSrc {
    Imm(u8),
    Reg(u8),
    /// A scratch byte, at this offset from Y.
    Mem(u16),
    /// Above a narrow slot's size: r2.
    Zero,
}

#[derive(Default)]
struct Life { first: Option<u32>, last: u32, accesses: u32 }

struct Colour { size: u8, last: u32, accesses: u32, first: u32, offset: u16 }

/// The only value/variable shape decision. Other types retain their existing width.
fn shape(ty: u8, interval: Interval) -> u8 {
    if ty == TY_BOOL || (ty == TY_I32 && interval.lo >= 0 && interval.hi <= 255) { 1 } else { 4 }
}

fn fixed_index(off: u32, extent: u32, konst: &[Option<u32>]) -> Option<u32> {
    if off == NONE { Some(0) } else { konst[off as usize].map(|x| x.min(extent - 1)) }
}

/// The only store-width decision. Every possible destination must keep its
/// untouched upper bytes zero; frame words have no such proof. The state
/// intervals are a contract with every writer of state ([`Built::ranges`]).
fn store_width(i: Instruction<'_>, konst: &[Option<u32>], state: &[Interval]) -> u8 {
    if i.sub != REGION_STATE { return 4; }
    let a = i.args;
    let words = match fixed_index(a[2], a[1], konst) {
        Some(off) => (a[0] + off) as usize..(a[0] + off + 1) as usize,
        None => a[0] as usize..(a[0] + a[1]) as usize,
    };
    let ranges = &state[words];
    if ranges.iter().all(|r| r.lo >= 0 && r.hi <= 255) { 1 }
    else if ranges.iter().all(|r| r.lo >= 0 && r.hi <= 65535) { 2 }
    else { 4 }
}

/// The only index-width decision: 4 retains the unsigned i32 clamp; 1 or 2
/// elides it and scales at the width holding the extent's largest byte offset.
fn index_width(interval: Interval, extent: u32) -> u8 {
    if interval.lo < 0 || i64::from(interval.hi) > i64::from(extent - 1) { 4 }
    else if 4 * u64::from(extent - 1) <= 255 { 1 }
    else { 2 }
}

// Keep immediate orientation, compare width and flag sense identical in planning and emission.
fn compare_operands(i: Instruction<'_>, konst: &[Option<u32>]) -> (u32, u32, u32) {
    let (x, y, sub) = (i.args[1], i.args[2], i.sub);
    if konst[x as usize].is_some() && konst[y as usize].is_none() {
        (y, x, match sub { CMP_LT => CMP_GT, CMP_GT => CMP_LT, CMP_LE => CMP_GE, CMP_GE => CMP_LE, s => s })
    } else { (x, y, sub) }
}

fn compare_narrow(i: Instruction<'_>, konst: &[Option<u32>], ranges: &[Interval]) -> bool {
    let (x, y, sub) = compare_operands(i, konst);
    if shape(TY_I32, ranges[x as usize]) != 1 || shape(TY_I32, ranges[y as usize]) != 1 { return false; }
    konst[y as usize].is_none_or(|k| {
        let compared = if matches!(sub, CMP_LE | CMP_GT) { k.checked_add(1) } else { Some(k) };
        compared.is_some_and(|k| k <= 255)
    })
}

fn compare_flags(sub: u32, immediate: Option<u32>, narrow: bool) -> Option<(u8, bool)> {
    let sub = if let Some(k) = immediate {
        match sub {
            // These emit a constant bool, not a compare, and keep their slot.
            CMP_LE | CMP_GT if k == i32::MAX as u32 => return None,
            CMP_LE => CMP_LT, CMP_GT => CMP_GE, sub => sub,
        }
    } else { sub };
    let order = if narrow { 0 } else { 4 }; // unsigned carry or signed S
    Some(match sub {
        CMP_LT | CMP_GT => (order, true), CMP_LE | CMP_GE => (order, false),
        CMP_EQ => (1, true), CMP_NE => (1, false), _ => unreachable!("verified compare"),
    })
}

/// Control/data facts shared by emission and range analysis. In particular,
/// zero_vars is A3's definite-assignment result, not a second approximation.
pub(crate) struct AnalysisPlan<'a> {
    pub steps: Vec<Step<'a>>,
    pub dead: Vec<bool>,
    pub folded: Vec<Option<Folded>>,
    pub zero_vars: Vec<bool>,
    uses: Vec<u32>,
}

pub(crate) fn analysis_plan<'a>(m: &Module<'a>, k: usize) -> AnalysisPlan<'a> {
    let e = &m.entries()[k];
    let steps = steps(m, k);
    let folded = fold(m, k, &steps);
    // Unused values cost nothing, whatever their type: drop every pure
    // instruction whose value is never read, then what only fed it.
    let mut uses = vec![0u32; e.n_vals as usize];
    let mut def_step = vec![None; e.n_vals as usize];
    for (s, step) in steps.iter().enumerate() {
        if let Step::Op(i) = step {
            for v in reads(i, &folded) { uses[v as usize] += 1; }
            if pure(i) { def_step[i.args[0] as usize] = Some(s); }
        }
    }
    let mut dead = vec![false; steps.len()];
    let mut work: Vec<u32> = (0..e.n_vals).filter(|&v| uses[v as usize] == 0).collect();
    while let Some(v) = work.pop() {
        let Some(s) = def_step[v as usize] else { continue };
        if std::mem::replace(&mut dead[s], true) { continue; }
        let Step::Op(i) = &steps[s] else { unreachable!("values are defined by instructions") };
        for u in reads(i, &folded) {
            uses[u as usize] -= 1;
            if uses[u as usize] == 0 { work.push(u); }
        }
    }
    #[derive(Clone)]
    struct Assigned { vars: Vec<bool>, live: bool }
    enum Join { If(Assigned, Option<Assigned>), Loop(Assigned) }
    let mut assigned = Assigned { vars: vec![false; e.n_vars as usize], live: true };
    let mut joins = Vec::new();
    let mut zero_vars = vec![false; e.n_vars as usize];
    for (step, dead) in steps.iter().zip(&dead) {
        if *dead { continue; }
        match *step {
            Step::Op(i) => match i.op {
                OP_IF => joins.push(Join::If(assigned.clone(), None)),
                OP_LOOP => {
                    joins.push(Join::Loop(assigned.clone()));
                    assigned.live &= i.args[0] != 0;
                }
                OP_GET if assigned.live => zero_vars[i.args[1] as usize] |= !assigned.vars[i.args[1] as usize],
                OP_SET if assigned.live => assigned.vars[i.args[0] as usize] = true,
                OP_BREAK | OP_CONTINUE => assigned.live = false,
                _ => {}
            },
            Step::Else => {
                let Some(Join::If(before, then)) = joins.last_mut() else { unreachable!("structured if") };
                *then = Some(std::mem::replace(&mut assigned, before.clone()));
            }
            Step::EndIf => {
                let Some(Join::If(_, Some(then))) = joins.pop() else { unreachable!("structured if arms") };
                // Intersect even a terminating arm: ignoring its assignments
                // after break/continue is conservative for the surviving arm.
                for (var, yes) in assigned.vars.iter_mut().zip(then.vars) { *var &= yes; }
                assigned.live |= then.live;
            }
            Step::EndLoop => {
                let Some(Join::Loop(before)) = joins.pop() else { unreachable!("structured loop") };
                // No body assignment escapes the loop, even a nonzero cap:
                // break may bypass it. The first iteration proves every get;
                // later iterations cannot make an assigned variable unassigned.
                assigned = before;
            }
        }
    }
    AnalysisPlan { steps, dead, folded, zero_vars, uses }
}

struct Plan<'a> {
    steps: Vec<Step<'a>>,
    /// Per step: a pure instruction whose value nothing reads (no code, no slot).
    dead: Vec<bool>,
    folded: Vec<Option<Folded>>,
    values: Vec<Home>,
    shapes: Vec<u8>,
    value_ranges: Vec<Interval>,
    state_ranges: Vec<Interval>,
    vars: Vec<Slot>,
    /// A live get can run before this variable has been assigned.
    zero_vars: Vec<bool>,
    konst: Vec<Option<u32>>,
    frame: u16,
    counters: u16,
    bytes: u16,
    slot_bytes: u16,
    var_bytes: u16,
    frame_bytes: u16,
    words: Cell<u32>,
    /// The entry's first word, and its instructions' code ([start, end), module instruction).
    start: Cell<u32>,
    ops: RefCell<Vec<(u32, u32, u32)>>,
    /// Each `if`'s conditional branch: (its word address, the `if`'s module instruction).
    branches: RefCell<Vec<(u32, u32)>>,
    /// Closed control ranges, children before parents, for cycle accounting.
    regions: RefCell<Vec<Region>>,
}

impl<'a> Plan<'a> {
    // Shared by linking and emission, including the existing inline fast paths.
    fn helper(&self, i: Instruction<'_>) -> Option<Helper> {
        if pure(&i) && self.folded[i.args[0] as usize].is_some() { return None; }
        match i.op {
            OP_BIN => match i.sub {
                BIN_MUL_I if self.shapes[i.args[0] as usize] == 4 => Some(Helper::Mul32),
                BIN_DIV_I => Some(Helper::Sdiv32),
                BIN_REM_I if !self.konst[i.args[2] as usize].is_some_and(|n| n <= i32::MAX as u32 && n.is_power_of_two()) => Some(Helper::Srem32),
                BIN_SHL_I | BIN_SHR_I | BIN_SHR_UI if self.konst[i.args[2] as usize].is_none() => Some(match i.sub {
                    BIN_SHL_I => Helper::Shl32, BIN_SHR_UI => Helper::Shr32, _ => Helper::Sar32,
                }),
                _ => None,
            },
            OP_WRAP if !i.args[2].is_power_of_two() => Some(Helper::Srem32),
            OP_FMA => Some(Helper::Mul32),
            _ => None,
        }
    }

    /// Convert every emission record together, before bounds or source maps read it.
    fn relocate(&self, relocation: &Relocation) {
        let start = self.start.get();
        // The body end names the builder's one-word RET, which cannot grow.
        let end = relocation.at(start + self.words.get() - 1) + 1;
        let start = relocation.at(start);
        self.start.set(start);
        self.words.set(end - start);
        for (start, end, _) in self.ops.borrow_mut().iter_mut() {
            *start = relocation.at(*start); *end = relocation.at(*end);
        }
        for (at, _) in self.branches.borrow_mut().iter_mut() { *at = relocation.at(*at); }
        let range = |r: &mut std::ops::Range<u32>| {
            r.start = relocation.at(r.start); r.end = relocation.at(r.end);
        };
        for region in self.regions.borrow_mut().iter_mut() {
            match region {
                Region::Branch { then, otherwise } => { range(then); range(otherwise); }
                Region::Repeat { body, .. } => range(body),
            }
        }
    }

    fn new(m: &Module<'a>, k: usize, o: &Options, ranges: &EntryRanges, state: &[Interval]) -> Result<Self, Refusal> {
        let e = &m.entries()[k];
        let AnalysisPlan { steps, dead, folded, zero_vars, uses } = analysis_plan(m, k);
        let types: Vec<_> = (0..e.n_vals).map(|i| m.ty(k, i, false)).collect();
        let var_types: Vec<_> = (0..e.n_vars).map(|i| m.ty(k, i, true)).collect();
        let shapes: Vec<_> = types.iter().zip(&ranges.values).map(|(&ty, &r)| shape(ty, r)).collect();
        let var_shapes: Vec<_> = var_types.iter().zip(&ranges.vars).map(|(&ty, &r)| shape(ty, r)).collect();
        let mut lives: Vec<_> = (0..e.n_vals).map(|_| Life::default()).collect();
        let mut vars: Vec<_> = (0..e.n_vars).map(|_| Life { first: Some(e.code), accesses: 1, last: 0 }).collect();
        let mut konst = vec![None; e.n_vals as usize];
        let mut loops = Vec::<(u32, u32)>::new();
        let mut depth = 0;
        for (s, step) in steps.iter().enumerate() {
            let i = match *step {
                Step::Op(i) if !dead[s] => i,
                Step::EndLoop => { loops.pop(); continue; }
                _ => continue,
            };
            let a = i.args;
            // Folded constants are constants too: they get no slot and no code.
            if pure(&i) && matches!(types[a[0] as usize], TY_I32 | TY_BOOL) {
                konst[a[0] as usize] = match folded[a[0] as usize] {
                    Some(Folded::K(k)) => Some(k),
                    _ => (i.op == OP_CONST).then_some(a[1]),
                };
            }
            unsupported(m, k, &steps, &dead, &folded, s, o)?;
            let mut used = |v: u32| {
                let life = &mut lives[v as usize];
                let def = life.first.expect("verified definition before use");
                let last = loops.iter().filter(|(start, _)| def < *start)
                    .map(|(_, end)| end - 1).max().unwrap_or(i.at).max(i.at);
                life.last = life.last.max(last);
                life.accesses += 1;
            };
            match i.op {
                OP_GET => vars[a[1] as usize].accesses += 1,
                OP_SET => vars[a[0] as usize].accesses += 1,
                _ => {}
            }
            for v in reads(&i, &folded) { used(v); }
            let mut define = |v: u32| lives[v as usize] = Life { first: Some(i.at), last: i.at, accesses: 1 };
            if (OP_CONST..=OP_FMA).contains(&i.op) { define(a[0]); }
            if i.op == OP_PORT { for &v in &a[4 + a[2] as usize..] { define(v); } }
            if i.op == OP_LOOP { loops.push((i.at, i.end)); depth = depth.max(loops.len()); }
        }
        // A get can use its var's slot until the next live set. Life::last already
        // extends through every loop containing a use but not the get, so a set
        // later in such a loop kills the home too (including in a nested branch).
        // Step order is conservative across branches, caps and control transfers.
        let mut var_homes = vec![None; lives.len()];
        let mut next_set = vec![None; vars.len()];
        for (s, step) in steps.iter().enumerate().rev() {
            let Step::Op(i) = step else { continue };
            if dead[s] { continue; }
            let a = i.args;
            if i.op == OP_SET { next_set[a[0] as usize] = Some(i.at); }
            if i.op == OP_GET {
                let (v, var) = (a[0] as usize, a[1] as usize);
                // A set at the last use reads its operand before writing the var.
                if !next_set[var].is_some_and(|at| at < lives[v].last) {
                    var_homes[v] = Some(var);
                    // Replace the get's one var read with one read per live use.
                    vars[var].accesses += lives[v].accesses - 2;
                }
            }
        }
        // Only dead, Imm and Var steps may separate the compare, an optional !,
        // and its if. Every other step (including control boundaries) is a barrier.
        let mut next_code = vec![None; steps.len()];
        let mut next = None;
        for (s, step) in steps.iter().enumerate().rev() {
            next_code[s] = next;
            if dead[s] { continue; }
            if let Step::Op(i) = step {
                if pure(i) && (konst[i.args[0] as usize].is_some() || var_homes[i.args[0] as usize].is_some()) { continue; }
            }
            next = Some(s);
        }
        let mut flags = vec![None; lives.len()];
        for (s, step) in steps.iter().enumerate() {
            let Step::Op(i) = *step else { continue };
            if dead[s] || i.op != OP_CMP_I || uses[i.args[0] as usize] != 1 { continue; }
            let (_, y, sub) = compare_operands(i, &konst);
            let Some((bit, set)) = compare_flags(sub, konst[y as usize], compare_narrow(i, &konst, &ranges.values)) else { continue };
            let Some(n) = next_code[s] else { continue };
            let Step::Op(consumer) = steps[n] else { continue };
            let v = i.args[0];
            let not = consumer.op == OP_UN && consumer.sub == UN_NOT_B && consumer.args[1] == v
                && uses[consumer.args[0] as usize] == 1;
            let (condition, branch) = if not {
                let Some(n) = next_code[n] else { continue };
                let Step::Op(branch) = steps[n] else { continue };
                (consumer.args[0], branch)
            } else { (v, consumer) };
            if branch.op != OP_IF || branch.args[0] != condition { continue; }
            flags[v as usize] = Some((bit, set));
            if not { flags[condition as usize] = Some((bit, !set)); }
        }
        let mut acc = vec![None; lives.len()];
        for (s, step) in steps.iter().enumerate() {
            let Step::Op(i) = *step else { continue };
            if dead[s] { continue; }
            let v = if pure(&i) { i.args[0] }
                else if i.op == OP_PORT && i.args[3] == 1 { i.args[4 + i.args[2] as usize] }
                else { continue };
            let n = v as usize;
            if uses[n] != 1 || konst[n].is_some() || var_homes[n].is_some() || flags[n].is_some() { continue; }
            let Some(next) = next_code[s] else { continue };
            let Step::Op(consumer) = steps[next] else { continue };
            // Flags are decided first, including the ! that only changes their sense. A Flags
            // compare still loads and compares its operands as any compare does, so it takes an
            // Acc; a Flags `!` emits nothing and its operand is a Flags compare, never an Acc.
            if pure(&consumer) && consumer.op != OP_CMP_I && flags[consumer.args[0] as usize].is_some() { continue; }
            let a = consumer.args;
            let takes = if let Some(Folded::Sel { c, .. }) = if pure(&consumer) { folded[a[0] as usize] } else { None } {
                c == v
            } else { match consumer.op {
                OP_SET | OP_UN | OP_WRAP => a[1] == v,
                OP_STORE => a[2] == v || (a[3] == v && (a[2] == NONE || konst[a[2] as usize].is_some())),
                OP_LOAD => a[3] == v,
                OP_BIN | OP_CMP_I | OP_FMA => a[1] == v || a[2] == v,
                OP_IF => a[0] == v,
                OP_SEL => a[1..4].contains(&v),
                OP_PORT => {
                    let args = &a[4..4 + a[2] as usize];
                    args.first() == Some(&v) || args.get(1) == Some(&v)
                        || (a[0] == u32::from(ports::SHIFT_OUT) && args.get(2) == Some(&v))
                }
                _ => false,
            } };
            if takes { acc[n] = Some(next); }
        }
        let mut order: Vec<_> = lives.iter().enumerate()
            .filter(|&(v, _)| konst[v].is_none() && var_homes[v].is_none() && flags[v].is_none() && acc[v].is_none())
            .filter_map(|(v, l)| l.first.map(|at| (at, v))).collect();
        order.sort_unstable();
        let mut colours = Vec::<Colour>::new();
        let mut value_colours = vec![None; lives.len()];
        for (at, v) in order {
            let life = &lives[v];
            let bytes = shapes[v];
            let colour = colours.iter().position(|c| c.size == bytes && c.last <= at).unwrap_or_else(|| {
                colours.push(Colour { size: bytes, last: 0, accesses: 0, first: at, offset: 0 });
                colours.len() - 1
            });
            colours[colour].last = life.last;
            colours[colour].accesses += life.accesses;
            value_colours[v] = Some(colour);
        }
        let val_count = colours.len();
        let slot_bytes: u32 = colours.iter().map(|c| u32::from(c.size)).sum();
        for (v, life) in vars.iter().enumerate() {
            colours.push(Colour { size: var_shapes[v], last: u32::MAX,
                accesses: life.accesses, first: e.code, offset: 0 });
        }
        let mut order: Vec<_> = (0..colours.len()).collect();
        order.sort_by_key(|&c| (std::cmp::Reverse(colours[c].accesses), colours[c].first, c));
        let mut bytes = 0u32;
        for c in order {
            colours[c].offset = u16::try_from(bytes).map_err(|_| error(m, k, e.body + 1, "slot RAM exceeds 65535 bytes"))?;
            bytes += u32::from(colours[c].size);
        }
        let var_bytes = bytes - slot_bytes;
        let frame = bytes;
        let frame_bytes = e.frame_words.checked_mul(4).ok_or_else(|| error(m, k, e.body + 1, "frame RAM size overflow"))?;
        let counters = frame.checked_add(frame_bytes).ok_or_else(|| error(m, k, e.body + 1, "scratch RAM size overflow"))?;
        let total = u64::from(counters) + depth as u64 * 4;
        if total > 2048 { return Err(error(m, k, e.body + 1, format!("scratch needs {total} bytes; the 328P has 2048 bytes of RAM"))); }
        let slot = |c: usize| Slot { offset: colours[c].offset, size: colours[c].size };
        Ok(Self {
            values: value_colours.into_iter().enumerate().map(|(v, c)| match (konst[v], var_homes[v], flags[v]) {
                (Some(k), _, _) => Home::Imm(k),
                (_, Some(var), _) => Home::Var(slot(val_count + var)),
                (_, _, Some((bit, set))) => Home::Flags { bit, set },
                _ if acc[v].is_some() => Home::Acc { size: shapes[v], consumer: acc[v].unwrap() },
                _ => Home::Slot(c.map_or(Slot::default(), slot)),
            }).collect(),
            steps, dead, folded, shapes, value_ranges: ranges.values.clone(), state_ranges: state.to_vec(),
            vars: (val_count..colours.len()).map(slot).collect(), zero_vars, konst,
            frame: frame as u16, counters: counters as u16, bytes: total as u16,
            slot_bytes: slot_bytes as u16, var_bytes: var_bytes as u16, frame_bytes: frame_bytes as u16,
            words: Cell::new(0), start: Cell::new(0), ops: RefCell::new(Vec::new()),
            branches: RefCell::new(Vec::new()),
            regions: RefCell::new(Vec::new()),
        })
    }
}

// A live instruction E1 cannot emit; a float has no code on the 328P.
fn unsupported(m: &Module<'_>, k: usize, steps: &[Step<'_>], dead: &[bool], folded: &[Option<Folded>],
    s: usize, o: &Options) -> Result<(), Refusal> {
    let Step::Op(i) = steps[s] else { unreachable!("refusals are per instruction") };
    let integer = pure(&i) && m.ty(k, i.args[0], false) == TY_I32 && folded[i.args[0] as usize].is_some();
    if let Some(reason) = float_use(m, k, i).filter(|_| !integer) {
        // Where the value is used: the first port call it flows into.
        let mut tainted = vec![false; m.entries()[k].n_vals as usize];
        if pure(&i) { tainted[i.args[0] as usize] = true; }
        let mut place = String::new();
        for (t, step) in steps.iter().enumerate().skip(s + 1) {
            let Step::Op(j) = step else { continue };
            if dead[t] || !reads(j, folded).iter().any(|&v| tainted[v as usize]) { continue; }
            if j.op == OP_PORT {
                let name = ports::PORTS[j.args[0] as usize].name;
                place = match o.sites.get(j.args[1] as usize) {
                    Some((line, col)) => format!("; it reaches `{name}` at line {line}:{col}"),
                    None => format!("; it reaches `{name}` at port site {}", j.args[1]),
                };
                break;
            }
            if pure(j) { tainted[j.args[0] as usize] = true; }
        }
        return Err(error(m, k, i.at, format!("{reason}, which the 328P cannot run{place}; give the value an i32 type")));
    }
    let i = &i;
    if matches!(i.op, OP_LOAD | OP_STORE) {
        let extent = i.args[if i.op == OP_LOAD { 2 } else { 1 }];
        if extent > 16383 { return Err(error(m, k, i.at, format!("LOAD/STORE extent {extent} exceeds 16383 words"))); }
    }
    if i.op == OP_PORT && i.args[0] == u32::from(ports::PROBE) {
        return Err(error(m, k, i.at, "`probe` is not available on the 328P yet"));
    }
    if i.op == OP_PORT && i.args[1] > u16::MAX as u32 {
        return Err(error(m, k, i.at, format!("{} site {} exceeds the watch protocol's 16-bit site", ports::PORTS[i.args[0] as usize].name, i.args[1])));
    }
    Ok(())
}

// A float in a live instruction: its result, a stored value, or a float op.
fn float_use(m: &Module<'_>, k: usize, i: Instruction<'_>) -> Option<String> {
    let name = |t: u8| if t == TY_F64 { "f64" } else { "f32" };
    let is_float = |t: u8| matches!(t, TY_F32 | TY_F64);
    let a = i.args;
    let what = match i.op {
        OP_CONST => "a constant", OP_CONST_D => "a constant", OP_GET => "a variable read",
        OP_UN => "a unary operation", OP_BIN => "an operation", OP_SEL => "a select (if/else value)",
        OP_LOAD => "a load", OP_SET => "a variable write", OP_STORE => "a store",
        OP_CMP_F | OP_CMP_D => "a comparison", _ => "an instruction",
    };
    let value = |t: u8| match (i.op, t) {
        (OP_CONST, TY_F32) => format!(" ({})", f32::from_bits(a[1])),
        _ => String::new(),
    };
    let ty = |v: u32| m.ty(k, v, false);
    if pure(&i) && is_float(ty(a[0])) {
        return Some(format!("{what}{} computes in {}", value(ty(a[0])), name(ty(a[0]))));
    }
    match i.op {
        OP_CONST_D => Some(format!("{what} is f64")),
        OP_SET | OP_STORE => {
            let t = ty(a[if i.op == OP_SET { 1 } else { 3 }]);
            is_float(t).then(|| format!("{what} stores {}", name(t)))
        }
        OP_CMP_F => Some(format!("{what} compares f32")),
        OP_CMP_D => Some(format!("{what} compares f64")),
        OP_UN if !matches!(i.sub, UN_NEG_I | UN_NOT_B) => Some(format!("{what} converts from a float")),
        OP_BIN if !matches!(i.sub, BIN_ADD_I..=BIN_OR_B) => Some(format!("{what} computes in a float")),
        _ => None,
    }
}

struct Loop { body: Label, next: Label, exit: Label, slot: Slot, start: u32, cap: u32 }
struct Branch { otherwise: Label, end: Label, start: u32, split: u32 }
#[derive(Clone, Copy)]
struct InlinePin { pin: u8, port: u8, input: u8, bit: u8, role: Role }

// One predicate for emission and linking: a constant pin declared with the call's role.
fn inline_pin(plan: &Plan<'_>, pins: &[InlinePin], a: &[u32]) -> Option<InlinePin> {
    let port = a[0] as u16;
    if !matches!(port, ports::PIN_WRITE | ports::PIN_READ | ports::ADC_READ) { return None; }
    let pin = plan.konst[*a.get(4)? as usize]?;
    pins.iter().copied().find(|p| u32::from(p.pin) == pin && match p.role {
        Role::Output => port == ports::PIN_WRITE, Role::Input { .. } => port == ports::PIN_READ,
        Role::Analog => port == ports::ADC_READ, Role::Pwm(_) => false,
    })
}

struct Emitter<'a, 'p, 'm> {
    a: &'a mut Asm, s: &'a Stubs, plan: &'p Plan<'m>, pins: &'p [InlinePin], watch: Level,
    acc: Option<LiveAcc>,
}

struct LiveAcc { value: u32, size: u8, consumer: usize, read: bool }

impl Emitter<'_, '_, '_> {
    // Except for Acc and Flags, values live in SRAM between operations. Only
    // Y (scratch base) and r2 (zero) persist: helpers preserve both. r0/r1,
    // r24..r27 and Z are operation-local, including addresses and port sites.
    // A Flags compare keeps SREG until its next emitting step, the if.
    // Both addressing forms take two cycles per byte. Bool uses zero-extended quads.
    fn insn(&mut self, i: Insn) {
        // Check every register-writing instruction, including ones outside load/value.
        // Reading the Acc into A itself is a read too, even though it emits no MOV.
        let (reg, bytes) = match i {
            Add(r, _) | Adc(r, _) | Sub(r, _) | Sbc(r, _) | And(r, _) | Or(r, _) |
            Eor(r, _) | Mov(r, _) | Subi(r, _) | Sbci(r, _) | Andi(r, _) | Ori(r, _) |
            Ldi(r, _) | Com(r) | Neg(r) | Swap(r) | Inc(r) | Dec(r) | Asr(r) | Lsr(r) |
            Ror(r) | Bld(r, _) | In(r, _) | Ld(r, _) | Ldd(r, _, _) | Lds(r, _) |
            LpmZ(r) | LpmZInc(r) | Pop(r) => (r, 1),
            Movw(r, _) | Adiw(r, _) | Sbiw(r, _) => (r, 2),
            Mul(..) | Muls(..) | Mulsu(..) | Fmul(..) | Fmuls(..) | Fmulsu(..) => (0, 2),
            Lpm => (0, 1),
            Cp(..) | Cpc(..) | Cpse(..) | Cpi(..) | Bset(_) | Bclr(_) | Bst(..) |
            Sbrc(..) | Sbrs(..) | Sbic(..) | Sbis(..) | Sbi(..) | Cbi(..) | Out(..) |
            Brbs(..) | Brbc(..) | Rjmp(_) | Rcall(_) | Jmp(_) | Call(_) | Ijmp | Icall |
            Ret | Reti | St(..) | Std(..) | Sts(..) | Push(_) | Nop | Sleep | Wdr | Break => (0, 0),
        };
        if let Some(acc) = &self.acc {
            assert!(acc.read || !(reg..reg + bytes).any(|r| (16..16 + acc.size).contains(&r)),
                "Acc value {} overwritten before its read by step {}: {i:?}", acc.value, acc.consumer);
        }
        self.a.emit(i);
    }
    fn is_acc(&self, v: u32) -> bool { matches!(self.plan.values[v as usize], Home::Acc { .. }) }
    fn read_acc(&mut self, v: u32) {
        if !self.is_acc(v) { return; }
        let acc = self.acc.as_mut().expect("an Acc operand must still be live");
        assert_eq!(acc.value, v, "only the live Acc can be read");
        acc.read = true;
    }
    fn size(&self, v: u32) -> u8 {
        self.plan.shapes[v as usize]
    }
    fn use_(&mut self, slot: Slot, quad: u8) { self.use_bytes(slot, quad, 4); }
    fn use_bytes(&mut self, slot: Slot, quad: u8, bytes: u8) {
        for byte in 0..bytes {
            self.load(quad + byte, if byte < slot.size { ByteSrc::Mem(slot.offset + u16::from(byte)) } else { ByteSrc::Zero });
        }
    }
    fn def(&mut self, slot: Slot, quad: u8) {
        for byte in 0..slot.size { self.store(slot.offset + u16::from(byte), quad + byte); }
    }
    /// Store `r` to a scratch byte: Y+q reaches the first 64.
    fn store(&mut self, offset: u16, r: u8) {
        self.insn(if offset < 64 { Std(true, offset as u8, r) } else { Sts(self.s.scratch + offset, r) });
    }
    fn flush(&mut self) {
        if let Some(acc) = &self.acc { assert!(acc.read, "Acc value {} crosses a control transfer or call unread", acc.value); }
    }
    /// Byte `byte` of value `v` as an operand.
    fn src(&self, v: u32, byte: u8) -> ByteSrc {
        match self.plan.values[v as usize] {
            Home::Imm(k) => ByteSrc::Imm(k.to_le_bytes()[byte as usize]),
            Home::Acc { size, .. } if byte < size => ByteSrc::Reg(16 + byte),
            Home::Acc { .. } => ByteSrc::Zero,
            Home::Slot(slot) | Home::Var(slot) if byte < slot.size => ByteSrc::Mem(slot.offset + u16::from(byte)),
            Home::Slot(_) | Home::Var(_) => ByteSrc::Zero,
            Home::Flags { .. } => unreachable!("a flags home is consumed only by its if"),
        }
    }
    /// The constant a value is, if it is one.
    fn imm_of(&self, v: u32) -> Option<u32> {
        match self.plan.values[v as usize] { Home::Imm(k) => Some(k), _ => None }
    }
    /// The slot of a stored result (never a constant or a variable home).
    fn slot(&self, v: u32) -> Slot {
        match self.plan.values[v as usize] {
            Home::Slot(slot) => slot,
            Home::Imm(_) => unreachable!("a constant has no slot"),
            Home::Var(_) => unreachable!("a variable home is never stored as a result"),
            Home::Flags { .. } => unreachable!("a flags home has no slot"),
            Home::Acc { .. } => unreachable!("an Acc home has no slot"),
        }
    }
    /// Load one operand byte into `r` (r16 and up: LDI needs the upper half).
    fn load(&mut self, r: u8, src: ByteSrc) {
        if src == ByteSrc::Reg(r) { return; }
        self.insn(match src {
            ByteSrc::Imm(x) => Ldi(r, x),
            ByteSrc::Reg(src) => Mov(r, src),
            ByteSrc::Zero => Mov(r, 2),
            ByteSrc::Mem(offset) if offset < 64 => Ldd(r, true, offset as u8),
            ByteSrc::Mem(offset) => Lds(r, self.s.scratch + offset),
        });
    }
    fn value(&mut self, v: u32, quad: u8) { self.value_bytes(v, quad, 4); }
    /// The low `bytes` bytes of `v`; an op whose result is that narrow needs no more.
    fn value_bytes(&mut self, v: u32, quad: u8, bytes: u8) {
        let mut byte = 0;
        while byte < bytes {
            let src = self.src(v, byte);
            if let ByteSrc::Reg(r) = src {
                if byte + 1 < bytes && r % 2 == 0 && (quad + byte) % 2 == 0
                    && self.src(v, byte + 1) == ByteSrc::Reg(r + 1) {
                    if r != quad + byte { self.insn(Movw(quad + byte, r)); }
                    byte += 2; continue;
                }
            }
            self.load(quad + byte, src); byte += 1;
        }
        self.read_acc(v);
    }
    /// A register-form operand uses zero and register bytes directly.
    fn register(&mut self, src: ByteSrc, temp: u8) -> u8 {
        match src {
            ByteSrc::Zero | ByteSrc::Imm(0) => 2,
            ByteSrc::Reg(r) => r,
            _ => { self.load(temp, src); temp }
        }
    }
    /// Put A in place, saving a noncommutative B before any load can overwrite it.
    fn operands(&mut self, mut x: u32, mut y: u32, commute: bool, bytes: u8) -> (u32, bool) {
        if commute && (self.is_acc(y) || (self.imm_of(x).is_some() && self.imm_of(y).is_none())) {
            std::mem::swap(&mut x, &mut y);
        }
        let saved = self.is_acc(y);
        if saved { self.value_bytes(y, 20, bytes); }
        self.value_bytes(x, 16, bytes);
        (y, saved)
    }
    fn rhs(&self, v: u32, byte: u8, saved: bool) -> ByteSrc {
        if saved && matches!(self.src(v, byte), ByteSrc::Reg(_)) { ByteSrc::Reg(20 + byte) }
        else { self.src(v, byte) }
    }
    fn bin_register(&mut self, sub: u32, v: u32, saved: bool, bytes: u8) {
        for byte in 0..bytes {
            let x = 16 + byte;
            let y = self.register(self.rhs(v, byte, saved), 20 + byte);
            if y == 2 && matches!(sub, BIN_OR_I | BIN_OR_B | BIN_XOR_I) { continue; }
            self.insn(match sub {
                BIN_ADD_I => if byte == 0 { Add(x, y) } else { Adc(x, y) },
                BIN_SUB_I => if byte == 0 { Sub(x, y) } else { Sbc(x, y) },
                BIN_AND_I | BIN_AND_B => if y == 2 { Mov(x, 2) } else { And(x, y) },
                BIN_OR_I | BIN_OR_B => Or(x, y), _ => Eor(x, y),
            });
        }
    }
    /// A's low `bytes` bytes op= k, in immediate forms.
    fn bin_imm(&mut self, sub: u32, k: u32, bytes: u8) {
        let kb = k.to_le_bytes();
        if matches!(sub, BIN_ADD_I | BIN_SUB_I) {
            // x + k is x - (-k). Low zero bytes subtract nothing and borrow nothing.
            let n = if sub == BIN_ADD_I { k.wrapping_neg() } else { k }.to_le_bytes();
            let Some(first) = (0..bytes).find(|&b| n[b as usize] != 0) else { return };
            for byte in first..bytes {
                let (r, m) = (16 + byte, n[byte as usize]);
                self.insn(if byte == first { Subi(r, m) } else { Sbci(r, m) });
            }
            return;
        }
        for byte in 0..bytes {
            let (r, m) = (16 + byte, kb[byte as usize]);
            match (sub, m) {
                (BIN_AND_I | BIN_AND_B, 0xff) | (BIN_OR_I | BIN_OR_B | BIN_XOR_I, 0) => {}
                (BIN_AND_I | BIN_AND_B, 0) => self.insn(Mov(r, 2)),
                (BIN_AND_I | BIN_AND_B, m) => self.insn(Andi(r, m)),
                (BIN_OR_I | BIN_OR_B, 0xff) => self.insn(Ldi(r, 0xff)),
                (BIN_OR_I | BIN_OR_B, m) => self.insn(Ori(r, m)),
                (_, 0xff) => self.insn(Com(r)),
                (_, m) => { self.insn(Ldi(20 + byte, m)); self.insn(Eor(r, 20 + byte)); }
            }
        }
    }
    fn result(&mut self, v: u32) {
        if !self.is_acc(v) { let slot = self.slot(v); self.def(slot, 16); }
    }
    fn imm(&mut self, quad: u8, value: u32) { self.imm_bytes(quad, value, 4); }
    fn imm_bytes(&mut self, quad: u8, value: u32, bytes: u8) {
        for (byte, value) in value.to_le_bytes().into_iter().take(bytes as usize).enumerate() { self.insn(Ldi(quad + byte as u8, value)); }
    }
    fn word(&mut self, quad: u8, value: u16) {
        self.insn(Ldi(quad, value as u8)); self.insn(Ldi(quad + 1, (value >> 8) as u8));
    }
    fn helper(&mut self, helper: Helper) {
        self.flush();
        self.a.jump(Target::FarCall, helper.label(&self.s.helpers));
    }
    fn cond(&mut self, bit: u8, set: bool, dest: Label) {
        let skip = self.a.label();
        self.a.jump(if set { Target::Brbc(bit) } else { Target::Brbs(bit) }, skip);
        self.a.jump(Target::Far, dest);
        self.a.bind(skip);
    }
    // Exactly three cycles; LDI preserves the comparison flags.
    fn boolean(&mut self, reg: u8, bit: u8, set: bool) {
        self.insn(Ldi(reg, 1));
        self.insn(if set { Brbs(bit, 1) } else { Brbc(bit, 1) });
        self.insn(Ldi(reg, 0));
    }
    fn compare(&mut self, a: u8, b: u8) {
        self.insn(Cp(a, b));
        for byte in 1..4 { self.insn(Cpc(a + byte, b + byte)); }
    }
    /// `if c { t } else { e }` for constants: e's bytes, with c's mask XORed into the bytes where
    /// t and e differ. Arms differing only in bit 0 (`sel(c, 1, 0)` is c, `sel(c, 0, 1)` is
    /// c ^ 1) take c itself, a bool being 0 or 1. Constant cycles, no branch.
    fn select_const(&mut self, c: u32, t: u32, e: u32, v: u32) {
        let size = self.size(v);
        let (eb, db) = (e.to_le_bytes(), (t ^ e).to_le_bytes());
        if t ^ e == 1 {
            self.value_bytes(c, 16, 1);
            if eb[0] != 0 { self.insn(Ldi(20, eb[0])); self.insn(Eor(16, 20)); }
            for byte in 1..size { self.load(16 + byte, ByteSrc::Imm(eb[byte as usize])); }
        } else {
            self.value_bytes(c, 24, 1); self.insn(Neg(24));
            for byte in 0..size {
                let r = 16 + byte;
                self.load(r, if eb[byte as usize] == 0 { ByteSrc::Zero } else { ByteSrc::Imm(eb[byte as usize]) });
                match db[byte as usize] {
                    0 => {}
                    0xff => self.insn(Eor(r, 24)),
                    d => { self.insn(Ldi(20, d)); self.insn(And(20, 24)); self.insn(Eor(r, 20)); }
                }
            }
        }
        self.result(v);
    }
    fn mask_select(&mut self, mask: u8, yes: u8, no: u8, size: u8) {
        for byte in 0..size {
            self.insn(Eor(yes + byte, no + byte));
            self.insn(And(yes + byte, mask));
            self.insn(Eor(yes + byte, no + byte));
        }
    }
    fn emit(&mut self) {
        self.word(28, self.s.scratch);
        for (&slot, &zero) in self.plan.vars.iter().zip(&self.plan.zero_vars) {
            if !zero { continue; }
            for byte in 0..slot.size { self.store(slot.offset + u16::from(byte), 2); }
        }
        if self.plan.frame_bytes != 0 {
            self.word(30, self.s.scratch + self.plan.frame);
            self.word(24, self.plan.frame_bytes);
            let again = self.a.label(); self.a.bind(again);
            let start = self.a.here();
            self.insn(St(Ptr::ZInc, 2)); self.insn(Sbiw(24, 1));
            self.cond(1, false, again);
            self.plan.regions.borrow_mut().push(Region::Repeat {
                body: start..self.a.here(), count: u32::from(self.plan.frame_bytes),
            });
        }
        let mut loops = Vec::<Loop>::new();
        let mut branches = Vec::<Branch>::new();
        let mut skipped = 0;
        let plan = self.plan;
        for (s, step) in plan.steps.iter().enumerate() {
            if plan.dead[s] { continue; }
            if skipped != 0 {
                match step { Step::Op(i) if i.op == OP_LOOP => skipped += 1,
                    Step::EndLoop => skipped -= 1, _ => {} }
                continue;
            }
            let quiet = matches!(step, Step::Op(i) if pure(i)
                && matches!(plan.values[i.args[0] as usize], Home::Imm(_) | Home::Var(_)));
            if !quiet {
                if let Some(acc) = &self.acc {
                    assert_eq!(acc.consumer, s, "Acc value {} reached a different emitting step", acc.value);
                }
            }
            match *step {
                Step::Else => {
                    self.flush();
                    let b = branches.last_mut().unwrap();
                    self.a.jump(Target::Far, b.end); self.a.bind(b.otherwise);
                    b.split = self.a.here();
                }
                Step::EndIf => {
                    self.flush();
                    let b = branches.pop().unwrap(); self.a.bind(b.end);
                    plan.regions.borrow_mut().push(Region::Branch {
                        then: b.start..b.split, otherwise: b.split..self.a.here(),
                    });
                }
                Step::EndLoop => {
                    self.flush();
                    let l = loops.pop().unwrap(); self.a.bind(l.next);
                    if l.slot.size == 1 {
                        self.load(16, ByteSrc::Mem(l.slot.offset));
                        self.insn(Subi(16, 1)); self.def(l.slot, 16);
                        self.cond(0, false, l.body); // borrow: zero was the last iteration
                    } else {
                        self.use_(l.slot, 16);
                        self.insn(Mov(24, 16)); for r in 17..20 { self.insn(Or(24, r)); }
                        self.cond(1, true, l.exit);
                        self.insn(Subi(16, 1)); for r in 17..20 { self.insn(Sbci(r, 0)); }
                        self.def(l.slot, 16); self.a.jump(Target::Far, l.body);
                    }
                    self.a.bind(l.exit);
                    plan.regions.borrow_mut().push(Region::Repeat { body: l.start..self.a.here(), count: l.cap });
                }
                Step::Op(i) => {
                    let a = i.args;
                    let start = self.a.here();
                    match i.op {
                        OP_IF => {
                            let (bit, set) = match self.plan.values[a[0] as usize] {
                                Home::Flags { bit, set } => (bit, set),
                                _ => {
                                    self.value_bytes(a[0], 16, 1); // a bool: its low byte
                                    self.insn(Cpi(16, 0));
                                    (1, false)
                                }
                            };
                            self.flush();
                            let mut b = Branch { otherwise: self.a.label(), end: self.a.label(), start: 0, split: 0 };
                            self.plan.branches.borrow_mut().push((self.a.here(), i.at));
                            self.cond(bit, !set, b.otherwise);
                            b.start = self.a.here(); branches.push(b);
                        }
                        OP_LOOP => {
                            if a[0] == 0 { skipped = 1; continue; }
                            self.flush();
                            let mut l = Loop { body: self.a.label(), next: self.a.label(), exit: self.a.label(), start: 0, cap: a[0],
                                // Keep four bytes per level; narrow and wide siblings share the level safely.
                                slot: Slot { offset: self.plan.counters + loops.len() as u16 * 4, size: if a[0] <= 256 { 1 } else { 4 } } };
                            if l.slot.size == 1 { self.insn(Ldi(16, (a[0] - 1) as u8)); }
                            else { self.imm(16, a[0] - 1); }
                            self.def(l.slot, 16); self.a.bind(l.body);
                            l.start = self.a.here(); loops.push(l);
                        }
                        OP_BREAK | OP_CONTINUE => {
                            self.flush();
                            let l = &loops[loops.len() - 1 - a[0] as usize];
                            self.a.jump(Target::Far, if i.op == OP_BREAK { l.exit } else { l.next });
                        }
                        _ => self.op(i),
                    }
                    let end = self.a.here();
                    if end > start { self.plan.ops.borrow_mut().push((start, end, i.at)); }
                }
            }
            if !quiet {
                if let Some(acc) = self.acc.take() {
                    assert!(acc.read, "step {s} did not read Acc value {}", acc.value);
                }
                if let Step::Op(i) = step {
                    let out = if pure(i) { Some(i.args[0]) }
                        else if i.op == OP_PORT && i.args[3] == 1 { Some(i.args[4 + i.args[2] as usize]) }
                        else { None };
                    if let Some(value) = out {
                        if let Home::Acc { size, consumer } = plan.values[value as usize] {
                            self.acc = Some(LiveAcc { value, size, consumer, read: false });
                        }
                    }
                }
            }
        }
        assert!(self.acc.is_none(), "an Acc cannot outlive its entry");
    }
    fn op(&mut self, i: Instruction<'_>) {
        let a = i.args;
        if pure(&i) {
            match self.plan.folded[a[0] as usize] {
                // Constants and selects materialize only the result shape.
                Some(Folded::K(_)) if self.imm_of(a[0]).is_some() => return,
                Some(Folded::K(k)) => { self.imm_bytes(16, k, self.size(a[0])); self.result(a[0]); return; }
                Some(Folded::Sel { c, t, e }) => { self.select_const(c, t, e, a[0]); return; }
                None => {}
            }
        }
        match i.op {
            OP_CONST if self.imm_of(a[0]).is_some() => {}
            OP_CONST => { self.imm_bytes(16, a[1], self.size(a[0])); self.result(a[0]); }
            OP_GET if matches!(self.plan.values[a[0] as usize], Home::Var(_)) => {}
            OP_GET => { self.use_bytes(self.plan.vars[a[1] as usize], 16, self.size(a[0])); self.result(a[0]); }
            OP_SET => {
                let var = self.plan.vars[a[0] as usize];
                if let Some(k) = self.imm_of(a[1]) {
                    // A constant goes straight to the var: zero bytes from r2.
                    for byte in 0..var.size {
                        let r = match k.to_le_bytes()[byte as usize] { 0 => 2, m => { self.insn(Ldi(16 + byte, m)); 16 + byte } };
                        self.store(var.offset + u16::from(byte), r);
                    }
                } else { self.value_bytes(a[1], 16, var.size); self.def(var, 16); }
            }
            OP_UN if matches!(self.plan.values[a[0] as usize], Home::Flags { .. }) => {}
            OP_UN => {
                let bytes = self.size(a[0]);
                self.value_bytes(a[1], 16, bytes);
                if i.sub == UN_NEG_I {
                    if bytes == 1 { self.insn(Neg(16)); }
                    else {
                        for r in 16..20 { self.insn(Com(r)); }
                        self.insn(Subi(16, 255)); for r in 17..20 { self.insn(Sbci(r, 255)); }
                    }
                } else { self.insn(Ldi(20, 1)); self.insn(Eor(16, 20)); }
                self.result(a[0]);
            }
            OP_BIN if matches!(i.sub, BIN_ADD_I | BIN_SUB_I | BIN_AND_I | BIN_OR_I | BIN_XOR_I | BIN_AND_B | BIN_OR_B) => {
                let bytes = self.size(a[0]);
                let (y, saved) = self.operands(a[1], a[2], i.sub != BIN_SUB_I, bytes);
                if let Some(k) = self.imm_of(y) {
                    self.bin_imm(i.sub, k, bytes);
                } else { self.bin_register(i.sub, y, saved, bytes); }
                self.result(a[0]);
            }
            OP_BIN => {
                let helper = self.plan.helper(i);
                let nonnegative = self.plan.value_ranges[a[1] as usize].lo >= 0;
                let bytes = if helper.is_some() { 4 } else { match i.sub {
                    BIN_SHR_I | BIN_SHR_UI => self.size(a[1]),
                    BIN_REM_I if !nonnegative => 4,
                    _ => self.size(a[0]),
                } };
                let (y, saved) = self.operands(a[1], a[2], i.sub == BIN_MUL_I, bytes);
                if let Some(helper) = helper {
                    for byte in 0..4 { self.load(20 + byte, self.rhs(y, byte, saved)); }
                    self.helper(helper);
                } else if i.sub == BIN_MUL_I {
                    self.load(20, self.rhs(y, 0, saved));
                    self.insn(Mul(16, 20)); self.insn(Mov(16, 0));
                } else if matches!(i.sub, BIN_SHL_I | BIN_SHR_I | BIN_SHR_UI) {
                    let amount = self.plan.konst[a[2] as usize].unwrap() & 31;
                    if bytes == 1 {
                        if amount >= 8 { self.insn(Mov(16, 2)); }
                        else { for _ in 0..amount { self.insn(if i.sub == BIN_SHL_I { Add(16, 16) } else { Lsr(16) }); } }
                    } else { self.shift(i.sub, amount); }
                } else if i.sub == BIN_REM_I {
                    let mask = self.plan.konst[a[2] as usize].unwrap() - 1;
                    if nonnegative {
                        for byte in 0..bytes { self.insn(Andi(16 + byte, mask.to_le_bytes()[byte as usize])); }
                    } else {
                        // Signed remainder by a positive power of two, including MIN:
                        // bias = (x >> 31) & mask; result = ((x + bias) & mask) - bias.
                        self.insn(Mov(24, 19)); self.insn(Add(24, 24)); self.insn(Sbc(24, 24));
                        for (byte, mask) in mask.to_le_bytes().into_iter().enumerate() {
                            let r = 20 + byte as u8;
                            self.insn(Mov(r, 24)); self.insn(Andi(r, mask));
                        }
                        self.insn(Add(16, 20)); for r in 17..20 { self.insn(Adc(r, r + 4)); }
                        for (byte, mask) in mask.to_le_bytes().into_iter().enumerate() {
                            self.insn(Andi(16 + byte as u8, mask));
                        }
                        self.insn(Sub(16, 20)); for r in 17..20 { self.insn(Sbc(r, r + 4)); }
                    }
                } else { unreachable!("checked inline op") }
                self.result(a[0]);
            }
            OP_FMA => {
                let (y, saved) = self.operands(a[1], a[2], true, 4);
                for byte in 0..4 { self.load(20 + byte, self.rhs(y, byte, saved)); }
                self.helper(self.plan.helper(i).expect("i32 FMA helper"));
                // An addend is re-read after mul32 clobbers B and temporaries, never Acc.
                if let Some(k) = self.imm_of(a[3]) { self.bin_imm(BIN_ADD_I, k, 4); }
                else { self.bin_register(BIN_ADD_I, a[3], false, 4); }
                self.result(a[0]);
            }
            OP_CMP_I => {
                // A constant compares as an immediate: on the right, mirroring the sense if it was
                // on the left. Against k, x <= k is x < k + 1 and x > k is x >= k + 1.
                let (x, y, sub) = compare_operands(i, &self.plan.konst);
                let narrow = compare_narrow(i, &self.plan.konst, &self.plan.value_ranges);
                let bytes = if narrow { 1 } else { 4 };
                let Some((bit, set)) = compare_flags(sub, self.imm_of(y), narrow) else {
                    if self.is_acc(x) { self.value_bytes(x, 16, 1); }
                    self.insn(Ldi(16, u8::from(sub == CMP_LE))); self.result(a[0]); return;
                };
                if let Some(k) = self.imm_of(y) {
                    let k = if matches!(sub, CMP_LE | CMP_GT) { k.wrapping_add(1) } else { k };
                    self.value_bytes(x, 16, bytes);
                    let kb = k.to_le_bytes();
                    self.insn(Cpi(16, kb[0]));
                    for byte in 1..bytes {
                        // LDI leaves the flags of the compare chain alone.
                        match kb[byte as usize] {
                            0 => self.insn(Cpc(16 + byte, 2)),
                            m => { self.insn(Ldi(20 + byte, m)); self.insn(Cpc(16 + byte, 20 + byte)); }
                        }
                    }
                } else {
                    let (y, saved) = self.operands(x, y, false, bytes);
                    for byte in 0..bytes {
                        let r = self.register(self.rhs(y, byte, saved), 20 + byte);
                        let (x, y) = if matches!(sub, CMP_LE | CMP_GT) { (r, 16 + byte) } else { (16 + byte, r) };
                        self.insn(if byte == 0 { Cp(x, y) } else { Cpc(x, y) });
                    }
                }
                if !matches!(self.plan.values[a[0] as usize], Home::Flags { .. }) {
                    self.boolean(16, bit, set); self.result(a[0]);
                }
            }
            OP_SEL if self.imm_of(a[2]).is_some() && self.imm_of(a[3]).is_some() => {
                self.select_const(a[1], self.imm_of(a[2]).unwrap(), self.imm_of(a[3]).unwrap(), a[0]);
            }
            OP_SEL => {
                let bytes = self.size(a[0]);
                if self.is_acc(a[3]) { self.value_bytes(a[3], 20, bytes); }
                if self.is_acc(a[2]) { self.value_bytes(a[2], 16, bytes); }
                self.value_bytes(a[1], 24, 1);
                if !self.is_acc(a[2]) { self.value_bytes(a[2], 16, bytes); }
                if !self.is_acc(a[3]) { self.value_bytes(a[3], 20, bytes); }
                self.insn(Neg(24));
                self.mask_select(24, 16, 20, self.size(a[0]));
                self.result(a[0]);
            }
            OP_WRAP => {
                if let Some(helper) = self.plan.helper(i) {
                    self.value(a[1], 16);
                    self.imm(20, a[2]); self.helper(helper);
                    // Add len only for a negative remainder, without a variable-cost branch.
                    self.insn(Mov(24, 19)); self.insn(Add(24, 24)); self.insn(Sbc(24, 24));
                    self.imm(20, a[2]);
                    for r in 20..24 { self.insn(And(r, 24)); }
                    self.insn(Add(16, 20)); for r in 17..20 { self.insn(Adc(r, r + 4)); }
                } else {
                    let bytes = self.size(a[0]);
                    self.value_bytes(a[1], 16, bytes);
                    for byte in 0..bytes { self.insn(Andi(16 + byte, (a[2] - 1).to_le_bytes()[byte as usize])); }
                }
                self.result(a[0]);
            }
            OP_LOAD => {
                self.address(i.sub, a[1], a[2], a[3]);
                let size = self.size(a[0]);
                for byte in 0..size { self.insn(if i.sub == REGION_SHARED { LpmZInc(16 + byte) } else { Ld(16 + byte, Ptr::ZInc) }); }
                self.result(a[0]);
            }
            OP_STORE => {
                self.address(i.sub, a[0], a[1], a[2]);
                self.read_acc(a[3]);
                for byte in 0..store_width(i, &self.plan.konst, &self.plan.state_ranges) {
                    let r = self.register(self.src(a[3], byte), 16 + byte);
                    self.insn(St(Ptr::ZInc, r));
                }
            }
            OP_PORT => self.port(a),
            _ => unreachable!("checked E1 instruction"),
        }
    }
    fn shift(&mut self, op: u32, amount: u32) {
        let bytes = (amount / 8) as u8;
        if bytes != 0 {
            if op == BIN_SHL_I {
                for byte in (0..4).rev() { self.insn(Mov(16 + byte, if byte < bytes { 2 } else { 16 + byte - bytes })); }
            } else {
                let fill = if op == BIN_SHR_I {
                    self.insn(Mov(24, 19)); self.insn(Add(24, 24)); self.insn(Sbc(24, 24)); 24
                } else { 2 };
                for byte in 0..4 { self.insn(Mov(16 + byte, if byte + bytes < 4 { 16 + byte + bytes } else { fill })); }
            }
        }
        for _ in 0..amount % 8 {
            if op == BIN_SHL_I {
                self.insn(Add(16, 16)); for r in 17..20 { self.insn(Adc(r, r)); }
            } else {
                self.insn(if op == BIN_SHR_I { Asr(19) } else { Lsr(19) });
                for r in (16..19).rev() { self.insn(Ror(r)); }
            }
        }
    }
    fn address(&mut self, region: u32, base: u32, extent: u32, off: u32) {
        let fixed = fixed_index(off, extent, &self.plan.konst);
        let offset = 4 * (base + fixed.unwrap_or(0));
        let bytes = if fixed.is_none() { index_width(self.plan.value_ranges[off as usize], extent) } else { 0 };
        if fixed.is_none() {
            if bytes == 4 {
                self.value(off, 16); self.imm(20, extent - 1); self.compare(16, 20);
                // Borrow -> ff when offset < bound, else zero. Select min without a branch.
                self.insn(Sbc(24, 24)); self.mask_select(24, 16, 20, 4);
            } else {
                self.value_bytes(off, 16, self.size(off));
                if bytes == 2 && self.size(off) == 1 { self.load(17, ByteSrc::Zero); }
            }
            for _ in 0..2 {
                self.insn(Add(16, 16));
                if bytes != 1 { self.insn(Adc(17, 17)); }
            }
        }
        if region == REGION_SHARED {
            self.a.ldi_label(30, 31, self.s.shared);
            self.insn(Add(30, 30)); self.insn(Adc(31, 31));
            self.word(20, offset as u16);
            self.insn(Add(30, 20)); self.insn(Adc(31, 21));
        } else {
            let start = if region == REGION_STATE { self.s.state } else { self.s.scratch + self.plan.frame };
            self.word(30, start + offset as u16);
        }
        if fixed.is_none() { self.insn(Add(30, 16)); self.insn(Adc(31, if bytes == 1 { 2 } else { 17 })); }
    }
    fn port(&mut self, a: &[u32]) {
        let port = a[0] as u16;
        let args = &a[4..4 + a[2] as usize];
        if port == ports::SHIFT_OUT {
            if self.is_acc(args[2]) { self.value(args[2], 20); }
            self.value(args[3], 16);
            if !self.is_acc(args[2]) { self.value(args[2], 20); }
            self.word(24, a[1] as u16); self.flush();
            let pair = (self.plan.konst[args[0] as usize].unwrap() as u8,
                self.plan.konst[args[1] as usize].unwrap() as u8);
            let target = self.s.shifts.iter().find(|s| s.0 == pair).expect("linked shift pair").1;
            self.a.jump(Target::FarCall, target);
            return;
        }
        if let Some(pin) = inline_pin(self.plan, self.pins, a) {
            if port == ports::ADC_READ {
                let channel = fab_elec_mcu_avr::chip::adc_channel(u32::from(pin.pin)).unwrap();
                self.insn(Ldi(20, 0x40 | channel));
                self.flush();
                self.a.jump(Target::FarCall, self.s.adc_convert);
                for &v in &a[4 + a[2] as usize..] { self.result(v); }
                return;
            }
            if port == ports::PIN_WRITE {
                if let Some(level) = self.plan.konst[args[1] as usize] {
                    self.insn(if level != 0 { Sbi(pin.port, pin.bit) } else { Cbi(pin.port, pin.bit) });
                    if self.watch == Level::Ports { self.insn(Ldi(20, u8::from(level != 0))); }
                } else {
                    let bytes = self.size(args[1]);
                    self.value_bytes(args[1], 20, bytes);
                    // A one-byte level still needs to set Z before the branch.
                    self.insn(Or(20, if bytes == 1 { 20 } else { 21 }));
                    for r in 22..20 + bytes { self.insn(Or(20, r)); }
                    self.insn(Brbs(1, 2));
                    self.insn(Sbi(pin.port, pin.bit)); self.insn(Rjmp(1));
                    self.insn(Cbi(pin.port, pin.bit));
                    // Bit effects preserve the OR's flags: normalize only for the record.
                    if self.watch == Level::Ports { self.boolean(20, 1, false); }
                }
            } else {
                self.insn(Ldi(16, 0)); self.insn(Sbic(pin.input, pin.bit)); self.insn(Ldi(16, 1));
                for r in 17..20 { self.insn(Ldi(r, 0)); }
                if self.watch == Level::Ports { self.insn(Mov(20, 16)); }
            }
            if self.watch == Level::Ports {
                self.insn(Ldi(16, pin.pin)); self.word(24, a[1] as u16);
                self.flush();
                self.a.jump(Target::FarCall, if port == ports::PIN_WRITE { self.s.record_write } else { self.s.record_read });
            }
            for &v in &a[4 + a[2] as usize..] { self.result(v); }
            return;
        }
        let saved = args.get(1).is_some_and(|&v| self.is_acc(v));
        if saved { self.value(args[1], 20); }
        if let Some(&v) = args.first() { self.value(v, 16); }
        if args.len() > 1 && !saved { self.value(args[1], 20); }
        if port == ports::PIN_WRITE {
            self.insn(Or(20, 21)); self.insn(Or(20, 22)); self.insn(Or(20, 23));
            self.boolean(20, 1, false);
        }
        self.word(24, a[1] as u16); self.flush();
        let target = match port {
            ports::PIN_WRITE => self.s.write, ports::PIN_READ => self.s.read,
            ports::ADC_READ => self.s.adc,
            ports::EEPROM_READ => self.s.eeprom_read, ports::EEPROM_WRITE => self.s.eeprom_write,
            ports::PWM_DUTY => self.s.duty, ports::CLOCK_US => self.s.clock_us,
            ports::CLOCK_MS => self.s.clock_ms, ports::TIMER_STOP => self.s.timer_stop,
            ports::TIMER_START => self.s.timer_start, ports::TIMER_PERIOD => self.s.timer_period,
            ports::SERIAL_BYTE => self.s.serial_byte, ports::LOOP_CAP => self.s.loop_cap,
            _ => unreachable!("verified port"),
        };
        self.a.jump(Target::FarCall, target);
        for &v in &a[4 + a[2] as usize..] { self.result(v); }
    }
}
