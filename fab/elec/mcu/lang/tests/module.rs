use fab_elec_mcu::{compile, compile_with, CompileError, Profile, PORTS};
use fab_elec_mcu_rt as rt;
use fab_elec_mcu_rt::{exec::{self, Exit, PortCall}, format::*, module::{self, LoadError, Module}, ports};
use makepad_script_compute::ir;
use std::ops::Range;

const MARK_PROFILE: Profile = Profile { id: 3, pins: 20, max_entry_ops: 50_000, call_ops: None, ram_words: 2048, buffer_bytes: 0, eeprom: 0, eeprom_write_ns: 0,
    f64: false, op_ns: 62, clock_hz: 16_000_000, analog: &rt::analog::NONE, pwm: &rt::pwm::ATMEGA328P, serial: &rt::serial::ATMEGA328P, motion: None, touch: false, audio: None, display: None };

#[test]
fn statement_table_tracks_user_statements_and_inlined_copies() {
    let source = "\
let led = output(P0)
var count: i32 = 0
fn bump() {
    count = count + 1
}
fn setup() {
    let unused = 7
    write(led, low)
    bump()
}
let tick = every(ms(1), fn() {
    bump()
    write(led, high)
})
let quiet = after(ms(2), fn() {
    for n in 0..9 {
        let gone = n
    }
})
";
    let c = compile(source, &MARK_PROFILE, &[("P0", 13)], &[]).unwrap_or_else(|e| panic!("statement source: {e:?}"));
    assert_eq!(c.stmts.iter().map(|s| (s.site.line, s.owner)).collect::<Vec<_>>(),
        [(7, 0), (8, 0), (9, 0), (4, 0), (12, 1), (4, 1), (13, 1), (16, 2), (17, 2)].map(|(line, k)| (line, fab_elec_mcu::Owner::Handler(k))),
        "user statements retain lowering order and entry ownership; prelude helpers add none");
    assert_eq!(c.stmts.len(), c.lowered.marks.len(), "every allocated mark has one statement table row");
    for (id, info) in c.stmts.iter().enumerate() {
        let span = c.lowered.marks[id];
        assert_eq!((info.site.start, info.site.end), (span.start as u32, span.end as u32), "statement {id} retains its byte span");
        assert_eq!(info.site.col, if info.site.line == 17 { 9 } else { 5 }, "statement {id} retains its 1-based column");
        assert!(c.lowered.entries[match info.owner { fab_elec_mcu::Owner::Handler(k) => k as usize, _ => panic!("inlined statement") }].marks.contains(&(id as u32)), "statement {id} belongs to its lowered entry");
    }
    assert!(!c.marks.is_empty(), "statement marks reach the encoder");
    // What a stepper needs: each statement marked once, where it begins, in the code's order; at
    // one position the statements beginning there, outermost first (whatever their ids).
    let mut ids: Vec<u32> = c.marks.iter().map(|&(_, id)| id).collect();
    ids.sort_unstable();
    assert!(ids.windows(2).all(|w| w[0] != w[1]), "each statement is marked once: {:?}", c.marks);
    assert!(c.marks.windows(2).all(|w| w[0].0 <= w[1].0), "marks follow the code's order: {:?}", c.marks);
    let at = |id: u32| c.marks.iter().find(|&&(_, mark)| mark == id).map(|&(at, _)| at);
    let order = |id: u32| c.marks.iter().position(|&(_, mark)| mark == id);
    for id in 1..=6 {
        assert!(at(id).is_some(), "statement {id} has code and is marked where it begins: {:?}", c.marks);
    }
    assert_eq!(at(0), at(1), "the codeless literal keeps its mark, at the following statement's position");
    for (call, inlined) in [(2, 3), (4, 5)] {
        assert_eq!(at(call), at(inlined), "call {call} and the first statement of its inlined helper begin together");
        assert!(order(call) < order(inlined), "call {call} is listed before the helper statement it contains");
    }
    let mut plain = c.lowered;
    fn strip(body: &mut Vec<ir::Stmt>) {
        body.retain(|s| !matches!(s, ir::Stmt::Mark(_)));
        for s in body {
            match s {
                ir::Stmt::If(_, t, e) => { strip(t); strip(e); }
                ir::Stmt::Loop { body, .. } => strip(body),
                _ => {}
            }
        }
    }
    for entry in &mut plain.entries { strip(&mut entry.program.body); }
    assert_eq!(fab_elec_mcu::encode::encode(&plain, MARK_PROFILE.id).expect("encode without marks"), c.module,
        "debug marks do not change module words or format");
}

