//! The sheet's edits as the AI calls them. Each is the plan a person's gesture or card makes
//! (`fab_elec_explore::plan`), so an AI edit writes the same source: a Library entry dropped with
//! its pins on pins or nets (`elec.part.add`, the Library panel's drop), Delete on parts
//! (`elec.part.remove`), a wire dragged from a pin or a name typed at its end
//! (`elec.net.connect`), Delete on a wire end (`elec.net.disconnect`), the not-connected mark
//! (`elec.pin.nc`), a net's name typed on its wire (`elec.net.rename`), the card's reference
//! (`elec.part.rename`), and a part's own text field as its call writes it, `fields: {key: "text"}`
//! (`elec.part.field`; the person's path is typing it in the circuit's code pane).
//! `elec.library.list` reads the Library panel's entries. A resolver costs O(parts + nets) of one
//! unit, a Library read O(entries); nothing runs per frame.

use crate::edit_actions::{arg, check_tool, error, find_part, required_part, units, unique, Args};
use crate::{Docs, Plan};
use fab_elec_circuit::{Circuit, CircuitOp, Component, End, Net, RefKind};
use fab_elec_core::{CompId, Diag, PinIx};
use fab_elec_design::action::{ActionDef, FieldDef, FieldKind, Risk};
use fab_elec_design::{By, Design, DesignOp, Resolved, Unit};
use fab_elec_explore::edit::{Target, Toward};
use fab_elec_explore::plan::{delete_parts, plan, Gesture, Planned};
use fab_elec_explore::sheet::TagKind;
use fab_elec_lang::{Catalog, CatalogEntry, CatalogKind};
use fab_elec_parts::{lifecycle_rank, Lifecycle, PinKind};
use makepad_widgets_core::makepad_script::syntax::write::quote;
use std::fmt::Write;

/// What an edit that can make parts or nets answers.
const MADE_RETURNS: &str = "seq: int  the step it committed
label: text  its name in the undo history
made: [text]  what it made: each new part by its reference, each new net by its name";

const EDIT_RETURNS: &str = "seq: int  the step it committed
label: text  its name in the undo history";

const UNIT: FieldDef = FieldDef {
    name: "unit", kind: FieldKind::Text, required: false, default: "",
    help: "The unit's name. Needed only when two units have a part or net of that name.",
};

const PIN: FieldDef = FieldDef {
    name: "pin", kind: FieldKind::Text, required: true, default: "",
    help: "The pin as part.pin: R1.2, U1.PB0 or mcu.boot_r.1.",
};

const PART: FieldDef = FieldDef {
    name: "part", kind: FieldKind::Text, required: true, default: "",
    help: "The part's reference (R1) or path (mcu.boot_r).",
};

pub const LIBRARY_LIST: ActionDef = ActionDef {
    name: "elec.library.list",
    label: "List Library",
    group: "Schematic",
    help: "The Library panel's entries: the parts, passives and blocks elec.part.add puts on the sheet, each with its pins in the order elec.part.add's ends take them.",
    returns: "total: int  entries that match, over every page
next: int  the from of the next page, when more remain
entries: [{..}]
  part: text  the name elec.part.add takes
  lib: text  its library; empty for the project's own
  kind: choice passive|part|block
  title: text
  summary: text
  pins: [text]  in order, each as name:kind
  defaults: text  the values it starts with
  footprint: text  its footprint's name; empty without one
  lifecycle: choice active|nrnd|obsolete  when the part states one
  approved: bool  true when the person approved the part; absent otherwise",
    fields: &[
        FieldDef {
            name: "find", kind: FieldKind::Text, required: false, default: "",
            help: "Only entries whose name, title, summary or library has this text; case does not matter.",
        },
        FieldDef {
            name: "from", kind: FieldKind::Int { min: 0, max: i64::MAX }, required: false, default: "0",
            help: "The first entry of the page: a reply's next.",
        },
    ],
    inputs: &[], risk: Risk::Read, keys: "",
};

