//! The self-drawing sheet (E2.2): blocks laid out bottom up, each alone in its own frame, then
//! placed as one rigid box in its parent. Inside a block: molecules hang on their pins, the
//! anchors (chips, connectors, child blocks, molecules with no host) are placed in layers by
//! signal flow, signal nets run as orthogonal wires in the channels between layers, rails are
//! tags at their pins (power up, ground down, outward on side pins), and labels go where nothing
//! is. A block's own ports sit on its frame: left and right ports at the ends of the flow, top and
//! bottom ports straight over their pin; a bus port gathers its member wires on a spine.
//!
//! Stability (the mental map): every choice is a pure function of structure and source order. A
//! block's layout depends only on its own contents, so an edit inside a block moves only that
//! block's contents and, through its box size, its siblings' positions; nothing outside its
//! parent chain changes order.
//!
//! Coordinates: integer grid steps, x right, y up. Cost: per block, the layered placement and
//! the channel router over that block's anchors (tens of items), the label placer over its
//! labels; the whole sheet is linear in blocks times that.

use crate::channels::{self, Channel, Face, Group, ROpts, Term};
use crate::graph::{LayoutGraph, NetKind, NetStyle, PortKind, Pt, Role};
use crate::labels::{self, LabelOpts, LabelReq, Obstacle, Spot};
use crate::layered::{self, LEdge, LGraph, LNode, LOpts, LPort, Side};
use crate::molecules::{self, MolKind, Molecule};
use fab_elec_core::{BBox, PQuad, P};
use std::ops::ControlFlow;

/// A box: min x, min y, max x, max y.
pub type Bx = [i32; 4];

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SheetMode {
    #[default]
    Schematic,
    Blocks,
}

/// Spacing, grid steps.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Opts {
    pub mode: SheetMode,
    /// The smallest gap between two layers' boxes.
    pub layer_gap: i32,
    /// Free space above and below each anchor.
    pub margin: i32,
    /// Room inside a block frame round its contents.
    pub pad: i32,
    /// The title band at the top of a block frame.
    pub title: i32,
    /// Gap between the members of an array of blocks.
    pub array_gap: i32,
    /// Width over height that a level's unconnected items are packed to: the shape of the pane
    /// showing the sheet (the view passes its own).
    pub aspect: f64,
}

impl Default for Opts {
    fn default() -> Self {
        Self { mode: SheetMode::Schematic, layer_gap: 5, margin: 1, pad: 2, title: 2, array_gap: 2, aspect: 1.5 }
    }
}

/// Where a part is: its origin and orientation (mirror x first, then quarter turns
/// counter-clockwise). `glyph`: draw the family glyph of a small part instead of its symbol.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct PartPlace {
    pub at: Pt,
    pub turn: u8,
    pub mirror: bool,
    pub glyph: bool,
    /// The body box on the sheet, pins included.
    pub body: Bx,
    /// The block whose frame it is drawn in.
    pub block: u32,
    /// False for parts not on this sheet.
    pub placed: bool,
    /// Drawn with its symbol's quarter-turn fit (`adapt::sheet_unit`), as its geometry here was.
    pub quarter: bool,
    /// How a user turns it: 1 by quarters for a part placed on its own (an anchor, or a molecule
    /// without a host), where a sheet hint's turn holds; 0 for a molecule member, whose turn
    /// follows its host or chain.
    pub turn_step: u8,
}

/// A port on a block's frame.
#[derive(Clone, Debug, PartialEq)]
pub struct FramePort {
    pub name: String,
    pub at: Pt,
    pub side: Side,
    pub bus: bool,
    pub kind: PortKind,
    pub ground: bool,
    pub open: bool,
}

/// A block's frame. The root's frame is the whole sheet.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Frame {
    pub planned: bool,
    pub role: Option<String>,
    pub rect: Bx,
    /// The block's arrange origin in world coordinates, before frame padding and the title.
    pub origin: Pt,
    pub ports: Vec<FramePort>,
    pub placed: bool,
    /// The depth of the title band at the frame's top (where the block is pressed and dragged).
    pub title: i32,
}

/// One orthogonal polyline of a net (or of a bus, for `SheetLayout::buses`).
#[derive(Clone, Debug, PartialEq)]
pub struct Run {
    pub net: u32,
    pub pts: Vec<Pt>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TagKind {
    Power,
    Ground,
    /// A net name flag: the net continues elsewhere by name.
    Label,
    NoConnect,
}

/// A tag at a pin end or at the end of a rail's stub: `dir` is the way it points, one of the four
/// axis directions (a rail: supply up, ground down).
#[derive(Clone, Debug, PartialEq)]
pub struct Tag {
    pub net: Option<u32>,
    pub at: Pt,
    pub dir: [i32; 2],
    pub kind: TagKind,
    pub text: String,
    /// A rail tag turned at the end of a stub from a side pin where another pin's wire runs a row
    /// away: the compact form that fits inside the pin's row band. `Some(out)`: the stub's outward
    /// direction (a supply's name sits on that side of its bar).
    pub band: Option<[i32; 2]>,
    /// Where an up or down supply tag's name sits: 0 centred beyond its bar, -1 left of the
    /// bar, 1 right of it (world x), at the bar's height.
    pub side: i8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TextRole {
    Refdes,
    Value,
    Net,
    Title,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Owner {
    Part(u32),
    Net(u32),
    /// A net's live strip chart, by graph net index: no label lines; its box is the strip.
    Trace(u32),
    /// A stacked strip, indexed into `LayoutGraph::buses`: one row per member net in member
    /// order, no label lines; its box is the whole stack.
    Bundle(u32),
    Bus(u32),
    Block(u32),
}

/// A placed text or trace strip: its box (world, y up) and its lines, top first.
#[derive(Clone, Debug, PartialEq)]
pub struct Label {
    pub owner: Owner,
    pub lines: Vec<(String, TextRole)>,
    pub bx: [f64; 4],
    /// A leader from the owner to the label when it had to move away.
    pub leader: Option<[[f64; 2]; 2]>,
}

/// A bus entry: a short diagonal from a member wire's end onto its bus.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Entry {
    pub net: u32,
    pub at: Pt,
    pub to: Pt,
}

/// The whole sheet. Indexed like the graph: `parts[node]`, `frames[block]`.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct SheetLayout {
    pub parts: Vec<PartPlace>,
    pub frames: Vec<Frame>,
    pub wires: Vec<Run>,
    /// Bus runs; `net` is the bus index.
    pub buses: Vec<Run>,
    pub entries: Vec<Entry>,
    pub tags: Vec<Tag>,
    pub junctions: Vec<(u32, Pt)>,
    pub labels: Vec<Label>,
    pub bounds: Bx,
    /// What a drag of each part moves, indexed by node (empty for a reveal).
    pub movers: Vec<Mover>,
    /// Parts in pinned columns that gave way (to a drag's lead, or to the column before them):
    /// not where their own pins or the free drawing put them. Pinned where they are drawn, the
    /// sheet lays out the same without a lead.
    pub yielded: Vec<u32>,
    /// Blocks in pinned columns that gave way, as `yielded` for parts.
    pub yielded_blocks: Vec<u32>,
    /// Per block: whether a hint places it (a drag moves it). Not the root, and not a member of an
    /// array drawn as one item.
    pub block_movable: Vec<bool>,
    /// Some level packed unconnected items in rows to `Opts::aspect`: the sheet's shape follows
    /// the pane's.
    pub packed: bool,
}

/// What a drag of a part moves. Molecules are rigid, so only an anchor or a free chain's first part
/// takes a position hint.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Mover {
    /// The part takes the hint itself.
    #[default]
    Itself,
    /// A later member of a free chain: the chain's first part takes the hint and the chain follows.
    Head(u32),
    /// A member of a molecule that hangs on a pin (node, port): it rides with that part.
    Rides { host: u32, port: u32 },
}

/// The world point of a block's content origin (local x right, y down).
pub fn block_origin(sheet: &SheetLayout, block: u32) -> Option<Pt> {
    let frame = sheet.frames.get(block as usize)?;
    frame.placed.then_some(frame.origin)
}

/// Convert a world point (y up) to a block-local hint (y down).
/// A run's points in a canonical direction (from its lesser end), for comparing drawn wires:
/// a run and its reverse draw the same line.
pub fn canon_run(pts: &[Pt]) -> Vec<Pt> {
    if pts.last() < pts.first() { pts.iter().rev().copied().collect() } else { pts.to_vec() }
}

pub fn to_local(sheet: &SheetLayout, block: u32, world: Pt) -> Option<Pt> {
    let origin = block_origin(sheet, block)?;
    Some([world[0] - origin[0], origin[1] - world[1]])
}

/// Convert a block-local hint (y down) to a world point (y up).
pub fn to_world(sheet: &SheetLayout, block: u32, local: Pt) -> Option<Pt> {
    let origin = block_origin(sheet, block)?;
    Some([origin[0] + local[0], origin[1] - local[1]])
}

impl SheetLayout {
    /// Move the whole drawing by `d` world steps: a view keeps a block where it was across
    /// relayouts (a part dragged left grows its block left instead of shifting its mates right).
    /// Block-local hints are unchanged. O(items).
    pub fn translate(&mut self, d: Pt) {
        if d == [0, 0] {
            return;
        }
        for p in &mut self.parts {
            p.at = add(p.at, d);
            p.body = bx_add(p.body, d);
        }
        for f in &mut self.frames {
            f.rect = bx_add(f.rect, d);
            f.origin = add(f.origin, d);
            for port in &mut f.ports {
                port.at = add(port.at, d);
            }
        }
        for r in self.wires.iter_mut().chain(self.buses.iter_mut()) {
            for p in &mut r.pts {
                *p = add(*p, d);
            }
        }
        for e in &mut self.entries {
            e.at = add(e.at, d);
            e.to = add(e.to, d);
        }
        for t in &mut self.tags {
            t.at = add(t.at, d);
        }
        for (_, j) in &mut self.junctions {
            *j = add(*j, d);
        }
        let (dx, dy) = (d[0] as f64, d[1] as f64);
        for l in &mut self.labels {
            l.bx = [l.bx[0] + dx, l.bx[1] + dy, l.bx[2] + dx, l.bx[3] + dy];
            if let Some(leader) = &mut l.leader {
                for p in leader.iter_mut() {
                    *p = [p[0] + dx, p[1] + dy];
                }
            }
        }
        self.bounds = bx_add(self.bounds, d);
    }
}

/// Text heights in grid steps per role.
pub const TEXT_REFDES: f64 = 0.9;
pub const TEXT_VALUE: f64 = 0.8;
pub const TEXT_NET: f64 = 0.8;
/// Half the height of a name flag (across its pin): under half a pin step.
pub const LABEL_HALF: f64 = 0.45;
pub const TEXT_TITLE: f64 = 1.1;
/// The spacing between text lines, as a fraction of the height.
pub const LINE_GAP: f64 = 0.25;

/// A strip's width in world units: 48 device pixels at the symbols rung's 8 pixels per unit.
pub const TRACE_W: f64 = 6.0;
/// A strip's height in world units: 12 device pixels at the symbols rung's 8 pixels per unit.
pub const TRACE_H: f64 = 1.5;
/// A row's height in a stacked bus strip, in world units.
pub const BUNDLE_ROW: f64 = 0.6;
/// Label obstacle owners: a net's wires and tags, a bus's lines, and the stacked strips (which
/// own no obstacle). Each range is offset past any index, since net and bus indices overlap.
pub const NET_OWNER: u32 = 1_000_000;
const BUS_OWNER: u32 = 2_000_000;
pub(crate) const BUNDLE_OWNER: u32 = 3_000_000;

/// A live strip's request beside a net's longest horizontal wire piece (at least 3 long, the
/// strip above or below it, right-aligned first so it follows the net's name), or else its
/// longest vertical piece (at least 2 long, the strip to its right or left, bottom-aligned before
/// top-aligned so it clears a label beside the part above). Optional and below
/// every name and part label, so strips never move them. `owner` lets the strip ignore its own
/// net's wires.
pub(crate) fn strip_request(owner: u32, pieces: impl Iterator<Item = (Pt, Pt)>) -> Option<LabelReq> {
    let (mut flat, mut upright): (Option<(Pt, Pt)>, Option<(Pt, Pt)>) = (None, None);
    let key = |a: Pt, b: Pt, k: usize| ((a[k] - b[k]).abs(), -a[k].min(b[k]));
    for (a, b) in pieces {
        if a[1] == b[1] && flat.is_none_or(|(p, q)| key(a, b, 0) >= key(p, q, 0)) {
            flat = Some((a, b));
        } else if a[0] == b[0] && a[1] != b[1] && upright.is_none_or(|(p, q)| key(a, b, 1) >= key(p, q, 1)) {
            upright = Some((a, b));
        }
    }
    let (anchor, spots) = match (flat, upright) {
        (Some((a, b)), _) if (a[0] - b[0]).abs() >= 3 => {
            let y = a[1] as f64;
            ([a[0].min(b[0]) as f64 + 0.5, y - 0.1, a[0].max(b[0]) as f64 - 0.5, y + 0.1],
                vec![Spot::AboveRight, Spot::BelowRight, Spot::Above, Spot::Below])
        }
        (_, Some((a, b))) if (a[1] - b[1]).abs() >= 2 => {
            let x = a[0] as f64;
            ([x - 0.1, a[1].min(b[1]) as f64 + 0.5, x + 0.1, a[1].max(b[1]) as f64 - 0.5],
                vec![Spot::Right, Spot::RightBottom, Spot::RightTop, Spot::Left, Spot::LeftBottom, Spot::LeftTop])
        }
        _ => return None,
    };
    Some(LabelReq { anchor, size: [TRACE_W, TRACE_H], spots, priority: -1, owner, optional: true })
}

/// The width of `text` drawn `h` high, from the drawn font's advances (`crate::text`).
pub fn text_w(text: &str, h: f64) -> f64 {
    crate::text::width(text, h)
}

// ---------------------------------------------------------------------------------------------
// Geometry helpers.

fn rot(p: Pt, turn: u8, mirror: bool) -> Pt {
    let p = if mirror { [-p[0], p[1]] } else { p };
    match turn & 3 {
        0 => p,
        1 => [-p[1], p[0]],
        2 => [-p[0], -p[1]],
        _ => [p[1], -p[0]],
    }
}

fn side_dir(s: Side) -> [i32; 2] {
    match s {
        Side::Left => [-1, 0],
        Side::Right => [1, 0],
        Side::Top => [0, 1],
        Side::Bottom => [0, -1],
    }
}

fn dir_side(d: [i32; 2]) -> Side {
    match d {
        [x, _] if x < 0 => Side::Left,
        [x, _] if x > 0 => Side::Right,
        [_, y] if y > 0 => Side::Top,
        _ => Side::Bottom,
    }
}

fn rot_side(s: Side, turn: u8, mirror: bool) -> Side {
    dir_side(rot(side_dir(s), turn, mirror))
}

fn bx_rot(b: Bx, turn: u8, mirror: bool) -> Bx {
    let a = rot([b[0], b[1]], turn, mirror);
    let c = rot([b[2], b[3]], turn, mirror);
    [a[0].min(c[0]), a[1].min(c[1]), a[0].max(c[0]), a[1].max(c[1])]
}

fn bx_add(b: Bx, d: Pt) -> Bx {
    [b[0] + d[0], b[1] + d[1], b[2] + d[0], b[3] + d[1]]
}

/// Boxes that overlap with positive area, or a stroke (a zero-width box) that crosses a box or
/// another stroke; touching edges do not count.
fn bx_hit(a: Bx, b: Bx) -> bool {
    (a[0] < b[2] && b[0] < a[2] && a[1] < b[3] && b[1] < a[3])
        || ((a[0] == a[2] || a[1] == a[3] || b[0] == b[2] || b[1] == b[3]) && crosses(a, b))
}

/// For boxes where one is a stroke: they share an interior point (a stroke along another box's
/// edge does not).
fn crosses(a: Bx, b: Bx) -> bool {
    let span = |lo0: i32, hi0: i32, lo1: i32, hi1: i32| {
        if lo0 == hi0 && lo1 == hi1 { lo0 == lo1 } else if lo0 == hi0 { lo1 < lo0 && lo0 < hi1 } else if lo1 == hi1 { lo0 < lo1 && lo1 < hi0 } else { lo0 < hi1 && lo1 < hi0 }
    };
    span(a[0], a[2], b[0], b[2]) && span(a[1], a[3], b[1], b[3])
}

/// How a part's rail tags may be drawn (see `Ctx::part_rails`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Rails {
    Auto,
    Band,
}

fn bx_union(a: Bx, b: Bx) -> Bx {
    [a[0].min(b[0]), a[1].min(b[1]), a[2].max(b[2]), a[3].max(b[3])]
}

fn bx_pt(b: Bx, p: Pt) -> Bx {
    bx_union(b, [p[0], p[1], p[0], p[1]])
}

const EMPTY: Bx = [i32::MAX, i32::MAX, i32::MIN, i32::MIN];

fn add(a: Pt, b: Pt) -> Pt {
    [a[0] + b[0], a[1] + b[1]]
}

fn scale(d: [i32; 2], k: i32) -> Pt {
    [d[0] * k, d[1] * k]
}

/// The extent of a tag at `at` pointing `dir`, as a box (text included for power tags).
/// The grid box a tag occupies: the item the layout keeps clear of bodies, other tags and labels.
pub fn tag_box(t: &Tag) -> Bx {
    tag_box_of(t.kind, t.at, t.dir, &t.text, t.band, t.side)
}

/// The box of a tag as drawn (the explorer's glyphs, strokes included): what labels avoid and the
/// overlap count measures. `tag_box` rounds it outward to the grid.
/// The name flags part `n` of `g` gets when it is placed free at `part` (`arrange`): one at each
/// pin end on a signal net that continues to another pin, each a box on the sheet. A paste uses
/// it to keep a free part's whole ink inside its block. O(pins of `g`) for the port nets.
pub fn free_flags(g: &LayoutGraph, n: u32, part: &PartPlace) -> Vec<[f64; 4]> {
    let Some(node) = g.nodes.get(n as usize) else { return Vec::new() };
    let nets = g.port_nets();
    node.ports.iter().enumerate().filter_map(|(p, port)| {
        let net = &g.nets[nets[n as usize][p]? as usize];
        (net.kind == NetKind::Signal && net.pins.len() > 1).then(|| {
            let at = add(part.at, rot(port.at, part.turn, part.mirror));
            tag_item_box_of(TagKind::Label, at, side_dir(rot_side(port.side, part.turn, part.mirror)), &short_name(&net.name), None, 0)
        })
    }).collect()
}

pub fn tag_item_box(t: &Tag) -> [f64; 4] {
    tag_item_box_of(t.kind, t.at, t.dir, &t.text, t.band, t.side)
}

fn tag_item_box_of(kind: TagKind, at: Pt, dir: [i32; 2], text: &str, band: Option<[i32; 2]>, side: i8) -> [f64; 4] {
    let at = at.map(f64::from);
    let d = dir.map(f64::from);
    // A box given along the tag's direction (a0..a1) and across it (c0..c1).
    let local = |a0: f64, a1: f64, c0: f64, c1: f64| {
        let pts = [(a0, c0), (a0, c1), (a1, c0), (a1, c1)].map(|(a, c)| [at[0] + d[0] * a - d[1] * c, at[1] + d[1] * a + d[0] * c]);
        let xs = pts.map(|p| p[0]);
        let ys = pts.map(|p| p[1]);
        [xs.iter().copied().fold(f64::MAX, f64::min), ys.iter().copied().fold(f64::MAX, f64::min),
         xs.iter().copied().fold(f64::MIN, f64::max), ys.iter().copied().fold(f64::MIN, f64::max)]
    };
    let tw = text_w(text, TEXT_NET);
    let u = |a: [f64; 4], b: [f64; 4]| [a[0].min(b[0]), a[1].min(b[1]), a[2].max(b[2]), a[3].max(b[3])];
    match (kind, band) {
        (TagKind::NoConnect, _) => [at[0] - 0.35, at[1] - 0.35, at[0] + 0.35, at[1] + 0.35],
        (TagKind::Ground, None) => local(0.0, 1.7, -0.65, 0.65),
        (TagKind::Ground, Some(_)) => local(0.0, 0.8, -0.55, 0.55),
        (TagKind::Power, None) if matches!(dir, [0, -1] | [0, 1]) && !text.is_empty() && side != 0 => {
            let x0 = at[0] + f64::from(side) * 0.9;
            let x1 = x0 + f64::from(side) * tw;
            let y = at[1] + d[1];
            let half = TEXT_NET * 0.5 + 0.05;
            u(local(0.0, 1.4, -0.65, 0.65), [x0.min(x1), y - half, x0.max(x1), y + half])
        }
        (TagKind::Power, None) if d[1] != 0.0 => local(0.0, 2.2, -(tw * 0.5 + 0.05).max(0.65), (tw * 0.5 + 0.05).max(0.65)),
        (TagKind::Power, None) => local(0.0, 1.4 + tw, -0.65, 0.65),
        (TagKind::Power, Some(out)) => {
            let bar = local(0.0, 0.4, -0.55, 0.55);
            if text.is_empty() {
                return bar;
            }
            let x0 = at[0] + out[0] as f64 * 0.7;
            let x1 = at[0] + out[0] as f64 * (0.7 + tw);
            u(bar, [x0.min(x1), at[1] + d[1] * 0.35 - 0.4, x0.max(x1), at[1] + d[1] * 0.35 + 0.4])
        }
        // A name flag reads along its pin, sideways on a top or bottom pin, and is no taller than
        // a pin step, so flags on neighbouring pins never cross.
        (TagKind::Label, _) => local(0.0, 1.3 + tw, -LABEL_HALF, LABEL_HALF),
    }
}

fn tag_box_of(kind: TagKind, at: Pt, dir: [i32; 2], text: &str, band: Option<[i32; 2]>, side: i8) -> Bx {
    if let Some(out) = band {
        // The compact rail: a bar one step from the stub's end, within the band; a supply's name
        // beside it on the outward side.
        let b = [at[0] - 1, at[1].min(at[1] + dir[1]), at[0] + 1, at[1].max(at[1] + dir[1])];
        if kind == TagKind::Power && !text.is_empty() {
            let far = add(at, scale(out, 1 + text_w(text, TEXT_NET).ceil() as i32));
            return bx_union(b, [far[0].min(at[0]), b[1], far[0].max(at[0]), b[3]]);
        }
        return b;
    }
    if kind == TagKind::Power && matches!(dir, [0, -1] | [0, 1]) && !text.is_empty() && side != 0 {
        let b = tag_item_box_of(kind, at, dir, text, band, side);
        return [b[0].floor() as i32, b[1].floor() as i32, b[2].ceil() as i32, b[3].ceil() as i32];
    }
    let along = match kind {
        TagKind::Power => {
            if dir[1] != 0 {
                3
            } else {
                2 + text_w(text, TEXT_NET).ceil() as i32
            }
        }
        TagKind::Ground => 2,
        TagKind::Label => 2 + text_w(text, TEXT_NET).ceil() as i32,
        TagKind::NoConnect => 1,
    };
    let across = if kind == TagKind::Power && dir[1] != 0 {
        (text_w(text, TEXT_NET) * 0.5).ceil() as i32 + 1
    } else {
        1
    };
    let end = add(at, scale(dir, along));
    let perp = [dir[1].abs(), dir[0].abs()];
    let b = bx_pt([at[0], at[1], at[0], at[1]], end);
    [b[0] - perp[0] * across, b[1] - perp[1] * across, b[2] + perp[0] * across, b[3] + perp[1] * across]
}

