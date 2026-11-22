//! Phase 6a's differential oracle: one flat RAM bus, CALL0 port stubs, and
//! rt::exec fed exactly the same port answers. No ESP32-S3 machine is used.
use fab_elec_mcu::{compile_with, Compiled};
use fab_elec_mcu_emit32::xtensa::enc;
use fab_elec_mcu_emit_s3::{build_handlers, build_slot, chip, debug_table, Code, Options, SlotOptions, Where};

// Retired-instruction bounds of float.rs's f32 div and sqrt for normal operands, counted
// statically: imm is one, shl/shr three, a forward branch one untaken and two taken, a short
// backward branch one; from the first guard through packing, without the operand loads, the
// result store and the handler's wrapper. A subnormal operand normalized by s shifts adds
// 4s - 2. The restoring loops these replaced bounded the routines at 232 and 644.
// Div: guards 15 + normalization 22 + exponent 8 + QUOU/REMU digits 2 + 3*7 + rounding 23.
const DIV_F32_MAX_RETIRED: u64 = 91;
// Sqrt: guards 7 + normalization 11 + parity 4 + Newton seed 14 + 3*17 + correction 21 +
// rounding 13. The seed is floor(sqrt(N)) or one above, so the correction takes at most one
// decrement and no increment.
const SQRT_F32_MAX_RETIRED: u64 = 121;
use fab_elec_mcu_emu::bus::{Memory, Perm};
use fab_elec_mcu_rt::{exec::{self, Exit, PortCall, Ports}, fmath, format::*, module::{self, Elem, Module}, ports};
use fab_elec_mcu_xtensa::{insn::{decode, Op}, sim::{Config, Cpu, IntType, Stop, Unaligned, Vectors}};

const BASE: u32 = 0x1000;
const RETURN: u32 = 0x100;
const PORT_BASE: u32 = 0x200;
const STATE: u32 = 0x80000;
const SHARED: u32 = 0x90000;
const STACK: u32 = 0x100000;

fn options() -> Options {
    Options { base: BASE, ports: (0..ports::PORTS.len()).map(|p| (p as u16, PORT_BASE + p as u32 * 16)).collect(),
        ..Options::default() }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Call { port: u16, site: u32, args: Vec<u32>, rets: Vec<u32> }

struct Replay<'a> { calls: &'a [Call], at: usize, buffers: &'a mut [Vec<u32>] }
impl Ports for Replay<'_> {
    fn buffer(&mut self, index: u32) -> &mut [u32] { &mut self.buffers[index as usize] }
    fn call(&mut self, port: u16, site: u32, _: u64, args: &[u32], rets: &mut [u32]) -> PortCall {
        let call = self.calls.get(self.at).expect("reference made an extra port call");
        assert_eq!((port, site, args), (call.port, call.site, call.args.as_slice()), "port order/site/args");
        assert!(rets.iter().all(|&w| w == 0), "exec supplies zeroed result words");
        rets.copy_from_slice(&call.rets); self.at += 1; PortCall::Done
    }
}

struct Oracle { code: Code, mem: Memory, ram: usize, scratch: Vec<u32>, buffers: Vec<u32> }
impl Oracle {
    fn new(m: &Module<'_>, o: &Options) -> Self {
        let code = build_handlers(m, o).unwrap_or_else(|e| panic!("{e}"));
        assert!(code.base as usize + code.bytes.len() < STATE as usize);
        let mut mem = Memory::new(); let ram = mem.store(STACK as usize + 16);
        mem.map(0, ram, 0, STACK as usize + 16, Perm { read: true, write: true, exec: true });
        mem.bytes_mut(ram)[code.base as usize..code.base as usize + code.bytes.len()].copy_from_slice(&code.bytes);
        mem.bytes_mut(ram)[RETURN as usize..RETURN as usize + 3].copy_from_slice(&0x004000u32.to_le_bytes()[..3]);
        for &(_, address) in &o.ports {
            mem.bytes_mut(ram)[address as usize..address as usize + 2].copy_from_slice(&enc::ret_n().to_le_bytes());
        }
        // Decode every emitted instruction, including exceptional branches
        // not taken in a particular case. ISA p. 109: handlers never observe U.
        for &(start, end) in &code.map.entries {
            let mut pc = start;
            while pc < end {
                let bytes = &mem.bytes(ram)[pc as usize..pc as usize + 3];
                let bits = u32::from(bytes[0]) | u32::from(bytes[1]) << 8 | u32::from(bytes[2]) << 16;
                let (i, len) = decode(bits).expect("every generated instruction decodes");
                assert!(!matches!(i.op, Op::RurFsr | Op::WurFcr | Op::WurFsr), "handler must neither read FSR nor change FP control/status");
                pc += len;
            }
            assert_eq!(pc, end);
        }
        for span in &code.map.ops {
            assert_eq!(code.map.find(span.start), Where::Entry { entry: span.entry, code: Some(span.code), site: span.site, mark: span.mark });
            assert_eq!(code.map.find(span.end - 1), code.map.find(span.start));
        }
        let scratch = vec![0; (0..m.entries().len()).map(|k| m.scratch_words(k)).max().unwrap_or(0)];
        Self { code, mem, ram, scratch, buffers: o.buffers.clone() }
    }
    fn put(&mut self, address: u32, words: &[u32]) {
        for (n, word) in words.iter().enumerate() {
            let at = address as usize + n * 4;
            self.mem.bytes_mut(self.ram)[at..at + 4].copy_from_slice(&word.to_le_bytes());
        }
    }
    fn words(&self, address: u32, count: usize) -> Vec<u32> {
        self.mem.bytes(self.ram)[address as usize..address as usize + count * 4].chunks_exact(4)
            .map(|w| u32::from_le_bytes(w.try_into().unwrap())).collect()
    }
    fn run(&mut self, m: &Module<'_>, k: usize, initial: &[u32]) -> (Vec<u32>, Vec<Call>, bool) {
        let mut buffers: Vec<_> = self.buffers.iter().zip(m.buffers())
            .map(|(&address, b)| self.words(address, b.words())).collect();
        self.put(STATE, initial); self.put(SHARED, m.shared());
        let report = &self.code.entries[k];
        let stack_bytes = report.stack_bytes;
        let entry = self.code.base + report.offset;
        self.mem.bytes_mut(self.ram)[(STACK - stack_bytes) as usize..STACK as usize].fill(0xa5);
        let config = Config { mul32_high: true, div32: true, unaligned: Unaligned::Hardware, excm_level: 3,
            mac16: true, s32c1i: true, threadptr: true,
            vecbase_reset: 0, vectors: Vectors { kernel: 0x100, user: 0x180, window: [0, 0x40, 0x80, 0xc0, 0x100, 0x140] },
            int_type: [IntType::None; 32], int_level: [0; 32], prid: None };
        let mut cpu = Cpu::new(config, entry);
        for (r, value) in cpu.ar.iter_mut().enumerate() { *value = 0x5a5a_0000 | r as u32; }
        cpu.ar[0] = RETURN; cpu.ar[1] = STACK; cpu.ar[2] = STATE; cpu.ar[3] = SHARED;
        cpu.cpenable = 1;
        cpu.fcr = 0; // 6b's contract: round to nearest even, no FP traps.
        let saved = cpu.ar[12..16].to_vec();
        let mut calls = Vec::new();
        let mut completed = false;
        for _ in 0..3_000_000 {
            if let Some(port) = self.code.ports.iter().find(|p| p.address == cpu.pc) {
                let sig = &ports::PORTS[port.port as usize];
                let args = cpu.ar[2..2 + sig.params.len()].to_vec();
                let site = cpu.ar[10];
                // A deterministic input script independent of arithmetic under
                // test. Replay passes these exact words to exec's Ports.
                let answer = match port.port {
                    ports::PIN_READ => (calls.len() as u32 ^ site) & 1,
                    ports::SERIAL_BYTE => 0x80 | (calls.len() as u32 & 0x7f),
                    _ => 0x8000_0001u32.wrapping_add(calls.len() as u32 * 997),
                };
                let rets = vec![answer; sig.rets.len()];
                calls.push(Call { port: port.port, site, args, rets: rets.clone() });
                cpu.ar[2..12].fill(0xdead_beef); // Real CALL0 scratch clobbers.
                cpu.ar[2..2 + rets.len()].copy_from_slice(&rets);
            }
            match cpu.step(&mut self.mem) {
                Ok(()) => {}
                Err(Stop::Break { pc: RETURN }) => { completed = true; break; }
                Err(stop) => panic!("entry {k} from {initial:08x?}: unexpected {stop:?}; {:?}", self.code.map.find(cpu.pc)),
            }
        }
        assert!(completed, "entry {k}: handler did not return within the oracle bound");
        let report = &self.code.entries[k];
        assert!(cpu.instret <= report.max_insns,
            "handler bound falsifier: entry {k}, initial {initial:08x?}, retired {}, bound {}; listing: {:#?}",
            cpu.instret, report.max_insns,
            range_listing(&self.code, self.code.map.entries[k].0, self.code.map.entries[k].1));
        assert_eq!(cpu.ar[1], STACK, "CALL0 stack restoration");
        assert_eq!(&cpu.ar[12..16], saved.as_slice(), "CALL0 callee-saved registers");
        assert_eq!(cpu.fcr, 0, "FP traps remain disabled");
        assert_eq!(self.words(SHARED, m.shared().len()), m.shared(), "shared region is immutable");
        let actual = self.words(STATE, initial.len());
        let mut expected = initial.to_vec(); let mut replay = Replay { calls: &calls, at: 0, buffers: &mut buffers };
        assert!(matches!(exec::run(m, k, &mut expected, &mut self.scratch, &mut replay), Exit::Done { .. }), "reference completion");
        assert_eq!(replay.at, calls.len(), "reference consumed all scripted port results");
        assert_eq!(actual, expected, "entry {k} from {initial:08x?}: state words");
        for (k, (&address, expected)) in self.buffers.iter().zip(&buffers).enumerate() {
            assert_eq!(self.words(address, expected.len()), *expected, "buffer {k}: exact reference words");
        }
        (actual, calls, cpu.fsr_underflow_unknown)
    }
}

struct Program { types: Vec<u8>, vars: Vec<u8>, frame: u32, body: Vec<u32> }

fn op_listing(code: &Code, entry: usize, at: u32) -> Vec<fab_elec_mcu_xtensa::insn::Insn> {
    let span = code.map.ops.iter().find(|s| s.entry == entry && s.code == at).expect("emitted AIR operation");
    range_listing(code, span.start, span.end)
}

fn range_listing(code: &Code, start: u32, end: u32) -> Vec<fab_elec_mcu_xtensa::insn::Insn> {
    let mut pc = start;
    let mut out = Vec::new();
    while pc < end {
        let at = (pc - code.base) as usize;
        // The last instruction in a handler is a two-byte RET_N.
        let mut bytes = [0; 3];
        let len = (code.bytes.len() - at).min(3);
        bytes[..len].copy_from_slice(&code.bytes[at..at + len]);
        let bits = u32::from(bytes[0]) | u32::from(bytes[1]) << 8 | u32::from(bytes[2]) << 16;
        let (insn, len) = decode(bits).expect("operation instruction decodes");
        out.push(insn); pc += len;
    }
    assert_eq!(pc, end, "listing ends on an instruction boundary");
    out
}

// Minimal format writer lets the oracle exercise every AIR opcode directly,
// even if source-level folding/fusion would otherwise erase it.
fn words(programs: &[Program], state: u32, shared: &[u32]) -> Vec<u32> {
    words_with_buffer(programs, state, shared, 0)
}

fn words_with_buffer(programs: &[Program], state: u32, shared: &[u32], buffer_words: u32) -> Vec<u32> {
    let buffers = if buffer_words == 0 { vec![] } else { vec![(Elem::Word, buffer_words)] };
    words_with_buffers(programs, state, shared, &buffers)
}

fn words_with_buffers(programs: &[Program], state: u32, shared: &[u32], buffers: &[(Elem, u32)]) -> Vec<u32> {
    let mut sections = vec![Vec::<u32>::new(); SECTION_COUNT];
    sections[0] = vec![state, shared.len() as u32, 32, programs.len() as u32, programs.len() as u32, 0, 0, 0];
    sections[1] = vec![0; state as usize]; sections[2] = shared.to_vec();
    for (k, p) in programs.iter().enumerate() {
        let name = format!("op{k}"); sections[3].push(name.len() as u32);
        for chunk in name.as_bytes().chunks(4) {
            let mut packed = [0; 4]; packed[..chunk.len()].copy_from_slice(chunk);
            sections[3].push(u32::from_le_bytes(packed));
        }
        let start = sections[7].len() as u32;
        sections[7].extend([p.types.len() as u32, p.vars.len() as u32, p.frame,
            u32::from(p.types.contains(&TY_F64) || p.vars.contains(&TY_F64))]);
        for types in [&p.types, &p.vars] {
            for chunk in types.chunks(4) {
                let mut packed = [0; 4]; packed[..chunk.len()].copy_from_slice(chunk);
                sections[7].push(u32::from_le_bytes(packed));
            }
        }
        sections[7].push(p.body.len() as u32); sections[7].extend(&p.body);
        let len = sections[7].len() as u32 - start;
        sections[6].extend([k as u32, start, len, 50_000, 0]);
    }
    if !buffers.is_empty() {
        sections[0][L_DECLS] = buffers.len() as u32;
        let mut rows = Vec::new();
        for (k, &(elem, len)) in buffers.iter().enumerate() {
            let form = match elem {
                Elem::Word => ports::FORM_WORDS, Elem::U8 => ports::FORM_BYTES,
                Elem::U16 => ports::FORM_SURFACE, Elem::I16 => ports::FORM_SAMPLES,
            };
            sections[5].extend([form, k as u32, 0, if elem == Elem::U16 { 2 } else { 1 }, ARG_INT, len]);
            if elem == Elem::U16 { sections[5].extend([ARG_INT, 1]); }
            rows.extend([k as u32, elem as u32, len, 1]);
        }
        sections.push(rows);
    }
    let mut out = vec![0; HEADER_WORDS + sections.len() * DIRECTORY_WORDS];
    out[H_MAGIC] = MAGIC; out[H_VERSION] = FORMAT_VERSION; out[H_PORT_VERSION] = ports::PORT_TABLE_VERSION;
    out[H_SECTIONS] = sections.len() as u32;
    for (k, section) in sections.iter().enumerate() {
        let at = HEADER_WORDS + k * DIRECTORY_WORDS;
        out[at] = if k == SECTION_COUNT { SEC_BUFS } else { k as u32 + 1 }; out[at + 1] = out.len() as u32; out[at + 2] = section.len() as u32;
        out.extend(section);
    }
    out[H_WORDS] = out.len() as u32; out
}