const PART_ADD: ActionDef = ActionDef {
    name: "elec.part.add",
    label: "Add part",
    group: "Schematic",
    help: "Put a Library entry on the sheet as dropping it from the Library panel does, its pins on the pins or nets ends names; the sheet lays it out. With series, a two-pin part goes between a pin and its net instead. elec.value.set then sets its value.",
    returns: MADE_RETURNS,
    fields: &[
        FieldDef {
            name: "part", kind: FieldKind::Text, required: true, default: "",
            help: "The Library entry's part, as elec.library.list gives it (r 10kohm 0402), or its binding alone (r) when one entry has it.",
        },
        FieldDef {
            name: "lib", kind: FieldKind::Text, required: false, default: "",
            help: "The entry's library, when two libraries have an entry of that name.",
        },
        FieldDef {
            name: "ends", kind: FieldKind::Text, required: false, default: "",
            help: "Where each of its pins goes, in the entry's pin order, comma-separated: a pin (U1.PB0) joins that pin's net, or the pin when it is on none; a net's name (GND) joins that net; - leaves the pin open. Pins past the last end stay open. Omit it to put the part on the sheet unconnected.",
        },
        FieldDef {
            name: "series", kind: FieldKind::Text, required: false, default: "",
            help: "A connected pin (U1.PB0): the new two-pin passive goes between that pin and its net, as dropping it on the pin's wire end offers. Not with ends.",
        },
        UNIT,
    ],
    inputs: &[], risk: Risk::Edit, keys: "",
};

const PART_REMOVE: ActionDef = ActionDef {
    name: "elec.part.remove",
    label: "Remove parts",
    group: "Schematic",
    help: "Delete parts as the Delete key does on the sheet's selection: their statements leave the circuit and their pins leave their nets, in one undo step.",
    returns: EDIT_RETURNS,
    fields: &[
        FieldDef {
            name: "parts", kind: FieldKind::Text, required: true, default: "",
            help: "The parts by reference or path, comma-separated (R1, R2), all in one unit.",
        },
        UNIT,
    ],
    inputs: &[], risk: Risk::Edit, keys: "",
};

const PART_RENAME: ActionDef = ActionDef {
    name: "elec.part.rename",
    label: "Rename part",
    group: "Schematic",
    help: "Set a part's reference as typing it in its card does. An empty to numbers it automatically again.",
    returns: EDIT_RETURNS,
    fields: &[
        PART,
        FieldDef {
            name: "to", kind: FieldKind::Text, required: true, default: "",
            help: "The new reference (R7); empty numbers it automatically.",
        },
        UNIT,
    ],
    inputs: &[], risk: Risk::Edit, keys: "",
};

const PART_FIELD: ActionDef = ActionDef {
    name: "elec.part.field",
    label: "Set part field",
    group: "Schematic",
    help: "Write or remove one of a part's own text fields (an MPN, a supplier's number, a note) as typing it in the fields of the call that makes it does.",
    returns: EDIT_RETURNS,
    fields: &[
        PART,
        FieldDef {
            name: "key", kind: FieldKind::Text, required: true, default: "",
            help: "The field's name: letters, digits and _ (mpn, note).",
        },
        FieldDef {
            name: "text", kind: FieldKind::Text, required: false, default: "",
            help: "The field's text; omit it to remove the field.",
        },
        UNIT,
    ],
    inputs: &[], risk: Risk::Edit, keys: "",
};

const NET_CONNECT: ActionDef = ActionDef {
    name: "elec.net.connect",
    label: "Connect pin",
    group: "Schematic",
    help: "Wire a pin as dragging a wire from it does: to another pin, onto a net, or, for a pin on no net, to a new net by typing its name at the wire's end. Two nets that meet become one.",
    returns: MADE_RETURNS,
    fields: &[
        PIN,
        FieldDef {
            name: "to", kind: FieldKind::Text, required: true, default: "",
            help: "Another pin (R2.1), a net's name (GND), or a new net's name for a pin on no net.",
        },
        UNIT,
    ],
    inputs: &[], risk: Risk::Edit, keys: "",
};

const NET_DISCONNECT: ActionDef = ActionDef {
    name: "elec.net.disconnect",
    label: "Disconnect pin",
    group: "Schematic",
    help: "Take a pin off its net as Delete on its wire end does; the rest of the net stays.",
    returns: EDIT_RETURNS,
    fields: &[PIN, UNIT],
    inputs: &[], risk: Risk::Edit, keys: "",
};

const NET_RENAME: ActionDef = ActionDef {
    name: "elec.net.rename",
    label: "Rename net",
    group: "Schematic",
    help: "Name a net as typing a name on its wire does.",
    returns: EDIT_RETURNS,
    fields: &[
        FieldDef {
            name: "net", kind: FieldKind::Text, required: true, default: "",
            help: "The net's current name, as elec.design.read gives it.",
        },
        FieldDef {
            name: "to", kind: FieldKind::Text, required: true, default: "",
            help: "Its new name.",
        },
        UNIT,
    ],
    inputs: &[], risk: Risk::Edit, keys: "",
};

