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
//! A road (plan P3, `RoadParams`) is painted on the floor along the room's
//! long side: asphalt whose albedo varies over a few metres, solid edge
//! lines and dashed lines between the lanes, each line's edges and each
//! dash's ends box-filtered over a 2048 wide map's texel where the floor is
//! seen (`LINE_FOOT`, as the stands' edges), and its own gloss and roughness
//! for the mirror, blended into the floor's across the road's filtered edge.
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
use makepad_draw::*;
use makepad_render_material::ibl::{dir_to_equirect_uv, equirect_uv_to_dir, EnvMap};

use super::image_light::SummedArea;
use super::noise::fbm2;
use super::params::{CoveWalls, RoadParams, RoomParams};
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
/// A road's asphalt varies over patches about this wide, metres, and its noise's seed.
const ROAD_PATCH_M: f32 = 3.0;
const ROAD_SEED: u32 = 0x726f_6164;
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
        let centre = (a + b) * 0.5;
        let reach = 0.5 * (b - a).length() + radius;
        let far = centre.length();
        let (axis, cos_reach) = if far > reach * 1.001 {
            let angle = (reach / far).asin() + LINE_FOOT;
            (centre / far, if angle < std::f32::consts::PI { angle.cos() } else { -1.0 })
        } else {
            (vec3f(0.0, 1.0, 0.0), -1.0)
        };
        Rod { a, b, radius, axis, cos_reach }
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
}

/// One open wall (`Room::openings`): its middle, the way into the room, its
/// own axes along the wall (horizontal `u`, up `v`) and its half sizes along them, metres.
#[derive(Clone, Copy, Debug)]
pub struct Opening {
    pub center: Vec3f,
    pub normal: Vec3f,
    pub u: Vec3f,
    pub v: Vec3f,
    pub half: Vec2f,
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

/// A road on the floor (`RoadParams`) as the room draws it: `across` and
/// `along` are the floor's axes across and along the road (x and z for a road
/// running north to south), `centre` the road's middle line and `anchor` the
/// middle of a dash, in those coordinates.
#[derive(Clone, Debug)]
struct Road {
    along_z: bool,
    centre: f32,
    anchor: f32,
    half: f32,
    lane: f32,
    lanes: u32,
    line_half: f32,
    asphalt: Vec3f,
    variation: f32,
    paint: Vec3f,
    edge_lines: bool,
    dash: f32,
    gap: f32,
    gloss: f32,
    roughness: f32,
}

#[derive(Debug)]
pub struct Room {
    min: Vec3f,
    max: Vec3f,
    cove: f32,
    walls: CoveWalls,
    /// Which planes stand open (indexed as FLOOR .. SOUTH; only walls do).
    open: [bool; 6],
    floor: Vec3f,
    wall: Vec3f,
    ceiling: Vec3f,
    gloss: f32,
    roughness: f32,
    bounce: f32,
    road: Option<Road>,
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

fn smooth(e0: f32, e1: f32, x: f32) -> f32 {
    let t = ((x - e0) / (e1 - e0)).max(0.0).min(1.0);
    t * t * (3.0 - 2.0 * t)
}

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

/// The share of `s - w/2 .. s + w/2` that dashes `dash` long, `gap` apart,
/// the first starting at 0, cover: the dashes box-filtered over `w`.
fn dashes(s: f32, dash: f32, gap: f32, w: f32) -> f32 {
    let period = dash + gap;
    // How much of 0..x the dashes cover (x of any sign).
    let on = |x: f32| {
        let k = (x / period).floor();
        k * dash + (x - k * period).max(0.0).min(dash)
    };
    if !(w > 1.0e-6) {
        let k = s - (s / period).floor() * period;
        return if k < dash { 1.0 } else { 0.0 };
    }
    ((on(s + 0.5 * w) - on(s - 0.5 * w)) / w).max(0.0).min(1.0)
}

impl Road {
    fn new(p: &RoadParams, min: Vec3f, max: Vec3f) -> Road {
        let p = p.clamped();
        let along_z = max.z - min.z >= max.x - min.x;
        let middle = (min + max) * 0.5;
        let (centre, anchor) = if along_z { (middle.x, middle.z) } else { (middle.z, middle.x) };
        Road {
            along_z,
            centre,
            anchor: anchor - 0.5 * p.dash_m,
            half: 0.5 * p.lanes as f32 * p.lane_width_m,
            lane: p.lane_width_m,
            lanes: p.lanes,
            line_half: 0.5 * p.line_width_m,
            asphalt: rgb(p.asphalt),
            variation: p.variation,
            paint: Vec3f::all(p.paint),
            edge_lines: p.edge_lines,
            dash: p.dash_m,
            gap: p.gap_m,
            gloss: p.gloss,
            roughness: p.roughness,
        }
    }

