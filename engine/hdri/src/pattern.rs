//! Plan P2: pattern lights (shape "pattern", `PatternParams`): an SVG
//! pattern of light tubes and panels on a light's 3D face, the LED tube
//! grids of car rooms (a hexagon ceiling over a car, wall bars, chevrons,
//! LED portals).
//!
//! The pattern is SVG text: a generator writes it (`generate_svg`: hexagons,
//! bars, chevrons, triangles, frames, lines, in millimetres at the pattern's
//! natural size), or the document carries its own (`PatternParams::svg`).
//! Both go through one path: makepad free's SVG parser (`makepad_svg`), the
//! nodes walked with their transforms (`items`), each a stroke (a tube) or a
//! fill (a flat panel) with its colour or gradient, then rastered.
//!
//! The face and the picture. The SVG's viewBox fills the light's face (its
//! `SpaceParams` width and height, metres); clamp() holds the face to the
//! pattern's aspect, its height following its width (`hold_face`), so the
//! face is never stretched and scaling it scales the pattern uniformly. A
//! new pattern light's face takes the pattern's natural size (a generator's
//! metres, or a drawn SVG's real units: mm, cm, m, in or pt on its root
//! width and height; `natural_size_m`); `fit_face` sets it so again.
//!
//! The raster (`PatternRaster`). Once per map build the pattern is rastered
//! into an emission picture on the face's plane, RGB the light's tint (white
//! is the light's own colour) and A the cover of its tubes and panels, at a
//! resolution matched to the map: a face texel no larger than the map's
//! texel (its vertical size, 2 pi / width, the larger of its two) seen at
//! the face's nearest point, the longer side rounded up to a power of two
//! so that small moves keep it. The width is held to at least
//! `CASTER_REFERENCE_WIDTH`, as a shadow caster's is (D6): a narrower map
//! (the held 512 preview, a 64 wide test bake) reads the 1024 one's raster,
//! so the room it lights, which is read from the raster, is the same at
//! every width up to it and an Env built for any of them is the reference
//! one's. Bounded gracefully: past `RASTER_MAX_SIDE`
//! a side or `RASTER_MAX_TEXELS` in all it is rastered coarser, with one log
//! line, and its tubes are still drawn exactly (`Crisp`, below). Rasters
//! are kept across builds by what they depend on (the own
//! drawing's text, else the generator's numbers; the look, the mounts and
//! the resolution): every raster a map being built or shown holds, and of
//! the others the most recently used up to `RASTERS_SPARE_BYTES`, so a face
//! dragged with the gizmo is not rastered again however many pattern lights
//! there are. The generator's SVG is written only when its raster is made.
//!
//! What the raster is bounded by, so a drawing cannot hang a build: a
//! `<use>` that names a symbol it is inside of is skipped (SVG holds it an
//! error), and the walk visits at most `WALK_BUDGET` nodes and points in
//! all (what repeats past that, through `<use>` fan-out, is left out and
//! logged); geometry further off the face than a tube's reach is flattened
//! coarsely and not dashed; a tube is at most `HALF_MAX_DIAGONALS` face
//! diagonals wide each side (along the face, that one covers it whole); a dash
//! pattern finer than `DASH_MIN_PERIOD` is drawn at that period, its share
//! of on and off kept (logged); frames nest at most `PATTERN_COUNT_MAX`
//! deep, spread wider past that (logged).
//!
//! Tubes are drawn from the distance to their centre line through the look's
//! cross-section (`Profile`): 0 neon, a round glass tube, its core brighter,
//! a soft glow round it and its brightness a little uneven along it; 1 an
//! LED tube, a flat even diffuser with crisp edges and dark end caps;
//! between, a blend. The cross-section is box-filtered over a texel across
//! the tube, so a tube far thinner than a texel still puts its whole light
//! into the picture. Mounts dim a tube where it is fixed (every
//! `mount_spacing_m`) and at its corners and bends (`corner_dim`). Within
//! one path (a generator writes all its tubes as one) each line is drawn
//! from its nearest piece, and the lines' light and cover add up where they
//! share a texel, so tubes side by side in one texel fill it as they do on
//! the face; the sum is held to what one tube gives at its brightest, so
//! tubes that cross do not glow brighter where they do, and a piece lying
//! on another line's piece (a honeycomb's shared edge, written once per
//! cell) is drawn once. Fills are covered by exact horizontal spans on four
//! sub-scanlines a row, over the edges that span each (an active list).
//! Paths go over one another in document order, each hiding what it covers.
//!
//! Drawing a map texel (`PatternLight::lookup`): one hit of the view ray on
//! the face's plane (`studio::project`, plan P1), then the picture filtered
//! over the texel's footprint on the plane: the texel's two sides carried
//! onto the plane through the hit's Jacobian, their box in the picture's
//! texels, read from the mip level the narrower side asks for (box-averaged
//! levels, so the light a level holds is exact) with up to `MAX_TAPS` taps
//! along each side, blended between two levels. A dense honeycomb seen far
//! off is its mean light, not a moire, and a thin tube far off adds its
//! whole light.
//!
//! Where the footprint is narrow (its shorter side at most `CRISP_SHORT`
//! raster texels, its longer at most `CRISP_LONG`; blended into the levels
//! up to twice that), the tubes are drawn exactly instead (`Crisp`): the
//! drawing's tube items kept as their pieces, laid out at the size the map
//! asks for (the raster's before any bound), all found through one grid
//! over the face whose cells list the pieces and panel pictures near them,
//! so a lookup costs the same however many items the drawing has; per
//! lookup each line's nearest piece is filtered across the footprint from
//! the cross-section's running integrals (`TubeSums`: two boxes convolved,
//! exact), its mounts, caps and corners as the raster draws them, the lines
//! summed and held as there. So a tube's edge is as sharp as the map's
//! texel at any bake size, with no stair of raster texels behind it, near
//! and at a slant. The footprint is filtered twice (a tent as wide at half
//! height as the footprint, reaching as far as the footprint is wide each
//! way, and the grid is searched that far), so a tube far brighter than the
//! display's white shows a round foot, not the texel grid. Each run of
//! panels between tube items is its own picture at the raster's level-0
//! texels over what it covers, with its levels, filtered as the raster is,
//! composited in document order. Past `CRISP_PANELS_BYTES` of such pictures
//! the drawing's first items are drawn into one picture as the raster draws
//! them (their tubes no longer exact), logged. Where the levels hold
//! nothing the exact tubes are not looked at.
//!
//! Cost. Per build: the cache key (a few numbers, an own drawing's hash
//! taken when it was loaded), then on a miss the raster, once per pattern
//! and resolution (the generator's SVG written and parsed, one pass over
//! each segment's box of texels and each fill's rows; a 2048 x 1024
//! honeycomb of 20 hexagons in tens of milliseconds). Per map
//! texel the light's cull cone holds: one plane hit, the footprint (a few
//! dot products) and 1 to 2 x `MAX_TAPS`² bilinear taps (typically 2 to 8);
//! where the footprint is narrow and the levels hold light, the exact tubes
//! besides: a grid cell or a few, each line's nearest piece among the few
//! listed there, eight reads of its tables, and for each panel picture
//! listed there the taps the raster's levels take. The exact tubes take up
//! to a few hundred kilobytes a light (pieces, grid, tables), built with
//! the raster; a drawing with panels between its tube items keeps those
//! panels' pictures besides, each as large as what it covers, all within
//! `CRISP_PANELS_BYTES`.
//! The key and the room read the level-0 picture once each per build.

use makepad_draw::*;
use makepad_svg as svg;
use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::{Arc, Mutex};

use super::params::{LightParams, PatternParams, PatternSource, SpaceParams, PATTERN_COUNT_MAX, SPACE_NEAR_M, SPACE_SIZE_RANGE_M};
use super::room::Housing;
use super::shape::CullCone;
use super::shaped::{box_x_sign, ShapeSample};
use super::space::{space_frame, space_half, to_3d};
use super::studio::{project, LightFrame};
use super::{Blend, CASTER_REFERENCE_WIDTH};
use crate::sky::luminance;

/// A generated pattern's border round its cells, in tube widths: room for a
/// neon tube's glow, which the face's edge would otherwise cut.
const MARGIN_TUBES: f32 = 2.0;
/// The longest side a pattern's raster takes, texels.
pub const RASTER_MAX_SIDE: usize = 8192;
/// The most texels a pattern's raster takes (16 bytes each, a third more
/// for its mip levels: about 85 MB at the bound).
pub const RASTER_MAX_TEXELS: usize = 1 << 22;
/// The fewest texels along a raster's longer side.
const RASTER_MIN_SIDE: usize = 16;
/// The bytes of rasters no map holds any longer that are kept across builds,
/// the most recently used first (`kept_raster`; the rasters a map being
/// built or shown holds are kept besides, at no cost).
const RASTERS_SPARE_BYTES: usize = 128 << 20;
/// The most nodes and flattened points the walk over a drawing visits
/// (`Walk`): far past a real drawing (a 50,000-edge panel is a few hundred
/// thousand), a bound on what `<use>` fan-out repeats.
const WALK_BUDGET: usize = 1 << 23;
/// The widest tube, in face diagonals each side of its centre line.
const HALF_MAX_DIAGONALS: f32 = 4.0;
/// The finest dash pattern drawn (one dash and one gap, raster texels).
const DASH_MIN_PERIOD: f32 = 1.0;
/// How far a piece of a curve wholly off the face may stray from the true
/// one, as a share of its control points' extent: coarse, its length within
/// about 0.1 percent (what a dash pattern and mounts running on are spaced by).
const FLATTEN_OFF_FACE: f32 = 1.0 / 64.0;
/// The most taps along each side of a texel's footprint (`PatternRaster::filtered`).
const MAX_TAPS: usize = 4;
/// A footprint whose shorter side is at most this many raster texels draws
/// its tubes exactly (`Crisp`), one up to twice that blends the exact tubes
/// into the raster's levels; wider ones read the levels alone.
const CRISP_SHORT: f32 = 4.0;
/// The same for the footprint's longer side (a face seen at a slant).
const CRISP_LONG: f32 = 32.0;
/// The most cells of the grid a drawing's exact tubes and panel pictures are found through.
const CRISP_CELLS: usize = 1 << 16;
/// The most bytes a light's panel pictures take besides its raster (`Crisp`:
/// each run of panels between tube items with its levels, over what it
/// covers). Past it the drawing's first items are drawn into one picture as
/// the raster draws them, logged; all of them fit (a whole face's picture
/// is held to `RASTER_MAX_TEXELS`).
const CRISP_PANELS_BYTES: usize = 128 << 20;
/// Entries a half width in a tube's cross-section tables (`TubeSums`).
const TUBE_SUMS_PER_HALF: usize = 256;
/// Sub-scanlines a row when a fill is covered.
const FILL_SUBROWS: usize = 4;
/// How far a flattened curve may stray from the true one, raster texels.
const FLATTEN_TOLERANCE: f32 = 0.1;
/// A bend sharper than this between two pieces of a tube is a corner (its connector), degrees.
const CORNER_TURN_DEG: f32 = 25.0;
/// The neon look's glow just outside its glass, and how fast it falls (in half widths).
const NEON_GLOW: f32 = 0.3;
const NEON_GLOW_FALL: f32 = 1.0;
/// How uneven the neon look is along its length (its share either way), and
/// its period, in tube widths.
const NEON_UNEVEN: f32 = 0.12;
const NEON_UNEVEN_PERIOD: f32 = 6.0;
/// What an LED tube's end cap keeps of its light, and the cap's length in tube widths.
const LED_CAP_LEFT: f32 = 0.1;
const LED_CAP_TUBES: f32 = 0.75;
/// A mount's and a connector's reach along the tube, in tube widths each way.
const MOUNT_TUBES: f32 = 1.0;

// ---- The pattern's sizes and its generators ----

/// The bars' greatest tilt off upright either way, degrees.
pub const BAR_TILT_MAX_DEG: f32 = 75.0;
/// The chevrons' arms' slope off the row, degrees: 45 plus the angle, held to this.
pub const CHEVRON_SLOPE_RANGE_DEG: (f32, f32) = (10.0, 80.0);

/// The angle a generator draws, degrees: `PatternParams::angle_deg` as
/// clamp() keeps it (wrapped), then held as the generator reads it: the
/// bars' tilt to `BAR_TILT_MAX_DEG` either way, the chevrons' arms' turn off
/// 45 degrees to `CHEVRON_SLOPE_RANGE_DEG` less 45 (-35..=35); the others
/// do not read it.
pub fn drawn_angle_deg(p: &PatternParams) -> f32 {
    let p = p.clamped();
    match p.source() {
        PatternSource::Bars => p.angle_deg.max(-BAR_TILT_MAX_DEG).min(BAR_TILT_MAX_DEG),
        PatternSource::Chevrons => (45.0 + p.angle_deg).max(CHEVRON_SLOPE_RANGE_DEG.0).min(CHEVRON_SLOPE_RANGE_DEG.1) - 45.0,
        _ => p.angle_deg,
    }
}

/// The bars' tilt off upright, radians.
fn bar_tilt(p: &PatternParams) -> f32 {
    p.angle_deg.max(-BAR_TILT_MAX_DEG).min(BAR_TILT_MAX_DEG).to_radians()
}

/// The chevrons' arms' slope off the row, radians.
fn chevron_slope(p: &PatternParams) -> f32 {
    (45.0 + p.angle_deg).max(CHEVRON_SLOPE_RANGE_DEG.0).min(CHEVRON_SLOPE_RANGE_DEG.1).to_radians()
}

/// A hexagon honeycomb's steps for hexagons of corner radius `r` and the
/// gap `g` between them (flat-topped): the column step across and the row
/// step down (odd columns sit half a row step lower).
fn hex_steps(r: f32, g: f32) -> (f32, f32) {
    let across = 3f32.sqrt() * r + g;
    (across * 0.5 * 3f32.sqrt(), across)
}

/// The size of a generator's cells together, metres (its border not included).
pub fn lattice_m(p: &PatternParams) -> [f32; 2] {
    let (c, t, g) = (p.cell_m, p.tube_m, p.gap_m);
    let (cols, rows) = (p.columns.max(1) as f32, p.rows.max(1) as f32);
    match p.source() {
        PatternSource::Hexagons => {
            let r = 0.5 * c;
            let (hx, vy) = hex_steps(r, g);
            let shift = if p.columns > 1 { 0.5 * vy } else { 0.0 };
            [2.0 * r + (cols - 1.0) * hx, 3f32.sqrt() * r + (rows - 1.0) * vy + shift]
        }
        PatternSource::Bars => {
            let a = bar_tilt(p);
            [(cols - 1.0) * (t + g) + c * a.sin().abs(), (rows - 1.0) * (c * a.cos() + g) + c * a.cos()]
        }
        PatternSource::Chevrons => {
            let h = 0.5 * c * chevron_slope(p).tan();
            [cols * c + (cols - 1.0) * g, rows * h + (rows - 1.0) * g]
        }
        PatternSource::Triangles => {
            let h = 0.5 * 3f32.sqrt() * c;
            [c + (cols - 1.0) * (0.5 * c + g), rows * h + (rows - 1.0) * g]
        }
        PatternSource::Frames | PatternSource::Lines | PatternSource::Svg => [cols * c, rows * c],
    }
}

/// The pattern's natural size, metres (width, height): a generator's cells
/// with their border (`MARGIN_TUBES`), or a drawn SVG's root width and
/// height when both are in real units (mm, cm, m, in, pt). None for a drawn
/// SVG without them: its face keeps the size it has.
pub fn natural_size_m(p: &PatternParams) -> Option<[f32; 2]> {
    let p = p.clamped();
    if p.source() == PatternSource::Svg {
        return svg_real_size_m(&p.svg);
    }
    let [w, h] = lattice_m(&p);
    let m = 2.0 * MARGIN_TUBES * p.tube_m;
    Some([w + m, h + m])
}

/// The pattern's aspect, height over width: a generator's natural size's,
/// a drawn SVG's viewBox's (else its root width and height's). None for an
/// empty SVG or one without either: its face keeps its own aspect and the
/// drawing's bounds fill it.
pub fn pattern_aspect(p: &PatternParams) -> Option<f32> {
    let p = p.clamped();
    let [w, h] = match p.source() {
        PatternSource::Svg => svg_view_size(&p.svg)?,
        _ => natural_size_m(&p)?,
    };
    let aspect = h / w;
    (aspect.is_finite() && aspect > 0.0).then_some(aspect)
}