fn instruction(op: u32, sub: u32) -> u32 { op | sub << SUB_SHIFT }
fn operation(op: u32, sub: u32, inputs: [u8; 3], output: u8) -> Program {
    let mut p = Program { types: inputs.into_iter().chain([output]).collect(), vars: vec![], frame: 0, body: vec![] };
    for v in 0..3 { p.body.extend([instruction(OP_LOAD, REGION_STATE), v, v, 1, NONE]); }
    p.body.extend([instruction(op, if op == OP_WRAP { 0 } else { sub }), 3]);
    match op {
        OP_CONST => p.body.push(0x1234_5678), OP_UN => p.body.push(0),
        OP_BIN | OP_CMP_I | OP_CMP_F => p.body.extend([0, 1]),
        OP_SEL | OP_FMA => p.body.extend([0, 1, 2]),
        OP_WRAP => p.body.extend([0, sub]),
        _ => panic!("not an arithmetic family"),
    }
    p.body.extend([instruction(OP_STORE, REGION_STATE), 3, 1, NONE, 3]); p
}

#[test]
fn integer_bool_edges_match_exec() {
    let mut programs: Vec<_> = (BIN_ADD_I..=BIN_SHR_UI).map(|sub| operation(OP_BIN, sub, [TY_I32; 3], TY_I32)).collect();
    programs.extend((CMP_LT..=CMP_NE).map(|sub| operation(OP_CMP_I, sub, [TY_I32; 3], TY_BOOL)));
    programs.push(operation(OP_UN, UN_NEG_I, [TY_I32; 3], TY_I32));
    programs.push(operation(OP_FMA, FMA_MUL_ADD_I, [TY_I32; 3], TY_I32));
    programs.push(operation(OP_CONST, 0, [TY_I32; 3], TY_I32));
    for len in [1, 3, 4, 31, 0x8000_0000, u32::MAX] { programs.push(operation(OP_WRAP, len, [TY_I32; 3], TY_I32)); }
    let encoded = words(&programs, 4, &[]); let m = module::load(&encoded, &chip::profile()).unwrap();
    let mut oracle = Oracle::new(&m, &options());
    let edges = [0, 1, u32::MAX, i32::MIN as u32, i32::MAX as u32, 31, 32, 33];
    for a in edges { for b in edges { for k in 0..programs.len() {
        oracle.run(&m, k, &[a, b, a ^ b, 0xa5a5_a5a5]);
    } } }
    let mut programs = vec![operation(OP_UN, UN_NOT_B, [TY_BOOL; 3], TY_BOOL),
        operation(OP_SEL, 0, [TY_BOOL, TY_I32, TY_I32], TY_I32)];
    programs.extend([BIN_AND_B, BIN_OR_B].map(|sub| operation(OP_BIN, sub, [TY_BOOL; 3], TY_BOOL)));
    programs.extend([CMP_EQ, CMP_NE].map(|sub| operation(OP_CMP_I, sub, [TY_BOOL; 3], TY_BOOL)));
    let encoded = words(&programs, 4, &[]); let m = module::load(&encoded, &chip::profile()).unwrap();
    let mut oracle = Oracle::new(&m, &options());
    for a in 0..2 { for b in 0..2 { for k in 0..programs.len() { oracle.run(&m, k, &[a, b, 1 - b, 99]); } } }
}

#[test]
fn bit_fields_and_constant_divisors_match_exec_and_listing() {
    use fab_elec_mcu_emit32::control::{steps, Analysis, Step};

    #[derive(Clone, Copy)]
    enum Pattern {
        Mask { shift: Option<(u32, u32)>, width: u8, second: bool, reverse: bool },
        Divide { sub: u32, divisor: Option<i32> },
    }
    let mut cases = Vec::new();
    for (kind, sub) in [("signed", BIN_SHR_I), ("unsigned", BIN_SHR_UI)] {
        for width in [1, 8, 16] {
            // Includes shift zero, the high shift bit, the bit-31 boundary,
            // a field past bit 31, and AIR's wrapping shift counts.
            for amount in [0, 7, 16, 32, 39, u32::MAX] {
                cases.push((format!("{kind} shift {amount:#x}, mask {width}"),
                    Pattern::Mask { shift: Some((sub, amount)), width, second: false, reverse: false }));
            }
        }
        for width in [17, 32] {
            cases.push((format!("{kind} wide mask {width}"),
                Pattern::Mask { shift: Some((sub, 7)), width, second: false, reverse: false }));
        }
        cases.push((format!("{kind} shift with second use"),
            Pattern::Mask { shift: Some((sub, 7)), width: 8, second: true, reverse: false }));
        cases.push((format!("{kind} mask on left"),
            Pattern::Mask { shift: Some((sub, 7)), width: 8, second: false, reverse: true }));
    }
    for width in [1, 8, 16, 17, 32] {
        cases.push((format!("bare mask {width}"),
            Pattern::Mask { shift: None, width, second: false, reverse: true }));
    }
    for sub in [BIN_DIV_I, BIN_REM_I] {
        for divisor in [Some(7), Some(-7), Some(1), Some(0), Some(-1), None] {
            cases.push((format!("{} by {divisor:?}", if sub == BIN_DIV_I { "division" } else { "remainder" }),
                Pattern::Divide { sub, divisor }));
        }
    }
    let programs: Vec<_> = cases.iter().map(|(_, pattern)| {
        let mut body = vec![instruction(OP_LOAD, REGION_STATE), 0, 0, 1, NONE];
        let values = match *pattern {
            Pattern::Mask { shift, width, second, reverse } => {
                let source = if let Some((sub, amount)) = shift {
                    body.extend([OP_CONST, 1, amount, instruction(OP_BIN, sub), 2, 0, 1]); 2
                } else { 0 };
                // Deliberately separate the shift and its use with a constant.
                body.extend([OP_CONST, 3, u32::MAX >> (32 - width)]);
                let (left, right) = if reverse { (3, source) } else { (source, 3) };
                body.extend([instruction(OP_BIN, BIN_AND_I), 4, left, right,
                    instruction(OP_STORE, REGION_STATE), 2, 1, NONE, 4]);
                if second { body.extend([instruction(OP_STORE, REGION_STATE), 3, 1, NONE, source]); }
                5
            }
            Pattern::Divide { sub, divisor } => {
                if let Some(c) = divisor { body.extend([OP_CONST, 1, c as u32]); }
                else { body.extend([instruction(OP_LOAD, REGION_STATE), 1, 1, 1, NONE]); }
                body.extend([instruction(OP_BIN, sub), 2, 0, 1,
                    instruction(OP_STORE, REGION_STATE), 2, 1, NONE, 2]);
                3
            }
        };
        Program { types: vec![TY_I32; values], vars: vec![], frame: 0, body }
    }).collect();
    // One verified module, emitted image and model memory for every case.
    let encoded = words(&programs, 4, &[]);
    let m = module::load(&encoded, &chip::profile()).expect("peephole AIR verifies");
    let mut oracle = Oracle::new(&m, &options());
    for (k, (name, pattern)) in cases.iter().enumerate() {
        let steps = steps(&m, k);
        let analysis = Analysis::new(&m, k, &steps);
        let instructions: Vec<_> = steps.iter().filter_map(|s| match s { Step::Op(i) => Some(*i), _ => None }).collect();
        match *pattern {
            Pattern::Mask { shift, width, second, .. } => {
                let mask = instructions.iter().find(|i| i.op == OP_BIN && i.sub == BIN_AND_I).unwrap();
                let listing = op_listing(&oracle.code, k, mask.at);
                let fused = width <= 16 && !second
                    && shift.is_some_and(|(_, amount)| (amount & 31) + u32::from(width) <= 32);
                if width <= 16 {
                    assert_eq!(listing.len(), 1, "{name}: mask is one instruction without a mask register");
                    assert_eq!(listing[0].op, Op::Extui, "{name}: narrow mask uses EXTUI");
                    let amount = if fused { shift.unwrap().1 & 31 } else { 0 };
                    assert_eq!((listing[0].imm, listing[0].r + 1), (amount as i32, width),
                        "{name}: EXTUI shift and mask fields");
                    let constant = instructions.iter().find(|i| i.op == OP_CONST && i.args[0] == 3).unwrap();
                    assert!(!oracle.code.map.ops.iter().any(|s| s.entry == k && s.code == constant.at),
                        "{name}: folded-only mask has no materialization");
                } else {
                    assert_eq!(listing.len(), 1, "{name}: wide mask keeps one register AND");
                    assert_eq!(listing[0].op, Op::And, "{name}: mask wider than 16 bits stays AND");
                }
                if let Some((sub, amount)) = shift {
                    let shifted = instructions.iter().find(|i| i.op == OP_BIN && i.sub == sub).unwrap();
                    let emitted = oracle.code.map.ops.iter().any(|s| s.entry == k && s.code == shifted.at);
                    if fused {
                        assert!(!emitted, "{name}: single-use shift is consumed by EXTUI");
                        let constant = instructions.iter().find(|i| i.op == OP_CONST && i.args[0] == 1).unwrap();
                        assert!(!oracle.code.map.ops.iter().any(|s| s.entry == k && s.code == constant.at),
                            "{name}: consumed shift amount has no materialization");
                    } else if (amount & 31) != 0 {
                        assert!(emitted, "{name}: shared shift, wide mask or field past bit 31 stays unfused");
                        let listing = op_listing(&oracle.code, k, shifted.at);
                        let expected = if sub == BIN_SHR_I { Op::Srai }
                            else if (amount & 31) < 16 { Op::Srli } else { Op::Extui };
                        assert_eq!(listing[0].op, expected, "{name}: original shift remains");
                    }
                }
                let n = steps.iter().position(|s| matches!(s, Step::Op(i) if i.at == mask.at)).unwrap();
                assert_eq!(analysis.bitfields[n].unwrap().width, width,
                    "{name}: shared bit-field facts retain target-neutral widths");
            }
            Pattern::Divide { sub, divisor } => {
                let divide = instructions.iter().find(|i| i.op == OP_BIN && i.sub == sub).unwrap();
                let listing = op_listing(&oracle.code, k, divide.at);
                let op = if sub == BIN_DIV_I { Op::Quos } else { Op::Rems };
                assert_eq!(listing.iter().filter(|i| i.op == op).count(), 1,
                    "{name}: exactly one signed divide or remainder instruction");
                if divisor.is_some_and(|c| c != 0 && c != -1) {
                    assert_eq!(listing.len(), 1, "{name}: constant divisor has no guards or jumps");
                    assert_eq!(listing[0].op, op, "{name}: constant divisor keeps hardware division");
                } else {
                    assert!(listing.iter().any(|i| matches!(i.op, Op::Beqz | Op::BeqzN)),
                        "{name}: zero-divisor test remains");
                    assert!(listing.iter().any(|i| i.op == Op::Bnei && i.imm == -1),
                        "{name}: minus-one test remains");
                }
            }
        }
        for dividend in [0i32, 7, -7, i32::MIN, i32::MAX] {
            let divisors: &[i32] = if matches!(pattern, Pattern::Divide { divisor: None, .. }) {
                &[7, -7, 1, 0, -1]
            } else { &[7] };
            for &runtime_divisor in divisors {
                let (actual, _, _) = oracle.run(&m, k, &[dividend as u32, runtime_divisor as u32, 0xa5a5_a5a5, 0x5a5a_5a5a]);
                let expected = match *pattern {
                    Pattern::Mask { shift, width, second, .. } => {
                        let shifted = match shift {
                            Some((BIN_SHR_I, amount)) => dividend.wrapping_shr(amount) as u32,
                            Some((_, amount)) => (dividend as u32).wrapping_shr(amount),
                            None => dividend as u32,
                        };
                        if second { assert_eq!(actual[3], shifted, "{name}: second use keeps the complete shift result"); }
                        shifted & (u32::MAX >> (32 - width))
                    }
                    Pattern::Divide { sub, divisor } => {
                        let divisor = divisor.unwrap_or(runtime_divisor);
                        if sub == BIN_DIV_I {
                            if divisor == 0 { 0 } else { dividend.wrapping_div(divisor) as u32 }
                        } else if divisor == 0 { dividend as u32 }
                        else { dividend.wrapping_rem(divisor) as u32 }
                    }
                };
                assert_eq!(actual[2], expected, "{name}: dividend {dividend}, runtime divisor {runtime_divisor}");
            }
        }
    }
}

fn float_edges() -> Vec<u32> {
    [0, 1, 3, 0x007f_ffff, 0x0080_0000, 0x0080_0001, 0x00ff_ffff,
        0x3eff_ffff, 0x3f00_0000, 0x3f00_0001, 0x3f7f_ffff, 0x3f80_0000,
        0x3fbf_ffff, 0x3fc0_0000, 0x3fc0_0001, 0x4020_0000, 0x4b00_0000,
        0x4eff_ffff, 0x4f00_0000, 0x4f00_0001, 0x7f7f_ffff, 0x7f80_0000,
        0x7f80_0001, 0x7fa1_2345, 0x7fc5_4321].into_iter().flat_map(|x| [x, x | 0x8000_0000]).collect()
}

#[test]
fn float_binary_edges_and_nan_guards_match_exec() {
    let mut programs: Vec<_> = (BIN_ADD_F..=BIN_MAX_F).map(|sub| operation(OP_BIN, sub, [TY_F32; 3], TY_F32)).collect();
    let arithmetic = programs.len();
    programs.extend((CMP_LT..=CMP_NE).map(|sub| operation(OP_CMP_F, sub, [TY_F32; 3], TY_BOOL)));
    let encoded = words(&programs, 4, &[]); let m = module::load(&encoded, &chip::profile()).unwrap();
    let mut oracle = Oracle::new(&m, &options()); let edges = float_edges();
    for &a in &edges { for &b in &edges { for k in 0..arithmetic { oracle.run(&m, k, &[a, b, 0, 99]); } } }
    // Compares take one FPU instruction behind the NaN guard: zeros, a
    // subnormal, ordered neighbours, infinity and both NaN kinds suffice.
    let ordered: Vec<u32> = [0, 1, 0x3f7f_ffff, 0x3f80_0000, 0x7f80_0000, 0x7f80_0001, 0x7fc5_4321]
        .into_iter().flat_map(|x| [x, x | 0x8000_0000]).collect();
    for &a in &ordered { for &b in &ordered { for k in arithmetic..programs.len() { oracle.run(&m, k, &[a, b, 0, 99]); } } }
    // ISA Summary p. 109: correctly rounded result and Inexact are usable
    // even though the model carries an unknown Underflow beside FSR.
    let (state, _, unknown) = oracle.run(&m, BIN_MUL_F as usize, &[0x0080_0000, 0x3f7f_ffff, 0, 0]);
    assert_eq!(state[3], 0x0080_0000); assert!(unknown);
    let mut seed = 0xa341_316cu32;
    for _ in 0..64 {
        seed ^= seed << 13; seed ^= seed >> 17; seed ^= seed << 5; let a = seed;
        seed ^= seed << 13; seed ^= seed >> 17; seed ^= seed << 5;
        oracle.run(&m, BIN_DIV_F as usize, &[a, seed, 0, 0]);
    }
}

