//! Schematic and board actions shared by Fab's action catalog and engine.

use crate::{legal_plan::{legal_plan, PlacementEdit, LEGALISE}, Docs, Plan};
use fab_elec_board::{BoardOp, ItemKind};
use fab_elec_circuit::{CheckKind, CircuitOp, Component, Measure, QtyUnit, Violation};
use fab_elec_core::{Angle, CompRef, Diag, FeatureRef, NetRef, PinIx, PinRef, Sel, Severity, Side, P};
use fab_elec_design::action::{ActionDef, FieldDef, FieldKind, Risk};
use fab_elec_design::{Design, DesignOp, LinkKind, PartRef, Place, Resolved, SysOp, Unit};
use fab_elec_lang::{board_doc::read_point_mm, map::site_text, units::parse_value};
use fab_elec_view::board::tray::placement_op;
use fab_elec_view::{DesignSnapshot, StageFlags};
use makepad_widgets_core::makepad_script::syntax::write::{list, quote};
use std::collections::HashSet;
use std::fmt::Write;

const EDIT_RETURNS: &str = "seq: int  the step it committed
label: text  its name in the undo history";

const UNIT: FieldDef = FieldDef {
    name: "unit", kind: FieldKind::Text, required: false, default: "",
    help: "The unit's name. Needed only when two units have a part of that name.",
};

const VALUE_SET: ActionDef = ActionDef {
    name: "elec.value.set",
    label: "Set part value",
    group: "Schematic",
    help: "Set a value as typing it in the part's card does. The edit writes the circuit's source in one undo step, and a playing run hears the change.",
    returns: EDIT_RETURNS,
    fields: &[
        FieldDef {
            name: "comp", kind: FieldKind::Text, required: true, default: "",
            help: "The part's reference (R1) or path (mcu.boot_r).",
        },
        FieldDef {
            name: "value", kind: FieldKind::Num { unit: "" }, required: true, default: "",
            help: "The new value with its unit or prefix, such as 22k, 4.7kohm or 100n. A bare number uses the part's own unit.",
        },
        UNIT,
    ],
    inputs: &[], risk: Risk::Edit, keys: "",
};

pub const PART_PLACE: ActionDef = ActionDef {
    name: "elec.part.place",
    label: "Place or move part",
    group: "Board",
    help: "Place an unplaced part as dropping it from the tray does, or move a placed part as dragging it does. Rotation and side apply on first placement. A placed part keeps its rotation and side. Pad-anchored track ends follow the move. Its answer's checks name a track end it left off copper as a new Dangling finding and a connection it broke as a new Unconnected one, as the board marks them.",
    returns: "seq: int  the step it committed
label: text  its name in the undo history
legalised: [{..}]  parts shifted from their requested spots, when any
  part: text  the part reference
  moved_mm: num  distance from the requested spot in millimetres
no_room: [text]  parts left where asked because there was no legal spot, when any",
    fields: &[
        FieldDef {
            name: "part", kind: FieldKind::Text, required: true, default: "",
            help: "The part's reference (R1) or path (mcu.boot_r).",
        },
        FieldDef {
            name: "at", kind: FieldKind::Point, required: true, default: "",
            help: "Where its anchor goes on its board, in mm, as [12mm 30mm].",
        },
        FieldDef {
            name: "rot", kind: FieldKind::Num { unit: "deg" }, required: false, default: "",
            help: "Rotation in degrees for a new placement. Omit for the tray's default.",
        },
        FieldDef {
            name: "side", kind: FieldKind::Choice(&["top", "bottom"]), required: false, default: "",
            help: "Board side for a new placement. Omit for the tray's default.",
        },
        LEGALISE, UNIT,
    ],
    inputs: &[], risk: Risk::Edit, keys: "",
};

const DESIGN_READ: ActionDef = ActionDef {
    name: "elec.design.read",
    label: "Read design",
    group: "Schematic",
    help: "Read the design's units with their parts and nets before an edit. The edit actions name parts by reference or path.",
    returns: "units: [{..}]  a prefix includes each unit with matching parts
  unit: text
  board: bool  whether it has a board
  parts: [{..}]
    ref: text
    path: text
    kind: text
    value: text  empty when it has none
    placed: bool  on its unit's board
  nets: [{..}]  a part or prefix keeps the nets touching the selected parts, with all their pins
    net: text
    pins: [text]  each as ref.pin",
    fields: &[
        FieldDef { help: "Read only this named unit.", ..UNIT },
        FieldDef {
            name: "only", kind: FieldKind::Text, required: false, default: "",
            help: "One part by reference or path, or one net with its pins and their parts. A letter prefix (SW, case-insensitive) selects matching parts and their nets across units. An exact part or net name takes precedence.",
        },
    ],
    inputs: &[], risk: Risk::Read, keys: "",
};

