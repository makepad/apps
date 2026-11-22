use std::collections::BTreeMap;

use fab_elec_mcu_emit32::xtensa::{assemble, assemble_relaxed, ccz, reg::*, Asm};
use fab_elec_mcu_emit_s3::radio::{helpers::{helpers, HELPERS}, window_block};
use fab_elec_mcu_emu::bus::{Memory, Perm};
use fab_elec_mcu_island::GlueRegion;
use fab_elec_mcu_s3::config::s3_config;
use fab_elec_mcu_xtensa::sim::Cpu;

const VECTORS: u32 = 0x4000_0000;
const CODE: u32 = 0x4001_0000;
const RAM: u32 = 0x3ffb_0000;
const DATA: u32 = RAM + 0x2000;
const DATA_LEN: usize = 288;
const LENGTHS: [usize; 12] = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 31, 64];
type Buffer = [u8; DATA_LEN];

struct Harness {
    cpu: Cpu,
    memory: Memory,
    mailbox: usize,
    data: usize,
    entries: BTreeMap<String, u32>,
    stack_words: usize,
    step_limit: usize,
}

impl Harness {
    fn new(max_helper_steps: usize) -> Self {
        Self::with_stack_words(max_helper_steps, 0)
    }

    fn with_stack_words(max_helper_steps: usize, stack_words: usize) -> Self {
        // a2 points at the mailbox in the driver's initial live window.
        // Word 0 selects the callee, 1..=6 are its arguments, 7..=8 its result.
        let mut code = assemble(&mut Asm::new(1, A8), |a| {
            a.base.label_assign(0);
            a.l32i_n(A8, A2, 0);
            for i in 0..6 {
                a.l32i_n(A10 + i, A2, 4 + 4 * u32::from(i));
            }
            for i in 0..stack_words as u32 {
                a.l32i_n(A9, A2, 36 + 4 * i);
                a.s32i_n(A9, A1, 4 * i);
            }
            a.callx8(A8);
            a.s32i_n(A10, A2, 28);
            a.s32i_n(A11, A2, 32);
            a.j_label(0);
        });
        let mut entries = BTreeMap::new();
        let objects = helpers();
        assert_eq!(objects.len(), HELPERS.len(), "helper import count");
        for (name, glue) in HELPERS.into_iter().zip(objects) {
            assert_eq!(glue.name, name, "{name} import order");
            assert_eq!(glue.region, GlueRegion::Text, "{name} code region");
            assert_eq!(glue.align, 4, "{name} entry alignment");
            assert_eq!(glue.entry, 0, "{name} starts at byte zero");
            assert!(glue.relocations.is_empty(), "{name} carries relocations");
            code.resize(code.len().next_multiple_of(4), 0);
            entries.insert(glue.name, CODE + code.len() as u32);
            code.extend_from_slice(&glue.bytes);
        }
        let mut memory = Memory::new();
        let rx = Perm { read: true, write: false, exec: true };
        for (address, bytes) in [(VECTORS, window_block()), (CODE, code)] {
            let store = memory.store(bytes.len());
            memory.bytes_mut(store).copy_from_slice(&bytes);
            memory.map(address, store, 0, bytes.len(), rx);
        }
        let rw = Perm { read: true, write: true, exec: false };
        let mailbox = memory.store(0x1000); // stack grows down from its far end
        memory.map(RAM, mailbox, 0, 0x1000, rw);
        let data = memory.store(DATA_LEN);
        memory.map(DATA, data, 0, DATA_LEN, rw);
        let mut cpu = Cpu::new(s3_config(), CODE);
        cpu.vecbase = VECTORS;
        cpu.ps = (1 << 18) | (1 << 5); // WOE, user mode, interrupts unmasked
        cpu.ar[1] = RAM + 0x1000 - (4 * stack_words as u32).next_multiple_of(16);
        cpu.ar[2] = RAM;
        // Seven loads, CALLX8, two result stores and the jump back, plus a
        // load/store pair per outgoing stack word.
        let step_limit = 11 + 2 * stack_words + max_helper_steps;
        Self { cpu, memory, mailbox, data, entries, stack_words, step_limit }
    }

    fn call(&mut self, name: &str, args: &[u32]) -> [u32; 2] {
        assert!(args.len() <= 6 + self.stack_words, "{name}({args:#x?}) exceeds driver frame");
        let mailbox = self.memory.bytes_mut(self.mailbox);
        mailbox[..4].copy_from_slice(&self.entries[name].to_le_bytes());
        mailbox[4..28].fill(0xa5);
        for (word, arg) in mailbox[4..28].chunks_exact_mut(4).zip(args) {
            word.copy_from_slice(&arg.to_le_bytes());
        }
        let stack = &mut mailbox[36..36 + 4 * self.stack_words];
        stack.fill(0xa5);
        for (word, arg) in stack.chunks_exact_mut(4).zip(args.iter().skip(6)) {
            word.copy_from_slice(&arg.to_le_bytes());
        }
        // A broken guest loop should fail this case with its inputs.
        for _ in 0..self.step_limit {
            self.cpu.step(&mut self.memory).unwrap_or_else(|stop| {
                panic!("{name}({args:#x?}) stopped at {:#x}: {stop:?}", self.cpu.pc);
            });
            if self.cpu.pc == CODE {
                let mailbox = self.memory.bytes(self.mailbox);
                return [
                    u32::from_le_bytes(mailbox[28..32].try_into().unwrap()),
                    u32::from_le_bytes(mailbox[32..36].try_into().unwrap()),
                ];
            }
        }
        panic!("{name}({args:#x?}) did not return, pc={:#x}", self.cpu.pc);
    }