#[test]
fn float_unary_conversions_and_payloads_match_exec() {
    let ops = [UN_NEG_F, UN_ABS_F, UN_SQRT_F, UN_FLOOR_F, UN_CEIL_F, UN_TRUNC_F, UN_ROUND_F, UN_F2I, UN_BITS_FI];
    let programs: Vec<_> = ops.into_iter().map(|sub| operation(OP_UN, sub, [TY_F32; 3],
        if matches!(sub, UN_F2I | UN_BITS_FI) { TY_I32 } else { TY_F32 })).collect();
    let encoded = words(&programs, 4, &[]); let m = module::load(&encoded, &chip::profile()).unwrap();
    let mut oracle = Oracle::new(&m, &options());
    for a in float_edges() { for k in 0..programs.len() { oracle.run(&m, k, &[a, 0, 0, 99]); } }
    let mut seed = 0x91e1_0da5u32;
    for _ in 0..64 {
        seed ^= seed << 13; seed ^= seed >> 17; seed ^= seed << 5;
        oracle.run(&m, 2, &[seed, 0, 0, 99]); // sqrt: varied exponent/mantissa combinations.
    }
    let programs = [operation(OP_UN, UN_I2F, [TY_I32; 3], TY_F32), operation(OP_UN, UN_BITS_IF, [TY_I32; 3], TY_F32)];
    let encoded = words(&programs, 4, &[]); let m = module::load(&encoded, &chip::profile()).unwrap();
    let mut oracle = Oracle::new(&m, &options());
    for a in [0, 1, u32::MAX, 0x00ff_ffff, 0x0100_0001, 0x0100_0003, i32::MIN as u32, i32::MAX as u32]
        .into_iter().chain(float_edges()) {
        for k in 0..programs.len() { oracle.run(&m, k, &[a, 0, 0, 99]); }
    }
    let programs = [operation(OP_SEL, 0, [TY_BOOL, TY_F32, TY_F32], TY_F32)];
    let encoded = words(&programs, 4, &[]); let m = module::load(&encoded, &chip::profile()).unwrap();
    let mut oracle = Oracle::new(&m, &options());
    for a in float_edges() { for condition in [0, 1] { oracle.run(&m, 0, &[condition, a, a ^ 0x8000_0000, 99]); } }
}

#[test]
fn float_div_sqrt_seeded_bits_and_instruction_budgets() {
    let programs = [operation(OP_BIN, BIN_DIV_F, [TY_F32; 3], TY_F32),
        operation(OP_UN, UN_SQRT_F, [TY_F32; 3], TY_F32)];
    let encoded = words(&programs, 4, &[]); let m = module::load(&encoded, &chip::profile()).unwrap();
    // One build for both programs; the model's result is checked against fmath directly.
    let mut oracle = Oracle::new(&m, &options());
    // ESP32-S3 core-isa.h:65–66: MUL32_HIGH and DIV32 are both present.
    let config = Config { mul32_high: true, div32: true, unaligned: Unaligned::Hardware, excm_level: 3,
        mac16: true, s32c1i: true, threadptr: true,
        vecbase_reset: 0, vectors: Vectors { kernel: 0x100, user: 0x180, window: [0, 0x40, 0x80, 0xc0, 0x100, 0x140] },
        int_type: [IntType::None; 32], int_level: [0; 32], prid: None };
    let mut template = Cpu::new(config, 0);
    for (r, value) in template.ar.iter_mut().enumerate() { *value = 0x5a5a_0000 | r as u32; }
    template.ar[0] = RETURN; template.ar[1] = STACK; template.ar[2] = STATE; template.ar[3] = SHARED;
    template.cpenable = 1; template.fcr = 0;
    let budgets = [DIV_F32_MAX_RETIRED, SQRT_F32_MAX_RETIRED];
    // Count the arithmetic source span itself: register allocation changes
    // wrapper and state-access traffic. These small frames keep each operand
    // transfer to/from the fixed float ABI at one instruction (3 div, 2 sqrt).
    let arithmetic: Vec<_> = (0..2).map(|k| {
        assert!(oracle.code.entries[k].stack_bytes <= 1020, "float fixture has short stack offsets");
        *oracle.code.map.ops.iter().find(|span| span.entry == k &&
            m.code()[span.code as usize] & OP_MASK == if k == 0 { OP_BIN } else { OP_UN }).unwrap()
    }).collect();
    let mut maximum = [0u64; 2]; let mut normal_maximum = [0u64; 2];
    let normal = |bits: u32| (0x0080_0000..0x7f80_0000).contains(&(bits & 0x7fff_ffff));
    let normalization_extra = |bits: u32| {
        let mag = bits & 0x7fff_ffff;
        if (1..0x0080_0000).contains(&mag) { u64::from(4 * (mag.leading_zeros() - 8) - 2) } else { 0 }
    };
    let mut check = |k: usize, a: u32, b: u32| {
        oracle.put(STATE, &[a, b, 0, 0xa5a5_a5a5]);
        let mut cpu = template.clone();
        cpu.pc = oracle.code.base + oracle.code.entries[k].offset;
        let extra = normalization_extra(a) + if k == 0 { normalization_extra(b) } else { 0 };
        let mut arithmetic_retired = 0u64;
        let mut completed = false;
        let limit = budgets[k] + extra + u64::from(oracle.code.entries[k].stack_bytes) + 64;
        for _ in 0..limit {
            let in_arithmetic = (arithmetic[k].start..arithmetic[k].end).contains(&cpu.pc);
            match cpu.step(&mut oracle.mem) {
                Ok(()) => arithmetic_retired += u64::from(in_arithmetic),
                Err(Stop::Break { pc: RETURN }) => { completed = true; break; }
                Err(stop) => panic!("op {k}, a={a:08x}, b={b:08x}: unexpected {stop:?}"),
            }
        }
        assert!(completed, "op {k}, a={a:08x}, b={b:08x}: return within instruction budget");
        assert!(cpu.instret <= oracle.code.entries[k].max_insns,
            "handler bound falsifier: op {k}, a={a:08x}, b={b:08x}, retired {}, bound {}; listing: {:#?}",
            cpu.instret, oracle.code.entries[k].max_insns,
            range_listing(&oracle.code, oracle.code.map.entries[k].0, oracle.code.map.entries[k].1));
        let at = STATE as usize + 12;
        let actual = u32::from_le_bytes(oracle.mem.bytes(oracle.ram)[at..at + 4].try_into().unwrap());
        let expected = (if k == 0 { fmath::div_f32(f32::from_bits(a), f32::from_bits(b)) }
            else { fmath::sqrt_f32(f32::from_bits(a)) }).to_bits();
        assert_eq!(actual, expected, "op {k}, a={a:08x}, b={b:08x}: exact fmath bits");
        assert_eq!(cpu.ar[1], STACK, "CALL0 stack restoration");
        assert_eq!(&cpu.ar[12..16], &template.ar[12..16], "CALL0 callee-saved registers");
        let retired = arithmetic_retired.checked_sub(if k == 0 { 3 } else { 2 }).expect("float ABI operand transfers");
        maximum[k] = maximum[k].max(retired);
        if normal(a) && (k == 1 || normal(b)) { normal_maximum[k] = normal_maximum[k].max(retired); }
        assert!(retired <= budgets[k] + extra,
            "op {k}, a={a:08x}, b={b:08x}: {retired} retired, budget {} + normalization {extra}", budgets[k]);
    };
    let edges = float_edges();
    for &a in &edges {
        for &b in &edges { check(0, a, b); }
        check(1, a, 0);
    }
    let mut seed = 0xa341_316cu32;
    let mut next = || { seed ^= seed << 13; seed ^= seed >> 17; seed ^= seed << 5; seed };
    // Every exponent field in each operand first, then full bit patterns (signs,
    // subnormals and NaN payloads included) without bias.
    for i in 0..50_000u32 {
        let mut a = next(); let mut b = next(); let mut x = next();
        if i < 256 {
            a = (a & 0x807f_ffff) | i << 23;
            b = (b & 0x807f_ffff) | (255 - i) << 23;
            x = (x & 0x807f_ffff) | i << 23;
        }
        check(0, a, b); check(1, x, 0);
    }
    for k in 0..2 {
        assert!(normal_maximum[k] > 0 && normal_maximum[k] <= budgets[k],
            "op {k}: normal maximum {} exceeds {}", normal_maximum[k], budgets[k]);
    }
    println!("f32 div/sqrt maximum retired instructions (wrapper excluded): normal={normal_maximum:?}, all={maximum:?}, normal budgets={budgets:?}");
}

#[test]
fn regions_stack_initialization_large_slots_and_all_ports_match_exec() {
    let mut memory = Program { types: vec![TY_I32; 6], vars: vec![TY_I32; 2], frame: 300, body: vec![
        instruction(OP_LOAD, REGION_STATE), 0, 0, 1, NONE,
        instruction(OP_LOAD, REGION_STATE), 1, 1, 1, NONE,
        OP_SET, 0, 0, OP_GET, 2, 0,
        instruction(OP_STORE, REGION_FRAME), 1, 299, 1, 2,
        instruction(OP_LOAD, REGION_FRAME), 3, 1, 299, 1,
        instruction(OP_STORE, REGION_STATE), 2, 1, NONE, 3,
        instruction(OP_LOAD, REGION_SHARED), 4, 0, 3, 1,
        instruction(OP_STORE, REGION_STATE), 3, 1, NONE, 4,
        OP_GET, 5, 1, instruction(OP_STORE, REGION_STATE), 4, 1, NONE, 5,
    ] };
    // Unwritten frame memory must also be zero on every invocation.
    memory.types.push(TY_I32); memory.body.extend([instruction(OP_LOAD, REGION_FRAME), 6, 0, 1, NONE,
        instruction(OP_STORE, REGION_STATE), 5, 1, NONE, 6]);
    let mut slots = Program { types: vec![TY_I32; 599], vars: vec![], frame: 0, body: vec![] };
    // Constants rematerialize instead of occupying slots. Nonliteral loads
    // keep this coverage of frame offsets beyond the L32I/S32I immediate.
    for v in 0..300 { slots.body.extend([instruction(OP_LOAD, REGION_SHARED), v, v, 1, NONE]); }
    let mut sum = 0;
    for v in 1..300 {
        let dst = 299 + v;
        slots.body.extend([instruction(OP_BIN, BIN_ADD_I), dst, sum, v]); sum = dst;
    }
    slots.body.extend([instruction(OP_STORE, REGION_STATE), 2, 1, NONE, sum]);
    // Eight argument values, the widest port's arity (a2..a9).
    let mut calls = Program { types: vec![TY_I32; 8], vars: vec![], frame: 0, body: vec![
        instruction(OP_LOAD, REGION_STATE), 0, 0, 1, NONE,
        instruction(OP_LOAD, REGION_STATE), 1, 1, 1, NONE,
    ] };
    for v in 2..8 { calls.body.extend([OP_CONST, v, v * 2]); }
    // This stub-port fixture has no buffer declarations, so it skips PRESENT
    // (a buffer source); buffers.rs and display.rs cover the buffer ports
    // through the real runtime.
    for (port, sig) in ports::PORTS[..=ports::FILL_RECT as usize].iter().enumerate()
        .filter(|&(port, _)| port != usize::from(ports::PRESENT)) {
        calls.body.extend([OP_PORT, port as u32, port as u32, sig.params.len() as u32, sig.rets.len() as u32]);
        calls.body.extend(0..sig.params.len() as u32);
        for _ in sig.rets {
            let v = calls.types.len() as u32; calls.types.push(TY_I32); calls.body.push(v);
        }
        if !sig.rets.is_empty() {
            calls.body.extend([instruction(OP_STORE, REGION_STATE), 2, 1, NONE, calls.types.len() as u32 - 1]);
        }
    }
    let shared: Vec<_> = [11, 22, 33].into_iter().chain((3..300).map(|v| v * 7919)).collect();
    let encoded = words(&[memory, slots, calls], 6, &shared);
    let m = module::load(&encoded, &chip::profile()).unwrap(); let mut oracle = Oracle::new(&m, &options());
    assert!(oracle.code.entries[1].slot_bytes > 1020, "exercise expanded stack loads/stores");
    for index in [0, 1, 2, 298, 299, 300, 0x8000_0000, u32::MAX] {
        oracle.run(&m, 0, &[0x8765_4321, index, 0, 0, 99, 99]);
    }
    oracle.run(&m, 1, &[0; 6]);
    let (_, calls, _) = oracle.run(&m, 2, &[13, 255, 0, 0, 0, 0]);
    assert_eq!(calls.iter().map(|c| c.port).collect::<Vec<_>>(), (0..=ports::FILL_RECT).filter(|&port| port != ports::PRESENT).collect::<Vec<_>>());
}

fn compile(source: &str) -> Compiled {
    let names = pin_names(); let pins: Vec<_> = names.iter().map(|(n, v)| (n.as_str(), *v)).collect();
    compile_with(source, &chip::profile(), &pins, &[], &[]).unwrap_or_else(|e| panic!("{e:?}\n{source}"))
}

fn pin_names() -> Vec<(String, u32)> {
    let mut names: Vec<_> = (0..49).flat_map(|n| [(format!("D{n}"), n), (format!("P{n}"), n)]).collect();
    names.extend([("PD2".into(), 2), ("PB0".into(), 8), ("PB5".into(), 13)]); names
}

fn replay_compiled(c: &Compiled, rounds: usize) {
    let m = module::load(&c.module, &chip::profile()).unwrap();
    let o = Options { sites: c.sites.iter().map(|s| (s.line, s.col)).collect(), marks: c.marks.clone(), ..options() };
    let mut oracle = Oracle::new(&m, &o); let mut state = m.state_init().to_vec();
    for _ in 0..rounds { for k in 0..m.entries().len() { state = oracle.run(&m, k, &state).0; } }
}