pub const ROUTE: ActionDef = ActionDef {
    name: "elec.route",
    label: "Route",
    group: "Board",
    help: "Route a board's unrouted nets as the board pane's Route all does, or only the named nets. The router works in the background and its copper lands as one undo step when it ends; the answer comes then, with each net it left open and why. elec.check.list then shows what the checks find.",
    returns: "seq: int  the step it committed
label: text  its name in the undo history
route: {..}
  end: choice finished|stopped  stopped: another edit or Stop ended it, and what it had routed was kept
  routed: int  nets it routed
  failed: int  nets it left open
  open: [{..}]  each net left open
    net: text
    why: text  why, and what to try",
    fields: &[
        FieldDef {
            name: "unit", kind: FieldKind::Text, required: false, default: "",
            help: "The board unit's name. Needed only when the design has more than one board.",
        },
        FieldDef {
            name: "nets", kind: FieldKind::Text, required: false, default: "",
            help: "Route only these nets, by name, comma-separated (GND, SDA). Omit it for every unrouted net.",
        },
        FieldDef {
            name: "method", kind: FieldKind::Choice(&["interactive", "organic"]), required: false, default: "interactive",
            help: "interactive: straight copper at 45 degrees, as the board's router lays it; organic: smooth curved copper with teardrops.",
        },
        FieldDef {
            name: "effort", kind: FieldKind::Choice(&["full", "fast"]), required: false, default: "full",
            help: "full: the router rips up and reroutes until a whole pass leaves no fewer nets open; fast: one pass, no rip-up, in seconds, naming each net it leaves open and what blocks it (for checking a placement).",
        },
        FieldDef {
            name: "reroute", kind: FieldKind::Bool, required: false, default: "false",
            help: "true: the router may move the existing copper of the nets it routes; false keeps every existing track.",
        },
    ],
    inputs: &[], risk: Risk::Edit, keys: "",
};

pub const CHECK_LIST: ActionDef = ActionDef {
    name: "elec.check.list",
    label: "List check findings",
    group: "Checks",
    help: "What the checks find, as the Checks pane lists it: ERC on the circuits, DRC on the boards, and the checks across boards. Errors come first. Read it after an edit or a route.",
    returns: "current: bool  false while a check still runs on the newest edit: read again
pending: [text]  the checks still running
errors: int  errors among the findings of this unit and family, at any severity
warnings: int  warnings among them
awaiting: int  findings of this unit and family that wait for a pour's refill, not listed
total: int  findings that match unit, only and severity, over every page
next: int  the from of the next page, when more remain
elsewhere: [{..}]  with a unit, the other units' errors and warnings of this family, not listed; absent when they have none
  unit: text
  errors: int
  warnings: int
findings: [{..}]
  unit: text
  check: text  its name (Clearance, UnconnectedPin)
  family: choice erc|drc|system|route|custom
  severity: choice error|warning|info
  message: text
  fix: text  how to fix it, when the check says
  at: point  where on the board, in mm; none for a circuit finding
  items: [text]  what it is about: parts (R1), pins (R1.2), nets, copper (the GND track)
  actual: text  the measured amount, when there is one
  required: text  what the rule asks",
    fields: &[
        FieldDef { help: "Only this unit's findings.", ..UNIT },
        FieldDef {
            name: "only", kind: FieldKind::Choice(&["erc", "drc", "system", "route", "all"]), required: false, default: "all",
            help: "Only one family of checks: erc (circuit), drc (board), system (across boards and the case), route (the sharp corners a drag kept).",
        },
        FieldDef {
            name: "severity", kind: FieldKind::Choice(&["error", "warning", "info"]), required: false, default: "info",
            help: "The least severity listed: error lists errors only, warning errors and warnings.",
        },
        FieldDef {
            name: "from", kind: FieldKind::Int { min: 0, max: i64::MAX }, required: false, default: "0",
            help: "The first finding of the page: a reply's next.",
        },
    ],
    inputs: &[], risk: Risk::Read, keys: "",
};

pub static ACTIONS: &[ActionDef] = &[VALUE_SET, PART_PLACE, DESIGN_READ, ROUTE, CHECK_LIST];

pub fn register_actions() {
    fab_elec_design::action::register(ACTIONS);
    fab_elec_design::action::register(crate::board_actions::ACTIONS);
    fab_elec_design::action::register(crate::model_actions::ACTIONS);
    fab_elec_design::action::register(crate::doc_actions::ACTIONS);
    fab_elec_design::action::register(crate::sheet_actions::ACTIONS);
    fab_elec_design::action::register(crate::bench_actions::ACTIONS);
    fab_elec_design::action::register(crate::req_actions::ACTIONS);
    fab_elec_design::action::register(crate::bom_actions::ACTIONS);
    fab_elec_design::action::register(crate::board_settings::ACTIONS);
    fab_elec_lang::sys_actions::register_actions();
}

