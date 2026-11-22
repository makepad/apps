//! The commands a printer's firmware takes for what standard G-code leaves out: object labels,
//! the change counter, the idle nozzle's temperature, pressure advance, the spiral lift, the
//! auxiliary fan, the print's progress and the file's header. The writer says what it wants;
//! the job's printer's firmware ([`Flavor`]) says how it is written.
//!
//! Moves, extrusion, the part fan and Fab's own comment tags (`; FEATURE:`,
//! `; CHANGE_LAYER`, the object label comments) Fab keeps common to every firmware: the preview,
//! the time model and the print check read them.

use crate::fmt;
use crate::writer::Stats;
use makepad_fab_settings::family::Flavor;
use makepad_fab_settings::machine::Job;

/// The print's facts the file's header states.
pub struct Head<'a> {
    pub generator: &'a str,
    pub stats: &'a Stats,
    pub layers: usize,
    /// The filaments used (0-based), sorted.
    pub used: &'a [usize],
    /// The object label ids and each object's height, when the plate labels its objects.
    pub labels: Option<(&'a [u32], Vec<f64>)>,
    /// Preset density and diameter text: used filaments for the default family, all slots
    /// for the second family.
    pub density: String,
    pub diameter: String,
    pub max_z: f64,
    pub filaments: usize,
    pub changes: usize,
    pub objects: Vec<Object>,
}

/// A named object's bed footprint and centre.
pub struct Object {
    pub name: String,
    pub center: [f64; 2],
    pub polygon: Vec<[f64; 2]>,
}