#[test]
fn slot_debug_table_maps_source_to_handler_code() {
    let source = r#"
var count: i32 = 0
var on = false
fn setup() {
    count = 1
    on = true
}
let tick = every(ms(1), fn() {
    count = count + 1
    on = !on
})
"#;
    let c = compile(source);
    let m = module::load(&c.module, &chip::profile()).expect("debug program's module loads");
    let slot = build_slot(&m, &SlotOptions {
        sites: c.sites.iter().map(|s| (s.line, s.col)).collect(), marks: c.marks.clone(), ..SlotOptions::default()
    }).unwrap_or_else(|e| panic!("debug program's slot builds: {e}"));
    let (debug, note) = debug_table(&c, source.len(), &slot);
    assert_eq!(note, None, "a valid slot has no stepping note");
    let map = debug.map.as_ref().expect("a valid slot has a step map");
    assert_eq!(map.check(), Ok(()), "the slot's step map is ascending and unique");
    assert_eq!(debug.image_id, slot.image_id, "debug table identifies its slot");
    assert_eq!(debug.source_rev, 0, "the host sets the source revision");
    assert_eq!(debug.stmts.len(), 4, "both handlers retain their two source statements");
    for (id, stmt) in debug.stmts.iter().enumerate() {
        let at = stmt.addr.expect("each assignment has a statement start");
        assert!(slot.map.handlers.stmt_starts.contains(&(at, id as u32)),
            "statement {id} retains the emitter's start");
        assert!(debug.starting_at(at).contains(&(id as u32)), "statement {id} is grouped at its start");
    }
    for &(at, id) in &slot.map.handlers.stmt_starts {
        assert!(slot.map.handlers.ops.iter().any(|op| op.start == at && c.stmts[id as usize].owner == fab_elec_mcu::Owner::Handler(op.entry as u32)),
            "statement {id} starts at an emitted op in its own entry");
    }
    let mut entries: Vec<_> = slot.handlers.iter().map(|h| (h.address, h.id)).collect();
    entries.sort_unstable();
    assert_eq!(map.entries, entries, "step entries are the handlers' addresses and ids");
    let mut returns = vec![slot.symbols.setup_call + 3, slot.symbols.handler_return];
    returns.sort_unstable();
    assert_eq!(map.returns, returns, "setup and timer returns follow their CALLX0 instructions");
    assert_eq!(debug.vars.len(), c.lowered.state_vars.len(), "every state variable appears in the table");
    for (var, lowered) in debug.vars.iter().zip(&c.lowered.state_vars) {
        assert_eq!(var.addr, slot.layout.state + lowered.offset * 4, "variable {} uses its state word offset", var.name);
    }
    assert!(!slot.map.runtime.is_empty(), "the slot has named runtime routines");
    for routine in &slot.map.runtime {
        assert_eq!(debug.routine(routine.start), Some(routine.name), "runtime routine {} is named at its start", routine.name);
    }
    assert_eq!(debug.runtime_end, slot.code_base, "the runtime ends before handler code and literal pools");
    assert_eq!(debug.routine(debug.runtime_end), None, "handler code is outside the runtime");
}

#[test]
fn registers_spills_calls_constants_and_clamps_match_exec() {
    // Raw AIR keeps every operand form and live range under test, independent
    // of source-level constant folding. Build once, then reuse the dirty stack
    // and buffer for several inputs and repeated handler invocations.
    let mut next = 0;
    let mut fresh = || { let v = next; next += 1; v };
    let mut bools = Vec::new();
    let mut body = Vec::new();
    let values: Vec<_> = (0..12).map(|n| {
        let v = fresh(); body.extend([instruction(OP_LOAD, REGION_STATE), v, n, 1, NONE]);
        body.extend([OP_SET, n, v]); v
    }).collect();
    let zero = fresh(); let one = fresh(); let two = fresh(); let three = fresh();
    body.extend([OP_CONST, zero, 0, OP_CONST, one, 1, OP_CONST, two, 2, OP_CONST, three, 3]);
    let mut outer = Vec::new();
    let old_outer = fresh(); let outer_index = fresh(); let acc = fresh(); let sum = fresh(); let stamp = fresh();
    outer.extend([OP_GET, old_outer, 12, instruction(OP_BIN, BIN_ADD_I), outer_index, old_outer, one,
        OP_SET, 12, outer_index, OP_GET, acc, 14, instruction(OP_BIN, BIN_ADD_I), sum, acc, values[0],
        OP_SET, 14, sum, OP_PORT, u32::from(ports::CLOCK_US), 0, 0, 1, stamp,
        instruction(OP_STORE, REGION_STATE), 12, 1, NONE, stamp, OP_SET, 13, zero]);
    let mut inner = Vec::new();
    let old_inner = fresh(); let inner_index = fresh(); let acc = fresh(); let sum = fresh();
    inner.extend([OP_GET, old_inner, 13, instruction(OP_BIN, BIN_ADD_I), inner_index, old_inner, one,
        OP_SET, 13, inner_index, OP_GET, acc, 14, instruction(OP_BIN, BIN_ADD_I), sum, acc, inner_index,
        OP_SET, 14, sum]);
    let skip = fresh(); let leave = fresh(); bools.extend([skip, leave]);
    inner.extend([instruction(OP_CMP_I, CMP_EQ), skip, inner_index, two,
        OP_IF, skip, 2, OP_CONTINUE, 0, 0,
        instruction(OP_CMP_I, CMP_EQ), leave, outer_index, three,
        OP_IF, leave, 2, OP_BREAK, 1, 0]);
    for (n, &v) in values.iter().enumerate() {
        let old = fresh(); let updated = fresh();
        inner.extend([OP_GET, old, n as u32, instruction(OP_BIN, BIN_ADD_I), updated, old, v,
            OP_SET, n as u32, updated]);
    }
    let leave_inner = fresh(); bools.push(leave_inner);
    inner.extend([instruction(OP_CMP_I, CMP_EQ), leave_inner, inner_index, three,
        OP_IF, leave_inner, 2, OP_BREAK, 0, 0]);
    outer.extend([OP_LOOP, 4, inner.len() as u32]); outer.extend(inner);
    body.extend([OP_LOOP, 4, outer.len() as u32]); body.extend(outer);
    for (n, &v) in values.iter().enumerate() {
        let current = fresh();
        body.extend([instruction(OP_STORE, REGION_STATE), n as u32, 1, NONE, v,
            OP_GET, current, n as u32, instruction(OP_STORE, REGION_STATE), 16 + n as u32, 1, NONE, current]);
    }
    for (var, output) in [(12, 13), (13, 14), (14, 15)] {
        let v = fresh(); body.extend([OP_GET, v, var, instruction(OP_STORE, REGION_STATE), output, 1, NONE, v]);
    }
    let mut forms = Vec::new();
    for sub in [BIN_SHL_I, BIN_SHR_I, BIN_SHR_UI] {
        for amount in [0, 31, 32, u32::MAX, 1, 15, 16] { forms.push((sub, amount)); }
    }
    forms.extend([(-128i32) as u32, 127, 128, (-129i32) as u32].map(|c| (BIN_ADD_I, c)));
    forms.extend([1, 31, 0xffff, 0x1ffff].map(|c| (BIN_AND_I, c)));
    for (n, &(sub, constant)) in forms.iter().enumerate() {
        let c = fresh(); let dst = fresh();
        body.extend([OP_CONST, c, constant, instruction(OP_BIN, sub), dst, values[0], c,
            instruction(OP_STORE, REGION_STATE), 28 + n as u32, 1, NONE, dst]);
    }
    let negative = fresh(); let past = fresh(); let read = fresh(); let read_again = fresh();
    body.extend([OP_CONST, negative, u32::MAX, OP_CONST, past, 99,
        instruction(OP_STORE, REGION_BUF_BASE), 0, 4, negative, values[0],
        instruction(OP_LOAD, REGION_BUF_BASE), read, 0, 4, past,
        instruction(OP_STORE, REGION_STATE), 57, 1, NONE, read,
        instruction(OP_STORE, REGION_BUF_BASE), 1, 3, past, values[1],
        instruction(OP_LOAD, REGION_BUF_BASE), read_again, 0, 4, negative,
        instruction(OP_STORE, REGION_STATE), 58, 1, NONE, read_again,
        instruction(OP_STORE, REGION_STATE), 59, 1, NONE, negative,
        instruction(OP_STORE, REGION_STATE), 60, 1, NONE, past]);
    let conditional = fresh(); bools.push(conditional); let uninitialized = fresh();
    body.extend([instruction(OP_CMP_I, CMP_EQ), conditional, values[0], zero,
        OP_IF, conditional, 3, OP_SET, 15, values[1], 0,
        OP_GET, uninitialized, 15, instruction(OP_STORE, REGION_STATE), 61, 1, NONE, uninitialized]);
    let mut types = vec![TY_I32; next as usize];
    for v in bools { types[v as usize] = TY_BOOL; }
    let encoded = words_with_buffer(&[Program { types, vars: vec![TY_I32; 16], frame: 0, body }], 62, &[], 4);
    let m = module::load(&encoded, &chip::profile()).expect("register-pressure AIR verifies");
    let o = Options { buffers: vec![0xa0000], ..options() };
    let mut oracle = Oracle::new(&m, &o);
    assert!(oracle.code.entries[0].slot_bytes >= 16 * 4, "simultaneous values and vars force spills through the loop call");
    let mut clamp_count = 0;
    for &(start, end) in &oracle.code.map.entries {
        let mut pc = start;
        while pc < end {
            let bytes = &oracle.mem.bytes(oracle.ram)[pc as usize..pc as usize + 3];
            let bits = u32::from(bytes[0]) | u32::from(bytes[1]) << 8 | u32::from(bytes[2]) << 16;
            let (i, len) = decode(bits).unwrap();
            clamp_count += usize::from(i.op == Op::Minu); pc += len;
        }
    }
    assert_eq!(clamp_count, 4, "both buffer loads and stores use MINU");
    for seed in [0u32, 2, 0x8000_0001] {
        let mut state = vec![0xa5a5_a5a5; 62];
        for n in 0..12 { state[n] = seed.wrapping_add(n as u32); }
        for _ in 0..2 {
            let (actual, calls, _) = oracle.run(&m, 0, &state);
            assert_eq!(&actual[..12], &state[..12], "values survive calls and nested loop backedges");
            for n in 0..12 { assert_eq!(actual[16 + n], state[n].wrapping_mul(5), "var {n}: spilled loop-carried update"); }
            assert_eq!(calls.len(), 3, "outer break stops further clock calls");
            assert!(calls.iter().all(|c| c.port == ports::CLOCK_US), "loop calls use the clobbering clock stub");
            assert_eq!(actual[12], calls[2].rets[0], "port result reaches state");
            assert_eq!(&actual[13..16], &[3, 1, seed.wrapping_mul(3).wrapping_add(13)],
                "nested break/continue preserve both counters and the accumulator");
            for (n, &(sub, constant)) in forms.iter().enumerate() {
                let expected = match sub {
                    BIN_SHL_I => seed.wrapping_shl(constant),
                    BIN_SHR_I => (seed as i32).wrapping_shr(constant) as u32,
                    BIN_SHR_UI => seed.wrapping_shr(constant),
                    BIN_ADD_I => seed.wrapping_add(constant),
                    _ => seed & constant,
                };
                assert_eq!(actual[28 + n], expected, "constant form {sub}, operand {constant:#x}");
            }
            assert_eq!(&actual[57..61], &[state[0], state[1], u32::MAX, 99],
                "MINU clamps negative/past-end indices and preserves their original values");
            assert_eq!(actual[61], if seed == 0 { state[1] } else { 0 }, "skipped first SET retains the handler's zero var");
            assert_eq!(oracle.words(o.buffers[0], 4), [0, 0, 0, state[1]], "buffer writes select the last element");
        }
    }
}