const PIN_NC: ActionDef = ActionDef {
    name: "elec.pin.nc",
    label: "Mark pin not connected",
    group: "Schematic",
    help: "Mark a pin not connected, or clear the mark, as the sheet's not-connected toggle does; the checks then leave a marked pin alone. A pin on a net with other pins is disconnected first with elec.net.disconnect.",
    returns: EDIT_RETURNS,
    fields: &[
        PIN,
        FieldDef {
            name: "nc", kind: FieldKind::Bool, required: false, default: "true",
            help: "true marks it, false clears the mark.",
        },
        UNIT,
    ],
    inputs: &[], risk: Risk::Edit, keys: "",
};

pub static ACTIONS: &[ActionDef] = &[
    LIBRARY_LIST, PART_ADD, PART_REMOVE, PART_RENAME, PART_FIELD, NET_CONNECT, NET_DISCONNECT, NET_RENAME, PIN_NC,
];

/// A Library page's size: about this many entries per answer.
const LIBRARY_PAGE: usize = 40;

/// A list field's words: comma- or space-separated, a tool's array (`[a b]`) too.
fn words(text: &str) -> Vec<&str> {
    let text = text.trim();
    let text = text.strip_prefix('[').and_then(|t| t.strip_suffix(']')).unwrap_or(text);
    text.split(|c: char| c == ',' || c.is_whitespace())
        .map(|word| word.trim_matches('"')).filter(|word| !word.is_empty()).collect()
}

fn pin_kind(kind: PinKind) -> &'static str {
    match kind {
        PinKind::In => "in",
        PinKind::Out => "out",
        PinKind::Bidi => "bidi",
        PinKind::TriState => "tristate",
        PinKind::Passive => "passive",
        PinKind::PowerIn => "power_in",
        PinKind::PowerOut => "power_out",
        PinKind::OpenCollector => "open_collector",
        PinKind::OpenEmitter => "open_emitter",
        PinKind::NoConnect => "nc",
        PinKind::Free => "free",
        PinKind::Unspecified => "unspecified",
    }
}

fn pin_text(comp: &Component, pin: PinIx) -> String {
    format!("{}.{}", comp.refdes, comp.part.pins.get(pin.ix()).map_or("?", |p| p.name.as_str()))
}

/// A pin by the words the reads write, part.pin (R1.2, U1.PB0, mcu.boot_r.1); None when the
/// text names no part before its last dot. O(parts + pins of the part).
fn find_pin<'a>(units: &'a [Unit], field: &str, text: &str) -> Result<Option<(&'a Unit, &'a Component, PinIx)>, Diag> {
    let text = text.trim();
    let Some((part, pin)) = text.rsplit_once('.') else { return Ok(None) };
    let Some((unit, comp)) = find_part(units, field, part)? else { return Ok(None) };
    let at = comp.part.pins.iter().position(|p| p.name == pin).ok_or_else(|| {
        let names: Vec<&str> = comp.part.pins.iter().map(|p| p.name.as_str()).collect();
        error(field, format_args!("{} has no pin {pin}; its pins are {}", comp.refdes, names.join(", ")))
    })?;
    Ok(Some((unit, comp, PinIx(at as u16))))
}

fn required_pin<'a>(units: &'a [Unit], args: &Args, field: &str) -> Result<(&'a Unit, &'a Component, PinIx), Diag> {
    let text = arg(args, field).expect("required field checked");
    find_pin(units, field, text)?
        .ok_or_else(|| error(field, format_args!("no pin {}: a pin is part.pin, as R1.2 or U1.PB0", text.trim())))
}

/// Every unit a call's pins and nets name must be the same one.
fn same_unit<'a>(unit: &mut Option<&'a Unit>, found: &'a Unit, field: &str) -> Result<(), Diag> {
    match unit {
        Some(first) if first.id != found.id => Err(error(field, format_args!("{} and {} are different units: one unit per call", first.name, found.name))),
        _ => {
            *unit = Some(found);
            Ok(())
        }
    }
}

/// A sheet plan as an action's answer: its edit, or why nothing changes (`nothing` when the
/// sheet gives no words).
fn planned(field: &str, planned: Planned, nothing: impl FnOnce() -> String) -> Result<Plan, Diag> {
    match planned {
        Planned::Edit(resolved) => Ok(Plan::Edit(resolved)),
        Planned::Nothing { chip } if chip.is_empty() => Err(error(field, nothing())),
        Planned::Nothing { chip } => Err(error(field, chip)),
        Planned::Refused { chip, sites } => Err(Diag::error(format!("{field}: {chip}"), sites)),
    }
}

fn lib_name(entry: &CatalogEntry) -> &str {
    if entry.lib.is_empty() { "the project" } else { &entry.lib }
}

