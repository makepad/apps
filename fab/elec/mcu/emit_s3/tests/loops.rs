//! Fixed pixel-loop baselines through a real S3 image and the reference device.
use std::sync::Arc;

use fab_elec_mcu::compile;
use fab_elec_mcu_emit32::{bounds::bounds, control::{steps, Analysis, Step}};
use fab_elec_mcu_emit_s3::{
    build_image, chip, Board, BoardTime, Image, SlotOptions, SLOT_BASE, VOICEBOX,
};
use fab_elec_mcu_emu::{Lockstep, Machine, PcSet, Ran, Status, Tier, Until};
use fab_elec_mcu_parts::{BoardDesc, Netlist};
use fab_elec_mcu_rt::{
    format::{NONE, OP_LOAD, OP_LOOP, OP_STORE, REGION_BUF_BASE, REGION_BUF_LONG},
    module,
    sched::{Device, Fault, Hal, Role},
    serial, watch,
};
use fab_elec_mcu_s3::{CpuClock, PinsProfile, S3Machine, S3Profile, TimeProfile};
use fab_elec_mcu_xtensa::insn::{decode, target, Op};

// Seeding is separate from every measured handler. Keep the sizes and seeds
// fixed so later emitter changes can use exactly the same per-element baseline.
const SOURCE: &str = r#"
let g = surface(32, 32)
let colours = bytes(128)
let picture = surface(32, 32)
let src = surface(20, 20)
let scaled = surface(46, 56)
let a = surface(32, 32)
let b = surface(32, 32)
let blended = surface(32, 32)
let grid = bytes(32 * 32)
var alpha: i32 = 13

fn setup() {
    for cell in 0..64 {
        let colour = rgb((cell % 8) * 32, (cell / 8) * 32, cell * 4)
        colours[cell * 2] = colour & 255
        colours[cell * 2 + 1] = colour >> 8
    }
    for y in 0..20 {
        for x in 0..20 {
            src[y * 20 + x] = rgb(x * 12, y * 12, (x + y) * 6)
        }
    }
}

let seed_blend = after(ms(100), fn() {
    for y in 0..32 {
        for x in 0..32 {
            a[y * 32 + x] = rgb(x * 8, y * 8, (x + y) * 4)
            b[y * 32 + x] = rgb(248 - y * 8, 252 - x * 8, 248 - (x + y) * 4)
        }
    }
})

let seed_sand = after(ms(200), fn() {
    for y in 0..32 {
        for x in 0..32 {
            if y % 4 == 3 {
                if x % 8 == 1 || x % 8 == 3 {
                    grid[y * 32 + x] = 0
                } else {
                    grid[y * 32 + x] = 1
                }
            } else if y % 4 == 2 {
                grid[y * 32 + x] = 1
            } else if (x + y) % 3 == 0 {
                grid[y * 32 + x] = 1
            } else {
                grid[y * 32 + x] = 0
            }
        }
    }
})

let gradient = after(ms(300), fn() {
    for y in 0..32 {
        for x in 0..32 {
            g[y * 32 + x] = rgb(x * 8, y * 8, (x + y) * 4)
        }
    }
})

let refine = after(ms(400), fn() {
    for cell in 0..64 {
        let colour = colours[cell * 2] | (colours[cell * 2 + 1] << 8)
        let x = (cell % 8) * 4
        let y = (cell / 8) * 4
        for dy in 0..4 {
            for dx in 0..4 {
                picture[(y + dy) * 32 + x + dx] = colour
            }
        }
    }
})

let scale = after(ms(500), fn() {
    for y in 0..56 {
        for x in 0..46 {
            scaled[y * 46 + x] = src[(y * 20 / 56) * 20 + x * 20 / 46]
        }
    }
})

