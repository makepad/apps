//! How much plastic a part takes as it prints: walls and skins are solid lines, the rest is
//! infill at its density. A lightened variant is judged by this, never by its solid volume.
//! On a thin plate at a low infill, a hole adds wall loops around its edge, and that can
//! outweigh the sparse infill it takes away: such a hole makes the part heavier.

use crate::grid::Grid;
use crate::material::Materials;

/// The plastic share of a Gauss point's material: its walls and skin solid, its infill at
/// `density` (everything solid when the part is taken as moulded).
fn solid_share(wall: u8, skin: u8, density: f64, as_printed: bool) -> f64 {
    if !as_printed {
        return 1.0;
    }
    let lines = (wall as f64 + skin as f64) / 255.0;
    lines.min(1.0) + (1.0 - lines).max(0.0) * density
}

/// One cell's plastic (mm³) of `m`, the materials its grid was graded with: each Gauss point's
/// infill at its own density, a zone's where one holds and the part's elsewhere.
pub fn cell_plastic(grid: &Grid, c: usize, m: &Materials) -> f64 {
    let octant = grid.h[0] * grid.h[1] * grid.h[2] / 8.0;
    grid.gp[c].iter().map(|g| g.fill as f64 / 255.0 * solid_share(g.wall, g.skin, m.fills[g.infill as usize].density, m.as_printed)).sum::<f64>() * octant
}

/// The part's plastic (mm³) of `m`, the materials its grid was graded with.
pub fn plastic(grid: &Grid, m: &Materials) -> f64 {
    (0..grid.cells()).map(|c| cell_plastic(grid, c, m)).sum()
}

/// A filament family's density (g/cm³), typical values for when the slicer's
/// `filament_density` is not known.
pub fn density_of(family: &str) -> f64 {
    match family {
        "PETG" => 1.27,
        "ABS/ASA" => 1.05,
        "PA-CF" => 1.20,
        _ => 1.24,
    }
}

/// Grams of `mm3` of plastic of `density` (g/cm³).
pub fn grams(mm3: f64, density: f64) -> f64 {
    mm3 * 1e-3 * density
}
