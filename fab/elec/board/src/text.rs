//! Board lettering: text as strokes of the bundled technical stroke font, the one definition the view
//! draws, DRC checks and the silk plot prints.

use crate::{Board, BoardOp, Field, LabelPose, Origin, Placed};
use fab_elec_core::{Angle, BBox, ItemId, LayerId, LayerSide, Nm, PadIx, Shape, Side, P};
use fab_elec_parts::{Footprint, FpText, FpTextKind};
use makepad_typography::stroke::{stroke_layout, StrokeFont, StrokeStyle};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, LazyLock};

static FONT: LazyLock<StrokeFont> =
    LazyLock::new(|| StrokeFont::bundled("technical").expect("the technical stroke font is bundled"));

/// A box containing every text stroke at any rotation or mirror. An em is 1.4 cap heights,
/// a glyph advances at most about one em, and each letter adds one stroke width of spacing.
/// A box of this radius contains the centred text, including thick strokes, at any turn.
pub fn text_bound(at: P, text: &str, size: Nm, thick: Nm) -> BBox {
    let chars = text.chars().count() as Nm;
    BBox::point(at).inflate((size + thick) * (chars + 1))
}

/// The strokes of `text` laid out with the bundled technical stroke font: cap height `size` (nm),
/// stroke width `thick` (nm), centred on `at` (the text's anchor is its centre, both ways), turned by
/// `rot` (CCW) kept upright (`upright`), mirrored in x when `mirror` (bottom-side text reads from
/// below). Board frame nm.
pub fn text_strokes(text: &str, at: P, rot: Angle, size: Nm, thick: Nm, mirror: bool) -> Vec<Shape> {
    let rot = upright(rot, mirror);
    let font = &*FONT;
    if size <= 0 || text.is_empty() {
        return Vec::new();
    }

    // Default layout coordinates are ems; the font's cap height is in font units.
    let scale = size as f64 * font.em as f64 / font.cap_height as f64;
    let line_step = 1.4 * size as f64;
    // The font spaces letters by their centre lines; a silk stroke is thick, so the optical gap is
    // kept between the ink's edges (one stroke width more), or neighbours touch: "R1" read "Rl".
    let plain = StrokeStyle::default();
    let style = StrokeStyle { spacing: plain.spacing + (thick as f64 / scale) as f32, ..plain };
    let mut lines = Vec::new();
    let (mut y_min, mut y_max) = (f64::INFINITY, f64::NEG_INFINITY);
    for (i, line) in text.split('\n').enumerate() {
        let layout = stroke_layout(font, line, &style);
        let baseline = -(i as f64) * line_step;
        let (mut x_min, mut x_max) = (f64::INFINITY, f64::NEG_INFINITY);
        for stroke in &layout.strokes {
            for &[x, y] in &stroke.points {
                let (x, y) = (x as f64 * scale, y as f64 * scale + baseline);
                x_min = x_min.min(x);
                x_max = x_max.max(x);
                y_min = y_min.min(y);
                y_max = y_max.max(y);
            }
        }
        if x_min <= x_max {
            lines.push((layout, (x_min + x_max) * 0.5, baseline));
        }
    }
    if lines.is_empty() {
        return Vec::new();
    }

    // Centre the ink as a block before mirroring and rotating in the board frame.
    let y_center = (y_min + y_max) * 0.5;
    let (sin, cos) = rot.sin_cos();
    let mut shapes = Vec::new();
    for (layout, x_center, baseline) in lines {
        let point = |p: [f32; 2]| {
            let x = p[0] as f64 * scale - x_center;
            let x = if mirror { -x } else { x };
            let y = p[1] as f64 * scale + baseline - y_center;
            P::new((x * cos - y * sin).round() as Nm, (x * sin + y * cos).round() as Nm).add(at)
        };
        for stroke in &layout.strokes {
            for pair in stroke.points.windows(2) {
                let (a, b) = (point(pair[0]), point(pair[1]));
                if a != b {
                    shapes.push(Shape::Segment { a, b, w: thick });
                }
            }
        }
    }
    shapes
}

/// `rot`, or its half-turn when that would read upside down: text never reads upside down, and
/// vertical text reads from the bottom or the right of the side it is read from. A mirrored text is
/// read from the back, with the board turned over left to right, where it shows at `-rot`. The
/// anchor is the text's centre, so the half-turn keeps it in place.
pub fn upright(rot: Angle, mirror: bool) -> Angle {
    let seen = if mirror { rot.neg() } else { rot };
    let quarter = Angle::QUARTER.udeg();
    if seen.udeg() > quarter && seen.udeg() <= 3 * quarter { rot.add(Angle::HALF) } else { rot }
}

/// The string shown for a footprint text, using the placement's reference and value when set.
pub fn shown_text<'a>(placed: &'a Placed, fp: &'a FpText) -> &'a str {
    match fp.kind {
        FpTextKind::Reference => &placed.refdes,
        FpTextKind::Value => placed.value.as_deref().unwrap_or(&fp.text),
        FpTextKind::User => &fp.text,
    }
}

/// A footprint text's pose in the footprint frame: a Reference or Value text at the placement's own
/// label pose when it has one, every other text where the footprint puts it.
pub fn text_pose(placed: &Placed, fp: &FpText) -> (P, Angle) {
    let own = match fp.kind {
        FpTextKind::Reference => placed.ref_label,
        FpTextKind::Value => placed.value_label,
        FpTextKind::User => None,
    };
    own.map_or((fp.at, fp.rot), |label| (label.at, label.rot))
}

/// A footprint text as placed: the board layer it is on and its strokes. Reference and Value
/// resolve through `shown_text`; visibility is the caller's.
pub fn footprint_text(placed: &Placed, fp: &FpText, copper_count: u8) -> (LayerId, Vec<Shape>) {
    let text = shown_text(placed, fp);
    let layer = placed.fp_layer(fp.layer, copper_count);
    let (at, rot) = text_pose(placed, fp);
    let rot = placed.rot.add(if placed.side == Side::Bottom { rot.neg() } else { rot });
    let strokes = text_strokes(text, placed.to_board(at), rot, fp.size, fp.thick, layer.side() == LayerSide::Back);
    (layer, strokes)
}

fn ink_box(strokes: &[Shape]) -> BBox {
    strokes.iter().fold(BBox::EMPTY, |b, s| b.union(s.bbox()))
}

/// Width and height of the text's ink box, including the stroke width; `(0, 0)` for no strokes.
pub fn ink_size(text: &str, size: Nm, thick: Nm) -> (Nm, Nm) {
    let b = ink_box(&text_strokes(text, P::ZERO, Angle::ZERO, size, thick, false));
    (b.width() as Nm, b.height() as Nm)
}

const GAP: Nm = 150_000;
const LABEL_CELL: i64 = 2_000_000;
/// Parts of one row (or column) have their centres within this of one line.
const ROW_ALIGN: i64 = 50_000;
/// ... and at most this much space between neighbouring courtyards along it.
const ROW_GAP: Nm = 2_000_000;

