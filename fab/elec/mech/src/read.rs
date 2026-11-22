//! Read a board's mechanical facts through the shared UI and AI action catalog.
//!
//! A call computes the mechanical summary once per board it reads. Writing a full reply is
//! O(parts + ports + holes + outline points), including keepout and profile vertices. A filter
//! miss also sorts the placed references. Nothing runs per frame.

use crate::board_mech::{board_mech_with, decimal, escaped, length, point, ring, side, BoardMech};
use fab_elec_circuit::Component;
use fab_elec_core::{mm, refdes_prefix, refdes_ranges, BBox};
use fab_elec_design::action::{ActionDef, FieldDef, FieldKind, Risk};
use fab_elec_design::Design;
use fab_elec_parts::{BodyPortKind, Forbid, PartKind};
use std::fmt::Write;

/// The board reader's command and tool definition.
pub static ACTIONS: [ActionDef; 1] = [ActionDef {
    name: "elec.board.read",
    label: "Read board",
    group: "Board",
    help: "Read a board's mechanical facts in board millimetres: size, outline, thickness, holes, ports, keepouts, height envelope, and each part's placement. These are the same facts a case script reads with board(\"name\").",
    returns: "board: text  the selected or sole board unit name
frame: text  the origin, axis directions and length unit
size: [num mm]  outline bounding width and height
outline: [points]  even-odd contours in board millimetres, arcs polygonised within 0.05 mm
thickness: num mm
holes: [{..}]
  at: point  board x and y in millimetres
  d: num mm  drill width, the narrow side of a slot
  plated: bool
ports: [{..}]  a reference or prefix keeps the matching parts' ports. A port kind keeps just that kind.
  part: text  the component refdes
  kind: choice plug|light|press|air|cable|shaft
  side: choice top|bottom
  at: [num mm]  board x, y and z
  dir: [num]  unit direction out of the part
  opening: [num mm]  profile bounding width and height before clearance
keepouts: [{..}]
  area: [points]  all polygon rings in board millimetres
  layers: [text]
  forbid: [choice tracks|vias|pads|zones|footprints]
  z: [num mm]  minimum and maximum height off the named faces, empty for every height
  reason: text
envelope: {..}
  top: num mm  height above the top face
  bottom: num mm  height below the bottom face
  covered: num mm  height above the top face of the tallest part a lid covers (a part plugged from above passes through the lid)
parts: [{..}]  placed parts matching only; a port kind keeps the owners of the selected ports
  ref: text  the component refdes
  footprint: text
  at: point  the board placement's x and y in millimetres
  rot: num deg  counter-clockwise rotation in the board frame
  side: choice top|bottom
  height: num mm  height off its face, zero when no body is stated
boards: [{..}]  when board is omitted and several boards exist, the reply contains boards and pick
  board: text  the board unit name
  size: [num mm]  outline bounding width and height
  parts: int  placed parts
  ports: int  mechanical ports
pick: text  how to read one of the listed boards",
    fields: &[
        FieldDef {
            name: "board",
            kind: FieldKind::Text,
            help: "The design's board unit name. Omit to read the sole board or list each board's name, size, placed part count and port count.",
            required: false,
            default: "",
        },
        FieldDef {
            name: "only",
            kind: FieldKind::Text,
            help: "Filter entries by part reference (SW1), letter prefix (SW, case-insensitive), part kind (button, led, connector, hole, keepout, part), or port kind (plug, light, press, air, cable, shaft). Board dimensions and envelope remain complete.",
            required: false,
            default: "",
        },
    ],
    inputs: &[],
    risk: Risk::Read,
    keys: "",
}];

/// Register the board reader for both UI commands and AI tools.
pub fn register_actions() {
    fab_elec_design::action::register(&ACTIONS);
}

