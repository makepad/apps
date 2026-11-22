//! What a support costs and how well it stands, told the same way for any support's paths (the
//! prototype's measures): its filament, its print time on a simple printer, and the worst wobble
//! the nozzle's side push gives any layer of it while that layer prints.

use super::material::SupportPath;
use std::collections::HashMap;

/// The filament's density (g/mm3).
const DENSITY: f64 = 1.24e-3;
/// The printer: top speed (mm/s), acceleration (mm/s2), the speed it takes a right-angle corner
/// at (mm/s), and what starting a path costs (s).
const SPEED: f64 = 100.0;
const ACCEL: f64 = 5000.0;
const CORNER90: f64 = 9.0;
const START: f64 = 0.35;
/// The nozzle's side push (N), and the support material's stiffness (N/mm2).
const PUSH: f64 = 0.15;
const MODULUS: f64 = 2000.0;
/// Paths this close (mm) stand as one column.
pub const COLUMN: f64 = 1.2;
/// A column stands on the one under it that this much (mm) past the reach meets most of it.
const UNDER: f64 = 0.6;

fn mm(p: &SupportPath) -> Vec<(f64, f64)> {
    p.pts.iter().map(|q| (q.x_mm(), q.y_mm())).collect()
}

fn length(v: &[(f64, f64)]) -> f64 {
    v.windows(2).map(|s| (s[1].0 - s[0].0).hypot(s[1].1 - s[0].1)).sum()
}

/// The filament (g) `layers` take, each layer `h[l]` (mm) high.
pub fn grams(layers: &[Vec<SupportPath>], h: &[f64]) -> f64 {
    layers.iter().zip(h).map(|(l, &h)| l.iter().map(|p| length(&mm(p)) * p.width * h).sum::<f64>()).sum::<f64>() * DENSITY
}

/// How long (s) printing a path takes: from rest to rest, slowing for its corners.
pub fn path_time(v: &[(f64, f64)]) -> f64 {
    let n = v.len();
    if n < 2 {
        return 0.0;
    }
    let seg: Vec<(f64, f64, f64)> = v.windows(2).map(|s| {
        let (ex, ey) = (s[1].0 - s[0].0, s[1].1 - s[0].1);
        let l = ex.hypot(ey);
        if l > 1e-12 { (l, ex / l, ey / l) } else { (l, 0.0, 0.0) }
    }).collect();
    let s90 = std::f64::consts::FRAC_1_SQRT_2;
    let jd = CORNER90 * CORNER90 / (ACCEL * (s90 / (1.0 - s90)));
    let vmax2 = SPEED * SPEED;
    let mut v2 = vec![vmax2; n];
    for i in 1..n - 1 {
        let ((la, ax, ay), (lb, bx, by)) = (seg[i - 1], seg[i]);
        if la < 1e-12 || lb < 1e-12 {
            continue;
        }
        let c = (ax * bx + ay * by).clamp(-1.0, 1.0);
        let (s, sh) = ((0.5 * (1.0 + c)).sqrt(), (0.5 * (1.0 - c)).sqrt());
        if sh < 1e-9 {
            continue;
        }
        let tanh = s / sh;
        let lim = if s > 1.0 - 1e-12 { vmax2 } else { ACCEL * jd * s / (1.0 - s) };
        v2[i] = lim.min(0.5 * ACCEL * la * tanh).min(0.5 * ACCEL * lb * tanh).min(vmax2);
    }
    v2[0] = 0.0;
    v2[n - 1] = 0.0;
    for i in 0..n - 1 {
        v2[i + 1] = v2[i + 1].min(v2[i] + 2.0 * ACCEL * seg[i].0);
    }
    for i in (0..n - 1).rev() {
        v2[i] = v2[i].min(v2[i + 1] + 2.0 * ACCEL * seg[i].0);
    }
    let mut t = 0.0;
    for i in 0..n - 1 {
        let l = seg[i].0;
        if l < 1e-12 {
            continue;
        }
        let (v0, v1) = (v2[i].sqrt(), v2[i + 1].sqrt());
        let vp2 = vmax2.min((2.0 * ACCEL * l + v2[i] + v2[i + 1]) / 2.0);
        let vp = vp2.sqrt();
        let (da, dd) = ((vp2 - v2[i]) / (2.0 * ACCEL), (vp2 - v2[i + 1]) / (2.0 * ACCEL));
        t += (vp - v0) / ACCEL + (vp - v1) / ACCEL + (l - da - dd).max(0.0) / vp;
    }
    t
}

