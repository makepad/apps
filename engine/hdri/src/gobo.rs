//! Gobos (cookies): a pattern hung in front of a light, evaluated in that
//! light's own rectangle (a stroke's is the square round the stroke itself,
//! `light_fx::face_axes`). B6 has windows (a frame and muntin bars), plain
//! grids of bars and venetian blinds; B7 adds leaves and fern fronds; B8 image
//! masks and the shadow a gobo throws onto the studio backdrop.
//!
//! Positions are face coordinates `q`: x to the right and y up as the viewer
//! sees the light, in tangent units (`LightFx::face`). Patterns are laid out
//! in "s-units", `q / s` with `s` the light's smaller tangent half extent, so
//! a pane or a slat keeps its proportions on a wide light.
//!
//! A gobo passes a fraction 0..=1 of the light and never adds any. Its
//! penumbra is a box filter of the pattern, not a softened edge: blurring
//! moves light about without losing it, so a gobo passes the same share of
//! its light at any `distance`. A window's pattern is separable (glass is
//! "inside a pane across" and "inside a pane down"), and so is a box filter,
//! so the blurred window is exactly the product of two one-axis filters.
//! Leaves and fern (`foliage.rs`) box-filter each blade and stem as a strip
//! across it, which keeps the area of its shade too (D9), so a canopy passes
//! the same share of its light as it moves away, to within a few per cent
//! where blurred shades overlap.
//! An image mask (B8) is box-filtered too, exactly, through a summed-area
//! table of what its texels pass, one per picture, shared by every light and
//! map that shows it (D15).
//!
//! Venetian blinds hang as real ones do (`BlindLayout`): rails, ladders with
//! their lift cords, a tilt wand, the pull cord, and slats that never all sit
//! at one angle.
//!
//! The penumbra takes the light's own shape (`Penumbra`): a far light turns
//! every small gap into an image of itself, so a disc light's dapples are
//! round and a ring light's are rings, where a box made them square (the
//! lighting survey, sheet 03). A rect keeps B6's box. Far enough off (past
//! RASTER_FROM of a leaf), a canopy is read through a raster of its leaves'
//! union filtered by that shape (`CanopyRaster`): its leaves overlap within
//! their clumps, and blurring each on its own and multiplying them darkened
//! it, up to a quarter of its light at small leaves.

use super::*;
use super::foliage::Foliage;
use super::image_light::{PictureMemo, SummedArea};
use super::light_fx::face_axes;
use super::noise::hash01;
use std::sync::{Arc, OnceLock};

/// The shape of a gobo's penumbra: the light's own, shrunk to the penumbra.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Penumbra {
    /// A rect's, and every other shape's but a round one: B6's box, the
    /// penumbra's half-width each way.
    Box,
    /// A disc light's: a disc, the penumbra's half-width its radius; a ring
    /// light's: that ring, its hole `inner` of the radius (0 for a disc).
    Round { inner: f32 },
}

impl Penumbra {
    /// The penumbra of `light`'s gobo: round for a disc or a ring light, the
    /// box for every other shape.
    pub fn of(light: &LightParams) -> Penumbra {
        match light.shape() {
            LightShape::Disc => Penumbra::Round { inner: 0.0 },
            // max/min, not clamp: a NaN reads as no hole.
            LightShape::Ring => Penumbra::Round { inner: light.inner.max(0.0).min(0.95) },
            _ => Penumbra::Box,
        }
    }

    /// Its area over the square of its half-width.
    fn area(self) -> f64 {
        match self {
            Penumbra::Box => 4.0,
            Penumbra::Round { inner } => std::f64::consts::PI * (1.0 - (inner as f64) * (inner as f64)),
        }
    }
}

/// A canopy is read through a raster (`CanopyRaster`) once its penumbra's
/// half-width is this share of its leaf length (a fern's pinnule) or more.
/// Past it the shades of the leaves overlapping within a clump, each blurred
/// on its own and multiplied, darken the canopy (a tree at leaf size 0.3, 60 x
/// 45 degrees, density 0.9, passed 0.066 of the light at distance 1 where
/// sharp it passed 0.090), and a gap smaller than the penumbra does not turn
/// into an image of the light. Short of it the leaves' own filters
/// (foliage.rs) pass the light sharp ones do to within about 1 %.
const RASTER_FROM: f32 = 0.2;
/// A canopy raster's cells per penumbra half-width, and its most cells: past
/// them its cells grow and it reads a little softer than asked.
const RASTER_CELLS_PER_SOFT: f32 = 4.0;
const RASTER_CELLS_MAX: f32 = 262_144.0;
/// The most horizontal slabs a round penumbra is taken as over an image mask's
/// table (`RoundSlabs`): each is two boxes, its band's share of the disc.
const ROUND_SLABS: f64 = 12.0;

/// Venetian blinds' hardware, sized as on a 2 inch blind (51 mm slats on a 46
/// mm pitch) in slat pitches, so it keeps to the slats whatever their number
/// (the lighting survey, sheet 06, and the photographed blinds beside it). The
/// head rail (55 mm) and the bottom rail (28 mm), each held to a share of the
/// light's height, so a coarse blind of a few slats still passes its light.
const HEAD_RAIL: f32 = 1.2;
const HEAD_RAIL_MAX: f32 = 0.08;
const BOTTOM_RAIL: f32 = 0.6;
const BOTTOM_RAIL_MAX: f32 = 0.04;
/// A ladder's two strings (1.2 mm) run down the slats' front and back edges,
/// its lift cord (1.6 mm) between them through the slats' middles.
const LADDER_STRING: f32 = 0.026;
const LIFT_CORD: f32 = 0.035;
/// The outer ladders hang this share of the width in from the blind's ends
/// (at most LADDER_INSET_MAX pitches, 16 cm), and two neighbouring ladders at
/// most LADDER_SPAN pitches (60 cm) apart: two on a square blind of a dozen
/// slats, three or more on a wider or finer one.
const LADDER_INSET: f32 = 0.12;
const LADDER_INSET_MAX: f32 = 3.5;
const LADDER_SPAN: f32 = 13.0;
/// The tilt wand (9 mm across) hangs this share of the width in from the
/// left end (at most WAND_IN_MAX pitches: the tilter sits outside the outer
/// ladder), down to this share of the light's height from its top.
const WAND: f32 = 0.2;
const WAND_IN: f32 = 0.05;
const WAND_IN_MAX: f32 = 1.2;
const WAND_DROP: f32 = 0.66;
/// The pull cord (the lift cords side by side, 4 mm) hangs this share of the
/// width in from the right end (at most PULL_IN_MAX pitches, at the cord
/// lock), down to this share of the height, its tassel (25 x 40 mm) below.
const PULL_CORD: f32 = 0.09;
const PULL_IN: f32 = 0.07;
const PULL_IN_MAX: f32 = 1.5;
const PULL_DROP: f32 = 0.75;
const TASSEL: [f32; 2] = [0.55, 0.9];
/// The wand and the pull cord hang in front of the slats, this many half slat widths.
const IN_FRONT: f32 = 1.3;
/// Vertical vanes (a 76 mm pitch) hang from a head rail (38 mm, at most a
/// share of the height), a bead chain (3 mm) links them this share of the
/// height above the light's foot, and the control chain hangs at the right
/// end, down to this share of the height.
const VANE_RAIL: f32 = 0.5;
const VANE_RAIL_MAX: f32 = 0.05;
const VANE_CHAIN: f32 = 0.04;
const VANE_CHAIN_UP: f32 = 0.04;
const CONTROL_DROP: f32 = 0.85;
/// How far, in degrees, a slat sits off the blind's tilt at most: its
/// ladder's rungs are not all alike (UNEVEN_FLOOR), and their slack shows
/// most half way shut (UNEVEN_TILT more at 45 degrees, as sin 2·tilt). Each
/// slat's share of it is fixed by the gobo's seed.
const UNEVEN_FLOOR: f32 = 1.5;
const UNEVEN_TILT: f32 = 4.5;
/// The salt that keeps the slats' tilts apart from the leaves the same seed grows.
const SLAT_SEED: u32 = 0xb11d_5a75;

use crate::patterns::band_covers;

/// Half-width, in s-units, of the penumbra of a gobo hanging `distance` 0..=1
/// in front of its light. Against the light (0) its edges are sharp; far off
/// (1) they blur over about an eighth of the light's smaller half size, as a
/// cookie's shadow does when it moves away from the lamp.
pub fn penumbra(distance: f32) -> f32 {
    // max/min, not clamp: a NaN distance reads as 0.
    0.002 + 0.12 * distance.max(0.0).min(1.0)
}

/// The share of the blur window `x - w ..= x + w` that falls on glass along
/// one axis of a window. The opening runs from `-inner` to `inner` and holds
/// `n` panes; neighbouring panes are separated by a bar `bar` wide, centred on
/// the pane boundary. With a frame, everything outside the opening is opaque;
/// without one (a grid, or a frame of zero width) the outer panes run on past
/// the light's edge, so the gobo does not darken the light's own edge.
fn panes_open(x: f32, inner: f32, n: u32, bar: f32, framed: bool, w: f32) -> f32 {
    let w = w.max(1.0e-5);
    let pitch = 2.0 * inner / n as f32;
    let (lo, hi) = (x - w, x + w);
    // Only the panes the window can touch.
    let first = (((lo + inner) / pitch).floor().max(0.0) as u32).min(n - 1);
    let last = (((hi + inner) / pitch).floor().max(0.0) as u32).min(n - 1);
    let mut open = 0.0f32;
    for k in first..=last {
        // A pane's glass: its cell, less half a bar on each side that has a neighbour.
        let mut a = -inner + k as f32 * pitch;
        let mut b = a + pitch;
        if k > 0 {
            a += 0.5 * bar;
        } else if !framed {
            a = f32::NEG_INFINITY;
        }
        if k + 1 < n {
            b -= 0.5 * bar;
        } else if !framed {
            b = f32::INFINITY;
        }
        open += (hi.min(b) - lo.max(a)).max(0.0);
    }
    (open / (2.0 * w)).min(1.0)
}

