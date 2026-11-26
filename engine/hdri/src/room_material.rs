//! The room's surface materials (`RoomParams::floor_material`, `wall_material`, `ceiling_material`): procedural, in
//! metres on the surface's own plane (`u` across, `v` along: on the floor and the ceiling x and z, on a wall along it
//! and up from the floor), seen at a footprint `foot` (m: what one texel of the map covers there).
//!
//! Each material is a pattern round the surface's own colour (`RoomParams::floor`, `wall`, `ceiling`): its mean over
//! any area larger than its pattern is that colour, so the room's bounce (its mean albedo) and the colour controls keep
//! meaning what they did, and None (no material) is the flat colour itself. Each gives an albedo and, for the floor's
//! mirror, a factor on its gloss and a roughness added (joints, grain and grime mirror less).
//!
//! Every detail is band-limited: a feature of wavelength `lam` fades into its own mean as the footprint passes a
//! quarter to a half of it (`lod`), so a surface far off or at a grazing angle is its mean and never shimmers. The
//! relief shows as what it does to the albedo (a recessed joint's shade, a bevel's dark line), not through the
//! normal: the room's light on a plane is one smooth grid (`room.rs`, plan P3), which a tilted normal would have to
//! sum again per texel.
//!
//! Wear (`RoomParams::wear`, 0 new to 1 well used) lays the room's own geometry over it (`Weather`): grime and dust
//! gather where the floor meets a wall, a grime band and kick marks run along a wall's foot, and the materials age in
//! their own way (paint peels, plaster takes stains and leaks from the ceiling's seam, brick keeps remnants of plaster,
//! a concrete floor takes oil and repairs).
//!
//! The techniques follow the references the owner pointed to (a survey of material add-ons: a tile
//! coordinate core with a random per element and a texture read at an offset per element, Voronoi edge distance for
//! terrazzo's angular chips, masks from the distance to a seam with a wandering radius, FBM thresholded into patches
//! with a lip), written our own way; the look targets are photographs of real walls and floors.
use crate::noise::hash3_u32;
use crate::patterns::{
    curtain, edge, fbm, from_holes, gl, gn, grunge, h01, kick_marks, leak_columns, lod, mix3, mul3, over, runs, seam_grime, skirting_band, smooth,
    water_stain, wet_stain,
};
use makepad_draw::*;

/// What a surface of the room can be made of (`ROOM_MATERIAL_NAMES` after "none").
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RoomMaterial {
    /// Paint on plaster: a roller's stipple, a trowel's long undulation, touch-up patches; with wear stains, leaks
    /// from the ceiling's seam and peeling paint.
    Plaster,
    /// Bare brick in running bond: every brick its own tone and hue, lighter recessed mortar, worn and chipped edges;
    /// with wear remnants of plaster and a pale bloom of salts low on the wall.
    Brick,
    /// Brick under paint (the surface's colour): the bond shows through as shade in the joints and a little per
    /// brick; with wear the paint peels off to the brick.
    PaintedBrick,
    /// Concrete cast against plywood: panel seams, tie holes, bug holes, the pour's mottle and streaks.
    FormConcrete,
    /// A ground and sealed concrete floor: mottle at three scales, cut aggregate, cloudy burnish, hairline cracks, saw
    /// joints every 4 m; with wear oil stains and patched repairs.
    PolishedConcrete,
    /// Terrazzo: angular chips of marble in two sizes, white, grey, black and rose, in a binder of the surface's colour.
    Terrazzo,
    /// A painted floor (epoxy): even colour, satin, with scuffs, fine scratches and rubber marks.
    Epoxy,
    /// Long lacquered planks: staggered boards each its own tone and hue, flat-sawn grain, pores, bevelled edges.
    Planks,
    /// Oak blocks in a 90 degree herringbone, 7 x 42 cm.
    Herringbone,
    /// Oak boards in a chevron: mitred ends meeting on straight centre lines, 9 cm boards in 60 cm columns.
    Chevron,
    /// Basket weave: squares of three slats, turned a quarter between neighbours.
    BasketWeave,
}

impl RoomMaterial {
    /// The material a name of `ROOM_MATERIAL_NAMES` stands for; None for "none" or a name it does not know.
    pub fn from_name(name: &str) -> Option<RoomMaterial> {
        Some(match name.trim().to_ascii_lowercase().as_str() {
            "plaster" => RoomMaterial::Plaster,
            "brick" => RoomMaterial::Brick,
            "painted_brick" => RoomMaterial::PaintedBrick,
            "form_concrete" => RoomMaterial::FormConcrete,
            "polished_concrete" => RoomMaterial::PolishedConcrete,
            "terrazzo" => RoomMaterial::Terrazzo,
            "epoxy" => RoomMaterial::Epoxy,
            "planks" => RoomMaterial::Planks,
            "herringbone" => RoomMaterial::Herringbone,
            "chevron" => RoomMaterial::Chevron,
            "basket_weave" => RoomMaterial::BasketWeave,
            _ => return None,
        })
    }

    /// Its gloss and roughness change from point to point (a floor's mirror reads them).
    fn wood(self) -> bool {
        matches!(self, RoomMaterial::Planks | RoomMaterial::Herringbone | RoomMaterial::Chevron | RoomMaterial::BasketWeave)
    }
}

/// One point of a surface: its albedo, a factor on the floor's gloss (mean about 1) and roughness added to it.
#[derive(Clone, Copy, Debug)]
pub struct Surface {
    pub albedo: Vec3f,
    pub gloss: f32,
    pub roughness: f32,
}

/// Which of the room's surfaces a point lies on, for its wear.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Floor,
    Wall,
    Ceiling,
}

/// The room's geometry at a point, for wear: the surface, the distance to the nearest seam with a wall (m; on a
/// wall its height over the floor), on a wall the distance down from the ceiling (infinite on the floor and the
/// ceiling), and how worn the room is (0..1); the distance to the nearest open wall (a mouth, where rain and daylight
/// come in; infinite in a closed room), and for each wall its distance when open (infinite when closed) and the point's
/// place along its edge (m), for the leaks.
#[derive(Clone, Copy, Debug)]
pub struct Weather {
    pub side: Side,
    pub seam_m: f32,
    pub top_m: f32,
    pub amount: f32,
    pub open_m: f32,
    pub mouths: [(f32, f32); 4],
}

fn luma(c: Vec3f) -> f32 {
    0.2126 * c.x + 0.7152 * c.y + 0.0722 * c.z
}

fn grey(l: f32) -> Vec3f {
    vec3f(l, l, l)
}

