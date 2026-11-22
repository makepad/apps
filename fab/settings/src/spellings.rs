//! Enum values that other slicers store under a different name for the meaning Fab reads.
//! Fab's names are the default family's. A preset value spelled another way is read as Fab's
//! when the preset loads, so the settings, the job and its settings block all show one name.
//! Each value set differs exactly where the spellings do, so no stored value means two things.

/// The keys that share the fill-pattern enum.
const FILL: &[&str] = &["sparse_infill_pattern", "top_surface_pattern", "bottom_surface_pattern", "internal_solid_infill_pattern", "ironing_pattern", "support_ironing_pattern"];

/// (keys, stored value, Fab's value for the same meaning).
const SAME: &[(&[&str], &str, &str)] = &[
    // Parallel lines linked end to end. The other `zigzag`, without the hyphen, is another pattern.
    (FILL, "rectilinear", "zig-zag"),
    (&["support_style"], "organic", "tree_organic"),
    (&["ensure_vertical_shell_thickness"], "none", "disabled"),
    (&["ensure_vertical_shell_thickness"], "ensure_all", "enabled"),
    // A switch stored as an enum.
    (&["enable_arc_fitting"], "disabled", "0"),
    (&["enable_arc_fitting"], "enabled", "1"),
    (&["filament_map_mode"], "Auto", "Auto For Flush"),
    (&["curr_bed_type"], "SuperTack Plate", "Supertack Plate"),
];

/// `value` of `key` as Fab names it.
pub fn read(key: &str, value: String) -> String {
    match SAME.iter().find(|(keys, stored, _)| *stored == value && keys.contains(&key)) {
        Some((_, _, fab)) => (*fab).to_string(),
        None => value,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn another_spelling_reads_as_fabs_and_fabs_own_stays() {
        assert_eq!(read("internal_solid_infill_pattern", "rectilinear".into()), "zig-zag");
        assert_eq!(read("ironing_pattern", "rectilinear".into()), "zig-zag");
        assert_eq!(read("sparse_infill_pattern", "zigzag".into()), "zigzag", "a different pattern");
        assert_eq!(read("sparse_infill_pattern", "zig-zag".into()), "zig-zag");
        assert_eq!(read("support_base_pattern", "rectilinear".into()), "rectilinear", "the support enum has its own rectilinear");
        assert_eq!(read("support_style", "organic".into()), "tree_organic");
        assert_eq!(read("enable_arc_fitting", "disabled".into()), "0");
        assert_eq!(read("filament_map_mode", "Auto".into()), "Auto For Flush");
        // Every value a row maps to is one Fab's option table offers for each of its keys.
        let opts = crate::options::options();
        for (keys, _, fab) in SAME {
            for k in *keys {
                let d = opts.get(k).unwrap_or_else(|| panic!("{k} is not an option"));
                assert!(d.enum_values.is_empty() || d.enum_values.iter().any(|v| v == fab), "{k}: {fab}");
            }
        }
    }
}
