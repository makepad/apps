//! Declared buffers agree with compute AIR, including packing and range faults.
use fab_elec_mcu::{compile, encode, Compiled, Profile};
use fab_elec_mcu_rt as rt;
use makepad_script_compute::{ir::{self, RawBuf}, lower::event::EventLowered};
use makepad_zune_png::{PngEncoder, makepad_zune_core::{bit_depth::BitDepth, colorspace::ColorSpace, options::EncoderOptions}};
use rt::{format::*, module::{self, LoadError, Module}, ports, sched::{Device, Fault, Hal, Role}};

const P: Profile = Profile {
    id: 0, pins: 0, max_entry_ops: 50_000, call_ops: None, ram_words: 4096, buffer_bytes: 4096, eeprom: 0, eeprom_write_ns: 0,
    f64: false, op_ns: 0, clock_hz: 1_000_000,
    analog: &rt::analog::NONE, pwm: &rt::pwm::NONE, serial: &rt::serial::NONE, motion: None, touch: false, audio: None, display: None,
};

#[derive(Default)]
struct RecordingHal { faults: Vec<(u32, i32)> }

impl Hal for RecordingHal {
    fn eeprom_read(&mut self, _: u64, _: u16, _: u32) -> u8 { unreachable!("this profile has no EEPROM") }
    fn eeprom_write(&mut self, _: u64, _: u16, _: u8, _: u32) { unreachable!("this profile has no EEPROM") }
    fn shift(&mut self, _: u64, _: u32, _: u32, _: u32, _: u8, _: u32) { unreachable!("this fixture shifts nothing") }
    fn configure(&mut self, _: u64, _: u32, _: Role) { panic!("buffer programs declare no pins"); }
    fn serial(&mut self, _: u64, _: u32, _: rt::serial::SerialSetting) { panic!("buffer programs declare no serial links"); }
    fn write(&mut self, _: u64, _: u32, _: bool, _: u32) { panic!("buffer operations never write HAL pins"); }
    fn duty(&mut self, _: u64, _: u32, _: u8, _: u32) { panic!("buffer operations never set HAL PWM"); }
    fn read(&mut self, _: u64, _: u32, _: u32) -> bool { panic!("buffer operations never read HAL pins"); }
    fn adc(&mut self, _: u64, _: u32, _: u32) -> u16 { unreachable!("this fixture declares no analog input") }
    fn eth_send(&mut self, _: u64, _: &[u8], _: u32) -> bool { false }
    fn eth_mac(&mut self, _: u64, _: u32) -> [u8; 6] { [2, 0, 0, 0, 0, 1] }
    fn clock(&mut self, t: u64, _: u32) -> u64 { t }
    fn fault(&mut self, _: u64, fault: Fault) {
        let Fault::Range { site, n } = fault else { panic!("buffer ranges are the only expected faults: {fault:?}"); };
        self.faults.push((site, n));
    }
}

struct Core {
    state: Vec<u32>,
    buffers: Vec<Vec<u32>>,
    seen: Vec<bool>,
    faults: Vec<(u32, i32)>,
}

impl Core {
    fn new(c: &Compiled) -> Self {
        let module = module::load(&c.module, &P).expect("core oracle receives a verified module");
        let buffers = module.buffers().iter().map(|b| {
            let mut words = vec![0; b.words()];
            let d = &module.decls()[b.decl as usize];
            if let (ports::FORM_IMAGE, [module::DeclArg::Int(index), ..]) = (d.form, d.args.as_slice()) {
                let image = &module.images()[*index as usize];
                for y in 0..image.height { for x in 0..image.width {
                    let k = (y * image.width + x) as usize;
                    words[k / 2] |= u32::from(image.pixel(x, y).expect("oracle image pixel exists")) << (16 * (k % 2));
                } }
            }
            words
        }).collect();
        Self {
            state: c.lowered.state_init.clone(),
            buffers,
            seen: vec![false; c.sites.len()], faults: Vec::new(),
        }
    }

    fn boot(&mut self, c: &Compiled) {
        for (buffer, row) in self.buffers.iter_mut().zip(&c.lowered.buffers) {
            if row.writable { buffer.fill(0); }
        }
        self.seen.fill(false);
    }

