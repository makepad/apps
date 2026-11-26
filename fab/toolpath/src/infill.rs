//! Sparse and surface infill per island and layer: the pattern dispatch.
//!
//! Each layer's fill is a pure function of its region, the layer's
//! context and any read-only [`Aux`]: a block's lightning or cubic field,
//! or the whole part's TPMS warp. Layers can be filled in any order and in parallel.

use crate::adaptive::{Field, Seeds};
use crate::clipper::{link, Clipper, Pieces};
use crate::fill::{self, RawSeg};
use crate::grade;
use crate::pattern::{Sparse, Surface};
use crate::patterns::{self, Ctx};
use crate::tpms::{self, Tpms};
use crate::warp::Gradient;
use makepad_fab_settings::machine::Job;
use makepad_fab_slicer::clip::difference_ex;
use makepad_fab_slicer::geom::*;
use makepad_fab_slicer::offset::{offset_ex, DEFAULT_MITER};
use std::f64::consts::{FRAC_PI_2, TAU};
use std::sync::Arc;

/// Settings of the sparse patterns beyond the pattern name.
#[derive(Clone, Debug)]
pub struct SparseOpts {
    /// Distance grading for the part's sparse TPMS pattern.
    pub adaptive: Option<grade::Adaptive>,
    /// `infill_shift_step` (mm per layer, crosszag).
    pub shift_step: f64,
    /// `infill_rotate_step` (radians per layer, line patterns).
    pub rotate_step: f64,
    /// `sparse_infill_lattice_angle_1/2` (radians from vertical).
    pub lattice: [f64; 2],
    /// Locked zag: skin and skeleton patterns, densities and widths.
    pub skin: Sparse,
    pub skeleton: Sparse,
    pub skin_density: f64,
    pub skeleton_density: f64,
    pub skin_width: f64,
    pub skeleton_width: f64,
    /// `skin_infill_depth`, `infill_lock_depth` (mm).
    pub skin_depth: f64,
    pub lock_depth: f64,
}

pub(crate) fn is_tpms(name: &str) -> bool {
    matches!(name, "gyroid" | "tpmsd" | "tpmsfk")
}

pub(crate) fn adaptive_gradient(job: &Job) -> Option<Gradient> {
    match job.s("adaptive_infill_gradient") {
        "" if !job.cfg.has("adaptive_infill_gradient") => Some(Gradient::Linear),
        "linear" => Some(Gradient::Linear),
        "quadratic" => Some(Gradient::Quadratic),
        "exponential" => Some(Gradient::Exponential),
        _ => None,
    }
}

impl SparseOpts {
    pub fn from_job(job: &Job, nozzle: f64, lw: f64) -> SparseOpts {
        let width = |k: &str| crate::line_width(job, k, nozzle, lw);
        let pattern = job.s("sparse_infill_pattern");
        let mode = job.s("adaptive_infill_mode");
        let locked = pattern == "lockedzag";
        SparseOpts {
            adaptive: (matches!(mode, "distance_warp" | "lobes") && is_tpms(pattern)).then(|| grade::Adaptive {
                interior: if job.cfg.has("adaptive_infill_interior_density") { job.f("adaptive_infill_interior_density") / 100.0 } else { 0.1 },
                gradient: adaptive_gradient(job).expect("toolpath::check the adaptive infill gradient before planning"),
                lobes: mode == "lobes",
            }),
            shift_step: job.f("infill_shift_step").max(0.0),
            rotate_step: job.f("infill_rotate_step").to_radians(),
            lattice: [job.f("sparse_infill_lattice_angle_1").to_radians(), job.f("sparse_infill_lattice_angle_2").to_radians()],
            // Only read when the sparse pattern is locked zag (checked then).
            skin: if locked { crate::pattern::sparse_of(job, "locked_skin_infill_pattern") } else { Sparse::CrossZag },
            skeleton: if locked { crate::pattern::sparse_of(job, "locked_skeleton_infill_pattern") } else { Sparse::ZigZag },
            skin_density: job.f("skin_infill_density") / 100.0,
            skeleton_density: job.f("skeleton_infill_density") / 100.0,
            skin_width: width("skin_infill_line_width"),
            skeleton_width: width("skeleton_infill_line_width"),
            skin_depth: job.f("skin_infill_depth").max(0.0),
            lock_depth: job.f("infill_lock_depth").max(0.0),
        }
    }
}

/// What a sparse region prints: the part's density and pattern, or a region's own (`m.settings`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SparseFill {
    /// A factor (1: solid).
    pub density: f64,
    pub pattern: Sparse,
}

