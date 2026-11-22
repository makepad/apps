//! ledavr's board: ledctl's LED row, button, USB-C and outline where ledctl has them, the ATmega328P
//! and its parts in the old MCU's area, every reference label beside its part and every net routed.
//!
//! How it was made: the placement was written by hand, keeping ledctl's eight anode tracks, its two
//! GND vias and its GND pour on both layers. The reference labels are key A's plan in Fab's board
//! view (`arrange_references`), applied as `SetField` label ops. The other tracks and vias are the
//! route worker's last proposal for the requests Fab's Route action sends (every net, the organic
//! method on the grid engine, default effort), applied whole. Each step was written with the
//! canonical writer.

use fab_elec_board::Factories;
use fab_elec_design::{engines, Design};
use fab_elec_fixtures::ledctl;
use fab_elec_lang::{evaluate, std_libs, Sources, UnitState};

fn ledavr() -> Design {
    let mut sources = Sources::new();
    for (file, text) in [
        ("ledavr.ckt.splash", include_str!("../ledavr.ckt.splash")),
        ("ledavr.pcb.splash", include_str!("../ledavr.pcb.splash")),
        ("parts/avr/ATMEGA328P_TQFP32.part.splash", include_str!("../parts/avr/ATMEGA328P_TQFP32.part.splash")),
    ] {
        sources.add(file, text).unwrap_or_else(|e| panic!("{file}: an electronics document: {e:?}"));
    }
    sources.ensure_system().expect("ledavr has a system document");
    let evaluated = evaluate(&sources, &std_libs()).unwrap_or_else(|diags| panic!("ledavr evaluates: {diags:?}"));
    let unit = evaluated.units.iter().find(|u| u.name == "ledavr").expect("ledavr has a unit");
    assert_eq!(unit.state, UnitState::Built, "ledavr builds");
    evaluated.design
}

#[test]
fn ledavr_board_is_routed_on_ledctls_led_row_with_clear_labels() {
    let design = ledavr();
    let unit = design.units.iter().find(|u| u.name == "ledavr").expect("the ledavr unit lowers");
    let board = unit.board.as_ref().expect("ledavr has a board");

    let ctl = ledctl();
    let ctl_board = ctl.units[0].board.as_ref().expect("ledctl has a board");
    assert_eq!(board.outline, ctl_board.outline, "ledavr keeps ledctl's outline");
    let pose = |b: &fab_elec_board::Board, path: &str| {
        b.placed.iter().find(|p| p.path == path).map(|p| (p.at, p.rot, p.side)).unwrap_or_else(|| panic!("{path} is placed"))
    };
    let kept = (0..8).flat_map(|i| [format!("lamps[{i}].d"), format!("lamps[{i}].r")])
        .chain(["btn.sw", "usb.j", "usb.cc[0]", "usb.cc[1]"].map(String::from));
    for path in kept {
        assert_eq!(pose(board, &path), pose(ctl_board, &path), "{path} sits where ledctl has it, to the nanometre");
    }
    assert_eq!(board.placed.len(), unit.circuit.comps.len(), "every component is placed");

    let findings = engines::violations(&engines::full_with(&design, &Factories::new()));
    assert!(findings.is_empty(), "routed ledavr has no finding at all, an unrouted net included: {findings:#?}");
    assert_eq!(
        fab_elec_lang::write(board, &unit.circuit).as_bytes(),
        include_bytes!("../ledavr.pcb.splash"),
        "the checked-in board document is the canonical writer's fixed point"
    );
}
