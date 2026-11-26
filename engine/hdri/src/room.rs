//! The studio's room (`StudioParams::room`): a box round the camera that the
//! lights stand in and light, in place of the backdrop gradient.
//!
//! The camera is at the origin of the map's frame, `camera_height_m` above
//! the floor; the room's walls are planes at its width and depth (north is
//! -z, azimuth 0; east +x, azimuth 90), its ceiling a plane at its height. A
//! cyc cove rounds the floor into the back wall (the one at azimuth 0) or,
//! for "all", into every wall, and then rounds the corners between the walls
//! too, as a photographer's cyc does. Each view ray is cast once into that
//! shape (`Room::cast`): the planes analytically, a cove by bisecting its
//! distance between the box it rounds off and the planes.
//!
//! Every light stands in the room at its distance (`LightParams::distance_m`,
//! else 70 % of the way to the surface it faces), as large as the camera
//! sees it, facing the camera or its aim; a light placed in 3D (plan P1,
//! `LightParams::space`) stands where it is, facing its own way, its stand
//! under its middle. A light longer than a piece stands
//! in it as pieces, each such a light where that piece is seen (a long LED
//! tube or strip lights the room all along itself): a stroke's along its
//! centre line, any other's across its long side, on its face. Its light on
//! a surface is a Lambertian emitter's: `I0 cos(at the light) cos(at the
//! surface) / (r^2 + A / pi)`, where `I0` comes from what the map itself
//! draws of the light (its whole emission, past what hangs in front of it
//! and under the flags after it, `Studio::build_room`) and `A / pi` keeps a
//! surface right by a big softbox from blowing up, as a disc's own form
//! factor does. A grid and barn doors narrow its beam (`LightFx::beam`); its
//! gobo, when it casts onto the backdrop, patterns its pool (cast from its
//! middle: such a light stands whole). The light the room bounces round itself
//! is one even irradiance (`ambient`): what the lights put out, times the
//! room's mean albedo over its area and what that albedo absorbs. There are
//! no shadows in the room.
//!
//! The lights' light on the room's six flat surfaces is smooth (plan P3):
//! a light whose beam nothing shapes is a Lambertian face, whose irradiance
//! on a plane changes over about its distance from it. So each plane keeps a
//! grid of what those lights give it (`PlaneGrid`), its spacing GRID_SHARE
//! of the nearest such light's distance from the part of the plane it
//! lights, and a texel on the plane reads it bilinearly. Where a light's own
//! plane cuts the room's (behind its face it gives nothing, a crease no
//! blend follows), the cells the cut crosses add that light exactly. The
//! lights whose beam is shaped (a grid, barn doors, a spot's beam, a gobo
//! that casts: their pools have edges), the daylight's discs and every
//! surface off the planes (a cove, a stand, a grid pipe) sum the lights per
//! lookup, as before. A grid takes at most GRID_NODES_MAX nodes: the lights
//! that stand so close to a plane that its grid would need more are left out
//! of it and summed per lookup on that plane, exactly, nearest first until
//! the rest fit (said once in the log). The grids do not depend on the map's
//! width, so a light close to a wall costs a preview no more than summing per
//! texel.
//!
//! A glossy floor mirrors the room (cast again from the floor) and the
//! lights (`Mirror`), one light of the studio's list at a time (each alone
//! over black, under the flags after it). Each light is looked up where the
//! mirrored ray meets its own place in the room (`MirrorProxy`): the plane
//! its face stands on (a 2D light's facing the camera at its distance, a 3D
//! light's its face's), or for a stroke, whose pieces follow its line, a
//! sphere as far off as they stand. A planar floor's image of a light there
//! is the light seen from the mirrored eye, so where the floor's lobe is
//! narrow the light is drawn along the camera's direction to that point,
//! for the footprint the map's texel has there (a pattern filters its
//! picture over it), as sharp as the map draws it straight on. Where the
//! lobe is wide (a rough floor) a sharp drawing would need many draws per
//! texel to blur, so the light is read from an equirect of it alone blurred
//! through its summed-area table (`MirrorLayer`), its texel half the
//! narrowest lobe it serves and never finer than two of the map's texels
//! (or than MIRROR_LAYER_MAX wide; past MIRROR_LAYERS_MAX such lights, those
//! standing nearest one another share one), and the two hand over between
//! one and two of the layer's texels of lobe. So a light's mirror image lies where a
//! mirror shows it, and a floor point beyond a light (further from the
//! camera) shows none of it: its mirrored ray runs away from the light's
//! plane. Its Fresnel is a dielectric's (Schlick, F0 0.04), scaled by
//! `floor_gloss`.
//!
//! A room whose floor side is open (`RoomParams::open_floor`: a tunnel, a
//! bridge) stands on the scene's ground (`Ground`): its floor's albedo, gloss
//! and roughness are the ground's there, its road (an old file's room road
//! is moved onto the ground's by `HdriParams::clamp`) and what lies beside it.
//!
//! Open walls (plan P3, `RoomParams::open_walls`) are a tunnel's mouths: a
//! view ray that leaves the room through one sees the environment outside
//! (`sky`, the lightless copy a window filled with sky looks out at), and
//! so does the floor's mirror. The outside lights the room through them
//! (`Daylight`): each opening is a uniform area light the size of the
//! opening, its radiance the outside's mean seen through it (cosine weighted
//! over the half of the sky it faces, the discs left out), whose irradiance
//! on a surface is the rectangle's exact form factor (`Opening::on`, cut to
//! the half of the sky the surface faces), so the lip of a mouth sees half
//! its sky and the light falls off inward as in a real tunnel; and each
//! bright disc outside (the sun, the moon) shines straight in where a ray
//! from the surface toward it leaves through an open wall, so a low sun
//! lights a patch inside a tunnel's mouth with the opening's own edges. The
//! floor's mirror of a disc seen through an open wall is spread by the
//! floor's roughness, its energy kept (`Room::outside_mirrored`), and so is
//! the mirror image of the opening itself: the share of the floor's lobe
//! that leaves through it (`OnMouth::share`) shows the outside, the rest
//! the room beside it, and a rough floor reads the outside from a small
//! table of it blurred over its lobe (OUTSIDE_W). An open
//! wall takes no light and reflects none: its area counts in the bounce at
//! albedo 0. A cove under an open wall stays (the floor curves up there and
//! then opens: the opening starts above it), and the plan's corners next to
//! an open wall are square, so a mouth has no rounded jambs.
//!
//! Stands (a pole and three legs under a light on the floor, a drop pipe over
//! one hung near the ceiling) and the ceiling's pipe grid are thin cylinders
//! drawn over the room with an analytic coverage, their edges faded over
//! one fixed width (`LINE_FOOT`), so every lookup draws the same room. They are dark metal lit as the room is.
//! So are the light fittings' bodies (`Housing`, a pattern light's housed tubes: pattern.rs `housings`): boxes
//! behind the lights' faces, their front face on the face, of their own albedo; the light's picture is drawn over
//! them. Each light's are kept in a grid over its face (`HousingGroup`), so a lookup finds the few its ray comes
//! near from the cells under its stretch through the face; for each of those in the cone round one, the nearest
//! approach of the ray to the box (15 pairs of its slabs' sides) and, where it is covered, the irradiance there
//! summed as a stand's is.
//!
//! The floor, the walls and the ceiling are each a material (`RoomParams::floor_material` and the others,
//! room_material.rs) or their flat colour: a pattern round that colour, read in the plane's own metres at the
//! footprint the lookup's texel has there (`Room::foot`: its width over the way to the surface, stretched as the
//! surface turns away), worn by where it lies in the room. A cove blends the floor's into the wall's, a window's
//! reveal is the wall's material, and the floor's mirror reads the material's gloss. A material's mean is its
//! colour, so the bounce (the room's mean albedo) is the same with or without it.
use makepad_draw::*;
use makepad_render_material::ibl::{dir_to_equirect_uv, equirect_uv_to_dir, EnvMap};

use super::image_light::SummedArea;
use super::params::{CoveWalls, RoomParams, RoomWindow, ROOM_WEAR_DEFAULT, ROOM_WINDOW_DEPTH_M};
use super::room_material::{self, RoomMaterial, Side, Surface, Weather};
use super::room_window::{Met, Paint, Styled};
use crate::sky::luminance;

/// A mirror layer (`MirrorLayer`, a light's equirect for the wide lobes of a
/// rough floor) is at most this wide and at least MIRROR_LAYER_MIN (2:1).
/// Its texel is half the narrowest lobe it serves; a lobe finer than this
/// width's two texels is drawn directly instead, a little sharper than the
/// floor's roughness would make it (said in the log). Each layer costs a
/// draw of its light per texel and 24 bytes a texel, for every light the
/// floor mirrors: at 1024 that is 0.5 M draws and 12.6 MB a light, about
/// what a 1K bake of the lights costs.
const MIRROR_LAYER_MAX: usize = 1024;
const MIRROR_LAYER_MIN: usize = 64;
/// The most layers the floor's mirror keeps (`Room::with_mirror`): past it
/// the lights that want one and stand nearest one another share a layer,
/// summed and looked up at their places' mean, which the log says. Each
/// layer is a summed table of up to 12.6 MB, and a floor lookup whose lobe
/// is wide reads every layer; a light still draws its own sharp image where
/// the lobe is narrow, however many share its layer.
pub const MIRROR_LAYERS_MAX: usize = 32;
/// The outside a rough floor mirrors through an open wall is read, blurred
/// over the floor's lobe, from an equirect of it this wide (2:1, its discs
/// left out), padded half a turn each side so a lobe across its seam
/// reads both sides; a lobe within one of its texels reads the sky itself.
/// 128 is 2.8 degrees a texel: a lobe that wide (a floor rougher than about
/// 0.2) is that soft, and the table costs 16 384 lookups of the sky.
const OUTSIDE_W: usize = 128;
/// The floor's lobes a mirror layer serves are found over this many view
/// directions round and down to the floor (`Room::mirror_lobes`).
const MIRROR_PROBE_W: usize = 128;
const MIRROR_PROBE_H: usize = 48;
/// A light with no distance of its own stands this share of the way to the surface it faces.
pub const DEFAULT_REACH: f32 = 0.7;
/// Stands, drop pipes and grid pipes: their radius in metres and their albedo.
const POLE_RADIUS: f32 = 0.02;
const LEG_RADIUS: f32 = 0.014;
const PIPE_RADIUS: f32 = 0.025;
const METAL_ALBEDO: f32 = 0.08;
const PIPE_ALBEDO: f32 = 0.06;
/// The width, in radians, over which a stand's or a pipe's edge fades: a
/// 2048 wide map's texel. One width for every lookup (a bake at any size,
/// its refined texels, the keyless copy), so they all draw the same room.
const LINE_FOOT: f32 = std::f32::consts::TAU / 2048.0;
/// The footprint, metres, a surface beside an opening is read at in the
/// floor's mirror of it (`Room::beside_mouth`): its materials' mean.
const BESIDE_MOUTH_FOOT: f32 = 1.0;
/// How far under the ceiling the grid hangs, metres.
const GRID_DROP: f32 = 0.08;
/// A light whose bottom is within this of the ceiling, or above this share of
/// the room's height, hangs from it (a drop pipe) instead of standing.
const HANG_GAP: f32 = 0.6;
const HANG_SHARE: f32 = 0.78;
/// A tripod's legs: where they meet the pole and how far out they reach, metres.
const LEG_TOP: f32 = 0.6;
const LEG_SPREAD: f32 = 0.5;
/// A plane's irradiance grid (`PlaneGrid`): its spacing as a share of the
/// nearest light's distance from the plane (measured on the studio presets
/// at 4096 against summing per texel: within 0.01 % on the mean and a few
/// percent at the worst texel, a dark one beside a light's cut), and the
/// most nodes one plane's grid takes: a light closer than that allows is
/// summed per lookup on the plane instead. 2^16 nodes is half a 512 wide
/// preview's texels, so a plane's grid never costs more than summing its
/// lights per texel over half that preview (the studio presets' largest
/// grid, a showroom's ceiling, takes about 38 000).
const GRID_SHARE: f32 = 0.08;
const GRID_NODES_MAX: usize = 1 << 16;
/// The nearest a light counts as standing to a plane, metres: no grid is
/// finer than GRID_SHARE of it.
const GRID_NEAR_M: f32 = 0.01;
/// The outside's light through an opening is measured over this many
/// cosine-weighted directions a side (`Opening::daylight`), and a disc in it
/// over this many points a side.
const DAYLIGHT_SIDE: usize = 24;
const DISC_SIDE: usize = 16;

/// The room's flat surfaces, the planes of its box (`Hit::plane`, the grids):
/// the floor, the ceiling, the walls at -x (west), +x (east), -z (north) and +z (south).
const FLOOR: usize = 0;
const CEILING: usize = 1;
const WEST: usize = 2;
const EAST: usize = 3;
const NORTH: usize = 4;
const SOUTH: usize = 5;

/// What holds one light in the room (`Room::with_lights`): where the light
/// stands, metres, and its half height. A light placed by its direction
/// hangs when it reaches near the ceiling or starts high; a face placed in
/// metres (`by_bottom`) hangs only when its bottom is high, so a face
/// turned until a corner nears the ceiling still stands on its tripod.
#[derive(Clone, Copy, Debug)]
pub struct Stand {
    pub at: Vec3f,
    pub half_height: f32,
    pub by_bottom: bool,
}

/// One light in the room.
#[derive(Clone, Debug)]
pub struct RoomLight {
    /// Where it stands (map frame, metres from the camera) and the way it shines.
    pub pos: Vec3f,
    pub normal: Vec3f,
    /// Its radiant intensity along its normal, per channel.
    pub intensity: Vec3f,
    /// Its area over pi: the near-field term of its form factor.
    pub near: f32,
    /// Its entry in the studio's list, for its beam (`LightFx::beam`).
    pub entry: usize,
    /// Whether its grid, doors or gobo shape its beam (else it is a bare Lambertian face).
    pub shaped_beam: bool,
    /// It shines all round (a candle's flame): `intensity` is its intensity
    /// every way and `normal` is not read.
    pub omni: bool,
}

impl RoomLight {
    /// Whether a point at `x` lies in front of it (always, for one that shines all round).
    fn fronts(&self, x: Vec3f) -> bool {
        self.omni || self.normal.dot(x - self.pos) > 0.0
    }
}

/// A thin cylinder from `a` to `b`.
#[derive(Clone, Copy, Debug)]
struct Rod {
    a: Vec3f,
    b: Vec3f,
    radius: f32,
    /// The cone round the camera that holds the rod and its faded edge:
    /// its axis and the cosine of its half angle (-1 when the camera is
    /// inside the rod's bounding sphere). A view ray outside it misses the rod.
    axis: Vec3f,
    cos_reach: f32,
}

impl Rod {
    fn new(a: Vec3f, b: Vec3f, radius: f32) -> Rod {
        let (axis, cos_reach) = reach_cone((a + b) * 0.5, 0.5 * (b - a).length() + radius);
        Rod { a, b, radius, axis, cos_reach }
    }
}

/// The cone round the camera that holds a sphere of radius `reach` round
/// `centre` and a faded edge past it: its axis and the cosine of its half
/// angle (-1 when the camera is inside the sphere).
fn reach_cone(centre: Vec3f, reach: f32) -> (Vec3f, f32) {
    let far = centre.length();
    if far > reach * 1.001 {
        let angle = (reach / far).asin() + LINE_FOOT;
        (centre / far, if angle < std::f32::consts::PI { angle.cos() } else { -1.0 })
    } else {
        (vec3f(0.0, 1.0, 0.0), -1.0)
    }
}

/// A light fitting's body (pattern.rs `housings`): a box round `centre`
/// (map frame, metres from the camera), `half` its half extents along its
/// `axes` (unit, at right angles), of grey `albedo`, lit as the room is.
#[derive(Clone, Copy, Debug)]
pub struct Housing {
    pub centre: Vec3f,
    pub axes: [Vec3f; 3],
    pub half: Vec3f,
    pub albedo: f32,
}

/// Where a view ray from the camera along `d` passes box `h`: the least,
/// over the ray in front of the camera, of how far its point lies outside
/// the box along the axis it is farthest out on (metres, negative inside),
/// and where the ray enters the box grown by that much (or by nothing, where
/// it passes through): its distance and the face's outward normal. The
/// excess along the ray is the greatest of six lines in the distance (the
/// slabs' sides), so its least lies where two of them cross (or at the
/// camera). None where it enters nowhere in front of the camera.
fn ray_to_box(d: Vec3f, h: &Housing) -> Option<(f32, f32, Vec3f)> {
    let o = [-h.centre.dot(h.axes[0]), -h.centre.dot(h.axes[1]), -h.centre.dot(h.axes[2])];
    let v = [d.dot(h.axes[0]), d.dot(h.axes[1]), d.dot(h.axes[2])];
    let half = [h.half.x, h.half.y, h.half.z];
    let excess = |t: f32| (0..3).map(|i| (o[i] + t * v[i]).abs() - half[i]).fold(f32::MIN, f32::max);
    // The six lines: slope s v_i, offset s o_i - half_i.
    let lines: [(f32, f32); 6] = std::array::from_fn(|k| {
        let (i, s) = (k / 2, if k % 2 == 0 { 1.0 } else { -1.0 });
        (s * v[i], s * o[i] - half[i])
    });
    let mut least = excess(0.0);
    for a in 0..6 {
        for b in a + 1..6 {
            let slope = lines[a].0 - lines[b].0;
            if slope.abs() < 1.0e-9 {
                continue;
            }
            let t = (lines[b].1 - lines[a].1) / slope;
            if t > 0.0 {
                least = least.min(excess(t));
            }
        }
    }
    // Where the ray enters the box grown by the excess: the latest of its slabs' near sides.
    let grow = least.max(0.0) + 1.0e-6;
    let (mut t_in, mut t_out, mut normal) = (0.0f32, f32::INFINITY, Vec3f::default());
    for i in 0..3 {
        let hg = half[i] + grow;
        if v[i].abs() < 1.0e-9 {
            if o[i].abs() > hg {
                return None;
            }
            continue;
        }
        let (t1, t2) = ((-hg - o[i]) / v[i], (hg - o[i]) / v[i]);
        let (near, far) = (t1.min(t2), t1.max(t2));
        if near > t_in {
            t_in = near;
            normal = h.axes[i] * if v[i] > 0.0 { -1.0 } else { 1.0 };
        }
        t_out = t_out.min(far);
    }
    (t_in > 0.0 && t_in <= t_out).then_some((least, t_in, normal))
}