fn item_box_distance(a: [f64; 4], b: [f64; 4]) -> f64 {
    let dx = (a[0] - b[2]).max(b[0] - a[2]).max(0.0);
    let dy = (a[1] - b[3]).max(b[1] - a[3]).max(0.0);
    dx.hypot(dy)
}

/// The ink around a name flag. The quadtree finds neighbours on its run; the fractional boxes
/// decide clearance. Tag indices come first, so moving one leaves every other reservation intact.
#[derive(Default)]
struct FlagSpace {
    index: PQuad<usize>,
    items: Vec<(labels::Bx, Option<u32>)>,
}

impl FlagSpace {
    fn bounds(b: labels::Bx) -> BBox {
        BBox::new(P::new(b[0].floor() as i32, b[1].floor() as i32), P::new(b[2].ceil() as i32, b[3].ceil() as i32))
    }

    fn insert(&mut self, b: labels::Bx, wire: Option<u32>) {
        self.index.insert(Self::bounds(b), self.items.len());
        self.items.push((b, wire));
    }

    fn wire(&mut self, net: u32, a: Pt, b: Pt) {
        let bounds = bx_pt([a[0], a[1], a[0], a[1]], b).map(f64::from);
        self.insert([bounds[0] - 0.1, bounds[1] - 0.1, bounds[2] + 0.1, bounds[3] + 0.1], Some(net));
    }

    /// Grid steps along `dir` to clear the ink intersecting this box (zero when free). While
    /// choosing a new slot, foreign wires count too. Each jump passes its blocking neighbour.
    fn advance(&self, b: labels::Bx, dir: Pt, tag: usize, net: Option<u32>) -> i32 {
        let axis = usize::from(dir[1] != 0);
        let mut step = 0;
        self.index.query(Self::bounds(b), |_, &i| {
            let (other, wire) = self.items[i];
            if i != tag && (wire.is_none() || (net.is_some() && wire != net))
                && b[0] < other[2] && other[0] < b[2] && b[1] < other[3] && other[1] < b[3]
            {
                let exit = if dir[axis] > 0 { other[axis + 2] - b[axis] } else { b[axis + 2] - other[axis] };
                step = step.max(exit.ceil() as i32);
            }
            ControlFlow::Continue(())
        });
        step
    }
}

/// Place the finished name flags once all drop markers have their text and all titles and live
/// strips have their boxes. A crowded pin gets a flag at the nearest free end of a straight stub,
/// continuing outward or turning to either side. Only its own net gains a wire. One spatial
/// index on the layout worker; a fixed number of directions per conflict, jumping past each
/// obstacle along that ray. Nothing runs per frame.
fn place_name_flags(sheet: &mut SheetLayout) {
    if !sheet.tags.iter().any(|t| t.kind == TagKind::Label) { return; }
    let mut space = FlagSpace::default();
    for t in &sheet.tags { space.insert(tag_item_box(t), None); }
    for p in sheet.parts.iter().filter(|p| p.placed) { space.insert(p.body.map(f64::from), None); }
    for l in &sheet.labels { space.insert(l.bx, None); }
    let mut indexed_wires = false;
    for (i, t) in sheet.tags.iter_mut().enumerate() {
        if t.kind != TagKind::Label || space.advance(space.items[i].0, t.dir, i, None) == 0 { continue; }
        let Some(net) = t.net else { continue };
        if !indexed_wires {
            let runs = sheet.wires.iter().map(|r| (r.net, r)).chain(sheet.buses.iter().map(|r| (BUS + r.net, r)));
            for (wire_net, r) in runs {
                for s in r.pts.windows(2) { space.wire(wire_net, s[0], s[1]); }
            }
            for entry in &sheet.entries { space.wire(entry.net, entry.at, entry.to); }
            indexed_wires = true;
        }
        let side = [-t.dir[1], t.dir[0]];
        let directions = [t.dir, side, scale(side, -1)];
        let mut best = None;
        for facing in directions {
            let at_pin = tag_item_box_of(t.kind, t.at, facing, &t.text, t.band, t.side);
            for along in directions {
                if facing == scale(along, -1) { continue; }
                let mut length = 0;
                loop {
                    if matches!(best, Some(((false, nearest), _, _)) if length >= nearest) { break; }
                    let delta = scale(along, length);
                    let [dx, dy] = delta.map(f64::from);
                    let b = [at_pin[0] + dx, at_pin[1] + dy, at_pin[2] + dx, at_pin[3] + dy];
                    let step = space.advance(b, along, i, Some(net));
                    if step > 0 {
                        length += step;
                        continue;
                    }
                    let end = add(t.at, delta);
                    let stub = bx_pt([end[0], end[1], end[0], end[1]], t.at);
                    let crossed = length > 0 && space.advance(stub.map(f64::from), along, i, Some(net)) > 0;
                    let cost = (crossed, length);
                    if best.is_none_or(|(prior, _, _)| cost < prior) { best = Some((cost, end, facing)); }
                    break;
                }
            }
        }
        let (_, end, facing) = best.expect("a name flag has an outward direction");
        if end != t.at {
            sheet.wires.push(Run { net, pts: vec![t.at, end] });
            space.wire(net, t.at, end);
        }
        space.index.remove(FlagSpace::bounds(space.items[i].0), &i);
        t.at = end;
        t.dir = facing;
        space.items[i].0 = tag_item_box(t);
        space.index.insert(FlagSpace::bounds(space.items[i].0), i);
        sheet.bounds = bx_union(sheet.bounds, tag_box(t));
    }
}

/// The distance from an axis-aligned wire segment to an item box, including its edges.
pub(crate) fn segment_box_distance(a: Pt, b: Pt, bx: [f64; 4]) -> f64 {
    let segment = [a[0].min(b[0]), a[1].min(b[1]), a[0].max(b[0]), a[1].max(b[1])];
    item_box_distance(segment.map(f64::from), bx)
}

/// Keep vertical supply names clear of the finished layout, preferring centre, then left, then
/// right. Runs once per layout on the worker: tags times (wires + bodies + labels + tags).
pub fn settle_tag_names(tags: &mut [Tag], wires: &[Run], bodies: &[Bx], labels: &[[f64; 4]]) {
    let mut boxes: Vec<_> = tags.iter().map(tag_item_box).collect();
    for i in 0..tags.len() {
        let t = &tags[i];
        if t.kind != TagKind::Power || t.text.is_empty() || !matches!(t.dir, [0, -1] | [0, 1]) || t.band.is_some() {
            continue;
        }
        let at = t.at;
        let mut chosen = 0;
        for side in [0, -1, 1] {
            tags[i].side = side;
            let b = tag_item_box(&tags[i]);
            if wires.iter().all(|run| run.pts.windows(2).all(|w| {
                w[0] == at || w[1] == at || segment_box_distance(w[0], w[1], b) >= 0.1
            })) && bodies.iter().all(|body| item_box_distance(b, body.map(f64::from)) >= 0.1)
                && labels.iter().all(|&label| item_box_distance(b, label) >= 0.1)
                && boxes.iter().enumerate().all(|(j, &other)| i == j || item_box_distance(b, other) >= 0.1)
            {
                chosen = side;
                break;
            }
        }
        tags[i].side = chosen;
        boxes[i] = tag_item_box(&tags[i]);
    }
}

// ---------------------------------------------------------------------------------------------
// Pieces: a laid-out part of the sheet in its own frame.

/// A routable terminal of a piece: a pin end (or a frame port) on a signal net or a bus.
#[derive(Clone, Copy, Debug, PartialEq)]
struct PPort {
    at: Pt,
    side: Side,
    /// A net, or `BUS + bus` for a bus port.
    net: u32,
    kind: PortKind,
}

const BUS: u32 = 1 << 30;

#[derive(Clone, Debug, Default)]
struct Piece {
    parts: Vec<(u32, PartPlace)>,
    frames: Vec<(u32, Frame)>,
    wires: Vec<Run>,
    buses: Vec<Run>,
    entries: Vec<Entry>,
    tags: Vec<Tag>,
    junctions: Vec<(u32, Pt)>,
    labels: Vec<Label>,
    bx: Bx,
    ports: Vec<PPort>,
    /// Parts and blocks in pinned columns that gave way (`SheetLayout::yielded`, `yielded_blocks`).
    yielded: Vec<u32>,
    yielded_blocks: Vec<u32>,
    /// It or a level inside it packed loose items in rows (`SheetLayout::packed`).
    packed: bool,
    /// Nets a level hands to the sheet router instead of its channels (`LevelRoute`).
    routes: Vec<LevelRoute>,
}

/// A net one level routes on the sheet once everything is placed (fable-answers A7 c, 2a): one
/// with a terminal off its item's outer face (an array member's port, an anchor's pin behind a
/// molecule hung on that side), whose channel run would pass through the item to its edge, or one
/// whose terminals fall in different channels, each of which would go on by name.
#[derive(Clone, Debug, Default)]
struct LevelRoute {
    level: u32,
    net: u32,
    /// Its terminals split across channels: a wire longer than `FREE_REACH` reads worse than a name.
    split: bool,
    /// Pin ends, outward directions, and whether each is the level's own port.
    terms: Vec<(Pt, [i32; 2], bool)>,
}

impl Piece {
    fn new() -> Self {
        Self { bx: EMPTY, ..Default::default() }
    }

    fn shift(&mut self, d: Pt) {
        for (_, p) in &mut self.parts {
            p.at = add(p.at, d);
            p.body = bx_add(p.body, d);
        }
        for (_, f) in &mut self.frames {
            f.rect = bx_add(f.rect, d);
            f.origin = add(f.origin, d);
            for port in &mut f.ports {
                port.at = add(port.at, d);
            }
        }
        for r in self.wires.iter_mut().chain(self.buses.iter_mut()) {
            for p in &mut r.pts {
                *p = add(*p, d);
            }
        }
        for e in &mut self.entries {
            e.at = add(e.at, d);
            e.to = add(e.to, d);
        }
        for t in &mut self.tags {
            t.at = add(t.at, d);
        }
        for (_, j) in &mut self.junctions {
            *j = add(*j, d);
        }
        for l in &mut self.labels {
            let (dx, dy) = (d[0] as f64, d[1] as f64);
            l.bx = [l.bx[0] + dx, l.bx[1] + dy, l.bx[2] + dx, l.bx[3] + dy];
            if let Some(ld) = &mut l.leader {
                for p in ld.iter_mut() {
                    *p = [p[0] + dx, p[1] + dy];
                }
            }
        }
        self.bx = bx_add(self.bx, d);
        for p in &mut self.ports {
            p.at = add(p.at, d);
        }
        for r in &mut self.routes {
            for t in &mut r.terms {
                t.0 = add(t.0, d);
            }
        }
    }

    fn absorb(&mut self, mut o: Piece) {
        self.parts.append(&mut o.parts);
        self.frames.append(&mut o.frames);
        self.wires.append(&mut o.wires);
        self.buses.append(&mut o.buses);
        self.entries.append(&mut o.entries);
        self.tags.append(&mut o.tags);
        self.junctions.append(&mut o.junctions);
        self.labels.append(&mut o.labels);
        self.yielded.append(&mut o.yielded);
        self.yielded_blocks.append(&mut o.yielded_blocks);
        self.routes.append(&mut o.routes);
        self.packed |= o.packed;
        self.bx = bx_union(self.bx, o.bx);
    }

    fn wire(&mut self, net: u32, pts: Vec<Pt>) {
        for &p in &pts {
            self.bx = bx_pt(self.bx, p);
        }
        if net >= BUS {
            self.buses.push(Run { net: net - BUS, pts });
        } else {
            self.wires.push(Run { net, pts });
        }
    }

    fn tag(&mut self, kind: TagKind, net: Option<u32>, at: Pt, dir: [i32; 2], text: String) {
        self.tag_band(kind, net, at, dir, text, None);
    }

    fn tag_band(&mut self, kind: TagKind, net: Option<u32>, at: Pt, dir: [i32; 2], text: String, band: Option<[i32; 2]>) {
        let t = Tag { net, at, dir, kind, text, band, side: 0 };
        self.bx = bx_union(self.bx, tag_box(&t));
        self.tags.push(t);
    }

    /// What this piece occupies, for keeping later items clear of it: part bodies, tag boxes and
    /// wire strokes (as zero-width boxes).
    fn footprint(&self) -> Vec<Bx> {
        let mut out: Vec<Bx> = self.parts.iter().map(|(_, p)| p.body).collect();
        out.extend(self.tags.iter().map(tag_box));
        for r in &self.wires {
            for w in r.pts.windows(2) {
                out.push([w[0][0].min(w[1][0]), w[0][1].min(w[1][1]), w[0][0].max(w[1][0]), w[0][1].max(w[1][1])]);
            }
        }
        out
    }
}

// ---------------------------------------------------------------------------------------------
// The context shared by every block.

struct Ctx<'a> {
    g: &'a LayoutGraph,
    o: &'a Opts,
    port_nets: Vec<Vec<Option<u32>>>,
    /// Per net: bound to some block's port.
    bound: Vec<bool>,
    /// Signal nets whose only connection is between a pin and the flags hanging on it.
    flag_ends: Vec<bool>,
    mols: Vec<Molecule>,
    /// Per node: the molecule it belongs to.
    mol_of: Vec<Option<usize>>,
    /// Per node: the molecules hung on it.
    hung: Vec<Vec<usize>>,
    children: Vec<Vec<u32>>,
    parts_of: Vec<Vec<u32>>,
    /// Ports whose net goes on by name at the parent level, as (block, net): `named_ports`.
    named: std::collections::HashSet<(u32, u32)>,
}

fn rail_text(g: &LayoutGraph, net: u32) -> String {
    short_name(&g.nets[net as usize].name)
}

/// A local net's name without its block path (`mcu.SWDIO` -> `SWDIO`).
pub fn short_name(name: &str) -> String {
    match name.rfind('.') {
        Some(i) if i + 1 < name.len() => name[i + 1..].to_string(),
        _ => name.to_string(),
    }
}

/// `piece` with part `n` marked as turning on its own (`PartPlace::turn_step`).
fn own_turn(mut piece: Piece, n: u32) -> Piece {
    if let Some((_, place)) = piece.parts.iter_mut().find(|(k, _)| *k == n) {
        place.turn_step = 1;
    }
    piece
}

fn small_glyph(g: &LayoutGraph, n: u32) -> bool {
    let node = &g.nodes[n as usize];
    node.role.is_small()
        && node.ports.len() == 2
        && node.ports[0].at == [-2, 0]
        && node.ports[1].at == [2, 0]
}

/// The turn that puts port `p` of a two-pin part (pins at (-2,0) and (2,0)) at the end pointing
/// along `dir`.
fn turn_for(p: usize, dir: [i32; 2]) -> u8 {
    // Port 0 sits at -x, port 1 at +x at turn 0.
    let base = if p == 0 { [-1, 0] } else { [1, 0] };
    (0..4u8).find(|&t| rot(base, t, false) == dir).unwrap_or(0)
}

impl<'a> Ctx<'a> {
    /// Whether a signal pin on `net` continues somewhere: the net has another pin or is bound to
    /// a block's port, and is not only a pin and its flags. Only then does its end become a port
    /// the router wires or labels; a pin alone on its net is open and gets no tag.
    fn continues(&self, net: u32) -> bool {
        !self.flag_ends[net as usize] && (self.g.nets[net as usize].pins.len() >= 2 || self.bound[net as usize])
    }

    /// A part on its own at `turn`/`mirror`: its symbol, tags on rail and not-connected pins, its
    /// routable ports. `skip` names ports the caller connects itself.
    fn part(&self, n: u32, turn: u8, mirror: bool, skip: &[usize]) -> Piece {
        self.part_rails(n, turn, mirror, skip, Rails::Auto)
    }

    /// A part with its tags. Rails on side pins take a stub and turn, supply up and ground down;
    /// `rails` says whether a turned tag may be full size (`Auto`: where no other connected pin of
    /// that side lies beyond it) or must fit its row band (`Band`).
    fn part_rails(&self, n: u32, turn: u8, mirror: bool, skip: &[usize], rails: Rails) -> Piece {
        let g = self.g;
        let node = &g.nodes[n as usize];
        let mut pc = Piece::new();
        let body = bx_rot(node.body, turn, mirror);
        pc.bx = body;
        pc.parts.push((
            n,
            PartPlace {
                at: [0, 0], turn, mirror, glyph: node.role == Role::Flag || small_glyph(g, n), body, block: node.block, placed: true,
                quarter: node.quarter, turn_step: 0,
            },
        ));
        // Connected pins per side (after the pose), in order along the side.
        let mut connected: Vec<(Side, Pt, usize)> = node
            .ports
            .iter()
            .enumerate()
            .filter(|&(pi, port)| !port.nc && port.kind != PortKind::NoConnect && self.port_nets[n as usize][pi].is_some())
            .map(|(pi, port)| (rot_side(port.side, turn, mirror), rot(port.at, turn, mirror), pi))
            .collect();
        let along = |side: Side, p: Pt| if matches!(side, Side::Left | Side::Right) { p[1] } else { p[0] };
        connected.sort_by_key(|&(side, p, pi)| (side as u8, along(side, p), pi));
        let mut rail_pins: Vec<usize> = Vec::new();
        for (pi, port) in node.ports.iter().enumerate() {
            if skip.contains(&pi) {
                continue;
            }
            let at = rot(port.at, turn, mirror);
            let side = rot_side(port.side, turn, mirror);
            let dir = side_dir(side);
            if port.nc || port.kind == PortKind::NoConnect {
                pc.tag(TagKind::NoConnect, None, at, dir, String::new());
                continue;
            }
            let Some(net) = self.port_nets[n as usize][pi] else { continue };
            let gn = &g.nets[net as usize];
            match gn.kind {
                NetKind::Power | NetKind::Ground => rail_pins.push(pi),
                NetKind::Signal => {
                    if gn.style == Some(NetStyle::Label) {
                        pc.tag(TagKind::Label, Some(net), at, dir, short_name(&gn.name));
                    } else if self.continues(net) {
                        pc.ports.push(PPort { at, side, net, kind: port.kind });
                    }
                }
            }
        }
        // Rails in runs: neighbouring connected pins of one side on one rail share one tag, their
        // stubs joined by a wire, as people draw them.
        let mut runs: Vec<Vec<(Side, Pt, usize)>> = Vec::new();
        for &(side, at, pi) in &connected {
            if !rail_pins.contains(&pi) {
                runs.push(Vec::new());
                continue;
            }
            let net = self.port_nets[n as usize][pi];
            match runs.last_mut() {
                Some(run) if run.last().is_some_and(|&(s, _, q)| s == side && self.port_nets[n as usize][q] == net) => {
                    run.push((side, at, pi))
                }
                _ => runs.push(vec![(side, at, pi)]),
            }
        }
        let mut rail_boxes: Vec<Bx> = Vec::new();
        for run in runs.iter().filter(|r| !r.is_empty()) {
            let (side, _, pi0) = run[0];
            let Some(net) = self.port_nets[n as usize][pi0] else { continue };
            let power = g.nets[net as usize].kind == NetKind::Power;
            let (kind, text) = if power { (TagKind::Power, rail_text(g, net)) } else { (TagKind::Ground, String::new()) };
            let dir = side_dir(side);
            if matches!(side, Side::Left | Side::Right) {
                // A side pin's rail takes a stub and turns: supply up from the run's top, ground
                // down from its bottom; full size where no connected pin of the side lies beyond,
                // else fitted to the band.
                let up = if power { [0, 1] } else { [0, -1] };
                let (lo, hi) = (run[0].1[1], run[run.len() - 1].1[1]);
                let tip_y = if power { hi } else { lo };
                let beyond = connected.iter().any(|&(s, p, _)| s == side && if power { p[1] > hi } else { p[1] < lo });
                let band = (rails == Rails::Band || beyond).then_some(dir);
                let x0 = run[0].1[0];
                let mut len = 2;
                let tip = |len: i32| [x0 + dir[0] * len, tip_y];
                while len < 16 && rail_boxes.iter().any(|&r| bx_hit(r, tag_box_of(kind, tip(len), up, &text, band, 0))) {
                    len += 1;
                }
                let x = x0 + dir[0] * len;
                for &(_, at, _) in run {
                    pc.wire(net, vec![at, [x, at[1]]]);
                    if run.len() > 1 && ((at[1] != lo && at[1] != hi) || at[1] == tip_y) {
                        pc.junctions.push((net, [x, at[1]]));
                    }
                }
                if run.len() > 1 {
                    pc.wire(net, vec![[x, lo], [x, hi]]);
                }
                rail_boxes.push(tag_box_of(kind, tip(len), up, &text, band, 0));
                pc.tag_band(kind, Some(net), tip(len), up, text, band);
            } else if run.len() == 1 {
                // A top or bottom pin: the tag continues the pin.
                pc.tag(kind, Some(net), run[0].1, dir, text);
            } else {
                // Neighbouring top or bottom pins: one step out, joined, one tag from the middle.
                let ends: Vec<Pt> = run.iter().map(|&(_, at, _)| add(at, dir)).collect();
                let mid = (run.len() - 1) / 2;
                for (i, &(_, at, _)) in run.iter().enumerate() {
                    pc.wire(net, vec![at, ends[i]]);
                    if i == mid || (i > 0 && i + 1 < run.len()) {
                        pc.junctions.push((net, ends[i]));
                    }
                }
                pc.wire(net, vec![ends[0], ends[run.len() - 1]]);
                pc.tag(kind, Some(net), ends[mid], dir, text);
            }
        }
        pc
    }

