use fab_elec_core::{Canon, Writer};
use fab_elec_lang::{blocks_alone, catalog, evaluate, std_libs, Sources};
use fab_elec_parts::VRange;

#[test]
fn planned_block_evaluates_without_build_and_preserves_port_budgets() {
    let libs = std_libs();
    let mut src = Sources::new();
    src.add("device.blocks.splash", r#"let e = mod.elec
let supply = e.block({name: "Supply" planned: true role: "Regulator"
    ports: {out: e.port(@power_out, {volts: 3.3V current: 300mA})}})
let load = e.block({name: "Load" planned: true ports: {s: e.port(@in)}
    build: fn(p, o) { let r = e.r(1kohm) }})
let plain = e.block({name: "Plain" ports: {} build: fn(p, o) {}})
"#).unwrap();
    src.add("device.ckt.splash", r#"use mod.project.*
let e = mod.elec
let rail = e.net({})
let source = supply({out: rail})
let sink = load({s: rail})
let built = plain({})
"#).unwrap();
    let ev = evaluate(&src, &libs).expect("planned and implemented blocks can share a circuit");
    let c = &ev.design.units[0].circuit;
    let def = &c.block_defs[0];
    assert!(def.planned, "the evaluated definition lost its planned status");
    assert_eq!(def.role.as_deref(), Some("Regulator"), "the block lost its role");
    assert_eq!(def.ports[0].volts, Some(VRange::fixed(3.3)), "the output lost its voltage");
    assert_eq!(def.ports[0].current, Some(0.3), "the output lost its current budget in amperes");
    assert_eq!(c.comps.iter().map(|c| c.path.as_str()).collect::<Vec<_>>(), ["sink.r"],
        "only the instance with an implementation should produce a component");

    let plain = &c.block_defs[2];
    let mut old = Writer::hasher();
    plain.id.canon(&mut old);
    plain.name.canon(&mut old);
    plain.ports.canon(&mut old);
    plain.params.canon(&mut old);
    plain.sheet.canon(&mut old);
    plain.template.canon(&mut old);
    assert_eq!(plain.content_hash(), old.finish(), "existing block definitions must keep their six-field hash");
    let mut extended = plain.clone();
    extended.planned = true;
    let planned = extended.content_hash();
    extended.planned = false;
    extended.role = Some("Regulator".into());
    assert_ne!(extended.content_hash(), planned, "role and planned must have distinct extension tags");

    let catalog = catalog(&libs, &ev);
    let entries: Vec<_> = catalog.entries.iter().filter(|entry| entry.lib.is_empty() && entry.name != "plain").collect();
    let minis = blocks_alone(ev.defs.source(), &entries);
    assert_eq!(minis.len(), 2, "both planned definitions must yield catalog miniatures");
    for mini in minis {
        let mini = mini.expect("a planned architecture has a miniature even without a builder");
        assert!(mini.circuit.comps.is_empty(), "a planned miniature must skip the builder");
        assert_eq!(mini.circuit.tree.len(), 2, "the miniature must retain the planned instance");
        assert_eq!(mini.circuit.nets.len(), 1, "the miniature must retain its port's net");
    }
}

#[test]
fn non_planned_block_still_requires_build_and_connected_ports() {
    let libs = std_libs();
    for planned in ["", "planned: false"] {
        for (build, instantiate, expected) in [
            ("", "", "build: expected fn(p, o)"),
            ("build: fn(p, o) {}", "let b = cell({})", "port input of Cell is not connected"),
        ] {
            let mut src = Sources::new();
            src.add("device.ckt.splash", format!(
                "let e = mod.elec\nlet cell = e.block({{name: \"Cell\" {planned} ports: {{input: e.port(@in)}} {build}}})\n{instantiate}\n"
            )).unwrap();
            let err = evaluate(&src, &libs).expect_err("a realized block must still have a builder and connected ports");
            assert!(err.iter().any(|d| d.message.contains(expected)), "the original refusal {expected:?} was lost: {err:?}");
        }
    }
}

#[test]
fn planned_block_allows_omitted_port_for_open_port_check() {
    let mut src = Sources::new();
    src.add("device.ckt.splash", r#"let e = mod.elec
let cell = e.block({name: "Cell" planned: true ports: {a: e.port(@in) b: e.port(@out)}})
let block = cell({b: nil})
"#).unwrap();
    let ev = evaluate(&src, &std_libs()).expect("omitted and nil planned ports must reach ERC");
    let ports = &ev.design.units[0].circuit.tree[1].ports;
    assert_eq!(ports.len(), 2, "both open ports must survive lowering");
    for name in ["a", "b"] {
        assert!(ports.iter().any(|(n, nets)| n == name && nets.is_empty()), "open port {name} lost its empty binding");
    }
}
