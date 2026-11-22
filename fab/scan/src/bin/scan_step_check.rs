//! A made-up capture of the stepped block (`scan-session-synth --shape stepped`) through the scan
//! as Fab runs it (the base from the headset's depth, each keyframe's detail matched on this
//! machine's GPU, settled and gathered), measured against the block it was made of: how wide
//! the plate's edge comes out, and how far the plate's top and the box's top lie from where they
//! are. The output is numbers.
//!
//! usage: scan-step-check [--step-mm <mm>] [--grow <k>] [--shrink <n>] [--object] [--window] [--hurried] [--turned] [--at <n,n,..>] <session>
//!
//! `--object`: the pictures are matched around the block alone (a square 0.22 m each way of its
//! middle), as a match of what is being scanned at the cameras' own size would be;
//! `--window`: the pictures are matched as Fab's scan matches them (a square about the block's
//! middle, sized by how far it is: `refine_looked`); `--hurried` (with `--window`): a walk faster than the matching, every keyframe after the
//! first two matched at half the size and again at the full size after the last, the measure
//! printed before and after; `--at`: the keyframe counts the measure is
//! also printed at. Every keyframe's time is printed, in order.

use makepad_desk_link::xr_capture::{CaptureCamera, CaptureRecord};
use makepad_fab_scan::detail::{DetailPoint, Local};
use makepad_fab_scan::live::{LiveScan, Taken};
use makepad_fab_scan::refine::{looked_floor_pixels, refine_around, refine_looked, refine_window, rematch, Hurried, Refined, WINDOW};
use std::path::PathBuf;
use std::sync::Arc;

/// The box before `--grow`: its half sizes x, height / 2, z (as the made-up capture's).
const HALF: [f64; 3] = [0.05, 0.04, 0.03];
const CELL: f32 = 0.0015;
/// The width of a bin across the edge, and how far each side of the edge is looked at, metres.
const BIN: f64 = 0.0005;
const REACH: f64 = 0.012;

fn p95(mut values: Vec<f64>) -> f64 {
    values.sort_by(f64::total_cmp);
    values.get((0.95 * values.len().saturating_sub(1) as f64).round() as usize).copied().unwrap_or(0.0)
}

fn median(mut values: Vec<f64>) -> f64 {
    values.sort_by(f64::total_cmp);
    values.get(values.len() / 2).copied().unwrap_or(0.0)
}