/// The words of a commit these actions label, as the outside-edit cue words a change found by
/// comparing designs ("R1 to 22k", "R1 moved"): "Set R1 to 22k" is "R1 to 22k", "Move R1" is
/// "R1 moved", "Place R1" is "R1 placed". The undo history keeps the label itself. Any other
/// label is its own words. O(label).
pub fn cue_words(label: &str) -> String {
    if let Some(rest) = label.strip_prefix("Set ") {
        return rest.to_owned();
    }
    for (verb, done) in [("Move ", "moved"), ("Place ", "placed")] {
        if let Some(part) = label.strip_prefix(verb) {
            return format!("{part} {done}");
        }
    }
    label.to_owned()
}

pub(crate) type Args = [(String, String)];

pub(crate) fn error(field: &str, reason: impl std::fmt::Display) -> Diag {
    Diag::error(format!("{field}: {reason}"), vec![])
}

pub(crate) fn check_tool(tool: &str, action: &ActionDef, args: &Args) -> Result<(), Diag> {
    // The engine also dispatches dotted suffixes of registered names.
    if tool != action.name { return Err(error("tool", format_args!("unknown {tool}"))); }
    action.check_args(args)
}

pub(crate) fn arg<'a>(args: &'a Args, name: &str) -> Option<&'a str> {
    args.iter().find(|(key, _)| key == name).map(|(_, text)| text.as_str())
}

pub(crate) fn units<'a>(design: &'a Design, args: &Args) -> Result<&'a [Unit], Diag> {
    match arg(args, "unit") {
        None => Ok(&design.units),
        Some(name) => design.unit_by_name(name).map(std::slice::from_ref)
            .ok_or_else(|| error("unit", format_args!("no unit named {name}"))),
    }
}

/// A part or net name shared between units needs a unit qualifier.
pub(crate) fn unique<'a, T: 'a>(
    mut matches: impl Iterator<Item = (&'a Unit, &'a T)>,
    field: &str, text: &str,
) -> Result<Option<(&'a Unit, &'a T)>, Diag> {
    let first = matches.next();
    if let (Some((a, _)), Some((b, _))) = (first, matches.next()) {
        return Err(error(field, format_args!("{text} is in {} and {}; pass unit", a.name, b.name)));
    }
    Ok(first)
}

pub(crate) fn find_part<'a>(units: &'a [Unit], field: &str, text: &str) -> Result<Option<(&'a Unit, &'a Component)>, Diag> {
    unique(units.iter().filter_map(|unit| {
        unit.circuit.comps.iter().find(|c| c.refdes == text || c.path == text).map(|comp| (unit, comp))
    }), field, text)
}

pub(crate) fn required_part<'a>(design: &'a Design, args: &Args, field: &str) -> Result<(&'a Unit, &'a Component), Diag> {
    let text = arg(args, field).expect("required field checked");
    find_part(units(design, args)?, field, text)?
        .ok_or_else(|| error(field, format_args!("no part {text}; elec.design.read lists them")))
}

/// The part `text` names (a reference R1 or a path mcu.boot_r) in any unit; one shared between
/// units is refused, naming them. O(parts).
pub fn find_comp<'a>(design: &'a Design, field: &str, text: &str) -> Result<(CompRef, &'a Component), Diag> {
    let text = text.trim();
    find_part(&design.units, field, text)?
        .map(|(unit, comp)| (CompRef { board: unit.id, comp: comp.id }, comp))
        .ok_or_else(|| error(field, format_args!("no part {text}; elec.design.read lists them")))
}

/// A net, a pin or a part by the words the reads write: a net's name (GND), a part's reference
/// or path (R1, mcu.boot_r), or a pin as part.pin (R1.2, U1.PB0). A part wins over a net of
/// the same name, a net over a pin. O(parts + nets + pins of the part).
pub fn find_target(design: &Design, field: &str, text: &str) -> Result<Sel, Diag> {
    let text = text.trim();
    if let Some((unit, comp)) = find_part(&design.units, field, text)? {
        return Ok(Sel::Comp(CompRef { board: unit.id, comp: comp.id }));
    }
    let nets = design.units.iter().filter_map(|unit| unit.circuit.nets.iter().find(|net| net.name == text).map(|net| (unit, net)));
    if let Some((unit, net)) = unique(nets, field, text)? {
        return Ok(Sel::Net(NetRef { board: unit.id, net: net.id }));
    }
    if let Some((part, pin)) = text.rsplit_once('.') {
        if let Some((unit, comp)) = find_part(&design.units, field, part)? {
            let at = comp.part.pins.iter().position(|p| p.name == pin)
                .ok_or_else(|| error(field, format_args!("{} has no pin {pin}", comp.refdes)))?;
            return Ok(Sel::Pin(PinRef { board: unit.id, comp: comp.id, pin: PinIx(at as u16) }));
        }
    }
    Err(error(field, format_args!("no net, part or pin {text}; elec.design.read lists them")))
}