/// clamp()'s rule for a pattern light (`LightParams::clamp_fields`): one
/// without a face gets one where its 2D placement shows it (`space::to_3d`,
/// its beam diffuse) at the pattern's natural width; then the face's height
/// follows its width at the pattern's aspect (both held to
/// `SPACE_SIZE_RANGE_M`; at the range's end the width gives way). The field
/// it changed on a face that was there, or None. Idempotent: a second call
/// changes nothing.
pub fn hold_face(light: &mut LightParams) -> Option<&'static str> {
    let pattern = light.pattern.clone().unwrap_or_default();
    let (lo, hi) = SPACE_SIZE_RANGE_M;
    if light.space.is_none() {
        to_3d(light, None);
        if let (Some(space), Some([w, _])) = (light.space.as_mut(), natural_size_m(&pattern)) {
            space.width_m = w.max(lo).min(hi);
        }
    }
    let aspect = pattern_aspect(&pattern)?;
    let space = light.space.as_mut()?;
    let mut w = space.width_m;
    let tall = w * aspect;
    if tall > hi {
        w = hi / aspect;
    } else if tall < lo {
        w = lo / aspect;
    }
    let w = w.max(lo).min(hi);
    let h = (w * aspect).max(lo).min(hi);
    let changed = w != space.width_m || h != space.height_m;
    space.width_m = w;
    space.height_m = h;
    changed.then_some("height_m")
}

/// Sets a pattern light's face to the pattern's natural size (its width,
/// the height following at the pattern's aspect): what a host does when the
/// pattern changes (a generator's numbers, an SVG loaded with real units).
/// A pattern without a natural size keeps its face's width. Places the light
/// in 3D first when it is not.
pub fn fit_face(light: &mut LightParams) {
    let pattern = light.pattern.clone().unwrap_or_default();
    if light.space.is_none() {
        to_3d(light, None);
    }
    if let (Some(space), Some([w, _])) = (light.space.as_mut(), natural_size_m(&pattern)) {
        let (lo, hi) = SPACE_SIZE_RANGE_M;
        space.width_m = w.max(lo).min(hi);
    }
    hold_face(light);
}

/// A housing's size round its tube, in tube widths: how far it reaches past
/// a straight open tube's ends (past its light, which ends there), beside
/// the tube each side, and how deep it is behind the face. A 0.18 m tube
/// sits in a body 0.31 m wide, its length plus 0.18 m, 0.11 m deep: a road
/// tunnel's or an underpass's luminaire.
const HOUSING_END_TUBES: f32 = 0.5;
const HOUSING_SIDE_TUBES: f32 = 0.35;
const HOUSING_DEPTH_TUBES: f32 = 0.6;

/// The housings of `pattern`'s tubes on the face `space` (`PatternParams::housing`;
/// none without it): one box round each line a generator lays, its front
/// face on the light's face and the rest behind it. A straight open line's
/// box lies along it, reaching HOUSING_END_TUBES past its ends; any other
/// line's (a hexagon, a chevron, a frame) is its box on the face's own axes
/// grown by half a tube. Each is as wide as the tube and HOUSING_SIDE_TUBES
/// either side and HOUSING_DEPTH_TUBES deep. A drawn SVG's tubes have none
/// yet. Per build, a box per line.
pub fn housings(pattern: &PatternParams, space: &SpaceParams) -> Vec<Housing> {
    let p = pattern.clamped();
    let Some(albedo) = p.housing else { return Vec::new() };
    if p.source() == PatternSource::Svg {
        return Vec::new();
    }
    let Some([w, h]) = natural_size_m(&p) else { return Vec::new() };
    let k = space.width_m / w.max(1.0e-6);
    let frame = space_frame(space);
    let middle = frame.center * space.distance().max(SPACE_NEAR_M);
    let front = frame.front();
    let sx = box_x_sign();
    let tube = p.tube_m * k;
    let depth = HOUSING_DEPTH_TUBES * tube;
    // A point of the generator's drawing (metres, y down) on the face, as `PatternLight::for_each_texel` lays it.
    let on_face = |q: [f32; 2]| middle + frame.right * (sx * (q[0] - 0.5 * w) * k) + frame.up * ((0.5 * h - q[1]) * k);
    let mut out = Vec::new();
    for (pts, closed) in generator_lines(&p) {
        let points: Vec<Vec3f> = pts.iter().map(|&q| on_face(q)).collect();
        let straight = !closed && points.len() == 2 && (points[1] - points[0]).length() > 1.0e-6;
        let (along, grow) = if straight {
            ((points[1] - points[0]).normalize(), HOUSING_END_TUBES * tube)
        } else {
            (frame.right, (0.5 + HOUSING_SIDE_TUBES) * tube)
        };
        let across = Vec3f::cross(front, along).normalize();
        let (mut lo, mut hi) = ([f32::MAX; 2], [f32::MIN; 2]);
        for q in &points {
            let r = *q - middle;
            for (i, axis) in [along, across].iter().enumerate() {
                lo[i] = lo[i].min(r.dot(*axis));
                hi[i] = hi[i].max(r.dot(*axis));
            }
        }
        let centre = middle + along * (0.5 * (lo[0] + hi[0])) + across * (0.5 * (lo[1] + hi[1])) - front * (0.5 * depth);
        let half = vec3f(0.5 * (hi[0] - lo[0]) + grow, 0.5 * (hi[1] - lo[1]) + (0.5 + HOUSING_SIDE_TUBES) * tube, 0.5 * depth);
        out.push(Housing { centre, axes: [along, across, front], half, albedo });
    }
    out
}

/// The SVG text the pattern draws: a generator's (`generate_svg`) or the
/// document's own. None for an empty drawn one.
pub fn pattern_svg(p: &PatternParams) -> Option<String> {
    let p = p.clamped();
    match p.source() {
        PatternSource::Svg => (!p.svg.trim().is_empty()).then(|| p.svg.to_string()),
        _ => Some(generate_svg(&p)),
    }
}

/// The centre lines a generator lays, metres, y down, inside its natural
/// size (its border included), each with whether it is closed.
fn generator_lines(p: &PatternParams) -> Vec<(Vec<[f32; 2]>, bool)> {
    let (c, t, g) = (p.cell_m, p.tube_m, p.gap_m);
    let (cols, rows) = (p.columns.max(1) as usize, p.rows.max(1) as usize);
    let m = MARGIN_TUBES * t;
    let at = |x: f32, y: f32| [x + m, y + m];
    let mut out = Vec::new();
    match p.source() {
        PatternSource::Hexagons => {
            let r = 0.5 * c;
            let (hx, vy) = hex_steps(r, g);
            for i in 0..cols {
                for j in 0..rows {
                    let cx = r + i as f32 * hx;
                    let cy = 0.5 * 3f32.sqrt() * r + j as f32 * vy + if i % 2 == 1 { 0.5 * vy } else { 0.0 };
                    let pts = (0..6)
                        .map(|k| {
                            let a = (k as f32 * 60.0).to_radians();
                            at(cx + r * a.cos(), cy + r * a.sin())
                        })
                        .collect();
                    out.push((pts, true));
                }
            }
        }
        PatternSource::Bars => {
            let a = bar_tilt(p);
            let (s, co) = a.sin_cos();
            for i in 0..cols {
                for j in 0..rows {
                    let cx = 0.5 * c * s.abs() + i as f32 * (t + g);
                    let cy = 0.5 * c * co + j as f32 * (c * co + g);
                    out.push((vec![at(cx + 0.5 * c * s, cy - 0.5 * c * co), at(cx - 0.5 * c * s, cy + 0.5 * c * co)], false));
                }
            }
        }
        PatternSource::Chevrons => {
            let h = 0.5 * c * chevron_slope(p).tan();
            for i in 0..cols {
                for j in 0..rows {
                    let (x0, y0) = (i as f32 * (c + g), j as f32 * (h + g));
                    out.push((vec![at(x0, y0 + h), at(x0 + 0.5 * c, y0), at(x0 + c, y0 + h)], false));
                }
            }
        }
        PatternSource::Triangles => {
            let h = 0.5 * 3f32.sqrt() * c;
            for j in 0..rows {
                for k in 0..cols {
                    let x = 0.5 * c + k as f32 * (0.5 * c + g);
                    let y0 = j as f32 * (h + g);
                    let pts = if (k + j) % 2 == 0 {
                        vec![at(x - 0.5 * c, y0 + h), at(x, y0), at(x + 0.5 * c, y0 + h)]
                    } else {
                        vec![at(x - 0.5 * c, y0), at(x + 0.5 * c, y0), at(x, y0 + h)]
                    };
                    out.push((pts, true));
                }
            }
        }
        PatternSource::Frames => {
            let (w, h) = (cols as f32 * c, rows as f32 * c);
            // Nested inward every gap (no closer than two tubes); one frame without a gap. At most
            // PATTERN_COUNT_MAX of them, as a generator's rows: a closer gap spreads them wider.
            let mut step = if g > 0.0 { g.max(2.0 * t) } else { f32::INFINITY };
            let fit = (0.5 * (w.min(h) - t) / step).ceil();
            if step.is_finite() && fit > PATTERN_COUNT_MAX as f32 {
                let wider = 0.5 * (w.min(h) - t) / PATTERN_COUNT_MAX as f32;
                log!(
                    "hdri pattern: frames every {:.4} m would nest {fit:.0} deep; nested {PATTERN_COUNT_MAX} deep, every {wider:.4} m",
                    step
                );
                step = wider;
            }
            let mut k = 0usize;
            loop {
                // (the outer frame at 0: without a gap the step is infinite, and 0 times it is not a number)
                let e = if k == 0 { 0.0 } else { k as f32 * step };
                if k > 0 && !(w - 2.0 * e > t && h - 2.0 * e > t) {
                    break;
                }
                out.push((vec![at(e, e), at(w - e, e), at(w - e, h - e), at(e, h - e)], true));
                k += 1;
                if !step.is_finite() {
                    break;
                }
            }
        }
        PatternSource::Lines => {
            let (w, h) = (cols as f32 * c, rows as f32 * c);
            if g > 0.0 && g < c {
                // Each side of each square its own tube, a gap at every crossing.
                for i in 0..=cols {
                    for j in 0..rows {
                        let x = i as f32 * c;
                        out.push((vec![at(x, j as f32 * c + 0.5 * g), at(x, (j + 1) as f32 * c - 0.5 * g)], false));
                    }
                }
                for j in 0..=rows {
                    for i in 0..cols {
                        let y = j as f32 * c;
                        out.push((vec![at(i as f32 * c + 0.5 * g, y), at((i + 1) as f32 * c - 0.5 * g, y)], false));
                    }
                }
            } else {
                for i in 0..=cols {
                    let x = i as f32 * c;
                    out.push((vec![at(x, 0.0), at(x, h)], false));
                }
                for j in 0..=rows {
                    let y = j as f32 * c;
                    out.push((vec![at(0.0, y), at(w, y)], false));
                }
            }
        }
        PatternSource::Svg => {}
    }
    out
}

/// The SVG a generator writes (`PatternParams::source` other than "svg"):
/// one white path of round-capped, round-joined tubes `tube_m` wide, in
/// millimetres, its root width and height its natural size in mm
/// (`natural_size_m`), so it can be saved and loaded again as it is.
pub fn generate_svg(p: &PatternParams) -> String {
    let p = p.clamped();
    let [w, h] = natural_size_m(&p).unwrap_or([1.0, 1.0]);
    let mm = |v: f32| v * 1000.0;
    let mut d = String::new();
    for (pts, closed) in generator_lines(&p) {
        for (k, pt) in pts.iter().enumerate() {
            d.push_str(&format!("{}{:.2} {:.2} ", if k == 0 { "M" } else { "L" }, mm(pt[0]), mm(pt[1])));
        }
        if closed {
            d.push_str("Z ");
        }
    }
    format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{w:.2}mm\" height=\"{h:.2}mm\" viewBox=\"0 0 {w:.2} {h:.2}\">\
<path d=\"{d}\" fill=\"none\" stroke=\"#ffffff\" stroke-width=\"{t:.2}\" stroke-linecap=\"round\" stroke-linejoin=\"round\"/></svg>",
        w = mm(w),
        h = mm(h),
        d = d.trim_end(),
        t = mm(p.tube_m),
    )
}

// ---- The root tag, read without parsing the document ----

/// The attributes of the SVG's root `<svg>` tag, as written.
fn root_tag(text: &str) -> Option<&str> {
    let bytes = text.as_bytes();
    let mut i = 0;
    while i + 4 <= bytes.len() {
        if bytes[i] == b'<' && bytes[i + 1..i + 4].eq_ignore_ascii_case(b"svg") {
            let next = bytes.get(i + 4).copied().unwrap_or(b'>');
            if next.is_ascii_whitespace() || next == b'>' || next == b'/' {
                let start = i + 4;
                let mut quote = None;
                for (k, &b) in bytes[start..].iter().enumerate() {
                    match quote {
                        Some(q) if b == q => quote = None,
                        Some(_) => {}
                        None if b == b'"' || b == b'\'' => quote = Some(b),
                        None if b == b'>' => return text.get(start..start + k),
                        None => {}
                    }
                }
                return None;
            }
        }
        i += 1;
    }
    None
}

/// Attribute `name` of a tag's attribute text (case-insensitive), its value as written.
fn attr<'a>(tag: &'a str, name: &str) -> Option<&'a str> {
    let b = tag.as_bytes();
    let mut i = 0;
    while i < b.len() {
        while i < b.len() && (b[i].is_ascii_whitespace() || b[i] == b'/') {
            i += 1;
        }
        let start = i;
        while i < b.len() && !b[i].is_ascii_whitespace() && b[i] != b'=' {
            i += 1;
        }
        let key = &tag[start..i];
        while i < b.len() && b[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= b.len() || b[i] != b'=' {
            if key.is_empty() {
                i += 1;
            }
            continue;
        }
        i += 1;
        while i < b.len() && b[i].is_ascii_whitespace() {
            i += 1;
        }
        let value = if i < b.len() && (b[i] == b'"' || b[i] == b'\'') {
            let q = b[i];
            let s = i + 1;
            let mut e = s;
            while e < b.len() && b[e] != q {
                e += 1;
            }
            i = e + 1;
            &tag[s..e.min(b.len())]
        } else {
            let s = i;
            while i < b.len() && !b[i].is_ascii_whitespace() {
                i += 1;
            }
            &tag[s..i]
        };
        if key.eq_ignore_ascii_case(name) {
            return Some(value);
        }
    }
    None
}

/// A length in real units (mm, cm, m, in, pt) as metres; None for px, a bare number or a percentage.
fn real_length_m(s: &str) -> Option<f32> {
    let s = s.trim();
    for (suffix, metres) in [("mm", 0.001f32), ("cm", 0.01), ("in", 0.0254), ("pt", 0.0254 / 72.0), ("m", 1.0)] {
        if let Some(num) = s.strip_suffix(suffix) {
            let v = num.trim().parse::<f32>().ok()? * metres;
            return (v.is_finite() && v > 0.0).then_some(v);
        }
    }
    None
}

/// A drawn SVG's root width and height in metres, when both are in real units.
fn svg_real_size_m(text: &str) -> Option<[f32; 2]> {
    let tag = root_tag(text)?;
    Some([real_length_m(attr(tag, "width")?)?, real_length_m(attr(tag, "height")?)?])
}

/// The size of a drawn SVG's viewBox, else of its root width and height.
fn svg_view_size(text: &str) -> Option<[f32; 2]> {
    let tag = root_tag(text)?;
    let ok = |w: f32, h: f32| (w.is_finite() && h.is_finite() && w > 0.0 && h > 0.0).then_some([w, h]);
    if let Some(vb) = attr(tag, "viewBox").and_then(svg::units::parse_viewbox) {
        if let Some(size) = ok(vb.width, vb.height) {
            return Some(size);
        }
    }
    let length = |name: &str| {
        let v = attr(tag, name)?;
        real_length_m(v).or_else(|| svg::units::parse_length(v))
    };
    ok(length("width")?, length("height")?)
}

