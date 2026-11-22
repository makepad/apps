//! One firmware image, real S3 boot/USB path, and the reference buffer semantics.
use std::sync::Arc;

use fab_elec_mcu::{compile, Compiled};
use fab_elec_mcu_emit_s3::{build_image, chip, Board, BoardTime, Image, SlotOptions, SLOT_BASE, VOICEBOX, VOICEBOX_PANEL};
use fab_elec_mcu_emu::{Machine, PcSet, Ran, Status, Until};
use fab_elec_mcu_parts::{BoardDesc, Netlist};
use fab_elec_mcu_rt::{format::*, module::{self, Module}, sched::{Device, Fault, Hal, Role}, serial, watch};
use fab_elec_mcu_s3::{CpuClock, PinsProfile, S3Machine, S3Profile, TimeProfile};

fn profile() -> S3Profile {
    // The explicit handoff inputs from display.rs, without panel declarations.
    S3Profile {
        mac: [2, 0, 0, 0, 0, 1], pvt: fab_elec_mcu_s3::NO_PVT,
        flash_bytes: VOICEBOX.flash_bytes as usize, handoff_cpu: CpuClock::Xtal { div: 2 },
        handoff_mapped_pages: 64, entry: 0x4200_0408, seed: 0x5300,
        time: TimeProfile {
            systimer_first_long: true, rc_slow_hz: (136_000, 1), wdt_delay_sel: 0,
            swd_timeout_ticks: 432_000_000, swd_request_lead_ticks: 48_000_000,
            wdt_sync_cycles: 2, mwdt_rephase_on_feed: false, mwdt_rephase_on_config: true,
            wdt_restart_on_enable: true,
        },
        pins: PinsProfile {
            power_on_gpio0_high: true, usb_service_ticks: 480_000,
            usb_frame_phase_ticks: 0, input_sync_delay_ticks: 0,
            handoff_digital_pads_unheld: true, handoff_out: [0; 2], handoff_enable: [0; 2],
            handoff_pin: [0; 45], handoff_io_mux: [1 << 12; 45], handoff_out_sel: [256; 45],
            handoff_usb_tx: Vec::new(), handoff_usb_tx_flushed: false,
            handoff_usb_rx: Vec::new(), handoff_usb_int_raw: 8, handoff_usb_int_ena: 0,
        },
    }
}

#[derive(Default)]
struct Reference { faults: Vec<watch::Event> }

impl Hal for Reference {
    fn eeprom_read(&mut self, _: u64, _: u16, _: u32) -> u8 { unreachable!("this profile has no EEPROM") }
    fn eeprom_write(&mut self, _: u64, _: u16, _: u8, _: u32) { unreachable!("this profile has no EEPROM") }
    fn shift(&mut self, _: u64, _: u32, _: u32, _: u32, _: u8, _: u32) { unreachable!("this fixture shifts nothing") }
    fn configure(&mut self, _: u64, _: u32, _: Role) { panic!("buffer fixture has no GPIO declarations"); }
    fn serial(&mut self, _: u64, _: u32, _: serial::SerialSetting) { panic!("buffer fixture has no serial links"); }
    fn write(&mut self, _: u64, _: u32, _: bool, _: u32) { panic!("buffers never write HAL pins"); }
    fn duty(&mut self, _: u64, _: u32, _: u8, _: u32) { panic!("buffers never set PWM"); }
    fn read(&mut self, _: u64, _: u32, _: u32) -> bool { panic!("buffers never read HAL pins"); }
    fn adc(&mut self, _: u64, _: u32, _: u32) -> u16 { panic!("buffers never read analog inputs"); }
    fn eth_send(&mut self, _: u64, _: &[u8], _: u32) -> bool { false }
    fn eth_mac(&mut self, _: u64, _: u32) -> [u8; 6] { [2, 0, 0, 0, 0, 1] }
    fn clock(&mut self, t: u64, _: u32) -> u64 { t }
    fn fault(&mut self, _: u64, fault: Fault) {
        let Fault::Range { site, n } = fault else { panic!("only range faults are expected: {fault:?}"); };
        self.faults.push(watch::Event::Fault { code: watch::FAULT_RANGE, site: site as u16, arg: n });
    }
}