/// Read the selected or sole board. With several boards and no name, summarise each in design order.
/// Each board read computes its drills and calls `board_mech_with` once. The summary also walks
/// each board's outline to measure its size. A filter miss sorts the selected board's references.
pub fn board_read(design: &Design, board: Option<&str>, only: Option<&str>) -> Result<String, String> {
    let boards: Vec<_> = design.units.iter()
        .filter_map(|unit| unit.board.as_deref().map(|board| (unit, board)))
        .collect();
    let names = || boards.iter().map(|(unit, _)| quoted(&unit.name)).collect::<Vec<_>>().join(", ");
    if boards.is_empty() {
        return Err("The design has no board units; add a board unit and pass its name as board.".into());
    }
    let (unit, board) = match board {
        Some(name) => boards.iter().find(|(unit, _)| unit.name == name).copied()
            .ok_or_else(|| format!("Unknown board {}; pass board as one of: {}.", quoted(name), names()))?,
        None if boards.len() == 1 => boards[0],
        None => {
            let mut out = String::from("{boards: [");
            for (unit, board) in &boards {
                let mech = board_mech_with(board, &unit.circuit, &board.drills());
                write!(out, "{{board: {} size: {} parts: {} ports: {}}} ",
                    quoted(&unit.name), outline_size(&mech), board.placed.len(), mech.ports.len()).unwrap();
            }
            out.push_str("] pick: \"pass board: one of these names to read it\"}");
            return Ok(out);
        }
    };
    let mech = board_mech_with(board, &unit.circuit, &board.drills());
    let refs = || board.placed.iter().map(|placed| unit.circuit.comps[placed.comp.ix()].refdes.as_str());
    let filter = match only {
        None => Filter::All,
        Some("part") => Filter::Parts,
        Some("hole") => Filter::Holes,
        Some("keepout") => Filter::Keepouts,
        Some("button") => Filter::Button,
        Some("led") => Filter::Led,
        Some("connector") => Filter::Connector,
        Some(name) if refs().any(|r| r == name) => Filter::Ref(name),
        Some(kind @ ("plug" | "light" | "press" | "air" | "cable" | "shaft")) => Filter::Port(kind),
        Some(prefix) => Filter::Prefix(prefix),
    };
    // BoardMech uses SceneBoard's z frame; core::geom fixes the outline origin and planar axes.
    let frame = "Origin at the outline bounding box's lower left; X right, Y up, Z out of the top side, measured from the bottom face (z = 0; top at thickness); lengths in millimetres.";
    let mut out = format!("{{board: {} frame: {} size: {} outline: [", quoted(&unit.name), quoted(frame), outline_size(&mech));
    for contour in &mech.outline {
        write!(out, "{} ", ring(&contour.to_ring(mm(0.05)))).unwrap();
    }
    write!(out, "] thickness: {} holes: [", length(mech.thickness)).unwrap();
    let mut matched = false;
    if matches!(filter, Filter::All | Filter::Holes) {
        for hole in &mech.holes {
            write!(out, "{{at: {} d: {} plated: {}}} ", point(hole.at), length(hole.d), hole.plated).unwrap();
            matched = true;
        }
    }
    out.push_str("] ports: [");
    for port in &mech.ports {
        let comp = &unit.circuit.comps[port.comp.ix()];
        let kind = port_kind(port.kind);
        let keep = match filter {
            Filter::Button => port.kind == BodyPortKind::Press,
            Filter::Led => port.kind == BodyPortKind::Light,
            Filter::Connector => matches!(port.kind, BodyPortKind::Plug | BodyPortKind::Cable),
            Filter::Port(wanted) => kind == wanted,
            Filter::Parts => false,
            _ => filter.keeps_part(comp),
        };
        if !keep {
            continue;
        }
        write!(out, "{{part: {} kind: @{} side: @{} at: [{}, {}, {}] dir: [{}, {}, {}] opening: {}}} ",
            quoted(&comp.refdes), kind, side(port.side), length(port.at[0]), length(port.at[1]), length(port.at[2]),
            decimal(port.dir[0], 6), decimal(port.dir[1], 6), decimal(port.dir[2], 6), size(port.profile.bbox())).unwrap();
        matched = true;
    }
    out.push_str("] keepouts: [");
    if matches!(filter, Filter::All | Filter::Keepouts) {
        for keepout in &mech.keepouts {
            out.push_str("{area: [");
            for points in keepout.area.rings() {
                write!(out, "{} ", ring(points)).unwrap();
            }
            out.push_str("] layers: [");
            for layer in keepout.layers.iter() {
                write!(out, "{} ", quoted(&layer.name())).unwrap();
            }
            out.push_str("] forbid: [");
            for (flag, name) in [(Forbid::TRACKS, "tracks"), (Forbid::VIAS, "vias"), (Forbid::PADS, "pads"), (Forbid::ZONES, "zones"), (Forbid::FOOTPRINTS, "footprints")] {
                if keepout.forbid.has(flag) {
                    write!(out, "@{name} ").unwrap();
                }
            }
            out.push_str("] z: [");
            if let Some((min, max)) = keepout.z {
                write!(out, "{}, {}", length(min), length(max)).unwrap();
            }
            write!(out, "] reason: {}}} ", quoted(&keepout.reason)).unwrap();
            matched = true;
        }
    }
    write!(out, "] envelope: {{top: {} bottom: {} covered: {}}} parts: [", length(mech.envelope[0]), length(mech.envelope[1]), length(mech.covered)).unwrap();
    for placed in &board.placed {
        let comp = &unit.circuit.comps[placed.comp.ix()];
        if !filter.keeps_part(comp) {
            continue;
        }
        write!(out, "{{ref: {} footprint: {} at: {} rot: {}deg side: @{} height: {}}} ",
            quoted(&comp.refdes), quoted(&placed.footprint.name), point(placed.at), decimal(placed.rot.degrees(), 6),
            side(placed.side), length(comp.part.body.as_ref().map_or(0, |body| body.height))).unwrap();
        matched = true;
    }
    out.push_str("]}");
    if let Some(only) = only.filter(|_| !matched) {
        let refs = refdes_ranges(refs());
        let refs = if refs.is_empty() { "none placed" } else { &refs };
        return Err(format!("Nothing matches only {}; use a ref or prefix ({refs}), part kind (button, led, connector, hole, keepout, part) or port kind (plug, light, press, air, cable, shaft).", quoted(only)));
    }
    Ok(out)
}

