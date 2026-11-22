// Ported from TiNRS Psychedelic Router 0.1.0: src/core/SampleBoards.cpp (createEurorackModule, createMcuDevBoard,
// createBgaEscapeBoard, createSimpleDiffPairBoard, createKeepoutBoard, createLockedViaBoard; the `guard_` boards
// are Fab's own). Copyright (c) 2026 Stijn Kuipers. MIT License: see src/organic/LICENSE.

//! SampleBoards.cpp fixtures, in source component/pad/net order. Source mm/Y-down becomes
//! Fab nm/Y-up by reflecting Y, including each footprint's local pad coordinates. The `guard_`
//! boards are Fab's own (not the reference's): each makes one case of the organic lift happen.

use fab_elec_board::{
    Area, Board, BoardKeepout, BoardNet, Finish, Flash, LayerRole, NetPair, Origin, Placed, RuleSet, StackKind,
    StackLayer, Tent, Via, ViaKind, Zone, ZoneSettings,
};
use fab_elec_circuit::{ClassRules, NetClass, PairRule, ViaSize};
use fab_elec_core::{
    mm, Angle, BoardId, ClassId, CompId, Contour, LayerId, LayerSet, NetId, Nm, Path,
    PolySet, Side, P,
};
use fab_elec_design::fixture::fp;
use fab_elec_parts::{Footprint, Forbid, FpAttrs, FpMount, Pad, PadKind, PadShape, ThermalRule};
use std::sync::Arc;

type Land = (Pad, Option<NetId>);

fn at(x: f64, y: f64) -> P {
    P::mm(x, -y)
}

fn thru(no: &str, pos: P, diameter: f64, drill: f64, net: impl Into<Option<NetId>>) -> Land {
    (fp::thru(no, pos, mm(diameter), mm(drill), false), net.into())
}

fn smd(no: &str, pos: P, size: P, net: Option<NetId>) -> Land {
    // Board.h: RoundRect's default radius is 0.25 * min(size), as in fp::smd.
    // isSmd is represented by a front-only stack; through pads use all stack copper.
    (fp::smd(no, pos, size), net)
}

fn rect(no: &str, pos: P, size: P, net: impl Into<Option<NetId>>) -> Land {
    let (mut pad, net) = smd(no, pos, size, net.into());
    pad.stack.front = PadShape::Rect { size };
    (pad, net)
}

fn place(b: &mut Board, refdes: &str, footprint: &str, pos: P, lands: Vec<Land>) {
    let (mut pads, pad_nets): (Vec<_>, Vec<_>) = lands.into_iter().unzip();
    for pad in &mut pads {
        pad.at = pad.at.sub(pos);
    }
    let mount = if pads.iter().all(|pad| pad.kind == PadKind::Smd) {
        FpMount::Smd
    } else {
        FpMount::Thru
    };
    let id = b.alloc_id();
    b.placed.push(Placed {
        id,
        comp: CompId(b.placed.len() as u32),
        path: refdes.into(),
        refdes: refdes.into(),
        footprint: Arc::new(Footprint {
            name: footprint.into(),
            pads,
            attrs: FpAttrs { mount, ..Default::default() },
            ..Default::default()
        }),
        pad_nets,
        at: pos,
        rot: Angle::ZERO,
        side: Side::Top,
        // Board.h: footprintsLockedByDefault; no sample unlocks a component.
        locked: true,
        origin: Origin::Authored,
        body_z: None,
        ref_label: None, value_label: None, legend: None,
        value: None,
    });
}

fn net(b: &mut Board, name: &str, class: &str) -> NetId {
    let class = match b.classes.iter().position(|c| c.name == class) {
        Some(i) => ClassId(i as u16),
        None => {
            let id = ClassId(b.classes.len() as u16);
            // addNet only labels classes; these samples define no NetClass overrides.
            // Board.cpp classTrackWidth/classClearance/viaSizeClass therefore use these
            // DesignRules defaults for Power, Audio, Signal, Clock, DiffPair and Default.
            // Fab uses class.track as both the routing width and the DRC minimum; the
            // source's separate 0.15 mm neckdown minimum cannot also be expressed here.
            b.classes.push(NetClass {
                id,
                name: class.into(),
                rules: ClassRules {
                    track: Some(mm(0.25)),
                    clearance: Some(mm(0.20)),
                    via: Some(ViaSize { drill: mm(0.40), pad: mm(0.80) }),
                    pair: (class == "DiffPair").then_some(PairRule {
                        width: mm(0.25),
                        gap: mm(0.20),
                    }),
                    via_in_pad: Some(false),
                    ..Default::default()
                },
            });
            id
        }
    };
    let id = NetId(b.nets.len() as u32);
    b.nets.push(BoardNet { name: name.into(), class });
    id
}