#[derive(Default)]
struct Observed { reader: watch::Reader, faults: Vec<watch::Event> }

impl Observed {
    /// Returns how many watch bytes left on USB.
    fn take(&mut self, m: &mut S3Machine<Netlist>) -> usize {
        let mut out = Vec::new(); m.take_out(&mut out);
        let sent = out.len();
        for byte in out {
            assert_eq!(byte.link, 0, "buffer faults use USB link zero");
            if let Some(frame) = self.reader.push(byte.byte) {
                let frame = frame.expect("every buffer watch frame has a valid CRC");
                if let event @ watch::Event::Fault { .. } = frame.event { self.faults.push(event); }
                assert!(!matches!(frame.event, watch::Event::Lost { .. }), "watch transport loses no buffer events");
            }
        }
        m.mark();
        sent
    }

    fn until_pc(&mut self, m: &mut S3Machine<Netlist>, image: &Image, pc: u32) {
        let mut marks = PcSet::new(SLOT_BASE, image.slot.bytes.len() as u32); marks.insert(pc);
        let deadline = m.now() + 2 * m.tick_hz();
        loop {
            let ran = m.run(&Until { tick: deadline, marks: &marks, stops: &[pc],
                stops_after: m.now(), on_drive: false });
            self.take(m);
            match ran {
                Ran::Pc => break,
                Ran::Barrier => (),
                other => panic!("firmware must reach {pc:#x}: {other:?}, {:?}", m.status()),
            }
        }
        assert_eq!(m.status(), Status::Running, "buffer execution returns without a model halt");
        assert_eq!(m.pc(), pc, "firmware stops at the requested real-image instruction");
    }

    fn drain(&mut self, m: &mut S3Machine<Netlist>, image: &Image) {
        // The runtime queues frames in its TX ring and the model's host reads one
        // 64-byte packet per 1 ms USB frame. Run whole frames until the ring is
        // empty and a frame passes with no output. Timer firings are 100 ms
        // apart, so this cannot run the next handler.
        let marks = PcSet::new(SLOT_BASE, image.slot.bytes.len() as u32);
        let l = &image.slot.layout;
        let word = |m: &S3Machine<Netlist>, address: u32| {
            let mut b = [0; 4];
            assert!(m.read_data(address, &mut b), "the TX ring indices are readable in SRAM");
            u32::from_le_bytes(b)
        };
        for frame in 0.. {
            assert!(frame < 50, "queued watch frames leave within 50 USB frames");
            let deadline = m.now() + m.tick_hz() / 1000;
            let mut sent = 0;
            loop {
                let ran = m.run(&Until { tick: deadline, marks: &marks, stops: &[], stops_after: 0, on_drive: false });
                sent += self.take(m);
                match ran {
                    Ran::Tick => break,
                    Ran::Barrier => (),
                    other => panic!("USB drain must preserve model success: {other:?}, {:?}", m.status()),
                }
            }
            if sent == 0 && word(m, l.tx_head) == word(m, l.tx_tail) { break; }
        }
    }
}

fn read(m: &S3Machine<Netlist>, address: u32, bytes: usize) -> Vec<u8> {
    let mut data = vec![0; bytes];
    assert!(m.read_data(address, &mut data), "allocated state/buffer bytes are readable in SRAM");
    data
}

fn bytes(words: &[u32]) -> Vec<u8> { words.iter().flat_map(|w| w.to_le_bytes()).collect() }

fn same(m: &S3Machine<Netlist>, image: &Image, reference: &Device<'_>, hal: &Reference, observed: &Observed, phase: &str) {
    let l = &image.slot.layout;
    assert_eq!(read(m, l.state, l.state_words as usize * 4), bytes(reference.state()),
        "{phase}: every state word equals the reference");
    assert_eq!(l.buffers.len(), reference.buffers().len(), "{phase}: every buffer has an allocation");
    for (placed, expected) in l.buffers.iter().zip(reference.buffers()) {
        assert_eq!(read(m, placed.address, placed.allocated_bytes as usize), bytes(expected),
            "{phase}: buffer {} equals the reference byte for byte, including padding", placed.name);
    }
    assert_eq!(observed.faults, hal.faults, "{phase}: fault codes, sites, requested counts and suppression equal the reference");
    assert!(m.last_reset().is_none(), "{phase}: boot and buffer loops keep the watchdog fed");
}