enum Filter<'a> {
    All, Parts, Holes, Keepouts, Button, Led, Connector, Port(&'a str), Ref(&'a str), Prefix(&'a str),
}

impl Filter<'_> {
    fn keeps_part(&self, comp: &Component) -> bool {
        match *self {
            Self::All | Self::Parts => true,
            Self::Holes | Self::Keepouts => false,
            Self::Button => comp.part.kind == PartKind::Switch,
            Self::Led => comp.part.kind == PartKind::Led,
            Self::Connector => comp.part.kind == PartKind::Connector,
            Self::Port(kind) => comp.part.body.iter().any(|body| body.ports.iter().any(|port| port_kind(port.kind) == kind)),
            Self::Ref(name) => comp.refdes == name,
            Self::Prefix(prefix) => refdes_prefix(&comp.refdes).is_some_and(|p| p.eq_ignore_ascii_case(prefix)),
        }
    }
}

fn port_kind(kind: BodyPortKind) -> &'static str {
    match kind {
        BodyPortKind::Plug => "plug",
        BodyPortKind::Light => "light",
        BodyPortKind::Press => "press",
        BodyPortKind::Air => "air",
        BodyPortKind::Cable => "cable",
        BodyPortKind::Shaft => "shaft",
    }
}

fn outline_size(mech: &BoardMech) -> String {
    size(mech.outline.iter().fold(BBox::EMPTY, |bounds, contour| bounds.union(contour.bbox())))
}

fn size(bounds: BBox) -> String {
    format!("[{}, {}]", length(bounds.width()), length(bounds.height()))
}

fn quoted(text: &str) -> String {
    format!("\"{}\"", escaped(text).replace('\n', "\\n").replace('\r', "\\r").replace('\t', "\\t"))
}