/// Cross-layer data shared by sparse infill.
#[derive(Clone, Debug, Default)]
pub enum Aux {
    #[default]
    None,
    /// Lightning tree segments per layer.
    Lightning(Vec<Vec<(Pt, Pt)>>),
    /// Distance field of adaptive or support cubic.
    Field(Field),
    /// Guarded displacement field shared by the part's TPMS layers.
    Warp(Arc<grade::Graded>),
}

/// Prepare cross-layer data for a block window: `sparse` and `fill` are its
/// per-layer sparse and inside-the-walls areas (all islands), `s` the
/// sparse bead spacing.
pub fn prepare(pattern: Sparse, sparse: &[Vec<ExPolygon>], fill: &[Vec<ExPolygon>], heights: &[(f64, f64)], s: f64, density: f64, pivot: Pt) -> Aux {
    if density <= 0.0 {
        return Aux::None;
    }
    let base = s / density.min(1.0);
    let h: Vec<f64> = heights.iter().map(|x| x.1).collect();
    match pattern {
        Sparse::Lightning => Aux::Lightning(crate::lightning::trees(sparse, fill, &h, base, pivot)),
        Sparse::AdaptiveCubic => Aux::Field(Field::new(sparse, fill, &h, 3.0 * base, Seeds::Solid)),
        Sparse::SupportCubic => Aux::Field(Field::new(sparse, fill, &h, 3.0 * base, Seeds::UnderTops)),
        _ => Aux::None,
    }
}

/// A stroke of fill: open, or a closed loop (first point not repeated).
#[derive(Clone, Debug)]
pub struct Stroke {
    pub pts: Vec<Pt>,
    pub closed: bool,
    pub width: f64,
}

/// Inputs of one island's sparse fill.
pub struct SparseIn<'a> {
    /// Where line centres may go (the fill region inset by half a bead).
    pub region: &'a [ExPolygon],
    pub width: f64,
    pub spacing: f64,
    pub density: f64,
    pub ctx: Ctx,
    /// Free-end anchor, the longest boundary join, and the corner reach.
    pub anchor: f64,
    pub max: f64,
    pub reach: f64,
    /// A floating-shell loop borders this island (grid keeps short reach).
    pub bridge_loop: bool,
    /// The layer's height (spacing of locked-zag sub-patterns).
    pub height: f64,
}

/// Full TPMS period in mm at the given bead spacing divided by sparse density.
pub fn tpms_period(pattern: Sparse, base: f64) -> Option<f64> {
    Some(match pattern {
        Sparse::Gyroid => 2.0 * patterns::GYROID_PERIOD * base,
        Sparse::TpmsD => tpms::DIAMOND_PERIOD * base,
        Sparse::TpmsFk => tpms::FISCHER_KOCH_PERIOD * base,
        _ => return None,
    })
}