#[test]
fn addressing_proofs_loop_literals_and_fallbacks_match_exec_and_listing() {
    use fab_elec_mcu_emit32::{bounds::bounds, control::{steps, Analysis, Step}, literals::{Literal, LoopLiterals}};

    #[derive(Clone, Copy)]
    enum Index {
        Counted, NegativeStart, Unknown, CounterWrite, VaryingLimit, Masked,
        Affine, FusedAffine, Divided, OnePast, Wrapped, UnsignedShift,
        SignedShift, UnknownSignedShift, LeftShift, Variable, PoisonedVariable,
        Selected, MultiplyOverflow, AddOverflow, SignedDivision, SignedMultiply,
        Pressure,
    }
    // One verified module, one emission and one model/oracle for every case.
    // The architect's falsifier, exactly as written:
    // "a loop whose index is not provable keeps its MINU and still equals the reference for an out-of-range index."
    let cases = [
        ("counted index", Index::Counted, Some(3), 0, 0),
        ("unknown index in a loop", Index::Unknown, None, 0, 0),
        ("body writes the counter", Index::CounterWrite, None, 0, 0),
        ("limit varies in the body", Index::VaryingLimit, None, 0, 0),
        ("negative counted start", Index::NegativeStart, None, 0, 0),
        ("masked index", Index::Masked, Some(7), 0, 0),
        ("y * w + x", Index::Affine, Some(7), 0, 0),
        ("fused y * w + x", Index::FusedAffine, Some(7), 0, 0),
        ("divided index", Index::Divided, Some(3), 0, 0),
        ("bound one past the end", Index::OnePast, Some(8), 0, 0),
        ("constant base within range", Index::Counted, Some(3), 0, 4),
        ("constant base reduces range", Index::Counted, Some(3), 0, 5),
        ("wrapped index", Index::Wrapped, Some(7), 0, 0),
        ("unsigned shift", Index::UnsignedShift, Some(7), 0, 0),
        ("non-negative signed shift", Index::SignedShift, Some(3), 0, 0),
        ("unknown signed shift", Index::UnknownSignedShift, None, 0, 0),
        ("bounded left shift", Index::LeftShift, Some(4), 0, 0),
        ("largest variable SET", Index::Variable, Some(7), 0, 0),
        ("unknown variable SET", Index::PoisonedVariable, None, 0, 0),
        ("selected bounds", Index::Selected, Some(7), 0, 0),
        ("u32 product overflow", Index::MultiplyOverflow, None, 0, 0),
        ("u32 sum overflow", Index::AddOverflow, None, 0, 0),
        ("unsigned bound cannot prove signed division", Index::SignedDivision, None, 0, 0),
        ("unsigned bound cannot prove signed multiplication", Index::SignedMultiply, None, 0, 0),
        ("byte elements", Index::Counted, Some(3), 1, 0),
        ("word elements", Index::Counted, Some(3), 2, 0),
        ("signed halfword elements", Index::Counted, Some(3), 3, 0),
        ("base and bound lose to call-live values", Index::Pressure, None, 0, 0),
    ];
    let mut programs = Vec::new();
    let mut indices = Vec::new();
    for &(_, pattern, _, buffer, base) in &cases {
        let start = if matches!(pattern, Index::NegativeStart) { u32::MAX } else { 0 };
        let mut body = vec![instruction(OP_LOAD, REGION_STATE), 0, 0, 1, NONE,
            OP_CONST, 1, start, OP_CONST, 2, 1, OP_CONST, 3, 4, OP_CONST, 4, 7,
            OP_CONST, 5, 2, OP_CONST, 6, 8, OP_CONST, 7, 5, OP_CONST, 8, 3,
            OP_CONST, 9, i32::MAX as u32, OP_CONST, 27, 0, OP_CONST, 28, 29,
            OP_SET, 0, 1, OP_SET, 1, 3];
        if matches!(pattern, Index::Variable | Index::PoisonedVariable) {
            body.extend([instruction(OP_BIN, BIN_AND_I), 13, 0, 2,
                instruction(OP_BIN, BIN_AND_I), 14, 0, 4, OP_SET, 2, 13, OP_SET, 2, 14]);
            if matches!(pattern, Index::PoisonedVariable) { body.extend([OP_SET, 2, 0]); }
        }
        let mut inner = vec![OP_GET, 10, 0];
        let limit = if matches!(pattern, Index::VaryingLimit) {
            inner.extend([OP_GET, 12, 1]); 12
        } else { 3 };
        inner.extend([instruction(OP_CMP_I, CMP_GE), 11, 10, limit,
            OP_IF, 11, 2, OP_BREAK, 0, 0,
            instruction(OP_BIN, BIN_ADD_I), 15, 10, 2, OP_SET, 0, 15]);
        if matches!(pattern, Index::CounterWrite) { inner.extend([OP_SET, 0, 0]); }
        if matches!(pattern, Index::VaryingLimit) {
            inner.extend([instruction(OP_BIN, BIN_ADD_I), 16, 12, 2, OP_SET, 1, 16]);
        }
        let index = match pattern {
            Index::Counted | Index::NegativeStart | Index::CounterWrite | Index::VaryingLimit => 10,
            Index::Unknown | Index::Pressure => 0,
            Index::Masked => { inner.extend([instruction(OP_BIN, BIN_AND_I), 20, 0, 4]); 20 }
            Index::Affine | Index::FusedAffine => {
                inner.extend([instruction(OP_BIN, BIN_AND_I), 20, 0, 2]);
                if matches!(pattern, Index::FusedAffine) {
                    inner.extend([instruction(OP_FMA, FMA_MUL_ADD_I), 22, 20, 3, 10]);
                } else {
                    inner.extend([instruction(OP_BIN, BIN_MUL_I), 21, 20, 3,
                        instruction(OP_BIN, BIN_ADD_I), 22, 21, 10]);
                }
                22
            }
            Index::Divided => {
                inner.extend([instruction(OP_BIN, BIN_MUL_I), 20, 10, 5,
                    instruction(OP_BIN, BIN_DIV_I), 21, 20, 5]); 21
            }
            Index::OnePast => { inner.extend([instruction(OP_BIN, BIN_ADD_I), 20, 10, 7]); 20 }
            Index::Wrapped => { inner.extend([OP_WRAP, 20, 0, 8]); 20 }
            Index::UnsignedShift => { inner.extend([instruction(OP_BIN, BIN_SHR_UI), 20, 0, 28]); 20 }
            Index::SignedShift => {
                inner.extend([instruction(OP_BIN, BIN_AND_I), 20, 0, 4,
                    instruction(OP_BIN, BIN_SHR_I), 21, 20, 2]); 21
            }
            Index::UnknownSignedShift => { inner.extend([instruction(OP_BIN, BIN_SHR_I), 20, 0, 2]); 20 }
            Index::LeftShift => {
                inner.extend([instruction(OP_BIN, BIN_AND_I), 20, 0, 2,
                    instruction(OP_BIN, BIN_SHL_I), 21, 20, 5]); 21
            }
            Index::Variable | Index::PoisonedVariable => { inner.extend([OP_GET, 20, 2]); 20 }
            Index::Selected => { inner.extend([OP_SEL, 20, 27, 10, 4]); 20 }
            Index::MultiplyOverflow => {
                inner.extend([instruction(OP_BIN, BIN_AND_I), 20, 0, 9,
                    instruction(OP_BIN, BIN_MUL_I), 21, 20, 8]); 21
            }
            Index::AddOverflow => {
                inner.extend([instruction(OP_BIN, BIN_AND_I), 20, 0, 9,
                    instruction(OP_BIN, BIN_MUL_I), 21, 20, 5,
                    instruction(OP_BIN, BIN_ADD_I), 22, 21, 6]); 22
            }
            Index::SignedDivision | Index::SignedMultiply => {
                inner.extend([OP_WRAP, 20, 0, u32::MAX,
                    instruction(OP_BIN, if matches!(pattern, Index::SignedDivision) { BIN_DIV_I } else { BIN_MUL_I }),
                    21, 20, 2]); 21
            }
        };
        // Check stores AND loads in both addressing paths, including FRAME.
        inner.extend([instruction(OP_STORE, REGION_BUF_BASE + buffer), base, 8, index, 0,
            instruction(OP_LOAD, REGION_BUF_BASE + buffer), 24, base, 8, index,
            instruction(OP_STORE, REGION_STATE), 1, 1, NONE, 24,
            instruction(OP_STORE, REGION_STATE), 4, 8, index, 0,
            instruction(OP_LOAD, REGION_STATE), 25, 4, 8, index,
            instruction(OP_STORE, REGION_STATE), 2, 1, NONE, 25,
            instruction(OP_STORE, REGION_FRAME), 0, 8, index, 0,
            instruction(OP_LOAD, REGION_FRAME), 26, 0, 8, index,
            instruction(OP_STORE, REGION_STATE), 3, 1, NONE, 26]);
        if matches!(pattern, Index::Pressure) {
            // Both nonliteral values cross the call and have greater use
            // density than the address and clamp literals. Only a14/a15
            // survive CALL0, so the literals must rematerialize at access.
            for _ in 0..8 {
                inner.extend([instruction(OP_STORE, REGION_STATE), 12, 1, NONE, 0,
                    instruction(OP_STORE, REGION_STATE), 13, 1, NONE, 10]);
            }
            inner.extend([OP_PORT, u32::from(ports::CLOCK_US), 0, 0, 1, 29]);
        }
        body.extend([OP_LOOP, 10, inner.len() as u32]); body.extend(inner);
        let mut types = vec![TY_I32; 30]; types[11] = TY_BOOL; types[27] = TY_BOOL;
        programs.push(Program { types, vars: vec![TY_I32; 3], frame: 8, body });
        indices.push(index);
    }
    let buffers = [(Elem::U16, 8), (Elem::U8, 8), (Elem::Word, 8), (Elem::I16, 8)];
    let encoded = words_with_buffers(&programs, 14, &[], &buffers);
    let m = module::load(&encoded, &chip::profile()).expect("addressing AIR verifies");
    let o = Options { buffers: vec![0xa0000, 0xa0100, 0xa0200, 0xa0300], ..options() };
    let mut oracle = Oracle::new(&m, &o);
    for (k, &(name, pattern, expected_bound, buffer, base)) in cases.iter().enumerate() {
        let steps = steps(&m, k);
        let analysis = Analysis::new(&m, k, &steps);
        let proof = bounds(&m, k, &steps, &analysis);
        assert_eq!(proof[indices[k] as usize], expected_bound, "{name}: u32 bound independently proved from AIR");
        if matches!(pattern, Index::CounterWrite | Index::VaryingLimit) {
            assert!(analysis.counted.iter().all(Option::is_none), "{name}: C1 must not supply a counted-loop fact");
        }
        let mut accesses = 0;
        for &step in &steps {
            let Step::Op(i) = step else { continue; };
            if !matches!(i.op, OP_LOAD | OP_STORE) { continue; }
            let index = i.args[if i.op == OP_LOAD { 3 } else { 2 }];
            if index == NONE { continue; }
            let listing = op_listing(&oracle.code, k, i.at);
            let max = if i.sub >= REGION_BUF_BASE { 7 - base } else { 7 };
            let clamp = expected_bound.is_none_or(|b| b > max);
            assert_eq!(listing.iter().filter(|i| i.op == Op::Minu).count(), usize::from(clamp),
                "{name}: AIR {} retains MINU exactly when its u32 bound exceeds {max} or is unknown", i.at);
            if i.sub >= REGION_BUF_BASE {
                let expected_add = match buffers[buffer as usize].0 {
                    Elem::Word => Op::Addx4, Elem::U8 => Op::AddN, Elem::U16 | Elem::I16 => Op::Addx2,
                };
                assert!(listing.iter().any(|i| i.op == expected_add), "{name}: element-sized address arithmetic");
                if matches!(pattern, Index::Counted | Index::Unknown) {
                    assert_eq!(listing.len(), 2 + usize::from(clamp), "{name}: held base and any needed clamp bound");
                    assert!(!listing.iter().any(|i| matches!(i.op, Op::L32r | Op::Movi | Op::MoviN)),
                        "{name}: access has no base/bound rematerialization");
                }
                if matches!(pattern, Index::Pressure) {
                    assert!(listing.iter().any(|i| i.op == Op::L32r), "{name}: base falls back to a literal load");
                    assert!(listing.iter().any(|i| matches!(i.op, Op::Movi | Op::MoviN) && i.imm == 7),
                        "{name}: clamp bound falls back to an immediate");
                    assert!(!listing.iter().any(|i| matches!(i.op, Op::L32i | Op::S32i) && i.s == 1),
                        "{name}: address literals never use frame slots");
                }
            }
            accesses += 1;
        }
        assert_eq!(accesses, 6, "{name}: both indexed access kinds in buffer/state/frame were checked");
        if matches!(pattern, Index::Counted) {
            let span = oracle.code.map.loops.iter().find(|s| s.entry == k).expect("counted loop span");
            let listing = range_listing(&oracle.code, span.start, span.end);
            let address = o.buffers[buffer as usize] + base * buffers[buffer as usize].0.bytes();
            assert!(!listing.iter().any(|i| i.op == Op::L32r ||
                (matches!(i.op, Op::Movi | Op::MoviN) && i.imm as u32 == address)),
                "{name}: held base is never materialized inside the loop");
        }
        for seed in [0, 99, u32::MAX, i32::MIN as u32, i32::MAX as u32] {
            for (buffer, &(elem, len)) in buffers.iter().enumerate() {
                oracle.put(o.buffers[buffer], &vec![0x5aa5_a55a; (len * elem.bytes() / 4) as usize]);
            }
            let mut initial = vec![0; 14]; initial[0] = seed;
            let (state, _, _) = oracle.run(&m, k, &initial);
            assert_eq!((state[2], state[3]), (seed, seed), "{name}: state and frame round trips, input {seed:#x}");
            if matches!(pattern, Index::Unknown) && seed == 99 {
                assert_eq!(oracle.words(o.buffers[0], 4)[3] >> 16, 99,
                    "unknown loop index: out-of-range 99 writes the last element and equals the reference");
            }
        }
    }
    // Link-time placeholders must not merge bases from different buffers.
    // Use the same fixture steps, with two equal literal bits at one access.
    let steps = steps(&m, 0);
    let at = steps.iter().position(|s| matches!(s, Step::Op(i) if i.op == OP_LOAD && i.sub >= REGION_BUF_BASE)).unwrap();
    let mut uses = vec![Vec::new(); steps.len()];
    uses[at] = vec![(Literal::BufferBase { buffer: 0, element: 0 }, 0),
        (Literal::BufferBase { buffer: 1, element: 0 }, 0)];
    let literals = LoopLiterals::new(&steps, &uses, &[]);
    assert_eq!(literals.intervals.len(), 2, "buffer identities stay distinct before placement");
}

#[test]
fn eleven_register_pool_and_call_intervals_match_exec() {
    use fab_elec_mcu_emit32::control::{steps, Step};

    let mut programs = Vec::new();
    for call in [false, true] {
        let mut body = Vec::new();
        for v in 0..9 { body.extend([instruction(OP_LOAD, REGION_STATE), v, v, 1, NONE]); }
        if call {
            body.extend([OP_PORT, u32::from(ports::SERIAL_BYTE), 0, 1, 1, 6, 11,
                instruction(OP_STORE, REGION_STATE), 20, 1, NONE, 11]);
        }
        body.extend([instruction(OP_BIN, BIN_DIV_I), 9, 6, 7,
            instruction(OP_FMA, FMA_MUL_ADD_I), 10, 6, 7, 8,
            instruction(OP_STORE, REGION_STATE), 18, 1, NONE, 9,
            instruction(OP_STORE, REGION_STATE), 19, 1, NONE, 10]);
        for v in 0..9 { body.extend([instruction(OP_STORE, REGION_STATE), 9 + v, 1, NONE, v]); }
        programs.push(Program { types: vec![TY_I32; 12], vars: vec![], frame: 0, body });
    }
    let encoded = words(&programs, 21, &[]);
    let m = module::load(&encoded, &chip::profile()).expect("pool and port AIR verifies");
    let mut oracle = Oracle::new(&m, &options());
    for entry in 0..2 {
        let mut pool = Vec::new();
        for step in steps(&m, entry) {
            let Step::Op(i) = step else { continue; };
            let listing = op_listing(&oracle.code, entry, i.at);
            if i.op == OP_LOAD {
                let dst = listing.iter().find(|i| i.op == Op::L32i).expect("input load").t;
                if entry == 0 {
                    pool.push(dst);
                    if (6..=8).contains(&i.args[0]) {
                        assert_eq!(dst, [0, 2, 3][i.args[0] as usize - 6], "new volatile register holds its input");
                    }
                } else {
                    assert!(![0, 2, 3].contains(&dst), "call-spanning input stays off a0/a2/a3");
                }
            }
            if entry == 0 && i.op == OP_BIN {
                let divide = listing.iter().find(|i| i.op == Op::Quos).expect("integer quotient");
                assert_eq!((divide.s, divide.t), (0, 2), "divide reads pool operands directly");
                pool.push(divide.r);
            }
            if entry == 0 && i.op == OP_FMA {
                pool.push(listing.iter().find(|i| i.op == Op::Mull).expect("FMA multiply").r);
            }
            if entry == 1 && i.op == OP_STORE && i.args[0] == 20 {
                let src = listing.last().expect("port result store").t;
                assert!(![0, 2, 3].contains(&src), "call result endpoint stays off a0/a2/a3");
            }
        }
        if entry == 0 {
            pool.sort_unstable(); pool.dedup();
            assert_eq!(pool, [0, 2, 3, 4, 5, 6, 7, 9, 10, 14, 15], "all eleven registers carry live values");
            assert_eq!(oracle.code.entries[entry].slot_bytes, 0, "eleven live values need no spills");
        }
        for (dividend, divisor) in [(12345, 37), (i32::MIN as u32, u32::MAX), (12345, 0)] {
            let mut initial: Vec<_> = (0..21).map(|n| 0x1234_5000 + n).collect();
            initial[6] = dividend; initial[7] = divisor; initial[8] = 0x55aa_55aa;
            let (actual, calls, _) = oracle.run(&m, entry, &initial);
            assert_eq!(&actual[9..18], &initial[..9], "all live inputs survive the body and call");
            assert_eq!(calls.len(), entry, "only the clobbering case calls a port");
        }
    }
}

