//! Part primitives reproduce the reference layouts and preferred values.

use fab_elec_bench::{input::LedColor, ModelLibs};
use fab_elec_core::{mm, Canon, LayerId, Path, PinIx, Shape, P};
use fab_elec_design::fixture::fp;
use fab_elec_lang::eseries::{nearest, Series};
use fab_elec_lang::led::{forward_volts, forward_volts_at};
use fab_elec_lang::prims::{finish, finish_with, ic_box, smd, thru, two_terminal};
use fab_elec_parts::{Footprint, FpMount, Gfx};

#[test]
fn primitives_match_the_standard_shapes() {
    let same_footprint = |actual: Footprint, expected: Footprint| {
        assert_eq!(
            actual.content_hash(),
            expected.content_hash(),
            "{}: primitives preserve canonical footprint content",
            actual.name,
        );
    };

    for (name, size, pad_size, pad_x, body_min, body_max) in [
        (
            "R_0402",
            "0402",
            P::mm(0.56, 0.62),
            0.48,
            P::mm(-0.5, -0.25),
            P::mm(0.5, 0.25),
        ),
        (
            "C_0603",
            "0603",
            P::mm(0.9, 0.95),
            0.8,
            P::mm(-0.8, -0.4),
            P::mm(0.8, 0.4),
        ),
    ] {
        same_footprint(
            finish(
                name,
                vec![
                    smd("1", P::mm(-pad_x, 0.0), pad_size),
                    smd("2", P::mm(pad_x, 0.0), pad_size),
                ],
                body_min,
                body_max,
                FpMount::Smd,
                mm(0.6),
            ),
            fp::chip(name, size),
        );
    }

    let pad_size = P::mm(0.9, 0.95);
    let pads = vec![
        smd("1", P::mm(-0.8, 0.0), pad_size),
        smd("2", P::mm(0.8, 0.0), pad_size),
    ];
    let pad = &pads[0];
    let x = pad.stack.front.shape(pad.at, pad.rot).bbox().min.x - mm(0.2);
    same_footprint(
        finish_with(
            "LED_0603",
            pads,
            P::mm(-0.8, -0.4),
            P::mm(0.8, 0.4),
            FpMount::Smd,
            mm(0.6),
            vec![(
                LayerId::F_SILK,
                Gfx::Stroke {
                    path: Path::new(P::new(x, mm(-0.4))).line(P::new(x, mm(0.4))),
                    w: mm(0.12),
                },
            )],
        ),
        fp::led_chip("LED_0603", "0603"),
    );

    let size = P::mm(0.6, 1.1);
    same_footprint(
        finish(
            "LDO_3V3_SOT23-5",
            vec![
                smd("1", P::mm(-0.95, -1.1), size),
                smd("2", P::mm(0.0, -1.1), size),
                smd("3", P::mm(0.95, -1.1), size),
                smd("4", P::mm(0.95, 1.1), size),
                smd("5", P::mm(-0.95, 1.1), size),
            ],
            P::mm(-1.45, -0.8),
            P::mm(1.45, 0.8),
            FpMount::Smd,
            mm(0.8),
        ),
        fp::sot23_5("LDO_3V3_SOT23-5"),
    );

    let pins = 20;
    let pitch = 0.65;
    let half = pins / 2;
    let top_y = (half - 1) as f64 / 2.0 * pitch;
    let size = P::mm(1.5, 0.4);
    let mut pads = Vec::with_capacity(pins);
    for i in 0..half {
        let y = ((half - 1) as f64 / 2.0 - i as f64) * pitch;
        pads.push(smd(&(i + 1).to_string(), P::mm(-2.9, y), size));
    }
    for i in 0..half {
        let y = -((half - 1) as f64 / 2.0 - i as f64) * pitch;
        pads.push(smd(&(half + i + 1).to_string(), P::mm(2.9, y), size));
    }
    // The package body is 6.5 x 4.4 mm.
    let body_half_y = 6.5 / 2.0;
    same_footprint(
        finish_with(
            "MCU_20_TSSOP",
            pads,
            P::mm(-2.2, -body_half_y),
            P::mm(2.2, body_half_y),
            FpMount::Smd,
            mm(1.0),
            vec![(
                LayerId::F_SILK,
                Gfx::Fill(Shape::Circle {
                    c: P::mm(-2.9, top_y + 0.7),
                    r: mm(0.15),
                }),
            )],
        ),
        fp::tssop("MCU_20_TSSOP", pins, pitch, (6.5, 4.4)),
    );

    let pins = 5;
    let pitch = 1.27_f64;
    let d = mm((pitch * 0.8 * 100.0).round() / 100.0);
    let drill = mm((pitch * 0.5 * 100.0).round() / 100.0);
    let pads = (0..pins)
        .map(|i| {
            thru(
                &(i + 1).to_string(),
                P::mm(0.0, -(i as f64) * pitch),
                d,
                drill,
                i == 0,
            )
        })
        .collect();
    same_footprint(
        finish(
            "PinHeader_1x5_P1.27mm",
            pads,
            P::mm(-pitch / 2.0, -((pins - 1) as f64) * pitch - pitch / 2.0),
            P::mm(pitch / 2.0, pitch / 2.0),
            FpMount::Thru,
            mm(1.0),
        ),
        fp::pin_header("PinHeader_1x5_P1.27mm", pins, pitch),
    );

    let size = P::mm(0.6, 1.2);
    let mut pads = vec![
        smd("A12", P::mm(-2.0, 1.5), size),
        smd("A9", P::mm(-1.2, 1.5), size),
        smd("A5", P::mm(-0.4, 1.5), size),
        smd("B5", P::mm(0.4, 1.5), size),
        smd("B9", P::mm(1.2, 1.5), size),
        smd("B12", P::mm(2.0, 1.5), size),
    ];
    for at in [
        P::mm(-4.3, 0.5),
        P::mm(4.3, 0.5),
        P::mm(-4.3, -3.0),
        P::mm(4.3, -3.0),
    ] {
        pads.push(thru("S1", at, mm(1.2), mm(0.6), false));
    }
    same_footprint(
        finish(
            "USB_C_Power",
            pads,
            P::mm(-4.47, -3.9),
            P::mm(4.47, 3.4),
            FpMount::Smd,
            mm(1.0),
        ),
        fp::usb_c_power("USB_C_Power"),
    );

    let size = P::mm(1.0, 0.8);
    same_footprint(
        finish(
            "TactSwitch",
            vec![
                smd("1", P::mm(-2.6, 1.6), size),
                smd("2", P::mm(2.6, 1.6), size),
                smd("3", P::mm(-2.6, -1.6), size),
                smd("4", P::mm(2.6, -1.6), size),
            ],
            P::mm(-2.25, -2.25),
            P::mm(2.25, 2.25),
            FpMount::Smd,
            mm(1.0),
        ),
        fp::tact_switch("TactSwitch"),
    );

    assert_eq!(
        two_terminal().content_hash(),
        fp::two_terminal().content_hash(),
        "two-terminal symbol preserves canonical graphics and pins",
    );
    let left = [PinIx(0), PinIx(1)];
    let right = [PinIx(2)];
    let top = [PinIx(4)];
    let bottom = [PinIx(3)];
    assert_eq!(
        ic_box(&left, &right, &top, &bottom).content_hash(),
        fp::ic_box(&left, &right, &top, &bottom).content_hash(),
        "LDO box preserves canonical graphics and all four pin sides",
    );

    for (name, series, count) in [
        ("E3", Series::E3, 3),
        ("E6", Series::E6, 6),
        ("E12", Series::E12, 12),
        ("E24", Series::E24, 24),
        ("E48", Series::E48, 48),
        ("E96", Series::E96, 96),
        ("E192", Series::E192, 192),
    ] {
        assert_eq!(Series::of_name(name), Some(series), "{name}: exact name lookup");
        assert_eq!(series.values().len(), count, "{name}: values per decade");
        assert!(
            series.values().windows(2).all(|pair| pair[0] < pair[1]),
            "{name}: mantissas strictly increase",
        );
        assert_eq!(nearest(1.0, series), 1.0, "{name}: unity stays exact");
    }
    for name in ["E4", "e24", " E24", ""] {
        assert_eq!(Series::of_name(name), None, "{name:?}: unknown series has no table");
    }
    let green_vf = forward_volts_at("green", 0.005).expect("generic green curve");
    assert_eq!(nearest((3.3 - green_vf) / 0.005 - 25.0, Series::E24), 82.0,
        "true green at 5 mA and a 25 ohm sink leave 79.4 ohms, rounded to E24 82 ohms");
    assert_eq!(nearest(5100.0, Series::E24), 5100.0, "E24 preserves 5100 exactly");
    assert_eq!(nearest(9.9, Series::E12), 10.0, "E12 crosses into the next decade");
    assert_eq!(nearest(9.8, Series::E12), 10.0, "next-decade unity remains a candidate");
    assert_eq!(nearest(0.0047, Series::E6).to_bits(), 4.7e-3_f64.to_bits(), "E6 uses one decimal rounding");
    assert_eq!(nearest(3.25, Series::E3), 4.7, "selection uses ratio rather than absolute distance");
    for value in [0.0, -0.0, -1.0, f64::INFINITY, f64::NEG_INFINITY, f64::NAN] {
        assert_eq!(nearest(value, Series::E24).to_bits(), value.to_bits(), "nonpositive and nonfinite inputs retain their bits");
    }

    for (name, color, volts) in [
        ("red", LedColor::Red, 2.00),
        ("orange", LedColor::Orange, 2.05),
        ("yellow", LedColor::Yellow, 2.10),
        ("green", LedColor::Green, 3.00),
        ("blue", LedColor::Blue, 3.00),
        ("white", LedColor::White, 3.00),
        ("infrared", LedColor::Infrared, 1.20),
        ("yellow-green", LedColor::YellowGreen, 2.10),
    ] {
        let vf = forward_volts(name);
        assert_eq!(vf, ModelLibs::generic().led_vf(color, 0.02), "the simulator's {name} curve at its rated 20 mA");
        assert!((vf.expect("generic LED curve") - volts).abs() < 1e-9, "the simulator's {name} curve at its rated 20 mA");
    }
    for color in ["violet", "Red", ""] {
        assert_eq!(forward_volts(color), None, "{color:?}: unknown colour has no forward voltage");
    }
    assert_eq!(Some(green_vf), ModelLibs::generic().led_vf(LedColor::Green, 0.005), "the bench's exact curve");
    assert!(green_vf > 2.7 && green_vf < 2.8, "a true green die at 5 mA");
    assert!((forward_volts_at("green", 0.02).expect("generic green curve") - 3.0).abs() < 1e-9, "the rated point");
    let infrared_vf = forward_volts_at("infrared", 0.005).expect("generic infrared curve");
    assert!(infrared_vf > 1.06 && infrared_vf < 1.20, "a GaAs IR die at 5 mA");
    assert_eq!(forward_volts_at("violet", 0.005), None, "no curve");
    for amps in [0.0, -0.001, f64::NAN, f64::INFINITY] {
        assert_eq!(forward_volts_at("green", amps), None, "{amps}: current must be positive and finite");
    }
    assert_eq!(forward_volts_at("Green", 0.005), None, "colour names must be lowercase");
}