/// How far, per metre along a view ray, a point of it can lie outside a
/// housing's box in the group's frame and still be drawn over it: the edge
/// fade (half LINE_FOOT) grown by sqrt 3 for the box's own axes against the
/// frame's, rounded up so that the float error of `ray_to_box` stays inside;
/// and the same in metres at the camera.
const HOUSING_NEAR_SLOPE: f32 = LINE_FOOT;
const HOUSING_NEAR_M: f32 = 1.0e-3;

/// One light's housings (`Room::with_housings`), with a grid over its face
/// that finds the few a view ray comes near: a lookup tests only those, in
/// the light's own order, so a pattern of many tubes costs about what one
/// of its cells does.
#[derive(Debug)]
struct HousingGroup {
    /// Each housing with the cone round the camera that holds it (`reach_cone`).
    housings: Vec<(Housing, Vec3f, f32)>,
    /// The cone round the camera that holds them all (and so each one's).
    axis: Vec3f,
    cos_reach: f32,
    /// The grid's frame: two axes along the face, then the face's normal
    /// (the first housing's axes), and the box holding every housing in it,
    /// metres from the camera.
    frame: [Vec3f; 3],
    lo: [f32; 3],
    hi: [f32; 3],
    /// Cells along the frame's first two axes, and each cell's size, metres.
    cells: [usize; 2],
    cell: [f32; 2],
    /// Per cell (along the first axis, then rows up the second), the
    /// housings whose box in the frame overlaps it, in order: cell k's are
    /// `members[starts[k]..starts[k + 1]]`.
    starts: Vec<u32>,
    members: Vec<u32>,
}

thread_local! {
    /// A lookup's housings from more than one cell, sorted (`HousingGroup::near`).
    static HOUSINGS_NEAR: std::cell::RefCell<Vec<u32>> = const { std::cell::RefCell::new(Vec::new()) };
}

impl HousingGroup {
    /// About one cell per housing, square on the face. Per build, each
    /// housing's eight corners and the cells its box overlaps.
    fn new(housings: Vec<Housing>) -> Option<HousingGroup> {
        let first = *housings.first()?;
        let frame = first.axes;
        // Each housing's box in the frame, from its half extents along its own axes.
        let boxes: Vec<([f32; 3], [f32; 3])> = housings
            .iter()
            .map(|h| {
                let (mut lo, mut hi) = ([0.0f32; 3], [0.0f32; 3]);
                for i in 0..3 {
                    let c = h.centre.dot(frame[i]);
                    let reach = (0..3).map(|k| h.axes[k].dot(frame[i]).abs() * [h.half.x, h.half.y, h.half.z][k]).sum::<f32>();
                    (lo[i], hi[i]) = (c - reach, c + reach);
                }
                (lo, hi)
            })
            .collect();
        let (mut lo, mut hi) = ([f32::MAX; 3], [f32::MIN; 3]);
        for (l, h) in &boxes {
            for i in 0..3 {
                lo[i] = lo[i].min(l[i]);
                hi[i] = hi[i].max(h[i]);
            }
        }
        let n = housings.len();
        let (w, h) = ((hi[0] - lo[0]).max(1.0e-6), (hi[1] - lo[1]).max(1.0e-6));
        let across = ((n as f32 * w / h).sqrt().round() as usize).clamp(1, n);
        let cells = [across, n.div_ceil(across).clamp(1, n)];
        let cell = [w / cells[0] as f32, h / cells[1] as f32];
        let span = |b: &([f32; 3], [f32; 3]), i: usize| {
            let at = |x: f32| (((x - lo[i]) / cell[i]).floor().max(0.0) as usize).min(cells[i] - 1);
            at(b.0[i])..=at(b.1[i])
        };
        // Count each cell's housings, then lay them in, in order.
        let mut starts = vec![0u32; cells[0] * cells[1] + 1];
        for b in &boxes {
            for y in span(b, 1) {
                for x in span(b, 0) {
                    starts[y * cells[0] + x + 1] += 1;
                }
            }
        }
        for k in 1..starts.len() {
            starts[k] += starts[k - 1];
        }
        let mut fill = starts.clone();
        let mut members = vec![0u32; *starts.last().unwrap() as usize];
        for (j, b) in boxes.iter().enumerate() {
            for y in span(b, 1) {
                for x in span(b, 0) {
                    let k = y * cells[0] + x;
                    members[fill[k] as usize] = j as u32;
                    fill[k] += 1;
                }
            }
        }
        // The group's cone: round the sphere holding its box.
        let centre = (0..3).fold(Vec3f::default(), |s, i| s + frame[i] * (0.5 * (lo[i] + hi[i])));
        let radius = (0..3).map(|i| (0.5 * (hi[i] - lo[i])).powi(2)).sum::<f32>().sqrt();
        let (axis, cos_reach) = reach_cone(centre, radius);
        let housings = housings
            .into_iter()
            .map(|h| {
                let (axis, cos_reach) = reach_cone(h.centre, h.half.length());
                (h, axis, cos_reach)
            })
            .collect();
        Some(HousingGroup { housings, axis, cos_reach, frame, lo, hi, cells, cell, starts, members })
    }

    /// Calls `each` on every housing a view ray along `d` from the camera,
    /// before `t_room`, can be drawn over by (`ray_to_box` within the edge
    /// fade, where the ray enters it), in order; it may call it on a few
    /// more. Those it leaves out add nothing. Per lookup: the group's cone,
    /// the stretch of the ray within its box grown by the fade (six slab
    /// sides), the cells under that stretch and their housings, sorted when
    /// more than one cell holds them.
    fn near(&self, d: Vec3f, t_room: f32, mut each: impl FnMut(&(Housing, Vec3f, f32))) {
        if d.dot(self.axis) < self.cos_reach {
            return;
        }
        // Where along the ray its point lies within the box grown by HOUSING_NEAR_M plus HOUSING_NEAR_SLOPE t.
        let v = [d.dot(self.frame[0]), d.dot(self.frame[1]), d.dot(self.frame[2])];
        let (mut t0, mut t1) = (0.0f32, t_room);
        let mut clip = |slope: f32, from: f32| {
            // slope t >= from
            if slope > 0.0 {
                t0 = t0.max(from / slope);
            } else if slope < 0.0 {
                t1 = t1.min(from / slope);
            } else if from > 0.0 {
                t1 = -1.0;
            }
        };
        for i in 0..3 {
            clip(v[i] + HOUSING_NEAR_SLOPE, self.lo[i] - HOUSING_NEAR_M);
            clip(HOUSING_NEAR_SLOPE - v[i], -self.hi[i] - HOUSING_NEAR_M);
        }
        if !(t0 <= t1) {
            return;
        }
        // The cells under that stretch, grown by its reach at its far end.
        let grow = HOUSING_NEAR_M + HOUSING_NEAR_SLOPE * t1;
        let mut range = [0..=0, 0..=0];
        for i in 0..2 {
            let (a, b) = (t0 * v[i], t1 * v[i]);
            let at = |x: f32| (((x - self.lo[i]) / self.cell[i]).floor().max(0.0) as usize).min(self.cells[i] - 1);
            range[i] = if t1.is_finite() { at(a.min(b) - grow)..=at(a.max(b) + grow) } else { 0..=self.cells[i] - 1 };
        }
        let cell = |x: usize, y: usize| {
            let k = y * self.cells[0] + x;
            &self.members[self.starts[k] as usize..self.starts[k + 1] as usize]
        };
        let ([x0, x1], [y0, y1]) = ([*range[0].start(), *range[0].end()], [*range[1].start(), *range[1].end()]);
        if x0 == x1 && y0 == y1 {
            for &j in cell(x0, y0) {
                each(&self.housings[j as usize]);
            }
            return;
        }
        let mut near = HOUSINGS_NEAR.with(|n| std::mem::take(&mut *n.borrow_mut()));
        near.clear();
        for y in y0..=y1 {
            for x in x0..=x1 {
                near.extend_from_slice(cell(x, y));
            }
        }
        near.sort_unstable();
        near.dedup();
        for &j in &near {
            each(&self.housings[j as usize]);
        }
        HOUSINGS_NEAR.with(|n| *n.borrow_mut() = near);
    }
}

/// Where the floor's mirror looks one light up (`Mirror`): the place in the
/// room its light stands on, which the mirrored ray meets.
#[derive(Clone, Copy, Debug)]
pub enum MirrorProxy {
    /// The plane `p . normal = offset` (metres): a light's face.
    Plane { normal: Vec3f, offset: f32 },
    /// The sphere round the camera this far off, metres: a stroke's pieces,
    /// or a light that stands nowhere of its own.
    Sphere { radius: f32 },
}

impl MirrorProxy {
    /// How far along the mirrored ray `r` from `o` it meets this place: a
    /// sphere's no further than `reach`, where the room stops it. None when
    /// the ray runs away from it (a floor point beyond that light).
    fn meet(&self, o: Vec3f, r: Vec3f, reach: f32) -> Option<f32> {
        match *self {
            MirrorProxy::Plane { normal, offset } => {
                let t = (offset - o.dot(normal)) / r.dot(normal);
                (t > 0.0 && t < f32::INFINITY).then_some(t)
            }
            MirrorProxy::Sphere { radius } => {
                // |o + r t| = radius: the near root from outside it, the far one from within.
                let b = o.dot(r);
                let c = o.dot(o) - radius * radius;
                let disc = b * b - c;
                if !(disc > 0.0) {
                    return None;
                }
                let (near, far) = (-b - disc.sqrt(), -b + disc.sqrt());
                let t = if near > 0.0 {
                    near
                } else if far > 0.0 {
                    far
                } else {
                    return None;
                };
                Some(t.min(reach))
            }
        }
    }
}

/// The half-angle, radians, of a floor of `roughness`'s lobe where the
/// mirrored ray lands `t` along from the floor, `len` from the camera, as
/// the camera sees it: about roughness squared, spread over the path.
fn mirror_lobe(roughness: f32, t: f32, len: f32) -> f32 {
    (1.4 * roughness * roughness * t / len.max(1.0e-4)).min(1.2)
}

/// The floor's lobe round a mirrored ray as its mirror of the outside and of
/// the open walls takes it (`Room::outside_mirrored`): half-angles `a`
/// across (horizontal) and `b` up, about 1.4 roughness^2 radians, the
/// upward one drawn out toward the camera the flatter the floor is seen,
/// neither narrower than half the map's texel.
struct Lobe {
    r: Vec3f,
    a: f32,
    b: f32,
    across: Vec3f,
    up: Vec3f,
}

/// Where a lobe meets an open wall's plane (`Lobe::on`): the middle of its
/// footprint and the footprint's half sizes, in the opening's own axes,
/// metres, and the opening's half sizes.
struct OnMouth {
    half: Vec2f,
    u: f32,
    v: f32,
    du: f32,
    dv: f32,
}

impl Lobe {
    fn new(r: Vec3f, roughness: f32, texel: f32) -> Lobe {
        let spread = (1.4 * roughness * roughness).min(1.2);
        let streak = 1.0 / r.y.max(0.15).sqrt();
        let across = vec3f(-r.z, 0.0, r.x);
        let across = if across.length() > 1.0e-6 { across.normalize() } else { vec3f(1.0, 0.0, 0.0) };
        Lobe { r, a: spread.max(0.5 * texel), b: (spread * streak).min(1.2).max(0.5 * texel), across, up: Vec3f::cross(r, across).normalize() }
    }

    /// The lobe from `o` where it meets `mouth`'s plane, its footprint to
    /// first order in its angles; None when its ray does not run out through
    /// that plane.
    fn on(&self, mouth: &Opening, o: Vec3f) -> Option<OnMouth> {
        let r = self.r;
        let along = r.dot(mouth.normal);
        if !(along < -1.0e-3) {
            return None;
        }
        let t = (mouth.center - o).dot(mouth.normal) / along;
        if !(t > 0.0) {
            return None;
        }
        let p = o + r * t - mouth.center;
        // How far the point on the plane moves as the ray turns toward `w`, per radian.
        let slide = |w: Vec3f| (w - r * (w.dot(mouth.normal) / along)) * t;
        let (sa, sb) = (slide(self.across) * self.a, slide(self.up) * self.b);
        Some(OnMouth {
            half: mouth.half,
            u: p.dot(mouth.u),
            v: p.dot(mouth.v),
            du: sa.dot(mouth.u).abs() + sb.dot(mouth.u).abs(),
            dv: sa.dot(mouth.v).abs() + sb.dot(mouth.v).abs(),
        })
    }
}

impl OnMouth {
    /// The share of the footprint inside the opening: its rectangle
    /// box-filtered over the footprint. The room is a box, so a ray that
    /// meets an opening's plane inside its rectangle leaves there.
    fn share(&self) -> f32 {
        stripe(self.u, self.half.x, 2.0 * self.du) * stripe(self.v, self.half.y, 2.0 * self.dv)
    }
}

/// One light of the mirror: its entry in the list the studio hands over
/// (`Room::with_mirror`'s `light`), its place, and the layer its wide lobes
/// read (None when no lobe on the floor is that wide); `first` is set on
/// the first light of its layer, which reads the layer for all of them.
struct MirrorLight {
    entry: usize,
    proxy: MirrorProxy,
    layer: Option<usize>,
    first: bool,
}

/// An equirect of one light alone, or of a few standing near one another
/// (`mirror_groups`), summed for blurring, `texel` radians a texel, looked
/// up at `proxy` (their places' mean).
struct MirrorLayer {
    proxy: MirrorProxy,
    table: SummedArea<3>,
    texel: f32,
}

/// The floor's mirror of the lights: each light, in the studio's order, and
/// the layers their wide lobes read (one per light up to MIRROR_LAYERS_MAX).
pub struct Mirror {
    lights: Vec<MirrorLight>,
    layers: Vec<MirrorLayer>,
}

impl std::fmt::Debug for Mirror {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Mirror").field("lights", &self.lights.len()).field("layers", &self.layers.len()).finish()
    }
}

/// What a view ray meets in the room.
#[derive(Clone, Copy, Debug)]
pub struct Hit {
    pub t: f32,
    pub pos: Vec3f,
    /// Into the room.
    pub normal: Vec3f,
    /// The flat surface it lies on (FLOOR .. SOUTH); None on a cove.
    plane: Option<usize>,
    /// It is an open wall: the ray leaves the room there.
    pub open: bool,
    /// What of a real window it is (`WindowPart`): none off them.
    pub window: WindowPart,
    /// A styled window's own paint there (`room_window::Styled`): its
    /// albedo, in place of the room's surfaces'.
    paint: Option<Vec3f>,
}

/// What of a real window a ray meets (`Hit::window`): nothing of one, its
/// reveal (the wall's depth round it: the jambs, the head and the sill), its
/// frame or a bar (their faces and their sides), or a pane's glass, with how
/// much glass it holds (`RoomWindow::glass`: 0 an open hole).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum WindowPart {
    #[default]
    None,
    Reveal,
    Bar,
    Glass(f32),
    /// A styled window's frame, a sash or its bars (`room_window`).
    Frame,
    /// A styled window's surround on the wall's room face, a door's handles.
    Surround,
    /// A door's leaf.
    Door,
}

/// One open wall or real window (`Room::openings`): its middle, the way into
/// the room, its own axes along the wall (horizontal `u`, up `v`) and its half
/// sizes along them, metres, and the share of it that is open (`glass`: 1 for
/// an open wall, a real window's panes without its frame and bars).
#[derive(Clone, Copy, Debug)]
pub struct Opening {
    pub center: Vec3f,
    pub normal: Vec3f,
    pub u: Vec3f,
    pub v: Vec3f,
    pub half: Vec2f,
    pub glass: f32,
}

/// The ground under a room whose floor side is open (`RoomParams::open_floor`)
/// at a floor point: its albedo, how much it mirrors and how blurred.
#[derive(Clone, Copy, Debug)]
pub struct GroundAt {
    pub albedo: Vec3f,
    pub gloss: f32,
    pub roughness: f32,
}

/// The scene's ground as a room standing on it reads it: a floor point (in the
/// room's frame, the camera at the origin) to the ground there, and the
/// ground's mean albedo (the bounce's). Shared with the outside it is read
/// from, so it reads the ground's near relief once that is handed in.
#[derive(Clone)]
pub struct Ground {
    pub at: std::sync::Arc<dyn Fn(Vec3f) -> GroundAt + Send + Sync>,
    pub mean: Vec3f,
}