let blend = after(ms(600), fn() {
    for i in 0..1024 {
        let c1 = a[i]
        let c2 = b[i]
        let red = (((c1 >> 11) & 31) * (32 - alpha) + ((c2 >> 11) & 31) * alpha) >> 5
        let green = (((c1 >> 5) & 63) * (32 - alpha) + ((c2 >> 5) & 63) * alpha) >> 5
        let blue = ((c1 & 31) * (32 - alpha) + (c2 & 31) * alpha) >> 5
        blended[i] = (red << 11) | (green << 5) | blue
    }
})

let sand = after(ms(700), fn() {
    for row in 0..32 {
        let y = 31 - row
        for x in 0..32 {
            let i = y * 32 + x
            if y < 31 && grid[i] == 1 {
                let below = i + 32
                if grid[below] == 0 {
                    grid[below] = 1
                    grid[i] = 0
                } else if x > 0 && grid[below - 1] == 0 {
                    grid[below - 1] = 1
                    grid[i] = 0
                } else if x < 31 && grid[below + 1] == 0 {
                    grid[below + 1] = 1
                    grid[i] = 0
                }
            }
        }
    }
})
"#;

fn profile() -> S3Profile {
    // Explicit model inputs, matching buffers.rs; these are not chip timings.
    S3Profile {
        mac: [2, 0, 0, 0, 0, 1], pvt: fab_elec_mcu_s3::NO_PVT,
        flash_bytes: VOICEBOX.flash_bytes as usize,
        handoff_cpu: CpuClock::Xtal { div: 2 },
        handoff_mapped_pages: 64,
        entry: 0x4200_0408,
        seed: 0x5300,
       
        time: TimeProfile {
            systimer_first_long: true,
            rc_slow_hz: (136_000, 1),
            wdt_delay_sel: 0,
            swd_timeout_ticks: 432_000_000,
            swd_request_lead_ticks: 48_000_000,
            wdt_sync_cycles: 2,
            mwdt_rephase_on_feed: false,
            mwdt_rephase_on_config: true,
            wdt_restart_on_enable: true,
        },
        pins: PinsProfile {
            power_on_gpio0_high: true,
            usb_service_ticks: 480_000,
            usb_frame_phase_ticks: 0,
            input_sync_delay_ticks: 0,
            handoff_digital_pads_unheld: true,
            handoff_out: [0; 2],
            handoff_enable: [0; 2],
            handoff_pin: [0; 45],
            handoff_io_mux: [1 << 12; 45],
            handoff_out_sel: [256; 45],
            handoff_usb_tx: Vec::new(),
            handoff_usb_tx_flushed: false,
            handoff_usb_rx: Vec::new(),
            handoff_usb_int_raw: 8,
            handoff_usb_int_ena: 0,
        },
    }
}

struct Reference;

impl Hal for Reference {
    fn eeprom_read(&mut self, _: u64, _: u16, _: u32) -> u8 { unreachable!("this profile has no EEPROM") }
    fn eeprom_write(&mut self, _: u64, _: u16, _: u8, _: u32) { unreachable!("this profile has no EEPROM") }
    fn shift(&mut self, _: u64, _: u32, _: u32, _: u32, _: u8, _: u32) { unreachable!("this fixture shifts nothing") }
    fn configure(&mut self, _: u64, _: u32, _: Role) {
        panic!("pixel loops have no GPIO declarations");
    }
    fn serial(&mut self, _: u64, _: u32, _: serial::SerialSetting) {
        panic!("pixel loops have no serial links");
    }
    fn write(&mut self, _: u64, _: u32, _: bool, _: u32) {
        panic!("pixel loops never write HAL pins");
    }
    fn duty(&mut self, _: u64, _: u32, _: u8, _: u32) {
        panic!("pixel loops never set PWM");
    }
    fn read(&mut self, _: u64, _: u32, _: u32) -> bool {
        panic!("pixel loops never read HAL pins");
    }
    fn adc(&mut self, _: u64, _: u32, _: u32) -> u16 {
        panic!("pixel loops never read analog inputs");
    }
    fn eth_send(&mut self, _: u64, _: &[u8], _: u32) -> bool { false }
    fn eth_mac(&mut self, _: u64, _: u32) -> [u8; 6] { [2, 0, 0, 0, 0, 1] }
    fn clock(&mut self, t: u64, _: u32) -> u64 {
        t
    }
    fn fault(&mut self, _: u64, fault: Fault) {
        panic!("pixel loops must run without reference faults: {fault:?}");
    }
}