/// Across one of the plate's edges (`along`: the axis across it, 0 or 2; `side`: which end).
/// The scan as a whole lies where the tracker put its first keyframe, a few millimetres off the
/// block, so the two levels are taken from the scan itself: the median height each side of the
/// edge. Then: the step between them, how flat each is (p95 from its median), and the width
/// over which the mean height goes from 10% to 90% of the step.
fn edge(points: &[DetailPoint], along: usize, side: f64, half: [f64; 3], step: f64) -> (String, Vec<(f64, f64)>) {
    let (base_top, edge_at, other) = (2.0 * half[1], 0.6 * half[along], 2 - along);
    // every point of the strip across the edge: how far across, how high
    let strip: Vec<(f64, f64)> = points
        .iter()
        .filter_map(|p| {
            let (across, y, beside) = (side * f64::from(p.point[along]), f64::from(p.point[1]), f64::from(p.point[other]));
            (beside.abs() <= 0.5 * half[other] && y >= base_top - 0.012 && y <= base_top + step + 0.012 && (across - edge_at).abs() < 2.0 * REACH).then_some((across - edge_at, y))
        })
        .collect();
    // The scan lies where the tracker put its first keyframe, a few millimetres off the block
    // sideways too: the edge is taken where the scan has it (the mean height half way up), and
    // the two levels each side of that.
    let levels = |shift: f64| {
        let plate: Vec<f64> = strip.iter().filter(|(a, _)| (-REACH..-0.005).contains(&(a - shift))).map(|(_, y)| *y).collect();
        let lower: Vec<f64> = strip.iter().filter(|(a, _)| (0.005..REACH).contains(&(a - shift))).map(|(_, y)| *y).collect();
        (plate, lower)
    };
    let profile = |shift: f64, low: f64, high: f64| -> Vec<Option<f64>> {
        let bins = (2.0 * REACH / BIN) as usize;
        let mut sums = vec![(0.0f64, 0usize); bins];
        for (a, y) in &strip {
            let from = a - shift;
            if from.abs() < REACH {
                let bin = (((from + REACH) / BIN) as usize).min(bins - 1);
                sums[bin].0 += y;
                sums[bin].1 += 1;
            }
        }
        sums.iter().map(|(sum, count)| (*count >= 5 && (high - low).abs() > 1e-6).then(|| (sum / *count as f64 - low) / (high - low))).collect()
    };
    // where the scan's edge is: the place across the strip that best parts its points into two
    // levels (the least summed squared distance of the heights from their side's mean), from all
    // the strip's points, so a sparse or noisy bin does not place it
    let mut sorted: Vec<(f64, f64)> = strip.iter().copied().filter(|(a, _)| a.abs() < REACH).collect();
    sorted.sort_by(|a, b| a.0.total_cmp(&b.0));
    let (total, total_squares): (f64, f64) = sorted.iter().fold((0.0, 0.0), |(sum, squares), (_, y)| (sum + y, squares + y * y));
    let (mut best, mut shift, mut sum, mut squares) = (f64::INFINITY, 0.0, 0.0, 0.0);
    for (i, (across, y)) in sorted.iter().enumerate() {
        sum += y;
        squares += y * y;
        let (left, right) = ((i + 1) as f64, (sorted.len() - i - 1) as f64);
        if left < 20.0 || right < 20.0 {
            continue;
        }
        let cost = (squares - sum * sum / left) + (total_squares - squares - (total - sum).powi(2) / right);
        if cost < best {
            (best, shift) = (cost, *across);
        }
    }
    let (plate, lower) = levels(shift);
    let (plate_level, box_level) = (median(plate.clone()), median(lower.clone()));
    let measured = plate_level - box_level;
    let rise = profile(shift, box_level, plate_level);
    let flat = |values: &[f64], level: f64| p95(values.iter().map(|y| 1000.0 * (y - level).abs()).collect());
    let leaves = rise.iter().rposition(|r| r.is_some_and(|r| r >= 0.9));
    let lands = leaves.and_then(|from| rise.iter().skip(from).position(|r| r.is_some_and(|r| r <= 0.1)));
    let width = lands.map_or("none".to_string(), |bins| format!("{:.1}", bins as f64 * BIN * 1000.0));
    let empty = rise.iter().filter(|r| r.is_none()).count();
    // every point of the strip from the edge as the scan has it, its height as a part of the step:
    // the four edges' points together are one edge's profile with four times the points
    let risen: Vec<(f64, f64)> = strip.iter().filter(|(a, _)| (a - shift).abs() < REACH && (plate_level - box_level).abs() > 1e-6).map(|(a, y)| (a - shift, (y - box_level) / (plate_level - box_level))).collect();
    // (how many of the scan's points stand over one another in the box-top band: its columns, a cell
    // square, that hold one point, two, three or more)
    let mut columns: std::collections::HashMap<(i64, i64), usize> = std::collections::HashMap::new();
    for p in points {
        let (across, y, beside) = (side * f64::from(p.point[along]) - edge_at - shift, f64::from(p.point[1]), f64::from(p.point[other]));
        if beside.abs() <= 0.5 * half[other] && (0.005..REACH).contains(&across) && y >= base_top - 0.012 && y <= base_top + step + 0.012 {
            *columns.entry(((across / 0.0015).floor() as i64, (beside / 0.0015).floor() as i64)).or_insert(0) += 1;
        }
    }
    let mut stacked = [0usize; 3];
    for count in columns.values() {
        stacked[(*count - 1).min(2)] += 1;
    }
    // (the band where the block truly has it, not where the scan's edge was found: the same patch
    // of the block in every scan of it; its points within half a millimetre of the level)
    let at_level_true_band = points
        .iter()
        .filter(|p| {
            let (across, y, beside) = (side * f64::from(p.point[along]) - edge_at, f64::from(p.point[1]), f64::from(p.point[other]));
            beside.abs() <= 0.5 * half[other] && (0.005..REACH).contains(&across) && (y - box_level).abs() < 0.0005
        })
        .count();
    // (where the box-top band's points lie from its level, mm: under -3, -3..-1, -1..-0.5, within 0.5, 0.5..1, 1..3, over 3)
    let mut spread = [0usize; 7];
    for y in &lower {
        let d = 1000.0 * (y - box_level);
        spread[[-3.0, -1.0, -0.5, 0.5, 1.0, 3.0].iter().position(|edge| d < *edge).unwrap_or(6)] += 1;
    }
    let line = format!(
        "step_measured_mm={:.2} edge_width_mm={width} plate_flat_p95_mm={:.2} box_flat_p95_mm={:.2} off_the_block_mm={:.2} edge_sideways_mm={:.1} plate_points={} box_points={} empty_bins={empty}/{} box_spread={spread:?} box_columns_of_1_2_3={stacked:?} at_level_in_the_true_band={at_level_true_band}",
        1000.0 * measured, flat(&plate, plate_level), flat(&lower, box_level), 1000.0 * (box_level - base_top), 1000.0 * shift, plate.len(), lower.len(), rise.len(),
    );
    (line, risen)
}