    // Enter eleven CALL4 frames before starting the arithmetic driver. Its
    // CALL8 puts the leaf at WindowBase 13: a12..a15 then overlap frame 0.
    fn enter_window_chain(&mut self) {
        let base = CODE + 0x10000;
        let code = assemble_relaxed(&mut Asm::new(1, A8), |a| {
            a.entry(A1, 32);
            a.bccz_reg_label(ccz::EQ, A3, 0);
            a.addi(A7, A3, -1); // child's a3
            a.mov_n(A8, A4);    // child's a4 points back to ENTRY
            a.callx4(A4);
            a.retw_n();
            a.base.label_assign(0);
        });
        let store = self.memory.store(code.len());
        self.memory.bytes_mut(store).copy_from_slice(&code);
        self.memory.map(base, store, 0, code.len(), Perm { read: true, write: false, exec: true });
        self.cpu.pc = base + 3; // bootstrap already owns window 0
        self.cpu.ar[3] = 11;
        self.cpu.ar[4] = base;
        for _ in 0..11 * 5 + 1 {
            self.cpu.step(&mut self.memory).expect("CALL4 window chain");
        }
        assert_eq!(self.cpu.pc, base + code.len() as u32, "window chain reached the driver");
        assert_eq!(self.cpu.window_start, (1 << 12) - 1, "frames 0 through 11 are live");
        self.cpu.ar[2] = RAM;
        self.cpu.pc = CODE;
    }

    fn check(&mut self, name: &str, args: &[u32], input: &Buffer, expected: &Buffer, result: u32) {
        self.memory.bytes_mut(self.data).copy_from_slice(input);
        let [actual, _] = self.call(name, args);
        let compare = matches!(name, "memcmp" | "strncmp");
        let normalize = |x: u32| if compare { (x as i32).signum() as u32 } else { x };
        assert_eq!(normalize(actual), normalize(result), "{name}({args:#x?}), input={input:02x?}");
        // Checking the entire mapping covers both guards, every source byte
        // outside a move's destination, and any unwritten suffix on error.
        let output = self.memory.bytes(self.data);
        if let Some(at) = output.iter().zip(expected).position(|(a, b)| a != b) {
            panic!("{name}({args:#x?}) at byte {at}: got {:02x}, expected {:02x}, input={input:02x?}",
                output[at], expected[at]);
        }
    }
}

fn buffer() -> Buffer {
    std::array::from_fn(|i| (i as u8).wrapping_mul(73).wrapping_add(19))
}

fn ptr(offset: usize) -> u32 { DATA + offset as u32 }

fn check_copy(h: &mut Harness, name: &str, input: &Buffer, dst: usize, src: usize, n: usize) {
    // Read the original bytes even when a move overlaps in either direction.
    let mut expected = *input;
    expected[dst..dst + n].copy_from_slice(&input[src..src + n]);
    h.check(name, &[ptr(dst), ptr(src), n as u32], input, &expected, ptr(dst));
}

fn check_fill(h: &mut Harness, input: &Buffer, dst: usize, c: u32, n: usize) {
    let mut expected = *input;
    expected[dst..dst + n].fill(c as u8);
    h.check("memset", &[ptr(dst), c, n as u32], input, &expected, ptr(dst));
}

fn check_transfers(h: &mut Harness) {
    let input = buffer();
    for n in LENGTHS {
        for d in 0..4 {
            let dst = 8 + d;
            for c in [0, 0xffff_ff80, 0x1234_56ff] {
                check_fill(h, &input, dst, c, n);
            }
            for s in 0..4 {
                let src = 144 + s;
                check_copy(h, "memcpy", &input, dst, src, n);
                for (to, from) in [(dst, src), (src, dst), (140 + d, src), (148 + d, src)] {
                    check_copy(h, "memmove", &input, to, from, n);
                }
            }
            h.check("memmove", &[ptr(dst), ptr(dst), n as u32], &input, &input, ptr(dst));
        }
    }
}

fn check_pair(h: &mut Harness, input: &Buffer, left: usize, right: usize, n: usize) {
    for name in ["memcmp", "strncmp"] {
        let mut diff = 0;
        for i in 0..n {
            let (a, b) = (input[left + i], input[right + i]);
            diff = i32::from(a) - i32::from(b);
            if diff != 0 || (name == "strncmp" && a == 0) { break; }
        }
        h.check(name, &[ptr(left), ptr(right), n as u32], input, input, diff as u32);
    }
}

fn check_comparisons(h: &mut Harness) {
    for n in LENGTHS {
        for left in 8..12 {
            for right in 144..148 {
                let mut input = buffer();
                for i in 0..n {
                    input[left + i] |= 0x80;
                    input[right + i] = input[left + i];
                }
                input[left + n] = 0;
                input[right + n] = 255;
                check_pair(h, &input, left, right, n);
                if n != 0 {
                    input[left + n - 1] = 0x7f;
                    input[right + n - 1] = 0x80;
                    check_pair(h, &input, left, right, n);
                    check_pair(h, &input, right, left, n);
                    input[right] = 0;
                    check_pair(h, &input, left, right, n);
                    input[left] = 0;
                    check_pair(h, &input, left, right, n);
                }
            }
        }
    }
}