    /// The floor point `pos` (seen from the camera at its distance) in the
    /// road's coordinates, and the footprint of a LINE_FOOT texel there
    /// across and along the road, metres.
    fn place(&self, pos: Vec3f) -> (f32, f32, f32, f32) {
        let (across, along) = if self.along_z { (pos.x - self.centre, pos.z - self.anchor) } else { (pos.z - self.centre, pos.x - self.anchor) };
        let t = pos.length();
        let flat = vec2f(pos.x, pos.z).length().max(1.0e-6);
        // The texel's footprint on the floor: t LINE_FOOT across the view, stretched by 1 / sin(depression) along it.
        let foot = t * LINE_FOOT;
        let stretch = t / (-pos.y).max(0.02 * t);
        let (hx, hz) = (pos.x / flat, pos.z / flat);
        let (h_across, h_along) = if self.along_z { (hx, hz) } else { (hz, hx) };
        let w_across = foot * ((h_across * stretch).powi(2) + h_along * h_along).sqrt();
        let w_along = foot * ((h_along * stretch).powi(2) + h_across * h_across).sqrt();
        (across, along, w_across, w_along)
    }

    /// How much of the texel at floor point `pos` the road covers, filtered.
    fn cover(&self, pos: Vec3f) -> f32 {
        let (across, _, w_across, _) = self.place(pos);
        stripe(across, self.half, w_across)
    }

    /// The floor's albedo at `pos` where the road lies: the asphalt, its
    /// patches and its paint, blended into `floor` across the road's edge.
    fn albedo(&self, pos: Vec3f, floor: Vec3f) -> Vec3f {
        let (across, along, w_across, w_along) = self.place(pos);
        let road = stripe(across, self.half, w_across);
        if !(road > 0.0) {
            return floor;
        }
        let (x, y) = (across / ROAD_PATCH_M, along / ROAD_PATCH_M);
        let asphalt = self.asphalt * (1.0 + self.variation * 1.2 * (fbm2(x, y, 3, ROAD_SEED) - 0.5));
        let mut paint = 0.0f32;
        if self.edge_lines {
            let edge = self.half - self.line_half;
            paint += stripe(across - edge, self.line_half, w_across) + stripe(across + edge, self.line_half, w_across);
        }
        for k in 1..self.lanes {
            let at = -self.half + k as f32 * self.lane;
            let line = stripe(across - at, self.line_half, w_across);
            if line > 0.0 {
                paint += line * if self.gap > 0.0 { dashes(along, self.dash, self.gap, w_along) } else { 1.0 };
            }
        }
        let paint = paint.min(1.0);
        let surface = asphalt + (self.paint - asphalt) * paint;
        floor + (surface - floor) * road
    }

