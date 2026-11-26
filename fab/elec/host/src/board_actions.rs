//! Board placement edits. The Board strip, keys and catalog share the plans in the view.
use crate::edit_actions::{arg, board_unit, check_tool, error, find_part, units, Args, PART_PLACE};
use crate::{legal_plan::{legal_plan, legalise_scope, PlacementEdit, LEGALISE}, Docs, Job, Plan};
use fab_elec_board::placement::PlacementIndex;
use fab_elec_board::{BoardOp, Item};
use fab_elec_core::{Angle, CompId, Diag, ItemId, Side};
use fab_elec_design::action::{ActionDef, FieldDef, FieldKind, Risk};
use fab_elec_design::{Design, DesignOp, PartRef, Unit};
use fab_elec_route::{JobId, PlaceJob, PlaceMode};
use fab_elec_view::board::plan::{self, Axis, Edge};
use fab_elec_view::board::tray::placement_op;
use fab_elec_view::board::view::StitchPress;
use std::collections::BTreeSet;

const PARTS: FieldDef = FieldDef {
    name: "parts", kind: FieldKind::Text, required: true, default: "",
    help: "Placed parts on one board, by reference or path, comma-separated (R1, mcu.boot_r).",
};
const UNIT: FieldDef = FieldDef {
    name: "unit", kind: FieldKind::Text, required: false, default: "",
    help: "The unit's name when references or paths match more than one unit.",
};

const ROTATE: ActionDef = ActionDef {
    name: "elec.part.rotate", label: "Rotate parts", group: "Board", risk: Risk::Edit, keys: "R",
    help: "Rotate one part about its anchor, or a group about its combined courtyard centre. Pad-anchored track ends follow their pads.",
    returns: PART_PLACE.returns, inputs: &[],
    fields: &[PARTS, FieldDef {
        name: "by", kind: FieldKind::Num { unit: "deg" }, required: false, default: "90",
        help: "Degrees counter-clockwise. Use a negative angle to turn clockwise.",
    }, LEGALISE, UNIT],
};

const FLIP: ActionDef = ActionDef {
    name: "elec.part.flip", label: "Flip parts", group: "Board", risk: Risk::Edit, keys: "B",
    help: "Switch board sides and mirror left-right about the part's anchor or the group's courtyard centre. Anchored ends follow pads on their layer. An SMD pad leaving that layer releases its end, which checks can report as Dangling.",
    returns: PART_PLACE.returns, inputs: &[], fields: &[PARTS, LEGALISE, UNIT],
};

const ALIGN: ActionDef = ActionDef {
    name: "elec.part.align", label: "Align parts", group: "Board", risk: Risk::Edit, keys: "",
    help: "Align two or more parts by their courtyard edges or centres. Pad-anchored track ends follow.",
    returns: PART_PLACE.returns, inputs: &[],
    fields: &[PARTS, FieldDef {
        name: "edge", kind: FieldKind::Choice(&["left", "right", "top", "bottom", "centre_x", "centre_y"]),
        required: true, default: "", help: "The edge or centre line of the selection to align to.",
    }, LEGALISE, UNIT],
};

const DISTRIBUTE: ActionDef = ActionDef {
    name: "elec.part.distribute", label: "Distribute parts", group: "Board", risk: Risk::Edit, keys: "",
    help: "Space three or more parts with equal courtyard gaps along an axis. The two outer parts stay in place and pad-anchored track ends follow the moved parts.",
    returns: PART_PLACE.returns, inputs: &[],
    fields: &[PARTS, FieldDef {
        name: "axis", kind: FieldKind::Choice(&["x", "y"]), required: true, default: "",
        help: "The board axis along which to space the parts.",
    }, LEGALISE, UNIT],
};

pub(crate) const PANEL_HELD_REASON: &str = "the panel parts the case is built round stay where they stand; panel: @free places them too";