/// The width of the step's edge from the four edges' points together (each from its own edge as
/// the scan has it, its height as a part of its own step): from where the mean height last is
/// nine tenths of the step to where it first is a tenth, in bins of [`BIN`]; and how far the
/// points lie from a sharp step there (the rms of their heights from 1 before the edge and 0
/// after, as parts of the step, within 3 mm of it).
fn pooled(risen: &[(f64, f64)]) -> String {
    let bins = (2.0 * REACH / BIN) as usize;
    let mut sums = vec![(0.0f64, 0usize); bins];
    for (a, r) in risen {
        let bin = (((a + REACH) / BIN) as usize).min(bins - 1);
        sums[bin].0 += r;
        sums[bin].1 += 1;
    }
    let rise: Vec<Option<f64>> = sums.iter().map(|(sum, count)| (*count >= 20).then(|| sum / *count as f64)).collect();
    let leaves = rise.iter().rposition(|r| r.is_some_and(|r| r >= 0.9));
    let lands = leaves.and_then(|from| rise.iter().skip(from).position(|r| r.is_some_and(|r| r <= 0.1)));
    let width = lands.map_or("none".to_string(), |bins| format!("{:.1}", bins as f64 * BIN * 1000.0));
    let near: Vec<f64> = risen.iter().filter(|(a, _)| a.abs() < 0.003).map(|(a, r)| (r - if *a < 0.0 { 1.0 } else { 0.0 }).powi(2)).collect();
    let soft = (near.iter().sum::<f64>() / near.len().max(1) as f64).sqrt();
    format!("points={} edge_width_mm={width} off_a_sharp_step_rms={soft:.3} thin_bins={}/{bins}", risen.len(), rise.iter().filter(|r| r.is_none()).count())
}

