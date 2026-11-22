//! Pad stacks, footprint finishes and symbol bodies for part generators.

use fab_elec_core::{mm, Angle, BBox, LayerId, Nm, Path, PinIx, PolySet, SchPt, P};
use fab_elec_parts::{
    Drill, Footprint, FpAttrs, FpMount, FpText, FpTextKind, Gfx, Pad, PadKind, PadProp,
    PadShape, PadStack, PasteRule, PinDir, PinShape, SymFill, SymGraphic, SymPin, Symbol,
    SymbolUnit,
};

/// A front surface pad with rounded corners at 25 % of its smaller side.
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

/// A plated through pad with a centred round drill and a square or circular land.
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

/// A closed rectangle traced counter-clockwise from its minimum corner.
pub fn rect_path(min: P, max: P) -> Path {
    Path::new(min)
        .line(P::new(max.x, min.y))
        .line(max)
        .line(P::new(min.x, max.y))
        .line(min)
}

/// Add a 0.1 mm fab body outline and a courtyard 0.25 mm beyond the pads and body.
/// The silk reference sits above that box; the fab value sits at the origin.
/// Both texts have height `text` and thickness `text / 7`.
pub fn finish(
    name: &str,
    pads: Vec<Pad>,
    body_min: P,
    body_max: P,
    mount: FpMount,
    text: Nm,
) -> Footprint {
    finish_with(name, pads, body_min, body_max, mount, text, Vec::new())
}

/// Apply [`finish`]'s rules, then append `extra` after the body outline.
/// Extra graphics do not enlarge the courtyard or move the reference text.
pub fn finish_with(
    name: &str,
    pads: Vec<Pad>,
    body_min: P,
    body_max: P,
    mount: FpMount,
    text: Nm,
    extra: Vec<(LayerId, Gfx)>,
) -> Footprint {
    let mut bb = BBox::new(body_min, body_max);
    for pad in &pads {
        bb = bb.union(pad.stack.front.shape(pad.at, pad.rot).bbox());
    }
    let mut graphics = vec![(
        LayerId::F_FAB,
        Gfx::Stroke {
            path: rect_path(body_min, body_max),
            w: mm(0.1),
        },
    )];
    graphics.extend(extra);

    Footprint {
        name: name.to_string(),
        pads,
        graphics,
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
        courtyard: PolySet::rect(bb.inflate(mm(0.25))),
        keepouts: vec![],
        anchor: P::ZERO,
        attrs: FpAttrs {
            mount,
            ..FpAttrs::default()
        },
    }
}

/// A rectangular two-terminal symbol with one-grid-step pins and hidden labels.
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

/// An op-amp's triangle pointing right: the non-inverting input (pin 0, marked +) and the
/// inverting input (pin 1, marked -) on its left edge, the output (pin 2) at its apex, the
/// positive supply (pin 3) above and the negative (pin 4) below, each pin ending on the triangle.
pub fn opamp() -> Symbol {
    let pin = |pin: u16, at: SchPt, dir: PinDir| SymPin {
        pin: PinIx(pin),
        at,
        dir,
        len: 2 * SchPt::GRID,
        shape: PinShape::Line,
        name_shown: false,
        number_shown: false,
    };
    // the left edge at x = -4 from y = -4 to 4, the apex at (4, 0): the slanted edges pass
    // through (0, 2) and (0, -2), where the supply pins end
    let corner = |x: i32, y: i32| SchPt::new(x, y).to_p();
    let mark = |text: &str, y: i32| SymGraphic::Text {
        at: P::new(SchPt::new(-3, y).to_p().x + mm(0.3), SchPt::new(0, y).to_p().y),
        text: text.to_string(),
        size: mm(1.0),
        rot: Angle::ZERO,
    };
    Symbol {
        units: vec![SymbolUnit {
            name: String::new(),
            body_style: 0,
            graphics: vec![
                SymGraphic::Poly {
                    pts: vec![corner(-4, 4), corner(4, 0), corner(-4, -4)],
                    closed: true,
                    w: mm(0.25),
                    fill: SymFill::Body,
                },
                mark("+", 2),
                mark("-", -2),
            ],
            pins: vec![
                pin(0, SchPt::new(-6, 2), PinDir::Right),
                pin(1, SchPt::new(-6, -2), PinDir::Right),
                pin(2, SchPt::new(6, 0), PinDir::Left),
                pin(3, SchPt::new(0, 4), PinDir::Down),
                pin(4, SchPt::new(0, -4), PinDir::Up),
            ],
        }],
    }
}

/// A filled box sized for labelled, two-grid-step pins on the requested sides.
/// Pins retain side order: left, right, top, then bottom.
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
