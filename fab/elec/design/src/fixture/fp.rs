//! Footprint and symbol generators for the hand-written `ledctl` test fixture.
//! These are our own land patterns from the fixture dimensions, not a part library.

use fab_elec_core::{mm, Angle, BBox, LayerId, Nm, Path, PinIx, PolySet, SchPt, Shape, P};
use fab_elec_parts::{
    Drill, FpAttrs, FpMount, FpText, FpTextKind, Footprint, Gfx, Pad, PadKind, PadProp, PadShape,
    PadStack, PasteRule, PinDir, PinShape, SymFill, SymGraphic, SymPin, Symbol, SymbolUnit,
};

/// Make a rounded rectangular surface-mount pad.
pub fn smd(no: &str, at: P, size: P) -> Pad {
    Pad {
        no: no.to_string(),
        kind: PadKind::Smd,
        stack: PadStack::smd(PadShape::RoundRect {
            size,
            ratio_ppm: 250_000,
        }),
        at,
        rot: Angle::ZERO,
        drill: None,
        mask_margin: None,
        paste: PasteRule::default(),
        thermal: None,
        zone_connect: None,
        die_len: 0,
        prop: PadProp::None,
    }
}

/// Make a plated through-hole pad with a square or circular land.
pub fn thru(no: &str, at: P, d: Nm, drill: Nm, square: bool) -> Pad {
    Pad {
        no: no.to_string(),
        kind: PadKind::Thru,
        stack: PadStack::thru(if square {
            PadShape::Rect { size: P::new(d, d) }
        } else {
            PadShape::Circle { d }
        }),
        at,
        rot: Angle::ZERO,
        drill: Some(Drill {
            size: P::new(drill, drill),
            offset: P::ZERO,
        }),
        mask_margin: None,
        paste: PasteRule::default(),
        thermal: None,
        zone_connect: None,
        die_len: 0,
        prop: PadProp::None,
    }
}

/// Trace a closed rectangle from its minimum corner.
pub fn rect_path(min: P, max: P) -> Path {
    Path::new(min)
        .line(P::new(max.x, min.y))
        .line(max)
        .line(P::new(min.x, max.y))
        .line(min)
}

/// Add the body outline, courtyard, and reference and value texts.
/// How far (mm) a courtyard stands round its body: a case reads a body as its courtyard.
pub const COURTYARD: f64 = 0.25;

pub fn finish(
    name: &str,
    pads: Vec<Pad>,
    body_min: P,
    body_max: P,
    mount: FpMount,
    text: Nm,
) -> Footprint {
    let mut bb = BBox::new(body_min, body_max);
    for pad in &pads {
        bb = bb.union(pad.stack.front.shape(pad.at, pad.rot).bbox());
    }

    Footprint {
        name: name.to_string(),
        pads,
        graphics: vec![(
            LayerId::F_FAB,
            Gfx::Stroke {
                path: rect_path(body_min, body_max),
                w: mm(0.1),
            },
        )],
        texts: vec![
            FpText {
                kind: FpTextKind::Reference,
                text: "REF**".to_string(),
                layer: LayerId::F_SILK,
                at: P::new(0, bb.max.y + mm(0.25) + text),
                rot: Angle::ZERO,
                size: text,
                thick: text / 7,
                visible: true,
            },
            FpText {
                kind: FpTextKind::Value,
                text: "VAL**".to_string(),
                layer: LayerId::F_FAB,
                at: P::ZERO,
                rot: Angle::ZERO,
                size: text,
                thick: text / 7,
                visible: true,
            },
        ],
        courtyard: PolySet::rect(bb.inflate(mm(COURTYARD))),
        keepouts: vec![],
        anchor: P::ZERO,
        attrs: FpAttrs {
            mount,
            ..FpAttrs::default()
        },
    }
}

/// Make a two-pad chip footprint; sizes other than 0402 and 0603 panic.
pub fn chip(name: &str, size: &str) -> Footprint {
    let (pad_size, pad_x, body_min, body_max) = match size {
        "0402" => (
            P::mm(0.56, 0.62),
            0.48,
            P::mm(-0.5, -0.25),
            P::mm(0.5, 0.25),
        ),
        "0603" => (
            P::mm(0.9, 0.95),
            0.8,
            P::mm(-0.8, -0.4),
            P::mm(0.8, 0.4),
        ),
        _ => panic!("unsupported chip size {size:?}; expected 0402 or 0603"),
    };
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
    )
}

/// Make a chip footprint with a silk bar beside cathode pad 1.
pub fn led_chip(name: &str, size: &str) -> Footprint {
    let mut fp = chip(name, size);
    let pad = &fp.pads[0];
    let x = pad.stack.front.shape(pad.at, pad.rot).bbox().min.x - mm(0.2);
    fp.graphics.push((
        LayerId::F_SILK,
        Gfx::Stroke {
            path: Path::new(P::new(x, mm(-0.4))).line(P::new(x, mm(0.4))),
            w: mm(0.12),
        },
    ));
    fp
}

