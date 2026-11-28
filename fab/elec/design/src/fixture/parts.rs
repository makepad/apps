//! Generic parts for the typed circuit fixtures.

use super::fp;
use fab_elec_core::{mm, Angle, Contour, Nm, PinIx, PolySet, Xform3, P};
use fab_elec_parts::{Body3d, BodyPort, BodyPortKind, BodySource, Emitter, Fields, Footprint, FpMount, Light, Limits, PartDef, PartKind, PinDef, PinKind, Symbol, VRange};
use std::sync::Arc;

fn pin(name: &str, kind: PinKind, pads: &[&str]) -> PinDef {
    PinDef {
        name: name.into(),
        pads: pads.iter().map(|pad| (*pad).into()).collect(),
        kind,
        unit: 0,
        limits: None,
        alternates: vec![],
        delay: None,
    }
}

/// The courtyard extruded to `height` mm above the board surface, with `ports` (E1 5).
fn body(height: f64, ports: Vec<BodyPort>) -> Body3d {
    Body3d { source: BodySource::Extrude, offset: Xform3::IDENTITY, height: mm(height), ports, surface: None, missing_mesh: None }
}

/// A round port on the body's top face pointing away from the board (a light, a button): a
/// profile of radius `r` mm round the body's axis at `height` mm, clearance `margin` mm.
fn top_port(kind: BodyPortKind, height: f64, r: f64, margin: f64, reach: f64) -> BodyPort {
    BodyPort {
        kind,
        profile: PolySet::from_ring_nonzero(Contour::circle(P::ZERO, mm(r)).to_ring(mm(0.01))),
        at: [0, 0, mm(height)],
        dir: [0.0, 0.0, 1.0],
        reach: mm(reach),
        margin: mm(margin),
        travel: 0,
        stand: 0,
    }
}

fn part(name: &str, kind: PartKind, pins: Vec<PinDef>, symbol: Symbol, footprint: Footprint, body: Body3d) -> Arc<PartDef> {
    Arc::new(PartDef {
        name: name.into(),
        kind,
        symbol,
        footprint: Some(Arc::new(footprint)),
        body: Some(body),
        pins,
        fields: Fields::default(),
        emitter: None,
        gpio: None, lifecycle: None, approved: false,
    })
}

fn chip(prefix: &str, kind: PartKind, package: &str, height: f64) -> Arc<PartDef> {
    let name = format!("{prefix}_{package}");
    part(
        &name,
        kind,
        vec![pin("1", PinKind::Passive, &["1"]), pin("2", PinKind::Passive, &["2"])],
        fp::two_terminal(),
        fp::chip(&name, package),
        body(height, vec![]),
    )
}

/// The chip's body height (mm) by package; sizes other than 0402 and 0603 panic in `fp::chip`.
fn chip_height(package: &str, small: f64, large: f64) -> f64 {
    if package == "0402" { small } else { large }
}

pub fn resistor(package: &str) -> Arc<PartDef> {
    chip("R", PartKind::Resistor, package, chip_height(package, 0.35, 0.45))
}

pub fn capacitor(package: &str) -> Arc<PartDef> {
    chip("C", PartKind::Capacitor, package, chip_height(package, 0.5, 0.8))
}

pub fn led(package: &str, color: &str) -> Arc<PartDef> {
    let name = format!("LED_{package}_{color}");
    let led = part(
        &name,
        PartKind::Led,
        vec![pin("A", PinKind::Passive, &["2"]), pin("K", PinKind::Passive, &["1"])],
        fp::two_terminal(),
        fp::led_chip(&name, package),
        // the lens on top: light leaves straight up through an opening of 0.7 mm radius (a row
        // of these at a 2 mm pitch keeps 0.6 mm of wall between their openings)
        body(0.6, vec![top_port(BodyPortKind::Light, 0.6, 0.5, 0.2, 10.0)]),
    );
    let mut led = Arc::unwrap_or_clone(led);
    led.emitter = led_emitter(led.body.as_ref(), color);
    Arc::new(led)
}

/// The fixture LED's emitting face: its body's light port (the lens on top, its opening's
/// extent), an indicator die of `color` with a wide chip-LED beam. The flux is a typical red chip
/// LED's: about 100 mcd on axis at 20 mA, which a beam this wide spreads to about 0.3 lm.
fn led_emitter(body: Option<&Body3d>, color: &str) -> Option<Emitter> {
    let port = body?.ports.iter().find(|port| port.kind == BodyPortKind::Light)?;
    let open = port.profile.bbox();
    Some(Emitter {
        at: port.at,
        dir: port.dir,
        size: P::new(open.width() as Nm, open.height() as Nm),
        angle: Angle::deg(120.0),
        light: Light::of_led_color(color)?,
        flux_per_a: Some(15.0),
        faces: Vec::new(),
    })
}