/// A gobo, built once per light when the map is built (`Env::new_with`), then
/// sampled per direction from many threads.
#[derive(Clone, Debug)]
pub struct Gobo {
    kind: GoboKind,
    /// The s-unit: the light's smaller tangent half extent.
    s: f32,
    /// The light's rectangle in s-units: its half extents over `s` (the smaller is 1).
    rect: Vec2f,
    /// Penumbra half-width in s-units, from `distance`.
    soft: f32,
    panes: [u32; 2],
    bar: f32,
    frame: f32,
    /// Half a slat's width, in slat pitches (`slat_width / 2`).
    slat_half: f32,
    /// sin and cos of the slats' tilt from edge-on.
    tilt_sin: f32,
    tilt_cos: f32,
    /// How far in front of the light the gobo hangs, 0..=1: the slits between
    /// tilted slats spread their light that far (`blinds`).
    distance: f32,
    vertical: bool,
    /// Venetian blinds: where their slats and hardware hang.
    blind: Option<BlindLayout>,
    /// The shape of the penumbra: the light's own.
    penumbra: Penumbra,
    /// Leaves or fern fronds, grown once from the seed (B7).
    canopy: Option<Canopy>,
    /// The image mask (B8).
    image: Option<ImageMask>,
    /// A soft image mask's round penumbra, laid out once over its table
    /// (`RoundSlabs`): None for a box, a sharp mask, or a penumbra under a
    /// texel across, which is read as a box.
    round: Option<RoundSlabs>,
}

/// A leaves or fern gobo's canopy, as the gobo reads it.
#[derive(Clone, Debug)]
enum Canopy {
    /// Sharp or nearly: its primitives, each box-filtered across itself (foliage.rs).
    Primitives(Foliage),
    /// Further off: its raster, filtered by the light's own shape.
    Raster(Arc<CanopyRaster>),
}

/// Where a venetian blind's slats and hardware hang (`Gobo::blinds`), s-units.
#[derive(Clone, Debug)]
struct BlindLayout {
    /// The slats' pitch, and the band across them that they fill: between the
    /// rails for horizontal slats; for vertical vanes the light's width, past
    /// which they go on. Slat k pivots (k + 1/2) pitches in from `lo`.
    pitch: f32,
    lo: f32,
    hi: f32,
    /// Each slat's tilt, (sin, cos), the lowest slat (the leftmost vane) first:
    /// the blind's tilt, each slat off it by up to UNEVEN_FLOOR + UNEVEN_TILT
    /// sin(2·tilt) degrees.
    tilts: Vec<[f32; 2]>,
    /// Horizontal slats: the first ladder's x, the ladders' spacing and their number.
    ladder_x0: f32,
    ladder_step: f32,
    ladders: i32,
}

impl BlindLayout {
    /// The blinds of `p` (clamped) over a light `rect` s-units half wide and tall.
    fn new(p: &GoboParams, rect: Vec2f) -> BlindLayout {
        let slats = p.slats.max(1) as f32;
        let (pitch, lo, hi) = if p.vertical {
            (2.0 * rect.x / slats, -rect.x, rect.x)
        } else {
            // The slats hang between the rails, which are sized in pitches.
            let height = 2.0 * rect.y;
            let pitch = height / (slats + HEAD_RAIL + BOTTOM_RAIL);
            let head = (HEAD_RAIL * pitch).min(HEAD_RAIL_MAX * height);
            let bottom = (BOTTOM_RAIL * pitch).min(BOTTOM_RAIL_MAX * height);
            ((height - head - bottom) / slats, -rect.y + bottom, rect.y - head)
        };
        let uneven = UNEVEN_FLOOR + UNEVEN_TILT * (2.0 * p.tilt_deg).to_radians().sin().max(0.0);
        let tilts = (0..p.slats.max(1) as i32)
            .map(|k| {
                let off = uneven * (2.0 * hash01(k, 0, 0, p.seed ^ SLAT_SEED) - 1.0);
                let (sin, cos) = (p.tilt_deg + off).to_radians().sin_cos();
                [sin, cos]
            })
            .collect();
        let width = 2.0 * rect.x;
        let inset = (LADDER_INSET * width).min(LADDER_INSET_MAX * pitch);
        let inner = (width - 2.0 * inset).max(0.0);
        let ladders = ((inner / (LADDER_SPAN * pitch)).ceil() as i32 + 1).max(2);
        BlindLayout { pitch, lo, hi, tilts, ladder_x0: -rect.x + inset, ladder_step: (inner / (ladders - 1) as f32).max(1.0e-6), ladders }
    }
}

/// An image gobo's picture, as the gobo reads it.
#[derive(Clone, Debug)]
enum ImageMask {
    /// Against the light: the picture itself, shared with the app's
    /// ImageSet and read bilinearly (`image_mask`).
    Sharp(Arc<EnvMap>),
    /// Further off: the summed-area table that box-filters it, one per
    /// picture (`shared_pass_table`).
    Soft(Arc<PassTable>),
}

/// The penumbra up to which an image gobo is read as sharp as the picture:
/// `penumbra` of a distance of 1/60 or less.
const SHARP_SOFT: f32 = 0.004;

impl Gobo {
    /// The gobo of `p` over a light with tangent half extents `half`. None
    /// for an image gobo whose name is not in `images`: the map is built
    /// from the parameters and the set alone and never reads a file, so a
    /// name the app has not loaded is simply no gobo.
    pub fn new(p: &GoboParams, half: Vec2f, images: &ImageSet) -> Option<Gobo> {
        Gobo::new_with(p, half, images, Penumbra::Box)
    }

    /// `new` for a light whose own shape the penumbra takes (`Penumbra::of`).
    pub fn new_with(p: &GoboParams, half: Vec2f, images: &ImageSet, shape: Penumbra) -> Option<Gobo> {
        // As clamp() holds it: its ranges and defaults are written once (L:132).
        let p = &p.clamped();
        let kind = p.kind();
        let picture = if kind == GoboKind::Image { Some(images.get(&p.image)?.clone()) } else { None };
        let half = vec2f(half.x.max(1.0e-4), half.y.max(1.0e-4));
        let s = half.x.min(half.y);
        let rect = vec2f(half.x / s, half.y / s);
        let soft = penumbra(p.distance);
        let image = match picture {
            Some(img) if soft <= SHARP_SOFT => Some(ImageMask::Sharp(img)),
            // A picture without texels masks nothing (as `image_mask` reads it).
            Some(img) => shared_pass_table(&img).map(ImageMask::Soft),
            None => None,
        };
        // A soft mask's penumbra is the same disc or ring at every lookup.
        let round = match (&image, shape) {
            (Some(ImageMask::Soft(table)), Penumbra::Round { inner }) => RoundSlabs::new(table, vec2f(soft / rect.x, soft / rect.y), inner as f64),
            _ => None,
        };
        // How fine the canopy is: past RASTER_FROM of it the penumbra reads
        // its raster, whose cells are its mean over them (half a cell's
        // penumbra), and the canopy is binned for that.
        let feature = match kind {
            GoboKind::Leaves => Foliage::leaf_length(rect, p.density, p.scale),
            GoboKind::Fern => crate::foliage::FERN_PINNULE * Foliage::frond_length(rect, p.density, p.scale),
            _ => 0.0,
        };
        let raster = (soft >= RASTER_FROM * feature).then(|| CanopyRaster::side(rect, soft));
        let binned = raster.map_or(soft, |side| 0.5 * side);
        let foliage = match kind {
            GoboKind::Leaves => Some(Foliage::leaves(rect, p.density, p.scale, p.seed, binned)),
            GoboKind::Fern => Some(Foliage::fern(rect, p.density, p.scale, p.seed, binned)),
            _ => None,
        };
        let canopy = foliage.map(|f| match raster {
            Some(side) => Canopy::Raster(Arc::new(CanopyRaster::new(f, side))),
            None => Canopy::Primitives(f),
        });
        let (tilt_sin, tilt_cos) = p.tilt_deg.to_radians().sin_cos();
        let blind = (kind == GoboKind::Blinds).then(|| BlindLayout::new(p, rect));
        Some(Gobo {
            kind,
            s,
            rect,
            soft,
            panes: [p.panes_x, p.panes_y],
            bar: p.bar,
            frame: p.frame,
            slat_half: 0.5 * p.slat_width,
            tilt_sin,
            tilt_cos,
            distance: p.distance,
            vertical: p.vertical,
            blind,
            penumbra: shape,
            canopy,
            image,
            round,
        })
    }

    /// The fraction 0..=1 of the light that passes at face coordinates `q`.
    pub fn transmission(&self, q: Vec2f) -> f32 {
        // A NaN direction passes the light instead of poisoning the map.
        if !(q.x.is_finite() && q.y.is_finite()) {
            return 1.0;
        }
        let p = q / self.s;
        let t = match self.kind {
            GoboKind::Window => self.window(p, true),
            GoboKind::Grid => self.window(p, false),
            GoboKind::Blinds => self.blinds(p, q),
            GoboKind::Leaves | GoboKind::Fern => match &self.canopy {
                Some(Canopy::Primitives(f)) => f.transmission(p),
                Some(Canopy::Raster(raster)) => raster.mean(p, self.soft, self.penumbra),
                None => 1.0,
            },
            GoboKind::Image => self.image.as_ref().map_or(1.0, |mask| self.image_at(mask, p)),
        };
        if t.is_finite() {
            t.max(0.0).min(1.0)
        } else {
            1.0
        }
    }

    /// An image mask at `p` (s-units), stretched over the light's rectangle.
    /// Against the light it is as sharp as the image. Further off it is the
    /// mean of the mask over the penumbra, soft s-units each way in the
    /// light's own shape, as a window's is: a box filter moves light about
    /// and loses none, and it blurs fine detail away instead of ghosting it.
    fn image_at(&self, mask: &ImageMask, p: Vec2f) -> f32 {
        // The unit box: -1..1 over the light's rectangle.
        let b = vec2f(p.x / self.rect.x, p.y / self.rect.y);
        match mask {
            ImageMask::Sharp(img) => image_mask(img, b),
            ImageMask::Soft(table) => pass_mean(table, b, vec2f(self.soft / self.rect.x, self.soft / self.rect.y), self.round.as_ref()),
        }
    }

    /// A window at `p` (s-units): a frame `frame` wide inside the light's
    /// edge (only when `framed`), and muntin bars between the panes inside
    /// it, `bar` of the smaller pane pitch wide, the same width both ways.
    fn window(&self, p: Vec2f, framed: bool) -> f32 {
        let fw = if framed { self.frame } else { 0.0 };
        let inner_x = (self.rect.x - fw).max(1.0e-3);
        let inner_y = (self.rect.y - fw).max(1.0e-3);
        let pitch_x = 2.0 * inner_x / self.panes[0] as f32;
        let pitch_y = 2.0 * inner_y / self.panes[1] as f32;
        let bar = self.bar * pitch_x.min(pitch_y);
        let framed = fw > 0.0;
        panes_open(p.x, inner_x, self.panes[0], bar, framed, self.soft) * panes_open(p.y, inner_y, self.panes[1], bar, framed, self.soft)
    }

