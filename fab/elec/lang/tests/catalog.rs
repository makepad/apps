//! Catalog snapshots retain definitions and supply source templates for circuit insertion.

use fab_elec_circuit::{CircuitOp, End, NetKind};
use fab_elec_core::{Canon, Sel, Severity};
use fab_elec_design::{DesignOp, TextEdit};
use fab_elec_lang::{NO_CANCEL, blocks_alone, catalog, evaluate, evaluate_from, splice, std_libs, Catalog, CatalogEntry, CatalogKind, ProjectDefs, Sources, UnitState};
use fab_elec_parts::{PartKind, PinKind};
use std::sync::Arc;

const PART: &str = r#"let e = mod.elec
let f = e.f

// Four-pin sensor for the board.
// A second sentence is not a summary.
e.define("SENSOR_X", {
    kind: @ic prefix: "U"
    fields: {title: "Sensor original" mpn: "SENSOR_X"}
    symbol: f.ic_symbol("Sensor", {left: [["1", @in], ["VCC", @power_in]] right: [["OUT", @out], ["GND", @power_in]]})
})
"#;

const BLOCKS: &str = r#"let e = mod.elec

// Filter with a bus and
// a differential pair. More implementation detail.
let rc_filter = e.block({
    name: "RC filter"
    ports: {
        "in": e.port(@in) out: e.port(@out) gnd: e.port(@ground)
        data: e.port(@bidi, {width: 2}) pair: e.port(@pair)
    }
    params: {r: 1kohm}
    build: fn(p, o) { let r = e.r(o.r, p.in, p.out) }
})
let __private = e.block({name: "Private" ports: {} build: fn(p, o) {}})
let catalog = fn() {[ fn(a, b) { e.r(22ohm, a, b) } ]}
"#;

const BOARD: &str = r#"let e = mod.elec
let f = e.f
let gnd = e.net({name: "GND" kind: @ground})
let vcc = e.net({name: "VCC" kind: @power volts: 5V})
let sensor = e.part("SENSOR_X", {})
let local_block = e.block({name: "Circuit local" ports: {} build: fn(p, o) {}})
// The regulator block's default part belongs to this circuit, not the catalog.
e.define("LDO_3V3_SOT23-5", {
    kind: @regulator prefix: "U"
    symbol: f.ic_symbol("Regulator", {
        left: [["VIN", @power_in], ["EN", @in]]
        right: [["VOUT", @power_out]] bottom: [["GND", @power_in], ["NC", @no_connect]]
    })
})
"#;

const LIBRARY_PART: &str = r#"let e = mod.elec
// A part defined while the library loads.
let reference_part = e.define("LIB_REFERENCE", {
    kind: @ic prefix: ""
    fields: {mpn: "Library reference"}
    symbol: e.f.two_terminal([["A", @passive], ["B", @passive]])
})
let reference_alias = reference_part
"#;

fn entry<'a>(catalog: &'a Catalog, lib: &str, name: &str) -> &'a CatalogEntry {
    catalog.entries.iter().find(|entry| entry.lib == lib && entry.name == name)
        .unwrap_or_else(|| panic!("catalog must contain {lib}.{name}"))
}