impl std::fmt::Debug for Ground {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Ground").field("mean", &self.mean).finish()
    }
}

/// A real window (`RoomWindow`) as the room casts into it: the plane it lies
/// in (WEST .. SOUTH), its rectangle (`Opening`'s middle, axes and half
/// sizes), its frame's and bars' half width and its panes' size along `u` and `v`.
#[derive(Clone, Debug)]
struct RealWindow {
    plane: usize,
    center: Vec3f,
    u: Vec3f,
    v: Vec3f,
    half: Vec2f,
    bar: f32,
    pane: Vec2f,
    /// Out of the room, through the wall.
    out: Vec3f,
    /// The wall's thickness round it (its reveal), the frame's middle this far
    /// into it, and the frame's and bars' own depth.
    depth: f32,
    set: f32,
    bar_depth: f32,
    /// Its panes' glass (`RoomWindow::glass`).
    glass_amount: f32,
    /// A styled window or a door (`RoomWindow::is_plain` false): its own
    /// shape, which `trace` and `glass` then leave to it.
    styled: Option<Box<Styled>>,
}

impl RealWindow {
    /// What a ray from the room along `d` meets once it crosses the wall's
    /// inner face at `at` inside the window's rectangle: how much further it
    /// runs, the surface's normal and the part. The wall's depth makes a
    /// reveal round the window (the ray leaves the rectangle sideways before
    /// the frame: a jamb, the head or the sill); the frame and the bars stand
    /// `bar_depth` deep round `set` (their faces, and their sides where the
    /// ray crosses into one between its faces); past them the ray meets the
    /// pane's glass at `set`. None off the rectangle or for a ray going back.
    fn trace(&self, at: Vec3f, d: Vec3f) -> Option<(f32, Vec3f, WindowPart)> {
        let q = at - self.center;
        let (u0, v0) = (q.dot(self.u), q.dot(self.v));
        if !(u0.abs() <= self.half.x && v0.abs() <= self.half.y) {
            return None;
        }
        let dz = d.dot(self.out);
        if !(dz > 1.0e-6) {
            return None;
        }
        let (du, dv) = (d.dot(self.u), d.dot(self.v));
        let side = |c: f32, dc: f32, half: f32| {
            if dc > 1.0e-9 {
                (half - c) / dc
            } else if dc < -1.0e-9 {
                (-half - c) / dc
            } else {
                f32::INFINITY
            }
        };
        let (su, sv) = (side(u0, du, self.half.x), side(v0, dv, self.half.y));
        let (s_side, side_normal) = if su < sv { (su, self.u * -du.signum()) } else { (sv, self.v * -dv.signum()) };
        let front = (self.set - 0.5 * self.bar_depth).max(0.0) / dz;
        let back = (self.set + 0.5 * self.bar_depth).min(self.depth) / dz;
        if s_side < front {
            return Some((s_side.max(0.0), side_normal, WindowPart::Reveal));
        }
        if !self.opens(at + d * front) {
            return Some((front, -self.out, WindowPart::Bar));
        }
        if s_side < back {
            // the frame's inner edge, round the rectangle
            return Some((s_side, side_normal, WindowPart::Bar));
        }
        if !self.opens(at + d * back) {
            // a bar's side, the one the ray runs into between the faces
            let normal = if du.abs() * self.pane.y > dv.abs() * self.pane.x { self.u * -du.signum() } else { self.v * -dv.signum() };
            return Some((0.5 * (front + back), normal, WindowPart::Bar));
        }
        Some((self.set / dz, -self.out, WindowPart::Glass(self.glass_amount)))
    }

    /// The share of the light outside that comes in through its panes' glass (a
    /// clear pane's mean Fresnel loss, about a tenth, and a little absorption).
    fn transmission(&self) -> f32 {
        1.0 - 0.12 * self.glass_amount
    }

    /// A point in the window's own frame (`room_window`): along the wall, up, out through it.
    fn local(&self, p: Vec3f) -> Vec3f {
        let q = p - self.center;
        vec3f(q.dot(self.u), q.dot(self.v), q.dot(self.out))
    }

    fn local_dir(&self, d: Vec3f) -> Vec3f {
        vec3f(d.dot(self.u), d.dot(self.v), d.dot(self.out))
    }

    /// A direction of the window's own frame in the room's.
    fn world(&self, n: Vec3f) -> Vec3f {
        self.u * n.x + self.v * n.y + self.out * n.z
    }

    /// The share of the outside's light it lets through: a plain window's
    /// panes' share and its glass's transmission, a styled one's own.
    fn passes(&self) -> f32 {
        match &self.styled {
            Some(styled) => styled.share,
            None => self.glass() * self.transmission(),
        }
    }

    /// The share of its rectangle its panes leave open (its frame and bars closed).
    fn glass(&self) -> f32 {
        let (w, h) = (2.0 * self.half.x, 2.0 * self.half.y);
        let (open_w, open_h) = ((w - 2.0 * self.bar * (w / self.pane.x)).max(0.0), (h - 2.0 * self.bar * (h / self.pane.y)).max(0.0));
        (open_w * open_h / (w * h).max(1.0e-6)).clamp(0.0, 1.0)
    }

    /// The point `p` on its plane lies in one of its panes (off the frame and the bars).
    fn opens(&self, p: Vec3f) -> bool {
        let q = p - self.center;
        let (u, v) = (q.dot(self.u), q.dot(self.v));
        if !(u.abs() < self.half.x && v.abs() < self.half.y) {
            return false;
        }
        // where it lies in its pane, from the pane's lower left corner: a bar (half of one on each side of a line,
        // the frame's on the outer edges) takes `bar` from every edge
        let (iu, iv) = ((u + self.half.x).rem_euclid(self.pane.x), (v + self.half.y).rem_euclid(self.pane.y));
        iu > self.bar && iu < self.pane.x - self.bar && iv > self.bar && iv < self.pane.y - self.bar
    }
}

/// A bright disc outside (the sun, the moon) shining in through the open
/// walls: its direction (toward it), its angular radius and its irradiance
/// on a surface facing it.
#[derive(Clone, Copy, Debug)]
pub struct Daylight {
    pub dir: Vec3f,
    pub radius: f32,
    pub irradiance: Vec3f,
}

/// A grid of the smooth lights' irradiance over one of the room's planes
/// (`Room::with_grids`): `nu` x `nv` nodes `step` metres apart from (`u0`,
/// `v0`) in the plane's own coordinates (`plane_uv`). A light's light stops
/// sharply where its own plane crosses this one (behind its face it gives
/// none), which no grid follows: each cell that line crosses lists that
/// light (the cell's run of `kinked` from `kink_start[cell]`), and a lookup
/// there takes it out of the blend and adds it exactly. The lights too close
/// to the plane for GRID_NODES_MAX nodes are not in it (`apart`): a lookup
/// adds them exactly.
#[derive(Debug)]
struct PlaneGrid {
    u0: f32,
    v0: f32,
    step: f32,
    nu: usize,
    nv: usize,
    values: Vec<Vec3f>,
    /// The smooth lights (indices into `Room::lights`) summed per lookup on this plane.
    apart: Vec<u32>,
    /// Per cell ((nu - 1) x (nv - 1), row by row), where its lights start in
    /// `kinked`, and one past the last cell; empty when no cut crosses a cell.
    kink_start: Vec<u32>,
    /// The lights (indices into `Room::lights`) whose cut crosses each cell.
    kinked: Vec<u32>,
}

#[derive(Debug)]
pub struct Room {
    min: Vec3f,
    max: Vec3f,
    cove: f32,
    walls: CoveWalls,
    /// Which planes stand open (indexed as FLOOR .. SOUTH; only walls do).
    open: [bool; 6],
    /// The real windows in the closed walls (`RoomParams::windows`).
    real: Vec<RealWindow>,
    /// Some of them are styled (`RealWindow::styled`): their reveals take
    /// their daylight from their glass, and their surrounds stand in the room.
    styled: bool,
    /// The floor side stands open: the floor is the scene's ground (`ground`)
    /// in place of `floor`, where the ground has been handed in (`with_ground`).
    open_floor: bool,
    /// The scene's ground outside a hybrid room (`with_ground`): under an open
    /// floor the floor itself, and past the openings what the room's lights
    /// fall on out there (`lamps_outside`).
    ground: Option<Ground>,
    floor: Vec3f,
    wall: Vec3f,
    ceiling: Vec3f,
    /// What the floor, the walls and the ceiling are made of (`room_material`); None the flat colour.
    floor_material: Option<RoomMaterial>,
    wall_material: Option<RoomMaterial>,
    ceiling_material: Option<RoomMaterial>,
    /// How worn they are (`RoomParams::wear`).
    wear: f32,
    gloss: f32,
    roughness: f32,
    bounce: f32,
    lights: Vec<RoomLight>,
    /// The lights whose beam nothing shapes (summed into the grids) and the others, as indices into `lights`.
    smooth: Vec<usize>,
    shaped: Vec<usize>,
    /// The discs outside shining in through the open walls.
    daylight: Vec<Daylight>,
    /// The open walls lighting the room, each with the outside's radiance through it.
    windows: Vec<(Opening, Vec3f)>,
    /// The even irradiance the room bounces round itself.
    ambient: Vec3f,
    rods: Vec<Rod>,
    /// The light fittings' bodies, one group per light (`HousingGroup`).
    housings: Vec<HousingGroup>,
    /// The ceiling grid's spacing, metres; None without one.
    grid: Option<f32>,
    /// Each plane's grid of the smooth lights' irradiance (`with_grids`); None sums them per lookup.
    grids: [Option<PlaneGrid>; 6],
    mirror: Option<Mirror>,
    /// The open walls (`openings`), for the floor's mirror of them.
    mouths: Vec<Opening>,
    /// The outside through them, OUTSIDE_W wide and padded, summed for
    /// blurring (`with_mirror`); None without an open wall or a glossy floor.
    outside: Option<SummedArea<3>>,
    /// The texel, radians, the floor's mirror was built for (`with_mirror`):
    /// the narrowest footprint a lookup filters its image of an open wall, of
    /// a disc out there and of a light drawn directly over. A lookup for a
    /// coarser map filters over that map's texel instead (`radiance`). A 2048
    /// wide map's until a mirror is put in.
    texel: f32,
}

fn rgb(c: [f32; 3]) -> Vec3f {
    vec3f(c[0], c[1], c[2])
}

fn mul(a: Vec3f, b: Vec3f) -> Vec3f {
    vec3f(a.x * b.x, a.y * b.y, a.z * b.z)
}

use makepad_draw::smoothstep as smooth;

/// The share of the window `x - w/2 .. x + w/2` that lies inside `-half ..
/// half`: a stripe `2 half` wide box-filtered over a footprint `w` wide.
fn stripe(x: f32, half: f32, w: f32) -> f32 {
    if !(w > 1.0e-6) {
        return if x.abs() <= half { 1.0 } else { 0.0 };
    }
    let lo = (x - 0.5 * w).max(-half);
    let hi = (x + 0.5 * w).min(half);
    ((hi - lo) / w).max(0.0).min(1.0)
}

impl Room {
    /// The room of `p` (as clamp() holds it), without its lights yet
    /// (`with_lights`): its shape, its surfaces, its grid, its road and its open walls.
    pub fn new(p: &RoomParams) -> Room {
        let p = p.clamped();
        let (w, d) = (p.width_m, p.depth_m);
        let (ox, oz) = (p.offset_x * w, p.offset_z * d);
        // The camera moved toward the east (+x) and the north (-z).
        let min = vec3f(-0.5 * w - ox, -p.camera_height_m, -0.5 * d + oz);
        let max = vec3f(0.5 * w - ox, p.height_m - p.camera_height_m, 0.5 * d + oz);
        let walls = p.cove();
        // A cove never rounds more than half the room's smaller side or its height.
        let cove = if walls == CoveWalls::None { 0.0 } else { p.cove_m.min(0.45 * w.min(d)).min(0.9 * p.height_m) };
        let [north, south, east, west] = p.open();
        let mut open = [false; 6];
        (open[NORTH], open[SOUTH], open[EAST], open[WEST]) = (north, south, east, west);
        let real: Vec<RealWindow> = p.real_windows().iter().filter_map(|window| real_window(window, min, max, &open, rgb(p.wall), rgb(p.floor))).collect();
        let styled = real.iter().any(|w| w.styled.is_some());
        let mut room = Room {
            min,
            max,
            cove,
            walls,
            open,
            real,
            styled,
            open_floor: p.open_floor(),
            ground: None,
            floor: rgb(p.floor),
            wall: rgb(p.wall),
            ceiling: rgb(p.ceiling),
            floor_material: p.floor_material.as_deref().and_then(RoomMaterial::from_name),
            wall_material: p.wall_material.as_deref().and_then(RoomMaterial::from_name),
            ceiling_material: p.ceiling_material.as_deref().and_then(RoomMaterial::from_name),
            wear: p.wear.unwrap_or(ROOM_WEAR_DEFAULT),
            gloss: p.floor_gloss,
            roughness: p.floor_roughness,
            bounce: p.bounce,
            lights: Vec::new(),
            smooth: Vec::new(),
            shaped: Vec::new(),
            daylight: Vec::new(),
            windows: Vec::new(),
            ambient: Vec3f::default(),
            rods: Vec::new(),
            housings: Vec::new(),
            grid: if p.ceiling_grid { Some(p.grid_spacing_m) } else { None },
            grids: Default::default(),
            mirror: None,
            mouths: Vec::new(),
            outside: None,
            texel: LINE_FOOT,
        };
        room.mouths = room.openings();
        room
    }

    /// How far a ray from the camera along `d` (unit) runs to the room's surface.
    pub fn reach(&self, d: Vec3f) -> f32 {
        self.cast(Vec3f::default(), d).map_or(1.0, |hit| hit.t)
    }

    /// The open walls, each a rectangle the width of the wall from the top of
    /// its floor cove (the floor, where none curves up into it) to the ceiling.
    pub fn openings(&self) -> Vec<Opening> {
        let (lo, hi) = (self.min, self.max);
        let mid = (lo + hi) * 0.5;
        let half = (hi - lo) * 0.5;
        let up = vec3f(0.0, 1.0, 0.0);
        (WEST..=SOUTH)
            .filter(|&k| self.open[k])
            .map(|k| {
                let coved = self.walls == CoveWalls::All || (self.walls == CoveWalls::Back && k == NORTH);
                let bottom = lo.y + if coved { self.cove } else { 0.0 };
                let (y, half_y) = (0.5 * (bottom + hi.y), 0.5 * (hi.y - bottom));
                let (normal, center, u, half_u) = match k {
                    WEST => (vec3f(1.0, 0.0, 0.0), vec3f(lo.x, y, mid.z), vec3f(0.0, 0.0, 1.0), half.z),
                    EAST => (vec3f(-1.0, 0.0, 0.0), vec3f(hi.x, y, mid.z), vec3f(0.0, 0.0, 1.0), half.z),
                    NORTH => (vec3f(0.0, 0.0, 1.0), vec3f(mid.x, y, lo.z), vec3f(1.0, 0.0, 0.0), half.x),
                    _ => (vec3f(0.0, 0.0, -1.0), vec3f(mid.x, y, hi.z), vec3f(1.0, 0.0, 0.0), half.x),
                };
                Opening { center, normal, u, v: up, half: vec2f(half_u, half_y), glass: 1.0 }
            })
            .chain(self.real.iter().map(|w| Opening { center: w.center, normal: plane_normal(w.plane), u: w.u, v: w.v, half: w.half, glass: w.passes() }))
            .collect()
    }

    /// The scene's ground round a hybrid room: past its openings the room's
    /// lights light it (`lamps_outside`), and with the floor side open the
    /// floor reads its albedo, gloss and roughness from it. Before the lights
    /// (`with_lights`), whose bounce takes the ground's mean albedo.
    pub fn with_ground(mut self, ground: Ground) -> Room {
        self.ground = Some(ground);
        self
    }

    /// The ground the floor is, where the room stands on it.
    fn floor_ground(&self) -> Option<&Ground> {
        if self.open_floor { self.ground.as_ref() } else { None }
    }

    /// The room's floor is the scene's ground (`with_ground`).
    pub fn on_ground(&self) -> bool {
        self.open_floor
    }

    /// A real window where a window light (a fake window) is seen: on the
    /// closed wall the camera sees along `d` (unit, the light's middle),
    /// `width_rad` x `height_rad` across as the camera sees it there, with
    /// `panes` and a `frame_m` frame. None when `d` meets the floor, the
    /// ceiling or an open wall.
    pub fn window_seen(&self, d: Vec3f, width_rad: f32, height_rad: f32, panes: (u32, u32), frame_m: f32) -> Option<RoomWindow> {
        let hit = self.cast(Vec3f::default(), d)?;
        let plane = hit.plane?;
        if !(WEST..=SOUTH).contains(&plane) || self.open[plane] {
            return None;
        }
        let mid = (self.min + self.max) * 0.5;
        let p = hit.pos;
        // `RoomWindow::along_m`: toward the east on the north and south walls, toward the north (-z) on the others
        let (wall, along) = match plane {
            NORTH => ("north", p.x - mid.x),
            SOUTH => ("south", p.x - mid.x),
            EAST => ("east", mid.z - p.z),
            _ => ("west", mid.z - p.z),
        };
        let (width, height) = (2.0 * hit.t * (0.5 * width_rad).tan(), 2.0 * hit.t * (0.5 * height_rad).tan());
        let window = RoomWindow {
            wall: wall.to_string(),
            along_m: along,
            sill_m: (p.y - self.min.y - 0.5 * height).max(0.0),
            width_m: width,
            height_m: height,
            panes_x: panes.0.max(1),
            panes_y: panes.1.max(1),
            frame_m,
            ..RoomWindow::default()
        };
        Some(window.clamped())
    }

