//! Small, original presets for the second family's in-memory print test.
use std::path::Path;

pub const MACHINE: &str = "Second family 0.4";
pub const PROCESS: &str = "Second family 0.20";
pub const FILAMENT: &str = "Second family PLA";

const MACHINE_JSON: &str = r#"{
    "type": "machine",
    "name": "Second family 0.4",
    "from": "system",
    "printer_model": "@MODEL@",
    "printer_technology": "FFF",
    "gcode_flavor": "klipper",
    "nozzle_diameter": [
        "0.4",
        "0.4",
        "0.4",
        "0.4"
    ],
    "printable_area": [
        "0x0",
        "100x0",
        "100x100",
        "0x100"
    ],
    "printable_height": "100",
    "extruder_printable_area": [
        "0x0,100x0,100x100,0x100",
        "0x0,100x0,100x100,0x100",
        "0x0,100x0,100x100,0x100",
        "0x0,100x0,100x100,0x100"
    ],
    "extruder_printable_height": [
        "100",
        "100",
        "100",
        "100"
    ],
    "extruder_offset": [
        "0x0",
        "0x0",
        "0x0",
        "0x0"
    ],
    "thumbnails": "48x48/PNG",
    "retraction_length": [
        "0.5"
    ],
    "retract_lift_above": [
        "0"
    ],
    "machine_start_gcode": "G28\nG90\nM83\nT[initial_tool]\nG1 Z1 F600\nM140 S50\nM104 S200 T[initial_tool]\nM190 S50\nM109 S200 T[initial_tool]\n",
    "machine_end_gcode": "M104 S0\nM140 S0\nM107\n",
    "change_filament_gcode": "T[next_extruder]\n",
    "machine_max_speed_x": [
        "200"
    ],
    "machine_max_speed_y": [
        "200"
    ],
    "machine_max_speed_z": [
        "10"
    ],
    "machine_max_speed_e": [
        "30"
    ],
    "machine_max_acceleration_x": [
        "1000"
    ],
    "machine_max_acceleration_y": [
        "1000"
    ],
    "machine_max_acceleration_z": [
        "100"
    ],
    "machine_max_acceleration_e": [
        "1000"
    ],
    "machine_max_jerk_x": [
        "10"
    ],
    "machine_max_jerk_y": [
        "10"
    ],
    "machine_max_jerk_z": [
        "1"
    ],
    "machine_max_jerk_e": [
        "5"
    ],
    "machine_max_acceleration_extruding": [
        "1000"
    ],
    "machine_max_acceleration_retracting": [
        "1000"
    ],
    "machine_max_acceleration_travel": [
        "1000"
    ]
}"#;

const PROCESS_JSON: &str = r#"{
    "type": "process",
    "name": "Second family 0.20",
    "from": "system",
    "compatible_printers": [
        "Second family 0.4"
    ],
    "layer_height": "0.2",
    "initial_layer_print_height": "0.2",
    "wall_loops": "2",
    "top_shell_layers": "2",
    "bottom_shell_layers": "2",
    "sparse_infill_density": "10%",
    "enable_support": "0",
    "outer_wall_speed": "30",
    "inner_wall_speed": "30",
    "sparse_infill_speed": "30",
    "internal_solid_infill_speed": "30",
    "top_surface_speed": "30",
    "initial_layer_speed": "20",
    "initial_layer_infill_speed": "20",
    "travel_speed": "100",
    "default_acceleration": "500",
    "travel_acceleration": "500",
    "initial_layer_acceleration": "500",
    "outer_wall_acceleration": "500",
    "inner_wall_acceleration": "500",
    "top_surface_acceleration": "500",
    "line_width": "0.42",
    "brim_type": "no_brim",
    "skirt_loops": "0"
}"#;

const FILAMENT_JSON: &str = r#"{
    "type": "filament",
    "name": "Second family PLA",
    "from": "system",
    "compatible_printers": [
        "Second family 0.4"
    ],
    "filament_type": [
        "PLA"
    ],
    "filament_diameter": [
        "1.75"
    ],
    "filament_density": [
        "1.2"
    ],
    "filament_flow_ratio": [
        "1"
    ],
    "filament_max_volumetric_speed": [
        "10"
    ],
    "filament_volume_type": [
        "standard"
    ],
    "nozzle_temperature_range_low": [
        "180"
    ],
    "nozzle_temperature_range_high": [
        "220"
    ],
    "nozzle_temperature_initial_layer": [
        "200"
    ],
    "nozzle_temperature": [
        "200"
    ],
    "textured_plate_temp_initial_layer": [
        "50"
    ],
    "textured_plate_temp": [
        "50"
    ],
    "required_nozzle_HRC": [
        "0"
    ],
    "chamber_temperatures": [
        "0"
    ]
}"#;

/// The second family's printer, as the machine preset names it.
pub fn model() -> String {
    makepad_fab_settings::family::FAMILIES[1].models[0].to_string()
}

pub fn install(home: &Path) {
    use makepad_fab_settings::{home::vendor_data_dir_in, profiles::SLICER_APPS};
    let folder = SLICER_APPS[1];
    let dir = vendor_data_dir_in(home, folder);
    let machine = MACHINE_JSON.replace("@MODEL@", &model());
    for (kind, json) in [("machine", machine.as_str()), ("process", PROCESS_JSON), ("filament", FILAMENT_JSON)] {
        let presets = dir.join("system").join("second_family").join(kind);
        std::fs::create_dir_all(&presets).unwrap();
        std::fs::write(presets.join("fixture.json"), json).unwrap();
    }
    let settings = format!(r#"{{"presets":{{"machine":"{MACHINE}"}},"orca_presets":[{{"machine":"{MACHINE}","process":"{PROCESS}","filament":"{FILAMENT}","curr_bed_type":"4"}}]}}"#);
    std::fs::write(dir.join(format!("{folder}.conf")), settings).unwrap();
}