/// Make the fixture's five-pad small-outline footprint.
pub fn sot23_5(name: &str) -> Footprint {
    let size = P::mm(0.6, 1.1);
    finish(
        name,
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
    )
}

/// Make a two-column footprint with a positive even pin count and a pin-1 mark.
/// `body` is the package's (length, width) in mm: its half length bounds the courtyard along the
/// pin rows.
pub fn tssop(name: &str, pins: usize, pitch: f64, body: (f64, f64)) -> Footprint {
    assert!(
        pins >= 2 && pins % 2 == 0,
        "tssop requires a positive even pin count"
    );
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
    let body_half_y = body.0 / 2.0;
    let mut fp = finish(
        name,
        pads,
        P::mm(-body.1 / 2.0, -body_half_y),
        P::mm(body.1 / 2.0, body_half_y),
        FpMount::Smd,
        mm(1.0),
    );
    fp.graphics.push((
        LayerId::F_SILK,
        Gfx::Fill(Shape::Circle {
            c: P::mm(-2.9, top_y + 0.7),
            r: mm(0.15),
        }),
    ));
    fp
}

/// Make a nonempty single-row through-hole header with square pad 1 at the origin.
pub fn pin_header(name: &str, pins: usize, pitch: f64) -> Footprint {
    assert!(pins > 0, "pin header requires at least one pin");
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
    finish(
        name,
        pads,
        P::mm(-pitch / 2.0, -((pins - 1) as f64) * pitch - pitch / 2.0),
        P::mm(pitch / 2.0, pitch / 2.0),
        FpMount::Thru,
        mm(1.0),
    )
}

/// Make a power-only receptacle with its mouth towards negative Y.
pub fn usb_c_power(name: &str) -> Footprint {
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
    finish(
        name,
        pads,
        P::mm(-4.47, -3.9),
        P::mm(4.47, 3.4),
        FpMount::Smd,
        mm(1.0),
    )
}

/// Make the fixture's four-pad tactile switch footprint.
pub fn tact_switch(name: &str) -> Footprint {
    let size = P::mm(1.0, 0.8);
    finish(
        name,
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
    )
}

/// Make a two-terminal rectangular symbol with hidden pin labels.
pub fn two_terminal() -> Symbol {
    Symbol {
        units: vec![SymbolUnit {
            name: String::new(),
            body_style: 0,
            graphics: vec![SymGraphic::Rect {
                min: P::mm(-1.27, -0.5),
                max: P::mm(1.27, 0.5),
                w: mm(0.15),
                fill: SymFill::None,
            }],
            pins: vec![
                SymPin {
                    pin: PinIx(0),
                    at: SchPt::new(-2, 0),
                    dir: PinDir::Right,
                    len: SchPt::GRID,
                    shape: PinShape::Line,
                    name_shown: false,
                    number_shown: false,
                },
                SymPin {
                    pin: PinIx(1),
                    at: SchPt::new(2, 0),
                    dir: PinDir::Left,
                    len: SchPt::GRID,
                    shape: PinShape::Line,
                    name_shown: false,
                    number_shown: false,
                },
            ],
        }],
    }
}

/// Make a filled box symbol with labelled pins on the requested sides.
pub fn ic_box(left: &[PinIx], right: &[PinIx], top: &[PinIx], bottom: &[PinIx]) -> Symbol {
    let rows = left.len().max(right.len()).max(1) as i32;
    let cols = top.len().max(bottom.len()).max(4) as i32;
    let hw = cols / 2 + 2;
    let hh = (rows + 1) / 2 + 1;
    let mut pins = Vec::with_capacity(left.len() + right.len() + top.len() + bottom.len());
    let mut add_pin = |pin, at, dir| {
        pins.push(SymPin {
            pin,
            at,
            dir,
            len: 2 * SchPt::GRID,
            shape: PinShape::Line,
            name_shown: true,
            number_shown: true,
        });
    };
    for (i, &pin) in left.iter().enumerate() {
        add_pin(pin, SchPt::new(-hw - 2, hh - 1 - i as i32), PinDir::Right);
    }
    for (i, &pin) in right.iter().enumerate() {
        add_pin(pin, SchPt::new(hw + 2, hh - 1 - i as i32), PinDir::Left);
    }
    for (i, &pin) in top.iter().enumerate() {
        add_pin(pin, SchPt::new(-cols / 2 + i as i32, hh + 2), PinDir::Down);
    }
    for (i, &pin) in bottom.iter().enumerate() {
        add_pin(pin, SchPt::new(-cols / 2 + i as i32, -hh - 2), PinDir::Up);
    }
    Symbol {
        units: vec![SymbolUnit {
            name: String::new(),
            body_style: 0,
            graphics: vec![SymGraphic::Rect {
                min: SchPt::new(-hw, -hh).to_p(),
                max: SchPt::new(hw, hh).to_p(),
                w: mm(0.25),
                fill: SymFill::Body,
            }],
            pins,
        }],
    }
}
