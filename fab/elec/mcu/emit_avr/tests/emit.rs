use fab_elec_mcu::{compile, Compiled};
use fab_elec_mcu_avr::sim::Cpu;
use fab_elec_mcu_avr::{chip::NAMES, machine::AvrMachine};
use fab_elec_mcu_emit_avr::{chip, firmware, Image, Options, Where};
use fab_elec_mcu_emu::{FwTy, Machine};
use fab_elec_mcu_rt::{exec::{self, PortCall, Ports}, module::{self, Module}, sched::Device, watch};

#[path = "support/oracle.rs"]
mod oracle;
#[path = "support/model.rs"]
mod model;
#[path = "../../lang/tests/support/blink.rs"]
mod blink;
use model::{Model, ECHO_SOURCE, PROFILE, RX_BYTES};
use oracle::*;

/// The chip table's pin names and, for these Uno-style sources, `D0`..`D19` by runtime number.
fn pin_names() -> Vec<(String, u32)> {
    chip::PINS.iter().flat_map(|p| [(p.name.to_owned(), p.number), (format!("D{}", p.number), p.number)])
        .chain(chip::ANALOG_ONLY.iter().map(|p| (p.name.to_owned(), p.number))).collect()
}

fn compiled(source: &str) -> Compiled {
    let names = pin_names();
    let pins: Vec<(&str, u32)> = names.iter().map(|(n, k)| (n.as_str(), *k)).collect();
    compile(source, &PROFILE, &pins, &[]).unwrap_or_else(|e| panic!("{e:?}\n{source}"))
}

fn state(cpu: &Cpu, image: &Image) -> Vec<u32> {
    (0..image.layout.state_bytes / 4).map(|i| load(cpu, image.layout.state + 4 * i, 4) as u32).collect()
}

struct NoPorts;
impl Ports for NoPorts {
    fn call(&mut self, port: u16, site: u32, _: u64, _: &[u32], _: &mut [u32]) -> PortCall {
        panic!("unexpected port {port} at site {site}")
    }
}

fn compare_entry(cpu: &mut Cpu, image: &Image, m: &Module<'_>, k: usize,
    initial: &[u32], scratch: &mut [u32]) -> u64 {
    for (i, &word) in initial.iter().enumerate() { store(cpu, image.layout.state + 4 * i as u16, u64::from(word), 4); }
    // Fresh runs must not inherit var/frame contents from the prior handler.
    for i in 0..image.layout.scratch_bytes { cpu.write(image.layout.scratch + i, 0xa5); }
    cpu.sreg = 0;
    // Caller-clobbered registers hold garbage: nothing may stand in for r2's zero.
    for r in (0..2).chain(16..28).chain(30..32) { cpu.r[r] = 0x5a ^ r as u8; }
    let saved = cpu.r[2..16].to_vec();
    let setup = m.entry(fab_elec_mcu_rt::SETUP);
    let target = if setup == Some(k) { image.symbols.setup } else {
        image.symbols.entries[k - usize::from(setup.is_some_and(|s| s < k))]
    };
    let call = invoke(cpu, image, target);
    assert_eq!(&cpu.r[2..16], saved, "entry must preserve runtime registers");
    assert_eq!(cpu.sreg & 128, 0, "entry must leave the caller's interrupt mask alone");
    let mut want = initial.to_vec();
    assert!(matches!(exec::run(m, k, &mut want, scratch, &mut NoPorts), exec::Exit::Done { .. }));
    assert_eq!(state(cpu, image), want, "entry {} from {initial:?}", m.string(m.entries()[k].name));
    call.clocks
}

fn inline_bodies() -> Vec<String> {
    let mut bodies: Vec<String> = ["a = a + b", "a = a - b", "a = a & b", "a = a | b", "a = a ^ b", "a = -a",
        "p = a < b", "p = a <= b", "p = a > b", "p = a >= b", "p = a == b", "p = a != b",
        "p = p == q", "p = p != q", "p = p && q", "p = p || q", "p = !p", "a = if p { a } else { b }",
        "a = data[a]", "data[a] = b", "a = b - (a ^ b)", "data[a + 1] = b",
        "p = !(p && q)", "a = if (a & b) == 0 { a } else { b }"].into_iter().map(str::to_owned).collect();
    for op in ["<<", ">>", ">>>"] {
        for amount in [0, 1, 3, 7, 8, 9, 15, 16, 17, 23, 24, 25, 31, 32, 33, 40] {
            bodies.push(format!("a = a {op} {amount}"));
        }
    }
    // Constants are immediates: on either side, at byte edges, and against the compare's k + 1.
    let constants = ["0", "1", "(0 - 1)", "255", "256", "65280", "2147483647", "(0 - 2147483647 - 1)", "1431655765"];
    for k in constants {
        for op in ["+", "-", "&", "|", "^"] { bodies.push(format!("a = a {op} {k}")); bodies.push(format!("a = {k} {op} a")); }
        for op in ["<", "<=", ">", ">=", "==", "!="] { bodies.push(format!("p = a {op} {k}")); bodies.push(format!("p = {k} {op} a")); }
        bodies.push(format!("a = {k}"));
    }
    bodies.extend(["p = true", "p = false", "p = p && true", "p = p || false", "p = p && false", "p = p || true"].map(str::to_owned));
    // AIR requires i32 BIN operands: convert bools explicitly through a 0/1 select.
    for op in ["+", "-", "&", "|", "^"] {
        bodies.push(format!("a = a {op} (if p {{ int(1) }} else {{ int(0) }})"));
        bodies.push(format!("a = (if p {{ int(1) }} else {{ int(0) }}) {op} a"));
    }
    for k in ["0", "256", "65536", "16777216", "16777472"] {
        bodies.push(format!("data[a] = {k}"));
    }
    // Selects between constants: one-bit differences take the condition itself, others its mask.
    // Untyped literals in arms are f32: int() folds them to i32 constants (exact in f32 here).
    for (t, e) in [("1", "0"), ("0", "1"), ("7", "6"), ("6", "7"), ("0", "0 - 1"), ("0 - 1", "0"),
        ("255", "256"), ("65280", "0"), ("0 - 16777216", "16777215"), ("5", "5")] {
        bodies.push(format!("a = if p {{ int({t}) }} else {{ int({e}) }}"));
    }
    bodies.extend(["p = if q { true } else { false }", "p = if q { false } else { true }"].map(str::to_owned));
    // An if whose compare takes the value computed just before it, on either side, plain and under !.
    bodies.extend(["if (a & 1) != 0 { a = b }", "if (a & b) == 0 { a = 7 }", "if (a + 1) < b { a = 0 }",
        "if b <= (a ^ b) { a = 1 }", "if (a - b) > 255 { a = 2 }", "if !((a | b) >= 0) { a = 3 }",
        "if (a >> 3) == b { a = 4 }"].map(str::to_owned));
    // Each mask has a proved interval; combine narrow/wide inputs and cross 255.
    for (x, y) in [("(a & 127)", "(b & 127)"), ("(a & 255)", "(b & 1)"),
        ("a", "(b & 255)"), ("(a & 255)", "b")] {
        for op in ["+", "-", "&", "|", "^"] { bodies.push(format!("a = {x} {op} {y}")); }
        for op in ["<", "<=", ">", ">=", "==", "!="] {
            bodies.push(format!("p = {x} {op} {y}"));
            bodies.push(format!("if {x} {op} {y} {{ a = 1 }} else {{ a = 0 }}"));
        }
        bodies.push(format!("a = if p {{ {x} }} else {{ {y} }}"));
    }
    for k in [254, 255, 256] { for op in ["<", "<=", ">", ">=", "==", "!="] {
        for (x, y) in [("(a & 255)".to_owned(), k.to_string()), (k.to_string(), "(a & 255)".to_owned())] {
            bodies.push(format!("p = {x} {op} {y}"));
            bodies.push(format!("if !({x} {op} {y}) {{ a = 1 }} else {{ a = 0 }}"));
        }
    } }
    for op in ["<<", ">>", ">>>"] { for amount in [0, 1, 7, 8, 9] {
        for mask in [1, 255] { bodies.push(format!("a = (a & {mask}) {op} {amount}")); }
    } }
    bodies.extend(["a = (a & 255) + 1", "a = 255 - (a & 255)",
        "a = -((a & 255) - 255)", "a = -(a & 255)",
        "a = (a & 15) * (b & 15)", "a = (a & 255) * (b & 255)",
        "a = (a & 255) % 16", "a = (a & 65535) % 512", "a = a % 16",
        "a = (a & 1) << b", "a = (a & 255) / 2", "a = (a & 255) % 7",
        "a = if p { int(255) } else { int(0) }", "a = if p { int(256) } else { int(0) }",
        "data[a & 3] = b & 1", "a = data[a & 3]", "data[0] = a & 255",
        "let x = a & 127 let y = x x = x + 1 data[0] = y a = x"].map(str::to_owned));
    // Indexed state accesses: in-range masks, retained clamps (including
    // negative inputs), and constant/no-index stores at each unsigned width.
    for index in ["a & 7", "(0 - 1)"] {
        bodies.push(format!("a = data[{index}]"));
    }
    for (array, value) in [("byte_data", "a & 255"), ("half_data", "a & 65535"),
        ("wide_data", "a"), ("mixed_data", "a & 255")] {
        for index in ["0", "(0 - 1)", "b & 3", "b & 7", "b"] {
            bodies.push(format!("{array}[{index}] = {value} a = {array}[{index}]"));
        }
    }
    bodies.extend(["byte_word = a & 255", "half_word = a & 65535", "wide_word = a",
        // Neighbouring arrays must use their own destination intervals.
        "byte_data[b & 3] = a & 255 wide_data[b & 3] = a",
        "byte_data[b & 3] = a & 255 wide_data[b & 3] = a & 255"].map(str::to_owned));
    // Extents straddle the byte-offset boundary. At 65/128 the byte index
    // needs a zero high byte before 16-bit scaling, even with dirty registers.
    for (len, index) in [(64, "b & 63"), (65, "(b & 63) + 1"), (128, "b & 127")] {
        bodies.push(format!("scale{len}[{index}] = a & 255 a = scale{len}[{index}]"));
    }
    // LPM follows the same address rules, including an i32-shaped index whose
    // interval fits 257 words, and a byte index that still requires a clamp.
    for (len, index) in [(4, "a & 3"), (4, "a & 7"), (4, "a"), (4, "(0 - 1)"),
        (64, "a & 63"), (65, "(a & 63) + 1"), (257, "(a & 255) + 1")] {
        bodies.push(format!("a = table{len}[{index}]"));
    }
    // Frame stores still clear all upper bytes, even when the stored value
    // and address computation are narrow.
    for (len, index) in [(4, "b & 3"), (65, "(b & 63) + 1")] {
        bodies.push(format!("let local = [int(0 - 1); {len}] local[{index}] = a & 255 a = local[{index}]"));
    }
    bodies
}

#[test]
fn inline_ops_match_exec() {
    let bodies = inline_bodies();
    let mut edges = vec![0i32, 1, -1, -128, -129, 2, -2, 7, 8, 127, 128, 255, 256, -256, 65535, 65536,
        i32::MIN, i32::MIN + 1, i32::MAX, i32::MAX - 1, 0x55555555, 0xaaaaaaaau32 as i32, 3, 4];
    for k in [4, 15, 16, 23, 24, 30] { edges.extend([1 << k, (1 << k) - 1]); }
    // Small batches leave flash room for the fixed runtime and the oracle return pad.
    for batch in bodies.chunks(8) {
        let source = inline_source(batch);
        let c = compiled(&source);
        let m = module::load(&c.module, &PROFILE).unwrap();
        let built = build_module(&m, &Options::default()).unwrap_or_else(|e| panic!("{e}\n{source}"));
        let wide = build_module(&m, &Options { full_ranges: true, ..Options::default() }).unwrap();
        for image in [&built, &wide] {
            use fab_elec_mcu_avr::insn::{decode, Insn::*};
            let s = &image.image.symbols;
            let mut calls = Vec::new();
            for &(start, end) in &image.map.entries {
                let mut pc = start;
                while pc < end {
                    let (insn, len) = decode(&image.image.words[pc as usize..]).unwrap();
                    match insn {
                        Call(target) => calls.push(target),
                        Rcall(offset) => calls.push((i64::from(pc) + 1 + i64::from(offset)) as u32),
                        _ => {}
                    }
                    pc += len as u32;
                }
            }
            for helper in [s.mul32, s.sdiv32, s.srem32, s.shl32, s.shr32, s.sar32] {
                assert_eq!(helper != 0, calls.contains(&helper), "only helpers called by a live operation are linked");
            }
            assert_eq!(s.udivmod32 != 0, s.sdiv32 != 0 || s.srem32 != 0, "division dependency follows its users");
        }
        let (mut cpu, _) = booted(&built.image);
        let (mut wide_cpu, _) = booted(&wide.image);
        let mut scratch = vec![0; (0..m.entries().len()).map(|k| m.scratch_words(k)).max().unwrap()];
        let slots: Vec<_> = ["a", "b", "p", "q"].map(|name|
            m.vars().iter().find(|v| m.string(v.name) == name).unwrap().offset as usize).to_vec();
        for (k, body) in batch.iter().enumerate() {
            let entry = m.entry(&format!("op{k}")).unwrap();
            let mut initial = m.state_init().to_vec();
            // Sweep only the operands the body reads.
            let reads = |name: &str| body.split(|c: char| !c.is_ascii_alphanumeric()).any(|t| t == name);
            let ints = |name| if reads(name) { edges.clone() } else { vec![0] };
            let bools = |name| if reads(name) { vec![0, 1] } else { vec![0] };
            let (a_grid, b_grid, p_grid, q_grid) = (ints("a"), ints("b"), bools("p"), bools("q"));
            for &a in &a_grid { for &b in &b_grid { for &p in &p_grid { for &q in &q_grid {
                for (slot, value) in slots.iter().zip([a as u32, b as u32, p, q]) { initial[*slot] = value; }
                compare_entry(&mut cpu, &built.image, &m, entry, &initial, &mut scratch);
                compare_entry(&mut wide_cpu, &wide.image, &m, entry, &initial, &mut scratch);
                assert_eq!(state(&cpu, &built.image), state(&wide_cpu, &wide.image), "FALSIFIER 1: {body}, {initial:?}");
            } } } }
        }
    }
    // A byte product alone must not pull in mul32; a neighbouring wide product
    // in the batched sweep would otherwise hide a selective-linking regression.
    let c = compiled(&inline_source(&["a = (a & 15) * (b & 15)".to_owned()]));
    let m = module::load(&c.module, &PROFILE).unwrap();
    let narrow = build_module(&m, &Options::default()).unwrap();
    let wide = build_module(&m, &Options { full_ranges: true, ..Options::default() }).unwrap();
    assert_eq!(narrow.image.symbols.mul32, 0, "a byte product uses the chip multiply");
    assert_ne!(wide.image.symbols.mul32, 0, "the proof build retains the i32 helper");
    assert!(narrow.image.words.len() < wide.image.words.len(), "omitting mul32 reduces flash");
    // Port arguments keep the i32 ABI; compare both images with the device,
    // including an inline level and a computed pin whose own shape is a byte.
    for a in [0i32, 1, 127, 128, 255, 256, -1, -128, -129] {
        let source = format!("let out = output(D13)\nlet one = output(D4)\nlet two = output(D5)\nlet motor = pwm(D9, hz(490))\nvar a: i32 = {a}\nfn setup() {{ write(out, a & 255) write((a & 1) + 4, a & 127) duty(motor, a & 255) }}\n");
        replay_program(&source, 1_000_000);
    }
}

#[test]
fn helper_ops_match_exec_and_bounds() {
    use fab_elec_mcu_emit_avr::helpers::{self, Helper};
    use fab_elec_mcu_rt::format::*;
    let edges = [0i32, 1, -1, 2, 7, 127, 128, 255, 256, i32::MIN, i32::MAX, i32::MIN + 1];
    let shifts = [0, 1, 7, 8, 31, 32, 33, -1];
    let mut cases = Vec::new();
    for (op, helper) in [(BIN_MUL_I, Helper::Mul32), (BIN_DIV_I, Helper::Sdiv32),
        (BIN_REM_I, Helper::Srem32), (BIN_SHL_I, Helper::Shl32),
        (BIN_SHR_UI, Helper::Shr32), (BIN_SHR_I, Helper::Sar32)] {
        cases.push((vec![OP_BIN | op << SUB_SHIFT, 3, 0, 1], Some(helper), None));
    }
    // The addend is a separate live value, or aliases either multiply input.
    for addend in [2, 0, 1] {
        cases.push((vec![OP_FMA | FMA_MUL_ADD_I << SUB_SHIFT, 3, 0, 1, addend], Some(Helper::Mul32), None));
    }
    for len in [0u32, 1, 4, 3, 5, 7, 12, 100] {
        cases.push((vec![OP_WRAP, 3, 0, len], (len != 0 && !len.is_power_of_two()).then_some(Helper::Srem32), None));
    }
    // Constants still use the power-of-two path, but zero/negative/other divisors call srem32.
    for divisor in [0u32, 1, 2, 128, 3, u32::MAX, i32::MIN as u32] {
        let helper = (!(divisor <= i32::MAX as u32 && divisor.is_power_of_two())).then_some(Helper::Srem32);
        cases.push((vec![OP_BIN | BIN_REM_I << SUB_SHIFT, 3, 0, 1], helper, Some(divisor)));
    }
    let plain = fixture(&[], &[], &[], 0, &[0; 4], &[]);
    let plain = module::load(&plain, &PROFILE).unwrap();
    let baseline = build_module(&plain, &Options::default()).unwrap();
    for (op, helper, constant) in cases {
        let mut body = vec![OP_LOAD | REGION_STATE << SUB_SHIFT, 0, 0, 1, NONE];
        if let Some(value) = constant { body.extend([OP_CONST, 1, value]); }
        else { body.extend([OP_LOAD | REGION_STATE << SUB_SHIFT, 1, 1, 1, NONE]); }
        body.extend([OP_LOAD | REGION_STATE << SUB_SHIFT, 2, 2, 1, NONE]);
        body.extend_from_slice(&op);
        body.extend([OP_STORE | REGION_STATE << SUB_SHIFT, 3, 1, NONE, 3]);
        let words = fixture_inputs(&body, &[TY_I32; 4], &[], 0, &[0; 4], &[], &[0, 1, 2]);
        if op[0] == OP_WRAP && op[3] == 0 {
            assert!(matches!(module::load(&words, &PROFILE), Err(module::LoadError::WrapLength { len: 0, .. })),
                "zero-length WRAP stays refused by module verification");
            continue;
        }
        let m = module::load(&words, &PROFILE).unwrap();
        let built = build_module(&m, &Options::default()).unwrap();
        let image = &built.image;
        let (s, b) = (&image.symbols, &image.bounds);
        let linked = [
            (Helper::Mul32, s.mul32, b.mul32, "mul32"),
            (Helper::Udivmod32, s.udivmod32, b.udivmod32, "udivmod32"),
            (Helper::Sdiv32, s.sdiv32, b.sdiv32, "sdiv32"),
            (Helper::Srem32, s.srem32, b.srem32, "srem32"),
            (Helper::Shl32, s.shl32, b.shl32, "shl32"),
            (Helper::Shr32, s.shr32, b.shr32, "shr32"),
            (Helper::Sar32, s.sar32, b.sar32, "sar32"),
        ];
        for (h, address, bound, name) in linked {
            let needed = helper == Some(h) || (h == Helper::Udivmod32 && matches!(helper, Some(Helper::Sdiv32 | Helper::Srem32)));
            assert_eq!(address != 0, needed, "op {op:?}: only requested helpers and their dependencies link: {h:?}");
            assert_eq!(bound, if needed { h.cycles() - 4 } else { 0 }, "{h:?}: bound excludes caller CALL");
            if needed {
                assert!(address < s.setup, "helper precedes all relaxable entry code");
                assert_eq!(built.map.find(address), Where::Runtime(name));
                let end = linked.iter().map(|r| r.1).filter(|&pc| pc > address).min().unwrap_or(s.setup);
                eprintln!("{name}: {} bytes, bound={bound}", 2 * (end - address));
            }
        }
        assert_eq!(image.layout.stack_bytes, baseline.image.layout.stack_bytes + if helper.is_some() { helpers::STACK_BYTES as u16 } else { 0 },
            "only linked helpers add stack reserve");
        let (mut cpu, _) = booted(image);
        let mut scratch = vec![0; m.scratch_words(0)];
        let b_grid: &[i32] = if op[0] == OP_WRAP || constant.is_some() { &[0] }
            else if op[0] & OP_MASK == OP_BIN && matches!(op[0] >> SUB_SHIFT, BIN_SHL_I | BIN_SHR_I | BIN_SHR_UI) { &shifts }
            else { &edges };
        let c_grid: &[i32] = if op[0] & OP_MASK == OP_FMA && op[4] == 2 { &edges } else { &[0] };
        for &a in &edges { for &b in b_grid { for &c in c_grid {
            let clocks = compare_entry(&mut cpu, image, &m, 0, &[a as u32, b as u32, c as u32, 0], &mut scratch);
            assert!(clocks <= built.entries[0].max_cycles,
                "op {op:?}, a={a}, b={b}, c={c}: measured {clocks} exceeds bound {}", built.entries[0].max_cycles);
        } } }
    }
}