    /// The road's mean albedo, its paint's share in.
    fn mean(&self) -> Vec3f {
        let width = 2.0 * self.half;
        let mut paint = 0.0;
        if self.edge_lines {
            paint += 4.0 * self.line_half;
        }
        let dashed = if self.gap > 0.0 { self.dash / (self.dash + self.gap) } else { 1.0 };
        paint += (self.lanes.saturating_sub(1)) as f32 * 2.0 * self.line_half * dashed;
        let share = (paint / width).min(1.0);
        self.asphalt + (self.paint - self.asphalt) * share
    }
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
        let mut room = Room {
            min,
            max,
            cove,
            walls,
            open,
            floor: rgb(p.floor),
            wall: rgb(p.wall),
            ceiling: rgb(p.ceiling),
            gloss: p.floor_gloss,
            roughness: p.floor_roughness,
            bounce: p.bounce,
            road: p.road.as_ref().map(|road| Road::new(road, min, max)),
            lights: Vec::new(),
            smooth: Vec::new(),
            shaped: Vec::new(),
            daylight: Vec::new(),
            windows: Vec::new(),
            ambient: Vec3f::default(),
            rods: Vec::new(),
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
                Opening { center, normal, u, v: up, half: vec2f(half_u, half_y) }
            })
            .collect()
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
        // An open wall reflects nothing; a road takes its share of the floor.
        let size = self.max - self.min;
        let (floor_area, ceiling_area) = (size.x * size.z, size.x * size.z);
        let wall_areas = [size.y * size.z, size.y * size.z, size.y * size.x, size.y * size.x];
        let wall_area: f32 = wall_areas.iter().sum();
        let openings = self.openings();
        let open_area: f32 = openings.iter().map(|o| 4.0 * o.half.x * o.half.y).sum();
        let closed_wall = (wall_area - open_area).max(0.0);
        let area = floor_area + wall_area + ceiling_area;
        let floor_mean = match &self.road {
            Some(road) => {
                let (lo, hi) = if road.along_z { (self.min.x, self.max.x) } else { (self.min.z, self.max.z) };
                let covered = ((road.centre + road.half).min(hi) - (road.centre - road.half).max(lo)).max(0.0) / (hi - lo).max(1.0e-6);
                self.floor + (road.mean() - self.floor) * covered
            }
            None => self.floor,
        };
        let mean = (floor_mean * floor_area + self.wall * closed_wall + self.ceiling * ceiling_area) / area;
        let mut put_out = lights.iter().fold(Vec3f::default(), |sum, l| sum + l.intensity * std::f32::consts::PI);
        // An opening puts in pi times its radiance over its area, as a Lambertian face filling it does.
        for (opening, radiance) in &self.windows {
            put_out += *radiance * (std::f32::consts::PI * 4.0 * opening.half.x * opening.half.y);
        }
        // A disc outside puts in its irradiance over each opening it shines in through, as the opening faces it.
        for sun in &self.daylight {
            for opening in &openings {
                put_out += sun.irradiance * (opening.half.x * opening.half.y * 4.0 * (-opening.normal.dot(sun.dir)).max(0.0));
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
                .filter(|&i| corners.iter().any(|c| self.lights[i].normal.dot(*c - self.lights[i].pos) > 0.0))
                .filter_map(|i| {
                    let l = &self.lights[i];
                    let d = (l.pos - corners[0]).dot(normal);
                    let lit = if l.normal.dot(normal) < 0.0 { d } else { d / Vec3f::cross(l.normal, normal).length().max(1.0e-6) };
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
                    let front = corners.iter().filter(|c| l.normal.dot(**c - l.pos) > 0.0).count();
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
        let glossy = self.gloss > 0.0 || self.road.as_ref().is_some_and(|road| road.gloss > 0.0);
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

    /// Where a ray from `o` (inside the room) along `d` (unit) meets the room.
    pub fn cast(&self, o: Vec3f, d: Vec3f) -> Option<Hit> {
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
            return Some(Hit { t: t_box, pos: at, normal: n_box, plane: Some(plane), open: self.open[plane] });
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
        Some(Hit { t, pos, normal, plane: None, open: false })
    }

    /// The floor's albedo at `pos`: the room's, or its road's where it lies.
    fn floor_at(&self, pos: Vec3f) -> Vec3f {
        match &self.road {
            Some(road) => road.albedo(pos, self.floor),
            None => self.floor,
        }
    }

    /// The albedo where `hit` lands: the floor's (its road's on the flat
    /// floor), turned up through a cove into the wall's, or the ceiling's.
    fn albedo(&self, hit: &Hit) -> Vec3f {
        if hit.normal.y < -0.5 {
            return self.ceiling;
        }
        let floor = if hit.plane == Some(FLOOR) { self.floor_at(hit.pos) } else { self.floor };
        let k = smooth(0.25, 0.95, hit.normal.y);
        self.wall + (floor - self.wall) * k
    }

    /// The floor's gloss and roughness at floor point `pos`: its road's where it lies.
    fn gloss_at(&self, pos: Vec3f) -> (f32, f32) {
        match &self.road {
            Some(road) => {
                let k = road.cover(pos);
                (self.gloss + (road.gloss - self.gloss) * k, self.roughness + (road.roughness - self.roughness) * k)
            }
            None => (self.gloss, self.roughness),
        }
    }

    /// The outside's light on a surface at `x` facing `n`: each opening's
    /// (`Opening::on`), and each disc's where a ray toward it leaves through an open wall.
    fn daylight_on(&self, x: Vec3f, n: Vec3f) -> Vec3f {
        let mut e = Vec3f::default();
        for (opening, radiance) in &self.windows {
            e += *radiance * opening.on(x, n);
        }
        for sun in &self.daylight {
            let facing = n.dot(sun.dir);
            if !(facing > 0.0) {
                continue;
            }
            if self.cast(x + n * 1.0e-3, sun.dir).is_some_and(|hit| hit.open) {
                e += sun.irradiance * facing;
            }
        }
        e
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

    /// What a diffuse surface at `hit` sends back.
    fn diffuse(&self, hit: &Hit, beam: &dyn Fn(usize, Vec3f) -> f32) -> Vec3f {
        mul(self.albedo(hit), self.surface_irradiance(hit, beam)) * std::f32::consts::FRAC_1_PI
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
        let Some(hit) = self.cast(Vec3f::default(), d) else { return Vec3f::default() };
        if hit.open {
            return self.over_rigging(d, hit.t, sky(d), beam);
        }
        let mut c = self.diffuse(&hit, beam);
        // A glossy floor mirrors the room, the outside through its open walls, and the lights.
        let (gloss, roughness) = if hit.plane == Some(FLOOR) { self.gloss_at(hit.pos) } else { (self.gloss, self.roughness) };
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
                seen += mirror.look(o, r, reach, roughness, texel, light);
            }
            c = c * (1.0 - fresnel) + seen * fresnel;
        }
        self.over_rigging(d, hit.t, c, beam)
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
        if self.mouths.is_empty() {
            return self.diffuse(back, beam);
        }
        let lobe = Lobe::new(r, roughness, texel);
        let open = self.mouths.iter().filter_map(|mouth| lobe.on(mouth, o)).map(|on| on.share()).sum::<f32>().min(1.0);
        if !(open > 0.0) {
            // (where `r` leaves with no share, the filter's own rounding: it sees the outside)
            return if back.open { self.outside_mirrored(r, roughness, texel, sky) } else { self.diffuse(back, beam) };
        }
        let inside = if open >= 1.0 {
            Vec3f::default()
        } else if back.open {
            self.beside_mouth(o, r, beam)
        } else {
            self.diffuse(back, beam)
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
                        sum += self.diffuse(&hit, beam) * w;
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
    let (at_surface, at_light) = (n.dot(v), -l.normal.dot(v));
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
    fn look(&self, o: Vec3f, r: Vec3f, reach: f32, roughness: f32, texel: f32, light: &dyn Fn(usize, Vec3f, f32) -> Vec3f) -> Vec3f {
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
                if blurred < 1.0 {
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
