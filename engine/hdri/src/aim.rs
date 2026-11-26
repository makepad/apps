//! A light's aim (B9): which way its face points, turned off the direction
//! to the centre, and what hangs on the face: a honeycomb grid and four barn
//! doors.
//!
//! Aim does not change the outline the map shows: a light keeps its angular
//! size and shape, and a diffuse face is as bright from the side as from the
//! front. What changes is how much of the face the centre can see, which is
//! where an environment map is seen from. So an aimed light only looks
//! different through its grid, past its doors, or when it is nearly edge-on.
//!
//! The face is one plane, and the centre sees all of it from one side: as it
//! turns toward edge-on it fades out as a whole, by its own turn (D9, L:187).
//! A ray through a wide face can still leave it more than 90 degrees from its
//! normal (the outline does not turn with it); that ray grazes the face, so
//! its doors and its grid block it as they block a ray just short of 90.
//!
//! Positions are face coordinates (x to the viewer's right, y up, tangent
//! units); the aimed face's own axes are `r` (its right), `u` (its up) and
//! `n` (its normal, the way it shines).

use super::*;
use crate::noise::{fbm2, hash01};

/// Honeycomb cells across the light's smaller side.
pub const GRID_CELLS: f32 = 12.0;
/// Cell depth over cell width at grid 1. A tube shows nothing once the ray
/// is displaced by a whole cell width over its depth, which is at
/// atan(width / depth) off its axis: 1 / tan(15 degrees), a 15 degree grid.
pub const GRID_DEPTH_MAX: f32 = 3.732;
/// Half the thickness of a cell wall at full strength, and the softening of
/// a cell's edge, both in cell widths. The softening is the honeycomb's only
/// anti-aliasing (the bake point-samples it), so it keeps its width at every
/// grid.
const GRID_WALL: f32 = 0.04;
const GRID_EDGE: f32 = 0.04;
/// How far the face turns, as the cosine between its normal and the way back
/// to the centre, over which it fades out: the last 4.6 degrees before 90.
const EDGE_ON_FADE: f32 = 0.08;
const SQRT_3: f32 = 1.732_050_8;

/// A soft light's grid is sewn fabric, not a honeycomb: square cells of
/// black cloth strips that let `FABRIC_WALL_LIGHT` of the face through.
/// Which egg-crate it is (`FabricGrid`) sets its cells and strips
/// (`FabricLook`); the strip's (RLS020) has `FABRIC_CELLS` across the face's
/// smaller side.
pub const FABRIC_CELLS: f32 = 7.0;
const FABRIC_WALL_LIGHT: f32 = 0.05;
/// The softening of a strip's edge, in cell widths (the fabric grid's only
/// anti-aliasing, as `GRID_EDGE` the honeycomb's).
const FABRIC_EDGE: f32 = 0.02;

/// Which sewn egg-crate hangs on a soft light (`Aim::fabric_grid`): a
/// strip's, a softbox's or an octabox's, or a round one on an umbrella or a
/// parabolic.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FabricGrid {
    Strip,
    Box,
    Round,
}

/// A sewn egg-crate's cells and strips, the strips in cell widths. The
/// upright strips hang straight: `upright` is half their thickness seen
/// edge-on, `twist` more where a strip twists a little and shows its face,
/// `bow` a little bow sideways; both change from row to row. The crossing
/// strips are limp: each droops between two uprights, its front edge `sag`
/// below its back one (`across` is half its thickness edge-on), as a
/// crescent over the cell under it (`wedge` false: most at the middle) or
/// twisted at the tabs (`wedge` true: each crossing holds it at its own
/// droop, so a strip runs as a wedge from one tab to the next). Where the
/// strips cross, a stitched tab hangs `tab` down, `tab_wide` wide at the
/// strip. `border` is half the grid's outer strip round the face (0: as an
/// upright). `turn` turns the cells on the face (radians), and `dome` is how
/// far a round grid's front bulges (its height at the middle over the
/// radius): its strips stand square to the dome, so off the middle they lean
/// out and show their faces, more toward the rim. `warp` is how far the
/// cloth lattice wanders off true (cell widths, over a few cells): sewn by
/// hand and hung limp, its rows and columns wave. `face_light` is what a
/// strip lets through where its face shows (the droop, the twist, a lean),
/// a sheen on the black cloth lit by the face behind it; edge-on it lets
/// through `FABRIC_WALL_LIGHT`.
struct FabricLook {
    cells: f32,
    upright: f32,
    twist: f32,
    bow: f32,
    across: f32,
    sag: f32,
    wedge: bool,
    tab: f32,
    tab_wide: f32,
    border: f32,
    turn: f32,
    dome: f32,
    warp: f32,
    face_light: f32,
}