/// The nearest of jittered points, cells of 1, to (x, y): (its cell's offset from the point's cell, its place in that
/// frame, the squared distance to it, its hash).
fn nearest(x: f32, y: f32, seed: u32) -> ((i32, i32), (f32, f32), f32, u32, (i32, i32), (f32, f32)) {
    let (ix, iy) = (x.floor() as i32, y.floor() as i32);
    let (fx, fy) = (x - ix as f32, y - iy as f32);
    let (mut best, mut bx, mut by, mut bh, mut bc) = (1.0e9f32, 0.0, 0.0, 0u32, (0, 0));
    for j in -1..=1 {
        for i in -1..=1 {
            let (px, py, h) = site(ix, iy, i, j, seed);
            let d = (px - fx) * (px - fx) + (py - fy) * (py - fy);
            if d < best {
                best = d;
                bx = px;
                by = py;
                bh = h;
                bc = (i, j);
            }
        }
    }
    (bc, (bx, by), best, bh, (ix, iy), (fx, fy))
}

/// The jittered point of cell (ix + i, iy + j), in the frame of cell (ix, iy), and its hash.
fn site(ix: i32, iy: i32, i: i32, j: i32, seed: u32) -> (f32, f32, u32) {
    let h = hash3_u32(ix + i, iy + j, 7, seed);
    let ox = (h & 0xffff) as f32 / 65535.0;
    let oy = (h >> 16) as f32 / 65535.0;
    (i as f32 + 0.1 + 0.8 * ox, j as f32 + 0.1 + 0.8 * oy, h)
}

/// Voronoi of jittered points, cells of 1: the distance to the nearest point and that cell's hash (no edge).
fn voronoi_near(x: f32, y: f32, seed: u32) -> (f32, u32) {
    let (_, _, best, h, _, _) = nearest(x, y, seed);
    (best.sqrt(), h)
}

/// Voronoi of jittered points, cells of 1: (the distance to the nearest point, the distance to the nearest cell edge,
/// the nearest cell's hash). The edge is sought among the cells round the nearest point's (with points jittered
/// within 0.1..0.9 of their cells those hold every neighbour but in rare corners, where the edge is a little long).
fn voronoi(x: f32, y: f32, seed: u32) -> (f32, f32, u32) {
    let ((ci, cj), (bx, by), best, bh, (ix, iy), (fx, fy)) = nearest(x, y, seed);
    let mut edge = 1.0e9f32;
    for j in cj - 1..=cj + 1 {
        for i in ci - 1..=ci + 1 {
            let (px, py, _) = site(ix, iy, i, j, seed);
            let (dx, dy) = (px - bx, py - by);
            let l2 = dx * dx + dy * dy;
            if l2 < 1.0e-8 {
                continue;
            }
            let (mx, my) = (0.5 * (px + bx) - fx, 0.5 * (py + by) - fy);
            edge = edge.min((mx * dx + my * dy) / l2.sqrt());
        }
    }
    (best.sqrt(), edge, bh)
}

/// A large offset from a hash: a texture read at (u, v) plus it is a different part of it for every element.
fn offset(h: u32) -> (f32, f32) {
    (37.0 + 300.0 * h01(h, 91), -53.0 + 300.0 * h01(h, 92))
}

// ---------------------------------------------------------------------------------------------------------------------
// Wood

/// A board of wood seen at (a across it, b along it, both from its corner; m), its own hash `h`: the grain's albedo
/// factor (mean about 1). Flat-sawn: the rings cut by a plane slightly tilted to the log, so they show as arches
/// (cathedrals) down the board, wavering with the log; pores as fine dark streaks along it; broad figure.
fn grain(a: f32, b: f32, width: f32, h: u32, foot: f32) -> f32 {
    let ring = 0.0035 + 0.003 * h01(h, 1);
    // about a third of the boards are quarter-sawn: straight lines along the board
    let quarter = h01(h, 8) < 0.35;
    // the log's axis across the board and its depth under the face, drifting along the board
    let c = width * (0.2 + 0.6 * h01(h, 2)) + 0.02 * gn(b * 0.7, 3.0, h);
    let depth = 0.04 + 0.08 * h01(h, 3) + 0.03 * gn(b * 0.35 + 5.0, 1.0, h ^ 9) + b * 0.03 * (h01(h, 4) - 0.5);
    let r = if quarter {
        a + 0.004 * gn(a / 0.02, b / 0.4, h ^ 4)
    } else {
        ((a - c) * (a - c) * 6.0 + depth * depth).sqrt()
    } + 0.0015 * gl(a, b * 0.016, 0.004, foot, h ^ 3);
    let t = r / ring;
    let k = lod(ring, foot);
    // late wood: a darker band a third of each ring, its edge toward the bark sharp
    let f = t.fract();
    let late = smooth(0.55, 0.75, f) * (1.0 - smooth(0.92, 1.0, f));
    let mean_late = 0.31;
    let tone = 1.0 - 0.30 * (k * late + (1.0 - k) * mean_late) + 0.30 * mean_late;
    // pores: fine dark streaks along the board
    let kp = lod(0.0015, foot);
    let pore = if kp > 0.0 { smooth(0.55, 0.85, gn(a / 0.0007, b / 0.012, h ^ 77)) * kp } else { 0.0 };
    let pore_mean = 0.12;
    let tone = tone * (1.0 - 0.18 * (pore + (1.0 - kp) * pore_mean) + 0.18 * pore_mean);
    // figure: broad lighter and darker streaks along the board
    let figure = 1.0 + 0.10 * gl(a, b * 0.05, 0.03, foot, h ^ 5) + 0.06 * gl(a, b * 0.053, 0.08, foot, h ^ 6);
    tone * figure
}

/// The board under (u, v) of a floor laid as `m` (a wood): (across, along, its width, its length, its hash).
fn board(m: RoomMaterial, u: f32, v: f32, seed: u32) -> (f32, f32, f32, f32, u32) {
    match m {
        RoomMaterial::Planks => {
            let w = 0.19;
            let row = (u / w).floor();
            let rh = hash3_u32(row as i32, 0, 21, seed);
            let len = 1.2 + 1.0 * h01(rh, 1);
            let vv = v + len * h01(rh, 2);
            let bi = (vv / len).floor();
            (u - row * w, vv - bi * len, w, len, hash3_u32(row as i32, bi as i32, 22, seed))
        }
        RoomMaterial::Herringbone => herringbone(u, v, 0.07, 6.0, seed),
        RoomMaterial::Chevron => chevron(u, v, 0.6, 0.09, seed),
        _ => basket(u, v, 0.27, 3, seed),
    }
}

