//! What another slicer's presets leave to that slicer's own defaults, where those differ from
//! Fab's. A preset lists only the keys it sets. Fab's option table holds the default family's
//! defaults, so a printer whose presets come from another slicer's data folder starts from that
//! slicer's defaults for the keys listed here. A key the other slicer lacks is set to the value
//! that prints as that slicer prints without it.
//!
//! A value is profile text, or `=key`: the job's value of `key` once the presets apply.

use makepad_fab_slicer::xml::{attr, XmlEvent, XmlReader};

/// One slicer's data folder and its defaults that differ from Fab's.
pub struct Tree {
    /// The data folder's name.
    pub folder: &'static str,
    pub defaults: &'static [(&'static str, &'static str)],
}

/// Defaults both trees of the other slicer share.
const COMMON: &[(&str, &str)] = &[
    ("travel_acceleration", "10000"),
    ("initial_layer_travel_acceleration", "=travel_acceleration"),
    ("accel_to_decel_enable", "1"),
    ("accel_to_decel_factor", "50%"),
    // Read by the writer only when present: half the outer wall's.
    ("bridge_acceleration", "50%"),
    ("travel_jerk", "12"),
    ("nozzle_height", "2.5"),
    ("support_air_filtration", "1"),
    ("support_chamber_temp_control", "1"),
    ("fuzzy_skin_point_distance", "0.3"),
    ("fuzzy_skin_thickness", "0.2"),
    ("seam_slope_conditional", "0"),
    ("seam_slope_inner_walls", "0"),
    ("seam_slope_min_length", "20"),
    ("seam_slope_start_height", "0"),
    ("skirt_per_object", "0"),
    // Features of the default family's slicer that the other one does not have: off.
    ("detect_floating_vertical_shell", "0"),
    ("smooth_speed_discontinuity_area", "0"),
    ("vertical_shell_speed", "100%"),
    ("filament_cooling_before_tower", "0"),
    ("machine_prepare_compensation_time", "0"),
    // Short travels onto an outer wall keep the travel acceleration.
    ("travel_short_distance_acceleration", "0"),
    // Fab's own: the wipe retracts what the retraction speed allows while it moves, the rest in
    // place before it, at the acceleration in force.
    ("fab_wipe_retract_at_retraction_speed", "1"),
    // Fab's own: a tool change pushes back only what a parked head retracted; overhang walls
    // take the bridge acceleration; the bed's other-layer temperature is set at layer 2; a
    // layer start lifts above the new layer.
    ("fab_toolchange_prime_parked_only", "1"),
    ("fab_overhang_wall_bridge_acceleration", "1"),
    ("fab_bed_other_layers_always", "1"),
    ("fab_layer_start_lift_above_new_layer", "1"),
    // Bridges over sparse infill are internal bridges, at this share of the bridge speed.
    ("internal_bridge_speed", "150%"),
    // The layer under a top surface is internal solid infill.
    ("sub_top_surface_pattern", "=internal_solid_infill_pattern"),
    ("sparse_infill_pattern", "crosshatch"),
    ("top_surface_pattern", "monotonicline"),
    ("bottom_surface_pattern", "monotonic"),
    // A head change rams what the filament's multitool ramming sets, a volume the writer reads
    // when present, not a length of filament.
    ("filament_change_length", "0"),
    // Purge rows a bead apart.
    ("prime_tower_infill_gap", "100%"),
];

/// The second slicer's own build.
const FORK: &[(&str, &str)] = &[
    // Fab's own: a tool change runs at a 100 % speed factor.
    ("fab_toolchange_speed_guard", "1"),
    ("independent_support_layer_height", "0"),
    ("max_bridge_length", "0"),
    ("bed_temperature_formula", "by_highest_temp"),
    ("internal_solid_infill_pattern", "monotonicline"),
    // A plain box: its upstream's tower has ribs.
    ("prime_tower_rib_wall", "0"),
];