/// Whether `a` and `b` share more than an edge. What a label must clear is held as its box grown by
/// the label gap, so a label exactly the gap away touches that box and is clear of it.
fn overlaps(a: &BBox, b: &BBox) -> bool {
    !a.is_empty() && !b.is_empty() && a.min.x < b.max.x && b.min.x < a.max.x && a.min.y < b.max.y && b.min.y < a.max.y
}

fn corners(b: BBox) -> [P; 4] {
    [b.min, P::new(b.max.x, b.min.y), b.max, P::new(b.min.x, b.max.y)]
}

fn courtyard_box(placed: &Placed) -> BBox {
    let b = placed.footprint.courtyard.bbox();
    if b.is_empty() { placed.bbox() } else { BBox::of_points(corners(b).map(|p| placed.to_board(p))) }
}

/// Everything of a footprint a label must clear, in its own frame: its courtyard, and its pads on
/// either side grown by the larger of their and the board's mask margin, with their drills.
fn footprint_extent(footprint: &Footprint, margin: Nm) -> BBox {
    let mut b = footprint.courtyard.bbox();
    for pad in &footprint.pads {
        let grow = pad.mask_margin.unwrap_or(margin).max(margin).max(0);
        for shape in [Some(&pad.stack.front), pad.stack.back.as_ref()].into_iter().flatten() {
            b = b.union(shape.shape(pad.at, pad.rot).bbox().inflate(grow));
        }
        if let Some(drill) = &pad.drill {
            let offset = drill.offset.x.abs() + drill.offset.y.abs();
            b = b.union(BBox::point(pad.at).inflate(offset + drill.size.x.max(drill.size.y) / 2 + grow));
        }
    }
    b
}

fn label_side(layer: LayerId) -> Side {
    if layer.side() == LayerSide::Back { Side::Bottom } else { Side::Top }
}

fn side_index(side: Side) -> usize {
    if side == Side::Bottom { 1 } else { 0 }
}

fn label_cells(b: BBox) -> impl Iterator<Item = (i64, i64)> {
    let min_x = (b.min.x as i64).div_euclid(LABEL_CELL);
    let min_y = (b.min.y as i64).div_euclid(LABEL_CELL);
    let max_x = (b.max.x as i64).div_euclid(LABEL_CELL);
    let max_y = (b.max.y as i64).div_euclid(LABEL_CELL);
    (min_x..=max_x).flat_map(move |x| (min_y..=max_y).map(move |y| (x, y)))
}

/// What keeps a label off a spot.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LabelBlocker {
    Courtyard(ItemId),
    Pad(ItemId),
    Via(ItemId),
    Hole(ItemId),
    Text(ItemId),
    /// Another part's label.
    Label(ItemId),
    /// The spot leaves the board outline, or comes within the label gap of it.
    Outline,
}

/// A label that could not take its row's side and turn and was placed on its own.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct OffRow {
    pub item: ItemId,
    /// The first part of its row.
    pub row: ItemId,
    /// What holds the row's spot for it.
    pub blocked_by: LabelBlocker,
}

/// `arrange_references`' answer: the ops for the labels that move, and the labels that left their row.
#[derive(Clone, Debug, Default)]
pub struct Arranged {
    pub ops: Vec<BoardOp>,
    pub off_row: Vec<OffRow>,
}

#[derive(Default)]
struct LabelObstacles {
    boxes: Vec<(BBox, LabelBlocker)>,
    cells: HashMap<(i64, i64), Vec<u32>>,
}

impl LabelObstacles {
    fn insert(&mut self, b: BBox, what: LabelBlocker) {
        if b.is_empty() {
            return;
        }
        let b = b.inflate(GAP);
        let id = self.boxes.len() as u32;
        self.boxes.push((b, what));
        for cell in label_cells(b) {
            self.cells.entry(cell).or_default().push(id);
        }
    }

    /// The first obstacle `b` touches, and the summed intersection area of all of them (boxes
    /// spanning several cells count once).
    fn overlap(&self, b: BBox) -> (Option<LabelBlocker>, i128) {
        if b.is_empty() {
            return (None, 0);
        }
        let mut nearby = Vec::new();
        for cell in label_cells(b) {
            if let Some(ids) = self.cells.get(&cell) {
                nearby.extend_from_slice(ids);
            }
        }
        nearby.sort_unstable();
        nearby.dedup();
        let mut first = None;
        let mut area = 0;
        for id in nearby {
            let (obstacle, what) = self.boxes[id as usize];
            if overlaps(&b, &obstacle) {
                first.get_or_insert(what);
                let w = b.max.x.min(obstacle.max.x) as i128 - b.min.x.max(obstacle.min.x) as i128;
                let h = b.max.y.min(obstacle.max.y) as i128 - b.min.y.max(obstacle.min.y) as i128;
                area += w * h;
            }
        }
        (first, area)
    }
}

/// One reference label's spots: beside its courtyard, two rings out, the side its footprint put it on
/// first, its own orientation first; each of those centred and then slid to either end.
struct LabelSpots<'a> {
    placed: &'a Placed,
    fp: &'a FpText,
    layer: LayerId,
    current: BBox,
    courtyard: BBox,
    size: (Nm, Nm),
    /// The ink size its spots are laid out for: the largest of its row, so a row's labels share one
    /// centre line however long each is.
    extent: (Nm, Nm),
    /// (ring, side: 0 top, 1 bottom, 2 left, 3 right, turn), in order of preference.
    options: Vec<(Nm, usize, Angle)>,
}

const SLIDES: usize = 3;

impl<'a> LabelSpots<'a> {
    fn new(placed: &'a Placed, fp: &'a FpText, n: u8) -> Self {
        let (layer, strokes) = footprint_text(placed, fp, n);
        let direction = placed.to_board(fp.at).sub(placed.at);
        // Stable sorting preserves top, bottom, left, right when their directions tie.
        let mut sides = [(0, direction.y as i64), (1, -(direction.y as i64)), (2, -(direction.x as i64)), (3, direction.x as i64)];
        sides.sort_by_key(|&(_, dot)| -dot);
        let mirror = layer.side() == LayerSide::Back;
        let own_rot = placed.rot.add(if placed.side == Side::Bottom { fp.rot.neg() } else { fp.rot });
        let turn = upright(own_rot, mirror).udeg() % Angle::HALF.udeg();
        let horizontal = turn <= Angle::QUARTER.udeg() / 2 || turn >= 3 * Angle::QUARTER.udeg() / 2;
        let rotations = if horizontal { [Angle::ZERO, Angle::QUARTER] } else { [Angle::QUARTER, Angle::ZERO] };
        let size = ink_size(shown_text(placed, fp), fp.size, fp.thick);
        let mut options = Vec::with_capacity(2 * sides.len() * rotations.len());
        for k in 0..2 {
            for &(side, _) in &sides {
                for rot in rotations {
                    options.push((k, side, rot));
                }
            }
        }
        LabelSpots { placed, fp, layer, current: ink_box(&strokes), courtyard: courtyard_box(placed), size, extent: size, options }
    }