    /// The outside's light through the open walls: `windows`, each opening
    /// (`openings`) with the radiance of the outside through it
    /// (`Opening::daylight`), and the discs out there (`Daylight`). Their
    /// light enters the bounce through the openings.
    pub fn with_daylight(mut self, windows: Vec<(Opening, Vec3f)>, daylight: Vec<Daylight>) -> Room {
        self.windows = windows.into_iter().filter(|(_, radiance)| luminance(*radiance) > 0.0).collect();
        self.daylight = daylight.into_iter().filter(|d| luminance(d.irradiance) > 0.0).collect();
        self
    }

    /// Puts the lights in: their places, beams and intensities, the bounce
    /// they and the daylight (`with_daylight`, first) give the room, and a
    /// stand or a drop pipe under or over each of `stands`. A light standing
    /// as pieces has one stand, so the stands are not in step with `lights`.
    pub fn with_lights(mut self, lights: Vec<RoomLight>, stands: &[Stand]) -> Room {
        // The bounce: each light puts out pi I0 (a Lambertian face), which the
        // room's mean albedo hands round its whole area until it is absorbed.
        // An open wall reflects nothing; a floor on the ground bounces the ground's mean.
        let size = self.max - self.min;
        let (floor_area, ceiling_area) = (size.x * size.z, size.x * size.z);
        let wall_areas = [size.y * size.z, size.y * size.z, size.y * size.x, size.y * size.x];
        let wall_area: f32 = wall_areas.iter().sum();
        let openings = self.openings();
        // (a styled window or a door is open only where its glass is; a plain one counts whole, as it always has)
        let open_walls = openings.len() - self.real.len();
        let pass: Vec<f32> = (0..openings.len()).map(|i| if i < open_walls { 1.0 } else { self.real[i - open_walls].styled.as_ref().map_or(1.0, |s| s.share) }).collect();
        let open_area: f32 = openings.iter().zip(&pass).map(|(o, pass)| 4.0 * o.half.x * o.half.y * pass).sum();
        let closed_wall = (wall_area - open_area).max(0.0);
        let area = floor_area + wall_area + ceiling_area;
        let floor_mean = self.floor_ground().map_or(self.floor, |g| g.mean);
        let mean = (floor_mean * floor_area + self.wall * closed_wall + self.ceiling * ceiling_area) / area;
        // a face puts out pi times its intensity along its normal, a flame 4 pi times its intensity
        let mut put_out = lights.iter().fold(Vec3f::default(), |sum, l| sum + l.intensity * (if l.omni { 4.0 } else { 1.0 } * std::f32::consts::PI));
        // An opening puts in pi times its radiance over its area, as a Lambertian face filling it does.
        for (opening, radiance) in &self.windows {
            put_out += *radiance * (std::f32::consts::PI * 4.0 * opening.half.x * opening.half.y);
        }
        // A disc outside puts in its irradiance over each opening it shines in through, as the opening faces it.
        for sun in &self.daylight {
            for (opening, pass) in openings.iter().zip(&pass) {
                put_out += sun.irradiance * (opening.half.x * opening.half.y * 4.0 * (-opening.normal.dot(sun.dir)).max(0.0) * pass);
            }
        }
        let keep = |a: f32| a.min(0.95) / (1.0 - a.min(0.95));
        self.ambient = mul(put_out, vec3f(keep(mean.x), keep(mean.y), keep(mean.z))) * (self.bounce / area);
        for &Stand { at: p, half_height, by_bottom } in stands {
            let bottom = p.y - half_height;
            let top = p.y + half_height;
            let high = bottom - self.min.y > HANG_SHARE * size.y;
            let hangs = high || (!by_bottom && top > self.max.y - HANG_GAP);
            if hangs {
                if top < self.max.y {
                    self.rods.push(Rod::new(vec3f(p.x, self.max.y, p.z), vec3f(p.x, top, p.z), PIPE_RADIUS));
                }
                continue;
            }
            if bottom <= self.min.y + 0.05 {
                continue;
            }
            let foot = vec3f(p.x, self.min.y, p.z);
            self.rods.push(Rod::new(foot, vec3f(p.x, bottom, p.z), POLE_RADIUS));
            // Three legs from the pole, turned to the light's own bearing.
            let knee = vec3f(p.x, self.min.y + LEG_TOP.min(0.6 * (bottom - self.min.y)), p.z);
            let bearing = p.x.atan2(p.z);
            for k in 0..3 {
                let a = bearing + (k as f32) * std::f32::consts::TAU / 3.0 + 0.5;
                self.rods.push(Rod::new(knee, foot + vec3f(a.sin(), 0.0, a.cos()) * LEG_SPREAD, LEG_RADIUS));
            }
        }
        (self.smooth, self.shaped) = (0..lights.len()).partition(|&i| !lights[i].shaped_beam);
        self.lights = lights;
        self
    }

    /// Puts the light fittings' bodies in (pattern.rs `housings`), one list
    /// per light, drawn over the room as the stands are, in that order.
    pub fn with_housings(mut self, housings: Vec<Vec<Housing>>) -> Room {
        self.housings = housings.into_iter().filter_map(HousingGroup::new).collect();
        self
    }

    /// Lays each plane's grid of the smooth lights' irradiance (rows spread
    /// by `run`). The same grids whatever the map's width, so every lookup
    /// (a bake at any size, the keyless copy, the key) reads the same room.
    /// A grid takes at most GRID_NODES_MAX nodes: the lights nearest the
    /// plane are left out of it (`PlaneGrid::apart`, summed per lookup there)
    /// until the rest fit. A plane whose rows the runner skipped keeps none
    /// and sums its lights per lookup. Cost per bake: each node the sum of
    /// the smooth lights that light the plane, at most GRID_NODES_MAX x those
    /// lights a plane; each cell a sign test per light whose cut crosses it.
    pub fn with_grids(mut self, run: &dyn Fn(usize, &(dyn Fn(usize) + Sync))) -> Room {
        // (plane, step, nodes across and up, the lights in its grid, those apart, the lights whose cut crosses it)
        let mut plans = Vec::new();
        for k in FLOOR..=SOUTH {
            if self.open[k] {
                continue;
            }
            let ((u0, u1), (v0, v1)) = self.plane_range(k);
            let normal = plane_normal(k);
            let corners = [(u0, v0), (u1, v0), (u0, v1), (u1, v1)].map(|(u, v)| self.plane_point(k, u, v));
            // The lights that light the plane at all (in front of it, and
            // facing some of it: its front half-space holds a corner), nearest
            // first, by how far each is from the part of the plane it lights:
            // the plane under it when it faces the plane, else where its own
            // plane crosses this one, further off the more the two lie alike.
            // The others give it nothing.
            let mut lighting: Vec<(f32, usize)> = self
                .smooth
                .iter()
                .copied()
                .filter(|&i| corners.iter().any(|c| self.lights[i].fronts(*c)))
                .filter_map(|i| {
                    let l = &self.lights[i];
                    let d = (l.pos - corners[0]).dot(normal);
                    let lit = if l.omni || l.normal.dot(normal) < 0.0 { d } else { d / Vec3f::cross(l.normal, normal).length().max(1.0e-6) };
                    (d > 0.0).then(|| (lit.max(l.near.sqrt()).max(GRID_NEAR_M), i))
                })
                .collect();
            lighting.sort_by(|a, b| a.0.total_cmp(&b.0));
            // The grid's spacing follows the nearest light in it; a light so
            // near that the grid would take more than GRID_NODES_MAX nodes
            // stands apart, and so on until the nearest left fits.
            let span = (u1 - u0).max(v1 - v0);
            let nodes = |step: f32| (((u1 - u0) / step).ceil() as usize + 1).saturating_mul(((v1 - v0) / step).ceil() as usize + 1);
            let first = lighting.iter().position(|&(near, _)| nodes(GRID_SHARE * near) <= GRID_NODES_MAX).unwrap_or(lighting.len());
            if first > 0 {
                static SAID: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
                if !SAID.swap(true, std::sync::atomic::Ordering::Relaxed) {
                    log!(
                        "hdri room: {first} light(s) stand within {:.3} m of plane {k}, too near for a grid of {GRID_NODES_MAX} nodes; it sums them per texel",
                        lighting[first - 1].0
                    );
                }
            }
            let step = lighting.get(first).map_or(span, |&(near, _)| GRID_SHARE * near);
            let nu = (((u1 - u0) / step).ceil() as usize + 1).max(2);
            let nv = (((v1 - v0) / step).ceil() as usize + 1).max(2);
            let step = ((u1 - u0) / (nu - 1) as f32).max((v1 - v0) / (nv - 1) as f32).max(1.0e-6);
            let apart: Vec<u32> = lighting[..first].iter().map(|&(_, i)| i as u32).collect();
            // In the studio's order, as the sum per lookup takes them.
            let mut inside: Vec<usize> = lighting[first..].iter().map(|&(_, i)| i).collect();
            inside.sort_unstable();
            // The lights whose cut crosses the plane: it has corners on both sides of their face's plane.
            let crossing: Vec<u32> = inside
                .iter()
                .copied()
                .filter(|&i| {
                    let l = &self.lights[i];
                    let front = corners.iter().filter(|c| l.fronts(**c)).count();
                    front < 4
                })
                .map(|i| i as u32)
                .collect();
            plans.push((k, step, nu, nv, inside, apart, crossing));
        }
        for (k, step, nu, nv, inside, apart, crossing) in plans {
            let ((u0, _), (v0, _)) = self.plane_range(k);
            let normal = plane_normal(k);
            let node = |i: usize, j: usize| self.plane_point(k, u0 + i as f32 * step, v0 + j as f32 * step);
            // Row j: its nodes, and the cells above it with the lights whose cut crosses each (their corners on both sides).
            let rows: Vec<std::sync::Mutex<(Vec<Vec3f>, Vec<Vec<u32>>)>> = (0..nv).map(|_| std::sync::Mutex::new((Vec::new(), Vec::new()))).collect();
            run(nv, &|j| {
                let values: Vec<Vec3f> = (0..nu)
                    .map(|i| {
                        let x = node(i, j);
                        inside.iter().fold(Vec3f::default(), |e, &l| e + light_on(&self.lights[l], x, normal))
                    })
                    .collect();
                let mut cells: Vec<Vec<u32>> = Vec::new();
                if j + 1 < nv && !crossing.is_empty() {
                    cells = vec![Vec::new(); nu - 1];
                    for &l in &crossing {
                        let light = &self.lights[l as usize];
                        let side = |i: usize, j: usize| light.normal.dot(node(i, j) - light.pos);
                        // The side is linear along the row: only the cells near where it is 0 can hold the cut.
                        let (at, along) = (side(0, j), side(1, j) - side(0, j));
                        let up = side(0, j + 1) - at;
                        let (first, last) = if along.abs() > 1.0e-12 {
                            let (a, b) = ((-at) / along, (-at - up) / along);
                            ((a.min(b).floor() - 2.0).max(0.0), (a.max(b).ceil() + 2.0).min((nu - 2) as f32))
                        } else {
                            (0.0, (nu - 2) as f32)
                        };
                        if !(first <= last) {
                            continue;
                        }
                        for i in first as usize..=last as usize {
                            let sides = [side(i, j) > 0.0, side(i + 1, j) > 0.0, side(i, j + 1) > 0.0, side(i + 1, j + 1) > 0.0];
                            if sides.contains(&true) && sides.contains(&false) {
                                cells[i].push(l);
                            }
                        }
                    }
                }
                *rows[j].lock().unwrap() = (values, cells);
            });
            let rows: Vec<(Vec<Vec3f>, Vec<Vec<u32>>)> = rows.into_iter().map(|row| row.into_inner().unwrap()).collect();
            if rows.iter().any(|row| row.0.len() != nu) {
                continue;
            }
            let values: Vec<Vec3f> = rows.iter().flat_map(|row| row.0.iter().copied()).collect();
            let (mut kink_start, mut kinked) = (Vec::new(), Vec::new());
            if rows.iter().any(|row| row.1.iter().any(|cell| !cell.is_empty())) {
                kink_start.reserve((nu - 1) * (nv - 1) + 1);
                for row in rows.iter().take(nv - 1) {
                    for cell in &row.1 {
                        kink_start.push(kinked.len() as u32);
                        kinked.extend(cell);
                    }
                }
                kink_start.push(kinked.len() as u32);
            }
            self.grids[k] = Some(PlaneGrid { u0, v0, step, nu, nv, values, apart, kink_start, kinked });
        }
        self
    }

    /// Lights the mirror for a map whose texel is `texel` radians (the
    /// narrowest footprint its lookups filter over, `radiance`):
    /// `light(k, d, foot)` is entry `k` of `proxies` alone over black along
    /// `d`, drawn for a footprint `foot` radians wide, and `proxies` are
    /// where each of those lights stands. Each layer's lobes on the floor
    /// (`mirror_lobes`) set its equirect's texel (half the narrowest, no finer
    /// than two of the map's texels, MIRROR_LAYER_MIN..=MIRROR_LAYER_MAX
    /// wide), and only a light some lobe is wider than a texel of gets one,
    /// its rows spread by `run`; past MIRROR_LAYERS_MAX such lights, those
    /// standing nearest one another share one (`mirror_groups`, summed, as
    /// fine as its finest member wants). A light whose equirect draws
    /// nothing is dropped. With an open wall, `sky(d)` (the outside, as
    /// `radiance` takes it) is tabled for a rough floor's mirror of it
    /// (`outside_mirrored`). Without gloss on the floor or its road there is
    /// no mirror.
    pub fn with_mirror(
        mut self,
        proxies: Vec<MirrorProxy>,
        texel: f32,
        light: &(dyn Fn(usize, Vec3f, f32) -> Vec3f + Sync),
        sky: &(dyn Fn(Vec3f) -> Vec3f + Sync),
        run: &dyn Fn(usize, &(dyn Fn(usize) + Sync)),
    ) -> Room {
        self.texel = texel;
        // (the ground always mirrors a little: its asphalt's and its grass's sheen, a lake's water)
        let glossy = self.gloss > 0.0 || self.floor_ground().is_some();
        if !glossy {
            return self;
        }
        if !self.mouths.is_empty() {
            // The outside's table, half a turn of padding each side.
            let (w, h, pad) = (OUTSIDE_W, OUTSIDE_W / 2, OUTSIDE_W / 2);
            let rows: Vec<std::sync::Mutex<Vec<Vec3f>>> = (0..h).map(|_| std::sync::Mutex::new(Vec::new())).collect();
            run(h, &|y| {
                let row = (0..w + 2 * pad)
                    .map(|x| {
                        let u = (x as f32 - pad as f32 + 0.5) / w as f32;
                        let d = equirect_uv_to_dir([u.rem_euclid(1.0), (y as f32 + 0.5) / h as f32]);
                        sky(self.off_discs(vec3f(d[0], d[1], d[2])))
                    })
                    .collect();
                *rows[y].lock().unwrap() = row;
            });
            let rows: Vec<Vec<Vec3f>> = rows.into_iter().map(|row| row.into_inner().unwrap()).collect();
            if rows.iter().any(|row| row.len() != w + 2 * pad) {
                // A runner that skipped rows (its job overtaken): no mirror.
                return self;
            }
            let data: Vec<[f32; 4]> = rows.iter().flatten().map(|c| [c.x, c.y, c.z, 1.0]).collect();
            self.outside = Some(SummedArea::build(&EnvMap { width: w + 2 * pad, height: h, data }, |t| [t[0] as f64, t[1] as f64, t[2] as f64]));
        }
        if proxies.is_empty() {
            return self;
        }
        let lobes = self.mirror_lobes(&proxies);
        let mut lights = Vec::new();
        let mut held = 0;
        // The lights a layer serves, each with the texel it wants and the width that gives.
        let mut want_layer = Vec::new();
        for (k, (proxy, (finest, widest))) in proxies.into_iter().zip(lobes).enumerate() {
            let want = (0.5 * finest).max(2.0 * texel);
            let w = ((std::f32::consts::TAU / want).ceil() as usize).clamp(MIRROR_LAYER_MIN, MIRROR_LAYER_MAX).next_multiple_of(2);
            if !(widest > std::f32::consts::TAU / w as f32) {
                lights.push(MirrorLight { entry: k, proxy, layer: None, first: false });
                continue;
            }
            if want < std::f32::consts::TAU / w as f32 {
                held += 1;
            }
            want_layer.push((k, proxy, w));
        }
        let n = want_layer.len();
        let (group, places) = mirror_groups(want_layer.iter().map(|&(_, proxy, _)| proxy).collect());
        if places.len() < n {
            log!("hdri room: {n} lights mirror in the floor through {} blurred layers (at most {MIRROR_LAYERS_MAX}), those standing nearest one another sharing one", places.len());
        }
        let mut layers = Vec::new();
        for (g, place) in places.into_iter().enumerate() {
            let members: Vec<usize> = (0..n).filter(|&i| group[i] == g).collect();
            let w = members.iter().map(|&i| want_layer[i].2).max().unwrap_or(MIRROR_LAYER_MIN);
            let (h, layer_texel) = (w / 2, std::f32::consts::TAU / w as f32);
            let entries: Vec<usize> = members.iter().map(|&i| want_layer[i].0).collect();
            let rows: Vec<std::sync::Mutex<Vec<Vec3f>>> = (0..h).map(|_| std::sync::Mutex::new(Vec::new())).collect();
            run(h, &|y| {
                let row = (0..w)
                    .map(|x| {
                        let d = equirect_uv_to_dir([(x as f32 + 0.5) / w as f32, (y as f32 + 0.5) / h as f32]);
                        let d = vec3f(d[0], d[1], d[2]);
                        entries.iter().fold(Vec3f::default(), |sum, &k| sum + light(k, d, layer_texel))
                    })
                    .collect();
                *rows[y].lock().unwrap() = row;
            });
            let rows: Vec<Vec<Vec3f>> = rows.into_iter().map(|row| row.into_inner().unwrap()).collect();
            if rows.iter().any(|row| row.len() != w) {
                // A runner that skipped rows (its job overtaken): no mirror.
                return self;
            }
            let data: Vec<[f32; 4]> = rows.iter().flatten().map(|c| [c.x, c.y, c.z, 1.0]).collect();
            if !data.iter().any(|t| t[0] > 0.0 || t[1] > 0.0 || t[2] > 0.0) {
                continue;
            }
            let table = SummedArea::build(&EnvMap { width: w, height: h, data }, |t| [t[0] as f64, t[1] as f64, t[2] as f64]);
            let layer = layers.len();
            lights.extend(members.iter().map(|&i| MirrorLight { entry: want_layer[i].0, proxy: want_layer[i].1, layer: Some(layer), first: false }));
            layers.push(MirrorLayer { proxy: place, table, texel: layer_texel });
        }
        if held > 0 {
            // A bound that degrades says so: those lobes are drawn directly, a little sharp.
            log!(
                "hdri room: the floor mirrors {held} light(s) through layers held to {MIRROR_LAYER_MAX} wide; lobes under {:.2} degrees draw them directly",
                (2.0 * std::f32::consts::TAU / MIRROR_LAYER_MAX as f32).to_degrees()
            );
        }
        if !lights.is_empty() {
            lights.sort_by_key(|l| l.entry);
            let mut read = vec![false; layers.len()];
            for l in &mut lights {
                if let Some(g) = l.layer {
                    l.first = !read[g];
                    read[g] = true;
                }
            }
            self.mirror = Some(Mirror { lights, layers });
        }
        self
    }