/// Its upstream.
const MAINLINE: &[(&str, &str)] = &[("bed_temperature_formula", "by_highest_temp"), ("internal_solid_infill_pattern", "monotonic")];

const TREES: &[(&str, &[&[(&str, &str)]])] = &[("Snapmaker_Orca", &[COMMON, FORK]), ("OrcaSlicer", &[COMMON, MAINLINE])];

/// The other slicer's key for a setting Fab reads under its own: (stored key, Fab's key, values).
/// An empty value list keeps the value; a value the list lacks leaves the key as stored, which
/// Fab then reports as one it does not read. No stored key here is one of Fab's own. The default
/// family's presets carry some of these keys too, unread, so only the listed trees rename them.
const RENAMED: &[(&str, &str, &[(&str, &str)])] = &[
    ("wipe_tower_extra_rib_length", "prime_tower_extra_rib_length", &[]),
    ("wipe_tower_wall_type", "prime_tower_rib_wall", &[("rib", "1"), ("rectangle", "0")]),
    ("wipe_tower_fillet_wall", "prime_tower_fillet_wall", &[]),
    ("wipe_tower_rib_width", "prime_tower_rib_width", &[]),
    ("wipe_tower_max_purge_speed", "prime_tower_max_speed", &[]),
    // The prime rows' pitch, as a share of their bead width.
    ("wipe_tower_extra_spacing", "prime_tower_infill_gap", &[]),
    // The flow a filament rams at before its head hands over.
    ("filament_multitool_ramming_flow", "filament_ramming_volumetric_speed", &[]),
    ("infill_anchor", "sparse_infill_anchor", &[]),
    ("infill_anchor_max", "sparse_infill_anchor_max", &[]),
    ("role_based_wipe_speed", "role_base_wipe_speed", &[]),
    ("bottom_solid_infill_flow_ratio", "initial_layer_flow_ratio", &[]),
    ("min_width_top_surface", "top_area_threshold", &[]),
    // One wall on every top surface wider than the threshold, the topmost layer included.
    ("only_one_wall_top", "top_one_wall_type", &[("1", "all top"), ("0", "not apply")]),
    ("dont_slow_down_outer_wall", "no_slow_down_for_cooling_on_outwalls", &[]),
    ("machine_tool_change_time", "machine_switch_extruder_time", &[]),
    ("extruder_clearance_radius", "extruder_clearance_max_radius", &[]),
    ("chamber_temperature", "chamber_temperatures", &[]),
    ("ironing_angle", "ironing_direction", &[]),
    // "perobject" -> 1 is inferred from the meaning, not from a spec: to be confirmed.
    ("skirt_type", "skirt_per_object", &[("combined", "0"), ("perobject", "1")]),
    // "0" -> Disabled is inferred from the meaning, not from a spec: to be confirmed.
    ("reduce_infill_retraction", "reduce_infill_retraction_mode", &[("1", "Enabled"), ("0", "Disabled")]),
    // The wall order only: where the infill goes is not read from it.
    (
        "wall_infill_order",
        "wall_sequence",
        &[
            ("inner wall/outer wall/infill", "inner wall/outer wall"),
            ("infill/inner wall/outer wall", "inner wall/outer wall"),
            ("outer wall/inner wall/infill", "outer wall/inner wall"),
            ("infill/outer wall/inner wall", "outer wall/inner wall"),
            ("inner-outer-inner wall/infill", "inner-outer-inner wall"),
        ],
    ),
];

/// The slicers whose data folders are listed, by the names their G-code's `; generated by` line
/// gives.
const GENERATORS: &[(&str, &str)] = &[("Snapmaker Orca", "Snapmaker_Orca"), ("Snapmaker_Orca", "Snapmaker_Orca"), ("OrcaSlicer", "OrcaSlicer")];

