use fab_elec_circuit::CheckKind;
use fab_elec_core::{NetRef, Sel};
use fab_elec_design::engines::{full, update_held, DesignResults};
use fab_elec_lang::{evaluate, evaluate_from, std_libs, Sources, NO_CANCEL};
use std::sync::Arc;

#[test]
fn bench_supplies_drive_only_their_boards_rails() {
    let mut sources = Sources::new();
    let circuit = r#"let e = mod.elec
let va = e.net({name: "VA" kind: @power})
let en = e.net({name: "EN"})
let gnd = e.net({name: "GND" kind: @ground})
let reg = e.part(mod.std.ldo_3v3(), {pins: {VIN: va EN: en GND: gnd} nc: [@VOUT @NC]})
"#;
    sources.add("main.ckt.splash", circuit).expect("add the bench's circuit");
    sources.add("other.ckt.splash", circuit).expect("add another board with the same nets");
    let bench = sources.add("rails.bench.splash", r#"bench("bringup", {circuit: "main"}, fn(b, c) {
    b.supply(c.net("EN"), {v: 5V})
    b.supply(c.net("VA"), {v: 5V})
})
bench("repeat", {circuit: "main"}, fn(b, c) {
    b.supply(c.net("VA"), {v: 5V})
})
"#).expect("add both benches");

    let libs = std_libs();
    let powered = evaluate(&sources, &libs).expect("evaluate both boards and the bench document");
    let main = powered.design.unit_by_name("main").expect("main board was built");
    let other = powered.design.unit_by_name("other").expect("other board was built");
    let va = main.circuit.net("VA").expect("main has VA").id;
    let en = main.circuit.net("EN").expect("main has EN").id;
    let mut expected = [va, en];
    expected.sort_unstable();
    assert_eq!(main.supplied, expected, "supplies from both benches are sorted and VA is counted once");
    assert!(other.supplied.is_empty(), "equal net ids on a different board receive no bench supply");
    let missing = |results: &DesignResults, board| {
        let item = Sel::Net(NetRef { board, net: va });
        results.erc.iter().any(|v| v.kind == CheckKind::PowerNotDriven && v.items.contains(&item))
    };
    let results = full(&powered.design);
    assert!(!missing(&results, main.id), "on load the bench drives the main board's VA");
    assert!(missing(&results, other.id), "the other board still lacks a driver for VA");

    sources.set_present(bench, false).expect("remove only the bench document");
    let unpowered = evaluate_from(Some(&powered), &sources, &libs, &NO_CANCEL).expect("reevaluate without the bench");
    let unit = unpowered.design.unit(main.id).expect("main still exists");
    assert!(Arc::ptr_eq(&main.circuit, &unit.circuit), "the bench-only edit reuses the circuit");
    assert!(unit.supplied.is_empty(), "removing the bench clears its supplies from the reused unit");
    let refreshed = update_held(&results, &unpowered.design, &[], &Default::default());
    assert!(missing(&refreshed, main.id), "ERC reports the disconnected rail even without a board delta");
}