#[test]
fn bench_constants_replace_literals_and_report_all_invalid_overrides() {
    let source = "\
let chase_ms = 125
let signed = -1
let gain = 1.5
let calculated = 1 + 2
var count: i32 = 0
var sample: f32 = 0.0
fn setup() {
    let nested = 1
    count = signed
    sample = gain
}
let tick = every(ms(chase_ms), fn() { count = count + 1 })
";
    let overridden = compile_with(source, &MARK_PROFILE, &[], &[("chase_ms", 20.0)], &[])
        .unwrap_or_else(|e| panic!("valid bench override: {e:?}"));
    let edited = compile(&source.replace("125", "20"), &MARK_PROFILE, &[], &[]).unwrap_or_else(|e| panic!("edited literal: {e:?}"));
    assert_eq!(overridden.module, edited.module, "bench constant is equivalent to editing the firmware literal, including ms()");
    let overridden = compile_with(source, &MARK_PROFILE, &[], &[("signed", i32::MIN as f64), ("gain", -2.25)], &[])
        .unwrap_or_else(|e| panic!("negative integer and fractional float overrides: {e:?}"));
    let edited = compile(&source.replace("= -1", "= -2147483648").replace("= 1.5", "= -2.25"), &MARK_PROFILE, &[], &[])
        .unwrap_or_else(|e| panic!("edited negative literals: {e:?}"));
    assert_eq!(overridden.module, edited.module, "negative replacements preserve literal kind and integer boundary");

    for (overrides, name, at) in [
        (vec![("missing", 1.0)], "missing", false),
        (vec![("calculated", 1.0)], "calculated", true),
        (vec![("chase_ms", 1.5)], "chase_ms", true),
        (vec![("chase_ms", 20.0), ("chase_ms", 30.0)], "chase_ms", true),
        (vec![("nested", 1.0)], "nested", false),
        (vec![("high", 1.0)], "high", false),
        (vec![("signed", i32::MAX as f64 + 1.0)], "signed", true),
        (vec![("signed", i32::MIN as f64 - 1.0)], "signed", true),
        (vec![("gain", f64::NAN)], "gain", true),
        (vec![("gain", f64::INFINITY)], "gain", true),
        (vec![("chase_ms", f64::NEG_INFINITY)], "chase_ms", true),
    ] {
        let errors = match compile_with(source, &MARK_PROFILE, &[], &overrides, &[]) {
            Err(CompileError::Consts(errors)) => errors,
            Err(CompileError::Source(errors)) => panic!("{name}: override error escaped into firmware diagnostics: {errors:?}"),
            Ok(_) => panic!("{name}: invalid override compiled"),
        };
        assert_eq!(errors.len(), 1, "{name}: each failing override reports one error");
        assert_eq!(errors[0].name, name, "{name}: the bench can attribute its parameter error");
        assert!(errors[0].message.contains(name), "{name}: its diagnostic names the override");
        let span = at.then(|| {
            let start = source.find(&format!("let {name} =")).expect("top-level let");
            let end = start + source[start..].find('\n').expect("let ends at newline");
            (start as u32, end as u32)
        });
        assert_eq!(errors[0].at, span, "{name}: the error points at the whole firmware let only when top level");
    }
    let errors = match compile_with(source, &MARK_PROFILE, &[], &[("missing", 1.0), ("calculated", 2.0)], &[]) {
        Err(CompileError::Consts(errors)) => errors,
        _ => panic!("two invalid bench parameters must report Consts together"),
    };
    assert_eq!(errors.iter().map(|e| e.name.as_str()).collect::<Vec<_>>(), ["missing", "calculated"],
        "validation reports all failures in override order");
}

const SOURCE: &str = r#"
let led = output(P0)
let button = input(P1, pull_up)
let weights = [0.25, 1.5, 2.75, 4.0, 5.25]
var ticks: i32 = 3
var acc = 0.125
var on = false

