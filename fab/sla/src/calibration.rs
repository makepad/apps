//! The orientation test: one small print that says whether a resin printer's screen shows the
//! layers the right way round. A thin pad with a raised capital F on it, 10 mm tall, at the
//! screen's centre, printed with the Normal group whatever the plate's process.
//!
//! The F reads one way only. Taken off the plate and laid pad down, the F is seen from above as
//! it was designed: it reads as an F when the profile mounts the image as the printer shows it,
//! and mirrored when the profile must flip it ([`crate::printer::ResinScreen`]'s mirror). A
//! half turn only moves it on the plate, which doesn't matter.

use crate::grid::PixelGrid;
use crate::image::LayerStack;
use crate::printer::{ResinProfile, BUILTIN_PROCESSES};
use crate::section::{MeshSection, Section};
use fab_plate::machine::Machine;
use fab_plate::PartMesh;
use makepad_fab_settings::Config;
use std::sync::Arc;

/// The test's name, which its file is named after.
pub const NAME: &str = "Fab orientation test";

/// The F's outline (mm, counter-clockwise from its foot's left corner): a 2 mm stem 10 mm tall,
/// a 7 mm bar at its top and a 5.5 mm bar at its middle.
const F: [[f64; 2]; 10] = [[0.0, 0.0], [2.0, 0.0], [2.0, 4.0], [5.5, 4.0], [5.5, 6.0], [2.0, 6.0], [2.0, 8.0], [7.0, 8.0], [7.0, 10.0], [0.0, 10.0]];
/// The triangles (counter-clockwise) that fill the F's outline: the stem fanned from its foot,
/// then the middle bar and the top bar.
const F_FILL: [[u32; 3]; 8] = [[0, 1, 2], [0, 2, 5], [0, 5, 6], [0, 6, 9], [2, 3, 4], [2, 4, 5], [6, 7, 8], [6, 8, 9]];
/// The F's size (mm).
const F_SIZE: [f64; 2] = [7.0, 10.0];
/// The pad's margin around the F, and its thickness (mm); the F's height on it.
const MARGIN: f64 = 4.0;
const PAD: f64 = 0.5;
const RELIEF: f64 = 1.0;

/// The test's job on `machine`: the printer's own keys in `printer` (its file and its route),
/// with the built-in Normal group's process over them.
pub fn job(machine: &Machine, printer: &Config) -> Result<ResinProfile, String> {
    let normal = BUILTIN_PROCESSES.iter().find(|p| p.1.contains(&("fab_sla_group", "normal"))).ok_or("no built-in Normal process")?;
    let mut cfg = printer.clone();
    for (k, v) in normal.1 {
        cfg.set_str(k, *v);
    }
    ResinProfile::on(machine, &cfg)
}

/// The test plate on `job`'s screen: the pad on the plate and the F on the pad, at the screen's
/// centre, the F upright along the plate's +Y (its back).
pub fn plate(job: &ResinProfile) -> LayerStack {
    let grid = PixelGrid::of(&job.screen);
    let centre = [grid.pixels[0] as f64 * grid.pitch[0] / 2.0, grid.pixels[1] as f64 * grid.pitch[1] / 2.0];
    let foot = [centre[0] - F_SIZE[0] / 2.0, centre[1] - F_SIZE[1] / 2.0];
    let f: Vec<[f64; 2]> = F.iter().map(|p| [foot[0] + p[0], foot[1] + p[1]]).collect();
    let (lo, hi) = ([foot[0] - MARGIN, foot[1] - MARGIN], [foot[0] + F_SIZE[0] + MARGIN, foot[1] + F_SIZE[1] + MARGIN]);
    let pad = [lo, [hi[0], lo[1]], hi, [lo[0], hi[1]]];
    let sections: Vec<Arc<dyn Section>> = vec![Arc::new(MeshSection::new(&prism(&pad, &[[0, 1, 2], [0, 2, 3]], 0.0, PAD))), Arc::new(MeshSection::new(&prism(&f, &F_FILL, PAD, PAD + RELIEF)))];
    LayerStack::new(sections, job)
}