fn run() -> Result<(), String> {
    let usage = "usage: scan-step-check [--step-mm <mm>] [--grow <k>] [--shrink <n>] [--object] [--window] [--full] [--hurried] [--slants] [--poses <file>] [--turned] [--at <n,n,..>] <session>";
    let mut args = std::env::args().skip(1);
    let (mut step_mm, mut grow, mut shrink, mut folder) = (4.0_f64, 2.5_f64, 2_usize, None);
    let (mut object, mut window, mut hurried, mut slants, mut at) = (false, false, false, false, Vec::<usize>::new());
    let mut poses: Option<PathBuf> = None;
    let mut full = false;
    let mut turned = false;
    while let Some(arg) = args.next() {
        let mut value = || args.next().ok_or(usage.to_string());
        match arg.as_str() {
            "--step-mm" => step_mm = value()?.parse().map_err(|_| usage.to_string())?,
            "--grow" => grow = value()?.parse().map_err(|_| usage.to_string())?,
            "--shrink" => shrink = value()?.parse().map_err(|_| usage.to_string())?,
            "--object" => object = true,
            "--window" => window = true,
            "--hurried" => hurried = true,
            "--slants" => slants = true,
            "--full" => full = true,
            "--poses" => poses = Some(PathBuf::from(value()?)),
            "--turned" => turned = true,
            "--at" => at = value()?.split(',').filter_map(|n| n.parse().ok()).collect(),
            _ => folder = Some(PathBuf::from(arg)),
        }
    }
    let folder = folder.ok_or(usage)?;
    let mut names: Vec<PathBuf> = std::fs::read_dir(&folder).map_err(|e| format!("the session: {}", e.kind()))?.filter_map(|e| e.ok()).map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "bin")).collect();
    names.sort();
    let matcher = Local { gpu: Some(makepad_compute_program_gpu::spawn().map_err(|e| format!("no GPU device: {e}"))?) };
    let (mut scan, mut refined, mut cameras) = (LiveScan::default(), Refined::new(CELL), Vec::<CaptureCamera>::new());
    let (mut keyframes, mut settled, mut ms, mut pixels) = (0usize, Vec::new(), Vec::new(), 0usize);
    let mut sizes: Vec<usize> = Vec::new();
    let mut floor = [[[0usize; 6]; 5]; 2];
    let truth: Vec<[f64; 14]> = match &poses {
        Some(path) => std::fs::read_to_string(path).map_err(|e| format!("the poses: {}", e.kind()))?.lines().filter_map(|l| l.split_whitespace().filter_map(|v| v.parse().ok()).collect::<Vec<f64>>().try_into().ok()).collect(),
        None => Vec::new(),
    };
    let mut off: Vec<([f64; 3], [f64; 3])> = Vec::new();
    let mut corners: [Vec<[f64; 3]>; 4] = Default::default();
    let mut placed_truly = Refined::new(CELL);
    let mut probe = [[(0usize, 0usize); 8]; 4];
    let (mut frame, mut moves) = ([[0.0f64; 3]; 4], ([0.0f64; 3], [0.0f64; 3]));
    let mut placed_off = Refined::new(CELL);
    let mut as_said = Refined::new(CELL);
    let (mut again, mut held): (Vec<Hurried>, usize) = (Vec::new(), 0);
    // (`--turned`: the capture was made with the block a quarter turn about its up)
    let half = if turned { [HALF[2], HALF[1], HALF[0]] } else { HALF }.map(|v| v * grow);
    let around = object.then_some(([0.0, half[1], 0.0], 0.22));
    let measure = |refined: &Refined, keyframes: usize, settled: &[f64], ms: &[f64], pixels: usize| {
        let points = refined.points();
        let mut sorted = ms.to_vec();
        sorted.sort_by(f64::total_cmp);
        println!(
            "STEP step_mm={step_mm} shrink={shrink} object={object} keyframes={keyframes} detail_points={} settled_p95_mm={:.2} ms_a_keyframe_median={:.0} matched_pixels_a_view={pixels}",
            points.len(), p95(settled.to_vec()), sorted.get(sorted.len() / 2).copied().unwrap_or(0.0),
        );
        let mut all = Vec::new();
        for (name, along, side) in [("x+", 0, 1.0), ("x-", 0, -1.0), ("z+", 2, 1.0), ("z-", 2, -1.0)] {
            let (line, risen) = edge(&points, along, side, half, step_mm / 1000.0);
            println!("EDGE keyframes={keyframes} at={name} {line}");
            all.extend(risen);
        }
        println!("EDGES keyframes={keyframes} {}", pooled(&all));
    };
    for name in names {
        let record = CaptureRecord::decode(&std::fs::read(&name).map_err(|e| e.kind().to_string())?).map_err(|_| "a record that does not decode".to_string())?;
        if let CaptureRecord::Cameras(list) = &record {
            cameras = list.clone();
        }
        if !matches!(scan.take(&record)?, Taken::Keyframe(_)) {
            continue;
        }
        let CaptureRecord::Keyframe(keyframe) = &record else { continue };
        let base: Vec<([f32; 3], f32)> = scan.surface(150_000).points.iter().map(|s| (s.point, s.size)).collect();
        if slants {
            // (`--slants`: where the floor's pixels go, by how slanted the look at them is, unhurried and hurried)
            for (i, counts) in [false, true].into_iter().map(|h| looked_floor_pixels(&matcher, keyframe, &cameras, &base, Some([0.0, half[1], 0.0]), h)).enumerate() {
                for (sum, row) in floor[i].iter_mut().zip(counts?) {
                    for k in 0..6 {
                        sum[k] += row[k];
                    }
                }
            }
        }
        let began = std::time::Instant::now();
        let mut points = match window {
            true => {
                // (`--hurried`: a walk faster than the matching: every keyframe after the first two is
                // matched at half the size, as Fab does with keyframes waiting, and matched again after the last)
                // (`--full`: every keyframe at the pictures' own size, whatever Fab's rule would pick: the
                // rig's own, to measure what the size does)
                let (points, at_size) = match full {
                    true => (refine_window(&matcher, keyframe, &cameras, &base, 1, CELL, Some([0.0, half[1], 0.0]), WINDOW)?, 1),
                    false => refine_looked(&matcher, keyframe, &cameras, &base, CELL, Some([0.0, half[1], 0.0]), hurried && keyframes >= 2)?,
                };
                sizes.push(at_size);
                points
            }
            false => refine_around(&matcher, keyframe, &cameras, &base, shrink, CELL, around)?,
        };
        ms.push(began.elapsed().as_secs_f64() * 1000.0);
        pixels = match around {
            Some((_, reach)) => keyframe.views.first().map_or(0, |v| {
                let away = (0..3).map(|i| (f64::from(v.head.position[i]) - [0.0, half[1], 0.0][i]).powi(2)).sum::<f64>().sqrt();
                let side = (2.0 * reach * 0.68 * f64::from(v.planes.first().map_or(0, |p| p.width)) / shrink as f64 / away.max(0.05)) as usize;
                side * side
            }),
            None => keyframe.views.first().and_then(|v| v.planes.first()).map_or(0, |p| (p.width as usize / shrink) * (p.height as usize / shrink)),
        };
        if let Some(pose) = truth.get(keyframes) {
            // (the keyframe's points put where the head truly was: what the matching alone gives,
            // with nothing of the tracker's error or the settling in it)
            let truly: Vec<DetailPoint> = points.iter().map(|p| DetailPoint { point: unplaced(pose, p.point.map(f64::from)).map(|v| v as f32), ..*p }).collect();
            placed_truly.add(&truly);
            // (the probe: this keyframe's own points in the box-top band beside each plate edge, those
            // lifted over a millimetre, by where the camera stood: 0 outside the edge looking at it,
            // 4 across the plate from it, counted round in eighths)
            let top = 2.0 * half[1];
            for (e, (along, side)) in [(0usize, 1.0f64), (0, -1.0), (2, 1.0), (2, -1.0)].into_iter().enumerate() {
                let other = 2 - along;
                let mut out = [0.0f64; 3];
                out[along] = side;
                let at_edge = [out[0] * 0.6 * half[0], 0.0, out[2] * 0.6 * half[2]];
                let to = [pose[0] - at_edge[0], pose[2] - at_edge[2]];
                let (dot, cross) = (out[0] * to[0] + out[2] * to[1], out[0] * to[1] - out[2] * to[0]);
                let sector = ((cross.atan2(dot) / std::f64::consts::FRAC_PI_4).round() as i64).rem_euclid(8) as usize;
                for p in &truly {
                    let (across, y, beside) = (side * f64::from(p.point[along]) - 0.6 * half[along], f64::from(p.point[1]), f64::from(p.point[other]));
                    if beside.abs() <= 0.5 * half[other] && (0.005..REACH).contains(&across) && y >= top - 0.012 && y <= top + step_mm / 1000.0 + 0.012 {
                        probe[e][sector].1 += 1;
                        probe[e][sector].0 += usize::from(y - top > 0.001);
                    }
                }
            }
            // (and the same, with the whole scan a tracker's error off: 3 mm and 5 mrad, every keyframe alike)
            placed_off.add(&truly.iter().map(|p| DetailPoint { point: off_by(p.point, 1.0), ..*p }).collect::<Vec<_>>());
        }
        let as_tracked = if truth.is_empty() { Vec::new() } else { points.clone() };
        let began = std::time::Instant::now();
        let moved = refined.settle(&mut points);
        if !as_tracked.is_empty() {
            // (the keyframe's points moved by what the settling says it did, as one turn and shift:
            // the same store as the settled one if the points went where it says)
            let (turn, shift) = moved.map_or(([0.0; 3], [0.0; 3]), |m| (m.turn, m.shift));
            let said: Vec<DetailPoint> = as_tracked
                .iter()
                .map(|p| {
                    let at = p.point.map(f64::from);
                    DetailPoint { point: [(at[0] + turn[1] * at[2] - turn[2] * at[1] + shift[0]) as f32, (at[1] + turn[2] * at[0] - turn[0] * at[2] + shift[1]) as f32, (at[2] + turn[0] * at[1] - turn[1] * at[0] + shift[2]) as f32], ..*p }
                })
                .collect();
            let apart = said.iter().zip(&points).map(|(a, b)| (0..3).map(|i| f64::from(a.point[i] - b.point[i]).powi(2)).sum::<f64>().sqrt()).fold(0.0, f64::max);
            println!("SAID keyframe={keyframes} points_from_where_settling_says_most_mm={:.3}", 1000.0 * apart);
            as_said.add_settled(&said, moved.as_ref());
        }
        if let Some(pose) = truth.get(keyframes) {
            // where this keyframe puts the block's middle: off by the tracker's error, then moved by the settling
            let middle = [0.0, half[1], 0.0];
            let reported = placed(pose, middle);
            let (turn, shift) = moved.map_or(([0.0; 3], [0.0; 3]), |m| (m.turn, m.shift));
            let settled_at = [
                reported[0] + turn[1] * reported[2] - turn[2] * reported[1] + shift[0],
                reported[1] + turn[2] * reported[0] - turn[0] * reported[2] + shift[1],
                reported[2] + turn[0] * reported[1] - turn[1] * reported[0] + shift[2],
            ];
            off.push((std::array::from_fn(|i| reported[i] - middle[i]), std::array::from_fn(|i| settled_at[i] - middle[i])));
            // (the block's middle and a tenth of a metre from it along each axis, as this keyframe is settled:
            // where the settled scan puts the truth's frame)
            for (n, at) in [middle, [middle[0] + 0.1, middle[1], middle[2]], [middle[0], middle[1] + 0.1, middle[2]], [middle[0], middle[1], middle[2] + 0.1]].into_iter().enumerate() {
                let reported = placed(pose, at);
                for i in 0..3 {
                    let j = (i + 1) % 3;
                    let k = (i + 2) % 3;
                    frame[n][i] += reported[i] + turn[j] * reported[k] - turn[k] * reported[j] + shift[i];
                }
            }
            for i in 0..3 {
                moves.0[i] += turn[i];
                moves.1[i] += shift[i];
            }
            // (and the middles of the plate's four edges: a turn left over shows there, not at the middle)
            let top = 2.0 * half[1];
            for (n, at) in [[0.6 * half[0], top, 0.0], [-0.6 * half[0], top, 0.0], [0.0, top, 0.6 * half[2]], [0.0, top, -0.6 * half[2]]].into_iter().enumerate() {
                let reported = placed(pose, at);
                let settled_at = [
                    reported[0] + turn[1] * reported[2] - turn[2] * reported[1] + shift[0],
                    reported[1] + turn[2] * reported[0] - turn[0] * reported[2] + shift[1],
                    reported[2] + turn[0] * reported[1] - turn[1] * reported[0] + shift[2],
                ];
                corners[n].push(std::array::from_fn(|i| settled_at[i] - at[i]));
            }
        }
        if let Some(moved) = &moved {
            settled.push(1000.0 * f64::from(moved.by));
            let size = |v: [f64; 3]| (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
            println!("SETTLE keyframe={keyframes} points={} by_mm={:.2} turn_mrad={:.2} shift_mm={:.2} settle_ms={:.1}", points.len(), 1000.0 * f64::from(moved.by), 1000.0 * size(moved.turn), 1000.0 * size(moved.shift), began.elapsed().as_secs_f64() * 1000.0);
        }
        if hurried && keyframes >= 2 {
            let kept = refined.add_kept(&points, moved);
            held += kept.bytes();
            again.push(Hurried { keyframe: Arc::new(keyframe.clone()), cameras: Arc::new(cameras.clone()), place: Some([0.0, half[1], 0.0]), kept });
        } else {
            let gather = std::time::Instant::now();
            refined.add_settled(&points, moved.as_ref());
            println!("GATHER keyframe={keyframes} points={} gather_ms={:.1}", points.len(), gather.elapsed().as_secs_f64() * 1000.0);
        }
        // what the keyframe sees through, from where its first camera stood as it was settled
        if let Some(eye) = keyframe.views.first().and_then(|view| cameras.get(view.camera as usize).and_then(|c| c.lens_pose.as_ref()).map(|lens| makepad_fab_scan::session::camera_to_world(&view.head, lens))) {
            let eye = [eye[0][3], eye[1][3], eye[2][3]];
            let carve = std::time::Instant::now();
            refined.carve(moved.map_or(eye, |m| m.moved(eye)), &points);
            println!("CARVE keyframe={keyframes} carve_ms={:.1}", carve.elapsed().as_secs_f64() * 1000.0);
        }
        keyframes += 1;
        if at.contains(&keyframes) {
            measure(&refined, keyframes, &settled, &ms, pixels);
        }
    }
    if !at.contains(&keyframes) {
        measure(&refined, keyframes, &settled, &ms, pixels);
    }
    if hurried {
        let mut took = Vec::new();
        let base: Vec<([f32; 3], f32)> = scan.surface(150_000).points.iter().map(|s| (s.point, s.size)).collect();
        for one in &again {
            let began = std::time::Instant::now();
            rematch(&matcher, &mut refined, one, &base, CELL)?;
            took.push(began.elapsed().as_secs_f64() * 1000.0);
        }
        println!("AGAIN keyframes={} kept_mb_each={:.2} ms_each={}", took.len(), held as f64 / 1e6 / again.len().max(1) as f64, took.iter().map(|v| format!("{v:.0}")).collect::<Vec<_>>().join("/"));
        measure(&refined, keyframes, &settled, &ms, pixels);
    }
    if slants {
        for (walk, counts) in ["unhurried", "hurried"].iter().zip(floor) {
            for (slant, c) in ["over_0.7", "0.5-0.7", "0.35-0.5", "0.2-0.35", "under_0.2"].iter().zip(counts) {
                let of = |n: usize| 100.0 * n as f64 / c[0].max(1) as f64;
                println!("SLANT match={walk} cos={slant} pixels={} no_agreement_pct={:.1} cost_pct={:.1} depth_pct={:.1} off_base_pct={:.1} kept_pct={:.1}", c[0], of(c[1]), of(c[2]), of(c[3]), of(c[4]), of(c[5]));
            }
        }
    }
    if !off.is_empty() {
        // between the keyframes (each less their mean: a move they all share blurs nothing): how far
        // apart they put the block's middle, up (across the plate) and level (along it), as the
        // tracker reports them and after the settling
        for (name, pick) in [("tracked", 0usize), ("settled", 1)] {
            let of = |o: &([f64; 3], [f64; 3])| if pick == 0 { o.0 } else { o.1 };
            let mean: [f64; 3] = std::array::from_fn(|i| off.iter().map(|o| of(o)[i]).sum::<f64>() / off.len() as f64);
            let up: Vec<f64> = off.iter().map(|o| 1000.0 * (of(o)[1] - mean[1]).abs()).collect();
            let level: Vec<f64> = off.iter().map(|o| 1000.0 * ((of(o)[0] - mean[0]).powi(2) + (of(o)[2] - mean[2]).powi(2)).sqrt()).collect();
            let rms = |v: &[f64]| (v.iter().map(|x| x * x).sum::<f64>() / v.len() as f64).sqrt();
            if pick == 1 {
                // (each keyframe: which one lies off the others)
                for (k, o) in off.iter().enumerate() {
                    let d: [f64; 3] = std::array::from_fn(|i| 1000.0 * (of(o)[i] - mean[i]));
                    let tracked: [f64; 3] = std::array::from_fn(|i| 1000.0 * o.0[i]);
                    println!("OFF keyframe={k} settled_x_mm={:.2} y_mm={:.2} z_mm={:.2} tracked_x_mm={:.2} y_mm={:.2} z_mm={:.2}", d[0], d[1], d[2], tracked[0], tracked[1], tracked[2]);
                }
            }
            println!("APART poses={name} keyframes={} up_rms_mm={:.2} up_p95_mm={:.2} level_rms_mm={:.2} level_p95_mm={:.2}", off.len(), rms(&up), p95(up.clone()), rms(&level), p95(level.clone()));
        }
    }
    if !truth.is_empty() {
        for (name, sectors) in ["x+", "x-", "z+", "z-"].iter().zip(&probe) {
            println!("PROBE at={name} lifted_of_points_by_where_the_camera_stood={}", sectors.iter().map(|(lifted, all)| format!("{lifted}/{all}")).collect::<Vec<_>>().join(" "));
        }
        println!("TRULY-PLACED (each keyframe's points where the head truly was, gathered unsettled):");
        measure(&placed_truly, keyframes, &settled, &ms, pixels);
        // The settled scan in the truth's frame: where the keyframes, settled, put the block's middle and
        // its axes (their mean), then moved as the scan's points are given (back by the mean of the
        // settling moves); the scan's points taken back by that one rigid move and measured again.
        let n = keyframes as f64;
        let (turn, shift) = (moves.0.map(|v| -v / n), moves.1.map(|v| -v / n));
        let given: Vec<[f64; 3]> = frame.iter().map(|sum| {
            let at = sum.map(|v| v / n);
            [at[0] + turn[1] * at[2] - turn[2] * at[1] + shift[0], at[1] + turn[2] * at[0] - turn[0] * at[2] + shift[1], at[2] + turn[0] * at[1] - turn[1] * at[0] + shift[2]]
        }).collect();
        let middle = [0.0, half[1], 0.0];
        let axes: [[f64; 3]; 3] = std::array::from_fn(|a| std::array::from_fn(|i| (given[a + 1][i] - given[0][i]) / 0.1));
        let lies = (0..3).map(|i| (given[0][i] - middle[i]).powi(2)).sum::<f64>().sqrt();
        let turned = (((axes[0][0] + axes[1][1] + axes[2][2] - 1.0) / 2.0).clamp(-1.0, 1.0)).acos();
        println!("SETTLED-FRAME the settled scan, as its points are given, lies {:.2} mm and {:.2} mrad from the truth (middle at {:.2} {:.2} {:.2} mm off)", 1000.0 * lies, 1000.0 * turned, 1000.0 * (given[0][0] - middle[0]), 1000.0 * (given[0][1] - middle[1]), 1000.0 * (given[0][2] - middle[2]));
        // (a point of the scan back in the truth's frame: its offset from the middle's image, along each axis' image)
        let back: Vec<DetailPoint> = refined.points().iter().map(|p| {
            let d: [f64; 3] = std::array::from_fn(|i| f64::from(p.point[i]) - given[0][i]);
            DetailPoint { point: std::array::from_fn(|a| (middle[a] + (0..3).map(|i| d[i] * axes[a][i]).sum::<f64>()) as f32), ..*p }
        }).collect();
        println!("SETTLED, REGISTERED (the settled scan's points taken back to the truth's frame by that one rigid move):");
        let mut all = Vec::new();
        for (name, along, side) in [("x+", 0, 1.0), ("x-", 0, -1.0), ("z+", 2, 1.0), ("z-", 2, -1.0)] {
            let (line, risen) = edge(&back, along, side, half, step_mm / 1000.0);
            println!("EDGE keyframes={keyframes} at={name} {line}");
            all.extend(risen);
        }
        println!("EDGES keyframes={keyframes} {}", pooled(&all));
        println!("AS-SAID (each keyframe's points moved by the settling's reported turn and shift, gathered):");
        measure(&as_said, keyframes, &settled, &ms, pixels);
        println!("TRULY-PLACED, OFF (the same, the whole scan 3 mm and 5 mrad off while gathered, moved back to be measured):");
        let back: Vec<DetailPoint> = placed_off.points().iter().map(|p| DetailPoint { point: off_by(p.point, -1.0), ..*p }).collect();
        let mut all = Vec::new();
        for (name, along, side) in [("x+", 0, 1.0), ("x-", 0, -1.0), ("z+", 2, 1.0), ("z-", 2, -1.0)] {
            let (line, risen) = edge(&back, along, side, half, step_mm / 1000.0);
            println!("EDGE keyframes={keyframes} at={name} {line}");
            all.extend(risen);
        }
        println!("EDGES keyframes={keyframes} {}", pooled(&all));
    }
    for (name, at) in ["x+", "x-", "z+", "z-"].iter().zip(&corners) {
        if at.is_empty() {
            continue;
        }
        let mean: [f64; 3] = std::array::from_fn(|i| at.iter().map(|o| o[i]).sum::<f64>() / at.len() as f64);
        let rms = |pick: &dyn Fn(&[f64; 3]) -> f64| 1000.0 * (at.iter().map(|o| pick(o).powi(2)).sum::<f64>() / at.len() as f64).sqrt();
        let most = at.iter().map(|o| 1000.0 * ((o[0] - mean[0]).powi(2) + (o[2] - mean[2]).powi(2)).sqrt()).fold(0.0, f64::max);
        println!("APART at_edge={name} up_rms_mm={:.2} level_rms_mm={:.2} level_most_mm={most:.2}", rms(&|o| o[1] - mean[1]), rms(&|o| ((o[0] - mean[0]).powi(2) + (o[2] - mean[2]).powi(2)).sqrt()));
    }
    println!("MS window={window} every_keyframe={} matched_at_1_over={:?}", ms.iter().map(|v| format!("{v:.0}")).collect::<Vec<_>>().join("/"), sizes);
    Ok(())
}

