//! The flagship single-board computer's mechanical skeleton: its outline, mounting holes and
//! the parts a case reads (bodies at true size and height, ports), placed. The circuit (nets,
//! passives, block definitions), the six-layer stackup and the processor's and memory's pads
//! (the `bga` generator) are added onto this fixture later.

use super::build::{board_unit, circuit, hole, place};
use super::parts;
use crate::{Assembly, Design, Frame, Mount};
use fab_elec_board::Board;
use fab_elec_circuit::{Block, BlockSource, Circuit, Component};
use fab_elec_core::{mm, BlockId, BoardId, CompId, Contour, Side, Xform3, P};
use fab_elec_parts::{Fields, PartDef};
use std::sync::Arc;

pub fn sbc() -> Design {
    let circuit = sbc_circuit();
    let board = sbc_board(&circuit);
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

fn sbc_circuit() -> Circuit {
    let mut c = circuit(BoardId(0), "sbc");
    let power = block(&mut c, "power");
    let soc = block(&mut c, "soc");
    let ddr = block(&mut c, "ddr");
    let eth = block(&mut c, "eth");
    let hdmi = block(&mut c, "hdmi");
    let usb = block(&mut c, "usb");
    let sd = block(&mut c, "sd");
    let gpio = block(&mut c, "gpio");
    let camera = block(&mut c, "camera");
    let display = block(&mut c, "display");
    let status = block(&mut c, "status");

    component(&mut c, "power.j", "J1", parts::usb_c_power(), power);
    component(&mut c, "hdmi.j", "J2", parts::hdmi_a(), hdmi);
    component(&mut c, "usb.j", "J3", parts::usb_a_stack(), usb);
    component(&mut c, "eth.j", "J4", parts::rj45_leds(), eth);
    component(&mut c, "sd.j", "J5", parts::microsd_push(), sd);
    component(&mut c, "gpio.j", "J6", parts::header_2x20(), gpio);
    component(&mut c, "camera.j", "J7", parts::fpc_side(15, 1.0), camera);
    component(&mut c, "display.j", "J8", parts::fpc_side(22, 0.5), display);
    component(&mut c, "status.sw", "SW1", parts::side_switch(), status);
    component(&mut c, "status.led[0]", "D1", parts::side_led("green"), status);
    component(&mut c, "status.led[1]", "D2", parts::side_led("red"), status);
    component(&mut c, "soc.u", "U1", parts::soc_bga(), soc);
    component(&mut c, "ddr.u", "U2", parts::memory_bga(), ddr);
    component(&mut c, "power.u", "U3", parts::pmic_qfn(), power);
    let inductor = parts::power_inductor();
    for i in 0..4 {
        component(&mut c, &format!("power.l[{i}]"), &format!("L{}", i + 1), inductor.clone(), power);
    }
    let bulk = parts::polymer_cap();
    for i in 0..2 {
        component(&mut c, &format!("power.c_bulk[{i}]"), &format!("C{}", i + 1), bulk.clone(), power);
    }
    c
}

fn sbc_board(c: &Circuit) -> Board {
    let mut b = board(c, 85.0, 56.0);
    // Bottom placement flips local X and Z before rotation. The card face at local +7 mm X
    // lands at board (0.3, 22.0, -0.925) mm, facing -X, with the body at x 0.3..14.3.
    // Positive rotation is counter-clockwise: the flat-cable mouths on local -Y turn to
    // +X at 90 degrees (camera, x 84.7) and -X at -90 degrees (display, x 0.3).
    for (path, x, y, rot, side) in [
        ("power.j", 12.0, 4.2, 0.0, Side::Top),
        ("hdmi.j", 27.0, 6.05, 0.0, Side::Top),
        ("usb.j", 75.95, 9.4, 0.0, Side::Top),
        ("eth.j", 74.95, 45.0, 0.0, Side::Top),
        ("sd.j", 7.3, 22.0, 0.0, Side::Bottom),
        ("gpio.j", 32.5, 52.5, 0.0, Side::Top),
        ("camera.j", 82.2, 26.5, 90.0, Side::Top),
        ("display.j", 2.55, 40.5, -90.0, Side::Top),
        ("status.sw", 40.0, 2.1, 0.0, Side::Top),
        ("status.led[0]", 47.0, 0.8, 0.0, Side::Top),
        ("status.led[1]", 50.0, 0.8, 0.0, Side::Top),
        ("soc.u", 34.0, 30.0, 0.0, Side::Top),
        ("ddr.u", 49.0, 30.0, 0.0, Side::Top),
        ("power.u", 17.0, 20.0, 0.0, Side::Top),
        ("power.l[0]", 9.5, 13.0, 0.0, Side::Top),
        ("power.l[1]", 9.5, 18.5, 0.0, Side::Top),
        ("power.l[2]", 9.5, 24.0, 0.0, Side::Top),
        ("power.l[3]", 23.5, 15.0, 0.0, Side::Top),
        ("power.c_bulk[0]", 17.0, 30.0, 0.0, Side::Top),
        ("power.c_bulk[1]", 17.0, 40.5, 0.0, Side::Top),
    ] {
        place(&mut b, c, path, P::mm(x, y), rot, side);
    }
    hole(&mut b, "mount1", P::mm(3.5, 3.5), 2.7);
    hole(&mut b, "mount2", P::mm(61.5, 3.5), 2.7);
    hole(&mut b, "mount3", P::mm(61.5, 52.5), 2.7);
    hole(&mut b, "mount4", P::mm(3.5, 52.5), 2.7);
    b.snap_anchors();
    b.reindex();
    b
}

pub(super) fn block(c: &mut Circuit, path: &str) -> BlockId {
    let id = BlockId(c.tree.len() as u32);
    c.tree.push(Block {
        id, path: path.into(), def: None, parent: Some(BlockId(0)), ports: vec![], params: vec![], source: BlockSource::Code,
    });
    id
}

pub(super) fn component(c: &mut Circuit, path: &str, refdes: &str, part: Arc<PartDef>, block: BlockId) -> CompId {
    let id = CompId(c.comps.len() as u32);
    c.comps.push(Component {
        id, path: path.into(), refdes: refdes.into(), ref_kind: Default::default(), part, value: None, conns: vec![], nc: vec![], block, dnp: false, fields: Fields::default(),
    });
    id
}

pub(super) fn board(c: &Circuit, width: f64, height: f64) -> Board {
    let mut board = Board::new(c.unit, &c.name);
    board.outline = vec![Contour::rect(P::ZERO, P::mm(width, height), mm(3.0))];
    board.mirror(c);
    board
}