    /// The board box of option `option` slid by `slide`, with its centre and turn.
    fn spot(&self, option: usize, slide: usize) -> (P, Angle, BBox) {
        let (ring, side, rot) = self.options[option];
        let c = self.courtyard;
        let turned = |(w, h): (Nm, Nm)| if rot == Angle::ZERO { (w, h) } else { (h, w) };
        // Laid out for the row's extent, the box is the label's own ink.
        let (ew, eh) = turned(self.extent);
        let distance = GAP + ring * (GAP + self.extent.1);
        let along = |low: Nm, high: Nm, size: Nm| match slide {
            0 => ((low as i64 + high as i64) / 2) as Nm,
            1 => low + size / 2,
            _ => high - size / 2,
        };
        let center = match side {
            0 => P::new(along(c.min.x, c.max.x, ew), c.max.y + distance + eh / 2),
            1 => P::new(along(c.min.x, c.max.x, ew), c.min.y - distance - eh / 2),
            2 => P::new(c.min.x - distance - ew / 2, along(c.min.y, c.max.y, eh)),
            _ => P::new(c.max.x + distance + ew / 2, along(c.min.y, c.max.y, eh)),
        };
        let (width, height) = turned(self.size);
        let b = BBox::new(center.sub(P::new(width / 2, height / 2)), center.add(P::new((width + 1) / 2, (height + 1) / 2)));
        (center, rot, b)
    }

    /// The grid cells this label's spots can reach: its courtyard grown by the farthest ring and the
    /// label's longer side, plus where it is now.
    fn reach(&self) -> BBox {
        let (w, h) = self.extent;
        self.courtyard.inflate(3 * GAP + h.max(w) * 2).union(self.current.inflate(GAP))
    }

    /// Its first clear spot in its order (inside the outline, touching no obstacle), else the spot
    /// inside of least overlap, else its first: a label is never dropped.
    fn clear_spot(&self, obstacles: &LabelObstacles, inside: &dyn Fn(BBox) -> bool) -> (P, Angle, BBox) {
        let mut least: Option<(i128, (P, Angle, BBox))> = None;
        for option in 0..self.options.len() {
            for slide in 0..SLIDES {
                let spot = self.spot(option, slide);
                if !inside(spot.2) { continue; }
                let (hit, overlap) = obstacles.overlap(spot.2);
                if hit.is_none() { return spot; }
                if least.is_none_or(|(l, _)| overlap < l) { least = Some((overlap, spot)); }
            }
        }
        least.map_or_else(|| self.spot(0, 0), |(_, spot)| spot)
    }

    /// The label pose, in the footprint frame, that draws this label centred at `center` with turn `rot`.
    fn pose(&self, center: P, rot: Angle) -> LabelPose {
        let placed = self.placed;
        let at = center.sub(placed.at).rotated(placed.rot.neg());
        LabelPose {
            at: if placed.side == Side::Bottom { P::new(-at.x, at.y) } else { at },
            rot: if placed.side == Side::Bottom { placed.rot.sub(rot) } else { rot.sub(placed.rot) },
            pinned: false,
        }
    }
}

/// The rows of `labels`: labels of the same footprint, turn and side whose parts line up (the same y
/// for a row, the same x for a column) with at most `ROW_GAP` between neighbouring courtyards. A part
/// in no row is a row of its own. Rows come in the order of their first label, labels in order.
fn label_rows(labels: &[(&Placed, &FpText)]) -> Vec<Vec<usize>> {
    fn root(parent: &mut [usize], mut i: usize) -> usize {
        while parent[i] != i {
            parent[i] = parent[parent[i]];
            i = parent[i];
        }
        i
    }
    let mut parent: Vec<usize> = (0..labels.len()).collect();
    let courtyards: Vec<BBox> = labels.iter().map(|(placed, _)| courtyard_box(placed)).collect();
    for axis in 0..2 {
        let mut lines: HashMap<(usize, &str, i32, i64), Vec<usize>> = HashMap::new();
        for (i, &(p, _)) in labels.iter().enumerate() {
            let across = (if axis == 0 { p.at.y } else { p.at.x }) as i64;
            let key = (side_index(p.side), p.footprint.name.as_str(), p.rot.udeg(), (across + ROW_ALIGN / 2).div_euclid(ROW_ALIGN));
            lines.entry(key).or_default().push(i);
        }
        for line in lines.values_mut() {
            let along = |i: usize| if axis == 0 { courtyards[i].min.x } else { courtyards[i].min.y };
            line.sort_by_key(|&i| (along(i), i));
            for pair in line.windows(2) {
                let (a, b) = (courtyards[pair[0]], courtyards[pair[1]]);
                let gap = if axis == 0 { b.min.x - a.max.x } else { b.min.y - a.max.y };
                if gap <= ROW_GAP {
                    let (ra, rb) = (root(&mut parent, pair[0]), root(&mut parent, pair[1]));
                    parent[ra.max(rb)] = ra.min(rb);
                }
            }
        }
    }
    let mut rows: Vec<Vec<usize>> = Vec::new();
    let mut row_of = HashMap::new();
    for i in 0..labels.len() {
        let r = root(&mut parent, i);
        let row = *row_of.entry(r).or_insert_with(|| {
            rows.push(Vec::new());
            rows.len() - 1
        });
        rows[row].push(i);
    }
    rows
}

/// What labels near `near` must clear, by side (`side_index`): the references that stay where they
/// are (`fixed_reference`), every legend when `legends`, courtyards, pads at their mask openings
/// and their drills, vias, holes and silk board texts. A part far from every label is skipped
/// whole, by its footprint's extent. Cost: O(items) boxes; ink is laid out only near a label.
fn label_obstacles(board: &Board, near: &dyn Fn(BBox) -> bool, fixed_reference: &dyn Fn(&Placed) -> bool, legends: bool) -> [LabelObstacles; 2] {
    let n = board.copper_count();
    let mut obstacles = [LabelObstacles::default(), LabelObstacles::default()];
    let mut add = |sides: &[Side], b: BBox, what: LabelBlocker| {
        if near(b) {
            for &side in sides {
                obstacles[side_index(side)].insert(b, what);
            }
        }
    };
    const BOTH: [Side; 2] = [Side::Top, Side::Bottom];
    let margin = board.mask_margin();
    let mut extents: HashMap<*const Footprint, BBox> = HashMap::new();
    for placed in &board.placed {
        let references = placed.footprint.texts.iter().filter(|fp| fp.visible && fp.kind == FpTextKind::Reference && fixed_reference(placed));
        for fp in references.chain(placed.legend.as_ref().filter(|_| legends)) {
            let bound = text_bound(placed.to_board(text_pose(placed, fp).0), shown_text(placed, fp), fp.size, fp.thick);
            if near(bound) {
                let (layer, strokes) = footprint_text(placed, fp, n);
                add(&[label_side(layer)], ink_box(&strokes), LabelBlocker::Label(placed.id));
            }
        }
        // A part far from every label is skipped whole, by its footprint's extent.
        let extent = *extents.entry(Arc::as_ptr(&placed.footprint)).or_insert_with(|| footprint_extent(&placed.footprint, margin));
        if !extent.is_empty() && !near(BBox::of_points(corners(extent).map(|p| placed.to_board(p)))) {
            continue;
        }
        add(&[placed.side], courtyard_box(placed), LabelBlocker::Courtyard(placed.id));
        for i in 0..placed.footprint.pads.len() {
            let pad = PadIx(i as u16);
            for side in BOTH {
                let shape = board.mask_opening(placed.id, pad, side)
                    .or_else(|| placed.pad_shape(pad, LayerId::F_CU.on(side), n));
                if let Some(shape) = shape {
                    add(&[side], shape.bbox(), LabelBlocker::Pad(placed.id));
                }
            }
            if let Some(hole) = placed.drill_hole(pad) {
                add(&BOTH, BBox::point(hole.c).inflate(hole.size.x.max(hole.size.y) / 2), LabelBlocker::Pad(placed.id));
            }
        }
    }
    for via in &board.vias {
        add(&BOTH, BBox::point(via.at).inflate(via.pad.max(via.drill) / 2), LabelBlocker::Via(via.id));
    }
    for hole in &board.holes {
        add(&BOTH, BBox::point(hole.at).inflate(hole.pad.unwrap_or(0).max(hole.drill) / 2), LabelBlocker::Hole(hole.id));
    }
    for text in &board.texts {
        if matches!(text.layer, LayerId::F_SILK | LayerId::B_SILK) {
            let ink = ink_box(&text_strokes(&text.text, text.at, text.rot, text.size, text.thick, text.mirrored));
            add(&[label_side(text.layer)], ink, LabelBlocker::Text(text.id));
        }
    }
    obstacles
}