    /// Lay out a chain in O(parts), along its head's hinted turn or upright with supply above ground.
    fn chain(&self, m: &Molecule) -> Piece {
        let g = self.g;
        // The free ends: the first part's port not shared with the second, the last's likewise.
        let shared = |a: u32, b: u32| -> Option<usize> {
            (0..2).find(|&p| {
                let net = self.port_nets[a as usize][p];
                net.is_some() && (0..2).any(|q| self.port_nets[b as usize][q] == net)
            })
        };
        let k = m.parts.len();
        let first_link = shared(m.parts[0], m.parts[1]).unwrap_or(1);
        let first_free = 1 - first_link;
        let net_kind = |n: u32, p: usize| self.port_nets[n as usize][p].map(|x| g.nets[x as usize].kind);
        let last_link = shared(m.parts[k - 1], m.parts[k - 2]).unwrap_or(0);
        let last_free = 1 - last_link;
        let hint = g.nodes[m.parts[0] as usize].turn;
        // The head's hinted pose, mirror included: every member mirrors with it across the
        // chain's axis, so the symbols flip and no pin moves.
        let mirror = hint.is_some() && g.nodes[m.parts[0] as usize].mirror;
        let flip = hint.is_none() && (net_kind(m.parts[k - 1], last_free) == Some(NetKind::Power)
            || net_kind(m.parts[0], first_free) == Some(NetKind::Ground));
        let node_at = |i: usize| m.parts[if flip { k - 1 - i } else { i }];
        // In head-to-tail order the head's free pin starts at the top of the upright stack: the
        // chain runs the way that pin points at the head's hinted pose.
        let up = hint.map_or([0, 1], |turn| rot(if first_free == 0 { [-1, 0] } else { [1, 0] }, turn, mirror));
        let down = scale(up, -1);
        let start_port = if flip { last_free } else { first_free };
        let mut pc = Piece::new();
        let mut prev_bottom: Option<Pt> = None;
        for i in 0..k {
            let n = node_at(i);
            let up_port = if i == 0 { start_port } else { shared(n, node_at(i - 1)).unwrap_or(0) };
            // Mirrored first, a two-pin part's ports trade ends.
            let turn = turn_for(if mirror { 1 - up_port } else { up_port }, up);
            let mut part = self.part(n, turn, mirror, &[up_port, 1 - up_port]);
            let center = scale(down, 2 + 5 * i as i32);
            part.shift(center);
            let top = add(center, scale(up, 2));
            let bottom = add(center, scale(down, 2));
            if let Some(pb) = prev_bottom {
                let net = self.port_nets[n as usize][up_port].unwrap_or(0);
                pc.wire(net, vec![pb, top]);
            }
            pc.absorb(part);
            for (outer, p, at, dir) in [(i == 0, up_port, top, up), (i + 1 == k, 1 - up_port, bottom, down)] {
                if !outer { continue; }
                let Some(net) = self.port_nets[n as usize][p] else { continue };
                match g.nets[net as usize].kind {
                    NetKind::Power => pc.tag(TagKind::Power, Some(net), at, dir, rail_text(g, net)),
                    NetKind::Ground => pc.tag(TagKind::Ground, Some(net), at, dir, String::new()),
                    NetKind::Signal if !self.continues(net) => {}
                    NetKind::Signal => pc.ports.push(PPort { at, side: dir_side(dir), net, kind: g.nodes[n as usize].ports[p].kind }),
                }
            }
            prev_bottom = Some(bottom);
        }
        pc
    }

    /// Lay out a row in O(caps and label text), supply up unless hinted. Leave at least four
    /// steps between centres and room for each cap's body, rail tags and label.
    fn decoupling_row(&self, mols: &[usize]) -> Piece {
        let gap = LabelOpts::default().gap;
        let mut pc = Piece::new();
        let mut next: Option<(i32, i32)> = None;
        for &mi in mols {
            let n = self.mols[mi].parts[0];
            let node = &self.g.nodes[n as usize];
            let (turn, mirror) = match node.turn {
                Some(turn) => (turn & 3, node.mirror),
                None => {
                    let up = self.port_nets[n as usize].iter()
                        .position(|net| net.is_some_and(|x| self.g.nets[x as usize].kind == NetKind::Power)).unwrap_or(0);
                    (turn_for(up, [0, 1]), false)
                }
            };
            let mut part = self.part(n, turn, mirror, &[]);
            if let Some((at, left)) = next {
                part.shift([at.max(left - part.bx[0]), 0]);
            }
            let label = node.value.as_deref().map_or(0.0, |v| text_w(v, TEXT_VALUE)).max(text_w(&node.name, TEXT_REFDES));
            let (at, body) = part.parts.first().map_or(([0, 0], part.bx), |(_, p)| (p.at, p.body));
            let label_end = (body[2] as f64 + 2.0 * gap + label).ceil() as i32;
            next = Some((at[0] + 4, label_end.max(part.bx[2])));
            pc.absorb(part);
        }
        pc
    }

    /// A crystal parallel to its host side, with load capacitors reaching outward and down.
    fn crystal(&self, m: &Molecule, turn: u8, mirror: bool, c: i32, rails: Rails) -> Piece {
        let g = self.g;
        let n = m.parts[0];
        let (h, hp) = m.host.expect("a crystal molecule has a host");
        let host = &g.nodes[h as usize];
        let nets = [0, 1].map(|p| self.port_nets[n as usize][p].expect("a crystal pin has a net"));
        let at = nets.map(|net| {
            let p = self.port_nets[h as usize].iter().position(|&other| other == Some(net)).expect("a crystal net has a host pin");
            rot(host.ports[p].at, turn, mirror)
        });
        let dir = side_dir(rot_side(host.ports[hp as usize].side, turn, mirror));
        let normal = if dir[0] != 0 { 0 } else { 1 };
        let along = 1 - normal;
        let mut end_dir = [0, 0];
        end_dir[along] = if at[0][along] <= at[1][along] { -1 } else { 1 };
        let pose = turn_for(0, end_dir);
        let mut center = [(at[0][0] + at[1][0]) / 2, (at[0][1] + at[1][1]) / 2];
        center[normal] = if dir[normal] > 0 { at[0][normal].max(at[1][normal]) } else { at[0][normal].min(at[1][normal]) };
        center = add(center, scale(dir, c));
        let ends = [0, 1].map(|p| add(center, rot(g.nodes[n as usize].ports[p].at, pose, false)));
        let mut pc = self.part(n, pose, false, &[0, 1]);
        pc.shift(center);
        for p in 0..2 {
            // Turn beside the body, then reach its end: adjacent chip pins can be closer
            // together than the crystal's pin span, so turning on its axis would cross it.
            let elbow = add(ends[p], scale(dir, -2));
            let mut out = at[p];
            out[normal] = elbow[normal];
            pc.wire(nets[p], vec![at[p], out, elbow, ends[p]]);
        }
        // Lower ends first: a higher capacitor can then reach past the lower one's ground
        // tag. The molecule's member order itself remains crystal-pin order.
        let mut caps: Vec<(u32, usize, usize)> = m.parts.iter().skip(1).map(|&cap| {
            let near = (0..2).find(|&p| self.port_nets[cap as usize][p].is_some_and(|net| nets.contains(&net)))
                .expect("a load capacitor joins a crystal net");
            let end = usize::from(self.port_nets[cap as usize][near] == Some(nets[1]));
            (cap, near, end)
        }).collect();
        caps.sort_by_key(|&(cap, _, end)| (ends[end][1], g.nodes[cap as usize].order, cap));
        for (cap, near, end) in caps {
            let pose = turn_for(near, [0, 1]);
            let pin = rot(g.nodes[cap as usize].ports[near].at, pose, false);
            let out = if dir[0] != 0 { dir[0] } else if ends[end][0] < center[0] { -1 } else { 1 };
            let occupied = pc.footprint();
            let mut stub = 3;
            let mut part = loop {
                let top = add(ends[end], [out * stub, 0]);
                let mut part = self.part_rails(cap, pose, false, &[near], rails);
                part.shift([top[0] - pin[0], top[1] - pin[1]]);
                if !part.footprint().iter().any(|&f| occupied.iter().any(|&t| bx_hit(f, t))) {
                    break part;
                }
                stub += 1;
            };
            // Loads on the same end share their horizontal stub.
            part.wire(nets[end], vec![ends[end], add(ends[end], [out * stub, 0])]);
            if !pc.junctions.contains(&(nets[end], ends[end])) {
                pc.junctions.push((nets[end], ends[end]));
            }
            pc.absorb(part);
        }
        pc
    }

    /// An anchor with decoupling below, pulls and series parts in line, crystals beside it.
    fn anchor(&self, n: u32, turn: u8, mirror: bool) -> Piece {
        let g = self.g;
        let mut skip = Vec::new();
        let mut pulls = Vec::new();
        let mut crystals = Vec::new();
        let mut decs = Vec::new();
        for &mi in &self.hung[n as usize] {
            let m = &self.mols[mi];
            match m.kind {
                MolKind::Pull { .. } | MolKind::Series => {
                    if let Some((_, hp)) = m.host {
                        skip.push(hp as usize);
                        pulls.push(mi);
                    }
                }
                MolKind::Crystal => {
                    let nets = &self.port_nets[m.parts[0] as usize];
                    skip.extend(self.port_nets[n as usize].iter().enumerate().filter_map(|(p, net)| {
                        (net.is_some() && nets.contains(net)).then_some(p)
                    }));
                    crystals.push(mi);
                }
                MolKind::Decoupling => decs.push(mi),
                MolKind::Chain | MolKind::Flag => {}
            }
        }
        let mut pc = self.part(n, turn, mirror, &skip);
        let node = &g.nodes[n as usize];
        // Molecules keep clear of the host, its rail stubs and tags, the wires its routed pins send
        // outward to the channels, and the molecules placed before them. A pull whose rail turns down
        // goes nearest when its pin is lowest (the higher ones reach past it), one turning up when
        // its pin is highest.
        let mut taken: Vec<Bx> = pc.footprint();
        for p in &pc.ports {
            let far = add(p.at, scale(side_dir(p.side), 1 << 16));
            taken.push([p.at[0].min(far[0]), p.at[1].min(far[1]), p.at[0].max(far[0]), p.at[1].max(far[1])]);
        }
        let pull_key = |mi: usize| -> (bool, i32) {
            let m = &self.mols[mi];
            let up = matches!(m.kind, MolKind::Pull { up: true });
            let hp = m.host.map_or(0, |(_, hp)| hp as usize);
            let y = rot(node.ports[hp].at, turn, mirror)[1];
            (up, if up { -y } else { y })
        };
        pulls.sort_by_key(|&mi| (pull_key(mi), mi));
        for mi in pulls {
            let m = &self.mols[mi];
            let (_, hp) = m.host.unwrap_or((n, 0));
            let port = &node.ports[hp as usize];
            let at = rot(port.at, turn, mirror);
            let dir = side_dir(rot_side(port.side, turn, mirror));
            let p = m.parts[0];
            // The molecule's pin on the host's net faces the host.
            let net = self.port_nets[n as usize][hp as usize];
            let near = (0..2).find(|&q| self.port_nets[p as usize][q] == net).unwrap_or(0);
            let back = [-dir[0], -dir[1]];
            // Half the part's length, pin end to centre.
            let half = g.nodes[p as usize].ports.iter().map(|q| q.at[0].abs().max(q.at[1].abs())).max().unwrap_or(2);
            let build = |c: i32, rails: Rails| {
                let pose = turn_for(near, back);
                let mut part = if m.kind == MolKind::Series {
                    let mut part = self.part_rails(p, pose, false, &[0, 1], rails);
                    let far = 1 - near;
                    let far_net = self.port_nets[p as usize][far].expect("a series part has a far net");
                    if self.continues(far_net) {
                        part.ports.push(PPort {
                            at: rot(g.nodes[p as usize].ports[far].at, pose, false),
                            side: dir_side(dir),
                            net: far_net,
                            kind: g.nodes[p as usize].ports[far].kind,
                        });
                    }
                    part
                } else {
                    self.part_rails(p, pose, false, &[near], rails)
                };
                part.shift(add(at, scale(dir, c)));
                part.wire(net.unwrap_or(0), vec![at, add(at, scale(dir, c - half))]);
                part
            };
            let clear = |part: &Piece| !part.footprint().iter().any(|&f| taken.iter().any(|&t| bx_hit(f, t)));
            // Its near pin one step out from the host pin, or further while it would clash; its rail
            // full size if it can be, else in its band; else (never seen) the first spot, logged.
            let mut part = [Rails::Auto, Rails::Band]
                .into_iter()
                .flat_map(|rails| (half + 1..half + 25).map(move |c| (c, rails)))
                .map(|(c, rails)| build(c, rails))
                .find(|part| clear(part))
                .unwrap_or_else(|| {
                    eprintln!("fab.explore: {:?} {} has no clear spot on {}; drawn in its band at the pin", m.kind, g.nodes[p as usize].name, node.name);
                    build(half + 1, Rails::Band)
                });
            taken.extend(part.footprint());
            // absorb deliberately leaves ports behind; the series far end must join the
            // host's ports so the block router continues the signal from there.
            for port in &part.ports {
                let far = add(port.at, scale(side_dir(port.side), 1 << 16));
                taken.push([port.at[0].min(far[0]), port.at[1].min(far[1]), port.at[0].max(far[0]), port.at[1].max(far[1])]);
            }
            pc.ports.append(&mut part.ports);
            pc.absorb(part);
        }
        for mi in crystals {
            let m = &self.mols[mi];
            // Dense chip rows can leave no corridor between the reserved routing rays. In
            // that case keep bodies and tags clear, including room for a signal end that
            // the router may continue by name. Never fall back onto an occupied body.
            let mut solid_taken: Vec<Bx> = pc.parts.iter().map(|(_, p)| p.body).chain(pc.tags.iter().map(tag_box)).collect();
            solid_taken.extend(pc.ports.iter().filter(|p| p.net < BUS).map(|p| {
                tag_box_of(TagKind::Label, p.at, side_dir(p.side), &short_name(&g.nets[p.net as usize].name), None, 0)
            }));
            let part = [Rails::Auto, Rails::Band]
                .into_iter()
                .flat_map(|rails| (3..27).map(move |c| (c, rails)))
                .map(|(c, rails)| self.crystal(m, turn, mirror, c, rails))
                .find(|part| !part.footprint().iter().any(|&f| taken.iter().chain(solid_taken.iter()).any(|&t| bx_hit(f, t))))
                .unwrap_or_else(|| {
                    eprintln!("fab.explore: crystal {} crosses reserved routing rays on {}; keeping bodies and tags clear", g.nodes[m.parts[0] as usize].name, node.name);
                    let mut c = 3;
                    loop {
                        let part = self.crystal(m, turn, mirror, c, Rails::Band);
                        if !part.parts.iter().map(|(_, p)| p.body).chain(part.tags.iter().map(tag_box))
                            .any(|f| solid_taken.iter().any(|&t| bx_hit(f, t)))
                        {
                            break part;
                        }
                        c += 1;
                    }
                });
            taken.extend(part.footprint());
            pc.absorb(part);
        }
        if !decs.is_empty() {
            let mut row = self.decoupling_row(&decs);
            let host_box = pc.bx;
            // Under the host's box, its left edge with the host body's.
            let dy = host_box[1] - 1 - row.bx[3];
            let dx = pc.parts[0].1.body[0] + 1 - row.bx[0];
            row.shift([dx, dy]);
            pc.absorb(row);
        }
        pc
    }

    /// Add markers after their hosts' poses, rail tags and molecule wiring are known. Only a
    /// flagged pin searches its item's geometry for a free spot beside the connection.
    fn hang_flags(&self, pc: &mut Piece) {
        for i in 0..pc.parts.len() {
            let (n, host) = pc.parts[i];
            for &mi in &self.hung[n as usize] {
                let m = &self.mols[mi];
                if m.kind != MolKind::Flag { continue }
                let (_, pin) = m.host.expect("a hanging flag has a host pin");
                let port = &self.g.nodes[n as usize].ports[pin as usize];
                let net = self.port_nets[n as usize][pin as usize].expect("a hanging flag has a net");
                let at = add(host.at, rot(port.at, host.turn, host.mirror));
                let dir = side_dir(rot_side(port.side, host.turn, host.mirror));
                self.flag_on(pc, m.parts[0], net, at, dir, host.block);
            }
        }
    }

    /// Attach one flag to its pin, branching beside any rail or continuing signal at that pin.
    fn flag_on(&self, pc: &mut Piece, flag: u32, net: u32, at: Pt, dir: Pt, block: u32) {
        let pose = (0..4).find(|&t| rot([0, 1], t, false) == dir).expect("a pin faces a sheet axis");
        let mut flag = self.part(flag, pose, false, &[0]);
        flag.parts[0].1.block = block;
        // Branch one step along an existing wire. A tag or routed port directly on the pin gets a
        // one-step stub, so its branch clears the neighbouring pins on the same body edge.
        let out = add(at, dir);
        let has_stub = pc.wires.iter().filter(|w| w.net == net).flat_map(|w| w.pts.windows(2)).any(|s| {
            crate::wiretree::on_stroke(at, s[0], s[1]) && crate::wiretree::on_stroke(out, s[0], s[1])
        });
        let mut tap = at;
        if has_stub {
            tap = out;
        } else {
            if self.flag_ends[net as usize] && self.g.nets[net as usize].pins.len() > 2 { tap = out; }
            for t in pc.tags.iter_mut().filter(|t| t.net == Some(net) && t.at == at) {
                t.at = out;
                pc.bx = bx_union(pc.bx, tag_box(t));
                tap = out;
            }
            for p in pc.ports.iter_mut().filter(|p| p.net == net && p.at == at) {
                p.at = out;
                tap = out;
            }
            if at != tap { pc.wire(net, vec![at, tap]); }
        }
        let mut occupied = pc.footprint();
        // A port the router continues by name needs room for that tag, too.
        for p in &pc.ports {
            let text = short_name(&self.g.nets[p.net as usize].name);
            occupied.push(tag_box_of(TagKind::Label, p.at, side_dir(p.side), &text, None, 0));
        }
        let clear = |p| !occupied.iter().any(|&b| bx_hit(b, bx_add(flag.bx, p)));
        let mut foot = tap;
        if !clear(foot) {
            let side = [dir[1], -dir[0]];
            // Where a side pin's rail turns up or down, the flag can continue straight ahead.
            let turned = pc.tags.iter().any(|t| t.at == tap && t.net == Some(net) && t.dir != dir);
            let directions = [dir, side, scale(side, -1)];
            foot = directions.into_iter().filter(|&d| d != dir || turned).map(|d| {
                let mut gap = 1;
                while !clear(add(tap, scale(d, gap))) { gap += 1; }
                let p = add(tap, scale(d, gap));
                let branch = bx_pt([tap[0], tap[1], tap[0], tap[1]], p);
                let crosses = occupied.iter().any(|&b| bx_hit(b, branch));
                ((crosses, gap), p)
            }).min_by_key(|&(cost, _)| cost).expect("a flag has two sides to branch to").1;
            pc.wire(net, vec![tap, foot]);
        }
        if tap != at && !pc.junctions.contains(&(net, tap)) {
            pc.junctions.push((net, tap));
        }
        flag.shift(foot);
        pc.absorb(flag);
    }

    /// An independently placed part takes its sheet hint's turn and mirror in O(1).
    /// Without a hint, its connections determine its orientation.
    fn orient(&self, n: u32, block: u32) -> (u8, bool) {
        let node = &self.g.nodes[n as usize];
        match node.turn {
            Some(turn) => (turn & 3, node.mirror),
            None => self.auto_orient(n, block),
        }
    }

    /// The layout's own orientation of an anchor part: chips as drawn; connectors mirrored when
    /// their signal pins face left (a connector feeds the flow); two-pin parts lying along the flow
    /// with their signal end towards where its partner faces.
    fn auto_orient(&self, n: u32, block: u32) -> (u8, bool) {
        let g = self.g;
        let node = &g.nodes[n as usize];
        let signal = |p: usize| self.port_nets[n as usize][p].is_some_and(|x| g.nets[x as usize].kind == NetKind::Signal);
        match node.role {
            Role::Connector => {
                let (mut l, mut r) = (0, 0);
                for (p, port) in node.ports.iter().enumerate() {
                    if signal(p) {
                        match port.side {
                            Side::Left => l += 1,
                            Side::Right => r += 1,
                            _ => {}
                        }
                    }
                }
                (0, l > r)
            }
            _ if small_glyph(g, n) => {
                // The signal end faces its partner: a partner pin facing west lies to the right.
                for p in 0..2 {
                    let Some(net) = self.port_nets[n as usize][p] else { continue };
                    if g.nets[net as usize].kind != NetKind::Signal {
                        continue;
                    }
                    let partner = g.unflagged_pins(net).find(|&(o, _)| o != n && g.nodes[o as usize].block == block);
                    let east = match partner {
                        Some((o, op)) => {
                            let (t, m) = if g.nodes[o as usize].role.is_small() { (0, false) } else { self.orient(o, block) };
                            rot_side(g.nodes[o as usize].ports[op as usize].side, t, m) == Side::Left
                        }
                        None => {
                            // A block port: on the left (an input) or the right.
                            let b = &g.blocks[block as usize];
                            let port = b.ports.iter().find(|bp| bp.nets.contains(&net));
                            !matches!(port.and_then(|bp| bp.side), Some(Side::Left))
                        }
                    };
                    return (turn_for(p, if east { [1, 0] } else { [-1, 0] }), false);
                }
                (0, false)
            }
            _ => (0, false),
        }
    }
}

// ---------------------------------------------------------------------------------------------
// One block.

/// An opaque frame is shared by the architecture sheet and an unbuilt block on a schematic.
fn block_box(g: &LayoutGraph, b: u32, bundles: Option<&[bool]>) -> Piece {
    let block = &g.blocks[b as usize];
    let mut lines = vec![block.path.clone()];
    if let Some(title) = &block.title { lines.push(title.clone()); }
    if block.planned { lines.push("planned".into()); }
    if let Some(role) = &block.role { lines.push(role.clone()); }
    let band = 2 * lines.len() as i32;
    let mut width = lines.iter().map(|t| text_w(t, TEXT_TITLE).ceil() as i32 + 2).max().unwrap_or(2);
    let mut edges: [Vec<_>; 3] = Default::default();
    for p in &block.ports {
        let edge = if p.ground { 2 } else if matches!(p.kind, PortKind::PowerOut | PortKind::Out | PortKind::Bidi) { 1 } else { 0 };
        let bus = p.nets.first().and_then(|&n| g.nets[n as usize].bus)
            .filter(|&bus| p.nets.len() > 1 && g.buses[bus as usize].nets == p.nets
                && bundles.is_none_or(|bundle| bundle[bus as usize]));
        if bus.is_none() && p.nets.len() > 1 {
            for (bit, &n) in p.nets.iter().enumerate() {
                edges[edge].push((p, format!("{}[{bit}]", p.name), Some(n)));
            }
        } else {
            edges[edge].push((p, p.name.clone(), bus.map(|id| BUS + id).or_else(|| p.nets.first().copied())));
        }
    }
    for edge in &mut edges[..2] {
        edge.sort_by_key(|(p, _, _)| !matches!(p.kind, PortKind::PowerIn | PortKind::PowerOut));
    }
    let name_width = |name: &str| text_w(name, TEXT_NET).ceil() as i32 + 2;
    let side_width: i32 = edges[..2].iter().map(|ps| ps.iter().map(|(_, name, _)| name_width(name)).max().unwrap_or(0)).sum();
    width = width.max(side_width + 2).max(edges[2].iter().map(|(_, name, _)| name_width(name)).sum());
    let height = band + 2 * edges[0].len().max(edges[1].len()) as i32 + 3;
    let rect = [0, -height, width, 0];
    let mut pc = Piece { bx: rect, ..Piece::default() };
    let mut frame = Frame { rect, title: band, planned: block.planned, role: block.role.clone(), placed: true, ..Frame::default() };
    let label = |pc: &mut Piece, text: &str, role, x: f64, top: f64, h: f64| {
        pc.labels.push(Label { owner: Owner::Block(b), lines: vec![(text.into(), role)],
            bx: [x, top - h, x + text_w(text, h), top], leader: None });
    };
    for (i, text) in lines.iter().enumerate() {
        label(&mut pc, text, TextRole::Title, 1.0, -0.3 - 2.0 * i as f64, TEXT_TITLE);
    }
    let mut ground_x = 0;
    for (edge, ports) in edges.iter().enumerate() {
        for (i, (p, name, net)) in ports.iter().enumerate() {
            let y = -band - 1 - 2 * i as i32;
            let (side, at, x, top) = match edge {
                0 => (Side::Left, [0, y], 1.0, f64::from(y) + 0.4),
                1 => (Side::Right, [width, y], f64::from(width - 1) - text_w(name, TEXT_NET), f64::from(y) + 0.4),
                _ => {
                    let w = name_width(name);
                    ground_x += w;
                    (Side::Bottom, [ground_x - w / 2, -height], f64::from(ground_x - w + 1), f64::from(2 - height))
                }
            };
            frame.ports.push(FramePort { name: name.clone(), at, side, kind: p.kind, ground: p.ground, open: p.open, bus: p.nets.len() > 1 });
            label(&mut pc, name, TextRole::Net, x, top, TEXT_NET);
            if let Some(net) = *net {
                if net >= BUS {
                    pc.ports.push(PPort { at, side, net, kind: p.kind });
                    continue;
                }
                let n = &g.nets[net as usize];
                if p.ground || n.kind == NetKind::Ground || bundles.is_none() && n.kind == NetKind::Power {
                    let dir = side_dir(side);
                    let end = add(at, dir);
                    pc.wire(net, vec![at, end]);
                    pc.tag(if p.ground || n.kind == NetKind::Ground { TagKind::Ground } else { TagKind::Power }, Some(net), end, dir, n.name.clone());
                } else {
                    pc.ports.push(PPort { at, side, net, kind: p.kind });
                }
            }
        }
    }
    pc.frames.push((b, frame));
    pc
}

