//! A plan's supports as loose rigid pieces, covered in balls that keep off the part: a coarse set
//! for the sim to run, a finer one for the way-out search and its exact check. A section whose
//! disc reaches into the part's clearance is covered by smaller balls in what room there is.

use crate::grid::DistGrid;
use crate::mesh::PieceMesh;
use makepad_fab_slicer::clip::intersection_ex;
use makepad_fab_slicer::geom::*;
use makepad_fab_toolpath::skeleton::{Chain, Skeleton, SkeletonPiece};

/// The most balls a plate runs in the sim, whatever is on it: its steps go in solving their contacts.
/// A whole tree grown as one shows what it does with fewer, and the check always has all of its fine
/// ones.
const MOST_SIM_BALLS: usize = 3000;
/// Every ball keeps this far off the part at rest (mm), so nothing starts in contact.
const GAP: f32 = 0.1;
/// Below this, a disc's covering stops (mm): the sim's and the check's.
const LEAST_SIM: f32 = 0.4;
const LEAST_FINE: f32 = 0.2;
/// The smallest ball that still stands for a thin pad or foot (mm).
const LEAST_THIN: f32 = 0.12;
/// A chain keeps a section per this many radii of its length.
const SPACING_SIM: f64 = 1.2;
const SPACING_FINE: f64 = 0.5;
/// Pads' balls: rim and inside spacing (mm).
const PAD_SIM: (f64, f64) = (2.0, 2.5);
const PAD_FINE: (f64, f64) = (0.5, 0.75);
/// A normal support region's starting pitch (mm) and the most balls a set may hold.
const PRISM_SIM: (f64, usize) = (1.6, 2000);
const PRISM_FINE: (f64, usize) = (0.8, 8000);

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Ball {
    pub c: [f32; 3],
    pub r: f32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rod {
    pub a: [f32; 3],
    pub b: [f32; 3],
    pub r: f32,
}

/// What a piece breaks into at a joint: a stretch of branch or a pad. A normal support region
/// is one section.
#[derive(Clone, Debug, Default)]
pub struct Section {
    pub balls: Vec<Ball>,
}

#[derive(Clone, Debug, Default)]
pub struct Piece {
    pub name: String,
    /// The sim's solid.
    pub balls: Vec<Ball>,
    /// The coarse solid against other pieces.
    pub rods: Vec<Rod>,
    pub sections: Vec<Section>,
    /// What it prints, mm^3 before its fill.
    pub volume: f64,
    pub mesh: PieceMesh,
    /// What of the plan it is: its chains, pads and footprints.
    pub of: SkeletonPiece,
}

impl Piece {
    pub fn fine_balls(&self) -> Vec<Ball> {
        self.sections.iter().flat_map(|s| s.balls.iter().copied()).collect()
    }
}

/// A support's solid, where the plan's frame puts it on the plate.
struct Fit<'a> {
    grid: &'a DistGrid,
    at: [f64; 2],
}