    /// Venetian blinds at `p` (s-units; `q` is the same point in tangent
    /// units), hung as real ones hang (`BlindLayout`): what the slats pass
    /// (`slats_open`) past the blind's hardware (`blind_hardware`).
    fn blinds(&self, p: Vec2f, q: Vec2f) -> f32 {
        let Some(blind) = &self.blind else {
            return 1.0;
        };
        // The axis across the slats: up the light for horizontal slats.
        let (a, view) = if self.vertical { (p.x, q.x) } else { (p.y, q.y) };
        let open = self.slats_open(blind, a, view);
        if !(open > 0.0) {
            return 0.0;
        }
        open * self.blind_hardware(blind, p, q)
    }

    /// The share of the light the slats pass at `a` across them, seen along a
    /// ray whose slope across them is `view` (which is the tangent coordinate
    /// itself). `slats` slats pivot on lines one pitch apart; a slat is
    /// `slat_width` pitches deep (1.08 by default, so shut blinds overlap;
    /// below 1 a gap stays open).
    ///
    /// A slat tilted by `tilt` from edge-on is the segment
    /// ±(slat_width/2)·pitch·(sin tilt, cos tilt) in (across the window,
    /// toward the viewer). Seen along the ray it hides a band
    /// (slat_width/2)·pitch·|sin tilt − cos tilt · view| either side of its
    /// pivot, and never less than its own thickness. So open blinds seen
    /// from an angle close a little, and overlapping shut blinds stay shut
    /// from every side. Each slat has its own tilt (`BlindLayout::tilts`), so
    /// the gaps are not all alike.
    ///
    /// The gaps are box-filtered, wider as the blinds close and the further
    /// they hang from the light (the gaps become slits that spread the
    /// light): a gap narrower than the blur dims instead of vanishing, and
    /// shut slats pass nothing. Against the light they are sharp. Horizontal
    /// slats fill the band between the rails, which pass nothing; vertical
    /// vanes go on past the light's sides.
    fn slats_open(&self, blind: &BlindLayout, a: f32, view: f32) -> f32 {
        let pitch = blind.pitch;
        // D9 (L:137): a slit spreads its light up to a quarter pitch each way,
        // as far as the blinds hang from the light, as the penumbra does:
        // against it (distance 0) they are sharp, as the Distance row
        // promises, where a spread held at a quarter pitch left shut 0.8-pitch
        // slats showing wide bands of sky at a third of it.
        let w = (self.soft + 0.25 * pitch * self.tilt_sin * self.distance).min(0.45 * pitch).max(1.0e-5);
        let (lo, hi) = if self.vertical { (a - w, a + w) } else { ((a - w).max(blind.lo), (a + w).min(blind.hi)) };
        if !(hi > lo) {
            return 0.0;
        }
        // The slats whose band can reach the box: a band reaches at most
        // (slat_width/2)·sqrt(1 + view²) pitches from its pivot.
        let reach = (self.slat_half * (1.0 + view * view).sqrt()).max(0.03);
        let n = blind.tilts.len() as i64;
        let mut first = ((lo - blind.lo) / pitch - 0.5 - reach).floor() as i64;
        let mut last = ((hi - blind.lo) / pitch - 0.5 + reach).ceil() as i64;
        if !self.vertical {
            first = first.max(0);
            last = last.min(n - 1);
        }
        // The gaps between their bands within the box, in order: where shut
        // slats overlap there are none, and the blinds pass exactly nothing.
        let (mut open, mut covered_to) = (0.0f32, lo);
        for k in first..=last {
            let pivot = blind.lo + (k as f32 + 0.5) * pitch;
            // Vanes past the light's sides repeat its vanes' tilts.
            let [sin, cos] = blind.tilts[k.rem_euclid(n) as usize];
            let band = (self.slat_half * pitch * (sin - cos * view).abs()).max(0.03 * pitch);
            if pivot - band > covered_to {
                open += (pivot - band).min(hi) - covered_to;
            }
            covered_to = covered_to.max(pivot + band);
            if covered_to >= hi {
                break;
            }
        }
        if covered_to < hi {
            open += hi - covered_to;
        }
        (open / (2.0 * w)).max(0.0).min(1.0)
    }

    /// What the blind's hardware lets through at `p` (s-units; `q` the same
    /// point in tangent units), each part box-filtered by the gobo's
    /// penumbra as a window's bars are. Horizontal slats: the ladders, each two
    /// strings down the slats' front and back edges (so seen from the side
    /// they part, by their depth times the ray's slope) with the lift cord
    /// between them, and the tilt wand and the pull cord with its tassel,
    /// which hang in front of the slats; the rails are `slats_open`'s. Vertical
    /// vanes: the head rail, the chain linking the vanes near their foot and
    /// the control chain.
    fn blind_hardware(&self, blind: &BlindLayout, p: Vec2f, q: Vec2f) -> f32 {
        let w = self.soft.max(1.0e-5);
        let (hx, hy, pitch) = (self.rect.x, self.rect.y, blind.pitch);
        let (width, height) = (2.0 * hx, 2.0 * hy);
        // A rod `thick` across at x, hanging from above the light down to `bottom`.
        let rod = |x: f32, thick: f32, bottom: f32| 1.0 - band_covers(p.x, x - 0.5 * thick, x + 0.5 * thick, w) * band_covers(p.y, bottom, f32::INFINITY, w);
        let pull_in = (PULL_IN * width).min(PULL_IN_MAX * pitch);
        if self.vertical {
            let rail = (VANE_RAIL * pitch).min(VANE_RAIL_MAX * height);
            let chain = -hy + VANE_CHAIN_UP * height;
            let half_chain = 0.5 * VANE_CHAIN * pitch;
            return (1.0 - band_covers(p.y, hy - rail, f32::INFINITY, w))
                * (1.0 - band_covers(p.y, chain - half_chain, chain + half_chain, w))
                * rod(hx - pull_in, VANE_CHAIN * pitch, hy - CONTROL_DROP * height);
        }
        let mut t = 1.0;
        // The ladders either side of p.
        let depth = (self.slat_half * pitch * self.tilt_cos * q.x).abs();
        let (string, cord) = (0.5 * LADDER_STRING * pitch, 0.5 * LIFT_CORD * pitch);
        let near = ((p.x - blind.ladder_x0) / blind.ladder_step).round() as i32;
        for k in (near - 1).max(0)..=(near + 1).min(blind.ladders - 1) {
            let x = blind.ladder_x0 + k as f32 * blind.ladder_step;
            // Straight on the strings and the cord are one band; from the side three.
            let covered = if depth - string <= cord {
                let reach = (depth + string).max(cord);
                band_covers(p.x, x - reach, x + reach, w)
            } else {
                band_covers(p.x, x - depth - string, x - depth + string, w) + band_covers(p.x, x - cord, x + cord, w) + band_covers(p.x, x + depth - string, x + depth + string, w)
            };
            t *= (1.0 - covered).max(0.0);
        }
        // The wand at the left end and the pull cord at the right hang in front of the slats.
        let front = IN_FRONT * self.slat_half * pitch * q.x;
        t *= rod(-hx + (WAND_IN * width).min(WAND_IN_MAX * pitch) - front, WAND * pitch, hy - WAND_DROP * height);
        let (pull, end) = (hx - pull_in - front, hy - PULL_DROP * height);
        t *= rod(pull, PULL_CORD * pitch, end);
        let tassel = band_covers(p.x, pull - 0.5 * TASSEL[0] * pitch, pull + 0.5 * TASSEL[0] * pitch, w) * band_covers(p.y, end - TASSEL[1] * pitch, end, w);
        t * (1.0 - tassel)
    }
}

/// What one texel of an image mask passes, 0..=1.
/// - An opaque texel passes its linear luminance, clamped to 0..=1, so an HDR
///   picture only blocks light and never adds any.
/// - A transparent texel passes everything, whatever its colour: since B14
///   an 8-bit file keeps its alpha (straight), so a cut-out PNG masks as it
///   is drawn.
fn texel_passes(t: &[f32; 4]) -> f32 {
    // max/min, not clamp: a NaN reads as 0, so a NaN alpha is transparent.
    let pass = crate::sky::luminance(vec3f(t[0], t[1], t[2])).max(0.0).min(1.0);
    let alpha = t[3].max(0.0).min(1.0);
    1.0 - alpha * (1.0 - pass)
}

/// Transmission of an image mask at unit-box point `b` (-1..1 each way, x to
/// the viewer's right, y up; the image's top row is the light's top).
/// Bilinear in what the texels pass (`texel_passes`), not in their colours:
/// a colour kept under a transparent texel then never fringes an opaque
/// edge (as B14's premultiplied lookup does for a light's picture), and a
/// bright HDR texel next to a black one dims to their mean instead of
/// passing everything. Held inside the texel centres rather than wrapped:
/// a mask has edges, an equirect does not.
fn image_mask(img: &EnvMap, b: Vec2f) -> f32 {
    let (w, h) = (img.width, img.height);
    if w == 0 || h == 0 || img.data.len() != w * h {
        return 1.0;
    }
    // Texel space, texel centres on the integers; max/min keep a NaN at 0.
    let fx = ((0.5 + 0.5 * b.x) * w as f32 - 0.5).max(0.0).min((w - 1) as f32);
    let fy = ((0.5 - 0.5 * b.y) * h as f32 - 0.5).max(0.0).min((h - 1) as f32);
    let (x0, y0) = (fx.floor() as usize, fy.floor() as usize);
    let (x1, y1) = ((x0 + 1).min(w - 1), (y0 + 1).min(h - 1));
    let (tx, ty) = (fx - x0 as f32, fy - y0 as f32);
    let at = |x: usize, y: usize| texel_passes(&img.data[y * w + x]);
    let top = at(x0, y0) + (at(x1, y0) - at(x0, y0)) * tx;
    let bottom = at(x0, y1) + (at(x1, y1) - at(x0, y1)) * tx;
    top + (bottom - top) * ty
}

/// Prefix sums of what an image mask's texels pass (`texel_passes`), one
/// number a texel (`image_light::SummedArea`): the exact mean of the mask
/// over a box of any size from sixteen reads. At 2048 x 2048 that is 33.6 MB
/// and about 64 ms in release (L:174), so a picture has one table, built for
/// the first gobo that hangs off its light and shared by every other light,
/// cast and `Env` that shows it (`shared_pass_table`, D15).
type PassTable = SummedArea<1>;

