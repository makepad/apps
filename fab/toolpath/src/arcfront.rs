//! Arc overhangs (EXPSLICE §2.6): the first layer of a region that hangs
//! out over air is filled with fronts. Each front is a line at a constant
//! distance from the supported edge, one bead spacing past the last, so
//! every front is laid against the one before it along its whole length,
//! and 90° overhangs print without support. The distance is geodesic
//! (measured inside the overhang), so fronts wrap around notches instead of
//! jumping across them.
//!
//! Per layer and pure: (this layer's slices, the layer below's outline,
//! settings) → (the overhang region, its fronts in print order).

use crate::exp::ArcOverhang;
use crate::{flow, ExtrusionPath, Role};
use makepad_fab_slicer::clip::{difference_ex, intersection_ex};
use makepad_fab_slicer::geom::*;
use makepad_fab_slicer::offset::{offset_ex, DEFAULT_MITER};
use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap};

/// The part of this layer that hangs past the layer below by more than a
/// printable overhang, in pieces big enough to be worth fronts.
pub fn overhang(slices: &[ExPolygon], below: &[ExPolygon], a: &ArcOverhang, w: f64) -> Vec<ExPolygon> {
    if below.is_empty() {
        return Vec::new();
    }
    let reach = offset_ex(below, a.allow * w * SCALE, DEFAULT_MITER);
    let ov = difference_ex(slices, &reach);
    // Open by a bead: slivers along a gently sloping wall are not overhangs.
    let core = offset_ex(&offset_ex(&ov, -0.5 * w * SCALE, DEFAULT_MITER), 0.5 * w * SCALE, DEFAULT_MITER);
    let core = intersection_ex(&core, &ov);
    core.into_iter().filter(|e| e.area_mm2() >= a.min_area).collect()
}

/// Where fronts start from: this layer's area that the layer below holds up.
pub fn anchor(slices: &[ExPolygon], below: &[ExPolygon], a: &ArcOverhang, w: f64) -> Vec<ExPolygon> {
    intersection_ex(slices, &offset_ex(below, a.allow * w * SCALE, DEFAULT_MITER))
}

const CELL: f64 = 0.1;