fn board(name: &str, width: f64, height: f64, radius: f64, copper: u8) -> Board {
    let mut b = Board::new(BoardId(0), name);
    // Board.cpp setRectangularOutline: eight straight chords per rounded corner,
    // not Fab's analytic rounded rectangle. Keep the source vertex order.
    let points = if radius <= 0.1 {
        vec![at(0.0, 0.0), at(width, 0.0), at(width, height), at(0.0, height)]
    } else {
        let mut points = Vec::new();
        for (x, y, start) in [
            (width - radius, radius, -std::f64::consts::FRAC_PI_2),
            (width - radius, height - radius, 0.0),
            (radius, height - radius, std::f64::consts::FRAC_PI_2),
            (radius, radius, std::f64::consts::PI),
        ] {
            for i in 0..=8 {
                let a = start + std::f64::consts::FRAC_PI_2 * i as f64 / 8.0;
                points.push(at(x + a.cos() * radius, y + a.sin() * radius));
            }
        }
        points
    };
    let mut path = Path::new(points[0]);
    for p in &points[1..] {
        path = path.line(*p);
    }
    b.outline = vec![Contour::new(path)];

    // Board.cpp setStackupPreset plus Board.h BoardStackup::makeDefault: 1.6 mm,
    // 35 um copper, 10 um masks, equal FR4 dielectrics, alternating core/prepreg.
    // GroundPlane/PowerPlane both map to LayerRole::Plane. The named builders and
    // setStackupPreset assign no plane NET and create no zones, so do not invent one
    // (in particular the eurorack's Power class contains GND and three supplies).
    let technical = |kind, thickness| StackLayer {
        kind, thickness, material: String::new(), er: None, loss_tan: None,
        role: LayerRole::Signal, velocity: None,
    };
    let mut layers = vec![
        technical(StackKind::Silk(Side::Top), 0),
        technical(StackKind::Paste(Side::Top), 0),
        technical(StackKind::Mask(Side::Top), mm(0.01)),
    ];
    let dielectric = (mm(1.6) - mm(0.02) - i32::from(copper) * mm(0.035)) / i32::from(copper - 1);
    for i in 0..copper {
        let layer = if i == 0 { LayerId::F_CU } else if i == copper - 1 {
            LayerId::B_CU
        } else {
            LayerId::inner(i)
        };
        layers.push(StackLayer {
            kind: StackKind::Copper(layer),
            thickness: mm(0.035),
            material: "copper".into(),
            er: None,
            loss_tan: None,
            role: if copper > 2 && (i == 1 || i == copper - 2) {
                LayerRole::Plane
            } else {
                LayerRole::Signal
            },
            velocity: None,
        });
        if i < copper - 1 {
            layers.push(StackLayer {
                kind: StackKind::Dielectric { core: i % 2 == (copper / 2 - 1) % 2 },
                thickness: dielectric,
                material: "FR4".into(),
                er: Some(4.5),
                loss_tan: Some(0.02),
                role: LayerRole::Signal,
                velocity: None,
            });
        }
    }
    layers.extend([
        technical(StackKind::Mask(Side::Bottom), mm(0.01)),
        technical(StackKind::Paste(Side::Bottom), 0),
        technical(StackKind::Silk(Side::Bottom), 0),
    ]);
    b.stackup.layers = layers;
    b.stackup.finish = Finish::None;
    b.rules.base = RuleSet {
        track: Some(mm(0.15)),
        clearance: Some(mm(0.20)),
        via: Some(ViaSize { drill: mm(0.40), pad: mm(0.80) }),
        edge: Some(mm(0.50)),
        hole_clearance: Some(mm(0.25)),
        hole_to_hole: Some(mm(0.25)),
        drill: Some(0),
        annular: Some(0),
        ..RuleSet::standard()
    };
    b
}