/// A part's legend `text` (what it does: `TEMPO`) as a footprint text: on the front silkscreen
/// (its part's own side once placed), half again its reference's size, and first offered the side
/// across the part from its reference. `text::place_legends` gives it its spot.
pub fn legend_text(text: &str, footprint: &Footprint) -> FpText {
    let reference = footprint.texts.iter().find(|t| t.kind == FpTextKind::Reference);
    let (size, thick) = reference.map_or((1_000_000, 150_000), |r| (r.size, r.thick));
    let across = reference.map(|r| P::new(-r.at.x, -r.at.y)).filter(|at| *at != P::ZERO).unwrap_or(P::new(0, -1));
    FpText {
        kind: FpTextKind::User, text: text.to_string(), layer: LayerId::F_SILK, at: across, rot: Angle::ZERO,
        size: size * 3 / 2, thick: thick * 3 / 2, visible: true,
    }
}

/// Places every legend (`Placed::legend`) on its part's first clear spot, read level on the board:
/// beside its courtyard, the side its legend asks for first, clear of pads and their drills,
/// courtyards (and so of what comes up through a case over the parts: shafts, pipes, keys), vias,
/// holes, references, silk text and the legends placed before it, in item order. Without a clear
/// spot it takes the one of least overlap: a legend is never dropped. Cost: nothing on a board
/// without legends; else O(items) boxes near them, then a few dozen spot tests a legend.
pub fn place_legends(board: &mut Board) {
    let n = board.copper_count();
    let mut legends: Vec<(usize, FpText)> = board.placed.iter().enumerate().filter_map(|(i, placed)| {
        let mut legend = placed.legend.clone()?;
        legend.rot = if placed.side == Side::Bottom { placed.rot } else { placed.rot.neg() };
        Some((i, legend))
    }).collect();
    if legends.is_empty() {
        return;
    }
    legends.sort_by_key(|(i, _)| board.placed[*i].id);
    let poses: Vec<LabelPose> = {
        // Level spots only: a legend reads level on the board whatever its part's turn.
        let labels: Vec<LabelSpots> = legends.iter().map(|(i, legend)| {
            let mut label = LabelSpots::new(&board.placed[*i], legend, n);
            label.options.retain(|&(_, _, rot)| rot == Angle::ZERO);
            label
        }).collect();
        let mut reach = HashSet::new();
        for label in &labels {
            reach.extend(label_cells(label.reach()));
        }
        let near = |b: BBox| !b.is_empty() && label_cells(b.inflate(GAP)).any(|cell| reach.contains(&cell));
        let mut obstacles = label_obstacles(board, &near, &|_| true, false);
        let outline = board.outline_polyset(10_000);
        let inside = |b: BBox| b.is_empty() || board.outline.is_empty() || corners(b.inflate(GAP)).into_iter().all(|p| outline.contains(p));
        labels.iter().map(|label| {
            let obstacles = &mut obstacles[side_index(label_side(label.layer))];
            let (center, rot, b) = label.clear_spot(obstacles, &inside);
            obstacles.insert(b, LabelBlocker::Label(label.placed.id));
            label.pose(center, rot)
        }).collect()
    };
    for ((i, legend), pose) in legends.into_iter().zip(poses) {
        board.placed[i].legend = Some(FpText { at: pose.at, rot: pose.rot, ..legend });
    }
}

/// The parts of `riding` whose reference label is clear where it rode to: inside the outline and
/// off every pad, courtyard, via, hole and text, and off the labels of parts in neither
/// `requested` nor `riding` (the labels an edit disturbs move round it instead). Cost: one
/// obstacle pass round those labels.
fn ridden_clear(board: &Board, requested: &HashSet<ItemId>, riding: &[ItemId], n: u8) -> HashSet<ItemId> {
    let labels: Vec<LabelSpots> = board.placed.iter().filter(|placed| riding.contains(&placed.id))
        .filter_map(|placed| placed.footprint.texts.iter().find(|fp| fp.visible && fp.kind == FpTextKind::Reference)
            .map(|fp| LabelSpots::new(placed, fp, n)))
        .collect();
    if labels.is_empty() {
        return HashSet::new();
    }
    let reach: HashSet<_> = labels.iter().flat_map(|label| label_cells(label.current.inflate(GAP))).collect();
    let near = |b: BBox| !b.is_empty() && label_cells(b.inflate(GAP)).any(|cell| reach.contains(&cell));
    let stays = |placed: &Placed| !requested.contains(&placed.id) && !riding.contains(&placed.id);
    let obstacles = label_obstacles(board, &near, &stays, true);
    let outline = board.outline_polyset(10_000);
    let inside = |b: BBox| b.is_empty() || board.outline.is_empty() || corners(b.inflate(GAP)).into_iter().all(|p| outline.contains(p));
    labels.iter()
        .filter(|label| inside(label.current) && obstacles[side_index(label_side(label.layer))].overlap(label.current).0.is_none())
        .map(|label| label.placed.id)
        .collect()
}

/// How far from an edited item a part's reference label is looked at again: a few label lengths.
/// An edit's label pass takes the parts this near what it moved (`arrange_references`' `items`).
pub const LABEL_NEAR: Nm = 3_000_000;