/// The table of every live gobo picture (`image_light::PictureMemo`).
static TABLES: PictureMemo<PassTable> = PictureMemo::new();

/// Drops the table of every picture whose last `Arc` is gone
/// (`composite::drop_dead_facts` calls this).
pub(super) fn drop_dead_tables() {
    TABLES.drop_dead();
}

/// The table of `img`, built on the first call for the picture and shared
/// from then on. None for a picture without texels.
fn shared_pass_table(img: &Arc<EnvMap>) -> Option<Arc<PassTable>> {
    let (w, h) = (img.width, img.height);
    if w == 0 || h == 0 || img.data.len() != w * h {
        return None;
    }
    Some(TABLES.get(img, |img| {
        #[cfg(test)]
        crate::probe::count(crate::probe::Work::PassTable);
        SummedArea::build(img, |t| [texel_passes(t) as f64])
    }))
}

/// The mean of the mask over the penumbra round unit-box point `b` (placed as
/// `image_mask` places it) with half-widths `r`, in unit-box units: the box,
/// or for a round light (`Penumbra::Round`) the disc or ring laid out in
/// `round`. The box is never narrower than a texel (`SummedArea::box_mean`),
/// so a soft gobo is never blockier than a sharp one; a round penumbra under
/// a texel across is read as that box (`RoundSlabs::new` lays out none).
fn pass_mean(table: &PassTable, b: Vec2f, r: Vec2f, round: Option<&RoundSlabs>) -> f32 {
    let (w, h) = (table.width() as f64, table.height() as f64);
    let x = (0.5 + 0.5 * b.x as f64) * w;
    let y = (0.5 - 0.5 * b.y as f64) * h;
    match round {
        Some(round) => round.mean(table, x, y),
        None => table.box_mean(x, y, 0.5 * r.x as f64 * w, 0.5 * r.y as f64 * h)[0] as f32,
    }
}

/// A round penumbra over an image mask's table, the same at every lookup of
/// its gobo, so it is laid out once (`Gobo::new_with`): the ellipse with half
/// axes `rx`, `ry` in texels (a round penumbra on the light, stretched as the
/// picture is), less its hole `inner` of that for a ring, as horizontal slabs,
/// up to ROUND_SLABS and none under a texel tall, each as wide as the disc's
/// area in its band (`chord_mean`) and read as one box of the table. A slab
/// under half a texel wide is read half a texel wide, and the mean is over
/// the boxes as read, so the light stays.
#[derive(Clone, Debug)]
struct RoundSlabs {
    /// Each box: its middle's offset from the lookup's row, its half width
    /// and half height (texels), +1 for the disc's or -1 for the hole's, and
    /// its area.
    boxes: Vec<RoundBox>,
    /// The boxes' signed area together.
    area: f64,
    /// The half axes, for the box a penumbra with no area left is read as.
    rx: f64,
    ry: f64,
}

#[derive(Clone, Copy, Debug)]
struct RoundBox {
    dy: f64,
    half_x: f64,
    half_y: f64,
    sign: f64,
    area: f64,
}

impl RoundSlabs {
    /// The disc or ring with hole `inner` over `table` for a penumbra of
    /// half-widths `r` (unit-box units, as `pass_mean` takes them): None
    /// where it is under a texel across either way, which is read as a box.
    fn new(table: &PassTable, r: Vec2f, inner: f64) -> Option<RoundSlabs> {
        let (w, h) = (table.width() as f64, table.height() as f64);
        let (rx, ry) = (0.5 * r.x as f64 * w, 0.5 * r.y as f64 * h);
        if !(rx >= 1.0 && ry >= 1.0) {
            return None;
        }
        let n = (2.0 * ry).floor().max(2.0).min(ROUND_SLABS) as usize;
        let (mut boxes, mut area) = (Vec::with_capacity(2 * n), 0.0f64);
        for k in 0..n {
            let (v0, v1) = (-1.0 + 2.0 * k as f64 / n as f64, -1.0 + 2.0 * (k + 1) as f64 / n as f64);
            let (dy, half_y) = (0.5 * (v0 + v1) * ry, (0.5 * (v1 - v0) * ry).max(0.5));
            for (radius, sign) in [(1.0, 1.0), (inner, -1.0)] {
                let half_x = chord_mean(v0, v1, radius) * rx;
                // No disc in this band, or a sliver of the hole under half a texel: nothing to take.
                if !(half_x > 0.0) || (sign < 0.0 && half_x < 0.5) {
                    continue;
                }
                let half_x = half_x.max(0.5);
                let box_area = 4.0 * half_x * half_y;
                boxes.push(RoundBox { dy, half_x, half_y, sign, area: box_area });
                area += sign * box_area;
            }
        }
        Some(RoundSlabs { boxes, area, rx, ry })
    }

    /// The mean of the mask over the penumbra round (x, y), in texels.
    fn mean(&self, table: &PassTable, x: f64, y: f64) -> f32 {
        if !(self.area > 0.0) {
            return table.box_mean(x, y, self.rx, self.ry)[0] as f32;
        }
        let mut sum = 0.0f64;
        for b in &self.boxes {
            sum += b.sign * table.box_mean(x, y + b.dy, b.half_x, b.half_y)[0] * b.area;
        }
        (sum / self.area) as f32
    }
}

/// The mean half-chord over the band a..b of a disc of `radius` centred on 0:
/// its area in the band over twice the band's height. 0 for no disc.
fn chord_mean(a: f64, b: f64, radius: f64) -> f64 {
    if !(radius > 0.0 && b > a) {
        return 0.0;
    }
    0.5 * radius * radius * (disc_area(b / radius) - disc_area(a / radius)) / (b - a)
}

/// Points of `disc_area`'s table over -1..=1.
const DISC_AREA_STEPS: usize = 2048;

/// The area of the unit disc below the line y = v, -1..=1 (held past it): 2
/// ∫ sqrt(1 - t²) dt from -1, from a table, linear between its points (the
/// table is exact at them; between, off by less than 1e-5 of the disc's
/// area, worst at its rim).
fn disc_area(v: f64) -> f64 {
    static TABLE: OnceLock<Vec<f64>> = OnceLock::new();
    let table = TABLE.get_or_init(|| {
        (0..=DISC_AREA_STEPS)
            .map(|k| {
                let v = (-1.0 + 2.0 * k as f64 / DISC_AREA_STEPS as f64).max(-1.0).min(1.0);
                v * (1.0 - v * v).max(0.0).sqrt() + v.asin() + 0.5 * std::f64::consts::PI
            })
            .collect()
    });
    // max/min, not clamp: a NaN reads as the bottom of the disc.
    let x = ((v + 1.0) * 0.5 * DISC_AREA_STEPS as f64).max(0.0).min(DISC_AREA_STEPS as f64);
    let k = (x as usize).min(DISC_AREA_STEPS - 1);
    table[k] + (x - k as f64) * (table[k + 1] - table[k])
}

/// A canopy far from its light (`RASTER_FROM`): the union of its leaves and
/// stems rastered over the rectangle they are binned over, each cell its
/// mean over the cell (their own filters at half a cell, foliage.rs), and the
/// penumbra the raster's mean over the light's own shape (`Penumbra`). So
/// leaves overlapping within a clump shade as one, and a gap smaller than the
/// penumbra turns into an image of the light, a round dapple under a disc.
/// Its rows are filled on their first read, by whichever bake thread reads
/// them (a row is a function of its index alone, so a bake equals its
/// parallel bake, as `lazy_grid`'s are), and kept as running sums, so a box
/// or a disc of any size costs two reads a row it spans. Past its rectangle
/// the edge cells hold, as an image mask's do.
struct CanopyRaster {
    /// The canopy binned for half a cell's penumbra: a lookup at a cell's
    /// middle is its mean over the cell.
    cells: Foliage,
    /// The raster's lower left corner, s-units, a cell's side, and its size.
    min: Vec2f,
    side: f32,
    nx: usize,
    ny: usize,
    /// Each row's running sums from its left end, nx + 1 of them.
    rows: Vec<OnceLock<Box<[f64]>>>,
}

/// Its shape only: which rows are filled depends on what has been read.
impl std::fmt::Debug for CanopyRaster {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CanopyRaster").field("side", &self.side).field("nx", &self.nx).field("ny", &self.ny).finish()
    }
}

impl CanopyRaster {
    /// A cell's side, s-units, for a canopy over the light's rectangle `rect`
    /// (s-units, its glow GLOW past it) and a penumbra of half-width `soft`:
    /// RASTER_CELLS_PER_SOFT cells to it, at most RASTER_CELLS_MAX cells.
    fn side(rect: Vec2f, soft: f32) -> f32 {
        let half = rect + Vec2f::all(crate::foliage::GLOW);
        (soft / RASTER_CELLS_PER_SOFT).max((4.0 * half.x * half.y / RASTER_CELLS_MAX).sqrt())
    }

    /// The raster of `foliage`, binned for half a cell's penumbra, cells `side` wide.
    fn new(foliage: Foliage, side: f32) -> CanopyRaster {
        let half = foliage.binned_half();
        let (nx, ny) = (((2.0 * half.x / side).ceil() as usize).max(1), ((2.0 * half.y / side).ceil() as usize).max(1));
        CanopyRaster { cells: foliage, min: vec2f(-half.x, -half.y), side, nx, ny, rows: (0..ny).map(|_| OnceLock::new()).collect() }
    }

    /// Row `j`'s running sums, filled on its first read.
    fn row(&self, j: usize) -> &[f64] {
        self.rows[j].get_or_init(|| {
            let y = self.min.y + (j as f32 + 0.5) * self.side;
            let mut sums = Vec::with_capacity(self.nx + 1);
            let mut run = 0.0f64;
            sums.push(run);
            for i in 0..self.nx {
                run += self.cells.transmission(vec2f(self.min.x + (i as f32 + 0.5) * self.side, y)) as f64;
                sums.push(run);
            }
            sums.into_boxed_slice()
        })
    }

    /// A row's cells integrated from x0 to x1, in cells, its end cells held
    /// past its ends.
    fn span(row: &[f64], x0: f64, x1: f64) -> f64 {
        let n = row.len() - 1;
        let at = |x: f64| {
            // `as usize` saturates: left of the row is its first cell.
            let i = (x.floor() as usize).min(n - 1);
            row[i] + (x - i as f64) * (row[i + 1] - row[i])
        };
        at(x1) - at(x0)
    }