/// How long (min) printing `layers` takes, each path started once.
pub fn minutes(layers: &[Vec<SupportPath>]) -> f64 {
    layers.iter().flatten().map(|p| path_time(&mm(p)) + START).sum::<f64>() / 60.0
}

/// The least second moment of area (mm4) of paths printed `width` wide, each a strip along its
/// centreline.
pub fn section(paths: &[(&[(f64, f64)], f64)]) -> f64 {
    let (mut a, mut sx, mut sy, mut ixx, mut iyy, mut ixy) = (0.0, 0.0, 0.0, 0.0, 0.0, 0.0);
    for &(v, t) in paths {
        for s in v.windows(2) {
            let ((x1, y1), (x2, y2)) = (s[0], s[1]);
            let w = t * (x2 - x1).hypot(y2 - y1);
            a += w;
            sx += w * (x1 + x2) / 2.0;
            sy += w * (y1 + y2) / 2.0;
            iyy += w * (x1 * x1 + x1 * x2 + x2 * x2) / 3.0;
            ixx += w * (y1 * y1 + y1 * y2 + y2 * y2) / 3.0;
            ixy += w * (2.0 * x1 * y1 + x1 * y2 + x2 * y1 + 2.0 * x2 * y2) / 6.0;
        }
    }
    if a == 0.0 {
        return 0.0;
    }
    let (cx, cy) = (sx / a, sy / a);
    let (ixx, iyy, ixy) = (ixx - a * cy * cy, iyy - a * cx * cx, ixy - a * cx * cy);
    (ixx + iyy) / 2.0 - (((ixx - iyy) / 2.0).powi(2) + ixy * ixy).sqrt()
}

/// A layer's paths in cells, to find which come near a point.
struct Cells {
    cell: f64,
    map: HashMap<(i64, i64), Vec<(usize, (f64, f64), (f64, f64))>>,
}

impl Cells {
    fn new(paths: &[Vec<(f64, f64)>], cell: f64) -> Cells {
        let mut map: HashMap<(i64, i64), Vec<_>> = HashMap::new();
        for (i, v) in paths.iter().enumerate() {
            for s in v.windows(2) {
                let (a, b) = (s[0], s[1]);
                let (i0, i1) = ((a.0.min(b.0) / cell).floor() as i64, (a.0.max(b.0) / cell).floor() as i64);
                let (j0, j1) = ((a.1.min(b.1) / cell).floor() as i64, (a.1.max(b.1) / cell).floor() as i64);
                for ci in i0..=i1 {
                    for cj in j0..=j1 {
                        map.entry((ci, cj)).or_default().push((i, a, b));
                    }
                }
            }
        }
        Cells { cell, map }
    }

    /// The paths with a segment within `d` (mm) of `p`.
    fn near(&self, p: (f64, f64), d: f64, out: &mut Vec<usize>) {
        let r = (d / self.cell).ceil() as i64;
        let (i0, j0) = ((p.0 / self.cell).floor() as i64, (p.1 / self.cell).floor() as i64);
        for ci in i0 - r..=i0 + r {
            for cj in j0 - r..=j0 + r {
                for &(i, a, b) in self.map.get(&(ci, cj)).map_or(&[][..], |v| &v[..]) {
                    let (vx, vy, wx, wy) = (b.0 - a.0, b.1 - a.1, p.0 - a.0, p.1 - a.1);
                    let l2 = vx * vx + vy * vy;
                    let t = if l2 > 1e-18 { ((wx * vx + wy * vy) / l2).clamp(0.0, 1.0) } else { 0.0 };
                    if (wx - vx * t).hypot(wy - vy * t) <= d && !out.contains(&i) {
                        out.push(i);
                    }
                }
            }
        }
    }
}