fn step() {
    let sample = read(button)
    let local = [0.0; 3]
    for k in 0..(ticks % 5 + 1) {
        if k == 1 { continue }
        local[k] = float(k + read(button)) * 0.25
    }
    let n = 0
    while n < 3 {
        if n == 2 { break }
        n += 1
    }
    on = !on
    let z = f64(acc) + f64(sample) * 0.125
    let q = sqrt(abs(z) + 1.0) + floor(z) + ceil(z) + trunc(z) + round(z)
    if on && sample != 0 {
        acc = float(q) + weights[ticks - 9] + local[ticks]
    } else {
        acc = acc * 0.5 - local[0]
    }
    ticks = (ticks * 2 + sample + int(n)) % 7 + 1
    write(led, if on { high } else { low })
}

fn setup() { step() }
let tick = every(us(100), fn() { step() })
"#;

#[derive(Default)]
struct Trace {
    calls: Vec<(u16, u32, Vec<u32>)>,
    ops: Vec<u64>,
    input: u32,
    suspend: bool,
    skip_reads: usize,
}

impl Trace {
    fn record(&mut self, port: u16, site: u32, args: &[u32], rets: &mut [u32]) {
        assert!(rets.iter().all(|w| *w == 0), "port results must arrive zeroed");
        self.calls.push((port, site, args.to_vec()));
        if port == ports::PIN_READ { rets[0] = self.input; }
    }
}

impl ir::Ports for Trace {
    fn call(&mut self, port: u16, site: u32, args: &[u32], rets: &mut [u32]) { self.record(port, site, args, rets); }
}

impl exec::Ports for Trace {
    fn call(&mut self, port: u16, site: u32, ops: u64, args: &[u32], rets: &mut [u32]) -> PortCall {
        self.record(port, site, args, rets);
        self.ops.push(ops);
        if port == ports::PIN_READ && self.suspend && self.skip_reads == 0 {
            self.suspend = false;
            // A suspended call's immediate results must not be committed.
            rets.fill(u32::MAX);
            PortCall::Suspend
        } else {
            if port == ports::PIN_READ && self.suspend { self.skip_reads -= 1; }
            PortCall::Done
        }
    }
}

fn section(words: &[u32], kind: u32) -> Range<usize> {
    words[HEADER_WORDS..HEADER_WORDS + words[H_SECTIONS] as usize * DIRECTORY_WORDS]
        .chunks_exact(DIRECTORY_WORDS)
        .find_map(|d| (d[0] == kind).then(|| d[1] as usize..(d[1] + d[2]) as usize))
        .expect("required section")
}

// Locate statements by their encoded lengths, including both branches and
// loop bodies. Tests never depend on the lowerer's instruction ordering.
fn statements(words: &[u32], m: &Module<'_>) -> Vec<usize> {
    let code = section(words, SEC_CODE).start;
    let mut blocks: Vec<usize> = m.entries().iter().chain(m.funcs().iter().map(|f| &f.program)).map(|e| code + e.body as usize).collect();
    let mut out = Vec::new();
    while let Some(block) = blocks.pop() {
        let mut pc = block + 1;
        let end = pc + words[block] as usize;
        while pc < end {
            out.push(pc);
            let op = words[pc] & OP_MASK;
            pc += match op {
                OP_CONST | OP_GET | OP_UN | OP_SET => 3,
                OP_CONST_D | OP_BIN | OP_CMP_F | OP_CMP_I | OP_CMP_D | OP_WRAP => 4,
                OP_LOAD | OP_STORE => 5 + usize::from(words[pc] >> SUB_SHIFT == REGION_BUF_LONG),
                OP_SEL | OP_FMA => 5,
                OP_BREAK | OP_CONTINUE => 2,
                OP_PORT => 5 + words[pc + 3] as usize + words[pc + 4] as usize,
                OP_CALL => 4 + words[pc + 2] as usize + words[pc + 3] as usize,
                OP_IF => {
                    let t = pc + 2;
                    let e = t + 1 + words[t] as usize;
                    blocks.extend([t, e]);
                    e + 1 + words[e] as usize - pc
                }
                OP_LOOP => {
                    let b = pc + 2;
                    blocks.push(b);
                    3 + words[b] as usize
                }
                _ => panic!("unexpected opcode in good module"),
            };
        }
        assert_eq!(pc, end, "statement walk consumes its block");
    }
    out
}

fn done(exit: Exit) -> u64 {
    match exit { Exit::Done { ops } => ops, Exit::Suspended { .. } => panic!("unexpected suspension") }
}