/// What a pattern SVG may hold that a light pattern does not draw, as its
/// name in the log: the elements and attributes the walk skips.
fn skipped_features(text: &str) -> Vec<&'static str> {
    let lower = text.to_ascii_lowercase();
    let mut out = Vec::new();
    for (needles, name) in [
        (&["<text", "<tspan", "<textpath"][..], "text"),
        (&["<filter", "filter="][..], "filters"),
        (&["<mask", "mask="][..], "masks"),
        (&["<clippath", "clip-path"][..], "clip paths"),
        (&["<script"][..], "scripts"),
        (&["<image", "<foreignobject"][..], "images"),
        (&["<pattern"][..], "pattern fills"),
        (&["<style"][..], "style sheets"),
        (&["<animate", "<set "][..], "animation"),
        (&["href=\"http", "href='http", "href=\"file", "href='file", "url(http", "url(file"][..], "external files"),
    ] {
        if needles.iter().any(|n| lower.contains(n)) {
            out.push(name);
        }
    }
    out
}

// ---- The SVG as tubes and panels ----

/// What one tube or panel is coloured with: a linear colour (white is the
/// light's own), or a gradient in raster texels (its stops in sRGB).
#[derive(Clone, Debug)]
enum Paint {
    Solid([f32; 3], f32),
    Gradient(svg::VectorPaint),
}

impl Paint {
    /// The linear colour and alpha at raster point (x, y).
    fn at(&self, x: f32, y: f32) -> ([f32; 3], f32) {
        match self {
            Paint::Solid(c, a) => (*c, *a),
            Paint::Gradient(g) => {
                let [r, gr, b, a] = g.color_at(x, y);
                if !(a > 0.0) {
                    return ([0.0; 3], 0.0);
                }
                ([srgb_to_linear(r / a), srgb_to_linear(gr / a), srgb_to_linear(b / a)], a.min(1.0))
            }
        }
    }
}

fn srgb_to_linear(c: f32) -> f32 {
    let c = c.max(0.0).min(1.0);
    if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
}

/// One stroke or fill of the pattern, in raster texels.
#[derive(Clone, Debug)]
enum Item {
    /// A tube: its centre lines (closed or not) and half its width.
    Stroke { lines: Vec<(Vec<[f32; 2]>, bool)>, half: f32, paint: Paint, opacity: f32 },
    /// A panel: its contours and its fill rule.
    Fill { contours: Vec<Vec<[f32; 2]>>, even_odd: bool, paint: Paint, opacity: f32 },
}

/// A node's local bounding box (for a gradient in objectBoundingBox units).
fn path_bbox(path: &svg::VectorPath) -> [f32; 4] {
    let mut b = [f32::MAX, f32::MAX, f32::MIN, f32::MIN];
    let mut add = |x: f32, y: f32| {
        b[0] = b[0].min(x);
        b[1] = b[1].min(y);
        b[2] = b[2].max(x);
        b[3] = b[3].max(y);
    };
    for cmd in &path.cmds {
        match *cmd {
            svg::PathCmd::MoveTo(x, y) | svg::PathCmd::LineTo(x, y) => add(x, y),
            svg::PathCmd::BezierTo(a, b2, c, d, x, y) => {
                add(a, b2);
                add(c, d);
                add(x, y);
            }
            _ => {}
        }
    }
    if b[0] > b[2] {
        [0.0; 4]
    } else {
        b
    }
}

/// A gradient as a paint in the coordinates `xf` takes the node to, as
/// makepad's own SVG drawing maps it.
fn gradient_paint(grad: &svg::SvgGradient, xf: &svg::Transform2d, bbox: [f32; 4]) -> svg::VectorPaint {
    if grad.stops.is_empty() {
        return svg::VectorPaint::solid(0.0, 0.0, 0.0, 1.0);
    }
    let map = |u: f32, v: f32| (bbox[0] + u * (bbox[2] - bbox[0]), bbox[1] + v * (bbox[3] - bbox[1]));
    let gxf = grad.transform.then(xf);
    match grad.kind {
        svg::GradientKind::Linear => {
            let (x1, y1, x2, y2) = match grad.units {
                svg::GradientUnits::ObjectBoundingBox => {
                    let (a, b) = map(grad.x1, grad.y1);
                    let (c, d) = map(grad.x2, grad.y2);
                    (a, b, c, d)
                }
                svg::GradientUnits::UserSpaceOnUse => (grad.x1, grad.y1, grad.x2, grad.y2),
            };
            let (x0, y0) = gxf.apply(x1, y1);
            let (x1, y1) = gxf.apply(x2, y2);
            svg::VectorPaint::LinearGradient { x0, y0, x1, y1, stops: grad.stops.clone() }
        }
        svg::GradientKind::Radial => {
            let (cx, cy, r) = match grad.units {
                svg::GradientUnits::ObjectBoundingBox => {
                    let (a, b) = map(grad.cx, grad.cy);
                    (a, b, grad.r * 0.5 * ((bbox[2] - bbox[0]) + (bbox[3] - bbox[1])))
                }
                svg::GradientUnits::UserSpaceOnUse => (grad.cx, grad.cy, grad.r),
            };
            let (cx, cy) = gxf.apply(cx, cy);
            let sx = (gxf.a * gxf.a + gxf.b * gxf.b).sqrt();
            let sy = (gxf.c * gxf.c + gxf.d * gxf.d).sqrt();
            svg::VectorPaint::RadialGradient { cx, cy, rx: r * sx, ry: r * sy, stops: grad.stops.clone() }
        }
    }
}

/// An SVG paint as a light's paint and its alpha; None for no paint.
fn paint_of(paint: &svg::SvgPaint, style: &svg::SvgStyle, defs: &svg::SvgDefs, xf: &svg::Transform2d, bbox: [f32; 4]) -> Option<Paint> {
    let solid = |(r, g, b, a): (f32, f32, f32, f32)| Paint::Solid([srgb_to_linear(r), srgb_to_linear(g), srgb_to_linear(b)], a.max(0.0).min(1.0));
    match paint {
        svg::SvgPaint::None => None,
        svg::SvgPaint::Color(r, g, b, a) => Some(solid((*r, *g, *b, *a))),
        svg::SvgPaint::CurrentColor => Some(solid(style.color)),
        svg::SvgPaint::GradientRef(id) => Some(match defs.gradients.get(id) {
            Some(grad) => Paint::Gradient(gradient_paint(grad, xf, bbox)),
            None => Paint::Solid([0.0; 3], 1.0),
        }),
    }
}

/// Flattens `path`, taken through `xf`, into polylines (raster texels): a
/// curve to `FLATTEN_TOLERANCE`, but a piece of one wholly outside `clip`
/// ([x0, y0, x1, y1], the face and the reach round it) coarsely
/// (`FLATTEN_OFF_FACE`), as it draws nothing there.
fn flatten(path: &svg::VectorPath, xf: &svg::Transform2d, clip: [f32; 4]) -> Vec<(Vec<[f32; 2]>, bool)> {
    let mut out: Vec<(Vec<[f32; 2]>, bool)> = Vec::new();
    let tp = |x: f32, y: f32| {
        let (x, y) = xf.apply(x, y);
        [x, y]
    };
    let push = |out: &mut Vec<(Vec<[f32; 2]>, bool)>, p: [f32; 2]| {
        if let Some((line, _)) = out.last_mut() {
            if line.last().is_none_or(|q| (q[0] - p[0]).abs() + (q[1] - p[1]).abs() > 1.0e-4) {
                line.push(p);
            }
        }
    };
    for cmd in &path.cmds {
        match *cmd {
            svg::PathCmd::MoveTo(x, y) => out.push((vec![tp(x, y)], false)),
            svg::PathCmd::LineTo(x, y) => {
                if out.is_empty() {
                    out.push((Vec::new(), false));
                }
                push(&mut out, tp(x, y));
            }
            svg::PathCmd::BezierTo(x1, y1, x2, y2, x, y) => {
                let Some(p0) = out.last().and_then(|(l, _)| l.last().copied()) else { continue };
                let mut pts = Vec::new();
                bezier([p0, tp(x1, y1), tp(x2, y2), tp(x, y)], 0, Some(clip), FLATTEN_TOLERANCE, &mut pts);
                for p in pts {
                    push(&mut out, p);
                }
            }
            svg::PathCmd::Close => {
                if let Some((line, closed)) = out.last_mut() {
                    *closed = true;
                    let first = line.first().copied();
                    if let Some(p) = first {
                        out.push((vec![p], false));
                    }
                }
            }
            svg::PathCmd::Winding(_) => {}
        }
    }
    out.retain(|(line, _)| line.len() >= 2);
    for (line, closed) in &mut out {
        // A closed line that ends on its start: the closing piece is implied.
        if *closed && line.len() > 2 {
            let (a, b) = (line[0], line[line.len() - 1]);
            if (a[0] - b[0]).abs() + (a[1] - b[1]).abs() < 1.0e-4 {
                line.pop();
            }
        }
    }
    out
}

/// A cubic's points after its first, cut until each piece strays less than
/// `tolerance`; a piece whose control points all lie outside `clip` (so the
/// curve does) is cut to `FLATTEN_OFF_FACE` of its extent from there on.
fn bezier([p0, p1, p2, p3]: [[f32; 2]; 4], level: u32, clip: Option<[f32; 4]>, tolerance: f32, out: &mut Vec<[f32; 2]>) {
    let (mut clip, mut tolerance) = (clip, tolerance);
    if let Some(c) = clip {
        let xs = [p0[0], p1[0], p2[0], p3[0]];
        let ys = [p0[1], p1[1], p2[1], p3[1]];
        let (x0, x1) = (xs.iter().copied().fold(f32::MAX, f32::min), xs.iter().copied().fold(f32::MIN, f32::max));
        let (y0, y1) = (ys.iter().copied().fold(f32::MAX, f32::min), ys.iter().copied().fold(f32::MIN, f32::max));
        if x1 < c[0] || y1 < c[1] || x0 > c[2] || y0 > c[3] {
            tolerance = tolerance.max(FLATTEN_OFF_FACE * (x1 - x0).max(y1 - y0));
            clip = None;
        }
    }
    let (dx, dy) = (p3[0] - p0[0], p3[1] - p0[1]);
    let len = (dx * dx + dy * dy).sqrt();
    let off = |p: [f32; 2]| {
        if len > 1.0e-6 {
            ((p[0] - p0[0]) * dy - (p[1] - p0[1]) * dx).abs() / len
        } else {
            ((p[0] - p0[0]).powi(2) + (p[1] - p0[1]).powi(2)).sqrt()
        }
    };
    if level >= 16 || !(off(p1).max(off(p2)) > tolerance) {
        out.push(p3);
        return;
    }
    let mid = |a: [f32; 2], b: [f32; 2]| [0.5 * (a[0] + b[0]), 0.5 * (a[1] + b[1])];
    let (p01, p12, p23) = (mid(p0, p1), mid(p1, p2), mid(p2, p3));
    let (p012, p123) = (mid(p01, p12), mid(p12, p23));
    let m = mid(p012, p123);
    bezier([p0, p01, p012, m], level + 1, clip, tolerance, out);
    bezier([m, p123, p23, p3], level + 1, clip, tolerance, out);
}

/// The part of the piece a -> b inside `clip` ([x0, y0, x1, y1]), as
/// distances along it from `a`; None when it misses.
fn clip_span(a: [f32; 2], b: [f32; 2], clip: [f32; 4]) -> Option<(f64, f64)> {
    let (ax, ay) = (a[0] as f64, a[1] as f64);
    let (dx, dy) = (b[0] as f64 - ax, b[1] as f64 - ay);
    let (mut t0, mut t1) = (0.0f64, 1.0f64);
    for (p, q) in [(-dx, ax - clip[0] as f64), (dx, clip[2] as f64 - ax), (-dy, ay - clip[1] as f64), (dy, clip[3] as f64 - ay)] {
        if p == 0.0 {
            if q < 0.0 {
                return None;
            }
        } else {
            let r = q / p;
            if p < 0.0 {
                if r > t1 {
                    return None;
                }
                t0 = t0.max(r);
            } else {
                if r < t0 {
                    return None;
                }
                t1 = t1.min(r);
            }
        }
    }
    let len = (dx * dx + dy * dy).sqrt();
    (t0 <= t1 && len.is_finite()).then_some((t0 * len, t1 * len))
}

/// Where a dash pattern is along a line (`dashed`): in element `k` (even
/// ones dashes, odd ones gaps), `left` texels of it still to go.
struct Pen<'a> {
    pattern: &'a [f64],
    period: f64,
    k: usize,
    left: f64,
}

impl Pen<'_> {
    fn on(&self) -> bool {
        self.k % 2 == 0
    }

    fn next(&mut self) {
        self.k = (self.k + 1) % self.pattern.len();
        self.left = self.pattern[self.k];
    }

    /// From the start of element `k`, `into` texels on (less than a period).
    fn land(&mut self, mut into: f64) {
        for _ in 0..2 * self.pattern.len() {
            if into < self.pattern[self.k] {
                break;
            }
            into -= self.pattern[self.k];
            self.k = (self.k + 1) % self.pattern.len();
        }
        self.left = (self.pattern[self.k] - into).max(0.0);
    }

    /// Carries the pattern from `from` to `to` along a piece without making
    /// the dashes between (they are off the face), by length alone: a dash
    /// under way that ends there is ended where it ends, and one under way
    /// at `to` is started where it starts, so they are exactly as the whole
    /// pattern lays them. `at` is the point a distance along the piece.
    fn skip(&mut self, from: f64, to: f64, at: &dyn Fn(f64) -> [f32; 2], current: &mut Vec<[f32; 2]>, out: &mut Vec<(Vec<[f32; 2]>, bool)>) {
        let len = to - from;
        if !(len > 0.0) {
            return;
        }
        if len < self.left {
            self.left -= len;
            return;
        }
        if self.on() {
            current.push(at(from + self.left));
            out.push((std::mem::take(current), false));
        }
        let rest = (len - self.left) % self.period;
        self.next();
        self.land(rest);
        if self.on() {
            *current = vec![at(to - (self.pattern[self.k] - self.left))];
        }
    }
}

/// Cuts polylines into a dash pattern (lengths in texels, starting `offset`
/// in). Only the dashes inside `clip` ([x0, y0, x1, y1], the face and a
/// tube's reach round it) are made, one by one; the pattern is carried
/// across the rest by its length (`Pen::skip`), so the dashes made lie
/// exactly where the whole pattern puts them and a line far off the face
/// costs nothing per dash. Lengths along a piece in f64, so a piece far
/// longer than the face still moves on by a dash.
fn dashed(lines: Vec<(Vec<[f32; 2]>, bool)>, dashes: &[f32], offset: f32, clip: [f32; 4]) -> Vec<(Vec<[f32; 2]>, bool)> {
    let mut pattern: Vec<f64> = dashes.iter().map(|d| d.max(0.0) as f64).collect();
    if pattern.len() % 2 == 1 {
        pattern.extend_from_within(..);
    }
    let period: f64 = pattern.iter().sum();
    if !(period > 1.0e-3 && period.is_finite()) {
        return lines;
    }
    let offset = if offset.is_finite() { offset as f64 } else { 0.0 };
    let mut out = Vec::new();
    for (mut line, closed) in lines {
        if closed {
            line.push(line[0]);
        }
        // Where in the pattern the line starts.
        let mut pen = Pen { pattern: &pattern, period, k: 0, left: pattern[0] };
        pen.land(offset.rem_euclid(period));
        let mut current: Vec<[f32; 2]> = if pen.on() { vec![line[0]] } else { Vec::new() };
        for w in line.windows(2) {
            let (a, b) = (w[0], w[1]);
            let (dx, dy) = (b[0] as f64 - a[0] as f64, b[1] as f64 - a[1] as f64);
            let seg = (dx * dx + dy * dy).sqrt();
            let at = |t: f64| {
                let f = if seg > 0.0 { t / seg } else { 0.0 };
                [(a[0] as f64 + dx * f) as f32, (a[1] as f64 + dy * f) as f32]
            };
            // Up to the face, across it dash by dash, then past it.
            let (v0, v1) = clip_span(a, b, clip).unwrap_or((seg, seg));
            pen.skip(0.0, v0, &at, &mut current, &mut out);
            let mut t = v0;
            while v1 - t > pen.left {
                t += pen.left;
                let p = at(t);
                if pen.on() {
                    current.push(p);
                    out.push((std::mem::take(&mut current), false));
                } else {
                    current = vec![p];
                }
                pen.next();
            }
            pen.left -= (v1 - t).max(0.0);
            pen.skip(v1, seg, &at, &mut current, &mut out);
            if pen.on() {
                current.push(b);
            }
        }
        if pen.on() && current.len() >= 2 {
            out.push((current, false));
        }
    }
    out
}