pub fn eurorack_module() -> Board {
    // Reference: SampleBoards::createEurorackModule, SampleBoards.cpp:11-157.
    let mut b = board("eurorack_module", 110.0, 75.0, 3.0, 4);
    let gnd = net(&mut b, "GND", "Power");
    let p12 = net(&mut b, "+12V", "Power");
    let n12 = net(&mut b, "-12V", "Power");
    let p5 = net(&mut b, "+5V", "Power");
    let sig_a = net(&mut b, "AUDIO_L", "Audio");
    let sig_b = net(&mut b, "AUDIO_R", "Audio");
    let cv1 = net(&mut b, "CV_FREQ", "Signal");
    let cv2 = net(&mut b, "CV_RESON", "Signal");
    let gate = net(&mut b, "TRIG_IN", "Signal");
    let modulation = net(&mut b, "LFO_OUT", "Signal");

    let mut pads = Vec::new();
    for r in 0..5 {
        for c in 0..2 {
            let n = match r { 0 => n12, 4 => p12, 3 => p5, _ => gnd };
            pads.push(thru(&(r * 2 + c + 1).to_string(),
                at(12.0 + c as f64 * 2.54, 38.0 + (r as f64 - 2.0) * 2.54), 1.8, 1.0, n));
        }
    }
    place(&mut b, "J_PWR", "PinHeader_2x05_P2.54mm", at(12.0, 38.0), pads);

    for (refdes, x, y, nets) in [
        ("U1", 42.0, 25.0, [sig_a, sig_a, cv1, n12, gnd, cv2, sig_b, p12]),
        ("U2", 42.0, 52.0, [gate, gate, modulation, n12, gnd, sig_a, sig_b, p12]),
    ] {
        let mut pads = Vec::new();
        for i in 0..4 {
            pads.push(rect(&(i + 1).to_string(), at(x - 2.7, y + (i as f64 - 1.5) * 1.27), P::mm(1.6, 0.65), nets[i]));
        }
        for i in 0..4 {
            pads.push(rect(&(8 - i).to_string(), at(x + 2.7, y + (i as f64 - 1.5) * 1.27), P::mm(1.6, 0.65), nets[7 - i]));
        }
        place(&mut b, refdes, "SOIC-8", at(x, y), pads);
    }
    for (refdes, y, nets) in [("RV1", 24.0, [gnd, cv1, p12]), ("RV2", 52.0, [gnd, cv2, p12])] {
        let pads = (0..3).map(|i| thru(&(i + 1).to_string(), at(72.0, y + (i as f64 - 1.0) * 2.5), 2.0, 1.2, nets[i])).collect();
        place(&mut b, refdes, "Potentiometer_9mm", at(72.0, y), pads);
    }
    for (refdes, y, signal) in [("J1", 18.0, sig_a), ("J2", 38.0, sig_b), ("J3", 58.0, modulation)] {
        let pads = vec![
            thru("TIP", at(96.0, y - 2.5), 1.8, 1.1, signal),
            thru("SLEEVE", at(96.0, y + 2.5), 1.8, 1.1, gnd),
        ];
        place(&mut b, refdes, "Jack_3.5mm", at(96.0, y), pads);
    }
    b.reindex();
    b
}

pub fn mcu_dev() -> Board {
    // Reference: SampleBoards::createMcuDevBoard, SampleBoards.cpp:159-287.
    let mut b = board("mcu_dev", 90.0, 65.0, 2.5, 4);
    let gnd = net(&mut b, "GND", "Power");
    let v33 = net(&mut b, "+3V3", "Power");
    let v5 = net(&mut b, "+5V", "Power");
    let dp = net(&mut b, "USB_D+", "DiffPair");
    let dm = net(&mut b, "USB_D-", "DiffPair");
    let x1 = net(&mut b, "OSC_IN", "Clock");
    let x2 = net(&mut b, "OSC_OUT", "Clock");
    let scl = net(&mut b, "I2C_SCL", "Signal");
    let sda = net(&mut b, "I2C_SDA", "Signal");
    let led = net(&mut b, "STATUS_LED", "Signal");
    // Source pairs dp/dm. BoardNet and problem() have no partner field: retain the
    // DiffPair class width/gap, but this pipeline routes the two nets individually.

    let mut pads = Vec::new();
    for side in 0..4 {
        for p in 0..8 {
            let offset = (p as f64 - 3.5) * 0.8;
            let (pos, size, n) = match side {
                0 => (at(45.0 + offset, 32.5 + 4.2), P::mm(0.5, 1.5),
                    match p { 0 => Some(dp), 1 => Some(dm), 7 => Some(gnd), _ => None }),
                1 => (at(45.0 + 4.2, 32.5 - offset), P::mm(1.5, 0.5),
                    match p { 2 => Some(scl), 3 => Some(sda), 7 => Some(v33), _ => None }),
                2 => (at(45.0 - offset, 32.5 - 4.2), P::mm(0.5, 1.5),
                    match p { 1 => Some(x1), 2 => Some(x2), 7 => Some(gnd), _ => None }),
                _ => (at(45.0 - 4.2, 32.5 + offset), P::mm(1.5, 0.5),
                    match p { 0 => Some(led), 4 => Some(v33), 7 => Some(v5), _ => None }),
            };
            pads.push(smd(&(side * 8 + p + 1).to_string(), pos, size, n));
        }
    }
    place(&mut b, "U1", "LQFP-32_7x7mm", at(45.0, 32.5), pads);
    let pads = [v5, dm, dp, gnd].into_iter().enumerate().map(|(i, n)| {
        smd(["VBUS", "D-", "D+", "GND"][i], at(8.0, 32.5 + (i as f64 - 1.5) * 1.5), P::mm(1.2, 0.6), Some(n))
    }).collect();
    place(&mut b, "J_USB", "USB_C_Receptacle", at(8.0, 32.5), pads);
    let pads = vec![
        rect("1", at(45.0 - 1.5, 12.0), P::mm(1.2, 1.0), x1),
        rect("2", at(45.0 + 1.5, 12.0), P::mm(1.2, 1.0), x2),
    ];
    place(&mut b, "Y1", "Crystal_SMD_3225", at(45.0, 12.0), pads);
    let pads = [v33, scl, sda, gnd].into_iter().enumerate().map(|(i, n)| {
        thru(&(i + 1).to_string(), at(80.0, 32.5 + (i as f64 - 1.5) * 2.54), 1.8, 1.0, n)
    }).collect();
    place(&mut b, "J_I2C", "PinHeader_1x04_P2.54mm", at(80.0, 32.5), pads);
    b.reindex();
    b
}