impl Fit<'_> {
    fn room(&self, p: [f32; 3]) -> f32 {
        self.grid.distance(p) - GAP
    }

    /// A plan point on the plate, its ball clear of the bed.
    fn place(&self, x: f64, y: f64, z: f64, r: f32) -> [f32; 3] {
        [(x + self.at[0]) as f32, (y + self.at[1]) as f32, (z as f32).max(r)]
    }

    /// A disc of radius `r` as one ball if there is room for it, or smaller ones in what room there is.
    fn cover_disc(&self, x: f64, y: f64, z: f64, r: f64, least: f32, out: &mut Vec<Ball>) {
        let r = r as f32;
        let c = self.place(x, y, z, r);
        let room = self.room(c);
        if room >= r {
            out.push(Ball { c, r });
            return;
        }
        let first = out.len();
        let mut tier = r / 2.0;
        while tier >= least {
            let pitch = 1.5 * tier;
            let steps = ((r / pitch).ceil() as i32) + 1;
            for j in -steps..=steps {
                for i in -steps..=steps {
                    let dx = (i as f32 + if j & 1 != 0 { 0.5 } else { 0.0 }) * pitch;
                    let dy = j as f32 * pitch * 0.866;
                    if dx * dx + dy * dy > r * r {
                        continue;
                    }
                    let p = self.place(x + dx as f64, y + dy as f64, z, tier);
                    if out[first..].iter().any(|b| dist(b.c, p) + 0.5 * tier <= b.r) {
                        continue;
                    }
                    if self.room(p) >= tier {
                        out.push(Ball { c: p, r: tier });
                    }
                }
            }
            tier /= 2.0;
        }
        if out.len() == first && room >= LEAST_THIN {
            out.push(Ball { c, r: room });
        }
    }

    /// A ball of radius `r`, or as much of it as there is room for.
    fn thin_ball(&self, x: f64, y: f64, z: f64, r: f32, out: &mut Vec<Ball>) {
        let c = self.place(x, y, z, r);
        let fits = r.min(self.room(c));
        if fits >= LEAST_THIN {
            out.push(Ball { c, r: fits });
        }
    }

    /// Balls over an island at height `z`: along its rings and on a grid inside.
    fn island_balls(&self, e: &ExPolygon, z: f64, r: f32, (rim, inner): (f64, f64), out: &mut Vec<Ball>) {
        for ring in e.rings() {
            for (x, y) in along_ring(ring, rim) {
                self.thin_ball(x, y, z, r, out);
            }
        }
        let bb = e.bbox();
        let (x0, y0, x1, y1) = (bb.min.x_mm(), bb.min.y_mm(), bb.max.x_mm(), bb.max.y_mm());
        let mut y = y0 + inner / 2.0;
        while y < y1 {
            let mut x = x0 + inner / 2.0;
            while x < x1 {
                if e.contains(Pt::from_mm(x, y)) {
                    self.thin_ball(x, y, z, r, out);
                }
                x += inner;
            }
            y += inner;
        }
    }
}

fn dist(a: [f32; 3], b: [f32; 3]) -> f32 {
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt()
}

/// Points along a closed ring, about `step` apart, its corners' neighbourhood included.
fn along_ring(ring: &Polygon, step: f64) -> Vec<(f64, f64)> {
    let mut out = Vec::new();
    let n = ring.pts.len();
    let mut left = 0.0;
    for i in 0..n {
        let (a, b) = (ring.pts[i], ring.pts[(i + 1) % n]);
        let (ax, ay, bx, by) = (a.x_mm(), a.y_mm(), b.x_mm(), b.y_mm());
        let l = (bx - ax).hypot(by - ay);
        while left <= l && l > 0.0 {
            let t = left / l;
            out.push((ax + (bx - ax) * t, ay + (by - ay) * t));
            left += step;
        }
        left -= l;
    }
    out
}

/// A layer's middle.
fn mid(height: (f64, f64)) -> f64 {
    height.0 - height.1 / 2.0
}

/// A chain's sections to keep, as (x, y, z, r): one per `spacing` radii of its length, with
/// its ends.
fn kept(c: &Chain, heights: &[(f64, f64)], spacing: f64) -> Vec<(f64, f64, f64, f64)> {
    let at = |k: usize| {
        let (x, y, r) = c.sections[k];
        (x, y, mid(heights[c.top - k]), r)
    };
    let n = c.sections.len();
    let mut out = Vec::new();
    let mut run = 0.0;
    for k in 0..n {
        let s = at(k);
        if k > 0 {
            let p = at(k - 1);
            run += ((s.0 - p.0).powi(2) + (s.1 - p.1).powi(2) + (s.2 - p.2).powi(2)).sqrt();
        }
        if k == 0 || k + 1 == n || run >= spacing * s.3 {
            out.push(s);
            run = 0.0;
        }
    }
    out
}

