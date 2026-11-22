use fab_elec_board::Factories;
use fab_elec_circuit::{CheckKind, QtyUnit};
use fab_elec_core::Severity;
use fab_elec_design::engines;
use fab_elec_fixtures::ledctl_eval;
use fab_elec_lang::UnitState;

#[test]
fn ledctl_documents_build_the_controller() {
    let evaluated = ledctl_eval();
    assert_eq!(evaluated.units.len(), 1, "the controller documents evaluate one unit");
    assert_eq!(evaluated.units[0].name, "ledctl", "the evaluated unit is named ledctl");
    assert_eq!(evaluated.units[0].state, UnitState::Built, "the controller unit builds");
    let design = &evaluated.design;
    assert_eq!(design.units.len(), 1, "the controller design contains one unit");
    let unit = &design.units[0];
    assert_eq!(unit.name, "ledctl", "the design unit is named ledctl");
    let board = unit.board.as_ref().expect("the controller unit has a board");
    let circuit = &unit.circuit;
    assert_eq!(board.placed.len(), 30, "the controller board has 30 placements");
    assert_eq!(board.tracks.len(), 8, "the controller board has 8 tracks");
    assert_eq!(board.vias.len(), 2, "the controller board has 2 vias");
    assert_eq!(board.zones.len(), 1, "the controller board has 1 zone");
    assert_eq!(board.holes.len(), 2, "the controller board has 2 mounting holes");
    assert_eq!(board.placed.len(), circuit.comps.len(), "every placement has a component");
    for comp in &circuit.comps {
        assert_eq!(
            board.placed.iter().filter(|placed| placed.comp == comp.id).count(),
            1,
            "component {} is placed exactly once",
            comp.path
        );
    }
    let resistance = circuit
        .comp("lamps[0].r")
        .and_then(|comp| comp.value.as_ref())
        .and_then(|value| value.qty)
        .map(|qty| (qty.si, qty.unit));
    assert_eq!(
        resistance,
        Some((82.0, QtyUnit::Ohm)),
        "the green LED curve at 5 mA with a 25 ohm sink selects an E24 82 ohm resistor"
    );
    let factories = Factories::new();
    let errors: Vec<_> = engines::violations(&engines::full_with(design, &factories))
        .into_iter()
        .filter(|finding| finding.severity == Severity::Error && finding.kind != CheckKind::Unconnected)
        .collect();
    assert!(errors.is_empty(), "the controller has no error-severity findings besides its ratsnest (it is left unrouted on purpose): {errors:#?}");
    assert_eq!(
        fab_elec_lang::write(board, circuit).as_bytes(),
        include_bytes!("../ledctl.pcb.splash"),
        "the checked-in board document is the canonical writer's fixed point"
    );
}