fn check_string(h: &mut Harness, input: &Buffer, src: usize, dst: usize, n: usize) {
    let len = input[src..].iter().position(|&c| c == 0).unwrap();
    let copied = len.min(n);
    h.check("strlen", &[ptr(src)], input, input, len as u32);
    h.check("strnlen", &[ptr(src), n as u32], input, input, copied as u32);
    let mut expected = *input;
    expected[dst..=dst + len].copy_from_slice(&input[src..=src + len]);
    h.check("strcpy", &[ptr(dst), ptr(src)], input, &expected, ptr(dst));
    expected = *input;
    expected[dst..dst + copied].copy_from_slice(&input[src..src + copied]);
    expected[dst + copied..dst + n].fill(0);
    h.check("strncpy", &[ptr(dst), ptr(src), n as u32], input, &expected, ptr(dst));
}

fn check_string_bounds(h: &mut Harness) {
    for len in LENGTHS {
        for src in 144..148 {
            let mut input = buffer();
            for byte in &mut input[src..src + len] { *byte |= 0x80; }
            input[src + len] = 0;
            for n in [len.saturating_sub(1), len, len + 3] {
                for dst in 8..12 { check_string(h, &input, src, dst, n); }
            }
        }
    }
}

fn nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        _ => None,
    }
}

fn check_hex(h: &mut Harness, input: &Buffer, src: usize, dst: usize, n: usize) {
    let mut expected = *input;
    let mut result = 0;
    for i in 0..n {
        let Some(hi) = nibble(input[src + 2 * i]) else { result = u32::MAX; break };
        let Some(lo) = nibble(input[src + 2 * i + 1]) else { result = u32::MAX; break };
        expected[dst + i] = (hi << 4) | lo;
    }
    h.check("hexstr2bin", &[ptr(src), ptr(dst), n as u32], input, &expected, result);
}

fn check_hex_strings(h: &mut Harness) {
    let digits = b"0123456789aBcDeFAbCdEf";
    for n in LENGTHS {
        for src in 144..148 {
            let mut input = buffer();
            for i in 0..2 * n { input[src + i] = digits[(i + src) % digits.len()]; }
            for dst in 8..12 { check_hex(h, &input, src, dst, n); }
        }
    }
    // Vary both nibbles through every byte value after one good pair. An
    // invalid digit must preserve the completed byte and the untouched suffix.
    for pos in [2, 3] {
        for byte in 0..=255 {
            let mut input = buffer();
            input[144..150].copy_from_slice(b"2aF01c");
            input[144 + pos] = byte;
            check_hex(h, &input, 144, 8, 3);
        }
    }
}

fn check_read_boundaries(h: &mut Harness) {
    // The address following the data mapping is inaccessible. A speculative
    // load after a bound, terminator or invalid high nibble faults here.
    let mut input = buffer();
    let end = ptr(DATA_LEN);
    for name in ["memcpy", "memmove", "memset", "strncpy"] {
        h.check(name, &[end, end, 0], &input, &input, end);
    }
    check_pair(h, &input, DATA_LEN, DATA_LEN, 0);
    h.check("strnlen", &[end, 0], &input, &input, 0);
    check_hex(h, &input, DATA_LEN, DATA_LEN, 0);

    let last = DATA_LEN - 1;
    input[last] = 0xff;
    h.check("strnlen", &[ptr(last), 1], &input, &input, 1);
    check_hex(h, &input, last, 8, 1);
    input[last] = 0;
    input[8] = 0;
    h.check("strlen", &[ptr(last)], &input, &input, 0);
    h.check("strncmp", &[ptr(last), ptr(8), 64], &input, &input, 0);
    let mut expected = input;
    expected[16] = 0;
    h.check("strcpy", &[ptr(16), ptr(last)], &input, &expected, ptr(16));
    expected[16..80].fill(0);
    h.check("strncpy", &[ptr(16), ptr(last), 64], &input, &expected, ptr(16));
}

fn random(seed: &mut u32) -> u32 {
    *seed ^= *seed << 13;
    *seed ^= *seed >> 17;
    *seed ^= *seed << 5;
    *seed
}

fn check_random_inputs(h: &mut Harness) {
    let mut seed = 0x7183_cae5;
    for _ in 0..200 {
        let mut input: Buffer = std::array::from_fn(|_| random(&mut seed) as u8);
        let dst = 8 + (random(&mut seed) % 8) as usize;
        let src = 144 + (random(&mut seed) % 8) as usize;
        let n = (random(&mut seed) % 65) as usize;
        check_copy(h, "memcpy", &input, dst, src, n);
        check_copy(h, "memmove", &input, src - 32 + (random(&mut seed) % 65) as usize, src, n);
        check_fill(h, &input, dst, random(&mut seed), n);

        let prefix = random(&mut seed) as usize % (n + 1);
        input.copy_within(src..src + prefix, dst);
        check_pair(h, &input, dst, src, n);
        input[src + (random(&mut seed) % 65) as usize] = 0;
        check_string(h, &input, src, dst, n);

        let digits = b"0123456789abcdefABCDEF";
        for byte in &mut input[src..src + 2 * n] {
            *byte = digits[random(&mut seed) as usize % digits.len()];
        }
        if n != 0 && random(&mut seed) & 1 != 0 {
            input[src + random(&mut seed) as usize % (2 * n)] = random(&mut seed) as u8;
        }
        check_hex(h, &input, src, dst, n);
    }
}

