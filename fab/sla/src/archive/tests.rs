//! The archive as the independent reader reads it back: every rule of its layout holds, and its
//! layers decode to the screen images Fab sliced, pixel for pixel.

use super::*;
use crate::export::screen_image;
use crate::grid::PixelGrid;
use crate::printer::{config_of, ResinProfile, BUILTIN_PRINTER, BUILTIN_PROCESSES};
use crate::section::{Box3, MeshSection, Near, Section};
use crate::tests::{field, prism, rect};
use fab_sla_archive::read::{read, Archive, Encoding as Layers};
use makepad_csg_sdf::SdfNode;
use std::sync::Arc;

/// The built-in printer in its fast group (0.1 mm layers), on a screen of `pixels` at its own
/// 16.8 × 24.8 µm pitch.
pub(crate) fn printer(pixels: [u32; 2]) -> ResinProfile {
    let cfg = config_of(&[BUILTIN_PRINTER.1, BUILTIN_PROCESSES[1].1]);
    let mut p = ResinProfile::from_config(BUILTIN_PRINTER.0, &cfg).expect("the built-in printer");
    p.screen.pixels = pixels;
    p
}

/// `stack`'s archive as `job`, read back: every layer decodes to its screen image's levels.
pub(crate) fn read_back(stack: &LayerStack, job: &ResinProfile) -> (Archive, Written) {
    let (bytes, written) = archive(stack, job).expect("the archive");
    assert_eq!(written.bytes, bytes.len() as u64);
    let back = read(bytes).unwrap_or_else(|f| panic!("the reader refuses it: {f}"));
    for k in 0..stack.layers.count {
        let (size, image) = screen_image(&stack.layer(k).spans, &job.screen);
        assert_eq!(size, back.screen.pixels, "layer {k}'s image");
        let levels: Vec<u8> = image.iter().map(|v| v / 17).collect();
        assert!(back.raster(k).expect("the layer decodes") == levels, "layer {k} decodes to its screen image");
    }
    (back, written)
}

/// A block, a ball and a ring (a part with a hole) on a small screen of the printer's own
/// non-square pixels: the reader takes the archive, and it says what Fab sliced. Its entries
/// are in order, its job is the profile's, its scene table gives each layer's area, box and
/// outlines (parts and holes), and its totals are the job's resin and time.
#[test]
fn an_archive_reads_back_as_the_layers_fab_sliced() {
    let job = printer([600, 400]);
    let block: Arc<dyn Section> = Arc::new(field(SdfNode::Box { size: [2.0, 2.0, 2.0], radius: 0.0 }, [2.5, 2.5, 1.0], ([1.4, 1.4, -0.1], [3.6, 3.6, 2.0])));
    let ball: Arc<dyn Section> = Arc::new(field(SdfNode::Sphere { radius: 1.5 }, [7.0, 2.5, 1.5], ([5.4, 0.9, 0.0], [8.6, 4.1, 3.0])));
    let ring: Arc<dyn Section> = Arc::new(MeshSection::new(&prism(&rect([1.0, 5.5], [4.0, 8.5]), Some(&rect([2.0, 6.5], [3.0, 7.5])), 0.0, 1.5)));
    let stack = LayerStack::new(vec![block, ball, ring], &job);
    let (back, written) = read_back(&stack, &job);
    assert_eq!((back.layers, written.layers, back.encoding), (30, 30, Layers::Raster), "3 mm in 0.1 mm layers");
    let mut names: Vec<String> = format::LEADING.iter().map(|n| n.to_string()).collect();
    names.extend((0..30).map(|k| format::layer_entry(k, format::RASTER_EXTENSION)));
    names.push(format::SCENE.into());
    assert_eq!(back.names, names, "the entries in the maker's order");
    assert_eq!(back.screen.pixels, [600, 400]);
    assert!((back.screen.pitch[0] - 0.0168).abs() < 1e-12 && (back.screen.pitch[1] - 0.0248).abs() < 1e-12, "{:?}", back.screen.pitch);
    let j = &back.job;
    assert_eq!((j.layer_height, j.bottom_layers, j.bottom_exposure, j.exposure, j.light_off), (0.1, 3, 25.0, 2.2, 0.5));
    assert_eq!((j.lift_height, j.lift_speed, j.retract_speed, j.anti_alias), (4.0, 15.0, 15.0, 16));
    // a layer through all three: the block, the ball, and the ring with its hole
    assert_eq!(back.scene.records[2].contours, 4);
    assert_eq!(back.scene.records[20].contours, 1, "the ball alone, over the block and the ring: {:?}", back.scene.records[20]);
    // the block's box at its foot, from the screen's centre (5.04 × 4.96 mm), in the image's
    // frame: mirrored along X, the back at the top
    let b = back.scene.records[29].bounds;
    assert!(b[2] < b[0] + 1.0 && b[0] > -2.5 && b[2] < -0.4, "the ball's top, mirrored to the left of the centre: {b:?}");
    let area: f64 = back.scene.records.iter().map(|r| r.area as f64).sum();
    assert!((back.totals.volume_ml - written.resin_ml).abs() < 1e-6 && (written.resin_ml - area * 0.1 / 1000.0).abs() < 1e-6);
    // the block's 8, the ball's 14.14 and the ring's 12 mm³, to what the pixels round off
    assert!((written.resin_ml - 0.03414).abs() < 0.0003, "{} ml", written.resin_ml);
    assert_eq!(back.totals.print_seconds, written.print_seconds.round() as u64);
    assert_eq!(back.resin, docs::resin_name(&job));
}