/// The strip's egg-crate (RLS020): 7 cells across its 30 cm, 4.3 cm each.
const FABRIC_STRIP: FabricLook = FabricLook { cells: FABRIC_CELLS, upright: 0.045, twist: 0.05, bow: 0.04, across: 0.025, sag: 0.15, wedge: false, tab: 0.12, tab_wide: 0.07, border: 0.0, turn: 0.0, dome: 0.0, warp: 0.0, face_light: FABRIC_WALL_LIGHT };
/// A softbox's (RLS016): 10 cells across its 60 cm, 6 cm each, sewn of the
/// same strips; its crossing strips taut, twisted at the tabs rather than
/// drooping, and its outer strip a dark band round the face.
const FABRIC_BOX: FabricLook = FabricLook { cells: 10.0, upright: 0.04, twist: 0.05, bow: 0.03, across: 0.035, sag: 0.12, wedge: true, tab: 0.085, tab_wide: 0.05, border: 0.55, turn: 0.0, dome: 0.0, warp: 0.06, face_light: 0.15 };
/// A round one (RLS029, a 115 cm parabolic's): 27 cells across, 4.3 cm as
/// the strip's, turned on the face as it was hung, the strips alike both
/// ways and barely drooping, on a front that bulges a tenth of the radius.
const FABRIC_ROUND: FabricLook = FabricLook { cells: 27.0, upright: 0.03, twist: 0.025, bow: 0.05, across: 0.03, sag: 0.04, wedge: true, tab: 0.05, tab_wide: 0.03, border: 0.0, turn: 0.6, dome: 0.08, warp: 0.18, face_light: 0.15 };

