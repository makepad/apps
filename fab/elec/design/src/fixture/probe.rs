//! A small Rust probe board that mates with the document-backed controller.

use super::build::{board_unit, circuit, hole, place};
use super::parts;
use crate::Unit;
use fab_elec_board::Board;
use fab_elec_circuit::{
    Block, BlockDefId, BlockSource, Circuit, Component, Net, NetKind, QtyUnit, Quantity, Value,
};
use fab_elec_core::{BlockId, BoardId, ClassId, CompId, Contour, NetId, Side, P};
use fab_elec_parts::{Fields, PartDef, VRange};
use std::sync::Arc;

/// The probe unit (board 1), ready to mate with the controller's debug header.
pub fn probe() -> Unit {
    let mut c = circuit(BoardId(1), "probe");
    let gnd = net(&mut c, "GND", NetKind::Ground, None, ClassId(0));
    let v33 = net(&mut c, "3V3", NetKind::Power, Some(VRange::fixed(3.3)), ClassId(0));
    let swdio = net(&mut c, "SWDIO", NetKind::Signal, None, ClassId(0));
    let swclk = net(&mut c, "SWCLK", NetKind::Signal, None, ClassId(0));
    let nrst = net(&mut c, "NRST", NetKind::Signal, None, ClassId(0));
    let anode = net(&mut c, "led.A", NetKind::Signal, None, ClassId(0));
    let led = block(&mut c, "led", None, &[]);
    component(
        &mut c, "j", "J1", parts::header(5, 1.27), None,
        &[("1", v33), ("2", swdio), ("3", swclk), ("4", nrst), ("5", gnd)], &[], BlockId(0),
    );
    component(&mut c, "led.r", "R1", parts::resistor("0402"), quantity("1k", 1000.0, QtyUnit::Ohm), &[("1", v33), ("2", anode)], &[], led);
    component(&mut c, "led.d", "D1", parts::led("0603", "red"), label("red"), &[("A", anode), ("K", gnd)], &[], led);
    let mut b = board(&c, 20.0, 15.0);
    for (path, x, y, rot) in [("j", 10.0, 10.0, 90.0), ("led.r", 5.0, 4.0, 0.0), ("led.d", 14.0, 4.0, 0.0)] {
        place(&mut b, &c, path, P::mm(x, y), rot, Side::Top);
    }
    hole(&mut b, "mount1", P::mm(3.0, 12.0), 2.2);
    b.snap_anchors();
    b.reindex();
    board_unit(c, b)
}

fn net(c: &mut Circuit, name: &str, kind: NetKind, volts: Option<VRange>, class: ClassId) -> NetId {
    let id = NetId(c.nets.len() as u32);
    c.nets.push(Net { id, name: name.into(), class, volts, kind, scope: None });
    id
}

fn block(c: &mut Circuit, path: &str, def: Option<BlockDefId>, ports: &[(&str, &[NetId])]) -> BlockId {
    let id = BlockId(c.tree.len() as u32);
    let params = def.map(|id| c.block_defs[id.0 as usize].params.clone()).unwrap_or_default();
    c.tree.push(Block {
        id,
        path: path.into(),
        def,
        parent: Some(BlockId(0)),
        ports: ports.iter().map(|&(name, nets)| (name.into(), nets.to_vec())).collect(),
        params,
        source: BlockSource::Code,
    });
    id
}

fn quantity(text: &str, si: f64, unit: QtyUnit) -> Option<Value> {
    Some(Value { text: text.into(), qty: Some(Quantity { si, unit }), tol: None })
}

fn label(text: &str) -> Option<Value> {
    Some(Value { text: text.into(), qty: None, tol: None })
}

fn component(
    c: &mut Circuit,
    path: &str,
    refdes: &str,
    part: Arc<PartDef>,
    value: Option<Value>,
    connections: &[(&str, NetId)],
    nc: &[&str],
    block: BlockId,
) -> CompId {
    // Refdes prefixes follow part kind (R, C, D, U, J, SW), numbered in component order.
    let id = CompId(c.comps.len() as u32);
    let mut conns: Vec<_> = connections
        .iter()
        .map(|&(name, net)| (part.pin(name).expect("fixture connection names a part pin"), net))
        .collect();
    conns.sort_by_key(|&(pin, _)| pin);
    let nc = nc.iter().map(|name| part.pin(name).expect("fixture NC names a part pin")).collect();
    c.comps.push(Component {
        id, path: path.into(), refdes: refdes.into(), ref_kind: Default::default(), part, value, conns, nc, block, dnp: false, fields: Fields::default(),
    });
    id
}

fn board(c: &Circuit, width: f64, height: f64) -> Board {
    let mut board = Board::new(c.unit, &c.name);
    board.outline = vec![Contour::rect(P::ZERO, P::mm(width, height), 0)];
    board.mirror(c);
    board
}