/// What the walk over a drawing found and left out (`items`), for the log.
#[derive(Clone, Copy, Debug, Default)]
struct WalkNotes {
    /// A shape had a filter (drawn without it).
    filtered: bool,
    /// A `<use>` named a symbol it was inside of (skipped).
    looped: bool,
    /// Nodes not visited past `WALK_BUDGET`.
    left_out: usize,
    /// A dash pattern was finer than `DASH_MIN_PERIOD` (drawn at it).
    fine_dashes: bool,
}

/// The walk over the document (`items`).
struct Walk<'a> {
    defs: &'a svg::SvgDefs,
    items: Vec<Item>,
    notes: WalkNotes,
    /// The face, texels: what lies further off than a tube's reach draws nothing.
    face: [f32; 2],
    /// A tube's reach in half widths for the look (`reach_halves`).
    reach_halves: f32,
    /// The symbols being expanded, outermost first.
    open: Vec<&'a str>,
    /// Nodes and flattened points the walk may still visit.
    budget: usize,
}

impl<'a> Walk<'a> {
    /// The face grown by `margin` texels each way, [x0, y0, x1, y1].
    fn clip(&self, margin: f32) -> [f32; 4] {
        [-margin, -margin, self.face[0] + margin, self.face[1] + margin]
    }

    fn nodes(&mut self, nodes: &'a [svg::SvgNode], parent: &svg::Transform2d, depth: u32) {
        // Groups nested past any real drawing's depth.
        if depth > 64 {
            return;
        }
        for node in nodes {
            // Past the budget the rest is left out (a drawing that repeats itself through <use> fan-out).
            if self.budget == 0 {
                self.notes.left_out += 1;
                continue;
            }
            self.budget -= 1;
            match node {
                svg::SvgNode::Group(g) => self.nodes(&g.children, &g.transform.then(parent), depth + 1),
                svg::SvgNode::Use(u) => {
                    // A <use> that names a symbol it is inside of would repeat without end (SVG holds it an error).
                    if self.open.contains(&u.href.as_str()) {
                        self.notes.looped = true;
                        continue;
                    }
                    let defs = self.defs;
                    let Some(symbol) = defs.symbols.get(&u.href) else { continue };
                    let mut local = svg::Transform2d::translate(u.x, u.y).then(&u.transform);
                    if let Some(vb) = &symbol.viewbox {
                        let (sx, sy, tx, ty) = svg::units::viewbox_transform(vb, u.width.unwrap_or(vb.width), u.height.unwrap_or(vb.height));
                        local = svg::Transform2d { a: sx, c: 0.0, e: tx, b: 0.0, d: sy, f: ty }.then(&local);
                    }
                    self.open.push(u.href.as_str());
                    self.nodes(&symbol.children, &local.then(parent), depth + 1);
                    self.open.pop();
                }
                svg::SvgNode::Path(p) => self.shape(&p.path, &p.style, &p.transform.then(parent)),
                svg::SvgNode::Rect(r) => {
                    let mut path = svg::VectorPath::new();
                    let round = r.rx.max(r.ry);
                    if round > 0.0 {
                        path.rounded_rect(r.x, r.y, r.width, r.height, round);
                    } else {
                        path.rect(r.x, r.y, r.width, r.height);
                    }
                    self.shape(&path, &r.style, &r.transform.then(parent));
                }
                svg::SvgNode::Circle(c) => {
                    let mut path = svg::VectorPath::new();
                    path.circle(c.cx, c.cy, c.r);
                    self.shape(&path, &c.style, &c.transform.then(parent));
                }
                svg::SvgNode::Ellipse(e) => {
                    let mut path = svg::VectorPath::new();
                    path.ellipse(e.cx, e.cy, e.rx, e.ry);
                    self.shape(&path, &e.style, &e.transform.then(parent));
                }
                svg::SvgNode::Line(l) => {
                    let mut path = svg::VectorPath::new();
                    path.move_to(l.x1, l.y1);
                    path.line_to(l.x2, l.y2);
                    self.shape(&path, &l.style, &l.transform.then(parent));
                }
                svg::SvgNode::Polyline(p) => self.points(&p.points, false, &p.style, &p.transform.then(parent)),
                svg::SvgNode::Polygon(p) => self.points(&p.points, true, &p.style, &p.transform.then(parent)),
            }
        }
    }

    fn points(&mut self, points: &[(f32, f32)], closed: bool, style: &svg::SvgStyle, xf: &svg::Transform2d) {
        let mut path = svg::VectorPath::new();
        for (k, &(x, y)) in points.iter().enumerate() {
            if k == 0 {
                path.move_to(x, y);
            } else {
                path.line_to(x, y);
            }
        }
        if closed {
            path.close();
        }
        self.shape(&path, style, xf);
    }

    /// One shape: its fill (a panel), then its stroke (a tube) over it, as SVG paints them.
    fn shape(&mut self, path: &svg::VectorPath, style: &svg::SvgStyle, xf: &svg::Transform2d) {
        if style.filter.is_some() {
            self.notes.filtered = true;
        }
        let bbox = path_bbox(path);
        // The tube's half width, held to `HALF_MAX_DIAGONALS` face diagonals (a tube that wide along
        // the face covers it whole already; it keeps the profile's tables and reach finite).
        let half = 0.5 * style.stroke_width * xf.scale_factor();
        let stroked = half > 0.0 && half.is_finite() && style.opacity * style.stroke_opacity > 0.0 && style.stroke.as_ref().is_some_and(|p| !matches!(p, svg::SvgPaint::None));
        let half = half.min(HALF_MAX_DIAGONALS * self.face[0].hypot(self.face[1]).max(1.0));
        // What lies further off the face than the tube's reach (or a texel, for a fill) draws nothing.
        let margin = if stroked { self.reach_halves * half + 2.0 } else { 1.0 };
        let clip = self.clip(margin);
        let lines = flatten(path, xf, clip);
        self.budget = self.budget.saturating_sub(lines.iter().map(|(l, _)| l.len()).sum());
        if lines.is_empty() {
            return;
        }
        if let Some(paint) = style.fill.as_ref().and_then(|p| paint_of(p, style, self.defs, xf, bbox)) {
            let opacity = style.opacity * style.fill_opacity;
            if opacity > 0.0 {
                let contours: Vec<Vec<[f32; 2]>> = lines.iter().filter(|(l, _)| l.len() >= 3).map(|(l, _)| l.clone()).collect();
                if !contours.is_empty() {
                    let even_odd = matches!(style.fill_rule, svg::FillRule::EvenOdd);
                    self.items.push(Item::Fill { contours, even_odd, paint, opacity });
                }
            }
        }
        if !stroked {
            return;
        }
        if let Some(paint) = style.stroke.as_ref().and_then(|p| paint_of(p, style, self.defs, xf, bbox)) {
            let opacity = style.opacity * style.stroke_opacity;
            let lines = match &style.stroke_dasharray {
                Some(dashes) if !dashes.is_empty() => {
                    let s = xf.scale_factor();
                    let mut scaled: Vec<f32> = dashes.iter().map(|d| (d * s).max(0.0)).collect();
                    let mut offset = style.stroke_dashoffset * s;
                    // Finer than a texel the dashes are drawn at DASH_MIN_PERIOD, their share of on and off kept.
                    let period = scaled.iter().sum::<f32>() * if scaled.len() % 2 == 1 { 2.0 } else { 1.0 };
                    if period > 0.0 && period < DASH_MIN_PERIOD {
                        let k = DASH_MIN_PERIOD / period;
                        scaled.iter_mut().for_each(|d| *d *= k);
                        offset *= k;
                        self.notes.fine_dashes = true;
                    }
                    let lines = dashed(lines, &scaled, offset, clip);
                    self.budget = self.budget.saturating_sub(lines.iter().map(|(l, _)| l.len()).sum());
                    lines
                }
                _ => lines,
            };
            self.items.push(Item::Stroke { lines, half, paint, opacity });
        }
    }
}

/// A tube's reach past its centre line for the look, in half widths: its
/// glow's (neon) till it is a thousandth of the core, or just past its edge (LED).
fn reach_halves(look: f32) -> f32 {
    if look.max(0.0).min(1.0) >= 1.0 {
        1.1
    } else {
        1.0 + NEON_GLOW_FALL * (NEON_GLOW / 1.0e-3f32).ln()
    }
}

/// Whether an inline `style` sets the fill: a `fill:` declaration, not `fill-rule` or `fill-opacity`.
fn style_sets_fill(style: &str) -> bool {
    style.split(';').any(|decl| decl.split_once(':').is_some_and(|(name, _)| name.trim().eq_ignore_ascii_case("fill")))
}

/// The SVG `text` as tubes and panels in raster texels for a face `wf` x
/// `hf` texels: the viewBox (else the root size, else the drawing's bounds)
/// stretched over it (the face holds the pattern's aspect, so evenly), and
/// what the walk found and left out. An element without a fill of its own
/// is not filled (SVG fills black by default, which emits nothing). `look`
/// sets how far off the face a tube still reaches it.
fn items(text: &str, wf: f32, hf: f32, look: f32) -> (Vec<Item>, WalkNotes) {
    // SVG's own default fill is black; makepad's parser fills white. A root
    // without a fill is given black, so an unfilled tube is no panel.
    let has_fill = root_tag(text).is_some_and(|tag| attr(tag, "fill").is_some() || attr(tag, "style").is_some_and(style_sets_fill));
    let doc = if has_fill {
        svg::parse_svg(text)
    } else {
        match text.to_ascii_lowercase().find("<svg") {
            Some(at) => svg::parse_svg(&format!("{}<svg fill=\"#000\"{}", &text[..at], &text[at + 4..])),
            None => svg::parse_svg(text),
        }
    };
    let view = match &doc.viewbox {
        Some(vb) if vb.width > 0.0 && vb.height > 0.0 => Some((vb.x, vb.y, vb.width, vb.height)),
        _ => match (doc.width, doc.height) {
            (Some(w), Some(h)) if w > 0.0 && h > 0.0 => Some((0.0, 0.0, w, h)),
            _ => doc.compute_bounds().filter(|b| b.2 > b.0 && b.3 > b.1).map(|b| (b.0, b.1, b.2 - b.0, b.3 - b.1)),
        },
    };
    let Some((vx, vy, vw, vh)) = view else {
        return (Vec::new(), WalkNotes::default());
    };
    let (sx, sy) = (wf / vw, hf / vh);
    let base = svg::Transform2d { a: sx, c: 0.0, e: -vx * sx, b: 0.0, d: sy, f: -vy * sy };
    let mut walk = Walk {
        defs: &doc.defs,
        items: Vec::new(),
        notes: WalkNotes::default(),
        face: [wf, hf],
        reach_halves: reach_halves(look),
        open: Vec::new(),
        budget: WALK_BUDGET,
    };
    walk.nodes(&doc.root, &base, 0);
    (walk.items, walk.notes)
}

// ---- The raster ----

/// One level of a pattern's picture: RGBA texels, row 0 at the top.
#[derive(Debug)]
struct Level {
    w: usize,
    h: usize,
    data: Vec<[f32; 4]>,
}

impl Level {
    fn texel(&self, i: isize, j: isize) -> [f32; 4] {
        if i < 0 || j < 0 || i as usize >= self.w || j as usize >= self.h {
            return [0.0; 4];
        }
        self.data[j as usize * self.w + i as usize]
    }

    /// A footprint `fu` x `fv` texels round (u, v) averaged over up to
    /// `MAX_TAPS` bilinear taps each way.
    fn taps(&self, u: f32, v: f32, fu: f32, fv: f32) -> [f32; 4] {
        let (nu, nv) = ((fu.ceil() as usize).max(1).min(MAX_TAPS), (fv.ceil() as usize).max(1).min(MAX_TAPS));
        if nu == 1 && nv == 1 {
            return self.bilinear(u, v);
        }
        let mut sum = [0.0f32; 4];
        for j in 0..nv {
            let y = v + ((j as f32 + 0.5) / nv as f32 - 0.5) * fv;
            for i in 0..nu {
                let x = u + ((i as f32 + 0.5) / nu as f32 - 0.5) * fu;
                let t = self.bilinear(x, y);
                for c in 0..4 {
                    sum[c] += t[c];
                }
            }
        }
        let n = (nu * nv) as f32;
        sum.map(|s| s / n)
    }

    /// Bilinear at (u, v) in this level's texels (texel centres at i + 0.5); 0 outside.
    fn bilinear(&self, u: f32, v: f32) -> [f32; 4] {
        let (x, y) = (u - 0.5, v - 0.5);
        let (i, j) = (x.floor(), y.floor());
        let (fx, fy) = (x - i, y - j);
        let (i, j) = (i as isize, j as isize);
        let (a, b, c, d) = (self.texel(i, j), self.texel(i + 1, j), self.texel(i, j + 1), self.texel(i + 1, j + 1));
        let mut out = [0.0; 4];
        for k in 0..4 {
            let top = a[k] + (b[k] - a[k]) * fx;
            let bottom = c[k] + (d[k] - c[k]) * fx;
            out[k] = top + (bottom - top) * fy;
        }
        out
    }
}

/// `level0` and the levels under it, `count` in all, each the one before
/// averaged 2 x 2 (level 0's sides divide by 2 to the `count - 1`).
fn mip_chain(level0: Level, count: usize) -> Vec<Level> {
    let mut chain = vec![level0];
    for _ in 1..count {
        let prev = chain.last().unwrap();
        let (nw, nh) = (prev.w / 2, prev.h / 2);
        let mut data = vec![[0.0f32; 4]; nw * nh];
        for j in 0..nh {
            for i in 0..nw {
                let mut s = [0.0f32; 4];
                for (di, dj) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                    let t = prev.data[(2 * j + dj) * prev.w + 2 * i + di];
                    for c in 0..4 {
                        s[c] += 0.25 * t[c];
                    }
                }
                data[j * nw + i] = s;
            }
        }
        chain.push(Level { w: nw, h: nh, data });
    }
    chain
}

/// The bytes of a level-0 picture `w` x `h` texels with its levels, `count` in all.
fn chain_bytes(w: usize, h: usize, count: usize) -> usize {
    (0..count).map(|k| (w >> k) * (h >> k)).sum::<usize>() * std::mem::size_of::<[f32; 4]>()
}

/// A footprint `fu` x `fv` level-0 texels round (u, v) box-filtered from
/// `levels` (module docs): read from the level the narrower side asks for
/// (or a quarter of the longer one) with up to `MAX_TAPS` taps each way,
/// blended between two levels. `origin`: where level 0's first texel lies,
/// level-0 texels (a multiple of every level's step); 0 outside.
fn chain_filtered(levels: &[Level], (ox, oy): (f32, f32), u: f32, v: f32, fu: f32, fv: f32) -> [f32; 4] {
    let wanted = fu.min(fv).max(fu.max(fv) / MAX_TAPS as f32).max(1.0);
    let top = levels.len() - 1;
    let lambda = wanted.log2().max(0.0);
    let l0 = (lambda.floor() as usize).min(top);
    let frac = if l0 >= top { 0.0 } else { lambda - l0 as f32 };
    let level_box = |k: usize| {
        let scale = 1.0 / (1u32 << k) as f32;
        levels[k].taps((u - ox) * scale, (v - oy) * scale, fu * scale, fv * scale)
    };
    let a = level_box(l0);
    if !(frac > 1.0e-3) {
        return a;
    }
    let b = level_box(l0 + 1);
    std::array::from_fn(|k| a[k] + (b[k] - a[k]) * frac)
}