const PLACE: ActionDef = ActionDef {
    name: "elec.place", label: "Place parts", group: "Board", risk: Risk::Edit, keys: "",
    help: "Place parts on legal spots; locked parts stay fixed. @legalise moves illegally placed parts to the nearest legal spots. @place holds the standing panel parts by default and answers when the job ends. The other parts go where their nets and routing demand pull them, and a part with no room goes in the row under the board. A part placed again (its pose changes) takes off the unlocked tracks and vias laid to its old spot, hand-drawn ones included (the answer counts them by part in hand_drawn_removed; one undo brings them back), and the router's copper its new spot collides with; the rest of its nets and locked copper stay: route after placing. Repeated blocks (copies of one block definition) are then arranged like one copy, the one that leaves the least overflow, where that leaves no part without room, breaks no near and raises no overflow; conformed has a row per group: its copies, the copy the others follow (like), kept or why not, and the change in overflow and wirelength.",
    returns: PART_PLACE.returns, inputs: &[],
    fields: &[
        FieldDef { required: false, help: "Part references or paths, comma-separated. @legalise: empty selects every unlocked part on the board. @place: empty selects the circuit's parts the board does not place yet and those outside its outline; named parts select what to place again, subject to the panel hold.", ..PARTS },
        FieldDef { name: "mode", kind: FieldKind::Choice(&["legalise", "place"]), required: false, default: "legalise",
            help: "legalise: make the current placement legal with the least movement. place: place the parts afresh." },
        FieldDef { name: "all", kind: FieldKind::Bool, required: false, default: "false",
            help: "@place: select every unlocked part and bring the parts the board does not place yet. The panel setting applies." },
        FieldDef { name: "panel", kind: FieldKind::Choice(&["hold", "free"]), required: false, default: "hold",
            help: "@place: hold keeps the panel parts that stand on the board (those with a body port: keys, pots, jacks, lights, plugs) where they are, since the case is built round them; free places them too." },
        FieldDef { name: "pin", required: false, help: "@legalise: part references or paths to settle first, keeping their spots when legal.", ..PARTS },
        UNIT,
    ],
};

pub(crate) const PAIR_ROUTE: ActionDef = ActionDef {
    name: "elec.pair.route", label: "Route differential pair", group: "Board", risk: Risk::Edit, keys: "",
    help: "Route both tracks of a declared pair through centreline waypoints on one copper layer. A blocked pair ends together at the last legal station. Both tracks commit in one undo step.",
    returns: "seq: int  committed undo step
label: text  its name in the undo history
side: text  p_left or n_left looking along the route
p_start_stub: num  P start escape in mm
p_end_stub: num  P end escape in mm
n_start_stub: num  N start escape in mm
n_end_stub: num  N end escape in mm
p_uncoupled: num  new P length outside coupling in mm
n_uncoupled: num  new N length outside coupling in mm
p_length: num  resulting whole P length in mm
n_length: num  resulting whole N length in mm
skew: num  absolute difference of the whole-net lengths in mm
blocked_leg: text  net that stopped a partial pair
need: num  envelope plus clearances at the obstruction in mm
reason: text  what blocked the partial pair",
    inputs: &[], fields: &[UNIT,
        FieldDef { name: "pair", kind: FieldKind::Text, required: true, default: "", help: "Name of either net of a declared differential pair." },
        FieldDef { name: "from", kind: FieldKind::Text, required: true, default: "", help: "Start at a part.pin, part:pad, or a board point [x y] in mm on a pair pad or track end." },
        FieldDef { name: "to", kind: FieldKind::Text, required: true, default: "", help: "Arrival pin, pad or point. An open point ends both legs at one station." },
        FieldDef { name: "waypoints", kind: FieldKind::Points, required: false, default: "", help: "Ordered spine points [[x y] [x y]] in mm." },
        FieldDef { name: "layer", kind: FieldKind::Layer, required: false, default: "", help: "One copper layer, inferred from the endpoints when omitted." },
    ],
};