pub fn bga_escape() -> Board {
    // Reference: SampleBoards::createBgaEscapeBoard, SampleBoards.cpp:289-355.
    let mut b = board("bga_escape", 70.0, 70.0, 2.0, 6);
    let a = net(&mut b, "BUS_0", "Default");
    let b1 = net(&mut b, "BUS_1", "Default");
    let c = net(&mut b, "BUS_2", "Default");
    let d = net(&mut b, "BUS_3", "Default");
    let gnd = net(&mut b, "GND", "Default");
    let vcc = net(&mut b, "VCC", "Default");
    let nets = [
        [a, b1, gnd, vcc, c, d],
        [b1, gnd, vcc, gnd, vcc, a],
        [gnd, vcc, gnd, gnd, vcc, gnd],
        [vcc, gnd, gnd, gnd, gnd, vcc],
        [c, vcc, gnd, vcc, gnd, b1],
        [d, c, vcc, gnd, a, c],
    ];
    let mut pads = Vec::new();
    for (r, row) in nets.iter().enumerate() {
        for (c, n) in row.iter().enumerate() {
            // The source comment/name says 1.0 mm, but its actual geometry uses 1.5 mm.
            let (mut pad, n) = smd(&format!("{}{}", char::from(b'A' + r as u8), c + 1),
                at(35.0 + (c as f64 - 2.5) * 1.5, 35.0 + (r as f64 - 2.5) * 1.5), P::mm(0.7, 0.7), Some(*n));
            pad.stack.front = PadShape::Circle { d: mm(0.7) };
            pads.push((pad, n));
        }
    }
    place(&mut b, "U1", "BGA-36_1.0mm", at(35.0, 35.0), pads);
    let pads = [a, b1, c, d].into_iter().enumerate().map(|(i, n)| {
        thru(&(i + 1).to_string(), at(12.0, 35.0 + (i as f64 - 1.5) * 4.0), 1.6, 0.9, n)
    }).collect();
    place(&mut b, "J_ESC", "", at(12.0, 35.0), pads);
    b.reindex();
    b
}

pub fn diff_pair_simple() -> Board {
    // Reference: SampleBoards::createSimpleDiffPairBoard, SampleBoards.cpp:357-386.
    let mut b = board("diff_pair_simple", 80.0, 50.0, 2.0, 2);
    let p = net(&mut b, "PAIR_P", "DiffPair");
    let n = net(&mut b, "PAIR_N", "DiffPair");
    let gnd = net(&mut b, "GND", "Power");
    b.pairs.push(NetPair { p, n, gap: None });

    // Source pads have componentRef only; Fab needs a placement to own each group.
    // Pad defaults are circular through holes with a 0.8 mm drill, including TX/RX.
    for (refdes, x) in [("TX", 15.0), ("RX", 65.0)] {
        let pads = vec![
            thru("P", at(x, 23.0), 1.4, 0.8, p),
            thru("N", at(x, 27.0), 1.4, 0.8, n),
        ];
        place(&mut b, refdes, "", at(x, 25.0), pads);
    }
    place(&mut b, "OBS1", "", at(38.0, 25.0), vec![
        thru("GND", at(38.0, 25.0), 3.2, 1.6, gnd),
    ]);
    b.reindex();
    b
}

/// Unexpanded source polygons, also used by the independent sampled intrusion check.
pub struct RuleArea {
    pub name: &'static str,
    pub poly: Vec<P>,
    pub layers: LayerSet,
    pub forbid: Forbid,
    pub clearance: Nm,
}