/// The Library entry `name` (in `lib` when given), or the one entry whose binding is `name`
/// (r for "r 10kohm 0402"). O(entries).
fn library_entry<'a>(catalog: &'a Catalog, name: &str, lib: Option<&str>) -> Result<&'a CatalogEntry, Diag> {
    let name = name.trim();
    let lib = lib.map(str::trim);
    let in_lib = |entry: &&CatalogEntry| lib.is_none_or(|lib| entry.lib == lib);
    let by_name = catalog.entries.iter().filter(in_lib).any(|entry| entry.name == name);
    let mut found = catalog.entries.iter().filter(in_lib).filter(|entry| if by_name { entry.name == name } else { entry.stem == name });
    match (found.next(), found.next()) {
        (Some(entry), None) => Ok(entry),
        (Some(a), Some(b)) => Err(error("lib", format_args!("{name} is in {} and {}; give lib", lib_name(a), lib_name(b)))),
        (None, _) => Err(error("part", format_args!("no Library entry {name}; elec.library.list lists them"))),
    }
}

/// `elec.library.list`: the Library panel's entries, paged, those matching `find` only.
/// O(entries x find).
pub fn library_list(catalog: Option<&Catalog>, args: &Args) -> Result<String, Diag> {
    LIBRARY_LIST.check_args(args)?;
    let catalog = catalog.ok_or_else(|| Diag::error("the plate has no electronics documents, so no Library", vec![]))?;
    let find = arg(args, "find").map(|text| text.trim().to_lowercase()).filter(|text| !text.is_empty());
    let from = arg(args, "from").map_or(Ok(0), |text| text.trim().parse::<usize>().map_err(|_| error("from", "expected an entry number from 0")))?;
    let mut found: Vec<&CatalogEntry> = catalog.entries.iter().filter(|entry| find.as_ref().is_none_or(|find| {
        [&entry.name, &entry.title, &entry.summary, &entry.lib].iter().any(|text| text.to_lowercase().contains(find.as_str()))
    })).collect();
    // The person's approved, active parts first.
    found.sort_by(|a, b| {
        let key = |e: &&CatalogEntry| {
            let (approved, lifecycle) = e.part.as_ref().map_or((false, None), |p| (p.approved, p.lifecycle));
            (!approved, lifecycle_rank(lifecycle))
        };
        key(a).cmp(&key(b)).then_with(|| a.lib.cmp(&b.lib)).then_with(|| a.name.cmp(&b.name))
    });
    let mut out = format!("{{total: {} entries: [", found.len());
    for entry in found.iter().skip(from).take(LIBRARY_PAGE) {
        let kind = match entry.kind {
            CatalogKind::Passive => "passive",
            CatalogKind::Part => "part",
            CatalogKind::Block => "block",
        };
        let pins: Vec<String> = entry.pins.iter().map(|pin| quote(&format!("{}:{}", pin.name, pin_kind(pin.kind)))).collect();
        let defaults: Vec<String> = entry.defaults.iter().map(|(name, value)| format!("{name}: {value}")).collect();
        let footprint = entry.part.as_ref().and_then(|p| p.footprint.as_ref()).map_or("", |fp| fp.name.as_str());
        write!(out, "{{part: {} lib: {} kind: {kind} title: {} summary: {} pins: [{}] defaults: {} footprint: {}",
            quote(&entry.name), quote(&entry.lib), quote(&entry.title), quote(&entry.summary), pins.join(" "), quote(&defaults.join(", ")), quote(footprint)).unwrap();
        if let Some(part) = &entry.part {
            if let Some(lifecycle) = part.lifecycle {
                write!(out, " lifecycle: {}", lifecycle.name()).unwrap();
            }
            if part.approved {
                out.push_str(" approved: true");
            }
        }
        out.push_str("} ");
    }
    out.push(']');
    if from + LIBRARY_PAGE < found.len() {
        write!(out, " next: {}", from + LIBRARY_PAGE).unwrap();
    }
    out.push('}');
    Ok(out)
}