pub(crate) fn pair_route(design: &Design, _: Option<&Docs>, tool: &str, args: &Args) -> Result<Plan, Diag> {
    use fab_elec_route::pair::{Endpoint, Route};
    use fab_elec_lang::board_doc::read_point_mm;
    check_tool(tool, &PAIR_ROUTE, args)?;
    let unit = board_unit(design, "unit", arg(args, "unit"))?;
    let board = unit.board.as_ref().ok_or_else(|| error("unit", "this unit has no board"))?;
    let name = arg(args, "pair").ok_or_else(|| error("pair", "expected a pair net"))?;
    let net = board.net(name).ok_or_else(|| error("pair", format!("{} has no net {name}", unit.name)))?;
    let endpoint = |field| {
        let text = arg(args, field).ok_or_else(|| error(field, "expected a part pin or board point"))?;
        if let Some(p) = read_point_mm(text) { return Ok(Endpoint::Point(p)); }
        let (part, pin) = text.rsplit_once(':').or_else(|| text.rsplit_once('.'))
            .ok_or_else(|| error(field, "expected part.pin, part:pad or a point [x y] in mm"))?;
        let (_, comp) = find_part(std::slice::from_ref(unit), field, part)?
            .ok_or_else(|| error(field, format!("no part {part}")))?;
        let placed = board.placed_of(comp.id).ok_or_else(|| error(field, format!("{part} is not placed")))?;
        let pad = if text.contains(':') { placed.footprint.pad(pin) } else {
            comp.part.pin(pin).and_then(|p| comp.part.pin_def(p))
                .and_then(|p| p.pads.iter().find_map(|no| placed.footprint.pad(no)))
        }.ok_or_else(|| error(field, format!("{text} has no board pad")))?;
        Ok(Endpoint::Anchor(fab_elec_board::Anchor::Pad { placed: placed.id, pad }))
    };
    let mut waypoints = Vec::new();
    if let Some(text) = arg(args, "waypoints") {
        let list = text.trim().strip_prefix('[').and_then(|s| s.strip_suffix(']'))
            .ok_or_else(|| error("waypoints", "expected [[x y] [x y]] in mm"))?;
        let mut rest = list.trim();
        while !rest.is_empty() {
            let end = rest.find(']').ok_or_else(|| error("waypoints", "expected a point [x y]"))?;
            let point = read_point_mm(&rest[..=end]).ok_or_else(|| error("waypoints", "invalid waypoint in mm"))?;
            waypoints.push(point);
            rest = rest[end+1..].trim_start();
            if let Some(next) = rest.strip_prefix(',') { rest = next.trim_start(); }
        }
    }
    let layer = arg(args,"layer").map(|s| fab_elec_core::LayerId::parse(s.trim_start_matches('@'))
        .ok_or_else(|| error("layer", "expected one copper layer; pair vias come next"))).transpose()?;
    Ok(Plan::Job(Job::Pair(fab_elec_route::request::PairJob {
        job: JobId(0), unit: unit.id, route: Route { net, from: endpoint("from")?, to: endpoint("to")?, waypoints, layer },
    })))
}

pub(crate) const TUNE: ActionDef = ActionDef {
    name: "elec.net.tune", label: "Tune net length", group: "Board", risk: Risk::Edit, keys: "",
    help: "Lengthen one track span with clearance-checked 45-degree folds. The worker measures the whole net including via barrels and commits what fits in one undo step.",
    returns: "seq: int  the step it committed
label: text  its name in the undo history
reached: num  resulting whole-net length in mm
target: num  requested whole-net length in mm
short_by: num  length still missing in mm
uncoupled_added: num  additional uncoupled length in mm, for a pair leg",
    inputs: &[], fields: &[UNIT,
        FieldDef { name: "net", kind: FieldKind::Text, required: true, default: "", help: "Net to lengthen." },
        FieldDef { name: "target", kind: FieldKind::Text, required: false, default: "", help: "Length in mm, or mate for a declared pair. Omit for the longest member of the net's single length match." },
        FieldDef { name: "at", kind: FieldKind::Point, required: false, default: "", help: "Board point [x y] in mm to select a nearby span. Omit for the longest available straight span." },
        FieldDef { name: "side", kind: FieldKind::Choice(&["left", "right", "both"]), required: false, default: "", help: "Side relative to the track direction. Defaults to alternating, or away from the mate on a pair." },
        FieldDef { name: "amplitude", kind: FieldKind::Num { unit: "mm" }, required: false, default: "", help: "Maximum excursion from the chord in mm." },
        FieldDef { name: "pitch", kind: FieldKind::Num { unit: "mm" }, required: false, default: "", help: "Spacing of parallel fold legs in mm. Defaults to four track widths and respects clearance." },
    ],
};

pub(crate) fn tune(design: &Design, _: Option<&Docs>, tool: &str, args: &Args) -> Result<Plan, Diag> {
    use fab_elec_route::meander::{Side, Span, Target, Tune};
    check_tool(tool, &TUNE, args)?;
    let unit = board_unit(design, "unit", arg(args, "unit"))?;
    let name = arg(args, "net").expect("required field checked");
    let net = unit
        .board
        .as_ref()
        .and_then(|b| b.net(name))
        .ok_or_else(|| error("net", format!("{} has no net {name}", unit.name)))?;
    let length = |field, value: &str, max: i64| -> Result<i64, Diag> {
        let mm = value.trim().strip_suffix("mm").unwrap_or(value.trim()).trim_end().parse::<f64>().ok();
        mm.map(|n| (n * 1e6).round())
            .filter(|n| *n >= 1. && *n <= max as f64)
            .map(|n| n as i64)
            .ok_or_else(|| error(field, "expected a positive length in mm"))
    };
    let dimension =
        |field| arg(args, field).map(|s| length(field, s, i64::from(i32::MAX)).map(|n| n as i32)).transpose();
    let target = match arg(args, "target") {
        None => Target::Match,
        Some("mate" | "@mate") => Target::Mate,
        Some(s) => Target::Length(length("target", s, i64::MAX)?),
    };
    let span = arg(args, "at")
        .map(|text| {
            fab_elec_lang::board_doc::read_point_mm(text)
                .map(Span::Nearest)
                .ok_or_else(|| error("at", "expected a board point, as [12mm 30mm]"))
        })
        .transpose()?
        .unwrap_or(Span::Longest);
    let side = arg(args, "side")
        .map(|s| match s.trim_start_matches('@') {
            "left" => Ok(Side::Left),
            "right" => Ok(Side::Right),
            "both" => Ok(Side::Both),
            _ => Err(error("side", "expected left, right or both")),
        })
        .transpose()?;
    Ok(Plan::Job(Job::Tune(fab_elec_route::request::TuneJob {
        job: JobId(0),
        unit: unit.id,
        circuit: unit.circuit.clone(),
        tune: Tune { net, target, span, side, amplitude: dimension("amplitude")?, pitch: dimension("pitch")? },
    })))
}

