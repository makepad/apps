//! A part's slime mould veins raised as tubes (`m.veins`): the host's answer to a
//! [`VeinsRequest`]. The mould grows on the part's source as a mix's pattern grows on a part
//! (by the source's shape alone, so the same source grows the same mould), and the tubes on its
//! veins that print without support with the printer's up where the part stands come back as a
//! grid's table for the script to raise.

use makepad_model_script::lang::csg_sdf::{SdfGrid, SdfShape};
use makepad_model_script::lang::VeinsRequest;
use std::sync::Arc;

/// How much steeper than the printer's overhang angle a tube's way down is kept from (degrees):
/// the slicer measures a curved side layer by layer, a little off the true slope.
const MARGIN: f64 = 15.0;

/// The steepest way down out of a surface that prints without support, as the least
/// up-component of its normal: the printer's overhang angle (`support_threshold_angle`: flatter
/// than it gets support; 30° unset, as the slicer has it) less steep by [`MARGIN`].
pub fn lowest(settings: &[(String, String)]) -> f64 {
    let own = settings.iter().rev().find(|(k, _)| k == "support_threshold_angle").and_then(|(_, v)| fab_plate::setting_number(v));
    let angle = own.or_else(|| makepad_fab_settings::options::options().get("support_threshold_angle").and_then(|d| d.default.parse().ok())).filter(|a: &f64| *a > 0.0).unwrap_or(30.0);
    -(angle + MARGIN).min(90.0).to_radians().cos()
}

/// What an answer to `request` depends on: its source's shape, box and mould, the printer's up
/// in its frame and the overhang it keeps to.
pub fn asked(request: &VeinsRequest, up: [f64; 3], lowest: f64) -> u64 {
    let shape = SdfShape { root: request.source.clone(), min: request.lo, max: request.hi, materials: Vec::new() };
    let p = &request.pattern;
    format!("{:?}|{:?}|{:?}|{}|{}|{:?}|{up:?}|{lowest}", shape.key(), request.lo, request.hi, p.kind, p.preset, p.values).bytes().fold(0xcbf2_9ce4_8422_2325u64, |h, b| (h ^ b as u64).wrapping_mul(0x100_0000_01b3))
}

/// The tubes `request` asks for with the printer's up along unit `up` (the source's frame) and
/// `lowest` the steepest way down that needs no support ([`lowest`]): its mould grown on its
/// source to its steps (waited for), and a tube on each vein that prints without support
/// ([`fab_paint_field::pattern::veins::raised`]), twelve words a tube in the grid's table. None:
/// `stop` ended it first, or the source has no skin in its box.
pub fn answer(request: &VeinsRequest, up: [f64; 3], lowest: f64, stop: &dyn Fn() -> bool) -> Result<Option<Arc<SdfGrid>>, String> {
    use fab_paint_field::pattern::{band::Surface, veins, Spec};
    let p = &request.pattern;
    let spec = Spec::new(Some(p.preset), &p.values).map_err(|e| format!("{}: {e}", p.kind))?;
    let shape = SdfShape { root: request.source.clone(), min: request.lo, max: request.hi, materials: Vec::new() };
    let key = shape.key().iter().fold(0xcbf2_9ce4_8422_2325u64, |h, b| (h ^ *b as u64).wrapping_mul(0x100_0000_01b3));
    let field = shape.field();
    let distance: fab_paint_field::pattern::band::Many = Arc::new(move |ps: &[[f64; 3]], out: &mut [f64]| {
        let ps: Vec<makepad_csg_math::Vec3d> = ps.iter().map(|p| makepad_csg_math::Vec3d::new(p[0], p[1], p[2])).collect();
        makepad_csg_sdf::Sdf3::distance_many(&*field, &ps, out);
    });
    let surface = Surface { distance, clearance: shape.clearance(), bounds: (shape.min, shape.max), key };
    let fan = |n: usize, f: &(dyn Fn(usize) + Sync)| makepad_fab_slicer::par::fan(n, f);
    let Some(tubes) = veins::raised(&spec, &surface, up, lowest, &fan, stop) else { return Ok(None) };
    let words: Vec<f32> = tubes.iter().flat_map(|t| t.words()).collect();
    Ok(Some(Arc::new(SdfGrid::new([0.0; 3], 1.0, [2, 2, 2], 0.0, vec![0.0; 8].into()).with_table(words.into()))))
}