#[test]
fn modules_load_run_and_refuse() {
    let profile = Profile { id: 7, pins: 32, max_entry_ops: 100_000, call_ops: None, ram_words: 1 << 16, buffer_bytes: 0, eeprom: 0, eeprom_write_ns: 0, f64: true, op_ns: 100, clock_hz: 16_000_000, analog: &rt::analog::NONE, pwm: &rt::pwm::NONE, serial: &rt::serial::NONE, motion: None, touch: false, audio: None, display: None };
    let compiled = compile(SOURCE, &profile, &[("P0", 0), ("P1", 1)], &[]).unwrap_or_else(|e| panic!("compile: {e:?}"));
    let words = &compiled.module;
    let lowered = &compiled.lowered;
    let m = module::load(words, &profile).expect("valid module");
    assert_eq!(m.entries().len(), 2);
    assert_eq!(m.state_init(), lowered.state_init);
    assert_eq!(m.shared(), lowered.shared_init);
    assert_eq!(m.n_sites() as usize, lowered.sites.len());
    assert!(m.entries().iter().all(|e| e.wide && e.frame_words > 0));
    let mut actual = m.state_init().to_vec();
    let mut expected = lowered.state_init.clone();
    for round in 0..4 {
        for (k, entry) in lowered.entries.iter().enumerate() {
            assert_eq!(m.entry(&entry.name), Some(k));
            ir::validate_ports(&entry.program, &lowered.regions(entry), PORTS).unwrap();
            let mut host = Trace { input: round % 2, ..Trace::default() };
            let mut device = Trace { input: round % 2, ..Trace::default() };
            let mut reference_scratch = vec![0; entry.program.scratch_words()];
            ir::run_ports(&entry.program, &mut reference_scratch, &mut ir::Mem {
                ctx: &mut [], state: &mut expected, shared: ir::Shared::Read(&lowered.shared_init), bufs: &[],
            }, &mut ir::Io::default(), 1, &mut host);
            let mut scratch = vec![u32::MAX; m.scratch_words(k)];
            let ops = done(exec::run(&m, k, &mut actual, &mut scratch, &mut device));
            assert_eq!(actual, expected, "state: round {round}, entry {}", entry.name);
            assert_eq!(device.calls, host.calls, "ports: round {round}, entry {}", entry.name);
            assert!(ops <= m.entries()[k].cost, "executed cost is bounded");
            assert!(device.ops.windows(2).all(|w| w[0] < w[1]));
            assert!(device.ops.iter().all(|count| *count < ops));
        }
    }
    assert_ne!(actual, m.state_init(), "the program changed its state");
    for (k, skip_reads) in (0..m.entries().len()).flat_map(|k| [(k, 0), (k, 1)]) {
        let mut direct_state = m.state_init().to_vec();
        let mut resumed_state = direct_state.clone();
        let mut direct_scratch = vec![0; m.scratch_words(k)];
        let mut resumed_scratch = vec![0; m.scratch_words(k)];
        let mut direct = Trace { input: 1, ..Trace::default() };
        let mut suspended = Trace { input: 1, suspend: true, skip_reads, ..Trace::default() };
        let direct_ops = done(exec::run(&m, k, &mut direct_state, &mut direct_scratch, &mut direct));
        let stopped = exec::run(&m, k, &mut resumed_state, &mut resumed_scratch, &mut suspended);
        let Exit::Suspended { ops } = stopped else { panic!("pin_read must suspend") };
        assert_eq!(ops, suspended.ops.last().expect("suspended port") + 1, "suspension counts its port once");
        let resumed_ops = done(exec::resume(&m, &mut resumed_state, &mut resumed_scratch, &mut suspended, &[1]));
        assert_eq!(resumed_ops, direct_ops, "resume preserves the op count");
        assert_eq!(resumed_state, direct_state, "resume preserves registers and control flow");
        assert_eq!(suspended.calls, direct.calls, "resume does not repeat the suspended port");
        assert_eq!(suspended.ops, direct.ops);
    }

    let instructions = statements(words, &m);
    let find = |op| *instructions.iter().find(|at| words[**at] & OP_MASK == op).expect("source exercises opcode");
    let mut bad = words.clone();
    bad[H_MAGIC] ^= 1;
    assert!(matches!(module::load(&bad, &profile), Err(LoadError::Magic { .. })));
    let mut bad = words.clone();
    bad.pop();
    assert!(matches!(module::load(&bad, &profile), Err(LoadError::ModuleLength { .. })));
    let mut bad = words.clone();
    bad[instructions[0]] = 0xFE;
    assert!(matches!(module::load(&bad, &profile), Err(LoadError::Instruction { .. })));
    let mut bad = words.clone();
    bad[find(OP_PORT) + 1] = ports::PORTS.len() as u32;
    assert!(matches!(module::load(&bad, &profile), Err(LoadError::Port { .. })));
    let state_load = *instructions.iter().find(|at| words[**at] == (OP_LOAD | (REGION_STATE << SUB_SHIFT))).expect("state load");
    let mut bad = words.clone();
    bad[state_load + 2] = m.state_words();
    assert!(matches!(module::load(&bad, &profile), Err(LoadError::MemoryRange { .. })));
    // A loop without a cap the cost can bound (its cap word made the
    // largest) is refused by its cost.
    let mut bad = words.clone();
    bad[find(OP_LOOP) + 1] = u32::MAX;
    assert!(matches!(module::load(&bad, &profile), Err(LoadError::CostUnderstated { .. } | LoadError::CostLimit { .. })));
    let mut bad = words.clone();
    bad[find(OP_LOOP) + 2] -= 1;
    assert!(matches!(module::load(&bad, &profile), Err(LoadError::BlockLength { .. })));
    let mut decl = section(words, SEC_DECLS).start;
    let decl_end = section(words, SEC_DECLS).end;
    while decl < decl_end && words[decl] != ports::FORM_OUTPUT { decl += DECL_HEADER_WORDS + words[decl + 3] as usize * 2; }
    assert!(decl < decl_end, "output declaration");
    let mut bad = words.clone();
    bad[decl + 1] = 32;
    bad[decl + DECL_HEADER_WORDS + 1] = 32;
    assert!(matches!(module::load(&bad, &profile), Err(LoadError::PinRange { .. })));
    let entries = section(words, SEC_ENTRIES);
    let mut bad = words.clone();
    bad[entries.start + 3] = profile.max_entry_ops as u32 + 1;
    bad[entries.start + 4] = 0;
    assert!(matches!(module::load(&bad, &profile), Err(LoadError::CostLimit { .. })));
    let mut string = section(words, SEC_STRINGS).start;
    let string_end = section(words, SEC_STRINGS).end;
    while string < string_end && words[string] == 0 { string += 1; }
    assert!(string < string_end, "nonempty string");
    let mut bad = words.clone();
    bad[string + 1] = (bad[string + 1] & !0xFF) | 0xFF;
    assert!(matches!(module::load(&bad, &profile), Err(LoadError::Utf8 { .. })));
}

