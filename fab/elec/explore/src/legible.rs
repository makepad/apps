//! Part references and optional values re-placed for a screen text floor, in world units.

use crate::labels::{place_labels, LabelOpts, LabelReq, Obstacle, Spot};
use crate::sheet::{self, Owner, SheetLayout, TextRole, LINE_GAP, TEXT_REFDES, TEXT_VALUE};

/// One part label re-placed for text `scale` times its layout size (world units, Y up).
#[derive(Clone, Debug, PartialEq)]
pub struct Legible {
    /// Index into `SheetLayout::labels` (an `Owner::Part` label).
    pub label: usize,
    /// The refdes line's box; None when no spot beside its own part is free at this scale (a
    /// reference is never drawn away from its part: zooming in shows it).
    pub refdes: Option<[f64; 4]>,
    /// The value line's box; None when the label has no value or it does not fit beside its shown
    /// reference.
    pub value: Option<[f64; 4]>,
}

/// Every part label of `sheet`, re-placed with its text `scale` (>= 1) times its layout size:
/// beside its own part, nearer it than any other part, inside the sheet's bounds, or not at all.
/// Pure and deterministic. Cost: three `place_labels` calls over the sheet's part labels (the
/// second over those with no free spot beside their part) and obstacles, O(n log n + m + n * k)
/// under the placer's grid occupancy assumptions, and nearness checks over boxes sorted by their
/// left edges.
pub fn legible_labels(sheet: &SheetLayout, scale: f64) -> Vec<Legible> {
    let parts: Vec<_> = sheet.labels.iter().enumerate().filter_map(|(i, label)| {
        match label.owner {
            Owner::Part(n) => Some((i, n, label)),
            _ => None,
        }
    }).collect();
    if scale <= 1.0 {
        return parts.iter().map(|&(i, _, label)| Legible {
            label: i,
            refdes: Some(line_box(&label.lines[0].0, label.bx[0], label.bx[3], TEXT_REFDES)),
            value: label.lines.get(1).filter(|(_, role)| *role == TextRole::Value).map(|(value, _)| {
                line_box(value, label.bx[0], label.bx[3] - TEXT_REFDES - LINE_GAP * TEXT_VALUE, TEXT_VALUE)
            }),
        }).collect();
    }

    let mut obstacles: Vec<_> = sheet.parts.iter().enumerate().map(|(n, part)| {
        Obstacle::Box { b: part.body.map(f64::from), owner: n as u32 }
    }).collect();
    for run in sheet.wires.iter().chain(&sheet.buses) {
        for segment in run.pts.windows(2) {
            obstacles.push(Obstacle::Seg {
                a: segment[0].map(f64::from), b: segment[1].map(f64::from), half: 0.15, owner: u32::MAX,
            });
        }
    }
    for tag in &sheet.tags {
        obstacles.push(Obstacle::Box { b: sheet::tag_item_box(tag), owner: u32::MAX });
    }
    for frame in &sheet.frames {
        let [left, bottom, right, top] = frame.rect.map(f64::from);
        let corners = [[left, bottom], [right, bottom], [right, top], [left, top], [left, bottom]];
        for edge in corners.windows(2) {
            obstacles.push(Obstacle::Seg { a: edge[0], b: edge[1], half: 0.05, owner: u32::MAX });
        }
    }
    for label in &sheet.labels {
        if !matches!(label.owner, Owner::Part(_)) {
            obstacles.push(Obstacle::Box { b: label.bx, owner: u32::MAX });
        }
    }

    let opts = LabelOpts { gap: 0.25 * scale, step: 0.5 * scale };
    // The sheet's bounds hold every label: strips round them, wide enough that no box beside a
    // part inside can clear them, keep the fitted view from cutting a label off.
    let widest = parts.iter().flat_map(|&(_, _, label)| &label.lines)
        .map(|(text, _)| sheet::text_w(text, TEXT_REFDES * scale)).fold(0.0, f64::max);
    let reach = widest + TEXT_REFDES * scale + 2.0 * opts.gap;
    let [left, bottom, right, top] = sheet.bounds.map(f64::from);
    for b in [[left - reach, bottom - reach, left, top + reach], [right, bottom - reach, right + reach, top + reach],
        [left, bottom - reach, right, bottom], [left, top, right, top + reach]] {
        obstacles.push(Obstacle::Box { b, owner: u32::MAX });
    }
    let refs: Vec<_> = parts.iter().map(|&(_, owner, label)| {
        let anchor = sheet.parts[owner as usize].body.map(f64::from);
        let first = if label.bx[1] >= anchor[3] { Spot::AboveLeft }
            else if label.bx[3] <= anchor[1] { Spot::BelowLeft }
            else if label.bx[0] >= anchor[2] { Spot::RightTop }
            else if label.bx[2] <= anchor[0] { Spot::LeftTop }
            else { Spot::AboveLeft };
        let mut spots = vec![first];
        for spot in [Spot::AboveLeft, Spot::Above, Spot::RightTop, Spot::Right,
            Spot::LeftTop, Spot::Left, Spot::BelowLeft, Spot::Below] {
            if !spots.contains(&spot) { spots.push(spot); }
        }
        LabelReq {
            anchor, size: [sheet::text_w(&label.lines[0].0, TEXT_REFDES * scale), TEXT_REFDES * scale],
            spots, priority: 0, owner, optional: true,
        }
    }).collect();
    let placed = place_labels(&refs, &obstacles, &opts);
    // A reference slid next to another part reads as that part's: shown only while it is
    // strictly nearer its own part than any other.
    let bodies = Near::new(sheet.parts.iter().enumerate()
        .filter(|(_, part)| part.placed).map(|(n, part)| (n, part.body.map(f64::from))).collect());
    let own = |b: [f64; 4], owner: u32| box_distance(b, sheet.parts[owner as usize].body.map(f64::from));
    let mut out: Vec<_> = parts.iter().zip(&placed.boxes).zip(&placed.left_out).map(|((&(label, owner, _), &b), &out)| {
        let shown = !out && !bodies.nearer(b, owner as usize, own(b, owner), false);
        Legible { label, refdes: shown.then_some(b), value: None }
    }).collect();
    obstacles.extend(out.iter().filter_map(|o| o.refdes).map(|b| Obstacle::Box { b, owner: u32::MAX }));
    // A reference with no free spot beside its part (a part framed tight in its own block) takes
    // the nearest free place further out, shown only where it still reads as its part's with no
    // leader (none is drawn while text is enlarged): inside the sheet, every other part at least
    // twice as far as its own.
    let rest: Vec<usize> = (0..out.len()).filter(|&i| out[i].refdes.is_none()).collect();
    if !rest.is_empty() {
        let reqs: Vec<_> = rest.iter().map(|&i| LabelReq { optional: false, ..refs[i].clone() }).collect();
        let placed = place_labels(&reqs, &obstacles, &opts);
        for (&i, &b) in rest.iter().zip(&placed.boxes) {
            let owner = parts[i].1;
            let inside = b[0] >= left && b[1] >= bottom && b[2] <= right && b[3] <= top;
            if inside && !bodies.nearer(b, owner as usize, 2.0 * own(b, owner), false) {
                out[i].refdes = Some(b);
                obstacles.push(Obstacle::Box { b, owner: u32::MAX });
            }
        }
    }

    let mut values = Vec::new();
    let mut value_labels = Vec::new();
    for (i, &(_, _, label)) in parts.iter().enumerate() {
        let Some((value, TextRole::Value)) = label.lines.get(1) else { continue };
        let Some(anchor) = out[i].refdes else { continue };
        values.push(LabelReq {
            anchor,
            size: [sheet::text_w(value, TEXT_VALUE * scale), TEXT_VALUE * scale],
            spots: vec![Spot::BelowLeft, Spot::AboveLeft, Spot::RightTop],
            priority: 0, owner: u32::MAX - 1 - i as u32, optional: true,
        });
        value_labels.push(i);
    }
    let placed = place_labels(&values, &obstacles, &opts);
    for (i, &label) in value_labels.iter().enumerate() {
        if !placed.left_out[i] { out[label].value = Some(placed.boxes[i]); }
    }
    // A value slid next to another part's reference reads as that part's: shown only while it is
    // nearer its own reference than any other.
    let shown = Near::new(out.iter().enumerate().filter_map(|(i, o)| Some((i, o.refdes?))).collect());
    for i in 0..out.len() {
        let (Some(value), Some(refdes)) = (out[i].value, out[i].refdes) else { continue };
        if shown.nearer(value, i, box_distance(value, refdes), true) {
            out[i].value = None;
        }
    }
    out
}