pub(crate) const NET_LENGTH: ActionDef = ActionDef {
    name: "elec.net.length", label: "Net lengths", group: "Board", risk: Risk::Read, keys: "",
    help: "Routed track length plus via barrels between anchored copper layers. A span without layer thickness is omitted and reported as “vias not counted: no stackup”. Read every net in a length match, or name nets to measure. A match is checked only when every member is fully routed.",
    returns: "unit: text
measurement: text  track length plus via barrels, or “vias not counted: no stackup” when layer thickness is unavailable
nets: [{..}]
  net: text
  length_nm: int  total routed length in nanometres
  partial: bool  routing is incomplete
  branched: bool  the net has more than two pins
  matches: [{..}]
    nets: [text]
    reference: text  the longest member
    difference_nm: int  the reference length minus this net's length
    tolerance_nm: int
    status: choice partial|within|broken  the state of the whole match",
    inputs: &[], fields: &[UNIT, FieldDef { name: "nets", kind: FieldKind::Text, required: false, default: "",
        help: "Net names on this board, comma-separated or as a list. Omit for every matched net." }],
};

pub(crate) fn net_length(snap: &fab_elec_view::DesignSnapshot, args: &Args) -> Result<String, Diag> {
    use fab_elec_board::length::LengthReadout;
    use makepad_widgets_core::makepad_script::syntax::write::{list, quote};
    use std::fmt::Write;

    NET_LENGTH.check_args(args)?;
    let unit = board_unit(&snap.design, "unit", arg(args, "unit"))?;
    let board = unit.board.as_deref().expect("board_unit selects a board");
    let conn = snap.conn.iter().find(|c| c.of == fab_elec_view::StateOf::Committed && c.unit == Some(unit.id))
        .filter(|c| !snap.checking(unit.id) && c.value.generation == board.generation)
        .ok_or_else(|| error("unit", "board connectivity is still being checked"))?;
    let mut requested = Vec::new();
    if let Some(text) = arg(args, "nets") {
        let text = text.trim();
        let resolve = |name: &str| board.net(name)
            .ok_or_else(|| error("nets", format_args!("{} has no net {name}", unit.name)));
        if let Some(net) = board.net(text) {
            requested.push(net);
        } else if text.starts_with('[') {
            use makepad_widgets_core::makepad_script::syntax;
            let invalid = || error("nets", "expected a list of net names");
            let doc = syntax::parse(text).map_err(|_| invalid())?;
            let [array] = doc.statements() else { return Err(invalid()) };
            for entry in array.items().ok_or_else(invalid)? {
                let name = entry.string().or_else(|| entry.ident()).or_else(|| entry.name()).ok_or_else(invalid)?;
                requested.push(resolve(name)?);
            }
        } else {
            for name in text.split(',').map(str::trim) {
                requested.push(resolve(name)?);
            }
        }
    } else {
        requested.extend(board.length_matches.iter().flat_map(|m| m.nets.iter().copied()));
    }
    let mut seen = BTreeSet::new();
    requested.retain(|n| seen.insert(*n));
    let read = LengthReadout::new(board, &unit.circuit, &conn.value, &requested);
    let mut memberships = std::collections::HashMap::<_, Vec<String>>::new();
    let name = |n: fab_elec_core::NetId| quote(&board.nets[n.ix()].name);
    for group in &board.length_matches {
        let Some(cmp) = group.compare(&read.lengths) else { continue };
        let members = list(group.nets.iter().copied().map(name));
        let status = if !cmp.complete { "partial" } else if cmp.spread > i64::from(group.tol) { "broken" } else { "within" };
        for &n in &group.nets {
            let diff = read.lengths[&cmp.reference].nm - read.lengths[&n].nm;
            memberships.entry(n).or_default().push(format!(
                "{{nets: {members} reference: {} difference_nm: {diff} tolerance_nm: {} status: @{status}}}", name(cmp.reference), group.tol));
        }
    }
    let mut out = format!("{{unit: {} measurement: {} nets: [", quote(&unit.name), quote(read.measurement.text()));
    for n in requested {
        let length = read.lengths[&n];
        write!(out, "{{net: {} length_nm: {} partial: {} branched: {} matches: {}}} ",
            name(n), length.nm, length.partial, read.branched.contains(&n), list(memberships.remove(&n).unwrap_or_default())).unwrap();
    }
    out.push_str("]}");
    Ok(out)
}