pub fn ldo_3v3() -> Arc<PartDef> {
    let name = "LDO_3V3_SOT23-5";
    let mut pins = vec![
        pin("VIN", PinKind::PowerIn, &["1"]),
        pin("EN", PinKind::In, &["3"]),
        pin("VOUT", PinKind::PowerOut, &["5"]),
        pin("GND", PinKind::PowerIn, &["2"]),
        pin("NC", PinKind::NoConnect, &["4"]),
    ];
    pins[0].limits = Some(Limits { volts: Some(VRange { min: 2.5, max: 6.0 }), current: None, power: None });
    pins[2].limits = Some(Limits { volts: Some(VRange::fixed(3.3)), current: Some(0.6), power: None });
    part(
        name,
        PartKind::Regulator,
        pins,
        fp::ic_box(&[PinIx(0), PinIx(1)], &[PinIx(2)], &[PinIx(4)], &[PinIx(3)]),
        fp::sot23_5(name),
        body(1.1, vec![]),
    )
}

pub fn mcu_20() -> Arc<PartDef> {
    let name = "MCU_20_TSSOP";
    let mut pins = vec![
        pin("VDD", PinKind::PowerIn, &["4"]),
        pin("VSS", PinKind::PowerIn, &["5"]),
        pin("NRST", PinKind::In, &["3"]),
        pin("BOOT0", PinKind::In, &["17"]),
        pin("PA13", PinKind::Bidi, &["18"]),
        pin("PA14", PinKind::Bidi, &["19"]),
        pin("PA0", PinKind::Bidi, &["6"]),
        pin("PA1", PinKind::Bidi, &["7"]),
        pin("PA2", PinKind::Bidi, &["8"]),
        pin("PA3", PinKind::Bidi, &["9"]),
        pin("PA4", PinKind::Bidi, &["10"]),
        pin("PA5", PinKind::Bidi, &["11"]),
        pin("PA6", PinKind::Bidi, &["12"]),
        pin("PA7", PinKind::Bidi, &["13"]),
        pin("PA8", PinKind::Bidi, &["14"]),
        pin("PA9", PinKind::Bidi, &["15"]),
        pin("PA10", PinKind::Bidi, &["16"]),
        pin("PB6", PinKind::Bidi, &["20"]),
        pin("PB7", PinKind::Bidi, &["1"]),
        pin("PC14", PinKind::Bidi, &["2"]),
    ];
    pins[0].limits = Some(Limits { volts: Some(VRange { min: 2.0, max: 3.6 }), current: None, power: None });
    for pin in &mut pins {
        if pin.kind == PinKind::Bidi {
            pin.limits = Some(Limits { volts: Some(VRange { min: -0.3, max: 3.6 }), current: Some(0.015), power: None });
        }
    }
    let left: Vec<_> = (0..6).map(PinIx).collect();
    let right: Vec<_> = (6..20).map(PinIx).collect();
    part(name, PartKind::Mcu, pins, fp::ic_box(&left, &right, &[], &[]), fp::tssop(name, 20, 0.65, (6.5, 4.4)), body(1.2, vec![]))
}

pub fn usb_c_power() -> Arc<PartDef> {
    let name = "USB_C_POWER";
    let mut pins = vec![
        // symbol order: the right side (VBUS, CC1, CC2), then the bottom (GND, SHIELD)
        pin("VBUS", PinKind::PowerOut, &["A9", "B9"]),
        pin("CC1", PinKind::Passive, &["A5"]),
        pin("CC2", PinKind::Passive, &["B5"]),
        pin("GND", PinKind::PowerOut, &["A12", "B12"]),
        pin("SHIELD", PinKind::Passive, &["S1"]),
    ];
    pins[0].limits = Some(Limits { volts: Some(VRange::fixed(5.0)), current: Some(1.5), power: None });
    part(
        name,
        PartKind::Connector,
        pins,
        fp::ic_box(&[], &[PinIx(0), PinIx(1), PinIx(2)], &[], &[PinIx(3), PinIx(4)]),
        fp::usb_c_power(name),
        // the receptacle's mouth on the footprint's -Y edge, centred in its 3.26 mm shell: a plug
        // goes in along +Y, its overmould needs 6.6 mm in front of the mouth
        body(
            3.26,
            vec![BodyPort {
                kind: BodyPortKind::Plug,
                profile: PolySet::from_ring_nonzero(Contour::rect(P::mm(-4.47, -1.63), P::mm(4.47, 1.63), mm(1.6)).to_ring(mm(0.01))),
                at: [0, mm(-3.9), mm(1.63)],
                dir: [0.0, -1.0, 0.0],
                reach: mm(6.6),
                margin: mm(0.3),
                travel: 0,
                stand: 0,
            }],
        ),
    )
}

