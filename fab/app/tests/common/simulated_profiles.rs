//! Original, generic presets for the simulated printer, never a physical machine's profiles.

use std::path::Path;

pub const MACHINE: &str = "Simulated 0.4";
pub const FILAMENT: &str = "Simulated PLA";

/// A system preset's JSON: its header, each (key, value) as a string, then `lists`, each
/// (key, value) as a list of one string.
fn preset_json(kind: &str, name: &str, values: &[(&str, &str)], lists: &[(&str, &str)]) -> String {
    let esc = |t: &str| t.replace('\\', "\\\\").replace('"', "\\\"");
    let mut fields: Vec<String> = [("type", kind), ("name", name), ("from", "system")].iter().chain(values).map(|(k, v)| format!("\"{}\": \"{}\"", esc(k), esc(v))).collect();
    fields.extend(lists.iter().map(|(k, v)| format!("\"{}\": [\"{}\"]", esc(k), esc(v))));
    format!("{{{}}}", fields.join(", "))
}

/// The printer model the simulated machine preset names: one of the first family's.
pub fn model() -> String {
    format!("{}H2D", makepad_fab_settings::family::DEFAULT.models[0])
}

pub fn install(home: &Path) {
    use makepad_fab_settings::{home::vendor_data_dir_in, profiles::VENDOR_APP_DIR};
    let dir = vendor_data_dir_in(home, VENDOR_APP_DIR);
    let presets = [
        ("machine", r#"{
            "type": "machine", "name": "Simulated 0.4", "from": "system",
            "printer_model": "@MODEL@", "printer_technology": "FFF",
            "gcode_flavor": "marlin", "nozzle_diameter": ["0.4", "0.4"],
            "printable_area": ["0x0", "100x0", "100x100", "0x100"],
            "printable_height": "100",
            "extruder_printable_area": ["0x0,100x0,100x100,0x100", "0x0,100x0,100x100,0x100"],
            "extruder_printable_height": ["100", "100"], "extruder_offset": ["0x0", "0x0"],
            "machine_max_speed_x": ["200"], "machine_max_speed_y": ["200"],
            "machine_max_speed_z": ["10"], "machine_max_speed_e": ["30"],
            "machine_max_acceleration_x": ["1000"], "machine_max_acceleration_y": ["1000"],
            "machine_max_acceleration_z": ["100"], "machine_max_acceleration_e": ["1000"],
            "machine_max_acceleration_extruding": ["1000"],
            "machine_max_acceleration_retracting": ["1000"],
            "machine_max_acceleration_travel": ["1000"],
            "machine_max_jerk_x": ["10"], "machine_max_jerk_y": ["10"],
            "machine_max_jerk_z": ["1"], "machine_max_jerk_e": ["5"],
            "retraction_length": ["0.5"], "retract_length_toolchange": ["0.5"], "retract_lift_above": ["0"],
            "machine_start_gcode": "G90\nM83\nG28\nT[initial_tool]\nG1 Z1 F600\nM140 S50\nM104 S200\nM190 S50\nM109 S200\n",
            "change_filament_gcode": "T[next_extruder]\n",
            "machine_end_gcode": "M104 S0\nM140 S0\nM107\n"
        }"#),
        ("process", r#"{
            "type": "process", "name": "Simulated 0.20", "from": "system",
            "compatible_printers": ["Simulated 0.4"],
            "layer_height": "0.2", "initial_layer_print_height": "0.2",
            "wall_loops": "2", "top_shell_layers": "2", "bottom_shell_layers": "2",
            "sparse_infill_density": "10%", "enable_support": "0",
            "outer_wall_speed": "30", "inner_wall_speed": "30",
            "sparse_infill_speed": "30", "internal_solid_infill_speed": "30",
            "top_surface_speed": "30", "initial_layer_speed": "20",
            "initial_layer_infill_speed": "20", "travel_speed": "100",
            "default_acceleration": "500", "travel_acceleration": "500",
            "initial_layer_acceleration": "500", "outer_wall_acceleration": "500",
            "inner_wall_acceleration": "500", "top_surface_acceleration": "500",
            "brim_type": "no_brim", "skirt_loops": "0"
        }"#),
        ("filament", r#"{
            "type": "filament", "name": "Simulated PLA", "from": "system",
            "compatible_printers": ["Simulated 0.4"],
            "filament_type": ["PLA"], "filament_diameter": ["1.75"],
            "filament_density": ["1.2"], "filament_flow_ratio": ["1"],
            "filament_max_volumetric_speed": ["10"],
            "nozzle_temperature_range_low": ["180"], "nozzle_temperature_range_high": ["220"],
            "nozzle_temperature_initial_layer": ["200"], "nozzle_temperature": ["200"],
            "textured_plate_temp_initial_layer": ["50"], "textured_plate_temp": ["50"],
            "required_nozzle_HRC": ["0"], "chamber_temperatures": ["0"]
        }"#),
    ];
    for (kind, json) in presets {
        let folder = dir.join("system").join("simulated").join(kind);
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join("simulated.json"), json.replace("@MODEL@", &model())).unwrap();
    }
    // the simulated resin printer beside it (`fab_sla::printer::SIMULATED_PRINTER`), its
    // process made for it
    let (printer, process) = (fab_sla::printer::SIMULATED_PRINTER, fab_sla::printer::SIMULATED_PROCESS);
    let resin = [
        ("machine", preset_json("machine", printer.0, printer.1, &[])),
        ("process", preset_json("process", process.0, process.1, &[("compatible_printers", printer.0)])),
    ];
    for (kind, json) in resin {
        let folder = dir.join("system").join("simulated").join(kind);
        std::fs::write(folder.join("simulated_resin.json"), json).unwrap();
    }
    select(home, MACHINE, "Simulated 0.20");
}

/// The installed app's selection: `machine` and `process`, printing [`FILAMENT`].
pub fn select(home: &Path, machine: &str, process: &str) {
    use makepad_fab_settings::{home::vendor_data_dir_in, profiles::VENDOR_APP_DIR};
    let dir = vendor_data_dir_in(home, VENDOR_APP_DIR);
    let conf = format!(r#"{{"presets": {{"machine":"{machine}", "process":"{process}", "filaments":["{FILAMENT}"]}}}}"#);
    std::fs::write(dir.join(format!("{VENDOR_APP_DIR}.conf")), conf).unwrap();
}