/// The data folder of the slicer that wrote a G-code file, from its `; generated by <name>
/// <version> on <date> at <time>` line near the top. The name must be one listed, followed by a
/// version and then the date or the line's end. None for any other slicer, Fab's own default
/// family included.
pub fn of_generator(text: &str) -> Option<&'static str> {
    let line = text.lines().take(64).find_map(|l| l.strip_prefix("; generated by "))?;
    let version = |v: &str| v.starts_with(|c: char| c.is_ascii_digit()) && v.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '+'));
    GENERATORS.iter().find(|(name, _)| {
        let Some(rest) = line.strip_prefix(name).and_then(|r| r.strip_prefix(' ')) else { return false };
        let (v, after) = rest.split_once(' ').unwrap_or((rest, ""));
        version(v) && (after.is_empty() || after.starts_with("on "))
    }).map(|&(_, folder)| folder)
}

/// The `key` of each item in a slice header. A part that does not parse, or whose elements do
/// not nest and close, names nothing.
fn header_keys(text: &str) -> Vec<String> {
    let mut keys = Vec::new();
    let mut open = Vec::new();
    let mut xml = XmlReader::new(text);
    while let Some(event) = xml.next_event() {
        let Ok(event) = event else { return Vec::new() };
        match event {
            XmlEvent::Start { name, attrs, .. } => {
                if name == "header_item" { keys.extend(attr(&attrs, "key").map(str::to_string)); }
                open.push(name);
            }
            XmlEvent::End { name } => if open.pop() != Some(name) { return Vec::new() },
            XmlEvent::Text(_) => {}
        }
    }
    if open.is_empty() { keys } else { Vec::new() }
}

/// The model's metadata items before its resources, as (name, text). Only that far is read: a
/// part that does not parse there, or whose elements there do not nest, names nothing.
fn model_metadata(text: &str) -> Vec<(String, String)> {
    let mut items = Vec::new();
    let mut open = Vec::new();
    let mut item: Option<(String, String)> = None;
    let mut xml = XmlReader::new(text);
    while let Some(event) = xml.next_event() {
        let Ok(event) = event else { return Vec::new() };
        match event {
            XmlEvent::Start { name: "resources", .. } => return items,
            XmlEvent::Start { name, attrs, .. } => {
                if item.is_some() { return Vec::new(); }
                if name == "metadata" { item = Some((attr(&attrs, "name").unwrap_or("").to_string(), String::new())); }
                open.push(name);
            }
            // Text split by a comment is one text.
            XmlEvent::Text(t) => if let Some((_, text)) = &mut item { text.push_str(&t) },
            XmlEvent::End { name } => {
                if open.pop() != Some(name) { return Vec::new(); }
                if name == "metadata" { items.extend(item.take().map(|(n, t)| (n, t.trim().to_string()))); }
            }
        }
    }
    if open.is_empty() { items } else { Vec::new() }
}

/// The data folder of the slicer that made a sliced project, from its files (name, bytes): the
/// slice header's items first, then the model's application, then a plate's G-code. None when
/// none of them names a listed slicer.
pub fn of_project(files: &[(String, Vec<u8>)]) -> Option<&'static str> {
    let text = |name: &str| files.iter().find(|(n, _)| n == name).and_then(|(_, d)| std::str::from_utf8(d).ok());
    if let Some(info) = text("Metadata/slice_info.config") {
        let keys = header_keys(info);
        if keys.iter().any(|k| k == "OrcaSlicer-Version") {
            return Some("OrcaSlicer");
        }
        if keys.iter().any(|k| k == "X-SM-Client-Type") {
            return Some("Snapmaker_Orca");
        }
    }
    if let Some(model) = text("3D/3dmodel.model") {
        let items = model_metadata(model);
        let app = items.iter().find(|(n, _)| n == "Application").map_or("", |(_, t)| t.as_str());
        if app.starts_with("Snapmaker_Orca-") {
            return Some("Snapmaker_Orca");
        }
        // The upstream build writes the default family's name here, with an item of its own.
        if app.starts_with("OrcaSlicer-") || (app.starts_with("BambuStudio-") && items.iter().any(|(n, _)| n == "OrcaSlicer")) {
            return Some("OrcaSlicer");
        }
    }
    files.iter().filter(|(n, _)| n.starts_with("Metadata/plate_") && n.ends_with(".gcode")).find_map(|(_, d)| std::str::from_utf8(d).ok().and_then(of_generator))
}