pub fn header(pins: usize, pitch_mm: f64) -> Arc<PartDef> {
    assert!(pins > 0 && pins <= u16::MAX as usize + 1, "header pin count must fit PinIx");
    let name = format!("PinHeader_1x{pins}_P{pitch_mm}mm");
    let defs = (1..=pins)
        .map(|i| {
            let no = i.to_string();
            pin(&no, PinKind::Passive, &[&no])
        })
        .collect();
    let left: Vec<_> = (0..pins).map(|i| PinIx(i as u16)).collect();
    // the insulator only: the pins through the board are not part of the body
    part(&name, PartKind::Connector, defs, fp::ic_box(&left, &[], &[], &[]), fp::pin_header(&name, pins, pitch_mm), body(2.5, vec![]))
}

pub fn tact_switch() -> Arc<PartDef> {
    let name = "SW_TACT_4PAD";
    part(
        name,
        PartKind::Switch,
        vec![pin("A", PinKind::Passive, &["1", "2"]), pin("B", PinKind::Passive, &["3", "4"])],
        fp::two_terminal(),
        fp::tact_switch(name),
        // 5 mm tall with its plunger on top; a finger or a cap presses it down
        body(5.0, vec![top_port(BodyPortKind::Press, 5.0, 1.25, 0.75, 10.0)]),
    )
}

/// A mechanical fixture part: its centred body, courtyard and ports; pins and pads come later.
fn mechanical_part(name: &str, kind: PartKind, size: (f64, f64, f64), mount: FpMount, ports: Vec<BodyPort>) -> Arc<PartDef> {
    part(
        name,
        kind,
        vec![],
        fp::ic_box(&[], &[], &[], &[]),
        fp::finish(name, vec![], P::mm(-size.0 / 2.0, -size.1 / 2.0), P::mm(size.0 / 2.0, size.1 / 2.0), mount, mm(1.0)),
        body(size.2, ports),
    )
}

/// A connector with its mouth on local -Y; dimensions are in mm. `opening` is what passes the
/// case wall in front of the mouth, width by height: None for the body's own face, as wide as
/// the case reads the body (its courtyard), when the body itself enters the wall; else the mating
/// plug's overmold. Square corners, so a square body inside it keeps the whole margin.
pub fn edge_plug(name: &str, size: (f64, f64, f64), opening: Option<(f64, f64)>, mount: FpMount, reach: f64) -> Arc<PartDef> {
    let (width, depth, height) = size;
    let (w, h) = opening.unwrap_or((width + 2.0 * fp::COURTYARD, height));
    mechanical_part(
        name, PartKind::Connector, size, mount,
        vec![BodyPort {
            kind: BodyPortKind::Plug,
            profile: PolySet::from_ring_nonzero(Contour::rect(P::mm(-w / 2.0, -h / 2.0), P::mm(w / 2.0, h / 2.0), 0).to_ring(mm(0.01))),
            at: [0, mm(-depth / 2.0), mm(height / 2.0)],
            dir: [0.0, -1.0, 0.0],
            reach: mm(reach),
            margin: mm(0.3),
            travel: 0,
            stand: 0,
        }],
    )
}

pub fn female_header(pins: usize, height: f64) -> Arc<PartDef> {
    mechanical_part(
        &format!("PinSocket_1x{pins:02}_P2.54mm"), PartKind::Connector,
        (pins as f64 * 2.54, 2.54, height), FpMount::Thru, vec![],
    )
}

pub fn dip28_socketed() -> Arc<PartDef> {
    mechanical_part("DIP28_SOCKETED", PartKind::Mcu, (35.6, 7.6, 4.5), FpMount::Thru, vec![])
}

pub fn qfp32() -> Arc<PartDef> {
    mechanical_part("QFP32_7x7", PartKind::Mcu, (7.0, 7.0, 1.2), FpMount::Smd, vec![])
}