#[test]
fn shared_functions_round_trip_match_inlining_and_resume() {
    use fab_elec_mcu::Owner;
    const SOURCE: &str = include_str!("support/shared.splash");
    // Every helper big enough is shared; the prelude's port wrappers stay inlined (the core never
    // shares a library helper), so the sites compared below name the user's calls.
    let profile = Profile { call_ops: Some(0), max_entry_ops: 100_000, ram_words: 16_384, ..MARK_PROFILE };
    let called = compile(SOURCE, &profile, &[("P0", 0), ("P1", 1)], &[]).expect("shared source syntax and lowering");
    let inlined = compile(SOURCE, &Profile { call_ops: None, ..profile }, &[("P0", 0), ("P1", 1)], &[]).expect("inlined source");
    let m = module::load(&called.module, &profile).expect("version 2 loads");
    let plain = module::load(&inlined.module, &profile).expect("version 1 loads");
    assert_eq!(called.module[H_VERSION], FUNCTION_VERSION);
    assert_eq!(inlined.module[H_VERSION], FORMAT_VERSION);
    assert!(plain.funcs().is_empty());
    assert_eq!(called.module, fab_elec_mcu::encode::encode(&called.lowered, profile.id).unwrap());
    assert_eq!(m.funcs().len(), called.lowered.funcs.len());
    assert_eq!(called.module[section(&called.module, SEC_FUNCS)].len(), m.funcs().len() * FUNC_WORDS);
    assert_eq!(m.code(), &called.module[section(&called.module, SEC_CODE)]);
    for name in ["scale", "twice"] {
        assert_eq!(m.funcs().iter().filter(|f| m.string(f.program.name) == name).count(), 1, "{name} stored once");
    }
    let functions = &called.lowered.entries[0].program.funcs;
    for (k, f) in m.funcs().iter().enumerate() {
        let p = &functions[k];
        assert_eq!(f.params, p.params.iter().map(|v| v.0).collect::<Vec<_>>());
        assert_eq!(f.results, p.results.iter().map(|v| v.0).collect::<Vec<_>>());
        assert_eq!(f.program.frame_words, m.entries()[0].frame_words);
        assert_eq!(f.program.cost, ir::Program { funcs: functions.clone(), ..p.clone() }.total_cost());
        for (id, &ty) in p.vals.iter().enumerate() {
            let expected = match ty { ir::Ty::F32 => TY_F32, ir::Ty::I32 => TY_I32, ir::Ty::Bool => TY_BOOL, ir::Ty::F64 => TY_F64 };
            assert_eq!(m.func_ty(k, id as u32, false), expected);
        }
    }
    let positions = statements(&called.module, &m);
    let calls: Vec<_> = positions.iter().copied().filter(|&at| called.module[at] == OP_CALL).collect();
    assert!(!calls.is_empty());
    for at in calls {
        let f = &m.funcs()[called.module[at + 1] as usize];
        assert_eq!((called.module[at + 2] as usize, called.module[at + 3] as usize), (f.params.len(), f.results.len()));
        let code_at = (at - section(&called.module, SEC_CODE).start) as u32;
        if let Some(caller) = m.funcs().iter().position(|g| g.program.body < code_at && code_at < g.program.code + g.program.len) {
            let rank = |f| m.func_order().iter().position(|&id| id == f).unwrap();
            assert!(rank(called.module[at + 1]) < rank(caller as u32), "callee precedes caller");
        }
    }
    let scale = called.lowered.funcs.iter().position(|f| f.name == "scale").unwrap() as u32;
    let helper = called.stmts.iter().find(|s| &SOURCE[s.site.start as usize..s.site.end as usize] == "hits = hits + 1").unwrap();
    assert_eq!(helper.owner, Owner::Func(scale));
    assert!(called.conds.iter().any(|c| c.owner == Owner::Func(scale)), "helper loop condition is owned by its function");
    assert!(called.stmts.iter().any(|s| matches!(s.owner, Owner::Handler(_))), "entry statements retain entry owners");
    for &(at, id) in &called.marks {
        if let Owner::Func(k) = called.stmts[id as usize].owner {
            let f = &m.funcs()[k as usize].program;
            assert!(f.body < at && at <= f.code + f.len, "function mark indexes its own code");
        }
    }
    // Site ids differ when inlining duplicates a helper; compare the source
    // sites those ids name, as the compute core's differential does.
    let trace = |c: &fab_elec_mcu::Compiled, t: &Trace| t.calls.iter().map(|(port, site, args)|
        (*port, c.sites[*site as usize], args.clone())).collect::<Vec<_>>();
    let mut state = m.state_init().to_vec();
    let mut inline_state = plain.state_init().to_vec();
    for round in 0..3 {
        for k in 0..m.entries().len() {
            let before = state.clone();
            let mut scratch = vec![u32::MAX; m.scratch_words(k)];
            let mut ports = Trace { input: round % 2, ..Trace::default() };
            let ops = done(exec::run(&m, k, &mut state, &mut scratch, &mut ports));
            let mut in_ports = Trace { input: round % 2, ..Trace::default() };
            done(exec::run(&plain, k, &mut inline_state, &mut vec![0; plain.scratch_words(k)], &mut in_ports));
            assert_eq!(state, inline_state, "called/inlined state after entry {k}, round {round}");
            assert_eq!(trace(&called, &ports), trace(&inlined, &in_ports), "called/inlined ports and sites");
            assert!(ops <= m.entries()[k].cost);
            // Suspend at every read, including a nested helper's read, then
            // continue through its return and the next call of the same helper.
            let mut resumed = before;
            let mut stopped = Trace { input: round % 2, suspend: true, ..Trace::default() };
            let mut exit = exec::run(&m, k, &mut resumed, &mut scratch, &mut stopped);
            while matches!(exit, Exit::Suspended { .. }) {
                stopped.suspend = true;
                exit = exec::resume(&m, &mut resumed, &mut scratch, &mut stopped, &[round % 2]);
            }
            assert_eq!(done(exit), ops, "suspension preserves the cost");
            assert_eq!(resumed, state, "static caller locals survive nested calls and suspension");
            assert_eq!(stopped.calls, ports.calls, "no port is repeated on resume");
        }
    }
}