pub fn keepout_areas() -> Vec<RuleArea> {
    // Reference: createKeepoutBoard's rule areas, SampleBoards.cpp:573-606.
    let rectangle = |x0, y0, x1, y1| vec![at(x0, y0), at(x1, y0), at(x1, y1), at(x0, y1)];
    let all = LayerSet::of(&[LayerId::F_CU, LayerId::B_CU]);
    vec![
        RuleArea { name: "KO_A_FCU_NO_TRACKS", poly: rectangle(16.0, 3.0, 24.0, 31.0),
            layers: LayerSet::one(LayerId::F_CU), forbid: Forbid::TRACKS.with(Forbid::ZONES), clearance: 0 },
        RuleArea { name: "KO_B_ALL", poly: (0..6).map(|i| {
            let t = i as f64 * std::f64::consts::PI / 3.0;
            at(40.0 + 6.0 * t.cos(), 17.0 + 6.0 * t.sin())
        }).collect(), layers: all, forbid: Forbid::TRACKS.with(Forbid::VIAS).with(Forbid::ZONES), clearance: mm(0.2) },
        RuleArea { name: "KO_C_NO_VIAS", poly: rectangle(49.5, 3.0, 58.5, 31.0),
            layers: all, forbid: Forbid::VIAS.with(Forbid::ZONES), clearance: 0 },
        RuleArea { name: "KO_D_BCU_NO_TRACKS", poly: rectangle(60.0, 34.0, 66.0, 57.0),
            layers: LayerSet::one(LayerId::B_CU), forbid: Forbid::TRACKS.with(Forbid::ZONES), clearance: 0 },
        RuleArea { name: "KO_E_NEAR_U1", poly: vec![at(35.5, 47.6), at(44.5, 47.6), at(44.5, 52.5),
            at(41.0, 52.5), at(41.0, 50.0), at(35.5, 50.0)],
            layers: all, forbid: Forbid::TRACKS.with(Forbid::VIAS).with(Forbid::ZONES), clearance: 0 },
    ]
}

pub fn keepout() -> Board {
    // Reference: SampleBoards::createKeepoutBoard, SampleBoards.cpp:492-610, cut to 6 of its 22 nets,
    // one or two per rule area: A4 and A5 (the hexagon's rows: across KO_A, round KO_B, through
    // KO_C), C0 and C1 (down the no-vias strip KO_C), B0 (through U1, across KO_D) and B5 (under U1,
    // past KO_E). The other nets' pads stay as netless copper, so every obstacle is the source's.
    let mut b = board("keepout", 80.0, 60.0, 0.0, 2);
    let mut kept = |name: String, keep: bool| keep.then(|| net(&mut b, &name, "Default"));
    let a: Vec<_> = (0..10).map(|i| kept(format!("A{i}"), i == 4 || i == 5)).collect();
    let bn: Vec<_> = (0..8).map(|i| kept(format!("B{i}"), i == 0 || i == 5)).collect();
    let cn: Vec<_> = (0..4).map(|i| kept(format!("C{i}"), i < 2)).collect();
    let header = |b: &mut Board, refdes, x, y0, nets: &[Option<NetId>]| {
        let pads = nets.iter().enumerate().map(|(i, n)| {
            thru(&(i + 1).to_string(), at(x, y0 + i as f64 * 2.54), 1.7, 1.0, *n)
        }).collect();
        place(b, refdes, &format!("PinHeader_1x{}", nets.len()), at(x, y0 + (nets.len() - 1) as f64 * 1.27), pads);
    };
    header(&mut b, "J1", 6.0, 6.0, &a);
    header(&mut b, "J2", 74.0, 6.0, &a);
    let mut t = Vec::new();
    let mut u = Vec::new();
    for (i, n) in cn.iter().enumerate() {
        t.push(rect(&(i + 1).to_string(), at(51.0 + i as f64 * 2.0, 1.6), P::mm(1.0, 1.0), *n));
        u.push(rect(&(i + 1).to_string(), at(51.0 + i as f64 * 2.0, 33.2), P::mm(1.0, 1.0), *n));
    }
    // Source addPad alternates TP1/TP2. Fab stores pads within each footprint; each
    // component's pad order and the source component order are preserved.
    place(&mut b, "TP1", "TestPads_1x4", at(54.0, 1.6), t);
    place(&mut b, "TP2", "TestPads_1x4", at(54.0, 33.2), u);
    header(&mut b, "J3", 6.0, 36.0, &bn);
    header(&mut b, "J4", 74.0, 36.0, &bn);
    let mut pads = Vec::new();
    for (i, n) in bn[..4].iter().enumerate() {
        pads.push(rect(&(i + 1).to_string(), at(37.3, 43.1 + i as f64 * 1.27), P::mm(1.6, 0.65), *n));
    }
    for (i, n) in bn[..4].iter().enumerate() {
        pads.push(rect(&(8 - i).to_string(), at(42.7, 43.1 + i as f64 * 1.27), P::mm(1.6, 0.65), *n));
    }
    place(&mut b, "U1", "SOIC-8", at(40.0, 45.0), pads);
    for k in keepout_areas() {
        let area = PolySet::from_ring_nonzero(k.poly);
        // Fab has no per-keepout clearance: represent the margin in the area, with
        // 1 um offset chord error (the independent reference check has 1 um tolerance).
        let area = if k.clearance > 0 { area.offset(k.clearance, mm(0.001)) } else { area };
        let id = b.alloc_id();
        b.keepouts.push(BoardKeepout {
            id, area, layers: k.layers, forbid: k.forbid, reason: k.name.into(),
            origin: Origin::Authored, z: None,
        });
    }
    b.reindex();
    b
}