/// The board unit `field` names, or the design's only board when it names none.
pub fn board_unit<'a>(design: &'a Design, field: &str, name: Option<&str>) -> Result<&'a Unit, Diag> {
    match name.map(str::trim) {
        Some(name) => {
            let unit = design.unit_by_name(name).ok_or_else(|| error(field, format_args!("no unit named {name}")))?;
            unit.board.is_some().then_some(unit).ok_or_else(|| error(field, format_args!("{name} has no board")))
        }
        None => {
            let mut boards = design.units.iter().filter(|unit| unit.board.is_some());
            match (boards.next(), boards.next()) {
                (Some(unit), None) => Ok(unit),
                (None, _) => Err(error(field, "the design has no board")),
                (Some(a), Some(b)) => Err(error(field, format_args!("say which board: {} or {}", a.name, b.name))),
            }
        }
    }
}

/// `elec.factory.set`: a factory profile for a board, written as its document's
/// `factory(..)` line (the person's path: typing that line). Its capabilities then raise the
/// board's effective rules. O(units + profiles).
pub fn factory_set(design: &Design, profiles: &[fab_elec_cam::profile_doc::ProfileDef], tool: &str, args: &[(String, String)]) -> Result<Plan, Diag> {
    let def = fab_elec_cam::actions::ACTIONS.iter().find(|def| def.name == tool)
        .ok_or_else(|| error("tool", format_args!("unknown {tool}")))?;
    check_tool(tool, def, args)?;
    let unit = board_unit(design, "board", arg(args, "board"))?;
    let asked = arg(args, "profile").expect("required field checked").trim();
    let profile = &fab_elec_cam::profile_doc::lookup(profiles, asked).map_err(|why| error("profile", why))?.name;
    if unit.factory.as_deref() == Some(profile.as_str()) {
        return Err(error("profile", format_args!("{} is made by {profile} already", unit.name)));
    }
    Ok(Plan::Edit(Resolved {
        ops: vec![DesignOp::Sys(SysOp::SetFactory { unit: unit.id, profile: Some(profile.to_string()) })],
        place: vec![],
        label: format!("Factory of {}: {profile}", unit.name),
    }))
}

/// `elec.link.*` and `elec.unit.*` (`fab_elec_lang::sys_actions`): a link or a mount written as
/// the system document's line (the person's path: typing that line in its code pane), one undo
/// step; a new link's label names its id. O(units + links) and one small statement parse.
pub fn sys_edit(design: &Design, _docs: Option<&Docs>, tool: &str, args: &[(String, String)]) -> Result<Plan, Diag> {
    let pairs: Vec<(&str, &str)> = args.iter().map(|(name, text)| (name.as_str(), text.as_str())).collect();
    let ops = fab_elec_lang::sys_actions::sys_ops(design, tool, &pairs)?;
    let unit = |id| design.unit(id).map_or("the unit", |unit: &Unit| unit.name.as_str());
    let label = match ops.as_slice() {
        [DesignOp::Sys(SysOp::AddLink(link))] => {
            format!("Add {} link {}", match link.kind { LinkKind::Mate => "mate", LinkKind::Wire => "wire" }, link.id.0)
        }
        [DesignOp::Sys(SysOp::RemoveLink(id))] => format!("Remove link {}", id.0),
        [DesignOp::Sys(SysOp::ReplaceLink(link))] => format!("Change link {}", link.id.0),
        [DesignOp::Sys(SysOp::SetMount(mount))] => format!("Mount {}", unit(mount.unit)),
        [DesignOp::Sys(SysOp::ClearMount(id))] => format!("Unmount {}", unit(*id)),
        _ => tool.to_owned(),
    };
    Ok(Plan::Edit(Resolved { ops, place: vec![], label }))
}

/// The inspector card's typed value, with the same source-site and quantity checks.
pub fn value_set(design: &Design, docs: Option<&Docs>, tool: &str, args: &[(String, String)]) -> Result<Plan, Diag> {
    check_tool(tool, &VALUE_SET, args)?;
    let (unit, comp) = required_part(design, args, "comp")?;
    let current = comp.value.as_ref().ok_or_else(|| error("value", format_args!("{} has no value to set", comp.refdes)))?;
    let site = docs.and_then(|d| d.map().value_site(&CompRef { board: unit.id, comp: comp.id }));
    site_text(site, &comp.path).map_err(|e| error("value", e))?;
    let value = parse_value(arg(args, "value").expect("required field checked"), current, &comp.part.kind).map_err(|e| error("value", e))?;
    Ok(Plan::Edit(Resolved {
        label: format!("Set {} to {}", comp.refdes, value.text),
        ops: vec![DesignOp::Circuit { unit: unit.id, op: CircuitOp::SetValue { comp: comp.id, value } }],
        place: vec![],
    }))
}