    /// The raster's mean over the penumbra round `p` (s-units): the box of
    /// half-width `soft`, or the disc or ring of that radius, taken row by row,
    /// each row's band as wide as the shape's area in it (a disc's: its
    /// integral `disc_area` across the band over the band's height).
    fn mean(&self, p: Vec2f, soft: f32, penumbra: Penumbra) -> f32 {
        let cx = ((p.x - self.min.x) / self.side) as f64;
        let cy = ((p.y - self.min.y) / self.side) as f64;
        let r = (soft / self.side).max(1.0e-3) as f64;
        let inner = match penumbra {
            Penumbra::Round { inner } => inner as f64,
            Penumbra::Box => 0.0,
        };
        let (lo, hi) = (cy - r, cy + r);
        let mut sum = 0.0f64;
        let mut j = lo.floor();
        // The disc's and its hole's area below the band's lower edge, each over its radius squared.
        let (mut outer_below, mut hole_below) = (disc_area(-1.0), disc_area(-1.0));
        while j < hi {
            let (a, b) = (lo.max(j), hi.min(j + 1.0));
            // max/min: the rows past the raster hold its edge rows.
            let row = self.row((j.max(0.0) as usize).min(self.ny - 1));
            sum += match penumbra {
                Penumbra::Box => (b - a) * CanopyRaster::span(row, cx - r, cx + r),
                Penumbra::Round { .. } => {
                    // Half the band's share of the disc (and of its hole), in cells².
                    let outer_to = disc_area((b - cy) / r);
                    let outer = 0.5 * r * r * (outer_to - outer_below);
                    outer_below = outer_to;
                    let mut band = CanopyRaster::span(row, cx - outer / (b - a), cx + outer / (b - a)) * (b - a);
                    if inner > 0.0 {
                        let ri = inner * r;
                        let hole_to = disc_area(((b - cy) / ri).max(-1.0).min(1.0));
                        let hole = 0.5 * ri * ri * (hole_to - hole_below);
                        hole_below = hole_to;
                        if hole > 0.0 {
                            band -= CanopyRaster::span(row, cx - hole / (b - a), cx + hole / (b - a)) * (b - a);
                        }
                    }
                    band
                }
            };
            j += 1.0;
        }
        (sum / (penumbra.area() * r * r)) as f32
    }
}

/// The studio a projected gobo lands on, in units of the camera's height
/// above the floor: the floor is the plane one unit below the camera, the
/// cyc wall a cylinder WALL_RADIUS round it.
///
/// A light in a map is infinitely far off, so its rays arrive parallel: its
/// gobo's shadow is the light's rectangle laid along them onto the floor,
/// round the spot the camera sees under the light, as far below the horizon
/// as the light is above it (held three quarters of the way to the wall,
/// SPOT_REACH, so a low light's shadow stays in the room). The rectangle is
/// the light as wide as it looks from PROJECTOR_DISTANCE: a 40 degree light
/// throws a pattern 1.1 units across its beam, stretched along the floor by
/// 1 / sin(elevation). At 45 degrees that is a patch under the light from
/// about 30 to 77 degrees down, a tenth of the lower half's solid angle; the
/// rest of the backdrop is untouched.
pub const FLOOR_HEIGHT: f32 = 1.0;
pub const WALL_RADIUS: f32 = 4.0;
pub const PROJECTOR_DISTANCE: f32 = 1.5;
const SPOT_REACH: f32 = 0.75 * WALL_RADIUS;
/// How dark a projected gobo's shadow gets: the studio's fill keeps it off black.
pub const SHADOW_DEPTH: f32 = 0.8;

/// A light whose gobo is cast: an enabled emitter that asks for it. Never a
/// shadow caster, whose pattern only darkens the map (redesign §5).
fn casts(l: &LightParams) -> bool {
    l.enabled && !l.is_caster() && l.blend() == Blend::Add && l.gobo.as_ref().map_or(false, |g| g.project_on_backdrop)
}

/// A gobo's shadow on the studio backdrop, thrown from its light's own
/// direction: the key light when it casts, else the first light that does.
/// An environment map has no geometry, so this paints the shadow a cookie
/// would throw onto a floor and a cyc wall. It cannot fall across a subject
/// in the scene; that is the engine light-cookie stretch goal.
#[derive(Clone, Debug)]
pub struct BackdropGobo {
    /// Toward the light; the light's right as the viewer facing it sees it; its up.
    center: Vec3f,
    right: Vec3f,
    up: Vec3f,
    /// The light's tangent half extents: the beam's rectangle.
    half: Vec2f,
    /// The point of the floor under the light that the beam is centred on.
    spot: Vec3f,
    gobo: Gobo,
}

impl BackdropGobo {
    /// The shadow of the key light's gobo when the key casts one, else of the
    /// first light that does. None when no light casts, or the caster's gobo
    /// draws nothing (an image whose name is not in `images`).
    pub fn new(lights: &[LightParams], images: &ImageSet) -> Option<BackdropGobo> {
        let light = lights.iter().find(|l| l.key && casts(l)).or_else(|| lights.iter().find(|l| casts(l)))?;
        let (frame, sx, half) = face_axes(light);
        let gobo = Gobo::new_with(light.gobo.as_ref()?, half, images, Penumbra::of(light))?;
        // The spot is where a ray as far below the horizon as the light is
        // above it meets the floor: FLOOR_HEIGHT / tan(elevation) out.
        let c = frame.center;
        let toward = vec3f(c.x, 0.0, c.z);
        let h = toward.length();
        let out = if c.y > 1.0e-4 { (FLOOR_HEIGHT * h / c.y).min(SPOT_REACH) } else { SPOT_REACH };
        let under = if h > 1.0e-6 { toward * (out / h) } else { Vec3f::default() };
        let spot = under + vec3f(0.0, -FLOOR_HEIGHT, 0.0);
        Some(BackdropGobo { center: c, right: frame.right * sx, up: frame.up, half, spot, gobo })
    }

