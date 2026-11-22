//! The printer every arm is timed against: the H2D, built in from its published limits (no
//! profile is read). One nozzle on its standard hot end, 0.4 mm, at its published top speed,
//! acceleration and melt flow; the process runs it by the arms' own rule, so the two sides are
//! timed alike: every feature as fast as the melt ceiling allows, at the acceleration whose ring
//! after a speed change stays near 0.05 mm on its frame's lowest mode, layers no shorter than
//! 20 s per mm of their height; two walls, 15 % gyroid, 1 mm skins (the note's §5d.6 part). Fab's
//! own slice of a part for it gives its time. A profile on a real printer that runs walls slower
//! than its melt ceiling only takes longer, so an arm's ratio against this one is its worst.

use crate::fdm::print_accel;
use fab_robot::arm::{SheetRow, Tag};
use makepad_fab_settings::profiles::{JobSetup, Preset};

/// The H2D's frame's lowest mode (Hz; the note gives a 40-100 Hz frame, the lowest taken).
pub const H2D_FRAME_HZ: f64 = 40.0;
/// Its published limits: top speed (mm/s), acceleration (mm/s²), the standard hot end's melt
/// flow (mm³/s), the nozzle (mm), the one-nozzle build volume (mm).
pub const H2D_SPEED: f64 = 1000.0;
pub const H2D_ACCEL: f64 = 20000.0;
pub const H2D_FLOW: f64 = 40.0;
pub const H2D_NOZZLE: f64 = 0.4;
pub const H2D_VOLUME: [f64; 3] = [325.0, 320.0, 325.0];
/// Its layer height (mm) and the shortest a layer may take per mm of it (s, the arms' figure).
pub const H2D_LAYER: f64 = 0.2;
pub const COOL_PER_MM: f64 = 20.0;

pub const H2D_LABEL: &str = "H2D, max speed, 0.4 mm";

use Tag::{Estimated as E, Published as P};

/// Where each H2D figure comes from.
pub const SHEET_H2D: &[SheetRow] = &[
    ("Build volume, one nozzle", "325 × 320 × 325 mm", P),
    ("Nozzle", "0.4 mm, standard hot end (0.2-0.8 fit)", P),
    ("Top speed; acceleration", "1000 mm/s; 20 m/s²", P),
    ("Melt flow ceiling", "40 mm³/s (65 on the high-flow hot end)", P),
    ("Print acceleration", "12.6 m/s²: a 0.05 mm ring on a 40 Hz frame (40-100), the arms' rule", E),
    ("Layers; lines", "0.2 mm; 0.4 mm", E),
    ("Shortest layer", "4 s (20 s per mm of layer, the arms' figure)", E),
    ("Part", "2 walls, 15 % gyroid, 1 mm skins; every feature at the melt ceiling", E),
];

fn num(v: f64) -> String {
    format!("{v}")
}

/// The H2D's job setup: its limits and the process above, one PLA filament on a textured PEI
/// plate.
pub fn h2d() -> JobSetup {
    let mut machine = Preset { name: H2D_LABEL.into(), kind: "machine".into(), ..Default::default() };
    let [x, y, z] = H2D_VOLUME;
    let m = &mut machine.values;
    m.set_vec("printable_area", vec!["0x0".into(), format!("{x}x0"), format!("{x}x{y}"), format!("0x{y}")]);
    m.set_str("printable_height", num(z));
    m.set_vec("nozzle_diameter", vec![num(H2D_NOZZLE)]);
    for (k, v) in [
        ("machine_max_speed_x", H2D_SPEED),
        ("machine_max_speed_y", H2D_SPEED),
        ("machine_max_acceleration_x", H2D_ACCEL),
        ("machine_max_acceleration_y", H2D_ACCEL),
        ("machine_max_acceleration_extruding", H2D_ACCEL),
        ("machine_max_acceleration_travel", H2D_ACCEL),
    ] {
        m.set_vec(k, vec![num(v), num(v)]);
    }
    let mut process = Preset { name: "H2D at its melt ceiling".into(), kind: "process".into(), ..Default::default() };
    let accel = print_accel(H2D_FRAME_HZ).round();
    let p = &mut process.values;
    for (k, v) in [
        ("outer_wall_speed", H2D_SPEED),
        ("inner_wall_speed", H2D_SPEED),
        ("sparse_infill_speed", H2D_SPEED),
        ("internal_solid_infill_speed", H2D_SPEED),
        ("top_surface_speed", H2D_SPEED),
        ("gap_infill_speed", H2D_SPEED),
        ("travel_speed", H2D_SPEED),
        // The first layer slower, for adhesion (Fab's standard process).
        ("initial_layer_speed", 50.0),
        ("initial_layer_infill_speed", 80.0),
        ("default_acceleration", accel),
        ("outer_wall_acceleration", accel),
        ("top_surface_acceleration", accel),
        ("initial_layer_acceleration", accel),
        ("travel_acceleration", H2D_ACCEL),
    ] {
        p.set_vec(k, vec![num(v)]);
    }
    for (k, v) in [
        ("layer_height", num(H2D_LAYER)),
        ("initial_layer_print_height", num(H2D_LAYER)),
        ("wall_loops", "2".into()),
        ("sparse_infill_density", "15%".into()),
        ("sparse_infill_pattern", "gyroid".into()),
        ("top_shell_layers", "1".into()),
        ("bottom_shell_layers", "1".into()),
        ("top_shell_thickness", "1".into()),
        ("bottom_shell_thickness", "1".into()),
    ] {
        p.set_str(k, v);
    }
    let mut filament = Preset { name: "PLA".into(), kind: "filament".into(), ..Default::default() };
    let f = &mut filament.values;
    for (k, v) in [
        ("filament_max_volumetric_speed", num(H2D_FLOW)),
        ("filament_density", "1.24".into()),
        ("filament_type", "PLA".into()),
        ("slow_down_layer_time", num((COOL_PER_MM * H2D_LAYER).round())),
    ] {
        f.set_vec(k, vec![v]);
    }
    JobSetup {
        machine,
        process,
        filaments: vec![filament],
        filament_map: vec![1],
        nozzle_volume_type: vec!["Standard".into()],
        bed_type: "Textured PEI Plate".into(),
        filament_map_mode: "Manual".into(),
        filament_colours: vec![None],
    }
}

/// One of the H2D's limits taken away, to weigh what it costs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lift {
    MeltFlow,
    Accel,
    Cooling,
}

/// [`h2d`] with `lift` gone: the melt flow, every acceleration (the machine's limits too) a
/// hundred times over, or no cooling slowdown.
pub fn h2d_lifted(lift: Lift) -> JobSetup {
    let mut s = h2d();
    match lift {
        Lift::MeltFlow => s.filaments[0].values.set_vec("filament_max_volumetric_speed", vec![num(100.0 * H2D_FLOW)]),
        Lift::Accel => {
            let fast = num(100.0 * H2D_ACCEL);
            for k in ["machine_max_acceleration_x", "machine_max_acceleration_y", "machine_max_acceleration_extruding", "machine_max_acceleration_travel"] {
                s.machine.values.set_vec(k, vec![fast.clone(), fast.clone()]);
            }
            for k in ["default_acceleration", "outer_wall_acceleration", "top_surface_acceleration", "initial_layer_acceleration", "travel_acceleration"] {
                s.process.values.set_vec(k, vec![fast.clone()]);
            }
        }
        Lift::Cooling => s.filaments[0].values.set_vec("slow_down_for_layer_cooling", vec!["0".into()]),
    }
    s
}
