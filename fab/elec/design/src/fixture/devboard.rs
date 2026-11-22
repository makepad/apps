//! A development board's mechanical skeleton: its outline, holes and the parts a case reads.
//! A case record overrides its measured values; there is no electrical circuit yet.

use super::build::{board_unit, circuit, hole, place};
use super::fp::COURTYARD;
use super::parts;
use super::sbc::{block, board, component};
use crate::options::{Field, Refused};
use crate::{Assembly, Design, Frame, Mount};
use fab_elec_board::Board;
use fab_elec_circuit::Circuit;
use fab_elec_core::{BlockId, BoardId, Contour, Side, Xform3, P};
use fab_elec_parts::{FpMount, PartDef};
use std::sync::Arc;

const LENGTH: f64 = 68.6;
const DEPTH: f64 = 53.4;
// The jack's width (along the edge) and depth (into the board), and the strip along its axis
// its pins stand in (a barrel jack's pins sit near its axis, not under its whole body).
const DC_WIDTH: f64 = 9.0;
const DC_DEPTH: f64 = 14.0;
const DC_PINS: f64 = 4.0;
// Pin counts and row centres; header bodies and their tails share these positions.
const HEADERS: [(usize, f64, f64); 4] = [
    (10, 32.75, 50.8),
    (8, 57.15, 50.8),
    (8, 36.8, 2.54),
    (6, 57.15, 2.54),
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UsbKind { B, Micro, C }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum McuKind { Dip28, Smd }
#[derive(Clone, Debug, PartialEq)]
pub struct Usb { pub kind: UsbKind, pub height: f64, pub overhang: f64, pub y: f64 }
#[derive(Clone, Debug, PartialEq)]
pub struct Dc { pub present: bool, pub height: f64, pub overhang: f64, pub y: f64 }
/// What a person measures on their board (mm); a case document's record overrides any of it.
#[derive(Clone, Debug, PartialEq)]
pub struct Measured { pub usb: Usb, pub dc: Dc, pub mcu: McuKind, pub header_height: f64, pub tallest: f64, pub pins_below: f64 }

/// The defaults (mm) a record overrides.
pub const MEASURED: Measured = Measured {
    usb: Usb { kind: UsbKind::B, height: 10.9, overhang: 6.3, y: 38.1 },
    dc: Dc { present: true, height: 11.0, overhang: 1.9, y: 7.6 },
    mcu: McuKind::Dip28,
    header_height: 8.5,
    tallest: 6.5,
    pins_below: 2.0,
};

impl Measured {
    /// Every key `set` takes, dotted as in the record.
    pub const KEYS: &'static [&'static str] = &[
        "usb.kind", "usb.height", "usb.overhang", "usb.y", "dc.present", "dc.height",
        "dc.overhang", "dc.y", "mcu", "header_height", "tallest", "pins_below",
    ];

    /// Set the value at dotted `key` from a record leaf; Err is what is wrong with it.
    pub fn set(&mut self, key: &str, value: &Field) -> Result<(), String> {
        match key {
            "usb.kind" => self.usb.kind = match value {
                Field::Id(id) if id == "b" => UsbKind::B,
                Field::Id(id) if id == "micro" => UsbKind::Micro,
                Field::Id(id) if id == "c" => UsbKind::C,
                _ => return Err("is one of @b, @micro, @c".into()),
            },
            "mcu" => self.mcu = match value {
                Field::Id(id) if id == "dip28" => McuKind::Dip28,
                Field::Id(id) if id == "smd" => McuKind::Smd,
                _ => return Err("is one of @dip28, @smd".into()),
            },
            "dc.present" => self.dc.present = match value {
                Field::Bool(present) => *present,
                _ => return Err("is a flag (true or false)".into()),
            },
            _ => return self.set_length(key, value),
        }
        Ok(())
    }

    fn set_length(&mut self, key: &str, value: &Field) -> Result<(), String> {
        let (target, hi, positive) = match key {
            "usb.height" => (&mut self.usb.height, LENGTH, true),
            "dc.height" => (&mut self.dc.height, LENGTH, true),
            "header_height" => (&mut self.header_height, LENGTH, true),
            "tallest" => (&mut self.tallest, LENGTH, true),
            "usb.overhang" => (&mut self.usb.overhang, LENGTH, false),
            "dc.overhang" => (&mut self.dc.overhang, LENGTH, false),
            "pins_below" => (&mut self.pins_below, LENGTH, false),
            "usb.y" => (&mut self.usb.y, DEPTH, false),
            "dc.y" => (&mut self.dc.y, DEPTH, false),
            _ => return Err(format!("is not a devboard measurement; it has {}", Self::KEYS.join(", "))),
        };
        *target = length(value, hi, positive)?;
        Ok(())
    }
}