/// A raised cube and its supports read back pixel for pixel, including the pad and the
/// separate pillars above it, whose layers have more than one contour.
#[test]
fn a_supported_part_reads_back_as_sliced() {
    use crate::load::Load;
    use crate::points::PointSettings;
    use crate::supports::{supported, SupportSection, SupportSettings, Supports};

    fn supports_of(parts: Vec<Arc<dyn Section>>, job: &ResinProfile, settings: &SupportSettings) -> (Vec<Arc<dyn Section>>, Arc<Supports>) {
        let count = parts.len();
        let built = supported(parts, PixelGrid::of(&job.screen), job.layer_height, settings, &PointSettings::default(), &Load::default());
        (built.stack.sections[..count].to_vec(), built.supports)
    }

    let job = printer([600, 400]);
    let settings = SupportSettings { elevation: 1.0, pad: 1.0, head_width: 0.3, tip_radius: 0.15, penetration: 0.2, pillar_radius: 0.25, base_radius: 0.6, base_height: 0.3, gap: 0.2, brim: 0.5, ..SupportSettings::default() };
    let cube: Arc<dyn Section> = Arc::new(MeshSection::new(&prism(&rect([3.0, 3.0], [7.0, 7.0]), None, 0.0, 2.0)));
    let (mut parts, supports) = supports_of(vec![cube], &job, &settings);
    assert!(supports.pillars.len() > 1, "separate pillars under the cube: {:?}", supports.dropped);
    assert!(!supports.pads.is_empty(), "the supports have a pad");
    let support = SupportSection::new(supports.clone()).expect("support section");
    let (lo, hi) = support.bounds();
    let size = job.screen.pixels.map(|v| v as f64);
    assert!(lo[0] >= 0.0 && lo[1] >= 0.0 && hi[0] <= size[0] * job.screen.pitch[0] && hi[1] <= size[1] * job.screen.pitch[1], "the pad and supports fit on the screen");
    parts.push(Arc::new(support));
    let stack = LayerStack::new(parts, &job);
    let (back, _) = read_back(&stack, &job);
    assert_eq!(back.layers, 40, "2 mm cube raised 2 mm, at 0.1 mm per layer");
    assert!(back.scene.records[0].area > 0.0, "the pad starts at the plate");
    assert!(back.scene.records.iter().any(|r| r.z as f64 > settings.ground() && (r.z as f64) < settings.lift() && r.contours > 1), "separate supports above the pad have multiple contours");
}