pub(crate) const STITCH: ActionDef = ActionDef {
    name: StitchPress::ACTION, label: "Stitch pours", group: "Board", risk: Risk::Edit, keys: "",
    help: "Join overlapping pours of a net with a grid of through vias. Use a zone item id to limit sites to that pour. Existing vias and clearances are respected.",
    returns: "seq: int  the step it committed
label: text  the net and number of vias in the undo history",
    inputs: &[], fields: &[
        FieldDef { required: true, help: "Board unit name.", ..UNIT },
        FieldDef { name: "net", kind: FieldKind::Text, required: false, default: "", help: "Net name. Defaults to the zone's net, then the circuit ground." },
        FieldDef { name: "zone", kind: FieldKind::Int { min: 0, max: u32::MAX as i64 }, required: false, default: "", help: "Zone item id. Omit to stitch all the net's pours." },
        FieldDef { name: "pitch", kind: FieldKind::Num { unit: "mm" }, required: true, default: "", help: "Grid pitch in millimetres." },
        FieldDef { name: "pad", kind: FieldKind::Num { unit: "mm" }, required: false, default: "", help: "Via pad diameter in millimetres. Defaults to the net class or board size." },
        FieldDef { name: "drill", kind: FieldKind::Num { unit: "mm" }, required: false, default: "", help: "Via drill diameter in millimetres. Defaults to the net class or board size." },
    ],
};

pub(crate) fn stitch(design: &Design, results: &fab_elec_design::engines::DesignResults,
    tool: &str, args: &Args) -> Result<Plan, Diag> {
    check_tool(tool, &STITCH, args)?;
    let unit = board_unit(design, "unit", arg(args, "unit"))?;
    let board = unit.board.as_deref().unwrap();
    let zone = arg(args, "zone").map(|v| v.trim().replace('_', "").parse::<u32>().map(ItemId)
        .map_err(|_| error("zone", "expected a zone item id"))).transpose()?;
    let net = match (arg(args, "net"), zone) {
        (Some(name), _) => board.net(name).ok_or_else(|| error("net", format!("{} has no net {name}", unit.name)))?,
        (None, Some(id)) => board.zones.iter().find(|z| z.id == id).and_then(|z| z.net)
            .ok_or_else(|| error("zone", format!("zone {} does not name a pour with a net", id.0)))?,
        (None, None) => plan::ground_net(design, unit.id).ok_or_else(|| error("net", "name a net or a zone to stitch"))?,
    };
    let dimension = |field| -> Result<Option<i32>, Diag> {
        arg(args, field).map(|text| {
            let max = f64::from(i32::MAX) / 1e6;
            let mm = fab_elec_view::board::status::parse_mm(text, 0.0, max)
                .ok_or_else(|| error(field, "expected a positive length in millimetres"))?;
            let nm = fab_elec_core::mm(mm);
            if nm <= 0 { return Err(error(field, "expected a positive length in nanometres")); }
            Ok(nm)
        }).transpose()
    };
    let fills = results.boards.iter().find(|b| b.unit == unit.id)
        .ok_or_else(|| error("stitch", format!("the {} pour is refilling: stitch again in a moment", board.nets[net.ix()].name)))?;
    let edit = plan::stitch(design, unit.id, fills, net, zone, dimension("pitch")?.unwrap(), dimension("pad")?, dimension("drill")?)
        .map_err(|msg| error("stitch", msg))?;
    Ok(Plan::Edit(fab_elec_design::Resolved { ops: edit.ops, label: edit.label, place: Vec::new() }))
}

const BOARD_UNIT: FieldDef = FieldDef { required: true, help: "Board unit name.", ..UNIT };
const RECT: FieldDef = FieldDef { name: "rect", kind: FieldKind::Text, required: false, default: "",
    help: "Two opposite corners in mm, as x0 y0 x1 y1. Supply exactly one of rect and points." };
const POINTS: FieldDef = FieldDef { name: "points", kind: FieldKind::Text, required: false, default: "",
    help: "Polygon vertices in mm, as x y, x y, ... . The last vertex closes to the first." };