fn length(value: &Field, hi: f64, positive: bool) -> Result<f64, String> {
    let Field::Num(metres) = value else {
        return Err("is a length, as in 7.9mm".into());
    };
    let v = metres * 1000.0;
    let above_lo = if positive { v > 0.0 } else { v >= 0.0 };
    if !(above_lo && v <= hi) {
        let rounded = format!("{v:.4}");
        let v = rounded.trim_end_matches('0').trim_end_matches('.');
        return Err(format!("is {v} mm, outside 0 to {hi} mm; a length needs its unit, as in 7.9mm"));
    }
    Ok(v)
}

pub fn devboard() -> Design {
    devboard_with(&MEASURED)
}

pub fn devboard_with(m: &Measured) -> Design {
    let (usb, usb_depth) = usb_part(&m.usb);
    let circuit = devboard_circuit(m, usb);
    let board = devboard_board(&circuit, m, usb_depth);
    Design {
        units: vec![board_unit(circuit, board)],
        links: vec![],
        assembly: Assembly {
            mounts: vec![Mount { unit: BoardId(0), parent: Frame::World, xform: Xform3::IDENTITY }],
            spacers: vec![],
            order: vec![],
        },
        bindings: vec![],
        generation: 0,
    }
}

/// MEASURED with each leaf set in order; the first refused leaf stops it. Each overhang then
/// leaves part of its connector on the board, whichever kind the record names.
pub fn devboard_over(fields: &[(String, Field)]) -> Result<Design, Refused> {
    let mut m = MEASURED;
    for (key, value) in fields {
        m.set(key, value).map_err(|message| Refused { key: key.clone(), message })?;
    }
    let (_, usb_depth) = usb_part(&m.usb);
    for (key, overhang, depth) in [("usb.overhang", m.usb.overhang, usb_depth), ("dc.overhang", m.dc.overhang, DC_DEPTH)] {
        if overhang >= depth {
            return Err(Refused { key: key.into(), message: format!("is {overhang} mm, not less than the connector's {depth} mm depth") });
        }
    }
    Ok(devboard_with(&m))
}

fn usb_part(usb: &Usb) -> (Arc<PartDef>, f64) {
    let (name, width, depth, opening, mount, reach) = match usb.kind {
        // The larger receptacle's body enters the wall, so the opening follows its face.
        UsbKind::B => ("USB_B_RECEPTACLE", 12.0, 16.3, None, FpMount::Thru, 10.0),
        // The smaller mouths need room in front for the mating plug's overmold.
        UsbKind::Micro => ("USB_MICRO_RECEPTACLE", 7.5, 5.0, Some((10.6, 6.0)), FpMount::Smd, 8.0),
        UsbKind::C => ("USB_C_RECEPTACLE", 8.94, 7.35, Some((12.4, 6.6)), FpMount::Smd, 8.0),
    };
    (parts::edge_plug(name, (width, depth, usb.height), opening, mount, reach), depth)
}