/// Boxes with their ids, sorted by left edge once, for nearness questions.
struct Near {
    boxes: Vec<(usize, [f64; 4])>,
    widest: f64,
}

impl Near {
    fn new(mut boxes: Vec<(usize, [f64; 4])>) -> Self {
        boxes.sort_by(|a, b| a.1[0].total_cmp(&b.1[0]));
        let widest = boxes.iter().map(|(_, b)| b[2] - b[0]).fold(0.0, f64::max);
        Self { boxes, widest }
    }

    /// Whether a box other than `own_id`'s is nearer `b` than `own` (or as near, with `ties`).
    /// A box that near starts within `own` plus the widest box left of `b`: binary search, then
    /// the boxes up to `own` right of it.
    fn nearer(&self, b: [f64; 4], own_id: usize, own: f64, ties: bool) -> bool {
        let from = self.boxes.partition_point(|(_, o)| o[0] < b[0] - own - self.widest);
        self.boxes[from..].iter().take_while(|(_, o)| o[0] <= b[2] + own).any(|&(id, o)| {
            let d = box_distance(b, o);
            id != own_id && if ties { d <= own } else { d < own }
        })
    }
}

/// The gap between two boxes (zero when they touch or overlap).
pub fn box_distance(a: [f64; 4], b: [f64; 4]) -> f64 {
    let dx = (a[0] - b[2]).max(b[0] - a[2]).max(0.0);
    let dy = (a[1] - b[3]).max(b[1] - a[3]).max(0.0);
    dx.hypot(dy)
}

fn line_box(text: &str, left: f64, top: f64, height: f64) -> [f64; 4] {
    [left, top - height, left + sheet::text_w(text, height), top]
}