fn state(c: &Compiled, d: &Device<'_>, name: &str) -> i32 {
    let v = c.lowered.state_vars.iter().find(|v| v.name == name).expect("property names a state variable");
    d.state()[v.offset as usize] as i32
}

// Walk verified AIR rather than searching operand words for opcode patterns.
fn instructions(m: &Module<'_>) -> Vec<usize> {
    let code = m.code(); let mut out = Vec::new();
    let mut blocks: Vec<_> = m.entries().iter().map(|e| e.body as usize).collect();
    while let Some(block) = blocks.pop() {
        let mut pc = block + 1; let end = pc + code[block] as usize;
        while pc < end {
            out.push(pc);
            let word = code[pc];
            pc += match word & OP_MASK {
                OP_CONST | OP_GET | OP_UN | OP_SET => 3,
                OP_CONST_D | OP_BIN | OP_CMP_F | OP_CMP_I | OP_CMP_D | OP_WRAP => 4,
                OP_LOAD | OP_STORE => 5 + usize::from(word >> SUB_SHIFT == REGION_BUF_LONG),
                OP_SEL | OP_FMA => 5,
                OP_PORT => 5 + code[pc + 3] as usize + code[pc + 4] as usize,
                OP_BREAK | OP_CONTINUE => 2,
                OP_IF => {
                    let yes = pc + 2; let no = yes + 1 + code[yes] as usize;
                    blocks.extend([yes, no]); no + 1 + code[no] as usize - pc
                }
                OP_LOOP => { blocks.push(pc + 2); 3 + code[pc + 2] as usize }
                op => panic!("verified buffer fixture has recognized opcode {op}"),
            };
        }
        assert_eq!(pc, end, "long region operands preserve AIR block boundaries");
    }
    out
}