/// The closed mesh between `z0` and `z1` over the counter-clockwise `outline`, its caps the
/// counter-clockwise triangles `fill` of the outline's points.
fn prism(outline: &[[f64; 2]], fill: &[[u32; 3]], z0: f64, z1: f64) -> PartMesh {
    let n = outline.len() as u32;
    let positions: Vec<[f64; 3]> = [z0, z1].iter().flat_map(|z| outline.iter().map(move |p| [p[0], p[1], *z])).collect();
    let mut triangles: Vec<[u32; 3]> = Vec::new();
    for a in 0..n {
        let b = (a + 1) % n;
        triangles.extend([[a, b, n + b], [a, n + b, n + a]]);
    }
    for [a, b, c] in fill {
        triangles.extend([[*c, *b, *a], [n + a, n + b, n + c]]);
    }
    PartMesh { positions, triangles }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::archive::tests::read_back;
    use crate::printer::{config_of, resin_machine, BUILTIN_PRINTER};

    /// The test, on the built-in printer (a 20 × 25 mm piece of its screen, its own pixels and
    /// mounting): the Normal group's job, a file the reader takes whose layers are Fab's own, a
    /// fifth of a millilitre of resin, and in its image the F mirrored along X as the printer's
    /// profile mounts it: its stem's outer edge at the top bar's high column, the bar reaching
    /// 5 mm further to the low ones.
    #[test]
    fn the_orientation_test_is_an_f_on_a_pad_mounted_as_the_profile_says() {
        let printer = config_of(&[BUILTIN_PRINTER.1]);
        let machine = resin_machine(BUILTIN_PRINTER.0, &printer).expect("the printer");
        let mut job = job(&machine, &printer).expect("the test's job");
        assert_eq!((job.layer_height, job.bottom_layers, job.exposure), (0.05, 5, 1.8), "the Normal group");
        assert!(job.screen.mirror[0] && !job.screen.mirror[1]);
        job.screen.pixels = [1200, 1000];
        let stack = plate(&job);
        assert_eq!(stack.layers.count, 30, "a 0.5 mm pad and a 1 mm F in 0.05 mm layers");
        let (back, written) = read_back(&stack, &job);
        // the pad's 15 × 18 mm, half a millimetre; the F's 37 mm², a millimetre
        assert!((written.resin_ml - (15.0 * 18.0 * 0.5 + 37.0) / 1000.0).abs() < 0.002, "{} ml", written.resin_ml);
        assert_eq!((back.scene.records[0].contours, back.scene.records[20].contours), (1, 1));
        // the F's layer in the image: rows of the stem alone, and rows of the top bar
        let [w, h] = back.screen.pixels;
        let image = back.raster(20).expect("the F's layer");
        let lit = |r: u32| -> Option<(u32, u32)> {
            let row = &image[(r * w) as usize..((r + 1) * w) as usize];
            Some((row.iter().position(|v| *v > 0)? as u32, row.iter().rposition(|v| *v > 0)? as u32))
        };
        // the plate's back is the image's top row: the top bar 1 mm under the F's top, the stem
        // alone 2 mm over its foot
        let row_of = |y: f64| ((h as f64 * job.screen.pitch[1] - y) / job.screen.pitch[1]) as u32;
        let centre_y = h as f64 * job.screen.pitch[1] / 2.0;
        let (bar, stem) = (lit(row_of(centre_y + 4.0)).expect("the top bar"), lit(row_of(centre_y - 3.0)).expect("the stem"));
        let mm = |cols: u32| cols as f64 * job.screen.pitch[0];
        assert!((mm(bar.1 - bar.0) - 7.0).abs() < 0.1 && (mm(stem.1 - stem.0) - 2.0).abs() < 0.1, "{bar:?} {stem:?}");
        assert!(stem.1.abs_diff(bar.1) <= 1 && stem.0 > bar.0 + 200, "mirrored along X: the stem at the bar's high columns, {bar:?} {stem:?}");
    }
}