pub fn can(height: f64) -> Arc<PartDef> {
    mechanical_part("CAP_CAN_6.3", PartKind::Capacitor, (6.3, 6.3, height), FpMount::Smd, vec![])
}

/// Through-hole tails under a part, placed on the bottom side so the case's standoffs clear them.
pub fn pin_tails(length: f64, width: f64, below: f64) -> Arc<PartDef> {
    mechanical_part("PIN_TAILS", PartKind::Other("tails".into()), (length, width, below), FpMount::Thru, vec![])
}

pub fn hdmi_a() -> Arc<PartDef> {
    mechanical_part(
        "HDMI_A", PartKind::Connector, (15.0, 11.5, 6.0), FpMount::Smd,
        vec![BodyPort {
            kind: BodyPortKind::Plug,
            profile: PolySet::from_ring_nonzero(Contour::rect(P::mm(-7.5, -2.8), P::mm(7.5, 2.8), mm(0.5)).to_ring(mm(0.01))),
            at: [0, mm(-5.75), mm(2.8)],
            dir: [0.0, -1.0, 0.0],
            reach: mm(10.0),
            margin: mm(0.3),
            travel: 0,
            stand: 0,
        }],
    )
}

pub fn usb_a_stack() -> Arc<PartDef> {
    mechanical_part(
        "USB_A_STACK", PartKind::Connector, (17.5, 13.25, 15.6), FpMount::Smd,
        vec![BodyPort {
            kind: BodyPortKind::Plug,
            profile: PolySet::from_ring_nonzero(Contour::rect(P::mm(-6.625, -7.8), P::mm(6.625, 7.8), mm(0.5)).to_ring(mm(0.01))),
            at: [mm(8.75), 0, mm(7.8)],
            dir: [1.0, 0.0, 0.0],
            reach: mm(10.0),
            margin: mm(0.3),
            travel: 0,
            stand: 0,
        }],
    )
}

pub fn rj45_leds() -> Arc<PartDef> {
    mechanical_part(
        "RJ45_LEDS", PartKind::Connector, (19.5, 16.0, 13.5), FpMount::Smd,
        vec![BodyPort {
            kind: BodyPortKind::Plug,
            profile: PolySet::from_ring_nonzero(Contour::rect(P::mm(-8.0, -6.75), P::mm(8.0, 6.75), mm(0.5)).to_ring(mm(0.01))),
            at: [mm(9.75), 0, mm(6.75)],
            dir: [1.0, 0.0, 0.0],
            reach: mm(12.0),
            margin: mm(0.3),
            travel: 0,
            stand: 0,
        }],
    )
}

pub fn microsd_push() -> Arc<PartDef> {
    mechanical_part(
        "MICROSD_PUSH", PartKind::Connector, (14.0, 15.0, 1.85), FpMount::Smd,
        // The card face is local +X: the bottom placement mirrors it to board -X.
        vec![BodyPort {
            kind: BodyPortKind::Plug,
            profile: PolySet::from_ring_nonzero(Contour::rect(P::mm(-6.0, -0.65), P::mm(6.0, 0.65), mm(0.2)).to_ring(mm(0.01))),
            at: [mm(7.0), 0, mm(0.925)],
            dir: [1.0, 0.0, 0.0],
            reach: mm(4.0),
            margin: mm(0.3),
            travel: 0,
            stand: 0,
        }],
    )
}

pub fn header_2x20() -> Arc<PartDef> {
    mechanical_part(
        "PinHeader_2x20_P2.54mm", PartKind::Connector, (50.8, 5.08, 8.5), FpMount::Thru,
        // The height includes the pins above the insulator, where a mating plug starts.
        vec![BodyPort {
            kind: BodyPortKind::Plug,
            profile: PolySet::from_ring_nonzero(Contour::rect(P::mm(-25.4, -2.54), P::mm(25.4, 2.54), mm(0.5)).to_ring(mm(0.01))),
            at: [0, 0, mm(8.5)],
            dir: [0.0, 0.0, 1.0],
            reach: mm(20.0),
            margin: mm(0.5),
            travel: 0,
            stand: 0,
        }],
    )
}