const SHAPE_RETURNS: &str = "seq: int  the step it committed
label: text  its name in the undo history";

pub(crate) const OUTLINE: ActionDef = ActionDef {
    name: "elec.board.outline", label: "Set board outline", group: "Board", risk: Risk::Edit, keys: "",
    help: "Replace the outer board contour with a rectangle or polygon. Existing inner contours and cutouts are kept.",
    returns: SHAPE_RETURNS, inputs: &[], fields: &[BOARD_UNIT, RECT, POINTS],
};
pub(crate) const KEEPOUT: ActionDef = ActionDef {
    name: "elec.keepout.add", label: "Add keepout", group: "Board", risk: Risk::Edit, keys: "",
    help: "Draw a rectangular or polygonal keepout. The board checks report conflicts with its restrictions.",
    returns: SHAPE_RETURNS, inputs: &[], fields: &[BOARD_UNIT, RECT, POINTS,
        FieldDef { name: "layers", kind: FieldKind::Text, required: false, default: "",
            help: "Layer names, comma-separated (F.Cu, B.Cu). Omit for every copper layer on this board." },
        FieldDef { name: "forbid", kind: FieldKind::Text, required: false, default: "",
            help: "Comma-separated tracks, vias, pads, zones, footprints. Omit to forbid all five." },
        FieldDef { name: "reason", kind: FieldKind::Text, required: false, default: "", help: "Why the area must stay clear." },
    ],
};
pub(crate) const CUTOUT: ActionDef = ActionDef {
    name: "elec.cutout.add", label: "Add cutout", group: "Board", risk: Risk::Edit, keys: "",
    help: "Draw a rectangular or polygonal opening through the board.",
    returns: SHAPE_RETURNS, inputs: &[], fields: &[BOARD_UNIT, RECT, POINTS,
        FieldDef { name: "name", kind: FieldKind::Text, required: false, default: "Cutout", help: "Name of the opening." },
    ],
};

/// Read signed board coordinates in millimetres, rejecting overflow before converting to Nm.
fn coordinates<const N: usize>(field: &str, text: &str) -> Result<[i32; N], Diag> {
    let mut words = text.split_whitespace();
    let mut values = [0; N];
    for value in &mut values {
        let word = words.next().ok_or_else(|| error(field, format!("expected {N} coordinates in mm")))?;
        let nm = word.strip_suffix("mm").unwrap_or(word).parse::<f64>().ok().map(|mm| (mm * 1e6).round())
            .filter(|nm| (f64::from(i32::MIN)..=f64::from(i32::MAX)).contains(nm))
            .ok_or_else(|| error(field, format!("invalid board coordinate {word} in mm")))?;
        *value = nm as i32;
    }
    if words.next().is_some() { return Err(error(field, format!("expected {N} coordinates in mm"))); }
    Ok(values)
}

pub(crate) fn board_shape(design: &Design, _: Option<&Docs>, tool: &str, args: &Args) -> Result<Plan, Diag> {
    use fab_elec_core::{BBox, LayerId, LayerSet, P};
    use fab_elec_parts::Forbid;
    let action = [OUTLINE, KEEPOUT, CUTOUT].into_iter().find(|a| a.name == tool)
        .ok_or_else(|| error("tool", format!("unknown {tool}")))?;
    check_tool(tool, &action, args)?;
    let unit = board_unit(design, "unit", arg(args, "unit"))?;
    let (field, points) = match (arg(args, "rect"), arg(args, "points")) {
        (Some(text), None) => {
            let [x0, y0, x1, y1] = coordinates("rect", text)?;
            ("rect", plan::rect_points(BBox::new(P::new(x0, y0), P::new(x1, y1))).to_vec())
        }
        (None, Some(text)) => {
            let points = text.split(',').map(|pair| coordinates("points", pair).map(|[x, y]| P::new(x, y)))
                .collect::<Result<Vec<_>, _>>()?;
            ("points", points)
        }
        _ => return Err(error("shape", "supply exactly one of rect and points")),
    };
    let edit = if tool == OUTLINE.name {
        plan::outline(design, unit.id, &points)
    } else if tool == CUTOUT.name {
        plan::cutout(design, unit.id, &points, arg(args, "name").unwrap_or("Cutout"))
    } else {
        let board = unit.board.as_deref().ok_or_else(|| error("unit", "no board here"))?;
        let layers = match arg(args, "layers") {
            None => board.copper_set(),
            Some(list) => list.split(',').try_fold(LayerSet::EMPTY, |layers, word| {
                LayerId::parse(word.trim()).map(|l| layers.with(l)).ok_or_else(|| error("layers", format!("unknown layer {word}")))
            })?,
        };
        let forbid = match arg(args, "forbid") {
            None => Forbid::ALL,
            Some(list) => list.split(',').try_fold(Forbid::default(), |flags, word| {
                let flag = match word.trim().trim_start_matches('@') {
                    "tracks" => Forbid::TRACKS, "vias" => Forbid::VIAS, "pads" => Forbid::PADS,
                    "zones" => Forbid::ZONES, "footprints" => Forbid::FOOTPRINTS,
                    _ => return Err(error("forbid", format!("unknown restriction {word}"))),
                };
                Ok(flags.with(flag))
            })?,
        };
        plan::keepout(design, unit.id, &points, layers, forbid, arg(args, "reason").unwrap_or(""))
    }.map_err(|msg| error(field, msg))?;
    Ok(Plan::Edit(fab_elec_design::Resolved { ops: edit.ops, label: edit.label, place: Vec::new() }))
}