/// A normal support region's balls over its footprints (slab index, island): the footprint rings on
/// layers about a pitch apart, the insides at twice the pitch, the pitch widened to fit `most`.
fn region_balls(fit: &Fit, skeleton: &Skeleton, slabs: &[(usize, ExPolygon)], (pitch, most): (f64, usize), out: &mut Vec<Ball>) {
    let perimeter: f64 = slabs.iter().flat_map(|(_, e)| e.rings()).map(|ring| {
        let n = ring.pts.len();
        (0..n).map(|i| {
            let (a, b) = (ring.pts[i], ring.pts[(i + 1) % n]);
            (a.x_mm() - b.x_mm()).hypot(a.y_mm() - b.y_mm())
        }).sum::<f64>()
    }).sum();
    let area: f64 = slabs.iter().map(|(_, e)| e.area_mm2()).sum();
    let thick: f64 = skeleton.slabs.iter().map(|s| s.height).sum::<f64>() / skeleton.slabs.len().max(1) as f64;
    let mut pitch = pitch;
    // Per slab the rings take perimeter / pitch and the insides area / (2 pitch)^2; a slab in
    // every pitch / thick is drawn.
    while (perimeter / pitch + area / (4.0 * pitch * pitch)) * thick / pitch > most as f64 {
        pitch *= 1.25;
    }
    let r = (0.7 * pitch) as f32;
    let mut last_z = f64::MIN;
    let mut chosen = usize::MAX;
    for (s, e) in slabs {
        let z = skeleton.slabs[*s].z - skeleton.slabs[*s].height / 2.0;
        if *s != chosen && z - last_z >= pitch {
            chosen = *s;
            last_z = z;
        }
        if *s == chosen {
            fit.island_balls(e, z, r, (pitch, 2.0 * pitch), out);
        }
    }
}

fn root(lead: &mut [usize], mut i: usize) -> usize {
    while lead[i] != i {
        lead[i] = lead[lead[i]];
        i = lead[i];
    }
    i
}

/// Whether the straight line between two points stays out of the part.
fn clear_between(a: [f32; 3], b: [f32; 3], grid: &DistGrid) -> bool {
    let length = dist(a, b).max(1e-6);
    let mut along = 0.0;
    while along < length {
        let t = along / length;
        let p = std::array::from_fn(|k| a[k] + (b[k] - a[k]) * t);
        let room = grid.distance(p);
        if room <= 0.0 {
            return false;
        }
        along += room.max(grid.cell_at(p));
    }
    grid.distance(b) > 0.0
}

/// A chain's balls by what is joined. The plan's chain runs on where the part is in its way, and
/// what prints on each side of a wall is not joined there. Balls this far apart, with no part
/// between them, are one thing.
fn joined(balls: Vec<Ball>, within: f32, grid: &DistGrid) -> Vec<Section> {
    let mut lead: Vec<usize> = (0..balls.len()).collect();
    for i in 1..balls.len() {
        for j in 0..i {
            if dist(balls[i].c, balls[j].c) <= balls[i].r + balls[j].r + within {
                let (a, b) = (root(&mut lead, i), root(&mut lead, j));
                if a != b && clear_between(balls[i].c, balls[j].c, grid) {
                    lead[a.max(b)] = a.min(b);
                }
            }
        }
    }
    let mut sections: Vec<Section> = Vec::new();
    let mut index = vec![usize::MAX; balls.len()];
    for (i, ball) in balls.into_iter().enumerate() {
        let r = root(&mut lead, i);
        if index[r] == usize::MAX {
            index[r] = sections.len();
            sections.push(Section::default());
        }
        sections[index[r]].balls.push(ball);
    }
    sections
}