/// `elec.part.add`: the Library drop's `AddPart`, its ends on the named pins and nets, after the
/// part it lands on (the first end's), or the root block's last part; or the drop on a wire end's
/// `InsertSeries`. O(parts + nets + entries).
pub fn part_add(design: &Design, docs: Option<&Docs>, tool: &str, args: &Args) -> Result<Plan, Diag> {
    check_tool(tool, &PART_ADD, args)?;
    let docs = docs.ok_or_else(|| error("part", "the design has no documents to add a part to"))?;
    let entry = library_entry(docs.catalog(), arg(args, "part").expect("required field checked"), arg(args, "lib"))?;
    let scope = units(design, args)?;
    if let Some(series) = arg(args, "series") {
        if arg(args, "ends").is_some() {
            return Err(error("series", "give ends or series, not both"));
        }
        let (unit, comp, pin) = find_pin(scope, "series", series)?
            .ok_or_else(|| error("series", format_args!("no pin {}: a pin is part.pin, as R1.2", series.trim())))?;
        if entry.kind != CatalogKind::Passive || entry.pins.len() != 2 {
            return Err(error("series", format_args!("{} is not a two-pin passive: only those go in series", entry.name)));
        }
        let at = pin_text(comp, pin);
        let net = unit.circuit.net_of(comp.id, pin)
            .ok_or_else(|| error("series", format_args!("{at} is on no net: series puts the part between a pin and its net")))?;
        return Ok(Plan::Edit(Resolved {
            ops: vec![DesignOp::Circuit { unit: unit.id, op: CircuitOp::InsertSeries { net, stmt: entry.stmt.clone(), moved: vec![(comp.id, pin)] } }],
            place: vec![],
            label: format!("Put {} in series with {at}", entry.stem),
        }));
    }
    let named = arg(args, "ends").map(words).unwrap_or_default();
    if named.len() > entry.pins.len() {
        let pins: Vec<&str> = entry.pins.iter().map(|pin| pin.name.as_str()).collect();
        return Err(error("ends", format_args!("{} has {} pins ({}); ends names {}", entry.name, pins.len(), pins.join(", "), named.len())));
    }
    let mut unit = (scope.len() == 1).then(|| &scope[0]);
    let mut ends = Vec::with_capacity(named.len());
    let mut after = None;
    for (slot, word) in named.iter().enumerate() {
        if *word == "-" {
            ends.push(End::Open);
            continue;
        }
        if let Some((found, comp, pin)) = find_pin(scope, "ends", word)? {
            same_unit(&mut unit, found, "ends")?;
            after.get_or_insert(comp.id);
            // As the Library drop writes a pin end: its net when it is on one.
            ends.push(found.circuit.net_of(comp.id, pin).map_or(End::Pin(comp.id, pin), End::Net));
            continue;
        }
        let nets = scope.iter().filter_map(|unit| unit.circuit.net(word).map(|net| (unit, net)));
        let (found, net) = unique(nets, "ends", word)?.ok_or_else(|| error("ends", format_args!(
            "no pin or net {word} for pin {}; elec.design.read lists them, and elec.net.connect starts a new net", entry.pins[slot].name)))?;
        same_unit(&mut unit, found, "ends")?;
        if after.is_none() {
            after = found.circuit.comps.iter().find(|comp| comp.conns.iter().any(|&(_, id)| id == net.id)).map(|comp| comp.id);
        }
        ends.push(End::Net(net.id));
    }
    let unit = unit.ok_or_else(|| {
        let names: Vec<&str> = scope.iter().map(|unit| unit.name.as_str()).collect();
        error("unit", format_args!("say which unit: {}", names.join(", ")))
    })?;
    let circuit = &unit.circuit;
    if after.is_none() {
        let root = circuit.tree.iter().find(|block| block.parent.is_none()).map(|block| block.id);
        after = circuit.comps.iter().filter(|comp| Some(comp.block) == root).map(|comp| comp.id).max();
    }
    let on: Vec<&str> = named.iter().copied().filter(|word| *word != "-").collect();
    let label = if on.is_empty() { format!("Add {}", entry.stem) } else { format!("Add {} on {}", entry.stem, on.join(", ")) };
    Ok(Plan::Edit(Resolved {
        ops: vec![DesignOp::Circuit { unit: unit.id, op: CircuitOp::AddPart { stmt: entry.stmt.clone(), after, ends, hint: None } }],
        place: vec![],
        label,
    }))
}

/// `elec.part.remove`: the sheet's Delete on the named parts, one undo step. O(parts named x parts).
pub fn part_remove(design: &Design, _docs: Option<&Docs>, tool: &str, args: &Args) -> Result<Plan, Diag> {
    check_tool(tool, &PART_REMOVE, args)?;
    let scope = units(design, args)?;
    let mut unit = None;
    let mut ids = Vec::new();
    for word in words(arg(args, "parts").expect("required field checked")) {
        let (found, comp) = find_part(scope, "parts", word)?
            .ok_or_else(|| error("parts", format_args!("no part {word}; elec.design.read lists them")))?;
        same_unit(&mut unit, found, "parts")?;
        ids.push(comp.id);
    }
    let unit = unit.ok_or_else(|| error("parts", "name at least one part"))?;
    planned("parts", delete_parts(&unit.circuit, &ids), || "nothing to delete".into())
}