pub(crate) static ACTIONS: &[ActionDef] = &[ROTATE, FLIP, ALIGN, DISTRIBUTE, PLACE, NET_LENGTH, TUNE, PAIR_ROUTE, STITCH, OUTLINE, KEEPOUT, CUTOUT];

fn parts<'a>(design: &'a Design, tool: &str, action: &ActionDef, args: &Args) -> Result<(&'a Unit, Vec<CompId>), Diag> {
    check_tool(tool, action, args)?;
    let (board, parts) = named_parts(units(design, args)?, "parts", arg(args, "parts").expect("required field checked"))?;
    Ok((board.ok_or_else(|| error("parts", "name at least one part"))?, parts))
}

fn named_parts<'a>(scope: &'a [Unit], field: &str, text: &str) -> Result<(Option<&'a Unit>, Vec<CompId>), Diag> {
    let text = text.trim();
    let text = text.strip_prefix('[').and_then(|s| s.strip_suffix(']')).unwrap_or(text);
    let mut board: Option<&Unit> = None;
    let mut parts = Vec::new();
    for name in text.split(|c: char| c == ',' || c.is_whitespace()).map(|s| s.trim_matches('"')).filter(|s| !s.is_empty()) {
        let (unit, comp) = find_part(scope, field, name)?
            .ok_or_else(|| error(field, format_args!("no part {name}; elec.design.read lists them")))?;
        if board.is_some_and(|other| other.id != unit.id) {
            return Err(error(field, "select parts on one board"));
        }
        board = Some(unit);
        parts.push(comp.id);
    }
    Ok((board, parts))
}

pub(crate) fn place(design: &Design, _: Option<&Docs>, tool: &str, args: &Args) -> Result<Plan, Diag> {
    check_tool(tool, &PLACE, args)?;
    let (unit, ids) = named_parts(units(design, args)?, "parts", arg(args, "parts").unwrap_or(""))?;
    let unit = match unit {
        Some(unit) => unit,
        None => board_unit(design, "unit", arg(args, "unit"))?,
    };
    if arg(args, "mode").is_some_and(|mode| mode.trim().trim_start_matches('@') == "place") {
        let all = match arg(args, "all").map(str::trim) {
            None | Some("false") => false,
            Some("true") => true,
            Some(_) => return Err(error("all", "expected true or false")),
        };
        let hold_panel = arg(args, "panel").is_none_or(|panel| panel.trim().trim_start_matches('@') != "free");
        return place_job(design, unit, ids, all, hold_panel);
    }
    let (_, pin) = named_parts(std::slice::from_ref(unit), "pin", arg(args, "pin").unwrap_or(""))?;
    legalise_scope(unit, &ids, pin)
}

