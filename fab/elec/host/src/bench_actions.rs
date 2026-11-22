//! The Bench editor's edits as the AI calls them (`instruments::bench_setup`): a supply put on a
//! net, or its v, limit or ramp picked in its card (`elec.bench.supply`); a source or load added
//! between two nets, or its values picked (`elec.bench.source`); a net probed into the Scope, its
//! lane renamed (`elec.bench.probe`); and each taken off as its card's ⋯ does. One call is one
//! undo step, and a playing run re-records its take as after a person's pick. A call costs
//! O(supplies + sources + probes + nets) of one bench; nothing runs per frame.

use crate::edit_actions::{arg, check_tool, error, Args};
use crate::{Docs, Plan};
use fab_elec_bench::bench::SourceKind;
use fab_elec_circuit::{Circuit, NetKind, QtyUnit, Quantity, Value};
use fab_elec_core::{Diag, NetRef};
use fab_elec_design::action::{ActionDef, FieldDef, FieldKind, Risk};
use fab_elec_design::{BenchOp, Design, DesignOp, ProbeOf, ProbeQuantity, Resolved};
use fab_elec_lang::{stimulus_unit, units::parse_value, BenchView};
use fab_elec_parts::PartKind;
use fab_elec_view::instruments::bench_setup::{SOURCE_KINDS, SUPPLY_V};

const EDIT_RETURNS: &str = "seq: int  the step it committed
label: text  its name in the undo history";

const BENCH: FieldDef = FieldDef {
    name: "bench", kind: FieldKind::Text, required: false, default: "",
    help: "The bench's name (chase) or its document's (ledavr.bench.splash). Needed only when the project has more than one bench.",
};

const REMOVE: FieldDef = FieldDef {
    name: "remove", kind: FieldKind::Bool, required: false, default: "false",
    help: "true takes it off the bench, as its card's ⋯ does.",
};

const BENCH_SUPPLY: ActionDef = ActionDef {
    name: "elec.bench.supply",
    label: "Bench supply",
    group: "Bench",
    help: "Put a bench supply on a net as the Bench editor's POWER + Add does (5 V unless v says), set its v, limit or ramp as picking one in its card does, or take it off. A new supply takes v only; a change is one value per call. A supply stands in for the power the board takes.",
    returns: EDIT_RETURNS,
    fields: &[
        FieldDef {
            name: "net", kind: FieldKind::Text, required: true, default: "",
            help: "The supplied net's name (VBUS, 3V3); never ground.",
        },
        FieldDef {
            name: "v", kind: FieldKind::Num { unit: "V" }, required: false, default: "",
            help: "Its voltage.",
        },
        FieldDef {
            name: "limit", kind: FieldKind::Num { unit: "A" }, required: false, default: "",
            help: "Its current limit: it holds the current there while its voltage drops.",
        },
        FieldDef {
            name: "ramp", kind: FieldKind::Num { unit: "s" }, required: false, default: "",
            help: "How long it takes to rise from 0 V to v.",
        },
        REMOVE,
        BENCH,
    ],
    inputs: &[], risk: Risk::Edit, keys: "",
};

const BENCH_SOURCE: ActionDef = ActionDef {
    name: "elec.bench.source",
    label: "Bench source",
    group: "Bench",
    help: "Add a source or load between two nets as the Bench editor's SOURCES + Add does, change a value as picking one in its card does, or take it off. A new one starts with that menu's values, and values gives any of them; a change is one value per call.",
    returns: EDIT_RETURNS,
    fields: &[
        FieldDef {
            name: "a", kind: FieldKind::Text, required: true, default: "",
            help: "The net it drives, or draws its load current from.",
        },
        FieldDef {
            name: "b", kind: FieldKind::Text, required: false, default: "",
            help: "Its other net; ground when omitted.",
        },
        FieldDef {
            name: "kind", kind: FieldKind::Choice(&["vsrc", "isrc", "pulse", "load"]), required: false, default: "",
            help: "What to add: vsrc a DC voltage, isrc a DC current, pulse a voltage pulse, load a resistor or current sink. Needed to add one.",
        },
        FieldDef {
            name: "values", kind: FieldKind::Splash, required: false, default: "",
            help: "Its options as the bench document writes them: vsrc and isrc {dc}, pulse {v low at width every edge}, load {r} or {i}; as {v: 3.3V width: 1ms every: 10ms}.",
        },
        REMOVE,
        BENCH,
    ],
    inputs: &[], risk: Risk::Edit, keys: "",
};

