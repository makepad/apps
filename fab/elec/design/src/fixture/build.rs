//! Shared builders for the Rust board fixtures.

use crate::{Unit, UnitKind};
use fab_elec_board::{Board, Hole, Origin, Placed};
use fab_elec_circuit::{Block, BlockSource, Circuit, ClassRules, NetClass};
use fab_elec_core::{mm, Angle, BlockId, BoardId, ClassId, Side, P};
use std::sync::Arc;

pub(super) fn circuit(unit: BoardId, name: &str) -> Circuit {
    Circuit {
        unit,
        name: name.into(),
        classes: vec![NetClass { id: ClassId(0), name: "default".into(), rules: ClassRules::default() }],
        tree: vec![Block {
            id: BlockId(0), path: String::new(), def: None, parent: None, ports: vec![], params: vec![], source: BlockSource::Code,
        }],
        ..Default::default()
    }
}

pub(super) fn place(b: &mut Board, c: &Circuit, path: &str, at: P, rot: f64, side: Side) {
    let comp = c.comp(path).expect("fixture places a circuit component");
    let footprint = comp.part.footprint.as_ref().expect("fixture part has a footprint").clone();
    let pad_nets = comp
        .part
        .pad_pins()
        .into_iter()
        .map(|pin| pin.and_then(|pin| comp.conns.iter().find(|&&(p, _)| p == pin).map(|&(_, net)| net)))
        .collect();
    let id = b.alloc_id();
    b.placed.push(Placed {
        id,
        comp: comp.id,
        path: comp.path.clone(),
        refdes: comp.refdes.clone(),
        footprint,
        pad_nets,
        at,
        rot: Angle::deg(rot),
        side,
        locked: false,
        origin: Origin::Authored,
        body_z: Placed::band_of(comp.part.body.as_ref()),
        ref_label: None, value_label: None, legend: None,
        value: None,
    });
}

pub(super) fn hole(b: &mut Board, name: &str, at: P, drill_mm: f64) {
    let id = b.alloc_id();
    b.holes.push(Hole {
        id, name: name.into(), at, drill: mm(drill_mm), pad: None, net: None, plated: false, origin: Origin::Authored,
    });
}

pub(super) fn board_unit(circuit: Circuit, board: Board) -> Unit {
    Unit {
        id: circuit.unit,
        name: circuit.name.clone(),
        kind: UnitKind::Board,
        circuit: Arc::new(circuit),
        supplied: Vec::new(),
        board: Some(Arc::new(board)),
        factory: None,
    }
}