    fn run(&mut self, c: &Compiled, entry: usize) {
        let bufs: Vec<_> = c.lowered.buffers.iter().zip(&mut self.buffers).map(|(b, words)| RawBuf {
            ptr: words.as_mut_ptr(), len: b.len as usize, writable: b.writable, elem: b.elem,
        }).collect();
        let program = &c.lowered.entries[entry].program;
        let mut scratch = vec![0; program.scratch_words()];
        let mut mem = ir::Mem { ctx: &mut [], state: &mut self.state, shared: ir::Shared::Read(&c.lowered.shared_init), bufs: &bufs };
        let mut io = ir::Io::default();
        let mut ports = CorePorts { bufs: &bufs, seen: &mut self.seen, faults: &mut self.faults };
        ir::run_ports(program, &mut scratch, &mut mem, &mut io, 1, &mut ports);
    }
}

struct CorePorts<'a> {
    bufs: &'a [RawBuf],
    seen: &'a mut [bool],
    faults: &'a mut Vec<(u32, i32)>,
}

fn core_range(off: i32, n: i32, len: usize) -> (usize, usize, bool) {
    let valid = off >= 0 && n >= 0 && off as u64 + n as u64 <= len as u64;
    let start = (off.max(0) as usize).min(len);
    (start, (n.max(0) as usize).min(len - start), !valid)
}

impl ir::Ports for CorePorts<'_> {
    fn call(&mut self, port: u16, site: u32, args: &[u32], rets: &mut [u32]) {
        assert!(rets.is_empty(), "copy and fill have no results");
        let dst = self.bufs[args[0] as usize];
        let n = args[if port == ports::COPY { 4 } else { 2 }] as i32;
        let (start, count, mut bad) = core_range(args[1] as i32, n, dst.len);
        match port {
            ports::COPY => {
                let src = self.bufs[args[2] as usize];
                let (source, available, source_bad) = core_range(args[3] as i32, n, src.len);
                bad |= source_bad;
                // A snapshot is an independent overlap oracle for the device's
                // directional, allocation-free implementation.
                let values: Vec<_> = (source..source + count.min(available)).map(|i| src.load_word(i)).collect();
                for (i, value) in values.into_iter().enumerate() { dst.store_word(start + i, value); }
            }
            ports::FILL => for i in start..start + count { dst.store_word(i, args[3]); },
            _ => panic!("only copy and fill may reach the buffer oracle, got {port}"),
        }
        if bad && !self.seen[site as usize] {
            self.seen[site as usize] = true;
            self.faults.push((site, n));
        }
    }
}

fn bytes(words: &[u32]) -> Vec<u8> { words.iter().flat_map(|w| w.to_le_bytes()).collect() }

fn same(c: &Compiled, device: &Device<'_>, core: &Core, hal: &RecordingHal, handler: &str) {
    assert_eq!(device.state(), core.state, "{handler}: every state word matches compute AIR");
    assert_eq!(device.buffers().len(), core.buffers.len(), "{handler}: every declared buffer is allocated");
    for (k, (actual, expected)) in device.buffers().iter().zip(&core.buffers).enumerate() {
        let b = &c.lowered.buffers[k];
        assert_eq!(bytes(actual), bytes(expected), "{handler}: buffer {k} matches AIR byte for byte, including word padding");
        assert!(bytes(actual)[b.len as usize * b.elem.bytes()..].iter().all(|b| *b == 0), "{handler}: buffer {k} padding remains zero");
    }
    assert_eq!(hal.faults, core.faults, "{handler}: range fault sites, requested counts and once-per-boot suppression match");
}

fn state(c: &Compiled, device: &Device<'_>, name: &str) -> i32 {
    let v = c.lowered.state_vars.iter().find(|v| v.name == name).expect("property names an existing state variable");
    device.state()[v.offset as usize] as i32
}

fn section(words: &[u32], kind: u32) -> usize {
    words[HEADER_WORDS..HEADER_WORDS + words[H_SECTIONS] as usize * DIRECTORY_WORDS]
        .chunks_exact(DIRECTORY_WORDS).find(|row| row[0] == kind).expect("fixture contains the requested section")[1] as usize
}