pub fn locked_via() -> Board {
    // Reference: SampleBoards::createLockedViaBoard, SampleBoards.cpp:732-811.
    let mut b = board("locked_via", 30.0, 20.0, 0.0, 2);
    let gnd = net(&mut b, "GND", "Default");
    let sig = net(&mut b, "SIG", "Default");
    let sig2 = net(&mut b, "SIG2", "Default");
    let pads = vec![
        rect("EP", at(15.0, 10.0), P::mm(3.0, 3.0), gnd),
        rect("1", at(12.6, 9.5), P::mm(0.8, 0.3), sig),
        rect("2", at(12.6, 10.5), P::mm(0.8, 0.3), sig2),
    ];
    place(&mut b, "U1", "QFN_EP", at(15.0, 10.0), pads);
    let pads = vec![
        thru("1", at(4.0, 7.46), 1.7, 1.0, gnd),
        thru("2", at(4.0, 10.0), 1.7, 1.0, sig),
        thru("3", at(4.0, 12.54), 1.7, 1.0, sig2),
    ];
    place(&mut b, "J1", "PinHeader_1x3", at(4.0, 10.0), pads);
    let pads = vec![
        thru("1", at(26.0, 9.5 - 1.27), 1.7, 1.0, sig),
        thru("2", at(26.0, 10.5 + 1.27), 1.7, 1.0, sig2),
    ];
    place(&mut b, "J2", "PinHeader_1x2", at(26.0, 10.0), pads);
    for i in 0..4 {
        let id = b.alloc_id();
        b.vias.push(Via {
            id, net: gnd,
            at: at(15.0 + if i & 1 != 0 { 0.6 } else { -0.6 }, 10.0 + if i & 2 != 0 { 0.6 } else { -0.6 }),
            pad: mm(0.6), drill: mm(0.3),
            span: (LayerId::F_CU, LayerId::B_CU),
            kind: ViaKind::Through, tent: Tent::Both, backdrill: None, locked: true,
            // Fab has no imported flag; authored + locked is fixed input copper.
            origin: Origin::Authored,
            flash: Flash::All,
        });
    }
    // No via fits beside the QFN's signal pads: once SIG's track runs round SIG2's pad, only
    // rip-up and reroute connects SIG2.
    let id = b.alloc_id();
    b.keepouts.push(BoardKeepout {
        id,
        area: PolySet::from_ring_nonzero(vec![at(12.0, 10.7), at(13.5, 10.7), at(13.5, 11.8), at(12.0, 11.8)]),
        layers: LayerSet::of(&[LayerId::F_CU, LayerId::B_CU]),
        forbid: Forbid::VIAS,
        reason: "no vias beside the QFN".into(),
        origin: Origin::Authored,
        z: None,
    });
    b.reindex();
    b
}

/// Fab's guard of the lift's connectivity check: a GND pour on F.Cu (0.5 mm minimum width) whose
/// two pads join only through the strip between a copper keepout's end (x 12 mm) and SIG, which
/// runs from S1 to S2 across the board's height (no pour passes its pads at the edges). SIG's
/// straight line (x 12.3 mm) leaves no strip the pour can fill; the test's grid form bows away.
/// G2 sits in a `track_fence`, so no pour path joins it and nothing keeps SIG off the strip: the
/// cut is one the relax makes whatever pour paths it lays.
pub fn guard_pour_squeeze() -> Board {
    let mut b = board("guard_pour_squeeze", 24.0, 24.0, 0.0, 2);
    let gnd = net(&mut b, "GND", "Default");
    let sig = net(&mut b, "SIG", "Default");
    for (refdes, x, y, size, n) in [
        ("G1", 5.0, 5.0, 1.0, gnd),
        ("G2", 5.0, 19.0, 1.0, gnd),
        ("S1", 12.3, 1.4, 1.6, sig),
        ("S2", 12.3, 22.6, 1.6, sig),
    ] {
        place(&mut b, refdes, "TestPad", at(x, y), vec![rect("1", at(x, y), P::mm(size, size), n)]);
    }
    let id = b.alloc_id();
    b.keepouts.push(BoardKeepout {
        id,
        area: PolySet::from_ring_nonzero(vec![at(0.0, 11.5), at(12.0, 11.5), at(12.0, 12.5), at(0.0, 12.5)]),
        layers: LayerSet::of(&[LayerId::F_CU]),
        forbid: Forbid(Forbid::TRACKS.0 | Forbid::VIAS.0 | Forbid::ZONES.0),
        reason: "wall".into(),
        origin: Origin::Authored,
        z: None,
    });
    track_fence(&mut b, 5.0, 19.0);
    let id = b.alloc_id();
    b.zones.push(Zone {
        id,
        net: Some(gnd),
        name: "GND".into(),
        layers: LayerSet::of(&[LayerId::F_CU]),
        area: Area::Board,
        priority: 0,
        // spokes wider than the minimum width, or the fill drops them and no pad joins the pour
        settings: ZoneSettings {
            min_width: mm(0.5),
            thermal: ThermalRule { gap: mm(0.3), spoke: mm(0.6), angle: Angle::ZERO },
            ..ZoneSettings::default()
        },
        locked: false,
        origin: Origin::Authored,
    });
    b.reindex();
    b
}