/// The herringbone block under (u, v): blocks `w` wide and `k` widths long, a 90 degree herringbone turned 45 degrees
/// (its zigzag running along v). The tiling's lattice is m (1, 1) + n (k, -k) in widths, each cell one block lying
/// along x at the lattice point and one standing along y beside it at (k, 1 - k): (across, along, width, length, hash).
fn herringbone(u: f32, v: f32, w: f32, k: f32, seed: u32) -> (f32, f32, f32, f32, u32) {
    let s = std::f32::consts::FRAC_1_SQRT_2;
    let (x, y) = ((u * s - v * s) / w, (u * s + v * s) / w);
    let m0 = ((x + y) * 0.5).floor();
    let n0 = ((x - y) / (2.0 * k)).floor();
    // (x + y) / 2 - m is in [0, (k + 2) / 2) in either block, so its lattice point's m is at most that far below m0
    let reach = ((k + 2.0) * 0.5).ceil() as i32 - 1;
    for dm in -reach..=0 {
        for dn in -1..=1 {
            let (m, n) = (m0 + dm as f32, n0 + dn as f32);
            let (ox, oy) = (m + n * k, m - n * k);
            // the block lying along x
            let (lx, ly) = (x - ox, y - oy);
            if (0.0..k).contains(&lx) && (0.0..1.0).contains(&ly) {
                return (ly * w, lx * w, w, k * w, hash3_u32(m as i32, n as i32, 23, seed));
            }
            // the block standing along y
            let (sx, sy) = (x - ox - k, y - oy - (1.0 - k));
            if (0.0..1.0).contains(&sx) && (0.0..k).contains(&sy) {
                return (sx * w, sy * w, w, k * w, hash3_u32(m as i32, n as i32, 24, seed));
            }
        }
    }
    (0.5 * w, 0.5 * k * w, w, k * w, 0)
}

/// The chevron board under (u, v): columns `col` wide along v, each of two halves of boards `w` wide at 45 degrees,
/// mitred where the halves meet on the column's centre line and at its edges: (across, along, width, length, hash).
fn chevron(u: f32, v: f32, col: f32, w: f32, seed: u32) -> (f32, f32, f32, f32, u32) {
    let ci = (u / col).floor();
    let x = u - ci * col;
    let half = 0.5 * col;
    let right = x > half;
    // the distance in from the column's edge (the board's along), the same on both halves, so they meet in a V
    let a = if right { col - x } else { x };
    let pitch = w * std::f32::consts::SQRT_2;
    let s = (v - a) / pitch;
    let bi = s.floor();
    let across = (s - bi) * w;
    let along = a * std::f32::consts::SQRT_2;
    let len = half * std::f32::consts::SQRT_2;
    // a mitred end is cut square to the column: the board's along runs to its end at the centre line and the edge
    (across, along, w, len, hash3_u32(ci as i32 * 2 + right as i32, bi as i32, 25, seed))
}

/// The basket-weave slat under (u, v): squares `sq` wide of `n` slats, turned a quarter between neighbours.
fn basket(u: f32, v: f32, sq: f32, n: u32, seed: u32) -> (f32, f32, f32, f32, u32) {
    let (i, j) = ((u / sq).floor(), (v / sq).floor());
    let (x, y) = (u - i * sq, v - j * sq);
    let turned = (i as i32 + j as i32).rem_euclid(2) == 1;
    let (across, along) = if turned { (y, x) } else { (x, y) };
    let w = sq / n as f32;
    let si = (across / w).floor();
    (across - si * w, along, w, sq, hash3_u32(i as i32 * 8 + si as i32, j as i32, 26, seed))
}

/// A wooden floor at (u, v): each board its own tone (much spread) and hue (little), its grain read at its own
/// offset, bevelled edges with a dark seam. (albedo factor, gloss factor, roughness added)
fn wood(m: RoomMaterial, base: Vec3f, u: f32, v: f32, foot: f32, seed: u32) -> (Vec3f, f32, f32) {
    let (a, b, w, l, h) = board(m, u, v, seed);
    let e = a.min(w - a).min(b).min(l - b);
    // the seams' share of a board's area at 1.5 mm each side: what a far floor takes for them
    let seam_mean = (0.0015 / w + 0.0015 / l).min(0.5);
    let bev = edge(e - 0.0015, foot, w, seam_mean);
    let kb = lod(w * 2.0, foot);
    let g = grain(a, b, w, h, foot);
    // each board its own tone and warmth (the boards' spread is the first thing a wooden floor shows)
    let t = 1.0 + kb * (0.36 * (h01(h, 9) - 0.5));
    let warm = kb * (h01(h, 10) - 0.5);
    let tone = t * (1.0 + 0.04 * fbm(u, v, 1.5, 2, foot, seed ^ 31));
    let bc = vec3f(base.x * (1.0 + 0.08 * warm), base.y * (1.0 + 0.01 * warm), base.z * (1.0 - 0.14 * warm)) * tone;
    let c = bc * (g * (1.0 - 0.6 * bev + 0.6 * seam_mean));
    (c, 1.0 + kb * 0.3 * (h01(h, 11) - 0.5) - 0.7 * (bev - seam_mean), kb * 0.02 * h01(h, 12))
}

// ---------------------------------------------------------------------------------------------------------------------
// Brick

/// A brick's length and height and the joint between them, metres (a metric brick, 10 mm joints).
const BRICK_L: f32 = 0.215;
const BRICK_H: f32 = 0.065;
const JOINT: f32 = 0.010;
/// The share of a brick wall's face that is mortar.
const JOINT_SHARE: f32 = 1.0 - BRICK_L * BRICK_H / ((BRICK_L + JOINT) * (BRICK_H + JOINT));

