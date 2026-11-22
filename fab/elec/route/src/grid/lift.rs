//! Lift grid search paths into track runs and vias, retaining true endpoint stubs.
//! The grid is only a search aid: the resulting copper geometry is what the verifier checks.
//! Straight stretches and consecutive layer changes are coalesced in O(cells) time.

use super::{search::GridPath, GridSpec};
use fab_elec_core::{Path, Seg, P};

/// One run of track on one layer, from its first point to its last.
pub struct Run {
    /// Layer index in the grid specification.
    pub li: usize,
    /// Track centre line, containing only nonzero straight segments.
    pub path: Path,
}

/// A lifted route with runs and layer changes in path order.
pub struct Lifted {
    /// Nonzero track runs in path order.
    pub runs: Vec<Run>,
    /// Via centre and the two layer indices it joins, lower index first.
    pub vias: Vec<(P, usize, usize)>,
}

/// Lift a valid grid path, adding stubs to its true endpoints and dropping zero-length runs.
/// `start` and `end` are nonempty attach polylines from the terminal toward its cell, each
/// containing the attach point and an optional knee. The end polyline is traversed in reverse.
/// Consecutive layer changes at one cell become one via joining the first and last layers.
pub fn lift(spec: &GridSpec, path: &GridPath, start: &[P], end: &[P]) -> Lifted {
    let mut lifted = Lifted { runs: Vec::new(), vias: Vec::new() };
    let Some(&(mut li, first)) = path.cells.first() else {
        return lifted;
    };
    let mut run = Path::new(start[0]);
    for &point in &start[1..] {
        push_point(&mut run, point);
    }
    push_point(&mut run, spec.centre(first));
    let mut index = 1;
    while index < path.cells.len() {
        let (next_li, cell) = path.cells[index];
        let centre = spec.centre(cell);
        if next_li == li {
            push_point(&mut run, centre);
            index += 1;
            continue;
        }

        if !run.segs.is_empty() {
            lifted.runs.push(Run { li, path: run });
        }
        let from_li = li;
        li = next_li;
        index += 1;
        while index < path.cells.len() && path.cells[index].1 == cell && path.cells[index].0 != li {
            li = path.cells[index].0;
            index += 1;
        }
        lifted.vias.push((centre, from_li.min(li), from_li.max(li)));
        run = Path::new(centre);
    }
    for &point in end.iter().rev() {
        push_point(&mut run, point);
    }
    if !run.segs.is_empty() {
        lifted.runs.push(Run { li, path: run });
    }
    lifted
}

fn push_point(path: &mut Path, to: P) {
    let at = path.end();
    if to == at {
        return;
    }
    let count = path.segs.len();
    if count > 0 {
        let from = if count == 1 { path.start } else { path.segs[count - 2].end() };
        if same_direction(from, at, to) {
            path.segs[count - 1] = Seg::Line(to);
            return;
        }
    }
    path.segs.push(Seg::Line(to));
}

