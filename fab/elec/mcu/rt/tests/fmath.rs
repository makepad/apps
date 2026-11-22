use fab_elec_mcu_rt::fmath::*;
use std::hint::black_box;

fn check32(bits: u32) {
    let x = black_box(f32::from_bits(bits));
    let operations: [(&str, fn(f32) -> f32, fn(f32) -> f32); 6] = [
        ("sqrt", sqrt_f32, f32::sqrt), ("floor", floor_f32, f32::floor),
        ("ceil", ceil_f32, f32::ceil), ("trunc", trunc_f32, f32::trunc),
        ("round", round_f32, f32::round), ("abs", abs_f32, f32::abs),
    ];
    for (name, soft, native) in operations {
        let mut expected = native(x).to_bits();
        // The format fixes two details std does not promise across hosts:
        // arithmetic NaNs are quiet, and invalid sqrt has a positive NaN.
        // abs is a sign operation and preserves signaling payload bits.
        if name != "abs" && bits & 0x7FFF_FFFF > 0x7F80_0000 { expected |= 0x0040_0000; }
        if name == "sqrt" && x < 0.0 { expected &= 0x7FFF_FFFF; }
        let found = soft(x).to_bits();
        assert_eq!(found, expected, "f32 {name}({bits:08x})");
        if name != "abs" && bits & 0x7FFF_FFFF > 0x7F80_0000 {
            assert_eq!(found, bits | 0x0040_0000, "f32 {name} NaN payload");
        }
    }
}

fn check64(bits: u64) {
    let x = black_box(f64::from_bits(bits));
    let operations: [(&str, fn(f64) -> f64, fn(f64) -> f64); 6] = [
        ("sqrt", sqrt_f64, f64::sqrt), ("floor", floor_f64, f64::floor),
        ("ceil", ceil_f64, f64::ceil), ("trunc", trunc_f64, f64::trunc),
        ("round", round_f64, f64::round), ("abs", abs_f64, f64::abs),
    ];
    for (name, soft, native) in operations {
        let mut expected = native(x).to_bits();
        if name != "abs" && bits & 0x7FFF_FFFF_FFFF_FFFF > 0x7FF0_0000_0000_0000 { expected |= 0x0008_0000_0000_0000; }
        if name == "sqrt" && x < 0.0 { expected &= 0x7FFF_FFFF_FFFF_FFFF; }
        let found = soft(x).to_bits();
        assert_eq!(found, expected, "f64 {name}({bits:016x})");
        if name != "abs" && bits & 0x7FFF_FFFF_FFFF_FFFF > 0x7FF0_0000_0000_0000 {
            assert_eq!(found, bits | 0x0008_0000_0000_0000, "f64 {name} NaN payload");
        }
    }
}

#[test]
fn soft_float_matches_std_and_module_nan_rules() {
    let edges32 = [
        0, 1, 2, 0x003F_FFFF, 0x007F_FFFF, 0x0080_0000, 0x0080_0001,
        0x3EFF_FFFF, 0x3F00_0000, 0x3F00_0001, 0x3F7F_FFFF, 0x3F80_0000,
        0x3FBF_FFFF, 0x3FC0_0000, 0x3FC0_0001, 0x4020_0000,
        0x4AFF_FFFF, 0x4B00_0000, 0x4B00_0001, 0x597F_FFFF, 0x5980_0000,
        0x5980_0001, 0x7F7F_FFFF, 0x7F80_0000,
        0x7F80_0001, 0x7FA1_2345, 0x7FC0_0000, 0x7FC1_2345, 0x7FFF_FFFF,
    ];
    let edges64 = [
        0, 1, 2, 0x0007_FFFF_FFFF_FFFF, 0x000F_FFFF_FFFF_FFFF,
        0x0010_0000_0000_0000, 0x0010_0000_0000_0001,
        0x3FDF_FFFF_FFFF_FFFF, 0x3FE0_0000_0000_0000, 0x3FE0_0000_0000_0001,
        0x3FEF_FFFF_FFFF_FFFF, 0x3FF0_0000_0000_0000,
        0x3FF7_FFFF_FFFF_FFFF, 0x3FF8_0000_0000_0000, 0x3FF8_0000_0000_0001,
        0x4004_0000_0000_0000, 0x415F_FFFF_FFFF_FFFF, 0x4160_0000_0000_0000,
        0x4160_0000_0000_0001, 0x432F_FFFF_FFFF_FFFF, 0x4330_0000_0000_0000,
        0x4330_0000_0000_0001, 0x7FEF_FFFF_FFFF_FFFF, 0x7FF0_0000_0000_0000,
        0x7FF0_0000_0000_0001, 0x7FF1_2345_6789_ABCD, 0x7FF8_0000_0000_0000,
        0x7FF9_2345_6789_ABCD, 0x7FFF_FFFF_FFFF_FFFF,
    ];
    for bits in edges32 { check32(bits); check32(bits | 0x8000_0000); }
    for bits in edges64 { check64(bits); check64(bits | 0x8000_0000_0000_0000); }
    assert_eq!(sqrt_f32(-1.0).to_bits(), 0x7FC0_0000);
    assert_eq!(sqrt_f64(-1.0).to_bits(), 0x7FF8_0000_0000_0000);
    let mut seed = 0xD1B5_4A32_D192_ED03u64;
    for _ in 0..200_000 {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        check32(seed as u32);
        check64(seed);
    }
}