/// The brick of a running bond under (u, v up from the floor): (its hash, the distance in from its edge (negative in
/// the joint), how far down its bed joint from the brick above it is as a share of the joint (0 at that brick, for
/// its shade; 1 off the bed joints), how far it is chipped (0 none, 1 in the chip)).
fn bond(u: f32, v: f32, foot: f32, seed: u32) -> (u32, f32, f32, f32) {
    let (cw, ch) = (BRICK_L + JOINT, BRICK_H + JOINT);
    let row = (v / ch).floor();
    let rh = hash3_u32(row as i32, 0, 40, seed);
    // running bond: every other course half a brick over, laid by hand (a few millimetres off)
    let shift = 0.5 * (row as i32).rem_euclid(2) as f32 + 0.04 * (h01(rh, 1) - 0.5);
    let uu = u / cw + shift;
    let col = uu.floor();
    let h = hash3_u32(col as i32, row as i32, 41, seed);
    let (a, b) = ((uu - col) * cw, v - row * ch);
    // the joint's width wanders, and the brick's edge is not a ruled line
    let jw = JOINT * (0.85 + 0.3 * h01(h, 2)) + 0.002 * gl(u, v, 0.15, foot, seed ^ 42);
    let wob = 0.0025 * gl(u, v, 0.015, foot, seed ^ 43) + 0.0012 * gl(u, v, 0.005, foot, seed ^ 44);
    let (ea, eb) = ((a - 0.5 * jw).min(cw - 0.5 * jw - a), (b - 0.5 * jw).min(ch - 0.5 * jw - b));
    let e = ea.min(eb) + wob;
    // in a bed joint, how far down from the brick above it the point is, as a share of the joint (its upper part is
    // in that brick's shadow); 1 off the bed joints
    let under = if b < 0.5 * jw {
        (0.5 * jw - b) / jw
    } else if b > ch - 0.5 * jw {
        0.5 + (ch - b) / jw
    } else {
        1.0
    };
    // a quarter of the bricks have a corner or an edge knocked off: a line through the brick at a random angle
    let chip = if h01(h, 3) < 0.25 {
        let ang = h01(h, 4) * std::f32::consts::TAU;
        let (nx, ny) = (ang.cos(), ang.sin());
        let (px, py) = ((a - 0.5 * cw) / (0.5 * cw), (b - 0.5 * ch) / (0.5 * ch));
        let cut = px * nx + py * ny - (0.75 + 0.3 * h01(h, 5)) + 0.08 * gl(u, v, 0.02, foot, seed ^ 45);
        smooth(-0.02, 0.02, cut) * lod(0.03, foot)
    } else {
        0.0
    };
    (h, e, under, chip)
}

/// Bare brick round the wall's colour `base`.
fn brick(base: Vec3f, u: f32, v: f32, foot: f32, w: &Weather, seed: u32) -> Vec3f {
    let (h, e, under, chip) = bond(u, v, foot, seed);
    let mortar = edge(e, foot, BRICK_H + JOINT, JOINT_SHARE);
    let kb = lod(2.0 * (BRICK_H + JOINT), foot);
    // the brick's own colour: value spread wide, hue from orange to brown, a few nearly black or pale (overfired,
    // underfired)
    let (r1, r2, r3) = (h01(h, 11), h01(h, 12), h01(h, 13));
    let mut tone = 0.74 + 0.52 * r1;
    let hue = 2.0 * r2 - 1.0;
    let mut tint = vec3f(1.0 + 0.10 * hue, 1.0 - 0.03 * hue, 1.0 - 0.20 * hue);
    if r3 < 0.04 {
        tone *= 0.6;
        tint = vec3f(0.9, 0.95, 1.1);
    } else if r3 > 0.95 {
        tone *= 1.15;
        tint = vec3f(1.0, 1.12, 1.25);
    }
    let tone = 1.0 + kb * (tone - 1.0);
    let tint = mix3(vec3f(1.0, 1.0, 1.0), tint, kb);
    // the face: a fired mottle read at the brick's own offset, fine pits, darker toward its worn edges
    let (ox, oy) = offset(h);
    // (one end of a brick fired darker than the other; the fired skin's patches; sand and pits a few mm across)
    let fired = 0.10 * kb * (h01(h, 14) - 0.5) * gn(u / 0.25 + ox, v / 0.25 + oy, seed ^ 45);
    let face = 1.0 + fired + 0.20 * fbm(u + ox, v + oy, 0.07, 4, foot, seed ^ 46) * kb + 0.07 * gl(u, v, 0.0025, foot, seed ^ 55)
        - 0.22 * smooth(0.55, 0.85, gl(u, v, 0.004, foot, seed ^ 47));
    let worn = 1.0 - 0.18 * (1.0 - smooth(0.0, 0.006, e)) * lod(0.012, foot) - 0.25 * chip;
    let bc = mul3(base, tint) * (tone * face * worn);
    // mortar: lighter and greyer than the bricks, lumpy, the bed joint's top in the shade of the brick above
    let ml = (1.9 * luma(base)).min(0.7);
    let lump = 1.0 + 0.15 * gl(u, v, 0.005, foot, seed ^ 48) + 0.12 * gl(u, v, 0.015, foot, seed ^ 49) + 0.08 * gl(u, v, 0.05, foot, seed ^ 56)
        - 0.25 * smooth(0.6, 0.9, gl(u, v, 0.002, foot, seed ^ 57));
    let shade = 1.0 - 0.45 * (1.0 - smooth(0.0, 0.6, under)) * lod(2.0 * JOINT, foot) * kb;
    let mc = vec3f(ml * 1.02, ml, ml * 0.94) * (lump * shade);
    // what the bricks and mortar average to, without the pattern: the wall's mean is its colour
    let mean = base * (1.0 - JOINT_SHARE) + vec3f(ml * 1.02, ml, ml * 0.94) * (JOINT_SHARE * (1.0 - 0.45 * 0.3 * 0.67));
    let norm = vec3f(base.x / mean.x.max(1.0e-4), base.y / mean.y.max(1.0e-4), base.z / mean.z.max(1.0e-4));
    let mut c = mul3(mix3(bc, mc, mortar), norm);
    if w.amount > 0.0 {
        let a = w.amount;
        // salts bloom pale low on the wall, where damp rises
        if w.seam_m < 1.4 {
            let bloom = (1.0 - smooth(0.2, 0.9 + 0.5 * gn(u / 1.5, 0.0, seed ^ 50), w.seam_m)) * smooth(0.0, 0.6, fbm(u, v, 0.5, 4, foot, seed ^ 51) + 0.3);
            c = mix3(c, grey(0.55) * vec3f(1.0, 0.99, 0.96), 0.45 * a * bloom);
        }
        // remnants of plaster: a large field over a threshold, ragged, with a raised edge and a dark rim on the brick
        let f = fbm(u, v, 2.0, 5, foot, seed ^ 52) + 0.25 * fbm(u, v, 0.15, 3, foot, seed ^ 53);
        let t = 0.5 - 0.4 * a;
        let wide = (0.5 * foot).max(5.0e-4);
        let pl = smooth(t - wide * 2.0, t + wide * 2.0, f);
        let rim = (smooth(t - 0.06, t, f) - smooth(t, t + 0.01, f)).max(0.0) * lod(0.03, foot);
        c = c * (1.0 - 0.35 * rim);
        if pl > 0.0 {
            let plaster = vec3f(0.62, 0.6, 0.55)
                * (1.0 + 0.10 * fbm(u, v, 0.12, 4, foot, seed ^ 54) - 0.3 * smooth(0.65, 0.9, gl(u, v, 0.006, foot, seed ^ 58)));
            c = mix3(c, plaster, pl);
        }
    }
    c
}