    /// The narrowest and the widest lobe (`mirror_lobe`) the glossy floor
    /// has where each of `proxies` is mirrored, over MIRROR_PROBE_W x
    /// MIRROR_PROBE_H view directions down to the floor; (inf, 0) for one
    /// no floor point mirrors. A lobe between the probes reads a layer a
    /// little coarser or sharper than it would want, never a missing one:
    /// a lobe narrower than the layer's texel draws directly.
    fn mirror_lobes(&self, proxies: &[MirrorProxy]) -> Vec<(f32, f32)> {
        let mut lobes = vec![(f32::INFINITY, 0.0f32); proxies.len()];
        for j in 0..MIRROR_PROBE_H {
            let elevation = -std::f32::consts::FRAC_PI_2 * (j as f32 + 0.5) / MIRROR_PROBE_H as f32;
            let (sin_el, cos_el) = elevation.sin_cos();
            for i in 0..MIRROR_PROBE_W {
                let (sin_az, cos_az) = (std::f32::consts::TAU * (i as f32 + 0.5) / MIRROR_PROBE_W as f32).sin_cos();
                let d = vec3f(cos_el * sin_az, sin_el, -cos_el * cos_az);
                let Some(hit) = self.cast(Vec3f::default(), d) else { continue };
                if hit.open || !(hit.normal.y > 0.999) {
                    continue;
                }
                let (gloss, roughness) = if hit.plane == Some(FLOOR) { self.gloss_at(hit.pos) } else { (self.gloss, self.roughness) };
                if !(gloss > 0.0) {
                    continue;
                }
                let r = vec3f(d.x, -d.y, d.z);
                let o = hit.pos + vec3f(0.0, 1.0e-4, 0.0);
                let reach = self.cast(o, r).map_or(f32::INFINITY, |back| back.t);
                for (proxy, lobe) in proxies.iter().zip(lobes.iter_mut()) {
                    let Some(t) = proxy.meet(o, r, reach) else { continue };
                    let s = mirror_lobe(roughness, t, (o + r * t).length());
                    *lobe = (lobe.0.min(s), lobe.1.max(s));
                }
            }
        }
        lobes
    }

    pub fn lights(&self) -> &[RoomLight] {
        &self.lights
    }

    /// Plane `k`'s extent in its own coordinates (`plane_uv`).
    fn plane_range(&self, k: usize) -> ((f32, f32), (f32, f32)) {
        let (lo, hi) = (self.min, self.max);
        match k {
            FLOOR | CEILING => ((lo.x, hi.x), (lo.z, hi.z)),
            WEST | EAST => ((lo.z, hi.z), (lo.y, hi.y)),
            _ => ((lo.x, hi.x), (lo.y, hi.y)),
        }
    }

    /// The point of plane `k` at its own coordinates (u, v).
    fn plane_point(&self, k: usize, u: f32, v: f32) -> Vec3f {
        match k {
            FLOOR => vec3f(u, self.min.y, v),
            CEILING => vec3f(u, self.max.y, v),
            WEST => vec3f(self.min.x, v, u),
            EAST => vec3f(self.max.x, v, u),
            NORTH => vec3f(u, v, self.min.z),
            _ => vec3f(u, v, self.max.z),
        }
    }

    /// The distance from inside the room to its surface (positive inside).
    fn sdf(&self, p: Vec3f) -> f32 {
        let ceiling = self.max.y - p.y;
        let up = p.y - self.min.y;
        let r = self.cove;
        let cove = |a: f32, b: f32| if a < r && b < r { r - ((r - a) * (r - a) + (r - b) * (r - b)).sqrt() } else { a.min(b) };
        let below = match self.walls {
            CoveWalls::None => (p.x - self.min.x).min(self.max.x - p.x).min(p.z - self.min.z).min(self.max.z - p.z).min(up),
            CoveWalls::Back => {
                let others = (p.x - self.min.x).min(self.max.x - p.x).min(self.max.z - p.z);
                cove(p.z - self.min.z, up).min(others)
            }
            CoveWalls::All => {
                // The walls are a rounded rectangle in plan, its corners as
                // round as the cove; a corner beside an open wall is square
                // (the closed wall just ends there), so a mouth has no jambs.
                let c = (self.min + self.max) * 0.5;
                let half = (self.max - self.min) * 0.5;
                let (east, south) = (p.x > c.x, p.z > c.z);
                let side = if east { self.open[EAST] } else { self.open[WEST] };
                let end = if south { self.open[SOUTH] } else { self.open[NORTH] };
                let round = if side || end { 0.0 } else { r };
                let qx = (p.x - c.x).abs() - (half.x - round);
                let qz = (p.z - c.z).abs() - (half.z - round);
                let outside = (qx.max(0.0) * qx.max(0.0) + qz.max(0.0) * qz.max(0.0)).sqrt() + qx.max(qz).min(0.0) - round;
                cove(-outside, up)
            }
        };
        below.min(ceiling)
    }

    /// Where a ray from `o` (inside the room) along `d` (unit) meets the room:
    /// its shell (`cast_shell`), or nearer, a styled window's surround.
    pub fn cast(&self, o: Vec3f, d: Vec3f) -> Option<Hit> {
        self.cast_seen(o, d, 0.0)
    }

    /// `cast` for a lookup whose footprint is `foot` radians across (a
    /// map's texel): a styled window's bars are at least as wide as that
    /// footprint where the ray meets them (`Styled::trace`).
    fn cast_seen(&self, o: Vec3f, d: Vec3f, foot: f32) -> Option<Hit> {
        let hit = self.cast_shell(o, d, foot);
        if !self.styled {
            return hit;
        }
        let mut best: Option<(f32, Vec3f, Vec3f, Paint)> = None;
        for w in &self.real {
            let Some(styled) = &w.styled else { continue };
            let reach = best.map_or(hit.map_or(f32::INFINITY, |h| h.t), |b| b.0);
            if let Some((t, n, paint)) = styled.parts(w.local(o), w.local_dir(d), reach) {
                best = Some((t, w.world(n), styled.albedo(paint, 1.0), paint));
            }
        }
        match best {
            Some((t, normal, albedo, paint)) => {
                let window = if paint == Paint::Door { WindowPart::Door } else { WindowPart::Surround };
                Some(Hit { t, pos: o + d * t, normal, plane: None, open: false, window, paint: Some(albedo) })
            }
            None => hit,
        }
    }