/// The tray's drop or the board pane's drag, including the labels displaced by a move.
pub fn part_place(design: &Design, _docs: Option<&Docs>, tool: &str, args: &[(String, String)]) -> Result<Plan, Diag> {
    check_tool(tool, &PART_PLACE, args)?;
    let (unit, comp) = required_part(design, args, "part")?;
    let board = unit.board.as_ref().ok_or_else(|| error("part", format_args!("{} has no board", unit.name)))?;
    let at = read_point_mm(arg(args, "at").expect("required field checked"))
        .ok_or_else(|| error("at", "expected a point in mm, as [12mm 30mm]"))?;
    let rot = arg(args, "rot").map(|text| {
        let text = text.trim();
        let degrees = text.strip_suffix("deg").or_else(|| text.strip_suffix('°')).unwrap_or(text).trim_end();
        degrees.parse::<f64>().ok().filter(|v| v.is_finite()).map(|v| Angle::deg(v % 360.0))
            .ok_or_else(|| error("rot", "expected an angle in degrees"))
    }).transpose()?;
    let side = arg(args, "side").map(|text| match text.trim().strip_prefix('@').unwrap_or(text.trim()) {
        "top" => Side::Top,
        "bottom" => Side::Bottom,
        _ => unreachable!("choice checked"),
    });
    let refdes = &comp.refdes;
    if let Some(placed) = board.placed.iter().find(|p| p.comp == comp.id) {
        if rot.is_some_and(|r| r != placed.rot) {
            return Err(error("rot", format_args!("{refdes} is placed and keeps its rotation; only at moves it")));
        }
        if side.is_some_and(|s| s != placed.side) {
            return Err(error("side", format_args!("{refdes} is placed and keeps its side; only at moves it")));
        }
        let delta = |a: i32, b| a.checked_sub(b).ok_or_else(|| error("at", "move is outside the board coordinate range"));
        let by = P::new(delta(at.x, placed.at.x)?, delta(at.y, placed.at.y)?);
        let ops = vec![DesignOp::Board { unit: unit.id, op: BoardOp::Move { item: placed.id, by } }];
        let plan = legal_plan(unit, args, Resolved { ops, place: vec![], label: format!("Move {refdes}") }, PlacementEdit::Move(placed.id))?;
        if at == placed.at && matches!(plan, Plan::Edit(_)) {
            return Err(error("at", format_args!("{refdes} is there already")));
        }
        return Ok(plan);
    }
    let drop = placement_op(design, unit.id, comp.id, at, rot.unwrap_or(Angle::ZERO), side.unwrap_or(Side::Top))
        .ok_or_else(|| error("part", format_args!("{refdes} has no footprint to place")))?;
    legal_plan(unit, args, Resolved {
        ops: vec![],
        place: vec![Place { unit: unit.id, part: PartRef::Path(comp.path.clone()), at, rot, side }],
        label: format!("Place {refdes}"),
    }, PlacementEdit::Tray(&drop))
}