// Code offsets and block ends, including the end of an optimized-away mark.
fn walk(m: &Module<'_>) -> (Vec<usize>, Vec<usize>) {
    let code = m.code();
    let mut instructions = Vec::new();
    let mut ends = Vec::new();
    let mut blocks: Vec<_> = m.entries().iter().map(|e| e.body as usize).collect();
    while let Some(block) = blocks.pop() {
        let mut pc = block + 1;
        let end = pc + code[block] as usize;
        ends.push(end);
        while pc < end {
            instructions.push(pc);
            let word = code[pc];
            pc += match word & OP_MASK {
                OP_CONST | OP_GET | OP_UN | OP_SET => 3,
                OP_CONST_D | OP_BIN | OP_CMP_F | OP_CMP_I | OP_CMP_D | OP_WRAP => 4,
                OP_LOAD | OP_STORE => 5 + usize::from(word >> SUB_SHIFT == REGION_BUF_LONG),
                OP_SEL | OP_FMA => 5,
                OP_PORT => 5 + code[pc + 3] as usize + code[pc + 4] as usize,
                OP_BREAK | OP_CONTINUE => 2,
                OP_IF => {
                    let yes = pc + 2;
                    let no = yes + 1 + code[yes] as usize;
                    blocks.extend([yes, no]);
                    no + 1 + code[no] as usize - pc
                }
                OP_LOOP => { blocks.push(pc + 2); 3 + code[pc + 2] as usize }
                op => panic!("verified instruction walker recognizes opcode {op}"),
            };
        }
        assert_eq!(pc, end, "instruction lengths land exactly on each block end");
    }
    instructions.sort_unstable();
    (instructions, ends)
}

