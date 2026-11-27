//! Probes placed and removed from the views: the bench a pane no host names one for probes into,
//! and the edit a removed probe's card makes.

use fab_elec_view::instruments::bench::BenchProbe;
use fab_elec_view::live::probe::{host_bench, remove_probe_op};
use fab_elec_view::{DesignSnapshot, ElecLive, StageFlags, StateOf, Tagged, ViewAction};
use makepad_widgets_core::Cx;
use std::sync::Arc;

fn ledavr() -> (fab_elec_lang::Sources, fab_elec_lang::Libs, fab_elec_lang::Evaluated) {
    let sources = fab_elec_fixtures::docs("ledavr").expect("ledavr's documents");
    let libs = fab_elec_lang::std_libs();
    let evaluated = fab_elec_lang::evaluate(&sources, &libs).expect("ledavr evaluates");
    (sources, libs, evaluated)
}

/// A pane without a bench of its own probes into the bench the electronics host runs: in Fab no
/// host names one, so without this a P or a probe drop on the sheet or the board did nothing.
#[test]
fn a_pane_probes_into_the_bench_the_host_runs() {
    let (sources, _, evaluated) = ledavr();
    let mut cx = Cx::new(Box::new(|_, _| {}));
    assert_eq!(host_bench(&cx), None, "without an electronics host there is no bench to probe into");
    let benches = Arc::new(fab_elec_lang::bench_views(&evaluated, &sources));
    let snap = DesignSnapshot {
        proposals: Default::default(),
        profiles: Arc::default(), profile_diags: Arc::default(),
        design: Arc::new(evaluated.design.clone()), preview: None, preview_tag: None, preview_by: None, created: vec![],
        last_commit: None, sheets: vec![], proposal: None, job: None, conn: vec![], rules: vec![], fills: vec![],
        sysnets: None, export: vec![], violations: Tagged { unit: None, of: StateOf::Committed, generation: 0, value: Arc::new(vec![]) },
        awaiting: Arc::new(vec![]), cases: Arc::default(), checking: vec![], last_refusal: None, stale: StageFlags::NONE, head: String::new(),
        benches, source_map: None, docs: None, catalog: None,
    };
    cx.set_global(ElecLive::new(Arc::new(snap)));
    assert_eq!(host_bench(&cx).as_deref(), Some("chase"), "the host's run bench (ledavr's chase) takes new probes");
}

/// A probe card's x (on the Bench, or on a canvas) removes the bench's channel from its document:
/// the next evaluation has no card for it, so it leaves every view, and the other cards stay.
#[test]
fn a_removed_probe_card_leaves_its_bench_document() {
    let (sources, libs, evaluated) = ledavr();
    let chase = evaluated.benches.iter().find(|bench| bench.name == "chase").expect("ledavr's chase bench");
    assert!(remove_probe_op("chase", &chase.probes, "VBUS").is_none(), "a net the bench reads but does not probe has no card to remove");
    let Some(ViewAction::Ops { ops, place, label }) = remove_probe_op("chase", &chase.probes, "LED0") else {
        panic!("the probed LED0 net's card has a removal edit");
    };
    assert_eq!(label, "Remove probe D1 PD6", "the undo step names the card by its label");
    assert!(place.is_empty(), "a removal places nothing");
    let change = fab_elec_lang::splice(&sources, &evaluated.map, &evaluated.design, &ops).expect("the removal splices");
    let mut edited = sources.clone();
    edited.apply(&change.edits).expect("the removal applies");
    let after = fab_elec_lang::evaluate(&edited, &libs).expect("the edited documents evaluate");
    let chase = after.benches.iter().find(|bench| bench.name == "chase").expect("the chase bench stays");
    assert_eq!(chase.state, fab_elec_lang::UnitState::Built, "the bench still builds");
    let (cards, _) = BenchProbe::from_bench(&chase.probes);
    assert_eq!(cards.iter().map(|card| card.net.as_str()).collect::<Vec<_>>(),
        ["LED1", "LED2", "LED3", "LED4", "LED5", "LED6", "LED7", "BTN"], "only the removed card's channel goes");
}