/// Read in circuit order. Each included unit indexes its placements and pins once. Part and prefix
/// reads collect the selected parts' nets in one pass. A miss sorts the available references.
pub fn design_read(design: &Design, args: &[(String, String)]) -> Result<String, Diag> {
    use fab_elec_core::{refdes_prefix, refdes_ranges, CompId, NetId};

    enum Filter<'a> { All, Part(CompId), Net(NetId), Prefix(&'a str) }

    DESIGN_READ.check_args(args)?;
    let mut units = units(design, args)?;
    let filter = if let Some(text) = arg(args, "only") {
        if let Some((unit, comp)) = find_part(units, "only", text)? {
            units = std::slice::from_ref(unit);
            Filter::Part(comp.id)
        } else {
            let matches = units.iter().filter_map(|unit| unit.circuit.net(text).map(|net| (unit, net)));
            if let Some((unit, net)) = unique(matches, "only", text)? {
                units = std::slice::from_ref(unit);
                Filter::Net(net.id)
            } else {
                Filter::Prefix(text)
            }
        }
    } else {
        Filter::All
    };
    let mut matched_unit = false;
    let mut out = String::from("{units: [");
    for unit in units {
        let mut parts = unit.circuit.comps.iter().filter(|comp| match filter {
            Filter::All => true,
            Filter::Part(id) => comp.id == id,
            Filter::Net(net) => comp.conns.iter().any(|&(_, id)| id == net),
            Filter::Prefix(prefix) => refdes_prefix(&comp.refdes).is_some_and(|p| p.eq_ignore_ascii_case(prefix)),
        }).peekable();
        if matches!(filter, Filter::Prefix(_)) && parts.peek().is_none() { continue; }
        matched_unit = true;
        write!(out, "{{unit: {} board: {} parts: [", quote(&unit.name), unit.board.is_some()).unwrap();
        let placed: HashSet<_> = unit.board.iter().flat_map(|board| board.placed.iter().map(|p| p.comp)).collect();
        let mut part_nets = matches!(filter, Filter::Part(_) | Filter::Prefix(_)).then(HashSet::new);
        for comp in parts {
            if let Some(nets) = &mut part_nets {
                nets.extend(comp.conns.iter().map(|&(_, net)| net));
            }
            let value = comp.value.as_ref().map_or("", |value| value.text.as_str());
            write!(out, "{{ref: {} path: {} kind: {} value: {} placed: {}}} ",
                quote(&comp.refdes), quote(&comp.path), quote(comp.part.kind.name()), quote(value), placed.contains(&comp.id)).unwrap();
        }
        out.push_str("] nets: [");
        let pins = unit.circuit.net_pins();
        for net in &unit.circuit.nets {
            if matches!(filter, Filter::Net(id) if id != net.id) || part_nets.as_ref().is_some_and(|ids| !ids.contains(&net.id)) { continue; }
            write!(out, "{{net: {} pins: [", quote(&net.name)).unwrap();
            for &(comp, pin) in &pins.0[net.id.ix()] {
                let comp = &unit.circuit.comps[comp.ix()];
                write!(out, "{} ", quote(&format!("{}.{}", comp.refdes, comp.part.pins[pin.ix()].name))).unwrap();
            }
            out.push_str("]} ");
        }
        out.push_str("]} ");
    }
    if let Filter::Prefix(text) = filter {
        if !matched_unit {
            let refs = refdes_ranges(units.iter().flat_map(|u| u.circuit.comps.iter().map(|c| c.refdes.as_str())));
            let refs = if refs.is_empty() { "none" } else { &refs };
            return Err(error("only", format_args!("no part, prefix or net {}; parts: {refs} (elec.design.read without only lists the nets)", text.escape_debug())));
        }
    }
    out.push_str("]}");
    Ok(out)
}

/// Findings listed per page of `elec.check.list`: a page stays well inside a tool reply.
const CHECK_PAGE: usize = 40;

/// The family a check belongs to, as `elec.check.list` names it.
pub(crate) fn check_family(kind: &CheckKind) -> &'static str {
    use CheckKind::*;
    match kind {
        PinConflict | UndrivenInput | PowerNotDriven | UnconnectedPin | SinglePinNet | DuplicateRefdes
        | MissingFootprint | VoltageDomain | RailBudget => "erc",
        Mech | LinkPinMap | LinkMismatch | SystemVolts | SystemSource => "system",
        SharpCorner => "route",
        Custom(_) => "custom",
        _ => "drc",
    }
}

/// The check's own name (`Clearance`), or a custom check's rule name.
fn check_name(kind: &CheckKind) -> String {
    match kind {
        CheckKind::Custom(name) => name.clone(),
        kind => format!("{kind:?}"),
    }
}

fn severity_name(severity: Severity) -> &'static str {
    match severity {
        Severity::Error => "error",
        Severity::Warning => "warning",
        Severity::Info => "info",
    }
}

fn mm_text(nm: i64) -> String {
    let mm = format!("{:.4}", nm as f64 / 1_000_000.0);
    let mm = mm.trim_end_matches('0').trim_end_matches('.');
    format!("{}mm", if mm == "-0" { "0" } else { mm })
}

fn measure_text(measure: Measure) -> String {
    match measure {
        Measure::Len(nm) => mm_text(nm.into()),
        Measure::Qty(qty) => {
            let unit = match qty.unit {
                QtyUnit::Ohm => "ohm",
                QtyUnit::Farad => "F",
                QtyUnit::Henry => "H",
                QtyUnit::Volt => "V",
                QtyUnit::Ampere => "A",
                QtyUnit::Hertz => "Hz",
                QtyUnit::Watt => "W",
                QtyUnit::Metre => "m",
                QtyUnit::Second => "s",
                QtyUnit::Count => "",
            };
            format!("{}{unit}", qty.si)
        }
    }
}