/// A side-entry flat-cable connector; only the fixture's 15-way and 22-way sizes are defined.
pub fn fpc_side(pins: usize, pitch: f64) -> Arc<PartDef> {
    let (width, depth, cable_width) = match pins {
        15 if pitch == 1.0 => (19.5, 5.0, 16.0),
        22 if pitch == 0.5 => (16.0, 4.5, 13.0),
        _ => panic!("unsupported flat-cable connector {pins} pins at {pitch} mm; expected 15 at 1.0 or 22 at 0.5"),
    };
    let name = format!("FPC_SIDE_{pins}_P{pitch}mm");
    mechanical_part(
        &name, PartKind::Connector, (width, depth, 2.0), FpMount::Smd,
        vec![BodyPort {
            kind: BodyPortKind::Cable,
            profile: PolySet::from_ring_nonzero(Contour::rect(P::mm(-cable_width / 2.0, -0.15), P::mm(cable_width / 2.0, 0.15), 0).to_ring(mm(0.01))),
            at: [0, mm(-depth / 2.0), mm(1.0)],
            dir: [0.0, -1.0, 0.0],
            reach: mm(6.0),
            margin: mm(0.3),
            travel: 0,
            stand: 0,
        }],
    )
}

pub fn side_switch() -> Arc<PartDef> {
    mechanical_part(
        "SW_TACT_SIDE", PartKind::Switch, (6.0, 3.6, 3.5), FpMount::Smd,
        vec![BodyPort {
            kind: BodyPortKind::Press,
            profile: PolySet::from_ring_nonzero(Contour::circle(P::ZERO, mm(1.0)).to_ring(mm(0.01))),
            at: [0, mm(-1.8), mm(1.75)],
            dir: [0.0, -1.0, 0.0],
            reach: mm(4.0),
            margin: mm(0.5),
            travel: 0,
            stand: 0,
        }],
    )
}

pub fn side_led(color: &str) -> Arc<PartDef> {
    let name = format!("LED_SIDE_{color}");
    let led = mechanical_part(
        &name, PartKind::Led, (1.6, 1.0, 0.8), FpMount::Smd,
        vec![BodyPort {
            kind: BodyPortKind::Light,
            profile: PolySet::from_ring_nonzero(Contour::circle(P::ZERO, mm(0.5)).to_ring(mm(0.01))),
            at: [0, mm(-0.5), mm(0.4)],
            dir: [0.0, -1.0, 0.0],
            reach: mm(4.0),
            margin: mm(0.2),
            travel: 0,
            stand: 0,
        }],
    );
    let mut led = Arc::unwrap_or_clone(led);
    led.emitter = led_emitter(led.body.as_ref(), color);
    Arc::new(led)
}

pub fn soc_bga() -> Arc<PartDef> {
    mechanical_part("SOC_BGA", PartKind::Ic, (15.0, 15.0, 1.4), FpMount::Smd, vec![])
}

pub fn memory_bga() -> Arc<PartDef> {
    mechanical_part("MEMORY_BGA", PartKind::Ic, (10.0, 14.5, 1.0), FpMount::Smd, vec![])
}

pub fn pmic_qfn() -> Arc<PartDef> {
    mechanical_part("PMIC_QFN", PartKind::Regulator, (7.0, 7.0, 0.9), FpMount::Smd, vec![])
}

pub fn power_inductor() -> Arc<PartDef> {
    mechanical_part("L_POWER", PartKind::Inductor, (4.0, 4.0, 2.0), FpMount::Smd, vec![])
}

pub fn polymer_cap() -> Arc<PartDef> {
    mechanical_part("C_POLYMER", PartKind::Capacitor, (6.6, 6.6, 5.8), FpMount::Smd, vec![])
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_led_emits_from_the_top_of_its_body() {
        for package in ["0402", "0603"] {
            let led = super::led(package, "red");
            let body = led.body.as_ref().expect("the fixture LED has a body");
            let e = led.emitter.as_ref().expect("the fixture LED has an emitter");
            assert_eq!(e.at[2], body.height, "{package}: the emitting face is the body's top face");
            assert_eq!(e.dir, [0.0, 0.0, 1.0], "{package}: light leaves straight up");
            assert!(e.size.x > 0 && e.size.y > 0, "{package}: the emitting face has an area");
        }
        let green = super::led("0603", "green");
        let light = green.emitter.as_ref().map(|e| &e.light);
        assert_eq!(light, Some(&fab_elec_parts::Light::Peak(525.0)), "a green LED's die emits true green, not the red default");
        let yellow_green = super::led("0603", "yellow-green");
        let light = yellow_green.emitter.as_ref().map(|e| &e.light);
        assert_eq!(light, Some(&fab_elec_parts::Light::Peak(570.0)), "the older yellow-green die keeps its own name");
    }
}