fn rail_label(net: &crate::graph::GNet) -> String {
    let mut parts = vec![net.name.clone()];
    let Some(r) = &net.rail else { return net.name.clone() };
    let number = |v: f64| format!("{v:.6}").trim_end_matches('0').trim_end_matches('.').to_owned();
    let amps = |v: f64| if v == 0.0 { "0mA".into() } else if v.abs() < 0.001 { format!("{}uA", number(v * 1e6)) }
        else if v.abs() < 1.0 { format!("{}mA", number(v * 1e3)) } else { format!("{}A", number(v)) };
    if r.volts_conflict { parts.push("voltage conflict".into()); }
    else if let Some((lo, hi)) = r.volts {
        parts.push(if lo == hi { format!("{}V", number(lo)) } else { format!("{}–{}V", number(lo), number(hi)) });
    }
    if r.source_conflict { parts.push("multiple sources".into()); }
    else if let Some(cap) = r.budget { parts.push(format!("{}/{}", amps(r.demand), amps(cap))); }
    else if r.has_demand { parts.push(amps(r.demand)); }
    parts.join(" · ")
}

/// Direct children are single placement nodes. Index terminals by net and channel once.
/// A net spanning several channels crosses above the frames on its own row.
fn blocks_layout(g: &LayoutGraph, o: &Opts) -> SheetLayout {
    use std::collections::BTreeMap;
    let mut sheet = SheetLayout {
        parts: vec![PartPlace::default(); g.nodes.len()],
        frames: vec![Frame::default(); g.blocks.len()],
        movers: vec![Mover::Itself; g.nodes.len()],
        block_movable: vec![false; g.blocks.len()],
        ..Default::default()
    };
    let mut boxes = Vec::new();
    let mut graph = LGraph::default();
    let mut nets: BTreeMap<u32, Vec<(u32, u32)>> = BTreeMap::new();
    let children = || g.blocks.iter().filter(|b| b.parent == Some(0));
    // Carry a bus as one wire only when every visible terminal binds the whole bus.
    let mut bundles = vec![true; g.buses.len()];
    for p in children().flat_map(|b| &b.ports) {
        let whole = p.nets.first().and_then(|&net| g.nets[net as usize].bus)
            .filter(|&bus| g.buses[bus as usize].nets == p.nets);
        if whole.is_none() {
            for &n in &p.nets {
                if let Some(bus) = g.nets[n as usize].bus { bundles[bus as usize] = false; }
            }
        }
    }
    for block in children() {
        let piece = block_box(g, block.key, Some(&bundles));
        let b = piece.bx;
        let node = boxes.len() as u32;
        let ports = piece.ports.iter().enumerate().map(|(i, p)| {
            nets.entry(p.net).or_default().push((node, i as u32));
            LPort { side: p.side, offset: b[3] - p.at[1] }
        }).collect();
        graph.nodes.push(LNode { size: [b[2] - b[0], b[3] - b[1] + o.margin], ports, order: block.order, layer: None, at: None });
        boxes.push(piece);
    }
    if boxes.is_empty() { return sheet; }
    for ts in nets.values() {
        let side = |&(n, p): &(u32, u32)| boxes[n as usize].ports[p as usize].side;
        let east = ts.iter().find(|p| side(p) == Side::Right);
        let west = ts.iter().find(|p| side(p) == Side::Left);
        if let (Some(&e), Some(&w)) = (east, west) {
            for &t in ts {
                let (from, to) = if side(&t) == Side::Right { (t, w) } else if t != w { (e, t) } else { continue };
                if from.0 != to.0 { graph.edges.push(LEdge { from, to, weight: 1, drawn: true }); }
            }
        } else if ts.len() > 1 { graph.turns.push(ts.clone()); }
    }
    let placed = layered::place(&graph, &LOpts { layer_gap: o.layer_gap, node_gap: o.margin + 1, per_edge: 1 });
    for (piece, at) in boxes.iter_mut().zip(&placed.pos) {
        piece.shift([at[0] - piece.bx[0], -at[1] - piece.bx[3]]);
    }
    let cols = &placed.columns;
    let channels: Vec<_> = (0..=cols.len()).map(|c| {
        let x0 = if c == 0 { cols[0][0] - o.layer_gap } else { cols[c-1][0] + cols[c-1][1] };
        Channel { x0, x1: cols.get(c).map_or(x0 + o.layer_gap, |col| col[0]) }
    }).collect();
    let mut root = Piece::new();
    let mut top = boxes.iter().map(|p| p.bx[3]).max().unwrap_or(0);
    let mut groups = Vec::new();
    for (net, ts) in nets {
        let mut by_channel: BTreeMap<u32, Vec<Term>> = BTreeMap::new();
        for (n, p) in ts {
            let port = boxes[n as usize].ports[p as usize];
            let east = port.side == Side::Right;
            by_channel.entry(placed.layer[n as usize] + u32::from(east)).or_default().push(Term { at: port.at, face: if east { Face::East } else { Face::West } });
        }
        if by_channel.len() > 1 {
            top += 2;
            let mut previous = None;
            for (&c, terms) in &mut by_channel {
                // The bridge ends on the wall; the first vertical track is one step inside.
                let x = channels[c as usize].x0;
                let at = [x, top];
                terms.push(Term { at, face: Face::East });
                if let Some(last) = previous { root.wire(net, vec![last, at]); }
                previous = Some(at);
            }
        }
        groups.extend(by_channel.into_iter().map(|(channel, terms)| Group { net, channel, terms, weight: 1 }));
    }
    let routed = channels::route(&channels, &groups, &ROpts::default());
    for w in routed.wires { root.wire(w.net, w.pts); }
    root.junctions = routed.junctions.into_iter().filter(|(n, _)| *n < BUS).collect();
    for piece in boxes { root.absorb(piece); }

    let mut anchors: BTreeMap<u32, (i32, Pt, Pt, f64)> = BTreeMap::new();
    let mut obstacles: Vec<_> = root.frames.iter().map(|(_, f)| Obstacle::Box { b: f.rect.map(f64::from), owner: u32::MAX }).collect();
    let runs = root.wires.iter().map(|r| (r, r.net, 0.05))
        .chain(root.buses.iter().map(|r| (r, BUS + r.net, 0.15)));
    for (run, key, half) in runs {
        for seg in run.pts.windows(2) {
            let [a, b] = [seg[0], seg[1]];
            obstacles.push(Obstacle::Seg { a: a.map(f64::from), b: b.map(f64::from), half, owner: u32::MAX });
            if key >= BUS || g.nets[key as usize].kind != NetKind::Ground {
                let length = (a[0] - b[0]).abs() + (a[1] - b[1]).abs();
                let entry = anchors.entry(key).or_insert((0, a, b, half));
                if length > entry.0 { *entry = (length, a, b, half); }
            }
        }
    }
    let mut requests = Vec::new();
    let mut names = Vec::new();
    for (i, (key, (_, a, b, half))) in anchors.into_iter().enumerate() {
        let (net, text) = if key >= BUS {
            let bus = &g.buses[(key - BUS) as usize];
            (bus.nets[0], bus.name.clone())
        } else {
            (key, rail_label(&g.nets[key as usize]))
        };
        let pad = half + 1e-6;
        let horizontal = a[1] == b[1];
        let anchor = if horizontal {
            [a[0].min(b[0]) as f64, a[1] as f64 - pad, a[0].max(b[0]) as f64, a[1] as f64 + pad]
        } else {
            [a[0] as f64 - pad, a[1].min(b[1]) as f64, a[0] as f64 + pad, a[1].max(b[1]) as f64]
        };
        let spots = if horizontal {
            vec![Spot::Above, Spot::Below, Spot::Right, Spot::Left]
        } else {
            vec![Spot::Right, Spot::Left, Spot::Above, Spot::Below]
        };
        requests.push(LabelReq {
            anchor, size: [text_w(&text, TEXT_NET), TEXT_NET],
            spots, priority: 0, owner: i as u32, optional: false,
        });
        names.push((net, text));
    }
    let labels = labels::place_labels(&requests, &obstacles, &LabelOpts::default());
    for (i,(net,text)) in names.into_iter().enumerate() {
        let bx = labels.boxes[i];
        root.bx = bx_union(root.bx, [bx[0].floor() as i32, bx[1].floor() as i32, bx[2].ceil() as i32, bx[3].ceil() as i32]);
        root.labels.push(Label { owner: Owner::Net(net), lines: vec![(text, TextRole::Net)], bx, leader: labels.leader[i] });
    }
    sheet.bounds = root.bx;
    sheet.frames[0] = Frame { placed: true, rect: root.bx, ..Frame::default() };
    for (b,f) in root.frames { sheet.frames[b as usize] = f; }
    sheet.labels = root.labels;
    sheet.wires = root.wires;
    sheet.buses = root.buses;
    sheet.junctions = root.junctions;
    sheet.tags = root.tags;
    sheet
}

/// An item to place in a block: a piece and where it came from.
struct Item {
    piece: Piece,
    /// Pinned to the first or last layer (the block's own ports).
    pin: Option<bool>,
    order: u32,
    /// The child block it is (an array of blocks is none of them): its hint pins the item.
    block: Option<u32>,
}

impl<'a> Ctx<'a> {
    /// The child blocks of `b` as its items place them: arrays of like siblings (same base path
    /// and title) gathered into one, every other child on its own. In child order.
    fn kid_items(&self, b: u32) -> Vec<Vec<u32>> {
        let g = self.g;
        let kids = &self.children[b as usize];
        let mut done = vec![false; kids.len()];
        let mut out = Vec::new();
        for (i, &k) in kids.iter().enumerate() {
            if done[i] {
                continue;
            }
            let members: Vec<usize> = match array_base(&g.blocks[k as usize].path) {
                Some(base) => (i..kids.len())
                    .filter(|&j| {
                        array_base(&g.blocks[kids[j] as usize].path) == Some(base)
                            && g.blocks[kids[j] as usize].title == g.blocks[k as usize].title
                    })
                    .collect(),
                None => vec![i],
            };
            for &j in &members {
                done[j] = true;
            }
            out.push(members.iter().map(|&j| kids[j]).collect());
        }
        out
    }

    fn block(&self, b: u32) -> Piece {
        let g = self.g;
        let o = self.o;
        let blk = &g.blocks[b as usize];
        if b != 0 && blk.planned && self.children[b as usize].is_empty() && self.parts_of[b as usize].is_empty() {
            return block_box(g, b, None);
        }
        let mut items: Vec<Item> = Vec::new();
        // Child blocks, arrays of like siblings first gathered into one item.
        for members in self.kid_items(b) {
            let k = members[0];
            let (piece, block) = if members.len() > 1 {
                (self.array(&members, b), None)
            } else {
                (self.block(k), Some(k))
            };
            items.push(Item { piece, pin: None, order: g.blocks[k as usize].order, block });
        }
        // Own parts: anchors with their molecules, molecules without a host.
        let own_start = items.len();
        let mut row_mols: Vec<usize> = Vec::new();
        for &n in &self.parts_of[b as usize] {
            let order = 1000 + g.nodes[n as usize].order;
            match self.mol_of[n as usize] {
                Some(mi) => {
                    let m = &self.mols[mi];
                    if m.host.is_some() || m.parts[0] != n {
                        continue;
                    }
                    match m.kind {
                        MolKind::Chain => items.push(Item { piece: self.chain(m), pin: None, order, block: None }),
                        MolKind::Decoupling => row_mols.push(mi),
                        MolKind::Pull { .. } | MolKind::Series | MolKind::Crystal | MolKind::Flag => {
                            let (t, mi) = self.orient(n, b);
                            items.push(Item { piece: own_turn(self.part(n, t, mi, &[]), n), pin: None, order, block: None });
                        }
                    }
                }
                None => {
                    let (t, m) = self.orient(n, b);
                    items.push(Item { piece: own_turn(self.anchor(n, t, m), n), pin: None, order, block: None });
                }
            }
        }
        if !row_mols.is_empty() {
            items.push(Item { piece: self.decoupling_row(&row_mols), pin: None, order: 900, block: None });
        }
        // A flag can ride a passive inside any of these molecules, as well as an anchor.
        for item in &mut items[own_start..] {
            self.hang_flags(&mut item.piece);
        }
        // The block's own signal ports: one item per net on the left or right; top and bottom
        // ports are dropped straight onto the frame later; a bus port gathers its members.
        let mut drops: Vec<(String, u32, Side)> = Vec::new();
        let mut bus_ports: Vec<(String, u32, Vec<u32>)> = Vec::new();
        if b != 0 {
            for bp in &blk.ports {
                let signal: Vec<u32> = bp.nets.iter().copied().filter(|&n| g.nets[n as usize].kind == NetKind::Signal).collect();
                if signal.is_empty() {
                    continue;
                }
                let side = self.port_side(b, bp);
                let bus = signal.len() > 1 && signal.iter().all(|&n| g.nets[n as usize].bus.is_some() && g.nets[n as usize].bus == g.nets[signal[0] as usize].bus);
                if bus {
                    bus_ports.push((bp.name.clone(), g.nets[signal[0] as usize].bus.unwrap_or(0), signal.clone()));
                }
                for &n in &signal {
                    match side {
                        Side::Left | Side::Right => {
                            let mut pc = Piece::new();
                            pc.bx = [0, 0, 0, 0];
                            let inner = if side == Side::Left { Side::Right } else { Side::Left };
                            pc.ports.push(PPort { at: [0, 0], side: inner, net: n, kind: bp.kind });
                            items.push(Item { piece: pc, pin: Some(side == Side::Right), order: 0, block: None });
                        }
                        _ => drops.push((bp.name.clone(), n, side)),
                    }
                }
            }
        }
        let mut pc = self.arrange(items, b, &bus_ports);
        // Frame.
        if b == 0 {
            return pc;
        }
        // A drop leaves from this level's own marker of its net. A child's leftover marker (a
        // pin inside it that goes on by name) lies inside that child's frame and stays a flag.
        let kids: Vec<Bx> = pc.frames.iter().map(|(_, f)| f.rect).collect();
        let own_drop = |tags: &[Tag], n: u32| tags.iter().position(|t| t.net == Some(n) && t.kind == TagKind::Label && t.text == "\u{1}drop"
            && !kids.iter().any(|r| t.at[0] > r[0] && t.at[0] < r[2] && t.at[1] > r[1] && t.at[1] < r[3]));
        // A port whose net goes on by name (`named_ports`) keeps its flag inside this frame, at
        // its edge, horizontal: the frame is the hull of parts and flags. A left or right port's
        // flag rides its port item's point, outward. A top or bottom port's marker turns sideways
        // where it is clear, else at the end of a stub outward, one step further each try.
        let named = |n: u32| n < BUS && self.named.contains(&(b, n));
        let flag_box = |at: Pt, dir: [i32; 2], n: u32| tag_item_box_of(TagKind::Label, at, dir, &short_name(&g.nets[n as usize].name), None, 0);
        let outer = |f: [f64; 4]| [f[0].floor() as i32, f[1].floor() as i32, f[2].ceil() as i32, f[3].ceil() as i32];
        for p in pc.ports.clone().into_iter().filter(|p| named(p.net)) {
            let dir = if p.side == Side::Right { [-1, 0] } else { [1, 0] };
            pc.tags.push(Tag { net: Some(p.net), at: p.at, dir, kind: TagKind::Label, text: "\u{1}drop".into(), band: None, side: 0 });
            pc.bx = bx_union(pc.bx, outer(flag_box(p.at, dir, p.net)));
        }
        // Top and bottom ports whose flags went inside; one with no clear spot keeps its port.
        let mut inside: Vec<u32> = Vec::new();
        for &(_, n, _) in drops.iter().filter(|(_, n, _)| named(*n)) {
            let Some(t) = own_drop(&pc.tags, n) else { continue };
            let (at, out) = (pc.tags[t].at, pc.tags[t].dir);
            // The flag's box, and the stub past the pin end, stay clear of every body, title,
            // frame, tag and other net's wire at this level.
            let gap = |a: [f64; 4], b: [f64; 4]| item_box_distance(a, b) >= 0.1;
            let off = |pc: &Piece, f: [f64; 4]| {
                pc.parts.iter().all(|(_, part)| gap(f, part.body.map(f64::from)))
                    && pc.labels.iter().all(|l| gap(f, l.bx))
                    && kids.iter().all(|r| !bx_hit(outer(f), *r))
                    && pc.tags.iter().enumerate().filter(|&(j, _)| j != t).all(|(_, o)| gap(f, match (o.text.as_str(), o.net) {
                        ("\u{1}drop", Some(m)) => flag_box(o.at, o.dir, m),
                        _ => tag_item_box(o),
                    }))
                    && pc.wires.iter().chain(&pc.buses).filter(|w| w.net != n).all(|w| w.pts.windows(2).all(|q| segment_box_distance(q[0], q[1], f) >= 0.1))
            };
            // A sideways stub keeps its flag reading on along it, off the stub.
            let facings: &[[i32; 2]] = if out[1] != 0 { &[[1, 0], [-1, 0]] } else { std::slice::from_ref(&out) };
            // One step further out each try: past the content every spot is clear, unless the
            // stub meets something first, and then it would meet it on every later try.
            let mut k = 0;
            let spot = loop {
                let end = add(at, scale(out, k));
                if k > 0 && !off(&pc, bx_pt(bx_pt(EMPTY, add(at, out)), end).map(f64::from)) {
                    break None;
                }
                if let Some(&f) = facings.iter().find(|&&f| off(&pc, flag_box(end, f, n))) {
                    break Some((end, f));
                }
                k += 1;
            };
            let Some((end, facing)) = spot else { continue };
            if end != at {
                pc.wire(n, vec![at, end]);
            }
            pc.tags[t].at = end;
            pc.tags[t].dir = facing;
            pc.bx = bx_union(pc.bx, outer(flag_box(end, facing, n)));
            inside.push(n);
        }
        let mut rect = [pc.bx[0] - o.pad, pc.bx[1] - o.pad, pc.bx[2] + o.pad, pc.bx[3] + o.pad + o.title];
        // The title names the block by its own path segment (the parent's frame names the rest;
        // a child of the root keeps its whole path) and its definition's title: one line while
        // that fits the content's width, else the segment over the title. The frame widens, and
        // its band deepens, only as far as the text needs. Sized before the ports, which sit on
        // the final edges.
        let mut title = {
            let parent = blk.parent.filter(|&p| p != 0).map(|p| g.blocks[p as usize].path.as_str());
            let own = parent.and_then(|p| blk.path.strip_prefix(p)?.strip_prefix('.')).filter(|s| !s.is_empty())
                .unwrap_or(&blk.path);
            let one = match &blk.title {
                Some(t) => format!("{own} \u{b7} {t}"),
                None => own.to_string(),
            };
            match &blk.title {
                Some(t) if text_w(&one, TEXT_TITLE) + 2.0 > f64::from(rect[2] - rect[0]) => vec![own.to_string(), t.clone()],
                _ => vec![one],
            }
        };
        if blk.planned { title.push("planned".into()); }
        if let Some(role) = &blk.role { title.push(role.clone()); }
        let title_w = title.iter().map(|t| text_w(t, TEXT_TITLE)).fold(0.0, f64::max);
        let title_h = TEXT_TITLE * title.len() as f64 + LINE_GAP * TEXT_TITLE * (title.len() - 1) as f64;
        let band = o.title + (title_h - TEXT_TITLE).ceil() as i32;
        rect[2] = rect[2].max(rect[0] + (title_w + 2.0).ceil() as i32);
        rect[3] += band - o.title;
        // No top port's wire runs through the title: see `clear_title`. The jog rows lie between
        // the content (and the drop tags) and the title's foot.
        let drop_at = |n: u32| own_drop(&pc.tags, n).map(|t| pc.tags[t].at);
        let mut tops: Vec<Pt> = drops.iter().filter(|(_, n, side)| *side == Side::Top && !inside.contains(n)).filter_map(|(_, n, _)| drop_at(*n)).collect();
        tops.sort_by_key(|at| at[0]);
        let content_top = tops.iter().map(|at| at[1]).fold(pc.bx[3], i32::max);
        let foot = rect[3] as f64 - 0.4 - title_h;
        let rows = ((foot - TITLE_CLEAR).floor() as i32 - content_top).max(0) as usize;
        let tops_x: Vec<i32> = tops.iter().map(|at| at[0]).collect();
        let (title_x, moved, right) = clear_title(&tops_x, [rect[0], rect[2]], title_w, rows);
        rect[2] = right;
        // Extend the bus spines made before label placement to their frame ports.
        let mut ports = Vec::new();
        let mut bus_nets: Vec<u32> = Vec::new();
        for (name, bus, members) in &bus_ports {
            bus_nets.extend(members.iter().copied());
            let ends: Vec<(u32, Pt)> = pc
                .wires
                .iter()
                .filter(|w| members.contains(&w.net))
                .filter_map(|w| port_end(w, &pc.ports, w.net).map(|e| (w.net, e)))
                .collect();
            if ends.is_empty() {
                continue;
            }
            let x = ends.iter().map(|p| p.1[0]).max().unwrap_or(0) + 1;
            let low = ends.iter().map(|p| p.1[1]).min().unwrap_or(0) - 2;
            rect[2] = rect[2].max(x + o.pad);
            pc.wire(BUS + bus, vec![[x, low], [rect[2], low]]);
            ports.push((FramePort { name: name.clone(), at: [rect[2], low], side: Side::Right, bus: true, kind: PortKind::Bidi, ground: false, open: false }, BUS + bus, PortKind::Bidi));
        }
        // Left and right port items: a wire from the item to the frame edge.
        let items_ports: Vec<PPort> = pc.ports.clone();
        pc.ports.clear();
        for p in items_ports {
            if bus_nets.contains(&p.net) || named(p.net) {
                continue;
            }
            let (edge, side) = if p.side == Side::Right { (rect[0], Side::Left) } else { (rect[2], Side::Right) };
            pc.wire(p.net, vec![[edge, p.at[1]], p.at]);
            let name = blk.ports.iter().find(|bp| bp.nets.contains(&p.net)).map(|bp| bp.name.clone()).unwrap_or_default();
            ports.push((FramePort { name, at: [edge, p.at[1]], side, bus: false, kind: p.kind, ground: false, open: false }, p.net, p.kind));
        }
        for (name, n, side) in drops.iter().filter(|(_, n, _)| !inside.contains(n)) {
            if let Some(t) = own_drop(&pc.tags, *n) {
                let at = pc.tags[t].at;
                pc.tags.remove(t);
                let edge = if *side == Side::Top { rect[3] } else { rect[1] };
                // A moved top port jogs right above the content: the leftmost one highest, so
                // no jog crosses another port's wire.
                let shift = (*side == Side::Top).then(|| moved.iter().position(|&(x, _)| x == at[0])).flatten();
                let x = match shift {
                    Some(k) => {
                        let (to, jog) = (moved[k].1, content_top + (moved.len() - k) as i32);
                        pc.wire(*n, vec![at, [at[0], jog], [to, jog], [to, edge]]);
                        to
                    }
                    None => {
                        pc.wire(*n, vec![at, [at[0], edge]]);
                        at[0]
                    }
                };
                ports.push((FramePort { name: name.clone(), at: [x, edge], side: *side, bus: false, kind: PortKind::Bidi, ground: false, open: false }, *n, PortKind::Bidi));
            }
        }
        pc.labels.push(Label {
            owner: Owner::Block(b),
            lines: title.into_iter().map(|t| (t, TextRole::Title)).collect(),
            bx: [title_x, foot, title_x + title_w, foot + title_h],
            leader: None,
        });
        pc.bx = rect;
        pc.ports = ports.iter().map(|(fp, net, kind)| PPort { at: fp.at, side: fp.side, net: *net, kind: *kind }).collect();
        let declared: std::collections::HashMap<_, _> = blk.ports.iter().map(|p| (p.name.as_str(), p)).collect();
        let ports = ports.into_iter().map(|(mut fp, _, _)| {
            if let Some(p) = declared.get(fp.name.as_str()) {
                fp.kind = p.kind;
                fp.ground = p.ground;
                fp.open = p.open;
            }
            fp
        }).collect();
        pc.frames.push((b, Frame { rect, origin: [0, 0], ports, placed: true, title: band, planned: blk.planned, role: blk.role.clone() }));
        pc
    }