const BENCH_PROBE: ActionDef = ActionDef {
    name: "elec.bench.probe",
    label: "Bench probe",
    group: "Bench",
    help: "Probe a net's voltage into the bench's Scope as + Add probe does, give its lane a label as renaming it in the Scope does, or take it off. Its lane shows in the Scope and in the run's take.",
    returns: EDIT_RETURNS,
    fields: &[
        FieldDef {
            name: "net", kind: FieldKind::Text, required: true, default: "",
            help: "The probed net's name.",
        },
        FieldDef {
            name: "label", kind: FieldKind::Text, required: false, default: "",
            help: "Its lane's label; a new probe is named by its net without one.",
        },
        REMOVE,
        BENCH,
    ],
    inputs: &[], risk: Risk::Edit, keys: "",
};

pub static ACTIONS: &[ActionDef] = &[BENCH_SUPPLY, BENCH_SOURCE, BENCH_PROBE];

/// The bench `bench` names (its name, or its document's whole or file name), or the project's only
/// one. O(benches + documents).
fn find_bench<'a>(docs: &'a Docs, args: &Args) -> Result<&'a BenchView, Diag> {
    let views = docs.bench_views();
    let names = || views.iter().map(|view| view.out.name.as_str()).collect::<Vec<_>>().join(", ");
    match arg(args, "bench").map(str::trim) {
        Some(name) => views.iter().find(|view| view.out.name == name || docs.sources().docs().iter().any(|doc| {
            doc.id == view.out.doc && (doc.name == name || doc.name.rsplit('/').next() == Some(name))
        })).ok_or_else(|| error("bench", format_args!("no bench {name}; the benches are {}", names()))),
        None => match views.as_slice() {
            [view] => Ok(view),
            [] => Err(error("bench", "the project has no bench")),
            _ => Err(error("bench", format_args!("say which bench: {}", names()))),
        },
    }
}

/// The bench's circuit, and the net `field` names in it.
fn bench_net<'a>(design: &'a Design, view: &'a BenchView, args: &Args, field: &str) -> Result<(&'a Circuit, String), Diag> {
    let circuit = view.out.circuit(design).ok_or_else(|| error("bench", format_args!("{} runs no circuit the design has", view.out.name)))?;
    let name = arg(args, field).expect("the caller asks only for a given field").trim();
    circuit.net(name).ok_or_else(|| error(field, format_args!("no net {name} in {}'s circuit; elec.design.read lists them", view.out.name)))?;
    Ok((circuit, name.to_string()))
}

fn net_name(circuit: &Circuit, net: NetRef) -> &str {
    circuit.nets.get(net.net.ix()).map_or("", |net| net.name.as_str())
}

/// A quantity typed as a card's value is (3.3V, 500mA, 1ms; a bare number in SI units), in SI;
/// or why it is not one.
fn quantity(text: &str, unit: QtyUnit) -> Result<f64, String> {
    let like = Value { text: String::new(), qty: Some(Quantity { si: 0.0, unit }), tol: None };
    Ok(parse_value(text, &like, &PartKind::Other(String::new()))?.qty.map_or(0.0, |q| q.si))
}

fn remove(args: &Args) -> bool {
    arg(args, "remove").is_some_and(|text| text.trim() == "true")
}

fn edit(view: &BenchView, ops: Vec<BenchOp>, label: String) -> Result<Plan, Diag> {
    Ok(Plan::Edit(Resolved {
        ops: ops.into_iter().map(|op| DesignOp::Bench { bench: view.out.name.clone(), op }).collect(),
        place: vec![],
        label,
    }))
}