/// `elec.part.rename`: the card's reference field. O(parts).
pub fn part_rename(design: &Design, _docs: Option<&Docs>, tool: &str, args: &Args) -> Result<Plan, Diag> {
    check_tool(tool, &PART_RENAME, args)?;
    let (unit, comp) = required_part(design, args, "part")?;
    let refdes = &comp.refdes;
    if comp.ref_kind == RefKind::Shared {
        return Err(error("part", format_args!("{refdes} is numbered with the other parts its call makes: its reference is not its own to set")));
    }
    let to = arg(args, "to").expect("required field checked").trim();
    let (label, to) = if to.is_empty() {
        if comp.ref_kind != RefKind::Stored {
            return Err(error("to", format_args!("{refdes} is numbered automatically already")));
        }
        (format!("Number {refdes} automatically"), None)
    } else {
        if to == refdes {
            return Err(error("to", format_args!("{refdes} has that reference already")));
        }
        if unit.circuit.comps.iter().any(|other| other.refdes == to) {
            return Err(error("to", format_args!("{to} is another part's reference")));
        }
        (format!("Set reference of {refdes} to {to}"), Some(to.to_string()))
    };
    Ok(Plan::Edit(Resolved {
        ops: vec![DesignOp::Circuit { unit: unit.id, op: CircuitOp::SetRef { comp: comp.id, to } }],
        place: vec![],
        label,
    }))
}

/// `elec.part.field`: one of the part's own text fields, written or removed. `approved` is the
/// person's alone, and a `lifecycle` is one `Lifecycle::of_name` takes. O(parts + fields).
pub fn part_field(design: &Design, _docs: Option<&Docs>, tool: &str, args: &Args, by: By) -> Result<Plan, Diag> {
    check_tool(tool, &PART_FIELD, args)?;
    let (unit, comp) = required_part(design, args, "part")?;
    let key = arg(args, "key").expect("required field checked").trim();
    if key.is_empty() || !key.chars().all(|ch| ch.is_ascii_alphanumeric() || ch == '_') {
        return Err(error("key", "a field's name is letters, digits and _"));
    }
    let text = arg(args, "text");
    check_part_field(key, text, by)?;
    let current = comp.fields.0.iter().find(|(name, _)| name == key).map(|(_, text)| text.as_str());
    let refdes = &comp.refdes;
    let label = match (text, current) {
        (None, None) => return Err(error("key", format_args!("{refdes} has no field {key}"))),
        (Some(text), Some(current)) if text == current => return Err(error("text", format_args!("{refdes}'s {key} is that already"))),
        (Some(text), _) => format!("Set {refdes}'s {key} to {}", quote(text)),
        (None, Some(_)) => format!("Remove {refdes}'s {key}"),
    };
    Ok(Plan::Edit(Resolved {
        ops: vec![DesignOp::Circuit { unit: unit.id, op: CircuitOp::SetField { comp: comp.id, key: key.to_string(), text: text.map(str::to_string) } }],
        place: vec![],
        label,
    }))
}

/// The checks the field actions share (`elec.part.field`, `elec.bom.field.set`): `approved` is
/// the person's, set on the part definition, as `brief` and `waive` are; a `lifecycle` override
/// is one `Lifecycle::of_name` takes. O(1).
pub(crate) fn check_part_field(key: &str, text: Option<&str>, by: By) -> Result<(), Diag> {
    if key.eq_ignore_ascii_case("approved") && by != By::Person {
        return Err(error("key", "approved is set by the person on the part definition"));
    }
    if key.eq_ignore_ascii_case("lifecycle") {
        if let Some(text) = text.filter(|text| !text.is_empty()) {
            if Lifecycle::of_name(text).is_none() {
                return Err(error("text", format_args!("a lifecycle is active, nrnd or obsolete, got {}", quote(text))));
            }
        }
    }
    Ok(())
}