#[test]
fn shared_call_loader_refusals() {
    let profile = Profile { call_ops: Some(0), max_entry_ops: 100_000, ram_words: 16_384, ..MARK_PROFILE };
    let c = compile(include_str!("support/shared.splash"), &profile, &[("P0", 0), ("P1", 1)], &[]).unwrap();
    let m = module::load(&c.module, &profile).unwrap();
    let at = statements(&c.module, &m).into_iter().find(|&at| c.module[at] == OP_CALL && c.module[at + 2] != 0 && c.module[at + 3] != 0).unwrap();
    let mut bad = c.module.clone();
    bad[at + 1] = m.funcs().len() as u32;
    assert!(matches!(module::load(&bad, &profile), Err(LoadError::CallTarget { .. })));
    for count in [2, 3] {
        let mut bad = c.module.clone();
        bad[at + count] += 1;
        assert!(matches!(module::load(&bad, &profile), Err(LoadError::CallArity { .. })), "argument and result counts are checked before reading ids");
    }
    let f = &m.funcs()[c.module[at + 1] as usize];
    let mut bad = c.module.clone();
    let param = f.params[0];
    let ty = section(&bad, SEC_CODE).start + f.program.code as usize + PROGRAM_WORDS + param as usize / 4;
    let shift = (param % 4) * 8;
    bad[ty] = (bad[ty] & !(0xff << shift)) | (u32::from(TY_BOOL) << shift);
    assert!(matches!(module::load(&bad, &profile), Err(LoadError::TypeMismatch { .. })));
    let mut bad = c.module.clone();
    bad[section(&c.module, SEC_ENTRIES).start + 4] = 1;
    assert!(matches!(module::load(&bad, &profile), Err(LoadError::CostHighWord { .. })));
    let mut bad = c.module.clone();
    bad[section(&c.module, SEC_ENTRIES).start + 3] = 0;
    assert!(matches!(module::load(&bad, &profile), Err(LoadError::CostUnderstated { .. })), "entry cost includes calls");
    let mut bad = c.module.clone();
    bad[section(&c.module, SEC_FUNCS).start + 5] = 0;
    assert!(matches!(module::load(&bad, &profile), Err(LoadError::CostUnderstated { .. })), "function cost includes its body");
    // Tiny hand-built bodies: zero-argument calls whose only effect is a cycle.
    // They cannot be produced by the lowerer and never reach exec.
    let sections = [
        (SEC_LAYOUT, vec![0, 0, 0, 1, 0, 0, 0, 2]),
        (SEC_STATE_INIT, vec![]), (SEC_SHARED, vec![]),
        (SEC_STRINGS, vec![1, b'f' as u32]), (SEC_VARS, vec![]), (SEC_DECLS, vec![]), (SEC_ENTRIES, vec![]),
        (SEC_CODE, vec![0, 0, 0, 0, 4, OP_CALL, 1, 0, 0, 0, 0, 0, 0, 4, OP_CALL, 0, 0, 0]),
        (SEC_FUNCS, vec![0, 0, 9, 0, 0, 4, 0, 9, 9, 0, 0, 4]),
    ];
    let mut words = vec![MAGIC, FUNCTION_VERSION, ports::PORT_TABLE_VERSION, 0, 0, sections.len() as u32, 0, 0];
    words.resize(HEADER_WORDS + sections.len() * DIRECTORY_WORDS, 0);
    for (k, (kind, data)) in sections.iter().enumerate() {
        let at = HEADER_WORDS + k * DIRECTORY_WORDS;
        let offset = words.len() as u32;
        words[at..at + 3].copy_from_slice(&[*kind, offset, data.len() as u32]);
        words.extend_from_slice(data);
    }
    words[H_WORDS] = words.len() as u32;
    assert!(matches!(module::load(&words, &profile), Err(LoadError::CallCycle { .. })));
}