/// `elec.bench.supply`: POWER's + Add, a supply card's value picks, or its ⋯ remove.
pub fn bench_supply(design: &Design, docs: Option<&Docs>, tool: &str, args: &Args) -> Result<Plan, Diag> {
    check_tool(tool, &BENCH_SUPPLY, args)?;
    let view = find_bench(docs.ok_or_else(|| error("bench", "the project has no bench"))?, args)?;
    let (circuit, net) = bench_net(design, view, args, "net")?;
    let exists = view.out.spec.supplies.iter().any(|supply| net_name(circuit, supply.net) == net);
    let mut set = Vec::new();
    for key in ["v", "limit", "ramp"] {
        if let Some(text) = arg(args, key) {
            let unit = stimulus_unit("supply", key).expect("a supply's options have units");
            set.push((key, quantity(text, unit).map_err(|why| error(key, why))?));
        }
    }
    if remove(args) {
        if !set.is_empty() {
            return Err(error("remove", "a removed supply takes no values"));
        }
        if !exists {
            return Err(error("net", format_args!("{} has no supply on {net}", view.out.name)));
        }
        return edit(view, vec![BenchOp::RemoveSupply { net: net.clone() }], format!("Remove the {net} supply"));
    }
    if !exists {
        if circuit.net(&net).is_some_and(|found| found.kind == NetKind::Ground) {
            return Err(error("net", format_args!("{net} is ground: a supply feeds a net against it")));
        }
        // + Add writes the supply with its voltage; its limit and ramp are its card's, once it is on.
        if let Some((key, _)) = set.iter().find(|(key, _)| *key != "v") {
            return Err(error(key, format_args!("{net} has no supply yet: add it with v first, then set {key}")));
        }
        let v = set.first().map_or(SUPPLY_V, |(_, v)| *v);
        return edit(view, vec![BenchOp::AddSupply { net: net.clone(), v }], format!("Add a supply on {net}"));
    }
    match set.as_slice() {
        [] => Err(error("net", format_args!("{net} has a supply already: give v, limit or ramp to change it, or remove"))),
        [(key, value)] => edit(view, vec![BenchOp::SetSupply { net: net.clone(), key: (*key).into(), value: *value }], format!("{net} supply {key}")),
        // Two options new to one statement are two edits of the same text: the card picks one at a time.
        _ => Err(error(set[1].0, "a supply changes one value per call, as one pick in its card does")),
    }
}

/// A `values` option list, `{key: value ...}` as the bench writes it or a tool's JSON object.
fn options(text: &str) -> Result<Vec<(String, String)>, Diag> {
    let text = text.trim();
    let mut rest = text.strip_prefix('{').and_then(|t| t.strip_suffix('}')).unwrap_or(text).trim();
    let mut out = Vec::new();
    let bad = || error("values", "expected the options as {v: 3.3V width: 1ms}");
    while !rest.is_empty() {
        let (key, after) = rest.split_once(':').ok_or_else(bad)?;
        let key = key.trim().trim_matches('"');
        let after = after.trim_start();
        let end = after.find(|ch: char| ch.is_whitespace() || ch == ',').unwrap_or(after.len());
        let value = after[..end].trim_matches('"');
        if key.is_empty() || value.is_empty() {
            return Err(bad());
        }
        out.push((key.to_string(), value.to_string()));
        rest = after[end..].trim_start_matches(|ch: char| ch.is_whitespace() || ch == ',');
    }
    Ok(out)
}

/// The bench call a source of this kind is written with.
fn call_of(kind: &SourceKind) -> &'static str {
    match kind {
        SourceKind::Vdc(_) => "vsrc",
        SourceKind::Idc(_) => "isrc",
        SourceKind::Pulse { .. } => "pulse",
        SourceKind::LoadR(_) | SourceKind::LoadI(_) | SourceKind::LoadStep { .. } => "load",
    }
}

/// `values` read for `call`: each option's SI value, its unit checked.
fn source_values(call: &str, args: &Args) -> Result<Vec<(String, f64)>, Diag> {
    let Some(text) = arg(args, "values") else { return Ok(Vec::new()) };
    options(text)?.into_iter().map(|(key, text)| {
        let unit = stimulus_unit(call, &key).ok_or_else(|| error("values", format_args!("{call} has no option {key}")))?;
        let value = quantity(&text, unit).map_err(|why| error("values", format_args!("{key}: {why}")))?;
        Ok((key, value))
    }).collect()
}