/// A pattern's emission picture on its face (module docs): RGB the tint
/// times the light's share (white 1 where a tube or panel is as bright as
/// the light), A the cover. Level 0 spans the face over `wf` x `hf` texels
/// (its last row and column may reach past the face, empty); each further
/// level is the one before averaged 2 x 2.
#[derive(Debug)]
pub struct PatternRaster {
    levels: Vec<Level>,
    wf: f32,
    hf: f32,
    lit: bool,
    /// Its tubes drawn exactly, for the footprints the levels draw too
    /// coarsely (`Crisp`); None for a drawing of panels only.
    crisp: Option<Crisp>,
}

/// What shapes the tubes (`PatternParams`' look and mounts), in raster texels.
#[derive(Clone, Copy, Debug)]
struct Look {
    look: f32,
    mount_px: f32,
    mount_dim: f32,
    corner_dim: f32,
    /// The tubes sit in housings (`PatternParams::housing`): their light ends square at their lines' ends.
    housed: bool,
}

/// A tube's cross-section through the look, box-filtered over one texel
/// across (`new`): what it emits and what it covers at a distance from its
/// centre line, texels.
struct Profile {
    step: f32,
    emit: Vec<f32>,
    body: Vec<f32>,
    reach: f32,
}

/// A tube's emission `u` half widths off its centre line, for the look `l`
/// (0 neon, 1 LED; module docs), unfiltered.
fn tube_emit(u: f32, l: f32) -> f32 {
    let led = 1.0 - step01(0.94, 1.06, u);
    let edge = 1.0 - step01(0.8, 1.2, u);
    let core = 0.55 + 0.45 * (1.0 - u.min(1.0) * u.min(1.0));
    let glow = NEON_GLOW * (-(u - 1.0).max(0.0) / NEON_GLOW_FALL).exp();
    let neon = edge * core + (1.0 - edge) * glow;
    neon + (led - neon) * l
}

/// A tube's cover `u` half widths off its centre line, for the look `l`, unfiltered.
fn tube_body(u: f32, l: f32) -> f32 {
    let body_edge = 0.06 + 0.14 * (1.0 - l);
    1.0 - step01(1.0 - body_edge, 1.0 + body_edge, u)
}

impl Profile {
    fn new(half: f32, look: f32) -> Profile {
        let l = look.max(0.0).min(1.0);
        let reach = reach_halves(look) * half + 0.5;
        let emit_at = |u: f32| tube_emit(u, l);
        let body_at = |u: f32| tube_body(u, l);
        // The antiderivatives over [0, reach + 0.5], midpoint sums at half the table's step: an
        // eighth of a half width, at most a quarter texel, but at least 1/64 half width (a tube many
        // texels wide changes far slower than a texel, and its tables stay a few hundred long).
        let step = (half / 8.0).min(0.25).max(half / 64.0).max(1.0e-4);
        let g = 0.5 * step;
        let n = ((reach + 0.5) / g).ceil() as usize + 2;
        let (mut ge, mut gb) = (Vec::with_capacity(n + 1), Vec::with_capacity(n + 1));
        let (mut se, mut sb) = (0.0f64, 0.0f64);
        ge.push(0.0f64);
        gb.push(0.0f64);
        for k in 0..n {
            let u = (k as f32 + 0.5) * g / half;
            se += (emit_at(u) * g) as f64;
            sb += (body_at(u) * g) as f64;
            ge.push(se);
            gb.push(sb);
        }
        // Odd, held past the table.
        let at = |table: &[f64], x: f32| -> f64 {
            let s = x.signum() as f64;
            let p = x.abs() / g;
            let i = (p.floor() as usize).min(table.len() - 2);
            let f = (p - i as f32).min(1.0) as f64;
            s * (table[i] + (table[i + 1] - table[i]) * f)
        };
        let count = (reach / step).ceil() as usize + 2;
        let (mut emit, mut body) = (Vec::with_capacity(count), Vec::with_capacity(count));
        for k in 0..count {
            let d = k as f32 * step;
            emit.push((at(&ge, d + 0.5) - at(&ge, d - 0.5)) as f32);
            body.push(((at(&gb, d + 0.5) - at(&gb, d - 0.5)) as f32).min(1.0));
        }
        Profile { step, emit, body, reach }
    }

    /// (emission, cover) at distance `d` texels from the centre line.
    fn at(&self, d: f32) -> (f32, f32) {
        let p = d / self.step;
        let i = (p as usize).min(self.emit.len() - 2);
        let f = (p - i as f32).min(1.0);
        (self.emit[i] + (self.emit[i + 1] - self.emit[i]) * f, self.body[i] + (self.body[i + 1] - self.body[i]) * f)
    }
}

fn step01(a: f32, b: f32, x: f32) -> f32 {
    let t = ((x - a) / (b - a)).max(0.0).min(1.0);
    t * t * (3.0 - 2.0 * t)
}

/// (1 - x²)² inside 1, 0 outside: a mount's dimming along the tube.
fn bump(x: f32) -> f32 {
    if x >= 1.0 {
        0.0
    } else {
        let s = 1.0 - x * x;
        s * s
    }
}

/// Smooth value noise along a line, -1..1, period 1.
fn noise1(x: f32, seed: u32) -> f32 {
    let hash = |i: i64| {
        let mut h = (i as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ (seed as u64).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        h ^= h >> 31;
        h = h.wrapping_mul(0x94D0_49BB_1331_11EB);
        h ^= h >> 29;
        (h >> 40) as f32 / (1u64 << 24) as f32 * 2.0 - 1.0
    };
    let i = x.floor();
    let f = x - i;
    let s = f * f * (3.0 - 2.0 * f);
    let (a, b) = (hash(i as i64), hash(i as i64 + 1));
    a + (b - a) * s
}

/// One piece of a tube's centre line.
struct Seg {
    a: [f32; 2],
    b: [f32; 2],
    len: f32,
    /// Where it starts along its line, and the line's length, texels.
    s0: f32,
    total: f32,
    open: bool,
    corner_a: bool,
    corner_b: bool,
    seed: u32,
    /// The line it is a piece of, within its item.
    line: u32,
}

impl Seg {
    /// What dims or brightens the tube along its length at `t` of this piece.
    fn along(&self, t: f32, half: f32, look: &Look) -> f32 {
        let tc = t.max(0.0).min(1.0);
        let s = self.s0 + tc * self.len;
        let tube = 2.0 * half;
        let mut f = 1.0;
        if look.look < 1.0 {
            f *= 1.0 + NEON_UNEVEN * (1.0 - look.look) * noise1(s / (NEON_UNEVEN_PERIOD * tube).max(1.0), self.seed);
        }
        if self.open && look.housed {
            // In a housing the light ends square at the line's ends, over a texel; past them are its end plates.
            let s_line = self.s0 + t * self.len;
            f *= step01(-0.5, 0.5, s_line.min(self.total - s_line));
        } else if self.open && look.look > 0.0 {
            // Past an open end (t outside the piece) is the cap's own end.
            let from_end = if t < 0.0 && self.s0 == 0.0 {
                0.0
            } else if t > 1.0 && (self.s0 + self.len - self.total).abs() < 1.0e-3 {
                0.0
            } else {
                s.min(self.total - s)
            };
            let cap = LED_CAP_TUBES * tube;
            let lit = step01(cap - 0.5, cap + 0.5, from_end);
            f *= 1.0 - (1.0 - LED_CAP_LEFT) * look.look * (1.0 - lit);
        }
        let reach = (MOUNT_TUBES * tube).max(0.75);
        if look.mount_px > 0.0 && look.mount_dim > 0.0 {
            let phase = (s - 0.5 * look.mount_px).rem_euclid(look.mount_px);
            f *= 1.0 - look.mount_dim * bump(phase.min(look.mount_px - phase) / reach);
        }
        if look.corner_dim > 0.0 {
            if self.corner_a {
                f *= 1.0 - look.corner_dim * bump(tc * self.len / reach);
            }
            if self.corner_b {
                f *= 1.0 - look.corner_dim * bump((1.0 - tc) * self.len / reach);
            }
        }
        f
    }
}

/// The pieces of `lines`, with their lengths along their lines and their corners.
fn segments(lines: &[(Vec<[f32; 2]>, bool)], seed: u32) -> Vec<Seg> {
    let cos_turn = CORNER_TURN_DEG.to_radians().cos();
    let mut out = Vec::new();
    for (k, (pts, closed)) in lines.iter().enumerate() {
        let n = pts.len();
        let count = if *closed { n } else { n - 1 };
        let piece = |i: usize| (pts[i % n], pts[(i + 1) % n]);
        let dir = |i: usize| {
            let (a, b) = piece(i);
            let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
            let l = (dx * dx + dy * dy).sqrt().max(1.0e-9);
            [dx / l, dy / l]
        };
        let bent = |i: usize, j: usize| {
            let (u, v) = (dir(i), dir(j));
            u[0] * v[0] + u[1] * v[1] < cos_turn
        };
        let total: f32 = (0..count)
            .map(|i| {
                let (a, b) = piece(i);
                ((b[0] - a[0]).powi(2) + (b[1] - a[1]).powi(2)).sqrt()
            })
            .sum();
        let mut s0 = 0.0;
        for i in 0..count {
            let (a, b) = piece(i);
            let len = ((b[0] - a[0]).powi(2) + (b[1] - a[1]).powi(2)).sqrt();
            let corner_a = if i > 0 { bent(i - 1, i) } else { *closed && bent(count - 1, 0) };
            let corner_b = if i + 1 < count { bent(i, i + 1) } else { *closed && bent(count - 1, 0) };
            out.push(Seg { a, b, len, s0, total, open: !*closed, corner_a, corner_b, seed: seed.wrapping_add(k as u32), line: k as u32 });
            s0 += len;
        }
    }
    out
}

/// Drops each piece that lies on a piece of another line (its ends within
/// `tol` texels of that one's, either way round): a honeycomb's or a
/// triangle strip's shared edges, written once per cell, are one tube. The
/// pieces are found by their middles in a grid `tol` wide, so it costs a
/// hash lookup a piece.
fn drop_coincident(segs: &mut Vec<Seg>, tol: f32) {
    let near = |p: [f32; 2], q: [f32; 2]| (p[0] - q[0]).abs() <= tol && (p[1] - q[1]).abs() <= tol;
    let same = |s: &Seg, o: &Seg| s.line != o.line && ((near(s.a, o.a) && near(s.b, o.b)) || (near(s.a, o.b) && near(s.b, o.a)));
    let cell = |s: &Seg| (((s.a[0] + s.b[0]) * 0.5 / tol).floor() as i64, ((s.a[1] + s.b[1]) * 0.5 / tol).floor() as i64);
    let mut grid: HashMap<(i64, i64), Vec<u32>> = HashMap::new();
    let mut keep = vec![true; segs.len()];
    for i in 0..segs.len() {
        let (cx, cy) = cell(&segs[i]);
        let mut twin = false;
        'look: for dx in -1..=1 {
            for dy in -1..=1 {
                if let Some(list) = grid.get(&(cx + dx, cy + dy)) {
                    if list.iter().any(|&j| same(&segs[i], &segs[j as usize])) {
                        twin = true;
                        break 'look;
                    }
                }
            }
        }
        if twin {
            keep[i] = false;
        } else {
            grid.entry((cx, cy)).or_default().push(i as u32);
        }
    }
    let mut k = 0;
    segs.retain(|_| {
        k += 1;
        keep[k - 1]
    });
}

impl PatternRaster {
    /// Rasters `text` over a face `wf` x `hf` texels, its tubes kept for
    /// drawing exactly over a face `wa` x `ha` units (`Crisp`: the size the
    /// map asks for, which the raster may be held under). `metres_wide`: the
    /// pattern's width in metres as its mounts are spaced (its natural
    /// width, else the face's).
    fn build(text: &str, pattern: &PatternParams, (wf, hf): (f32, f32), (wa, ha): (f32, f32), metres_wide: f32) -> PatternRaster {
        let mut skipped = skipped_features(text);
        let (items, notes) = items(text, wf, hf, pattern.look);
        if notes.filtered && !skipped.contains(&"filters") {
            skipped.push("filters");
        }
        if notes.looped {
            skipped.push("<use> of a symbol inside itself");
        }
        if !skipped.is_empty() {
            log!("hdri pattern: skipped what a light pattern does not draw: {}", skipped.join(", "));
        }
        if notes.left_out > 0 {
            log!(
                "hdri pattern: the drawing repeats itself (through <use>) past {WALK_BUDGET} nodes and points; {} nodes left out",
                notes.left_out
            );
        }
        if notes.fine_dashes {
            log!("hdri pattern: a dash pattern finer than {DASH_MIN_PERIOD} texel drawn at {DASH_MIN_PERIOD} texel, its share of dash and gap kept");
        }
        // Levels down to a shorter side of about four texels, every level halving exactly.
        let short = wf.min(hf).max(1.0);
        let levels = 1 + ((short / 4.0).max(1.0).log2().floor() as usize);
        let unit = 1usize << (levels - 1);
        let pad = |v: f32| ((v.ceil() as usize).max(1)).div_ceil(unit) * unit;
        let (w, h) = (pad(wf), pad(hf));
        let mut img = vec![[0.0f32; 4]; w * h];
        let px_per_m = wf / metres_wide.max(1.0e-6);
        let look = Look {
            look: pattern.look,
            mount_px: pattern.mount_spacing_m * px_per_m,
            mount_dim: pattern.mount_dim,
            corner_dim: pattern.corner_dim,
            housed: pattern.housing.is_some(),
        };
        // Only the face is drawn: the viewBox is the viewport.
        let (draw_w, draw_h) = ((wf.ceil() as usize).min(w), (hf.ceil() as usize).min(h));
        let face = [0, 0, draw_w, draw_h];
        for (k, item) in items.iter().enumerate() {
            match item {
                Item::Stroke { lines, half, paint, opacity } => draw_stroke(&mut img, w, face, lines, *half, paint, *opacity, &look, k as u32, 1.0),
                Item::Fill { contours, even_odd, paint, opacity } => draw_fill(&mut img, w, face, contours, *even_odd, paint, *opacity, 1.0),
            }
        }
        let lit = img.iter().any(|t| t[0] > 0.0 || t[1] > 0.0 || t[2] > 0.0 || t[3] > 0.0);
        let chain = mip_chain(Level { w, h, data: img }, levels);
        let crisp = if !lit || !items.iter().any(|item| matches!(item, Item::Stroke { .. })) {
            None
        } else if wa == wf && ha == hf {
            Crisp::build(&items, pattern, (wf, hf), wf, (w, h), (draw_w, draw_h), levels, metres_wide)
        } else {
            let (fine, _) = self::items(text, wa, ha, pattern.look);
            Crisp::build(&fine, pattern, (wa, ha), wf, (w, h), (draw_w, draw_h), levels, metres_wide)
        };
        PatternRaster { levels: chain, wf, hf, lit, crisp }
    }

    /// The memory its levels and its exact tubes take, bytes.
    fn bytes(&self) -> usize {
        let levels: usize = self.levels.iter().map(|l| l.data.len() * std::mem::size_of::<[f32; 4]>()).sum();
        levels + self.crisp.as_ref().map_or(0, Crisp::bytes)
    }

    /// The picture over a texel whose two sides are `a` and `b` on the
    /// face, level-0 texels, round (u, v) (module docs): the levels where the
    /// footprint is wide, the exact tubes where it is narrow, blended between.
    fn drawn(&self, u: f32, v: f32, a: [f32; 2], b: [f32; 2]) -> [f32; 4] {
        let (fu, fv) = (a[0].abs() + b[0].abs(), a[1].abs() + b[1].abs());
        let Some(crisp) = &self.crisp else {
            return self.filtered(u, v, fu, fv);
        };
        let (short, long) = (fu.min(fv), fu.max(fv));
        let k = ((2.0 - short / CRISP_SHORT).max(0.0).min(1.0)) * ((2.0 - long / CRISP_LONG).max(0.0).min(1.0));
        let coarse = self.filtered(u, v, fu, fv);
        if !(k > 0.0) {
            return coarse;
        }
        // Where the levels hold nothing over the exact tubes' reach (their
        // tent is twice the footprint wide; the raster's tubes reach as far
        // as theirs), those are dark too and are not looked at.
        let lit = |t: [f32; 4]| t.iter().any(|c| *c > 0.0);
        if !lit(coarse) && !lit(self.filtered(u, v, 2.0 * fu + 2.0, 2.0 * fv + 2.0)) {
            return coarse;
        }
        let exact = crisp.at(u, v, a, b);
        if k >= 1.0 {
            return exact;
        }
        std::array::from_fn(|c| coarse[c] + (exact[c] - coarse[c]) * k)
    }