/// Pad islands stacked on each other, as lists of (layer, island) indices into `roofs`.
fn pad_stacks(roofs: &[(usize, ExPolygon)]) -> Vec<Vec<usize>> {
    let mut lead: Vec<usize> = (0..roofs.len()).collect();
    for i in 0..roofs.len() {
        for j in i + 1..roofs.len() {
            if roofs[j].0 != roofs[i].0 + 1 || !roofs[i].1.bbox().overlaps(&roofs[j].1.bbox()) {
                continue;
            }
            if intersection_ex(std::slice::from_ref(&roofs[i].1), std::slice::from_ref(&roofs[j].1)).iter().any(|e| e.area_mm2() > 1e-4) {
                let (a, b) = (root(&mut lead, i), root(&mut lead, j));
                lead[a.max(b)] = a.min(b);
            }
        }
    }
    let mut stacks: Vec<Vec<usize>> = Vec::new();
    let mut index = vec![usize::MAX; roofs.len()];
    for i in 0..roofs.len() {
        let r = root(&mut lead, i);
        if index[r] == usize::MAX {
            index[r] = stacks.len();
            stacks.push(Vec::new());
        }
        stacks[index[r]].push(i);
    }
    stacks
}

fn piece_of(fit: &Fit, skeleton: &Skeleton, of: &SkeletonPiece, name: String) -> Piece {
    let mut piece = Piece { name, of: of.clone(), ..Default::default() };
    let heights = &skeleton.heights;
    for &i in &of.chains {
        let c = &skeleton.chains[i];
        let mut fine = Vec::new();
        let mut within = 0.0f64;
        for (x, y, z, r) in kept(c, heights, SPACING_FINE) {
            fit.cover_disc(x, y, z, r, LEAST_FINE, &mut fine);
            within = within.max(SPACING_FINE * r);
        }
        let sim = kept(c, heights, SPACING_SIM);
        let mut run: Vec<([f32; 3], f32)> = Vec::new();
        for &(x, y, z, r) in &sim {
            let start = piece.balls.len();
            fit.cover_disc(x, y, z, r, LEAST_SIM, &mut piece.balls);
            let whole = piece.balls.len() == start + 1 && piece.balls[start].r == r as f32;
            if whole {
                run.push((piece.balls[start].c, r as f32));
            } else {
                piece.mesh.tube(&run);
                run.clear();
                for b in &piece.balls[start..] {
                    piece.mesh.ball(b.c, b.r);
                }
            }
        }
        piece.mesh.tube(&run);
        rods_of(fit, c, heights, &mut piece.rods);
        piece.sections.extend(joined(fine, within as f32, fit.grid));
        piece.volume += c.sections.iter().enumerate().map(|(k, s)| std::f64::consts::PI * s.2 * s.2 * heights[c.top - k].1).sum::<f64>();
    }
    for stack in pad_stacks(&of.roofs) {
        let mut fine = Vec::new();
        for (n, &i) in stack.iter().enumerate() {
            let (l, e) = &of.roofs[i];
            let h = heights[*l];
            let r = (0.75 * h.1 as f32).max(0.2);
            fit.island_balls(e, mid(h), r, PAD_FINE, &mut fine);
            if n % 2 == 0 {
                fit.island_balls(e, mid(h), r, PAD_SIM, &mut piece.balls);
            }
            piece.mesh.extrude(e, fit.at, h.0 - h.1, h.0);
            piece.volume += e.area_mm2() * h.1;
        }
        piece.sections.push(Section { balls: fine });
    }
    if !of.slabs.is_empty() {
        let mut fine = Vec::new();
        region_balls(fit, skeleton, &of.slabs, PRISM_FINE, &mut fine);
        region_balls(fit, skeleton, &of.slabs, PRISM_SIM, &mut piece.balls);
        for (s, e) in &of.slabs {
            let slab = &skeleton.slabs[*s];
            piece.mesh.extrude(e, fit.at, slab.z - slab.height, slab.z);
            piece.volume += e.area_mm2() * slab.height;
        }
        piece.sections.push(Section { balls: fine });
    }
    piece
}