    /// The side of a block port: its definition's, else facing its partner on a sibling (a
    /// sibling's port on the right means this one sits on the left, so the two face each other),
    /// else outputs right and inputs left.
    fn port_side(&self, b: u32, bp: &crate::graph::GBlockPort) -> Side {
        if let Some(s) = bp.side {
            return s;
        }
        let g = self.g;
        let parent = g.blocks[b as usize].parent;
        for (ci, c) in g.blocks.iter().enumerate() {
            if ci as u32 == b || c.parent != parent {
                continue;
            }
            for p in &c.ports {
                if p.nets.iter().any(|n| bp.nets.contains(n)) {
                    match p.side {
                        Some(Side::Right) => return Side::Left,
                        Some(Side::Left) => return Side::Right,
                        _ => {}
                    }
                }
            }
        }
        if bp.kind.drives() { Side::Right } else { Side::Left }
    }

    /// Like blocks side by side (an array, `lamps[0..8]`), bottoms aligned in rows: up to four in
    /// one row, else rows of max(4, ceil(sqrt n)) so a long array wraps instead of widening the
    /// sheet. Each row's bottom ports of one bus join a bus line under that row; a trunk on the left
    /// joins the rows at the array's one bus port.
    fn array(&self, kids: &[u32], b: u32) -> Piece {
        let o = self.o;
        let n = kids.len();
        let cols = if n <= 4 { n.max(1) } else { ((n as f64).sqrt().ceil() as usize).max(4) };
        let mut pc = Piece::new();
        let mut rows: Vec<(i32, Vec<(Pt, u32)>)> = Vec::new();
        let mut others: Vec<PPort> = Vec::new();
        for chunk in kids.chunks(cols) {
            let pieces: Vec<Piece> = chunk.iter().map(|&k| self.block(k)).collect();
            let h = pieces.iter().map(|p| p.bx[3] - p.bx[1]).max().unwrap_or(0);
            // The first row's bottoms sit at 0; a later row hangs below the bus of the one above.
            let bottom = rows.last().map_or(0, |&(bus_y, _)| bus_y - 1 - o.array_gap - h);
            let mut x = 0;
            let mut taps: Vec<(Pt, u32)> = Vec::new();
            for mut kid in pieces {
                let d = [x - kid.bx[0], bottom - kid.bx[1]];
                kid.shift(d);
                x = kid.bx[2] + o.array_gap;
                for p in kid.ports.drain(..) {
                    if p.side == Side::Bottom && p.net < BUS && self.g.nets[p.net as usize].bus.is_some() {
                        taps.push((p.at, p.net));
                    } else {
                        others.push(p);
                    }
                }
                pc.absorb(kid);
            }
            rows.push((bottom - 3, taps));
        }
        let own_wires = pc.wires.len();
        let own_buses = pc.buses.len();
        let own_entries = pc.entries.len();
        let first = rows.iter().flat_map(|(_, t)| t.first()).next().copied();
        if let Some((_, n0)) = first {
            let bus = self.g.nets[n0 as usize].bus.unwrap_or(0);
            let left = pc.bx[0] - 2;
            let mut ys: Vec<i32> = Vec::new();
            for (y, taps) in &rows {
                let Some(right) = taps.iter().map(|t| t.0[0]).max() else { continue };
                for &(at, net) in taps {
                    pc.wire(net, vec![at, [at[0], y + 1]]);
                    pc.entries.push(Entry { net, at: [at[0], y + 1], to: [at[0] + 1, *y] });
                }
                pc.wire(BUS + bus, vec![[left, *y], [right + 1, *y]]);
                ys.push(*y);
            }
            if let (Some(&top), Some(&low)) = (ys.first(), ys.last()) {
                if low != top {
                    pc.wire(BUS + bus, vec![[left, top], [left, low]]);
                }
                pc.ports.push(PPort { at: [left, top], side: Side::Left, net: BUS + bus, kind: PortKind::In });
            }
        }
        pc.ports.extend(others);
        self.place_labels(&mut pc, b, own_wires, own_buses, own_entries);
        pc
    }

    /// A position hint at this level, expressed as the margin box's top-left and the hint's x.
    fn item_pin(&self, it: &Item, b: u32, bx: Bx) -> Option<(Pt, i32)> {
        it.piece.parts.iter().find_map(|&(n, part)| {
            let node = &self.g.nodes[n as usize];
            if node.block != b || !self.can_pin(n) {
                return None;
            }
            let at = node.pin_at?;
            // Pin the part's origin, accounting for its offset in the entire rigid item,
            // including rail tags, molecules and the vertical margin.
            Some(([at[0] - (part.at[0] - bx[0]), at[1] - (bx[3] - part.at[1])], at[0]))
        }).or_else(|| {
            // A child block's hint pins its content origin in this block, the same way.
            let k = it.block?;
            let at = self.g.blocks[k as usize].pin_at?;
            let origin = it.piece.frames.iter().find(|&&(f, _)| f == k)?.1.origin;
            Some(([at[0] - (origin[0] - bx[0]), at[1] - (bx[3] - origin[1])], at[0]))
        })
    }

    /// Places connected items in layers by flow and loose items in rows, routes the signal nets
    /// between them and places the labels of the block's own parts and nets.
    fn arrange(&self, mut items: Vec<Item>, b: u32, bus_ports: &[(String, u32, Vec<u32>)]) -> Piece {
        let o = self.o;
        items.sort_by_key(|it| it.order);
        if items.is_empty() {
            let mut pc = Piece::new();
            pc.bx = [0, 0, 0, 0];
            return pc;
        }
        // Keep each group's source order; only connected items, and pinned ones that connect to
        // something outside themselves, enter the layered graph. An item with a position hint
        // whose nets reach nothing else (a part just placed on empty canvas: each pin on a net
        // of its own until it is wired) sits exactly at its hint, in no column and in no row: in
        // the graph it would stretch its column to the hint and push every later column past it.
        // A part's rail (power or ground) is drawn as a tag at its pin, so it wires nothing here
        // either: a part pasted with only rails and open ends lands where it was put. A child
        // block stays in the graph, where supply flow orders it and a drag can make it give way.
        // A part placed `free` sits at its hint the same way whatever it connects to: its signal
        // pins continue by name (below), so it pushes no column and no pin.
        let free = |it: &Item| it.block.is_none() && it.piece.parts.iter().any(|&(n, _)| self.g.nodes[n as usize].free);
        let inside = |it: &Item, net: u32| !self.bound[net as usize]
            && self.g.nets[net as usize].pins.iter().all(|&(node, _)| it.piece.parts.iter().any(|&(n, _)| n == node));
        let rail = |net: u32| self.g.nets[net as usize].kind != NetKind::Signal;
        let (connected, loose): (Vec<_>, Vec<_>) = items.into_iter().map(|it| {
            let m = if it.piece.ports.len() == 1 && it.piece.bx == [0, 0, 0, 0] { 0 } else { o.margin };
            let bx = [it.piece.bx[0], it.piece.bx[1] - m, it.piece.bx[2], it.piece.bx[3] + m];
            let pin = self.item_pin(&it, b, bx);
            (it, bx, pin)
        }).partition(|(it, _, pin)| {
            !(pin.is_some() && it.pin.is_none() && free(it)) && !(it.piece.ports.is_empty() && it.pin.is_none()
                && it.piece.parts.iter().all(|&(n, _)| self.port_nets[n as usize].iter().all(|net| match net {
                    None => true,
                    Some(net) => pin.is_some() && (inside(it, *net) || (it.block.is_none() && rail(*net))),
                })))
        });
        let (placed_free, loose): (Vec<_>, Vec<_>) = loose.into_iter().partition(|(_, _, pin)| pin.is_some());
        let nc = connected.len();
        let nl = loose.len();
        let boxes: Vec<Bx> = connected.iter().map(|(_, bx, _)| *bx).collect();
        let pins: Vec<Option<(Pt, i32)>> = connected.iter().map(|(_, _, pin)| *pin).collect();
        let loose_boxes: Vec<Bx> = loose.iter().map(|(_, bx, _)| *bx).collect();
        let free_places: Vec<(Bx, Pt)> = placed_free.iter().filter_map(|(_, bx, pin)| pin.map(|(at, _)| (*bx, at))).collect();
        let items: Vec<Item> = connected.into_iter().chain(loose).chain(placed_free).map(|(it, _, _)| it).collect();
        // Terminals per net.
        let mut terms: Vec<(u32, Vec<(usize, usize)>)> = Vec::new();
        for (ii, it) in items[..nc].iter().enumerate() {
            for (pi, p) in it.piece.ports.iter().enumerate() {
                match terms.iter_mut().find(|(n, _)| *n == p.net) {
                    Some((_, v)) => v.push((ii, pi)),
                    None => terms.push((p.net, vec![(ii, pi)])),
                }
            }
        }
        // The layered graph: boxes with their margins, ports at their pin ends.
        let mut lg = LGraph::default();
        for (ii, it) in items[..nc].iter().enumerate() {
            let bx = boxes[ii];
            // The item holding the part or the child block a drag moves leads its level's pinned
            // columns.
            if pins[ii].is_some() && (it.piece.parts.iter()
                .any(|&(n, _)| Some(n) == self.g.lead && self.g.nodes[n as usize].block == b)
                || it.block.is_some_and(|k| Some(k) == self.g.lead_block))
            {
                lg.lead = Some(ii as u32);
            }
            let ports = it
                .piece
                .ports
                .iter()
                .map(|p| match p.side {
                    Side::Left | Side::Right => LPort { side: p.side, offset: (bx[3] - p.at[1]).clamp(0, bx[3] - bx[1]) },
                    _ => LPort { side: p.side, offset: (p.at[0] - bx[0]).clamp(0, bx[2] - bx[0]) },
                })
                .collect();
            lg.nodes.push(LNode {
                size: [bx[2] - bx[0], bx[3] - bx[1]],
                ports,
                order: ii as u32,
                layer: match it.pin {
                    Some(false) => Some(0),
                    _ => None,
                },
                at: None,
            });
        }
        let mut edge_net: Vec<u32> = Vec::new();
        for (net, ts) in &terms {
            let east: Vec<(usize, usize)> = ts.iter().copied().filter(|&(i, p)| items[i].piece.ports[p].side == Side::Right).collect();
            let west: Vec<(usize, usize)> = ts.iter().copied().filter(|&(i, p)| items[i].piece.ports[p].side == Side::Left).collect();
            if east.is_empty() || west.is_empty() {
                // Ends that all face one way pull together through a turn beyond them.
                let ends = if west.is_empty() { &east } else { &west };
                if ends.iter().any(|&(i, _)| i != ends[0].0) {
                    lg.turns.push(ends.iter().map(|&(i, p)| (i as u32, p as u32)).collect());
                }
                continue;
            }
            let mut pairs: Vec<((usize, usize), (usize, usize))> = Vec::new();
            for &e in &east {
                pairs.push((e, west[0]));
            }
            for &w in west.iter().skip(1) {
                pairs.push((east[0], w));
            }
            for (e, w) in pairs {
                if e.0 == w.0 {
                    continue;
                }
                let weight = if *net >= BUS { 4 } else { 1 };
                lg.edges.push(LEdge { from: (e.0 as u32, e.1 as u32), to: (w.0 as u32, w.1 as u32), weight, drawn: true });
                edge_net.push(*net);
            }
        }
        // Supply flow orders blocks with no signal between them (a regulator after its source).
        self.power_edges(&items[..nc], &mut lg);
        // Pin the right-hand ports to the last layer: place once to learn it.
        let mut placed = if nc == 0 {
            layered::Placed::default()
        } else {
            layered::place(&lg, &LOpts { layer_gap: o.layer_gap, per_edge: 1, node_gap: 1 })
        };
        if items[..nc].iter().any(|it| it.pin == Some(true)) {
            let last = placed.layer.iter().enumerate().filter(|&(i, _)| items[i].pin != Some(true)).map(|(_, &l)| l).max().unwrap_or(0) + 1;
            for (i, it) in items[..nc].iter().enumerate() {
                if it.pin == Some(true) {
                    lg.nodes[i].layer = Some(last);
                }
            }
            placed = layered::place(&lg, &LOpts { layer_gap: o.layer_gap, per_edge: 1, node_gap: 1 });
        }
        let has_pins = pins.iter().any(Option::is_some);
        if has_pins {
            // Learn the columns without position hints, including the right-hand port layer.
            // The hint names a part, so its x (not its item's top-left) picks the column: a part
            // put inside another column joins it, among the columns its connections allow (after
            // the parts that feed it, before the parts it feeds); anywhere else it keeps its own.
            // A drag never sends a neighbour to its other side; past them the part's own column
            // widens and they make room. A pinned part a drag pushed stays in its column.
            for (i, &pin) in pins.iter().enumerate() {
                if let Some((_, hint_x)) = pin {
                    let x = i64::from(hint_x);
                    let own = placed.layer[i];
                    let (mut lo, mut hi) = (0, u32::MAX);
                    for e in &lg.edges {
                        let other = match (e.from.0 as usize == i, e.to.0 as usize == i) {
                            (true, false) => e.to.0,
                            (false, true) => e.from.0,
                            _ => continue,
                        };
                        let layer = placed.layer[other as usize];
                        if layer < own { lo = lo.max(layer + 1); }
                        if layer > own { hi = hi.min(layer - 1); }
                    }
                    let layer = placed.columns.iter().enumerate()
                        .filter(|&(layer, _)| (lo..=hi).contains(&(layer as u32)))
                        .find(|&(_, col)| (i64::from(col[0])..=i64::from(col[0]) + i64::from(col[1])).contains(&x))
                        .map_or(own, |(layer, _)| layer as u32);
                    lg.nodes[i].layer = Some(layer);
                }
            }
            if items[..nc].iter().any(|it| it.pin == Some(true)) {
                // Moving an anchor's layer can put its successors beyond the old last layer.
                // Relearn the right edge before installing positions (and resolving conflicts).
                let layers = layered::place(&lg, &LOpts { layer_gap: o.layer_gap, per_edge: 1, node_gap: 1 });
                let last = layers.layer.iter().enumerate().filter(|&(i, _)| items[i].pin != Some(true))
                    .map(|(_, &l)| l).max().unwrap_or(0) + 1;
                for (i, it) in items[..nc].iter().enumerate() {
                    if it.pin == Some(true) {
                        lg.nodes[i].layer = Some(last);
                    }
                }
            }
            for (node, pin) in lg.nodes.iter_mut().zip(pins) {
                node.at = pin.map(|(at, _)| at);
            }
            placed = layered::place(&lg, &LOpts { layer_gap: o.layer_gap, per_edge: 1, node_gap: 1 });
        }
        // Pack loose margin boxes in rows below the connected extent, top-aligned in each row, in
        // their order. The row width is the one whose packed shape is nearest `o.aspect`, never
        // narrower than the connected extent or the widest box.
        let mut loose_pos: Vec<Pt> = Vec::new();
        if !loose_boxes.is_empty() {
            let extent = boxes.iter().zip(&placed.pos).fold(EMPTY, |extent, (bx, pos)| {
                bx_union(extent, [pos[0], pos[1], pos[0] + bx[2] - bx[0], pos[1] + bx[3] - bx[1]])
            });
            let (x0, connected_w, top) = if nc == 0 { (0, 0, 0) } else { (extent[0], extent[2] - extent[0], extent[3] + o.layer_gap) };
            let (gap_x, gap_y) = (o.layer_gap, 1);
            // Positions for a row width, and the packed rows' width and height.
            let pack = |width: i32, pos: &mut Vec<Pt>| -> (i32, i32) {
                pos.clear();
                let (mut x, mut y, mut row_h, mut used_w) = (0, 0, 0, 0);
                for bx in &loose_boxes {
                    let (w, h) = (bx[2] - bx[0], bx[3] - bx[1]);
                    if x > 0 && x + w > width {
                        y += row_h + gap_y;
                        x = 0;
                        row_h = 0;
                    }
                    pos.push([x0 + x, top + y]);
                    used_w = used_w.max(x + w);
                    x += w + gap_x;
                    row_h = row_h.max(h);
                }
                (used_w, y + row_h)
            };
            let miss = |(w, h): (i32, i32)| (f64::from(w.max(1)) / f64::from(h.max(1)) / o.aspect).ln().abs();
            let widest = loose_boxes.iter().map(|bx| bx[2] - bx[0]).max().unwrap_or(0);
            let one_row: i32 = loose_boxes.iter().map(|bx| bx[2] - bx[0] + gap_x).sum::<i32>() - gap_x;
            // Wider rows are flatter: bisect for the narrowest width at least as flat as the aspect,
            // then keep it or the next narrower width, whichever is nearer.
            let floor = widest.max(connected_w);
            let (mut lo, mut hi) = (floor, one_row.max(floor));
            while lo < hi {
                let mid = lo + (hi - lo) / 2;
                let (w, h) = pack(mid, &mut loose_pos);
                if f64::from(w) >= o.aspect * f64::from(h) { hi = mid; } else { lo = mid + 1; }
            }
            let at = pack(lo, &mut loose_pos);
            let width = if lo > floor && miss(pack(lo - 1, &mut loose_pos)) < miss(at) { lo - 1 } else { lo };
            pack(width, &mut loose_pos);
        }
        // Move every item to its place (layered is y down, top-left).
        let mut pc = Piece::new();
        pc.packed = !loose_pos.is_empty();
        let mut placed_ports: Vec<Vec<PPort>> = Vec::new();
        // Per item port: a side port whose row is clear of the item out to its edge, where a
        // channel's stub may start (fable-answers A7 c, 2a).
        let mut on_face: Vec<Vec<bool>> = Vec::new();
        // Per item: a child block, an array of them, or one of this level's ports.
        let mut ported: Vec<bool> = Vec::new();
        let mut layer_of: Vec<u32> = Vec::new();
        for (i, it) in items.into_iter().enumerate() {
            let mut piece = it.piece;
            let (bx, pos) = if i < nc { (boxes[i], placed.pos[i]) }
                else if i < nc + nl { (loose_boxes[i - nc], loose_pos[i - nc]) }
                else { free_places[i - nc - nl] };
            piece.shift([pos[0] - bx[0], -pos[1] - bx[3]]);
            placed_ports.push(piece.ports.clone());
            ported.push(it.block.is_some() || it.pin.is_some() || !piece.frames.is_empty());
            on_face.push(piece.ports.iter().map(|q| {
                let edge = match q.side { Side::Left => piece.bx[0], Side::Right => piece.bx[2], _ => return true };
                let ray = bx_pt(bx_pt(EMPTY, q.at), [edge, q.at[1]]);
                edge == q.at[0] || !(piece.parts.iter().any(|(_, part)| bx_hit(ray, part.body))
                    || piece.frames.iter().any(|(_, f)| bx_hit(ray, f.rect))
                    || piece.tags.iter().any(|t| bx_hit(ray, tag_box(t)))
                    || piece.wires.iter().chain(&piece.buses).any(|w| w.pts.windows(2).any(|seg| bx_hit(ray, bx_pt(bx_pt(EMPTY, seg[0]), seg[1])))))
            }).collect());
            layer_of.push(placed.layer.get(i).copied().unwrap_or(0));
            let ports = std::mem::take(&mut piece.ports);
            if it.pin.is_some() {
                pc.ports.extend(ports);
            }
            // This level's own parts and child block in a column that gave way (a child block's
            // own parts keep their places in it).
            if placed.yielded.get(i) == Some(&true) {
                pc.yielded.extend(piece.parts.iter().map(|&(n, _)| n).filter(|&n| self.g.nodes[n as usize].block == b));
                pc.yielded_blocks.extend(it.block);
            }
            pc.absorb(piece);
        }
        // A free part's signal pins go on by name: a name marker at each pin end, which a frame
        // takes as a port when the net leaves the block and the root names otherwise.
        for ports in &placed_ports[nc + nl..] {
            for p in ports.iter().filter(|p| p.net < BUS && self.g.nets[p.net as usize].kind == NetKind::Signal) {
                pc.tags.push(Tag { net: Some(p.net), at: p.at, dir: side_dir(p.side), kind: TagKind::Label, text: "\u{1}drop".into(), band: None, side: 0 });
            }
        }
        // Geometry from here on is this level's own; the earlier geometry is its items'.
        let own_wires = pc.wires.len();
        let own_buses = pc.buses.len();
        let own_entries = pc.entries.len();
        // Channels: one left of each column, one after the last.
        let cols = &placed.columns;
        let mut chans: Vec<Channel> = Vec::new();
        for k in 0..=cols.len() {
            let x0 = if k == 0 { cols.first().map(|c| c[0]).unwrap_or(0) - o.layer_gap } else { cols[k - 1][0] + cols[k - 1][1] };
            let x1 = if k < cols.len() { cols[k][0] } else { x0 + o.layer_gap };
            chans.push(Channel { x0, x1 });
        }
        let pinned_at: Vec<Pt> = pc.ports.iter().map(|p| p.at).collect();
        let vertical_port = |n: u32| b != 0 && self.g.blocks[b as usize].ports.iter()
            .any(|bp| bp.nets.contains(&n) && matches!(self.port_side(b, bp), Side::Top | Side::Bottom));
        let mut groups: Vec<Group> = Vec::new();
        let mut throughs: Vec<(u32, Pt, Pt)> = Vec::new();
        let mut unrouted: Vec<(u32, PPort)> = Vec::new();
        for (net, ts) in &terms {
            let mut by_chan: Vec<(u32, Vec<Term>)> = Vec::new();
            let mut push = |c: u32, t: Term| match by_chan.iter_mut().find(|(k, _)| *k == c) {
                Some((_, v)) => v.push(t),
                None => by_chan.push((c, vec![t])),
            };
            for &(i, p) in ts {
                let port = placed_ports[i][p];
                let l = layer_of[i];
                match port.side {
                    Side::Right => push(l + 1, Term { at: port.at, face: Face::East }),
                    Side::Left => push(l, Term { at: port.at, face: Face::West }),
                    _ => unrouted.push((*net, port)),
                }
            }
            // Long edges pass through columns at their dummies' heights.
            for (ei, &en) in edge_net.iter().enumerate() {
                if en != *net {
                    continue;
                }
                let from_layer = placed.layer[lg.edges[ei].from.0 as usize];
                let to_layer = placed.layer[lg.edges[ei].to.0 as usize];
                for (k, pt) in placed.through[ei].iter().enumerate() {
                    // Position pins can put a driver to the right of its load. Follow its
                    // through points back towards the load; leave the unhinted route unchanged.
                    let c = if has_pins && from_layer > to_layer {
                        from_layer - 1 - k as u32
                    } else {
                        from_layer + 1 + k as u32
                    };
                    let Some(col) = cols.get(c as usize) else { continue };
                    let y = -pt[1];
                    push(c, Term { at: [col[0], y], face: Face::West });
                    push(c + 1, Term { at: [col[0] + col[1], y], face: Face::East });
                    throughs.push((*net, [col[0], y], [col[0] + col[1], y]));
                }
            }
            let weight = if *net >= BUS { 4 } else { 1 };
            let thr: Vec<u32> = throughs.iter().filter(|t| t.0 == *net).flat_map(|t| {
                let c = cols.iter().position(|col| col[0] == t.1[0]).unwrap_or(0) as u32;
                [c, c + 1]
            }).collect();
            // A net with a side terminal off its item's outer face, one between blocks whose side
            // terminals fall in different channels (each would go on by name), or one between two
            // items when one is ported goes to the sheet router once everything is placed
            // (`route_levels`). Not a bus, or a net leaving
            // through this level's top or bottom port, whose frame takes a marker of it. A split
            // net among parts alone keeps its names: a bundle of them wired one by one weaves.
            let sides: Vec<(usize, usize)> = ts.iter().copied().filter(|&(i, p)| matches!(placed_ports[i][p].side, Side::Left | Side::Right)).collect();
            let inner = sides.iter().any(|&(i, p)| !on_face[i][p]);
            let split = sides.len() >= 2 && sides.iter().any(|&(i, _)| ported[i])
                && by_chan.iter().any(|(c, terms)| terms.len() == 1 && !thr.contains(c));
            let mut items: Vec<usize> = sides.iter().map(|s| s.0).collect();
            items.sort_unstable();
            items.dedup();
            // Between two items, one of them ported, with its side terminals chained within the
            // sheet router's reach of each other (O(terminals²)): a net the router would leave
            // partly by name keeps its channel wires.
            let inter = items.len() >= 2 && sides.iter().any(|&(i, _)| ported[i]) && {
                let reach = FREE_REACH * self.o.layer_gap.max(1);
                let at: Vec<Pt> = sides.iter().map(|&(i, p)| placed_ports[i][p].at).collect();
                let mut joined = vec![false; at.len()];
                joined[0] = true;
                let mut stack = vec![0];
                while let Some(i) = stack.pop() {
                    for j in 0..at.len() {
                        if !joined[j] && (at[i][0] - at[j][0]).abs() + (at[i][1] - at[j][1]).abs() <= reach {
                            joined[j] = true;
                            stack.push(j);
                        }
                    }
                }
                joined.iter().all(|&j| j)
            };
            if *net < BUS && (inner || split || inter) && !vertical_port(*net) {
                throughs.retain(|t| t.0 != *net);
                let terms = sides.iter().map(|&(i, p)| {
                    let q = placed_ports[i][p];
                    (q.at, side_dir(q.side), pinned_at.contains(&q.at))
                }).collect();
                pc.routes.push(LevelRoute { level: b, net: *net, split: !inner, terms });
                continue;
            }
            for (c, terms) in by_chan {
                if terms.len() == 1 && !thr.contains(&c) {
                    // A lone terminal here: its block's frame takes it (a port) or the net goes on
                    // by name.
                    let t = terms[0];
                    if *net < BUS {
                        let dir = if t.face == Face::East { [1, 0] } else { [-1, 0] };
                        if !pinned_at.contains(&t.at) {
                            pc.tags.push(Tag { net: Some(*net), at: t.at, dir, kind: TagKind::Label, text: "\u{1}drop".into(), band: None, side: 0 });
                        }
                    }
                    continue;
                }
                groups.push(Group { net: *net, channel: c, terms, weight });
            }
        }
        let routed = channels::route(&chans, &groups, &ROpts::default());
        for w in routed.wires {
            pc.wire(w.net, w.pts);
        }
        for (net, a, bb) in throughs {
            pc.wire(net, vec![a, bb]);
        }
        for (net, p) in routed.junctions {
            if net < BUS {
                pc.junctions.push((net, p));
            }
        }
        for (net, port) in unrouted {
            if net < BUS && !pc.ports.iter().any(|p| p.at == port.at) {
                pc.tags.push(Tag { net: Some(net), at: port.at, dir: side_dir(port.side), kind: TagKind::Label, text: "\u{1}drop".into(), band: None, side: 0 });
            }
        }
        // Make the block's entries and spines before labels, so a bundle belongs beside the
        // entries at this level. The frame ports are extended after labels set the bounds.
        for (_, bus, members) in bus_ports {
            // A member the sheet router wires later enters at its port all the same.
            let ends: Vec<(u32, Pt)> = pc.wires.iter()
                .filter(|w| members.contains(&w.net))
                .filter_map(|w| port_end(w, &pc.ports, w.net).map(|e| (w.net, e)))
                .chain(pc.routes.iter().filter(|r| r.level == b && members.contains(&r.net))
                    .flat_map(|r| r.terms.iter().filter(|t| t.2).map(|t| (r.net, t.0))))
                .collect();
            if ends.is_empty() {
                continue;
            }
            let x = ends.iter().map(|p| p.1[0]).max().unwrap_or(0) + 1;
            let top = ends.iter().map(|p| p.1[1]).max().unwrap_or(0) - 1;
            let low = ends.iter().map(|p| p.1[1]).min().unwrap_or(0) - 2;
            for &(net, e) in &ends {
                pc.entries.push(Entry { net, at: e, to: [x, e[1] - 1] });
            }
            pc.wire(BUS + bus, vec![[x, top], [x, low]]);
        }
        self.place_labels(&mut pc, b, own_wires, own_buses, own_entries);
        pc
    }