impl FabricGrid {
    fn look(self) -> &'static FabricLook {
        match self {
            FabricGrid::Strip => &FABRIC_STRIP,
            FabricGrid::Box => &FABRIC_BOX,
            FabricGrid::Round => &FABRIC_ROUND,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Aim {
    /// The aimed face's normal (the way it shines) and its right and up axes.
    n: Vec3f,
    r: Vec3f,
    u: Vec3f,
    half: Vec2f,
    /// A honeycomb cell's width (flat side to flat side), tangent units.
    cell: f32,
    /// Cell depth over width; 0 without a grid.
    grid_depth: f32,
    /// Half a cell wall's thickness, in cell widths.
    grid_wall: f32,
    /// How much of its shadow the honeycomb casts, 0..=1: it fades in over
    /// the first quarter of the grid.
    grid_contrast: f32,
    /// What of the face the centre sees as it turns toward edge-on, 0..=1,
    /// the same all over it.
    fade: f32,
    /// The grid is a soft light's sewn fabric egg-crate (`fabric_grid`), not a honeycomb.
    fabric: Option<FabricGrid>,
    /// Each door's flap angle from lying flat outward, radians: top, right, bottom, left.
    flaps: [f32; 4],
    /// The soft edge of a door's shadow on each axis, tangent units.
    soft: Vec2f,
}

impl Aim {
    /// The aim of a light whose face looks back along `center` (the unit
    /// direction to the light), with `right` the viewer's right and `up`
    /// spanning the face, tangent half extents `half`, and edge `softness`
    /// 0..=1 (`LightFx::new` passes the light's as clamp() holds it; a NaN
    /// reads as 0). None when the aim is neutral, so a plain light pays
    /// nothing. The aim is read as clamp() holds it (`AimParams::clamped`,
    /// D17), so its ranges and its NaN rule are `AimParams::clamp_fields`' alone.
    pub fn new(a: &AimParams, center: Vec3f, right: Vec3f, up: Vec3f, half: Vec2f, softness: f32) -> Option<Aim> {
        let a = a.clamped();
        let (yaw, pitch, grid, doors) = (a.yaw_deg.to_radians(), a.pitch_deg.to_radians(), a.grid, a.barn_doors);
        if yaw == 0.0 && pitch == 0.0 && grid == 0.0 && doors.iter().all(|d| *d == 0.0) {
            return None;
        }
        // Facing the centre, then yawed about the face's up axis toward the
        // viewer's right, then pitched upward about its (yawed) right axis.
        let n0 = -center;
        let n1 = n0 * yaw.cos() + right * yaw.sin();
        let r = right * yaw.cos() - n0 * yaw.sin();
        let n = n1 * pitch.cos() + up * pitch.sin();
        let u = up * pitch.cos() - n1 * pitch.sin();
        let half = vec2f(half.x.max(1.0e-4), half.y.max(1.0e-4));
        // A number handed in, held to its own 0..=1 (max/min: a NaN reads as 0).
        let edge = 0.01 + 0.05 * softness.max(0.0).min(1.0);
        Some(Aim {
            n,
            r,
            u,
            half,
            cell: 2.0 * half.x.min(half.y) / GRID_CELLS,
            grid_depth: grid * GRID_DEPTH_MAX,
            // The walls thin out with the grid, and the honeycomb fades in
            // with them (D9, L:188): at full contrast every cell edge darkened
            // to half at grid 0.001, and off the axis of a wide light a
            // shallow grid's shadow bands, thinner than a texel, were drawn
            // as a dotted honeycomb down to what is behind the light.
            grid_wall: GRID_WALL * (4.0 * grid).min(1.0),
            grid_contrast: (4.0 * grid).min(1.0),
            // The face's own turn, n against the way back to the centre
            // (cos yaw x cos pitch), fades the whole face: per ray, a line swept
            // across a wide face from yaw 70 on and left its near half lit at 90.
            fade: smoothstep(0.0, EDGE_ON_FADE, n.dot(n0)),
            fabric: None,
            flaps: doors.map(|d| (2.0 * d).to_radians()),
            soft: vec2f(edge * half.x, edge * half.y),
        })
    }

    /// The way the face shines.
    pub fn normal(&self) -> Vec3f {
        self.n
    }

    /// What leaves the face's middle toward the receiver that `dir` runs
    /// from (`dir` from it to the face, as `transmission`'s from the centre):
    /// the doors and the grid, without the edge-on fade the centre sees.
    pub fn beam(&self, dir: Vec3f) -> f32 {
        if !(self.fade > 0.0) {
            // Edge-on to the centre: the face still lights the room; measure it unfaded.
            let open = Aim { fade: 1.0, ..self.clone() };
            return open.transmission(dir, Vec2f::default());
        }
        self.transmission(dir, Vec2f::default()) / self.fade
    }

    /// The same aim with its grid sewn of fabric (a strip's, a softbox's or
    /// an octabox's, an umbrella's) instead of a honeycomb.
    pub fn fabric_grid(self, kind: FabricGrid) -> Aim {
        Aim { fabric: Some(kind), ..self }
    }

    /// A honeycomb hangs on the face.
    pub fn has_grid(&self) -> bool {
        self.grid_depth > 0.0
    }

    /// The fraction 0..=1 of the face at face coordinates `q` (`dir` is the
    /// direction through it) that the centre sees past the doors and through
    /// the grid.
    pub fn transmission(&self, dir: Vec3f, q: Vec2f) -> f32 {
        // The ray from the face to the centre, in the face's own axes.
        let v = -dir;
        let vz = v.dot(self.n);
        if vz.is_nan() {
            return 0.0;
        }
        // Its slopes toward the face's right and up, per unit along its
        // normal. A ray past 90 degrees from the normal grazes the face: its
        // slopes go on at the clamp, as they reach it just short of 90.
        let vz = vz.max(1.0e-6);
        let tx = (v.dot(self.r) / vz).max(-50.0).min(50.0);
        let ty = (v.dot(self.u) / vz).max(-50.0).min(50.0);
        // A face turned nearly edge-on fades out as a whole.
        let mut t = self.fade;
        t *= door(q.y, self.half.y, self.flaps[0], ty, self.soft.y);
        t *= door(q.x, self.half.x, self.flaps[1], tx, self.soft.x);
        t *= door(-q.y, self.half.y, self.flaps[2], -ty, self.soft.y);
        t *= door(-q.x, self.half.x, self.flaps[3], -tx, self.soft.x);
        if self.grid_depth > 0.0 {
            t *= self.grid(q, tx, ty);
        }
        t
    }

    /// The honeycomb: hexagonal tubes on the face, `grid_depth` cell widths
    /// deep. The ray that leaves the face at `q` with slopes (tx, ty) comes
    /// out of a tube's mouth displaced by depth x slope; it gets out only if
    /// that point is still inside the cell it started in. Both ends of the
    /// ray are measured against that one cell's walls, so the cell pattern
    /// and the falloff with the angle are the same computation, and a ray
    /// along the axis sees the whole cell less its walls. A shallow grid
    /// casts only part of that shadow (`grid_contrast`).
    fn grid(&self, q: Vec2f, tx: f32, ty: f32) -> f32 {
        if let Some(kind) = self.fabric {
            return self.fabric(q, tx, ty, kind.look());
        }
        let at = hex_cell(q / self.cell);
        let out = vec2f(at.x + tx * self.grid_depth, at.y + ty * self.grid_depth);
        let worst = hex_dist(at).max(hex_dist(out));
        let open = 0.5 - self.grid_wall;
        1.0 - self.grid_contrast * smoothstep(open - GRID_EDGE, open + GRID_EDGE, worst)
    }

    /// The fabric egg-crate (`FABRIC_CELLS`): square cells laid edge to edge
    /// over the face, as many as fit, its strips `grid_depth` cell widths
    /// deep. As with the honeycomb, the ray leaving the face at `q` comes out
    /// of the grid's front displaced by depth x slope: the strips' back edges
    /// are measured where it leaves the face, their front edges (the droop
    /// and the tabs) where it comes out, both against the cell it started in.
    ///
    /// The face is drawn as its photo shows it, from far off: every cell is
    /// seen along the one slope its middle is seen along, so the strips keep
    /// their width from end to end (the centre, close to a wide face, would
    /// see its far cells closed to slits by the depth, which no plate shows).
    /// A ray from the face's middle (`beam`'s, q = 0) is as before. The slope
    /// from the face's middle is the ray's less `q`'s own share (a face one
    /// tangent unit off sees a point `q` along its slopes as -q). A domed
    /// grid's strips lean out with the dome, which adds its slope.
    fn fabric(&self, q: Vec2f, tx: f32, ty: f32, g: &FabricLook) -> f32 {
        let (mut q, mut tx, mut ty) = (q, tx + q.x, ty + q.y);
        if g.dome > 0.0 {
            // The dome's slope at q, its height dome R (1 - (q / R)^2): the
            // strips stand square to it, so the way out of a cell leans as much.
            let k = 2.0 * g.dome / self.half.x.min(self.half.y);
            tx -= k * q.x;
            ty -= k * q.y;
        }
        if g.turn != 0.0 {
            let (s, c) = g.turn.sin_cos();
            q = vec2f(c * q.x + s * q.y, c * q.y - s * q.x);
            (tx, ty) = (c * tx + s * ty, c * ty - s * tx);
        }
        let w = 2.0 * self.half.x.min(self.half.y) / g.cells;
        if g.warp > 0.0 {
            // The lattice wanders: a low noise over about four cells moves it.
            let (a, b) = (q.x / (4.0 * w), q.y / (4.0 * w));
            q = vec2f(q.x + g.warp * w * (2.0 * fbm2(a + 3.7, b + 1.3, 2, 0x51ab) - 1.0), q.y + g.warp * w * (2.0 * fbm2(a + 8.1, b + 5.9, 2, 0x52cd) - 1.0));
        }
        let nx = (2.0 * self.half.x / w).round().max(1.0);
        let ny = (2.0 * self.half.y / w).round().max(1.0);
        let (wx, wy) = (2.0 * self.half.x / nx, 2.0 * self.half.y / ny);
        let p = vec2f((q.x + self.half.x) / wx, (q.y + self.half.y) / wy);
        let (i, j) = (p.x.floor(), p.y.floor());
        let back = vec2f(p.x - i - 0.5, p.y - j - 0.5);
        let depth = self.grid_depth * w;
        let front = vec2f(back.x + tx * depth / wx, back.y + ty * depth / wy);
        let (ii, jj) = (i as i32, j as i32);
        let h = |a: i32, b: i32, k: u32| hash01(a, b, 0, 0x6a1d_5eed ^ k);
        // 1 clear past a strip's edge `d` (cell widths), 0 on it.
        let clear = |d: f32| smoothstep(-FABRIC_EDGE, FABRIC_EDGE, d);
        // The grid's outer strip, at the face's edges, where it has one.
        let outer = |k: i32, last: f32| g.border > 0.0 && (k == 0 || k == last as i32);
        // The upright strips at the cell's left (i) and right (i + 1): their
        // half width and bow from row to row, eased along the cell's height.
        let along = smoothstep(0.0, 1.0, back.y + 0.5);
        let upright = |k: i32| {
            if outer(k, nx) {
                return (g.border, 0.0);
            }
            let lerp = |a: f32, b: f32| a + (b - a) * along;
            let t = lerp(h(k, jj, 1), h(k, jj + 1, 1));
            let b = lerp(h(k, jj, 2), h(k, jj + 1, 2)) * 2.0 - 1.0;
            (g.upright + g.twist * t * t, g.bow * b)
        };
        let (left, right) = (upright(ii), upright(ii + 1));
        let uprights = |v: Vec2f| clear(v.x + 0.5 - left.1 - left.0) * clear(0.5 - v.x + right.1 - right.0);
        // The crossing strips under the cell (j) and over it (j + 1), half
        // their thickness edge-on.
        let under = if outer(jj, ny) { g.border } else { g.across };
        let over = if outer(jj + 1, ny) { g.border } else { g.across };
        // The crossing strip over the cell (j + 1) droops into it at its
        // front; the one under it (j) is seen edge-on.
        let s = (front.x + 0.5).clamp(0.0, 1.0);
        let sag = if g.wedge {
            // Twisted at the tabs: each crossing holds the strip at its own
            // droop, a little more between them.
            let (a, b) = (h(ii, jj + 1, 3), h(ii + 1, jj + 1, 3));
            g.sag * (a + (b - a) * s) * (1.0 + 2.4 * s * (1.0 - s))
        } else {
            g.sag * (0.5 + h(ii, jj + 1, 3)) * 4.0 * s * (1.0 - s)
        };
        // The tab under the crossing at the cell's nearer top corner.
        let reach = g.tab * (0.6 + 0.8 * h(ii + (front.x > 0.0) as i32, jj + 1, 4));
        let down = 0.5 - over - front.y;
        let tab = clear(0.5 - front.x.abs() - g.tab_wide * (1.0 - down / reach).max(0.0));
        let open_back = uprights(back) * clear(back.y + 0.5 - under) * clear(0.5 - back.y - over);
        let open_front = uprights(front) * clear(down - sag) * clear(front.y + 0.5 - under) * tab;
        if g.face_light == FABRIC_WALL_LIGHT {
            let open = open_back * open_front;
            return 1.0 - self.grid_contrast * (1.0 - FABRIC_WALL_LIGHT) * (1.0 - open);
        }
        // Edge-on where the strips meet the face, their faces past it.
        let blocked = (1.0 - FABRIC_WALL_LIGHT) * (1.0 - open_back) + (1.0 - g.face_light) * open_back * (1.0 - open_front);
        1.0 - self.grid_contrast * blocked
    }
}

/// The offset of `p` from the centre of its honeycomb cell. Cells are regular
/// hexagons one unit across between their flat sides, which face left and
/// right; rows are sqrt(3)/2 apart and every other row is shifted by half a
/// unit. The lattice is two interleaved rectangular grids of 1 x sqrt(3):
/// the nearer of the two candidate centres is the cell's.
fn hex_cell(p: Vec2f) -> Vec2f {
    let a = vec2f(p.x - p.x.floor() - 0.5, p.y - SQRT_3 * (p.y / SQRT_3).floor() - 0.5 * SQRT_3);
    let (sx, sy) = (p.x - 0.5, p.y - 0.5 * SQRT_3);
    let b = vec2f(sx - sx.floor() - 0.5, sy - SQRT_3 * (sy / SQRT_3).floor() - 0.5 * SQRT_3);
    if a.x * a.x + a.y * a.y < b.x * b.x + b.y * b.y {
        a
    } else {
        b
    }
}

/// How far the offset `v` from a cell's centre is toward that cell's edge:
/// 0 at the centre, 0.5 on the hexagon.
fn hex_dist(v: Vec2f) -> f32 {
    let (x, y) = (v.x.abs(), v.y.abs());
    x.max(0.5 * x + 0.5 * SQRT_3 * y)
}

/// How much of the face shows at coordinate `y0` along one axis (half size
/// `h`), past the flap hinged on that axis's + edge. The flap is as long as
/// the half size (two opposite doors closed meet in the middle). It is folded
/// `flap` radians from lying flat outward (0), through straight out (π/2), to
/// closed across the face (π). `slope` is the slope, toward that edge, of
/// the ray from the face to the centre. The flap's tip sits at
/// (h + h·cos flap) along the axis and (h·sin flap) out from the face; the
/// ray crosses the flap, and is blocked, exactly when it starts on the hinge
/// side of the point from which it would graze the tip:
/// y0 > h + h·(cos flap − sin flap · slope).
fn door(y0: f32, h: f32, flap: f32, slope: f32, soft: f32) -> f32 {
    if !(flap > 0.0) {
        return 1.0;
    }
    let edge = h + h * (flap.cos() - flap.sin() * slope);
    1.0 - smoothstep(edge - soft, edge + soft, y0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::light_fx::face_axes;

    /// An aimed square light `size_deg` across, due east on the horizon,
    /// with the axes its face coordinates run along.
    struct Face {
        aim: Aim,
        center: Vec3f,
        right: Vec3f,
        up: Vec3f,
        half: Vec2f,
    }

    impl Face {
        fn new(a: AimParams, size_deg: f32) -> Face {
            let light = LightParams { azimuth_deg: 90.0, elevation_deg: 0.0, width_deg: size_deg, height_deg: size_deg, ..LightParams::default() };
            let (frame, sx, half) = face_axes(&light);
            let right = frame.right * sx;
            let aim = Aim::new(&a, frame.center, right, frame.up, half, 0.2).expect("not neutral");
            Face { aim, center: frame.center, right, up: frame.up, half }
        }

        /// What shows at face coordinates (x, y), given as fractions of the half size.
        fn at(&self, x: f32, y: f32) -> f32 {
            let q = vec2f(x * self.half.x, y * self.half.y);
            let dir = (self.center + self.right * q.x + self.up * q.y).normalize();
            self.aim.transmission(dir, q)
        }

        fn mean(&self) -> f32 {
            let n = 120;
            let mut sum = 0.0f64;
            for j in 0..n {
                for i in 0..n {
                    sum += self.at((i as f32 + 0.5) / n as f32 * 2.0 - 1.0, (j as f32 + 0.5) / n as f32 * 2.0 - 1.0) as f64;
                }
            }
            (sum / (n * n) as f64) as f32
        }
    }

    #[test]
    fn a_neutral_aim_is_no_aim() {
        assert!(AimParams::default().is_neutral());
        let x = vec3f(1.0, 0.0, 0.0);
        let (right, up, half) = (vec3f(0.0, 0.0, 1.0), vec3f(0.0, 1.0, 0.0), vec2f(0.3, 0.2));
        assert!(Aim::new(&AimParams::default(), x, right, up, half, 0.2).is_none());
        // NaN reads as 0, so a broken aim is still no aim.
        let broken = AimParams { yaw_deg: f32::NAN, ..AimParams::default() };
        assert!(Aim::new(&broken, x, right, up, half, 0.2).is_none());
    }

    #[test]
    fn a_bare_face_keeps_its_brightness_until_it_is_edge_on() {
        // A diffuse face is as bright from the side as from the front.
        let turned = Face::new(AimParams { yaw_deg: 40.0, ..AimParams::default() }, 20.0);
        assert_eq!(turned.at(0.0, 0.0), 1.0);
        assert_eq!(turned.at(0.8, -0.8), 1.0);
        let edge_on = Face::new(AimParams { yaw_deg: 90.0, ..AimParams::default() }, 20.0);
        assert!(edge_on.at(0.0, 0.0) < 0.01);
    }

    #[test]
    fn a_closed_door_hides_its_half_of_the_face() {
        let top = Face::new(AimParams { barn_doors: [90.0, 0.0, 0.0, 0.0], ..AimParams::default() }, 40.0);
        assert!(top.at(0.0, 0.5) < 0.01, "the top half is behind the door");
        assert!(top.at(0.0, -0.5) > 0.99);
        assert!(top.at(0.5, -0.5) > 0.99);
        // The second door is the one on the viewer's right.
        let right = Face::new(AimParams { barn_doors: [0.0, 90.0, 0.0, 0.0], ..AimParams::default() }, 40.0);
        assert!(right.at(0.5, 0.0) < 0.01);
        assert!(right.at(-0.5, 0.0) > 0.99);
        let bottom_left = Face::new(AimParams { barn_doors: [0.0, 0.0, 90.0, 90.0], ..AimParams::default() }, 40.0);
        assert!(bottom_left.at(0.5, 0.5) > 0.99, "only the top right quarter shows");
        assert!(bottom_left.at(-0.5, 0.5) < 0.01 && bottom_left.at(0.5, -0.5) < 0.01);
    }

    #[test]
    fn a_door_standing_straight_out_hides_the_top_seen_from_above() {
        // Pitched 30 degrees down, the face is seen from above its axis, so a
        // top door standing straight out (45) hides the top of the face.
        let down = Face::new(AimParams { pitch_deg: -30.0, barn_doors: [45.0, 0.0, 0.0, 0.0], ..AimParams::default() }, 40.0);
        assert!(down.at(0.0, 0.9) < 0.01);
        assert!(down.at(0.0, -0.5) > 0.99);
        // Seen from below (pitched up), the same door hides nothing there.
        let up = Face::new(AimParams { pitch_deg: 30.0, barn_doors: [45.0, 0.0, 0.0, 0.0], ..AimParams::default() }, 40.0);
        assert!(up.at(0.0, 0.9) > 0.99);
    }

    #[test]
    fn a_grid_darkens_the_face_as_it_turns_away() {
        let means: Vec<f32> = [0.0f32, 8.0, 16.0]
            .iter()
            .map(|&yaw| Face::new(AimParams { yaw_deg: yaw, grid: 0.5, ..AimParams::default() }, 10.0).mean())
            .collect();
        assert!(means[0] > 0.6, "{means:?}");
        assert!(means[0] > means[1] && means[1] > means[2], "{means:?}");
        let cut = Face::new(AimParams { yaw_deg: 30.0, grid: 1.0, ..AimParams::default() }, 10.0);
        assert!(cut.mean() < 0.02, "a 15 degree grid turned 30 degrees away shows nothing");
        // Turned to the viewer's right, the right side of the face is the
        // further off its axis and goes dark first.
        let turned = Face::new(AimParams { yaw_deg: 10.0, grid: 0.6, ..AimParams::default() }, 30.0);
        let side = |x: f32| (0..31).map(|k| turned.at(x, (k as f32 - 15.0) / 20.0)).sum::<f32>() / 31.0;
        assert!(side(-0.7) > 0.4 && side(0.7) < 0.05, "left {}, right {}", side(-0.7), side(0.7));
    }

    /// D9 (L:187): a face turned toward edge-on fades out as a whole over the
    /// last five degrees of its turn, whatever its width: no fade line sweeps
    /// across a wide face from yaw 70 on, and at 89-90 no half of it stays at
    /// full brightness behind a hard edge.
    #[test]
    fn a_face_turned_edge_on_fades_out_as_a_whole() {
        for size in [20.0f32, 60.0, 120.0] {
            for (yaw_deg, pitch_deg) in [(70.0f32, 0.0f32), (80.0, 0.0), (86.0, 0.0), (88.0, 0.0), (89.5, 0.0), (90.0, 0.0), (0.0, 88.0), (-60.0, 81.5)] {
                let face = Face::new(AimParams { yaw_deg, pitch_deg, ..AimParams::default() }, size);
                let ts: Vec<f32> = (0..41 * 41).map(|k| face.at(-0.95 + 0.0475 * (k % 41) as f32, -0.95 + 0.0475 * (k / 41) as f32)).collect();
                let (lo, hi) = (ts.iter().cloned().fold(1.0f32, f32::min), ts.iter().cloned().fold(0.0f32, f32::max));
                assert!(hi - lo < 1.0e-6, "{size} degree face turned ({yaw_deg}, {pitch_deg}): {lo}..{hi} across it");
                // The face's own turn: the angle between where it points and the centre.
                let turn = (yaw_deg.to_radians().cos() * pitch_deg.to_radians().cos()).acos().to_degrees();
                if turn <= 85.0 {
                    assert_eq!(hi, 1.0, "{size} degrees, turned {turn}: still whole");
                } else if turn >= 89.99 {
                    assert!(hi < 1.0e-6, "{size} degrees, turned {turn}: gone");
                } else if turn >= 85.5 {
                    assert!(hi > 0.0 && hi < 1.0, "{size} degrees, turned {turn}: fading, {hi}");
                }
            }
        }
    }

    /// D9 (L:188): the first steps of the Grid slider ease in from no grid.
    /// The honeycomb fades in with the walls, so a grid of 0.01 does not draw
    /// a full-contrast honeycomb (every cell edge at 0.5, the face 3 % darker)
    /// over the light.
    #[test]
    fn the_first_grid_steps_ease_in_from_none() {
        // The mean over a face, and the share of it darker than 0.9.
        let measure = |grid: f32, size: f32| {
            let face = Face::new(AimParams { grid, ..AimParams::default() }, size);
            let n = 120;
            let ts: Vec<f32> = (0..n * n).map(|k| face.at((k % n) as f32 / n as f32 * 1.9 - 0.95, (k / n) as f32 / n as f32 * 1.9 - 0.95)).collect();
            let mean = ts.iter().sum::<f32>() / ts.len() as f32;
            (mean, ts.iter().filter(|t| **t < 0.9).count() as f32 / ts.len() as f32)
        };
        // Along its axis (a 2 degree face, every ray within a degree of it) a
        // grid of 0.001 is all but none, and 0.01 dims the face by under a per
        // cent (its walls, at 4 % of their full thickness, casting 4 % of
        // their shadow): both drew every cell edge at half, the face 3 %
        // darker and a tenth of it under 0.9.
        for (grid, least, most_lined) in [(0.001f32, 0.995f32, 0.01f32), (0.01, 0.99, 0.02)] {
            let (mean, lined) = measure(grid, 2.0);
            assert!(mean > least && lined < most_lined, "grid {grid}: mean {mean}, {:.1} % of the face on a cell edge", 100.0 * lined);
        }
        // From there the walls close in steadily. Off the axis a shallow
        // grid's walls also cast thin shadows, as long as the grid is deep.
        let means: Vec<f32> = [0.001f32, 0.01, 0.05, 0.1, 0.25, 0.5].iter().map(|&grid| measure(grid, 20.0).0).collect();
        assert!(means[0] > 0.995 && means[1] > 0.98, "{means:?}");
        for w in means.windows(2) {
            assert!(w[0] >= w[1], "{means:?}");
        }
        assert!(means[5] < 0.97, "a grid of 0.5 has walls: {means:?}");
    }

    /// The face's transmission on an n x n raster over a square face
    /// `size_deg` across (fractions -0.95..0.95 of its half size), row by row.
    fn raster(grid: f32, size_deg: f32, n: usize) -> (Face, Vec<f32>) {
        let face = Face::new(AimParams { grid, ..AimParams::default() }, size_deg);
        let ts = (0..n * n).map(|k| face.at(raster_frac(k % n, n), raster_frac(k / n, n))).collect();
        (face, ts)
    }

    /// The place of the raster's `i`th column or row, as a fraction of the half size.
    fn raster_frac(i: usize, n: usize) -> f32 {
        (i as f32 + 0.5) / n as f32 * 1.9 - 0.95
    }

    /// D9 (L:188) off the axis: on a wide light every ray but the middle's
    /// is well off the grid's axis, and a shallow grid's walls cast shadow
    /// bands thinner than a texel there. Point-sampled at full contrast they
    /// drew a dotted honeycomb down to what is behind the light: on a 100
    /// degree face at grid 0.001 and 0.01 the darkest point was 0, and grid
    /// 0.01 lost 4.4 % of the light (3.7 % of the review's 100 x 80 light in
    /// the app). The honeycomb fades in with the grid instead: nowhere on the
    /// face is it darker than 1 - 4 grid.
    #[test]
    fn a_shallow_grid_draws_no_honeycomb_on_a_wide_light() {
        for grid in [0.001f32, 0.01, 0.05, 0.1] {
            let (_, ts) = raster(grid, 100.0, 240);
            let darkest = ts.iter().cloned().fold(1.0f32, f32::min);
            let mean = ts.iter().sum::<f32>() / ts.len() as f32;
            assert!(darkest >= 1.0 - 4.0 * grid - 1.0e-5, "grid {grid}: darkest {darkest} on a 100 degree face, mean {mean}");
            if grid <= 0.01 {
                assert!(mean > 0.995, "grid {grid}: the face keeps {mean} of its light");
            }
        }
    }

    /// The softening of the cells' edges is the honeycomb's only
    /// anti-aliasing, so it keeps its width at every grid: within a cell the
    /// face never changes faster than the full-contrast edge's steepest slope
    /// (1.5 / (2 GRID_EDGE) per cell width) times the honeycomb's contrast.
    /// Thinned with the walls, the edge left grid 0.05's shadow bands as hard
    /// sub-texel lines, broken where the bake's texels missed them.
    #[test]
    fn a_grid_s_shadows_keep_their_soft_edge() {
        for grid in [0.01f32, 0.05, 0.1, 0.25, 0.5] {
            let n = 600;
            let (face, ts) = raster(grid, 100.0, n);
            // The raster's step in cell widths, and the centre of each sample's cell.
            let cells = face.half.x / face.aim.cell;
            let step = 1.9 * cells / n as f32;
            let cell_centre = |k: usize| {
                let p = vec2f(raster_frac(k % n, n) * cells, raster_frac(k / n, n) * cells);
                p - hex_cell(p)
            };
            let steepest = (4.0 * grid).min(1.0) * 1.5 / (2.0 * GRID_EDGE);
            let mut worst = 0.0f32;
            // Each sample against its right and upper neighbours in the same cell.
            for k in 0..n * n {
                for next in [k + 1, k + n] {
                    if next >= n * n || (next == k + 1 && next % n == 0) || (cell_centre(k) - cell_centre(next)).length() > 1.0e-3 {
                        continue;
                    }
                    worst = worst.max((ts[k] - ts[next]).abs() / step);
                }
            }
            assert!(worst <= 1.01 * steepest, "grid {grid}: {worst} per cell width, at most {steepest}");
        }
    }

    #[test]
    fn the_honeycomb_cells_are_hexagons_one_unit_across() {
        // A cell is centred on the origin, and the next one sits one unit to the right.
        assert!(hex_cell(vec2f(0.0, 0.0)).length() < 1.0e-6);
        let near = hex_cell(vec2f(0.49, 0.0));
        assert!((near.x - 0.49).abs() < 1.0e-5 && near.y.abs() < 1.0e-5);
        let next = hex_cell(vec2f(0.51, 0.0));
        assert!((next.x + 0.49).abs() < 1.0e-5, "{next:?}");
        // The row above is shifted by half a cell.
        assert!(hex_cell(vec2f(0.5, 0.866_025_4)).length() < 1.0e-5);
        // Every point is within half a unit (the cell's inner radius) of its cell's edge distance.
        for k in 0..400 {
            let p = vec2f(k as f32 * 0.137 - 20.0, k as f32 * 0.291 - 37.0);
            let d = hex_dist(hex_cell(p));
            assert!(d >= 0.0 && d <= 0.5 + 1.0e-5, "{p:?}: {d}");
        }
        assert!((hex_dist(vec2f(0.5, 0.0)) - 0.5).abs() < 1.0e-6, "the flat sides face left and right");
    }
}