/// The fronts filling `ov`, grown from where it touches `supported`
/// (this layer's slices minus the overhang).
pub fn fronts(ov: &ExPolygon, supported: &[ExPolygon], a: &ArcOverhang, w: f64, h: f64) -> Vec<ExtrusionPath> {
    let b = ov.bbox();
    let (x0, y0) = (unscaled(b.min.x) - 2.0 * CELL, unscaled(b.min.y) - 2.0 * CELL);
    let nx = ((unscaled(b.max.x) - x0) / CELL).ceil() as usize + 3;
    let ny = ((unscaled(b.max.y) - y0) / CELL).ceil() as usize + 3;
    if nx * ny > 16_000_000 {
        return Vec::new();
    }
    let at = |i: usize, j: usize| Pt::from_mm(x0 + i as f64 * CELL, y0 + j as f64 * CELL);
    // Cells: inside the overhang, or an anchor (supported, next to it).
    let near: Vec<ExPolygon> = intersection_ex(supported, &offset_ex(std::slice::from_ref(ov), 3.0 * CELL * SCALE, DEFAULT_MITER));
    let mut d = vec![f64::NAN; nx * ny];
    let mut heap = BinaryHeap::new();
    for j in 0..ny {
        for i in 0..nx {
            let p = at(i, j);
            if ov.contains(p) {
                d[j * nx + i] = f64::INFINITY;
            } else if near.iter().any(|e| e.contains(p)) {
                d[j * nx + i] = 0.0;
                heap.push(Reverse((0f64.to_bits(), i, j)));
            }
        }
    }
    let k5 = 5f64.sqrt();
    let nb: [(i64, i64, f64); 16] = [(1, 0, 1.0), (-1, 0, 1.0), (0, 1, 1.0), (0, -1, 1.0), (1, 1, 1.414_213_6), (1, -1, 1.414_213_6), (-1, 1, 1.414_213_6), (-1, -1, 1.414_213_6), (2, 1, k5), (2, -1, k5), (-2, 1, k5), (-2, -1, k5), (1, 2, k5), (1, -2, k5), (-1, 2, k5), (-1, -2, k5)];
    while let Some(Reverse((db, i, j))) = heap.pop() {
        let dv = f64::from_bits(db);
        if dv > d[j * nx + i] {
            continue;
        }
        for &(di, dj, c) in &nb {
            let (u, v) = (i as i64 + di, j as i64 + dj);
            if u < 0 || v < 0 || u >= nx as i64 || v >= ny as i64 {
                continue;
            }
            let k = v as usize * nx + u as usize;
            let nd = dv + c * CELL;
            // Outside (NaN) and anchors (0) never change.
            if !d[k].is_nan() && d[k] > 0.0 && nd < d[k] {
                d[k] = nd;
                heap.push(Reverse((nd.to_bits(), u as usize, v as usize)));
            }
        }
    }
    let spacing = a.spacing * flow::spacing(w, h);
    let dmax = d.iter().copied().filter(|v| v.is_finite()).fold(0.0, f64::max);
    let area = flow::cross_section(w, h);
    let mut out: Vec<ExtrusionPath> = Vec::new();
    let mut k = 0;
    loop {
        let iso = (k as f64 + 0.5) * spacing + 1e-4;
        if iso > dmax {
            break;
        }
        let mut lines = iso_lines(&d, nx, ny, x0, y0, iso);
        // In print order: nearest to where the last front ended.
        let mut cur = out.last().map(|p| p.last());
        while !lines.is_empty() {
            let pick = match cur {
                None => 0,
                Some(c) => (0..lines.len()).min_by_key(|&i| lines[i][0].dist2(c).min(lines[i].last().unwrap().dist2(c))).unwrap(),
            };
            let mut pl = lines.swap_remove(pick);
            if let Some(c) = cur {
                if pl.last().unwrap().dist2(c) < pl[0].dist2(c) {
                    pl.reverse();
                }
            }
            let pl = simplify_polyline(&pl, 0.02 * SCALE);
            let len: f64 = pl.windows(2).map(|w| unscaled(w[0].dist(w[1]) as i64)).sum();
            if pl.len() < 2 || len < 1.0 {
                continue;
            }
            cur = pl.last().copied();
            out.push(ExtrusionPath { role: Role::OverhangWall, pts: pl, width: w, height: h, area, speed: a.speed, mode: crate::exp::mode::ARC_FRONT, ..Default::default() });
        }
        k += 1;
    }
    out
}