#[test]
fn memory_and_string_helpers_match_their_c_definitions() {
    // Longest case: hexstr2bin with 64 alphabetic pairs. Three instructions
    // before the loop, seven per nibble plus seven per pair, and two to return.
    let mut h = Harness::new(3 + 64 * 21 + 2);
    check_transfers(&mut h);
    check_comparisons(&mut h);
    check_string_bounds(&mut h);
    check_hex_strings(&mut h);
    check_read_boundaries(&mut h);
    check_random_inputs(&mut h);
}

fn check_division(h: &mut Harness, x: u64, y: u64) {
    let args = [x as u32, (x >> 32) as u32, y as u32, (y >> 32) as u32];
    let (sx, sy) = (x as i64, y as i64);
    let (sq, sr, uq, ur) = if y == 0 {
        (u64::MAX, x, u64::MAX, x)
    } else {
        (sx.wrapping_div(sy) as u64, sx.wrapping_rem(sy) as u64, x / y, x % y)
    };
    for (name, expected) in [
        ("__divdi3", sq), ("__moddi3", sr), ("__udivdi3", uq), ("__umoddi3", ur),
    ] {
        let [lo, hi] = h.call(name, &args);
        let actual = (u64::from(hi) << 32) | u64::from(lo);
        assert_eq!(actual, expected, "{name}({x:#018x}, {y:#018x})");
    }
}

fn check_bswap(h: &mut Harness, x: u32) {
    let [actual, _] = h.call("__bswapsi2", &[x]);
    assert_eq!(actual, x.swap_bytes(), "__bswapsi2({x:#010x})");
}

fn random_u64(seed: &mut u32) -> u64 {
    (u64::from(random(seed)) << 32) | u64::from(random(seed))
}

#[test]
fn integer_helpers_match_wrapping_rust_ops() {
    // Including ENTRY, zero and sign checks, both negations, high-word
    // division and counter setup takes at most 22 instructions. In each
    // round: five to shift, three compares, one borrow, two subtracts, one
    // quotient bit and two to count/branch. At exit allow two remainder
    // moves, five for the sign and RETW.N. The driver is counted separately.
    let mut h = Harness::new(22 + 32 * 14 + 8);
    let word_max = u64::from(u32::MAX);
    let edges = [0, 1, 2, word_max, 1 << 32, i64::MIN as u64, i64::MAX as u64, u64::MAX];
    for x in edges {
        for y in edges { check_division(&mut h, x, y); }
    }
    for (x, y) in [
        (0x4_0000_0000, 0x1_ffff_ffff), // low-word borrow in the remainder
        (0xffff_ffff_0000_0001, 7),     // high word yields both quotient and remainder
        (0x7654_3210_fedc_ba98, 0xffff_fffe_0000_0000), // negative two-word divisor
    ] {
        check_division(&mut h, x, y);
    }
    for x in [0, u32::MAX, 0x0123_4567, 0x89ab_cdef] { check_bswap(&mut h, x); }
    for bit in 0..64 {
        let power = 1u64 << bit;
        for x in [power - 1, power, power + 1] {
            check_division(&mut h, x, power);
            if bit < 32 { check_bswap(&mut h, x as u32); }
        }
        check_division(&mut h, u64::MAX, power + 1);
    }

    let mut seed = 0xb719_38a5;
    for i in 0..200 {
        let x = random_u64(&mut seed);
        let y = random_u64(&mut seed);
        let (dividend, divisor) = match i % 4 {
            0 => (x, y),
            1 => (x, y >> (1 + i % 63)), // vary the number of quotient bits
            2 => (x, y as i32 as i64 as u64),
            _ => (x & word_max, y & word_max),
        };
        check_division(&mut h, dividend, divisor);
        check_bswap(&mut h, x as u32);
    }
}

fn check_word_conversions(h: &mut Harness, bits: u32) {
    for (name, expected) in [
        ("__extendsfdf2", f32::from_bits(bits) as f64),
        ("__floatsidf", (bits as i32) as f64),
        ("__floatunsidf", bits as f64),
    ] {
        let [lo, hi] = h.call(name, &[bits]);
        let actual = (u64::from(hi) << 32) | u64::from(lo);
        if expected.is_nan() {
            assert!(f64::from_bits(actual).is_nan(), "{name}({bits:#010x}) returned {actual:#018x}");
        } else {
            assert_eq!(actual, expected.to_bits(), "{name}({bits:#010x})");
        }
    }
}

fn check_single_result(h: &mut Harness, name: &str, bits: u64, expected: f32) {
    let [actual, _] = h.call(name, &[bits as u32, (bits >> 32) as u32]);
    if expected.is_nan() {
        assert!(f32::from_bits(actual).is_nan(), "{name}({bits:#018x}) returned {actual:#010x}");
    } else {
        assert_eq!(actual, expected.to_bits(), "{name}({bits:#018x})");
    }
}

fn check_double_input(h: &mut Harness, bits: u64) {
    let x = f64::from_bits(bits);
    check_single_result(h, "__truncdfsf2", bits, x as f32);
    let [actual, _] = h.call("__fixdfsi", &[bits as u32, (bits >> 32) as u32]);
    assert_eq!(actual as i32, x as i32, "__fixdfsi({bits:#018x})");
}