/// Sparse strokes of `pattern` for one island.
pub fn sparse(pattern: Sparse, o: &SparseOpts, aux: &Aux, inp: &SparseIn) -> Vec<Stroke> {
    let mut out = Vec::new();
    if inp.density <= 0.0 || inp.region.is_empty() {
        return out;
    }
    let base = inp.spacing / inp.density.min(1.0);
    let ctx = &inp.ctx;
    let w = inp.width;
    let open = |polys: Vec<Polyline>, out: &mut Vec<Stroke>| {
        out.extend(polys.into_iter().filter(|pl| pl.pts.len() >= 2).map(|pl| Stroke { pts: pl.pts, closed: false, width: w }));
    };
    let chained = |segs: &[RawSeg]| fill::chain_sparse(inp.region, segs, inp.max, inp.anchor, inp.reach);
    let curves = |pieces: &Pieces| fill::chain_pieces(inp.region, pieces, inp.max, inp.anchor, Some(inp.reach));
    let line_angle = |ctx: &Ctx| patterns::alternating(ctx) + ctx.li as f64 * o.rotate_step;
    match pattern {
        // Continuous zig-zags and rectilinear lines are the same geometry
        // at these settings: lines joined along the walls.
        Sparse::Rectilinear | Sparse::ZigZag => {
            let segs = patterns::family(inp.region, line_angle(ctx), base, 0.0, ctx.pivot);
            open(chained(&segs), &mut out);
        }
        Sparse::AlignedRectilinear => {
            let segs = patterns::family(inp.region, ctx.dir + ctx.li as f64 * o.rotate_step, base, 0.0, ctx.pivot);
            open(chained(&segs), &mut out);
        }
        Sparse::Line => {
            // Lines are never joined; each free end still anchors.
            let segs = patterns::family(inp.region, line_angle(ctx), base, 0.0, ctx.pivot);
            open(fill::chain_sparse(inp.region, &segs, 0.0, inp.anchor, 0.0), &mut out);
        }
        Sparse::CrossZag => {
            // Each direction's lines step sideways on the layers that print
            // that direction, turning back at half a spacing.
            let shift = patterns::bounce(ctx.li / 2, o.shift_step, base / 2.0);
            let segs = patterns::family(inp.region, line_angle(ctx), base, shift, ctx.pivot);
            open(chained(&segs), &mut out);
        }
        Sparse::Grid => {
            let sp = 2.0 * base;
            let mut segs = patterns::family(inp.region, ctx.dir, sp, 0.0, ctx.pivot);
            segs.extend(patterns::family(inp.region, ctx.dir + FRAC_PI_2, sp, 0.0, ctx.pivot));
            // Grid strokes join nearby strokes across up to three free
            // anchors; next to a bridge loop they keep its shorter reach.
            let reach = if inp.bridge_loop { inp.reach } else { inp.reach.max(3.0 * inp.anchor) };
            open(fill::chain_sparse(inp.region, &segs, inp.max, inp.anchor, reach), &mut out);
        }
        Sparse::Lattice2d => {
            let sp = 2.0 * base;
            let a = ctx.dir + FRAC_PI_2;
            let mut segs = patterns::family(inp.region, a, sp, ctx.z * o.lattice[0].tan(), ctx.pivot);
            segs.extend(patterns::family(inp.region, a, sp, ctx.z * o.lattice[1].tan(), ctx.pivot));
            open(chained(&segs), &mut out);
        }
        Sparse::Triangles => {
            let segs = patterns::three(inp.region, ctx.dir, 3.0 * base, [0.0; 3], ctx.pivot);
            open(chained(&segs), &mut out);
        }
        Sparse::TriHexagon => {
            let l = 3.0 * base;
            let segs = patterns::three(inp.region, ctx.dir, l, [0.0, 0.0, l / 2.0], ctx.pivot);
            open(chained(&segs), &mut out);
        }
        Sparse::Cubic => {
            let segs = patterns::three(inp.region, ctx.dir, 3.0 * base, patterns::cubic_offsets(ctx.z), ctx.pivot);
            open(chained(&segs), &mut out);
        }
        Sparse::AdaptiveCubic | Sparse::SupportCubic => {
            let Aux::Field(field) = aux else { panic!("{} needs its block pass (infill::prepare)", pattern.name()) };
            let mut pieces = Pieces::default();
            let off = patterns::cubic_offsets(ctx.z);
            for i in 0..3 {
                let a = i as f64 * TAU / 3.0;
                let segs = patterns::family(inp.region, a, field.l0, off[i], ctx.pivot);
                field.select(ctx.li, &segs, a, off[i], ctx.pivot, &mut pieces);
            }
            open(curves(&pieces), &mut out);
        }
        Sparse::Gyroid if !matches!(aux, Aux::Warp(_)) => {
            let mut clip = Clipper::new(inp.region);
            let mut pieces = Pieces::default();
            patterns::gyroid(&mut clip, ctx.z, base, ctx.pivot, &mut pieces);
            // Every gyroid line runs from wall to wall and neighbouring lines meet the wall
            // up to two half-periods apart (the two branches pair up): their ends join along
            // the wall up to the longest join, so a layer prints as one run, as in the reference.
            open(fill::chain_pieces(inp.region, &pieces, inp.max, inp.anchor, None), &mut out);
        }
        Sparse::Gyroid | Sparse::TpmsD | Sparse::TpmsFk => {
            let period = tpms_period(pattern, base).unwrap();
            let (surface, z) = match pattern {
                Sparse::Gyroid => (Tpms::Gyroid, ctx.z - period / 4.0),
                Sparse::TpmsD => (Tpms::Diamond, ctx.z),
                _ => (Tpms::FischerKoch, ctx.z),
            };
            let mut clip = Clipper::new(inp.region);
            let (pieces, loops) = if let Aux::Warp(g) = aux {
                surface.warped_contours(&mut clip, ctx.pivot, z, period, &g.warp.layer(ctx.z), g.stretch)
            } else {
                surface.contours(&mut clip, ctx.pivot, z, period)
            };
            open(fill::chain_pieces(inp.region, &pieces, inp.max, inp.anchor, None), &mut out);
            for pts in loops {
                let ring = Polygon::new(pts);
                if ring.perimeter() >= 4.0 * w * SCALE {
                    out.push(Stroke { pts: ring.pts, closed: true, width: w });
                }
            }
        }
        Sparse::Honeycomb => {
            let mut clip = Clipper::new(inp.region);
            let mut pieces = Pieces::default();
            patterns::honeycomb(&mut clip, base, inp.spacing, ctx, &mut pieces);
            open(curves(&pieces), &mut out);
        }
        Sparse::Honeycomb3d => {
            let mut clip = Clipper::new(inp.region);
            let mut pieces = Pieces::default();
            patterns::honeycomb_3d(&mut clip, base, ctx, &mut pieces);
            open(curves(&pieces), &mut out);
        }
        Sparse::CrossHatch => {
            let mut clip = Clipper::new(inp.region);
            let mut pieces = Pieces::default();
            patterns::crosshatch(&mut clip, base, ctx, &mut pieces);
            open(curves(&pieces), &mut out);
        }
        Sparse::Hilbert => {
            let mut clip = Clipper::new(inp.region);
            let mut pieces = Pieces::default();
            patterns::hilbert(&mut clip, base, ctx.dir, ctx.pivot, &mut pieces);
            open(curves(&pieces), &mut out);
        }
        Sparse::ArchimedeanChords => {
            let mut clip = Clipper::new(inp.region);
            let mut pieces = Pieces::default();
            patterns::archimedean(&mut clip, base, 0.0, ctx.pivot, &mut pieces);
            open(curves(&pieces), &mut out);
        }
        Sparse::OctagramSpiral => {
            let mut clip = Clipper::new(inp.region);
            let mut pieces = Pieces::default();
            patterns::octagram(&mut clip, base, 0.0, ctx.pivot, &mut pieces);
            open(curves(&pieces), &mut out);
        }
        Sparse::Concentric => {
            out.extend(patterns::concentric(inp.region, base).into_iter().map(|pts| Stroke { pts, closed: true, width: w }));
        }
        Sparse::Lightning => {
            let Aux::Lightning(trees) = aux else { panic!("lightning needs its block pass (infill::prepare)") };
            let Some(segs) = trees.get(ctx.li) else { return out };
            if segs.is_empty() {
                return out;
            }
            let mut clip = Clipper::new(inp.region);
            let mut pieces = Pieces::default();
            for pl in link(segs) {
                clip.clip(&pl, &mut pieces);
            }
            open(curves(&pieces), &mut out);
        }
        Sparse::LockedZag => {
            // Skin: the band within the skin depth of the walls. Skeleton:
            // the core, reaching the lock depth into the skin.
            let core = offset_ex(inp.region, -(o.skin_depth - o.lock_depth).max(0.0) * SCALE, DEFAULT_MITER);
            let skin = difference_ex(inp.region, &offset_ex(inp.region, -o.skin_depth * SCALE, DEFAULT_MITER));
            for (pat, region, width, density) in [(o.skin, &skin, o.skin_width, o.skin_density), (o.skeleton, &core, o.skeleton_width, o.skeleton_density)] {
                if region.is_empty() {
                    continue;
                }
                let sub = SparseIn { region, width, spacing: crate::flow::spacing(width, inp.height), density, ..*inp };
                out.extend(sparse(pat, o, &Aux::None, &sub));
            }
        }
    }
    out
}