    /// Edges from a supply's source to its loads between items that share no signal, so a
    /// regulator comes after its source and before the chips it feeds. Ports at mid height.
    fn power_edges(&self, items: &[Item], lg: &mut LGraph) {
        let g = self.g;
        // Per item: the supply nets it sources and the ones it takes, from its parts' pins.
        let mut src: Vec<Vec<u32>> = vec![Vec::new(); items.len()];
        let mut load: Vec<Vec<u32>> = vec![Vec::new(); items.len()];
        for (i, it) in items.iter().enumerate() {
            for (n, _) in &it.piece.parts {
                let node = &g.nodes[*n as usize];
                if node.role == Role::Flag { continue }
                for (p, port) in node.ports.iter().enumerate() {
                    let Some(net) = self.port_nets[*n as usize][p] else { continue };
                    if g.nets[net as usize].kind != NetKind::Power {
                        continue;
                    }
                    match port.kind {
                        PortKind::PowerOut => src[i].push(net),
                        PortKind::PowerIn => load[i].push(net),
                        _ => {}
                    }
                }
            }
        }
        for (i, s) in src.iter().enumerate() {
            for (j, l) in load.iter().enumerate() {
                if i == j || !s.iter().any(|n| l.contains(n)) || src[j].iter().any(|n| s.contains(n) && load[i].contains(n)) {
                    continue;
                }
                let pi = lg.nodes[i].ports.len() as u32;
                let h = lg.nodes[i].size[1] / 2;
                lg.nodes[i].ports.push(LPort { side: Side::Right, offset: h });
                let pj = lg.nodes[j].ports.len() as u32;
                let hj = lg.nodes[j].size[1] / 2;
                lg.nodes[j].ports.push(LPort { side: Side::Left, offset: hj });
                // Rails are drawn as tags: the edge orders the columns and is never a wire.
                lg.edges.push(LEdge { from: (i as u32, pi), to: (j as u32, pj), weight: 0, drawn: false });
            }
        }
    }

    /// Labels for the block's own parts (refdes over value) and its nets' names, clear of every
    /// body, tag, wire and child frame. The three start indices exclude absorbed children's
    /// wires, buses and entries: names and strips belong to the level drawing their geometry.
    fn place_labels(&self, pc: &mut Piece, b: u32, own_wires: usize, own_buses: usize, own_entries: usize) {
        let g = self.g;
        let own: Vec<(u32, PartPlace)> = pc.parts.iter().copied().filter(|(_, p)| p.block == b).collect();
        let f = |b: Bx| [b[0] as f64, b[1] as f64, b[2] as f64, b[3] as f64];
        let mut obstacles: Vec<Obstacle> = Vec::new();
        for (n, p) in &pc.parts {
            obstacles.push(Obstacle::Box { b: f(p.body), owner: *n });
        }
        for t in &pc.tags {
            let owner = t.net.map(|n| NET_OWNER + n).unwrap_or(u32::MAX);
            obstacles.push(Obstacle::Box { b: tag_item_box(t), owner });
        }
        // Owners: a net's wires and tags are `NET_OWNER + net`, a bus's lines `BUS_OWNER + bus`
        // (bus and net indices overlap, so they never share a range).
        let runs = pc.wires.iter().map(|r| (r, NET_OWNER)).chain(pc.buses.iter().map(|r| (r, BUS_OWNER)));
        for (r, base) in runs {
            for s in r.pts.windows(2) {
                obstacles.push(Obstacle::Seg {
                    a: [s[0][0] as f64, s[0][1] as f64],
                    b: [s[1][0] as f64, s[1][1] as f64],
                    half: 0.15,
                    owner: base + r.net,
                });
            }
        }
        // Every signal pin's exit lane, two steps out from its end, and the exits of the terminals
        // the sheet router draws from later (`LevelRoute`): taken as if the net's wire were there,
        // whether or not one is drawn, so text moves off them and placement never depends on the
        // routing. O(own pins + terminals).
        let mut lane = |at: Pt, dir: [i32; 2], net: u32| {
            let end = [at[0] + 2 * dir[0], at[1] + 2 * dir[1]];
            obstacles.push(Obstacle::Seg { a: [at[0] as f64, at[1] as f64], b: [end[0] as f64, end[1] as f64], half: 0.15, owner: NET_OWNER + net });
        };
        for (n, p) in &own {
            for (k, port) in g.nodes[*n as usize].ports.iter().enumerate() {
                let Some(net) = self.port_nets[*n as usize][k] else { continue };
                let gn = &g.nets[net as usize];
                if gn.kind != NetKind::Signal || gn.pins.len() < 2 { continue }
                lane(add(p.at, rot(port.at, p.turn, p.mirror)), side_dir(rot_side(port.side, p.turn, p.mirror)), net);
            }
        }
        for r in pc.routes.iter().filter(|r| r.level == b) {
            for &(at, dir, _) in &r.terms {
                lane(at, dir, r.net);
            }
        }
        for (_, fr) in &pc.frames {
            obstacles.push(Obstacle::Box { b: f(fr.rect), owner: u32::MAX });
        }
        for l in &pc.labels {
            obstacles.push(Obstacle::Box { b: l.bx, owner: u32::MAX });
        }
        let mut reqs: Vec<LabelReq> = Vec::new();
        let mut texts: Vec<(Owner, Vec<(String, TextRole)>)> = Vec::new();
        for (n, p) in &own {
            let node = &g.nodes[*n as usize];
            if node.name.starts_with('#') && node.value.is_none() { continue }
            let mut lines = vec![(node.name.clone(), TextRole::Refdes)];
            if let Some(v) = &node.value {
                lines.push((v.clone(), TextRole::Value));
            }
            let w = lines.iter().map(|(t, r)| text_w(t, if *r == TextRole::Refdes { TEXT_REFDES } else { TEXT_VALUE })).fold(0.0, f64::max);
            let h = if lines.len() > 1 { TEXT_REFDES + TEXT_VALUE + LINE_GAP * TEXT_VALUE } else { TEXT_REFDES };
            let vertical = p.turn & 1 == 1;
            let spots = if node.role.is_small() && vertical {
                vec![Spot::Right, Spot::Left, Spot::RightTop, Spot::LeftTop, Spot::Above, Spot::Below]
            } else if node.role.is_small() {
                vec![Spot::Above, Spot::Below, Spot::AboveLeft, Spot::BelowLeft, Spot::Right, Spot::Left]
            } else {
                vec![Spot::AboveLeft, Spot::Above, Spot::BelowLeft, Spot::RightTop, Spot::LeftTop]
            };
            reqs.push(LabelReq { anchor: f(p.body), size: [w, h], spots, priority: if node.role.is_small() { 1 } else { 2 }, owner: *n, optional: false });
            texts.push((Owner::Part(*n), lines));
        }
        // Net names along the longest horizontal piece of each signal net routed at this level,
        // where they fit: a name is optional (the wire shows the connection), so a crowded
        // bundle goes unnamed rather than named from afar.
        // A net that a free part of this level continues by name shows its name on its wired side
        // too: always, on its longest piece of either direction.
        let stubbed: Vec<u32> = pc.parts.iter().map(|&(n, _)| n as usize)
            .filter(|&n| g.nodes[n].free && g.nodes[n].pin_at.is_some() && g.nodes[n].block == b)
            .flat_map(|n| self.port_nets[n].iter().flatten().copied()).collect();
        let mut seen: Vec<u32> = Vec::new();
        for r in &pc.wires[own_wires..] {
            let net = r.net;
            if seen.contains(&net) || g.nets[net as usize].kind != NetKind::Signal || !self.local_net(net, b) {
                continue;
            }
            let named = stubbed.contains(&net);
            let pieces = || pc.wires[own_wires..].iter().filter(|w| w.net == net)
                .flat_map(|w| w.pts.windows(2).map(|s| (s[0], s[1])).collect::<Vec<_>>());
            let long = |(a, bb): &(Pt, Pt)| ((a[0] - bb[0]).abs() + (a[1] - bb[1]).abs(), -a[0].min(bb[0]));
            let best = pieces().filter(|(a, bb)| a[1] == bb[1]).max_by_key(long)
                .or_else(|| named.then(|| pieces().filter(|(a, bb)| a != bb).max_by_key(long)).flatten());
            let Some((a, bb)) = best else { continue };
            if (a[0] - bb[0]).abs() < 3 && !named {
                continue;
            }
            seen.push(net);
            let text = short_name(&g.nets[net as usize].name);
            let (anchor, spots) = if a[1] == bb[1] {
                let x0 = a[0].min(bb[0]) as f64 + 0.5;
                let x1 = (a[0].max(bb[0]) as f64 - 0.5).max(x0);
                ([x0, a[1] as f64 - 0.1, x1, a[1] as f64 + 0.1], vec![Spot::AboveLeft, Spot::BelowLeft])
            } else {
                let y0 = a[1].min(bb[1]) as f64 + 0.5;
                let y1 = (a[1].max(bb[1]) as f64 - 0.5).max(y0);
                ([a[0] as f64 - 0.1, y0, a[0] as f64 + 0.1, y1], vec![Spot::Right, Spot::Left])
            };
            reqs.push(LabelReq {
                anchor,
                size: [text_w(&text, TEXT_NET), TEXT_NET],
                spots,
                priority: 0,
                owner: NET_OWNER + net,
                optional: !named,
            });
            texts.push((Owner::Net(net), vec![(text, TextRole::Net)]));
        }
        // Stacked strips take precedence over single-net strips at the same priority. Keep
        // member order in the graph; the one label box reserves room for the whole stack.
        let mut bundled: Vec<u32> = pc.labels.iter().filter_map(|l| match l.owner {
            Owner::Bundle(bus) => Some(bus),
            _ => None,
        }).collect();
        for r in &pc.buses[own_buses..] {
            let bus = r.net;
            let members = &g.buses[bus as usize].nets;
            if bundled.contains(&bus) || members.iter().filter(|&&net| g.nets[net as usize].kind == NetKind::Signal).count() < 2 {
                continue;
            }
            bundled.push(bus);
            let entries = pc.entries[own_entries..].iter().filter(|e| members.contains(&e.net))
                .flat_map(|e| [e.at, e.to]).fold(EMPTY, bx_pt);
            let anchor = if entries != EMPTY {
                f(entries)
            } else {
                let best = pc.buses[own_buses..].iter().filter(|w| w.net == bus)
                    .flat_map(|w| w.pts.windows(2))
                    .max_by_key(|s| (s[0][0] - s[1][0]).abs() + (s[0][1] - s[1][1]).abs());
                let Some(s) = best else { continue };
                f(bx_pt(bx_pt(EMPTY, s[0]), s[1]))
            };
            reqs.push(LabelReq {
                anchor,
                size: [TRACE_W, BUNDLE_ROW * members.len() as f64 + 0.4],
                spots: vec![Spot::Right, Spot::Left, Spot::Below, Spot::Above, Spot::RightTop, Spot::RightBottom, Spot::LeftTop, Spot::LeftBottom],
                priority: -1,
                // An owner no obstacle has: the stack keeps clear of its own bus and entries too.
                owner: BUNDLE_OWNER + bus,
                optional: true,
            });
            texts.push((Owner::Bundle(bus), Vec::new()));
        }
        // A live strip beside every signal net, at the deepest level it runs at: children are
        // laid out first, so a net a child block already carries a strip for gets none here. A
        // member of a bundle (a child's or one requested above) asks for no single strip: the
        // stack shows it, or, when the stack finds no room, the hover chip does.
        let mut traced: Vec<u32> = pc.labels.iter().filter_map(|l| match l.owner {
            Owner::Trace(net) => Some(net),
            _ => None,
        }).collect();
        traced.extend(bundled.iter().flat_map(|&bus| g.buses[bus as usize].nets.iter().copied()));
        for r in &pc.wires[own_wires..] {
            let net = r.net;
            if traced.contains(&net) || g.nets[net as usize].kind != NetKind::Signal {
                continue;
            }
            traced.push(net);
            let pieces = pc.wires[own_wires..].iter().filter(|w| w.net == net).flat_map(|w| w.pts.windows(2).map(|s| (s[0], s[1])));
            if let Some(req) = strip_request(NET_OWNER + net, pieces) {
                reqs.push(req);
                texts.push((Owner::Trace(net), Vec::new()));
            }
        }
        let placed = labels::place_labels(&reqs, &obstacles, &LabelOpts::default());
        for (i, (owner, lines)) in texts.into_iter().enumerate() {
            // A strip sits beside its wire or not at all: one that had to slide away is left out.
            if placed.left_out[i] || (matches!(owner, Owner::Trace(_) | Owner::Bundle(_)) && placed.leader[i].is_some()) {
                continue;
            }
            let bx = placed.boxes[i];
            pc.bx = bx_union(pc.bx, [bx[0].floor() as i32, bx[1].floor() as i32, bx[2].ceil() as i32, bx[3].ceil() as i32]);
            pc.labels.push(Label { owner, lines, bx, leader: placed.leader[i] });
        }
    }

    /// A net is this block's to name when it is not bound to the block's own ports (the parent
    /// names those) or the block is the root.
    fn local_net(&self, net: u32, b: u32) -> bool {
        b == 0 || !self.g.blocks[b as usize].ports.iter().any(|p| p.nets.contains(&net))
    }
}

/// The space kept between a frame's title and a top port's wire, in grid steps.
const TITLE_CLEAR: f64 = 0.5;

/// Where a frame's title starts, the top ports it moves (`(from x, to x)`, left to right) and the
/// frame's right edge, for top ports at `tops` (sorted x) on a frame spanning `[left, right]`
/// with a title `w` wide, given `rows` free jog rows above the content. In order of preference:
/// the title at its place when no port crosses it; the crossing ports (and those they would
/// pass) moved right of the title, one step apart, while they stay inside the frame and each has
/// a jog row; the title in the first gap between ports it fits; else the frame widens, by
/// whichever of moving the ports or putting the title after the last port is narrower.
/// O(ports).
pub(crate) fn clear_title(tops: &[i32], [left, right]: [i32; 2], w: f64, rows: usize) -> (f64, Vec<(i32, i32)>, i32) {
    let start = f64::from(left) + 0.8;
    let crosses = |x: i32, s: f64| f64::from(x) > s - TITLE_CLEAR && f64::from(x) < s + w + TITLE_CLEAR;
    if !tops.iter().any(|&x| crosses(x, start)) {
        return (start, Vec::new(), right);
    }
    let mut placed = i32::MIN;
    let mut moved = Vec::new();
    for &x in tops.iter().filter(|&&x| f64::from(x) >= start - TITLE_CLEAR) {
        placed = x.max((start + w + TITLE_CLEAR).ceil() as i32).max(placed.saturating_add(1));
        if placed != x {
            moved.push((x, placed));
        }
    }
    let jogs = moved.len() <= rows;
    if jogs && placed < right {
        return (start, moved, right);
    }
    for (k, &x) in tops.iter().enumerate() {
        let s = f64::from(x) + TITLE_CLEAR;
        let end = tops.get(k + 1).map_or(f64::from(right) - 0.8, |&next| f64::from(next) - TITLE_CLEAR);
        if s >= start && end - s >= w {
            return (s, Vec::new(), right);
        }
    }
    let after = tops.last().map_or(start, |&x| f64::from(x) + TITLE_CLEAR);
    let after_right = right.max((after + w + 0.8).ceil() as i32);
    if jogs && placed + 1 < after_right {
        (start, moved, right.max(placed + 1))
    } else {
        (after, Vec::new(), after_right)
    }
}