/// The key Fab reads `key`'s setting under, with each of its values as Fab names them.
fn renamed(key: &str, values: &[String]) -> Option<(&'static str, Vec<String>)> {
    let (_, fab, map) = RENAMED.iter().find(|(stored, _, _)| *stored == key)?;
    if map.is_empty() {
        return Some((fab, values.to_vec()));
    }
    let mapped: Option<Vec<String>> = values.iter().map(|v| map.iter().find(|(from, _)| from == v).map(|(_, to)| (*to).to_string())).collect();
    mapped.map(|m| (*fab, m))
}

/// A preset read from the data folder `folder`: the other slicer's keys renamed to Fab's, where
/// the preset does not set Fab's key itself. Nothing changes for a folder not listed. Returns
/// each (stored key, Fab's key) renamed.
pub fn read_names(folder: &str, values: &mut crate::config::Config) -> Vec<(&'static str, &'static str)> {
    let mut done = Vec::new();
    if !is_known(folder) {
        return done;
    }
    let opts = crate::options::options();
    for (stored, _, _) in RENAMED {
        let Some(v) = values.get(stored) else { continue };
        let Some((fab, items)) = renamed(stored, &v.as_vec()) else { continue };
        if values.has(fab) {
            continue;
        }
        // A vector stays one; a single string reads as `fab`'s profile text, as the file
        // reader reads it, so a list written as text becomes a vector.
        let val = match v {
            crate::config::Value::Vector(_) => crate::config::Value::Vector(items),
            crate::config::Value::Scalar(_) => {
                let text = items.into_iter().next().unwrap_or_default();
                if opts.kind(fab).is_some_and(|kind| kind.is_vector()) { crate::config::parse_text(fab, &text) } else { crate::config::Value::Scalar(crate::spellings::read(fab, text)) }
            }
        };
        values.remove(stored);
        values.set(fab, val);
        done.push((*stored, fab));
    }
    done
}

/// The defaults of the slicer whose data folder is `folder`, in order (a later row of the same
/// key wins). Empty for Fab's own and for a folder not listed.
pub fn defaults(folder: &str) -> Vec<(&'static str, &'static str)> {
    TREES.iter().filter(|(f, _)| *f == folder).flat_map(|(_, tables)| tables.iter().flat_map(|t| t.iter().copied())).collect()
}