/// `elec.bench.source`: SOURCES' + Add, a source card's value picks, or its ⋯ remove.
pub fn bench_source(design: &Design, docs: Option<&Docs>, tool: &str, args: &Args) -> Result<Plan, Diag> {
    check_tool(tool, &BENCH_SOURCE, args)?;
    let view = find_bench(docs.ok_or_else(|| error("bench", "the project has no bench"))?, args)?;
    let (circuit, a) = bench_net(design, view, args, "a")?;
    let b = match arg(args, "b") {
        Some(_) => bench_net(design, view, args, "b")?.1,
        None => circuit.nets.iter().find(|net| net.kind == NetKind::Ground).map(|net| net.name.clone())
            .ok_or_else(|| error("b", format_args!("{}'s circuit has no ground: name b", view.out.name)))?,
    };
    let found = view.out.spec.sources.iter().find(|source| net_name(circuit, source.a) == a && net_name(circuit, source.b) == b);
    let kind = arg(args, "kind").map(|text| text.trim().trim_start_matches('@'));
    if remove(args) {
        if arg(args, "values").is_some() || kind.is_some() {
            return Err(error("remove", "a removed source takes no kind or values"));
        }
        if found.is_none() {
            return Err(error("a", format_args!("{} has no source from {a} to {b}", view.out.name)));
        }
        return edit(view, vec![BenchOp::RemoveSource { a: a.clone(), b }], format!("Remove the source on {a}"));
    }
    if let Some(source) = found {
        let call = call_of(&source.kind);
        if kind.is_some_and(|kind| kind != call) {
            return Err(error("kind", format_args!("the source from {a} to {b} is a {call}: remove it to put another kind there")));
        }
        let mut values = source_values(call, args)?;
        return match values.len() {
            0 => Err(error("values", format_args!("{a} to {b} has a {call} already: give values to change it, or remove"))),
            1 => {
                let (key, value) = values.remove(0);
                let label = format!("Source on {a} {key}");
                edit(view, vec![BenchOp::SetSource { a: a.clone(), b: b.clone(), key, value }], label)
            }
            _ => Err(error("values", "a source changes one value per call, as one pick in its card does")),
        };
    }
    let kind = kind.ok_or_else(|| error("kind", format_args!("{} has no source from {a} to {b}: give kind to add one", view.out.name)))?;
    let &(name, call, start) = SOURCE_KINDS.iter().find(|(_, call, _)| *call == kind).expect("choice checked");
    let values = source_values(call, args)?;
    // A load is a resistance or a current: an i given replaces the menu's r.
    let replaced = |key: &str| values.iter().any(|(given, _)| given == key) || (key == "r" && values.iter().any(|(given, _)| given == "i"));
    let mut opts: Vec<(String, f64)> = start.iter().filter(|(key, _)| !replaced(key)).map(|&(key, value)| (key.to_owned(), value)).collect();
    opts.extend(values);
    edit(view, vec![BenchOp::AddSource { kind: call.to_owned(), a: a.clone(), b, opts }], format!("Add a {} on {a}", name.to_lowercase()))
}

/// `elec.bench.probe`: + Add probe, a lane renamed in the Scope, or a probe's ⋯ remove.
pub fn bench_probe(design: &Design, docs: Option<&Docs>, tool: &str, args: &Args) -> Result<Plan, Diag> {
    check_tool(tool, &BENCH_PROBE, args)?;
    let view = find_bench(docs.ok_or_else(|| error("bench", "the project has no bench"))?, args)?;
    let (_, net) = bench_net(design, view, args, "net")?;
    let probe = view.out.probes.iter().find(|probe| probe.of.quantity == ProbeQuantity::V && probe.of.reference.is_none() && probe.of.target == net);
    let label = arg(args, "label").map(str::trim);
    if remove(args) {
        if label.is_some() {
            return Err(error("remove", "a removed probe takes no label"));
        }
        let probe = probe.ok_or_else(|| error("net", format_args!("{} does not probe {net}", view.out.name)))?;
        let name = probe.label.lines().next().unwrap_or(&net).to_string();
        return edit(view, vec![BenchOp::RemoveProbe { of: probe.of.clone() }], format!("Remove probe {name}"));
    }
    match (probe, label) {
        (None, label) => {
            let label = label.filter(|label| !label.is_empty()).unwrap_or(&net).to_string();
            edit(view, vec![BenchOp::AddProbe { of: ProbeOf::v(net.clone()), label }], format!("Probe {net}"))
        }
        (Some(probe), Some(label)) if !label.is_empty() && label != probe.label => {
            edit(view, vec![BenchOp::SetProbeLabel { of: probe.of.clone(), label: label.to_string() }], format!("Name probe {net} {label}"))
        }
        (Some(probe), _) => Err(error("net", format_args!("{} probes {net} already, as {}: give a new label, or remove", view.out.name, probe.label))),
    }
}