/// The worst wobble (mm) of `layers` (each `h[l]` high, its top at `z[l]`), the layer it is on
/// and the middle of the column that wobbles (mm), as [`wobbles`] finds it.
pub fn wobble(layers: &[Vec<SupportPath>], z: &[f64], h: &[f64], reach: f64) -> (f64, usize, (f64, f64)) {
    let Columns { of: col, wobble: wob, pts } = columns(layers, z, h, reach);
    let mut worst = (0.0, 0, (0.0, 0.0));
    for k in 0..layers.len() {
        for (c, &w) in wob[k].iter().enumerate() {
            if w > worst.0 {
                let mine: Vec<(f64, f64)> = pts[k].iter().zip(&col[k]).filter(|&(_, &cc)| cc == c).flat_map(|(v, _)| v.iter().copied()).collect();
                let n = mine.len().max(1) as f64;
                worst = (w, k, mine.iter().fold((0.0, 0.0), |a, p| (a.0 + p.0 / n, a.1 + p.1 / n)));
            }
        }
    }
    worst
}

/// Per layer of `layers` (each `h[l]` high, its top at `z[l]`), how far (mm) each path's column
/// wobbles as that layer prints: the nozzle pushes the column under it sideways, and the column
/// bends by the push over the material's stiffness times the sum of (z - s)^2 / I(s) ds down
/// it. A layer's columns are its paths that come together; each stands on the column under it
/// that `reach` (mm) and a little more meets most of it.
pub fn wobbles(layers: &[Vec<SupportPath>], z: &[f64], h: &[f64], reach: f64) -> Vec<Vec<f64>> {
    let Columns { of: col, wobble: wob, .. } = columns(layers, z, h, reach);
    col.iter().zip(&wob).map(|(c, w)| c.iter().map(|&cc| w[cc]).collect()).collect()
}

/// Per layer: each path's column, each column's wobble (mm) as the layer prints, and each path's
/// points (mm).
struct Columns {
    of: Vec<Vec<usize>>,
    wobble: Vec<Vec<f64>>,
    pts: Vec<Vec<Vec<(f64, f64)>>>,
}

/// The columns of `layers`, as [`wobbles`] finds them.
fn columns(layers: &[Vec<SupportPath>], z: &[f64], h: &[f64], reach: f64) -> Columns {
    // per layer: each path's column, and each column's I
    let mut cols: Vec<(Vec<usize>, Vec<f64>)> = Vec::with_capacity(layers.len());
    let mut pts: Vec<Vec<Vec<(f64, f64)>>> = Vec::with_capacity(layers.len());
    let mut cells: Vec<Cells> = Vec::with_capacity(layers.len());
    for l in layers {
        let v: Vec<Vec<(f64, f64)>> = l.iter().map(mm).collect();
        let c = Cells::new(&v, 0.5);
        let mut id: Vec<usize> = (0..v.len()).collect();
        fn find(id: &mut [usize], i: usize) -> usize {
            let mut r = i;
            while id[r] != r {
                r = id[r];
            }
            id[i] = r;
            r
        }
        let mut near = Vec::new();
        for (i, p) in v.iter().enumerate() {
            for &q in p {
                near.clear();
                c.near(q, COLUMN, &mut near);
                for &j in &near {
                    let (a, b) = (find(&mut id, i), find(&mut id, j));
                    if a != b {
                        id[a.max(b)] = a.min(b);
                    }
                }
            }
        }
        let roots: Vec<usize> = (0..v.len()).map(|i| find(&mut id, i)).collect();
        let mut index: HashMap<usize, usize> = HashMap::new();
        let col: Vec<usize> = roots.iter().map(|&r| { let n = index.len(); *index.entry(r).or_insert(n) }).collect();
        let mut members: Vec<Vec<(&[(f64, f64)], f64)>> = vec![Vec::new(); index.len()];
        for (i, &c) in col.iter().enumerate() {
            members[c].push((&v[i][..], l[i].width));
        }
        let inertia: Vec<f64> = members.iter().map(|m| section(m)).collect();
        cols.push((col, inertia));
        pts.push(v);
        cells.push(c);
    }
    // each column's column under it
    let mut down: Vec<Vec<Option<usize>>> = Vec::with_capacity(layers.len());
    for k in 0..layers.len() {
        let n = cols[k].1.len();
        if k == 0 || cols[k - 1].1.is_empty() {
            down.push(vec![None; n]);
            continue;
        }
        let mut hits: Vec<HashMap<usize, usize>> = vec![HashMap::new(); n];
        let mut near = Vec::new();
        for (i, p) in pts[k].iter().enumerate() {
            let mut below: Vec<usize> = Vec::new();
            for &q in p {
                near.clear();
                cells[k - 1].near(q, reach + UNDER, &mut near);
                for &j in &near {
                    if !below.contains(&j) {
                        below.push(j);
                    }
                }
            }
            for j in below {
                *hits[cols[k].0[i]].entry(cols[k - 1].0[j]).or_default() += 1;
            }
        }
        down.push(hits.iter().map(|m| m.iter().max_by_key(|&(c, n)| (*n, std::cmp::Reverse(*c))).map(|(&c, _)| c)).collect());
    }
    let wob: Vec<Vec<f64>> = (0..layers.len()).map(|k| {
        (0..cols[k].1.len()).map(|c| {
            let (mut w, mut j, mut q) = (0.0, k, Some(c));
            while let Some(cc) = q {
                let d = z[k] - z[j];
                w += d * d * h[j] / cols[j].1[cc].max(1e-6);
                q = down[j][cc];
                if j == 0 {
                    break;
                }
                j -= 1;
            }
            PUSH / MODULUS * w
        }).collect()
    }).collect();
    Columns { of: cols.into_iter().map(|c| c.0).collect(), wobble: wob, pts }
}