/// The far end of a wire that ends at one of `ports` (a port item), for bus spines.
fn port_end(w: &Run, ports: &[PPort], net: u32) -> Option<Pt> {
    let first = *w.pts.first()?;
    let last = *w.pts.last()?;
    ports.iter().filter(|p| p.net == net).find_map(|p| {
        if p.at == last {
            Some(last)
        } else if p.at == first {
            Some(first)
        } else {
            None
        }
    })
}

/// Repeated blocks are all-or-none: if one shown member of an array group has no strip, either
/// alone or in a stack (`carried`), no member keeps a single strip, so an array never looks
/// broken (each shows its trace on hover alike). `shown` says which signal nets are drawn: every
/// net on the sheet, the card's own on a reveal.
pub(crate) fn whole_arrays(g: &LayoutGraph, labels: &mut Vec<Label>, shown: impl Fn(u32) -> bool, carried: &[u32]) {
    let traced: Vec<u32> = labels.iter().filter_map(|l| match l.owner {
        Owner::Trace(net) => Some(net),
        _ => None,
    }).collect();
    let groups: Vec<Option<(&str, String)>> = (0..g.nets.len() as u32).map(|net| array_group(g, net)).collect();
    let broken: Vec<&(&str, String)> = groups.iter().enumerate()
        .filter(|&(net, _)| {
            let net = net as u32;
            g.nets[net as usize].kind == NetKind::Signal && shown(net) && !traced.contains(&net) && !carried.contains(&net)
        })
        .filter_map(|(_, group)| group.as_ref())
        .collect();
    labels.retain(|l| match l.owner {
        Owner::Trace(net) => groups[net as usize].as_ref().is_none_or(|group| !broken.contains(&group)),
        _ => true,
    });
}

/// The array group of a net that belongs to one instance of a repeated block: (the array's base
/// path, the port it is bound to or its name inside the instance). The same net of every instance
/// shares the group (`LED0` on `lamps[0]`'s port `in` and `LED7` on `lamps[7]`'s).
fn array_group(g: &LayoutGraph, net: u32) -> Option<(&str, String)> {
    let gn = &g.nets[net as usize];
    g.blocks.iter().enumerate().find_map(|(b, block)| {
        let base = array_base(&block.path)?;
        if let Some(port) = block.ports.iter().find(|p| p.nets.contains(&net)) {
            return Some((base, format!("port {}", port.name)));
        }
        let inside = !gn.pins.is_empty() && gn.pins.iter().all(|&(n, _)| g.nodes[n as usize].block == b as u32);
        inside.then(|| {
            let name = gn.name.strip_prefix(block.path.as_str()).and_then(|r| r.strip_prefix('.')).unwrap_or(&gn.name);
            (base, format!("net {name}"))
        })
    })
}

/// `lamps` for `lamps[3]`.
fn array_base(path: &str) -> Option<&str> {
    let open = path.rfind('[')?;
    path.ends_with(']').then(|| &path[..open])
}

/// A part with its rails as the sheet draws them, at the origin: the stubs, joined runs,
/// junctions and tags of its rail pins, and the box of all of it with the body.
#[derive(Clone, Debug, PartialEq)]
pub struct PartRails {
    pub wires: Vec<Run>,
    pub tags: Vec<Tag>,
    pub junctions: Vec<(u32, Pt)>,
    pub extent: Bx,
}

/// Draws single parts with the sheet's rail rules, for views that place parts themselves (the
/// reveal). Built once per graph: O(graph).
pub struct PartDrawer<'a>(Ctx<'a>);

impl<'a> PartDrawer<'a> {
    pub fn new(g: &'a LayoutGraph, o: &'a Opts) -> Self {
        PartDrawer(Ctx::new(g, o))
    }

    /// Part `n` at the origin with `turn` and `mirror`; the pins in `skip` get no rail. Each run
    /// goes from a pin to its tag: pins that share a tag share the stroke of the joined run.
    pub fn rails(&self, n: u32, turn: u8, mirror: bool, skip: &[usize]) -> PartRails {
        let pc = self.0.part(n, turn, mirror, skip);
        let extent = pc.footprint().into_iter().fold(pc.bx, bx_union);
        let pins: Vec<Pt> = self.0.g.nodes[n as usize].ports.iter().map(|p| rot(p.at, turn, mirror)).collect();
        let (stubs, joins): (Vec<Run>, Vec<Run>) = pc.wires.into_iter().partition(|r| pins.contains(&r.pts[0]));
        let on = |r: &Run, p: Pt| {
            r.pts.windows(2).any(|w| {
                p[0] >= w[0][0].min(w[1][0]) && p[0] <= w[0][0].max(w[1][0]) && p[1] >= w[0][1].min(w[1][1]) && p[1] <= w[0][1].max(w[1][1])
            })
        };
        let wires = stubs
            .into_iter()
            .map(|mut r| {
                let end = *r.pts.last().expect("a stub has two points");
                if !pc.tags.iter().any(|t| t.net == Some(r.net) && t.at == end) {
                    let tag = joins
                        .iter()
                        .filter(|j| j.net == r.net && on(j, end))
                        .find_map(|j| pc.tags.iter().find(|t| t.net == Some(r.net) && on(j, t.at)));
                    if let Some(t) = tag {
                        r.pts.push(t.at);
                    }
                }
                r
            })
            .collect();
        PartRails { wires, tags: pc.tags, junctions: pc.junctions, extent }
    }
}

impl<'a> Ctx<'a> {
    /// Molecules are rigid: only an anchor or the first part of a free chain can pin an item.
    fn can_pin(&self, n: u32) -> bool {
        self.mover(n) == Mover::Itself
    }

    fn mover(&self, n: u32) -> Mover {
        let Some(m) = self.mol_of[n as usize].map(|mi| &self.mols[mi]) else { return Mover::Itself };
        // Only chains hang on no pin.
        match m.host {
            Some((host, port)) => Mover::Rides { host, port },
            None if m.parts[0] == n => Mover::Itself,
            None => Mover::Head(m.parts[0]),
        }
    }

    fn new(g: &'a LayoutGraph, o: &'a Opts) -> Self {
        let port_nets = g.port_nets();
        let mut bound = vec![false; g.nets.len()];
        for b in &g.blocks {
            for p in &b.ports {
                for &n in &p.nets {
                    if let Some(x) = bound.get_mut(n as usize) {
                        *x = true;
                    }
                }
            }
        }
        let mols = molecules::find(g, &port_nets, &bound);
        let flag_ends = g.nets.iter().enumerate().map(|(i, net)| {
            net.kind == NetKind::Signal && !bound[i] && net.pins.len() > 1 && g.unflagged_pins(i as u32).take(2).count() == 1
        }).collect();
        let mut mol_of = vec![None; g.nodes.len()];
        let mut hung = vec![Vec::new(); g.nodes.len()];
        for (i, m) in mols.iter().enumerate() {
            for &p in &m.parts {
                mol_of[p as usize] = Some(i);
            }
            if let Some((h, _)) = m.host {
                hung[h as usize].push(i);
            }
        }
        let mut ctx = Ctx { g, o, port_nets, bound, flag_ends, mols, mol_of, hung, children: g.children(), parts_of: g.parts_of(), named: Default::default() };
        ctx.named = ctx.named_ports();
        ctx
    }

    /// Ports whose net goes on by name at the parent level, as (block, net), known from the graph
    /// before layout. A left or right port's net goes on by name when nothing else at the parent
    /// level carries it: no pin of the parent's own parts, no other port of the block, of a
    /// sibling or of the parent (a bus port carries its bus, not its members). A top or bottom
    /// port is never wired at the parent level: its net goes on by name unless the parent's own
    /// top or bottom port takes it, or an array's bus line taps it. Each such port keeps its flag
    /// inside its block's frame (fable-answers A7 b). O(blocks x ports x sibling ports + pins).
    fn named_ports(&self) -> std::collections::HashSet<(u32, u32)> {
        let g = self.g;
        let signal = |bp: &crate::graph::GBlockPort| -> Vec<u32> {
            bp.nets.iter().copied().filter(|&n| g.nets[n as usize].kind == NetKind::Signal).collect()
        };
        let bus = |nets: &[u32]| nets.len() > 1 && nets.iter().all(|&n| g.nets[n as usize].bus.is_some() && g.nets[n as usize].bus == g.nets[nets[0] as usize].bus);
        let carries = |bp: &crate::graph::GBlockPort, n: u32| { let s = signal(bp); !bus(&s) && s.contains(&n) };
        let mut out = std::collections::HashSet::new();
        for (k, blk) in g.blocks.iter().enumerate().skip(1) {
            let k = k as u32;
            let Some(parent) = blk.parent else { continue };
            let up = &g.blocks[parent as usize];
            let member = self.kid_items(parent).iter().any(|m| m.len() > 1 && m.contains(&k));
            for (i, bp) in blk.ports.iter().enumerate() {
                let s = signal(bp);
                if s.is_empty() || bus(&s) {
                    continue;
                }
                let side = self.port_side(k, bp);
                for &n in &s {
                    let named = match side {
                        Side::Left | Side::Right => !(blk.ports.iter().enumerate().any(|(j, q)| j != i && carries(q, n))
                            || (parent != 0 && up.ports.iter().any(|q| carries(q, n)))
                            || self.children[parent as usize].iter().any(|&c| c != k && g.blocks[c as usize].ports.iter().any(|q| carries(q, n)))
                            || g.nets[n as usize].pins.iter().any(|&(node, _)| g.nodes[node as usize].block == parent)),
                        _ => !(parent != 0 && up.ports.iter().any(|q| carries(q, n) && matches!(self.port_side(parent, q), Side::Top | Side::Bottom)))
                            && !(member && side == Side::Bottom && g.nets[n as usize].bus.is_some()),
                    };
                    if named {
                        out.insert((k, n));
                    }
                }
            }
        }
        out
    }
}

/// The sheet of a graph. Pure: the same graph and options give the same sheet.
pub fn layout_graph(g: &LayoutGraph, o: &Opts) -> SheetLayout {
    if o.mode == SheetMode::Blocks { return blocks_layout(g, o); }
    let ctx = Ctx::new(g, o);
    if g.nodes.iter().enumerate().any(|(n, node)| node.pin_at.is_some() && !ctx.can_pin(n as u32)) {
        eprintln!("fab.explore: position hint on a molecule member ignored; molecules are rigid");
    }
    let mut sheet = SheetLayout {
        parts: vec![PartPlace::default(); g.nodes.len()],
        frames: vec![Frame::default(); g.blocks.len()],
        ..Default::default()
    };
    if g.blocks.is_empty() {
        return sheet;
    }
    let mut root = ctx.block(0);
    let routes = std::mem::take(&mut root.routes);
    // Unrouted drop markers at the root become name flags.
    for t in &mut root.tags {
        if t.text == "\u{1}drop" {
            t.text = t.net.map(|n| short_name(&g.nets[n as usize].name)).unwrap_or_default();
        }
    }
    let rect = [root.bx[0] - o.pad, root.bx[1] - o.pad, root.bx[2] + o.pad, root.bx[3] + o.pad];
    sheet.frames[0] = Frame { rect, origin: [0, 0], ports: Vec::new(), placed: true, ..Frame::default() };
    for (n, p) in root.parts {
        sheet.parts[n as usize] = p;
    }
    for (b, f) in root.frames {
        sheet.frames[b as usize] = f;
    }
    sheet.wires = root.wires;
    sheet.buses = root.buses;
    sheet.entries = root.entries;
    sheet.tags = root.tags;
    sheet.junctions = root.junctions;
    sheet.labels = root.labels;
    sheet.yielded = root.yielded;
    sheet.yielded_blocks = root.yielded_blocks;
    sheet.packed = root.packed;
    sheet.block_movable = vec![false; g.blocks.len()];
    for b in 0..g.blocks.len() as u32 {
        for members in ctx.kid_items(b).into_iter().filter(|members| members.len() == 1) {
            sheet.block_movable[members[0] as usize] = true;
        }
    }
    // Sibling blocks a net or bus runs through each place a strip; the sheet keeps the first.
    let mut traced: Vec<u32> = Vec::new();
    let mut bundled: Vec<u32> = Vec::new();
    sheet.labels.retain(|l| match l.owner {
        Owner::Trace(net) if traced.contains(&net) => false,
        Owner::Trace(net) => { traced.push(net); true }
        Owner::Bundle(bus) if bundled.contains(&bus) => false,
        Owner::Bundle(bus) => { bundled.push(bus); true }
        _ => true,
    });
    // One display per net: a placed bundle carries its members instead of single strips.
    let carried: Vec<u32> = bundled.iter().flat_map(|&bus| g.buses[bus as usize].nets.iter().copied()).collect();
    sheet.labels.retain(|l| !matches!(l.owner, Owner::Trace(net) if carried.contains(&net)));
    whole_arrays(g, &mut sheet.labels, |_| true, &carried);
    sheet.bounds = rect;
    sheet.movers = (0..g.nodes.len() as u32).map(|n| ctx.mover(n)).collect();
    let bodies: Vec<_> = sheet.parts.iter().filter(|p| p.placed).map(|p| p.body).collect();
    route_levels(g, &mut sheet, &routes, o);
    wire_free_pins(g, &mut sheet, o);
    let labels: Vec<_> = sheet.labels.iter().map(|l| l.bx).collect();
    settle_tag_names(&mut sheet.tags, &sheet.wires, &bodies, &labels);
    place_name_flags(&mut sheet);
    sheet
}

/// The nets the levels handed to the sheet router (`LevelRoute`, fable-answers A7 c, 2a). Each
/// level's nets take one route, inside the level's frame, around every body, text and tag, and
/// across a foreign frame only at a cost; a split net reaches no further than `FREE_REACH`. A net
/// with no path goes on by name at each terminal but the level's own ports, whose frame ports
/// carry it on. Each route sees only what lies in its level's frame (and a split one only what
/// lies within reach of its terminals), so it costs O(those cells + obstacles) plus its searches,
/// on the layout worker; only the top level's inner nets search the whole sheet. A bus's members
/// at a level go in one route, drawn as a bundle (`RNet::bundle`), and the nets drawn get their
/// strips afterwards (`strip_routed`).
fn route_levels(g: &LayoutGraph, sheet: &mut SheetLayout, routes: &[LevelRoute], o: &Opts) {
    use crate::sheet_router::{route, Ink, Obstacle as Ob, RNet, RTerm, RouterOpts};
    if routes.is_empty() {
        return;
    }
    let out = |b: [f64; 4]| [b[0].floor() as i32, b[1].floor() as i32, b[2].ceil() as i32, b[3].ceil() as i32];
    let mut walls: Vec<Ob> = sheet.parts.iter().filter(|p| p.placed).map(|p| Ob { bx: p.body, ink: Ink::Wall }).collect();
    walls.extend(sheet.labels.iter().map(|l| Ob { bx: out(l.bx), ink: Ink::Wall }));
    walls.extend(sheet.tags.iter().filter(|t| !t.text.starts_with('\u{1}')).filter_map(|t| tag_wall(t).map(|bx| Ob { bx, ink: Ink::Wall })));
    let within = |mut k: u32, b: u32| loop {
        if k == b { return true; }
        match g.blocks[k as usize].parent { Some(p) => k = p, None => return false }
    };
    // A bus's members at one level go in one route, with the inner ones when any member is
    // inner, so the router draws the bundle together.
    let mut inner_buses: Vec<(u32, u32)> = routes.iter().filter(|r| !r.split)
        .filter_map(|r| g.nets[r.net as usize].bus.map(|bus| (r.level, bus))).collect();
    inner_buses.sort_unstable();
    inner_buses.dedup();
    let split_of = |r: &LevelRoute| r.split && !g.nets[r.net as usize].bus.is_some_and(|bus| inner_buses.binary_search(&(r.level, bus)).is_ok());
    let mut keys: Vec<(u32, bool)> = routes.iter().map(|r| (r.level, split_of(r))).collect();
    keys.sort_unstable();
    keys.dedup();
    // The part of `a` inside `c`, if any.
    let clip = |a: Bx, c: Bx| {
        let x = [a[0].max(c[0]), a[1].max(c[1]), a[2].min(c[2]), a[3].min(c[3])];
        (x[0] <= x[2] && x[1] <= x[3]).then_some(x)
    };
    let mut drew: Vec<(u32, u32)> = Vec::new();
    for (b, split) in keys {
        let reach = if split { FREE_REACH * o.layer_gap.max(1) } else { RouterOpts::default().reach };
        // A split net of two terminals farther apart than that goes on by name straight away.
        let (these, far): (Vec<&LevelRoute>, Vec<&LevelRoute>) = routes.iter().filter(|r| r.level == b && split_of(r) == split).partition(|r| {
            !split || r.terms.len() > 2 || r.terms.windows(2).all(|t| (t[0].0[0] - t[1].0[0]).abs() + (t[0].0[1] - t[1].0[1]).abs() <= reach)
        });
        let mut unrouted: Vec<&LevelRoute> = far;
        if !these.is_empty() {
            let span = these.iter().flat_map(|r| &r.terms).fold(EMPTY, |bx, t| bx_pt(bx, t.0));
            let near = split.then(|| [span[0] - reach, span[1] - reach, span[2] + reach, span[3] + reach]);
            let own = (b != 0).then(|| sheet.frames[b as usize].rect);
            let area = match (own, near) { (Some(f), Some(n)) => clip(f, n), (f, n) => f.or(n) };
            let mut obstacles: Vec<Ob> = walls.clone();
            for (k, f) in sheet.frames.iter().enumerate().skip(1).filter(|(_, f)| f.placed) {
                // A foreign frame costs to cross; the own frame's edges hold the runs inside it.
                if !within(b, k as u32) {
                    obstacles.push(Ob { bx: f.rect, ink: Ink::Frame });
                } else if k as u32 == b {
                    let r = f.rect;
                    obstacles.extend([[r[0], r[1], r[0], r[3]], [r[2], r[1], r[2], r[3]], [r[0], r[1], r[2], r[1]], [r[0], r[3], r[2], r[3]]].map(|bx| Ob { bx, ink: Ink::Wall }));
                }
            }
            let mut drawn: Vec<(u32, Vec<Pt>)> = Vec::new();
            for (net, w) in sheet.wires.iter().map(|w| (w.net, w)).chain(sheet.buses.iter().enumerate().map(|(i, w)| (BUS + i as u32, w))) {
                match area {
                    None => drawn.push((net, w.pts.clone())),
                    Some(a) => drawn.extend(w.pts.windows(2).filter_map(|q| clip(bx_pt(bx_pt(EMPTY, q[0]), q[1]), a))
                        .map(|x| (net, vec![[x[0], x[1]], [x[2], x[3]]]))),
                }
            }
            if let Some(a) = area {
                obstacles = obstacles.into_iter().filter_map(|ob| clip(ob.bx, a).map(|bx| Ob { bx, ..ob })).collect();
            }
            // A bus's members in one route are a bundle, drawn one after another in pin order
            // along their face from the inside of the turn outward, so each turns inside the one
            // before (fable-answers A8 d). The turn is where the far ends lie along the face.
            let mut bundle: Vec<Option<(u32, u32)>> = vec![None; these.len()];
            let mut buses: Vec<u32> = these.iter().filter_map(|r| g.nets[r.net as usize].bus).collect();
            buses.sort_unstable();
            buses.dedup();
            for bus in buses {
                let mut members: Vec<(i32, u32, usize)> = Vec::new();
                let mut turn = 0;
                for (i, r) in these.iter().enumerate().filter(|(_, r)| g.nets[r.net as usize].bus == Some(bus)) {
                    // Seen from the end the router grows the tree from, the first terminal of a
                    // pair: a free path takes the corner farthest from its search's start, which is
                    // the inside of the turn seen from that end. The other reading weaves drum16's
                    // trigger bus.
                    let (from, dir, to) = match r.terms.as_slice() {
                        [first, second] => (first.0, first.1, second.0),
                        _ => {
                            let Some(&(from, dir, _)) = r.terms.iter().find(|t| !t.2).or(r.terms.first()) else { continue };
                            (from, dir, r.terms.iter().find(|t| t.2).or(r.terms.iter().rev().find(|t| t.0 != from)).map_or(from, |t| t.0))
                        }
                    };
                    let k = if dir[0] != 0 { 1 } else { 0 };
                    turn += to[k] - from[k];
                    members.push((from[k], r.net, i));
                }
                // Turning towards lower coordinates, the lowest pin is inside; a bundle that does
                // not turn goes from its high end.
                if turn < 0 {
                    members.sort_unstable_by_key(|m| (m.0, m.1));
                } else {
                    members.sort_unstable_by_key(|m| (-m.0, m.1));
                }
                for (place, m) in members.iter().enumerate() {
                    bundle[m.2] = Some((bus, place as u32));
                }
            }
            let list: Vec<RNet> = these.iter().zip(&bundle).map(|(r, &bundle)| RNet { net: r.net, terms: r.terms.iter().map(|&(at, dir, _)| RTerm { at, dir }).collect(), tree: Vec::new(), bundle }).collect();
            let routed = route(&list, &drawn, &obstacles, &RouterOpts { reach, ..RouterOpts::default() });
            sheet.wires.extend(routed.runs.into_iter().map(|(net, pts)| Run { net, pts }));
            sheet.junctions.extend(routed.junctions);
            unrouted.extend(these.iter().filter(|r| routed.unrouted.contains(&r.net)));
            drew.extend(these.iter().filter(|r| !routed.unrouted.contains(&r.net)).map(|r| (b, r.net)));
        }
        for r in unrouted {
            let text = short_name(&g.nets[r.net as usize].name);
            for &(at, dir, _) in r.terms.iter().filter(|t| !t.2) {
                sheet.tags.push(Tag { net: Some(r.net), at, dir, kind: TagKind::Label, text: text.clone(), band: None, side: 0 });
            }
        }
    }
    strip_routed(g, sheet, &drew);
}