/// Brick under paint of the wall's colour `base`.
fn painted_brick(base: Vec3f, u: f32, v: f32, foot: f32, w: &Weather, seed: u32) -> Vec3f {
    let (h, e, under, chip) = bond(u, v, foot, seed);
    let mortar = edge(e, foot, BRICK_H + JOINT, JOINT_SHARE);
    let kb = lod(2.0 * (BRICK_H + JOINT), foot);
    // the paint over a brick takes its porosity a little (a few percent per brick), its edges round and shaded
    let tone = 1.0 + kb * 0.06 * (h01(h, 11) - 0.5) + 0.03 * fbm(u, v, 0.4, 3, foot, seed ^ 60);
    let worn = 1.0 - 0.10 * (1.0 - smooth(0.0, 0.005, e)) * lod(0.01, foot) - 0.12 * chip;
    // the joint is recessed: shade in it, deepest under the brick above
    let joint = 0.80 - 0.15 * (1.0 - smooth(0.0, 0.6, under)) * lod(2.0 * JOINT, foot);
    let stipple = 1.0 + 0.02 * gl(u, v, 0.002, foot, seed ^ 61);
    let mean = (1.0 - JOINT_SHARE) * (1.0 - 0.10 * 0.08) + JOINT_SHARE * (0.80 - 0.15 * 0.3 * 0.67);
    let mut c = base * (tone * stipple * (worn * (1.0 - mortar) + joint * mortar) / mean);
    if w.amount > 0.0 {
        let a = w.amount;
        // the paint peels off to the brick, most low on the wall and round its seams
        // (a few large areas, most low on the wall, their edges ragged at a few centimetres)
        let low = 0.15 * (1.0 - smooth(0.0, 1.5, w.seam_m));
        let f = fbm(u, v, 3.0, 6, foot, seed ^ 62) + 0.12 * fbm(u, v, 0.12, 3, foot, seed ^ 63) + low;
        let t = 0.48 - 0.34 * a;
        let wide = (0.5 * foot).max(5.0e-4);
        let bare = smooth(t - 2.0 * wide, t + 2.0 * wide, f);
        let lip = (smooth(t - 0.03, t, f) - smooth(t, t + 0.006, f)).max(0.0) * lod(0.02, foot);
        c = c * (1.0 + 0.15 * lip);
        if bare > 0.0 {
            let red = brick(vec3f(0.30, 0.14, 0.09), u, v, foot, &Weather { amount: 0.0, ..*w }, seed);
            // white paint stays in the joints and the pores of the bare brick
            let left = mortar * 0.6 + 0.15;
            c = mix3(c, mix3(red, base, left), bare);
        }
    }
    c
}

// ---------------------------------------------------------------------------------------------------------------------
// Plaster, concrete, terrazzo, epoxy

/// Paint on plaster, aging with `w`. (albedo factor)
fn plaster(base: Vec3f, u: f32, v: f32, foot: f32, w: &Weather, seed: u32) -> Vec3f {
    // roller stipple (orange peel) 1-3 mm, the paint's faint unevenness, the trowel's long undulation
    let st = gl(u, v, 0.0018, foot, seed) + 0.6 * gl(u, v, 0.004, foot, seed ^ 1);
    let tone = 1.0 + 0.015 * fbm(u, v, 0.6, 4, foot, seed ^ 3) + 0.012 * st;
    // touch-up patches: cells of paint from another tin, a shade off, edges sharp
    let (_, pe, ph) = voronoi(u / 1.1 + 0.3 * gn(u, v, seed ^ 4), v / 1.1, seed ^ 5);
    let patch = if h01(ph, 1) < 0.3 { (h01(ph, 2) - 0.5) * 0.035 * smooth(0.0, (foot / 1.1).max(0.004), pe) } else { 0.0 };
    let mut c = base * (tone + patch);
    if w.amount > 0.0 {
        let a = w.amount;
        // stains: a brown water mark where the field passes a threshold, its tide line darker
        let (s, tide) = water_stain(u, v, foot, a, seed ^ 6);
        c = mul3(c, mix3(vec3f(1.0, 1.0, 1.0), vec3f(0.93, 0.88, 0.76), a * s)) * (1.0 - 0.10 * a * tide);
        // leaks from the ceiling's seam on a wall: columns, each its own length, fading down
        if w.side == Side::Wall {
            if let Some(k) = leak_columns(u, w.top_m, foot, a, seed) {
                c = mul3(c, mix3(vec3f(1.0, 1.0, 1.0), vec3f(0.80, 0.74, 0.62), 0.6 * k));
            }
        }
        // peeling paint: where a field passes a high threshold the plaster shows, greyer, a light lip on the paint's
        // edge and its shade on the plaster beside it
        let f = fbm(u, v, 2.5, 6, foot, seed ^ 10) + 0.1 * fbm(u, v, 0.1, 3, foot, seed ^ 12) + 0.15 * (1.0 - smooth(0.0, 1.0, w.seam_m.min(w.top_m)));
        let t = 0.52 - 0.34 * a;
        let wide = (0.5 * foot).max(5.0e-4);
        let bare = smooth(t - 2.0 * wide, t + 2.0 * wide, f);
        let lip = (smooth(t - 0.025, t, f) - smooth(t, t + 0.004, f)).max(0.0) * lod(0.015, foot);
        let rim = (smooth(t, t + 0.004, f) - smooth(t + 0.01, t + 0.04, f)).max(0.0) * lod(0.015, foot);
        let bare_c = vec3f(0.66, 0.63, 0.57) * (1.0 + 0.08 * gl(u, v, 0.02, foot, seed ^ 11));
        c = mix3(c * (1.0 + 0.12 * lip), bare_c * (1.0 - 0.3 * rim), bare);
    }
    c
}