/// A slab lighting every pixel of the screen up to `top`, its box `tall`.
struct Slab {
    size: [f64; 2],
    top: f64,
    tall: f64,
}

impl Section for Slab {
    fn bounds(&self) -> Box3 {
        ([0.0, 0.0, 0.0], [self.size[0], self.size[1], self.tall])
    }
    fn cover(&self, z: f64, _grid: &PixelGrid, _rect: crate::grid::PixelRect, cov: &mut [u8]) {
        if z < self.top {
            cov.fill(255);
        }
    }
    fn near(&self, _p: &[[f64; 3]], out: &mut [Near]) {
        out.fill(Near { distance: 0.0, normal: [0.0, 0.0, 1.0] });
    }
    fn clear(&self, _p: &[[f64; 3]], _need: &[f64], out: &mut [f64]) {
        out.fill(0.0);
    }
}

/// At the printer's full 13312 × 5120, a layer lit edge to edge and an empty one: both streams
/// are the screen's 68 157 440 pixels in 16 645 runs, and they decode to full and to black.
#[test]
fn a_full_size_archive_holds_a_full_and_an_empty_layer() {
    let job = printer([13312, 5120]);
    let stack = LayerStack::new(vec![Arc::new(Slab { size: [223.6416, 126.976], top: 0.1, tall: 0.2 })], &job);
    let (bytes, _) = archive(&stack, &job).expect("the archive");
    let back = read(bytes).unwrap_or_else(|f| panic!("the reader refuses it: {f}"));
    assert_eq!(back.layers, 2);
    for (k, level) in [(0, 15u8), (1, 0)] {
        let stream = back.entry(&format::layer_entry(k, format::RASTER_EXTENSION)).expect("the layer");
        assert_eq!(stream.len(), 33_290, "layer {k}: 16 644 runs of 4095 and one of 260, two bytes each");
        assert!(back.raster(k).expect("the layer decodes").iter().all(|v| *v == level), "layer {k}");
    }
    let full = &back.scene.records[0];
    assert!((full.area as f64 - 223.6416 * 126.976).abs() < 0.01, "{}", full.area);
    assert_eq!(full.bounds, [-111.8208f32, -63.488, 111.8208, 63.488]);
}

/// Saved into a folder, the archive is there under its name, read back whole: no half-written
/// file is left beside it, and a second save replaces it. A folder that isn't there is said.
#[test]
fn a_saved_archive_is_read_back_from_its_folder() {
    let job = printer([120, 80]);
    let block: Arc<dyn Section> = Arc::new(field(SdfNode::Box { size: [1.0, 1.0, 0.5], radius: 0.0 }, [1.0, 0.7, 0.25], ([0.4, 0.1, -0.1], [1.6, 1.3, 0.5])));
    let stack = LayerStack::new(vec![block], &job);
    let dir = std::env::temp_dir().join(format!("fab-archive-save-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(file_name("block"));
    for _ in 0..2 {
        let written = save(&path, &stack, &job).expect("saved and read back");
        assert_eq!((written.layers, std::fs::metadata(&path).unwrap().len()), (5, written.bytes));
    }
    let names: Vec<String> = std::fs::read_dir(&dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    assert_eq!(names, [file_name("block")], "the file alone");
    let gone = dir.join("gone").join(file_name("block"));
    assert!(save(&gone, &stack, &job).unwrap_err().starts_with("writing "), "a missing folder is said");
    std::fs::remove_dir_all(&dir).unwrap();
}

/// A name the printer lists: letters, digits, `-` and `_`, the archive's extension, 60
/// characters at most.
#[test]
fn a_file_name_is_one_the_printer_lists() {
    let ext = format!(".{}", format::EXTENSION);
    assert_eq!(file_name("Robot arm v2 (left).fab"), format!("Robot_arm_v2_left_fab{ext}"));
    assert_eq!(file_name("ÄÖ"), format!("print{ext}"));
    let long = file_name(&"x".repeat(80));
    assert_eq!((long.len(), long.ends_with(&ext)), (60, true));
}