#[test]
fn call_signature_types_and_wide_values_are_checked_by_value() {
    use makepad_script_compute::{ir::{Op, Program, Stmt, Ty, Val, Var}, lower::event::EventFunc};
    let profile = Profile { call_ops: Some(0), f64: true, ram_words: 4096, ..MARK_PROFILE };
    let mut c = compile("fn setup() {}", &profile, &[], &[]).unwrap();
    let span = c.lowered.entries[0].span;
    // Return a high-word-sensitive f64 and a fresh i32 variable, then dirty
    // the variable. Calling this twice proves variables reset on every call.
    let f = Program { vals: vec![Ty::F64, Ty::I32, Ty::I32], vars: vec![Ty::I32], params: vec![Val(0)], results: vec![Val(0), Val(1)],
        body: vec![Stmt::Def(Val(1), Op::Get(Var(0))), Stmt::Def(Val(2), Op::ConstI(9)), Stmt::Set(Var(0), Val(2))], ..Program::default() };
    let x = 1.0000000000000002f64;
    let program = Program { vals: vec![Ty::F64, Ty::F64, Ty::I32, Ty::F64, Ty::I32], funcs: vec![f],
        body: vec![Stmt::Def(Val(0), Op::ConstD(x)),
            Stmt::Call { f: 0, args: vec![Val(0)], rets: vec![Val(1), Val(2)] },
            Stmt::Call { f: 0, args: vec![Val(1)], rets: vec![Val(3), Val(4)] }], ..Program::default() };
    c.lowered.funcs = vec![EventFunc { name: "identity".into(), span, marks: 0..0 }];
    c.lowered.entries[0].cost = program.total_cost();
    c.lowered.entries[0].program = program;
    let words = fab_elec_mcu::encode::encode(&c.lowered, profile.id).unwrap();
    let m = module::load(&words, &profile).unwrap();
    let f = &m.funcs()[0].program;
    assert_eq!(&m.code()[f.code as usize..(f.code + f.len) as usize], &[
        3, 1, 0, 1, u32::from(TY_F64) | (u32::from(TY_I32) << 8) | (u32::from(TY_I32) << 16), u32::from(TY_I32),
        0, 0, 1, 9, OP_GET, 1, 0, OP_CONST, 2, 9, OP_SET, 0, 2,
    ], "function header, packed types, parameter/result ids, and body words round trip");
    // Check both sides of the call contract independently of a callee body:
    // a parameter returned directly is valid with either type.
    let code = section(&words, SEC_CODE).start;
    let e = &m.entries()[0];
    let mut bad = words.clone();
    let type_word = code + e.code as usize + PROGRAM_WORDS;
    bad[type_word] = (bad[type_word] & !(0xff << 8)) | (u32::from(TY_I32) << 8);
    assert!(matches!(module::load(&bad, &profile), Err(LoadError::TypeMismatch { .. })), "result slot type must match");
    let mut bad = words.clone();
    let f = &m.funcs()[0];
    bad[code + f.program.code as usize + PROGRAM_WORDS] &= !0xff;
    bad[code + f.program.code as usize + P_WIDE] = 0;
    assert!(matches!(module::load(&bad, &profile), Err(LoadError::TypeMismatch { .. })), "argument type must match");
    // Observe every return through typed stores to state, including the two
    // halves of f64 (wide values cannot be stored as a single word).
    let p = &mut c.lowered.entries[0].program;
    for (value, op) in [(Val(5), ir::Un::LoD), (Val(6), ir::Un::HiD)] {
        p.vals.push(Ty::I32);
        p.body.push(Stmt::Def(value, Op::Un(op, Val(3))));
    }
    for (base, val) in [Val(5), Val(6), Val(2), Val(4)].into_iter().enumerate() {
        p.body.push(Stmt::Store { region: ir::Region::State, base: base as u32, extent: 1, off: None, val });
    }
    c.lowered.entries[0].cost = p.total_cost();
    c.lowered.state_init = vec![u32::MAX; 4];
    let words = fab_elec_mcu::encode::encode(&c.lowered, profile.id).unwrap();
    let m = module::load(&words, &profile).unwrap();
    let mut state = m.state_init().to_vec();
    done(exec::run(&m, 0, &mut state, &mut vec![0; m.scratch_words(0)], &mut Trace::default()));
    assert_eq!(state, [x.to_bits() as u32, (x.to_bits() >> 32) as u32, 0, 0]);
}