#[test]
fn catalog_lists_library_and_project_definitions_and_reuses_its_arc() {
    let mut libs = std_libs();
    // Standard parts are generators; this load-time definition makes Arc reuse observable.
    libs.add("kit", "reference.part.splash", LIBRARY_PART).expect("the library fixture has a distinct name");
    let mut sources = Sources::new();
    let board = sources.add("board.ckt.splash", BOARD).expect("the circuit fixture has a valid name");
    let part_doc = sources.add("parts/sensor.part.splash", PART).expect("the part fixture has a valid name");
    sources.add("filters.blocks.splash", BLOCKS).expect("the block fixture has a valid name");
    let ev = evaluate(&sources, &libs).unwrap_or_else(|diags| panic!("the initial circuit must build: {diags:?}"));
    let before_fill = ProjectDefs::new(&sources, &libs);
    let cat = catalog(&libs, &ev);
    assert_eq!(&before_fill, ev.defs.as_ref(), "filling lazy state must preserve input equality");
    assert!(Arc::ptr_eq(&cat, &catalog(&libs.clone(), &ev)), "cloned library snapshots must share the catalog Arc");

    let standard: Vec<_> = cat.entries.iter().filter(|entry| entry.lib == "std" && entry.kind == CatalogKind::Block)
        .map(|entry| (entry.name.as_str(), entry.title.as_str())).collect();
    assert_eq!(standard, [
        ("button", "Button"), ("crystal_load", "Crystal load"), ("decoupling", "Decoupling"),
        ("divider", "Divider"), ("i2c_pullups", "I2C pull-ups"), ("ldo", "LDO"),
        ("led_driver", "LED driver"), ("reset_rc", "Reset RC"), ("usb_c_power", "USB-C power in"),
    ], "all nine standard blocks must retain their exported names and declared titles");
    let usb = entry(&cat, "std", "usb_c_power");
    assert_eq!(usb.summary, "Power in from a USB-C receptacle: 5 V sink, two CC pull-downs.",
        "the standard block must own its comment sentence");
    assert_eq!(usb.pins.iter().map(|pin| (pin.name.as_str(), pin.kind)).collect::<Vec<_>>(),
        [("vbus", PinKind::PowerOut), ("gnd", PinKind::PowerIn)], "block pins must retain port order and electrical kinds");
    assert_eq!(usb.defaults, [("rd".into(), "5.1kohm".into())], "quantity defaults must use source literals");
    assert_eq!(usb.stmt, "let usb_c_power = mod.std.usb_c_power({vbus: _n0 gnd: _n1})",
        "the standard block template must call its qualified export");
    assert!(usb.part.is_none(), "a block must not carry a part definition");

    let passive: Vec<_> = cat.entries.iter().filter(|entry| entry.lib == "std" && entry.kind == CatalogKind::Passive)
        .map(|entry| (entry.name.as_str(), entry.title.as_str())).collect();
    assert_eq!(passive, [("c 100nF 0402", "Capacitor"), ("led @green 0603", "LED"), ("r 10kohm 0402", "Resistor")],
        "std offers exactly three passive templates with their owned comment titles");
    let resistor = entry(&cat, "std", "r 10kohm 0402");
    assert_eq!(resistor.stmt, "let r = e.r(10kohm, _n0, _n1, {package: \"0402\"})",
        "the resistor template renames only parameter identifiers in its call");
    assert_eq!(resistor.pins.iter().map(|pin| (pin.name.as_str(), pin.kind)).collect::<Vec<_>>(),
        [("1", PinKind::Passive), ("2", PinKind::Passive)], "passive pins follow the function parameters");
    assert_eq!(resistor.defaults, [("value".into(), "10kohm".into()), ("package".into(), "\"0402\"".into())],
        "defaults retain source spelling and options field order");
    assert!(resistor.part.is_some(), "a passive offer retains its part definition");
    for (name, title) in [
        ("L_0805", "Inductor"), ("D_sod123", "Diode"), ("XTAL_3225", "Crystal, 3.2 x 2.5 mm"),
        ("PinHeader_1x4_P2.54mm", "Pin header, 1 x 4, 2.54 mm"), ("USB_C_POWER", "USB-C receptacle"),
        ("JACK_3.5MM_STEREO", "Audio jack, 3.5 mm stereo"), ("SW_TACT_4PAD", "Tact switch"), ("PWR_FLAG", "Power flag"),
    ] {
        let offered = entry(&cat, "std", name);
        assert_eq!(offered.kind, CatalogKind::Part, "{name} is a part offer");
        assert_eq!(offered.title, title, "{name} uses its offer's comment title");
    }
    assert_eq!(entry(&cat, "std", "PWR_FLAG").stmt, "let flg = e.part(mod.std.power_flag(), {pins: {\"1\": _n0}})",
        "a refdes prefix that is not an identifier (#FLG) still makes a binding that parses");
    let crystal = entry(&cat, "std", "XTAL_3225");
    assert_eq!(crystal.stmt, "let y = e.part(mod.std.crystal_part(\"3225\"), {pins: {\"1\": _n0 \"2\": _n1 GND: _n2}})",
        "the crystal template calls its maker and uses its real pin keys");
    assert!(cat.entries.iter().filter(|entry| entry.lib == "std" && entry.kind == CatalogKind::Part)
        .all(|entry| entry.stmt.contains("mod.std.")), "offer-made parts must never be inserted by name");
    assert!(!cat.entries.iter().any(|entry| entry.lib == "std" && ["R_0402", "C_0402", "LED_0603_green"].contains(&entry.name.as_str())),
        "parts made by passive offers do not acquire by-name entries");
    let project_passive = entry(&cat, "", "r 22ohm");
    assert_eq!(project_passive.kind, CatalogKind::Passive, "the project catalog adds its passive to the project section");

    let block = entry(&cat, "", "rc_filter");
    assert_eq!(block.kind, CatalogKind::Block, "the project export must be a block");
    assert_eq!(block.title, "RC filter", "the project block title must come from its definition");
    assert_eq!(block.summary, "Filter with a bus and a differential pair.", "summaries must join comments and stop at the first sentence");
    assert_eq!(block.pins.iter().map(|pin| (pin.name.as_str(), pin.kind)).collect::<Vec<_>>(), [
        ("in", PinKind::In), ("out", PinKind::Out), ("gnd", PinKind::PowerIn),
        ("data[0]", PinKind::Bidi), ("data[1]", PinKind::Bidi), ("pair.p", PinKind::Bidi), ("pair.n", PinKind::Bidi),
    ], "bus and pair members must expand in placeholder order");
    assert_eq!(block.stmt, "let rc_filter = mod.project.rc_filter({\"in\": _n0 out: _n1 gnd: _n2 data: [_n3 _n4] pair: [_n5 _n6]})",
        "project block templates must group bus and pair placeholders");
    assert_eq!(block.defaults, [("r".into(), "1kohm".into())], "project defaults must retain their quantities");

    let sensor = entry(&cat, "", "SENSOR_X");
    assert_eq!(sensor.kind, CatalogKind::Part, "a load-time part must be offered as a Part");
    assert_eq!(sensor.title, "Sensor original", "a title field must take precedence over mpn");
    assert_eq!(sensor.summary, "Four-pin sensor for the board.", "a bare define statement must own its comments");
    assert_eq!(sensor.stem, "u", "a part prefix must be lower-cased");
    assert_eq!(sensor.stmt, "let u = e.part(\"SENSOR_X\", {pins: {\"1\": _n0 VCC: _n1 OUT: _n2 GND: _n3}})",
        "the part template must include every pin and quote a numeric pin name");
    let definition = sensor.part.as_ref().expect("the part entry must carry its session definition Arc");
    assert_eq!(sensor.pins.iter().map(|pin| (&pin.name, pin.kind)).collect::<Vec<_>>(),
        definition.pins.iter().map(|pin| (&pin.name, pin.kind)).collect::<Vec<_>>(), "catalog part pins must match the definition in order");
    let hash = definition.content_hash();
    let mut bytes = [0; 8];
    bytes.copy_from_slice(&hash.0[..8]);
    assert_eq!(sensor.hash, u64::from_le_bytes(bytes), "part hashes must fold the canonical content hash in little endian order");
    assert!(sensor.defaults.is_empty(), "part templates have no parameter defaults");
    let library_part = entry(&cat, "kit", "LIB_REFERENCE");
    assert_eq!(library_part.title, "Library reference", "mpn must supply the title when title is absent");
    assert_eq!(library_part.stem, "u", "an empty prefix must use u");
    assert_eq!(cat.entries.iter().filter(|entry| entry.name == "LIB_REFERENCE").count(), 1,
        "multiple exports of one part index must produce only one entry");
    assert!(!cat.entries.iter().any(|entry| ["local_block", "LDO_3V3_SOT23-5", "__private"].contains(&entry.name.as_str())),
        "circuit-local definitions and unexported blocks must not enter the catalog");
    assert!(cat.entries.windows(2).all(|pair| (&pair[0].lib, &pair[0].name) <= (&pair[1].lib, &pair[1].name)),
        "catalog entries must be sorted by library and name");

    let end = sources.doc(board).expect("the circuit remains present").text.len() as u32;
    sources.apply(&[TextEdit { doc: board, span: end..end, text: "\n// Circuit-only edit.\n".into() }])
        .expect("the circuit comment append must apply");
    let circuit_edit = evaluate_from(Some(&ev), &sources, &libs, &NO_CANCEL).expect("the circuit comment must evaluate");
    assert!(Arc::ptr_eq(&ev.defs, &circuit_edit.defs), "a circuit-only change must reuse definition inputs");
    assert!(Arc::ptr_eq(&cat, &catalog(&libs, &circuit_edit)), "a circuit-only change must reuse the catalog Arc");

    sources.apply(&[TextEdit { doc: part_doc, span: 0..PART.len() as u32, text: PART.replace("Sensor original", "Sensor revised") }])
        .expect("the part title change must apply");
    let part_edit = evaluate_from(Some(&circuit_edit), &sources, &libs, &NO_CANCEL).expect("the changed part must evaluate");
    let changed = catalog(&libs, &part_edit);
    assert!(!Arc::ptr_eq(&ev.defs, &part_edit.defs), "a part text change must replace definition inputs");
    assert!(!Arc::ptr_eq(&cat, &changed), "a part text change must replace the catalog Arc");
    assert_eq!(entry(&changed, "", "SENSOR_X").title, "Sensor revised", "the new catalog must reflect the changed project part");
    assert_ne!(sensor.hash, entry(&changed, "", "SENSOR_X").hash, "changed part content must change its hash");
    assert_ne!(block.hash, entry(&changed, "", "rc_filter").hash, "project block hashes must include project definition inputs");
    assert_eq!(usb.hash, entry(&changed, "std", "usb_c_power").hash, "project changes must preserve library block hashes");
    let library_parts: Vec<_> = cat.entries.iter().filter(|entry| !entry.lib.is_empty() && entry.part.is_some()).collect();
    assert!(!library_parts.is_empty(), "library part Arc reuse must be checked on actual definitions");
    for before in library_parts {
        let after = entry(&changed, &before.lib, &before.name);
        assert!(Arc::ptr_eq(before.part.as_ref().expect("the library entry is a part"),
            after.part.as_ref().expect("the library entry remains a part")),
            "{}.{} must retain the library enumeration's part Arc", before.lib, before.name);
    }

    let mut other_libs = libs.clone();
    other_libs.add("kit", "extra.part.splash", "let extra = 1\n").expect("the changed library has a distinct document");
    let mismatch = catalog(&other_libs, &part_edit);
    assert!(!Arc::ptr_eq(&changed, &mismatch), "mismatched library snapshots must not return the cached join");
    assert_ne!(entry(&changed, "", "rc_filter").hash, entry(&mismatch, "", "rc_filter").hash,
        "uncached project enumeration must hash the supplied library inputs");
    assert!(!Arc::ptr_eq(entry(&changed, "", "SENSOR_X").part.as_ref().expect("the cached project part exists"),
        entry(&mismatch, "", "SENSOR_X").part.as_ref().expect("the uncached project part exists")),
        "a library mismatch must rebuild project definitions in a fresh VM");
    assert!(!Arc::ptr_eq(&mismatch, &catalog(&other_libs, &part_edit)), "mismatched joins must remain uncached");
    assert!(Arc::ptr_eq(&changed, &catalog(&libs, &part_edit)), "a mismatched call must leave the original catalog intact");
    assert!(Arc::ptr_eq(&cat, &catalog(&libs, &ev)), "adding to a cloned library must leave its original cache intact");

    // A drop wires a definition's supply and ground pins to the rails, as a person does (a block
    // such as led_driver reads its supply's volts); every other end is open.
    let circuit = &part_edit.design.units[0].circuit;
    let rail = |name: &str| circuit.nets.iter().find(|net| net.name == name).map(|net| net.id).expect("the board has its rails");
    let (gnd, vcc) = (rail("GND"), rail("VCC"));
    let crystal = entry(&changed, "std", "XTAL_3225");
    let operation = DesignOp::Circuit {
        unit: part_edit.units[0].id,
        op: CircuitOp::AddPart { stmt: crystal.stmt.clone(), after: None, ends: vec![End::Net(vcc)], hint: None },
    };
    let edit = splice(&sources, &part_edit.map, &part_edit.design, &[operation]).expect("the partially connected crystal splices");
    let mut added = sources.clone();
    added.apply(&edit.edits).expect("the partially connected crystal insertion applies");
    assert!(added.doc(board).expect("the edited board remains present").text
        .ends_with("let y_vcc = e.part(mod.std.crystal_part(\"3225\"), {pins: {\"1\": vcc} added: 1})\n"),
        "a crystal dropped with one end writes only its first pin field");
    let crystal_eval = evaluate(&added, &libs).expect("the partially connected crystal evaluates");
    assert!(matches!(crystal_eval.units[0].state, UnitState::Built), "the partially connected crystal builds its unit");
    for entry in &changed.entries {
        let ends = entry.pins.iter().map(|pin| {
            if pin.name.eq_ignore_ascii_case("gnd") {
                End::Net(gnd)
            } else if pin.kind == PinKind::PowerIn {
                End::Net(vcc)
            } else {
                End::Open
            }
        }).collect();
        let operation = DesignOp::Circuit {
            unit: part_edit.units[0].id,
            op: CircuitOp::AddPart { stmt: entry.stmt.clone(), after: None, ends, hint: None },
        };
        let edit = splice(&sources, &part_edit.map, &part_edit.design, &[operation])
            .unwrap_or_else(|diag| panic!("{}.{} must splice: {diag:?}", entry.lib, entry.name));
        let mut added = sources.clone();
        added.apply(&edit.edits).expect("a verified catalog insertion must apply");
        let result = evaluate(&added, &libs)
            .unwrap_or_else(|diags| panic!("{}.{} must evaluate after insertion: {diags:?}", entry.lib, entry.name));
        assert!(matches!(result.units[0].state, UnitState::Built), "{}.{} must build its unit", entry.lib, entry.name);
        assert!(result.notes.iter().chain(&result.system.diags).all(|diag| diag.severity != Severity::Error),
            "{}.{} must leave no error diagnostics", entry.lib, entry.name);
    }
}