    /// Where a ray from `o` (inside the room) along `d` (unit) meets the
    /// room's walls, floor, ceiling and coves, and the real windows in them.
    fn cast_shell(&self, o: Vec3f, d: Vec3f, foot: f32) -> Option<Hit> {
        let exit = |lo: Vec3f, hi: Vec3f| -> (f32, Vec3f) {
            let mut t = f32::INFINITY;
            let mut n = Vec3f::default();
            let mut axis = |o: f32, d: f32, lo: f32, hi: f32, n_lo: Vec3f| {
                if d > 1.0e-9 {
                    let s = (hi - o) / d;
                    if s < t {
                        t = s;
                        n = -n_lo;
                    }
                } else if d < -1.0e-9 {
                    let s = (lo - o) / d;
                    if s < t {
                        t = s;
                        n = n_lo;
                    }
                }
            };
            axis(o.x, d.x, lo.x, hi.x, vec3f(1.0, 0.0, 0.0));
            axis(o.y, d.y, lo.y, hi.y, vec3f(0.0, 1.0, 0.0));
            axis(o.z, d.z, lo.z, hi.z, vec3f(0.0, 0.0, 1.0));
            (t, n)
        };
        let (t_box, n_box) = exit(self.min, self.max);
        if !t_box.is_finite() || !(t_box > 0.0) {
            return None;
        }
        let at = o + d * t_box;
        if self.cove <= 0.0 || self.sdf(at) > -1.0e-4 {
            let plane = plane_of(n_box);
            if !self.open[plane] && plane >= WEST {
                for w in self.real.iter().filter(|w| w.plane == plane) {
                    let Some(styled) = &w.styled else {
                        let Some((s, normal, part)) = w.trace(at, d) else { continue };
                        let (t, pos) = (t_box + s, at + d * s);
                        return Some(match part {
                            WindowPart::Glass(_) => Hit { t, pos, normal, plane: Some(plane), open: true, window: part, paint: None },
                            _ => Hit { t, pos, normal, plane: None, open: false, window: part, paint: None },
                        });
                    };
                    let q = at - w.center;
                    if !(q.dot(w.u).abs() <= w.half.x && q.dot(w.v).abs() <= w.half.y) {
                        continue;
                    }
                    let Some(m) = styled.trace(w.local(at), w.local_dir(d), 0.5 * foot * (o - at).length()) else { continue };
                    let (t, pos, normal) = (t_box + m.s, at + d * m.s, w.world(m.normal));
                    return Some(match m.met {
                        Met::Glass(glass) => Hit { t, pos, normal, plane: Some(plane), open: true, window: WindowPart::Glass(glass), paint: None },
                        Met::Open => Hit { t, pos, normal, plane: Some(plane), open: true, window: WindowPart::Glass(0.0), paint: None },
                        Met::Solid { paint, shade, reveal } => {
                            let window = match paint {
                                _ if reveal => WindowPart::Reveal,
                                Paint::Door => WindowPart::Door,
                                Paint::Surround | Paint::Metal => WindowPart::Surround,
                                _ => WindowPart::Frame,
                            };
                            Hit { t, pos, normal, plane: None, open: false, window, paint: Some(styled.albedo(paint, shade)) }
                        }
                    });
                }
            }
            return Some(Hit { t: t_box, pos: at, normal: n_box, plane: Some(plane), open: self.open[plane], window: WindowPart::None, paint: None });
        }
        // In a cove: the surface is between the box the cove rounds off and the planes.
        let r = self.cove;
        let inner_lo = vec3f(
            if self.walls == CoveWalls::All { self.min.x + r } else { self.min.x },
            self.min.y + r,
            self.min.z + r,
        );
        let inner_hi = vec3f(
            if self.walls == CoveWalls::All { self.max.x - r } else { self.max.x },
            self.max.y,
            if self.walls == CoveWalls::All { self.max.z - r } else { self.max.z },
        );
        let inside = o.x > inner_lo.x && o.x < inner_hi.x && o.y > inner_lo.y && o.z > inner_lo.z && o.z < inner_hi.z;
        let mut lo = if inside { exit(inner_lo, inner_hi).0.max(0.0) } else { 0.0 };
        let mut hi = t_box;
        for _ in 0..18 {
            let mid = 0.5 * (lo + hi);
            if self.sdf(o + d * mid) > 0.0 {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        let t = 0.5 * (lo + hi);
        let pos = o + d * t;
        // The cove's normal from its distance's gradient.
        let e = 1.0e-3 * r.max(0.1);
        let g = vec3f(
            self.sdf(pos + vec3f(e, 0.0, 0.0)) - self.sdf(pos - vec3f(e, 0.0, 0.0)),
            self.sdf(pos + vec3f(0.0, e, 0.0)) - self.sdf(pos - vec3f(0.0, e, 0.0)),
            self.sdf(pos + vec3f(0.0, 0.0, e)) - self.sdf(pos - vec3f(0.0, 0.0, e)),
        );
        let normal = if g.length() > 1.0e-12 { g.normalize() } else { n_box };
        Some(Hit { t, pos, normal, plane: None, open: false, window: WindowPart::None, paint: None })
    }

    /// The surface where `hit` lands, seen at a footprint `foot` metres
    /// across (`foot`): the floor's (the ground's on the flat floor of a room
    /// standing on it), turned up through a cove into the wall's, or the
    /// ceiling's, each its material's (`room_material`) or its flat colour.
    /// A window's reveal is the wall's material on its own faces.
    fn surface(&self, hit: &Hit, foot: f32) -> Surface {
        let flat = |albedo: Vec3f| Surface { albedo, gloss: 1.0, roughness: 0.0 };
        if let Some(paint) = hit.paint {
            return flat(paint);
        }
        // the frame and the bars are white paint, a little off the wall's colour
        match hit.window {
            WindowPart::Reveal => return self.side_surface(Side::Wall, hit.pos, hit.normal, foot),
            WindowPart::Bar => return flat(vec3f(0.78, 0.78, 0.76)),
            _ => {}
        }
        if hit.normal.y < -0.5 {
            return self.side_surface(Side::Ceiling, hit.pos, hit.normal, foot);
        }
        let k = smooth(0.25, 0.95, hit.normal.y);
        let floor = || match self.floor_ground().filter(|_| hit.plane == Some(FLOOR)) {
            Some(ground) => flat((ground.at)(hit.pos).albedo),
            None => self.side_surface(Side::Floor, hit.pos, hit.normal, foot),
        };
        if k >= 1.0 {
            return floor();
        }
        let wall = self.side_surface(Side::Wall, hit.pos, hit.normal, foot);
        if k <= 0.0 {
            return wall;
        }
        let floor = floor();
        Surface {
            albedo: wall.albedo + (floor.albedo - wall.albedo) * k,
            gloss: wall.gloss + (floor.gloss - wall.gloss) * k,
            roughness: wall.roughness + (floor.roughness - wall.roughness) * k,
        }
    }

    /// The floor, a wall or the ceiling at `pos` (a wall facing `normal`'s
    /// level part: a cove's, a reveal's own face) for a footprint `foot`:
    /// its material in its plane's own metres (a wall's up from the floor),
    /// worn by where it lies in the room, or its flat colour.
    fn side_surface(&self, side: Side, pos: Vec3f, normal: Vec3f, foot: f32) -> Surface {
        let (base, material) = match side {
            Side::Floor => (self.floor, self.floor_material),
            Side::Wall => (self.wall, self.wall_material),
            Side::Ceiling => (self.ceiling, self.ceiling_material),
        };
        let Some(material) = material else { return Surface { albedo: base, gloss: 1.0, roughness: 0.0 } };
        let (plane, (u, v)) = match side {
            Side::Floor => (FLOOR, (pos.x, pos.z)),
            Side::Ceiling => (CEILING, (pos.x, pos.z)),
            // a plain window's sill or head (level; a cove's normal never is this level on a wall): the wall it is cut
            // through (the one its point lies furthest beyond), u along that wall as on its face and v across its depth
            Side::Wall if normal.y.abs() > 0.99 => {
                let in_x = (self.min.x - pos.x).max(pos.x - self.max.x) > (self.min.z - pos.z).max(pos.z - self.max.z);
                let mid = (self.min + self.max) * 0.5;
                if in_x {
                    (if pos.x < mid.x { WEST } else { EAST }, (pos.z, pos.x))
                } else {
                    (if pos.z < mid.z { NORTH } else { SOUTH }, (pos.x, pos.z))
                }
            }
            Side::Wall => {
                let k = plane_of(vec3f(normal.x, 0.0, normal.z));
                let (u, _) = plane_uv(k, pos);
                (k, (u, pos.y - self.min.y))
            }
        };
        // the distance to the nearest closed wall (on the floor and the ceiling, which have no top: infinitely far), or
        // up from the floor and down from the ceiling (on a wall)
        let (seam_m, top_m) = match side {
            Side::Wall => (pos.y - self.min.y, self.max.y - pos.y),
            _ => {
                let gaps = [(WEST, pos.x - self.min.x), (EAST, self.max.x - pos.x), (NORTH, pos.z - self.min.z), (SOUTH, self.max.z - pos.z)];
                (gaps.iter().filter(|(k, _)| !self.open[*k]).map(|(_, g)| *g).fold(f32::INFINITY, f32::min).max(0.0), f32::INFINITY)
            }
        };
        // the open walls (mouths), each's distance and the point's place along its edge, and the nearest, for the leaks
        let mouths = [(WEST, pos.x - self.min.x, pos.z), (EAST, self.max.x - pos.x, pos.z), (NORTH, pos.z - self.min.z, pos.x), (SOUTH, self.max.z - pos.z, pos.x)]
            .map(|(k, g, along)| if self.open[k] { (g.max(0.0), along) } else { (f32::INFINITY, along) });
        let open_m = mouths.iter().map(|m| m.0).fold(f32::INFINITY, f32::min);
        let weather = Weather { side, seam_m, top_m, amount: self.wear, open_m, mouths };
        room_material::surface(material, base, u, v, foot, &weather, 0x9e37_79b9u32.wrapping_mul(plane as u32 + 1))
    }

    /// The footprint, metres, of a lookup `texel` radians wide (a 2048 wide
    /// map's for a point lookup) on `hit`, `reach` metres along the view `d`:
    /// stretched along the view as the surface turns away from it.
    fn foot(hit: &Hit, d: Vec3f, reach: f32, texel: f32) -> f32 {
        let texel = if texel > 0.0 { texel } else { LINE_FOOT };
        (texel * reach / hit.normal.dot(d).abs().max(0.05)).min(100.0)
    }

    /// The floor's gloss and roughness at floor point `pos`: the ground's where the room stands on it.
    fn gloss_at(&self, pos: Vec3f) -> (f32, f32) {
        match self.floor_ground() {
            Some(ground) => {
                let g = (ground.at)(pos);
                (g.gloss, g.roughness)
            }
            None => (self.gloss, self.roughness),
        }
    }

    /// The outside's light on a surface at `x` facing `n`: each opening's
    /// (`Opening::on`), and each disc's where a ray toward it leaves through an open wall.
    /// In a styled window's opening (its reveal, its frames, a door's leaf)
    /// the window's own opening lights it from its glass, deeper in the wall
    /// (so a frame's face toward the room takes none of it), and the sun
    /// reaches it only through that glass (`reaches_out`).
    fn daylight_on(&self, x: Vec3f, n: Vec3f) -> Vec3f {
        let recess = if self.styled { self.real.iter().find(|w| w.styled.as_ref().is_some_and(|s| s.in_recess(w.local(x)))) } else { None };
        let mut e = Vec3f::default();
        for (opening, radiance) in &self.windows {
            match recess.and_then(|w| w.styled.as_ref().map(|s| (w, s))) {
                Some((w, styled)) if (opening.center - w.center).length() < 1.0e-5 && opening.normal.dot(plane_normal(w.plane)) > 0.99 => {
                    let glass = Opening { center: w.center + w.out * styled.glass_w, ..*opening };
                    e += *radiance * glass.on(x, n);
                }
                _ => e += *radiance * opening.on(x, n),
            }
        }
        for sun in &self.daylight {
            let facing = n.dot(sun.dir);
            if !(facing > 0.0) {
                continue;
            }
            if self.reaches_out(x + n * 1.0e-3, sun.dir, recess) {
                e += sun.irradiance * facing;
            }
        }
        e
    }

    /// A ray from `x` along `dir` leaves the room through an opening: from a
    /// styled window's opening (`recess`), out through its glass, or back
    /// into the room past its reveal and on out through another.
    fn reaches_out(&self, x: Vec3f, dir: Vec3f, recess: Option<&RealWindow>) -> bool {
        let Some((w, styled)) = recess.and_then(|w| w.styled.as_ref().map(|s| (w, s))) else {
            return self.cast(x, dir).is_some_and(|hit| hit.open);
        };
        let (o, d) = (w.local(x), w.local_dir(dir));
        if !(o.z > 0.0) {
            // on the wall's room face (an arched head's corners, a door's lining): as anywhere in the room
            return self.cast(x, dir).is_some_and(|hit| hit.open);
        }
        if d.z > 1.0e-6 {
            return matches!(styled.trace(o, d, 0.0).map(|m| m.met), Some(Met::Glass(_) | Met::Open));
        }
        if !(d.z < -1.0e-6) {
            return false;
        }
        let at = x + dir * (o.z / -d.z);
        let face = w.local(at);
        styled.opens_at(vec2f(face.x, face.y)) && self.cast(at + dir * 1.0e-4, dir).is_some_and(|hit| hit.open)
    }

    /// The irradiance at `x` on a surface facing `n`: every light's, through
    /// its beam (`beam(entry, toward the light)`), the daylight's and the room's bounce.
    fn irradiance(&self, x: Vec3f, n: Vec3f, beam: &dyn Fn(usize, Vec3f) -> f32) -> Vec3f {
        let mut e = self.ambient;
        for l in &self.lights {
            e += lit_by(l, x, n, beam);
        }
        if !self.daylight.is_empty() || !self.windows.is_empty() {
            e += self.daylight_on(x, n);
        }
        e
    }

    /// The irradiance at `x` on a face of normal `n` (unit) from the room's
    /// bounce, its daylight and every light whose entry `apart` does not
    /// name, their beams unshaped: what lights a model standing in the room
    /// besides its own light (light_model_candle.rs).
    pub fn irradiance_apart(&self, x: Vec3f, n: Vec3f, apart: &dyn Fn(usize) -> bool) -> Vec3f {
        let mut e = self.ambient;
        for l in &self.lights {
            if !apart(l.entry) {
                e += light_on(l, x, n);
            }
        }
        if !self.daylight.is_empty() || !self.windows.is_empty() {
            e += self.daylight_on(x, n);
        }
        e
    }

    /// [`Self::irradiance`] on the surface at `hit`: on a plane with a grid
    /// the smooth lights' part is read from it.
    fn surface_irradiance(&self, hit: &Hit, beam: &dyn Fn(usize, Vec3f) -> f32) -> Vec3f {
        let Some((k, grid)) = hit.plane.and_then(|k| self.grids[k].as_ref().map(|grid| (k, grid))) else {
            return self.irradiance(hit.pos, hit.normal, beam);
        };
        let (u, v) = plane_uv(k, hit.pos);
        let mut e = self.ambient + self.grid_sample(k, grid, u, v, hit.pos);
        for &i in &self.shaped {
            e += lit_by(&self.lights[i], hit.pos, hit.normal, beam);
        }
        if !self.daylight.is_empty() || !self.windows.is_empty() {
            e += self.daylight_on(hit.pos, hit.normal);
        }
        e
    }

    /// Plane `k`'s `grid` at (u, v), the point `x`: the blend of its four
    /// nodes round it, in a cell a light's cut crosses that light exactly,
    /// and the lights apart from the grid exactly.
    fn grid_sample(&self, k: usize, grid: &PlaneGrid, u: f32, v: f32, x: Vec3f) -> Vec3f {
        let fu = ((u - grid.u0) / grid.step).max(0.0).min((grid.nu - 1) as f32);
        let fv = ((v - grid.v0) / grid.step).max(0.0).min((grid.nv - 1) as f32);
        let (i, j) = ((fu as usize).min(grid.nu - 2), (fv as usize).min(grid.nv - 2));
        let (tu, tv) = (fu - i as f32, fv - j as f32);
        let blend = |at: &dyn Fn(usize, usize) -> Vec3f| {
            let top = at(i, j) + (at(i + 1, j) - at(i, j)) * tu;
            let bottom = at(i, j + 1) + (at(i + 1, j + 1) - at(i, j + 1)) * tu;
            top + (bottom - top) * tv
        };
        let mut e = blend(&|i, j| grid.values[j * grid.nu + i]);
        if !grid.kink_start.is_empty() {
            let cell = j * (grid.nu - 1) + i;
            let normal = plane_normal(k);
            for &l in &grid.kinked[grid.kink_start[cell] as usize..grid.kink_start[cell + 1] as usize] {
                let light = &self.lights[l as usize];
                let node = |i: usize, j: usize| light_on(light, self.plane_point(k, grid.u0 + i as f32 * grid.step, grid.v0 + j as f32 * grid.step), normal);
                e = e - blend(&node) + light_on(light, x, normal);
            }
        }
        if !grid.apart.is_empty() {
            let normal = plane_normal(k);
            for &l in &grid.apart {
                e += light_on(&self.lights[l as usize], x, normal);
            }
        }
        e
    }

    /// What a diffuse surface at `hit` sends back, seen at a footprint `foot` metres across.
    fn diffuse(&self, hit: &Hit, foot: f32, beam: &dyn Fn(usize, Vec3f) -> f32) -> Vec3f {
        self.diffuse_of(hit, self.surface(hit, foot).albedo, beam)
    }

    /// What a diffuse surface at `hit` of `albedo` sends back.
    fn diffuse_of(&self, hit: &Hit, albedo: Vec3f, beam: &dyn Fn(usize, Vec3f) -> f32) -> Vec3f {
        mul(albedo, self.surface_irradiance(hit, beam)) * std::f32::consts::FRAC_1_PI
    }

    /// The room along `d` from the camera (unit, map frame), with stands and
    /// the grid over it. `sky(d)` is the environment outside, seen through an
    /// open wall (and in the floor's mirror of one); `light(k, d, foot)` is
    /// the mirror's light `k` alone along `d` for a footprint `foot` radians
    /// wide, as `with_mirror` was handed it. `texel` is the lookup's texel
    /// (radians, 0 for a point): the floor's mirror filters over it, or over
    /// the texel the mirror was built for where that is wider. So a preview
    /// narrower than the map the mirror was built for filters its pixels over
    /// their own texel, as a pattern's lookup does, from the same room.
    pub fn radiance(&self, d: Vec3f, texel: f32, beam: &dyn Fn(usize, Vec3f) -> f32, sky: &dyn Fn(Vec3f) -> Vec3f, light: &dyn Fn(usize, Vec3f, f32) -> Vec3f) -> Vec3f {
        let Some(hit) = self.cast_seen(Vec3f::default(), d, texel) else { return Vec3f::default() };
        if hit.open {
            let mut c = sky(d) + self.lamps_outside(d, beam);
            if let WindowPart::Glass(glass) = hit.window {
                c = self.through_glass(&hit, d, glass, c, texel, beam, sky, light);
            }
            return self.over_rigging(d, hit.t, c, beam);
        }
        let surface = self.surface(&hit, Self::foot(&hit, d, hit.t, texel));
        let mut c = self.diffuse_of(&hit, surface.albedo, beam);
        // A glossy floor mirrors the room, the outside through its open walls, and the lights.
        let (gloss, roughness) = if hit.plane == Some(FLOOR) { self.gloss_at(hit.pos) } else { (self.gloss, self.roughness) };
        // (at most 1: a glossier spot, oil on concrete, never mirrors more light than reaches it)
        let (gloss, roughness) = ((gloss * surface.gloss).min(1.0), (roughness + surface.roughness).min(1.0));
        if gloss > 0.0 && hit.normal.y > 0.999 {
            let cos = (-d.y).max(0.0);
            let fresnel = gloss * (0.04 + 0.96 * (1.0 - cos).powi(5));
            let r = vec3f(d.x, -d.y, d.z);
            let o = hit.pos + vec3f(0.0, 1.0e-4, 0.0);
            let texel = texel.max(self.texel);
            let mut seen = Vec3f::default();
            let reach = match self.cast(o, r) {
                Some(back) => {
                    seen = self.mirrored_room(o, r, &back, roughness, texel, beam, sky);
                    back.t
                }
                None => f32::INFINITY,
            };
            if let Some(mirror) = &self.mirror {
                seen += mirror.look(o, r, reach, roughness, texel, light, false);
            }
            c = c * (1.0 - fresnel) + seen * fresnel;
        }
        self.over_rigging(d, hit.t, c, beam)
    }

    /// A real window's pane seen along `d` at `hit`, `outside` beyond it: the
    /// glass (`glass` of it) lets the outside through less its Fresnel share
    /// and a little absorption, and mirrors the room at that share, 4 % seen
    /// straight on rising toward grazing: the room's surfaces where the
    /// mirrored ray meets them (the outside again through an opening) and the
    /// lights along it (`Mirror::look`, a polished surface's).
    #[allow(clippy::too_many_arguments)]
    fn through_glass(&self, hit: &Hit, d: Vec3f, glass: f32, outside: Vec3f, texel: f32, beam: &dyn Fn(usize, Vec3f) -> f32, sky: &dyn Fn(Vec3f) -> Vec3f, light: &dyn Fn(usize, Vec3f, f32) -> Vec3f) -> Vec3f {
        if !(glass > 0.0) {
            return outside;
        }
        let n = hit.normal;
        let cos = (-d.dot(n)).clamp(0.0, 1.0);
        let fresnel = glass * (0.04 + 0.96 * (1.0 - cos).powi(5));
        let r = d - n * (2.0 * d.dot(n));
        let o = hit.pos + n * 1.0e-3;
        let mut seen = Vec3f::default();
        let mut reach = f32::INFINITY;
        if let Some(back) = self.cast(o, r) {
            seen = if back.open { sky(r) } else { self.diffuse(&back, Self::foot(&back, r, hit.t + back.t, texel), beam) };
            reach = back.t;
        }
        if let Some(mirror) = &self.mirror {
            seen += mirror.look(o, r, reach, 0.0, texel.max(self.texel), light, true);
        }
        outside * ((1.0 - fresnel) * (1.0 - 0.04 * glass)) + seen * fresnel
    }

    /// The room's lights on the ground the camera sees along `d` past an
    /// opening: the ground's albedo there under each light that reaches it
    /// through an opening (its beam, the inverse square), so the light of a
    /// tunnel's or a bridge's lamps runs on out of its mouths and fades,
    /// where the outside itself (lightless, `sky`) has none of it. None
    /// without the ground, above the horizon or past the light's reach.
    fn lamps_outside(&self, d: Vec3f, beam: &dyn Fn(usize, Vec3f) -> f32) -> Vec3f {
        let Some(ground) = &self.ground else { return Vec3f::default() };
        if self.lights.is_empty() || !(d.y < -1.0e-4) {
            return Vec3f::default();
        }
        let p = d * (self.min.y / d.y);
        let up = vec3f(0.0, 1.0, 0.0);
        let mut e = Vec3f::default();
        for l in &self.lights {
            let k = lit_by(l, p, up, beam);
            if !(k.x + k.y + k.z > 0.0) {
                continue;
            }
            // the way from the light to the ground point leaves the room through an opening, not a wall
            let to = p - l.pos;
            let reach = to.length();
            if self.cast(l.pos, to * (1.0 / reach.max(1.0e-6))).is_some_and(|hit| hit.open && hit.t < reach) {
                e += k;
            }
        }
        if !(e.x + e.y + e.z > 0.0) {
            return Vec3f::default();
        }
        mul((ground.at)(p).albedo, e) * std::f32::consts::FRAC_1_PI
    }

    /// What the floor at `o` mirrors of the room and the outside along `r`,
    /// which meets the room at `back`, for a floor of `roughness`: the share
    /// of its lobe that leaves through the open walls (`OnMouth::share`)
    /// shows the outside (`outside_mirrored`), the rest the room's surface
    /// where `r` meets it, or where `r` itself leaves, the surfaces round the
    /// opening (`beside_mouth`). So a rough floor's image of a tunnel's mouth
    /// is as blurred as its image of the sun, and a polished one's is sharp,
    /// its edge filtered over the map's texel `texel`.
    fn mirrored_room(&self, o: Vec3f, r: Vec3f, back: &Hit, roughness: f32, texel: f32, beam: &dyn Fn(usize, Vec3f) -> f32, sky: &dyn Fn(Vec3f) -> Vec3f) -> Vec3f {
        // the room's image is as sharp as the camera's texel over the whole way, blurred by the floor's lobe
        let foot = Self::foot(back, r, o.length() + back.t, texel) + roughness * roughness * back.t;
        if self.mouths.is_empty() {
            return self.diffuse(back, foot, beam);
        }
        let lobe = Lobe::new(r, roughness, texel);
        let open = self.mouths.iter().filter_map(|mouth| lobe.on(mouth, o).map(|on| on.share() * mouth.glass)).sum::<f32>().min(1.0);
        if !(open > 0.0) {
            // (where `r` leaves with no share, the filter's own rounding: it sees the outside)
            return if back.open { self.outside_mirrored(r, roughness, texel, sky) } else { self.diffuse(back, foot, beam) };
        }
        let inside = if open >= 1.0 {
            Vec3f::default()
        } else if back.open {
            self.beside_mouth(o, r, beam)
        } else {
            self.diffuse(back, foot, beam)
        };
        inside * (1.0 - open) + self.outside_mirrored(r, roughness, texel, sky) * open
    }

    /// The room round the opening the mirrored ray `r` from `o` leaves
    /// through, for the part of the floor's lobe that does not leave: the
    /// surface at each of the opening's four edges, level with where `r`
    /// meets its plane (a wall, the ceiling, the floor or a cove meeting it
    /// there), weighted by the inverse square of the way to that edge. At an
    /// edge it is the surface there, so the room's image runs on unbroken
    /// across the opening's edge. Black where that is open too.
    fn beside_mouth(&self, o: Vec3f, r: Vec3f, beam: &dyn Fn(usize, Vec3f) -> f32) -> Vec3f {
        for mouth in &self.mouths {
            let along = r.dot(mouth.normal);
            if !(along < -1.0e-3) {
                continue;
            }
            let t = (mouth.center - o).dot(mouth.normal) / along;
            let p = o + r * t - mouth.center;
            let (u, v) = (p.dot(mouth.u), p.dot(mouth.v));
            if !(t > 0.0) || u.abs() > mouth.half.x || v.abs() > mouth.half.y {
                continue;
            }
            // Each edge: the way to it, and from a centimetre inside the room and the
            // opening, level with `r`'s point, the way out across it to its surface.
            let (inner_u, inner_v) = (mouth.half.x - 0.01, mouth.half.y - 0.01);
            let edges = [
                (mouth.half.x - u, mouth.u * inner_u + mouth.v * v, mouth.u),
                (mouth.half.x + u, mouth.u * -inner_u + mouth.v * v, -mouth.u),
                (mouth.half.y - v, mouth.u * u + mouth.v * inner_v, mouth.v),
                (mouth.half.y + v, mouth.u * u + mouth.v * -inner_v, -mouth.v),
            ];
            let (mut sum, mut weight) = (Vec3f::default(), 0.0f32);
            for (gap, from, out) in edges {
                let w = 1.0 / (gap * gap + 1.0e-8);
                if let Some(hit) = self.cast(mouth.center + from + mouth.normal * 0.01, out) {
                    if !hit.open {
                        // (the room's mean there: a blend of four points needs no finer detail)
                        sum += self.diffuse(&hit, BESIDE_MOUTH_FOOT, beam) * w;
                    }
                }
                weight += w;
            }
            return sum / weight;
        }
        Vec3f::default()
    }

    /// The outside along the mirrored ray `r` as a floor of `roughness`
    /// mirrors it. The sky is `sky(r)` as sharp as it is under a narrow lobe,
    /// and under a wide one the outside's table blurred over the lobe; a ray
    /// on a disc (the sun, the moon) reads the sky just beside it, and each disc is
    /// spread instead over the floor's lobe (about 1.4 roughness^2 radians
    /// across, drawn out toward the camera, as `mirror_lobe`), never less
    /// than its own size, its edges faded over the map's texel `texel`, its
    /// irradiance kept: a texel's sample shows its glint at the energy it
    /// has, wherever the texel falls, and a rough road blurs it.
    fn outside_mirrored(&self, r: Vec3f, roughness: f32, texel: f32, sky: &dyn Fn(Vec3f) -> Vec3f) -> Vec3f {
        let spread = (1.4 * roughness * roughness).min(1.2);
        let streak = 1.0 / r.y.max(0.15).sqrt();
        // A lobe within the outside's table's texel reads the sky itself, one two texels
        // wide or more the table blurred over it, and between the two blend.
        let table_texel = std::f32::consts::TAU / OUTSIDE_W as f32;
        let blurred = if self.outside.is_some() { smooth(table_texel, 2.0 * table_texel, spread) } else { 0.0 };
        let mut seen = Vec3f::default();
        if blurred < 1.0 {
            seen += sky(self.off_discs(r)) * (1.0 - blurred);
        }
        if let Some(table) = self.outside.as_ref().filter(|_| blurred > 0.0) {
            let pad = (OUTSIDE_W / 2) as f64;
            seen += lobe_mean(table, r, spread, (spread * streak).min(1.2), OUTSIDE_W as f64, pad) * blurred;
        }
        for sun in &self.daylight {
            let along = r.dot(sun.dir);
            if !(along > 0.0) {
                continue;
            }
            // The ray's angles from the disc across (horizontal) and up, in the disc's tangent plane.
            let across = perpendicular(sun.dir);
            let up = Vec3f::cross(sun.dir, across);
            let (x, y) = (r.dot(across).atan2(along), r.dot(up).atan2(along));
            let a = spread.max(sun.radius);
            let b = (spread * streak).min(1.2).max(sun.radius);
            let cover = stripe(x, a, texel) * stripe(y, b, texel);
            if cover > 0.0 {
                // The solid angle of the lobe: a rectangle of half angles a and b on the tangent plane.
                let (ta, tb) = (a.tan(), b.tan());
                let solid = 4.0 * (ta * tb / (1.0 + ta * ta + tb * tb).sqrt()).atan();
                seen += sun.irradiance * (cover / solid);
            }
        }
        seen
    }

    /// `d`, or where it falls on a disc outside (the sun, the moon), the sky
    /// just beside the disc, which the floor's mirror spreads on its own.
    fn off_discs(&self, d: Vec3f) -> Vec3f {
        let mut d = d;
        for sun in &self.daylight {
            let edge = (1.5 * sun.radius).min(std::f32::consts::FRAC_PI_2);
            let along = d.dot(sun.dir);
            if along > edge.cos() {
                let side = d - sun.dir * along;
                let side = if side.length() > 1.0e-6 { side.normalize() } else { perpendicular(sun.dir) };
                d = sun.dir * edge.cos() + side * edge.sin();
            }
        }
        d
    }

    /// The stands and the grid in front of the room along `d`, over `c`.
    fn over_rigging(&self, d: Vec3f, t_room: f32, c: Vec3f, beam: &dyn Fn(usize, Vec3f) -> f32) -> Vec3f {
        let mut c = c;
        let foot = 0.5 * LINE_FOOT;
        for rod in &self.rods {
            if d.dot(rod.axis) < rod.cos_reach {
                continue;
            }
            let Some((t, gap, normal)) = ray_to_segment(d, rod.a, rod.b) else { continue };
            if !(t > 0.0) || t >= t_room {
                continue;
            }
            let w = foot * t;
            let cover = smooth(rod.radius + w, (rod.radius - w).min(rod.radius * 0.5), gap);
            if cover > 0.0 {
                let lit = self.irradiance(d * t, normal, beam) * (METAL_ALBEDO * std::f32::consts::FRAC_1_PI);
                c = c + (lit - c) * cover;
            }
        }
        for group in &self.housings {
            group.near(d, t_room, |(housing, axis, cos_reach)| {
                if d.dot(*axis) < *cos_reach {
                    return;
                }
                let Some((excess, t, normal)) = ray_to_box(d, housing) else { return };
                if t >= t_room {
                    return;
                }
                let w = foot * t;
                let cover = smooth(w, -w, excess);
                if cover > 0.0 {
                    let lit = self.irradiance(d * t, normal, beam) * (housing.albedo * std::f32::consts::FRAC_1_PI);
                    c = c + (lit - c) * cover;
                }
            });
        }
        if let Some(spacing) = self.grid {
            let y = self.max.y - GRID_DROP;
            if d.y > 1.0e-4 {
                let t = y / d.y;
                if t < t_room {
                    let p = d * t;
                    let centre = (self.min + self.max) * 0.5;
                    let off = |v: f32| {
                        let s = v / spacing;
                        (s - s.round()).abs() * spacing
                    };
                    // Seen at a slant the pipe's band on the plane is wider.
                    let w = foot * t / d.y.max(0.08);
                    let cx = smooth(PIPE_RADIUS + w, (PIPE_RADIUS - w).min(0.5 * PIPE_RADIUS), off(p.x - centre.x));
                    let cz = smooth(PIPE_RADIUS + w, (PIPE_RADIUS - w).min(0.5 * PIPE_RADIUS), off(p.z - centre.z));
                    let cover = cx.max(cz);
                    if cover > 0.0 {
                        let lit = self.irradiance(p, vec3f(0.0, -1.0, 0.0), beam) * (PIPE_ALBEDO * std::f32::consts::FRAC_1_PI);
                        c = c + (lit - c) * cover;
                    }
                }
            }
        }
        c
    }
}

/// The light `l` gives a surface at `x` facing `n`, through its beam when shaped.
fn lit_by(l: &RoomLight, x: Vec3f, n: Vec3f, beam: &dyn Fn(usize, Vec3f) -> f32) -> Vec3f {
    let Some((k, w)) = facing(l, x, n) else { return Vec3f::default() };
    let k = if l.shaped_beam { k * beam(l.entry, w) } else { k };
    l.intensity * k
}

/// The light `l`, a bare Lambertian face, gives a surface at `x` facing `n`.
fn light_on(l: &RoomLight, x: Vec3f, n: Vec3f) -> Vec3f {
    facing(l, x, n).map_or(Vec3f::default(), |(k, _)| l.intensity * k)
}

/// The form factor of `l` at `x` facing `n` (its cosines over the distance
/// squared and its near-field term) and the way toward it; None where either
/// faces away.
fn facing(l: &RoomLight, x: Vec3f, n: Vec3f) -> Option<(f32, Vec3f)> {
    let v = l.pos - x;
    // The cosines' signs first, before the distance: most pairs face away.
    let (at_surface, at_light) = (n.dot(v), if l.omni { v.length() } else { -l.normal.dot(v) });
    if !(at_surface > 0.0) || !(at_light > 0.0) {
        return None;
    }
    let r2 = v.dot(v).max(1.0e-6);
    let inv = 1.0 / r2.sqrt();
    Some((at_surface * at_light * (inv * inv) / (r2 + l.near), v * inv))
}

/// The plane a box hit's inward normal names.
fn plane_of(n: Vec3f) -> usize {
    if n.y > 0.5 {
        FLOOR
    } else if n.y < -0.5 {
        CEILING
    } else if n.x > 0.5 {
        WEST
    } else if n.x < -0.5 {
        EAST
    } else if n.z > 0.5 {
        NORTH
    } else {
        SOUTH
    }
}

/// Plane `k`'s inward normal.
fn plane_normal(k: usize) -> Vec3f {
    match k {
        FLOOR => vec3f(0.0, 1.0, 0.0),
        CEILING => vec3f(0.0, -1.0, 0.0),
        WEST => vec3f(1.0, 0.0, 0.0),
        EAST => vec3f(-1.0, 0.0, 0.0),
        NORTH => vec3f(0.0, 0.0, 1.0),
        _ => vec3f(0.0, 0.0, -1.0),
    }
}

/// A real window (`RoomWindow`, as clamp() holds it) in the room between `min`
/// and `max`: its rectangle on its wall, held inside the wall (a hand's width
/// off its edges), its frame and bars, and its panes; None on an open wall or
/// where nothing of it is left inside the wall.
fn real_window(p: &RoomWindow, min: Vec3f, max: Vec3f, open: &[bool; 6], wall: Vec3f, floor: Vec3f) -> Option<RealWindow> {
    let p = p.clamped();
    let plane = [NORTH, SOUTH, EAST, WEST][p.wall_index()];
    if open[plane] {
        return None;
    }
    let mid = (min + max) * 0.5;
    let up = vec3f(0.0, 1.0, 0.0);
    // its wall's own axis along it (`Room::openings`'), where the wall stands, and how far "along" points along it:
    // toward the east on the north and south walls, toward the north (-z) on the east and west walls
    let (u, wall_at, along_sign, span) = match plane {
        NORTH => (vec3f(1.0, 0.0, 0.0), vec3f(mid.x, 0.0, min.z), 1.0, (min.x, max.x)),
        SOUTH => (vec3f(1.0, 0.0, 0.0), vec3f(mid.x, 0.0, max.z), 1.0, (min.x, max.x)),
        EAST => (vec3f(0.0, 0.0, 1.0), vec3f(max.x, 0.0, mid.z), -1.0, (min.z, max.z)),
        _ => (vec3f(0.0, 0.0, 1.0), vec3f(min.x, 0.0, mid.z), -1.0, (min.z, max.z)),
    };
    const EDGE: f32 = 0.05;
    let along = |a: Vec3f| a.dot(u);
    let c = along(wall_at) + along_sign * p.along_m;
    let (lo_u, hi_u) = ((c - 0.5 * p.width_m).max(span.0 + EDGE), (c + 0.5 * p.width_m).min(span.1 - EDGE));
    // (a door runs down to the floor)
    let (lo_v, hi_v) = if p.is_door() { (min.y, (min.y + p.height_m).min(max.y - EDGE)) } else { ((min.y + p.sill_m).max(min.y + EDGE), (min.y + p.sill_m + p.height_m).min(max.y - EDGE)) };
    if !(hi_u - lo_u > 0.05 && hi_v - lo_v > 0.05) {
        return None;
    }
    let half = vec2f(0.5 * (hi_u - lo_u), 0.5 * (hi_v - lo_v));
    let center = wall_at - u * along(wall_at) + u * (0.5 * (lo_u + hi_u)) + up * (0.5 * (lo_v + hi_v));
    let pane = vec2f(2.0 * half.x / p.panes_x as f32, 2.0 * half.y / p.panes_y as f32);
    let bar = (0.5 * p.frame_m).min(0.45 * pane.x.min(pane.y));
    let depth = p.depth_m.unwrap_or(ROOM_WINDOW_DEPTH_M).clamp(0.0, 1.0);
    // the frame stands two thirds of the way out in the wall, as deep as its bars are wide (at most the wall)
    let bar_depth = (2.0 * bar).min(depth);
    let set = (0.65 * depth).max(0.5 * bar_depth).min((depth - 0.5 * bar_depth).max(0.5 * bar_depth));
    let out = plane_normal(plane) * -1.0;
    let glass_amount = p.glass.unwrap_or(1.0).clamp(0.0, 1.0);
    let styled = (!p.is_plain()).then(|| Box::new(Styled::new(&p, half, depth, glass_amount, wall, floor, lo_v - min.y, max.y - hi_v)));
    Some(RealWindow { plane, center, u, v: up, half, bar, pane, out, depth, set, bar_depth, glass_amount, styled })
}

/// A point of plane `k` in the plane's own coordinates: (x, z) on the floor
/// and the ceiling, (z, y) on the east and west walls, (x, y) on the others.
fn plane_uv(k: usize, p: Vec3f) -> (f32, f32) {
    match k {
        FLOOR | CEILING => (p.x, p.z),
        WEST | EAST => (p.z, p.y),
        _ => (p.x, p.y),
    }
}

/// A horizontal unit vector square to `d` (unit); +x for a vertical `d`.
fn perpendicular(d: Vec3f) -> Vec3f {
    let h = vec3f(d.z, 0.0, -d.x);
    let len = h.length();
    if len > 1.0e-6 { h / len } else { vec3f(1.0, 0.0, 0.0) }
}

impl Opening {

    /// The outside's light through the opening, as the radiance of a
    /// Lambertian face filling it: the mean of `sky` over the half of the
    /// sphere the opening looks out at, cosine weighted (DAYLIGHT_SIDE²
    /// directions), leaving out `hot`'s discs (direction, angular radius),
    /// which shine in on their own (`disc_daylight`).
    pub fn daylight(&self, sky: &dyn Fn(Vec3f) -> Vec3f, hot: &[(Vec3f, f32)]) -> Vec3f {
        let out = -self.normal;
        let (a, b) = (self.u, self.v);
        let n = DAYLIGHT_SIDE;
        let (mut sum, mut count) = (Vec3f::default(), 0usize);
        for j in 0..n {
            for i in 0..n {
                // A cosine-weighted direction: an even point of the unit disc (concentric map) lifted onto the hemisphere.
                let (sx, sy) = (2.0 * (i as f32 + 0.5) / n as f32 - 1.0, 2.0 * (j as f32 + 0.5) / n as f32 - 1.0);
                let (r, phi) = if sx.abs() > sy.abs() {
                    (sx, std::f32::consts::FRAC_PI_4 * (sy / sx))
                } else {
                    (sy, std::f32::consts::FRAC_PI_2 - std::f32::consts::FRAC_PI_4 * (sx / sy))
                };
                let (px, py) = (r * phi.cos(), r * phi.sin());
                let up = (1.0 - px * px - py * py).max(0.0).sqrt();
                let d = (a * px + b * py + out * up).normalize();
                if hot.iter().any(|(dir, radius)| d.dot(*dir) > (1.5 * radius).min(std::f32::consts::FRAC_PI_2).cos()) {
                    continue;
                }
                sum += sky(d);
                count += 1;
            }
        }
        if count == 0 {
            return Vec3f::default();
        }
        sum / count as f32
    }

    /// The irradiance a surface at `x` facing `n` gets from the opening per
    /// unit of its radiance: the rectangle's exact form factor times pi
    /// (Lambert's formula, half the sum over its edges of the angle each
    /// spans times the cosine of its plane's normal with `n`), the rectangle
    /// first cut to the half-space `n` faces. A point at a mouth's lip sees
    /// it fill the half of its sky outward (pi / 2 on the floor beside it),
    /// a point deep inside a small far one. Four or five arctangents and
    /// square roots an opening per lookup.
    pub fn on(&self, x: Vec3f, n: Vec3f) -> f32 {
        let (du, dv) = (self.u * self.half.x, self.v * self.half.y);
        let c = self.center - x;
        let corners = [c - du - dv, c + du - dv, c + du + dv, c - du + dv];
        // Cut the rectangle to n . p >= 0: a quad cut by a plane keeps at most five corners.
        let mut poly = [Vec3f::default(); 5];
        let mut count = 0;
        for i in 0..4 {
            let (a, b) = (corners[i], corners[(i + 1) % 4]);
            let (da, db) = (n.dot(a), n.dot(b));
            if da >= 0.0 {
                poly[count] = a;
                count += 1;
            }
            if (da >= 0.0) != (db >= 0.0) {
                poly[count] = a + (b - a) * (da / (da - db));
                count += 1;
            }
        }
        if count < 3 {
            return 0.0;
        }
        let mut sum = 0.0f32;
        for i in 0..count {
            let (a, b) = (poly[i], poly[(i + 1) % count]);
            let (la, lb) = (a.length(), b.length());
            if !(la > 1.0e-6 && lb > 1.0e-6) {
                continue;
            }
            let (a, b) = (a / la, b / lb);
            let cross = Vec3f::cross(a, b);
            let len = cross.length();
            if len > 1.0e-9 {
                sum += len.atan2(a.dot(b)) * n.dot(cross) / len;
            }
        }
        0.5 * sum.abs()
    }
}

/// A disc of `sky` round `dir`, `radius` across (the sun, the moon), as
/// daylight: its irradiance on a surface facing it, summed over DISC_SIDE²
/// points of the square round it.
pub fn disc_daylight(sky: &dyn Fn(Vec3f) -> Vec3f, dir: Vec3f, radius: f32) -> Daylight {
    let dir = dir.normalize();
    let helper = if dir.y.abs() < 0.9 { vec3f(0.0, 1.0, 0.0) } else { vec3f(1.0, 0.0, 0.0) };
    let right = Vec3f::cross(helper, dir).normalize();
    let up = Vec3f::cross(dir, right);
    let n = DISC_SIDE;
    let half = radius.tan();
    let cell = 2.0 * half / n as f32;
    let mut sum = Vec3f::default();
    for j in 0..n {
        for i in 0..n {
            let (x, y) = (-half + (i as f32 + 0.5) * cell, -half + (j as f32 + 0.5) * cell);
            let r2 = 1.0 + x * x + y * y;
            // The solid angle of a tangent-plane cell.
            sum += sky((dir + right * x + up * y).normalize()) * (cell * cell / (r2 * r2.sqrt()));
        }
    }
    Daylight { dir, radius, irradiance: sum }
}

/// The mirror's layers for lights at `proxies`: each light's layer index
/// and the layers' places. Up to `MIRROR_LAYERS_MAX` lights each keep their
/// own; past it the two layers whose places lie nearest are merged, again
/// and again, into one at their mean (a plane's along their normal; planes
/// facing other ways, or a plane and a sphere, as a sphere as far off), so
/// the lights mirror where they stand as nearly as the layers allow. Cost:
/// nothing up to the bound; past it a pass over the pairs per merge, a few
/// million steps at most for the longest list a map holds.
fn mirror_groups(proxies: Vec<MirrorProxy>) -> (Vec<usize>, Vec<MirrorProxy>) {
    let n = proxies.len();
    if n <= MIRROR_LAYERS_MAX {
        return ((0..n).collect(), proxies);
    }
    // How far off a place is (a plane's offset along its normal, a sphere's radius).
    let far = |p: &MirrorProxy| match *p {
        MirrorProxy::Plane { offset, .. } => offset.abs(),
        MirrorProxy::Sphere { radius } => radius,
    };
    // Two places' gap as a share of how far off they stand; places of other kinds or facing other ways last.
    let gap = |a: &MirrorProxy, b: &MirrorProxy| match (*a, *b) {
        (MirrorProxy::Plane { normal: na, offset: oa }, MirrorProxy::Plane { normal: nb, offset: ob }) if na.dot(nb) > 0.999 => {
            (oa - ob).abs() / oa.abs().max(ob.abs()).max(0.1)
        }
        (MirrorProxy::Sphere { radius: ra }, MirrorProxy::Sphere { radius: rb }) => (ra - rb).abs() / ra.max(rb).max(0.1),
        _ => 2.0 + (far(a) - far(b)).abs() / far(a).max(far(b)).max(0.1),
    };
    // Per group: its place, its lights' count and its members.
    let mut groups: Vec<(MirrorProxy, f32, Vec<usize>)> = proxies.into_iter().enumerate().map(|(i, p)| (p, 1.0, vec![i])).collect();
    while groups.len() > MIRROR_LAYERS_MAX {
        let mut best = (f32::INFINITY, 0, 1);
        for i in 0..groups.len() {
            for j in i + 1..groups.len() {
                let d = gap(&groups[i].0, &groups[j].0);
                if d < best.0 {
                    best = (d, i, j);
                }
            }
        }
        let (_, i, j) = best;
        let (pb, wb, mb) = groups.swap_remove(j);
        let (pa, wa, ma) = &mut groups[i];
        let (ka, kb) = (*wa / (*wa + wb), wb / (*wa + wb));
        *pa = match (*pa, pb) {
            (MirrorProxy::Plane { normal: na, offset: oa }, MirrorProxy::Plane { normal: nb, offset: ob }) if na.dot(nb) > 0.999 => {
                MirrorProxy::Plane { normal: (na * ka + nb * kb).normalize(), offset: oa * ka + ob * kb }
            }
            (a, b) => MirrorProxy::Sphere { radius: far(&a) * ka + far(&b) * kb },
        };
        *wa += wb;
        ma.extend(mb);
    }
    let mut group = vec![0usize; n];
    for (k, (_, _, members)) in groups.iter().enumerate() {
        for &i in members {
            group[i] = k;
        }
    }
    (group, groups.into_iter().map(|(p, _, _)| p).collect())
}

impl Mirror {
    /// The lights seen from `o` along the mirrored ray `r`, each where the
    /// ray meets its own place (`MirrorProxy::meet`, a sphere's no further
    /// than `reach`, where the room stops it), for a floor of `roughness`. A
    /// lobe no wider than its layer's texel draws the light along the
    /// camera's direction to that point (`light`), for the footprint of the
    /// map's texel `texel` there (its path from the mirrored eye, seen from the
    /// camera) or of the lobe, whichever is wider; a lobe two of the layer's
    /// texels wide or more reads the layer, blurred over it; between, the
    /// two blend. A layer's share is set where the ray meets the layer's place
    /// (a light's own when it has the layer alone), and the layer is read
    /// once, with its first light. A light the ray runs away from (a floor
    /// point beyond that light) adds nothing.
    /// With `front_only` (a window's glass, which can look at a light from behind), a light's face seen from the
    /// side away from the camera adds nothing: its back is the housing's.
    #[allow(clippy::too_many_arguments)]
    fn look(&self, o: Vec3f, r: Vec3f, reach: f32, roughness: f32, texel: f32, light: &dyn Fn(usize, Vec3f, f32) -> Vec3f, front_only: bool) -> Vec3f {
        let mut sum = Vec3f::default();
        let from_eye = o.length();
        for l in &self.lights {
            // The layer's share of the lobe, and with its first light the layer itself.
            let (mut blurred, mut read) = (0.0, Vec3f::default());
            if let Some(layer) = l.layer.map(|g| &self.layers[g]) {
                if let Some(t) = layer.proxy.meet(o, r, reach) {
                    let q = o + r * t;
                    let len = q.length().max(1.0e-4);
                    let spread = mirror_lobe(roughness, t, len);
                    blurred = smooth(layer.texel, 2.0 * layer.texel, spread);
                    if l.first && blurred > 0.0 {
                        read = layer.at(q / len, spread, r) * blurred;
                    }
                }
            }
            if let Some(t) = l.proxy.meet(o, r, reach) {
                let behind = match l.proxy {
                    MirrorProxy::Plane { normal, offset } => front_only && !((o.dot(normal) - offset) * -offset > 0.0),
                    MirrorProxy::Sphere { .. } => false,
                };
                if blurred < 1.0 && !behind {
                    let q = o + r * t;
                    let len = q.length().max(1.0e-4);
                    let foot = (texel * (from_eye + t) / len).max(2.0 * mirror_lobe(roughness, t, len));
                    sum += light(l.entry, q / len, foot) * (1.0 - blurred);
                }
            }
            sum += read;
        }
        sum
    }
}

impl MirrorLayer {
    /// The layer along `dir` (where the mirrored ray `r` lands), blurred over
    /// a lobe of half-angle `spread` as the camera sees it, drawn out toward
    /// the camera the flatter the floor is seen, as a floor's reflections
    /// streak.
    fn at(&self, dir: Vec3f, spread: f32, r: Vec3f) -> Vec3f {
        let streak = 1.0 / r.y.max(0.15).sqrt();
        lobe_mean(&self.table, dir, spread, spread * streak, self.table.width() as f64, 0.0)
    }
}

/// The mean of an equirect `table` (its turn `w` texels wide, after `pad`
/// texels of padding) over a lobe round `dir` of half-angles `across`
/// (horizontal) and `up`, radians: a box as wide as the lobe on the sphere,
/// wider in texels toward the poles.
fn lobe_mean(table: &SummedArea<3>, dir: Vec3f, across: f32, up: f32, w: f64, pad: f64) -> Vec3f {
    let uv = dir_to_equirect_uv([dir.x, dir.y, dir.z]);
    let h = table.height() as f64;
    let rx = (across as f64 / std::f64::consts::PI) * h / (dir.y.abs().min(0.999) as f64).acos().sin().max(0.05);
    let ry = (up as f64 / std::f64::consts::PI) * h;
    let m = table.box_mean(uv[0] as f64 * w + pad, uv[1] as f64 * h, rx.min(0.5 * w), ry);
    vec3f(m[0] as f32, m[1] as f32, m[2] as f32)
}

/// The closest approach of the camera's ray along `d` (unit) to the segment
/// a..b: the ray's distance there, the gap between them, and the rod's
/// surface normal facing the ray. None for a degenerate segment.
fn ray_to_segment(d: Vec3f, a: Vec3f, b: Vec3f) -> Option<(f32, f32, Vec3f)> {
    let u = b - a;
    let uu = u.dot(u);
    if !(uu > 1.0e-10) {
        return None;
    }
    // Minimise |d t - (a + u s)| over t >= 0, s in 0..1.
    let du = d.dot(u);
    let da = d.dot(a);
    let ua = u.dot(a);
    let denom = uu - du * du;
    let mut s = if denom.abs() > 1.0e-9 { (du * da - ua) / denom } else { 0.0 };
    s = s.max(0.0).min(1.0);
    let p = a + u * s;
    let t = d.dot(p).max(0.0);
    let q = d * t;
    let gap_v = q - p;
    let gap = gap_v.length();
    let normal = if gap > 1.0e-9 { gap_v / gap } else { -d };
    Some((t, gap, normal))
}
/// Hybrid studios (2026-11-05): a room's sides open per wall and floor, its real windows, and an old room road.
#[cfg(test)]
mod hybrid_tests {
    use super::*;
    use crate::params::{FloorKind, RoadBeside, RoadParams, RoomSides, OPEN_WALL_NAMES};
    use crate::presets::preset;

    #[test]
    fn every_old_open_walls_name_loads_to_the_same_walls() {
        let (t, f) = (true, false);
        let want = [
            ("none", [f, f, f, f]),
            ("back", [t, f, f, f]),
            ("front", [f, t, f, f]),
            ("ends", [t, t, f, f]),
            ("east", [f, f, t, f]),
            ("west", [f, f, f, t]),
            ("sides", [f, f, t, t]),
            ("all", [t, t, t, t]),
        ];
        assert_eq!(OPEN_WALL_NAMES.len(), want.len());
        for (name, walls) in want {
            let room = RoomParams { open_walls: Some(name.to_string()), ..RoomParams::default() };
            assert_eq!(room.open(), walls, "{name}");
            assert!(!room.open_floor(), "{name}: an old file's floor stays closed");
            assert_eq!(room.is_hybrid(), walls.contains(&true), "{name}");
        }
        // `sides`, once written, wins over the old name
        let room = RoomParams { open_walls: Some("all".to_string()), sides: Some(RoomSides { west: true, ..RoomSides::default() }), ..RoomParams::default() };
        assert_eq!(room.open(), [f, f, f, t]);
    }

    #[test]
    fn a_fake_window_alone_is_not_hybrid() {
        let fake = preset("Wooden daylight").unwrap();
        assert!(fake.lights.iter().any(|l| l.gobo.as_ref().is_some_and(|g| g.fill_sky)), "the preset has a window light");
        assert!(!fake.is_hybrid(), "a window light is a fake window");
        let mut real = fake.clone();
        real.studio.room.get_or_insert_with(RoomParams::default).windows = Some(vec![RoomWindow::default()]);
        assert!(real.is_hybrid(), "a real window opens the room");
        let mut open = fake.clone();
        open.studio.room.get_or_insert_with(RoomParams::default).sides = Some(RoomSides { east: true, ..RoomSides::default() });
        assert!(open.is_hybrid(), "an open wall opens the room");
        assert!(!preset("Clear noon").unwrap().is_hybrid(), "a sky is not a studio");
        assert!(preset("Road tunnel").unwrap().is_hybrid() && preset("Bridge").unwrap().is_hybrid());
    }

    #[test]
    fn an_old_room_road_becomes_the_grounds_road() {
        let mut p = preset("Car studio").unwrap();
        let room = p.studio.room.get_or_insert_with(RoomParams::default);
        room.open_walls = Some("ends".to_string());
        room.road = Some(RoadParams { lanes: 3, lane_width_m: 3.25, ..RoadParams::default() });
        p.floor = None;
        let fixed = p.clamp();
        assert!(fixed.iter().any(|f| f == "studio.room.road"), "{fixed:?}");
        let room = p.studio.room.as_ref().unwrap();
        assert!(room.road.is_none());
        assert_eq!(room.open(), [true, true, false, false], "its open walls carry over");
        assert!(room.open_floor(), "it stands on the ground");
        let floor = p.floor.as_ref().unwrap();
        assert_eq!((floor.kind(), floor.lanes, floor.lane_width(), floor.beside()), (FloorKind::Road, 3, 3.25, RoadBeside::Room));
        assert_eq!(floor.beside_color, Some(room.floor));
        assert!(p.is_hybrid());
        assert!(p.clamp().is_empty(), "moved once");
    }

    #[test]
    fn a_real_window_opens_its_panes_and_keeps_its_bars() {
        // the default room: 10 m wide, 12 deep, the camera 1.6 m up in its middle; the window in the back wall (z -6)
        // from 1 m to 3 m above the floor (y -0.6 to 1.4 from the camera), x -1.5 to 1.5: 3 x 2 panes a metre square,
        // bars 10 cm wide at x -0.5 and 0.5 and y 0.4
        let window = RoomWindow { wall: "north".to_string(), along_m: 0.0, sill_m: 1.0, width_m: 3.0, height_m: 2.0, panes_x: 3, panes_y: 2, frame_m: 0.1, ..RoomWindow::default() };
        let room = Room::new(&RoomParams { windows: Some(vec![window]), ..RoomParams::default() });
        let open = |x: f32, y: f32| room.cast(Vec3f::default(), vec3f(x, y, -6.0).normalize()).unwrap().open;
        assert!(open(0.0, -0.1) && open(-1.0, 0.9) && open(1.0, -0.1), "a pane");
        assert!(!open(0.5, -0.1) && !open(0.0, 0.4), "a bar");
        assert!(!open(-1.47, -0.1), "the frame");
        assert!(!open(3.0, -0.1) && !open(0.0, 2.0), "the wall beside and above");
        assert!(!room.cast(Vec3f::default(), vec3f(0.0, -0.1, 6.0).normalize()).unwrap().open, "the other walls stay closed");
        let opening = *room.openings().last().unwrap();
        // (the panes' share of the rectangle, 0.81, times what the glass lets through, 0.88)
        assert!((opening.glass - 0.81 * 0.88).abs() < 1.0e-3, "the panes' share: {}", opening.glass);
        assert!((opening.half.x - 1.5).abs() < 1.0e-4 && (opening.half.y - 1.0).abs() < 1.0e-4);
    }

    #[test]
    fn a_window_light_made_real_stands_where_it_is_seen() {
        // looking at the back wall (6 m off) 0.5 m right of the middle and 0.4 m up, a window 2 x 1 m as seen
        let room = Room::new(&RoomParams::default());
        let d = vec3f(0.5, 0.4, -6.0).normalize();
        let t = vec3f(0.5, 0.4, -6.0).length();
        let (w, h) = (2.0 * (1.0 / t).atan(), 2.0 * (0.5 / t).atan());
        let window = room.window_seen(d, w, h, (4, 2), 0.05).unwrap();
        assert_eq!((window.wall.as_str(), window.panes_x, window.panes_y), ("north", 4, 2));
        assert!((window.along_m - 0.5).abs() < 1.0e-3 && (window.width_m - 2.0).abs() < 1.0e-3 && (window.height_m - 1.0).abs() < 1.0e-3, "{window:?}");
        // its middle 0.4 m over the camera, 2 m over the floor; its sill half its height under that
        assert!((window.sill_m - (1.6 + 0.4 - 0.5)).abs() < 1.0e-3, "{window:?}");
        // the real window opens where the light was seen
        let real = Room::new(&RoomParams { windows: Some(vec![window]), ..RoomParams::default() });
        assert!(real.cast(Vec3f::default(), vec3f(0.3, 0.3, -6.0).normalize()).unwrap().open);
        // the floor, the ceiling and an open wall take none
        assert!(room.window_seen(vec3f(0.0, -1.0, -0.1).normalize(), w, h, (1, 1), 0.05).is_none());
        let open = Room::new(&RoomParams { sides: Some(RoomSides { north: true, ..RoomSides::default() }), ..RoomParams::default() });
        assert!(open.window_seen(d, w, h, (1, 1), 0.05).is_none());
    }
}