    /// The picture box-filtered over a footprint `fu` x `fv` texels wide
    /// round (u, v), level-0 texels (module docs).
    fn filtered(&self, u: f32, v: f32, fu: f32, fv: f32) -> [f32; 4] {
        if u + 0.5 * fu + 1.0 < 0.0 || v + 0.5 * fv + 1.0 < 0.0 || u - 0.5 * fu - 1.0 > self.wf || v - 0.5 * fv - 1.0 > self.hf {
            return [0.0; 4];
        }
        chain_filtered(&self.levels, (0.0, 0.0), u, v, fu, fv)
    }

    /// Calls `f` with each lit texel of the face at level 0: its middle as
    /// (0..1 across, 0..1 down) of the face, and its RGBA.
    fn for_each_lit(&self, mut f: impl FnMut(f32, f32, [f32; 4])) {
        let level = &self.levels[0];
        let (cols, rows) = ((self.wf.ceil() as usize).min(level.w), (self.hf.ceil() as usize).min(level.h));
        for j in 0..rows {
            let y = (j as f32 + 0.5) / self.hf;
            for i in 0..cols {
                let t = level.data[j * level.w + i];
                if t[0] > 0.0 || t[1] > 0.0 || t[2] > 0.0 {
                    f((i as f32 + 0.5) / self.wf, y, t);
                }
            }
        }
    }
}

/// Draws one tube item over `img` (module docs): the texels x in `wx0..wx1`
/// and y in `wy0..wy1`, which `img` holds from (`wx0`, `wy0`) on, `stride`
/// a row. Its paint is read at the texel's middle times `paint_scale`.
fn draw_stroke(img: &mut [[f32; 4]], stride: usize, [wx0, wy0, wx1, wy1]: [usize; 4], lines: &[(Vec<[f32; 2]>, bool)], half: f32, paint: &Paint, opacity: f32, look: &Look, seed: u32, paint_scale: f32) {
    let mut segs = segments(lines, seed.wrapping_mul(7919));
    // Shared edges drawn once: ends within a tenth of the half width (a generator's
    // SVG rounds to 0.01 mm, at most a fiftieth of a 1 mm tube's half width).
    drop_coincident(&mut segs, (0.1 * half).max(1.0e-3));
    if segs.is_empty() || wx1 <= wx0 || wy1 <= wy0 {
        return;
    }
    let profile = Profile::new(half, look.look);
    let reach = profile.reach;
    // The item's box, texels, held to the face.
    let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
    for s in &segs {
        x0 = x0.min(s.a[0].min(s.b[0]));
        y0 = y0.min(s.a[1].min(s.b[1]));
        x1 = x1.max(s.a[0].max(s.b[0]));
        y1 = y1.max(s.a[1].max(s.b[1]));
    }
    let clip = |v: f32, lo: usize, hi: usize| (v.max(lo as f32) as usize).min(hi);
    let (bx0, by0) = (clip((x0 - reach).floor(), wx0, wx1), clip((y0 - reach).floor(), wy0, wy1));
    let (bx1, by1) = (clip((x1 + reach).ceil() + 1.0, wx0, wx1), clip((y1 + reach).ceil() + 1.0, wy0, wy1));
    if bx1 <= bx0 || by1 <= by0 {
        return;
    }
    let bw = bx1 - bx0;
    let cells = bw * (by1 - by0);
    // Per texel of the box, for the line being drawn: its nearest piece's distance, index and where along it.
    let mut best = vec![(f32::MAX, u32::MAX, 0.0f32); cells];
    // Per texel: the lines' light added up, their cover added up, and the most one line gives.
    let mut sum = vec![[0.0f32; 3]; cells];
    // The texels the line being drawn reaches.
    let mut touched: Vec<usize> = Vec::new();
    let resolve = |best: &[(f32, u32, f32)], sum: &mut [[f32; 3]], touched: &mut Vec<usize>| {
        for i in touched.drain(..) {
            let (d, k, t) = best[i];
            let (e, cover) = profile.at(d);
            let e = e * segs[k as usize].along(t, half, look);
            let s = &mut sum[i];
            s[0] += e;
            s[1] += cover;
            s[2] = s[2].max(e);
        }
    };
    let mut line = u32::MAX;
    for (k, s) in segs.iter().enumerate() {
        if s.line != line {
            resolve(&best, &mut sum, &mut touched);
            line = s.line;
        }
        let (sx0, sy0) = (clip((s.a[0].min(s.b[0]) - reach).floor(), bx0, bx1), clip((s.a[1].min(s.b[1]) - reach).floor(), by0, by1));
        let (sx1, sy1) = (clip((s.a[0].max(s.b[0]) + reach).ceil() + 1.0, bx0, bx1), clip((s.a[1].max(s.b[1]) + reach).ceil() + 1.0, by0, by1));
        let (dx, dy) = (s.b[0] - s.a[0], s.b[1] - s.a[1]);
        let l2 = (dx * dx + dy * dy).max(1.0e-12);
        for y in sy0..sy1 {
            let py = y as f32 + 0.5 - s.a[1];
            let row = (y - by0) * bw;
            for x in sx0..sx1 {
                let px = x as f32 + 0.5 - s.a[0];
                let t = (px * dx + py * dy) / l2;
                let tc = t.max(0.0).min(1.0);
                let (ex, ey) = (px - tc * dx, py - tc * dy);
                let d = (ex * ex + ey * ey).sqrt();
                if !(d < reach) {
                    continue;
                }
                let i = row + x - bx0;
                let cell = &mut best[i];
                if cell.1 == u32::MAX || segs[cell.1 as usize].line != line {
                    // The line's first piece here.
                    *cell = (d, k as u32, t);
                    touched.push(i);
                } else if d < cell.0 {
                    *cell = (d, k as u32, t);
                }
            }
        }
    }
    resolve(&best, &mut sum, &mut touched);
    for y in by0..by1 {
        for x in bx0..bx1 {
            let [e, cover, most] = sum[(y - by0) * bw + x - bx0];
            if !(e > 0.0 || cover > 0.0) {
                continue;
            }
            // Held to what one tube gives at its brightest, and to full cover.
            let (e, cover) = (e.min(most.max(1.0)), cover.min(1.0));
            let (color, alpha) = paint.at((x as f32 + 0.5) * paint_scale, (y as f32 + 0.5) * paint_scale);
            let op = opacity * alpha;
            let dst = &mut img[(y - wy0) * stride + x - wx0];
            let a = (cover * op).min(1.0);
            for c in 0..3 {
                dst[c] = dst[c] * (1.0 - a) + color[c] * e * op;
            }
            dst[3] += a * (1.0 - dst[3]);
        }
    }
}

/// Draws one panel item over `img` (its window as `draw_stroke` takes it):
/// its cover from exact spans on `FILL_SUBROWS` sub-scanlines a row, each
/// crossing only the edges that span it (sorted by their tops, an active
/// list): rows x sub-scanlines x active edges, plus a sort of the edges.
fn draw_fill(img: &mut [[f32; 4]], stride: usize, [wx0, wy0, wx1, wy1]: [usize; 4], contours: &[Vec<[f32; 2]>], even_odd: bool, paint: &Paint, opacity: f32, paint_scale: f32) {
    // Edges (x0, y0, x1, y1, winding), closed round each contour.
    let mut edges: Vec<(f32, f32, f32, f32, i32)> = Vec::new();
    let (mut y_lo, mut y_hi, mut x_lo, mut x_hi) = (f32::MAX, f32::MIN, f32::MAX, f32::MIN);
    for c in contours {
        for i in 0..c.len() {
            let (a, b) = (c[i], c[(i + 1) % c.len()]);
            x_lo = x_lo.min(a[0]);
            x_hi = x_hi.max(a[0]);
            if a[1] == b[1] {
                continue;
            }
            y_lo = y_lo.min(a[1].min(b[1]));
            y_hi = y_hi.max(a[1].max(b[1]));
            edges.push((a[0], a[1], b[0], b[1], if b[1] > a[1] { 1i32 } else { -1 }));
        }
    }
    if edges.is_empty() || wx1 <= wx0 || wy1 <= wy0 {
        return;
    }
    let clip = |v: f32, lo: usize, hi: usize| (v.max(lo as f32) as usize).min(hi);
    let (r0, r1) = (clip(y_lo.floor(), wy0, wy1), clip(y_hi.ceil() + 1.0, wy0, wy1));
    let (c0, c1) = (clip(x_lo.floor(), wx0, wx1), clip(x_hi.ceil() + 1.0, wx0, wx1));
    if c1 <= c0 {
        return;
    }
    let top = |e: &(f32, f32, f32, f32, i32)| e.1.min(e.3);
    edges.sort_by(|a, b| top(a).total_cmp(&top(b)));
    let (mut next, mut active) = (0usize, Vec::<usize>::new());
    let mut row = vec![0.0f32; c1 - c0];
    let mut hits: Vec<(f32, i32)> = Vec::new();
    for y in r0..r1 {
        row.iter_mut().for_each(|v| *v = 0.0);
        for sub in 0..FILL_SUBROWS {
            let sy = y as f32 + (sub as f32 + 0.5) / FILL_SUBROWS as f32;
            // The sub-scanlines only go down: edges join the list at their tops and leave past their bottoms.
            while next < edges.len() && top(&edges[next]) <= sy {
                active.push(next);
                next += 1;
            }
            active.retain(|&e| edges[e].1.max(edges[e].3) > sy);
            hits.clear();
            for &e in &active {
                let (ax, ay, bx, by, w) = edges[e];
                if (sy >= ay.min(by)) && (sy < ay.max(by)) {
                    hits.push((ax + (sy - ay) / (by - ay) * (bx - ax), w));
                }
            }
            hits.sort_by(|a, b| a.0.total_cmp(&b.0));
            let mut wind = 0i32;
            for k in 0..hits.len() {
                wind += hits[k].1;
                let inside = if even_odd { (k + 1) % 2 == 1 } else { wind != 0 };
                if !inside || k + 1 >= hits.len() {
                    continue;
                }
                let (xa, xb) = ((hits[k].0 - c0 as f32).max(0.0), (hits[k + 1].0 - c0 as f32).min((c1 - c0) as f32));
                if !(xb > xa) {
                    continue;
                }
                let share = 1.0 / FILL_SUBROWS as f32;
                let (ia, ib) = (xa.floor() as usize, (xb.ceil() as usize).min(row.len()));
                for i in ia..ib {
                    let over = xb.min(i as f32 + 1.0) - xa.max(i as f32);
                    if over > 0.0 {
                        row[i] += over * share;
                    }
                }
            }
        }
        for (i, cover) in row.iter().enumerate() {
            if !(*cover > 0.0) {
                continue;
            }
            let x = c0 + i;
            let (color, alpha) = paint.at((x as f32 + 0.5) * paint_scale, (y as f32 + 0.5) * paint_scale);
            let a = (cover.min(1.0) * opacity * alpha).min(1.0);
            let dst = &mut img[(y - wy0) * stride + x - wx0];
            for c in 0..3 {
                dst[c] = dst[c] * (1.0 - a) + color[c] * a;
            }
            dst[3] += a * (1.0 - dst[3]);
        }
    }
}

// ---- The tubes drawn exactly ----

/// A tube's cross-section unfiltered (`tube_emit`, `tube_body`) out from its
/// centre line, in half widths, with its first and second running integrals,
/// so it is filtered exactly over any footprint (`filtered`). Even, dark past
/// `end`.
struct TubeSums {
    step: f64,
    end: f64,
    /// Emission and cover, each its values, integrals and integrals' integrals.
    emit: [Vec<f64>; 3],
    body: [Vec<f64>; 3],
}

impl TubeSums {
    fn new(look: f32) -> TubeSums {
        let l = look.max(0.0).min(1.0);
        let end = reach_halves(look) as f64;
        let step = 1.0 / TUBE_SUMS_PER_HALF as f64;
        let n = (end / step).ceil() as usize + 1;
        let table = |f: &dyn Fn(f32) -> f32| {
            let values: Vec<f64> = (0..=n).map(|k| f((k as f64 * step) as f32) as f64).collect();
            let (mut first, mut second) = (vec![0.0f64; n + 1], vec![0.0f64; n + 1]);
            for k in 1..=n {
                first[k] = first[k - 1] + 0.5 * (values[k - 1] + values[k]) * step;
                second[k] = second[k - 1] + 0.5 * (first[k - 1] + first[k]) * step;
            }
            [values, first, second]
        };
        let emit = table(&|u| tube_emit(u, l));
        let body = table(&|u| tube_body(u, l));
        TubeSums { step, end: n as f64 * step, emit, body }
    }

    /// Entry `x` (half widths, 0 or more) of one table, linear between entries;
    /// past the end the values are 0, the integral holds and its integral grows along it.
    fn read(&self, t: &[Vec<f64>; 3], which: usize, x: f64) -> f64 {
        if x >= self.end {
            let last = t[which].len() - 1;
            return match which {
                0 => 0.0,
                1 => t[1][last],
                _ => t[2][last] + t[1][last] * (x - self.end),
            };
        }
        let p = x / self.step;
        let i = p.floor() as usize;
        let f = p - i as f64;
        t[which][i] + (t[which][i + 1] - t[which][i]) * f
    }

    /// The value at `x` half widths (either side), its integral from 0 (odd)
    /// and that one's integral from 0 (even).
    fn value(&self, t: &[Vec<f64>; 3], x: f64) -> f64 {
        self.read(t, 0, x.abs())
    }

    fn first(&self, t: &[Vec<f64>; 3], x: f64) -> f64 {
        x.signum() * self.read(t, 1, x.abs())
    }

    fn second(&self, t: &[Vec<f64>; 3], x: f64) -> f64 {
        self.read(t, 2, x.abs())
    }

    /// One table box-filtered over `w1` and then `w2` across the tube (a
    /// footprint's two sides seen across it), at `d` from the centre line,
    /// all in half widths: two boxes convolved, exact from the second integral.
    fn across(&self, t: &[Vec<f64>; 3], d: f64, w1: f64, w2: f64) -> f64 {
        let (w1, w2) = if w1 >= w2 { (w1, w2) } else { (w2, w1) };
        if w1 < 1.0e-3 {
            return self.value(t, d);
        }
        if w2 < 1.0e-3 {
            return (self.first(t, d + 0.5 * w1) - self.first(t, d - 0.5 * w1)) / w1;
        }
        let (p, q) = (0.5 * (w1 + w2), 0.5 * (w1 - w2));
        (self.second(t, d + p) - self.second(t, d + q) - self.second(t, d - q) + self.second(t, d - p)) / (w1 * w2)
    }

    /// (emission, cover) filtered so (`across`).
    fn filtered(&self, d: f32, w1: f32, w2: f32) -> (f32, f32) {
        let (d, w1, w2) = (d as f64, w1 as f64, w2 as f64);
        (self.across(&self.emit, d, w1, w2) as f32, (self.across(&self.body, d, w1, w2) as f32).min(1.0))
    }
}

/// The distance from `p` to piece `s`, where along it (0..1 inside, as
/// `draw_stroke` takes it: unclamped) and the unit direction from its
/// nearest point to `p` (across the piece where `p` lies on it).
fn segment_distance(s: &Seg, p: [f32; 2]) -> (f32, f32, [f32; 2]) {
    let (dx, dy) = (s.b[0] - s.a[0], s.b[1] - s.a[1]);
    let (px, py) = (p[0] - s.a[0], p[1] - s.a[1]);
    let l2 = (dx * dx + dy * dy).max(1.0e-12);
    let t = (px * dx + py * dy) / l2;
    let tc = t.max(0.0).min(1.0);
    let (ox, oy) = (px - tc * dx, py - tc * dy);
    let d = (ox * ox + oy * oy).sqrt();
    let n = if d > 1.0e-6 {
        [ox / d, oy / d]
    } else {
        let l = l2.sqrt();
        [-dy / l, dx / l]
    };
    (d, t, n)
}