fn check_double_neighbours(h: &mut Harness, positive: f64) {
    let bits = positive.to_bits();
    for magnitude in [bits - 1, bits, bits + 1] {
        check_double_input(h, magnitude);
        check_double_input(h, magnitude | (1 << 63));
    }
}

#[test]
fn float_conversions_match_the_host_bit_for_bit() {
    // The longest bound is u64 -> f32: ENTRY and at most seven setup
    // instructions, 20 to normalize (1 + 4*4 + 3), five to align the low
    // word and exponent, ten to round, MOV.N and RETW.N.
    let mut h = Harness::new(1 + 7 + 20 + 5 + 10 + 2);
    for magnitude in [
        0, 1, 0x007f_ffff, 0x0080_0000, 0x3f7f_ffff, 0x3f80_0000, 0x3f80_0001,
        0x7f7f_ffff, 0x7f80_0000, 0x7fc0_0001, 0x7f80_0001,
    ] {
        check_word_conversions(&mut h, magnitude);
        check_word_conversions(&mut h, magnitude | (1 << 31));
    }
    for magnitude in [
        0, 1, 0x000f_ffff_ffff_ffff, 0x0010_0000_0000_0000, f64::MAX.to_bits(),
        f64::INFINITY.to_bits(), 0x7ff8_0000_0000_0000, 0x7ff0_0000_0000_0001,
        0x7ff0_0001_0000_0000, // signalling payload entirely in the high word
    ] {
        check_double_input(&mut h, magnitude);
        check_double_input(&mut h, magnitude | (1 << 63));
    }
    for x in [1.0, 1.75, i32::MAX as f64, i32::MAX as f64 + 0.5, (1u64 << 31) as f64,
        (1u64 << 32) as f64, (1u64 << 63) as f64, u64::MAX as f64] {
        check_double_neighbours(&mut h, x);
        let word = (x as f32).to_bits();
        for bits in [word - 1, word, word + 1] {
            check_word_conversions(&mut h, bits);
            check_word_conversions(&mut h, bits | (1 << 31));
        }
    }

    // Exact midpoints between adjacent binary32 values. Include both
    // parities, zero/min-subnormal, and max-subnormal/min-normal.
    for lower in [0, 1, 2, 0x007f_ffff, 0x3f7f_ffff, 0x3f80_0000, 0x3f80_0001] {
        let lo = f64::from(f32::from_bits(lower));
        let hi = f64::from(f32::from_bits(lower + 1));
        let midpoint = (lo + hi) * 0.5;
        check_double_neighbours(&mut h, midpoint);
    }
    for bit in 0..23 {
        let lo = f64::from(f32::from_bits(1 << bit));
        let hi = f64::from(f32::from_bits((1 << bit) + 1));
        let midpoint = (lo + hi) * 0.5;
        check_double_neighbours(&mut h, midpoint);
        // The extra sticky bit comes from the high part of the significand
        // when aligning a subnormal; the low 21 bits of the input stay zero.
        check_double_input(&mut h, midpoint.to_bits() + (1 << 21));
    }
    let two_to_128 = f64::from_bits((1023u64 + 128) << 52);
    // The binary64 predecessor of this tie is the last to round to f32::MAX.
    check_double_neighbours(&mut h, (f64::from(f32::MAX) + two_to_128) * 0.5);

    for bit in 0..64 {
        let power = 1u64 << bit;
        for n in [power - 1, power, power + 1] {
            check_single_result(&mut h, "__floatundisf", n, n as f32);
            if bit < 32 { check_word_conversions(&mut h, n as u32); }
        }
        if bit >= 24 {
            let ulp = 1u64 << (bit - 23);
            for lower in [power, power + ulp] {
                let midpoint = lower + ulp / 2;
                for n in [midpoint - 1, midpoint, midpoint + 1] {
                    check_single_result(&mut h, "__floatundisf", n, n as f32);
                }
            }
        }
    }
    let last_midpoint = u64::MAX - ((1u64 << 39) - 1);
    for n in [last_midpoint - 1, last_midpoint, last_midpoint + 1, u64::MAX - 1, u64::MAX] {
        check_single_result(&mut h, "__floatundisf", n, n as f32);
    }
    check_word_conversions(&mut h, u32::MAX);

    let mut seed = 0x5a93_72e1;
    for _ in 0..200 {
        check_word_conversions(&mut h, random(&mut seed));
        check_double_input(&mut h, random_u64(&mut seed));
        let n = random_u64(&mut seed);
        check_single_result(&mut h, "__floatundisf", n, n as f32);
    }
}

fn check_double_op(h: &mut Harness, name: &str, x: f64, y: f64) {
    let expected = match name {
        "__adddf3" => x + y,
        "__subdf3" => x - y,
        "__muldf3" => x * y,
        "__divdf3" => x / y,
        _ => unreachable!(),
    };
    let (x, y) = (x.to_bits(), y.to_bits());
    let [lo, hi] = h.call(name, &[x as u32, (x >> 32) as u32, y as u32, (y >> 32) as u32]);
    let actual = (u64::from(hi) << 32) | u64::from(lo);
    if expected.is_nan() {
        assert!(f64::from_bits(actual).is_nan(), "{name}({x:#018x}, {y:#018x}) returned {actual:#018x}");
    } else {
        assert_eq!(actual, expected.to_bits(), "{name}({x:#018x}, {y:#018x})");
    }
}