/// At most `most` balls, spread as they were: the largest in each cube of a pitch that grows
/// until few enough are left.
fn thin_out(balls: &mut Vec<Ball>, most: usize) {
    if balls.len() <= most {
        return;
    }
    balls.sort_by(|a, b| b.r.total_cmp(&a.r));
    let all = std::mem::take(balls);
    let mut pitch = 1.0;
    loop {
        let mut taken = std::collections::HashSet::new();
        *balls = all.iter().copied().filter(|b| taken.insert(b.c.map(|v| (v / pitch).floor() as i32))).collect();
        if balls.len() <= most {
            return;
        }
        pitch *= 1.15;
    }
}

/// A chain's capsules, a few radii long and a little thinner than it, where they are clear of
/// the part.
fn rods_of(fit: &Fit, c: &Chain, heights: &[(f64, f64)], out: &mut Vec<Rod>) {
    const LEAST_ROD: f32 = 0.2;
    let stops = {
        let mut stops = Vec::new();
        let mut run = 0.0;
        let all: Vec<_> = kept(c, heights, 0.5);
        for (k, s) in all.iter().enumerate() {
            if k > 0 {
                let p = all[k - 1];
                run += ((s.0 - p.0).powi(2) + (s.1 - p.1).powi(2) + (s.2 - p.2).powi(2)).sqrt();
            }
            if k == 0 || k + 1 == all.len() || run >= (6.0 * s.3).max(4.0) {
                stops.push(*s);
                run = 0.0;
            }
        }
        stops
    };
    for w in stops.windows(2) {
        let r = (0.8 * w[0].3.min(w[1].3)) as f32;
        if r < LEAST_ROD {
            continue;
        }
        let a = fit.place(w[0].0, w[0].1, w[0].2, r);
        let b = fit.place(w[1].0, w[1].1, w[1].2, r);
        let middle = std::array::from_fn(|k| (a[k] + b[k]) / 2.0);
        if [a, middle, b].iter().all(|&p| fit.room(p) >= r) {
            out.push(Rod { a, b, r });
        }
    }
}

/// The distance between segments, by the closest points on each.
fn segments_gap(a: &Rod, b: &Rod) -> f32 {
    let sub = |p: [f32; 3], q: [f32; 3]| [p[0] - q[0], p[1] - q[1], p[2] - q[2]];
    let dot = |p: [f32; 3], q: [f32; 3]| p[0] * q[0] + p[1] * q[1] + p[2] * q[2];
    let (d1, d2, r) = (sub(a.b, a.a), sub(b.b, b.a), sub(a.a, b.a));
    let (aa, e, f) = (dot(d1, d1).max(1e-9), dot(d2, d2).max(1e-9), dot(d2, r));
    let (c, m) = (dot(d1, r), dot(d1, d2));
    let denom = aa * e - m * m;
    let mut s = if denom > 1e-9 { ((m * f - c * e) / denom).clamp(0.0, 1.0) } else { 0.0 };
    let mut t = (m * s + f) / e;
    if t < 0.0 {
        t = 0.0;
        s = (-c / aa).clamp(0.0, 1.0);
    } else if t > 1.0 {
        t = 1.0;
        s = ((m - c) / aa).clamp(0.0, 1.0);
    }
    let p: [f32; 3] = std::array::from_fn(|k| a.a[k] + d1[k] * s);
    let q: [f32; 3] = std::array::from_fn(|k| b.a[k] + d2[k] * t);
    dist(p, q)
}

/// The rods of a whole plate: every one costs the sim on every step it moves.
const MOST_RODS: usize = 300;
/// A piece keeps this many at the least, whatever the plate.
const LEAST_RODS: usize = 8;