/// One tube item drawn exactly (`Crisp`), in the crisp units: how it is
/// drawn. Its pieces are `Crisp::segs`, found through its slots.
struct TubeItem {
    half: f32,
    paint: Paint,
    opacity: f32,
    /// How far from its centre line a tube gives light, units.
    reach: f32,
}

/// A run of panels between tube items (or the drawing's first items drawn
/// into one, `CRISP_PANELS_BYTES`) as `Crisp` draws it: rastered on its own
/// at the raster's level-0 texels over what it covers, premultiplied over
/// nothing, with its levels, so it is filtered as the raster is.
struct PanelPicture {
    levels: Vec<Level>,
    /// Where its level 0's first texel lies in the raster's level 0, texels
    /// (a multiple of every level's step).
    origin: (f32, f32),
    /// What it covers (x0, y0, x1, y1), units.
    bounds: [f32; 4],
}

/// One layer of the drawing as `Crisp` draws it, in document order.
enum CrispLayer {
    /// A tube item, drawn exactly.
    Tubes(TubeItem),
    /// A run of panels.
    Panels(PanelPicture),
}

/// A drawing's tubes drawn exactly per lookup (module docs): its layers in
/// document order, each tube item from its pieces, the panels between them
/// from their own pictures. All are found through one grid over the face,
/// whose cells list the slots (a tube item's piece, or a panel picture)
/// whose light may reach a lookup there, so a lookup costs what lies near
/// it, not what the drawing holds. Its units are the face over `wa` x `ha`
/// (the size the map asks for), `scale` of them a raster texel.
struct Crisp {
    scale: f32,
    wa: f32,
    ha: f32,
    look: Look,
    /// The tubes' cross-section tables (one look for the whole drawing).
    sums: TubeSums,
    layers: Vec<CrispLayer>,
    /// The tube items' pieces (as `draw_stroke` takes them).
    segs: Vec<Seg>,
    /// Each slot's layer and piece in `segs` (`u32::MAX` for a panel
    /// picture), in document order.
    slots: Vec<(u32, u32)>,
    cell: f32,
    cols: usize,
    rows: usize,
    /// Each cell's run of `listed` (cells row by row; `starts` one longer).
    starts: Vec<u32>,
    /// Slots, ascending within each cell.
    listed: Vec<u32>,
}

impl std::fmt::Debug for Crisp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Crisp").field("scale", &self.scale).field("layers", &self.layers.len()).field("pieces", &self.segs.len()).finish()
    }
}

thread_local! {
    /// The slots a lookup over several cells gathers (`Crisp::at`), kept per thread.
    static FOUND: std::cell::RefCell<Vec<u32>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// An empty box (x0, y0, x1, y1): x0 past x1.
const EMPTY_BOX: [f32; 4] = [f32::MAX, f32::MAX, f32::MIN, f32::MIN];

/// The box round `points` (`EMPTY_BOX` for none).
fn points_box<'a>(points: impl Iterator<Item = &'a [f32; 2]>) -> [f32; 4] {
    points.fold(EMPTY_BOX, |b, p| [b[0].min(p[0]), b[1].min(p[1]), b[2].max(p[0]), b[3].max(p[1])])
}

fn box_union(a: [f32; 4], b: [f32; 4]) -> [f32; 4] {
    [a[0].min(b[0]), a[1].min(b[1]), a[2].max(b[2]), a[3].max(b[3])]
}

impl Crisp {
    /// From `items` laid over `wa` x `ha` units, for a raster `wf` texels
    /// across the face whose level 0 is `w` x `h` texels (drawn over `dw` x
    /// `dh`) in `count` levels. None where no tube item is left to draw
    /// exactly.
    fn build(items: &[Item], pattern: &PatternParams, (wa, ha): (f32, f32), wf: f32, (w, h): (usize, usize), (dw, dh): (usize, usize), count: usize, metres_wide: f32) -> Option<Crisp> {
        let scale = wa / wf.max(1.0e-6);
        let look = Look {
            look: pattern.look,
            mount_px: pattern.mount_spacing_m * wa / metres_wide.max(1.0e-6),
            mount_dim: pattern.mount_dim,
            corner_dim: pattern.corner_dim,
            housed: pattern.housing.is_some(),
        };
        // The same in raster texels, for tubes drawn into a picture.
        let texel_look = Look { mount_px: look.mount_px / scale, ..look };
        let halves = reach_halves(pattern.look);
        // Each item's box in raster texels (a tube's out to its reach).
        let boxes: Vec<[f32; 4]> = items
            .iter()
            .map(|item| {
                let (b, grow) = match item {
                    Item::Stroke { lines, half, .. } => (points_box(lines.iter().flat_map(|(l, _)| l)), halves * *half),
                    Item::Fill { contours, .. } => (points_box(contours.iter().flatten()), 0.0),
                };
                [(b[0] - grow) / scale, (b[1] - grow) / scale, (b[2] + grow) / scale, (b[3] + grow) / scale]
            })
            .collect();
        // A picture's window on level 0 round a box: a texel's margin, on the levels' step, within the drawn face.
        let unit = 1usize << (count - 1);
        let window = |b: [f32; 4]| -> Option<[usize; 4]> {
            if !(b[0] < dw as f32 && b[1] < dh as f32 && b[2] >= 0.0 && b[3] >= 0.0) {
                return None;
            }
            let lo = |v: f32| (v.floor().max(0.0) as usize) / unit * unit;
            let hi = |v: f32, d: usize, n: usize| (((v.ceil() + 1.0).max(0.0) as usize).min(d).div_ceil(unit) * unit).min(n);
            Some([lo(b[0]), lo(b[1]), hi(b[2], dw, w), hi(b[3], dh, h)])
        };
        let picture_bytes = |b: [f32; 4]| window(b).map_or(0, |[x0, y0, x1, y1]| chain_bytes(x1 - x0, y1 - y0, count));
        // The items as layers: each run of panels, and each tube item (true).
        let mut groups: Vec<(std::ops::Range<usize>, bool)> = Vec::new();
        for (k, item) in items.iter().enumerate() {
            let tube = matches!(item, Item::Stroke { .. });
            match groups.last_mut() {
                Some((range, false)) if !tube => range.end = k + 1,
                _ => groups.push((k..k + 1, tube)),
            }
        }
        let group_box = |range: &std::ops::Range<usize>| boxes[range.clone()].iter().fold(EMPTY_BOX, |a, b| box_union(a, *b));
        // The panels' pictures held to CRISP_PANELS_BYTES: past it the fewest first layers that bring them
        // within it are drawn into one picture, their tubes as the raster draws them. Drawing them all so
        // fits (one picture of the face).
        let run_bytes: Vec<usize> = groups.iter().map(|(range, tube)| if *tube { 0 } else { picture_bytes(group_box(range)) }).collect();
        let total: usize = run_bytes.iter().sum();
        let mut folded = 0;
        if total > CRISP_PANELS_BYTES {
            let (mut b, mut rest) = (EMPTY_BOX, total);
            folded = groups.len();
            for (g, (range, _)) in groups.iter().enumerate() {
                b = box_union(b, group_box(range));
                rest -= run_bytes[g];
                if picture_bytes(b) + rest <= CRISP_PANELS_BYTES {
                    folded = g + 1;
                    break;
                }
            }
            log!(
                "hdri pattern: the panels between the drawing's tubes would take {} MB as their own pictures, past {} MB; the first {} of its {} items are drawn as the raster draws them, their tubes not exactly",
                total >> 20,
                CRISP_PANELS_BYTES >> 20,
                groups[folded - 1].0.end,
                items.len()
            );
        }
        // Items `range` drawn into a picture over `b` as the raster draws them.
        let picture = |range: std::ops::Range<usize>, b: [f32; 4]| -> Option<PanelPicture> {
            let [x0, y0, x1, y1] = window(b)?;
            let (pw, ph) = (x1 - x0, y1 - y0);
            let mut img = vec![[0.0f32; 4]; pw * ph];
            let clip = [x0, y0, x1.min(dw), y1.min(dh)];
            let down = |p: &[f32; 2]| [p[0] / scale, p[1] / scale];
            for k in range {
                match &items[k] {
                    Item::Fill { contours, even_odd, paint, opacity } => {
                        let contours: Vec<Vec<[f32; 2]>> = contours.iter().map(|c| c.iter().map(down).collect()).collect();
                        draw_fill(&mut img, pw, clip, &contours, *even_odd, paint, *opacity, scale);
                    }
                    Item::Stroke { lines, half, paint, opacity } => {
                        let lines: Vec<(Vec<[f32; 2]>, bool)> = lines.iter().map(|(l, closed)| (l.iter().map(down).collect(), *closed)).collect();
                        draw_stroke(&mut img, pw, clip, &lines, *half / scale, paint, *opacity, &texel_look, k as u32, scale);
                    }
                }
            }
            Some(PanelPicture { levels: mip_chain(Level { w: pw, h: ph, data: img }, count), origin: (x0 as f32, y0 as f32), bounds: b.map(|v| v * scale) })
        };
        let mut layers: Vec<CrispLayer> = Vec::new();
        let mut segs: Vec<Seg> = Vec::new();
        let mut slots: Vec<(u32, u32)> = Vec::new();
        if folded > 0 {
            let b = groups[..folded].iter().fold(EMPTY_BOX, |a, (range, _)| box_union(a, group_box(range)));
            if let Some(p) = picture(0..groups[folded - 1].0.end, b) {
                slots.push((layers.len() as u32, u32::MAX));
                layers.push(CrispLayer::Panels(p));
            }
        }
        for (range, tube) in &groups[folded..] {
            if !*tube {
                if let Some(p) = picture(range.clone(), group_box(range)) {
                    slots.push((layers.len() as u32, u32::MAX));
                    layers.push(CrispLayer::Panels(p));
                }
                continue;
            }
            let k = range.start;
            let Item::Stroke { lines, half, paint, opacity } = &items[k] else {
                continue;
            };
            let mut pieces = segments(lines, (k as u32).wrapping_mul(7919));
            drop_coincident(&mut pieces, (0.1 * *half).max(1.0e-3));
            if pieces.is_empty() {
                continue;
            }
            let layer = layers.len() as u32;
            for s in pieces {
                slots.push((layer, segs.len() as u32));
                segs.push(s);
            }
            layers.push(CrispLayer::Tubes(TubeItem { half: *half, paint: paint.clone(), opacity: *opacity, reach: halves * *half }));
        }
        if segs.is_empty() {
            return None;
        }
        let reach_of = |layer: u32| match &layers[layer as usize] {
            CrispLayer::Tubes(item) => item.reach,
            CrispLayer::Panels(_) => 0.0,
        };
        // Cells a few reaches wide (the reach most pieces have; a wider tube is listed in more cells), wide
        // enough that a long footprint spans a few of them, and no more than CRISP_CELLS of them over the face.
        let mut reaches: Vec<f32> = slots.iter().filter(|(_, piece)| *piece != u32::MAX).map(|(layer, _)| reach_of(*layer)).collect();
        let mid = reaches.len() / 2;
        let typical = *reaches.select_nth_unstable_by(mid, |a, b| a.total_cmp(b)).1;
        let cell = (2.0 * typical + 2.0).max(CRISP_LONG * scale / 4.0).max((wa * ha / CRISP_CELLS as f32).sqrt());
        let (cols, rows) = (((wa / cell).ceil() as usize).max(1), ((ha / cell).ceil() as usize).max(1));
        // How far past what a panel picture covers its levels still give light to a lookup the exact path
        // draws, units: such a footprint (under twice CRISP_SHORT by twice CRISP_LONG raster texels) reads
        // levels at most 16 texels coarse, whose bilinear taps reach one and a half of those past it each
        // way; three of them reach past that across a diagonal too.
        let leak = 3.0 * (2.0 * CRISP_SHORT).max(2.0 * CRISP_LONG / MAX_TAPS as f32) * scale;
        let mut lists: Vec<Vec<u32>> = vec![Vec::new(); cols * rows];
        let half_diagonal = cell * std::f32::consts::FRAC_1_SQRT_2;
        let index = |v: f32, n: usize| ((v / cell).floor().max(0.0) as usize).min(n - 1);
        for (n, &(layer, piece)) in slots.iter().enumerate() {
            let piece = (piece != u32::MAX).then(|| &segs[piece as usize]);
            let (b, near) = match (piece, &layers[layer as usize]) {
                (Some(s), CrispLayer::Tubes(item)) => ([s.a[0].min(s.b[0]), s.a[1].min(s.b[1]), s.a[0].max(s.b[0]), s.a[1].max(s.b[1])], item.reach),
                (_, CrispLayer::Panels(p)) => (p.bounds, leak),
                _ => continue,
            };
            let (i0, i1) = (index(b[0] - near, cols), index(b[2] + near, cols));
            let (j0, j1) = (index(b[1] - near, rows), index(b[3] + near, rows));
            for j in j0..=j1 {
                for i in i0..=i1 {
                    let c = [(i as f32 + 0.5) * cell, (j as f32 + 0.5) * cell];
                    let d = match piece {
                        Some(s) => segment_distance(s, c).0,
                        None => (b[0] - c[0]).max(c[0] - b[2]).max(0.0).hypot((b[1] - c[1]).max(c[1] - b[3]).max(0.0)),
                    };
                    if d <= near + half_diagonal {
                        lists[j * cols + i].push(n as u32);
                    }
                }
            }
        }
        let mut starts = Vec::with_capacity(lists.len() + 1);
        let mut listed = Vec::new();
        starts.push(0u32);
        for list in &lists {
            listed.extend_from_slice(list);
            starts.push(listed.len() as u32);
        }
        Some(Crisp { scale, wa, ha, look, sums: TubeSums::new(pattern.look), layers, segs, slots, cell, cols, rows, starts, listed })
    }

    fn bytes(&self) -> usize {
        let pictures: usize = self
            .layers
            .iter()
            .map(|layer| match layer {
                CrispLayer::Tubes(_) => 0,
                CrispLayer::Panels(p) => p.levels.iter().map(|l| l.data.len() * std::mem::size_of::<[f32; 4]>()).sum(),
            })
            .sum();
        pictures
            + self.segs.len() * std::mem::size_of::<Seg>()
            + self.slots.len() * std::mem::size_of::<(u32, u32)>()
            + (self.starts.len() + self.listed.len()) * 4
            + self.sums.emit.iter().chain(&self.sums.body).map(|t| t.len() * 8).sum::<usize>()
    }

    /// The drawing over a texel whose sides on the face are `a` and `b`
    /// round (u, v), all in level-0 raster texels, premultiplied RGBA as the
    /// raster holds it; cut at the face's edges as the raster is.
    fn at(&self, u: f32, v: f32, a: [f32; 2], b: [f32; 2]) -> [f32; 4] {
        let s = self.scale;
        let p = [u * s, v * s];
        let (a, b) = ([a[0] * s, a[1] * s], [b[0] * s, b[1] * s]);
        // The footprint's extent each way: its box reaches half of it either side, its tent all of it.
        let (fx, fy) = (a[0].abs() + b[0].abs(), a[1].abs() + b[1].abs());
        let (ex, ey) = (0.5 * fx, 0.5 * fy);
        // The share of the footprint's box on the face, each way.
        let inside = |c: f32, e: f32, size: f32| {
            if e < 1.0e-6 {
                return if c >= 0.0 && c < size { 1.0 } else { 0.0 };
            }
            ((c + e).min(size) - (c - e).max(0.0)).max(0.0) / (2.0 * e)
        };
        let share = inside(p[0], ex, self.wa) * inside(p[1], ey, self.ha);
        if !(share > 0.0) {
            return [0.0; 4];
        }
        let (fu, fv) = (fx / s, fy / s);
        // The cells over the tent's reach.
        let cell_of = |v: f32, n: usize| ((v / self.cell).floor().max(0.0) as usize).min(n - 1);
        let (i0, i1) = (cell_of(p[0] - fx, self.cols), cell_of(p[0] + fx, self.cols));
        let (j0, j1) = (cell_of(p[1] - fy, self.rows), cell_of(p[1] + fy, self.rows));
        let run = |i: usize, j: usize| {
            let c = j * self.cols + i;
            &self.listed[self.starts[c] as usize..self.starts[c + 1] as usize]
        };
        let mut out = [0.0f32; 4];
        FOUND.with(|found| {
            let mut found = found.borrow_mut();
            let slots: &[u32] = if i0 == i1 && j0 == j1 {
                run(i0, j0)
            } else {
                found.clear();
                for j in j0..=j1 {
                    for i in i0..=i1 {
                        found.extend_from_slice(run(i, j));
                    }
                }
                found.sort_unstable();
                found.dedup();
                &found[..]
            };
            // Layer by layer in document order, each from its slots here.
            let mut i = 0;
            while i < slots.len() {
                let layer = self.slots[slots[i] as usize].0;
                let mut j = i + 1;
                while j < slots.len() && self.slots[slots[j] as usize].0 == layer {
                    j += 1;
                }
                let (rgb, alpha) = match &self.layers[layer as usize] {
                    CrispLayer::Tubes(item) => self.tubes_at(item, &slots[i..j], p, a, b),
                    CrispLayer::Panels(picture) => {
                        let t = chain_filtered(&picture.levels, picture.origin, u, v, fu, fv);
                        ([t[0], t[1], t[2]], t[3])
                    }
                };
                i = j;
                if !(alpha > 0.0 || rgb[0] > 0.0 || rgb[1] > 0.0 || rgb[2] > 0.0) {
                    continue;
                }
                for c in 0..3 {
                    out[c] = out[c] * (1.0 - alpha) + rgb[c];
                }
                out[3] += alpha * (1.0 - out[3]);
            }
        });
        out.map(|c| c * share)
    }