#[cfg(test)]
mod tests {
    use super::super::material::Role;
    use super::*;
    use makepad_fab_slicer::geom::Pt;
    use std::f64::consts::PI;

    fn ring(r: f64) -> SupportPath {
        let pts = (0..=96).map(|i| { let a = 2.0 * PI * i as f64 / 96.0; Pt::from_mm(r * a.cos(), r * a.sin()) }).collect();
        SupportPath { pts, width: 0.45, role: Role::Body, support: 0, frond: 0 }
    }

    /// A ring's section is pi r^3 t, and a straight column of them wobbles by the push over the
    /// stiffness times the sum of (z - s)^2 ds / I down it.
    #[test]
    fn a_column_of_rings_wobbles_as_a_beam() {
        let (r, n, h) = (2.0, 100, 0.2);
        let i = section(&[(&mm(&ring(r))[..], 0.45)]);
        assert!((i - PI * r * r * r * 0.45).abs() < 0.01 * i, "ring I {i}");
        let layers: Vec<Vec<SupportPath>> = (0..n).map(|_| vec![ring(r)]).collect();
        let z: Vec<f64> = (1..=n).map(|k| k as f64 * h).collect();
        let (w, at, _) = wobble(&layers, &z, &vec![h; n], 0.2);
        let want = PUSH / MODULUS * z.iter().map(|s| (z[n - 1] - s).powi(2) * h).sum::<f64>() / i;
        assert_eq!(at, n - 1);
        assert!((w - want).abs() < 0.02 * want, "wobble {w} against {want}");
    }

    /// Two columns apart each stand on their own: a thin one beside a stiff one wobbles as thin.
    #[test]
    fn columns_apart_stand_on_their_own() {
        let far = |r: f64, dx: f64| { let mut p = ring(r); for q in &mut p.pts { *q = Pt::from_mm(q.x_mm() + dx, q.y_mm()); } p };
        let n = 60;
        let layers: Vec<Vec<SupportPath>> = (0..n).map(|_| vec![far(0.6, 0.0), far(4.0, 20.0)]).collect();
        let z: Vec<f64> = (1..=n).map(|k| k as f64 * 0.2).collect();
        let thin = wobble(&(0..n).map(|_| vec![far(0.6, 0.0)]).collect::<Vec<_>>(), &z, &vec![0.2; n], 0.2).0;
        let both = wobble(&layers, &z, &vec![0.2; n], 0.2).0;
        assert!((both - thin).abs() < 1e-9 * thin.max(1.0), "{both} against {thin}");
    }

    /// A path's time is its length at speed plus what speeding up and slowing down cost.
    #[test]
    fn a_straight_path_takes_its_length_at_speed_and_its_ramps() {
        let t = path_time(&[(0.0, 0.0), (100.0, 0.0)]);
        let ramp = SPEED / ACCEL;
        assert!((t - (100.0 / SPEED + ramp)).abs() < 1e-9, "{t}");
    }
}