fn check_div(a: u32, b: u32) {
    let x = black_box(f32::from_bits(a));
    let y = black_box(f32::from_bits(b));
    let native = x / y;
    // No NaN operands here. Invalid pairs have a specified positive default
    // NaN; the host's sign/payload for those pairs is deliberately irrelevant.
    let expected = if native.is_nan() { 0x7FC0_0000 } else { native.to_bits() };
    assert_eq!(div_f32(x, y).to_bits(), expected, "div({a:08x}, {b:08x})");
}

#[test]
fn division_matches_host_edges_and_seeded_pairs() {
    let edges = [
        0, 1, 2, 3, 0x003F_FFFF, 0x0040_0000, 0x007F_FFFF,
        0x0080_0000, 0x0080_0001, 0x0080_0003,
        0x3EFF_FFFF, 0x3F00_0000, 0x3F00_0001,
        0x3F7F_FFFF, 0x3F80_0000, 0x3F80_0001, 0x3F80_0003,
        0x3FFF_FFFF, 0x4000_0000, 0x4000_0001, 0x4040_0000,
        0x4B00_0000, 0x7F7F_FFFE, 0x7F7F_FFFF, 0x7F80_0000,
    ];
    for a in edges {
        for b in edges {
            for sa in [0, 0x8000_0000] {
                for sb in [0, 0x8000_0000] { check_div(a | sa, b | sb); }
            }
        }
    }
    // Include even/odd subnormal ties and the carry into the normal range.
    for (a, b, expected) in [
        (1, 0x4000_0000, 0), (3, 0x4000_0000, 2),
        (5, 0x4000_0000, 2), (0x00FF_FFFF, 0x4000_0000, 0x0080_0000),
    ] {
        assert_eq!(div_f32(f32::from_bits(a), f32::from_bits(b)).to_bits(), expected);
    }
    let mut seed = 0xA341_316Cu32;
    let mut next = || {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        seed
    };
    // Explicitly span every pair of exponent fields with seeded mantissas and
    // signs, plus unrestricted pairs. Fewer than 100k short integer divisions.
    for ae in 0..255 {
        for be in 0..255 {
            check_div((next() & 0x807F_FFFF) | ae << 23,
                (next() & 0x807F_FFFF) | be << 23);
        }
    }
    for _ in 0..32_768 {
        let a = next();
        let b = next();
        if a & 0x7FFF_FFFF <= 0x7F80_0000 && b & 0x7FFF_FFFF <= 0x7F80_0000 {
            check_div(a, b);
        }
    }
}

#[test]
fn binary_nan_rule_preserves_first_payload_before_invalid() {
    let nan32 = [0x7F80_0001, 0x7FA1_2345, 0x7FC0_0000, 0x7FC5_4321];
    let nan64 = [0x7FF0_0000_0000_0001, 0x7FF1_2345_6789_ABCD,
        0x7FF8_0000_0000_0000, 0x7FFA_BCDE_1234_5678];
    for a in nan32 {
        for sa in [0, 0x8000_0000] {
            let a = a | sa;
            for b in nan32.into_iter().chain([0, 1, 0x3F80_0000, 0x7F80_0000]) {
                for sb in [0, 0x8000_0000] {
                    let b = b | sb;
                    for (x, y) in [(a, b), (b, a)] {
                        let expected = (if x & 0x7FFF_FFFF > 0x7F80_0000 { x } else { y }) | 0x0040_0000;
                        // Exec's add/sub/mul use this helper before arithmetic.
                        for invalid in [false, true] {
                            assert_eq!(binary_nan_f32(f32::from_bits(x), f32::from_bits(y), invalid)
                                .unwrap().to_bits(), expected);
                        }
                        assert_eq!(div_f32(f32::from_bits(x), f32::from_bits(y)).to_bits(), expected);
                    }
                }
            }
        }
    }
    for a in nan64 {
        for sa in [0, 0x8000_0000_0000_0000] {
            let a = a | sa;
            for b in nan64.into_iter().chain([0, 1, 0x3FF0_0000_0000_0000, 0x7FF0_0000_0000_0000]) {
                for sb in [0, 0x8000_0000_0000_0000] {
                    let b = b | sb;
                    for (x, y) in [(a, b), (b, a)] {
                        let expected = (if x & 0x7FFF_FFFF_FFFF_FFFF > 0x7FF0_0000_0000_0000 { x } else { y })
                            | 0x0008_0000_0000_0000;
                        for invalid in [false, true] {
                            assert_eq!(binary_nan_f64(f64::from_bits(x), f64::from_bits(y), invalid)
                                .unwrap().to_bits(), expected);
                        }
                    }
                }
            }
        }
    }
    assert_eq!(binary_nan_f32(0.0, 0.0, true).unwrap().to_bits(), 0x7FC0_0000);
    assert_eq!(binary_nan_f64(0.0, 0.0, true).unwrap().to_bits(), 0x7FF8_0000_0000_0000);
    assert_eq!(binary_nan_f32(1.0, 2.0, false), None);
    assert_eq!(binary_nan_f64(1.0, 2.0, false), None);
}