/// A finding's item in the words the edit actions take: a part by reference, a pin as R1.2, a
/// net by name, board copper by kind and net. O(parts) for a placed part's lookup.
fn sel_text(design: &Design, sel: &Sel) -> String {
    let comp = |c: &CompRef| design.unit(c.board).and_then(|unit| unit.circuit.comps.get(c.comp.ix()));
    match sel {
        Sel::Unit(id) => design.unit(*id).map_or_else(|| "a unit".to_owned(), |unit| unit.name.clone()),
        Sel::Comp(c) | Sel::Feature(FeatureRef::Part(c) | FeatureRef::Body(c) | FeatureRef::Port(c, _)) => {
            comp(c).map_or_else(|| "a part".to_owned(), |comp| comp.refdes.clone())
        }
        Sel::Pin(p) => comp(&CompRef { board: p.board, comp: p.comp }).map_or_else(|| "a pin".to_owned(), |comp| {
            comp.part.pins.get(p.pin.ix()).map_or_else(|| comp.refdes.clone(), |pin| format!("{}.{}", comp.refdes, pin.name))
        }),
        Sel::Net(n) => design.unit(n.board).and_then(|unit| unit.circuit.nets.get(n.net.ix()))
            .map_or_else(|| "a net".to_owned(), |net| net.name.clone()),
        Sel::Feature(FeatureRef::Pad(c, pad)) => {
            let board = design.unit(c.board).and_then(|unit| unit.board.as_ref());
            match board.and_then(|board| board.placed_of(c.comp)) {
                Some(placed) => placed.footprint.pads.get(usize::from(pad.0))
                    .map_or_else(|| placed.refdes.clone(), |land| format!("{} pad {}", placed.refdes, land.no)),
                None => "a pad".to_owned(),
            }
        }
        Sel::Item(unit, id) => {
            let Some(board) = design.unit(*unit).and_then(|unit| unit.board.as_ref()) else { return "board copper".to_owned() };
            let net = |net: fab_elec_core::NetId| board.nets.get(net.ix()).map(|net| net.name.clone());
            match board.locate(*id).map(|(kind, _)| kind) {
                Some(ItemKind::Placed) => board.placed_item(*id).map_or_else(|| "a part".to_owned(), |p| p.refdes.clone()),
                Some(ItemKind::Track) => board.tracks.iter().find(|t| t.id == *id).and_then(|t| net(t.net))
                    .map_or_else(|| "a track".to_owned(), |net| format!("the {net} track")),
                Some(ItemKind::Via) => board.vias.iter().find(|v| v.id == *id).and_then(|v| net(v.net))
                    .map_or_else(|| "a via".to_owned(), |net| format!("the {net} via")),
                Some(ItemKind::Zone) => board.zones.iter().find(|z| z.id == *id).and_then(|z| z.net).and_then(net)
                    .map_or_else(|| "a pour".to_owned(), |net| format!("the {net} pour")),
                Some(ItemKind::Keepout) => "a keepout".to_owned(),
                Some(ItemKind::Hole) => "a hole".to_owned(),
                _ => "board copper".to_owned(),
            }
        }
        Sel::Feature(FeatureRef::Hole(..) | FeatureRef::HoleSet(..)) => "a hole".to_owned(),
        Sel::Joint { mechanism, joint } => format!("joint {mechanism}.{joint}"),
        Sel::Solid(name) => name.clone(),
        other => format!("{other:?}"),
    }
}

/// The stages still running on the shown design, by name; empty when every check is current.
pub(crate) fn pending_checks(stale: StageFlags) -> Vec<&'static str> {
    [
        (StageFlags::EVAL, "evaluation"), (StageFlags::ENGINE, "engine"), (StageFlags::CONNECT, "connectivity"),
        (StageFlags::FILLS, "fills"), (StageFlags::DRC, "drc"), (StageFlags::MECH, "mechanical"),
        (StageFlags::CABLES, "cables"), (StageFlags::SYSNETS, "system nets"),
    ].into_iter().filter(|(flag, _)| stale.has(*flag)).map(|(_, name)| name).collect()
}