/// Where a keyframe's reported pose puts a point of the world: `pose` is the head's true place
/// and turn and the tracker's (each a position and a quaternion x y z w); what the cameras saw
/// at `point` is placed as if the head were where the tracker said.
fn placed(pose: &[f64; 14], point: [f64; 3]) -> [f64; 3] {
    let turn = |q: &[f64], v: [f64; 3], back: bool| {
        let (x, y, z, w) = (q[0], q[1], q[2], if back { -q[3] } else { q[3] });
        let t = [2.0 * (y * v[2] - z * v[1]), 2.0 * (z * v[0] - x * v[2]), 2.0 * (x * v[1] - y * v[0])];
        [v[0] + w * t[0] + y * t[2] - z * t[1], v[1] + w * t[1] + z * t[0] - x * t[2], v[2] + w * t[2] + x * t[1] - y * t[0]]
    };
    let seen = turn(&pose[3..7], std::array::from_fn(|i| point[i] - pose[i]), true);
    let out = turn(&pose[10..14], seen, false);
    std::array::from_fn(|i| out[i] + pose[7 + i])
}

/// [`placed`] back: a point placed as the tracker said, where it truly is.
fn unplaced(pose: &[f64; 14], point: [f64; 3]) -> [f64; 3] {
    let swapped: [f64; 14] = std::array::from_fn(|i| pose[(i + 7) % 14]);
    placed(&swapped, point)
}

/// A point with the whole scan a tracker's error off (`way` 1), or back (-1): a turn of 5 mrad
/// about a slanted axis through the origin and 3 mm.
fn off_by(point: [f32; 3], way: f64) -> [f32; 3] {
    let (turn, shift) = ([0.003 * way, 0.002 * way, 0.0035 * way], [0.002 * way, 0.001 * way, 0.002 * way]);
    let at = point.map(f64::from);
    let moved = [at[0] + turn[1] * at[2] - turn[2] * at[1] + shift[0], at[1] + turn[2] * at[0] - turn[0] * at[2] + shift[1], at[2] + turn[0] * at[1] - turn[1] * at[0] + shift[2]];
    // (back: the turn the other way about where the shift took the origin)
    let moved = if way < 0.0 { [moved[0] - (turn[1] * shift[2] - turn[2] * shift[1]), moved[1] - (turn[2] * shift[0] - turn[0] * shift[2]), moved[2] - (turn[0] * shift[1] - turn[1] * shift[0])] } else { moved };
    moved.map(|v| v as f32)
}

fn main() {
    if let Err(e) = run() {
        eprintln!("scan-step-check: {e}");
        std::process::exit(1);
    }
}