fn check_double_ops(h: &mut Harness, x: u64, y: u64) {
    for name in ["__adddf3", "__subdf3", "__muldf3", "__divdf3"] {
        check_double_op(h, name, f64::from_bits(x), f64::from_bits(y));
    }
}

fn check_single_division(h: &mut Harness, x: f32, y: f32) {
    let expected = x / y;
    let (x, y) = (x.to_bits(), y.to_bits());
    let [actual, _] = h.call("__divsf3", &[x, y]);
    if expected.is_nan() {
        assert!(f32::from_bits(actual).is_nan(), "__divsf3({x:#010x}, {y:#010x}) returned {actual:#010x}");
    } else {
        assert_eq!(actual, expected.to_bits(), "__divsf3({x:#010x}, {y:#010x})");
    }
}

fn special_values(fraction_bits: u32, exponent_bits: u32) -> Vec<u64> {
    let unit = 1u64 << fraction_bits;
    let sign = 1u64 << (exponent_bits + fraction_bits);
    let infinity = sign - unit;
    let one = (sign >> 1) - unit;
    let mut values = Vec::new();
    for mag in [0, 1, unit - 1, unit, one - 2, one - 1, one, one + 1, infinity - 1, infinity] {
        values.extend([mag, sign | mag]);
    }
    values.extend([infinity | (unit >> 1), infinity | 1]); // quiet, signalling
    values
}

fn nearby_exponents(x: u64, y: u64, fraction_bits: u32, exponent_bits: u32, delta: i32) -> (u64, u64) {
    let all_ones = (1u64 << exponent_bits) - 1;
    let mask = all_ones << fraction_bits;
    let xe = ((x & mask) >> fraction_bits).clamp(1, all_ones - 1);
    let ye = (xe as i32 + delta).clamp(1, all_ones as i32 - 1) as u64;
    ((x & !mask) | (xe << fraction_bits), (y & !mask) | (ye << fraction_bits))
}

#[test]
fn float_arithmetic_matches_the_host_bit_for_bit() {
    // Longest path bound, binary64 division: ENTRY/sign 4, unpack 2*39,
    // finite dispatch 6, quotient setup 22, 53 rounds of at most 14,
    // guard/sticky transfer 8, underflow/round/return 38. One collision
    // adds a faulting attempt and the five OF4 instructions. call adds
    // the driver's 11 steps separately: 898 + 6 + 11 = 915.
    let mut h = Harness::new(4 + 2 * 39 + 6 + 22 + 53 * 14 + 8 + 38 + 6);
    h.enter_window_chain();
    check_double_op(&mut h, "__divdf3", 1.0, 3.0);
    assert_eq!(h.cpu.window_start & 1, 0, "__divdf3(1, 3) spilled the oldest window");
    // OF4 writes the bootstrap's a2 at the first child's SP - 8.
    assert_eq!(&h.memory.bytes(h.mailbox)[0xfd8..0xfdc], &RAM.to_le_bytes(),
        "__divdf3(1, 3) saved the caller's mailbox pointer through the real vector");

    let doubles = special_values(52, 11);
    for &x in &doubles {
        for &y in &doubles { check_double_ops(&mut h, x, y); }
    }
    let singles = special_values(23, 8);
    for &x in &singles {
        for &y in &singles {
            check_single_division(&mut h, f32::from_bits(x as u32), f32::from_bits(y as u32));
        }
    }

    let power = |exponent: i32| f64::from_bits(((exponent + 1023) as u64) << 52);
    let odd = f64::from_bits(1f64.to_bits() + 1);
    let prev = f64::from_bits(1f64.to_bits() - 1);
    let tiny = f64::from_bits(1);
    for (x, y) in [
        (1.0, -1.0), (-0.0, -0.0), (1.0, -prev), (odd, -1.0),
        (1.0, power(-52)), (1.0, power(-53)), (odd, power(-53)),
        (1.0, power(-54)), (1.0, power(-55)), (1.0, -power(-300)),
        (prev, power(-54)), (f64::MAX, power(970)),
        (f64::MIN_POSITIVE, -tiny), (f64::MIN_POSITIVE, -f64::from_bits((1 << 52) - 1)),
    ] {
        check_double_op(&mut h, "__adddf3", x, y);
        check_double_op(&mut h, "__subdf3", x, -y);
    }
    for (x, y) in [
        (odd, 1.5), (f64::from_bits(1f64.to_bits() + 3), 1.5), // ties of opposite parity
        (tiny, 0.5), (f64::MIN_POSITIVE, prev), (f64::MIN_POSITIVE, 0.75),
        (f64::MAX, odd), (tiny, f64::MAX),
    ] {
        check_double_op(&mut h, "__muldf3", x, y);
    }
    for (x, y) in [
        (odd, odd), (1.0, 3.0), (2.0, 3.0), (tiny, 2.0), (f64::from_bits(3), 2.0),
        (f64::from_bits((2 << 52) - 1), 2.0), // tie rounds into the smallest normal
        (f64::MIN_POSITIVE, 3.0), (f64::MAX, 0.5), (f64::MIN_POSITIVE, tiny),
    ] {
        check_double_op(&mut h, "__divdf3", x, y);
    }
    for (x, y) in [
        (3.0, 3.0), (1.0, 3.0), (2.0, 3.0), (f32::from_bits(1), 2.0), (f32::from_bits(3), 2.0),
        (f32::from_bits((2 << 23) - 1), 2.0), (f32::MIN_POSITIVE, 3.0),
        (f32::MAX, 0.5), (f32::MIN_POSITIVE, f32::from_bits(1)),
    ] {
        check_single_division(&mut h, x, y);
    }

    let mut seed = 0x9ebc_347d;
    for _ in 0..200 {
        let (x, y) = (random_u64(&mut seed), random_u64(&mut seed));
        check_double_ops(&mut h, x, y);
        let (x, y) = (random(&mut seed), random(&mut seed));
        check_single_division(&mut h, f32::from_bits(x), f32::from_bits(y));
    }
    for _ in 0..200 {
        let delta = (random(&mut seed) % 121) as i32 - 60;
        let (x, y) = nearby_exponents(random_u64(&mut seed), random_u64(&mut seed), 52, 11, delta);
        check_double_ops(&mut h, x, y);
        let (x, y) = nearby_exponents(u64::from(random(&mut seed)), u64::from(random(&mut seed)), 23, 8, delta);
        check_single_division(&mut h, f32::from_bits(x as u32), f32::from_bits(y as u32));
    }
}