/// Concrete cast against plywood forms. (albedo factor)
fn form_concrete(u: f32, v: f32, foot: f32, seed: u32) -> f32 {
    // plywood forms 1.2 x 2.4 m: a panel's own tone, its seams, tie holes 30 cm in from its edges
    let (pw, ph) = (1.2, 2.4);
    let (pi, pj) = ((u / pw).floor(), (v / ph).floor());
    let (a, b) = (u - pi * pw, v - pj * ph);
    let panel_h = hash3_u32(pi as i32, pj as i32, 1, seed);
    let es = a.min(pw - a).min(b).min(ph - b);
    let seam = edge(es - 0.002, foot, pw, 0.004 / pw + 0.004 / ph);
    // tie holes: 25 mm cones, 60 cm apart, 30 cm in
    let (ta, tb) = (((a - 0.3) / 0.6).round() * 0.6 + 0.3, ((b - 0.3) / 0.6).round() * 0.6 + 0.3);
    let td = ((a - ta).powi(2) + (b - tb).powi(2)).sqrt();
    let in_panel = ta < pw - 0.1 && tb < ph - 0.1;
    let hole_mean = 0.0125f32.powi(2) * std::f32::consts::PI / 0.36;
    let hole = if in_panel { edge(td - 0.0125, foot, 0.6, hole_mean) } else { (1.0 - lod(0.05, foot)) * hole_mean };
    // bug holes: small pits, the Voronoi cells that get one
    let pk = lod(0.008, foot);
    let pit = if pk > 0.0 {
        let (bd, bh) = voronoi_near(u / 0.035, v / 0.035, seed ^ 11);
        if h01(bh, 1) < 0.18 { 1.0 - smooth(0.06, 0.06 + 0.12 * h01(bh, 2), bd) } else { 0.0 }
    } else {
        0.0
    };
    let pit = pit * pk + (1.0 - pk) * 0.006;
    // the pour's mottle, vertical streaks of laitance, the plywood's grain printed in the face
    let mott = fbm(u, v, 0.8, 4, foot, seed ^ 12);
    let streak = gl(u, v * 0.04, 0.05, foot, seed ^ 13);
    let wood = gl(u, v * 0.027, 0.004, foot, seed ^ 14);
    let panel = 1.0 + lod(1.0, foot) * 0.16 * (h01(panel_h, 1) - 0.5);
    let k = panel * (1.0 + 0.10 * mott + 0.05 * streak + 0.025 * wood) * (1.0 - 0.6 * hole) * (1.0 - 0.25 * seam) * (1.0 - 0.45 * pit);
    k / ((1.0 - 0.6 * hole_mean) * (1.0 - 0.25 * (0.004 / pw + 0.004 / ph)) * (1.0 - 0.45 * 0.006))
}

/// A ground and sealed concrete floor, aging with `w`. (albedo, gloss factor, roughness added)
fn polished_concrete(base: Vec3f, u: f32, v: f32, foot: f32, w: &Weather, seed: u32) -> (Vec3f, f32, f32) {
    let mott = 0.09 * fbm(u, v, 2.5, 2, foot, seed) + 0.06 * fbm(u, v, 0.35, 3, foot, seed ^ 1);
    // cut aggregate: stones of 4 to 14 mm, about a quarter of the face, each its own grey (some warm)
    let s = 0.011;
    let ka = lod(2.0 * s, foot);
    let (d, h) = if ka > 0.0 { voronoi_near(u / s, v / s, seed ^ 2) } else { (1.0, 0) };
    let r = 0.18 + 0.25 * h01(h, 1);
    let stone = if ka > 0.0 && h01(h, 2) < 0.55 { 1.0 - smooth(r - 0.06, r + 0.02, d) } else { 0.0 };
    let (st_tone, warm) = if ka > 0.0 { (0.75 + 0.5 * h01(h, 3), h01(h, 4)) } else { (1.0, 0.5) };
    let stone_c = vec3f(st_tone * (1.0 + 0.06 * warm), st_tone, st_tone * (1.0 - 0.08 * warm));
    let sand = 1.0 + 0.06 * gl(u, v, 0.0015, foot, seed ^ 3);
    // burnish: the grinder and the sealer leave cloudy patches a little darker and duller (rings drawn round the
    // trowel's strokes read as drawn, even faint)
    let burnish = smooth(-0.3, 0.5, fbm(u + 7.0, v - 3.0, 0.9, 3, foot, seed ^ 5));
    // hairline cracks: Voronoi edges 1.6 m apart, warped, about half of them shown
    let wu = u + 0.05 * gn(u / 0.15, v / 0.15, seed ^ 7);
    let wv = v + 0.05 * gn(u / 0.15 + 3.0, v / 0.15, seed ^ 7);
    let (_, ce, _) = voronoi(wu / 1.6, wv / 1.6, seed ^ 8);
    let cw = (0.0006f32).max(0.5 * foot);
    let crack = (1.0 - smooth(0.0, cw, ce * 1.6)) * (0.0006 / cw) * smooth(0.0, 0.4, gn(u / 2.0, v / 2.0, seed ^ 9));
    // saw joints every 4 m: 4 mm wide, dark
    let jx = (u / 4.0 - (u / 4.0).round()).abs() * 4.0;
    let jz = (v / 4.0 - (v / 4.0).round()).abs() * 4.0;
    let jw = 0.002f32.max(0.5 * foot);
    let joint = (1.0 - smooth(0.0, jw, jx.min(jz))) * (0.002 / jw);
    let stone_mix = stone * ka * 0.8 + (1.0 - ka) * 0.8 * 0.55 * 0.2;
    let mut c = mix3(base * (1.0 + mott) * sand, mul3(base, stone_c), stone_mix);
    c = c * (1.0 - 0.04 * burnish + 0.04 * 0.4) * (1.0 - 0.5 * crack) * (1.0 - 0.7 * joint);
    // (its sheen only a little: on a near-mirror floor any roughness more widens the lights' images patch by patch)
    let mut gloss = 1.0 - 0.1 * burnish - 0.8 * joint;
    let mut rough = 0.01 * burnish;
    if w.amount > 0.0 {
        let a = w.amount;
        // oil: dark blots, darkest in their middle, glossier
        let f = fbm(u, v, 1.4, 5, foot, seed ^ 20);
        let oil = smooth(0.42 - 0.2 * a, 0.55 - 0.2 * a, f) * smooth(0.0, 0.5, gn(u / 6.0, v / 6.0, seed ^ 21) + 0.2);
        c = c * (1.0 - 0.5 * a * oil);
        gloss += 0.3 * a * oil;
        // repairs: rectangles of newer concrete in a 3 m grid, their own tone, a dark seam round them
        let (ri, rj) = ((u / 3.0).floor(), (v / 3.0).floor());
        let rh = hash3_u32(ri as i32, rj as i32, 22, seed);
        if h01(rh, 1) < 0.25 * a {
            let (x0, z0) = (ri * 3.0 + 0.3 + 1.2 * h01(rh, 2), rj * 3.0 + 0.3 + 1.2 * h01(rh, 3));
            let (x1, z1) = (x0 + 0.6 + 0.9 * h01(rh, 4), z0 + 0.5 + 0.9 * h01(rh, 5));
            let e = (u - x0).min(x1 - u).min(v - z0).min(z1 - v);
            let inside = 1.0 - edge(e, foot, 3.0, 0.9);
            let seam = (1.0 - smooth(0.0, (0.004f32).max(0.5 * foot), e.abs())) * (0.004 / (0.004f32).max(0.5 * foot));
            c = c * (1.0 + inside * 0.14 * (h01(rh, 6) - 0.4)) * (1.0 - 0.5 * seam);
            gloss -= 0.5 * inside;
            rough += 0.2 * inside;
        }
    }
    (c, gloss.max(0.0), rough)
}