    /// Tube item `item` alone at `p` over a footprint of sides `a` and `b`
    /// (units), from its pieces among `slots` (in line order), premultiplied:
    /// what `draw_stroke` puts into a texel there, each line's nearest piece
    /// filtered exactly across the footprint (a tent: the box over the
    /// footprint twice).
    fn tubes_at(&self, item: &TubeItem, slots: &[u32], p: [f32; 2], a: [f32; 2], b: [f32; 2]) -> ([f32; 3], f32) {
        // Per line (the pieces come in line order): its nearest piece's distance, piece, place along it and widths.
        let (mut e_sum, mut cover_sum, mut most) = (0.0f32, 0.0f32, 0.0f32);
        let mut best: Option<(f32, usize, f32, f32, f32)> = None;
        let half = item.half;
        let mut settle = |best: &mut Option<(f32, usize, f32, f32, f32)>| {
            if let Some((d, k, t, w1, w2)) = best.take() {
                let (e, cover) = self.sums.filtered(d / half, w1 / half, w2 / half);
                let e = e * self.segs[k].along(t, half, &self.look);
                e_sum += e;
                cover_sum += cover;
                most = most.max(e);
            }
        };
        for &n in slots {
            let k = self.slots[n as usize].1 as usize;
            let s = &self.segs[k];
            if best.is_some_and(|(_, j, ..)| self.segs[j].line != s.line) {
                settle(&mut best);
            }
            let (d, t, n) = segment_distance(s, p);
            // The footprint seen across the tube there (its two sides' shares), filtered over twice:
            // a tent as wide at half height as the footprint, whose foot is round where a box's is cut,
            // so a tube far brighter than the display's white does not show the texel grid along its edges.
            let w = (a[0] * n[0] + a[1] * n[1]).abs() + (b[0] * n[0] + b[1] * n[1]).abs();
            let (w1, w2) = (w, w);
            if !(d - w < item.reach) {
                continue;
            }
            if best.is_none_or(|(bd, ..)| d < bd) {
                best = Some((d, k, t, w1, w2));
            }
        }
        settle(&mut best);
        if !(e_sum > 0.0 || cover_sum > 0.0) {
            return ([0.0; 3], 0.0);
        }
        // Held as `draw_stroke` holds a texel: to one tube at its brightest, and to full cover.
        let (e, cover) = (e_sum.min(most.max(1.0)), cover_sum.min(1.0));
        let (color, alpha) = item.paint.at(p[0], p[1]);
        let op = item.opacity * alpha;
        (color.map(|c| c * e * op), (cover * op).min(1.0))
    }
}

/// The rasters kept across builds (module docs), most recently used first.
static RASTERS: Mutex<Vec<(u64, Arc<PatternRaster>)>> = Mutex::new(Vec::new());

/// The raster for `key`, kept or made by `make` (outside the lock). After a
/// raster is made, the kept ones a map still holds stay (a build asks for its
/// lights' rasters one after another, so none it holds is dropped before it
/// asks for the next), and of the others the most recently used up to
/// `RASTERS_SPARE_BYTES`. Under the lock: a scan of the kept entries.
fn kept_raster(key: u64, make: impl FnOnce() -> PatternRaster) -> Arc<PatternRaster> {
    if let Ok(mut kept) = RASTERS.lock() {
        if let Some(at) = kept.iter().position(|(k, _)| *k == key) {
            let entry = kept.remove(at);
            let raster = entry.1.clone();
            kept.insert(0, entry);
            return raster;
        }
    }
    let made = Arc::new(make());
    if let Ok(mut kept) = RASTERS.lock() {
        kept.retain(|(k, _)| *k != key);
        kept.insert(0, (key, made.clone()));
        let mut spare = 0usize;
        kept.retain(|(_, raster)| {
            if Arc::strong_count(raster) > 1 {
                return true;
            }
            spare += raster.bytes();
            spare <= RASTERS_SPARE_BYTES
        });
    }
    made
}

// ---- The light ----

/// A pattern light as the map draws it (`shaped::ShapedKind::Pattern`): its
/// face, its raster and the texel its lookups filter over by default.
#[derive(Clone, Debug)]
pub struct PatternLight {
    frame: LightFrame,
    /// The face's half extents in its plane's units (`space::space_half`).
    half: Vec2f,
    sx: f32,
    raster: Arc<PatternRaster>,
    /// The map texel a lookup without one of its own filters over, radians
    /// (the build width's, 2 pi / width).
    texel: f32,
    cull: CullCone,
    blend: Blend,
    /// Half the tubes' width in the plane's units: the depth the light's softness would take.
    depth: f32,
    /// Its tubes' housings (`housings`), which a room draws.
    housings: Vec<Housing>,
}

impl PatternLight {
    /// `light` is held by clamp() and placed in 3D (its pattern has a face).
    /// `width`: the width of the map it is built for, which sets the raster's
    /// resolution (held to at least `CASTER_REFERENCE_WIDTH`). None for a
    /// pattern that draws nothing.
    pub fn new(light: &LightParams, width: usize) -> Option<PatternLight> {
        let space = light.in_space()?;
        let pattern = light.pattern.clone().unwrap_or_default().clamped();
        let source = pattern.source();
        if source == PatternSource::Svg && pattern.svg.trim().is_empty() {
            return None;
        }
        let frame = space_frame(space);
        let half = space_half(space);
        let texel = std::f32::consts::TAU / width.max(2) as f32;
        // Rastered for a map no narrower than the reference width (the module's notes).
        let ((wf, hf), (wa, ha)) = raster_size(space, std::f32::consts::TAU / width.max(CASTER_REFERENCE_WIDTH) as f32, &light.name);
        let metres_wide = natural_size_m(&pattern).map_or(space.width_m, |[w, _]| w);
        // The raster's key: the own drawing's hash (taken when it was loaded), else the numbers the
        // generator writes its SVG from, so neither is written nor hashed again while it is kept.
        let mut hasher = DefaultHasher::new();
        source.as_str().hash(&mut hasher);
        if source == PatternSource::Svg {
            pattern.svg.key().hash(&mut hasher);
            pattern.svg.len().hash(&mut hasher);
        } else {
            (pattern.rows, pattern.columns).hash(&mut hasher);
            for v in [pattern.cell_m, pattern.tube_m, pattern.gap_m, pattern.angle_deg] {
                v.to_bits().hash(&mut hasher);
            }
        }
        for v in [pattern.look, pattern.mount_spacing_m, pattern.mount_dim, pattern.corner_dim, wf, hf, wa, ha, if pattern.mount_spacing_m > 0.0 { metres_wide } else { 0.0 }] {
            v.to_bits().hash(&mut hasher);
        }
        pattern.housing.is_some().hash(&mut hasher);
        let raster = kept_raster(hasher.finish(), || match source {
            PatternSource::Svg => PatternRaster::build(&pattern.svg, &pattern, (wf, hf), (wa, ha), metres_wide),
            _ => PatternRaster::build(&generate_svg(&pattern), &pattern, (wf, hf), (wa, ha), metres_wide),
        });
        if !raster.lit {
            return None;
        }
        let cos = frame.cover_cos(half).max(-1.0).min(1.0);
        let d = space.distance().max(SPACE_NEAR_M);
        let scale = space.width_m / metres_wide.max(1.0e-6);
        Some(PatternLight {
            frame,
            half,
            sx: box_x_sign(),
            raster,
            texel,
            cull: CullCone::new(frame.center, cos.acos()),
            blend: light.blend(),
            depth: 0.5 * pattern.tube_m * scale / d,
            housings: housings(&pattern, space),
        })
    }

    pub fn cull(&self) -> CullCone {
        self.cull
    }

    pub fn depth(&self) -> f32 {
        self.depth
    }

    /// The picture along `dir` filtered over a map texel `texel` radians
    /// tall (0: the build width's), RGBA; None off the face or behind the
    /// camera.
    pub fn lookup(&self, dir: Vec3f, texel: f32) -> Option<[f32; 4]> {
        if !self.cull.contains(dir) {
            return None;
        }
        let n = self.frame.face?;
        let t = project(&self.frame, dir)?;
        let alpha = if texel > 0.0 { texel } else { self.texel };
        // The texel's two sides on the sky: along the row (its width there,
        // alpha cos elevation) and along the column; through the hit's
        // Jacobian onto the plane.
        let up = vec3f(0.0, 1.0, 0.0);
        let east = Vec3f::cross(up, dir);
        let flat = east.length();
        let (w1, w2) = if flat > 1.0e-6 {
            (east * alpha, Vec3f::cross(dir, east / flat) * alpha)
        } else {
            let side = Vec3f::cross(dir, vec3f(1.0, 0.0, 0.0)).normalize();
            (side * alpha, Vec3f::cross(dir, side) * alpha)
        };
        let dn = dir.dot(n);
        let along = self.frame.center.dot(n) / dn;
        let on_plane = |w: Vec3f| {
            let dp = (w - dir * (w.dot(n) / dn)) * along;
            vec2f(dp.dot(self.frame.right), dp.dot(self.frame.up))
        };
        let (a, b) = (on_plane(w1), on_plane(w2));
        let (wf, hf) = (self.raster.wf, self.raster.hf);
        let (ku, kv) = (wf / (2.0 * self.half.x), hf / (2.0 * self.half.y));
        // The two sides in the picture's texels (u runs with sx along right, v down).
        let a = [self.sx * a.x * ku, -a.y * kv];
        let b = [self.sx * b.x * ku, -b.y * kv];
        let u = (self.sx * t.x / self.half.x + 1.0) * 0.5 * wf;
        let v = (1.0 - t.y / self.half.y) * 0.5 * hf;
        if !(a.iter().chain(&b).all(|x| x.is_finite()) && u.is_finite() && v.is_finite()) {
            return None;
        }
        let out = self.raster.drawn(u, v, a, b);
        (out[0] > 0.0 || out[1] > 0.0 || out[2] > 0.0 || out[3] > 0.0).then_some(out)
    }

    /// Cover and emission along `dir` at the build width's texel: what a
    /// window's glass and the shape core's `sample` read.
    pub fn sample(&self, dir: Vec3f) -> ShapeSample {
        match self.lookup(dir, 0.0) {
            Some(t) => ShapeSample { mask: t[3].min(1.0), emit: luminance(vec3f(t[0], t[1], t[2])) },
            None => ShapeSample::default(),
        }
    }

    /// This light over `acc` in the colour `color` (the light's, through its
    /// beam), its picture filtered over a texel `texel` radians tall: Add adds
    /// the colour times the picture; Multiply scales what is behind by the
    /// colour times the picture's tint as far as it covers, as a flag does.
    pub fn compose(&self, dir: Vec3f, acc: Vec3f, color: Vec3f, texel: f32) -> Vec3f {
        let Some(t) = self.lookup(dir, texel) else { return acc };
        let rgb = vec3f(t[0], t[1], t[2]);
        match self.blend {
            Blend::Add => acc + color * rgb,
            Blend::Multiply => {
                let a = t[3].min(1.0);
                if !(a > 0.0) {
                    return acc;
                }
                let tint = rgb / t[3];
                acc * (Vec3f::all(1.0) + (color * tint - Vec3f::all(1.0)) * a)
            }
        }
    }

    /// Each lit texel of the face at full resolution: its point in the face's
    /// plane (the frame's units: `center + right x + up y`) and its RGB, with
    /// the plane area every texel covers. What the key and the room integrate.
    pub fn for_each_texel(&self, mut f: impl FnMut(Vec2f, Vec3f)) {
        let (hx, hy, sx) = (self.half.x, self.half.y, self.sx);
        self.raster.for_each_lit(|u, v, t| {
            f(vec2f(sx * (2.0 * u - 1.0) * hx, (1.0 - 2.0 * v) * hy), vec3f(t[0], t[1], t[2]));
        });
    }

    /// The plane area one level-0 texel covers (the frame's units squared).
    pub fn texel_area(&self) -> f32 {
        (2.0 * self.half.x / self.raster.wf) * (2.0 * self.half.y / self.raster.hf)
    }

    /// The frame and half extents the face is drawn in.
    pub fn face(&self) -> (LightFrame, Vec2f) {
        (self.frame, self.half)
    }

    /// Its tubes' housings (`housings`; none without `PatternParams::housing`).
    pub fn housings(&self) -> &[Housing] {
        &self.housings
    }
}

/// The raster's size over the face, texels (fractional: the face's own
/// extent), for a map texel `texel` radians tall: a face texel no larger
/// than that texel seen at the face's nearest point, the longer side rounded
/// up to a power of two (`RASTER_MIN_SIDE` at least), held within
/// `RASTER_MAX_SIDE` and `RASTER_MAX_TEXELS` by halving, which it logs once
/// (the tubes are still drawn exactly, `Crisp`). Then the size it would
/// take unheld, the exact tubes' units.
fn raster_size(space: &SpaceParams, texel: f32, name: &str) -> ((f32, f32), (f32, f32)) {
    let frame = space_frame(space);
    let d = space.distance().max(SPACE_NEAR_M);
    let middle = frame.center * d;
    let (hw, hh) = (0.5 * space.width_m, 0.5 * space.height_m);
    let to_camera = -middle;
    let x = to_camera.dot(frame.right).max(-hw).min(hw);
    let y = to_camera.dot(frame.up).max(-hh).min(hh);
    let nearest = (middle + frame.right * x + frame.up * y).length().max(SPACE_NEAR_M);
    let texel_m = (texel * nearest).max(1.0e-6);
    let (need_w, need_h) = ((space.width_m / texel_m).max(1.0), (space.height_m / texel_m).max(1.0));
    let long = need_w.max(need_h);
    let mut side = (long.ceil() as usize).next_power_of_two().max(RASTER_MIN_SIDE) as f32;
    let ratio = (need_w.min(need_h) / long).max(1.0e-6);
    let wanted = side;
    while side > RASTER_MIN_SIDE as f32 && (side > RASTER_MAX_SIDE as f32 || side * (side * ratio).ceil() > RASTER_MAX_TEXELS as f32) {
        side *= 0.5;
    }
    let (w, h) = if need_w >= need_h { (side, (side * ratio).max(1.0)) } else { ((side * ratio).max(1.0), side) };
    let (ww, wh) = if need_w >= need_h { (wanted, (wanted * ratio).max(1.0)) } else { ((wanted * ratio).max(1.0), wanted) };
    if side < wanted {
        log!(
            "hdri pattern: '{name}' would take {:.0} x {:.0} texels on its {:.2} x {:.2} m face; rastered coarser, at {:.0} x {:.0}, its tubes drawn exactly",
            ww,
            wh,
            space.width_m,
            space.height_m,
            w,
            h
        );
    }
    ((w, h), (ww, wh))
}