/// The iso-lines d = iso of a grid (NaN cells are outside), chained into polylines.
fn iso_lines(d: &[f64], nx: usize, ny: usize, x0: f64, y0: f64, iso: f64) -> Vec<Vec<Pt>> {
    // Edge ids: horizontal edge (i,j)-(i+1,j) = 2*(j*nx+i), vertical (i,j)-(i,j+1) = 2*(j*nx+i)+1.
    let val = |i: usize, j: usize| d[j * nx + i];
    let cross = |e: usize| -> Pt {
        let (c, vert) = (e / 2, e % 2 == 1);
        let (i, j) = (c % nx, c / nx);
        let (i2, j2) = if vert { (i, j + 1) } else { (i + 1, j) };
        let (a, b) = (val(i, j), val(i2, j2));
        let t = ((iso - a) / (b - a)).clamp(0.0, 1.0);
        let (xa, ya) = (x0 + i as f64 * CELL, y0 + j as f64 * CELL);
        let (xb, yb) = (x0 + i2 as f64 * CELL, y0 + j2 as f64 * CELL);
        Pt::from_mm(xa + (xb - xa) * t, ya + (yb - ya) * t)
    };
    let mut adj: HashMap<usize, Vec<usize>> = HashMap::new();
    for j in 0..ny - 1 {
        for i in 0..nx - 1 {
            let c = [val(i, j), val(i + 1, j), val(i + 1, j + 1), val(i, j + 1)];
            if c.iter().any(|v| v.is_nan() || v.is_infinite()) {
                continue;
            }
            let e = [2 * (j * nx + i), 2 * (j * nx + i + 1) + 1, 2 * ((j + 1) * nx + i), 2 * (j * nx + i) + 1];
            let mut hit: Vec<usize> = Vec::with_capacity(4);
            for k in 0..4 {
                if (c[k] >= iso) != (c[(k + 1) % 4] >= iso) {
                    hit.push(e[k]);
                }
            }
            let pairs: Vec<(usize, usize)> = match hit.len() {
                2 => vec![(hit[0], hit[1])],
                4 => vec![(hit[0], hit[1]), (hit[2], hit[3])],
                _ => Vec::new(),
            };
            for (a, b) in pairs {
                adj.entry(a).or_default().push(b);
                adj.entry(b).or_default().push(a);
            }
        }
    }
    // Walk chains, open ones from their ends first.
    let mut seen: HashMap<usize, bool> = HashMap::new();
    let mut starts: Vec<usize> = adj.iter().filter(|(_, v)| v.len() == 1).map(|(k, _)| *k).collect();
    starts.sort();
    let mut rest: Vec<usize> = adj.keys().copied().collect();
    rest.sort();
    starts.extend(rest);
    let mut out = Vec::new();
    for s in starts {
        if seen.contains_key(&s) {
            continue;
        }
        let mut chain = vec![s];
        seen.insert(s, true);
        let mut cur = s;
        loop {
            let next = adj[&cur].iter().copied().find(|n| !seen.contains_key(n));
            match next {
                Some(n) => {
                    seen.insert(n, true);
                    chain.push(n);
                    cur = n;
                }
                None => {
                    // A closed ring returns to its start.
                    if chain.len() > 2 && adj[&cur].contains(&s) {
                        chain.push(s);
                    }
                    break;
                }
            }
        }
        if chain.len() >= 2 {
            let mut pts: Vec<Pt> = chain.into_iter().map(cross).collect();
            pts.dedup();
            out.push(pts);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x0: f64, y0: f64, x1: f64, y1: f64) -> ExPolygon {
        ExPolygon::new(Polygon::new(vec![Pt::from_mm(x0, y0), Pt::from_mm(x1, y0), Pt::from_mm(x1, y1), Pt::from_mm(x0, y1)]))
    }

    fn params() -> ArcOverhang {
        ArcOverhang { speed: 5.0, allow: 0.6, min_area: 3.0, spacing: 0.9 }
    }

    #[test]
    fn a_shelf_fills_with_parallel_fronts_from_its_supported_edge() {
        // Layer: 30 x 20; below only the strip y < 5 exists.
        let layer = vec![rect(0.0, 0.0, 30.0, 20.0)];
        let below = vec![rect(0.0, 0.0, 30.0, 5.0)];
        let a = params();
        let ov = overhang(&layer, &below, &a, 0.45);
        assert_eq!(ov.len(), 1);
        let supported = anchor(&layer, &below, &a, 0.45);
        let fs = fronts(&ov[0], &supported, &a, 0.45, 0.2);
        let sp = 0.9 * flow::spacing(0.45, 0.2);
        let want = ((20.0 - 5.0 - 0.6 * 0.45) / sp) as usize;
        assert!(fs.len() >= want - 2 && fs.len() <= want + 2, "{} fronts, want ~{want}", fs.len());
        // Each front is a straight line across, at increasing y.
        let mut last_y = 0.0;
        for f in &fs {
            let ys: Vec<f64> = f.pts.iter().map(|p| p.y_mm()).collect();
            let (lo, hi) = ys.iter().fold((f64::MAX, f64::MIN), |(a, b), y| (a.min(*y), b.max(*y)));
            assert!(hi - lo < 0.2, "front not straight: {lo}..{hi}");
            assert!(lo > last_y - 1e-6, "fronts out of order");
            last_y = lo;
            assert!(f.length_mm() > 28.0, "front {} mm", f.length_mm());
            assert_eq!(f.mode, crate::exp::mode::ARC_FRONT);
        }
    }

    #[test]
    fn a_cap_on_a_post_fills_with_rings() {
        // Layer: a disc r 15; below a post r 4.
        let disc = |r: f64| ExPolygon::new(Polygon::new((0..180).map(|i| { let t = std::f64::consts::TAU * i as f64 / 180.0; Pt::from_mm(50.0 + r * t.cos(), 50.0 + r * t.sin()) }).collect()));
        let a = params();
        let layer = vec![disc(15.0)];
        let ov = overhang(&layer, &[disc(4.0)], &a, 0.45);
        assert_eq!(ov.len(), 1);
        let supported = anchor(&layer, &[disc(4.0)], &a, 0.45);
        let fs = fronts(&ov[0], &supported, &a, 0.45, 0.2);
        // Rings: closed loops of growing radius.
        let closed = fs.iter().filter(|f| f.first().dist(f.last()) as f64 / SCALE < 0.3).count();
        // The rings closest to the rim meet it and end there as arcs.
        assert!(closed as f64 > 0.7 * fs.len() as f64, "{closed} of {} closed", fs.len());
        assert!(fs.iter().take(20).all(|f| f.first().dist(f.last()) as f64 / SCALE < 0.3), "inner rings must close");
        let r = |f: &ExtrusionPath| { let p = f.pts[0]; (p.x_mm() - 50.0).hypot(p.y_mm() - 50.0) };
        assert!(r(&fs[0]) < 5.0 && r(fs.last().unwrap()) > 13.5);
    }
}

/// The pieces of a wall path that lie outside `ov` (the fronts print there).
pub fn clip_walls(walls: Vec<ExtrusionPath>, ov: &[ExPolygon]) -> Vec<ExtrusionPath> {
    if ov.is_empty() {
        return walls;
    }
    let bb = bbox_of(ov);
    let mut out = Vec::with_capacity(walls.len());
    for w in walls {
        let b = Polygon::new(w.pts.clone()).bbox();
        if b.max.x < bb.min.x || b.min.x > bb.max.x || b.max.y < bb.min.y || b.min.y > bb.max.y || !w.z.is_empty() {
            out.push(w);
            continue;
        }
        // Resample, then keep the runs outside the overhang (a variable-width
        // bead's segments keep their flow).
        let mut run: Vec<Pt> = Vec::new();
        let mut run_flow: Vec<f64> = Vec::new();
        let mut cut = false;
        let flush = |run: &mut Vec<Pt>, run_flow: &mut Vec<f64>, out: &mut Vec<ExtrusionPath>| {
            if run.len() >= 2 {
                out.push(ExtrusionPath { pts: std::mem::take(run), flow: std::mem::take(run_flow), closed: false, loop_pts: Vec::new(), dry: Vec::new(), ..w.clone() });
            }
            run.clear();
            run_flow.clear();
        };
        for (si, seg) in w.pts.windows(2).enumerate() {
            let (ax, ay, bx, by) = (seg[0].x_mm(), seg[0].y_mm(), seg[1].x_mm(), seg[1].y_mm());
            let k = ((bx - ax).hypot(by - ay) / 0.3).ceil().max(1.0) as usize;
            for j in 0..k {
                let t = j as f64 / k as f64;
                let q = Pt::from_mm(ax + (bx - ax) * t, ay + (by - ay) * t);
                if ov.iter().any(|e| e.contains(q)) {
                    cut = true;
                    flush(&mut run, &mut run_flow, &mut out);
                } else {
                    if !run.is_empty() && !w.flow.is_empty() {
                        // The segment into q lies on segment si (or ends at its start).
                        run_flow.push(w.flow[if j == 0 { si.saturating_sub(1) } else { si }]);
                    }
                    run.push(q);
                }
            }
        }
        let last = *w.pts.last().unwrap();
        if !ov.iter().any(|e| e.contains(last)) {
            if !run.is_empty() && !w.flow.is_empty() {
                run_flow.push(w.flow[w.flow.len() - 1]);
            }
            run.push(last);
        }
        if !cut {
            out.push(w.clone());
            continue;
        }
        flush(&mut run, &mut run_flow, &mut out);
    }
    out
}