fn devboard_circuit(m: &Measured, usb_part: Arc<PartDef>) -> Circuit {
    let mut c = circuit(BoardId(0), "devboard");
    let usb = block(&mut c, "usb");
    let dc = block(&mut c, "dc");
    let mcu = block(&mut c, "mcu");
    let header = block(&mut c, "header");
    let power = block(&mut c, "power");

    component(&mut c, "usb.j", "J1", usb_part, usb);
    if m.dc.present {
        // The jack body enters the wall, so its opening is the body's face.
        let jack = parts::edge_plug("DC_JACK", (DC_WIDTH, DC_DEPTH, m.dc.height), None, FpMount::Thru, 10.0);
        component(&mut c, "dc.j", "J2", jack, dc);
    }
    let processor = match m.mcu {
        McuKind::Dip28 => parts::dip28_socketed(),
        McuKind::Smd => parts::qfp32(),
    };
    component(&mut c, "mcu.u", "U1", processor, mcu);
    for (i, &(pins, _, _)) in HEADERS.iter().enumerate() {
        component(&mut c, &format!("header.j[{i}]"), &format!("J{}", i + 3), parts::female_header(pins, m.header_height), header);
    }
    component(&mut c, "power.c", "C1", parts::can(m.tallest), power);

    // Separate bottom bodies let a case include soldered tails in its standoff clearance.
    let mut next_refdes = 1;
    for (i, &(pins, _, _)) in HEADERS.iter().enumerate() {
        add_tails(&mut c, &format!("header.tails[{i}]"), header, (pins as f64 * 2.54, 2.54), m.pins_below, &mut next_refdes);
    }
    // Only the connector's on-board depth has tails; the overhanging body has none. Pins stand
    // back from the board's edge: a tail's courtyard ends at it.
    if m.usb.kind == UsbKind::B {
        add_tails(&mut c, "usb.tails", usb, (16.3 - m.usb.overhang - 2.0 * COURTYARD, 12.0), m.pins_below, &mut next_refdes);
    }
    if m.dc.present {
        add_tails(&mut c, "dc.tails", dc, (DC_DEPTH - m.dc.overhang - 2.0 * COURTYARD, DC_PINS), m.pins_below, &mut next_refdes);
    }
    if m.mcu == McuKind::Dip28 {
        add_tails(&mut c, "mcu.tails", mcu, (35.6, 7.6), m.pins_below, &mut next_refdes);
    }
    c
}

fn add_tails(c: &mut Circuit, path: &str, block: BlockId, size: (f64, f64), below: f64, next_refdes: &mut usize) {
    let part = parts::pin_tails(size.0, size.1, below);
    component(c, path, &format!("TP{next_refdes}"), part, block);
    *next_refdes += 1;
}

fn devboard_board(c: &Circuit, m: &Measured, usb_depth: f64) -> Board {
    let mut b = board(c, LENGTH, DEPTH);
    b.outline = vec![Contour::rect(P::ZERO, P::mm(LENGTH, DEPTH), 0)];
    // A -90 degree rotation turns the connector's local -Y mouth toward the board's -X edge.
    place(&mut b, c, "usb.j", P::mm(usb_depth / 2.0 - m.usb.overhang, m.usb.y), -90.0, Side::Top);
    if m.dc.present {
        place(&mut b, c, "dc.j", P::mm(DC_DEPTH / 2.0 - m.dc.overhang, m.dc.y), -90.0, Side::Top);
    }
    place(&mut b, c, "mcu.u", P::mm(44.5, 14.0), 0.0, Side::Top);
    for (i, &(_, x, y)) in HEADERS.iter().enumerate() {
        place(&mut b, c, &format!("header.j[{i}]"), P::mm(x, y), 0.0, Side::Top);
    }
    place(&mut b, c, "power.c", P::mm(20.0, 16.0), 0.0, Side::Top);
    place_tails(&mut b, c, m);
    for (name, x, y) in [
        ("mount1", 14.0, 2.5),
        ("mount2", 15.3, 50.7),
        ("mount3", 66.1, 7.6),
        ("mount4", 66.1, 35.5),
    ] {
        hole(&mut b, name, P::mm(x, y), 3.2);
    }
    b.snap_anchors();
    b.reindex();
    b
}

fn place_tails(b: &mut Board, c: &Circuit, m: &Measured) {
    // Bottom mirrors local X; these centred, symmetric tail bodies are unchanged by that flip.
    for (i, &(_, x, y)) in HEADERS.iter().enumerate() {
        place(b, c, &format!("header.tails[{i}]"), P::mm(x, y), 0.0, Side::Bottom);
    }
    if m.usb.kind == UsbKind::B {
        place(b, c, "usb.tails", P::mm((16.3 - m.usb.overhang) / 2.0, m.usb.y), 0.0, Side::Bottom);
    }
    if m.dc.present {
        place(b, c, "dc.tails", P::mm((DC_DEPTH - m.dc.overhang) / 2.0, m.dc.y), 0.0, Side::Bottom);
    }
    if m.mcu == McuKind::Dip28 {
        place(b, c, "mcu.tails", P::mm(44.5, 14.0), 0.0, Side::Bottom);
    }
}