#[test]
fn float_clobbers_exclude_volatile_operands_results_and_spans() {
    use fab_elec_mcu_emit32::control::{steps, Step};

    let mut programs = Vec::new();
    for (op, sub) in [(OP_UN, UN_NEG_F), (OP_BIN, BIN_MUL_F), (OP_CMP_F, CMP_LT)] {
        let mut body = Vec::new();
        for v in 0..9 { body.extend([instruction(OP_LOAD, REGION_STATE), v, v, 1, NONE]); }
        body.extend([instruction(op, sub), 9, 6]);
        if op != OP_UN { body.push(7); }
        body.extend([instruction(OP_STORE, REGION_STATE), 18, 1, NONE, 9]);
        // v6/v7 end at the helper; the other seven values span it.
        for v in (0..9).filter(|v| *v != 6 && (*v != 7 || op == OP_UN)) {
            body.extend([instruction(OP_STORE, REGION_STATE), 9 + v, 1, NONE, v]);
        }
        let mut types = vec![TY_F32; 10];
        if op == OP_CMP_F { types[9] = TY_BOOL; }
        programs.push(Program { types, vars: vec![], frame: 0, body });
    }
    let encoded = words(&programs, 19, &[]);
    let m = module::load(&encoded, &chip::profile()).expect("float call-boundary AIR verifies");
    let mut oracle = Oracle::new(&m, &options());
    for entry in 0..programs.len() {
        for step in steps(&m, entry) {
            let Step::Op(i) = step else { continue; };
            if i.op == OP_LOAD {
                let listing = op_listing(&oracle.code, entry, i.at);
                let dst = listing.iter().find(|i| i.op == Op::L32i).expect("float input load").t;
                assert!([8, 14, 15].contains(&dst), "float operand/span uses preserved storage");
            }
            if i.op == OP_STORE && i.args[0] == 18 {
                let listing = op_listing(&oracle.code, entry, i.at);
                assert!([11, 14, 15].contains(&listing.last().unwrap().t), "float result endpoint uses preserved storage");
            }
        }
        let initial: Vec<_> = (0..19).map(|n| (n as f32 - 7.5).to_bits()).collect();
        oracle.run(&m, entry, &initial);
    }
}

#[test]
fn rematerialized_constants_have_no_slots_and_reload_at_each_use() {
    use fab_elec_mcu_emit32::control::{steps, Step};

    let literals = [37, 1024, 0x1234_5678];
    let programs: Vec<_> = literals.into_iter().map(|literal| {
        let mut body = vec![OP_CONST, 0, literal, instruction(OP_STORE, REGION_STATE), 0, 1, NONE, 0];
        for v in 1..=11 { body.extend([instruction(OP_LOAD, REGION_SHARED), v, v - 1, 1, NONE]); }
        for v in 1..=11 { body.extend([instruction(OP_STORE, REGION_STATE), v, 1, NONE, v]); }
        body.extend([instruction(OP_STORE, REGION_STATE), 12, 1, NONE, 0]);
        Program { types: vec![TY_I32; 12], vars: vec![], frame: 0, body }
    }).collect();
    let shared: Vec<_> = (0..11).map(|v| 0x8000_0000 + v * 7919).collect();
    let encoded = words(&programs, 13, &shared);
    let m = module::load(&encoded, &chip::profile()).expect("rematerialization AIR verifies");
    let mut oracle = Oracle::new(&m, &options());
    for (entry, literal) in literals.into_iter().enumerate() {
        assert_eq!(oracle.code.entries[entry].slot_bytes, 0, "evicted literal consumes no frame slot");
        let mut uses = 0;
        for step in steps(&m, entry) {
            let Step::Op(i) = step else { continue; };
            if i.op == OP_CONST {
                assert!(!oracle.code.map.ops.iter().any(|s| s.entry == entry && s.code == i.at),
                    "evicted constant emits no definition or spill store");
            }
            if i.op == OP_STORE && i.args[3] == 0 {
                let listing = op_listing(&oracle.code, entry, i.at);
                let expected = match literal { 37 => Op::MoviN, 1024 => Op::Movi, _ => Op::L32r };
                assert!(listing.iter().any(|i| i.op == expected &&
                    (if i.op == Op::MoviN { i.s } else { i.t }) == 11), "literal rematerializes at this use");
                assert!(!listing.iter().any(|i| matches!(i.op, Op::L32i | Op::S32i) && i.s == 1),
                    "literal use has no frame traffic");
                uses += 1;
            }
        }
        assert_eq!(uses, 2, "check uses before and after pressure");
        let (actual, _, _) = oracle.run(&m, entry, &[0; 13]);
        assert_eq!((actual[0], actual[12]), (literal, literal), "both rematerialized values are exact");
        assert_eq!(&actual[1..12], shared.as_slice(), "all eleven nonliteral inputs survive pressure");
    }
}

#[test]
fn two_scratch_operands_and_in_place_divide_match_exec_under_pressure() {
    use fab_elec_mcu_emit32::control::{steps, Step};

    let mut body = Vec::new();
    for v in 0..8 { body.extend([instruction(OP_LOAD, REGION_STATE), v, v, 1, NONE]); }
    let mut hot = Vec::new();
    for _ in 0..4 { for v in 0..2 { hot.extend([instruction(OP_STORE, REGION_STATE), v, 1, NONE, v]); } }
    body.extend([OP_LOOP, 2, hot.len() as u32]); body.extend(hot);
    body.extend([
        instruction(OP_BIN, BIN_DIV_I), 8, 2, 3,
        instruction(OP_BIN, BIN_REM_I), 9, 2, 3,
        instruction(OP_FMA, FMA_MUL_ADD_I), 10, 2, 3, 4,
        OP_SEL, 11, 5, 6, 7,
        instruction(OP_CMP_I, CMP_NE), 12, 2, 3,
        instruction(OP_UN, UN_NOT_B), 13, 5,
        instruction(OP_BIN, BIN_SHL_I), 14, 2, 3,
        OP_WRAP, 15, 2, 3,
        OP_PORT, u32::from(ports::CLOCK_US), 0, 0, 1, 16,
    ]);
    for v in 0..17 { body.extend([instruction(OP_STORE, REGION_STATE), 8 + v, 1, NONE, v]); }
    let mut types = vec![TY_I32; 17];
    for v in [5, 12, 13] { types[v] = TY_BOOL; }
    let encoded = words(&[Program { types, vars: vec![], frame: 0, body }], 25, &[]);
    let m = module::load(&encoded, &chip::profile()).expect("two-scratch pressure AIR verifies");
    let mut oracle = Oracle::new(&m, &options());
    let mut divisions = 0;
    for step in steps(&m, 0) {
        let Step::Op(i) = step else { continue; };
        if i.op == OP_BIN && matches!(i.sub, BIN_DIV_I | BIN_REM_I) {
            let listing = op_listing(&oracle.code, 0, i.at);
            let divide = listing.iter().find(|i| matches!(i.op, Op::Quos | Op::Rems)).expect("division instruction");
            assert_eq!((divide.r, divide.s, divide.t), (8, 8, 11), "dividend in destination survives every guard");
            divisions += 1;
        }
    }
    assert_eq!(divisions, 2, "quotient and remainder both exercise destination aliasing");
    for (dividend, divisor) in [(i32::MIN as u32, u32::MAX), (123, 0), (u32::MAX, 3), (23, 7)] {
        for condition in [0, 1] {
            let mut initial = vec![0; 25];
            initial[..8].copy_from_slice(&[17, 19, dividend, divisor, 13, condition, 0x1234, 0x5678]);
            let (actual, _, _) = oracle.run(&m, 0, &initial);
            assert_eq!(&actual[8..16], &initial[..8], "spilled inputs survive arithmetic and the call");
            assert_eq!(actual[19], if condition == 0 { 0x5678 } else { 0x1234 }, "spilled select reads only after its branch");
        }
    }
}

#[test]
fn register_eviction_uses_density_and_literal_fallbacks() {
    use fab_elec_mcu_emit32::regalloc::{allocate, fill, Interval, Location};

    let interval = |start, end, weight| Interval { start, end, weight, crosses_call: false, rematerialize: None };
    let allocation = allocate(&[interval(0, 99, 100), interval(1, 2, 3), interval(3, 4, 2)], &[0], &[]);
    assert_eq!(allocation.locations, [Location::Spill(0), Location::Register(0), Location::Register(0)],
        "short hot value evicts a higher absolute weight; expired registers are reused");
    assert_eq!(allocation.spill_slots, 1, "whole evicted interval gets one slot");
    let allocation = allocate(&[interval(0, 3, 4), interval(1, 2, 2)], &[2], &[]);
    assert_eq!(allocation.locations, [Location::Register(2), Location::Spill(0)], "equal densities favor the earlier interval");
    let allocation = allocate(&[interval(0, u32::MAX, u64::MAX), interval(1, 1, u64::MAX)], &[3], &[]);
    assert_eq!(allocation.locations[1], Location::Register(3), "full-width density products do not overflow");
    let literal = Interval { rematerialize: Some(0x1234_5678), ..interval(0, 99, 1) };
    let allocation = allocate(&[literal, interval(1, 2, 3)], &[0], &[]);
    assert_eq!(allocation.locations, [Location::Rematerialize(0x1234_5678), Location::Register(0)],
        "evicted constant retains its bits instead of a spill location");
    assert_eq!(allocation.spill_slots, 0, "rematerialization allocates no frame slot");
    let call = Interval { crosses_call: true, ..interval(1, 2, 100) };
    let allocation = allocate(&[interval(0, 3, 1), call], &[0, 2, 3], &[14]);
    assert_eq!(allocation.locations[1], Location::Register(14), "call interval uses only a preserved register");
    let allocation = allocate(&[call], &[0, 2, 3], &[]);
    assert_eq!(allocation.locations, [Location::Spill(0)], "call interval cannot evict into a volatile register");
    // Fills never displace an allocated value, however dense: a value used
    // once across the loop keeps a0, and the hot literal rematerializes.
    let values = [interval(0, 9, 1), interval(12, 20, 5)];
    let allocation = allocate(&values, &[0], &[]);
    let hot = Interval { rematerialize: Some(7), ..interval(0, 9, 1000) };
    let gap = Interval { rematerialize: Some(8), ..interval(10, 11, 1) };
    let late = Interval { rematerialize: Some(9), ..interval(11, 12, 1) };
    assert_eq!(fill(&values, &allocation.locations, &[hot, gap, late], &[0], &[]),
        [Location::Rematerialize(7), Location::Register(0), Location::Rematerialize(9)],
        "a fill takes only a register free over its whole inclusive range");
    let dense = Interval { rematerialize: Some(1), ..interval(0, 9, 4) };
    let sparse = Interval { rematerialize: Some(2), ..interval(0, 9, 2) };
    let crossing = Interval { crosses_call: true, ..dense };
    assert_eq!(fill(&[], &[], &[sparse, dense, crossing], &[0], &[14]),
        [Location::Rematerialize(2), Location::Register(0), Location::Register(14)],
        "denser fills choose first; a call-crossing fill takes only a preserved register");
}