/// `elec.net.connect`: a wire dragged from the pin to another pin or onto a net, or a name typed
/// at its end for a pin on no net. O(parts + nets).
pub fn net_connect(design: &Design, _docs: Option<&Docs>, tool: &str, args: &Args) -> Result<Plan, Diag> {
    check_tool(tool, &NET_CONNECT, args)?;
    let (unit, comp, pin) = required_pin(units(design, args)?, args, "pin")?;
    let circuit = &unit.circuit;
    let at = pin_text(comp, pin);
    let from = Target::Pin { comp: comp.id, pin, toward: Toward::Across };
    let to = arg(args, "to").expect("required field checked").trim();
    let gesture = if let Some((_, other, other_pin)) = find_pin(std::slice::from_ref(unit), "to", to)? {
        Gesture::Wire { from, to: Target::Pin { comp: other.id, pin: other_pin, toward: Toward::Across } }
    } else if let Some(net) = circuit.net(to) {
        Gesture::Wire { from, to: Target::Tag { net: net.id, kind: TagKind::Label } }
    } else if to.is_empty() {
        return Err(error("to", "another pin, a net's name, or a new net's name"));
    } else if let Some(net) = circuit.net_of(comp.id, pin).and_then(|net| circuit.nets.get(net.ix())).filter(|net| !alone(circuit, net, comp.id, pin)) {
        return Err(error("to", format_args!("no pin or net {to}, and {at} is on {}: elec.net.rename names that net, or elec.net.disconnect takes the pin off it first", net.name)));
    } else {
        Gesture::NameEnd { from, name: to.to_string() }
    };
    planned("to", plan(circuit, &gesture), || format!("{at} is on {to} already"))
}

/// Whether `net` holds only this pin, as an open end's net does: the sheet treats such a pin as
/// free (a named net, or one a block port binds, is never alone). O(connections).
fn alone(circuit: &Circuit, net: &Net, comp: CompId, pin: PinIx) -> bool {
    net.scope.is_some()
        && !circuit.tree.iter().any(|block| block.ports.iter().any(|(_, nets)| nets.contains(&net.id)))
        && circuit.comps.iter().all(|other| other.conns.iter().all(|&(at, id)| id != net.id || (other.id == comp && at == pin)))
}

/// `elec.net.disconnect`: Delete on the pin's wire end. O(parts + nets).
pub fn net_disconnect(design: &Design, _docs: Option<&Docs>, tool: &str, args: &Args) -> Result<Plan, Diag> {
    check_tool(tool, &NET_DISCONNECT, args)?;
    let (unit, comp, pin) = required_pin(units(design, args)?, args, "pin")?;
    let target = Target::Pin { comp: comp.id, pin, toward: Toward::Across };
    planned("pin", plan(&unit.circuit, &Gesture::Delete(target)), || format!("{} is on no net", pin_text(comp, pin)))
}

/// `elec.net.rename`: a name typed on the net's wire. O(nets).
pub fn net_rename(design: &Design, _docs: Option<&Docs>, tool: &str, args: &Args) -> Result<Plan, Diag> {
    check_tool(tool, &NET_RENAME, args)?;
    let name = arg(args, "net").expect("required field checked").trim();
    let nets = units(design, args)?.iter().filter_map(|unit| unit.circuit.net(name).map(|net| (unit, net)));
    let (unit, net) = unique(nets, "net", name)?
        .ok_or_else(|| error("net", format_args!("no net {name}; elec.design.read lists them")))?;
    let to = arg(args, "to").expect("required field checked").trim();
    if to.is_empty() {
        return Err(error("to", "the net's new name"));
    }
    let target = Target::Wire { net: net.id, run: 0, seg: 0, at: [0, 0] };
    planned("to", plan(&unit.circuit, &Gesture::NameNet { target, to: to.to_string() }), || format!("{name} is named {to} already"))
}

/// `elec.pin.nc`: the sheet's not-connected toggle, toward the state asked for. O(parts + nets).
pub fn pin_nc(design: &Design, _docs: Option<&Docs>, tool: &str, args: &Args) -> Result<Plan, Diag> {
    check_tool(tool, &PIN_NC, args)?;
    let (unit, comp, pin) = required_pin(units(design, args)?, args, "pin")?;
    let nc = arg(args, "nc").is_none_or(|text| text.trim() == "true");
    let at = pin_text(comp, pin);
    if comp.nc.contains(&pin) == nc {
        return Err(error("nc", if nc { format!("{at} is marked not connected already") } else { format!("{at} has no not-connected mark") }));
    }
    let target = Target::Pin { comp: comp.id, pin, toward: Toward::Across };
    planned("pin", plan(&unit.circuit, &Gesture::NoConnect(target)), String::new)
}

#[cfg(test)]
mod tests {
    use super::*;
    use fab_elec_design::action::check_returns;
    use std::sync::Arc;