/// Fab's guard of a partly kept relax: `guard_pour_squeeze`'s board and nets (GND pads G1 G2 join
/// only through the strip SIG's straight line squeezes shut, G2 in its track fence) plus GND pads G3 G4 above the wall,
/// which a grid track joins, and net OTH (pads O1 O2) on the board's far side. The test's grid
/// forms bow, so the relax would move each of them.
pub fn guard_pour_partial() -> Board {
    pour_partial("guard_pour_partial", (19.0, 6.0), (19.0, 18.0))
}

/// Fab's guard of the relax's hold: `guard_pour_partial` with OTH's pads O1 O2 beside S1 and S2,
/// so that OTH's straight line runs 0.2 mm from the line of SIG's grid form: relaxed, OTH moves
/// into the room SIG's relaxed form leaves, and collides with SIG's grid form when SIG goes back
/// to it.
pub fn guard_pour_hold() -> Board {
    pour_partial("guard_pour_hold", (14.7, 1.4), (14.7, 22.6))
}

/// Track and via keepouts fencing in the 6 mm square around (`x`, `y`) mm (0.5 mm walls, closed).
/// They leave zones free, so the fill runs over the fence and joins a pad inside it to the pour
/// outside with copper of the full minimum width. The forest keeps its paths out of track keepouts
/// (they pass `PourTest::reach`), so no pour path crosses the fence: such a pad is a start-form
/// join the forest cannot protect (`pour_unprotected`).
fn track_fence(b: &mut Board, x: f64, y: f64) {
    let walls = [
        vec![at(x - 3.0, y - 3.0), at(x - 2.5, y - 3.0), at(x - 2.5, y + 3.0), at(x - 3.0, y + 3.0)],
        vec![at(x + 2.5, y - 3.0), at(x + 3.0, y - 3.0), at(x + 3.0, y + 3.0), at(x + 2.5, y + 3.0)],
        vec![at(x - 3.0, y + 2.5), at(x + 3.0, y + 2.5), at(x + 3.0, y + 3.0), at(x - 3.0, y + 3.0)],
        vec![at(x - 3.0, y - 3.0), at(x + 3.0, y - 3.0), at(x + 3.0, y - 2.5), at(x - 3.0, y - 2.5)],
    ];
    for ring in walls {
        let id = b.alloc_id();
        b.keepouts.push(BoardKeepout {
            id,
            area: PolySet::from_ring_nonzero(ring),
            layers: LayerSet::of(&[LayerId::F_CU]),
            forbid: Forbid(Forbid::TRACKS.0 | Forbid::VIAS.0),
            reason: "fence".into(),
            origin: Origin::Authored,
            z: None,
        });
    }
}

fn pour_partial(name: &str, o1: (f64, f64), o2: (f64, f64)) -> Board {
    let mut b = board(name, 24.0, 24.0, 0.0, 2);
    let gnd = net(&mut b, "GND", "Default");
    let sig = net(&mut b, "SIG", "Default");
    let oth = net(&mut b, "OTH", "Default");
    for (refdes, x, y, size, n) in [
        ("G1", 5.0, 5.0, 1.0, gnd),
        ("G2", 5.0, 19.0, 1.0, gnd),
        ("G3", 3.0, 8.0, 1.0, gnd),
        ("G4", 9.0, 8.0, 1.0, gnd),
        ("S1", 12.3, 1.4, 1.6, sig),
        ("S2", 12.3, 22.6, 1.6, sig),
        ("O1", o1.0, o1.1, 1.0, oth),
        ("O2", o2.0, o2.1, 1.0, oth),
    ] {
        place(&mut b, refdes, "TestPad", at(x, y), vec![rect("1", at(x, y), P::mm(size, size), n)]);
    }
    let id = b.alloc_id();
    b.keepouts.push(BoardKeepout {
        id,
        area: PolySet::from_ring_nonzero(vec![at(0.0, 11.5), at(12.0, 11.5), at(12.0, 12.5), at(0.0, 12.5)]),
        layers: LayerSet::of(&[LayerId::F_CU]),
        forbid: Forbid(Forbid::TRACKS.0 | Forbid::VIAS.0 | Forbid::ZONES.0),
        reason: "wall".into(),
        origin: Origin::Authored,
        z: None,
    });
    track_fence(&mut b, 5.0, 19.0);
    let id = b.alloc_id();
    b.zones.push(Zone {
        id,
        net: Some(gnd),
        name: "GND".into(),
        layers: LayerSet::of(&[LayerId::F_CU]),
        area: Area::Board,
        priority: 0,
        settings: ZoneSettings {
            min_width: mm(0.5),
            thermal: ThermalRule { gap: mm(0.3), spoke: mm(0.6), angle: Angle::ZERO },
            ..ZoneSettings::default()
        },
        locked: false,
        origin: Origin::Authored,
    });
    b.reindex();
    b
}