/// Terrazzo: angular chips (the inside of Voronoi cells past a margin) in two sizes, each from a palette of marbles,
/// in a binder of the surface's colour. (albedo factor)
fn terrazzo(u: f32, v: f32, foot: f32, seed: u32) -> Vec3f {
    // the palette's factors on the surface's colour (mean 1): white, light grey, black, rose, warm grey
    let palette = |h: u32| -> Vec3f {
        match (h01(h, 1) * 5.0) as u32 {
            0 => vec3f(1.55, 1.55, 1.5),
            1 => vec3f(1.15, 1.15, 1.15),
            2 => vec3f(0.3, 0.3, 0.32),
            3 => vec3f(1.2, 0.95, 0.9),
            _ => vec3f(0.85, 0.8, 0.75),
        }
    };
    let mut c = vec3f(1.0, 1.0, 1.0);
    for (size, margin, share, k) in [(0.03f32, 0.12f32, 0.85f32, 0u32), (0.009, 0.14, 0.7, 1)] {
        let lk = lod(2.0 * size, foot);
        if lk <= 0.0 {
            continue;
        }
        let warp = 0.15 * gn(u / (size * 2.0), v / (size * 2.0), seed ^ (70 + k));
        let (_, e, h) = voronoi(u / size + warp, v / size, seed ^ (72 + k));
        if h01(h, 2) > share {
            continue;
        }
        let chip = smooth(margin - (foot / size).max(0.01), margin + (foot / size).max(0.01), e) * lk;
        let face = 1.0 + 0.08 * gl(u, v, size * 0.4, foot, h);
        c = mix3(c, palette(h) * face, chip);
    }
    c * (1.0 + 0.03 * gl(u, v, 0.002, foot, seed ^ 74))
}

/// A painted floor. (albedo factor, gloss factor, roughness added)
fn epoxy(u: f32, v: f32, foot: f32, seed: u32) -> (f32, f32, f32) {
    let mott = 0.035 * fbm(u, v, 1.5, 3, foot, seed);
    // fine scratches: straight strokes in cells of 25 cm, any direction, lighter
    let mut scr = 0.0;
    let ks = lod(0.01, foot);
    if ks > 0.0 {
        for k in 0..3u32 {
            let cs = 0.25;
            let (ci, cj) = ((u / cs + 0.37 * k as f32).floor(), (v / cs + 0.61 * k as f32).floor());
            let hh = hash3_u32(ci as i32, cj as i32, 30 + k as i32, seed);
            if h01(hh, 1) < 0.5 {
                let ang = h01(hh, 2) * std::f32::consts::PI;
                let (ca, sa) = (ang.cos(), ang.sin());
                let (px, py) = ((u / cs + 0.37 * k as f32 - ci - 0.5) * cs, (v / cs + 0.61 * k as f32 - cj - 0.5) * cs);
                let along = px * ca + py * sa;
                let across = (-px * sa + py * ca + 0.002 * gn(along / 0.05, 1.0, hh)).abs();
                let len = 0.05 + 0.1 * h01(hh, 3);
                let ww = 0.0004f32.max(0.5 * foot);
                scr += (1.0 - smooth(0.0, ww, across)) * (0.0004 / ww) * (1.0 - smooth(len * 0.6, len, along.abs()));
            }
        }
    }
    let scr = scr.min(1.0) * ks;
    // rubber marks: short dark smears, stretched along a wandering heading
    let ang = gn(u / 3.0, v / 3.0, seed ^ 40) * 3.0;
    let (ca, sa) = (ang.cos(), ang.sin());
    let (ru, rv) = (u * ca + v * sa, -u * sa + v * ca);
    let rk = lod(0.06, foot);
    let rub = (smooth(0.55, 0.9, gn(ru / 0.25, rv / 0.03, seed ^ 41)) * rk + (1.0 - rk) * 0.06) * smooth(0.2, 0.6, gn(u / 1.2, v / 1.2, seed ^ 42));
    ((1.0 + mott) * (1.0 + 0.35 * scr) * (1.0 - 0.45 * rub), 1.0 - 0.5 * scr - 0.3 * rub, 0.25 * scr + 0.1 * rub)
}

/// Wear the room's geometry lays over any material: on a floor grime in a band along its seams with the walls (its
/// width wandering, darker where it is narrow) and a lighter fluff of dust beyond it; on a wall a grime band over the
/// floor and kick marks under 25 cm. (albedo factor, roughness added)
fn weather(w: &Weather, u: f32, v: f32, foot: f32, seed: u32) -> (Vec3f, f32) {
    if w.amount <= 0.0 || w.side == Side::Ceiling {
        return (vec3f(1.0, 1.0, 1.0), 0.0);
    }
    let a = w.amount;
    // (past the dust on a floor, over the kick marks on a wall, wear leaves the surface as it is)
    if (w.side == Side::Floor && w.seam_m > 0.5) || (w.side == Side::Wall && w.seam_m > 0.25) {
        return (vec3f(1.0, 1.0, 1.0), 0.0);
    }
    let grunge = grunge(u, v, 0.4, foot, seed ^ 60);
    if w.side == Side::Floor {
        let (grime, dust) = seam_grime(u, v, w.seam_m, grunge, seed ^ 61);
        let c = vec3f(1.0, 1.0, 1.0) * (1.0 - 0.55 * a * grime) + vec3f(0.06, 0.055, 0.05) * (a * dust);
        (c, 0.25 * a * (grime + dust))
    } else {
        let band = skirting_band(w.seam_m, grunge);
        let kick = kick_marks(u, v, w.seam_m, foot, seed ^ 62);
        // (the grime a little warm, as much as it is there)
        let c = mix3(vec3f(1.0, 1.0, 1.0), vec3f(1.0, 0.99, 0.97), a * band) * ((1.0 - 0.25 * a * band) * (1.0 - 0.4 * a * kick));
        (c, 0.1 * a * kick)
    }
}