// Fixed seeds vary operands, expression forms, bool state and loop exits.
fn generated(seed: u32) -> String {
    let mut rng = seed;
    let mut next = || { rng ^= rng << 13; rng ^= rng >> 17; rng ^= rng << 5; rng };
    let a = next() as i32;
    let b = next() as i32;
    let op = ["+", "-", "&", "|", "^", "*", "/", "%", "<<", ">>", ">>>"][(next() % 11) as usize];
    let limit = 3 + next() % 3;
    let shift = next() % 32;
    let len = [3, 4, 5, 7, 12][(seed % 5) as usize];
    let extra = if next() & 1 == 0 { "var c: i32 = 3\nvar q = true\n" } else { "" };
    let change = if extra.is_empty() { "" } else { "c = c + 1\nq = !q\n" };
    format!(r#"
// seed {seed}
let out = output(D4)
let aux = output(D5)
let led = output(D13)
let motor = pwm(D9, hz(490))
var a: i32 = {a}
var b: i32 = {b}
var p = false
var data = [int(0); {len}]
var n: i32 = 0
var bits = [int(0); 4]
var boundary: i32 = 0
{extra}
fn setup() {{ write(led, low) duty(motor, 0) }}
let first = every(ms(5), fn() {{
    let bias = b ^ {seed}
    let i: i32 = 0
    while i < 5 {{
        i = i + 1
        if i == 2 {{ continue }}
        if i == {limit} {{ break }}
        a = (a {op} bias) ^ i
    }}
    p = !p
    if p {{ if b < 0 {{ a = -a }} else {{ a = a >> {shift} }} }}
    else {{ if a == b {{ b = b + 1 }} else {{ b = b ^ a }} }}
    n = (n + 1) % 16
    bits[a & 3] = if p {{ int(1) }} else {{ int(0) }}
    boundary = (a & 255) + 1
    data[a] = b
    b = data[b] ^ data[a & 3] ^ bits[n & 3] ^ boundary
    {change}
    write(out, if p {{ high }} else {{ low }})
    duty(motor, b)
}})
let second = every(ms(7), fn() {{
    a = a * b + 1
    b = (b >>> a) ^ data[a]
    data[b] = a
    write(aux, a)
    duty(motor, a)
}})
"#)
}

fn observed(cpu: &Cpu) -> Vec<watch::Event> {
    frames(cpu).into_iter().filter_map(|(f, _)| {
        assert!(!matches!(f.event, watch::Event::Lost { .. }), "no watch record may be lost");
        matches!(f.event, watch::Event::Write { .. } | watch::Event::Duty { .. } | watch::Event::Shift { .. } | watch::Event::EepromRead { .. } | watch::Event::EepromWrite { .. } | watch::Event::Fault { .. }).then_some(f.event)
    }).collect()
}

/// SPI stores observed at execution, including the enable and the release to PORT.
fn spi_store(cpu: &Cpu) -> Option<(u16, u8)> {
    use fab_elec_mcu_avr::insn::{decode, Insn::*};
    if !cpu.will_execute() { return None; }
    match decode(&cpu.flash[cpu.pc as usize..]).unwrap().0 {
        Out(addr @ 0x2c..=0x2e, r) => Some((u16::from(addr) + 0x20, cpu.r[r as usize])),
        Sts(addr @ 0x4c..=0x4e, r) => Some((addr, cpu.r[r as usize])),
        _ => None,
    }
}

fn shift_samples(events: &[fab_elec_mcu_avr::sim::Event], data: u8, clock: u8, mut high: bool) -> Vec<bool> {
    use fab_elec_mcu_avr::sim::Event;
    let data = chip::by_number(u32::from(data)).unwrap();
    let clock = chip::by_number(u32::from(clock)).unwrap();
    let mut samples = Vec::new();
    for event in events {
        if let Event::Pin { port, bit, high: level, .. } = *event {
            if (port, bit) == (data.port, data.bit) { high = level; }
            if (port, bit) == (clock.port, clock.bit) && level { samples.push(high); }
        }
    }
    samples
}

fn pin_high(cpu: &Cpu, pin: u8) -> bool {
    let pin = chip::by_number(u32::from(pin)).unwrap();
    cpu.read(fab_elec_mcu_avr::chip::pin_register(pin.port)) & (1 << pin.bit) != 0
}

#[test]
fn shift_ports_match_reference_edges_frames_and_bounds() {
    use fab_elec_mcu_avr::sim::Event;
    use watch::{Event as W, Level};
    let cases = [(1, 1u32), (8, 0x80), (9, 0x1a5), (24, 0x80_a5d3), (32, 0x8000_0000),
        (24, 0xa3_5cd6), (24, 0x51_ae33)];
    for (data, clock, spi) in [(11, 13, true), (6, 7, false)] {
        let mut source = format!("let latch = output(PB2)\nlet data = output(D{data})\nlet clock = output(D{clock})\nvar v: i32 = 0\nvar n: i32 = 1\nfn setup() {{\n");
        for (i, &(bits, value)) in cases.iter().enumerate() {
            source.push_str(&format!("v = {}\nn = {bits}\nshift_out(data, clock, v, {})\n", value as i32,
                if i == 3 { "24" } else { "n" }));
            // The final two shifts have no intervening pin write or recorder.
            if i != cases.len() - 2 { source.push_str("write(latch, high)\nwrite(latch, low)\n"); }
        }
        source.push_str("}\n");
        let c = compiled(&source);
        let m = module::load(&c.module, &PROFILE).unwrap();
        let mut model = Model::default();
        Device::new(module::load(&c.module, &PROFILE).unwrap(), &PROFILE).boot(&mut model);
        assert_eq!(model.events.iter().filter(|e| matches!(e, W::Shift { .. })).count(), cases.len(), "reference HAL records every shift");
        for level in [Level::Off, Level::Faults, Level::Ports] {
            let built = build_module(&m, &Options { watch: level, ..Options::default() }).expect("declared shifts build");
            let image = &built.image;
            if level == Level::Ports {
                let plain = compiled("fn setup() {}\n");
                let plain = module::load(&plain.module, &PROFILE).unwrap();
                let baseline = build_module(&plain, &Options { watch: level, ..Options::default() }).unwrap();
                assert_eq!(image.bounds.encode, baseline.image.bounds.encode + 14, "Shift's continuation adds fourteen cycles to the encoder bound");
                assert_eq!(image.layout.w, baseline.image.layout.w + 2, "without timers or serial, Shift adds two ticks to W");
            }
            let (start, end, bound) = if spi {
                assert!(image.symbols.shift_bitbang.is_empty(), "SPI pair links no bit-bang stub");
                (image.symbols.shift_spi, image.symbols.shift_spi_end, image.bounds.shift_spi)
            } else {
                assert_eq!(image.symbols.shift_spi, 0, "other pair links no SPI stub");
                assert_eq!(image.symbols.shift_bitbang.len(), 1, "all calls share one pair's stub");
                let (_, start, end) = image.symbols.shift_bitbang[0];
                (start, end, image.bounds.shift_bitbang)
            };
            assert_eq!(built.map.find(start), Where::Runtime(if spi { "shift_out_spi" } else { "shift_out_bitbang" }), "firmware pane names the linked stub");
            let (mut cpu, t0) = booted(image);
            cpu.events.clear();
            let (mut stores, mut returned, mut stamps) = (Vec::new(), 0, Vec::new());
            while cpu.pc != image.symbols.pass {
                assert!(cpu.cycles - t0 < 30_000, "shift setup completes");
                let store = spi_store(&cpu);
                if let Some(store) = store { stores.push(store); }
                let before = cpu.events.len();
                if cpu.will_execute() && cpu.pc == end - 1 {
                    assert!(!pin_high(&cpu, clock), "call {returned}: clock ends low");
                    assert_eq!(pin_high(&cpu, data), cases[returned].1 & 1 != 0, "call {returned}: data holds bit zero");
                    assert_ne!(cpu.sreg & 128, 0, "shift preserves enabled interrupts");
                    if spi { assert_eq!(cpu.io(0x4c), 0, "return releases SPI"); }
                    returned += 1;
                }
                if level == Level::Ports && cpu.will_execute() && cpu.pc == image.symbols.push && cpu.read(image.layout.record) == watch::SHIFT {
                    assert_eq!(cpu.read(image.layout.record_count), 2, "one Shift occupies two records");
                    let tick = load(&cpu, image.layout.record + 1, 4);
                    let last = cpu.events.iter().rev().find_map(|e| match *e { Event::Pin { cycle, .. } => Some(cycle), _ => None }).unwrap();
                    assert!(t0 + tick * 8 + 7 >= last, "Shift stamp follows the completed effect");
                    stamps.push(tick);
                }
                cpu.step().expect("shift setup instruction executes");
                if store == Some((0x4c, 0)) {
                    assert!(!cpu.events[before..].iter().any(|e| matches!(e, Event::Pin { port: 'B', bit: 3 | 5, .. })), "SPE-off changes neither MOSI nor SCK");
                }
            }
            assert_eq!(returned, cases.len(), "every shift returns once");
            let expected: Vec<_> = cases.iter().flat_map(|&(bits, value)| (0..bits).rev().map(move |bit| value & (1 << bit) != 0)).collect();
            assert_eq!(shift_samples(&cpu.events, data, clock, false), expected, "{level:?}: exactly bits rising edges, MSB first, including consecutive calls");
            let controls: Vec<_> = stores.iter().filter_map(|&(a, v)| (a == 0x4c).then_some(v)).collect();
            assert_eq!(controls, if spi { [0x50, 0].repeat(cases.len() - 1) } else { vec![] }, "only byte-bearing hardware calls enable and disable SPI");
            if !spi { assert!(stores.is_empty(), "bit-bang never writes any SPI register"); }
            if spi { assert_eq!(cpu.io(0x4d) & 0xc0, 0, "SPIF is clear and no collision occurred after consecutive calls"); }
            drain(&mut cpu, image);
            let events = observed(&cpu);
            assert_eq!(events, if level == Level::Ports { model.events.clone() } else { vec![] }, "{level:?}: watch payloads and sites match the reference");
            let shift_frames: Vec<_> = frames(&cpu).into_iter().filter(|(f, _)| matches!(f.event, W::Shift { .. })).collect();
            assert_eq!(shift_frames.iter().map(|(_, t)| *t).collect::<Vec<_>>(), stamps, "foreground encoder retains each post-effect tick");
            for (frame, _) in shift_frames {
                let mut wire = [0; watch::MAX_WIRE];
                let n = watch::encode(&frame, &mut wire);
                let mut body = [0; watch::MAX_BODY];
                assert_eq!(watch::cobs_decode(&wire[..n - 1], &mut body), Some(16), "Shift has 15 body bytes plus CRC");
            }
            eprintln!("shift {data}/{clock} {level:?}: image={} bytes, stub={} bytes, bound={bound}, encoder={}, W={}",
                image.words.len() * 2, (end - start) * 2, image.bounds.encode, image.layout.w);
            // Sweep every count, including the mixed byte/bit paths, without services.
            // No build per case; the exact linked routine runs on this image's CPU.
            let (mut probe, _) = booted(image);
            if level == Level::Ports { drain(&mut probe, image); }
            probe.sreg = 0;
            let mut worst = (0, 0);
            for bits in 1..=32u32 {
                for value in [0, u32::MAX, 0x80a5_5ad3] {
                    probe.r[16..20].copy_from_slice(&bits.to_le_bytes());
                    probe.r[20..24].copy_from_slice(&value.to_le_bytes());
                    probe.r[24..26].copy_from_slice(&0x1234u16.to_le_bytes());
                    // An already-high clock must fall before the first data bit.
                    let pin = chip::by_number(u32::from(clock)).unwrap();
                    let port = fab_elec_mcu_avr::chip::pin_register(pin.port) + 2;
                    probe.write(port, probe.read(port) | (1 << pin.bit));
                    let before = probe.events.len();
                    let initial = pin_high(&probe, data);
                    let saved = probe.r[2..16].to_vec();
                    let call = invoke(&mut probe, image, start);
                    within("shift stub", call.clocks, bound);
                    assert_eq!(call.services, 0, "bound excludes service time");
                    assert_eq!(&probe.r[2..16], saved, "shift preserves the runtime registers");
                    assert_eq!(probe.sreg & 128, 0, "shift also preserves a masked caller");
                    assert_eq!(shift_samples(&probe.events[before..], data, clock, initial),
                        (0..bits).rev().map(|bit| value & (1 << bit) != 0).collect::<Vec<_>>(), "count {bits}: direct stub pin sequence");
                    assert!(!pin_high(&probe, clock), "count {bits}: clock ends low");
                    assert_eq!(pin_high(&probe, data), value & 1 != 0, "count {bits}: final data bit");
                    if call.clocks > worst.1 { worst = (bits, call.clocks); }
                    if bits == 24 && value == 0x80a5_5ad3 {
                        eprintln!("shift {data}/{clock} {level:?}: 24 bits measured {} cycles = {:.4} us at 16 MHz (stub including RET, excluding CALL and services)", call.clocks, call.clocks as f64 / 16.0);
                    }
                    if level == Level::Ports {
                        // One Shift is queued; Hello was drained above. Timer0 is the only
                        // possible service before publication (the UART was drained).
                        let overflows = probe.r[13];
                        let encoded = encode_frame(&mut probe, image);
                        let service = u64::from(probe.r[13].wrapping_sub(overflows)) * u64::from(fab_elec_mcu_emit_avr::runtime::TOV_INT);
                        within("Shift foreground encoder", encoded - service, image.bounds.encode);
                        drain(&mut probe, image); probe.sreg = 0;
                    }
                }
            }
            eprintln!("shift {data}/{clock} {level:?}: worst measured {} cycles at {} bits", worst.1, worst.0);
            for bits in [0i32, 33, -1, 256, 65536] {
                probe.r[16..20].copy_from_slice(&bits.to_le_bytes());
                probe.r[24..26].copy_from_slice(&0x1234u16.to_le_bytes());
                let pin = chip::by_number(u32::from(clock)).unwrap();
                let port = fab_elec_mcu_avr::chip::pin_register(pin.port) + 2;
                probe.write(port, probe.read(port) | (1 << pin.bit));
                let before = probe.events.len();
                let call = invoke(&mut probe, image, start);
                within("shift bits fault", call.clocks, bound);
                assert!(!probe.events[before..].iter().any(|e| matches!(e, Event::Pin { .. })), "invalid count {bits} leaves even an initially high clock alone");
                if level != Level::Off { drain(&mut probe, image); probe.sreg = 0; }
            }
            if level == Level::Off {
                // Force TOV during a leading-bit loop and during a hardware byte.
                for bits in [31u32, 32] {
                    probe.r[16..20].copy_from_slice(&bits.to_le_bytes());
                    probe.r[20..24].copy_from_slice(&0x80a5_5ad3u32.to_le_bytes());
                    probe.r[24..26].copy_from_slice(&0x1234u16.to_le_bytes());
                    probe.write(0x35, 1); probe.write(0x46, 252); probe.sreg = 128;
                    let before = probe.events.len();
                    let initial = pin_high(&probe, data);
                    let call = invoke(&mut probe, image, start);
                    assert!(call.services > 0, "Timer0 is serviced during the shift");
                    within("shift with TOV excluded", call.clocks - call.services as u64 * u64::from(fab_elec_mcu_emit_avr::runtime::TOV_INT), bound);
                    assert_eq!(shift_samples(&probe.events[before..], data, clock, initial),
                        (0..bits).rev().map(|bit| 0x80a5_5ad3u32 & (1 << bit) != 0).collect::<Vec<_>>(), "service delays edges without corrupting data");
                    assert_ne!(probe.sreg & 128, 0, "service returns with interrupts enabled");
                }
            }
        }
    }
}

#[test]
fn shift_selection_and_bits_faults() {
    use watch::{Event as W, Level};
    // SS may be PWM, but may never be an input; MISO may not be driven.
    for (extra, spi) in [("let ss = output(PB2)", true), ("let ss = pwm(PB2, hz(490))", true),
        ("let ss = output(PB2)\nlet miso = output(PB4)", false), ("", false)] {
        let source = format!("{extra}\nlet data = output(PB3)\nlet clock = output(PB5)\nvar n: i32 = 24\nfn setup() {{ shift_out(data, clock, 8388609, n) }}\n");
        let c = compiled(&source);
        let m = module::load(&c.module, &PROFILE).unwrap();
        let built = build_module(&m, &Options { watch: Level::Off, ..Options::default() }).unwrap();
        let (mut cpu, _) = booted(&built.image);
        let mut stores = Vec::new();
        while cpu.pc != built.image.symbols.pass {
            assert!(cpu.cycles < 30_000, "selection fixture completes");
            if let Some(store) = spi_store(&cpu) { stores.push(store); }
            cpu.step().unwrap();
        }
        assert_eq!(stores.iter().any(|&(addr, value)| addr == 0x4c && value == 0x50), spi, "SS/MISO declaration selects the safe path: {extra}");
        if !spi { assert!(stores.is_empty(), "fallback touches no SPI register"); }
    }
    for (data, clock) in [(11, 13), (6, 7)] {
        let source = format!("let ss = output(PB2)\nlet data = output(D{data})\nlet clock = output(D{clock})\nvar n: i32 = 0\nfn setup() {{\nshift_out(data, clock, 1, n)\nn = n + 33\nshift_out(data, clock, 1, n)\n}}\n");
        let c = compiled(&source);
        let m = module::load(&c.module, &PROFILE).unwrap();
        let mut model = Model::default();
        Device::new(module::load(&c.module, &PROFILE).unwrap(), &PROFILE).boot(&mut model);
        for level in [Level::Off, Level::Faults, Level::Ports] {
            let image = build_module(&m, &Options { watch: level, ..Options::default() }).unwrap().image;
            let (mut cpu, _) = booted(&image);
            cpu.events.clear();
            to_symbol(&mut cpu, image.symbols.pass, 10_000);
            assert!(!cpu.events.iter().any(|e| matches!(e, fab_elec_mcu_avr::sim::Event::Pin { .. })), "{level:?}: invalid bits drive no edge on either pin");
            assert_eq!(cpu.io(0x4c), 0, "invalid bits never enable SPI");
            if level == Level::Off {
                let W::Fault { site, .. } = model.events[1] else { panic!("reference reports two faults") };
                assert_eq!(fault_cell(&cpu, &image, watch::FAULT_SHIFT_BITS), (2, site), "off cell counts both faults and retains the last site");
            }
            drain(&mut cpu, &image);
            assert_eq!(observed(&cpu), if level == Level::Off { vec![] } else { model.events.clone() }, "{level:?}: fault code, site and signed bit argument match the reference");
            assert!(model.events.iter().all(|e| matches!(e, W::Fault { code: watch::FAULT_SHIFT_BITS, arg: 0 | 33, .. })), "both computed invalid counts reach the reference fault path");
        }
    }
}

#[test]
fn shift_computed_pins_are_refused_at_the_call() {
    use fab_elec_mcu_rt::{format::OP_PORT, ports};
    for (data, clock, computed) in [("pin", "clock", "data"), ("data", "pin", "clock"), ("pin", "pin", "data and clock")] {
        let call = format!("shift_out({data}, {clock}, 1, 24)");
        let source = format!("let data = output(PB3)\nlet clock = output(PB5)\nvar pin: i32 = 11\nfn setup() {{ {call} }}\n");
        let c = compiled(&source);
        let m = module::load(&c.module, &PROFILE).unwrap();
        let refusal = build_module(&m, &Options::default()).err().expect("computed pins are refused");
        let at = refusal.code as usize;
        assert_eq!(&m.code()[at..at + 2], &[OP_PORT, u32::from(ports::SHIFT_OUT)], "refusal names the port instruction");
        let site = &c.sites[m.code()[at + 2] as usize];
        assert_eq!(&source[site.start as usize..site.end as usize], call, "refusal retains the exact call location");
        assert!(refusal.message.ends_with(&format!("`shift_out` on the 328P needs its data and clock pins named by declarations; this call computes its {computed} pin")), "specific refusal: {}", refusal.message);
        let diagnostics = firmware(&source, 16e6, &[]).unwrap_err();
        let span = diagnostics[0].span.clone().expect("firmware refusal has a source span");
        assert!(source[span].contains(&call), "firmware pane highlights the refused call");
    }
    let refusal = build_module(&module::load(&compiled("fn setup() { probe(1, 2) }").module, &PROFILE).unwrap(), &Options::default())
        .err().expect("probe is refused on the AVR");
    assert!(refusal.message.ends_with("`probe` is not available on the 328P yet"), "specific refusal: {}", refusal.message);
}

#[test]
fn eeprom_ports_match_reference_frames_faults_and_restored_contents() {
    use fab_elec_mcu_avr::insn::{decode, Insn};
    use watch::{Event as W, Level};
    let source = "\
var fresh: i32 = 0
var stored: i32 = 0
var last: i32 = 0
var negative: i32 = -1
var past: i32 = 1024
var bad_low: i32 = 99
var bad_high: i32 = 99
fn setup() {
    fresh = eeprom_read(0)
    eeprom_write(0, 0x1234)
    eeprom_write(1023, -2)
    eeprom_write(0, 0x1AB)
    stored = eeprom_read(0)
    last = eeprom_read(1023)
    eeprom_write(negative, 7)
    bad_low = eeprom_read(negative)
    eeprom_write(past, 8)
    bad_high = eeprom_read(past)
}
";
    let c = compiled(source);
    let m = module::load(&c.module, &PROFILE).unwrap();
    for level in [Level::Off, Level::Faults, Level::Ports] {
        let built = build_module(&m, &Options { watch: level, ..Options::default() }).expect("EEPROM ports build");
        let image = &built.image;
        assert_eq!(built.map.find(image.symbols.eeprom_read), Where::Runtime("eeprom_read"));
        assert_eq!(built.map.find(image.symbols.eeprom_write), Where::Runtime("eeprom_write"));
        let mut bytes = vec![0xff; 1024];
        for restored in [false, true] {
            let mut model = Model::default(); model.eeprom.copy_from_slice(&bytes);
            let mut device = Device::new(module::load(&c.module, &PROFILE).unwrap(), &PROFILE);
            device.boot(&mut model).expect("reference setup runs");
            let mut cpu = Cpu::new(&image.words);
            cpu.set_eeprom(&bytes);
            let snapshot = cpu.clone();
            assert!(std::ptr::eq(cpu.eeprom().as_ptr(), snapshot.eeprom().as_ptr()), "snapshot shares committed bytes");
            to_symbol(&mut cpu, image.symbols.t0, 30_000);
            assert_eq!(cpu.read(0x60), 0, "reset disables the watchdog");
            let mut effects = Vec::new();
            let start = cpu.cycles;
            while cpu.pc != image.symbols.pass {
                assert!(cpu.cycles - start < built.entries[0].max_cycles + 10_000, "EEPROM setup completes");
                // Observe actual EEPROM strobes independently of watch output,
                // including at Off and Faults. No ISR accesses these registers.
                let effect = if cpu.will_execute() {
                    match decode(&cpu.flash[cpu.pc as usize..]).unwrap().0 {
                        Insn::Sbi(0x1f, bit @ (0 | 1)) => Some((bit, u16::from_le_bytes([cpu.read(0x41), cpu.read(0x42)]),
                            u16::from_le_bytes([cpu.r[24], cpu.r[25]]))),
                        _ => None,
                    }
                } else { None };
                cpu.step().expect("EEPROM setup instruction executes");
                assert_eq!(cpu.read(0x3f) & 0x38, 0, "EEPM stays 00 and EERIE is never enabled");
                if let Some((bit, addr, site)) = effect {
                    let value = cpu.read(0x40);
                    effects.push(if bit == 0 { W::EepromRead { addr, value, site } } else { W::EepromWrite { addr, value, site } });
                }
            }
            assert_eq!(state(&cpu, image), device.state(), "{level:?}, restored={restored}: reference state");
            assert_eq!(state(&cpu, image), [if restored { 0xab } else { 255 }, 0xab, 254, u32::MAX, 1024, 0, 0],
                "fresh/readback bytes are zero-extended; invalid reads return zero");
            assert_eq!(effects, model.outputs.iter().map(|(_, event)| *event).collect::<Vec<_>>(),
                "{level:?}: strobes match reference effects, including back-to-back writes and overwrite");
            assert_eq!(cpu.eeprom(), model.eeprom, "{level:?}: all EEPROM contents, including invalid-write guards");
            assert_eq!(snapshot.eeprom(), bytes, "commits leave the old copy-on-write snapshot untouched");
            assert_ne!(cpu.sreg & 128, 0, "accesses preserve the running caller's interrupt enable");
            if level == Level::Off {
                let W::Fault { site, .. } = *model.events.last().unwrap() else { panic!("last call faults") };
                assert_eq!(fault_cell(&cpu, image, watch::FAULT_EEPROM_ADDR), (4, site), "Off counts faults and retains the last site");
            }
            bytes = cpu.eeprom().to_vec();
            drain(&mut cpu, image);
            let expected = model.events.iter().copied().filter(|event| match level {
                Level::Off => false, Level::Faults => matches!(event, W::Fault { .. }), Level::Ports => true,
            }).collect::<Vec<_>>();
            assert_eq!(observed(&cpu), expected, "{level:?}: ordered watch events, including fault arguments and sites");
            for (frame, _) in frames(&cpu) {
                if matches!(frame.event, W::EepromRead { .. } | W::EepromWrite { .. }) {
                    let mut wire = [0; watch::MAX_WIRE];
                    let n = watch::encode(&frame, &mut wire);
                    assert_eq!(watch::cobs_decode(&wire[..n - 1], &mut [0; watch::MAX_BODY]), Some(12), "five-byte EEPROM payload plus header and CRC");
                }
            }
        }
    }
}

#[test]
fn eeprom_stub_bounds_include_previous_write_and_cpu_halts() {
    use watch::Level;
    let c = compiled("fn setup() { eeprom_write(0, 1) eeprom_read(0) }\n");
    let m = module::load(&c.module, &PROFILE).unwrap();
    let charged = (u64::from(PROFILE.eeprom_write_ns) * u64::from(PROFILE.clock_hz)).div_ceil(1_000_000_000);
    assert_eq!((PROFILE.eeprom_write_ns, charged), (3_662_223, 58_596), "calibrated RC allowance rounds upwards");
    for clock in [8_000_000, 18_432_000] {
        let profile = chip::profile(clock);
        let c = compile("fn setup() { eeprom_write(0, 1) eeprom_read(0) }\n", &profile, &[], &[]).unwrap();
        let m = module::load(&c.module, &profile).unwrap();
        let image = build_module(&m, &Options { f_clk: clock, watch: Level::Off, ..Options::default() }).unwrap().image;
        let clocks = (u64::from(profile.eeprom_write_ns) * u64::from(clock)).div_ceil(1_000_000_000);
        assert_eq!(u64::from(image.bounds.eeprom_wait), clocks.div_ceil(3) * 3, "{clock}: wait follows the CPU clock and rounds to full poll iterations");
    }
    for level in [Level::Off, Level::Faults, Level::Ports] {
        let built = build_module(&m, &Options { watch: level, ..Options::default() }).unwrap();
        let image = &built.image;
        assert_eq!(u64::from(image.bounds.eeprom_wait), charged.div_ceil(3) * 3, "wait uses the profile, rounded to complete busy polls");
        assert!(built.entries[0].max_cycles >= u64::from(image.bounds.eeprom_read + image.bounds.eeprom_write), "entry bound charges both stub waits");
        let (mut cpu, _) = booted(image);
        drain(&mut cpu, image); cpu.sreg = 0;
        for (read, start, end, bound) in [
            (false, image.symbols.eeprom_write, image.symbols.eeprom_write_end, image.bounds.eeprom_write),
            (true, image.symbols.eeprom_read, image.symbols.eeprom_read_end, image.bounds.eeprom_read),
        ] {
            // Prime a real write with this image's stub, then immediately call
            // the access being measured; neither drain nor clock fast-forward intervenes.
            let saved = cpu.r;
            cpu.r[16..20].copy_from_slice(&1023u32.to_le_bytes()); cpu.r[20] = 0xa5;
            cpu.r[24..26].copy_from_slice(&0x1234u16.to_le_bytes());
            let prime = invoke(&mut cpu, image, image.symbols.eeprom_write);
            within("EEPROM prime", prime.clocks, image.bounds.eeprom_write);
            assert_ne!(cpu.read(0x3f) & 2, 0, "write returns with programming still busy");
            cpu.r[16..20].copy_from_slice(&1023u32.to_le_bytes()); cpu.r[20] = 0x5a;
            cpu.r[24..26].copy_from_slice(&0x1234u16.to_le_bytes());
            let waiting = invoke(&mut cpu, image, start);
            within("EEPROM waiting access", waiting.clocks, bound);
            assert_eq!(waiting.services, 0, "uninterrupted bound excludes ISR service");
            // Nominal emulator time is 7.2% below the calibrated RC allowance.
            // Remove that known allowance to check tightness of the emitted path.
            let allowance = charged - 54_400;
            assert!((waiting.clocks + allowance) * 100 >= u64::from(bound) * 98, "waiting path is within 2% after the explicit nominal/charged timing margin");
            if read { assert_eq!(u32::from_le_bytes(cpu.r[16..20].try_into().unwrap()), 0xa5, "waiting read sees the completed write"); }
            else { assert_ne!(cpu.read(0x3f) & 2, 0, "waiting write starts its own programming cycle"); }
            assert_eq!(cpu.sreg & 128, 0, "stub preserves a masked caller");
            assert_eq!(&cpu.r[2..16], &saved[2..16], "stub preserves runtime registers");
            assert_eq!(&cpu.r[28..30], &saved[28..30], "stub preserves Y");
            eprintln!("EEPROM {read:?} {level:?}: stub={} bytes, bound={bound}, wait={}, waiting measured={}", (end - start) * 2, image.bounds.eeprom_wait, waiting.clocks);
            // Invalid access must not wait for or disturb the in-flight write.
            for addr in [-1i32, 1024, 65536, i32::MIN] {
                cpu.r[16..20].copy_from_slice(&addr.to_le_bytes());
                cpu.r[24..26].copy_from_slice(&0x4321u16.to_le_bytes());
                let registers = [cpu.read(0x41), cpu.read(0x42), cpu.read(0x40)];
                let call = invoke(&mut cpu, image, start);
                within("EEPROM address fault", call.clocks, bound);
                assert!(call.clocks < 1000, "invalid address does not enter the busy poll");
                assert_eq!([cpu.read(0x41), cpu.read(0x42), cpu.read(0x40)], registers, "invalid access leaves EEPROM I/O untouched");
                if read { assert_eq!(&cpu.r[16..20], &[0; 4], "invalid read returns zero"); }
            }
            if level == Level::Ports {
                let overflows = cpu.r[13];
                let clocks = encode_frame(&mut cpu, image);
                let service = u64::from(cpu.r[13].wrapping_sub(overflows)) * u64::from(fab_elec_mcu_emit_avr::runtime::TOV_INT);
                within("EEPROM foreground encoder", clocks - service, image.bounds.encode);
            }
            drain(&mut cpu, image); cpu.sreg = 0;
        }
        // Pin the emulator/profile pair by timing an actual mode-00 write.
        // Direct bus writes account the two CPU halt clocks as instructions do.
        cpu.write(0x41, 7); cpu.write(0x42, 0); cpu.write(0x40, 0x3c);
        cpu.write(0x3f, 4);
        let start = cpu.cycles; cpu.write(0x3f, 2);
        assert_eq!(cpu.cycles - start, 2, "EEPE charges its two halt cycles");
        let nop = scratch(&mut cpu, image);
        while cpu.read(0x3f) & 2 != 0 {
            assert!(cpu.cycles - start <= charged, "emulator may never exceed the charged write time");
            cpu.pc = nop; cpu.step().unwrap();
        }
        assert_eq!(cpu.cycles - start, 54_400, "mode 00 remains the nominal 3.4 ms at 16 MHz");
        assert_eq!(cpu.eeprom()[7], 0x3c, "nominal deadline commits the byte");
        if level == Level::Off {
            cpu.r[16..20].copy_from_slice(&7u32.to_le_bytes()); cpu.r[20] = 0x96;
            invoke(&mut cpu, image, image.symbols.eeprom_write);
            cpu.r[16..20].copy_from_slice(&7u32.to_le_bytes());
            cpu.write(0x35, 1); cpu.write(0x46, 252); cpu.sreg = 128;
            let call = invoke(&mut cpu, image, image.symbols.eeprom_read);
            assert!(call.services > 0, "Timer0 continues to be serviced during the busy poll");
            within("EEPROM read with TOV excluded", call.clocks - call.services as u64 * u64::from(fab_elec_mcu_emit_avr::runtime::TOV_INT), image.bounds.eeprom_read);
            assert_eq!(u32::from_le_bytes(cpu.r[16..20].try_into().unwrap()), 0x96, "interrupts leave address and value intact");
            assert_ne!(cpu.sreg & 128, 0, "waiting read retains enabled interrupts");
        }
    }
}

#[test]
fn eeprom_save_cost_serial_ram_and_selective_linking() {
    use fab_elec_mcu_rt::ports;
    use watch::Level;
    let writes: String = (0..16).map(|addr| format!("eeprom_write({addr}, {addr})\n")).collect();
    let source = format!("let save = after(ms(1), fn() {{\n{writes}}})\n");
    let c = compiled(&source);
    let m = module::load(&c.module, &PROFILE).expect("sixteen-byte handler verifies");
    // Each access is charged the write time in whole ops: ceil(3,662,223 / 62) = 59,069 at 16 MHz.
    let waits = u64::from(PROFILE.eeprom_write_ns).div_ceil(u64::from(PROFILE.op_ns)) * 16;
    assert_eq!(PROFILE.eeprom_access_ops() * 16, waits, "each access is charged the write time");
    assert!(waits <= PROFILE.max_entry_ops, "sixteen charged waits fit the handler limit: {waits}");
    let cost = m.entries()[0].cost;
    assert!(cost >= waits && cost <= PROFILE.max_entry_ops, "save includes waits and source work");
    for level in [Level::Off, Level::Faults, Level::Ports] {
        let built = build_module(&m, &Options { watch: level, ..Options::default() }).expect("sixteen-byte handler builds");
        assert_eq!((built.image.symbols.eeprom_read, built.image.bounds.eeprom_read), (0, 0), "write-only image links no read stub");
        assert!(built.entries[0].max_cycles >= u64::from(built.image.bounds.eeprom_write) * 16, "every native call charges its wait");
        let read = compiled("var value: i32 = 0\nfn setup() { value = eeprom_read(1023) }\n");
        let read = module::load(&read.module, &PROFILE).unwrap();
        let read = build_module(&read, &Options { watch: level, ..Options::default() }).unwrap();
        assert_eq!((read.image.symbols.eeprom_write, read.image.bounds.eeprom_write), (0, 0), "read-only image links no write stub");
        let (mut cpu, _) = booted(&read.image);
        to_symbol(&mut cpu, read.image.symbols.pass, 100_000);
        assert_eq!(state(&cpu, &read.image), [255], "read-only image sees erased EEPROM");
        drain(&mut cpu, &read.image);
        assert_eq!(observed(&cpu).len(), usize::from(level == Level::Ports), "single-kind encoder handles read-only images");
        let write = compiled("fn setup() { eeprom_write(1023, 42) }\n");
        let mut model = Model::default();
        Device::new(module::load(&write.module, &PROFILE).unwrap(), &PROFILE).boot(&mut model);
        let write = module::load(&write.module, &PROFILE).unwrap();
        let write = build_module(&write, &Options { watch: level, ..Options::default() }).unwrap();
        let (mut cpu, _) = booted(&write.image);
        to_symbol(&mut cpu, write.image.symbols.pass, 10_000);
        drain(&mut cpu, &write.image);
        assert_eq!(observed(&cpu), if level == Level::Ports { model.events } else { vec![] }, "single-kind encoder handles write-only images");

        // Removing EEPROM capability from the profile cannot change a non-EEPROM
        // image, its fault layout, or its foreground encoder at any watch level.
        for source in ["fn setup() {}\n", "let led = output(PB5)\nfn setup() { write(led, high) }\n"] {
            let c = compiled(source);
            let without = fab_elec_mcu_rt::Profile { eeprom: 0, eeprom_write_ns: 0, ..PROFILE };
            let plain = compile(source, &without, &chip::compiler_pins(), &[]).unwrap();
            assert_eq!(c.module, plain.module, "EEPROM profile fields cannot affect unrelated module bytes");
            let options = Options { watch: level, ..Options::default() };
            let enabled = build_module(&module::load(&c.module, &PROFILE).unwrap(), &options).unwrap();
            let disabled = build_module(&module::load(&plain.module, &without).unwrap(), &options).unwrap();
            assert_eq!(enabled.image.bytes(), disabled.image.bytes(), "unrelated image bytes at {level:?}");
            assert_eq!((enabled.image.symbols.eeprom_read, enabled.image.symbols.eeprom_write, enabled.image.bounds.eeprom_wait), (0, 0, 0), "unrelated images link no EEPROM code");
            assert_eq!(enabled.image.bounds.encode, if level == Level::Off { 0 } else { 2119 }, "unrelated images retain the original encoder bound");
            assert!(!enabled.image.layout.fault_cells.iter().any(|cell| cell.code == watch::FAULT_EEPROM_ADDR), "no EEPROM fault cell without EEPROM");
        }
        let default = fab_elec_mcu_emit_avr::Program { watch: level, ..Default::default() };
        assert_eq!(default.ports & ((1 << ports::EEPROM_READ) | (1 << ports::EEPROM_WRITE)), 0, "hand-built defaults preserve the pre-EEPROM port set");
    }
    let note = firmware(&source, 16e6, &[]).expect("save firmware builds").notes.into_iter().find(|note| note.starts_with("EEPROM:")).unwrap();
    assert!(note.contains("Assumed") && note.contains("Table 8-2") && note.contains("Table 29-9") && note.contains("3 V, 25 °C"), "firmware exposes the timing assumption");
    let serial = format!("let link = serial(57600, fn() {{\n{writes}}})\n");
    let c = compiled(&serial);
    let m = module::load(&c.module, &PROFILE).unwrap();
    let refusal = build_module(&m, &Options::default()).err().expect("full save with RX cannot fit its honest FIFO in RAM");
    assert!(refusal.message.contains("RX FIFO") && refusal.message.contains("RAM"), "FIFO sizing includes EEPROM waits: {refusal}");
}

#[test]
fn eeprom_and_shift_frames_share_the_encoder() {
    for access in ["value = eeprom_read(0)", "eeprom_write(0, value)", "eeprom_write(0, value) value = eeprom_read(0)"] {
        let source = format!("let data = output(PD6)\nlet clock = output(PD7)\nvar value: i32 = 1\nfn setup() {{\nshift_out(data, clock, value, 1)\n{access}\nshift_out(data, clock, value, 1)\n}}\n");
        replay_program(&source, 10_000_000);
    }
}

fn replay_program(source: &str, until_ns: u64) -> (Vec<watch::Event>, Vec<u32>) {
    replay_analog(source, until_ns, &[])
}

fn replay_analog(source: &str, until_ns: u64, analog: &[(u32, u16)]) -> (Vec<watch::Event>, Vec<u32>) {
    let c = compiled(source);
    let m = module::load(&c.module, &PROFILE).unwrap();
    let built = build_module(&m, &Options { watch: watch::Level::Ports, ..Options::default() }).unwrap_or_else(|e| panic!("{e}\n{source}"));
    let wide = build_module(&m, &Options { watch: watch::Level::Ports, full_ranges: true, ..Options::default() }).unwrap();
    let actual = replay_image(source, until_ns, analog, m, &built.image);
    let expected = replay_image(source, until_ns, analog, module::load(&c.module, &PROFILE).unwrap(), &wide.image);
    assert_eq!(actual, expected, "FALSIFIER 1: wide/narrow device replay\n{source}");
    actual
}

fn replay_image(source: &str, until_ns: u64, analog: &[(u32, u16)], m: Module<'_>, image: &Image)
    -> (Vec<watch::Event>, Vec<u32>) {
    let (mut cpu, t0) = booted(image);
    cpu.set_avcc(5.0);
    for &(pin, code) in analog {
        cpu.set_analog(fab_elec_mcu_avr::chip::adc_channel(pin).unwrap(), (f64::from(code) + 0.5) / 1024.0 * 5.0);
    }
    run(&mut cpu, until_ns * 2 / 125);
    // The end is between firings, so finishing watch output cannot run another handler.
    assert!(cpu.cycles >= t0 + until_ns * 2 / 125);
    // The drain takes the CPU over: first let a masked block in progress
    // finish. drain also completes any in-flight foreground frame encode.
    let end = cpu.cycles + 10_000;
    while cpu.sreg & 0x80 == 0 {
        assert!(cpu.cycles < end, "the interrupt in progress must return");
        cpu.step().expect("the interrupt runs on to its return");
    }
    let actual_state = state(&cpu, image);
    drain(&mut cpu, image);
    let mut device = Device::new(m, &PROFILE);
    let mut model = Model { analog: analog.to_vec(), ..Model::default() };
    device.boot(&mut model); device.run_until(until_ns, &mut model);
    let events = observed(&cpu);
    assert_eq!(events, model.events, "port replay\n{source}");
    assert_eq!(actual_state, device.state(), "state replay\n{source}");
    if !analog.is_empty() {
        assert!(frames(&cpu).iter().all(|(frame, _)| !matches!(frame.event, watch::Event::Read { .. })),
            "analog reads emit no digital read record, including at Ports level");
    }
    (events, actual_state)
}

#[test]
fn analog_reads_match_device_and_fault_at_their_sites() {
    let source = "\
let pot = analog(PC0)
let extra = analog(ADC6)
let led = output(PB5)
let aux = output(PB4)
var selected: i32 = 20
var missing: i32 = 21
var pc_pin: i32 = 14
var a: i32 = 0
var b: i32 = 0
var c: i32 = 0
var bad: i32 = 7
var digital: i32 = 7
fn setup() {
    a = analog_read(pot)
    b = analog_read(extra)
    c = analog_read(selected)
    bad = analog_read(missing)
    write(led, if a > 511 { high } else { low })
    write(aux, if b == c { high } else { low })
    digital = read(pc_pin)
    write(pc_pin, high)
    digital = read(selected)
    write(selected, high)
}
";
    let (events, state) = replay_analog(source, 4_000_000, &[(14, 321), (20, 789)]);
    assert_eq!(state, [20, 21, 14, 321, 789, 789, 0, 0], "known and computed codes, and zero fault results");
    let c = compiled(source);
    let site = |call: &str| c.sites.iter().position(|s|
        &source[s.start as usize..s.end as usize] == call).expect("call has a source site") as u16;
    assert_eq!(events, [
        watch::Event::Fault { code: watch::FAULT_PIN, site: site("analog_read(missing)"), arg: 21 },
        watch::Event::Write { pin: 13, level: false, site: site("write(led, if a > 511 { high } else { low })") },
        watch::Event::Write { pin: 12, level: true, site: site("write(aux, if b == c { high } else { low })") },
        watch::Event::Fault { code: watch::FAULT_PIN, site: site("read(pc_pin)"), arg: 14 },
        watch::Event::Fault { code: watch::FAULT_PIN, site: site("write(pc_pin, high)"), arg: 14 },
        watch::Event::Fault { code: watch::FAULT_PIN, site: site("read(selected)"), arg: 20 },
        watch::Event::Fault { code: watch::FAULT_PIN, site: site("write(selected, high)"), arg: 20 },
    ], "output comparisons and digital/analog role faults retain the exact source sites");
}

#[test]
fn known_analog_pins_link_only_the_converter_and_match_device() {
    let source = "\
let pot = analog(PC0)
let extra = analog(ADC6)
let led = output(PB5)
var total: i32 = 0
fn setup() {
    total = analog_read(pot) + analog_read(extra)
    write(led, if total > 1023 { high } else { low })
}
";
    let c = compiled(source);
    let m = module::load(&c.module, &PROFILE).unwrap();
    for level in [watch::Level::Off, watch::Level::Faults, watch::Level::Ports] {
        let built = build_module(&m, &Options { watch: level, ..Options::default() }).unwrap();
        assert_ne!(built.image.symbols.adc_convert, 0, "{level:?}: known analog pins link the converter");
        assert!(built.image.bounds.adc_convert > built.image.bounds.adc_sample && built.image.bounds.adc_sample > 0,
            "{level:?}: converter and sample bounds are present independently of watch records");
        assert_eq!((built.image.symbols.adc, built.image.bounds.adc), (0, 0),
            "{level:?}: known analog pins need no computed-pin stub");
    }
    let (events, state) = replay_analog(source, 4_000_000, &[(14, 321), (20, 789)]);
    assert_eq!(state, [1110], "the first result survives the second converter CALL");
    assert!(matches!(events.as_slice(), [watch::Event::Write { pin: 13, level: true, .. }]),
        "the two known analog inputs drive the output in lockstep");
}

#[test]
fn analog_tables_clock_limits_and_firmware_diagnostics() {
    use fab_elec_mcu_rt::analog;
    assert_eq!(PROFILE.pins, 22, "the profile includes ADC6 and ADC7");
    for pin in 0..=22 {
        let channel = analog::ATMEGA328P.get(pin).map(|p| p.channel);
        assert_eq!(channel, fab_elec_mcu_avr::chip::adc_channel(pin), "pin {pin}: runtime and machine channel tables agree");
        if let Some(p) = analog::ATMEGA328P.get(pin) {
            assert_eq!(p.digital, chip::by_number(pin).is_some(), "pin {pin}: digital capability agrees");
        }
    }
    let declared = "let pot = analog(ADC6)\nfn setup() {}";
    for f_clk in [99_999, 25_600_001] {
        let d = firmware(declared, f64::from(f_clk), &[]).err().expect("analog clock outside datasheet range is refused");
        assert_eq!(d.len(), 1, "one ADC clock refusal");
        assert!(d[0].message.contains(&format!("{f_clk} Hz")) && d[0].message.contains("50..=200 kHz"),
            "clock refusal names the CPU clock and ADC range: {d:?}");
        assert!(firmware("fn setup() {}", f64::from(f_clk), &[]).is_ok(), "ADC limits do not restrict a digital program");
    }
    for f_clk in [100_000, 25_600_000] {
        assert!(firmware(declared, f64::from(f_clk), &[]).is_ok(), "ADC clock boundary {f_clk} is inclusive");
    }
    let source = "let led = output(ADC6)\nfn setup() {}";
    let d = firmware(source, 16e6, &[]).err().expect("ADC6 cannot drive an output");
    assert_eq!(d[0].message, "pin 20 (ADC6) is an analog-only pin: declare it with `analog(..)`", "compiler refusal survives firmware()");
    assert_eq!(d[0].span.as_ref().map(|s| &source[s.clone()]), Some("output(ADC6)"), "compiler refusal retains its source span");
    for (source, computed) in [
        ("let pot = analog(ADC6)\nvar code: i32 = 0\nfn setup() { code = analog_read(pot) }", false),
        ("let pot = analog(ADC6)\nvar which: i32 = 20\nvar code: i32 = 0\nfn setup() { code = analog_read(which) }", true),
    ] {
        let fw = firmware(source, 16e6, &[]).unwrap();
        let c = compiled(source);
        let m = module::load(&c.module, &PROFILE).unwrap();
        let bounds = build_module(&m, &Options::default()).unwrap().image.bounds;
        let us = |cycles: u32| format!("{:.3} us", f64::from(cycles) / 16.0);
        let notes: Vec<_> = fw.notes.iter().filter(|s| s.starts_with("analog inputs:")).collect();
        assert_eq!(notes.len(), 1, "one ADC timing note per firmware image");
        assert!(notes[0].contains("125 kHz (16 MHz / 128)") && notes[0].contains(&us(bounds.adc_convert))
            && notes[0].contains(&us(bounds.adc_sample)), "ADC note uses the emitted cycle bounds: {notes:?}");
        assert_eq!(notes[0].contains("computed-pin stub"), computed, "only a linked computed-pin stub has a note");
        assert_eq!(bounds.adc != 0, computed, "only a linked computed-pin stub has a bound");
        if computed { assert!(notes[0].contains(&us(bounds.adc)), "stub note uses its emitted bound: {notes:?}"); }
    }
    assert!(!firmware(declared, 16e6, &[]).unwrap().notes.iter().any(|s| s.starts_with("analog inputs:")),
        "an unused analog declaration has no conversion timing note");
}

#[test]
fn programs_match_device() {
    for seed in [1, 7, 19, 37, 71, 101, 257, 509, 1021, 65537, 0x55555555, 0xdeadbeef] {
        let source = generated(seed);
        replay_program(&source, 24_000_000);
    }
    replay_program("let out = output(D13)\nvar value: i32 = 256\nfn setup() { write(out, value) }", 1_000_000);
    // Fault arguments stay intact, including values whose low byte is pin 13.
    replay_program("let out = output(D13)\nvar x: i32 = 300\nvar y: i32 = 269\nvar z: i32 = -243\nfn setup() { write(x, high) write(y, high) write(z, high) }", 1_000_000);
    // A float join of constants converted to i32 folds at emit time (floor, both arms, the condition).
    replay_program("let out = output(D13)\nvar n: i32 = 0\nvar p = false\nlet tick = every(ms(1), fn() {\n    p = !p\n    let s: i32 = if p { 2.5 } else { -1.5 }\n    n = n + n + s\n    write(out, n)\n})\n", 5_500_000);
    let source = "var x: i32 = 0\nfn setup() {\n while x >= 0 { x = x + 1 }\n}\n";
    let (events, state) = replay_program(source, 20_000_000);
    assert_eq!(state, [1024], "loop must stop at its source cap");
    assert!(matches!(events.as_slice(), [watch::Event::Fault { code: watch::FAULT_LOOP_CAP, arg: 0, .. }]), "one LoopCap at the source site: {events:?}");

    // Empty and full queues respect the stub's bound and saved A/Y contract.
    let image = fab_elec_mcu_emit_avr::build(&Default::default()).unwrap();
    let (mut cpu, _) = booted(&image);
    for full in [false, true] {
        cpu.sreg = 0;
        cpu.write(image.layout.head, if full { image.layout.records - 1 } else { 0 });
        cpu.write(image.layout.tail, 0);
        cpu.r[16..20].copy_from_slice(&[0x12, 0x34, 0x56, 0x78]);
        cpu.r[24..26].copy_from_slice(&0x1234u16.to_le_bytes());
        let saved = cpu.r;
        let call = invoke(&mut cpu, &image, image.symbols.loop_cap);
        within(&format!("loop_cap full={full}"), call.clocks, image.bounds.loop_cap);
        assert_eq!(&cpu.r[16..20], &saved[16..20], "saved A");
        assert_eq!(&cpu.r[28..30], &saved[28..30], "saved Y");
        assert_eq!(load(&cpu, image.layout.record + 9, 4), 0, "LoopCap has no argument");
    }
}

/// Grow transfers across a large arm and back edge, then check the same image's maps,
/// stub calls, uninterrupted entry bound and interpreter replay.
#[test]
fn relaxed_large_entry_keeps_maps_bounds_and_device_replay() {
    use fab_elec_mcu_avr::insn::{decode, Insn::*};
    use std::collections::BTreeSet;

    let mut source = String::from("\
let out = output(D13)
let motor = pwm(D9, hz(490))
var work: i32 = 0
var choose = true
fn setup() { write(out, low) }
let tick = after(ms(1), fn() {
    duty(motor, 1)
    for i in 0..2 {
        if choose {
");
    // Mixing operations prevents integer reassociation from collapsing the arm.
    // Keep the arm beyond RJMP range even when Acc removes the scratch round trips.
    for _ in 0..160 { source.push_str("            work = (work ^ 37) + 1\n"); }
    source.push_str("\
        } else { work = work - 1 }
        choose = !choose
    }
    if work > 0 { write(out, high) }
    duty(motor, 2)
})
let tail = after(ms(4), fn() { work = work + 7 write(out, low) })
");
    let c = compiled(&source);
    let m = module::load(&c.module, &PROFILE).unwrap();
    let built = build_module(&m, &Options {
        watch: watch::Level::Ports, marks: c.marks.clone(), conds: c.cond_marks.clone(),
        ..Options::default()
    }).unwrap();
    let (image, map) = (&built.image, &built.map);
    let tick = m.entry("tick").unwrap();
    assert!(built.entries[tick].flash_words > 2048, "fixture crosses the relative transfer range");
    let mut starts = Vec::new();
    let mut transfers = [0; 4]; // RJMP, JMP, RCALL, CALL in the large entry.
    for (entry, &(start, end)) in map.entries.iter().enumerate() {
        let mut boundaries = BTreeSet::new();
        let mut pc = start;
        while pc < end {
            boundaries.insert(pc);
            let (insn, len) = decode(&image.words[pc as usize..]).expect("entry instruction");
            if entry == tick {
                match insn {
                    Rjmp(_) => transfers[0] += 1,
                    Jmp(_) => transfers[1] += 1,
                    Rcall(k) => {
                        assert!((i64::from(pc) + 1 + i64::from(k)) < i64::from(image.symbols.setup), "RCALL targets a runtime stub");
                        transfers[2] += 1;
                    }
                    Call(target) => {
                        assert!(target < image.symbols.setup, "CALL targets a runtime stub");
                        transfers[3] += 1;
                    }
                    _ => {}
                }
            }
            pc += len as u32;
        }
        assert_eq!(pc, end, "entry {entry} ends on an instruction boundary");
        assert_eq!(decode(&image.words[end as usize - 1..]), Some((Ret, 1)), "entry {entry} includes its RET");
        assert_eq!(end - start, built.entries[entry].flash_words, "entry report uses the final size");
        starts.push(boundaries);
    }
    assert!(transfers.iter().all(|&n| n > 0), "large entry needs short and long jumps and stub calls: {transfers:?}");
    let setup = m.entry(fab_elec_mcu_rt::SETUP).unwrap();
    assert_eq!(map.entries[setup].0, image.symbols.setup);
    let handlers: Vec<_> = map.entries.iter().enumerate().filter_map(|(k, &(start, _))|
        (k != setup).then_some(start)).collect();
    assert_eq!(handlers, image.symbols.entries, "later handler symbols use the same relocation as the source map");
    for op in &map.ops {
        assert!(starts[op.entry].contains(&op.start), "op {} starts inside its entry", op.code);
        assert!(starts[op.entry].contains(&op.end) || op.end == map.entries[op.entry].1, "op {} ends at a boundary", op.code);
    }
    assert!(!map.stmt_starts.is_empty() && map.conds.len() >= 2, "fixture retains statement and condition marks");
    for &(pc, id) in &map.stmt_starts {
        assert!(starts[match c.stmts[id as usize].owner { fab_elec_mcu::Owner::Handler(k) => k as usize, _ => panic!("inlined owner") }].contains(&pc), "statement {id} starts inside its entry");
    }
    for &(pc, id) in &map.conds {
        assert!(starts[match c.conds[id as usize].owner { fab_elec_mcu::Owner::Handler(k) => k as usize, _ => panic!("inlined owner") }].contains(&pc), "condition {id} starts inside its entry");
        let (jump, len) = decode(&image.words[pc as usize + 1..]).expect("condition jump");
        assert!(matches!(jump, Rjmp(_) | Jmp(_)), "condition {id} skips a jump");
        let condition = &c.conds[id as usize];
        let text = &source[condition.site.start as usize..condition.site.end as usize];
        let bit = match text { "choose" => 1, "work > 0" => 4, _ => panic!("unexpected condition {text}") };
        assert_eq!(decode(&image.words[pc as usize..]), Some((Brbc(bit, len as i8), 1)),
            "condition {id} tests its true sense and skips exactly the jump to its else");
    }
    let (mut cpu, _) = booted(image);
    cpu.sreg = 0;
    cpu.write(0x6e, 0); // Measure only the handler, without Timer0 service.
    let measured = invoke(&mut cpu, image, map.entries[tick].0).clocks;
    assert!(measured <= built.entries[tick].max_cycles,
        "large entry: measured {measured} exceeds bound {}", built.entries[tick].max_cycles);
    assert_eq!(cpu.sreg & 128, 0, "measurement excludes interrupt service");
    replay_image(&source, 5_500_000, &[], m, image);
}

/// Steps to a deadline; a sleeping CPU stops there with Stop::Cycles.
fn step_to(cpu: &mut Cpu, end: u64) {
    if let Err(stop) = cpu.step_until(end) {
        assert_eq!(stop, fab_elec_mcu_avr::sim::Stop::Cycles, "run must reach its deadline");
    }
}

#[test]
fn off_fault_cells_match_device() {
    let source = "\
let led = output(PB5)
var missing_pin: i32 = -1
var missing_timer: i32 = -1
var bad_read: i32 = 7
var ticks: i32 = 0
fn setup() { stop(missing_timer) }
let tick = every(ms(1), fn() {
    ticks = ticks + 1
    write(missing_pin, high)
    bad_read = read(missing_pin)
    write(led, ticks & 1)
})
";
    let c = compiled(source);
    let m = module::load(&c.module, &PROFILE).unwrap();
    let built = build_module(&m, &Options { watch: watch::Level::Off, ..Options::default() }).unwrap();
    let image = &built.image;
    assert_unobserved_image(image);
    assert_eq!(image.layout.fault_cells.iter().map(|cell| cell.code).collect::<Vec<_>>(),
        [watch::FAULT_PIN, watch::FAULT_TIMER, watch::FAULT_OVERRUN], "only linked fault paths have cells");
    let fw = fab_elec_mcu_emit_avr::firmware_with_watch(source, 16e6, &[], watch::Level::Off).unwrap();
    assert_eq!(fw.debug.fault_cells, image.layout.fault_cells, "firmware publishes the data addresses");
    for level in [watch::Level::Faults, watch::Level::Ports] {
        let fw = fab_elec_mcu_emit_avr::firmware_with_watch(source, 16e6, &[], level).unwrap();
        assert!(fw.debug.fault_cells.is_empty(), "streaming images have no RAM fault cells");
    }
    let mut cpu = Cpu::new(&image.words);
    while cpu.pc != image.symbols.t0 {
        assert!(cpu.cycles < 30_000, "Off reset completes");
        assert_unobserved_uart(&cpu, false); cpu.step().unwrap();
    }
    let t0 = cpu.cycles;
    // Device's semantics do not depend on observation level; Hal still receives every fault.
    let mut device = Device::new(m, &PROFILE);
    let mut model = Model::default();
    device.boot(&mut model);
    for until_ns in [500_000, 1_500_000, 2_500_000, 3_500_000] {
        let end = t0 + until_ns * 2 / 125;
        while cpu.cycles < end {
            assert_unobserved_uart(&cpu, false); step_to(&mut cpu, end);
        }
        device.run_until(until_ns, &mut model);
        assert_eq!(state(&cpu, image), device.state(), "Off state at {until_ns} ns");
        for cell in &image.layout.fault_cells {
            let sites: Vec<_> = model.events.iter().filter_map(|event| match event {
                watch::Event::Fault { code, site, .. } if *code == cell.code => Some(*site), _ => None,
            }).collect();
            assert_eq!(fault_cell(&cpu, image, cell.code),
                (sites.len().min(65535) as u16, sites.last().copied().unwrap_or(0)),
                "Off fault {} at {until_ns} ns", cell.code);
        }
    }
    let pin_sites: Vec<_> = model.events.iter().filter_map(|event| match event {
        watch::Event::Fault { code: watch::FAULT_PIN, site, .. } => Some(*site), _ => None,
    }).collect();
    assert_eq!(pin_sites.len(), 6, "three firings raise two pin faults each");
    assert_ne!(pin_sites[0], pin_sites[1], "pin faults come from two different source sites");
    assert_eq!(pin_sites, [pin_sites[0], pin_sites[1]].repeat(3), "last site changes on every pin fault");
    assert!(frames(&cpu).is_empty(), "Off emits no frames");
}

#[test]
fn off_releases_uart_pins() {
    for pin in [0, 1] {
        let source = format!("let led = output(PD{pin})\nvar lit = false\nlet tick = every(ms(1), fn() {{ lit = !lit write(led, if lit {{ high }} else {{ low }}) }})\n");
        let fw = fab_elec_mcu_emit_avr::firmware_with_watch(&source, 16e6, &[], watch::Level::Off).unwrap();
        let mut cpu = Cpu::new(&fw.words);
        while cpu.cycles < 56_000 {
            assert_unobserved_uart(&cpu, false); step_to(&mut cpu, 56_000);
        }
        let edges: Vec<_> = cpu.events.iter().filter_map(|event| match *event {
            fab_elec_mcu_avr::sim::Event::Pin { port: 'D', bit, high, .. } if bit == pin => Some(high), _ => None,
        }).collect();
        assert_eq!(edges, [true, false, true], "PD{pin} toggles as program GPIO");
        let errors = firmware(&source, 16e6, &[]).unwrap_err();
        assert!(errors.iter().any(|e| e.message.contains("pin must be in 2..=19")),
            "the default Faults level still reserves PD{pin}");
        let serial = format!("{source}\nlet link = serial(19200, fn() {{ }})\n");
        let result = fab_elec_mcu_emit_avr::firmware_with_watch(&serial, 16e6, &[], watch::Level::Off);
        if pin == 0 {
            assert!(result.unwrap_err().iter().any(|e| e.message.contains("pin must be in 1..=19")), "serial owns RXD");
        } else {
            let fw = result.unwrap();
            let mut cpu = Cpu::new(&fw.words);
            while cpu.cycles < 56_000 {
                assert_unobserved_uart(&cpu, true); step_to(&mut cpu, 56_000);
            }
            assert!(cpu.events.iter().any(|e| matches!(e,
                fab_elec_mcu_avr::sim::Event::Pin { port: 'D', bit: 1, high: true, .. })), "serial leaves TXD free");
            assert_eq!(cpu.read(0xc1), 0x90, "serial only enables the receiver");
        }
    }
}

/// Observation is an image choice: pin effects, values and faults agree across levels.
#[test]
fn inline_pins_preserve_effects_state_and_faults_across_watch_levels() {
    use fab_elec_mcu_avr::{insn::{decode, Insn::*}, sim::Event};
    use watch::{Event as W, Level};
    let source = r#"
let led = output(D13)
let aux = output(D4)
let extra = output(D15)
let sense = input(D2, pull_up)
let quiet = input(D14, pull_none)
let motor = pwm(D9, hz(490))
var value: i32 = 65536
var chosen: i32 = 4
var input_pin: i32 = 2
var bad_input: i32 = 2
var bad_pwm: i32 = 9
var missing: i32 = 19
var sampled_hi: i32 = 0
var sampled_lo: i32 = 0
var sampled_dynamic: i32 = 0
fn setup() {
    write(led, high)
    write(led, low)
    write(aux, value)
    value = 0
    write(aux, value)
    write(extra, high)
    write(extra, low)
    sampled_hi = read(sense)
    sampled_lo = read(quiet)
    sampled_dynamic = read(input_pin)
    write(chosen, high)
    write(bad_input, high)
    write(bad_pwm, high)
    write(missing, high)
    duty(motor, 0)
}
"#;
    // Constant role mistakes are already refused by the module verifier. Computed pins
    // exercise the runtime fault path without changing that language rule.
    let c = compiled(source);
    let m = module::load(&c.module, &PROFILE).unwrap();
    let site = |text: &str| {
        let line = source.lines().position(|l| l.trim() == text).expect("source line") as u32 + 1;
        c.sites.iter().position(|s| s.line == line).expect("port site") as u16
    };
    let fault = |text, arg| W::Fault { code: watch::FAULT_PIN, site: site(text), arg };
    let faults = vec![fault("write(bad_input, high)", 2), fault("write(bad_pwm, high)", 9), fault("write(missing, high)", 19)];
    let mut runs = Vec::new();
    for watch in [Level::Faults, Level::Ports] {
        let built = build_module(&m, &Options { watch, ..Options::default() }).expect("pin forms build");
        let image = &built.image;
        assert_ne!(image.symbols.write, 0, "computed and wrong-role writes need the stub");
        assert_ne!(image.symbols.read, 0, "computed reads need the stub");
        assert_eq!(image.symbols.record_write != 0, watch == Level::Ports, "write recorder only at ports");
        assert_eq!(image.symbols.record_read != 0, watch == Level::Ports, "read recorder only at ports");
        let (mut cpu, _) = booted(image);
        cpu.set_pin('D', 2, true); cpu.set_pin('C', 0, false);
        cpu.events.clear();
        let mut constant_writes = Vec::new();
        while cpu.pc != image.symbols.pass {
            assert!(cpu.cycles < 60_000, "setup must finish");
            let (pc, at, executes) = (cpu.pc, cpu.cycles, cpu.will_execute());
            cpu.step().expect("pin instruction executes");
            if watch == Level::Faults && executes {
                if let Some(op) = built.map.ops.iter().find(|op| op.start == pc &&
                    op.site.is_some_and(|s| [site("write(led, high)"), site("write(led, low)")].contains(&(s as u16)))) {
                    assert_eq!(op.end - op.start, 1, "a constant write is exactly one instruction");
                    let insn = decode(&image.words[pc as usize..]).unwrap().0;
                    assert!(matches!(insn, Sbi(0x05, 5) | Cbi(0x05, 5)), "PORTB I/O address and bit: {insn:?}");
                    assert_eq!(cpu.cycles - at, 2, "the committed constant write takes two cycles");
                    constant_writes.push(insn);
                }
            }
        }
        if watch == Level::Faults { assert_eq!(constant_writes, [Sbi(0x05, 5), Cbi(0x05, 5)]); }
        let edges: Vec<_> = cpu.events.iter().filter_map(|e| match *e {
            Event::Pin { port, bit, high, .. } => Some((port, bit, high)), _ => None,
        }).collect();
        assert_eq!(edges, [('B', 5, true), ('B', 5, false), ('D', 4, true), ('D', 4, false),
            ('C', 1, true), ('C', 1, false), ('D', 4, true)], "only valid writes change pins, in source order");
        let actual_state = state(&cpu, image);
        for (name, want) in [("value", 0), ("sampled_hi", 1), ("sampled_lo", 0), ("sampled_dynamic", 1)] {
            let offset = m.vars().iter().find(|v| m.string(v.name) == name).unwrap().offset;
            assert_eq!(actual_state[offset as usize], want, "{watch:?}: {name}");
        }
        drain(&mut cpu, image);
        let frames = frames(&cpu);
        assert!(matches!(frames[0].0.event, W::Hello { proto: watch::PROTO, level, .. } if level == watch), "Hello names the build level");
        let events: Vec<_> = frames.into_iter().filter_map(|(f, _)| (!matches!(f.event, W::Hello { .. })).then_some(f.event)).collect();
        let actual_faults: Vec<_> = events.iter().copied().filter(|e| matches!(e, W::Fault { .. })).collect();
        assert_eq!(actual_faults, faults, "bad writes retain their arguments and source sites");
        if watch == Level::Faults {
            assert_eq!(events, faults, "faults emits no Write, Read or Duty frames");
        } else {
            let write = |text, pin, level| W::Write { pin, level, site: site(text) };
            let read = |text, pin, level| W::Read { pin, level, site: site(text) };
            let mut expected = vec![
                write("write(led, high)", 13, true), write("write(led, low)", 13, false),
                write("write(aux, value)", 4, true),
            ];
            // The two identical source calls have distinct sites.
            let second = c.sites.iter().enumerate().filter(|(_, s)|
                source.lines().nth(s.line as usize - 1).is_some_and(|l| l.trim() == "write(aux, value)"))
                .nth(1).expect("second computed level").0 as u16;
            expected.extend([W::Write { pin: 4, level: false, site: second },
                write("write(extra, high)", 15, true), write("write(extra, low)", 15, false),
                read("sampled_hi = read(sense)", 2, true), read("sampled_lo = read(quiet)", 14, false),
                read("sampled_dynamic = read(input_pin)", 2, true), write("write(chosen, high)", 4, true)]);
            expected.extend(faults.iter().copied());
            expected.push(W::Duty { pin: 9, level: 0, site: site("duty(motor, 0)") });
            assert_eq!(events, expected, "ports retains kind, payload, site and order");
        }
        runs.push((edges, actual_state, actual_faults));
    }
    assert_eq!(runs[0], runs[1], "watch level changes time, not pin sequence, state or faults");

    let source = "let led = output(PB5)\nlet sense = input(PD2, pull_up)\nvar v: i32 = 0\nfn setup() { write(led, high) v = read(sense) }";
    let c = compiled(source);
    let m = module::load(&c.module, &PROFILE).unwrap();
    for watch in [Level::Faults, Level::Ports] {
        let built = build_module(&m, &Options { watch, ..Options::default() }).unwrap();
        assert_eq!((built.image.symbols.write, built.image.symbols.read), (0, 0), "inline-only programs link no pin stubs");
    }
    let plain = firmware(source, 16e6, &[]).unwrap();
    let ports = fab_elec_mcu_emit_avr::firmware_with_watch(source, 16e6, &[], Level::Ports).unwrap();
    assert_ne!(plain.image_id, ports.image_id, "the watch level participates in the image identity");
}

#[test]
fn entry_cycle_bounds_cover_nested_loops_branches_frames_and_calls() {
    let mut source = String::from("\
let out = output(D13)
let input_pin = input(D2, pull_up)
let data = output(D6)
let clock = output(D7)
var out_pin: i32 = 13
var in_pin: i32 = 2
var choose = true
var visits: i32 = 0
var result: i32 = 1
");
    for (name, condition) in [("long", "choose"), ("short", "!choose")] {
        source.push_str(&format!("\
let {name} = after(ms(100), fn() {{
    let temp = [int(0); 16]
    for i in 0..40 {{
        for j in 0..40 {{
            visits = visits + 1
            if {condition} {{
                temp[j] = result
                result = result + temp[j] + i
            }} else {{ result = result + 1 }}
        }}
    }}
    write(out_pin, read(in_pin))
    shift_out(data, clock, result, 32)
}})\n"));
    }
    let c = compiled(&source);
    let m = module::load(&c.module, &PROFILE).unwrap();
    let built = build_module(&m, &Options { watch: watch::Level::Off, ..Options::default() }).unwrap();
    let image = &built.image;
    assert!(image.symbols.write != 0 && image.symbols.read != 0 && !image.symbols.shift_bitbang.is_empty(),
        "computed pin calls and shift_out must exercise the CALL-bound table");
    let visits = image.layout.state + 4 * m.vars().iter().find(|v| m.string(v.name) == "visits").unwrap().offset as u16;
    let (mut cpu, _) = booted(image);
    cpu.set_pin('D', 2, true);
    cpu.sreg = 0;
    cpu.write(0x6e, 0); // No Timer0 service in the measured entries.
    let mut measured = Vec::new();
    for name in ["long", "short"] {
        let k = m.entry(name).unwrap();
        let report = &built.entries[k];
        assert!(report.frame_bytes >= 64, "{name}: frame clearing must be present");
        store(&mut cpu, visits, 0, 4);
        let ret = scratch(&mut cpu, image) + 1;
        return_to(&mut cpu, ret); cpu.pc = built.map.entries[k].0;
        let start = cpu.cycles;
        to_symbol(&mut cpu, ret, report.max_cycles + 1);
        let clocks = cpu.cycles - start;
        assert!(clocks <= report.max_cycles, "{name}: measured {clocks} exceeds entry bound {}", report.max_cycles);
        assert_eq!(load(&cpu, visits, 4), 40 * 40, "{name}: both nested loops run their full source ranges");
        assert_eq!(cpu.sreg & 128, 0, "{name}: measurement excludes interrupt service");
        let slack = report.max_cycles - clocks;
        eprintln!("entry {name}: measured={clocks}, bound={}, slack={slack} ({:.2}%)",
            report.max_cycles, slack as f64 * 100.0 / clocks as f64);
        // The path-insensitive bound charges each loop's exit pass as a full body and
        // counts skipped jumps; that overcharge is per iteration, so it is checked per
        // inner iteration (14.5 cycles at R7 B3). A share of the measured time would
        // fail whenever the body gets faster while the bound stays as tight.
        if name == "long" {
            assert!(slack < 16 * 40 * 40, "long arm bound overcharges each inner iteration by under 16 cycles");
        }
        measured.push(clocks);
    }
    assert!(measured[1] < measured[0], "second entry must take the shorter arms");
}

#[test]
fn serial_depth_covers_a_long_handler_and_refuses_ram_and_service_overload() {
    // 115200 is exact here; the 16 MHz profile rightly refuses its baud error.
    const F_CLK: u32 = 18_432_000;
    let profile = chip::profile(F_CLK);
    let source = "\
var received = [int(0); 32]
var count: i32 = 0
var work: i32 = 0
let link = serial(115200, fn() {
    received[count] = byte(link)
    count = count + 1
})
let slow = after(ms(1), fn() {
    for i in 0..1024 { work = work + 1 }
})
";
    let c = compile(source, &profile, &chip::compiler_pins(), &[]).unwrap_or_else(|e| panic!("{e:?}"));
    let m = module::load(&c.module, &profile).unwrap();
    let slow = m.entry("slow").unwrap();
    let byte = m.serial()[0].setting.byte_cycles(10);
    let bytes: Vec<u8> = (0..32).map(|i| (i * 37 + 19) as u8).collect();
    for level in [watch::Level::Off, watch::Level::Faults] {
        let built = build_module(&m, &Options { f_clk: F_CLK, watch: level, ..Options::default() }).unwrap();
        let report = built.serial.expect("serial image reports its honest depth");
        let image = &built.image;
        assert_eq!(report.entry, slow, "the timer handler sets the queuing stretch");
        assert_eq!(u32::from(image.layout.rx_depth), report.depth, "RAM uses the reported depth");
        assert!(m.serial()[0].fifo < report.depth, "compiler op time must undercount this handler");
        assert_eq!(u64::from(report.depth), report.stretch_cycles.div_ceil(byte) + 2, "depth uses stretched wire time");
        eprintln!("serial long handler {level:?}: old={}, new={}, stretch={} cycles, entry={}, bound={}",
            m.serial()[0].fifo, report.depth, report.stretch_cycles, built.entries[slow].name, built.entries[slow].max_cycles);
        let addr = |name| image.layout.state
            + 4 * m.vars().iter().find(|v| m.string(v.name) == name).unwrap().offset as u16;
        let (mut cpu, _) = booted(image);
        to_symbol(&mut cpu, built.map.entries[slow].0, u64::from(F_CLK) / 100);
        let start = cpu.cycles;
        // Cpu::rx queues consecutive UART frames, each timed by UBRR/U2X.
        for &b in &bytes { cpu.rx(b); }
        let ret = built.map.returns.iter().find(|&&(_, k)| k == slow).unwrap().0;
        to_symbol(&mut cpu, ret, report.stretch_cycles);
        assert!(cpu.cycles - start >= 3 * u64::from(F_CLK) / 1000, "handler occupies several milliseconds");
        assert_eq!(load(&cpu, addr("count"), 4), 0, "no byte dispatch runs inside the timer handler");
        assert_eq!(load(&cpu, image.layout.rx_count, 2), bytes.len() as u64, "entire back-to-back burst queues during the handler");
        let deadline = cpu.cycles + report.stretch_cycles + bytes.len() as u64 * u64::from(image.bounds.rx_redispatch()) * 2;
        while load(&cpu, addr("count"), 4) < bytes.len() as u64 {
            assert!(cpu.cycles < deadline, "queued bytes must finish dispatching");
            cpu.step().expect("receiver continues executing");
        }
        let received: Vec<u8> = (0..32).map(|i| load(&cpu, addr("received") + 4 * i, 4) as u8).collect();
        assert_eq!(received, bytes, "every received byte is dispatched once, in wire order");
        assert_eq!(load(&cpu, image.layout.rx_count, 2), 0, "all queued bytes have been dispatched");
        assert_eq!(cpu.rx_lost, 0, "hardware UART must not overrun");
        assert_eq!(load(&cpu, image.layout.rx_lost, 4), 0, "RAM FIFO must not overflow");
        if level == watch::Level::Off {
            for code in [watch::FAULT_SERIAL_LOST, watch::FAULT_SERIAL_LOST_AT_LEAST] {
                assert_eq!(fault_cell(&cpu, image, code).0, 0, "no serial loss fault at Off");
            }
        } else {
            drain(&mut cpu, image);
            assert!(frames(&cpu).iter().all(|(f, _)| !matches!(f.event,
                watch::Event::Fault { code: watch::FAULT_SERIAL_LOST | watch::FAULT_SERIAL_LOST_AT_LEAST, .. })),
                "no serial loss fault at Faults");
        }
    }
    // Same program and link; only the timer's work grows. The compiler depth
    // still fits, so the refusal must come from the honest second layout.
    let source = source.replace("0..1024", "0..8000");
    let c = compile(&source, &profile, &chip::compiler_pins(), &[]).unwrap_or_else(|e| panic!("{e:?}"));
    let m = module::load(&c.module, &profile).unwrap();
    let refusal = build_module(&m, &Options { f_clk: F_CLK, ..Options::default() }).err().expect("honest FIFO exceeds RAM");
    for text in ["RX FIFO needs", "RAM has", "longest handler `slow`", "cycles", "µs", "115200 baud"] {
        assert!(refusal.message.contains(text), "RAM refusal must name {text}: {refusal}");
    }
    eprintln!("serial RAM refusal: {refusal}");
    let c = compiled("let link = serial(2000000, fn() {})\n");
    let m = module::load(&c.module, &PROFILE).unwrap();
    let refusal = build_module(&m, &Options::default()).err().expect("ISR load exceeds one at two megabaud");
    assert!(refusal.message.contains("2000000 baud") && refusal.message.contains("interrupt service load"),
        "service overload refusal names the link speed: {refusal}");
}

#[test]
fn serial_from_source() {
    let c = compiled(ECHO_SOURCE);
    let m = module::load(&c.module, &PROFILE).unwrap();
    let built = build_module(&m, &Options { watch: watch::Level::Ports, ..Options::default() }).unwrap();
    let (mut cpu, _) = booted(&built.image);
    for byte in RX_BYTES { cpu.rx(byte); }
    run(&mut cpu, 100_000);
    assert_eq!(cpu.rx_lost, 0, "source echo must keep up with the wire");
    drain(&mut cpu, &built.image);
    let received: Vec<_> = frames(&cpu).into_iter().filter_map(|(f, tick)| match f.event {
        watch::Event::Rx { byte } => Some((tick, byte)), _ => None,
    }).collect();
    assert_eq!(received.iter().map(|(_, b)| *b).collect::<Vec<_>>(), RX_BYTES);
    let mut device = Device::new(m, &PROFILE);
    let mut model = Model::default(); device.boot(&mut model);
    for (tick, byte) in received {
        device.run_until(tick * 500, &mut model);
        device.rx(tick * 500, 0, byte, &mut model);
    }
    device.run_until(u64::MAX, &mut model);
    assert_eq!(observed(&cpu), model.events, "source echo preserves values, order and sites");
    assert_eq!(state(&cpu, &built.image), device.state());
}

#[test]
fn blink_from_source() {
    let c = compiled(blink::BLINK);
    let m = module::load(&c.module, &PROFILE).unwrap();
    let built = build_module(&m, &Options { watch: watch::Level::Ports, ..Options::default() }).unwrap();
    let (mut cpu, _) = booted(&built.image);
    cpu.set_pin('D', 2, true);
    run(&mut cpu, 48_000_000);
    drain(&mut cpu, &built.image);
    let mut device = Device::new(m, &PROFILE);
    let mut model = Model { input_high: Some(true), ..Model::default() };
    device.boot(&mut model); device.run_until(3_000_000_000, &mut model);
    assert_eq!(observed(&cpu), model.events, "blink including stop and restart");
    let writes: Vec<_> = frames(&cpu).into_iter().filter_map(|(f, tick)| match f.event {
        watch::Event::Write { pin, level, site } => Some((tick, pin, level, site)), _ => None,
    }).collect();
    assert_eq!(writes.len(), 10, "setup plus nine toggles");
    let site = c.sites.iter().position(|s| s.line == 15).unwrap() as u16;
    for (k, &(_, pin, level, s)) in writes.iter().enumerate().skip(1) {
        assert_eq!((pin, level, s), (13, k % 2 == 1, site), "alternating source writes");
    }
    // Setup and the stop/restart gap have distinct timing by design.
    for group in [&writes[1..7], &writes[7..]] {
        for w in group.windows(2) { assert!((w[1].0 - w[0].0).abs_diff(500_000) <= 1, "250 ms grid; inline high/low paths can shift the record sample by one tick"); }
    }
}

/// The source map names the runtime's routines and each entry's code, and every call into a port
/// routine comes from the port instruction of the source line that made it.
#[test]
fn source_map_names_routines_entries_and_port_sites() {
    // Preserve source lines while reaching both dispatched entries in a few milliseconds.
    let source = blink::BLINK.replace("ms(250)", "ms(1)").replace("ms(2000)", "ms(2)");
    let c = compiled(&source);
    let m = module::load(&c.module, &PROFILE).unwrap();
    let built = build_module(&m, &Options { watch: watch::Level::Ports, marks: c.marks.clone(), ..Options::default() }).unwrap();
    let (map, s) = (&built.map, &built.image.symbols);
    assert_eq!(map.find(s.pass), Where::Runtime("pass"), "the dispatcher's pass is named");
    assert_eq!(map.find(s.record_write + 1), Where::Runtime("record_write"), "inside the inline write recorder is named");
    let setup = m.entry(fab_elec_mcu_rt::SETUP).unwrap();
    assert!(matches!(map.find(s.setup), Where::Entry { entry, .. } if entry == setup), "setup's first word is setup's");
    for (k, &(start, end)) in map.entries.iter().enumerate() {
        assert!(start < end, "entry {k} has code");
        assert!(matches!(map.find(end - 1), Where::Entry { entry, .. } if entry == k), "entry {k}'s last word (its RET) is its own");
    }
    assert_eq!(map.find(built.image.words.len() as u32 + 8), Where::Outside, "past the image is outside");
    assert!(map.stmt_starts.windows(2).all(|w| w[0].0 <= w[1].0), "statement starts ascend in word addresses");
    assert_eq!(map.returns.len(), m.entries().len(), "each entry, including setup, has one call return address");
    assert!(map.returns.contains(&(s.setup_call + 2, setup)), "setup returns to the word after reset's CALL");
    for (dispatch, &call) in s.dispatch_call.iter().enumerate() {
        let entry = dispatch + usize::from(setup <= dispatch);
        assert!(map.returns.contains(&(call + 2, entry)), "dispatcher {dispatch} returns after its two-word CALL");
    }
    for entry in 0..m.entries().len() {
        assert_eq!(map.returns.iter().filter(|&&(_, k)| k == entry).count(), 1,
            "entry {entry} has exactly one return address");
    }
    for op in &map.ops {
        let entry = &m.entries()[op.entry];
        let expected = c.marks.iter().rev().find(|&&(at, _)| at >= entry.body + 1 && at <= op.code).map(|&(_, id)| id);
        assert_eq!(op.mark, expected, "op {} uses the nearest preceding mark in its own entry", op.code);
        assert!(matches!(map.find(op.start), Where::Entry { mark, .. } if mark == expected), "find exposes op {}'s statement", op.code);
    }

    let line = |site: u32| c.sites[site as usize].line;
    let mut calls = std::collections::BTreeSet::new();
    let mut ran = vec![false; m.entries().len()];
    let mut returned = vec![false; m.entries().len()];
    let mut starts_seen = std::collections::BTreeSet::new();
    let mut cpu = Cpu::new(&built.image.words);
    cpu.set_pin('D', 2, true);
    while (calls.len() < 3 || returned.iter().any(|&done| !done)) && cpu.cycles < 160_000 {
        let prev = cpu.pc;
        if let Where::Entry { entry, .. } = map.find(prev) { ran[entry] = true; }
        let first = map.stmt_starts.partition_point(|&(pc, _)| pc < prev);
        for &(_, mark) in map.stmt_starts[first..].iter().take_while(|&&(pc, _)| pc == prev) {
            assert!(matches!(map.find(prev), Where::Entry { entry, .. } if c.stmts[mark as usize].owner == fab_elec_mcu::Owner::Handler(entry as u32)),
                "statement {mark} begins in its owning entry while stepping");
            starts_seen.insert(mark);
        }
        for &(pc, entry) in &map.returns {
            if prev == pc {
                assert!(ran[entry], "entry {entry}'s code runs before its return address is reached");
                assert!(matches!(fab_elec_mcu_avr::insn::decode(&built.image.words[pc as usize - 2..]),
                    Some((fab_elec_mcu_avr::insn::Insn::Call(target), _)) if target == map.entries[entry].0),
                    "entry {entry}'s return address follows a CALL to its code");
                returned[entry] = true;
            }
        }
        cpu.step().expect("blink runs");
        for (routine, name) in [(s.record_write, "write"), (s.record_read, "read")] {
            if cpu.pc == routine {
                match map.find(prev) {
                    Where::Entry { site: Some(site), mark: Some(mark), .. } => {
                        assert_eq!(c.stmts[mark as usize].site.line, line(site), "{name}'s statement and port site name the same source line");
                        calls.insert((name, line(site)));
                    }
                    other => panic!("the call into {name} from word {prev} maps to {other:?}, not a port site"),
                }
            }
        }
    }
    assert!(returned.iter().all(|&done| done), "setup and every dispatched entry reach their own return addresses");
    assert!(!starts_seen.is_empty(), "stepping reaches statement starts");
    assert_eq!(calls.into_iter().collect::<Vec<_>>(), [("read", 17), ("write", 9), ("write", 15)],
        "each port routine is entered from exactly the source lines that call it");
}

#[test]
fn signed_power_of_two_remainders_match_module() {
    let c = compiled("var x: i32 = 0\nvar one: i32 = 0\nvar small: i32 = 0\nvar wide: i32 = 0\nvar large: i32 = 0\nlet inputs = every(ms(1000), fn() { x = x + 1 })\nfn setup() {\n one = x % 1\n small = x % 8\n wide = x % 512\n large = x % 1073741824\n}\n");
    let m = module::load(&c.module, &PROFILE).expect("remainder source verifies");
    let built = build_module(&m, &Options::default()).expect("constant power-of-two remainders emit");
    assert!(built.map.stmt_starts.is_empty() && built.map.ops.iter().all(|op| op.mark.is_none()),
        "default emitter options omit statement metadata");
    let (mut cpu, _) = booted(&built.image);
    let setup = m.entry(fab_elec_mcu_rt::SETUP).expect("remainder setup");
    let mut scratch = vec![0; m.scratch_words(setup)];
    let slot = m.vars().iter().find(|v| m.string(v.name) == "x").expect("dividend state").offset as usize;
    for x in [i32::MIN, i32::MIN + 1, -513, -512, -511, -9, -8, -7, -1, 0, 1, 7, 8, 9, 511, 512, 513, i32::MAX] {
        let mut initial = m.state_init().to_vec();
        initial[slot] = x as u32;
        compare_entry(&mut cpu, &built.image, &m, setup, &initial, &mut scratch);
    }
}

#[test]
fn ledavr_fixture_has_statement_and_pc_tables() {
    let source = include_str!("../../../fixtures/ledavr.fw.splash");
    let c = compile(source, &PROFILE, &chip::compiler_pins(), &[]).unwrap_or_else(|e| panic!("ledavr firmware diagnostics: {e:?}"));
    let m = module::load(&c.module, &PROFILE).expect("ledavr module verifies for the 328P");
    let options = Options { marks: c.marks.clone(), sites: c.sites.iter().map(|s| (s.line, s.col)).collect(), ..Options::default() };
    let built = build_module(&m, &options).unwrap_or_else(|e| panic!("ledavr AVR diagnostics: {e}"));
    assert_eq!(c.pwm.len(), 6, "the unchanged demo configures all six PWM LEDs");
    assert_eq!(built.entries.len(), 4, "the demo emits setup and its three handlers");
    // Statement lines in each handler, including statements nested in conditions.
    // Inlined helpers add their own rows; a caller statement and its helper's first statement
    // both start at the same PC.
    for (name, lines) in [
        ("setup", &[56, 57][..]),
        ("chase", &[61, 62, 63, 64][..]),
        ("breathe", &[69, 70, 71, 72, 73, 74, 75, 76][..]),
        ("poll", &[81, 82, 83, 84, 85, 87][..]),
    ] {
        let entry = m.entry(name).expect("demo handler") as u32;
        for &line in lines {
            let statements: Vec<_> = c.stmts.iter().enumerate().filter(|(_, s)| s.owner == fab_elec_mcu::Owner::Handler(entry) && s.site.line == line).collect();
            assert!(!statements.is_empty(), "{name}: source statement on line {line} has a table entry");
            for (id, _) in statements {
                for &(at, _) in c.marks.iter().filter(|&&(_, mark)| mark == id as u32) {
                    let next = built.map.ops.iter().find(|op| op.entry == entry as usize && op.code >= at);
                    if let Some(op) = next {
                        assert!(built.map.stmt_starts.contains(&(op.start, id as u32)),
                            "{name}: retained statement {id} on line {line} starts at its first emitted op");
                    } else {
                        assert!(!built.map.stmt_starts.iter().any(|&(_, mark)| mark == id as u32),
                            "{name}: a terminal mark never borrows another entry's PC");
                    }
                }
            }
        }
        assert!(built.map.stmt_starts.iter().any(|&(_, id)| c.stmts[id as usize].owner == fab_elec_mcu::Owner::Handler(entry)),
            "{name}: the demo's handler has executable statement starts");
    }
    for &(pc, id) in &built.map.stmt_starts {
        assert!(matches!(built.map.find(pc), Where::Entry { entry, .. } if entry == match c.stmts[id as usize].owner { fab_elec_mcu::Owner::Handler(k) => k as usize, _ => panic!("inlined owner") }),
            "demo statement {id}'s word address belongs to its entry");
    }
}

/// The image's source conditions: each `if` condition the demo branches on (a helper's once per
/// inlined call) tests the bool byte or the compare's flags at its address, skipping the following
/// RJMP/JMP's word length to pass the jump to the `else`. Taken still means true. A condition that only
/// selects a value (`if on { low } else { high }`) takes no branch and has none.
#[test]
fn ledavr_conditions_are_the_branches_that_test_them() {
    use fab_elec_mcu_avr::insn::{decode, Insn};
    let source = include_str!("../../../fixtures/ledavr.fw.splash");
    let fw = firmware(source, 16e6, &[]).expect("ledavr builds");
    let debug = &fw.debug;
    let text = |span: (u32, u32)| &source[span.0 as usize..span.1 as usize];
    let mut spans: Vec<(u32, u32)> = debug.conds.iter().map(|c| c.span).collect();
    spans.sort_unstable();
    spans.dedup();
    let mut expected: Vec<String> = (0..8).map(|n| format!("n == {n}")).collect();
    expected.extend(["mode == 0", "mode == 1", "pressed && !was_pressed", "mode == 0"].map(String::from));
    assert_eq!(spans.iter().map(|&s| text(s)).collect::<Vec<_>>(), expected, "every branching condition, in source order");
    assert!(debug.conds.windows(2).all(|w| w[0].pc < w[1].pc), "one condition per branch, by address");
    for c in &debug.conds {
        let at = c.pc as usize;
        let (jump, words) = decode(&fw.words[at + 1..]).expect("condition's jump");
        assert!(matches!(jump, Insn::Rjmp(_) | Insn::Jmp(_)), "{}: jump to its else", text(c.span));
        // Equalities branch on Z set directly; the compound bool still tests its stored byte.
        let branch = if text(c.span).contains("==") { Insn::Brbs(1, words as i8) }
            else { Insn::Brbc(1, words as i8) };
        assert_eq!(decode(&fw.words[at..]).map(|d| d.0), Some(branch),
            "{} at 0x{:x}: the true sense skips the jump to its else", text(c.span), c.pc);
        assert!(c.taken_true, "{}: an `if` tests its condition itself", text(c.span));
        assert_eq!(debug.cond_at(c.pc).map(|k| &debug.conds[k as usize]), Some(c), "the condition is found by its branch's address");
    }
    let breathe = debug.handlers.iter().position(|h| h.name == "breathe").expect("breathe") as u32;
    let mode: Vec<_> = debug.conds.iter().filter(|c| text(c.span) == "mode == 1").collect();
    assert_eq!(mode.len(), 1, "breathe's `mode == 1` has one copy");
    assert_eq!((mode[0].owner, mode[0].line), (fab_elec_mcu_emu::Owner::Handler(breathe), source.lines().position(|l| l.contains("if mode == 1")).unwrap() as u32 + 1), "in breathe, on its line");
}

/// The chip entry E1 builds a bench's firmware with: the bench constant reaches the image as a
/// value, the pin table names the pins and the machine's rows, and every refusal is a diagnostic
/// with its span in the document where it has one.
#[test]
fn ledavr_builds_through_the_chip_entry_with_spanned_diagnostics() {
    let source = include_str!("../../../fixtures/ledavr.fw.splash");
    let fast = firmware(source, 16e6, &[("chase_ms", 20.0)]).unwrap_or_else(|d| panic!("ledavr builds: {d:?}"));
    let again = firmware(source, 16e6, &[("chase_ms", 20.0)]).expect("ledavr builds again");
    let plain = firmware(source, 16e6, &[]).expect("ledavr builds with its own constant");
    assert_eq!(fast, again, "a build is a function of the source, the constants and the clock");
    assert!(fast.words != plain.words && fast.image_id != plain.image_id, "the bench's chase_ms reaches the image");
    assert_eq!(fast.f_clk, 16_000_000);
    let debug = &fast.debug;
    let map = debug.map.as_ref().expect("the demo has a step map");
    assert_eq!(map.check(), Ok(()), "the step map holds together");
    assert_eq!(debug.image_id, fast.image_id, "the table names its image");
    assert_eq!(debug.handlers.iter().map(|h| h.name.as_str()).collect::<Vec<_>>(), ["setup", "chase", "breathe", "poll"]);
    assert!(debug.handlers.iter().all(|h| h.span.0 < h.span.1), "each handler has its span: {:?}", debug.handlers);
    assert_eq!(debug.starts.iter().map(|s| s.0).collect::<Vec<_>>(), map.statements, "the statement starts are the step map's");
    for (at, ids) in &debug.starts {
        assert!(ids.iter().all(|&id| debug.stmts[id as usize].addr == Some(*at)), "statement starts at {at} name statements first emitted there");
    }
    let chase_line = source.lines().position(|l| l.starts_with("let chase = every")).expect("the chaser") as u32 + 2;
    let first = debug.stmts.iter().find(|s| s.line == chase_line).expect("the chaser's first statement");
    assert!(first.addr.is_some() && source[first.span.0 as usize..first.span.1 as usize].lines().count() >= 1,
        "the chaser's first statement has an address and its source");
    assert_eq!(debug.vars.iter().map(|v| (v.name.as_str(), v.ty)).collect::<Vec<_>>(),
        [("mode", FwTy::I32), ("lit", FwTy::I32), ("phase", FwTy::I32), ("was_pressed", FwTy::Bool)], "the program's variables and types");
    let (first, name) = debug.runtime[0];
    assert_eq!((debug.routine(first), debug.routine(debug.runtime_end)), (Some(name), None), "the runtime ends where the handlers begin");
    // the machine runs the image, and its pins are the table's
    let mut m = AvrMachine::new(&fast.words, 16_000_000);
    assert_eq!(m.pins(), &NAMES[..], "the machine's pin rows are the table's");
    assert_eq!((chip::by_name("PD6").map(|(row, p)| (row, p.number))), Some((18, 6)), "PD6: machine row 18, runtime pin 6");
    let marks = map.marks();
    let ran = m.run(&fab_elec_mcu_emu::Until { tick: 200_000, marks: &marks, stops: &map.statements, stops_after: 0, on_drive: false });
    assert_eq!(ran, fab_elec_mcu_emu::Ran::Pc, "the image reaches a statement");
    assert!(!debug.starting_at(m.pc()).is_empty(), "it stops on a statement start");
    // the variables read back from RAM through the layout: setup has not run, so they hold their initial values
    let mut bytes = [0u8; 4];
    let lit = debug.vars.iter().find(|v| v.name == "lit").expect("lit");
    assert!(m.read_data(lit.addr, &mut bytes), "lit's bytes are readable");
    assert_eq!(lit.decode(&bytes), fab_elec_mcu_emu::FwValue::I32(0), "lit holds its initial value");

    // refusals, each with where it is
    let at = |text: &str| source.find(text).map(|k| k..k + text.len());
    let refused = |source: &str, f_clk: f64, consts: &[(&str, f64)]| firmware(source, f_clk, consts).err().expect("refused");
    let d = refused(source, 16e6, &[("chase_ms", 2.5)]);
    assert_eq!(d.len(), 1, "{d:?}");
    assert_eq!((d[0].param.as_deref(), d[0].span.clone()), (Some("chase_ms"), at("let chase_ms = 125")), "a value its let cannot hold: {d:?}");
    let d = refused(source, 16e6, &[("speed", 1.0)]);
    assert_eq!((d[0].param.as_deref(), d[0].span.clone()), (Some("speed"), None), "a constant the document does not declare: {d:?}");
    for form in ["every", "after"] {
        for (period, nearest) in [(7, "the nearest periods are 6 µs and 8 µs"), (1, "the nearest period is 2 µs")] {
            let fractional = format!("let tick = {form}(us({period}), fn() {{}})\n");
            let d = refused(&fractional, 20e6, &[]);
            assert_eq!(d.len(), 1, "one fractional-period refusal for {form} {period} µs: {d:?}");
            assert!(d[0].span.is_some() && d[0].message.contains(&format!(
                "{form} {period} µs is not a whole number of Timer0 ticks at 20000000 Hz (one tick per 8 CPU cycles); {nearest}")),
                "fractional period names its clock and positive neighbours at its source: {d:?}");
        }
    }
    let whole = firmware("let tick = every(us(8), fn() {})\n", 20e6, &[]).expect("a whole-tick period builds at 20 MHz");
    assert_eq!(whole.f_clk, 20_000_000, "the image keeps the crystal's clock");
    let typo = source.replacen("pwm(PD6,", "pwm(PD9,", 1);
    let d = refused(&typo, 16e6, &[]);
    let k = typo.find("PD9").expect("the typo");
    assert!(d.iter().any(|d| d.span.as_ref().is_some_and(|s| s.start <= k && k < s.end.max(k + 1))), "a name the chip lacks, at its use: {d:?}");
    let float = "let out = output(PB5)\nvar x: f32 = 0.5\nfn setup() { write(out, x) }";
    let d = refused(float, 16e6, &[]);
    let s = d[0].span.clone().expect("an emitter refusal has a span");
    assert!(float[s.clone()].contains("write(out, x)"), "an emitter refusal is at its statement: {:?} {d:?}", &float[s]);
}
/// ledavr's board sinks each LED into its pin (std's LED driver puts the cathode on the pin, and the
/// bench reads an LED as lit while its cathode is low), so the firmware lights an LED with a low
/// pin and dims it with a higher duty, in both modes:
/// - the chaser: in each of three 20 ms steps exactly one of the eight LED pins sinks, the other
///   seven stay high, and the sinking one walks on by one LED per step;
/// - breathe, after a press of the button: its ramp starts LED0 dark (its pin mostly high) and
///   LED3, half a ramp on, near full (its pin mostly low).
#[test]
fn ledavr_lights_its_leds_by_sinking_them_in_both_modes() {
    use fab_elec_mcu_emu::{Drive, PcSet, Ran, Until};
    const MS: u64 = 16_000;
    const SAMPLES: u64 = 200;
    fn run_to(m: &mut AvrMachine, tick: u64) {
        loop {
            match m.run(&Until { tick, marks: &PcSet::default(), stops: &[], stops_after: 0, on_drive: false }) {
                Ran::Tick => return,
                Ran::Barrier => m.mark(),
                other => panic!("the image runs on to tick {tick}, not {other:?}"),
            }
        }
    }
    // How many of SAMPLES even samples over 10 ms from `from` find each LED pin low.
    let sample = |m: &mut AvrMachine, leds: &[usize; 8], from: u64| {
        let mut low = [0u64; 8];
        for k in 0..SAMPLES {
            run_to(m, from + k * 10 * MS / SAMPLES);
            for (n, &pin) in leds.iter().enumerate() {
                low[n] += u64::from(m.drive(pin) == Drive::Low);
            }
        }
        low
    };
    let source = include_str!("../../../fixtures/ledavr.fw.splash");
    let fw = firmware(source, 16e6, &[("chase_ms", 20.0)]).unwrap_or_else(|d| panic!("ledavr builds: {d:?}"));
    let mut m = AvrMachine::new(&fw.words, 16_000_000);
    let row = |name: &str| chip::by_name(name).unwrap_or_else(|| panic!("{name}: a 328P pin")).0;
    // the button is released: its pull-up holds PD2 high
    m.set_input(row("PD2"), true);
    let leds = ["PD6", "PD5", "PB1", "PB2", "PB3", "PD3", "PD4", "PD7"].map(row);
    let mut lit = Vec::new();
    for step in 0..3u64 {
        // the middle 10 ms of the step, clear of the chaser's instants
        let low = sample(&mut m, &leds, (20 * step + 5) * MS);
        let sinking: Vec<usize> = (0..8).filter(|&n| low[n] * 8 > SAMPLES * 7).collect();
        assert!(sinking.len() == 1 && (0..8).all(|n| sinking.contains(&n) || low[n] * 8 < SAMPLES),
            "chaser step {step}: one LED pin sinks and the seven others stay high (samples low of {SAMPLES}, LED0..LED7: {low:?})");
        lit.push(sinking[0]);
    }
    assert!(lit.windows(2).all(|w| w[1] == (w[0] + 1) % 8), "the lit LED walks on by one per step: {lit:?}");

    // a 30 ms press: the 10 ms poll sees it and switches to breathe, whose 8 ms ramp then climbs
    m.set_input(row("PD2"), false);
    run_to(&mut m, 85 * MS);
    m.set_input(row("PD2"), true);
    let low = sample(&mut m, &leds, 100 * MS);
    assert!(low[0] * 4 < SAMPLES && low[3] * 4 > SAMPLES * 3,
        "breathe starts LED0 dark and LED3 near full (samples low of {SAMPLES}, LED0..LED7: {low:?})");
}

#[test]
fn refusals() {
    for (source, op) in [
        ("var x: f32 = 1.0\nfn setup() { x = x + 1.0 }", "f32"),
        ("let out = output(D13)\nvar x: f32 = 0.5\nfn setup() { write(out, x) }", "reaches `pin_write` at line 3:"),
        ("var x: f32 = 1.0\nvar y: f32 = 2.0\nfn setup() { x = x * y + x }", "f32"),
        ("let button = input(D2, pull_down)\nfn setup() { read(button) }", "pull-down"),
        ("let network = eth(1, fn() {})\nfn setup() {}", "eth declaration `network`: Ethernet is unavailable on the 328P"),
    ] {
        let c = compiled(source);
        let m = module::load(&c.module, &PROFILE).unwrap();
        let o = Options { sites: c.sites.iter().map(|s| (s.line, s.col)).collect(), ..Options::default() };
        let error = build_module(&m, &o).err().expect("must refuse");
        let message = error.message;
        assert!(message.contains("entry `setup`") && message.contains("code ") && message.contains(op), "{message}");
    }
    let with_touch = fab_elec_mcu_rt::Profile { touch: true, ..PROFILE };
    let c = compile("let finger = touch(10000, fn() {})", &with_touch, &[], &[])
        .expect("touch fixture verifies under a touch-capable profile");
    assert_eq!(module::load(&c.module, &PROFILE).unwrap_err().to_string(),
        "touch declaration needs a board with touch input", "AVR profile refuses touch at load");
    let touch = module::load(&c.module, &with_touch).unwrap();
    let error = build_module(&touch, &Options::default()).err().expect("AVR also refuses a foreign touch-capable module");
    assert!(error.message.contains("touch input is unavailable on the 328P"), "AVR touch emission refusal: {error}");

    let with_ram = fab_elec_mcu_rt::Profile { buffer_bytes: 8, ..PROFILE };
    let c = compile("let tile = surface(2, 2)\nfn setup() {}", &with_ram, &[], &[])
        .expect("writable buffer fixture compiles under its explicit RAM budget");
    let writable = module::load(&c.module, &with_ram).expect("writable fixture verifies");
    let error = build_module(&writable, &Options::default()).err().expect("AVR refuses even unused writable buffers");
    assert!(error.message.contains("buffer declaration `tile`") && error.message.contains("unavailable on the 328P"),
        "AVR names its unsupported buffer declaration: {error}");

    // Carry a tiny image directly in the module; no decoder dependency is
    // needed to prove the AVR gate for a read-only buffer with zero RAM cost.
    use fab_elec_mcu_rt::{format::*, ports};
    let mut sections: Vec<_> = c.module[HEADER_WORDS..HEADER_WORDS + c.module[H_SECTIONS] as usize * DIRECTORY_WORDS]
        .chunks_exact(DIRECTORY_WORDS).map(|r| (r[0], c.module[r[1] as usize..(r[1] + r[2]) as usize].to_vec())).collect();
    let decls = &mut sections.iter_mut().find(|(kind, _)| *kind == SEC_DECLS).expect("fixture has declarations").1;
    decls[0] = ports::FORM_IMAGE; decls[3] = 3;
    drop(decls.splice(DECL_HEADER_WORDS..DECL_HEADER_WORDS, [ARG_INT, 0]));
    sections.iter_mut().find(|(kind, _)| *kind == SEC_BUFS).expect("fixture has a buffer row").1[3] = 0;
    sections.push((SEC_IMAGES, vec![1, 2, 2, 0xf800_07e0, 0x001f_ffff]));
    let mut words = vec![0; HEADER_WORDS + sections.len() * DIRECTORY_WORDS];
    words[..HEADER_WORDS].copy_from_slice(&c.module[..HEADER_WORDS]);
    words[H_SECTIONS] = sections.len() as u32;
    for (k, (kind, section)) in sections.into_iter().enumerate() {
        let at = HEADER_WORDS + k * DIRECTORY_WORDS;
        let offset = words.len() as u32;
        words[at..at + DIRECTORY_WORDS].copy_from_slice(&[kind, offset, section.len() as u32]);
        words.extend(section);
    }
    words[H_WORDS] = words.len() as u32;
    let image = module::load(&words, &PROFILE).expect("read-only image verifies against AVR's zero buffer RAM budget");
    let error = build_module(&image, &Options::default()).err().expect("AVR refuses read-only images before emitting any code");
    assert!(error.message.contains("buffer declaration `tile`") && error.message.contains("read-only images")
        && error.message.contains("unavailable on the 328P"), "AVR names its image-buffer refusal: {error}");
}

// Format-level fixtures cover verified instructions the language folds away.
fn fixture(body: &[u32], values: &[u8], vars: &[u8], frame: u32,
    state: &[u32], shared: &[u32]) -> Vec<u32> {
    fixture_inputs(body, values, vars, frame, state, shared, &[])
}

// Input words have an independent wrapping increment entry. Repeated legal runs
// can reach every i32, so the existing host-injected grids respect state ranges.
fn fixture_inputs(body: &[u32], values: &[u8], vars: &[u8], frame: u32,
    state: &[u32], shared: &[u32], inputs: &[u32]) -> Vec<u32> {
    fixture_flags(body, values, vars, frame, state, shared, inputs, &[])
}

// Flag words, which the body reads as bools, toggle in the same entry instead:
// an increment would leave values a bool load can never produce.
fn fixture_flags(body: &[u32], values: &[u8], vars: &[u8], frame: u32,
    state: &[u32], shared: &[u32], inputs: &[u32], flags: &[u32]) -> Vec<u32> {
    use fab_elec_mcu_rt::{format::*, ports};
    let mut code = vec![values.len() as u32, vars.len() as u32, frame, 0];
    for types in [values, vars] {
        for bytes in types.chunks(4) {
            let mut word = [0; 4]; word[..bytes.len()].copy_from_slice(bytes);
            code.push(u32::from_le_bytes(word));
        }
    }
    code.push(body.len() as u32); code.extend_from_slice(body);
    let mut entries = vec![0, 0, code.len() as u32, PROFILE.max_entry_ops as u32, 0];
    let mut strings = vec![5, u32::from_le_bytes(*b"setu"), b'p' as u32];
    if !inputs.is_empty() || !flags.is_empty() {
        let start = code.len() as u32;
        let count = 1 + 2 * (inputs.len() + flags.len());
        code.extend([count as u32, 0, 0, 0]);
        for bytes in vec![TY_I32; count].chunks(4) {
            let mut word = [0; 4]; word[..bytes.len()].copy_from_slice(bytes);
            code.push(u32::from_le_bytes(word));
        }
        let mut update = vec![OP_CONST, 0, 1];
        let updates = inputs.iter().map(|&w| (w, BIN_ADD_I)).chain(flags.iter().map(|&w| (w, BIN_XOR_I)));
        for (n, (word, op)) in updates.enumerate() {
            let v = 1 + 2 * n as u32;
            update.extend([OP_LOAD | REGION_STATE << SUB_SHIFT, v, word, 1, NONE,
                OP_BIN | op << SUB_SHIFT, v + 1, v, 0,
                OP_STORE | REGION_STATE << SUB_SHIFT, word, 1, NONE, v + 1]);
        }
        code.push(update.len() as u32); code.extend(update);
        entries.extend([1, start, code.len() as u32 - start, PROFILE.max_entry_ops as u32, 0]);
        strings.extend([6, u32::from_le_bytes(*b"inpu"), u32::from_le_bytes(*b"ts\0\0")]);
    }
    let count = if inputs.is_empty() && flags.is_empty() { 1 } else { 2 };
    let sections = [
        vec![state.len() as u32, shared.len() as u32, 0, count, count, 0, 0, 0],
        state.to_vec(), shared.to_vec(), strings, vec![], vec![], entries, code,
    ];
    let mut words = vec![0; HEADER_WORDS + SECTION_COUNT * DIRECTORY_WORDS];
    words[..HEADER_WORDS].copy_from_slice(&[MAGIC, FORMAT_VERSION, ports::PORT_TABLE_VERSION,
        PROFILE.id, 0, SECTION_COUNT as u32, 0, 0]);
    for (k, section) in sections.into_iter().enumerate() {
        let at = HEADER_WORDS + k * DIRECTORY_WORDS;
        let start = words.len() as u32;
        words[at..at + 3].copy_from_slice(&[k as u32 + 1, start, section.len() as u32]);
        words.extend(section);
    }
    words[H_WORDS] = words.len() as u32;
    words
}

fn loop_body(body: &mut Vec<u32>, cap: u32, inner: &[u32]) {
    body.extend([fab_elec_mcu_rt::format::OP_LOOP, cap, inner.len() as u32]);
    body.extend_from_slice(inner);
}

fn if_body(body: &mut Vec<u32>, condition: u32, yes: &[u32], no: &[u32]) {
    body.extend([fab_elec_mcu_rt::format::OP_IF, condition, yes.len() as u32]);
    body.extend_from_slice(yes); body.push(no.len() as u32); body.extend_from_slice(no);
}

#[test]
fn acc_consumers_match_exec_and_bounds() {
    use fab_elec_mcu_avr::insn::{decode, Insn::*};
    use fab_elec_mcu_rt::format::*;
    let ld = OP_LOAD | REGION_STATE << SUB_SHIFT;
    let st = OP_STORE | REGION_STATE << SUB_SHIFT;
    let save = [st, 8, 1, NONE, 11];
    // Values 4..7 are Var homes, 8/9 are constants, 10 is the immediately preceding
    // result. Keep this AIR order even when source lowering would fold or share an op.
    let mut cases = Vec::new();
    for flag in [false, true] {
        let other = if flag { 7 } else { 5 };
        let bin: &[u32] = if flag { &[BIN_AND_B, BIN_OR_B] } else {
            &[BIN_ADD_I, BIN_SUB_I, BIN_AND_I, BIN_OR_I, BIN_XOR_I, BIN_MUL_I,
                BIN_DIV_I, BIN_REM_I, BIN_SHL_I, BIN_SHR_I, BIN_SHR_UI]
        };
        for &op in bin { for right in [false, true] {
            let (a, b) = if right { (other, 10) } else { (10, other) };
            cases.push((format!("bin {op}, bool={flag}, right={right}"), flag, flag,
                [vec![OP_BIN | op << SUB_SHIFT, 11, a, b], save.to_vec()].concat(), true));
        } }
        for sense in [CMP_EQ, CMP_NE, CMP_LT, CMP_LE, CMP_GT, CMP_GE] { for right in [false, true] {
            let (a, b) = if right { (other, 10) } else { (10, other) };
            cases.push((format!("cmp {sense}, bool={flag}, right={right}"), flag, true,
                [vec![OP_CMP_I | sense << SUB_SHIFT, 11, a, b], save.to_vec()].concat(), true));
        } }
        for position in [2, 3] {
            let mut op = vec![OP_SEL, 11, 6, other, other]; op[position + 1] = 10;
            cases.push((format!("sel {position}, bool={flag}"), flag, flag, [op, save.to_vec()].concat(), true));
        }
        for (name, tail) in [
            ("set", vec![OP_SET, if flag { 2 } else { 0 }, 10, OP_GET, 11, if flag { 2 } else { 0 }]),
            ("store", vec![st, 8, 1, NONE, 10, ld, 11, 8, 1, NONE]),
            ("constant index store", vec![st, 4, 4, 8, 10, ld, 11, 4, 4, 8]),
            ("un", vec![OP_UN | (if flag { UN_NOT_B } else { UN_NEG_I }) << SUB_SHIFT, 11, 10]),
        ] {
            cases.push((format!("{name}, bool={flag}"), flag, flag, [tail, save.to_vec()].concat(), true));
        }
    }
    for (name, op) in [
        ("load index", vec![ld, 11, 4, 4, 10]),
        ("store index", vec![st, 4, 4, 10, 5, ld, 11, 4, 4, 8]),
        ("wrap inline", vec![OP_WRAP, 11, 10, 4]),
        ("wrap helper", vec![OP_WRAP, 11, 10, 7]),
        ("constant shift", vec![OP_BIN | BIN_SHL_I << SUB_SHIFT, 11, 10, 8]),
        ("constant remainder", vec![OP_BIN | BIN_REM_I << SUB_SHIFT, 11, 10, 8]),
        ("fma A", vec![OP_FMA | FMA_MUL_ADD_I << SUB_SHIFT, 11, 10, 5, 4]),
        ("fma B", vec![OP_FMA | FMA_MUL_ADD_I << SUB_SHIFT, 11, 4, 10, 5]),
    ] {
        cases.push((name.to_owned(), false, false, [op, save.to_vec()].concat(), true));
    }
    for sense in [CMP_LT, CMP_LE, CMP_GT, CMP_GE, CMP_EQ, CMP_NE] { for right in [false, true] {
        let (a, b) = if right { (12, 10) } else { (10, 12) };
        cases.push((format!("cmp MAX {sense}, right={right}"), false, true,
            [vec![OP_CONST, 12, i32::MAX as u32, OP_CMP_I | sense << SUB_SHIFT, 11, a, b], save.to_vec()].concat(), true));
    } }
    cases.push(("sel condition".to_owned(), true, false, [vec![OP_SEL, 11, 10, 4, 5], save.to_vec()].concat(), true));
    cases.push(("sel constant arms".to_owned(), true, false,
        [vec![OP_CONST, 12, 256, OP_SEL, 11, 10, 8, 12], save.to_vec()].concat(), true));
    let mut branch = Vec::new();
    if_body(&mut branch, 10, &[st, 8, 1, NONE, 4], &[st, 8, 1, NONE, 5]);
    cases.push(("if bool".to_owned(), true, false, branch, true));
    let mut barrier = Vec::new();
    if_body(&mut barrier, 6, &[st, 9, 1, NONE, 5], &[]);
    barrier.extend([st, 8, 1, NONE, 10]);
    cases.push(("across if".to_owned(), false, false, barrier, false));
    for (name, tail) in [
        ("two uses", vec![OP_BIN | BIN_ADD_I << SUB_SHIFT, 11, 10, 10, st, 8, 1, NONE, 11]),
        ("intervening op", vec![st, 9, 1, NONE, 5, st, 8, 1, NONE, 10]),
        ("store value with variable index", vec![st, 4, 4, 4, 10]),
        ("fma addend", vec![OP_FMA | FMA_MUL_ADD_I << SUB_SHIFT, 11, 4, 5, 10, st, 8, 1, NONE, 11]),
    ] { cases.push((name.to_owned(), false, false, tail, false)); }
    // Dead, Imm and Var steps may all lie between producer and consumer.
    cases.push(("quiet steps".to_owned(), false, false,
        vec![OP_BIN | BIN_ADD_I << SUB_SHIFT, 12, 4, 5, OP_CONST, 13, 256, OP_GET, 14, 1,
            st, 8, 1, NONE, 10, st, 9, 1, NONE, 13, st, 4, 1, NONE, 14], true));
    // A compare selected for Flags still loads and compares its operands: it takes the Acc.
    let mut flags = vec![OP_CMP_I | CMP_NE << SUB_SHIFT, 11, 10, 8];
    if_body(&mut flags, 11, &[st, 8, 1, NONE, 4], &[st, 8, 1, NONE, 5]);
    cases.push(("Flags compare".to_owned(), false, true, flags, true));

    let edges = [0i32, 1, -1, 255, 256, i32::MIN, i32::MAX, i32::MIN + 1];
    for (name, flag, result_bool, tail, acc) in cases {
        let mut body = Vec::new();
        for v in 0..4 { body.extend([ld, v, v, 1, NONE, OP_SET, v, v]); }
        for v in 0..4 { body.extend([OP_GET, 4 + v, v]); }
        body.extend([OP_CONST, 8, 1, OP_CONST, 9, 1]);
        let producer = body.len() as u32;
        body.extend(if flag { [OP_BIN | BIN_AND_B << SUB_SHIFT, 10, 6, 7] }
            else { [OP_BIN | BIN_XOR_I << SUB_SHIFT, 10, 4, 5] });
        body.extend(tail);
        let mut types = [TY_I32; 15];
        for v in [2, 3, 6, 7, 9] { types[v] = TY_BOOL; }
        types[10] = if flag { TY_BOOL } else { TY_I32 };
        types[11] = if result_bool { TY_BOOL } else { TY_I32 };
        let words = fixture_flags(&body, &types, &[TY_I32, TY_I32, TY_BOOL, TY_BOOL], 0, &[0; 10], &[], &[0, 1, 4, 5, 6, 7], &[2, 3]);
        let m = module::load(&words, &PROFILE).unwrap_or_else(|e| panic!("{name}: {e:?}"));
        let built = build_module(&m, &Options::default()).unwrap_or_else(|e| panic!("{name}: {e}"));
        let op = built.map.ops.iter().find(|op| op.code == m.entries()[0].body + 1 + producer).unwrap();
        let mut pc = op.start;
        let mut stores = 0;
        while pc < op.end {
            let (insn, len) = decode(&built.image.words[pc as usize..]).unwrap(); pc += len as u32;
            stores += usize::from(matches!(insn, Std(..) | Sts(..)));
        }
        assert_eq!(stores == 0, acc, "{name}: only Acc avoids the producer store");
        assert!(stores <= if flag { 1 } else { 4 }, "{name}: home fits its type");
        let (mut cpu, _) = booted(&built.image);
        let mut scratch = vec![0; m.scratch_words(0)];
        for (i, a) in edges.into_iter().enumerate() { for (j, b) in edges.into_iter().enumerate() {
            let initial = [a as u32, b as u32, (i & 1) as u32, (j & 1) as u32, 11, 22, 33, 44, 0, 0];
            let clocks = compare_entry(&mut cpu, &built.image, &m, 0, &initial, &mut scratch);
            assert!(clocks <= built.entries[0].max_cycles, "{name}, a={a}, b={b}: {clocks} exceeds bound {}", built.entries[0].max_cycles);
        } }
    }
}

#[test]
fn acc_increments_and_constant_fma_need_no_round_trip() {
    use fab_elec_mcu_avr::insn::{decode, Insn::*};
    use fab_elec_mcu_rt::format::*;
    let ld = OP_LOAD | REGION_STATE << SUB_SHIFT;
    let st = OP_STORE | REGION_STATE << SUB_SHIFT;
    for local in [false, true] {
        let mut body = vec![ld, 0, 0, 1, NONE, OP_CONST, 1, 1];
        if local { body.extend([OP_SET, 0, 0, OP_GET, 2, 0]); }
        body.extend([OP_BIN | BIN_ADD_I << SUB_SHIFT, 3, if local { 2 } else { 0 }, 1]);
        let sum = body.len() as u32 - 4;
        if local { body.extend([OP_SET, 0, 3, OP_GET, 4, 0, st, 0, 1, NONE, 4]); }
        else { body.extend([st, 0, 1, NONE, 3]); }
        let words = fixture(&body, &[TY_I32; 5], if local { &[TY_I32] } else { &[] }, 0, &[0], &[]);
        let m = module::load(&words, &PROFILE).unwrap();
        let built = build_module(&m, &Options::default()).unwrap();
        assert_eq!(built.entries[0].slot_bytes, 0, "local={local}: neither load nor sum needs scratch");
        let sum = built.map.ops.iter().find(|op| op.code == m.entries()[0].body + 1 + sum).unwrap();
        let mut pc = sum.start;
        while pc < sum.end {
            let (insn, len) = decode(&built.image.words[pc as usize..]).unwrap(); pc += len as u32;
            assert!(!matches!(insn, Std(..) | Sts(..)), "sum is never stored to scratch");
        }
        let consumer = built.map.ops.iter().find(|op| op.start == sum.end).unwrap();
        let mut pc = consumer.start;
        while pc < consumer.end {
            let (insn, len) = decode(&built.image.words[pc as usize..]).unwrap(); pc += len as u32;
            assert!(!matches!(insn, Ld(..) | Ldd(..) | Lds(..)), "SET/STORE takes the sum without reloading it");
        }
        let (mut cpu, _) = booted(&built.image);
        for a in [0, 255, 65535, i32::MAX as u32, u32::MAX] {
            let clocks = compare_entry(&mut cpu, &built.image, &m, 0, &[a], &mut vec![0; m.scratch_words(0)]);
            assert!(clocks <= built.entries[0].max_cycles);
        }
    }
    for addend in [0u32, 1, 256, 65536, 0x01000100, u32::MAX, i32::MIN as u32] {
        let body = [ld, 0, 0, 1, NONE, ld, 1, 1, 1, NONE, OP_CONST, 2, addend,
            OP_FMA | FMA_MUL_ADD_I << SUB_SHIFT, 3, 0, 1, 2, st, 2, 1, NONE, 3];
        let words = fixture_inputs(&body, &[TY_I32; 4], &[], 0, &[0; 3], &[], &[0, 1]);
        let m = module::load(&words, &PROFILE).unwrap();
        let built = build_module(&m, &Options::default()).unwrap();
        let op = built.map.ops.iter().find(|op| m.code()[op.code as usize] & OP_MASK == OP_FMA).unwrap();
        let mut pc = op.start;
        let mut after_call = false;
        while pc < op.end {
            let (insn, len) = decode(&built.image.words[pc as usize..]).unwrap(); pc += len as u32;
            if after_call { assert!(matches!(insn, Subi(..) | Sbci(..)), "constant addend uses only immediate arithmetic: {insn:?}"); }
            if matches!(insn, Call(_) | Rcall(_)) { after_call = true; }
        }
        assert!(after_call, "FMA still calls mul32");
        let (mut cpu, _) = booted(&built.image);
        let mut scratch = vec![0; m.scratch_words(0)];
        for a in [0u32, 1, 255, 256, i32::MIN as u32, i32::MAX as u32, u32::MAX] {
            let clocks = compare_entry(&mut cpu, &built.image, &m, 0, &[a, a.rotate_left(7), 0], &mut scratch);
            assert!(clocks <= built.entries[0].max_cycles, "constant FMA addend {addend}: {clocks} exceeds bound");
        }
    }
}

#[test]
fn acc_ports_preserve_arguments_results_and_bounds() {
    use fab_elec_mcu_avr::insn::{decode, Insn::*};
    use fab_elec_mcu_rt::format::*;
    // Different expressions keep CSE from sharing a value across two calls, and each result is
    // stored to its own variable (a store overwritten later in the entry is dropped, and with it
    // the use of its port's result). Pins are declared before use; shift_out's fourth argument
    // is deliberately an immediate.
    let bodies = [
        "write((a & 1) + 4, high)\nwrite(pin, a ^ 1)\nwrite(led, a ^ 2)\nshift_out(data, clock, a ^ 4, 8)",
        "sampled = read(sense) + 1\npin = read(input_pin) + 2\nwrite(led, read(sense))\na = eeprom_read(0) + 1",
    ];
    for (group, body) in bodies.into_iter().enumerate() {
        for a in [0i32, 1, 255, 256, i32::MIN, i32::MAX] {
            let source = format!("\
let led = output(PD4)
let aux = output(PD5)
let data = output(PB3)
let clock = output(PB5)
let sense = input(PD2, pull_up)
var a: i32 = {a}
var pin: i32 = 4
var input_pin: i32 = 2
var sampled: i32 = 0
fn setup() {{
{body}
}}
");
            let c = compiled(&source);
            let m = module::load(&c.module, &PROFILE).unwrap();
            let setup = m.entry(fab_elec_mcu_rt::SETUP).unwrap();
            let high = a & 1 != 0;
            let mut model = Model { input_high: Some(high), ..Model::default() };
            let mut device = Device::new(module::load(&c.module, &PROFILE).unwrap(), &PROFILE);
            device.boot(&mut model);
            for level in [watch::Level::Off, watch::Level::Faults, watch::Level::Ports] {
                let built = build_module(&m, &Options { watch: level, ..Options::default() }).unwrap();
                let image = &built.image;
                let mut moved_b = 0;
                for op in &built.map.ops {
                    if m.code()[op.code as usize] & OP_MASK != OP_PORT { continue; }
                    let mut pc = op.start;
                    while pc < op.end {
                        let (insn, len) = decode(&image.words[pc as usize..]).unwrap(); pc += len as u32;
                        moved_b += usize::from(matches!(insn, Mov(20, 16) | Movw(20, 16)));
                        if group == 1 {
                            assert!(!matches!(insn, Std(..) | Sts(..)), "single-output ports hand their result to the next consumer");
                        }
                    }
                }
                if group == 0 { assert_eq!(moved_b, 3, "generic arg 1, inline level and shift arg 2 are saved to B first"); }
                let (mut cpu, _) = booted(image);
                cpu.set_pin('D', 2, high);
                to_symbol(&mut cpu, image.symbols.setup, 10_000);
                // EntryReport excludes interrupt service; leave timer interrupts masked here.
                cpu.sreg &= !128;
                let start = cpu.cycles;
                let ret = built.map.returns.iter().find(|&&(_, k)| k == setup).unwrap().0;
                to_symbol(&mut cpu, ret, 20_000);
                let clocks = cpu.cycles - start;
                assert!(clocks <= built.entries[setup].max_cycles,
                    "group {group}, a={a}, {level:?}: {clocks} exceeds bound {}", built.entries[setup].max_cycles);
                assert_eq!(state(&cpu, image), device.state(), "group {group}, a={a}, {level:?}");
                drain(&mut cpu, image);
                let expected = if level == watch::Level::Ports { model.events.clone() } else { Vec::new() };
                assert_eq!(observed(&cpu), expected, "group {group}, a={a}, {level:?}: exact port arguments and results");
            }
        }
    }
}

#[test]
fn flags_compares_match_exec_and_bounds() {
    use fab_elec_mcu_avr::insn::{decode, Insn::*};
    use fab_elec_mcu_rt::format::*;
    let ld = OP_LOAD | REGION_STATE << SUB_SHIFT;
    let st = OP_STORE | REGION_STATE << SUB_SHIFT;
    let edges = [0i32, 1, -1, 255, 256, i32::MIN, i32::MAX];
    for constant in std::iter::once(None).chain(edges.into_iter().map(Some)) {
        for left in [false, true] {
            if constant.is_none() && left { continue; }
            // Set/get keeps the dynamic operands in Var homes. The initial loads feed SET
            // through Acc, and Flags (or a constant-result Acc compare) needs no slot.
            let mut body = vec![ld, 0, 0, 1, NONE, OP_SET, 0, 0,
                ld, 1, 1, 1, NONE, OP_SET, 1, 1, OP_CONST, 4, 1, OP_CONST, 5, 0];
            for (v, var) in [(2, 0), (3, 1)] {
                if let Some(k) = constant.filter(|_| (v == 2) == left) { body.extend([OP_CONST, v, k as u32]); }
                else { body.extend([OP_GET, v, var]); }
            }
            let mut types = vec![TY_I32, TY_I32, TY_I32, TY_I32, TY_BOOL, TY_BOOL];
            let mut conditions = Vec::new();
            for sense in [CMP_LT, CMP_LE, CMP_GT, CMP_GE, CMP_EQ, CMP_NE] {
                for not in [false, true] {
                    let cmp = types.len() as u32; types.push(TY_BOOL);
                    let compare_at = body.len() as u32;
                    body.extend([OP_CMP_I | sense << SUB_SHIFT, cmp, 2, 3]);
                    let condition = if not {
                        let v = types.len() as u32; types.push(TY_BOOL);
                        body.extend([OP_UN | UN_NOT_B << SUB_SHIFT, v, cmp]); v
                    } else { cmp };
                    let folded = constant == Some(i32::MAX) && if left {
                        matches!(sense, CMP_LT | CMP_GE)
                    } else { matches!(sense, CMP_LE | CMP_GT) };
                    let offset = conditions.len() as u32 + 2;
                    conditions.push((body.len() as u32, compare_at, folded));
                    if_body(&mut body, condition, &[st, offset, 1, NONE, 4], &[st, offset, 1, NONE, 5]);
                }
            }
            // Dirty state is part of state_init, so the proof includes the
            // upper bytes these full-word overwrite checks deliberately use.
            let words = fixture_inputs(&body, &types, &[TY_I32; 2], 0, &[0xa5a5a5a5; 14], &[], &[0, 1]);
            let m = module::load(&words, &PROFILE).unwrap();
            let base = m.entries()[0].body + 1;
            let built = build_module(&m, &Options {
                conds: conditions.iter().enumerate().map(|(id, &(at, _, _))| (base + at, id as u32)).collect(),
                ..Options::default()
            }).unwrap();
            assert_eq!(built.entries[0].slot_bytes, 0,
                "{constant:?}, left={left}: Acc and Flags need no scratch value slots");
            assert_eq!(built.map.conds.len(), conditions.len(), "every if retains its condition record");
            for (expected, (&(pc, id), &(at, compare_at, folded))) in built.map.conds.iter().zip(&conditions).enumerate() {
                assert_eq!(id as usize, expected, "condition IDs retain source order");
                let op = built.map.ops.iter().find(|op| op.code == base + at).unwrap();
                let compare = built.map.ops.iter().find(|op| op.code == base + compare_at).unwrap();
                let mut instructions = Vec::new();
                let mut cursor = op.start;
                while cursor < op.end {
                    let (insn, len) = decode(&built.image.words[cursor as usize..]).unwrap();
                    instructions.push(insn); cursor += len as u32;
                }
                assert_eq!(instructions.contains(&Cpi(16, 0)), folded,
                    "{constant:?}, left={left}, condition {id}: only the fallback retests a bool byte");
                if !folded {
                    assert_eq!(compare.end, op.start, "nothing emits between compare and branch, even under !");
                    assert_eq!(pc, op.start, "the condition record names the Flags branch");
                    assert_eq!(instructions.len(), 2, "Flags if emits only the short branch and else jump");
                    assert!(matches!(decode(&built.image.words[compare.end as usize - 1..]).unwrap().0, Cp(..) | Cpi(..) | Cpc(..)),
                        "Flags compare leaves SREG, without materialising or storing a bool");
                }
            }
            let (mut cpu, _) = booted(&built.image);
            let mut scratch = vec![0; m.scratch_words(0)];
            for a in edges { for b in if constant.is_none() { edges.to_vec() } else { vec![0] } {
                let mut initial = [0xa5a5a5a5; 14]; initial[0] = a as u32; initial[1] = b as u32;
                // A left constant compares against the second variable.
                if left { initial[1] = a as u32; }
                let clocks = compare_entry(&mut cpu, &built.image, &m, 0, &initial, &mut scratch);
                assert!(clocks <= built.entries[0].max_cycles,
                    "{constant:?}, left={left}, a={a}, b={b}: measured {clocks} exceeds bound {}", built.entries[0].max_cycles);
            } }
        }
    }
}

#[test]
fn flags_homes_require_a_single_use_and_no_emitting_step_between() {
    use fab_elec_mcu_avr::insn::{decode, Insn::*};
    use fab_elec_mcu_rt::format::*;
    let ld = OP_LOAD | REGION_STATE << SUB_SHIFT;
    let st = OP_STORE | REGION_STATE << SUB_SHIFT;
    for (name, fused) in [("quiet steps", true), ("quiet steps around not", true),
        ("arithmetic", false), ("arithmetic after not", false), ("store", false), ("set", false), ("helper", false),
        ("control", false), ("loop", false), ("two compare uses", false),
        ("two not uses", false), ("two nots", false)] {
        let mut body = vec![ld, 0, 0, 1, NONE, ld, 1, 1, 1, NONE, ld, 7, 2, 1, NONE,
            OP_SET, 0, 0, OP_CMP_I | CMP_LT << SUB_SHIFT, 2, 0, 1];
        // A dead arithmetic op, a live constant and a live Var get all emit nothing.
        body.extend([OP_BIN | BIN_ADD_I << SUB_SHIFT, 4, 0, 1, OP_CONST, 5, 17, OP_GET, 6, 0]);
        let mut condition = 2;
        let not = matches!(name, "quiet steps around not" | "arithmetic after not" | "two not uses" | "two nots");
        if not {
            body.extend([OP_UN | UN_NOT_B << SUB_SHIFT, 3, 2]); condition = 3;
            body.extend([OP_CONST, 8, 23, OP_GET, 9, 0]);
        }
        match name {
            "arithmetic" | "arithmetic after not" | "helper" => {
                let sub = if name == "helper" { BIN_MUL_I } else { BIN_ADD_I };
                body.extend([OP_BIN | sub << SUB_SHIFT, 10, 0, 1]);
            }
            "store" => body.extend([st, 3, 1, NONE, 0]),
            "set" => body.extend([OP_SET, 0, 1]),
            "control" => if_body(&mut body, 7, &[OP_SET, 0, 1], &[]),
            "two nots" => { body.extend([OP_UN | UN_NOT_B << SUB_SHIFT, 11, 3]); condition = 11; }
            _ => {}
        }
        let mut branch = Vec::new();
        if_body(&mut branch, condition, &[st, 3, 1, NONE, 0], &[st, 3, 1, NONE, 1]);
        let at = body.len() as u32 + if name == "loop" { 3 } else { 0 };
        if name == "loop" { loop_body(&mut body, 2, &branch); }
        else { body.extend(branch); }
        if name == "two compare uses" { body.extend([st, 4, 1, NONE, 2]); }
        if name == "two not uses" { body.extend([st, 4, 1, NONE, 3]); }
        if matches!(name, "arithmetic" | "arithmetic after not" | "helper") { body.extend([st, 4, 1, NONE, 10]); }
        if not {
            body.extend([st, 7, 1, NONE, 8, st, 8, 1, NONE, 9]);
        }
        body.extend([st, 5, 1, NONE, 5, st, 6, 1, NONE, 6]);
        let mut types = [TY_I32; 12]; for v in [2, 3, 7, 11] { types[v] = TY_BOOL; }
        let words = fixture_flags(&body, &types, &[TY_I32], 0, &[0; 9], &[], &[0, 1], &[2]);
        let m = module::load(&words, &PROFILE).unwrap_or_else(|e| panic!("{name}: {e:?}"));
        let built = build_module(&m, &Options::default()).unwrap();
        let op = built.map.ops.iter().find(|op| op.code == m.entries()[0].body + 1 + at).unwrap();
        assert_eq!(matches!(decode(&built.image.words[op.start as usize..]).unwrap().0, Brbs(..) | Brbc(..)), fused,
            "{name}: only a single-use compare with no emitting intervening step becomes Flags");
        for op in &built.map.ops {
            if m.code()[op.code as usize] & OP_MASK != OP_CMP_I { continue; }
            let acc = matches!(name, "arithmetic after not" | "two not uses" | "two nots");
            assert_eq!(matches!(decode(&built.image.words[op.end as usize - 1..]).unwrap().0, Std(..)), !fused && !acc,
                "{name}: a compare stores only when neither Flags nor the next ! can consume it");
        }
        let (mut cpu, _) = booted(&built.image);
        let mut scratch = vec![0; m.scratch_words(0)];
        for (a, b) in [(-1i32, 0i32), (0, 0), (256, 255), (i32::MIN, i32::MAX)] { for flag in [0, 1] {
            let initial = [a as u32, b as u32, flag, 0, 0, 0, 0, 0, 0];
            let clocks = compare_entry(&mut cpu, &built.image, &m, 0, &initial, &mut scratch);
            assert!(clocks <= built.entries[0].max_cycles, "{name}: measured {clocks} exceeds bound {}", built.entries[0].max_cycles);
        } }
    }
}

#[test]
fn recorded_flags_branches_are_taken_exactly_when_the_condition_is_true() {
    // Distinct state operands keep source CSE from sharing a compare between the plain and ! ifs.
    let mut source = String::from("var inputs = [int(0); 12]\nvar hits = [int(0); 12]\nlet tick = every(ms(100), fn() {\n");
    for (k, op) in ["<", "<=", ">", ">=", "==", "!="].iter().enumerate() {
        for not in [false, true] {
            let id = k * 2 + usize::from(not);
            let condition = if not { format!("!(inputs[{id}] {op} 0)") } else { format!("inputs[{id}] {op} 0") };
            source.push_str(&format!("    if {condition} {{ hits[{id}] = hits[{id}] + 1 }}\n"));
        }
    }
    source.push_str("})\nlet input_update = every(ms(1000), fn() {\n");
    for i in 0..12 { source.push_str(&format!("inputs[{i}] = inputs[{i}] + 1\n")); }
    source.push_str("})\n");
    let c = compiled(&source);
    let m = module::load(&c.module, &PROFILE).unwrap();
    let built = build_module(&m, &Options { conds: c.cond_marks.clone(), ..Options::default() }).unwrap();
    let entry = m.entry("tick").unwrap();
    assert_eq!(built.map.conds.len(), 12, "every source if has a recorded branch");
    for &(pc, id) in &built.map.conds {
        let code = c.cond_marks.iter().find(|&&(_, cond)| cond == id).unwrap().0;
        let op = built.map.ops.iter().find(|op| op.code == code).unwrap();
        assert_eq!(op.start, pc, "condition {id} uses Flags, without a bool load or retest");
    }
    let inputs = m.vars().iter().find(|v| m.string(v.name) == "inputs").unwrap().offset as usize;
    let hits = m.vars().iter().find(|v| m.string(v.name) == "hits").unwrap().offset as usize;
    let (mut cpu, _) = booted(&built.image);
    let mut scratch_words = vec![0; m.scratch_words(entry)];
    let mut taken = [0; 12];
    let mut visited = [0; 12];
    let mut trues = [0; 12];
    for a in [-1i32, 0, 1] {
        let mut initial = m.state_init().to_vec(); initial[inputs..inputs + 12].fill(a as u32);
        let mut want = initial.clone();
        assert!(matches!(exec::run(&m, entry, &mut want, &mut scratch_words, &mut NoPorts), exec::Exit::Done { .. }));
        for (id, &true_count) in want[hits..hits + 12].iter().enumerate() { trues[id] += true_count; }
        for (i, &word) in initial.iter().enumerate() { store(&mut cpu, built.image.layout.state + 4 * i as u16, u64::from(word), 4); }
        cpu.sreg = 0;
        let ret = scratch(&mut cpu, &built.image) + 1;
        let sp = cpu.sp;
        return_to(&mut cpu, ret); cpu.pc = built.map.entries[entry].0;
        let start = cpu.cycles;
        while cpu.pc != ret || cpu.sp != sp {
            assert!(cpu.cycles - start < built.entries[entry].max_cycles, "a={a}: handler must return within its cycle bound");
            let before = cpu.pc;
            cpu.step().expect("condition-counting instruction executes");
            if let Some(id) = built.map.conds.iter().position(|&(pc, _)| pc == before) {
                visited[id] += 1;
                let was_taken = cpu.pc != before + 1;
                taken[id] += u32::from(was_taken);
                assert_eq!(was_taken, want[hits + id] == 1, "a={a}, condition {id}: taken must mean true");
            }
        }
        assert!(cpu.cycles - start <= built.entries[entry].max_cycles);
        assert_eq!(state(&cpu, &built.image), want, "a={a}: branch counts and state match the reference");
    }
    assert_eq!(visited, [3; 12], "each recorded condition executes once per input");
    assert_eq!(taken, trues, "every recorded branch's taken count equals the reference condition's true count");
    assert!(taken.iter().all(|&n| n > 0 && n < 3), "every polarity was exercised both ways");
}

#[test]
fn variable_zeroing_follows_definite_assignment() {
    use fab_elec_mcu_avr::insn::{decode, Insn::*};
    use fab_elec_mcu_rt::format::*;
    let st = OP_STORE | REGION_STATE << SUB_SHIFT;
    let mut types = vec![TY_BOOL, TY_I32, TY_I32, TY_BOOL, TY_BOOL];
    let mut value = |ty| { let v = types.len() as u32; types.push(ty); v };
    let mut body = vec![OP_LOAD | REGION_STATE << SUB_SHIFT, 0, 0, 1, NONE,
        OP_CONST, 1, 7, OP_CONST, 2, 0, OP_CONST, 3, 1, OP_CONST, 4, 0];
    if_body(&mut body, 0, &[OP_SET, 0, 1], &[]); // only one arm
    if_body(&mut body, 0, &[OP_SET, 1, 1], &[OP_SET, 1, 2]); // both arms
    let mut inner = Vec::new();
    if_body(&mut inner, 0, &[OP_BREAK, 0], &[]);
    inner.extend([OP_SET, 2, 1]); loop_body(&mut body, 2, &inner); // a break bypasses the set
    loop_body(&mut body, 2, &[OP_BREAK, 0, OP_SET, 3, 1]);
    body.extend([OP_SET, 5, 2]);
    let (top, sum, added) = (value(TY_I32), value(TY_I32), value(TY_I32));
    loop_body(&mut body, 2, &[OP_GET, top, 4, OP_GET, sum, 5,
        OP_BIN | BIN_ADD_I << SUB_SHIFT, added, sum, top, OP_SET, 5, added, OP_SET, 4, 1]);
    // The top read is zero on the first iteration, seven on the second: sum is seven.
    loop_body(&mut body, 2, &[OP_CONTINUE, 0, OP_SET, 6, 1]);
    body.extend([OP_GET, value(TY_I32), 7, OP_SET, 7, 1]); // a dead get needs no zero
    loop_body(&mut body, 0, &[OP_SET, 8, 1]);
    if_body(&mut body, 0, &[OP_SET, 9, 3], &[]); // bool slots obey the same rules
    if_body(&mut body, 0, &[OP_SET, 10, 3], &[OP_SET, 10, 4]);
    let mut inner = Vec::new();
    if_body(&mut inner, 0, &[OP_BREAK, 0], &[OP_CONTINUE, 0]);
    inner.extend([OP_SET, 11, 1]); loop_body(&mut body, 2, &inner);
    let local = value(TY_I32);
    // Each checked get is followed by a set of its own value, so it stays a copy (not a Var
    // home) and its load names the variable's slot.
    loop_body(&mut body, 2, &[OP_SET, 12, 1, OP_GET, local, 12, OP_SET, 12, local, st, 13, 1, NONE, local]);
    let mut vars = [TY_I32; 13]; vars[9] = TY_BOOL; vars[10] = TY_BOOL;
    for v in 0..12 {
        let out = value(vars[v]);
        body.extend([OP_GET, out, v as u32, OP_SET, v as u32, out, st, v as u32 + 1, 1, NONE, out]);
    }
    let zero = [true, false, true, true, true, false, true, false, true, true, false, true, false];
    let mut state_init = [0xaaaaaaaa; 14]; state_init[0] = 0;
    let words = fixture_flags(&body, &types, &vars, 0, &state_init, &[], &[], &[0]);
    let m = module::load(&words, &PROFILE).unwrap();
    let built = build_module(&m, &Options::default()).unwrap();
    let image = &built.image;
    let mut zeroed = Vec::new();
    let mut pc = built.map.entries[0].0;
    while pc < built.map.ops[0].start {
        let (insn, len) = decode(&image.words[pc as usize..]).unwrap();
        match insn {
            Ldi(28 | 29, _) => {},
            Std(true, offset, 2) => zeroed.push(image.layout.scratch + u16::from(offset)),
            Sts(address, 2) => zeroed.push(address),
            _ => panic!("unexpected prologue instruction {insn:?}"),
        }
        pc += len as u32;
    }
    let width = |var: usize| {
        let r = built.ranges.entries[0].vars[var];
        if vars[var] == TY_BOOL || (r.lo >= 0 && r.hi <= 255) { 1 } else { 4 }
    };
    let expected: usize = zero.iter().enumerate().filter(|(_, zero)| **zero).map(|(v, _)| width(v)).sum();
    assert_eq!(zeroed.len(), expected, "only variables read before assignment are zeroed, at their proved width");
    let mut checked = [false; 13];
    for op in &built.map.ops {
        if m.code()[op.code as usize] & OP_MASK != OP_GET { continue; }
        let var = m.code()[op.code as usize + 2] as usize;
        let address = match decode(&image.words[op.start as usize..]).unwrap().0 {
            Ldd(16, true, offset) => image.layout.scratch + u16::from(offset),
            Lds(16, address) => address,
            insn => panic!("variable {var}: unexpected get {insn:?}"),
        };
        for byte in 0..width(var) {
            assert_eq!(zeroed.contains(&(address + byte as u16)), zero[var], "variable {var}, byte {byte}");
        }
        checked[var] = true;
    }
    assert_eq!(checked, [true; 13], "every variable's slot was located through a get");
    let (mut cpu, _) = booted(image);
    let mut scratch = vec![0xa5a5a5a5; m.scratch_words(0)];
    for flag in [0, 1, 0] {
        let mut initial = vec![0xaaaaaaaa; 14]; initial[0] = flag;
        let clocks = compare_entry(&mut cpu, image, &m, 0, &initial, &mut scratch);
        assert!(clocks <= built.entries[0].max_cycles, "flag {flag}: {clocks} cycles exceeds bound");
        assert_eq!(state(&cpu, image), [flag, 7 * flag, 7 * flag, 7 * (1 - flag), 0, 7, 7, 0, 7, 0, flag, flag, 0, 7],
            "dirty entry, flag {flag}: skipped assignments still read zero");
    }
}

#[test]
fn loop_counter_caps_exits_and_bounds() {
    use fab_elec_mcu_avr::insn::{decode, Insn::*};
    use fab_elec_mcu_rt::format::*;
    let ld = OP_LOAD | REGION_STATE << SUB_SHIFT;
    let st = OP_STORE | REGION_STATE << SUB_SHIFT;
    for cap in [1, 2, 255, 256, 257] {
        let mut body = vec![ld, 0, 0, 1, NONE, ld, 1, 1, 1, NONE, ld, 2, 2, 1, NONE,
            OP_CONST, 3, 0, OP_CONST, 4, 1, OP_SET, 0, 3, OP_SET, 1, 3];
        let mut outer = vec![OP_GET, 5, 0, OP_BIN | BIN_ADD_I << SUB_SHIFT, 6, 5, 4, OP_SET, 0, 6];
        let mut inner = vec![OP_GET, 7, 1, OP_BIN | BIN_ADD_I << SUB_SHIFT, 8, 7, 4, OP_SET, 1, 8];
        if_body(&mut inner, 0, &[OP_BREAK, 1], &[]);
        if_body(&mut inner, 1, &[OP_BREAK, 0], &[]);
        if_body(&mut inner, 2, &[OP_CONTINUE, 1], &[]);
        inner.extend([OP_CONTINUE, 0, OP_SET, 1, 3]);
        loop_body(&mut outer, 2, &inner);
        outer.extend([OP_CONTINUE, 0, OP_SET, 0, 3]);
        loop_body(&mut body, cap, &outer);
        body.extend([OP_GET, 9, 0, st, 3, 1, NONE, 9, OP_GET, 10, 1, st, 4, 1, NONE, 10]);
        let mut types = [TY_I32; 11]; types[..3].fill(TY_BOOL);
        // The boundary cap also exercises absolute counter loads/stores beyond Y+63.
        // Reset takes the early break; the dirty invocations below exercise every path.
        let words = fixture_flags(&body, &types, &[TY_I32; 2], if cap == 256 { 17 } else { 0 },
            &[1, 0, 0, 0xaaaaaaaa, 0xaaaaaaaa], &[], &[], &[0, 1, 2]);
        let m = module::load(&words, &PROFILE).unwrap();
        let built = build_module(&m, &Options::default()).unwrap();
        let image = &built.image;
        let mut narrow = 0;
        for op in &built.map.ops {
            if m.code()[op.code as usize] & OP_MASK != OP_LOOP { continue; }
            let count = m.code()[op.code as usize + 1];
            let mut pc = op.start;
            let mut insns = Vec::new();
            while pc < op.end {
                let (insn, len) = decode(&image.words[pc as usize..]).unwrap();
                insns.push(insn); pc += len as u32;
            }
            let bytes = if count <= 256 { 1 } else { 4 };
            assert_eq!(insns.len(), bytes * 2, "cap {count}: counter initialization width");
            for byte in 0..bytes {
                assert_eq!(insns[byte], Ldi(16 + byte as u8, ((count - 1) >> (8 * byte)) as u8));
                assert!(matches!(insns[bytes + byte], Std(true, _, r) | Sts(_, r) if r == 16 + byte as u8));
            }
            narrow += usize::from(bytes == 1);
        }
        let mut ends = 0;
        let (mut pc, end) = built.map.entries[0];
        while pc < end {
            let (insn, len) = decode(&image.words[pc as usize..]).unwrap();
            if matches!(insn, Brbs(0, _)) { ends += 1; }
            pc += len as u32;
        }
        assert_eq!(ends, narrow, "each byte counter ends with the carry/borrow test");
        assert_eq!(narrow, if cap <= 256 { 2 } else { 1 }, "cap 257 retains the wide outer counter");
        let (mut cpu, _) = booted(image);
        let mut scratch = vec![0; m.scratch_words(0)];
        for flags in [[0, 0, 0], [0, 1, 0], [0, 0, 1], [1, 0, 0]] {
            let initial = [flags[0], flags[1], flags[2], 0xaaaaaaaa, 0xaaaaaaaa];
            let clocks = compare_entry(&mut cpu, image, &m, 0, &initial, &mut scratch);
            assert!(clocks <= built.entries[0].max_cycles,
                "cap {cap}, flags {flags:?}: measured {clocks} exceeds bound {}", built.entries[0].max_cycles);
            let outer = if flags[0] != 0 { 1 } else { cap };
            let inner = if flags[0] != 0 { 1 } else { cap * if flags[1] != 0 || flags[2] != 0 { 1 } else { 2 } };
            assert_eq!(&state(&cpu, image)[3..], &[outer, inner], "cap {cap}, flags {flags:?}: iteration counts");
        }
    }
}

#[test]
fn variable_homes_match_exec_and_bounds() {
    use fab_elec_mcu_rt::format::*;
    let ld = OP_LOAD | REGION_STATE << SUB_SHIFT;
    let st = OP_STORE | REGION_STATE << SUB_SHIFT;
    // Keep the exact get/use/set order: source lowering may eliminate a copy.
    // Vars 0/1 are i32/bool; var 2 indexes each iteration's observable result.
    let take: &[u32] = &[OP_GET, 4, 0, OP_GET, 5, 1];
    let use_: &[u32] = &[
        OP_GET, 10, 2, st, 3, 6, 10, 4, st, 9, 6, 10, 5,
        OP_BIN | BIN_ADD_I << SUB_SHIFT, 11, 10, 3, OP_SET, 2, 11,
    ];
    let change: &[u32] = &[
        OP_GET, 6, 0, OP_BIN | BIN_ADD_I << SUB_SHIFT, 7, 6, 3, OP_SET, 0, 7,
        OP_GET, 8, 1, OP_UN | UN_NOT_B << SUB_SHIFT, 9, 8, OP_SET, 1, 9,
    ];
    let branch = |then: &[u32], otherwise: &[u32]| {
        let mut body = vec![OP_IF, 2, then.len() as u32]; body.extend_from_slice(then);
        body.push(otherwise.len() as u32); body.extend_from_slice(otherwise); body
    };
    let repeat = |cap, inner: &[u32]| {
        let mut body = Vec::new(); loop_body(&mut body, cap, inner); body
    };
    let cases = [
        ("plain", [take, use_].concat(), true),
        ("last use is set", [take, use_, &[OP_SET, 0, 4, OP_SET, 1, 5]].concat(), true),
        ("set between uses", [take, &[st, 3, 1, NONE, 4, st, 9, 1, NONE, 5], change, use_].concat(), false),
        ("set in then", [take, &branch(change, &[]), use_].concat(), false),
        ("set in else", [take, &branch(&[], change), use_].concat(), false),
        ("mutually exclusive set and use", [take, &branch(change, use_)].concat(), false),
        ("get before loop", [take, &repeat(3, &[use_, change].concat())].concat(), false),
        ("get in loop", repeat(3, &[take, use_, change].concat()), true),
        ("unchanged through loop", [take, &repeat(3, use_)].concat(), true),
        ("get outside inner loop", repeat(2, &[take, &repeat(3, &[use_, change].concat())].concat()), false),
        ("get inside inner loop", repeat(2, &repeat(3, &[take, use_, change].concat())), true),
        ("set after inner loop", repeat(2, &[take, &repeat(3, use_), change].concat()), true),
        ("one iteration still kills", [take, &repeat(1, &[use_, change].concat())].concat(), false),
        ("zero cap still kills", [take, &repeat(0, change), use_].concat(), false),
        ("set after continue still kills", [take, &repeat(3, &[use_, &[OP_CONTINUE, 0], change].concat())].concat(), false),
        ("dead later uses", [take, use_, change,
            &[OP_BIN | BIN_ADD_I << SUB_SHIFT, 12, 4, 3, OP_UN | UN_NOT_B << SUB_SHIFT, 13, 5]].concat(), true),
    ];
    let mut types = [TY_I32; 14];
    for v in [1, 2, 5, 8, 9, 13] { types[v] = TY_BOOL; }
    for (name, tail, borrowed) in cases {
        let mut body = vec![ld, 0, 0, 1, NONE, ld, 1, 1, 1, NONE, ld, 2, 2, 1, NONE,
            OP_CONST, 3, 1, OP_SET, 0, 0, OP_SET, 1, 1];
        let get = body.len() + tail.windows(take.len()).position(|w| w == take).unwrap();
        body.extend(tail);
        let mut state_init = [0xa5a5a5a5; 15]; state_init[..3].fill(0);
        let words = fixture_flags(&body, &types, &[TY_I32, TY_BOOL, TY_I32], 0, &state_init, &[], &[0], &[1, 2]);
        let m = module::load(&words, &PROFILE).unwrap_or_else(|e| panic!("{name}: {e:?}"));
        let built = build_module(&m, &Options::default()).unwrap_or_else(|e| panic!("{name}: {e}"));
        for offset in [0, 3] {
            let code = m.entries()[0].body + 1 + get as u32 + offset;
            assert_eq!(built.map.ops.iter().any(|op| op.code == code), !borrowed,
                "{name}: a live get has no emitted instructions exactly when it can use its var's slot");
        }
        let (mut cpu, _) = booted(&built.image);
        let mut scratch = vec![0; m.scratch_words(0)];
        for value in [0i32, 255, 256, -1, i32::MIN, i32::MAX] {
            for flag in [0, 1] { for condition in [0, 1] {
                let mut initial = [0xa5a5a5a5; 15];
                initial[..3].copy_from_slice(&[value as u32, flag, condition]);
                let clocks = compare_entry(&mut cpu, &built.image, &m, 0, &initial, &mut scratch);
                assert!(clocks <= built.entries[0].max_cycles,
                    "{name}, value={value}, flag={flag}, condition={condition}: measured {clocks} exceeds bound {}",
                    built.entries[0].max_cycles);
            } }
        }
    }
}

#[test]
fn variable_homes_feed_ports_and_compares() {
    let source = "\
let led = output(PB5)
var value: i32 = 256
var flag = true
var small = false
fn setup() {
    let saved = value
    let held = flag
    write(led, saved)
    small = saved < 257
    value = value + 1
    write(led, if held { low } else { high })
    flag = !held
}
";
    let c = compiled(source);
    let m = module::load(&c.module, &PROFILE).unwrap();
    let setup = m.entry(fab_elec_mcu_rt::SETUP).unwrap();
    let built = build_module(&m, &Options { watch: watch::Level::Ports, ..Options::default() }).unwrap();
    let image = &built.image;
    let (mut cpu, _) = booted(image);
    to_symbol(&mut cpu, image.symbols.setup, 10_000);
    let start = cpu.cycles;
    let ret = built.map.returns.iter().find(|&&(_, k)| k == setup).unwrap().0;
    to_symbol(&mut cpu, ret, 10_000);
    let clocks = cpu.cycles - start;
    assert!(clocks <= built.entries[setup].max_cycles,
        "ports and compares: measured {clocks} exceeds bound {}", built.entries[setup].max_cycles);
    let mut model = Model::default();
    let mut device = Device::new(m, &PROFILE); device.boot(&mut model);
    assert_eq!(state(&cpu, image), device.state(), "saved values survive the port call before the compare");
    assert_eq!(device.state(), &[257, 0, 1], "i32 and bool updates follow their saved uses");
    drain(&mut cpu, image);
    assert_eq!(observed(&cpu), model.events, "variable operands retain the exact port events");
}

#[test]
fn memory_slots_and_loop_scopes() {
    use fab_elec_mcu_rt::format::*;
    let ld = |region| OP_LOAD | region << SUB_SHIFT;
    let st = |region| OP_STORE | region << SUB_SHIFT;
    let body = [
        ld(REGION_STATE), 0, 0, 1, NONE, ld(REGION_STATE), 1, 1, 1, NONE,
        ld(REGION_STATE), 2, 2, 1, NONE,
        OP_GET, 3, 0, st(REGION_STATE), 3, 1, NONE, 3,
        OP_GET, 4, 1, st(REGION_STATE), 4, 1, NONE, 4,
        ld(REGION_FRAME), 5, 0, 4, NONE, st(REGION_STATE), 5, 1, NONE, 5,
        ld(REGION_STATE), 6, 6, 4, 0, ld(REGION_SHARED), 7, 0, 4, 0,
        OP_BIN | BIN_XOR_I << SUB_SHIFT, 8, 6, 7,
        st(REGION_FRAME), 0, 4, 0, 8, ld(REGION_FRAME), 9, 0, 4, 0,
        st(REGION_STATE), 10, 4, 0, 9,
        st(REGION_FRAME), 0, 4, 0, 2, ld(REGION_FRAME), 10, 0, 4, 0,
        st(REGION_STATE), 14, 4, 0, 10,
        ld(REGION_SHARED), 11, 4, 4, 0, st(REGION_STATE), 18, 4, 0, 11,
        OP_CONST, 12, u32::MAX, ld(REGION_SHARED), 13, 0, 4, 12,
        st(REGION_STATE), 22, 1, NONE, 13,
        OP_CONST, 14, 1, ld(REGION_STATE), 15, 6, 4, 14,
        st(REGION_STATE), 23, 1, NONE, 15,
    ];
    let mut types = vec![TY_I32; 16];
    for k in [2, 4, 11] { types[k] = TY_BOOL; }
    let mut state_init = [0xaaaaaaaa; 24]; state_init[2] = 0;
    let words = fixture_flags(&body, &types, &[TY_I32, TY_BOOL], 4,
        &state_init, &[0x01020304, 0x88776655, 0x12345678, 0xfedcba98, 1, 0, 1, 0], &[0, 1, 6, 7, 8, 9], &[2]);
    let m = module::load(&words, &PROFILE).unwrap();
    let built = build_module(&m, &Options::default()).unwrap();
    let (mut cpu, _) = booted(&built.image);
    let mut scratch = vec![0; m.scratch_words(0)];
    for index in [-1i32, 0, 3, 4, i32::MAX, i32::MIN, 255, 65536] {
        for flag in 0..=1 {
            let mut initial = vec![0xaaaaaaaa; 24]; initial[0] = index as u32; initial[2] = flag;
            initial[6..10].copy_from_slice(&[1, 0x12345678, 0x80000000, u32::MAX]);
            compare_entry(&mut cpu, &built.image, &m, 0, &initial, &mut scratch);
        }
    }

    // Unread values of any type cost nothing, including every helper-backed op.
    let live = [OP_CONST, 0, 5, st(REGION_STATE), 0, 1, NONE, 0];
    let mut padded = live.to_vec();
    padded.extend([OP_CONST, 1, 0x3f80_0000, OP_CONST, 2, 7, OP_BIN | BIN_ADD_I << SUB_SHIFT, 3, 2, 2]);
    for (v, op) in (4..).zip([BIN_MUL_I, BIN_DIV_I, BIN_REM_I, BIN_SHL_I, BIN_SHR_UI, BIN_SHR_I]) {
        padded.extend([OP_BIN | op << SUB_SHIFT, v, 3, 3]);
    }
    padded.extend([OP_FMA | FMA_MUL_ADD_I << SUB_SHIFT, 10, 3, 3, 3, OP_WRAP, 11, 3, 7]);
    let lean = fixture(&live, &[TY_I32], &[], 0, &[0], &[]);
    let mut types = [TY_I32; 12]; types[1] = TY_F32;
    let fat = fixture(&padded, &types, &[], 0, &[0], &[]);
    let (lean, fat) = (module::load(&lean, &PROFILE).unwrap(), module::load(&fat, &PROFILE).unwrap());
    for watch in [watch::Level::Off, watch::Level::Faults, watch::Level::Ports] {
        let options = Options { watch, ..Options::default() };
        let (lean, fat) = (build_module(&lean, &options).unwrap(), build_module(&fat, &options).unwrap());
        assert_eq!(fat.image.bytes(), lean.image.bytes(), "{watch:?}: unused helpers must leave the complete image identical");
        assert_eq!(fat.image.layout.stack_bytes, if watch == watch::Level::Off { 16 } else { 32 },
            "{watch:?}: helper-free stack budget is unchanged");
        assert_eq!((fat.entries[0].flash_words, fat.entries[0].slot_bytes), (lean.entries[0].flash_words, lean.entries[0].slot_bytes),
            "dead values must add no code and no slots");
    }

    let mut body = Vec::new();
    for v in 0..24 { body.extend([ld(REGION_STATE), v, v, 1, NONE]); }
    body.extend([OP_CONST, 24, 0, OP_CONST, 25, 1, OP_SET, 0, 24]);
    let mut outer = Vec::new();
    loop_body(&mut outer, 4, &[OP_GET, 26, 0, OP_BIN | BIN_ADD_I << SUB_SHIFT, 27, 26, 25,
        OP_SET, 0, 27, OP_CONTINUE, 1]);
    outer.extend([OP_BREAK, 0]);
    loop_body(&mut body, 3, &outer);
    loop_body(&mut body, 0, &[OP_GET, 28, 0, OP_BIN | BIN_MUL_I << SUB_SHIFT, 29, 28, 25, OP_SET, 0, 29]);
    let mut outer = Vec::new(); loop_body(&mut outer, 3, &[OP_BREAK, 1]);
    outer.extend([OP_SET, 0, 25]); loop_body(&mut body, 2, &outer);
    body.extend([OP_GET, 30, 0, st(REGION_STATE), 24, 1, NONE, 30]);
    for v in 0..24 { body.extend([st(REGION_STATE), 26 + v, 1, NONE, v]); }
    let initial: Vec<_> = (0u32..50).map(|i| 0x12345678u32.wrapping_add(i << 16)).collect();
    let words = fixture(&body, &[TY_I32; 31], &[TY_I32], 0, &initial, &[]);
    let m = module::load(&words, &PROFILE).unwrap();
    let built = build_module(&m, &Options::default()).unwrap();
    assert!(built.entries[0].slot_bytes > 64, "simultaneously live values must exercise absolute slots");
    assert_eq!(built.image.symbols.mul32, 0, "a zero-iteration loop links no helper");
    let (mut cpu, _) = booted(&built.image);
    compare_entry(&mut cpu, &built.image, &m, 0, &initial, &mut vec![0; m.scratch_words(0)]);
    assert_eq!(state(&cpu, &built.image)[24], 3, "continue/break depths count loops; cap zero has no body");
}

#[test]
fn shared_functions_are_refused_until_calls_are_emitted() {
    let profile = fab_elec_mcu_rt::Profile { call_ops: Some(0), ram_words: 16_384, ..PROFILE };
    let c = compile(include_str!("../../lang/tests/support/shared.splash"), &profile, &[("P0", 0), ("P1", 1)], &[]).unwrap();
    let m = module::load(&c.module, &profile).unwrap();
    assert!(!m.funcs().is_empty());
    let error = build_module(&m, &Options::default()).err().expect("AVR must refuse calls");
    assert!(error.message.contains(fab_elec_mcu_emit_avr::FUNCTIONS_UNSUPPORTED));
    assert_eq!(chip::profile(16_000_000).call_ops, None);
}

fn inline_source(batch: &[String]) -> String {
    let mut source = String::from("var a: i32 = 0\nvar b: i32 = 0\nvar p = false\nvar q = false\nvar data = [int(11), int(22), int(33), int(44)]\n");
    for (name, declaration) in [
        ("byte_data", "var byte_data = [int(0), int(127), int(128), int(255)]\n"),
        ("half_data", "var half_data = [int(0), int(256), int(32768), int(65535)]\n"),
        ("wide_data", "var wide_data = [int(0), int(1), int(65536), int(0 - 1)]\n"),
        ("mixed_data", "var mixed_data = [int(0), int(1), int(2), int(65536)]\n"),
        ("byte_word", "var byte_word: i32 = 255\n"),
        ("half_word", "var half_word: i32 = 65535\n"),
        ("wide_word", "var wide_word: i32 = -1\n"),
    ] {
        if batch.iter().any(|body| body.contains(name)) { source.push_str(declaration); }
    }
    for len in [64, 65, 128] {
        if batch.iter().any(|body| body.contains(&format!("scale{len}"))) {
            source.push_str(&format!("var scale{len} = [int(3); {len}]\n"));
        }
    }
    for len in [4, 64, 65, 257] {
        if batch.iter().any(|body| body.contains(&format!("table{len}"))) {
            let values: Vec<_> = (0..len).map(|n| format!("int({})", n * 257)).collect();
            source.push_str(&format!("let table{len} = [{}]\n", values.join(", ")));
        }
    }
    // A real wrapping update makes both input words full-range on the reference.
    // Host-injected edge inputs below therefore stay inside the proved intervals.
    source.push_str("let inputs = every(ms(1000), fn() { a = a + 1 b = b + 1 p = !p q = !q })\n");
    for (k, body) in batch.iter().enumerate() { source.push_str(&format!("let op{k} = every(ms(100), fn() {{ {body} }})\n")); }
    source
}

#[path = "support/ranges.rs"]
mod ranges;

#[path = "support/widths.rs"]
mod widths;

// Every successfully built module in this suite also runs on the observed
// reference, from its declared initial state. This includes format fixtures
// and the helper/op sweeps without duplicating their program generators.
fn build_module(m: &Module<'_>, options: &Options) -> Result<fab_elec_mcu_emit_avr::Built, fab_elec_mcu_emit_avr::Refusal> {
    let built = fab_elec_mcu_emit_avr::build_module(m, options)?;
    let thread = std::thread::current();
    let program = thread.name().unwrap_or("emit-fixture");
    ranges::check(m, &built.ranges, program);
    Ok(built)
}

fn range_programs() -> Vec<(String, String)> {
    let mut programs = vec![
        ("drum16".to_owned(), include_str!("../../../fixtures/drum16.fw.splash").to_owned()),
        ("ledavr".to_owned(), include_str!("../../../fixtures/ledavr.fw.splash").to_owned()),
        ("blink".to_owned(), blink::BLINK.to_owned()),
        ("echo".to_owned(), ECHO_SOURCE.to_owned()),
    ];
    for seed in [1, 7, 19, 37, 71, 101, 257, 509, 1021, 65537, 0x55555555, 0xdeadbeef] {
        programs.push((format!("generated-{seed}"), generated(seed)));
    }
    for (n, batch) in inline_bodies().chunks(8).enumerate() {
        programs.push((format!("inline-{n}"), inline_source(batch)));
    }
    programs
}

#[test]
fn state_and_index_widths_reduce_corpus_accesses() {
    use fab_elec_mcu_avr::insn::{decode, Insn, Ptr};
    use fab_elec_mcu_rt::format::*;
    let instructions = |built: &fab_elec_mcu_emit_avr::Built, span: &fab_elec_mcu_emit_avr::emit::OpSpan| {
        let mut pc = span.start;
        let mut insns = Vec::new();
        while pc < span.end {
            let (insn, len) = decode(&built.image.words[pc as usize..]).unwrap();
            insns.push(insn); pc += len as u32;
        }
        insns
    };
    for (name, source) in [
        ("drum16", include_str!("../../../fixtures/drum16.fw.splash")),
        ("ledavr", include_str!("../../../fixtures/ledavr.fw.splash")),
    ] {
        let c = compiled(source);
        let m = module::load(&c.module, &PROFILE).unwrap();
        let narrow = build_module(&m, &Options::default()).unwrap();
        let wide = build_module(&m, &Options { full_ranges: true, ..Options::default() }).unwrap();
        let (mut stores, mut smaller_store, mut smaller_index) = (0, false, false);
        for w in &wide.map.ops {
            let code = &m.code()[w.code as usize..];
            let op = code[0] & OP_MASK;
            if !matches!(op, OP_LOAD | OP_STORE) { continue; }
            let n = narrow.map.ops.iter().find(|n| n.entry == w.entry && n.code == w.code).expect("the same live access");
            let (ni, wi) = (instructions(&narrow, n), instructions(&wide, w));
            if op == OP_STORE && code[0] >> SUB_SHIFT == REGION_STATE {
                let bytes = |insns: &[Insn]| insns.iter().filter(|i| matches!(i, Insn::St(Ptr::ZInc, _))).count();
                let (nb, wb) = (bytes(&ni), bytes(&wi));
                assert_eq!(wb, std::mem::size_of::<i32>(), "{name}, store {}: full-range state writes a whole word", w.code);
                assert!(nb > 0 && nb <= wb, "{name}, store {}: proven ranges never widen a state write", w.code);
                stores += 1;
                smaller_store |= nb < wb;
            }
            let (extent, off) = if op == OP_LOAD { (code[3], code[4]) } else { (code[2], code[3]) };
            if off != NONE {
                let r = narrow.ranges.entries[n.entry].values[off as usize];
                if r.lo != r.hi && r.lo >= 0 && i64::from(r.hi) < i64::from(extent)
                    && ni.contains(&Insn::Adc(31, 2)) && !wi.contains(&Insn::Adc(31, 2)) {
                    assert!(n.end - n.start < w.end - w.start,
                        "{name}, access {}: a proven byte offset reduces its emitted span, interval {r:?}", w.code);
                    smaller_index = true;
                }
            }
        }
        assert!(stores > 0, "{name}: the relation covers state writes");
        if name == "drum16" {
            assert!(smaller_store, "the drum has a state store narrower than its full-range counterpart");
            assert!(smaller_index, "the drum has an indexed access narrower than its full-range counterpart");
        }
    }
}

#[test]
fn wide_and_narrow_match_at_every_entry() {
    for (name, source) in range_programs() {
        let c = compiled(&source);
        let m = module::load(&c.module, &PROFILE).unwrap();
        let narrow = build_module(&m, &Options { watch: watch::Level::Ports, ..Options::default() })
            .unwrap_or_else(|e| panic!("{name}: {e}\n{source}"));
        let wide = build_module(&m, &Options { watch: watch::Level::Ports, full_ranges: true, ..Options::default() })
            .unwrap_or_else(|e| panic!("{name}, full ranges: {e}\n{source}"));
        let (mut nc, _) = booted(&narrow.image);
        let (mut wc, _) = booted(&wide.image);
        let setup = m.entry(fab_elec_mcu_rt::SETUP);
        // Setup once, then all handlers in module order. Repeated rounds cross
        // the generated modulo-16 counter's wrap and vary digital/ADC/RX input.
        let sequence = setup.into_iter().chain((0..24).flat_map(|_| (0..m.entries().len()).filter(|k| Some(*k) != setup)));
        for (phase, k) in sequence.enumerate() {
            widths::inputs(&mut nc, &narrow.image, &m, phase);
            widths::inputs(&mut wc, &wide.image, &m, phase);
            let ne = widths::entry(&mut nc, &narrow, k);
            let we = widths::entry(&mut wc, &wide, k);
            let ns = state(&nc, &narrow.image);
            let ws = state(&wc, &wide.image);
            for (word, (&n, &w)) in ns.iter().zip(&ws).enumerate() {
                assert_eq!(n, w,
                    "FALSIFIER 1: {name}, entry {k}, phase {phase}, state word {word}, interval {:?}\nentry ranges: {:?}\n{source}",
                    narrow.ranges.state[word], narrow.ranges.entries[k]);
            }
            assert_eq!(ne, we, "FALSIFIER 1: {name}, entry {k}, phase {phase}, effects\nentry ranges: {:?}\n{source}", narrow.ranges.entries[k]);
        }
    }
}

#[test]
fn ranges_hold_on_reference_and_census() {
    for (name, source) in range_programs() {
        let c = compiled(&source);
        let m = module::load(&c.module, &PROFILE).unwrap();
        let start = std::time::Instant::now();
        let r = fab_elec_mcu_emit_avr::range::analyse(&m);
        let elapsed = start.elapsed();
        ranges::check(&m, &r, &source);
        ranges::print_census(&name, &m, &r);
        if name == "drum16" { println!("RANGE_TIME drum16 {elapsed:?}"); }
        // Exercise the device's actual Io and scheduler too, with both pin
        // levels and ADC endpoints. No state pokes bypass state_init/stores.
        for high in [false, true] {
            let mut model = Model { input_high: Some(high),
                analog: (14..22).map(|pin| (pin, if high { 1023 } else { 0 })).collect(), ..Model::default() };
            let observer = ranges::Check::new(&m, &r, &source);
            let mut device = Device::with_observer(module::load(&c.module, &PROFILE).unwrap(), &PROFILE, observer);
            device.boot(&mut model);
            for link in 0..m.serial().len() {
                device.rx(0, link as u32, if high { 255 } else { 0 }, &mut model);
            }
            for _ in 0..24 { if device.step(&mut model).is_none() { break; } }
            assert!(device.observer().values > 0 || m.entries().iter().all(|e| e.n_vals == 0));
        }
    }
}

#[test]
fn range_trips_initial_zero_and_memory_extents() {
    use fab_elec_mcu_emit_avr::range::{analyse, Interval};
    use fab_elec_mcu_rt::format::*;
    // The inner cap is a lifetime bound only with a reset in each outer
    // iteration. Both programs run on the reference; no state injection.
    for (reset, captured) in [(false, false), (true, false), (true, true)] {
        let mut body = vec![OP_CONST, 0, 0, OP_CONST, 1, 1, OP_SET, 0, 0];
        let mut outer = Vec::new();
        if captured { outer.extend([OP_GET, 2, 0]); }
        if reset { outer.extend([OP_SET, 0, 0]); }
        let mut inner = Vec::new();
        if !captured { inner.extend([OP_GET, 2, 0]); }
        inner.extend([OP_BIN | BIN_ADD_I << SUB_SHIFT, 3, 2, 1, OP_SET, 0, 3]);
        loop_body(&mut outer, 4, &inner);
        loop_body(&mut body, if captured { 8 } else { 3 }, &outer);
        body.extend([OP_GET, 4, 0, OP_STORE | REGION_STATE << SUB_SHIFT, 0, 1, NONE, 4]);
        let words = fixture(&body, &[TY_I32; 5], &[TY_I32], 0, &[0], &[]);
        let m = module::load(&words, &PROFILE).unwrap();
        let r = analyse(&m);
        ranges::check(&m, &r, "nested counter with/without reset");
        if reset && !captured { assert_eq!(r.entries[0].vars[0], Interval { lo: 0, hi: 4 }, "trip rule retains the inner cap"); }
        else { assert!(r.entries[0].vars[0].contains(if captured { 8 } else { 12 }), "inner cap alone cannot bound repeated invocations or captured pre-reset gets"); }
    }
    let body = [
        OP_GET, 0, 0,
        OP_CONST, 1, 7,
        OP_SET, 0, 1,
        OP_STORE | REGION_STATE << SUB_SHIFT, 0, 1, NONE, 0,
        OP_CONST, 2, u32::MAX,
        OP_LOAD | REGION_SHARED << SUB_SHIFT, 3, 0, 3, 2,
        OP_STORE | REGION_STATE << SUB_SHIFT, 1, 1, NONE, 3,
        OP_PORT, u32::from(fab_elec_mcu_rt::ports::CLOCK_US), 0, 0, 1, 4,
        OP_LOAD | REGION_SHARED << SUB_SHIFT, 5, 0, 3, 4,
        OP_STORE | REGION_STATE << SUB_SHIFT, 2, 2, 4, 5,
        OP_LOAD | REGION_STATE << SUB_SHIFT, 6, 2, 2, 4,
        OP_STORE | REGION_STATE << SUB_SHIFT, 4, 1, NONE, 6,
        OP_LOAD | REGION_FRAME << SUB_SHIFT, 7, 0, 1, NONE,
        OP_STORE | REGION_STATE << SUB_SHIFT, 5, 1, NONE, 7,
        OP_WRAP, 8, 4, 16,
        OP_STORE | REGION_STATE << SUB_SHIFT, 6, 1, NONE, 8,
    ];
    let mut words = fixture(&body, &[TY_I32; 9], &[TY_I32], 1, &[0; 7], &[1, 127, 255]);
    let layout = words[HEADER_WORDS + 1] as usize;
    words[layout + L_SITES] = 1; // the clock read's site
    let m = module::load(&words, &PROFILE).unwrap();
    let r = analyse(&m);
    ranges::check(&m, &r, "initial zero, constant/computed memory extents and frame");
    assert!(r.entries[0].vars[0].contains(0) && r.entries[0].vars[0].contains(7), "A3 includes the get before set");
    assert_eq!(r.entries[0].values[3], Interval::point(255), "negative constant index clamps as u32 to last word");
    assert_eq!(r.entries[0].values[5], Interval { lo: 1, hi: 255 }, "computed shared load joins the whole table");
    for word in 2..=4 { assert!(r.state[word].contains(255), "computed store and load join every possible word"); }
    assert_eq!(r.entries[0].values[7], Interval::FULL, "frame loads stay full");
    assert_eq!(r.entries[0].values[8], Interval { lo: 0, hi: 15 }, "wrap bounds do not depend on its operand");
}