/// Replace runs of the named-object protocol's reserved characters, then append the plate id.
pub fn object_name(name: &str, object: usize) -> String {
    let mut out = String::new();
    let mut reserved = false;
    for c in name.chars() {
        if " !@#$%^&*()=+[]{};:\",'".contains(c) {
            if !reserved { out.push('_'); }
            reserved = true;
        } else {
            out.push(c);
            reserved = false;
        }
    }
    format!("{}_id_{object}_copy_0", out.trim_matches('_'))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Firmware(pub Flavor);

/// The object-skip mask: a 64-bit set, least significant byte first, base64.
fn b64_mask(bits: u64) -> String {
    String::from_utf8(makepad_base64::base64_encode(&bits.to_le_bytes(), &makepad_base64::BASE64_STANDARD)).unwrap_or_default()
}

impl Firmware {
    pub fn of(family: &makepad_fab_settings::family::Family) -> Self {
        Firmware(family.flavor)
    }

    /// Translate a filament slot to the tool number this firmware accepts.
    pub fn tool(&self, job: &Job, filament: usize) -> usize {
        match self.0 {
            Flavor::Mask => job.physical_index(job.ext_of(filament)),
            Flavor::Named => filament,
        }
    }

    /// The loaded nozzle, preserving the default family's optional comment.
    pub fn loaded_temperature(&self, tool: usize, temp: f64, why: Option<&str>) -> String {
        match self.0 {
            Flavor::Mask => {
                let mut line = format!("M104 S{}", temp.round());
                if let Some(why) = why { line.push_str(&format!(" ; {why}")); }
                line
            }
            Flavor::Named => format!("M104 S{} T{tool}", temp.round()),
        }
    }

    /// Label even a single object, keeping shared commands outside its run.
    pub fn labels_every_object(&self) -> bool {
        match self.0 {
            Flavor::Mask => false,
            Flavor::Named => true,
        }
    }

    pub fn header_all_filaments(&self) -> bool {
        match self.0 {
            Flavor::Mask => false,
            Flavor::Named => true,
        }
    }

    pub fn defines_objects(&self) -> bool {
        match self.0 {
            Flavor::Mask => false,
            Flavor::Named => true,
        }
    }

    /// Change acceleration, with an optional acceleration-to-deceleration value.
    pub fn acceleration(&self, a: f64, to_decel: Option<f64>) -> String {
        match self.0 {
            Flavor::Mask => format!("M204 S{}", fmt::trim(a, 0)),
            Flavor::Named => {
                let mut line = format!("SET_VELOCITY_LIMIT ACCEL={}", fmt::trim(a, 0));
                if let Some(v) = to_decel { line.push_str(&format!(" ACCEL_TO_DECEL={}", fmt::trim(v, 0))); }
                line
            }
        }
    }

    /// The lines after the printer's start sequence, before the first layer: the units and
    /// extrusion mode, and what the firmware watches during the print.
    pub fn begin(&self, first_filament: usize) -> Vec<String> {
        match self.0 {
            Flavor::Named => vec!["G90".into(), "G21".into(), "M83 ; use relative distances for extrusion".into()],
            Flavor::Mask => vec![format!(";VT{first_filament} H-1"), "G90".into(), "G21".into(), "M83 ; use relative distances for extrusion".into(), "M981 S1 P20000 ;open spaghetti detector".into()],
        }
    }

    /// The line before the end sequence: what `begin` started watching stops.
    pub fn finish(&self) -> Option<&'static str> {
        match self.0 {
            Flavor::Mask => Some("M981 S0 P20000 ; close spaghetti detector"),
            Flavor::Named => None,
        }
    }

    /// Power-loss recovery on from the second layer, off at the end.
    pub fn recovery(&self, on: bool) -> Vec<&'static str> {
        match (self.0, on) {
            (Flavor::Named, _) => Vec::new(),
            (Flavor::Mask, true) => vec!["; open powerlost recovery", "M1003 S1"],
            (Flavor::Mask, false) => vec!["; close powerlost recovery", "M1003 S0"],
        }
    }

    /// The printer's own pause, when its profile names none (`machine_pause_gcode`): the default
    /// family's waits for the person at the printer, the second family's runs its PAUSE macro.
    pub fn pause(&self) -> &'static str {
        match self.0 {
            Flavor::Mask => "M400 U1",
            Flavor::Named => "PAUSE",
        }
    }

    /// The active nozzle's temperature (firmware tool `tool`), waiting until it is there.
    pub fn wait_nozzle_temperature(&self, tool: usize, temp: f64) -> String {
        match self.0 {
            Flavor::Mask => format!("M109 S{}", temp.round()),
            Flavor::Named => format!("M109 S{} T{tool}", temp.round()),
        }
    }

    /// The active nozzle's temperature (firmware tool `tool`), without waiting.
    pub fn nozzle_temperature(&self, tool: usize, temp: f64) -> String {
        match self.0 {
            Flavor::Mask | Flavor::Named => format!("M104 S{} T{tool}", temp.round()),
        }
    }

    /// Another nozzle's temperature (firmware tool `tool`) while the active one prints on: no
    /// wait, `why` as the comment.
    pub fn idle_temperature(&self, tool: usize, temp: f64, why: &str) -> String {
        match self.0 {
            Flavor::Named => format!("M104 T{tool} S{} ; {why}", temp.round()),
            Flavor::Mask => format!("M104 T{tool} S{} N0 ;{why}", temp.round()),
        }
    }

    /// The idle nozzle cooled while it waits (after the moves before it finish).
    pub fn idle_cooling(&self, tool: usize, temp: f64) -> Vec<String> {
        match self.0 {
            Flavor::Named => vec![self.idle_temperature(tool, temp, "Multi extruder pre cooling")],
            Flavor::Mask => vec!["M400".into(), self.idle_temperature(tool, temp, "Multi extruder pre cooling")],
        }
    }

    /// The idle nozzle heated back up ahead of its next filament (0-based); the firmware may
    /// skip the bracket.
    pub fn idle_heating(&self, tool: usize, temp: f64, next_filament: usize) -> Vec<String> {
        match self.0 {
            Flavor::Named => vec![self.idle_temperature(tool, temp, "Multi extruder pre heating")],
            Flavor::Mask => vec![format!("M632 S{next_filament} W"), self.idle_temperature(tool, temp, "Multi extruder pre heating"), "M633".into()],
        }
    }

    pub fn pressure_advance(&self, pa: f64) -> String {
        match self.0 {
            Flavor::Named => format!("SET_PRESSURE_ADVANCE ADVANCE={}", format!("{pa:.4}").trim_end_matches('0').trim_end_matches('.')),
            Flavor::Mask => format!("M900 K{pa:.3}"),
        }
    }

    /// The auxiliary fan (0..255).
    pub fn aux_fan(&self, speed: i32) -> String {
        match self.0 {
            Flavor::Mask | Flavor::Named => format!("M106 P2 S{speed}"),
        }
    }

    /// A set feed that later passes of the firmware leave alone (mm/min).
    pub fn fixed_feed(&self, feed: f64) -> String {
        match self.0 {
            Flavor::Named => format!("G1 F{}", fmt::f(feed)),
            Flavor::Mask => format!("G1 F{};_EXTRUDE_SET_SPEED", fmt::f(feed)),
        }
    }

    /// A lift as one helical turn up to `z` round the centre at (`i`, `j`) from the head, in the
    /// XY plane.
    pub fn spiral_lift(&self, z: f64, i: f64, j: f64, feed: f64) -> Option<[String; 2]> {
        match self.0 {
            Flavor::Named => None,
            Flavor::Mask => Some(["G17".into(), format!("G3 Z{} I{} J{} P1  F{}", fmt::xyz(z), fmt::xyz(i), fmt::xyz(j), fmt::f(feed))]),
        }
    }

    /// What prints next belongs to object `object` (0-based on the plate).
    pub fn label_object(&self, object: usize, name: &str) -> String {
        match self.0 {
            Flavor::Named => format!("EXCLUDE_OBJECT_START NAME={name}"),
            Flavor::Mask => format!("M624 {}", b64_mask(1u64 << object.min(63))),
        }
    }

    /// What prints next belongs to every object in `mask` (the tower and the timelapse, which
    /// the print needs whichever objects are skipped).
    pub fn label_objects(&self, mask: u64) -> Option<String> {
        match self.0 {
            Flavor::Mask => Some(format!("M624 {}", b64_mask(mask))),
            Flavor::Named => None,
        }
    }

    /// The labelled run ends.
    pub fn label_end(&self, name: Option<&str>) -> Option<String> {
        match self.0 {
            Flavor::Mask => Some("M625".into()),
            Flavor::Named => name.map(|name| format!("EXCLUDE_OBJECT_END NAME={name}")),
        }
    }

    /// The line after a tool select: the changes left in the print.
    pub fn after_tool(&self, changes_left: usize) -> Option<String> {
        match self.0 {
            Flavor::Mask => Some(format!("M73 E{changes_left}")),
            Flavor::Named => None,
        }
    }

    /// The tower's tag where a change's wipe starts (change `count`, whether it changed nozzle).
    pub fn tower_wipe(&self, count: usize, nozzle_change: bool) -> String {
        match self.0 {
            Flavor::Mask | Flavor::Named => format!("; CP_TOOLCHANGE_WIPE CT{count} FL{}", if nozzle_change { 1 } else { 0 }),
        }
    }

    /// The print's progress: `percent` done, `minutes` left.
    pub fn progress(&self, percent: i64, minutes: i64) -> String {
        match self.0 {
            Flavor::Mask | Flavor::Named => format!("M73 P{percent} R{minutes}"),
        }
    }

    /// The file up to its moves: the header, the settings block (`config`: its `; key = value`
    /// lines) and the start of the moves.
    pub fn head(&self, text: &mut String, h: &Head, config: impl FnOnce(&mut String)) {
        match self.0 {
            Flavor::Named => {
                text.push_str("; HEADER_BLOCK_START\n");
                text.push_str(&format!("; generated by {}\n; total layer number: {}\n", h.generator, h.layers));
                text.push_str(&format!("; filament_density: {}\n; filament_diameter: {}\n; max_z_height: {:.2}\n", h.density, h.diameter, h.max_z));
                text.push_str("; HEADER_BLOCK_END\n\n; EXECUTABLE_BLOCK_START\n");
                for o in &h.objects {
                    // A closed outline: the first point again at the end.
                    let polygon = o.polygon.iter().chain(o.polygon.first()).map(|p| format!("[{},{}]", fmt::xyz(p[0]), fmt::xyz(p[1]))).collect::<Vec<_>>().join(",");
                    text.push_str(&format!("EXCLUDE_OBJECT_DEFINE NAME={} CENTER={},{} POLYGON=[{}]\n", o.name, fmt::xyz(o.center[0]), fmt::xyz(o.center[1]), polygon));
                }
            }
            Flavor::Mask => {
                let list = |v: &[f64], d: usize| -> String { h.used.iter().map(|&f| format!("{:.*}", d, v.get(f).copied().unwrap_or(0.0))).collect::<Vec<_>>().join(",") };
                text.push_str("; HEADER_BLOCK_START\n");
                text.push_str(&format!("; {}\n", h.generator));
                text.push_str(&format!("; model printing time: {}; total estimated time: {}\n", fmt::duration(h.stats.model_time), fmt::duration(h.stats.total_time)));
                text.push_str(&format!("; total layer number: {}\n", h.layers));
                text.push_str(&format!("; total filament length [mm] : {}\n", list(&h.stats.filament_mm, 2)));
                // The volume line carries mm³ under a cm³ label in the printer format.
                text.push_str(&format!("; total filament volume [cm^3] : {}\n", list(&h.stats.filament_mm3, 2)));
                text.push_str(&format!("; total filament weight [g] : {}\n", list(&h.stats.filament_g, 2)));
                if let Some((ids, heights)) = &h.labels {
                    text.push_str(&format!("; model label id: {}\n", ids.iter().map(|i| i.to_string()).collect::<Vec<_>>().join(",")));
                    text.push_str(&format!("; object max height: {}\n", heights.iter().map(|z| format!("{z:.2}")).collect::<Vec<_>>().join(",")));
                }
                text.push_str(&format!("; filament_density: {}\n", h.density));
                text.push_str(&format!("; filament_diameter: {}\n", h.diameter));
                text.push_str(&format!("; max_z_height: {:.2}\n", h.max_z));
                text.push_str(&format!("; filament: {}\n", h.used.iter().map(|f| (f + 1).to_string()).collect::<Vec<_>>().join(",")));
                text.push_str("; support_material_on_wipe_tower: 0\n");
                text.push_str("; HEADER_BLOCK_END\n\n");
                text.push_str("; CONFIG_BLOCK_START\n");
                config(text);
                text.push_str("; CONFIG_BLOCK_END\n\n");
                text.push_str("; EXECUTABLE_BLOCK_START\n");
            }
        }
    }

    /// The file after its moves, including settings (`config`) for the second family.
    pub fn tail(&self, text: &mut String, h: &Head, config: impl FnOnce(&mut String)) {
        match self.0 {
            Flavor::Mask => text.push_str("; EXECUTABLE_BLOCK_END\n\n"),
            Flavor::Named => {
                text.push_str("; EXECUTABLE_BLOCK_END\n\n");
                let list = |v: &[f64], scale: f64| (0..h.filaments).map(|f| format!("{:.2}", v.get(f).copied().unwrap_or(0.0) * scale)).collect::<Vec<_>>().join(",");
                text.push_str(&format!("; filament used [mm] = {}\n", list(&h.stats.filament_mm, 1.0)));
                text.push_str(&format!("; filament used [cm3] = {}\n", list(&h.stats.filament_mm3, 0.001)));
                text.push_str(&format!("; filament used [g] = {}\n", list(&h.stats.filament_g, 1.0)));
                text.push_str(&format!("; total filament used [g] = {:.2}\n", h.stats.filament_g.iter().sum::<f64>()));
                if h.changes > 0 { text.push_str(&format!("; total filament change = {}\n", h.changes)); }
                text.push_str(&format!("; total layers count = {}\n; estimated printing time (normal mode) = {}\n\n", h.layers, fmt::duration(h.stats.total_time)));
                text.push_str("; CONFIG_BLOCK_START\n");
                config(text);
                text.push_str("; CONFIG_BLOCK_END\n");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn acceleration_and_loaded_temperature_preserve_each_family() {
        let named = Firmware(Flavor::Named);
        let default = Firmware(Flavor::Mask);
        assert_eq!(named.acceleration(10000.0, None), "SET_VELOCITY_LIMIT ACCEL=10000");
        assert_eq!(named.acceleration(10000.0, Some(5000.0)), "SET_VELOCITY_LIMIT ACCEL=10000 ACCEL_TO_DECEL=5000");
        assert_eq!(named.acceleration(1234.6, Some(617.3)), "SET_VELOCITY_LIMIT ACCEL=1235 ACCEL_TO_DECEL=617");
        for a in [10000.0, 1234.6, 1234.5] {
            assert_eq!(default.acceleration(a, None), format!("M204 S{}", fmt::trim(a, 0)));
            assert_eq!(default.acceleration(a, Some(a / 2.0)), default.acceleration(a, None));
        }
        assert_eq!(default.acceleration(10000.0, None), "M204 S10000");
        for why in [None, Some("set nozzle temperature")] {
            assert_eq!(named.loaded_temperature(3, 210.4, why), "M104 S210 T3");
        }
        assert_eq!(default.loaded_temperature(3, 210.4, None), "M104 S210");
        assert_eq!(default.loaded_temperature(3, 210.4, Some("set nozzle temperature")), "M104 S210 ; set nozzle temperature");
    }

    #[test]
    fn named_begin_and_finish_omit_watchers_and_recovery() {
        let d = Firmware(Flavor::Named);
        assert_eq!(d.begin(3), ["G90", "G21", "M83 ; use relative distances for extrusion"]);
        assert_eq!(d.finish(), None);
        assert!(d.recovery(true).is_empty());
        assert!(d.recovery(false).is_empty());
        assert_eq!(d.after_tool(4), None);
    }

    #[test]
    fn named_temperatures_use_filament_tools_without_brackets() {
        let d = Firmware(Flavor::Named);
        let mut cfg = makepad_fab_settings::config::Config::new();
        cfg.set_vec("nozzle_diameter", vec!["0.4".into(); 4]);
        cfg.set_vec("filament_map", vec!["4".into(), "2".into()]);
        let job = Job::new(cfg);
        assert_eq!(d.tool(&job, 0), 0);
        assert_eq!(d.tool(&job, 1), 1);
        assert_eq!(Firmware(Flavor::Mask).tool(&job, 0), job.physical_index(job.ext_of(0)));
        assert_eq!(d.nozzle_temperature(31, 210.4), "M104 S210 T31");
        assert_eq!(d.idle_temperature(3, 180.0, "cool"), "M104 T3 S180 ; cool");
        assert_eq!(d.idle_cooling(3, 180.0), ["M104 T3 S180 ; Multi extruder pre cooling"]);
        assert_eq!(d.idle_heating(1, 220.0, 1), ["M104 T1 S220 ; Multi extruder pre heating"]);
    }

    #[test]
    fn named_pressure_advance_trims_four_decimals() {
        let d = Firmware(Flavor::Named);
        assert_eq!(d.pressure_advance(0.01234), "SET_PRESSURE_ADVANCE ADVANCE=0.0123");
        assert_eq!(d.pressure_advance(0.02), "SET_PRESSURE_ADVANCE ADVANCE=0.02");
        assert_eq!(d.pressure_advance(0.0), "SET_PRESSURE_ADVANCE ADVANCE=0");
    }

    #[test]
    fn named_feed_fans_progress_and_lifts() {
        let d = Firmware(Flavor::Named);
        assert_eq!(d.fixed_feed(1234.5), "G1 F1234.5");
        assert_eq!(d.aux_fan(128), "M106 P2 S128");
        assert_eq!(d.progress(42, 3), "M73 P42 R3");
        assert_eq!(d.spiral_lift(1.0, 2.0, 3.0, 1200.0), None);
        assert_eq!(d.tower_wipe(2, true), "; CP_TOOLCHANGE_WIPE CT2 FL1");
    }

    #[test]
    fn named_labels_sanitize_names_and_leave_shared_moves_unlabelled() {
        let d = Firmware(Flavor::Named);
        let name = object_name("_ !part @#$%^&*()=+[]{};:\",' two!_", 2);
        assert_eq!(name, "part_two_id_2_copy_0");
        assert_eq!(object_name("!@#", 0), "_id_0_copy_0");
        assert_eq!(object_name("a-b.c/é", 1), "a-b.c/é_id_1_copy_0");
        assert_eq!(d.label_object(2, &name), format!("EXCLUDE_OBJECT_START NAME={name}"));
        assert_eq!(d.label_end(Some(&name)), Some(format!("EXCLUDE_OBJECT_END NAME={name}")));
        assert_eq!(d.label_objects(7), None);
        assert_eq!(d.label_end(None), None);
    }

    #[test]
    fn named_head_and_tail_put_definitions_first_and_settings_last() {
        let d = Firmware(Flavor::Named);
        let stats = Stats { filament_mm: vec![12.5, 0.0, 20.0], filament_mm3: vec![1250.0, 0.0, 500.0], filament_g: vec![1.5, 0.0, 0.6], total_time: 90061.0, ..Default::default() };
        let mut h = Head {
            generator: "test", stats: &stats, layers: 3, used: &[0, 2], labels: None,
            density: "1.2,1.2,1.2".into(), diameter: "1.75,1.75,1.75".into(), max_z: 0.6,
            filaments: 3, changes: 2,
            objects: vec![Object { name: object_name("part", 0), center: [1.25, 2.0], polygon: vec![[0.0, 0.0], [2.5, 0.0], [2.5, 4.0]] }],
        };
        let mut text = String::new();
        d.head(&mut text, &h, |_| panic!("settings belong in the trailer"));
        assert_eq!(text, "; HEADER_BLOCK_START\n; generated by test\n; total layer number: 3\n; filament_density: 1.2,1.2,1.2\n; filament_diameter: 1.75,1.75,1.75\n; max_z_height: 0.60\n; HEADER_BLOCK_END\n\n; EXECUTABLE_BLOCK_START\nEXCLUDE_OBJECT_DEFINE NAME=part_id_0_copy_0 CENTER=1.25,2 POLYGON=[[0,0],[2.5,0],[2.5,4],[0,0]]\n");
        text.push_str("M73 P100 R0\n");
        let mut tail = String::new();
        d.tail(&mut tail, &h, |s| s.push_str("; key = value\n"));
        assert_eq!(tail, "; EXECUTABLE_BLOCK_END\n\n; filament used [mm] = 12.50,0.00,20.00\n; filament used [cm3] = 1.25,0.00,0.50\n; filament used [g] = 1.50,0.00,0.60\n; total filament used [g] = 2.10\n; total filament change = 2\n; total layers count = 3\n; estimated printing time (normal mode) = 1d 1h 1m 1s\n\n; CONFIG_BLOCK_START\n; key = value\n; CONFIG_BLOCK_END\n");
        h.changes = 0;
        tail.clear();
        d.tail(&mut tail, &h, |_| {});
        assert!(!tail.contains("total filament change"));
    }

    /// The mask is the 64-bit set's bytes, least significant first, in base64.
    #[test]
    fn object_masks_are_base64_of_the_little_endian_set() {
        let d = Firmware(Flavor::Mask);
        assert_eq!(d.label_object(0, ""), "M624 AQAAAAAAAAA=");
        assert_eq!(d.label_object(9, ""), "M624 AAIAAAAAAAA=");
        assert_eq!(d.label_object(200, ""), d.label_object(63, ""), "objects past 64 share the last bit");
        assert_eq!(d.label_objects(0b111).as_deref(), Some("M624 BwAAAAAAAAA="));
    }
}