// ---------------------------------------------------------------------------------------------------------------------
// Leaks

/// The leaks the material `m` takes at (u, v) on a wall or, along an open edge, a ceiling (worn by `w`): from the
/// wall's top dark grime, turning to green and brown moss where daylight and rain come in (within a few metres of an
/// opening) with a mossy band where the water collects at the foot; on cast concrete a pale wash of lime from the
/// panels' joints and rust from the tie holes' ends; on brick a wash of cement from the top; on a ceiling (a deck's
/// soffit) streaks of grime and lime in from its open edges. Walls of other materials keep their own (plaster's columns).
fn leaks(c: Vec3f, m: RoomMaterial, u: f32, v: f32, foot: f32, w: &Weather, seed: u32) -> Vec3f {
    let a = w.amount;
    if a <= 0.0 || !matches!(m, RoomMaterial::FormConcrete | RoomMaterial::Brick) {
        return c;
    }
    // moss grows where light and rain reach: near an opening
    let moss_share = if w.open_m.is_finite() { 1.0 - smooth(3.0, 10.0, w.open_m) } else { 0.0 };
    let share = (0.2 + 0.8 * a).min(0.95);
    let mut c = c;
    let stain = |c: Vec3f, cov: f32, near: f32, moss: f32, u: f32, d: f32| wet_stain(c, cov, near, moss, u, d, foot, seed);
    let wash = |c: Vec3f, cov: f32| mix3(c, vec3f(0.72, 0.71, 0.68), 0.35 * cov);
    match w.side {
        Side::Wall if w.top_m.is_finite() => {
            let height = w.seam_m + w.top_m;
            let reach = (0.8 + 4.5 * a).min(0.6 * height);
            let moss = moss_share * (0.55 + 0.45 * smooth(-0.3, 0.3, gl(u, 0.0, 2.5, foot, seed ^ 72)));
            // the mossy band where the water collects at the foot, its upper edge in tufts
            if moss > 0.0 && w.seam_m < 0.8 {
                let (f, lost) = runs(u, w.seam_m, foot, seed ^ 73);
                let band = over(f, -0.6 + 1.2 * w.seam_m / (0.15 + 0.4 * a), lost) * smooth(-0.5, 0.2, gl(u, 0.0, 1.5, foot, seed ^ 74));
                c = stain(c, 0.8 * band * moss, 0.0, 1.0, u, w.seam_m);
            }
            if m == RoomMaterial::FormConcrete {
                // lime from the panels' horizontal joints (2.4 m), under the top
                let joint = (v / 2.4).ceil() * 2.4;
                if joint < height - 0.05 {
                    let (cov, _) = curtain(u, joint - v, foot, 0.6, 0.1 + 0.5 * a, 0.1 + 0.3 * a, true, seed ^ 75);
                    c = wash(c, cov);
                }
                // rust from the ends of the tie rods (holes 0.6 m apart, 0.3 m in)
                let (cov, near) = from_holes(u, v, height, foot, 0.6, 0.3, 0.15 + 0.9 * a, 0.02 + 0.16 * a, seed ^ 76);
                if cov > 0.0 {
                    let rust = mix3(vec3f(0.30, 0.16, 0.08), vec3f(0.15, 0.075, 0.04), near);
                    c = mix3(c, rust, 0.85 * cov);
                }
            } else {
                // a wash of cement from the top, its drips
                let (cov, _) = curtain(u + 13.0, w.top_m, foot, 0.7, 0.3 + 0.9 * a, 0.15 + 0.4 * a, true, seed ^ 77);
                c = wash(c, cov);
            }
            let (cov, near) = curtain(u, w.top_m, foot, 0.9, reach, share, true, seed ^ 78);
            stain(c, cov, near, moss, u, w.top_m)
        }
        Side::Ceiling if w.open_m.is_finite() => {
            // the deck's edges: water creeps in under them before it drips; lime first, then grime. Each open edge
            // hangs its own (by its own place along it), so where two meet at a corner both lie over each other.
            let reach = 0.3 + 1.2 * a;
            let open = || w.mouths.iter().filter(|(d, _)| d.is_finite());
            for &(d, u) in open() {
                let (cov, _) = curtain(u + 7.0, d, foot, 0.5, 0.6 * reach, 0.1 + 0.4 * a, false, seed ^ 79);
                c = wash(c, cov);
            }
            for &(d, u) in open() {
                let (cov, near) = curtain(u, d, foot, 0.9, reach, share, false, seed ^ 80);
                c = stain(c, cov, near, 0.3 * moss_share, u, d);
            }
            c
        }
        _ => c,
    }
}

/// The surface of material `m` (its mean colour `base`) at (u, v) seen at footprint `foot` (m), worn by `w`.
pub fn surface(m: RoomMaterial, base: Vec3f, u: f32, v: f32, foot: f32, w: &Weather, seed: u32) -> Surface {
    let (c, gloss, rough) = match m {
        RoomMaterial::Plaster => (plaster(base, u, v, foot, w, seed), 1.0, 0.0),
        RoomMaterial::Brick => (brick(base, u, v, foot, w, seed), 1.0, 0.0),
        RoomMaterial::PaintedBrick => (painted_brick(base, u, v, foot, w, seed), 1.0, 0.0),
        RoomMaterial::FormConcrete => (base * form_concrete(u, v, foot, seed), 1.0, 0.0),
        RoomMaterial::PolishedConcrete => polished_concrete(base, u, v, foot, w, seed),
        RoomMaterial::Terrazzo => (mul3(base, terrazzo(u, v, foot, seed)), 1.0, 0.0),
        RoomMaterial::Epoxy => {
            let (k, g, r) = epoxy(u, v, foot, seed);
            (base * k, g, r)
        }
        _ if m.wood() => wood(m, base, u, v, foot, seed),
        _ => (base, 1.0, 0.0),
    };
    let c = leaks(c, m, u, v, foot, w, seed);
    let (wc, wr) = weather(w, u, v, foot, seed);
    let albedo = mul3(c, wc);
    Surface { albedo: vec3f(albedo.x.clamp(0.0, 1.0), albedo.y.clamp(0.0, 1.0), albedo.z.clamp(0.0, 1.0)), gloss: gloss * (1.0 - wr).max(0.0), roughness: rough + wr }
}