    /// The factor 1 - SHADOW_DEPTH ..= 1 by which the backdrop seen along
    /// `d` (unit, in the map's own frame) is darkened.
    pub fn shade(&self, d: Vec3f) -> f32 {
        // Where the view ray meets the floor or the wall, whichever is nearer.
        let horizontal = (d.x * d.x + d.z * d.z).sqrt();
        let t_floor = if d.y < -1.0e-4 { FLOOR_HEIGHT / -d.y } else { f32::INFINITY };
        let t_wall = if horizontal > 1.0e-4 { WALL_RADIUS / horizontal } else { f32::INFINITY };
        let t = t_floor.min(t_wall);
        if !t.is_finite() {
            return 1.0;
        }
        let p = d * t;
        // A surface turned away from the light gets none of it, so no shadow either.
        let normal = if t_floor <= t_wall { vec3f(0.0, 1.0, 0.0) } else { vec3f(-p.x, 0.0, -p.z).normalize() };
        if !(normal.dot(self.center) > 0.0) {
            return 1.0;
        }
        // The rays are parallel, so a surface point's place in the beam is
        // its offset from the spot across them, in the light's rectangle as
        // it looks PROJECTOR_DISTANCE away. The pattern lands the way the
        // light shows it, right side to the right; its top falls furthest
        // from the light, as a shadow's does.
        let o = p - self.spot;
        let q = vec2f(o.dot(self.right), o.dot(self.up)) / PROJECTOR_DISTANCE;
        // The cookie fades out over the last tenth of the beam, so the
        // shadow has no hard border where the beam ends.
        let rim = (q.x.abs() / self.half.x).max(q.y.abs() / self.half.y);
        let inside = 1.0 - smoothstep(0.9, 1.0, rim);
        if !(inside > 0.0) {
            return 1.0;
        }
        1.0 - SHADOW_DEPTH * inside * (1.0 - self.gobo.transmission(q))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gobo(p: GoboParams, half: Vec2f) -> Gobo {
        Gobo::new(&p, half, &ImageSet::default()).expect("a gobo")
    }

    /// Mean transmission over the light's rectangle, n x n samples.
    fn mean_over(g: &Gobo, half: Vec2f, n: usize) -> f32 {
        let mut sum = 0.0f64;
        for j in 0..n {
            for i in 0..n {
                let q = vec2f(
                    ((i as f32 + 0.5) / n as f32 * 2.0 - 1.0) * half.x,
                    ((j as f32 + 0.5) / n as f32 * 2.0 - 1.0) * half.y,
                );
                sum += g.transmission(q) as f64;
            }
        }
        (sum / (n * n) as f64) as f32
    }

    #[test]
    fn a_window_passes_its_panes_and_blocks_its_frame_and_bars() {
        // half (0.4, 0.3): s = 0.3, so the light is 1.333 x 1 s-units. A 0.1 frame
        // leaves 1.233 x 0.9 inside; two panes across put one bar on x = 0.
        let g = gobo(GoboParams { panes_x: 2, panes_y: 1, bar: 0.2, frame: 0.1, distance: 0.0, ..GoboParams::default() }, vec2f(0.4, 0.3));
        assert!(g.transmission(vec2f(0.185, 0.0)) > 0.99, "the right pane's middle");
        assert!(g.transmission(vec2f(-0.185, 0.0)) > 0.99, "the left pane's middle");
        assert!(g.transmission(vec2f(0.0, 0.0)) < 0.01, "the muntin between the panes");
        assert!(g.transmission(vec2f(0.0, 0.29)) < 0.01, "the frame along the top");
        assert!(g.transmission(vec2f(0.395, 0.0)) < 0.01, "the frame down the side");
    }

    #[test]
    fn a_grid_has_bars_but_no_frame() {
        let p = GoboParams { panes_x: 3, panes_y: 3, bar: 0.1, frame: 0.3, distance: 0.0, ..GoboParams::default() };
        let grid = gobo(GoboParams { kind: "grid".to_string(), ..p.clone() }, vec2f(0.4, 0.3));
        let window = gobo(GoboParams { kind: "window".to_string(), ..p }, vec2f(0.4, 0.3));
        // s-units (1.2, 0): inside a grid cell, but inside a 0.3 window frame.
        assert!(grid.transmission(vec2f(0.36, 0.0)) > 0.99);
        assert!(window.transmission(vec2f(0.36, 0.0)) < 0.01);
        // On a bar both are dark: three panes across put bars at a third of the width.
        assert!(grid.transmission(vec2f(0.4 / 3.0, 0.0)) < 0.01);
    }

    /// Mean transmission of blinds where only their slats hang, n x n
    /// samples: the band between the rails, over the middle half of the
    /// width, clear of the ladders, the wand and the pull cord.
    fn slats_mean(g: &Gobo, n: usize) -> f32 {
        let blind = g.blind.as_ref().expect("blinds");
        let mut sum = 0.0f64;
        for j in 0..n {
            for i in 0..n {
                let p = vec2f(0.5 * g.rect.x * ((i as f32 + 0.5) / n as f32 * 2.0 - 1.0), blind.lo + (blind.hi - blind.lo) * (j as f32 + 0.5) / n as f32);
                sum += g.transmission(p * g.s) as f64;
            }
        }
        (sum / (n * n) as f64) as f32
    }

    /// Mean transmission of the slats of 10-slat blinds over a 20 degree light.
    fn blinds_mean(tilt_deg: f32) -> f32 {
        let h = 10f32.to_radians().tan();
        slats_mean(&gobo(GoboParams { kind: "blinds".to_string(), slats: 10, tilt_deg, distance: 0.0, ..GoboParams::default() }, vec2f(h, h)), 200)
    }

    #[test]
    fn blinds_close_as_they_tilt() {
        assert!(blinds_mean(0.0) > 0.8, "open blinds pass most of the light: {}", blinds_mean(0.0));
        let m: Vec<f32> = [30.0, 50.0, 70.0, 90.0].iter().map(|&t| blinds_mean(t)).collect();
        for w in m.windows(2) {
            assert!(w[0] > w[1], "{m:?}");
        }
        assert_eq!(m[3], 0.0, "shut blinds pass nothing, from any side: {m:?}");
    }

    #[test]
    fn narrow_slats_leave_a_gap_when_shut() {
        let h = 10f32.to_radians().tan();
        let mean = |tilt_deg: f32, slat_width: f32| {
            let p = GoboParams { kind: "blinds".to_string(), slats: 10, tilt_deg, slat_width, distance: 0.0, ..GoboParams::default() };
            slats_mean(&gobo(p, vec2f(h, h)), 200)
        };
        // Shut, slats 0.8 pitches wide leave 0.2 of every pitch open; the
        // default 1.08 overlap and pass nothing.
        let shut = mean(90.0, 0.8);
        assert!((shut - 0.2).abs() < 0.02, "{shut}");
        assert_eq!(mean(90.0, GoboParams::default().slat_width), 0.0);
        // Narrower slats pass more at every tilt.
        for tilt in [0.0f32, 30.0, 60.0, 90.0] {
            assert!(mean(tilt, 0.8) > mean(tilt, 1.2), "tilt {tilt}");
        }
    }

    #[test]
    fn vertical_blinds_run_the_other_way() {
        let half = vec2f(0.3, 0.3);
        let make = |vertical: bool| {
            gobo(GoboParams { kind: "blinds".to_string(), slats: 8, tilt_deg: 30.0, vertical, distance: 0.0, ..GoboParams::default() }, half)
        };
        let (h, v) = (make(false), make(true));
        for i in 0..50 {
            let a = -0.29 + 0.58 * i as f32 / 49.0;
            // Horizontal slats vary only up the light, vertical ones only across it.
            assert_eq!(h.transmission(vec2f(-0.2, a)), h.transmission(vec2f(0.2, a)));
            assert_eq!(v.transmission(vec2f(a, -0.2)), v.transmission(vec2f(a, 0.2)));
        }
        let spread = |g: &Gobo, up: bool| {
            let ts: Vec<f32> = (0..200)
                .map(|i| {
                    let a = -0.29 + 0.58 * i as f32 / 199.0;
                    g.transmission(if up { vec2f(0.0, a) } else { vec2f(a, 0.0) })
                })
                .collect();
            ts.iter().cloned().fold(0.0f32, f32::max) - ts.iter().cloned().fold(1.0f32, f32::min)
        };
        assert!(spread(&h, true) > 0.5 && spread(&v, false) > 0.5);
    }

    #[test]
    fn a_gobo_only_ever_passes_part_of_the_light() {
        for kind in ["window", "blinds", "grid", "leaves", "fern"] {
            for distance in [0.0f32, 0.5, 1.0] {
                let g = gobo(GoboParams { kind: kind.to_string(), distance, ..GoboParams::default() }, vec2f(0.5, 0.2));
                for j in 0..41 {
                    for i in 0..41 {
                        // Well past the light's edges too.
                        let q = vec2f(-0.8 + 1.6 * i as f32 / 40.0, -0.4 + 0.8 * j as f32 / 40.0);
                        let t = g.transmission(q);
                        assert!(t.is_finite() && (0.0..=1.0).contains(&t), "{kind} at {q:?}: {t}");
                    }
                }
                assert_eq!(g.transmission(vec2f(f32::NAN, f32::NAN)), 1.0, "a NaN direction passes instead of poisoning the map");
            }
        }
    }

    #[test]
    fn a_wider_penumbra_keeps_the_light_a_gobo_passes() {
        // The blur is a box filter of the mask, so it moves light about and
        // loses none: sharp and soft pass the same share. Sampled finely
        // enough to see a blind's 1.2 mm ladder strings, which a coarser grid
        // either hits or misses while they are sharp.
        let half = vec2f(0.5, 0.2);
        for kind in ["grid", "blinds"] {
            let mean = |distance: f32| mean_over(&gobo(GoboParams { kind: kind.to_string(), distance, ..GoboParams::default() }, half), half, 1200);
            let (sharp, soft) = (mean(0.0), mean(1.0));
            assert!(sharp > 0.3 && sharp < 0.99, "{kind}: {sharp}");
            assert!((sharp - soft).abs() < 0.01 * sharp, "{kind}: sharp {sharp}, soft {soft}");
        }
        // D9 (L:146): leaves and fern keep their light as they move off too,
        // on the 60 x 45 degree light the finding measured (leaves 0.381 at
        // distance 0 and 0.263 at 1, fern 0.720 and 0.513), sparse and dense,
        // to within 2.5 % of the light. Each blade and stem keeps the area of
        // its shade (foliage.rs); what is left is where their blurred shades
        // meet, which the product of what passes them counts only roughly: a
        // little lighter where a fern's pinnules lie close side by side.
        let light = vec2f(30f32.to_radians().tan(), 22.5f32.to_radians().tan());
        for kind in ["leaves", "fern"] {
            for density in [0.5f32, 0.9] {
                let mean = |distance: f32| mean_over(&gobo(GoboParams { kind: kind.to_string(), density, distance, ..GoboParams::default() }, light), light, 300);
                let sharp = mean(0.0);
                assert!(sharp > 0.05 && sharp < 0.95, "{kind} at density {density}: {sharp}");
                for distance in [0.4f32, 1.0] {
                    let soft = mean(distance);
                    assert!((sharp - soft).abs() < 0.025, "{kind} at density {density}, distance {distance}: sharp {sharp}, soft {soft}");
                }
            }
        }
        assert!(penumbra(1.0) > 20.0 * penumbra(0.0));
        assert_eq!(penumbra(f32::NAN), penumbra(0.0));
    }

    /// D9 (L:137): against the light (distance 0) blinds are as sharp as a
    /// window, as the Distance row says: shut slats 0.8 pitches wide leave
    /// thin bright lines of sky, not wide bands at a third of it. Only further
    /// off do the slits between them spread their light.
    #[test]
    fn blinds_are_sharp_at_distance_zero() {
        let h = 10f32.to_radians().tan();
        let profile = |tilt_deg: f32, slat_width: f32, distance: f32| {
            let g = gobo(GoboParams { kind: "blinds".to_string(), slats: 10, tilt_deg, slat_width, distance, ..GoboParams::default() }, vec2f(h, h));
            (0..2000).map(|i| g.transmission(vec2f(0.0, h * (-1.0 + (i as f32 + 0.5) / 1000.0)))).collect::<Vec<f32>>()
        };
        for (tilt_deg, slat_width) in [(90.0f32, 0.8f32), (60.0, 0.8), (30.0, 1.08)] {
            let sharp = profile(tilt_deg, slat_width, 0.0);
            let brightest = sharp.iter().cloned().fold(0.0f32, f32::max);
            let grey = sharp.iter().filter(|t| **t > 0.05 && **t < 0.95).count() as f32 / sharp.len() as f32;
            assert!(brightest > 0.99, "tilt {tilt_deg}, slats {slat_width}: the gaps pass all of the light, not {brightest}");
            assert!(grey < 0.05, "tilt {tilt_deg}, slats {slat_width}: {:.1} % of the light is a blurred edge", 100.0 * grey);
            // Far off the slits spread: the same share, but no longer sharp.
            let soft = profile(tilt_deg, slat_width, 1.0);
            let mean = |ts: &[f32]| ts.iter().sum::<f32>() / ts.len() as f32;
            assert!((mean(&soft) - mean(&sharp)).abs() < 0.01, "tilt {tilt_deg}: {} against {}", mean(&soft), mean(&sharp));
            let soft_grey = soft.iter().filter(|t| **t > 0.05 && **t < 0.95).count() as f32 / soft.len() as f32;
            assert!(soft_grey > 4.0 * grey.max(0.01), "tilt {tilt_deg}: the slits spread far off ({soft_grey} against {grey})");
        }
    }

    /// D3 (L:166): a stroke placed by its points casts from where it is
    /// drawn, its middle, not from the light's own (unused) direction.
    #[test]
    fn a_stroke_casts_from_where_it_is_drawn() {
        let stroke = |azimuth_deg: f32, elevation_deg: f32| LightParams {
            shape: "stroke".to_string(),
            azimuth_deg,
            elevation_deg,
            stroke: Some(StrokeParams { points: vec![[200.0, 30.0], [220.0, 45.0], [240.0, 30.0]], width_deg: 6.0, ..StrokeParams::default() }),
            ..projecting_key()
        };
        let images = ImageSet::default();
        let here = BackdropGobo::new(&[stroke(220.0, 40.0)], &images).expect("casts");
        let elsewhere = BackdropGobo::new(&[stroke(60.0, 15.0)], &images).expect("casts");
        let (mut shaded, mut centre) = (0, Vec3f::default());
        for j in 0..30 {
            for i in 0..90 {
                let d = dir_from_az_el(4.0 * i as f32, -88.0 + 3.0 * j as f32);
                assert_eq!(here.shade(d), elsewhere.shade(d), "the light's own direction places nothing: {d:?}");
                if here.shade(d) < 1.0 {
                    shaded += 1;
                    let hit = d * (FLOOR_HEIGHT / -d.y);
                    centre += vec3f(hit.x, 0.0, hit.z);
                }
            }
        }
        assert!(shaded > 20, "the stroke's gobo falls on the floor: {shaded}");
        // The patch lies on the floor under the stroke's middle, about azimuth 220.
        let (az, _) = az_el_from_dir(centre.normalize());
        assert!((az - 220.0).abs() < 15.0, "the patch is centred at azimuth {az}");
    }

    fn studio(lights: Vec<LightParams>) -> HdriParams {
        let mut p = HdriParams::default();
        p.mode = "studio".to_string();
        p.lights = lights;
        p
    }

    /// One picture under the name "mask": grey `value(x, y)`, alpha `alpha(x, y)`.
    fn mask_set(width: usize, height: usize, value: impl Fn(usize, usize) -> f32, alpha: impl Fn(usize, usize) -> f32) -> ImageSet {
        let data = (0..width * height)
            .map(|i| {
                let v = value(i % width, i / width);
                [v, v, v, alpha(i % width, i / width)]
            })
            .collect();
        let mut images = ImageSet::default();
        images.insert("mask", EnvMap { width, height, data });
        images
    }

    fn image_gobo(images: &ImageSet, distance: f32) -> Gobo {
        let p = GoboParams { kind: "image".to_string(), image: "mask".to_string(), distance, ..GoboParams::default() };
        Gobo::new(&p, vec2f(0.3, 0.2), images).expect("the name is loaded")
    }

    #[test]
    fn an_image_gobo_hangs_the_way_the_image_reads() {
        // Left half black, right half white.
        let g = image_gobo(&mask_set(4, 2, |x, _| if x < 2 { 0.0 } else { 1.0 }, |_, _| 1.0), 0.0);
        assert!(g.transmission(vec2f(-0.25, 0.0)) < 0.01, "the image's left is the viewer's left: black blocks");
        assert!(g.transmission(vec2f(0.25, 0.0)) > 0.99, "white passes");
        let mid = g.transmission(vec2f(0.0, 0.0));
        assert!(mid > 0.3 && mid < 0.7, "bilinear between them: {mid}");
        // Top row white, bottom row black: the image's top is the light's top.
        let g = image_gobo(&mask_set(2, 2, |_, y| if y == 0 { 1.0 } else { 0.0 }, |_, _| 1.0), 0.0);
        assert!(g.transmission(vec2f(0.0, 0.15)) > 0.99);
        assert!(g.transmission(vec2f(0.0, -0.15)) < 0.01);
    }

    #[test]
    fn a_missing_image_is_no_gobo() {
        let images = mask_set(4, 2, |_, _| 1.0, |_, _| 1.0);
        let p = GoboParams { kind: "image".to_string(), image: "nowhere.png".to_string(), ..GoboParams::default() };
        assert!(Gobo::new(&p, vec2f(0.3, 0.2), &images).is_none(), "Env reads no files: a name the app has not loaded is no gobo");
        let p = GoboParams { kind: "image".to_string(), ..GoboParams::default() };
        assert!(Gobo::new(&p, vec2f(0.3, 0.2), &images).is_none(), "an empty name");
    }

    #[test]
    fn an_image_gobo_only_blocks_and_a_transparent_texel_passes() {
        // An HDR picture brighter than 1 passes everything, and no more.
        let g = image_gobo(&mask_set(2, 2, |_, _| 5.0, |_, _| 1.0), 0.0);
        assert_eq!(g.transmission(vec2f(0.1, 0.05)), 1.0);
        // A black cut-out: opaque on the left, transparent on the right.
        let g = image_gobo(&mask_set(4, 2, |_, _| 0.0, |x, _| if x < 2 { 1.0 } else { 0.0 }), 0.0);
        assert!(g.transmission(vec2f(-0.25, 0.0)) < 0.01, "opaque black blocks");
        assert!(g.transmission(vec2f(0.25, 0.0)) > 0.99, "transparent passes, whatever its colour");
    }

    #[test]
    fn a_distant_image_gobo_is_blurred() {
        // A hard edge down the middle of a 64 wide mask; just right of it.
        let images = mask_set(64, 2, |x, _| if x < 32 { 0.0 } else { 1.0 }, |_, _| 1.0);
        let q = vec2f(0.006, 0.0);
        assert!(image_gobo(&images, 0.0).transmission(q) > 0.99, "against the light the edge is as sharp as the image");
        let soft = image_gobo(&images, 1.0).transmission(q);
        assert!(soft > 0.4 && soft < 0.9, "far from it the black half bleeds over: {soft}");
    }

    /// The mean of what a mask's texels pass over the box centred on unit-box
    /// point `b` with half-widths `r` (unit-box units, at least half a texel
    /// each way), the edge texels held past the picture's border. By brute
    /// force: each texel weighs the length of the box it covers on each axis.
    fn box_mean_by_overlap(img: &EnvMap, b: Vec2f, r: Vec2f) -> f64 {
        let (w, h) = (img.width, img.height);
        let x = (0.5 + 0.5 * b.x as f64) * w as f64;
        let y = (0.5 - 0.5 * b.y as f64) * h as f64;
        let rx = (0.5 * r.x as f64 * w as f64).max(0.5);
        let ry = (0.5 * r.y as f64 * h as f64).max(0.5);
        // How much of lo..hi lies on texel i of n; the edge texels run on past the border.
        let cover = |lo: f64, hi: f64, i: usize, n: usize| {
            let start = if i == 0 { f64::NEG_INFINITY } else { i as f64 };
            let end = if i + 1 == n { f64::INFINITY } else { (i + 1) as f64 };
            (hi.min(end) - lo.max(start)).max(0.0)
        };
        let mut sum = 0.0;
        for j in 0..h {
            let down = cover(y - ry, y + ry, j, h);
            for i in 0..w {
                sum += texel_passes(&img.data[j * w + i]) as f64 * cover(x - rx, x + rx, i, w) * down;
            }
        }
        sum / (4.0 * rx * ry)
    }

    #[test]
    fn a_distant_image_gobo_is_a_box_filter_of_its_texels() {
        // Noise with HDR texels and cut-out holes. Off the light the gobo is
        // the exact mean of what the texels pass over its penumbra's box, as a
        // window's is of its panes, wherever the box falls: across the
        // picture's border too.
        let hash = |x: usize, y: usize| ((x * 7919 + y * 104729) % 1009) as f32 / 1008.0;
        let images = mask_set(96, 64, |x, y| 1.5 * hash(x, y), |x, y| if hash(y, x) < 0.2 { 0.0 } else { 1.0 });
        let img = images.get("mask").expect("the mask").clone();
        for distance in [0.1f32, 0.5, 1.0] {
            let g = image_gobo(&images, distance);
            let r = vec2f(g.soft / g.rect.x, g.soft / g.rect.y);
            for j in 0..23 {
                for i in 0..31 {
                    // Over the light (0.3 x 0.2) and a little past its edges.
                    let q = vec2f(-0.33 + 0.66 * i as f32 / 30.0, -0.22 + 0.44 * j as f32 / 22.0);
                    let b = vec2f(q.x / g.s / g.rect.x, q.y / g.s / g.rect.y);
                    let (got, want) = (g.transmission(q) as f64, box_mean_by_overlap(&img, b, r));
                    assert!((got - want).abs() < 1.0e-5, "distance {distance} at {q:?}: {got}, the box mean is {want}");
                }
            }
        }
    }

    #[test]
    fn a_distant_image_gobo_blurs_fine_detail_instead_of_ghosting_it() {
        // At distance 1 on this light the penumbra's box is 20.8 texels of a
        // 256 wide mask, and taps two thirds of a penumbra apart would be 7.
        let soft = |images: &ImageSet, b: f32| image_gobo(images, 1.0).transmission(vec2f(b * 0.3, 0.0));
        // Stripes 7 texels apart: the box all but evens them out. Taps one
        // period apart all see the same phase and keep the stripes whole.
        let stripes = mask_set(256, 128, |x, _| if x % 7 < 3 { 0.0 } else { 1.0 }, |_, _| 1.0);
        let ts: Vec<f32> = (0..200).map(|i| soft(&stripes, -0.5 + i as f32 / 199.0)).collect();
        let contrast = ts.iter().cloned().fold(0.0f32, f32::max) - ts.iter().cloned().fold(1.0f32, f32::min);
        assert!(contrast < 0.1, "stripes finer than the penumbra blur away: contrast {contrast}");
        // A thin black line blurs into one dip, not into offset copies of itself.
        let line = mask_set(256, 128, |x, _| if x == 127 || x == 128 { 0.0 } else { 1.0 }, |_, _| 1.0);
        let ts: Vec<f32> = (0..401).map(|i| soft(&line, -0.3 + 0.6 * i as f32 / 400.0)).collect();
        let lowest = (0..ts.len()).min_by(|&a, &b| ts[a].total_cmp(&ts[b])).expect("samples");
        assert!(ts[lowest] < 0.95, "the line still shows: {}", ts[lowest]);
        for i in 1..ts.len() {
            if i <= lowest {
                assert!(ts[i] <= ts[i - 1] + 1.0e-6, "one dip: rising toward the line at {i}: {ts:?}");
            } else {
                assert!(ts[i] + 1.0e-6 >= ts[i - 1], "one dip: falling past the line at {i}: {ts:?}");
            }
        }
    }

    #[test]
    fn the_colour_under_a_transparent_texel_never_darkens_its_edge() {
        // A black texel kept under full transparency beside opaque white:
        // between them the mask passes everything, sharp and soft alike.
        let images = mask_set(4, 2, |x, _| if x < 2 { 1.0 } else { 0.0 }, |x, _| if x < 2 { 1.0 } else { 0.0 });
        for distance in [0.0f32, 1.0] {
            let g = image_gobo(&images, distance);
            for i in 0..21 {
                let q = vec2f(-0.3 + 0.6 * i as f32 / 20.0, 0.0);
                let t = g.transmission(q);
                assert!(t > 1.0 - 1.0e-6, "distance {distance} at {q:?}: {t}");
            }
        }
        // A bright HDR texel beside a black one dims to the mean of what they
        // pass, not to all of the light.
        let g = image_gobo(&mask_set(2, 1, |x, _| if x == 0 { 5.0 } else { 0.0 }, |_, _| 1.0), 0.0);
        let mid = g.transmission(vec2f(0.0, 0.0));
        assert!((mid - 0.5).abs() < 1.0e-6, "{mid}");
    }

    /// D15 (M6): a picture's summed-area table is built once and shared. Two
    /// lights showing the same picture as their gobo (of different sizes, at
    /// different distances), the backdrop it is cast onto, and every later
    /// `Env` (a held drag's bakes) read one table: at 2048 x 2048 one is
    /// 33.6 MB and takes about 64 ms in release (L:174), and the review
    /// measured 16 lights sharing one 2048 picture at 1076 ms and about 570 MB
    /// per `Env`. Another picture with the same texels has its own, and a
    /// table goes with its picture's last `Arc`.
    #[test]
    fn two_lights_with_the_same_picture_share_one_table() {
        use crate::probe::{counted, Work};
        let texels = |x: usize, y: usize| ((x * 7 + y * 3) % 5) as f32 / 4.0;
        let images = mask_set(64, 32, texels, |_, _| 1.0);
        let gobo = |distance: f32, cast: bool| GoboParams {
            kind: "image".to_string(),
            image: "mask".to_string(),
            distance,
            project_on_backdrop: cast,
            ..GoboParams::default()
        };
        let lit = |azimuth_deg: f32, width_deg: f32, g: GoboParams| LightParams {
            azimuth_deg,
            elevation_deg: 30.0,
            width_deg,
            height_deg: 30.0,
            gobo: Some(g),
            ..LightParams::default()
        };
        let params = studio(vec![lit(60.0, 40.0, gobo(0.5, true)), lit(200.0, 20.0, gobo(0.9, false))]);
        let before = counted(Work::PassTable);
        let env = Env::new_with(&params, &images);
        assert!(env.backdrop_gobo.is_some(), "the first light casts its gobo");
        assert_eq!(counted(Work::PassTable), before + 1, "two lights and the cast share one table");
        // A held drag builds its Env again for every bake.
        let mut dragged = params.clone();
        dragged.lights[0].azimuth_deg = 61.0;
        let dragged = Env::new_with(&dragged, &images);
        assert_eq!(counted(Work::PassTable), before + 1, "a later Env reads the same table");
        drop((env, dragged));
        let table = |g: Gobo| match g.image {
            Some(ImageMask::Soft(table)) => table,
            _ => panic!("a soft image gobo"),
        };
        let (a, b) = (table(image_gobo(&images, 0.5)), table(image_gobo(&images, 1.0)));
        assert!(Arc::ptr_eq(&a, &b), "one table, whatever the distance");
        let twin = table(image_gobo(&mask_set(64, 32, texels, |_, _| 1.0), 0.5));
        assert!(!Arc::ptr_eq(&a, &twin), "another picture is another table");
        assert_eq!(counted(Work::PassTable), before + 2);
        let kept = Arc::downgrade(&a);
        drop((a, b, images));
        crate::composite::drop_dead_facts();
        assert!(kept.upgrade().is_none(), "a freed picture's table is freed with it");
    }

    #[test]
    fn a_gobo_image_is_one_of_the_keys_the_app_loads() {
        use crate::image_light::referenced_keys;
        let mut p = studio(vec![
            LightParams { gobo: Some(GoboParams { kind: "image".to_string(), image: "b.png".to_string(), ..GoboParams::default() }), ..LightParams::default() },
            // A name on a gobo of another kind counts too: the person may switch the kind back.
            LightParams { gobo: Some(GoboParams { image: "a.png".to_string(), ..GoboParams::default() }), ..LightParams::default() },
            LightParams { gobo: Some(GoboParams::default()), ..LightParams::default() },
        ]);
        assert_eq!(referenced_keys(&p), ["a.png", "b.png"]);
        p.lights[0].gobo = None;
        assert_eq!(referenced_keys(&p), ["a.png"]);
    }

    /// A key light up in the east, its window gobo cast onto the backdrop.
    fn projecting_key() -> LightParams {
        LightParams {
            azimuth_deg: 90.0,
            elevation_deg: 45.0,
            width_deg: 40.0,
            height_deg: 40.0,
            key: true,
            gobo: Some(GoboParams { panes_x: 3, panes_y: 3, bar: 0.4, project_on_backdrop: true, ..GoboParams::default() }),
            ..LightParams::default()
        }
    }

    #[test]
    fn a_projected_gobo_darkens_the_floor_under_its_light() {
        let cast = BackdropGobo::new(&[projecting_key()], &ImageSet::default()).expect("a casting light");
        let mut darker = 0;
        for j in 0..16 {
            for i in 0..32 {
                let d = dir_from_az_el(i as f32 * 11.25, -90.0 + 60.0 * (j as f32 + 0.5) / 16.0);
                let s = cast.shade(d);
                assert!(s <= 1.0 && s >= 1.0 - SHADOW_DEPTH - 1.0e-6, "{s}");
                if s < 0.9 {
                    darker += 1;
                }
            }
        }
        assert!(darker > 0, "the bars fall somewhere on the floor");
        assert_eq!(cast.shade(vec3f(0.0, 1.0, 0.0)), 1.0, "straight up meets no backdrop surface");
        // Not asked to cast, switched off, or a flag: no projection.
        let mut plain = projecting_key();
        plain.gobo.as_mut().unwrap().project_on_backdrop = false;
        assert!(BackdropGobo::new(&[plain], &ImageSet::default()).is_none());
        let off = LightParams { enabled: false, ..projecting_key() };
        assert!(BackdropGobo::new(&[off], &ImageSet::default()).is_none());
        let flag = LightParams { blend: "multiply".to_string(), ..projecting_key() };
        assert!(BackdropGobo::new(&[flag], &ImageSet::default()).is_none());
    }

    #[test]
    fn a_cast_is_a_patch_of_floor_under_its_light() {
        // The shadow lands round the spot the camera sees under the light, as
        // far below the horizon as the light is above it (held three quarters
        // of the way to the wall for a low light). It covers the beam's
        // rectangle there, stretched along the floor by 1 / sin(elevation):
        // not the floor all round the camera, and no wall.
        for (azimuth_deg, elevation_deg) in [(90.0f32, 45.0f32), (0.0, 30.0), (200.0, 65.0), (300.0, 12.0)] {
            let light = LightParams { azimuth_deg, elevation_deg, ..projecting_key() };
            let cast = BackdropGobo::new(&[light], &ImageSet::default()).expect("casts");
            let toward = dir_from_az_el(azimuth_deg, 0.0);
            let out = (FLOOR_HEIGHT / elevation_deg.to_radians().tan()).min(0.75 * WALL_RADIUS);
            let spot = vec3f(toward.x * out, -FLOOR_HEIGHT, toward.z * out);
            // Half the beam's diagonal on the floor: across it as wide as the
            // light seen PROJECTOR_DISTANCE away, along it stretched.
            let across = PROJECTOR_DISTANCE * 20f32.to_radians().tan();
            let reach = across * (1.0 + 1.0 / elevation_deg.to_radians().sin().powi(2)).sqrt();
            let mut darker = 0;
            for j in 0..90 {
                for i in 0..180 {
                    let d = dir_from_az_el(2.0 * i as f32 + 1.0, -90.0 + 2.0 * j as f32 + 1.0);
                    let s = cast.shade(d);
                    if s >= 1.0 {
                        continue;
                    }
                    darker += 1;
                    assert!(d.y < 0.0, "light at ({azimuth_deg}, {elevation_deg}): the sky side is never shaded: {d:?}");
                    let hit = d * (FLOOR_HEIGHT / -d.y);
                    assert!(vec3f(hit.x, 0.0, hit.z).length() < WALL_RADIUS, "light at ({azimuth_deg}, {elevation_deg}): no wall is shaded: {d:?}");
                    let off = (hit - spot).length();
                    assert!(off <= reach * 1.01, "light at ({azimuth_deg}, {elevation_deg}): shaded {off} from the spot under the light, the beam reaches {reach}: {d:?}");
                }
            }
            assert!(darker > 20, "light at ({azimuth_deg}, {elevation_deg}): the bars fall under it: {darker}");
        }
    }

    #[test]
    fn the_key_light_is_the_one_that_casts() {
        let images = ImageSet::default();
        let key = projecting_key();
        let other = LightParams { key: false, azimuth_deg: 270.0, ..projecting_key() };
        let both = BackdropGobo::new(&[other.clone(), key.clone()], &images).expect("casts");
        let only_key = BackdropGobo::new(&[key], &images).expect("casts");
        let only_other = BackdropGobo::new(&[other], &images).expect("the first light that casts, when no key does");
        let mut differs = false;
        for i in 0..64 {
            let d = dir_from_az_el(i as f32 * 5.625, -50.0);
            assert_eq!(both.shade(d), only_key.shade(d));
            differs |= both.shade(d) != only_other.shade(d);
        }
        assert!(differs);
    }

    #[test]
    fn projection_only_darkens_the_studio() {
        let mut plain = projecting_key();
        plain.gobo.as_mut().unwrap().project_on_backdrop = false;
        let a = Env::new(&studio(vec![plain])).bake(64);
        let b = Env::new(&studio(vec![projecting_key()])).bake(64);
        let mut darker = 0;
        for (x, y) in a.data.iter().zip(b.data.iter()) {
            for k in 0..3 {
                assert!(y[k] <= x[k] + 1.0e-6);
                if y[k] < x[k] * 0.9 {
                    darker += 1;
                }
            }
        }
        assert!(darker > 0, "the default backdrop is not black, so the shadow shows");
        // A sky has no floor: nothing changes there.
        let sky = |light: LightParams| Env::new(&HdriParams { lights: vec![light], ..HdriParams::default() }).bake(32);
        let mut unprojected = projecting_key();
        unprojected.gobo.as_mut().unwrap().project_on_backdrop = false;
        assert_eq!(sky(projecting_key()), sky(unprojected));
    }

    #[test]
    fn leaf_and_fern_gobos_are_part_dark_part_open() {
        for kind in ["leaves", "fern"] {
            let g = gobo(GoboParams { kind: kind.to_string(), density: 0.6, ..GoboParams::default() }, vec2f(0.4, 0.3));
            let (mut dark, mut open) = (false, false);
            for j in 0..30 {
                for i in 0..40 {
                    let q = vec2f(-0.4 + 0.8 * (i as f32 + 0.5) / 40.0, -0.3 + 0.6 * (j as f32 + 0.5) / 30.0);
                    let t = g.transmission(q);
                    assert!((0.0..=1.0).contains(&t));
                    dark |= t < 0.05;
                    open |= t > 0.95;
                }
            }
            assert!(dark && open, "{kind}: dark {dark}, open {open}");
        }
    }
}