// Work in bytes: %c can insert a NUL, and %s measures its width in bytes.
fn reference_sprintf(format: &[u8], args: &[u32], strings: &Buffer) -> Vec<u8> {
    let mut output = Vec::new();
    let mut words = args.iter();
    let mut i = 0;
    while i < format.len() {
        let start = i;
        i += 1;
        if format[start] != b'%' {
            output.push(format[start]);
            continue;
        }
        if format.get(i) == Some(&b'%') {
            output.push(b'%');
            i += 1;
            continue;
        }

        let (mut left, mut zero, mut supported) = (false, false, true);
        while let Some(&flag) = format.get(i) {
            match flag {
                b'-' => left = true,
                b'0' => zero = true,
                b'+' | b' ' | b'#' => supported = false,
                _ => break,
            }
            i += 1;
        }
        let mut width = 0;
        if format.get(i) == Some(&b'*') {
            supported = false;
            i += 1;
        } else {
            while let Some(&digit) = format.get(i).filter(|c| c.is_ascii_digit()) {
                width = width * 10 + usize::from(digit - b'0');
                i += 1;
            }
        }
        if format.get(i) == Some(&b'.') {
            supported = false;
            i += 1;
            if format.get(i) == Some(&b'*') { i += 1; }
            else { while format.get(i).is_some_and(u8::is_ascii_digit) { i += 1; } }
        }
        if format.get(i) == Some(&b'w') {
            let digits = i + 1 + usize::from(format.get(i + 1) == Some(&b'f'));
            if matches!(format.get(digits), Some(b'1'..=b'9')) {
                supported = false;
                i = digits + 1;
                while format.get(i).is_some_and(u8::is_ascii_digit) { i += 1; }
            }
        } else if let Some(&length) = format.get(i).filter(|c| b"hljztLHD".contains(c)) {
            i += 1;
            if matches!(length, b'l' | b'h' | b'D') && format.get(i) == Some(&length) {
                i += 1;
                supported = false;
            }
            supported &= length == b'l';
        }
        let conversion = format.get(i).copied();
        i += usize::from(conversion.is_some());
        let Some(c) = conversion.filter(|c| supported && b"diuxXcs".contains(c)) else {
            output.extend_from_slice(&format[start..i]);
            continue;
        };
        let &word = words.next().expect("missing reference sprintf argument");
        let value = match c {
            b'd' | b'i' => (word as i32).to_string().into_bytes(),
            b'u' => word.to_string().into_bytes(),
            b'x' => format!("{word:x}").into_bytes(),
            b'X' => format!("{word:X}").into_bytes(),
            b'c' => vec![word as u8],
            b's' if word == 0 => b"(null)".to_vec(),
            b's' => strings[(word - DATA) as usize..].iter().copied().take_while(|&b| b != 0).collect(),
            _ => unreachable!(),
        };
        let padding = width.saturating_sub(value.len());
        if left {
            output.extend_from_slice(&value);
            output.resize(output.len() + padding, b' ');
        } else if zero && !matches!(c, b'c' | b's') {
            let magnitude = if let Some(digits) = value.strip_prefix(b"-") {
                output.push(b'-');
                digits
            } else { &value };
            output.resize(output.len() + padding, b'0');
            output.extend_from_slice(magnitude);
        } else {
            output.resize(output.len() + padding, b' ');
            output.extend_from_slice(&value);
        }
    }
    assert!(words.next().is_none(), "unused sprintf reference argument for {format:?}, {args:#x?}");
    output
}