#[test]
fn buffers_match_the_core() {
    let source = "\
let b = bytes(8)
let s = samples(3)
let w = words(2)
let p = surface(3, 2)
var small: i32 = 0
var signed: i32 = 0
var unsigned: i32 = 0
var whole: i32 = 0
var last: i32 = 0
var lb: i32 = 0
var ls: i32 = 0
var lw: i32 = 0
var lp: i32 = 0
fn setup() { lb = len(b); ls = len(s); lw = len(w); lp = len(p) }
let edges = after(1, fn() {
    b[0] = 300
    s[0] = 0x1_8000
    p[0] = 0xFFFF
    w[0] = 0x1234_5678
    small = b[0]
    signed = s[0]
    unsigned = p[0]
    whole = w[0]
    b[100] = 511
    last = b[7]
    for i in 0..8 { b[i] = i + 1 }
})
let right = after(2, fn() { copy(b, 2, b, 0, 6) })
let left = after(3, fn() { copy(b, 0, b, 2, 6) })
let paint = after(4, fn() { fill(p, 0, len(p), 0x1_8000) })
let clamped = every(5, fn() {
    copy(b, -1, b, 6, 2147483647)
    fill(p, 4, 2147483647, 0x1_FFFF)
    fill(w, -1, -1, 7)
    copy(b, 2147483647, b, 0, 0)
})
";
    let mut png = Vec::new();
    PngEncoder::new(&[255; 16], EncoderOptions::new(2, 2, ColorSpace::RGBA, BitDepth::Eight))
        .encode(&mut png).expect("tiny image fixture encodes with the workspace PNG encoder");
    let c = compile(source, &P, &[], &[("tile.png", &png)]).expect("all four buffer forms compile with an optional image");
    // Both optional sections must work regardless of directory row order.
    let mut reordered = c.module.clone();
    let dir_end = HEADER_WORDS + reordered[H_SECTIONS] as usize * DIRECTORY_WORDS;
    let rows: Vec<_> = reordered[HEADER_WORDS..dir_end].chunks_exact(DIRECTORY_WORDS).map(|r| r.to_vec()).collect();
    for (dst, src) in reordered[HEADER_WORDS..dir_end].chunks_exact_mut(DIRECTORY_WORDS).zip(rows.iter().rev()) { dst.copy_from_slice(src); }
    let m = module::load(&reordered, &P).expect("IMAGES and BUFS are located by ID after directory reversal");
    assert_eq!(m.images()[0].pixel(0, 0), Some(0xffff), "optional IMAGES survives directory reversal alongside BUFS");
    assert_eq!(m.buffers().iter().map(|b| (b.elem as u32, b.len, b.writable)).collect::<Vec<_>>(),
        [(2, 4, false), (1, 8, true), (3, 3, true), (0, 2, true), (2, 6, true)], "buffer rows preserve declaration order, kinds, product lengths and writability");
    let surface = &c.lowered.decls[c.lowered.buffers[4].decl as usize];
    assert_eq!(surface.args[0], makepad_script_compute::lower::event::DeclArg::Int(3), "surface declaration retains its first-argument width");
    let mut device = Device::new(m, &P);
    let mut core = Core::new(&c);
    let mut hal = RecordingHal::default();
    for boot in 0..2 {
        core.boot(&c);
        let setup = device.boot(&mut hal).expect("setup runs at each boot");
        core.run(&c, setup.entry as usize);
        same(&c, &device, &core, &hal, "boot");
        assert!(device.buffers()[1..].iter().flatten().all(|w| *w == 0), "boot {boot}: every writable buffer is zero before event handlers");
        assert_eq!(device.buffers()[0], [u32::MAX; 2], "boot {boot}: read-only image retains its pixels");
        for (name, value) in [("lb", 8), ("ls", 3), ("lw", 2), ("lp", 6)] {
            assert_eq!(state(&c, &device, name), value, "{name}: len reports elements, with surface width times height");
        }
        for name in ["edges", "right", "left", "paint", "clamped", "clamped"] {
            let ran = device.step(&mut hal).expect("fixture has the next scheduled handler");
            assert_eq!(c.lowered.entries[ran.entry as usize].name, name, "handlers run in declaration timer order");
            core.run(&c, ran.entry as usize);
            same(&c, &device, &core, &hal, name);
            match name {
                "edges" => for (var, value, property) in [
                    ("small", 44, "U8 truncates and zero-extends"),
                    ("signed", -32768, "I16 truncates and sign-extends"),
                    ("unsigned", 65535, "U16 zero-extends"),
                    ("whole", 0x1234_5678, "Word preserves every bit"),
                    ("last", 255, "past-end store clamps to the last element"),
                ] { assert_eq!(state(&c, &device, var), value, "{property}"); },
                "right" => assert_eq!(bytes(&device.buffers()[1]), [1, 2, 1, 2, 3, 4, 5, 6], "copy safely overlaps toward higher indices"),
                "left" => assert_eq!(bytes(&device.buffers()[1]), [1, 2, 3, 4, 5, 6, 5, 6], "copy safely overlaps toward lower indices"),
                "paint" => assert_eq!(bytes(&device.buffers()[4]), [0, 128].repeat(6), "surface fill truncates each pixel and packs little endian"),
                "clamped" => {
                    assert_eq!(bytes(&device.buffers()[1]), [5, 6, 3, 4, 5, 6, 5, 6], "copy uses clamped starts and the shorter available range");
                    assert_eq!(&bytes(&device.buffers()[4])[8..], &[255; 4], "overflowing fill count clamps to the last two pixels");
                    assert_eq!(hal.faults.len(), (boot + 1) * 4, "each bad range site reports once per boot and stays silent on its second run");
                    assert_eq!(hal.faults[boot * 4..].iter().map(|f| f.1).collect::<Vec<_>>(), [i32::MAX, i32::MAX, -1, 0], "range faults retain the requested signed n, including negative and zero counts");
                }
                _ => unreachable!(),
            }
        }
    }
    assert_eq!(rt::watch::FAULT_RANGE, 10, "range fault uses the next free watch code");
}