#[test]
fn structured_control_flow_loop_liveness_and_caps_match_exec() {
    let c = compile(r#"
let led = output(P13)
let button = input(P2, pull_up)
var acc: i32 = 0
var n: i32 = 0
var data = [int(0); 4]
fn setup() {
    let bias = n + 17
    let local = [int(0); 3]
    for i in 0..5 {
        if i == 1 { continue }
        if i == 4 { break }
        for j in 0..3 {
            if j == 1 { continue }
            acc = acc + bias + i + j
        }
        if n < 0 { if i == 2 { acc = -acc } else { acc = acc ^ bias } }
        else { if i == 0 { acc = acc * 3 } else { acc = acc / 2 } }
        local[i] = acc
        data[n] = local[i]
    }
    n = n + read(button)
    write(led, if n == 0 { high } else { low })
}
"#);
    let m = module::load(&c.module, &chip::profile()).unwrap();
    let mut oracle = Oracle::new(&m, &options());
    let n = m.vars().iter().find(|v| m.string(v.name) == "n").unwrap().offset as usize;
    for initial in [u32::MAX, 0, 1] {
        let mut state = m.state_init().to_vec(); state[n] = initial;
        for _ in 0..3 { state = oracle.run(&m, m.entry("setup").unwrap(), &state).0; }
    }
    let cap = compile("var x: i32 = 0\nfn setup() { while x >= 0 { x = x + 1 } }\n");
    let m = module::load(&cap.module, &chip::profile()).unwrap(); let mut oracle = Oracle::new(&m, &options());
    let (state, calls, _) = oracle.run(&m, m.entry("setup").unwrap(), m.state_init());
    assert_eq!(state, [1024]); assert_eq!(calls.len(), 1); assert_eq!(calls[0].port, ports::LOOP_CAP);
    // AIR's depth operand can target an outer loop. Also cover a zero-cap
    // loop, whose definitions and transfers must not execute at all.
    let programs: Vec<_> = [(OP_BREAK, 4), (OP_CONTINUE, 4), (OP_CONTINUE, 0)].into_iter().map(|(transfer, cap)| {
        let inner = [transfer, 1];
        let mut outer = vec![OP_GET, 1, 0, instruction(OP_BIN, BIN_ADD_I), 2, 1, 0, OP_SET, 0, 2];
        outer.extend([OP_LOOP, 2, inner.len() as u32]); outer.extend(inner);
        let mut body = vec![OP_CONST, 0, 1, OP_LOOP, cap, outer.len() as u32]; body.extend(outer);
        body.extend([OP_GET, 3, 0, instruction(OP_STORE, REGION_STATE), 0, 1, NONE, 3]);
        Program { types: vec![TY_I32; 4], vars: vec![TY_I32], frame: 0, body }
    }).collect();
    let encoded = words(&programs, 1, &[]); let m = module::load(&encoded, &chip::profile()).unwrap();
    let mut oracle = Oracle::new(&m, &options());
    for (k, expected) in [1, 4, 0].into_iter().enumerate() { assert_eq!(oracle.run(&m, k, &[99]).0, [expected]); }
}

#[test]
fn emitted_bounds_cover_structures_and_internal_float_loops() {
    use fab_elec_mcu_emit32::control::{steps, Analysis};
    use fab_elec_mcu_xtensa::insn::target;

    // Constants v0/v1 are zero/one. Canonical counted heads read the
    // induction variable once; the emitter moves their increment to the tail.
    let head = |var, get, test, next, limit| vec![
        OP_GET, get, var, instruction(OP_CMP_I, CMP_GE), test, get, limit,
        OP_IF, test, 2, OP_BREAK, 0, 0,
        instruction(OP_BIN, BIN_ADD_I), next, get, 1, OP_SET, var, next,
    ];
    let mut inner = head(1, 7, 8, 9, 2);
    inner.extend([instruction(OP_LOAD, REGION_STATE), 10, 0, 1, NONE,
        instruction(OP_BIN, BIN_ADD_I), 11, 10, 1,
        instruction(OP_STORE, REGION_STATE), 0, 1, NONE, 11]);
    let mut outer = head(0, 4, 5, 6, 3);
    outer.extend([OP_SET, 1, 0, OP_LOOP, 3, inner.len() as u32]); outer.extend(inner);
    let mut body = vec![OP_CONST, 0, 0, OP_CONST, 1, 1, OP_CONST, 2, 2, OP_CONST, 3, 3,
        OP_SET, 0, 0, OP_LOOP, 4, outer.len() as u32]; body.extend(outer);
    let mut types = vec![TY_I32; 12]; types[5] = TY_BOOL; types[8] = TY_BOOL;
    let nested = Program { types, vars: vec![TY_I32; 2], frame: 0, body };

    // State writes and a tail break prevent counted-loop recognition. It
    // actually exits after two visits, but its emitted bound must use cap 7.
    let inner = [instruction(OP_LOAD, REGION_STATE), 2, 0, 1, NONE,
        instruction(OP_BIN, BIN_ADD_I), 3, 2, 0,
        instruction(OP_STORE, REGION_STATE), 0, 1, NONE, 3,
        instruction(OP_CMP_I, CMP_GE), 4, 3, 1, OP_IF, 4, 2, OP_BREAK, 0, 0];
    let mut body = vec![OP_CONST, 0, 1, OP_CONST, 1, 2, OP_LOOP, 7, inner.len() as u32]; body.extend(inner);
    let capped = Program { types: vec![TY_I32, TY_I32, TY_I32, TY_I32, TY_BOOL], vars: vec![], frame: 0, body };

    let yes = [instruction(OP_LOAD, REGION_STATE), 3, 0, 1, NONE,
        instruction(OP_BIN, BIN_ADD_I), 4, 3, 0,
        instruction(OP_STORE, REGION_STATE), 0, 1, NONE, 4,
        instruction(OP_LOAD, REGION_STATE), 5, 0, 1, NONE,
        instruction(OP_BIN, BIN_MUL_I), 6, 5, 4,
        instruction(OP_STORE, REGION_STATE), 0, 1, NONE, 6];
    let no = [instruction(OP_STORE, REGION_STATE), 0, 1, NONE, 0];
    let mut body = vec![OP_CONST, 0, 1, instruction(OP_LOAD, REGION_STATE), 1, 1, 1, NONE,
        instruction(OP_CMP_I, CMP_EQ), 2, 1, 0, OP_IF, 2, yes.len() as u32];
    body.extend(yes); body.push(no.len() as u32); body.extend(no);
    let mut types = vec![TY_I32; 7]; types[2] = TY_BOOL;
    let arms = Program { types, vars: vec![], frame: 0, body };

    let mut inner = head(0, 3, 4, 5, 2);
    inner.extend([OP_PORT, u32::from(ports::CLOCK_US), 0, 0, 1, 6,
        instruction(OP_STORE, REGION_STATE), 0, 1, NONE, 6]);
    let mut body = vec![OP_CONST, 0, 0, OP_CONST, 1, 1, OP_CONST, 2, 3,
        OP_SET, 0, 0, OP_LOOP, 4, inner.len() as u32]; body.extend(inner);
    let mut types = vec![TY_I32; 7]; types[4] = TY_BOOL;
    let port = Program { types, vars: vec![TY_I32], frame: 0, body };

    let programs = [nested, capped, arms, port,
        operation(OP_BIN, BIN_DIV_F, [TY_F32; 3], TY_F32),
        operation(OP_UN, UN_SQRT_F, [TY_F32; 3], TY_F32)];
    let encoded = words(&programs, 4, &[]);
    let m = module::load(&encoded, &chip::profile()).unwrap();
    let mut oracle = Oracle::new(&m, &options());
    for (k, expected) in [(0, vec![3, 2]), (1, vec![]), (3, vec![3])] {
        let steps = steps(&m, k); let analysis = Analysis::new(&m, k, &steps);
        let trips: Vec<_> = analysis.counted.iter().flatten().map(|c| c.trips).collect();
        assert_eq!(trips, expected, "entry {k}: intended counted/capped structure");
    }
    // Oracle::run compares exec state/ports and asserts instret <= max_insns,
    // printing the final listing on a falsifier. Build one module/model bus.
    assert_eq!(oracle.run(&m, 0, &[0; 4]).0[0], 6);
    assert_eq!(oracle.run(&m, 1, &[0; 4]).0[0], 2);
    assert_eq!(oracle.run(&m, 2, &[2, 0, 0, 0]).0[0], 1);
    assert_eq!(oracle.run(&m, 2, &[2, 1, 0, 0]).0[0], 9);
    assert_eq!(oracle.run(&m, 3, &[0; 4]).1.len(), 3);
    // Smallest subnormal in both operands exercises all 23 normalization
    // shifts, rather than just the short normal path.
    for k in [4, 5] { oracle.run(&m, k, &[1, 1, 0, 0]); }
    for (k, expected) in [(4, 2), (5, 3)] {
        let span = oracle.code.map.ops.iter().find(|s| s.entry == k &&
            m.code()[s.code as usize] & OP_MASK == if k == 4 { OP_BIN } else { OP_UN }).unwrap();
        let mut pc = span.start;
        let mut backward = 0;
        for insn in range_listing(&oracle.code, span.start, span.end) {
            backward += usize::from(target(&insn, pc).is_some_and(|to| (span.start..=pc).contains(&to)));
            pc += decode(insn.bits).unwrap().1;
        }
        assert_eq!(backward, expected, "entry {k}: normalization and sqrt correction backedges are covered");
    }
}

#[test]
fn counted_loops_and_fused_branches_match_exec_and_listing() {
    use fab_elec_mcu_emit32::control::{steps, Analysis, Step};
    use fab_elec_mcu_xtensa::insn::target;

    struct Case {
        name: String,
        program: Program,
        counted: bool,
        counter: bool,
        branch: Option<(Op, bool)>, // expected first branch, followed by a long J
    }
    let mut cases = Vec::new();
    // One module and oracle for all cases. Values 0..3 are preheader
    // constants, 4/5/6 are the canonical GET/compare/increment head.
    for (name, start, bound, cap, sub, change, counted, counter) in [
        ("counted from zero", 0, 4, 5, CMP_GE, 0, true, false),
        ("counted nonzero", 2, 5, 4, CMP_GE, 0, true, false),
        ("counted rematerialized bound", 1021, 1024, 4, CMP_GE, 0, true, false),
        ("counted negative", -3, 0, 4, CMP_GE, 0, true, false),
        ("zero trips equal", 5, 5, 1, CMP_GE, 0, true, false),
        ("zero trips past", 6, 5, 1, CMP_GE, 0, true, false),
        ("mid-body break", 0, 5, 6, CMP_GE, 1, true, false),
        ("var read after break", 0, 5, 6, CMP_GE, 2, false, true),
        ("continue to this loop", 0, 5, 6, CMP_GE, 3, false, true),
        ("second set", 0, 5, 6, CMP_GE, 4, false, true),
        ("short cap retained", 0, 6, 3, CMP_GE, 0, true, true),
        ("cap equals trips", 0, 4, 4, CMP_GE, 0, true, true),
        ("zero cap", 0, 4, 0, CMP_GE, 0, true, false),
        ("inclusive bound", 1, 3, 4, CMP_GT, 0, true, false),
        ("inclusive bound overflow", i32::MAX - 1, i32::MAX, 3, CMP_GT, 0, false, true),
        ("runtime bound", 0, 5, 6, CMP_GE, 5, false, true),
        ("continue from nested loop", 0, 5, 6, CMP_GE, 6, false, true),
        ("nested loop continue stays nested", 0, 5, 6, CMP_GE, 7, true, false),
        ("call in counted body", 0, 5, 6, CMP_GE, 8, true, false),
    ] {
        let mut body = vec![OP_CONST, 0, start as u32, OP_CONST, 1, 1, OP_CONST, 3, 2];
        if change == 5 { body.extend([instruction(OP_LOAD, REGION_STATE), 2, 3, 1, NONE]); }
        else { body.extend([OP_CONST, 2, bound as u32]); }
        body.extend([OP_SET, 0, 0]);
        let mut inner = vec![OP_GET, 4, 0, instruction(OP_CMP_I, sub), 5, 4, 2,
            OP_IF, 5, 2, OP_BREAK, 0, 0, instruction(OP_BIN, BIN_ADD_I), 6, 4, 1, OP_SET, 0, 6];
        if matches!(change, 1..=3) {
            inner.extend([instruction(OP_CMP_I, CMP_EQ), 7, 4, 3,
                OP_IF, 7, 2, if change == 3 { OP_CONTINUE } else { OP_BREAK }, 0, 0]);
        }
        if change == 4 { inner.extend([OP_SET, 0, 2]); }
        if matches!(change, 6 | 7) {
            inner.extend([OP_LOOP, 2, 2, OP_CONTINUE, if change == 6 { 1 } else { 0 }]);
        }
        if change == 8 { inner.extend([OP_PORT, u32::from(ports::CLOCK_US), 0, 0, 1, 11]); }
        inner.extend([instruction(OP_LOAD, REGION_STATE), 8, 0, 1, NONE,
            instruction(OP_BIN, BIN_ADD_I), 9, 8, 4,
            instruction(OP_STORE, REGION_STATE), 0, 1, NONE, 9,
            instruction(OP_STORE, REGION_STATE), 1, 1, NONE, 4]);
        body.extend([OP_LOOP, cap, inner.len() as u32]); body.extend(inner);
        if change == 2 {
            body.extend([OP_GET, 10, 0, instruction(OP_STORE, REGION_STATE), 2, 1, NONE, 10]);
        }
        let mut types = vec![TY_I32; 12]; types[5] = TY_BOOL; types[7] = TY_BOOL;
        cases.push(Case { name: name.into(), program: Program { types, vars: vec![TY_I32], frame: 0, body },
            counted, counter, branch: None });
    }
    let inner = vec![instruction(OP_LOAD, REGION_STATE), 2, 0, 1, NONE,
        instruction(OP_CMP_I, CMP_GE), 3, 2, 1, OP_IF, 3, 2, OP_BREAK, 0, 0,
        instruction(OP_BIN, BIN_ADD_I), 4, 2, 0, instruction(OP_STORE, REGION_STATE), 0, 1, NONE, 4];
    let mut body = vec![OP_CONST, 0, 1, OP_CONST, 1, 4, OP_LOOP, 6, inner.len() as u32]; body.extend(inner);
    cases.push(Case { name: "while runtime condition".into(),
        program: Program { types: vec![TY_I32, TY_I32, TY_I32, TY_BOOL, TY_I32], vars: vec![], frame: 0, body },
        counted: false, counter: true, branch: None });

    // AIR's CMP_I is signed only. Cover all six comparisons with two registers,
    // zero and table immediates, and constants on the left (operand reversal).
    for (sub, pair, zero, immediate) in [
        (CMP_LT, Op::Bge, Op::Bgez, Op::Bgei),
        (CMP_LE, Op::Blt, Op::Bgei, Op::Bgei),
        (CMP_GT, Op::Bge, Op::Blti, Op::Blti),
        (CMP_GE, Op::Blt, Op::Bltz, Op::Blti),
        (CMP_EQ, Op::Bne, Op::Bnez, Op::Bnei),
        (CMP_NE, Op::Beq, Op::Beqz, Op::Beqi),
    ] {
        for (form, constant, swapped, expected) in [
            ("registers", None, false, pair),
            ("zero", Some(0), false, zero),
            ("b4const", Some(1), false, immediate),
            ("swap registers", None, true, pair),
        ] {
            let mut body = vec![instruction(OP_LOAD, REGION_STATE), 0, 0, 1, NONE];
            if let Some(c) = constant { body.extend([OP_CONST, 1, c]); }
            else { body.extend([instruction(OP_LOAD, REGION_STATE), 1, 3, 1, NONE]); }
            body.extend([OP_CONST, 2, 17, OP_CONST, 3, 23,
                instruction(OP_CMP_I, sub), 4, if swapped { 1 } else { 0 }, if swapped { 0 } else { 1 },
                OP_IF, 4, 5, instruction(OP_STORE, REGION_STATE), 1, 1, NONE, 2,
                5, instruction(OP_STORE, REGION_STATE), 1, 1, NONE, 3]);
            cases.push(Case { name: format!("compare {sub} {form}"),
                program: Program { types: vec![TY_I32, TY_I32, TY_I32, TY_I32, TY_BOOL], vars: vec![], frame: 0, body },
                counted: false, counter: false, branch: Some((expected, false)) });
        }
    }
    // Every signed table entry, zero, unencodable constants, and both signed
    // boundaries; these also cover the constant-on-left immediate selection.
    for c in [-1i32, 0, 1, 2, 3, 4, 5, 6, 7, 8, 10, 12, 16, 32, 64, 128, 256, 999, i32::MIN, i32::MAX] {
        let body = vec![instruction(OP_LOAD, REGION_STATE), 0, 0, 1, NONE,
            OP_CONST, 1, c as u32, OP_CONST, 2, 17, OP_CONST, 3, 23,
            instruction(OP_CMP_I, CMP_EQ), 4, 1, 0,
            OP_IF, 4, 5, instruction(OP_STORE, REGION_STATE), 1, 1, NONE, 2,
            5, instruction(OP_STORE, REGION_STATE), 1, 1, NONE, 3];
        let expected = if c == 0 { Op::Bnez } else if enc::b4const(c).is_some() { Op::Bnei } else { Op::Bne };
        cases.push(Case { name: format!("constant on left {c}"),
            program: Program { types: vec![TY_I32, TY_I32, TY_I32, TY_I32, TY_BOOL], vars: vec![], frame: 0, body },
            counted: false, counter: false, branch: Some((expected, false)) });
    }
    for adjacent in [true, false] {
        let mut body = vec![instruction(OP_LOAD, REGION_STATE), 0, 0, 1, NONE,
            OP_CONST, 1, 0, OP_CONST, 2, 17, instruction(OP_CMP_I, CMP_LT), 3, 0, 1];
        if !adjacent { body.extend([instruction(OP_STORE, REGION_STATE), 2, 1, NONE, 0]); }
        body.extend([OP_IF, 3, 5, instruction(OP_STORE, REGION_STATE), 1, 1, NONE, 2, 0]);
        if adjacent { body.extend([instruction(OP_STORE, REGION_STATE), 2, 1, NONE, 3]); }
        cases.push(Case { name: if adjacent { "unfused multiple uses" } else { "unfused nonadjacent" }.into(),
            program: Program { types: vec![TY_I32, TY_I32, TY_I32, TY_BOOL], vars: vec![], frame: 0, body },
            counted: false, counter: false, branch: None });
    }
    for (name, constant, expected) in [("far B format", 1, Op::Beqi), ("far BZ format", 0, Op::Beqz)] {
        let mut then = Vec::new();
        for _ in 0..512 { then.extend([instruction(OP_STORE, REGION_STATE), 1, 1, NONE, 2]); }
        let mut body = vec![instruction(OP_LOAD, REGION_STATE), 0, 0, 1, NONE,
            OP_CONST, 1, constant, OP_CONST, 2, 17, OP_CONST, 3, 23,
            instruction(OP_CMP_I, CMP_EQ), 4, 0, 1, OP_IF, 4, then.len() as u32];
        body.extend(then); body.extend([5, instruction(OP_STORE, REGION_STATE), 1, 1, NONE, 3]);
        cases.push(Case { name: name.into(),
            program: Program { types: vec![TY_I32, TY_I32, TY_I32, TY_I32, TY_BOOL], vars: vec![], frame: 0, body },
            counted: false, counter: false, branch: Some((expected, true)) });
    }
    let programs: Vec<_> = cases.iter_mut().map(|c| std::mem::replace(&mut c.program,
        Program { types: vec![], vars: vec![], frame: 0, body: vec![] })).collect();
    let encoded = words(&programs, 4, &[]);
    let m = module::load(&encoded, &chip::profile()).expect("C1 cases verify");
    let mut oracle = Oracle::new(&m, &options());
    for (k, case) in cases.iter().enumerate() {
        let name = &case.name;
        let steps = steps(&m, k); let analysis = Analysis::new(&m, k, &steps);
        assert_eq!(analysis.counted.iter().any(Option::is_some), case.counted, "{name}: counted recognition");
        if case.branch.is_some() {
            assert_eq!(analysis.fused.iter().filter(|c| c.is_some()).count(), 1, "{name}: compare must fuse");
        }
        let ops: Vec<_> = oracle.code.map.ops.iter().filter(|s| s.entry == k).collect();
        let loop_op = steps.iter().find_map(|s| match s { Step::Op(i) if i.op == OP_LOOP => Some(i.at), _ => None });
        assert_eq!(loop_op.is_some_and(|at| ops.iter().any(|s| s.code == at)), case.counter,
            "{name}: cap counter initialization in listing");
        for (n, step) in steps.iter().enumerate() {
            let Step::Op(i) = step else { continue; };
            if analysis.counted[n].is_some() {
                for step in &steps[n + 1..n + 9] {
                    if let Step::Op(head) = step {
                        assert!(!ops.iter().any(|s| s.code == head.at), "{name}: head has no copy, test, or increment");
                    }
                }
            }
            if i.op == OP_CMP_I && analysis.omit[n] {
                assert!(!ops.iter().any(|s| s.code == i.at), "{name}: fused compare builds no boolean");
            }
            if name.starts_with("unfused") && i.op == OP_CMP_I {
                assert!(ops.iter().any(|s| s.code == i.at), "{name}: boolean remains materialized");
                assert!(analysis.fused.iter().all(Option::is_none), "{name}: compare must not fuse");
            }
            if i.op == OP_IF && analysis.fused[n].is_some() {
                if case.branch.is_some() { assert!(ops.iter().any(|s| s.code == i.at), "{name}: fused branch is emitted"); }
                if let Some(span) = ops.iter().find(|s| s.code == i.at) {
                    let mut pc = span.start;
                    let mut listing = Vec::new();
                    while pc < span.end {
                        let bytes = &oracle.mem.bytes(oracle.ram)[pc as usize..pc as usize + 3];
                        let bits = u32::from(bytes[0]) | u32::from(bytes[1]) << 8 | u32::from(bytes[2]) << 16;
                        let (insn, len) = decode(bits).unwrap(); listing.push(insn); pc += len;
                    }
                    assert!(!listing.iter().any(|i| matches!(i.op, Op::Salt | Op::Movi | Op::MoviN)),
                        "{name}: IF uses operands directly");
                    if let Some((expected, long)) = case.branch {
                        assert_eq!(listing.len(), if long { 2 } else { 1 }, "{name}: branch width");
                        assert_eq!(listing[0].op, expected, "{name}: branch instruction");
                        if long {
                            assert_eq!(listing[1].op, Op::J, "{name}: far target uses J");
                            assert_eq!(target(&listing[0], span.start), Some(span.end), "{name}: inverted condition skips J");
                        }
                    }
                }
            }
        }
        // Both outcomes, negative values, and signed extrema; every run also
        // compares all state and calls with exec using the same dirty stack.
        for seed in [i32::MIN as u32, u32::MAX, 0, 1, 2, 4, 128, 256, i32::MAX as u32] {
            let (state, _, _) = oracle.run(&m, k, &[seed, 99, 99, 4]);
            if name == "zero trips equal" || name == "zero trips past" || name == "zero cap" {
                assert_eq!(state, [seed, 99, 99, 4], "{name}: body skipped");
            }
            if name == "var read after break" { assert_eq!(state[2], 3, "{name}: head increment visible after break"); }
        }
        let (start, end) = oracle.code.map.entries[k];
        let mut pc = start;
        let mut in_place_increment = false;
        while pc < end {
            let b = &oracle.mem.bytes(oracle.ram)[pc as usize..pc as usize + 3];
            let (insn, len) = decode(u32::from(b[0]) | u32::from(b[1]) << 8 | u32::from(b[2]) << 16).unwrap();
            in_place_increment |= insn.op == Op::Addi && insn.imm == 1 && insn.s == insn.t;
            if insn.op == Op::J { assert_ne!(target(&insn, pc), Some(pc + len), "{name}: no J to next instruction"); }
            pc += len;
        }
        if case.counted && name != "zero cap" && analysis.counted.iter().flatten().any(|c| c.trips != 0) {
            assert!(in_place_increment, "{name}: tail increments the aliased variable in place");
        }
    }
}

#[test]
fn refusals_name_the_air_site_and_link_limits() {
    let c = compile("var stamp: i32 = 0\nfn setup() { stamp = now_us() }\n");
    let m = module::load(&c.module, &chip::profile()).unwrap();
    let refused = build_handlers(&m, &Options::default()).unwrap_err();
    assert!(refused.message.contains("clock_us") && refused.message.contains("site") && refused.message.contains("setup"));
    assert_eq!(m.code()[refused.code as usize] & OP_MASK, OP_PORT);
    for o in [Options { base: BASE + 1, ..options() }, Options { base: 0xffff_fffc, ..options() },
        Options { ports: vec![(ports::CLOCK_US, 3)], ..options() },
        Options { ports: vec![(ports::CLOCK_US, 4), (ports::CLOCK_US, 8)], ..options() }] {
        assert!(build_handlers(&m, &o).is_err());
    }
    let p = Program { types: vec![TY_F64], vars: vec![], frame: 0, body: vec![OP_CONST_D, 0, 0, 0x3ff0_0000] };
    let encoded = words(&[p], 0, &[]); let profile = fab_elec_mcu_rt::Profile { f64: true, ..chip::profile() };
    let m = module::load(&encoded, &profile).unwrap(); let refused = build_handlers(&m, &options()).unwrap_err();
    assert_eq!(refused.code, m.entries()[0].body + 1);
    assert!(refused.message.contains("f64 is not available on the ESP32-S3 yet"));
}

// Read the firmware literals from the language tests themselves, without
// importing those test targets (and their unrelated helpers/dependencies).
// Both Rust string forms used by those fixtures are supported, fail closed
// on an unexpected escape, and require each marker to remain in its source.
fn fixture(file: &str, marker: &str) -> String {
    let start = file.find(marker).unwrap_or_else(|| panic!("missing language fixture {marker}"));
    let tail = &file[start..]; let quote = tail.find('"').expect("fixture string");
    if tail[..quote].ends_with("r#") {
        return tail[quote + 1..].split_once("\"#").expect("raw fixture terminator").0.to_owned();
    }
    let mut chars = tail[quote + 1..].chars().peekable(); let mut out = String::new();
    while let Some(c) = chars.next() {
        match c {
            '"' => return out,
            '\\' => match chars.next().expect("fixture escape") {
                'n' => out.push('\n'), 'r' => out.push('\r'), 't' => out.push('\t'),
                '\\' => out.push('\\'), '"' => out.push('"'),
                '\n' => { while chars.peek().is_some_and(|c| c.is_whitespace()) { let _ = chars.next(); } }
                c => panic!("unsupported fixture escape {c:?}"),
            },
            c => out.push(c),
        }
    }
    panic!("unterminated fixture {marker}")
}

#[test]
fn language_test_programs_compile_with_the_s3_profile_and_match_exec() {
    const BLINK: &str = include_str!("../../lang/tests/blink.rs");
    const MODULE: &str = include_str!("../../lang/tests/module.rs");
    const PWM: &str = include_str!("../../lang/tests/pwm.rs");
    const SERIAL: &str = include_str!("../../lang/tests/serial.rs");
    let bench = fixture(MODULE, "fn bench_constants_replace_literals_and_report_all_invalid_overrides");
    // The positive firmware fixtures from all four language test targets.
    // The f64 fixture is submitted too, to make its profile refusal explicit.
    let mut programs = vec![
        ("blink", fixture(include_str!("../../lang/tests/support/blink.rs"), "const BLINK")),
        ("busy", fixture(BLINK, "const BUSY")),
        ("loop cap", fixture(BLINK, "let src = \"var x: i32")),
        ("statement marks", fixture(MODULE, "let source =")),
        ("bench constants", bench.clone()),
        ("bench period edit", bench.replace("125", "20")),
        ("bench value edit", bench.replace("= -1", "= -2147483648").replace("= 1.5", "= -2.25")),
        ("f64 module", fixture(MODULE, "const SOURCE:")),
        ("pwm six pins", fixture(PWM, "let c = compiled(\"\\")),
        ("pwm ramp", fixture(PWM, "const RAMP:")),
        ("pwm dynamic misuse", fixture(PWM, "let src =")),
        ("serial bytes", fixture(SERIAL, "const BYTES:")),
        ("serial busy timer", fixture(SERIAL, "const BUSY_TIMER:")),
        ("serial busy byte", fixture(SERIAL, "const BUSY_BYTE:")),
        ("serial two links", fixture(SERIAL, "let src =")),
        ("serial dynamic misuse", fixture(SERIAL, "let src = \"\\\nvar last:")),
    ];
    // The short, successful inline programs from pwm.rs and serial.rs;
    // repeated compilations of the same program are represented once.
    for (name, source) in [
        ("pwm free top", "let a = pwm(D9, hz(25000))\nlet b = pwm(D10, hz(25500))\nfn setup() {}\n"),
        ("pwm prescale", "let a = pwm(D9, 100050)\nfn setup() {}\n"),
        ("pwm tie", "let a = pwm(D9, 99505)\nfn setup() {}\n"),
        ("pwm fast", "let a = pwm(D9, 3200)\nfn setup() {}\n"),
        ("pwm tolerance", "let a = pwm(D11, hz(505))\nfn setup() {}\n"),
        ("pwm numeric pin", "let a = pwm(9, hz(490))\nfn setup() {}\n"),
        ("serial 19200", "let link = serial(19200, fn() {})\n"),
        ("serial 250000", "let link = serial(250000, fn() {})\n"),
        ("empty setup", "fn setup() {}\n"),
        ("serial and timer", "let link = serial(19200, fn() {})\nlet timer = after(us(1), fn() {})\n"),
    ] { programs.push((name, source.to_owned())); }
    let names = pin_names(); let pins: Vec<_> = names.iter().map(|(n, v)| (n.as_str(), *v)).collect();
    let mut accepted = Vec::new();
    for (name, source) in programs {
        match compile_with(&source, &chip::profile(), &pins, &[], &[]) {
            Ok(c) => { replay_compiled(&c, 2); accepted.push(name); }
            Err(fab_elec_mcu::CompileError::Source(errors)) => {
                // Only profile refusals justify a skip. No generator failure
                // or unexpected source diagnostic can silently lose coverage.
                assert!(!errors.is_empty() && errors.iter().all(|e|
                    e.message.contains("has no PWM output") || e.message.contains("0 serial links")
                        || e.message.contains("no 64-bit floats") || e.message.contains("operations")),
                    "{name}: unexpected refusal {errors:?}");
            }
            Err(e) => panic!("{name}: unexpected bench error {e:?}"),
        }
    }
    for name in ["blink", "busy", "loop cap", "statement marks", "bench constants", "bench period edit", "bench value edit", "empty setup"] {
        assert!(accepted.contains(&name), "required language program {name} must compile and execute");
    }
    for overrides in [vec![("chase_ms", 20.0)], vec![("signed", i32::MIN as f64), ("gain", -2.25)]] {
        let c = compile_with(&bench, &chip::profile(), &pins, &overrides, &[]).expect("valid language-test bench overrides");
        replay_compiled(&c, 2);
    }
}

#[test]
fn shared_functions_are_refused_before_building_an_image() {
    use fab_elec_mcu_emit_s3::{build_image, Board, BoardTime, VOICEBOX};
    let profile = fab_elec_mcu_rt::Profile { call_ops: Some(0), max_entry_ops: 100_000, ..chip::profile() };
    let c = fab_elec_mcu::compile(include_str!("../../lang/tests/support/shared.splash"), &profile, &[("P0", 0), ("P1", 1)], &[]).unwrap();
    let m = module::load(&c.module, &profile).unwrap();
    assert!(!m.funcs().is_empty());
    // Deliberately unusable timing: the function refusal must happen first.
    let board = Board { layout: VOICEBOX, time: BoardTime { rc_slow_hz: (0, 0), wdt_delay_sel: 0 },
        panel: None, i2c: None, motion: None, touch: None, radio: None };
    for error in [
        build_image(&m, &board, &SlotOptions::default()).err().expect("S3 image refuses calls"),
        build_slot(&m, &SlotOptions::default()).err().expect("S3 slot refuses calls"),
        build_handlers(&m, &Options::default()).err().expect("S3 handlers refuse calls"),
    ] {
        assert_eq!(error.message, fab_elec_mcu_emit_s3::image::FUNCTIONS_UNSUPPORTED);
    }
    assert_eq!(chip::profile().call_ops, None);
}