/// What the sim runs of a plate. Its biggest pieces give up balls first, down to one level for all
/// of them: a small piece has few to lose, and thinned it walks through walls. Rods are for pieces meeting
/// each other, coarsely: none on a plate of one piece, else the biggest of each piece. Rods of
/// different pieces that overlap where they were printed would throw the pieces apart, and the
/// later piece's go.
pub fn coarse(pieces: &mut [&mut Piece]) {
    let mut sizes: Vec<usize> = pieces.iter().map(|p| p.balls.len()).collect();
    sizes.sort_unstable();
    let (mut left, mut level) = (MOST_SIM_BALLS, MOST_SIM_BALLS);
    for (i, &size) in sizes.iter().enumerate() {
        level = (left / (sizes.len() - i)).max(1);
        if size > level {
            break;
        }
        left -= size;
    }
    for piece in pieces.iter_mut() {
        thin_out(&mut piece.balls, level);
    }
    if let [one] = pieces {
        one.rods.clear();
        return;
    }
    let each = (MOST_RODS / pieces.len().max(1)).max(LEAST_RODS);
    let size = |r: &Rod| r.r * (0..3).map(|k| (r.a[k] - r.b[k]).powi(2)).sum::<f32>().sqrt();
    for piece in pieces.iter_mut() {
        piece.rods.sort_by(|a, b| size(b).total_cmp(&size(a)));
        piece.rods.truncate(each);
    }
    for i in 1..pieces.len() {
        let (before, rest) = pieces.split_at_mut(i);
        rest[0].rods.retain(|r| !before.iter().any(|p| p.rods.iter().any(|o| segments_gap(r, o) < r.r + o.r + 0.05)));
    }
}

/// The part's supports as pieces, named after `object`. `at`: where the plan's frame sits on the
/// plate. `grid`: the part's distance.
pub fn pieces_of(object: &str, skeleton: &Skeleton, at: [f64; 2], grid: &DistGrid) -> Vec<Piece> {
    let fit = Fit { grid, at };
    skeleton.pieces().iter().enumerate().map(|(k, p)| piece_of(&fit, skeleton, p, format!("{object} support {}", k + 1))).filter(|p| !p.balls.is_empty()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::solid::Solid;
    use makepad_csg_math::Vec3d;
    use makepad_csg_sdf::Sdf3;
    use makepad_fab_toolpath::par::Ctl;
    use std::sync::Arc;

    /// A plate's biggest pieces give up balls first, and a small one keeps what it has.
    #[test]
    fn a_plate_shares_its_balls() {
        let many = |n: usize, y: f32| Piece { balls: (0..n).map(|i| Ball { c: [i as f32 * 0.5, y, 0.0], r: 0.2 }).collect(), ..Default::default() };
        let (mut big, mut other, mut small) = (many(8000, 0.0), many(4000, 100.0), many(30, 200.0));
        coarse(&mut [&mut big, &mut other, &mut small]);
        assert!(big.balls.len() + other.balls.len() + small.balls.len() <= MOST_SIM_BALLS);
        assert!(big.balls.len() > 1000 && other.balls.len() > 1000, "{} {}", big.balls.len(), other.balls.len());
        assert_eq!(small.balls.len(), 30);
    }

    /// A wall across z, 2 mm thick, up to x = 6.
    struct Wall;

    impl Sdf3 for Wall {
        fn distance(&self, p: Vec3d) -> f64 {
            ((p.z - 5.0).abs() - 1.0).max(p.x - 6.0)
        }
    }

    #[test]
    fn what_a_wall_parts_is_two_sections_and_what_goes_round_it_is_one() {
        let grid = DistGrid::check(Ctl::default(), &Solid::of_distance(Arc::new(Wall), [0.0; 3], [10.0; 3]), 0.2, crate::grid::Contact::Fast).unwrap();
        let ball = |x: f32, z: f32| Ball { c: [x, 5.0, z], r: 0.5 };
        let through: Vec<Ball> = [2.0, 3.0, 7.0, 8.0].iter().map(|&z| ball(3.0, z)).collect();
        assert_eq!(joined(through.clone(), 4.0, &grid).len(), 2);
        let mut round = through;
        round.extend([ball(5.0, 3.0), ball(7.0, 3.5), ball(7.0, 5.0), ball(7.0, 6.5), ball(5.0, 7.0)]);
        assert_eq!(joined(round, 1.5, &grid).len(), 1);
    }
}