#[derive(Default)]
struct Observed {
    reader: watch::Reader,
}

impl Observed {
    fn until_pc(&mut self, m: &mut Lockstep<S3Machine<Netlist>>, image: &Image, pc: u32, phase: &str) {
        let mut marks = PcSet::new(SLOT_BASE, image.slot.bytes.len() as u32);
        marks.insert(pc);
        let deadline = m.now() + 2 * m.tick_hz();
        loop {
            let ran = m.run(&Until {
                tick: deadline,
                marks: &marks,
                stops: &[pc],
                stops_after: m.now(),
                on_drive: false,
            });
            let mut out = Vec::new();
            m.take_out(&mut out);
            for byte in out {
                assert_eq!(byte.link, 0, "{phase}: watch bytes use USB link zero");
                if let Some(frame) = self.reader.push(byte.byte) {
                    let frame = frame.unwrap_or_else(|e| panic!("{phase}: invalid watch frame: {e:?}"));
                    assert!(
                        !matches!(
                            frame.event,
                            watch::Event::Fault { .. } | watch::Event::Lost { .. }
                        ),
                        "{phase}: unexpected watch event: {:?}",
                        frame.event
                    );
                }
            }
            m.mark();
            match ran {
                Ran::Pc => break,
                Ran::Barrier => (),
                other => panic!(
                    "{phase}: firmware must reach {pc:#x}: {other:?}, {:?}",
                    m.status()
                ),
            }
        }
        assert_eq!(m.status(), Status::Running, "{phase}: model keeps running");
        assert_eq!(
            m.pc(),
            pc,
            "{phase}: firmware reaches the requested instruction"
        );
        assert!(m.last_reset().is_none(), "{phase}: firmware never resets");
    }
}

fn same_buffers(m: &S3Machine<Netlist>, image: &Image, reference: &Device<'_>, phase: &str) {
    let buffers = &image.slot.layout.buffers;
    assert_eq!(
        buffers.len(),
        reference.buffers().len(),
        "{phase}: every buffer is allocated"
    );
    for (placed, expected) in buffers.iter().zip(reference.buffers()) {
        let mut actual = vec![0; placed.allocated_bytes as usize];
        assert!(
            m.read_data(placed.address, &mut actual),
            "{phase}: buffer {} is readable",
            placed.name
        );
        let expected: Vec<_> = expected.iter().flat_map(|word| word.to_le_bytes()).collect();
        assert_eq!(
            actual, expected,
            "{phase}: buffer {} equals the reference byte for byte",
            placed.name
        );
    }
}