/// Live strips for the nets the sheet router drew in `route_levels`: the level stage placed its
/// labels before those wires existed. Each goes beside the net's longest piece inside its level's
/// frame, clear of everything on the sheet, or not at all; it moves nothing else. The placer sees
/// only what lies near the strips it places (a strip takes one of its listed spots beside its
/// anchor or none), so a level costs O(sheet items) to cull plus the placer's work on that.
fn strip_routed(g: &LayoutGraph, sheet: &mut SheetLayout, drew: &[(u32, u32)]) {
    let f = |b: Bx| [b[0] as f64, b[1] as f64, b[2] as f64, b[3] as f64];
    let mut traced: Vec<u32> = sheet.labels.iter().filter_map(|l| match l.owner { Owner::Trace(n) => Some(n), _ => None }).collect();
    let bundled: Vec<u32> = sheet.labels.iter().filter_map(|l| match l.owner { Owner::Bundle(bus) => Some(bus), _ => None }).collect();
    traced.extend(bundled.iter().flat_map(|&bus| g.buses[bus as usize].nets.iter().copied()));
    let mut common: Vec<Obstacle> = Vec::new();
    for (n, p) in sheet.parts.iter().enumerate().filter(|(_, p)| p.placed) {
        common.push(Obstacle::Box { b: f(p.body), owner: n as u32 });
    }
    for t in &sheet.tags {
        common.push(Obstacle::Box { b: tag_item_box(t), owner: t.net.map(|n| NET_OWNER + n).unwrap_or(u32::MAX) });
    }
    for (r, base) in sheet.wires.iter().map(|r| (r, NET_OWNER)).chain(sheet.buses.iter().map(|r| (r, BUS_OWNER))) {
        for q in r.pts.windows(2) {
            common.push(Obstacle::Seg { a: [q[0][0] as f64, q[0][1] as f64], b: [q[1][0] as f64, q[1][1] as f64], half: 0.15, owner: base + r.net });
        }
    }
    for l in &sheet.labels {
        common.push(Obstacle::Box { b: l.bx, owner: u32::MAX });
    }
    let within = |mut k: u32, b: u32| loop {
        if k == b { return true; }
        match g.blocks[k as usize].parent { Some(p) => k = p, None => return false }
    };
    // Deepest level first: a net's strip goes at the deepest level it runs at, as in `place_labels`.
    let depth = |mut k: u32| { let mut d = 0; while let Some(p) = g.blocks[k as usize].parent { d += 1; k = p; } d };
    let mut levels: Vec<u32> = drew.iter().map(|d| d.0).collect();
    levels.sort_unstable_by_key(|&k| (std::cmp::Reverse(depth(k)), k));
    levels.dedup();
    for b in levels {
        let own = (b != 0).then(|| sheet.frames[b as usize].rect);
        let inside = |p: Pt| own.is_none_or(|r| r[0] <= p[0] && p[0] <= r[2] && r[1] <= p[1] && p[1] <= r[3]);
        let mut reqs: Vec<LabelReq> = Vec::new();
        let mut nets: Vec<u32> = Vec::new();
        for &(_, net) in drew.iter().filter(|d| d.0 == b) {
            if traced.contains(&net) || g.nets[net as usize].kind != NetKind::Signal { continue }
            traced.push(net);
            let pieces = sheet.wires.iter().filter(|w| w.net == net).flat_map(|w| w.pts.windows(2).map(|s| (s[0], s[1]))).filter(|&(a, c)| inside(a) && inside(c));
            if let Some(req) = strip_request(NET_OWNER + net, pieces) {
                reqs.push(req);
                nets.push(net);
            }
        }
        if reqs.is_empty() { continue }
        // Everything within a strip's size and a step of an anchor.
        let m = TRACE_W + TRACE_H + 1.0;
        let near = reqs.iter().fold([f64::MAX, f64::MAX, f64::MIN, f64::MIN], |a, r| {
            [a[0].min(r.anchor[0] - m), a[1].min(r.anchor[1] - m), a[2].max(r.anchor[2] + m), a[3].max(r.anchor[3] + m)]
        });
        let meets = |b: [f64; 4]| b[0] <= near[2] && near[0] <= b[2] && b[1] <= near[3] && near[1] <= b[3];
        let mut obstacles: Vec<Obstacle> = common.iter().filter(|ob| match ob {
            Obstacle::Box { b, .. } => meets(*b),
            Obstacle::Seg { a, b, half, .. } => meets([a[0].min(b[0]) - half, a[1].min(b[1]) - half, a[0].max(b[0]) + half, a[1].max(b[1]) + half]),
        }).cloned().collect();
        for (k, fr) in sheet.frames.iter().enumerate().skip(1).filter(|(_, fr)| fr.placed) {
            if !within(b, k as u32) && meets(f(fr.rect)) {
                obstacles.push(Obstacle::Box { b: f(fr.rect), owner: u32::MAX });
            }
        }
        if let Some(r) = own {
            for e in [[r[0], r[1], r[0], r[3]], [r[2], r[1], r[2], r[3]], [r[0], r[1], r[2], r[1]], [r[0], r[3], r[2], r[3]]] {
                obstacles.push(Obstacle::Box { b: f(e), owner: u32::MAX });
            }
        }
        let placed = labels::place_labels(&reqs, &obstacles, &LabelOpts::default());
        for (i, net) in nets.into_iter().enumerate() {
            if placed.left_out[i] || placed.leader[i].is_some() { continue }
            let bx = placed.boxes[i];
            common.push(Obstacle::Box { b: bx, owner: u32::MAX });
            sheet.labels.push(Label { owner: Owner::Trace(net), lines: Vec::new(), bx, leader: None });
        }
    }
}

/// The grid points a tag walls for the sheet router: those inside its drawn box, so a rail tag or
/// a name flag walls its own pin's row and not the exits of the pins beside it, and a no-connect
/// cross only its pin's end. None when the box holds no grid point; `overlaps` counts any wire
/// that would still touch it.
fn tag_wall(t: &Tag) -> Option<[i32; 4]> {
    let b = tag_item_box(t);
    let bx = if t.kind == TagKind::NoConnect { [t.at[0], t.at[1], t.at[0], t.at[1]] } else { [b[0].ceil() as i32, b[1].ceil() as i32, b[2].floor() as i32, b[3].floor() as i32] };
    (bx[0] <= bx[2] && bx[1] <= bx[3]).then_some(bx)
}

/// How far a free pin's wire reaches to its net, in layer gaps: about a block across. A pin
/// farther from its net's wires keeps its name flag, as a connection across the sheet reads
/// better by name; it also bounds each search to the pin's neighbourhood.
const FREE_REACH: i32 = 16;

/// Wires for the free parts' signal pins (fable-answers A3 step 2, slice 1). Each pin's name flag
/// becomes a run to its net's wires on the sheet, routed by `sheet_router`. The run goes around
/// every body, text and tag, stays inside its own block's frame, and crosses a foreign frame only
/// at a cost. A pin with no path keeps its flag. Nothing runs on a sheet without free parts. Each
/// block holding free parts takes one route: O(sheet cells + obstacles) to build, plus its
/// searches within `FREE_REACH`, on the layout worker.
fn wire_free_pins(g: &LayoutGraph, sheet: &mut SheetLayout, o: &Opts) {
    use crate::sheet_router::{route, Ink, Obstacle as Ob, RNet, RTerm, RouterOpts};
    let free: Vec<u32> = (0..g.nodes.len() as u32).filter(|&n| g.nodes[n as usize].free && sheet.parts[n as usize].placed).collect();
    if free.is_empty() {
        return;
    }
    let nets = g.port_nets();
    // (block, net, pin end, outward direction) of each pin that still shows a flag; a net that
    // leaves the block took a frame port instead.
    let mut pins: Vec<(u32, u32, Pt, [i32; 2])> = Vec::new();
    for &n in &free {
        let (node, part) = (&g.nodes[n as usize], &sheet.parts[n as usize]);
        for (p, port) in node.ports.iter().enumerate() {
            let Some(net) = nets[n as usize][p] else { continue };
            let at = add(part.at, rot(port.at, part.turn, part.mirror));
            // A net the person draws by name, or a bus member, keeps its name.
            let gn = &g.nets[net as usize];
            if gn.kind == NetKind::Signal && gn.style != Some(NetStyle::Label) && gn.bus.is_none()
                && sheet.tags.iter().any(|t| t.kind == TagKind::Label && t.net == Some(net) && t.at == at)
            {
                pins.push((part.block, net, at, side_dir(rot_side(port.side, part.turn, part.mirror))));
            }
        }
    }
    let out = |b: [f64; 4]| [b[0].floor() as i32, b[1].floor() as i32, b[2].ceil() as i32, b[3].ceil() as i32];
    let is_flag = |t: &Tag| t.kind == TagKind::Label && pins.iter().any(|&(_, net, at, _)| t.net == Some(net) && t.at == at);
    let mut walls: Vec<Ob> = sheet.parts.iter().filter(|p| p.placed).map(|p| Ob { bx: p.body, ink: Ink::Wall }).collect();
    walls.extend(sheet.labels.iter().map(|l| Ob { bx: out(l.bx), ink: Ink::Wall }));
    walls.extend(sheet.tags.iter().filter(|t| !t.text.starts_with('\u{1}') && !is_flag(t)).filter_map(|t| tag_wall(t).map(|bx| Ob { bx, ink: Ink::Wall })));
    let within = |mut k: u32, b: u32| loop {
        if k == b { return true; }
        match g.blocks[k as usize].parent { Some(p) => k = p, None => return false }
    };
    let mut blocks: Vec<u32> = pins.iter().map(|p| p.0).collect();
    blocks.sort_unstable();
    blocks.dedup();
    let mut wired: Vec<(u32, Pt)> = Vec::new();
    for b in blocks {
        let mut obstacles = walls.clone();
        for (k, f) in sheet.frames.iter().enumerate().skip(1).filter(|(_, f)| f.placed) {
            // A foreign frame costs to cross; the own frame's edges hold the run inside it.
            if !within(b, k as u32) {
                obstacles.push(Ob { bx: f.rect, ink: Ink::Frame });
            } else if k as u32 == b {
                let r = f.rect;
                obstacles.extend([[r[0], r[1], r[0], r[3]], [r[2], r[1], r[2], r[3]], [r[0], r[1], r[2], r[1]], [r[0], r[3], r[2], r[3]]].map(|bx| Ob { bx, ink: Ink::Wall }));
            }
        }
        let mut list: Vec<RNet> = Vec::new();
        for &(_, net, at, dir) in pins.iter().filter(|p| p.0 == b) {
            match list.iter_mut().find(|r| r.net == net) {
                Some(r) => r.terms.push(RTerm { at, dir }),
                None => list.push(RNet {
                    net,
                    bundle: None,
                    terms: vec![RTerm { at, dir }],
                    tree: sheet.wires.iter().filter(|w| w.net == net).map(|w| w.pts.clone()).collect(),
                }),
            }
        }
        let drawn: Vec<(u32, Vec<Pt>)> = sheet.wires.iter().map(|w| (w.net, w.pts.clone()))
            .chain(sheet.buses.iter().enumerate().map(|(i, w)| (BUS + i as u32, w.pts.clone()))).collect();
        let routed = route(&list, &drawn, &obstacles, &RouterOpts { reach: FREE_REACH * o.layer_gap.max(1), ..RouterOpts::default() });
        for r in list.iter().filter(|r| !routed.unrouted.contains(&r.net)) {
            wired.extend(r.terms.iter().map(|t| (r.net, t.at)));
        }
        sheet.wires.extend(routed.runs.into_iter().map(|(net, pts)| Run { net, pts }));
        sheet.junctions.extend(routed.junctions);
    }
    sheet.tags.retain(|t| !(t.kind == TagKind::Label && wired.iter().any(|&(net, at)| t.net == Some(net) && t.at == at)));
}

/// Pairs of things that overlap where nothing may: part bodies with each other and with labels,
/// labels with each other, frames of siblings, a wire through a frame's title, two nets' wires
/// along one stretch of a line (drawn as one). Zero on a good sheet.
/// Every pair of placed bodies, labels and tags whose boxes overlap, named (`part 3`, `label R1`).
/// A no-connect cross sits on its own pin's end, on its body's edge: that pair is not an overlap.
pub fn overlaps(s: &SheetLayout) -> Vec<(String, String)> {
    let inside = |a: [f64; 4], b: [f64; 4]| a[0] < b[2] - 1e-6 && b[0] < a[2] - 1e-6 && a[1] < b[3] - 1e-6 && b[1] < a[3] - 1e-6;
    let f = |b: Bx| [b[0] as f64, b[1] as f64, b[2] as f64, b[3] as f64];
    // (name, box, the body's grid box for a part, the pin end of a no-connect cross)
    let mut items: Vec<(String, [f64; 4], Option<Bx>, Option<Pt>)> = Vec::new();
    for (i, p) in s.parts.iter().enumerate() {
        if p.placed {
            items.push((format!("part {i}"), f(p.body), Some(p.body), None));
        }
    }
    for l in &s.labels {
        let text = l.lines.first().map(|(t, _)| t.as_str()).unwrap_or("");
        items.push((format!("label {text} of {:?}", l.owner), l.bx, None, None));
    }
    for t in s.tags.iter().filter(|t| !t.text.starts_with('\u{1}')) {
        let nc = (t.kind == TagKind::NoConnect).then_some(t.at);
        items.push((format!("{:?} tag {} at {:?}", t.kind, t.text, t.at), tag_item_box(t), None, nc));
    }
    let on_edge = |b: Bx, p: Pt| {
        p[0] >= b[0] && p[0] <= b[2] && p[1] >= b[1] && p[1] <= b[3] && (p[0] == b[0] || p[0] == b[2] || p[1] == b[1] || p[1] == b[3])
    };
    let own_pin = |a: &(String, [f64; 4], Option<Bx>, Option<Pt>), b: &(String, [f64; 4], Option<Bx>, Option<Pt>)| {
        matches!((a.2, b.3), (Some(body), Some(pin)) if on_edge(body, pin)) || matches!((b.2, a.3), (Some(body), Some(pin)) if on_edge(body, pin))
    };
    let mut out = Vec::new();
    for i in 0..items.len() {
        for j in i + 1..items.len() {
            if inside(items[i].1, items[j].1) && !own_pin(&items[i], &items[j]) {
                out.push((items[i].0.clone(), items[j].0.clone()));
            }
        }
    }
    // A wire through a frame's title.
    for l in s.labels.iter().filter(|l| matches!(l.owner, Owner::Block(_))) {
        for w in &s.wires {
            if w.pts.windows(2).any(|q| inside(f(bx_pt(bx_pt(EMPTY, q[0]), q[1])), l.bx)) {
                let text = l.lines.first().map(|(t, _)| t.as_str()).unwrap_or("");
                out.push((format!("wire of net {}", w.net), format!("title {text}")));
            }
        }
    }
    // Two nets' wires along one stretch: each unit step of a wire, by its lower end and axis, keeps
    // the first run on it. O(wire length).
    let mut steps: std::collections::HashMap<(Pt, bool), usize> = std::collections::HashMap::new();
    let mut shared: Vec<(usize, usize)> = Vec::new();
    for (i, w) in s.wires.iter().enumerate() {
        for q in w.pts.windows(2) {
            let d = [(q[1][0] - q[0][0]).signum(), (q[1][1] - q[0][1]).signum()];
            let mut p = q[0];
            while p != q[1] {
                let next = [p[0] + d[0], p[1] + d[1]];
                let key = (p.min(next), d[1] == 0);
                match steps.get(&key) {
                    Some(&j) => if s.wires[j].net != w.net && !shared.contains(&(j, i)) { shared.push((j, i)) },
                    None => { steps.insert(key, i); }
                }
                p = next;
            }
        }
    }
    out.extend(shared.into_iter().map(|(j, i)| (format!("wire of net {} {:?}", s.wires[j].net, s.wires[j].pts), format!("wire of net {} {:?} along it", s.wires[i].net, s.wires[i].pts))));
    out
}

pub fn overlap_count(s: &SheetLayout) -> usize {
    overlaps(s).len()
}

#[cfg(test)]
mod architecture {
    use super::*;
    use fab_elec_lang::{evaluate, std_libs, Sources};

    fn architecture(second: bool) -> LayoutGraph {
        let mut src = Sources::new();
        let mut text = r#"use mod.std.*
let e = mod.elec
let load = e.block({name: "Load" planned: true role: "Controller" ports: {
    v: e.port(@power_in, {current: 900mA}) g: e.port(@ground) io: e.port(@in)}})
let input = e.net({name: "5V" kind: @power volts: 5V})
let output = e.net({name: "3V3" kind: @power volts: 3.3V})
let gnd = e.net({name: "GND" kind: @ground})
let reg = ldo({vin: input vout: output gnd: gnd})
let ctl = load({v: output g: gnd})
let data = e.bus("DATA", 2)
let ack = e.net({name: "ACK"})
let ready = e.net({name: "READY"})
let driver = e.block({name: "Sender" planned: true ports: {tx: e.port(@out, {width: 2}) ack: e.port(@bidi) ready: e.port(@bidi)}})
let sender = driver({tx: data ack: ack ready: ready})
"#.to_owned();
        if second {
            text.push_str(r#"let other = ldo({vin: input vout: output gnd: gnd})
let receiver = e.block({name: "Receiver" planned: true ports: {a: e.port(@in) b: e.port(@in) ack: e.port(@bidi) ready: e.port(@bidi)}})
let sink = receiver({a: data[0] b: data[1] ack: ack ready: ready})
"#);
        } else {
            text.push_str(r#"let receiver = e.block({name: "Receiver" planned: true ports: {rx: e.port(@in, {width: 2}) ack: e.port(@bidi) ready: e.port(@bidi)}})
let sink = receiver({rx: data ack: ack ready: ready})
"#);
        }
        src.add("device.ckt.splash", text).unwrap();
        let mut ev = evaluate(&src, &std_libs()).expect("the realized regulator and planned load must evaluate");
        let circuit = std::sync::Arc::make_mut(&mut ev.design.units[0].circuit);
        if second {
            let reg = circuit.tree.iter_mut().find(|b| b.path == "reg").expect("the realized LDO must exist");
            reg.ports.reverse();
        }
        crate::adapt::graph_of(circuit, &Default::default())
    }

    #[test]
    fn blocks_mode_lays_out_planned_frames_ports_and_rail_budgets_deterministically() {
        let opts = Opts { mode: SheetMode::Blocks, ..Opts::default() };
        for second in [false, true] {
            let g = architecture(second);
            let sheet = layout_graph(&g, &opts);
            assert_eq!(sheet, layout_graph(&g, &opts), "identical architecture must lay out identically");
            for label in &sheet.labels {
                for run in sheet.wires.iter().chain(&sheet.buses) {
                    for seg in run.pts.windows(2) {
                        assert!(segment_box_distance(seg[0], seg[1], label.bx) > 0.0,
                            "label {:?} must stay clear of the segment {:?}, including on its own net", label.lines, seg);
                    }
                }
            }
            assert!(sheet.parts.iter().all(|p| !p.placed), "opaque blocks must hide their components");
            let frames: Vec<_> = sheet.frames.iter().skip(1).filter(|f| f.placed).collect();
            assert_eq!(frames.len(), if second { 5 } else { 4 }, "each direct child must have a frame");
            for (i, f) in frames.iter().enumerate() {
                let [l,b,r,t] = f.rect;
                assert!(l < r && b < t, "every block must occupy a box");
                assert!(frames[..i].iter().all(|other| !bx_hit(f.rect, other.rect)), "block frames must stay apart");
                if f.ports.iter().any(|p| p.ground) {
                    assert_eq!(f.ports.len(), 3, "the regulator and load must retain all power, signal and ground ports");
                }
                for p in &f.ports {
                    assert!(match p.side { Side::Left => p.at[0] == l, Side::Right => p.at[0] == r, Side::Bottom => p.at[1] == b, Side::Top => p.at[1] == t },
                        "{} must lie on its frame edge", p.name);
                    assert_eq!(p.ground, matches!(p.name.as_str(), "g" | "gnd"), "ground must follow the declared port name even after reordering bindings");
                    assert!(!p.ground || p.side == Side::Bottom, "ground must leave by the bottom");
                }
            }
            let label = if second { "3V3 · 3.3V · multiple sources" } else { "3V3 · 3.3V · 900mA/600mA" };
            assert!(sheet.labels.iter().flat_map(|l| &l.lines).any(|(text, _)| text == label),
                "the routed rail must show the same physical sources and budget as ERC: {label}");
            let n = g.nets.iter().position(|n| n.name == "3V3").unwrap() as u32;
            let data = g.buses.iter().find(|bus| bus.name == "DATA").expect("the signal bus must survive adaptation");
            let terminal = |name: &str| frames.iter().flat_map(|f| &f.ports).find(|p| p.name == name).unwrap().at;
            if second {
                // The split data bus orders the sender and receiver in separate columns.
                // ACK and READY leave both boxes on the right, bridging two channels.
                let roof = frames.iter().map(|f| f.rect[3]).max().unwrap();
                for name in ["ACK", "READY"] {
                    let net = g.nets.iter().find(|n| n.name == name).unwrap().key;
                    let bridge = sheet.wires.iter().filter(|w| w.net == net && w.pts.len() == 2
                        && w.pts[0][1] > roof && w.pts[0][1] == w.pts[1][1])
                        .max_by_key(|w| (w.pts[1][0] - w.pts[0][0]).abs())
                        .unwrap_or_else(|| panic!("{name} must bridge above the frames"));
                    for &[x, y] in &bridge.pts {
                        let crosses = |w: &Run| w.pts.windows(2).any(|s| s[0][0] == x && s[1][0] == x
                            && (s[0][1].min(s[1][1])..=s[0][1].max(s[1][1])).contains(&y));
                        assert!(!sheet.wires.iter().filter(|w| w.net != net).chain(&sheet.buses).any(crosses),
                            "no other net's channel track may run through the {name} bridge terminal at ({x}, {y})");
                        assert!(frames.iter().any(|f| f.rect[2] == x),
                            "the {name} bridge terminal at ({x}, {y}) must lie on the right edge of its block column");
                    }
                }
                for (i, name) in ["a", "b"].iter().enumerate() {
                    for at in [terminal(&format!("tx[{i}]")), terminal(name)] {
                        assert!(sheet.wires.iter().any(|w| w.net == data.nets[i] && w.pts.contains(&at)),
                            "a bus split at the receiver must connect bit {i} at both frames");
                    }
                }
            } else {
                let label = sheet.labels.iter().find(|l| l.owner == Owner::Net(data.nets[0])
                    && l.lines.iter().any(|(name, _)| name == "DATA")).expect("the bundled bus must have its name on the sheet");
                assert!(frames.iter().all(|f| item_box_distance(f.rect.map(f64::from), label.bx) > 0.0),
                    "the bus name must stay clear of every block frame");
                for other in sheet.labels.iter().filter(|l| matches!(l.owner, Owner::Net(n) if n != data.nets[0])) {
                    assert!(sheet.buses.iter().flat_map(|w| w.pts.windows(2)).all(|s| segment_box_distance(s[0], s[1], other.bx) > 0.15),
                        "rail and signal labels must stay clear of the bundled bus: {:?}", other.lines);
                }
                for at in [terminal("tx"), terminal("rx")] {
                    assert!(sheet.buses.iter().any(|w| w.pts.contains(&at)), "a whole bus must connect both frame ports");
                }
            }
            for f in frames.into_iter().filter(|f| f.ports.iter().any(|p| p.ground)) {
                let port = f.ports.iter().find(|p| p.name == "vout" || p.name == "v").unwrap();
                assert!(sheet.wires.iter().any(|w| w.net == n && w.pts.contains(&port.at)), "the rail must touch each supply and load");
            }
        }
    }

    #[test]
    fn schematic_mode_gives_planned_block_non_zero_frame() {
        let g = architecture(false);
        let sheet = layout_graph(&g, &Opts::default());
        let frame = sheet.frames.iter().find(|f| f.planned).expect("the schematic must keep the planned load");
        assert!(frame.placed && frame.rect[0] < frame.rect[2] && frame.rect[1] < frame.rect[3], "an unbuilt block must occupy a visible frame");
        assert!(frame.ports.iter().any(|p| p.name == "io" && p.open), "an unbound signal port must stay on its frame");
        assert!(sheet.parts.iter().any(|p| p.placed), "realized blocks must still show their components");
    }
}