/// `elec.check.list`: the shown findings (the Checks pane's), errors first, one page. Findings
/// that wait for a pour's refill are counted, not listed, as the pane shows them pending.
/// O(findings log findings) for the order, O(findings x units) for elsewhere, O(page x items)
/// for the words.
pub fn check_list(snap: &DesignSnapshot, args: &[(String, String)]) -> Result<String, Diag> {
    CHECK_LIST.check_args(args)?;
    let design = snap.live();
    let unit = units(design, args)?;
    let unit = (unit.len() == 1 && arg(args, "unit").is_some()).then(|| unit[0].id);
    let only = arg(args, "only").map(|text| text.trim().trim_start_matches('@')).unwrap_or("all");
    let least = match arg(args, "severity").map(|text| text.trim().trim_start_matches('@')) {
        Some("error") => Severity::Error,
        Some("warning") => Severity::Warning,
        _ => Severity::Info,
    };
    let from = arg(args, "from").map_or(Ok(0), |text| text.trim().replace('_', "").parse::<usize>())
        .map_err(|_| error("from", "expected a finding index"))?;
    let awaiting = &snap.awaiting;
    // The counts, the total and the list all read the same unit and family: a reply never counts
    // findings it does not list. Another unit's findings are named by unit under elsewhere, so a
    // quiet unit is never read as a quiet design.
    let of_unit = |v: &Violation| unit.is_none() || v.unit == unit || v.items.iter().any(|sel| sel.unit() == unit);
    let of_family = |v: &Violation| only == "all" || check_family(&v.kind) == only;
    let shown = || snap.violations.value.iter().filter(|v| awaiting.binary_search(&v.id).is_err());
    let matching = || shown().filter(|v| of_unit(v) && of_family(v));
    let mut found: Vec<&Violation> = matching().filter(|v| v.severity >= least).collect();
    found.sort_by(|a, b| b.severity.cmp(&a.severity).then(a.id.cmp(&b.id)));
    let count = |severity| matching().filter(|v| v.severity == severity).count();
    let waiting = snap.violations.value.iter().filter(|v| awaiting.binary_search(&v.id).is_ok() && of_unit(v) && of_family(v)).count();
    let unit_of = |v: &Violation| v.unit.or_else(|| v.items.iter().find_map(Sel::unit)).and_then(|id| design.unit(id)).map_or("", |u| u.name.as_str());
    let mut elsewhere: Vec<(&str, usize, usize)> = Vec::new();
    if unit.is_some() {
        for v in shown().filter(|v| !of_unit(v) && of_family(v) && v.severity >= Severity::Warning) {
            let name = unit_of(v);
            let at = match elsewhere.iter().position(|e| e.0 == name) {
                Some(at) => at,
                None => {
                    elsewhere.push((name, 0, 0));
                    elsewhere.len() - 1
                }
            };
            match v.severity {
                Severity::Error => elsewhere[at].1 += 1,
                _ => elsewhere[at].2 += 1,
            }
        }
        elsewhere.sort_by(|a, b| a.0.cmp(b.0));
    }
    let pending = pending_checks(snap.stale);
    let mut out = String::new();
    write!(out, "{{current: {} pending: [", pending.is_empty()).unwrap();
    for name in &pending {
        write!(out, "{} ", quote(name)).unwrap();
    }
    write!(out, "] errors: {} warnings: {} awaiting: {} total: {}",
        count(Severity::Error), count(Severity::Warning), waiting, found.len()).unwrap();
    let end = found.len().min(from.saturating_add(CHECK_PAGE));
    if end < found.len() {
        write!(out, " next: {end}").unwrap();
    }
    if !elsewhere.is_empty() {
        out.push_str(" elsewhere: [");
        for (name, errors, warnings) in &elsewhere {
            write!(out, "{{unit: {} errors: {errors} warnings: {warnings}}} ", quote(name)).unwrap();
        }
        out.push(']');
    }
    out.push_str(" findings: [");
    for v in found.get(from..end).unwrap_or_default() {
        out.push_str(&finding_text(design, v));
        out.push(' ');
    }
    out.push_str("]}");
    Ok(out)
}

/// One finding as `elec.check.list` lists it: `{unit check family severity message fix at items
/// actual required}`, its items in the words the edit actions take. O(items).
pub fn finding_text(design: &Design, v: &Violation) -> String {
    let unit_name = v.unit.or_else(|| v.items.iter().find_map(Sel::unit)).and_then(|id| design.unit(id)).map_or("", |u| u.name.as_str());
    let mut out = String::new();
    write!(out, "{{unit: {} check: {} family: {} severity: {} message: {}",
        quote(unit_name), quote(&check_name(&v.kind)), check_family(&v.kind), severity_name(v.severity), quote(&v.message)).unwrap();
    if !v.fix_hint.is_empty() {
        write!(out, " fix: {}", quote(&v.fix_hint)).unwrap();
    }
    if let Some(at) = v.at {
        write!(out, " at: {}", list([mm_text(at.x.into()), mm_text(at.y.into())])).unwrap();
    }
    out.push_str(" items: [");
    for sel in &v.items {
        write!(out, "{} ", quote(&sel_text(design, sel))).unwrap();
    }
    out.push(']');
    if let Some(actual) = v.actual {
        write!(out, " actual: {}", quote(&measure_text(actual))).unwrap();
    }
    if let Some(required) = v.required {
        write!(out, " required: {}", quote(&measure_text(required))).unwrap();
    }
    out.push('}');
    out
}

/// Whether every check has run on the shown design: no stage `elec.check.list` names as pending.
pub fn checks_current(snap: &DesignSnapshot) -> bool {
    pending_checks(snap.stale).is_empty()
}