/// Re-place the reference labels of the placements `items` (board ids), and of every part in a row
/// with one of them, as `SetField { item, field: Field::Label(FpTextKind::Reference, Some(..)) }` ops
/// for the labels that move. Other parts, pinned labels, locked placements and template or derived
/// placements are left where they are and only stand in the way.
///
/// A row (see `label_rows`) stays where it is while all its labels are clear and drawn alike;
/// otherwise the whole row takes the first side and turn where every label of it has a clear spot,
/// or else the one where most do. A label without a clear spot there is placed on its own and named
/// in `off_row`: its first clear spot beside its courtyard, else the candidate with the least overlap,
/// so a label is never dropped.
///
/// The parts in `riding` were moved by the edit, their labels with them: each such label that
/// lands clear (inside the outline, off every pad, courtyard, via, hole and text, and off the
/// labels of the parts outside `items`) stays where it rode to, out of its row, and the labels
/// the edit disturbs make room round it. One that lands blocked is arranged like the others.
pub fn arrange_references(board: &Board, items: &[ItemId], riding: &[ItemId]) -> Arranged {
    let requested: HashSet<_> = items.iter().copied().collect();
    let n = board.copper_count();
    let rode = ridden_clear(board, &requested, riding, n);
    let mut movable: Vec<(&Placed, &FpText)> = board.placed.iter().filter_map(|placed| {
        if placed.locked || placed.refdes.is_empty() || rode.contains(&placed.id)
            || placed.ref_label.is_some_and(|label| label.pinned)
            || matches!(placed.origin, Origin::Block { detached: false, .. } | Origin::Derived { .. })
        {
            return None;
        }
        placed.footprint.texts.iter().find(|fp| fp.visible && fp.kind == FpTextKind::Reference)
            .map(|fp| (placed, fp))
    }).collect();
    movable.sort_by_key(|(placed, _)| placed.id);
    // Rows from the cheap placement data; the labels' ink only for the rows that take part.
    let rows: Vec<Vec<LabelSpots>> = label_rows(&movable).into_iter()
        .filter(|row| row.iter().any(|&i| requested.contains(&movable[i].0.id)))
        .map(|row| {
            let mut row: Vec<LabelSpots> = row.into_iter().map(|i| LabelSpots::new(movable[i].0, movable[i].1, n)).collect();
            let extent = row.iter().fold((0, 0), |(w, h), label| (w.max(label.size.0), h.max(label.size.1)));
            for label in &mut row {
                label.extent = extent;
            }
            row
        })
        .collect();
    if rows.is_empty() {
        return Arranged::default();
    }
    let participating: HashSet<_> = rows.iter().flatten().map(|label| label.placed.id).collect();
    let outline = board.outline_polyset(10_000);
    let inside = |b: BBox| {
        b.is_empty() || board.outline.is_empty() || corners(b.inflate(GAP)).into_iter().all(|p| outline.contains(p))
    };

    // Obstacles elsewhere than a label's reach cannot touch one of its spots, so arranging the few
    // labels near a dragged part costs the same on a board of any size.
    let mut reach = HashSet::new();
    for label in rows.iter().flatten() {
        reach.extend(label_cells(label.reach()));
    }
    let near = |b: BBox| !b.is_empty() && label_cells(b.inflate(GAP)).any(|cell| reach.contains(&cell));
    let mut obstacles = label_obstacles(board, &near, &|placed| !participating.contains(&placed.id), true);

    // A spot is clear when it is inside the outline and touches neither an obstacle nor a label taken
    // for the same row in this try.
    let blocked = |obstacles: &LabelObstacles, taken: &[(BBox, ItemId)], b: BBox| -> Option<LabelBlocker> {
        if !inside(b) {
            return Some(LabelBlocker::Outline);
        }
        obstacles.overlap(b).0.or_else(|| {
            taken.iter().find(|(t, _)| overlaps(&t.inflate(GAP), &b)).map(|&(_, id)| LabelBlocker::Label(id))
        })
    };
    let mut out = Arranged::default();
    let emit = |out: &mut Arranged, label: &LabelSpots, center: P, rot: Angle| {
        let pose = label.pose(center, rot);
        if label.placed.ref_label != Some(pose) {
            out.ops.push(BoardOp::SetField { item: label.placed.id, field: Field::Label(FpTextKind::Reference, Some(pose)) });
        }
    };
    for row in &rows {
        let first = &row[0];
        let obstacles = &mut obstacles[side_index(label_side(first.layer))];
        let alike = row.iter().all(|label| text_pose(label.placed, label.fp) == text_pose(first.placed, first.fp));
        let mut taken = Vec::new();
        let clear_here = alike && row.iter().all(|label| {
            let clear = blocked(obstacles, &taken, label.current).is_none();
            taken.push((label.current, label.placed.id));
            clear
        });
        if clear_here {
            for (b, id) in taken {
                obstacles.insert(b, LabelBlocker::Label(id));
            }
            continue;
        }

        // The row's side and turn: the first option clear for every label, else the one clear for most.
        let mut best: Option<(usize, Vec<Option<(P, Angle, BBox)>>)> = None;
        for option in 0..first.options.len() {
            let mut taken = Vec::new();
            let picks: Vec<_> = row.iter().map(|label| {
                let pick = (0..SLIDES).map(|slide| label.spot(option, slide)).find(|&(_, _, b)| blocked(obstacles, &taken, b).is_none());
                if let Some((_, _, b)) = pick {
                    taken.push((b, label.placed.id));
                }
                pick
            }).collect();
            let count = picks.iter().flatten().count();
            if count > best.as_ref().map_or(0, |(_, picks)| picks.iter().flatten().count()) {
                best = Some((option, picks));
            }
            if count == row.len() {
                break;
            }
        }
        let (option, picks) = best.unwrap_or_else(|| (0, vec![None; row.len()]));
        for (label, pick) in row.iter().zip(&picks) {
            if let Some((center, rot, b)) = *pick {
                emit(&mut out, label, center, rot);
                obstacles.insert(b, LabelBlocker::Label(label.placed.id));
            }
        }
        for (label, pick) in row.iter().zip(&picks) {
            if pick.is_some() {
                continue;
            }
            // The row's spot is held: by an obstacle or a row-mate's label, both in the grid by now.
            if row.len() > 1 {
                let (_, _, b) = label.spot(option, 0);
                let blocked_by = blocked(obstacles, &[], b).unwrap_or(LabelBlocker::Outline);
                out.off_row.push(OffRow { item: label.placed.id, row: first.placed.id, blocked_by });
            }
            // On its own: the first clear spot in its order, else the least overlap, else where it is.
            let (_, mut least) = obstacles.overlap(label.current);
            let mut chosen = None;
            'search: for option in 0..label.options.len() {
                for slide in 0..SLIDES {
                    let (center, rot, b) = label.spot(option, slide);
                    let (hit, overlap) = obstacles.overlap(b);
                    if hit.is_none() && inside(b) {
                        chosen = Some((center, rot, b));
                        break 'search;
                    }
                    if overlap < least {
                        least = overlap;
                        chosen = Some((center, rot, b));
                    }
                }
            }
            match chosen {
                Some((center, rot, b)) => {
                    emit(&mut out, label, center, rot);
                    obstacles.insert(b, LabelBlocker::Label(label.placed.id));
                }
                None => obstacles.insert(label.current, LabelBlocker::Label(label.placed.id)),
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use fab_elec_core::mm;

    #[test]
    fn placement_value_draws_and_is_additive_content() {
        use fab_elec_core::{canon::{Canon, Writer}, CompId};
        use fab_elec_parts::Footprint;
        use std::sync::Arc;
        let fp = FpText {
            kind: FpTextKind::Value, text: "10k".into(), layer: LayerId::F_SILK,
            at: P::mm(1.0, 2.0), rot: Angle::ZERO, size: mm(1.0), thick: mm(0.15), visible: true,
        };
        let mut placed = Placed {
            id: ItemId(1), comp: CompId(0), path: "R1".into(), refdes: "R1".into(),
            footprint: Arc::new(Footprint { texts: vec![fp.clone()], ..Footprint::default() }),
            pad_nets: Vec::new(), at: P::mm(3.0, 4.0), rot: Angle::ZERO, side: Side::Top,
            locked: false, origin: Origin::Authored, body_z: None, ref_label: None, value_label: None, legend: None, value: None,
        };
        for band in [None, Some((0, mm(1.0)))] {
            placed.body_z = band;
            let mut old = [placed.id.canon_bytes(), placed.comp.canon_bytes(), placed.path.canon_bytes(), placed.refdes.canon_bytes(),
                placed.footprint.canon_bytes(), placed.pad_nets.canon_bytes(), placed.at.canon_bytes(), placed.rot.canon_bytes(),
                placed.side.canon_bytes(), placed.locked.canon_bytes(), placed.origin.canon_bytes()].concat();
            if band.is_some() { old.extend(band.canon_bytes()); }
            assert_eq!(placed.canon_bytes(), old, "None preserves the pre-value canonical form with band {band:?}");
            let hash = placed.content_hash();
            let mut old_hash = Writer::hasher();
            old_hash.raw(&old);
            assert_eq!(hash, old_hash.finish(), "no value or label preserves the old content hash with band {band:?}");
            assert_eq!(footprint_text(&placed, &fp, 2).1,
                text_strokes("10k", placed.to_board(fp.at), Angle::ZERO, fp.size, fp.thick, false),
                "None draws the footprint's Value text with band {band:?}");
            placed.value = Some("4k7".into());
            assert_eq!(footprint_text(&placed, &fp, 2).1,
                text_strokes("4k7", placed.to_board(fp.at), Angle::ZERO, fp.size, fp.thick, false),
                "Some draws the placement's Value text with band {band:?}");
            assert_ne!(placed.content_hash(), hash, "the placement's value is content with band {band:?}");
            for value in [None, Some("4k7".to_owned())] {
                placed.value = value;
                let mut old_with_value = old.clone();
                if let Some(value) = &placed.value {
                    old_with_value.push(2);
                    old_with_value.extend(value.canon_bytes());
                }
                assert_eq!(placed.canon_bytes(), old_with_value, "no label preserves the pre-label canonical bytes with band {band:?} and value {:?}", placed.value);
                let mut old_hash = Writer::hasher();
                old_hash.raw(&old_with_value);
                let old_hash = old_hash.finish();
                assert_eq!(placed.content_hash(), old_hash, "no label preserves the pre-label content hash with band {band:?} and value {:?}", placed.value);
                placed.ref_label = Some(LabelPose { at: P::mm(2.0, 3.0), rot: Angle::QUARTER, pinned: false });
                assert_ne!(placed.content_hash(), old_hash, "the reference label pose changes the content hash with band {band:?} and value {:?}", placed.value);
                assert_eq!(text_pose(&placed, &fp), (fp.at, fp.rot), "a reference label pose never moves Value text");
                placed.ref_label = None;
                let pose = LabelPose { at: P::mm(-1.0, 0.5), rot: Angle::ZERO, pinned: true };
                placed.value_label = Some(pose);
                assert_ne!(placed.content_hash(), old_hash, "the value label pose changes the content hash with band {band:?} and value {:?}", placed.value);
                assert_eq!(text_pose(&placed, &fp), (pose.at, pose.rot), "the value label pose places the Value text");
                placed.value_label = None;
                assert_eq!(placed.content_hash(), old_hash, "clearing the label restores the old content hash with band {band:?} and value {:?}", placed.value);
            }
            placed.value = None;
            assert_eq!(placed.content_hash(), hash, "restoring None restores the original hash with band {band:?}");
            placed.legend = Some(legend_text("GAIN", &placed.footprint));
            assert_ne!(placed.content_hash(), hash, "a legend is content with band {band:?}");
            placed.legend = None;
        }
    }

    #[test]
    fn arranged_labels_sit_clear_beside_their_parts() {
        use fab_elec_core::{BoardId, CompId, Contour, PolySet, SolverId};
        use fab_elec_parts::{Footprint, Pad, PadKind, PadProp, PadShape, PadStack, PasteRule};
        use std::sync::Arc;

        let pads: Vec<_> = [-0.8, 0.8].into_iter().enumerate().map(|(i, x)| Pad {
            no: (i + 1).to_string(), kind: PadKind::Smd,
            stack: PadStack::smd(PadShape::Rect { size: P::mm(0.9, 0.8) }),
            at: P::mm(x, 0.0), rot: Angle::ZERO, drill: None, mask_margin: None,
            paste: PasteRule::default(), thermal: None, zone_connect: None, die_len: 0, prop: PadProp::None,
        }).collect();
        let pad_box = pads.iter().fold(BBox::EMPTY, |b, pad| b.union(pad.stack.front.shape(pad.at, pad.rot).bbox()));
        let fp = FpText {
            kind: FpTextKind::Reference, text: "REF**".into(), layer: LayerId::F_SILK,
            at: P::new(0, pad_box.max.y + mm(0.25) + mm(0.8)), rot: Angle::ZERO,
            size: mm(0.8), thick: mm(0.12), visible: true,
        };
        let footprint = Arc::new(Footprint {
            pads, courtyard: PolySet::rect(pad_box.inflate(mm(0.25))), texts: vec![fp.clone()],
            ..Footprint::default()
        });
        let mut board = Board::new(BoardId(0), "reference labels");
        board.outline.push(Contour::rect(P::ZERO, P::mm(20.0, 12.0), 0));
        for i in 0..8 {
            let id = board.alloc_id();
            let refdes = format!("R{}", i + 1);
            board.placed.push(Placed {
                id, comp: CompId(i), path: refdes.clone(), refdes, footprint: footprint.clone(),
                pad_nets: vec![None; 2], at: P::mm(3.0 + 2.0 * i as f64, 6.0),
                rot: Angle::QUARTER, side: Side::Top, locked: false,
                origin: Origin::Solved { by: SolverId(0) }, body_z: None, value: None, ref_label: None, value_label: None, legend: None,
            });
        }
        let n = board.copper_count();
        let label_box = |placed: &Placed| ink_box(&footprint_text(placed, &fp, n).1);
        assert!(board.placed.iter().any(|placed| {
            let b = label_box(placed);
            board.placed.iter().any(|other| other.id != placed.id && (
                b.intersects(&courtyard_box(other)) || (0..other.footprint.pads.len()).any(|i| {
                    b.intersects(&board.mask_opening(other.id, PadIx(i as u16), Side::Top).expect("row pad has a mask opening").bbox())
                })
            ))
        }), "the original rotated labels overlap a neighbour's pads or courtyard");

        let r1_label = label_box(&board.placed[0]);
        assert!(board.placed[1..].iter().all(|other| !r1_label.intersects(&courtyard_box(other))) && !r1_label.intersects(&courtyard_box(&board.placed[0])),
            "R1, the row's end, starts with a clear label of its own");
        let items: Vec<_> = board.placed.iter().map(|placed| placed.id).rev().collect();
        let arranged = arrange_references(&board, &items, &[]);
        assert!(!arranged.ops.is_empty(), "arranging moves the overlapping row labels");
        assert!(arranged.off_row.is_empty(), "every label of the row has a clear spot on the row's side: {:?}", arranged.off_row);
        board.apply_all(&arranged.ops).expect("a reference pose ops apply to the row");
        let poses: Vec<_> = board.placed.iter().map(|placed| placed.ref_label).collect();
        assert!(poses.iter().all(|pose| pose.is_some() && *pose == poses[0]),
            "the row reads as one row: every label on the same side and turn, R1's clear one included: {poses:?}");
        // The first ring, where it is clear: the row's widest label sits a label gap from its
        // courtyard (the second ring is a label's height further out).
        let apart = |b: BBox, c: BBox| (c.min.x - b.max.x).max(b.min.x - c.max.x).max(c.min.y - b.max.y).max(b.min.y - c.max.y);
        let nearest = board.placed.iter().map(|placed| apart(label_box(placed), courtyard_box(placed))).min().expect("the row has parts");
        assert!(nearest < 2 * GAP, "the row's labels take the first ring, its widest label {nearest} nm from its courtyard");
        let outline = board.outline_polyset(10_000);
        for placed in &board.placed {
            let b = label_box(placed);
            assert!(!b.is_empty(), "{} retains its reference ink", placed.refdes);
            assert_eq!(placed.origin, Origin::Solved { by: SolverId(0) }, "{}'s cosmetic label edit preserves its solved origin", placed.refdes);
            assert!(corners(b.inflate(GAP)).into_iter().all(|p| outline.contains(p)), "{}'s label stays inside the outline with clearance", placed.refdes);
            for other in &board.placed {
                assert!(!b.intersects(&courtyard_box(other)), "{}'s label clears {}'s courtyard, including its own", placed.refdes, other.refdes);
                for i in 0..other.footprint.pads.len() {
                    let opening = board.mask_opening(other.id, PadIx(i as u16), Side::Top).expect("row pad has a mask opening");
                    assert!(!b.intersects(&opening.bbox()), "{}'s label clears {}'s pad {i}", placed.refdes, other.refdes);
                }
                if other.id != placed.id {
                    assert!(!b.intersects(&label_box(other)), "{}'s label clears {}'s label", placed.refdes, other.refdes);
                }
            }
        }
        assert!(arrange_references(&board, &items, &[]).ops.is_empty(), "arranging the arranged board is stable");

        // Legends on three parts of the row: each on the front silk, inside the outline, read level
        // although its part is turned, clear of every courtyard, pad, reference and other legend;
        // the arranged references keep their spots around them, and a legend moves with its part.
        let mut marked = board.clone();
        for (i, text) in [(0, "GAIN"), (3, "TONE"), (7, "LEVEL")] {
            marked.placed[i].legend = Some(legend_text(text, &marked.placed[i].footprint));
        }
        place_legends(&mut marked);
        let legend_box = |placed: &Placed| placed.legend.as_ref().map(|legend| ink_box(&footprint_text(placed, legend, n).1));
        for placed in marked.placed.iter().filter(|placed| placed.legend.is_some()) {
            let legend = placed.legend.as_ref().expect("a legend is kept");
            let (layer, strokes) = footprint_text(placed, legend, n);
            assert_eq!(layer, LayerId::F_SILK, "{}'s legend is on the front silk", legend.text);
            assert_eq!(upright(placed.rot.add(legend.rot), false), Angle::ZERO, "{} reads level on the turned part", legend.text);
            let b = ink_box(&strokes);
            assert!(!b.is_empty() && corners(b.inflate(GAP)).into_iter().all(|p| outline.contains(p)), "{} is inside the outline with clearance", legend.text);
            for other in &marked.placed {
                assert!(!b.intersects(&courtyard_box(other)), "{} clears {}'s courtyard", legend.text, other.refdes);
                assert!(!b.intersects(&label_box(other)), "{} clears {}'s reference", legend.text, other.refdes);
                for i in 0..other.footprint.pads.len() {
                    let opening = board.mask_opening(other.id, PadIx(i as u16), Side::Top).expect("row pad has a mask opening");
                    assert!(!b.intersects(&opening.bbox()), "{} clears {}'s pad {i}", legend.text, other.refdes);
                }
                if other.id != placed.id {
                    assert!(legend_box(other).is_none_or(|o| !b.inflate(GAP).intersects(&o)), "{} clears {}'s legend", legend.text, other.refdes);
                }
            }
        }
        assert!(arrange_references(&marked, &items, &[]).ops.is_empty(), "the arranged references keep their spots around the legends");
        let before = legend_box(&marked.placed[3]).expect("TONE has ink");
        marked.apply(&BoardOp::Move { item: marked.placed[3].id, by: P::mm(0.5, 1.0) }).expect("the part moves");
        let after = legend_box(&marked.placed[3]).expect("TONE has ink");
        assert_eq!(after.min.sub(before.min), P::mm(0.5, 1.0), "a legend moves with its part");

        // Vias on every first spot of R5: the row keeps the side and turn the rest of it can take,
        // and R5 alone leaves it, named with what holds its spot.
        let mut held = board.clone();
        let r5 = LabelSpots::new(&board.placed[4], &fp, n);
        for option in 0..r5.options.len() {
            let id = held.alloc_id();
            held.apply(&BoardOp::Add(crate::Item::Via(crate::Via {
                id, net: crate::NetId(0), at: r5.spot(option, 0).2.center(), drill: mm(0.3), pad: mm(0.6),
                span: (LayerId::F_CU, LayerId::B_CU), kind: crate::ViaKind::Through, tent: crate::Tent::Both,
                backdrill: None, locked: false, origin: Origin::Authored,
                flash: crate::Flash::All,
            }))).expect("a via on R5's spot applies");
        }
        let arranged = arrange_references(&held, &items, &[]);
        assert!(matches!(arranged.off_row.as_slice(), [OffRow { item, row, blocked_by: LabelBlocker::Via(_) }]
            if *item == board.placed[4].id && *row == board.placed[0].id), "R5 alone leaves its row, held by a via: {:?}", arranged.off_row);
        held.apply_all(&arranged.ops).expect("a reference pose ops apply to the held row");
        let poses: Vec<_> = held.placed.iter().map(|placed| placed.ref_label).collect();
        assert!(poses.iter().enumerate().all(|(i, pose)| i == 4 || *pose == poses[0]), "the rest of the row stays one row: {poses:?}");
        assert!(poses[4].is_some(), "R5 keeps a label: {poses:?}");

        // A dragged part's label rides with it. Two rows, their labels on opposite sides; R4 dragged
        // from the first onto the end of the second. Its label, clear where it rode to, stays there,
        // out of the row it joined, and that row keeps its labels: the drop moves nothing else. Key A,
        // which rides nothing, takes it into that row. A ridden label that lands on a via is arranged
        // like the others.
        let mut two = Board::new(BoardId(1), "two rows");
        two.outline.push(Contour::rect(P::ZERO, P::mm(24.0, 20.0), 0));
        for i in 0..8 {
            let id = two.alloc_id();
            let refdes = format!("R{}", i + 1);
            two.placed.push(Placed {
                id, comp: CompId(i), path: refdes.clone(), refdes, footprint: footprint.clone(),
                pad_nets: vec![None; 2], at: P::mm(3.0 + 2.0 * (i % 4) as f64, 6.0 + 8.0 * (i / 4) as f64),
                rot: Angle::QUARTER, side: Side::Top, locked: false,
                origin: Origin::Solved { by: SolverId(0) }, body_z: None, value: None, ref_label: None, value_label: None, legend: None,
            });
        }
        for row in [0..4, 4..8] {
            // The first row's labels below its parts, the second row's above, a ring out, each row
            // laid out for its widest label as the arranger lays a row out.
            let side = if row.start == 0 { 1 } else { 0 };
            let extent = row.clone().map(|i| LabelSpots::new(&two.placed[i], &fp, n).size).fold((0, 0), |(w, h), (a, b)| (w.max(a), h.max(b)));
            for i in row {
                let pose = {
                    let mut spots = LabelSpots::new(&two.placed[i], &fp, n);
                    spots.extent = extent;
                    let option = (0..spots.options.len()).find(|&o| spots.options[o].0 == 1 && spots.options[o].1 == side).expect("a second-ring spot on each side");
                    let (center, rot, _) = spots.spot(option, 0);
                    spots.pose(center, rot)
                };
                two.placed[i].ref_label = Some(pose);
            }
        }
        let all: Vec<_> = two.placed.iter().map(|placed| placed.id).collect();
        let start = arrange_references(&two, &all, &[]);
        assert!(start.ops.is_empty(), "the two rows start arranged, their labels on opposite sides: {:?}", start.ops);
        let r4 = two.placed[3].id;
        let mut dropped = two.clone();
        dropped.apply(&BoardOp::Move { item: r4, by: P::mm(2.0, 8.0) }).expect("R4 moves to the end of the second row");
        let ridden = label_box(&dropped.placed[3]);
        assert!(dropped.placed.iter().all(|other| !ridden.intersects(&courtyard_box(other)) && (other.id == r4 || !ridden.intersects(&label_box(other)))),
            "R4's label lands clear where it rode to");
        let arranged = arrange_references(&dropped, &all, &[r4]);
        assert!(arranged.ops.is_empty() && arranged.off_row.is_empty(),
            "R4's label stays where it rode to and the row R4 joined keeps its labels: {:?}", arranged.ops);
        assert!(arrange_references(&dropped, &all, &[]).ops.iter().any(|op| matches!(op, BoardOp::SetField { .. })),
            "key A, which rides nothing, arranges R4 into the row it joined");
        let via = dropped.alloc_id();
        dropped.apply(&BoardOp::Add(crate::Item::Via(crate::Via {
            id: via, net: crate::NetId(0), at: ridden.center(), drill: mm(0.3), pad: mm(0.6),
            span: (LayerId::F_CU, LayerId::B_CU), kind: crate::ViaKind::Through, tent: crate::Tent::Both,
            backdrill: None, locked: false, origin: Origin::Authored, flash: crate::Flash::All,
        }))).expect("a via where R4's label rode to applies");
        let arranged = arrange_references(&dropped, &all, &[r4]);
        assert!(arranged.ops.iter().any(|op| matches!(op, BoardOp::SetField { item, .. } if *item == r4)), "R4's label, blocked where it rode to, is arranged");
        dropped.apply_all(&arranged.ops).expect("a reference pose ops apply");
        let via_box = BBox::new(ridden.center().sub(P::new(mm(0.3), mm(0.3))), ridden.center().add(P::new(mm(0.3), mm(0.3))));
        assert!(!label_box(&dropped.placed[3]).intersects(&via_box), "R4's arranged label clears the via");

        let placed = &mut board.placed[0];
        let pinned_id = placed.id;
        // Put it inside its own courtyard, where an unpinned label would have to move.
        placed.ref_label = Some(LabelPose { at: P::ZERO, rot: Angle::ZERO, pinned: true });
        let ops = arrange_references(&board, &items, &[]).ops;
        assert!(!ops.iter().any(|op| matches!(op, BoardOp::SetField { item, .. } if *item == pinned_id)), "arranging never moves a pinned label");
        assert!(ops.is_empty(), "a pinned label inside its courtyard leaves the other arranged labels stable");
    }

    #[test]
    fn text_bound_contains_strokes() {
        let font = StrokeFont::bundled("technical").unwrap();
        let texts = font.chars().filter(|c| !c.is_control()).map(|c| c.to_string().repeat(8))
            .chain(["WMW@%".into(), "R1\nC22".into()]);
        let at = P::mm(10.0, 10.0);
        for text in texts {
            for rot in [0.0, 37.0, 90.0] {
                for mirror in [false, true] {
                    for thick in [mm(0.15), mm(1.0)] {
                        let bound = text_bound(at, &text, mm(1.0), thick);
                        let strokes = text_strokes(&text, at, Angle::deg(rot), mm(1.0), thick, mirror);
                        let ink = strokes.iter().fold(BBox::EMPTY, |b, s| b.union(s.bbox()));
                        assert!(
                            ink.is_empty() || (bound.contains(ink.min) && bound.contains(ink.max)),
                            "text {text:?}, rot {rot}, mirror {mirror}, thick {thick}: strokes fit the bound"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn text_reads_upright() {
        let strokes = |deg: f64, mirror: bool| text_strokes("R1", P::mm(10.0, 10.0), Angle::deg(deg), mm(1.0), mm(0.15), mirror);
        for (deg, reads_as) in [(180.0, 0.0), (270.0, 90.0), (-90.0, 90.0), (135.0, -45.0)] {
            assert_eq!(strokes(deg, false), strokes(reads_as, false), "text at {deg} deg is kept upright, drawn as at {reads_as} deg");
        }
        assert_eq!(strokes(90.0, true), strokes(270.0, true), "mirrored text at 90 deg reads from the back's right, as at 270 deg");
        assert!(matches!(strokes(90.0, false).first(), Some(Shape::Segment { a, .. }) if a.y < mm(10.0)),
            "vertical text reads from the right: at 90 deg its first letter is at the bottom");
    }
}