#[test]
fn pixel_loops_match_the_reference_and_report_instructions() {
    let language = chip::profile();
    let compiled = compile(SOURCE, &language, &[], &[])
        .unwrap_or_else(|e| panic!("P3 loops must compile within the S3 AIR cap: {e:?}"));
    let module = module::load(&compiled.module, &language).expect("P3 loop module verifies");
    let handoff = profile();
    let board = Board {
        layout: VOICEBOX,
        panel: None,
        i2c: None,
        motion: None, touch: None,
        radio: None,
        time: BoardTime {
            rc_slow_hz: handoff.time.rc_slow_hz,
            wdt_delay_sel: handoff.time.wdt_delay_sel,
        },
    };
    let image =
        build_image(&module, &board, &SlotOptions::default()).expect("P3 loop firmware fits and links");
    for name in ["gradient", "refine", "scale", "blend", "sand"] {
        let entry = image.slot.handlers.iter().position(|h| h.name == name).expect("fixture handler");
        let steps = steps(&module, entry);
        let mut innermost = 0;
        for (n, step) in steps.iter().enumerate() {
            let Step::Op(i) = step else { continue; };
            if i.op != OP_LOOP { continue; }
            // A first EndLoop before another OP_LOOP closes an innermost
            // loop. The emitter's range ends after this EndLoop's counter,
            // increment and backedge, even when OP_LOOP itself emits no code.
            if !matches!(steps[n + 1..].iter().find(|s| match s {
                Step::EndLoop => true, Step::Op(i) => i.op == OP_LOOP, _ => false,
            }), Some(Step::EndLoop)) {
                continue;
            }
            let span = image.slot.map.handlers.loops.iter()
                .find(|s| s.entry == entry && s.code == i.at).expect("innermost loop has an emitted range");
            assert!(span.start < span.end, "{name}: innermost loop is nonempty");
            let mut pc = span.start;
            while pc < span.end {
                let at = (pc - SLOT_BASE) as usize;
                let bytes = &image.slot.bytes[at..at + 3];
                let bits = u32::from(bytes[0]) | u32::from(bytes[1]) << 8 | u32::from(bytes[2]) << 16;
                let (insn, len) = decode(bits).expect("loop instruction decodes");
                assert!(!(matches!(insn.op, Op::L32i | Op::S32i | Op::L32iN | Op::S32iN) && insn.s == 1),
                    "{name}: stack traffic at {pc:#x} in innermost AIR loop {}: {insn:?}", i.at);
                pc += len;
            }
            assert_eq!(pc, span.end, "{name}: EndLoop range ends on an instruction boundary");
            innermost += 1;
        }
        assert!(innermost != 0, "{name}: stack-traffic check covers an innermost loop");
        let analysis = Analysis::new(&module, entry, &steps);
        let proof = bounds(&module, entry, &steps, &analysis);
        for &step in &steps {
            let Step::Op(i) = step else { continue; };
            if !matches!(i.op, OP_LOAD | OP_STORE) || i.sub < REGION_BUF_BASE { continue; }
            let (base, index) = if i.op == OP_LOAD { (i.args[1], i.args[3]) } else { (i.args[0], i.args[2]) };
            if index == NONE { continue; }
            let buffer = if i.sub == REGION_BUF_LONG { i.args[4] } else { i.sub - REGION_BUF_BASE } as usize;
            let desc = module.buffers()[buffer];
            let base = base.min(desc.len - 1);
            if base == desc.len - 1 { continue; }
            let max = desc.len - 1 - base;
            let placed = &image.slot.layout.buffers[buffer];
            let address = placed.address + base * desc.elem.bytes();
            let span = image.slot.map.handlers.ops.iter().find(|s| s.entry == entry && s.code == i.at)
                .expect("indexed fixture access has a listing");
            let mut pc = span.start;
            let mut clamps = 0;
            let mut rematerialized = false;
            while pc < span.end {
                let at = (pc - SLOT_BASE) as usize;
                let bytes = &image.slot.bytes[at..at + 3];
                let bits = u32::from(bytes[0]) | u32::from(bytes[1]) << 8 | u32::from(bytes[2]) << 16;
                let (insn, len) = decode(bits).expect("address instruction decodes");
                clamps += usize::from(insn.op == Op::Minu);
                let literal = if insn.op == Op::L32r {
                    let at = (target(&insn, pc).unwrap() - SLOT_BASE) as usize;
                    Some(u32::from_le_bytes(image.slot.bytes[at..at + 4].try_into().unwrap()))
                } else if matches!(insn.op, Op::Movi | Op::MoviN) { Some(insn.imm as u32) }
                else { None };
                rematerialized |= literal == Some(address);
                pc += len;
            }
            let bound = proof[index as usize];
            assert_eq!(clamps, usize::from(bound.is_none_or(|b| b > max)),
                "{name}: AIR {} elides MINU only with a sufficient u32 bound", i.at);
            if matches!(name, "gradient" | "scale" | "blend") {
                assert_eq!(clamps, 0, "{name}: every pixel index is proved within its buffer");
            }
            // If blend exceeds 36, the lead's same run identifies each
            // remaining clamp (unknown/too-large proof) or reloaded base
            // (no register free across its whole loop).
            eprintln!("P3 addressing: {name} AIR {} {} {}[base {base} + v{index}]: bound={bound:?}, max={max}, MINU={clamps}, base={}",
                i.at, if i.op == OP_LOAD { "load" } else { "store" }, placed.name,
                if rematerialized { "rematerialized (no free register)" } else { "held" });
        }
    }
    let desc = BoardDesc {
        seed: 0x5300,
        parts: Vec::new(),
        nets: Vec::new(),
    };
    // The native tier (page units, the decoded lane elsewhere) in lockstep with the reference:
    // every run must agree in full. Units compile at once, so the hot loops run native code.
    let mut machine = Lockstep::new(S3Machine::with_board(
        Arc::new(image.flash.clone()),
        &handoff,
        Netlist::new(desc).expect("pixel loops have a valid empty netlist"),
    )
    .expect("pixel-loop board binds to S3"));
    fab_elec_mcu_emu::jit::set_inline_compile(true);
    machine.set_tier(Tier::Native);
    let mut reference = Device::new(module, &language);
    let mut hal = Reference;
    reference.boot(&mut hal).expect("reference setup executes");
    let mut observed = Observed::default();
    observed.until_pc(
        &mut machine,
        &image,
        image.slot.symbols.setup_call + 3,
        "setup",
    );
    same_buffers(&machine, &image, &reference, "setup");

    let mut measurements = Vec::new();
    for (name, elements) in [
        ("seed_blend", None),
        ("seed_sand", None),
        ("gradient", Some(32 * 32)),
        ("refine", Some(64 * 4 * 4)),
        ("scale", Some(46 * 56)),
        ("blend", Some(32 * 32)),
        ("sand", Some(32 * 32)),
    ] {
        let ran = reference
            .step(&mut hal)
            .unwrap_or_else(|| panic!("{name}: reference timer is armed"));
        let handler = &image.slot.handlers[ran.entry as usize];
        assert_eq!(
            handler.name, name,
            "{name}: reference timers execute in fixture order"
        );
        observed.until_pc(&mut machine, &image, handler.address, name);
        let returned = machine.cpu().ar[0];
        let started = machine.cpu().instret;
        observed.until_pc(&mut machine, &image, returned, name);
        let retired = machine.cpu().instret - started;
        // These handlers use only arithmetic and element access: no port
        // calls or self-feeding USB polls need subtracting from the count.
        assert!(
            retired <= handler.max_insns,
            "{name}: {retired} retired instructions exceed max_insns {}",
            handler.max_insns
        );
        // All four have branch-free innermost source loops (refine too).
        // sand's data-dependent IF paths need only the upper-bound check.
        if matches!(name, "gradient" | "refine" | "scale" | "blend") {
            assert!(u128::from(handler.max_insns) * 4 <= u128::from(retired) * 5,
                "{name}: bound {} exceeds 125% of {retired} retired instructions", handler.max_insns);
        }
        same_buffers(&machine, &image, &reference, name);
        if let Some(elements) = elements {
            measurements.push((name, retired, handler.max_insns, elements));
        }
    }
    // The hot loops ran as native units where the host maps executable code.
    let jit = fab_elec_mcu_emu::jit::totals();
    assert!(jit.units > 0 || jit.failed > 0, "the hot pages were compiled: {jit:?}");
    eprintln!("P3 loops: loop       retired instructions        bound   elements   instructions/element");
    for (name, retired, bound, elements) in measurements {
        eprintln!(
            "          {name:<8} {retired:>20} {bound:>12} {elements:>10} {:>22.3}",
            retired as f64 / elements as f64
        );
    }
}