/// Whether `folder` is a slicer data folder with defaults of its own.
pub fn is_known(folder: &str) -> bool {
    TREES.iter().any(|(f, _)| *f == folder)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_tree_default_is_an_option_or_a_reference_to_one() {
        let opts = crate::options::options();
        for (folder, _) in TREES {
            for (k, v) in defaults(folder) {
                // Read only when present, so not in the default family's table: `bridge_acceleration`,
                // `internal_bridge_speed` and Fab's own `fab_` keys.
                assert!(matches!(k, "bridge_acceleration" | "internal_bridge_speed") || k.starts_with("fab_") || opts.get(k).is_some(), "{folder}: {k} is not an option");
                if let Some(r) = v.strip_prefix('=') {
                    assert!(opts.get(r).is_some(), "{folder}: {k} refers to {r}, not an option");
                } else if let Some(d) = opts.get(k).filter(|d| !d.enum_values.is_empty()) {
                    assert!(d.enum_values.iter().any(|e| e == v), "{folder}: {k} = {v} is not one of its values");
                }
            }
        }
        assert!(defaults(crate::profiles::VENDOR_APP_DIR).is_empty());
        assert!(is_known("Snapmaker_Orca") && !is_known(""));
        for &(_, folder) in GENERATORS {
            assert!(is_known(folder), "{folder}");
        }
        assert_eq!(of_generator("; HEADER_BLOCK_START\n; generated by Snapmaker Orca 2.4.1 on 2026-11-06 at 10:00:00\n"), Some("Snapmaker_Orca"));
        assert_eq!(of_generator("; generated by Snapmaker_Orca 2.4.2 on 2026-11-06 at 10:00:00"), Some("Snapmaker_Orca"));
        assert_eq!(of_generator("; generated by OrcaSlicer 2.5.0-dev on 2026-11-06 at 10:00:00"), Some("OrcaSlicer"));
        for other in ["; generated by Snapmaker OrcaX 1.0", "; generated by Snapmaker Orca Experimental 1.0", "; generated by Snapmaker Orca ", "; generated by OrcaSlicer 2.5 at 10:00", "; generated by Fab 0.1"] {
            assert_eq!(of_generator(other), None, "{other}");
        }
        let file = |name: &str, text: &str| (name.to_string(), text.as_bytes().to_vec());
        let plate = file("Metadata/plate_1.gcode", "; HEADER_BLOCK_START\n; generated by Snapmaker Orca 2.4.1 on 2026-11-06 at 10:00:00\n");
        let info = |key: &str| file("Metadata/slice_info.config", &format!("<config><header><header_item key=\"{key}\" value=\"x\"/></header></config>"));
        let model = |items: &str| file("3D/3dmodel.model", &format!("<model>{items}<resources/></model>"));
        assert_eq!(of_project(&[info("OrcaSlicer-Version"), plate.clone()]), Some("OrcaSlicer"), "the slice header first");
        assert_eq!(of_project(&[info("X-SM-Client-Type")]), Some("Snapmaker_Orca"));
        assert_eq!(of_project(&[info("X-BBL-Client-Type"), model("<metadata name=\"Application\">Snapmaker_Orca-2.4.2</metadata>")]), Some("Snapmaker_Orca"));
        assert_eq!(of_project(&[model("<metadata name=\"Application\">BambuStudio-02.08.01.55</metadata><metadata name=\"OrcaSlicer\">2.5.0-dev</metadata>")]), Some("OrcaSlicer"));
        assert_eq!(of_project(&[model("<metadata name=\"Application\">BambuStudio-02.08.01.55</metadata>"), plate]), Some("Snapmaker_Orca"), "then a plate's G-code");
        assert_eq!(of_project(&[model("<metadata name=\"Application\">BambuStudio-02.08.01.55</metadata>")]), None);
        // Read as XML: any quoting and spacing, any attribute order; comments and the
        // resources name nothing.
        let raw = |name: &str, text: &str| vec![file(name, text)];
        assert_eq!(of_project(&raw("Metadata/slice_info.config", "<config><header><header_item value='x' key = 'X-SM-Client-Type'/></header></config>")), Some("Snapmaker_Orca"));
        assert_eq!(of_project(&raw("Metadata/slice_info.config", "<config><!-- key=\"OrcaSlicer-Version\" --><header><header_item key=\"X-SM-Client-Type\" value=\"x\"/></header></config>")), Some("Snapmaker_Orca"));
        assert_eq!(of_project(&raw("Metadata/slice_info.config", "<config><!-- <header_item key=\"X-SM-Client-Type\"/> --></config>")), None);
        assert_eq!(of_project(&raw("3D/3dmodel.model", "<model><metadata\n type='xs:string' name = 'Application' > Snapmaker_Orca-2.4.2 </metadata><resources/></model>")), Some("Snapmaker_Orca"));
        assert_eq!(of_project(&raw("3D/3dmodel.model", "<model><!-- <metadata name=\"Application\">Snapmaker_Orca-2.4.2</metadata> --><resources/></model>")), None);
        assert_eq!(of_project(&raw("3D/3dmodel.model", "<model><resources/><metadata name=\"Application\">Snapmaker_Orca-2.4.2</metadata></model>")), None);
        assert_eq!(of_project(&raw("3D/3dmodel.model", "<model><metadata name=\"Application\">Snapmaker_Orca-2.4.2</model")), None, "a part that does not parse");
        assert_eq!(of_project(&raw("Metadata/slice_info.config", "<config><header><header_item key=\"X-SM-Client-Type\" value=\"x\"/></header>")), None, "an element left open");
        assert_eq!(of_project(&raw("Metadata/slice_info.config", "<config><header><header_item key=\"X-SM-Client-Type\" value=\"x\"/></config></header>")), None, "elements that do not nest");
        assert_eq!(of_project(&raw("3D/3dmodel.model", "<model><metadata name=\"Application\">Snapmaker_<!-- a note -->Orca-2.4.2</metadata><resources/></model>")), Some("Snapmaker_Orca"), "text around a comment is one text");
        assert_eq!(of_project(&raw("3D/3dmodel.model", "<model><metadata name=\"Application\"><b/>Snapmaker_Orca-2.4.2</metadata><resources/></model>")), None, "an element inside an item");
        assert_eq!(of_project(&raw("3D/3dmodel.model", "<model><metadata name=\"Application\">Snapmaker_Orca-2.4.2</metadata></model>")), Some("Snapmaker_Orca"), "no resources: the whole part is read");
    }

    #[test]
    fn the_other_slicers_keys_read_as_fabs_only_from_its_folders() {
        assert_eq!(renamed("only_one_wall_top", &["1".into()]), Some(("top_one_wall_type", vec!["all top".to_string()])));
        assert_eq!(renamed("wipe_tower_extra_rib_length", &["8".into()]), Some(("prime_tower_extra_rib_length", vec!["8".to_string()])));
        assert_eq!(renamed("wipe_tower_wall_type", &["cone".into()]), None, "a value Fab has no name for stays as stored");
        assert_eq!(renamed("prime_tower_rib_wall", &["1".into()]), None, "Fab's own key");
        assert_eq!(renamed("wipe_tower_extra_spacing", &["120%".into()]), Some(("prime_tower_infill_gap", vec!["120%".to_string()])));
        assert_eq!(renamed("filament_multitool_ramming_flow", &["25".into()]), Some(("filament_ramming_volumetric_speed", vec!["25".to_string()])));
        let mut values = crate::config::Config::new();
        values.set_str("only_one_wall_top", "1");
        values.set_str("wall_infill_order", "inner wall/outer wall/infill");
        values.set_str("extruder_clearance_radius", "49");
        values.set_str("extruder_clearance_max_radius", "68");
        let mut own = values.clone();
        read_names(crate::profiles::VENDOR_APP_DIR, &mut own);
        assert_eq!(own.str("only_one_wall_top"), "1", "the default family's presets keep these keys unread");
        assert!(!own.has("top_one_wall_type"));
        read_names("OrcaSlicer", &mut values);
        assert_eq!((values.str("top_one_wall_type"), values.str("wall_sequence")), ("all top", "inner wall/outer wall"));
        assert!(!values.has("only_one_wall_top"));
        assert_eq!(values.str("extruder_clearance_max_radius"), "68", "a key the preset sets itself wins");
        let mut text = crate::config::Config::new();
        text.set_str("chamber_temperature", "40,50");
        read_names("OrcaSlicer", &mut text);
        assert_eq!(text.get("chamber_temperatures").map(|v| v.as_vec()), Some(vec!["40".to_string(), "50".to_string()]), "a list written as text reads as a vector");
        let opts = crate::options::options();
        for (stored, fab, map) in RENAMED {
            assert!(opts.get(stored).is_none(), "{stored} is one of Fab's own keys");
            let d = opts.get(fab).unwrap_or_else(|| panic!("{fab} is not an option"));
            for (_, to) in *map {
                // The option table splits its value list at '/', which wall orders contain.
                let listed = d.enum_values.iter().any(|v| v == to) || (to.contains('/') && d.enum_values.join("/").contains(to));
                assert!(d.enum_values.is_empty() || listed, "{fab}: {to}");
            }
        }
    }
}