#[test]
fn buffer_errors_are_named() {
    let mut source = (0..241).map(|k| format!("let b{k} = bytes(1)\n")).collect::<String>();
    source.push_str("\
var seed: i32 = 5
var first: i32 = 0
var boundary: i32 = 0
var extended: i32 = 0
var last: i32 = 0
fn setup() {}
let write = after(1, fn() {
    b0[0] = 11
    b238[0] = 22
    b239[0] = 44
    b240[0] = 33
    fill(b1, 0, 1, seed)
    copy(b2, 0, b1, 0, 1)
})
let read = after(2, fn() {
    first = b0[0]
    boundary = b238[0]
    extended = b239[0]
    last = b240[0]
})
");
    let c = compile(&source, &P, &[], &[]).expect("241 small buffers compile without an encoding cap");
    let m = module::load(&c.module, &P).expect("long-form buffer module verifies");
    let (instructions, ends) = walk(&m);
    for (k, region) in [(0, REGION_BUF_BASE), (238, 254), (239, REGION_BUF_LONG), (240, REGION_BUF_LONG)] {
        for op in [OP_LOAD, OP_STORE] {
            assert!(instructions.iter().any(|&at| m.code()[at] == (op | region << SUB_SHIFT)
                && (region != REGION_BUF_LONG || m.code()[at + 5] == k)),
                "buffer {k}: opcode {op} uses the expected short/long region and trailing index");
        }
    }
    assert!(c.marks.iter().chain(&c.cond_marks).all(|(at, _)| instructions.contains(&(*at as usize)) || ends.contains(&(*at as usize))),
        "statement and condition marks stay on instruction boundaries after long-form operands");
    let code = section(&c.module, SEC_CODE);
    let bufs = section(&c.module, SEC_BUFS);
    let long = *instructions.iter().find(|&&at| m.code()[at] == (OP_STORE | REGION_BUF_LONG << SUB_SHIFT)).expect("long-form store is exercised");
    let fill = *instructions.iter().find(|&&at| m.code()[at] == OP_PORT && m.code()[at + 1] == u32::from(ports::FILL)).expect("buffer fill is exercised");
    let state_load = *instructions.iter().find(|&&at| at < fill && m.code()[at] == (OP_LOAD | REGION_STATE << SUB_SHIFT)).expect("fill's dynamic value is loaded from state");
    let mut device = Device::new(m, &P);
    let mut core = Core::new(&c);
    let mut hal = RecordingHal::default();
    let ran = device.boot(&mut hal).expect("long-form fixture boots");
    core.run(&c, ran.entry as usize);
    same(&c, &device, &core, &hal, "long-form boot");
    for name in ["write", "read"] {
        let ran = device.step(&mut hal).expect("long-form fixture has the next handler");
        core.run(&c, ran.entry as usize);
        same(&c, &device, &core, &hal, name);
    }
    for (var, value) in [("first", 11), ("boundary", 22), ("extended", 44), ("last", 33)] {
        assert_eq!(state(&c, &device, var), value, "{var}: stored element survives a separate handler through short/long encoding");
    }
    for (offset, value, cause) in [
        (bufs + 1, 0, "elem"),
        (bufs + 2, 2, "length"),
        (bufs + 3, 2, "flags"),
        (bufs, 1, "order"),
        (bufs, 241, "decl"),
        (bufs + 3, 0, "read-only"),
        (code + long + 5, 241, "region"),
        (code + fill + 5, c.module[code + state_load + 1], "constant"),
    ] {
        let mut bad = c.module.clone();
        bad[offset] = value;
        let error = module::load(&bad, &P).expect_err("mutated buffer module must be refused");
        let named = match cause {
            "elem" => matches!(error, LoadError::BufferElem { .. }),
            "length" => matches!(error, LoadError::BufferLength { .. }),
            "flags" => matches!(error, LoadError::BufferFlags { .. }),
            "order" => matches!(error, LoadError::BufferOrder { .. }),
            "decl" => matches!(error, LoadError::BufferDecl { .. }),
            "read-only" => matches!(error, LoadError::BufferWritable { buffer: 0, writable: false, .. }),
            "region" => matches!(error, LoadError::BufferRegion { .. }),
            "constant" => matches!(error, LoadError::BufferArgument { .. }),
            _ => unreachable!(),
        };
        assert!(named, "{cause}: refusal names its specific cause, got {error}");
        assert!(!error.to_string().contains("invalid microsplash module"), "{cause}: refusal has a sentence in words");
    }
    let mut bad = c.module.clone();
    bad[section(&c.module, SEC_DECLS) + DECL_HEADER_WORDS + 2] = ports::FORM_SAMPLES;
    bad[bufs + BUF_WORDS + 1] = module::Elem::I16 as u32;
    let error = module::load(&bad, &P).expect_err("copy refuses two otherwise valid buffers of different kinds");
    assert!(matches!(error, LoadError::CopyKinds { dst: 2, src: 1, .. }), "copy kind refusal identifies both buffer numbers");
    assert!(error.to_string().contains("U8") && error.to_string().contains("I16"), "copy kind refusal names both element kinds");
    for region in 4..REGION_BUF_BASE {
        let mut bad = c.module.clone();
        bad[code + long] = OP_STORE | region << SUB_SHIFT;
        assert!(matches!(module::load(&bad, &P), Err(LoadError::Region { region: found, .. }) if found == region),
            "unused region {region} remains refused");
    }
    for limit in [0, 963] {
        let profile = Profile { buffer_bytes: limit, ..P };
        let error = module::load(&c.module, &profile).expect_err("profile must refuse buffers beyond its byte allocation");
        assert_eq!(error, LoadError::BufferBytes { needed: 964, limit }, "buffer budget rounds each one-byte allocation to its own whole word");
        assert_eq!(error.to_string(), format!("the program's buffers need 964 bytes; this board has {limit}"), "zero-space and undersized profiles name both buffer bytes and board capacity");
    }
    let mut bad = c.module.clone();
    let limit = u32::from(rt::watch::SITE_RADIO_SCHEDULER);
    for sites in [limit + 1, u32::MAX] {
        bad[section(&c.module, SEC_LAYOUT) + L_SITES] = sites;
        assert_eq!(module::load(&bad, &P).expect_err("a site count reaching the reserved site values must be refused"),
            LoadError::Sites { sites, limit }, "a module's sites stay below the watch stream's radio scheduler and no-site values");
    }
    bad[section(&c.module, SEC_LAYOUT) + L_SITES] = limit;
    assert!(matches!(module::load(&bad, &Profile { ram_words: 2047, ..P }), Err(LoadError::BufferFaultRam { words: 2048, .. })),
        "an untrusted site count cannot allocate an unbudgeted buffer fault bitset");
    let mismatch = "let byte_data = bytes(1)\nlet audio_data = samples(1)\nfn setup() { copy(byte_data, 0, audio_data, 0, 1) }";
    let errors = match compile(mismatch, &P, &[], &[]) {
        Ok(_) => panic!("copy across element kinds must be a compile error"),
        Err(errors) => errors,
    };
    assert_eq!(errors[0].line_col(mismatch).0, 3, "copy kind error points to the port's source line");
    assert_eq!(&mismatch[errors[0].start..errors[0].end], "copy(byte_data, 0, audio_data, 0, 1)",
        "copy kind error carries the port call's exact source span");
    for word in ["copy", "byte_data", "U8", "audio_data", "I16"] {
        assert!(errors[0].message.contains(word), "copy kind error names {word}");
    }

    // The empty module's exact v4 words, derived from the pre-buffer encoder:
    // eight required sections, an eight-word zero layout and no payloads.
    let empty = EventLowered {
        entries: Vec::new(), funcs: Vec::new(), state_init: Vec::new(), state_vars: Vec::new(),
        shared_init: Vec::new(), decls: Vec::new(), buffers: Vec::new(), sites: Vec::new(),
        marks: Vec::new(), conds: Vec::new(), copies: Vec::new(),
    };
    let old = [
        MAGIC, 1, 4, 0, 40, 8, 0, 0,
        1, 32, 8, 2, 40, 0, 3, 40, 0, 4, 40, 0,
        5, 40, 0, 6, 40, 0, 7, 40, 0, 8, 40, 0,
        0, 0, 0, 0, 0, 0, 0, 0,
    ];
    let encoded = encode::encode(&empty, 0).expect("buffer-free legacy fixture encodes");
    assert_eq!(encoded.len(), old.len(), "buffer-free encoding adds no section or words");
    for (at, (&actual, &legacy)) in encoded.iter().zip(&old).enumerate() {
        assert_eq!(actual, if at == H_PORT_VERSION { ports::PORT_TABLE_VERSION } else { legacy },
            "buffer-free word {at} is unchanged except H_PORT_VERSION");
    }
}