/// Fab's guard of a pour the relax would cut: GND pad P sits in a pocket that unnetted copper
/// walls on three sides; its pour reaches the board's GND (pad G2) only through the 2 mm mouth.
/// SIG1's and SIG2's pads A1, A2 lie deeper in the pocket, 0.65 mm and 1.35 mm across the mouth,
/// their far pads B1, B2 straight out beyond it. Straight 0.25 mm tracks leave gaps of 0.525,
/// 0.45 and 0.525 mm in the mouth, each under the 0.6 mm the fill needs (twice the 0.2 mm gap
/// plus its 0.2 mm minimum width): together they close the mouth, each alone does not.
pub fn guard_pour_pocket() -> Board {
    let mut b = board("guard_pour_pocket", 24.0, 24.0, 0.0, 2);
    let gnd = net(&mut b, "GND", "Default");
    let s1 = net(&mut b, "SIG1", "Default");
    let s2 = net(&mut b, "SIG2", "Default");
    // the walls: top, sides, and the two shoulders that narrow the pocket to its mouth
    let walls = [((11.0, 2.5), (8.0, 1.0)), ((7.5, 6.0), (1.0, 6.0)), ((14.5, 6.0), (1.0, 6.0)),
        ((8.5, 9.75), (3.0, 1.5)), ((13.5, 9.75), (3.0, 1.5))];
    let lands = walls.iter().enumerate()
        .map(|(i, &((x, y), (w, h)))| rect(&(i + 1).to_string(), at(x, y), P::mm(w, h), None)).collect();
    place(&mut b, "W1", "Wall", at(11.0, 6.0), lands);
    for (refdes, x, y, size, n) in [
        ("P", 9.2, 7.0, 1.0, gnd),
        ("G2", 4.0, 18.0, 1.0, gnd),
        ("A1", 10.65, 4.5, 0.4, s1),
        ("A2", 11.35, 4.5, 0.4, s2),
        ("B1", 10.65, 14.0, 0.4, s1),
        ("B2", 11.35, 14.0, 0.4, s2),
    ] {
        place(&mut b, refdes, "TestPad", at(x, y), vec![rect("1", at(x, y), P::mm(size, size), n)]);
    }
    let id = b.alloc_id();
    b.zones.push(Zone {
        id,
        net: Some(gnd),
        name: "GND".into(),
        layers: LayerSet::of(&[LayerId::F_CU]),
        area: Area::Board,
        priority: 0,
        settings: ZoneSettings::default(),
        locked: false,
        origin: Origin::Authored,
    });
    b.reindex();
    b
}

/// Fab's guard of the relaxer's view of an unplated hole: SIG's pads S1 and S2 face each other on
/// F.Cu across H1, a post's 3 mm unplated hole (no copper, no net) on their straight line.
pub fn guard_npth() -> Board {
    let mut b = board("guard_npth", 24.0, 24.0, 0.0, 2);
    let sig = net(&mut b, "SIG", "Default");
    for (refdes, x) in [("S1", 4.0), ("S2", 20.0)] {
        place(&mut b, refdes, "TestPad", at(x, 12.0), vec![rect("1", at(x, 12.0), P::mm(1.0, 1.0), sig)]);
    }
    // as an unplated pad is made: no copper, its land the hole
    let (mut hole, _) = thru("", at(12.0, 12.0), 3.0, 3.0, None);
    hole.kind = PadKind::Npth;
    hole.stack.copper = LayerSet::EMPTY;
    hole.stack.front = PadShape::Circle { d: mm(3.0) };
    place(&mut b, "H1", "Post", at(12.0, 12.0), vec![(hole, None)]);
    b.reindex();
    b
}

/// Fab's guard of the lift's track ids: net A (pads A1 A2) and net B (pads B1 B2 B3). The test's
/// grid form gives A a loop of two tracks (the longer one last, so it has the highest port id) and
/// B a host from B1 to B2 with a branch from B3 joining its middle: the net simplifier drops A's
/// longer track, then splits B's host at the junction, and the new chain takes the dropped id.
pub fn guard_id_reuse() -> Board {
    let mut b = board("guard_id_reuse", 34.0, 20.0, 0.0, 2);
    let a = net(&mut b, "A", "Default");
    let bn = net(&mut b, "B", "Default");
    for (refdes, x, y, n) in [
        ("A1", 3.0, 5.0, a),
        ("A2", 13.0, 5.0, a),
        ("B1", 15.0, 5.0, bn),
        ("B2", 25.0, 10.0, bn),
        ("B3", 30.0, 5.0, bn),
    ] {
        place(&mut b, refdes, "TestPad", at(x, y), vec![rect("1", at(x, y), P::mm(1.0, 1.0), n)]);
    }
    b.reindex();
    b
}