/// `elec.place` with `@place`: the job the worker runs (`fab_elec_route::place::job`). The parts it
/// brings are the circuit's parts the board does not place yet, made as the tray's drop makes them
/// and put in the row the tray's own layout gives under the board (`plan::place_unplaced`): where
/// a part with no room stays. A part the circuit fixes to a spot is brought onto it. Its scope: the parts `named`; with `all`, every part on the board
/// (the job's problem leaves locked ones fixed); else the parts outside the outline. The brought
/// parts join the scope unless parts are named and they are not among them. By default the panel
/// parts already standing inside the outline (`plan::held_panel`) leave the scope before the
/// brought parts join it.
/// Scope selection costs O(n log n) for n parts and builds at most one placement index.
fn place_job(design: &Design, unit: &Unit, named: Vec<CompId>, all: bool, hold_panel: bool) -> Result<Plan, Diag> {
    let board = unit.board.as_deref().ok_or_else(|| error("unit", format_args!("{} has no board", unit.name)))?;
    let named: BTreeSet<_> = named.into_iter().collect();
    let rows = plan::place_unplaced(design, unit.id).map(|(rows, _)| rows).unwrap_or_default();
    let mut add = Vec::with_capacity(rows.len());
    for row in rows {
        let PartRef::Path(path) = &row.part else { continue };
        let Some(comp) = unit.circuit.comp(path) else { continue };
        if !named.is_empty() && !named.contains(&comp.id) { continue }
        // A part without a footprint has nothing to place: it stays in the tray.
        let Some(DesignOp::Board { op: BoardOp::Add(Item::Placed(mut part)), .. }) =
            placement_op(design, unit.id, comp.id, row.at, row.rot.unwrap_or(Angle::ZERO), row.side.unwrap_or(Side::Top)) else { continue };
        part.id = ItemId(board.next_item + add.len() as u32);
        add.push(part);
    }
    let mut scope = if !named.is_empty() {
        named
    } else if all {
        board.placed.iter().map(|p| p.comp).collect()
    } else {
        PlacementIndex::new(board, None).outside_outline().into_iter().map(|(p, _)| p.comp).collect()
    };
    let mut held = 0;
    if hold_panel {
        let scoped: Vec<CompId> = scope.iter().copied().collect();
        for comp in plan::held_panel(unit, &scoped) {
            scope.remove(&comp);
            held += 1;
        }
    }
    scope.extend(add.iter().map(|p| p.comp));
    if scope.is_empty() {
        if held > 0 {
            return Err(error("parts", format_args!("panel parts held: {held}. {PANEL_HELD_REASON}")));
        }
        return Err(error("parts", format_args!(
            "every part of {} is on its board, inside the outline: name parts to place them again, or say all: true", unit.name,
        )));
    }
    Ok(Plan::Job(Job::Place(PlaceJob {
        job: JobId(0), unit: unit.id, scope: scope.into_iter().collect(), mode: PlaceMode::Place, held, rooms: Vec::new(), plan: None, add, circuit: unit.circuit.clone(),
    })))
}

pub(crate) fn rotate(design: &Design, _: Option<&Docs>, tool: &str, args: &Args) -> Result<Plan, Diag> {
    let (unit, ids) = parts(design, tool, &ROTATE, args)?;
    let by = arg(args, "by").unwrap_or("90").trim();
    let degrees = by.strip_suffix("deg").or_else(|| by.strip_suffix('°')).unwrap_or(by).trim_end()
        .parse::<f64>().ok().filter(|n| n.is_finite()).ok_or_else(|| error("by", "expected an angle in degrees"))?;
    let edit = plan::rotate(design, unit.id, &ids, Angle::deg(degrees % 360.0)).map_err(|why| error("parts", why))?;
    legal_plan(unit, args, edit, PlacementEdit::Group(&ids))
}

pub(crate) fn flip(design: &Design, _: Option<&Docs>, tool: &str, args: &Args) -> Result<Plan, Diag> {
    let (unit, ids) = parts(design, tool, &FLIP, args)?;
    let edit = plan::flip(design, unit.id, &ids).map_err(|why| error("parts", why))?;
    legal_plan(unit, args, edit, PlacementEdit::Group(&ids))
}

pub(crate) fn align(design: &Design, _: Option<&Docs>, tool: &str, args: &Args) -> Result<Plan, Diag> {
    let (unit, ids) = parts(design, tool, &ALIGN, args)?;
    let edge = match arg(args, "edge").expect("required field checked").trim().trim_start_matches('@') {
        "left" => Edge::Left, "right" => Edge::Right, "top" => Edge::Top, "bottom" => Edge::Bottom,
        "centre_x" => Edge::CentreX, "centre_y" => Edge::CentreY,
        _ => unreachable!("choice checked"),
    };
    let edit = plan::align(design, unit.id, &ids, edge).map_err(|why| error("parts", why))?;
    legal_plan(unit, args, edit, PlacementEdit::Group(&ids))
}

pub(crate) fn distribute(design: &Design, _: Option<&Docs>, tool: &str, args: &Args) -> Result<Plan, Diag> {
    let (unit, ids) = parts(design, tool, &DISTRIBUTE, args)?;
    let axis = match arg(args, "axis").expect("required field checked").trim().trim_start_matches('@') {
        "x" => Axis::X, "y" => Axis::Y, _ => unreachable!("choice checked"),
    };
    let edit = plan::distribute(design, unit.id, &ids, axis).map_err(|why| error("parts", why))?;
    legal_plan(unit, args, edit, PlacementEdit::Group(&ids))
}