fn same_direction(a: P, b: P, c: P) -> bool {
    let abx = i64::from(b.x) - i64::from(a.x);
    let aby = i64::from(b.y) - i64::from(a.y);
    let bcx = i64::from(c.x) - i64::from(b.x);
    let bcy = i64::from(c.y) - i64::from(b.y);
    // With collinear, forward steps between i32 points, every product and the dot sum fit i64.
    // An overflow therefore rules out a merge, including for arbitrary endpoint stubs.
    let cross = abx.checked_mul(bcy).and_then(|x| aby.checked_mul(bcx).and_then(|y| x.checked_sub(y)));
    let dot = abx.checked_mul(bcx).and_then(|x| aby.checked_mul(bcy).and_then(|y| x.checked_add(y)));
    cross == Some(0) && dot.is_some_and(|value| value > 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grid::Cell;
    use fab_elec_core::LayerId;

    #[test]
    fn lift_shapes() {
        let spec = GridSpec {
            origin: P::ZERO,
            pitch: 100_000,
            cols: 50,
            rows: 50,
            layers: vec![LayerId::F_CU, LayerId::B_CU],
        };
        let grid = |cells| GridPath { cells, cost: 0 };
        let shapes = [
            (
                "straight row",
                grid((2..12).map(|x| (0, Cell { x, y: 2 })).collect()),
                1,
            ),
            (
                "L bend",
                grid(
                    (2..=7).map(|x| (0, Cell { x, y: 2 }))
                        .chain((3..=7).map(|y| (0, Cell { x: 7, y })))
                        .collect(),
                ),
                2,
            ),
            (
                "diagonal dog-leg",
                grid(
                    (2..=5).map(|x| (0, Cell { x, y: 2 }))
                        .chain((1..=3).map(|d| (0, Cell { x: 5 + d, y: 2 + d })))
                        .chain((9..=11).map(|x| (0, Cell { x, y: 5 })))
                        .collect(),
                ),
                3,
            ),
        ];
        let mut results = Vec::new();
        for (name, path, segments) in &shapes {
            let start = spec.centre(path.cells[0].1);
            let end = spec.centre(path.cells.last().expect("shape has an endpoint").1);
            let lifted = lift(&spec, path, &[start], &[end]);
            assert_eq!(lifted.runs.len(), 1, "{name}: one run on a single layer");
            assert!(lifted.vias.is_empty(), "{name}: no layer changes means no vias");
            let run = &lifted.runs[0];
            assert_eq!(run.li, 0, "{name}: run preserves its layer");
            assert_eq!(run.path.segs.len(), *segments, "{name}: maximal straight stretches merge");
            assert_eq!(run.path.start, start, "{name}: exact start centre");
            assert_eq!(run.path.end(), end, "{name}: exact end centre");
            let step_length: f64 = path.cells.windows(2).map(|pair| {
                let a = pair[0].1;
                let b = pair[1].1;
                let factor = if a.x != b.x && a.y != b.y { std::f64::consts::SQRT_2 } else { 1.0 };
                f64::from(spec.pitch) * factor
            }).sum();
            assert!((run.path.length() - step_length).abs() <= 1.0, "{name}: grid step length is preserved");
            results.push(lifted);
        }

        let row = &shapes[0].1;
        let start = spec.centre(row.cells[0].1);
        let end = spec.centre(row.cells.last().expect("row has an endpoint").1);
        for (name, true_start, true_end, segments) in [
            ("forward start stub", P::new(start.x - 30_000, start.y), end, 1),
            ("perpendicular start stub", P::new(start.x, start.y - 30_000), end, 2),
            ("forward end stub", start, P::new(end.x + 30_000, end.y), 1),
            ("perpendicular end stub", start, P::new(end.x, end.y + 30_000), 2),
            ("reversed start stub", P::new(start.x + 30_000, start.y), end, 2),
            ("reversed end stub", start, P::new(end.x - 30_000, end.y), 2),
        ] {
            let lifted = lift(&spec, row, &[true_start], &[true_end]);
            assert_eq!(lifted.runs.len(), 1, "{name}: stubs stay on the endpoint layer");
            assert_eq!(lifted.runs[0].path.segs.len(), segments, "{name}: only forward collinear stubs merge");
            assert_eq!(lifted.runs[0].path.start, true_start, "{name}: true start is retained");
            assert_eq!(lifted.runs[0].path.end(), true_end, "{name}: true end is retained");
            results.push(lifted);
        }

        let true_start = P::new(start.x - 30_000, start.y - 30_000);
        let start_knee = P::new(start.x, start.y - 30_000);
        let end_knee = P::new(end.x, end.y + 30_000);
        let true_end = P::new(end.x + 30_000, end.y + 30_000);
        let lifted = lift(&spec, row, &[true_start, start_knee], &[true_end, end_knee]);
        assert_eq!(lifted.runs.len(), 1, "knees stay in the endpoint run");
        assert!(lifted.vias.is_empty(), "knees do not change layers");
        assert_eq!(
            lifted.runs[0].path,
            Path::new(true_start).line(start_knee).line(start).line(end).line(end_knee).line(true_end),
            "knees are kept",
        );

        let via_cell = Cell { x: 7, y: 2 };
        let via_centre = spec.centre(via_cell);
        let via_path = grid(
            (2..=7).map(|x| (0, Cell { x, y: 2 }))
                .chain((7..=11).map(|x| (1, Cell { x, y: 2 })))
                .collect(),
        );
        let mut reversed = via_path.clone();
        reversed.cells.reverse();
        for (path, from, to, first_li, last_li) in [
            (&via_path, start, end, 0, 1),
            (&reversed, end, start, 1, 0),
        ] {
            let lifted = lift(&spec, path, &[from], &[to]);
            assert_eq!(lifted.runs.len(), 2, "layer change splits the route into two runs");
            assert_eq!(lifted.vias, vec![(via_centre, 0, 1)], "via has the cell centre and sorted layers");
            assert_eq!(lifted.runs[0].li, first_li, "first run follows traversal layer order");
            assert_eq!(lifted.runs[1].li, last_li, "last run follows traversal layer order");
            assert_eq!(lifted.runs[0].path.start, from, "layered route retains its true start");
            assert_eq!(lifted.runs[0].path.end(), via_centre, "incoming run ends at the via");
            assert_eq!(lifted.runs[1].path.start, via_centre, "outgoing run starts at the via");
            assert_eq!(lifted.runs[1].path.end(), to, "layered route retains its true end");
            results.push(lifted);
        }

        let three_layers = GridSpec {
            layers: vec![LayerId::F_CU, LayerId::inner(1), LayerId::B_CU],
            ..spec.clone()
        };
        let mut chain = via_path.clone();
        for (li, _) in &mut chain.cells {
            if *li == 1 {
                *li = 2;
            }
        }
        chain.cells.insert(6, (1, via_cell));
        let lifted = lift(&three_layers, &chain, &[start], &[end]);
        assert_eq!(lifted.runs.len(), 2, "consecutive vias leave no middle-layer run");
        assert_eq!(lifted.runs[0].li, 0, "via chain retains its first layer");
        assert_eq!(lifted.runs[1].li, 2, "via chain retains its last layer");
        assert_eq!(lifted.vias, vec![(via_centre, 0, 2)], "consecutive vias coalesce from first to last layer");
        results.push(lifted);

        let single = grid(vec![(0, via_cell)]);
        let stationary = lift(&spec, &single, &[via_centre], &[via_centre]);
        assert!(stationary.runs.is_empty(), "stationary single cell has no zero-length run");
        assert!(stationary.vias.is_empty(), "stationary single cell has no vias");
        let left = P::new(via_centre.x - 30_000, via_centre.y);
        for (name, to, segments) in [
            ("single-cell straight", P::new(via_centre.x + 30_000, via_centre.y), 1),
            ("single-cell corner", P::new(via_centre.x, via_centre.y + 30_000), 2),
            ("single-cell reversal", left, 2),
        ] {
            let lifted = lift(&spec, &single, &[left], &[to]);
            assert_eq!(lifted.runs.len(), 1, "{name}: nonzero stubs produce one run");
            assert_eq!(lifted.runs[0].path.segs.len(), segments, "{name}: centre drops only in forward order");
            assert_eq!(lifted.runs[0].path.start, left, "{name}: exact start stub");
            assert_eq!(lifted.runs[0].path.end(), to, "{name}: exact end stub");
            results.push(lifted);
        }

        let via_only = grid(vec![(0, via_cell), (1, via_cell)]);
        let lifted = lift(&spec, &via_only, &[via_centre], &[via_centre]);
        assert!(lifted.runs.is_empty(), "via-only endpoint cells have no zero-length runs");
        assert_eq!(lifted.vias, vec![(via_centre, 0, 1)], "dropping zero-length runs retains the via");
        let lifted = lift(&spec, &via_only, &[left], &[P::new(via_centre.x + 30_000, via_centre.y)]);
        assert_eq!(lifted.runs.len(), 2, "nonzero endpoint stubs survive a via-only grid path");
        assert_eq!(lifted.runs[0].li, 0, "start stub remains on the first layer");
        assert_eq!(lifted.runs[1].li, 1, "end stub remains on the last layer");
        results.push(lifted);

        for lifted in &results {
            for run in &lifted.runs {
                for (index, (from, seg)) in run.path.steps().enumerate() {
                    assert!(matches!(seg, Seg::Line(_)), "lifted runs contain only straight segments");
                    assert_ne!(from, seg.end(), "lifted runs contain no zero-length segments");
                    if index > 0 {
                        assert_eq!(spec.centre(spec.cell_at(from)), from, "interior vertices are cell centres");
                    }
                }
            }
        }
    }
}