#[test]
fn buffers_match_the_reference() {
    let language = chip::profile();
    assert_eq!(language.buffer_bytes, language.ram_words * 4, "S3 buffers share the 384 KiB SRAM pool");
    assert_eq!(VOICEBOX_PANEL.profile().buffer_bytes, language.buffer_bytes, "panel profile inherits the buffer pool");
    let mut source = String::from("\
let b = bytes(8)
let s = samples(3)
let w = words(3)
let p = surface(3, 1)
let bp = bytes(3)
let sp = samples(3)
let wp = words(3)
let pp = surface(3, 1)
let raw = words(3)
");
    for k in 0..241 { source.push_str(&format!("let tiny{k} = bytes(1)\n")); }
    source.push_str("\
var negative: i32 = -1
var huge: i32 = 2147483647
var one: i32 = 1
var small: i32 = 0
var signed: i32 = 0
var unsigned: i32 = 0
var whole: i32 = 0
var positive: i32 = 0
var high_pixel: i32 = 0
var last_word: i32 = 0
var neg_byte: i32 = 0
var neg_sample: i32 = 0
var neg_pixel: i32 = 0
var neg_word: i32 = 0
var huge_byte: i32 = 0
var huge_sample: i32 = 0
var huge_pixel: i32 = 0
var huge_word: i32 = 0
var raw_get: i32 = 0
var sum_get: i32 = 0
var long0: i32 = 0
var long1: i32 = 0
var lb: i32 = 0
var ls: i32 = 0
var lw: i32 = 0
var lp: i32 = 0
fn setup() { lb = len(b); ls = len(s); lw = len(w); lp = len(p)
");
    // Cross a bitmap-word boundary before any faulting sites. Valid empty
    // ranges also prove that off == len, n == 0 does not report a fault.
    for _ in 0..33 { source.push_str("fill(b, 8, 0, 7)\n"); }
    source.push_str("\
}
let edges = after(ms(100), fn() {
    b[0] = 300; s[0] = 0x1_8000; p[0] = 0x1_FFFF; w[0] = 0x7FC0_1234
    small = b[0]; signed = s[0]; unsigned = p[0]; whole = w[0]
    s[2] = 0x1_7FFF; p[2] = 0x1_8000; w[2] = -1
    positive = s[2]; high_pixel = p[2]; last_word = w[2]
    b[negative] = 511; s[negative] = 0x1_FFFF; p[negative] = -1; w[negative] = -2147483648
    huge_byte = b[huge]; huge_sample = s[huge]; huge_pixel = p[huge]; huge_word = w[huge]
    b[huge] = 256; s[huge] = 0x1_8000; p[huge] = 0x1_0000; w[huge] = 0x7FC0_1234
    neg_byte = b[negative]; neg_sample = s[negative]; neg_pixel = p[negative]; neg_word = w[negative]
    raw[one] = 0x1234_5678; raw_get = raw[one]
    wp[negative] = 0x7654_3210; sum_get = wp[negative]
    tiny239[0] = 287; tiny240[0] = 511
    long0 = tiny239[0]; long1 = tiny240[0]
    for i in 0..8 { b[i] = i + 1 }
})
let right = after(ms(200), fn() { copy(b, 2, b, 0, 6) })
let left = after(ms(300), fn() { copy(b, 0, b, 2, 6) })
let paint = after(ms(400), fn() {
    fill(bp, 0, 3, 0x123); copy(bp, 1, b, 0, 2)
    fill(sp, 0, 3, 0x1_8001); copy(s, 0, sp, 0, 3); copy(s, 1, s, 0, 2)
    fill(wp, 0, 3, 0x7FC0_1234); copy(w, 0, wp, 0, 3); copy(w, 0, w, 1, 2)
    fill(pp, 0, 3, 0x1_BEEF); copy(p, 0, pp, 0, 3)
})
let clamped = every(ms(500), fn() {
    copy(b, -1, b, 6, 2147483647)
    fill(p, 1, 2147483647, 0x1_FFFF)
    fill(w, -1, -1, 7)
    copy(b, 2147483647, b, 0, 0)
})
");
    let compiled = compile(&source, &language, &[], &[]).expect("all buffer forms and long regions compile");
    let verified = module::load(&compiled.module, &language).expect("buffer fixture verifies");
    let ops = instructions(&verified);
    for k in [248, 249] {
        for op in [OP_LOAD, OP_STORE] {
            assert!(ops.iter().any(|&at| verified.code()[at] == (op | REGION_BUF_LONG << SUB_SHIFT)
                && verified.code()[at + 5] == k), "buffer {k}: load/store uses a long region and its trailing buffer index");
        }
    }
    // Source indexing has no unsigned static-base operand. Give raw a MAX
    // base plus one, and wp a base of one plus MAX, then verify again. Both
    // engines run this same module and ordinary board image.
    let mut encoded = compiled.module.clone();
    let code_start = encoded[HEADER_WORDS..HEADER_WORDS + encoded[H_SECTIONS] as usize * DIRECTORY_WORDS]
        .chunks_exact(DIRECTORY_WORDS).find(|row| row[0] == SEC_CODE).expect("fixture has CODE")[1] as usize;
    let mut adjusted = 0;
    for &at in &ops {
        let word = verified.code()[at]; let op = word & OP_MASK;
        let region = word >> SUB_SHIFT;
        if matches!(op, OP_LOAD | OP_STORE) && [REGION_BUF_BASE + 6, REGION_BUF_BASE + 8].contains(&region) {
            let base = at + if op == OP_LOAD { 2 } else { 1 };
            encoded[code_start + base] = if region == REGION_BUF_BASE + 8 { u32::MAX } else { 1 };
            adjusted += 1;
        }
    }
    assert_eq!(adjusted, 4, "overflow fixture adjusts one load/store pair per unsigned sum order");
    let module = module::load(&encoded, &language).expect("unsigned base plus dynamic offset remains verified AIR");
    let handoff = profile();
    let board = Board { layout: VOICEBOX, panel: None, i2c: None, motion: None, touch: None, radio: None,
        time: BoardTime { rc_slow_hz: handoff.time.rc_slow_hz, wdt_delay_sel: handoff.time.wdt_delay_sel } };
    let image = build_image(&module, &board, &SlotOptions::default()).expect("buffer firmware fits and links");
    let l = &image.slot.layout;
    let mut order: Vec<_> = (0..l.buffers.len()).collect(); order.sort_by_key(|&k| (l.buffers[k].bytes, k));
    let mut next = l.state + l.state_words * 4;
    for k in order {
        let b = &l.buffers[k];
        assert_eq!(b.address, next, "buffer {k}: smallest first, ties by number, after state");
        assert_eq!(b.address & 3, 0, "buffer {k}: allocation is word aligned");
        assert_eq!(b.allocated_bytes, b.bytes.div_ceil(4) * 4, "buffer {k}: allocation includes only word padding");
        next += b.allocated_bytes as u32;
    }
    assert!(next <= l.stack_top - l.stack_bytes, "buffers end before the largest handler stack");
    assert!(l.range_fault_words >= 2, "range suppression spans multiple SRAM bitmap words");
    for boot in 0..2 {
        let desc = BoardDesc { seed: 0x5300, parts: Vec::new(), nets: Vec::new() };
        let mut machine = S3Machine::with_board(Arc::new(image.flash.clone()), &handoff,
            Netlist::new(desc).expect("buffer-only board has a valid empty netlist"))
            .expect("the buffer-only board binds to S3");
        for b in &l.buffers {
            assert!(machine.seed_data(b.address, &vec![0xa5; b.allocated_bytes as usize]),
                "boot {boot}: seed buffer {} with nonzero SRAM before firmware executes", b.name);
        }
        assert!(machine.seed_data(l.range_faults, &vec![0xff; l.range_fault_words as usize * 4]),
            "boot {boot}: seed every range-fault bit before firmware executes");
        let mut observed = Observed::default();
        observed.until_pc(&mut machine, &image, image.slot.symbols.setup_call);
        for b in &l.buffers {
            assert!(read(&machine, b.address, b.allocated_bytes as usize).iter().all(|&byte| byte == 0),
                "boot {boot}: buffer {} and padding are zero before setup", b.name);
        }
        assert!(read(&machine, l.range_faults, l.range_fault_words as usize * 4).iter().all(|&byte| byte == 0),
            "boot {boot}: range suppression clears before setup");
        let mut reference = Device::new(module::load(&encoded, &language).expect("same module verifies at reboot"), &language);
        let mut hal = Reference::default(); reference.boot(&mut hal).expect("reference setup executes");
        observed.until_pc(&mut machine, &image, image.slot.symbols.setup_call + 3);
        observed.drain(&mut machine, &image); same(&machine, &image, &reference, &hal, &observed, "setup");
        for (name, expected) in [("lb", 8), ("ls", 3), ("lw", 3), ("lp", 3)] {
            assert_eq!(state(&compiled, &reference, name), expected, "{name}: len counts elements including surface dimensions");
        }
        for phase in ["edges", "right", "left", "paint", "clamped", "clamped"] {
            let ran = reference.step(&mut hal).expect("the next reference timer is armed");
            assert_eq!(compiled.lowered.entries[ran.entry as usize].name, phase, "timer handlers execute in fixture order");
            let handler = &image.slot.handlers[ran.entry as usize];
            observed.until_pc(&mut machine, &image, handler.address);
            let started = machine.cpu().instret;
            observed.until_pc(&mut machine, &image, image.slot.symbols.handler_return);
            if phase == "edges" {
                assert!(machine.cpu().instret - started <= handler.max_insns,
                    "element access instructions fit the handler bound including wrapper and frame clear");
            }
            observed.drain(&mut machine, &image); same(&machine, &image, &reference, &hal, &observed, phase);
            match phase {
                "edges" => {
                    for (name, expected) in [("small", 44), ("signed", -32768), ("unsigned", 65535),
                        ("whole", 0x7fc0_1234), ("positive", 32767), ("high_pixel", 32768), ("last_word", -1),
                        ("huge_byte", 255), ("huge_sample", -1), ("huge_pixel", 65535), ("huge_word", i32::MIN),
                        ("neg_byte", 0), ("neg_sample", -32768), ("neg_pixel", 0), ("neg_word", 0x7fc0_1234),
                        ("raw_get", 0x1234_5678), ("sum_get", 0x7654_3210), ("long0", 31), ("long1", 255)] {
                        assert_eq!(state(&compiled, &reference, name), expected,
                            "{name}: element truncation, extension, unsigned clamping and long-region value");
                    }
                    assert_eq!(reference.buffers()[8], [0, 0, 0x1234_5678], "MAX base plus one clamps to last without wrapping");
                    assert_eq!(reference.buffers()[6], [0, 0, 0x7654_3210], "in-range base plus MAX offset clamps to last without wrapping");
                }
                "right" => assert_eq!(bytes(&reference.buffers()[0]), [1, 2, 1, 2, 3, 4, 5, 6], "copy safely overlaps rightward"),
                "left" => assert_eq!(bytes(&reference.buffers()[0]), [1, 2, 3, 4, 5, 6, 5, 6], "copy safely overlaps leftward"),
                "paint" => {
                    assert_eq!(bytes(&reference.buffers()[4]), [35, 1, 2, 0], "U8 fill truncates and cross-buffer copy preserves its prefix and padding");
                    assert_eq!(bytes(&reference.buffers()[1]), [1, 128, 1, 128, 1, 128, 0, 0], "I16 fill/copy retains truncated bits and padding");
                    assert_eq!(reference.buffers()[2], [0x7fc0_1234; 3], "Word fill/copy preserves floating-point payload bits");
                    assert_eq!(bytes(&reference.buffers()[3]), [239, 190, 239, 190, 239, 190, 0, 0], "U16 fill/copy packs little endian and preserves padding");
                }
                "clamped" => {
                    assert_eq!(bytes(&reference.buffers()[0]), [5, 6, 3, 4, 5, 6, 5, 6], "copy takes the smaller clamped count");
                    assert_eq!(bytes(&reference.buffers()[3]), [239, 190, 255, 255, 255, 255, 0, 0],
                        "clamped fill truncates the last two pixels and preserves padding");
                    assert_eq!(reference.buffers()[2], [0x7fc0_1234; 3], "negative fill count writes no elements");
                    assert_eq!(hal.faults.len(), 4, "every bad range site reports once per boot and is silent on repetition");
                    assert!(hal.faults.iter().all(|e| matches!(e, watch::Event::Fault { site, .. } if *site >= 32)),
                        "once-per-site faults address bitmap words beyond the first");
                    assert_eq!(hal.faults.iter().map(|e| match e { watch::Event::Fault { arg, .. } => *arg, _ => unreachable!() })
                        .collect::<Vec<_>>(), [i32::MAX, i32::MAX, -1, 0], "fault arguments retain requested signed counts");
                }
                _ => unreachable!(),
            }
        }
    }
    // Permit the compiler/verifier to describe an oversized program so the
    // actual board image builder, with its shared SRAM pool, owns this refusal.
    let oversized = fab_elec_mcu_rt::Profile { buffer_bytes: language.buffer_bytes + 4, ..language };
    let large = compile("let largest = bytes(393216)\nlet small = bytes(1)\nfn setup() {}\n", &oversized, &[], &[])
        .expect("oversized fixture reaches the image builder");
    let large = module::load(&large.module, &oversized).expect("oversized fixture verifies under its declared compiler budget");
    let refused = build_image(&large, &board, &SlotOptions::default()).err().expect("image builder refuses buffers beyond SRAM");
    for text in ["largest", "small", "U8", "393216 bytes", "free bytes", "does not fit", "0x3fc"] {
        assert!(refused.message.contains(text), "SRAM refusal includes complete placement property {text}: {refused}");
    }
}
