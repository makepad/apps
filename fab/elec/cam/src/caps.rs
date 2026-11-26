//! Factory limits and the board.
//!
//! One definition of legal: a factory's track, clearance, drill and annular minima reach the
//! board only as rules (`apply_capabilities`), which DRC checks like any other rule; there is
//! no second checker for items here. `apply_capabilities` is derived (the host's rules stage
//! resolves `resolve_rules(apply_capabilities(board.rules, caps))`) and never stored in the
//! board: a later, laxer factory must give the design's own values back.
//!
//! What DRC has no rule for stays here as `board_limits`: board size, copper layer count and
//! thickness, each a `Violation` with a stable id, the cause and the factory's nearest offer.

use crate::profile::Capabilities;
use fab_elec_board::Board;
use fab_elec_circuit::{CheckKind, Measure, QtyUnit, Quantity, Violation};
use fab_elec_core::{to_mm, BoardId, FeatureRef, Nm, Sel, Severity};

/// The factory's minima applied to the design rules: derived, never stored (in the board crate:
/// rule minima are a board concept).
pub use fab_elec_board::apply_capabilities;

fn count(n: u8) -> Measure {
    Measure::Qty(Quantity { si: f64::from(n), unit: QtyUnit::Count })
}

fn nearest(have: Nm, offered: impl IntoIterator<Item = Nm>) -> Nm {
    offered.into_iter().min_by_key(|&v| ((i64::from(have) - i64::from(v)).abs(), v)).unwrap_or(0)
}

/// What the board asks that the profile's factory cannot make and DRC has no rule for: size,
/// copper layer count, thickness. At most one violation per limit, ordered by rule name.
/// `rule` names the profile (`factory:<profile>`), so the profile form's list is the Checks
/// list filtered by it. Cost: O(outline segments).
pub fn board_limits(board: &Board, caps: &Capabilities, profile: &str, unit: BoardId) -> Vec<Violation> {
    let rule = format!("factory:{profile}");
    let mut out = Vec::new();
    let mut push = |kind: &str, message: String, hint: String, measure: Option<(Measure, Measure)>, items: Vec<Sel>| {
        let v = Violation::new(CheckKind::Custom(format!("factory.{kind}")), Severity::Error, Some(unit), items, &format!("{rule}:{kind}"), message).hint(hint);
        out.push(match measure {
            Some((have, limit)) => v.measures(have, limit),
            None => v,
        });
    };
    let outline = vec![Sel::Feature(FeatureRef::Outline(unit))];
    let bbox = board.outline_bbox();
    for (what, have, min, max) in [("height", bbox.height(), caps.board_min.1, caps.board_max.1), ("width", bbox.width(), caps.board_min.0, caps.board_max.0)] {
        let (limit, side) = if have < i64::from(min) { (min, "below the minimum") } else if have > i64::from(max) { (max, "above the maximum") } else { continue };
        push(
            &format!("board_{what}"),
            format!("board {what} {:.3} mm is {side} of {profile}: {:.3} mm", have as f64 / 1e6, to_mm(limit)),
            format!("resize the board to {:.3} mm {what} or choose another factory", to_mm(limit)),
            Some((Measure::Len(have.min(i64::from(Nm::MAX)) as Nm), Measure::Len(limit))),
            outline.clone(),
        );
    }
    let layers = board.copper_count();
    if !caps.layers.contains(&layers) {
        let near = nearest(Nm::from(layers), caps.layers.iter().copied().map(Nm::from));
        push(
            "layers",
            format!("{layers} copper layers; {profile} offers {:?}", caps.layers),
            format!("use {near} copper layers or choose another factory"),
            // a count is a count, not a length
            Some((count(layers), count(near as u8))),
            vec![Sel::Unit(unit)],
        );
    }
    let t = board.stackup.thickness;
    let within = |nominal: Nm| (i64::from(t) - i64::from(nominal)).abs() * 1_000_000 <= i64::from(nominal) * i64::from(caps.thickness_tol_ppm);
    if !caps.thickness.iter().any(|&n| within(n)) {
        let near = nearest(t, caps.thickness.iter().copied());
        push(
            "thickness",
            format!("board thickness {:.3} mm is not one {profile} offers ({} listed)", to_mm(t), caps.thickness.len()),
            format!("set the stackup to {:.3} mm or choose another factory", to_mm(near)),
            Some((Measure::Len(t), Measure::Len(near))),
            vec![Sel::Unit(unit)],
        );
    }
    out.sort_by(|a, b| a.rule.cmp(&b.rule));
    out
}

/// A factory minimum as shown beside an authored value. Keep all significant nanometres.
pub fn limit_text(nm: Nm) -> String {
    if nm % 10_000 == 0 { format!("{:.2} mm", to_mm(nm)) } else { format!("{} mm", to_mm(nm)) }
}

/// Only a base minimum the person explicitly wrote can be finer than the house.
pub fn raised_rules(board: &Board, caps: &Capabilities, profile: &str) -> Vec<Violation> {
    let base = &board.rules.base;
    let mut out = Vec::new();
    for (rule, authored, limit) in [
        ("track", base.track, caps.min_track), ("clearance", base.clearance, caps.min_clearance),
        ("drill", base.drill, caps.min_drill), ("annular", base.annular, caps.min_annular),
    ] {
        let Some(have) = authored.filter(|&v| v < limit) else { continue };
        let floor = limit_text(limit);
        out.push(Violation::new(CheckKind::Custom(format!("factory.{rule}")), Severity::Warning,
            Some(board.unit), vec![Sel::Unit(board.unit)], &format!("factory:{profile}:{rule}"),
            format!("{rule} {} is finer than {profile}'s {floor}; the checks use {floor}", limit_text(have)))
            .measures(Measure::Len(have), Measure::Len(limit))
            .hint(format!("raise it to {floor}, or pick a finer house or tier")));
    }
    out
}