#[test]
fn blocks_alone_builds_each_block_with_one_net_per_port() {
    let libs = std_libs();
    let mut sources = Sources::new();
    let board = sources.add("board.ckt.splash", BOARD).expect("the circuit fixture has a valid name");
    sources.add("parts/sensor.part.splash", PART).expect("the part fixture has a valid name");
    sources.add("filters.blocks.splash", BLOCKS).expect("the block fixture has a valid name");
    let ev = evaluate(&sources, &libs).unwrap_or_else(|diags| panic!("the initial circuit must build: {diags:?}"));
    let cat = catalog(&libs, &ev);
    let mut entries: Vec<_> = cat.entries.iter()
        .filter(|entry| entry.lib == "std" && entry.kind == CatalogKind::Block).collect();
    assert!(!entries.is_empty(), "the catalog must offer standard blocks");
    entries.push(entry(&cat, "", "rc_filter"));
    entries.push(entry(&cat, "", "SENSOR_X"));
    entries.push(entry(&cat, "std", "r 10kohm 0402"));
    let results = blocks_alone(ev.defs.source(), &entries);
    assert_eq!(results.len(), entries.len(), "every requested entry must have a result");
    let mut failures = Vec::new();
    for (entry, result) in entries.iter().zip(&results) {
        if entry.kind != CatalogKind::Block {
            let diags = result.as_ref().expect_err("parts and passives cannot build as blocks");
            let name = format!("{}.{}", entry.lib, entry.name);
            assert!(diags.iter().any(|diag| diag.message.contains(&name) && diag.message.contains("not a block")
                && diag.sel.iter().any(|sel| matches!(sel, Sel::Source { .. }))),
                "{name} must have a located diagnostic naming the non-block entry: {diags:?}");
            continue;
        }
        match result {
            Ok(miniature) => {
                let circuit = &miniature.circuit;
                assert_eq!(circuit.name, entry.name, "each result must belong to its requested entry");
                for pin in &entry.pins {
                    let nets: Vec<_> = circuit.nets.iter().filter(|net| net.name == pin.name).collect();
                    assert_eq!(nets.len(), 1, "{}.{} must have one net for port {}", entry.lib, entry.name, pin.name);
                    assert!(nets[0].scope.is_none(), "{}.{} port {} must have an explicit name", entry.lib, entry.name, pin.name);
                    assert_eq!(nets[0].volts.is_some(), nets[0].kind == NetKind::Power,
                        "{}.{} port {}: a power port net has volts (declared or the nominal rail), no other", entry.lib, entry.name, pin.name);
                }
            }
            Err(diags) => failures.push(format!("{}.{}: {diags:?}", entry.lib, entry.name)),
        }
    }
    let built = |lib: &str, name: &str| {
        let index = entries.iter().position(|entry| entry.lib == lib && entry.name == name)
            .expect("the block is among the requested entries");
        results[index].as_ref().unwrap_or_else(|diags| panic!("{lib}.{name} must build alone: {diags:?}"))
    };
    let usb_miniature = built("std", "usb_c_power");
    let usb = &usb_miniature.circuit;
    for (name, kind) in [("vbus", NetKind::Power), ("gnd", NetKind::Ground)] {
        let net = usb.nets.iter().find(|net| net.name == name).expect("the power block must retain its port net");
        assert_eq!(net.kind, kind, "usb_c_power's {name} net must retain its port's electrical kind");
    }
    assert!(!usb.comps.is_empty(), "usb_c_power must build its components");
    let ldo = built("std", "ldo");
    assert!(ldo.circuit.comps.iter().any(|comp| comp.part.name == "LDO_3V3_SOT23-5"),
        "std's ldo builds alone on std's own 3.3 V regulator");
    let led = built("std", "led_driver");
    let rail = led.circuit.nets.iter().find(|net| net.name == "vdd").expect("led_driver keeps its supply port net");
    assert_eq!(rail.volts.map(|v| (v.min, v.max)), Some((3.3, 3.3)), "an undeclared power port sits at the nominal 3.3 V rail");
    assert_eq!(led.previewed, ["vdd"], "the miniature names the port it previews at the nominal rail");
    assert_eq!(led.rail_note().as_deref(), Some("vdd previewed at 3.3 V"), "the tile says the rail is a preview");
    assert_eq!(usb_miniature.rail_note(), None, "a block whose power ports declare volts previews nothing");

    let filter = &built("", "rc_filter").circuit;
    assert_eq!(filter.nets.iter().map(|net| (net.name.as_str(), net.kind)).collect::<Vec<_>>(), [
        ("in", NetKind::Signal), ("out", NetKind::Signal), ("gnd", NetKind::Ground),
        ("data[0]", NetKind::Signal), ("data[1]", NetKind::Signal),
        ("pair.p", NetKind::Signal), ("pair.n", NetKind::Signal),
    ], "rc_filter must have exactly its scalar, bus and pair port nets in port order");
    let net = |name: &str| filter.nets.iter().find(|net| net.name == name).expect("the filter port net must exist").id;
    let resistor = filter.comps.iter().find(|comp| comp.part.kind == PartKind::Resistor)
        .expect("rc_filter must build its resistor");
    let connected: Vec<_> = resistor.conns.iter().map(|(_, net)| *net).collect();
    assert_eq!(connected.len(), 2, "the filter resistor must have two connected pins");
    assert!(connected.contains(&net("in")) && connected.contains(&net("out")),
        "the filter resistor must connect the in and out nets");

    let end = sources.doc(board).expect("the circuit remains present").text.len() as u32;
    sources.apply(&[TextEdit { doc: board, span: end..end, text: "\n// Circuit-only edit.\n".into() }])
        .expect("the circuit comment append must apply");
    let circuit_edit = evaluate_from(Some(&ev), &sources, &libs, &NO_CANCEL).expect("the circuit comment must evaluate");
    assert!(Arc::ptr_eq(ev.defs.source(), circuit_edit.defs.source()),
        "a circuit-only edit must share the same catalog source Arc");
    assert!(failures.is_empty(), "every standard and project block must build alone:\n{}", failures.join("\n"));
}