fn sprintf_cases() -> Vec<(String, Vec<u32>)> {
    let strings = [0, DATA, DATA + 4, DATA + 16];
    let flags = ["", "-", "0", "-0", "0-", "00--0"];
    let unsupported = [
        "%.2d", "%.s", "%.*s", "%*d", "%*.*X", "%+d", "% i", "%#08x", "%-+0#9.3ld",
        "%hd", "%hhu", "%lld", "%ju", "%zd", "%td", "%Lf", "%Hf", "%Df", "%DDf", "%w8d",
        "%wf32%", "%w", "%o", "%p", "%n", "%Q", "%4%",
    ];
    let mut cases = vec![
        (String::new(), vec![]),
        ("ESP_%02X%02X%02X".into(), vec![0x00, 0xa5, 0xff]),
        ("é%%%".into(), vec![]),
        ("%08l".into(), vec![]),
        ("%300u".into(), vec![1]),
        ("%u %u %u %u %#x %u %u %u %u%*s%".into(), (1..=8).collect()),
        ("%%[%-0000012ls]".repeat(8), vec![strings[3]; 8]),
    ];
    for bad in unsupported { cases.push((format!("{bad}%u"), vec![0x1234_5678])); }
    for width in 0..=12 {
        for flag in flags {
            let mut format = String::new();
            for c in ['d', 'i', 'u', 'x', 'X', 'c', 's'] {
                format.push_str(&format!("[%{flag}{width}l{c}]"));
            }
            // -1 and u32::MAX are the same argument word in this ABI.
            for (i, word) in [0, 1, u32::MAX, i32::MIN as u32, i32::MAX as u32].into_iter().enumerate() {
                let mut args = vec![word; 6];
                args.push(strings[i % strings.len()]);
                cases.push((format.clone(), args));
            }
        }
    }

    let mut seed = 0x53a1_07f4;
    for _ in 0..200 {
        let mut format = String::new();
        let mut args = Vec::new();
        for _ in 0..1 + random(&mut seed) % 8 {
            format.push('[');
            match random(&mut seed) % 6 {
                0 => format.push_str(unsupported[random(&mut seed) as usize % unsupported.len()]),
                1 => format.push_str("%%"),
                _ => {
                    let flag = flags[random(&mut seed) as usize % flags.len()];
                    let width = random(&mut seed) % 13;
                    let length = if random(&mut seed) & 1 != 0 { "l" } else { "" };
                    let c = b"diuxXcs"[random(&mut seed) as usize % 7];
                    format.push_str(&format!("%{flag}{width}{length}{}", char::from(c)));
                    let word = random(&mut seed);
                    args.push(if c == b's' { strings[word as usize % strings.len()] } else { word });
                }
            }
            format.push(']');
        }
        cases.push((format, args));
    }
    cases
}

#[test]
fn sprintf_matches_c_formatting_for_its_subset() {
    let mut strings = [0; DATA_LEN]; // empty string at DATA
    strings[4..9].copy_from_slice(b"caf\xc3\xa9");
    for (i, c) in strings[16..144].iter_mut().enumerate() { *c = b'a' + (i % 26) as u8; }
    let cases: Vec<_> = sprintf_cases().into_iter().map(|(fmt, args)| {
        let expected = reference_sprintf(fmt.as_bytes(), &args, &strings);
        (fmt, args, expected)
    }).collect();

    // 7 for ENTRY, saves and cursor setup + 6 for the final NUL and return.
    // Per '%' allow 80 for the fixed parser path (including length lookahead),
    // 32 for fetch and payload setup, and 16 for field setup/exit.
    // Each format byte adds at most 16 for a flag/digit iteration or ordinary
    // copy. Each output byte adds at most 12 for digit generation (a string
    // scan takes 4) and 6 for copying (padding takes 4, a literal takes 5).
    // The CALL8 driver adds 11 + 2*4 for its four outgoing stack words.
    // Eight long strings give the maximum: 13 + 128*24 + 16*120 + 18*1048 + 19 = 23888.
    let helper_steps = cases.iter().map(|(fmt, _, out)| {
        13 + 128 * fmt.bytes().filter(|&c| c == b'%').count() + 16 * fmt.len() + 18 * out.len()
    }).max().unwrap();
    let mut h = Harness::with_stack_words(helper_steps, 4);
    h.memory.bytes_mut(h.data).copy_from_slice(&strings);
    let fmt_len = cases.iter().map(|(fmt, _, _)| fmt.len()).max().unwrap() + 1;
    let out_len = cases.iter().map(|(_, _, out)| out.len()).max().unwrap() + 3; // guards and NUL
    let fmt_base = DATA + 0x1000;
    let out_base = DATA + 0x2000;
    let fmt_store = h.memory.store(fmt_len);
    let out_store = h.memory.store(out_len);
    h.memory.map(fmt_base, fmt_store, 0, fmt_len, Perm { read: true, write: false, exec: false });
    h.memory.map(out_base, out_store, 0, out_len, Perm { read: true, write: true, exec: false });
    for (fmt, words, expected) in cases {
        // Place the format NUL at the last readable address.
        let start = fmt_len - fmt.len() - 1;
        let bytes = h.memory.bytes_mut(fmt_store);
        bytes[start..fmt_len - 1].copy_from_slice(fmt.as_bytes());
        bytes[fmt_len - 1] = 0;
        h.memory.bytes_mut(out_store).fill(0xa5);
        let mut args = vec![out_base + 1, fmt_base + start as u32];
        args.extend_from_slice(&words);
        let [count, _] = h.call("sprintf", &args);
        let out = h.memory.bytes(out_store);
        let end = expected.len() + 1;
        assert_eq!(count as usize, expected.len(), "sprintf({fmt:?}, {words:#x?}) return");
        assert_eq!(&out[1..end], expected.as_slice(), "sprintf({fmt:?}, {words:#x?}) bytes");
        assert_eq!(out[end], 0, "sprintf({fmt:?}, {words:#x?}) terminator");
        assert!(out[0] == 0xa5 && out[end + 1..].iter().all(|&b| b == 0xa5),
            "sprintf({fmt:?}, {words:#x?}) destination guards");
    }
}