/// Space-filling and concentric solid surfaces at spacing `s`: strokes
/// for the surface patterns the line fill does not cover (None for line
/// patterns, which the caller fills).
pub fn surface(pattern: Surface, region: &[ExPolygon], s: f64, w: f64, ctx: &Ctx) -> Option<Vec<Stroke>> {
    let clip = || Clipper::new(region);
    let mut pieces = Pieces::default();
    match pattern {
        Surface::Concentric => return Some(patterns::concentric(region, s).into_iter().map(|pts| Stroke { pts, closed: true, width: w }).collect()),
        Surface::Hilbert => patterns::hilbert(&mut clip(), s, ctx.dir, ctx.pivot, &mut pieces),
        Surface::ArchimedeanChords => patterns::archimedean(&mut clip(), s, 0.0, ctx.pivot, &mut pieces),
        Surface::OctagramSpiral => patterns::octagram(&mut clip(), s, 0.0, ctx.pivot, &mut pieces),
        _ => return None,
    }
    // Neighbouring curve ends join along the boundary; nothing anchors.
    let polys = fill::chain_pieces(region, &pieces, 3.0 * s, 0.0, None);
    Some(polys.into_iter().filter(|pl| pl.pts.len() >= 2).map(|pl| Stroke { pts: pl.pts, closed: false, width: w }).collect())
}