    fn args(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    /// The `(part, lib)` of each entry a library page lists, in order.
    fn listed(out: &str) -> Vec<(String, String)> {
        out.split("{part: ").skip(1).map(|entry| {
            let (part, rest) = entry.split_once(" lib: ").expect("an entry names its library");
            let (lib, _) = rest.split_once(" kind: ").expect("an entry names its kind");
            (part.to_string(), lib.to_string())
        }).collect()
    }

    #[test]
    fn library_list_and_bom_read_report_lifecycle_and_approved() {
        check_returns(LIBRARY_LIST.returns).expect("elec.library.list's returns stay valid");
        let bom_read_def = crate::bom_actions::ACTIONS.iter().find(|a| a.name == "elec.bom.read").expect("elec.bom.read is a BOM action");
        check_returns(bom_read_def.returns).expect("elec.bom.read's returns stay valid");

        let (design, docs) = Docs::from_evaluated(fab_elec_fixtures::docs("ledctl").unwrap(), fab_elec_fixtures::ledctl_eval());
        let mut catalog = (**docs.catalog()).clone();
        let parts: Vec<usize> = (0..catalog.entries.len()).filter(|&i| catalog.entries[i].part.is_some()).collect();
        assert!(parts.len() >= 6, "the fixture's Library has parts to mark");
        let plain = listed(&library_list(Some(&catalog), &[]).unwrap());
        assert!(!plain.is_empty());
        // Marked from the back, so that catalog order alone would not put them first.
        let marks = [
            (true, Some(Lifecycle::Active)),
            (true, None),
            (false, Some(Lifecycle::Active)),
            (false, Some(Lifecycle::Nrnd)),
            (false, Some(Lifecycle::Obsolete)),
        ];
        let mut marked = Vec::new();
        for (&ix, (approved, lifecycle)) in parts.iter().rev().zip(marks) {
            let entry = &mut catalog.entries[ix];
            let part = Arc::make_mut(entry.part.as_mut().unwrap());
            part.approved = approved;
            part.lifecycle = lifecycle;
            marked.push((quote(&entry.name), quote(&entry.lib)));
        }
        let total = catalog.entries.len();
        let first = library_list(Some(&catalog), &[]).unwrap();
        assert!(first.starts_with(&format!("{{total: {total} ")), "{first}");
        let order = listed(&first);
        assert_eq!(order[..3], marked[..3], "approved active, then approved, then active parts lead the list");
        let from = (total - 2).to_string();
        let last = listed(&library_list(Some(&catalog), &args(&[("from", &from)])).unwrap());
        assert_eq!(last, marked[3..], "NRND and then obsolete parts end the list");
        let entry_text = |out: &str, (part, lib): &(String, String)| {
            let start = out.find(&format!("{{part: {part} lib: {lib} ")).expect("the entry is listed");
            out[start..].split_once("} ").expect("the entry closes").0.to_string()
        };
        let top = entry_text(&first, &marked[0]);
        assert!(top.contains(" footprint: ") && top.contains(" lifecycle: active") && top.ends_with(" approved: true"), "{top}");
        let approved_only = entry_text(&first, &marked[1]);
        assert!(!approved_only.contains("lifecycle:") && approved_only.ends_with(" approved: true"), "{approved_only}");
        let tail = library_list(Some(&catalog), &args(&[("from", &from)])).unwrap();
        let nrnd = entry_text(&tail, &marked[3]);
        assert!(nrnd.ends_with(" lifecycle: nrnd") && !nrnd.contains("approved:"), "{nrnd}");
        assert!(entry_text(&tail, &marked[4]).ends_with(" lifecycle: obsolete"));
        assert!(!library_list(Some(&**docs.catalog()), &[]).unwrap().contains("approved:"), "an unmarked Library approves nothing");

        // The BOM reads the parts' approval and the worst lifecycle on each line.
        let plain_bom = crate::bom_actions::bom_read(&design, &[]).unwrap();
        assert!(!plain_bom.contains(" approved: ") && !plain_bom.contains(" lifecycle: "), "{plain_bom}");
        let mut marked_design = design.clone();
        let sheet = fab_elec_bom::sheet(&design, design.units[0].id, &fab_elec_bom::SheetView::default()).unwrap();
        let row = sheet.rows.iter().find(|r| !r.comps.is_empty()).expect("a line with parts");
        let circuit = Arc::make_mut(&mut marked_design.units[0].circuit);
        for id in &row.comps {
            let part = Arc::make_mut(&mut circuit.comps[id.ix()].part);
            part.approved = true;
            part.lifecycle = Some(Lifecycle::Nrnd);
        }
        let line = crate::bom_actions::bom_read(&marked_design, &args(&[("only", &row.key)])).unwrap();
        assert!(line.contains(" lifecycle: nrnd approved: true}") && line.contains("\"NRND\""), "{line}");
    }
}
