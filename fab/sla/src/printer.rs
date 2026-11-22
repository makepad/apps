//! A masked resin printer's profile: its screen (pixels, pitch, mirror, turn, grey levels), its
//! height, and how a job exposes and peels each layer.
//!
//! It is read from the same flat configuration the filament printers use
//! ([`makepad_fab_settings::Config`]): a printer preset whose `printer_technology` is `SLA`,
//! with the `fab_sla_*` keys of [`makepad_fab_settings::options`], and a process preset
//! (`layer_height` and the exposure and peel keys). The two presets laid over each other are
//! one configuration; [`ResinProfile::from_config`] reads it.

use fab_plate::machine::{Machine, ResinScreen};
use makepad_fab_settings::config::{parse_num, parse_text};
use makepad_fab_settings::profiles::RawPreset;
use makepad_fab_settings::Config;

/// How a layer is peeled off the vat floor: the plate lifts `height` mm at `speed` mm/s.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Lift {
    pub height: f64,
    pub speed: f64,
}

/// The file a resin printer reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Container {
    /// Fab's own folder: an image per layer and a manifest.
    Folder,
    /// The printer's archive ([`crate::archive`]).
    Archive,
}

/// How the printer's archive holds each layer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Encoding {
    /// Grey pixels, run-length coded.
    Raster,
    /// Outlines the printer fills itself.
    Vector,
}

/// How a job gets to the printer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Route {
    /// Fab's own link ([`crate::link`]).
    Link,
    /// A removable drive: Fab writes the file onto it, the person carries it over and starts the
    /// print on the printer.
    Drive,
}

/// One of the maker's settings groups: the archive copies the values Fab doesn't set (the
/// maker's own, for that group) from it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Group {
    Normal,
    Fast,
    HighSpeed,
}

/// The resin in the vat, for the archive's cost line.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Bottle {
    /// A bottle's price.
    pub price: f64,
    /// A bottle's resin (ml).
    pub ml: f64,
    /// g/ml.
    pub density: f64,
}

/// What a job is written as and how it gets to the printer.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Output {
    pub container: Container,
    pub encoding: Encoding,
    pub route: Route,
    /// The anti-alias level the archive says its layers have (1 to 16).
    pub anti_alias: u8,
    pub group: Group,
    pub bottle: Bottle,
}

/// A resin printer and its job settings.
#[derive(Clone, Debug, PartialEq)]
pub struct ResinProfile {
    /// The printer preset's name.
    pub name: String,
    pub screen: ResinScreen,
    /// The plate's travel (mm): the tallest print.
    pub max_height: f64,
    /// One layer (mm).
    pub layer_height: f64,
    /// The first layers, exposed longer so they hold to the plate.
    pub bottom_layers: u32,
    /// Seconds of light per bottom layer.
    pub bottom_exposure: f64,
    /// Seconds of light per layer after them.
    pub exposure: f64,
    /// Seconds with the light off after each move, so the resin settles.
    pub light_off_delay: f64,
    pub bottom_lift: Lift,
    pub lift: Lift,
    /// The plate's speed back down after a lift (mm/s).
    pub retract_speed: f64,
    pub output: Output,
}

/// Whether a printer preset's values describe a masked resin printer.
pub fn is_resin(values: &Config) -> bool {
    values.str("printer_technology").trim() == "SLA"
}

/// The resin printer a printer preset's values describe: its screen and its height, as the
/// plate sees it ([`Machine::resin`]).
pub fn resin_machine(name: &str, values: &Config) -> Result<Machine, String> {
    let invalid = |key: &str| format!("the resin printer {name}: missing or invalid {key}");
    if !is_resin(values) {
        return Err(format!("the printer {name} is not a resin printer (printer_technology {:?})", values.str("printer_technology")));
    }
    let point = |key: &str| -> Result<[f64; 2], String> {
        match values.points(key).as_slice() {
            [(x, y)] if x.is_finite() && y.is_finite() && *x > 0.0 && *y > 0.0 => Ok([*x, *y]),
            _ => Err(invalid(key)),
        }
    };
    let resolution = point("fab_sla_resolution")?;
    if resolution.iter().any(|r| r.fract() != 0.0 || *r > 65536.0) {
        return Err(invalid("fab_sla_resolution"));
    }
    let pitch = point("fab_sla_pixel_pitch")?;
    let mirror = match values.str("fab_sla_mirror").trim() {
        "" | "none" => [false, false],
        "x" => [true, false],
        "y" => [false, true],
        "xy" => [true, true],
        _ => return Err(invalid("fab_sla_mirror")),
    };
    let quarter_turns = match values.str("fab_sla_rotation").trim() {
        "" | "0" => 0,
        "90" => 1,
        "180" => 2,
        "270" => 3,
        _ => return Err(invalid("fab_sla_rotation")),
    };
    let grey_levels = match values.str("fab_sla_grey_levels").trim() {
        "" => 256,
        t => t.parse::<u16>().ok().filter(|g| (2..=256).contains(g)).ok_or_else(|| invalid("fab_sla_grey_levels"))?,
    };
    let screen = ResinScreen { pixels: [resolution[0] as u32, resolution[1] as u32], pitch, mirror, quarter_turns, grey_levels };
    // The printable area, when the preset gives one, is the screen's.
    let area = values.points("printable_area");
    if !area.is_empty() {
        let (mut lo, mut hi) = ([f64::INFINITY; 2], [f64::NEG_INFINITY; 2]);
        for (x, y) in &area {
            lo = [lo[0].min(*x), lo[1].min(*y)];
            hi = [hi[0].max(*x), hi[1].max(*y)];
        }
        let s = screen.area();
        let off = (lo[0] - s.min[0]).abs().max((lo[1] - s.min[1]).abs()).max((hi[0] - s.max[0]).abs()).max((hi[1] - s.max[1]).abs());
        if off > 0.5 * pitch[0].max(pitch[1]) {
            return Err(format!("the resin printer {name}: its printable_area is not its screen's {:.2} × {:.2} mm", s.width(), s.depth()));
        }
    }
    let height = parse_num(values.str("printable_height")).filter(|v| *v > 0.0).ok_or_else(|| invalid("printable_height"))?;
    Ok(Machine::resin(name, screen, height))
}

impl ResinProfile {
    /// The profile `cfg` describes (a resin printer preset with a process preset over it).
    pub fn from_config(name: &str, cfg: &Config) -> Result<ResinProfile, String> {
        ResinProfile::on(&resin_machine(name, cfg)?, cfg)
    }

    /// The job `cfg` sets (its process keys: the layer height, exposures and peels) on the resin
    /// printer `machine`.
    pub fn on(machine: &Machine, cfg: &Config) -> Result<ResinProfile, String> {
        let name = &machine.name;
        let invalid = |key: &str| format!("the resin printer {name}: missing or invalid {key}");
        let screen = *machine.resin_screen().ok_or_else(|| format!("the printer {name} is not a resin printer"))?;
        let positive = |key: &str| -> Result<f64, String> { parse_num(cfg.str(key)).filter(|v| *v > 0.0).ok_or_else(|| invalid(key)) };
        let at_least_zero = |key: &str| -> Result<f64, String> { parse_num(cfg.str(key)).filter(|v| *v >= 0.0).ok_or_else(|| invalid(key)) };
        let bottom_layers = parse_num(cfg.str("fab_sla_bottom_layers")).filter(|v| *v >= 0.0 && v.fract() == 0.0).ok_or_else(|| invalid("fab_sla_bottom_layers"))? as u32;
        let choice = |key: &str| -> &str { cfg.str(key).trim() };
        let container = match choice("fab_sla_container") {
            "" | "nil" | "folder" => Container::Folder,
            "archive" => Container::Archive,
            _ => return Err(invalid("fab_sla_container")),
        };
        let encoding = match choice("fab_sla_layer_encoding") {
            "" | "nil" | "raster" => Encoding::Raster,
            "vector" => Encoding::Vector,
            _ => return Err(invalid("fab_sla_layer_encoding")),
        };
        let route = match choice("fab_sla_route") {
            "" | "nil" | "link" => Route::Link,
            "drive" if container == Container::Archive => Route::Drive,
            "drive" => return Err(format!("the resin printer {name}: fab_sla_route drive carries the printer's archive (fab_sla_container archive)")),
            _ => return Err(invalid("fab_sla_route")),
        };
        let group = match choice("fab_sla_group") {
            "" | "nil" | "normal" => Group::Normal,
            "fast" => Group::Fast,
            "high_speed" => Group::HighSpeed,
            _ => return Err(invalid("fab_sla_group")),
        };
        let or = |key: &str, default: f64| -> Result<f64, String> {
            match choice(key) {
                "" | "nil" => Ok(default),
                t => parse_num(t).filter(|v| *v > 0.0).ok_or_else(|| invalid(key)),
            }
        };
        let anti_alias = or("fab_sla_anti_alias", 16.0)?;
        if anti_alias.fract() != 0.0 || !(1.0..=16.0).contains(&anti_alias) {
            return Err(invalid("fab_sla_anti_alias"));
        }
        let bottle = Bottle { price: or("fab_sla_resin_price", 220.0)?, ml: or("fab_sla_resin_bottle", 1000.0)?, density: or("fab_sla_resin_density", 1.2)? };
        let output = Output { container, encoding, route, anti_alias: anti_alias as u8, group, bottle };
        let profile = ResinProfile {
            name: name.clone(),
            screen,
            max_height: machine.max_height,
            layer_height: positive("layer_height")?,
            bottom_layers,
            bottom_exposure: positive("fab_sla_bottom_exposure")?,
            exposure: positive("fab_sla_exposure")?,
            light_off_delay: at_least_zero("fab_sla_light_off_delay")?,
            bottom_lift: Lift { height: at_least_zero("fab_sla_bottom_lift_height")?, speed: positive("fab_sla_bottom_lift_speed")? },
            lift: Lift { height: at_least_zero("fab_sla_lift_height")?, speed: positive("fab_sla_lift_speed")? },
            retract_speed: positive("fab_sla_retract_speed")?,
            output,
        };
        if container == Container::Archive {
            profile.within_archive_bounds()?;
        }
        Ok(profile)
    }

    /// The printer's archive takes a job within these bounds, wider than any of the maker's
    /// groups: the printer's own limits aren't published. A value outside is named by its key.
    fn within_archive_bounds(&self) -> Result<(), String> {
        let lifts = [self.bottom_lift, self.lift];
        let checks = [
            ("layer_height", (0.01..=0.15).contains(&self.layer_height)),
            ("fab_sla_bottom_exposure", (0.1..=300.0).contains(&self.bottom_exposure)),
            ("fab_sla_exposure", (0.1..=120.0).contains(&self.exposure)),
            ("fab_sla_light_off_delay", self.light_off_delay <= 60.0),
            ("fab_sla_bottom_lift_height", (0.0..=20.0).contains(&lifts[0].height)),
            ("fab_sla_lift_height", (0.0..=20.0).contains(&lifts[1].height)),
            ("fab_sla_bottom_lift_speed", (0.1..=20.0).contains(&lifts[0].speed)),
            ("fab_sla_lift_speed", (0.1..=20.0).contains(&lifts[1].speed)),
            ("fab_sla_retract_speed", (0.1..=20.0).contains(&self.retract_speed)),
        ];
        match checks.iter().find(|(_, ok)| !ok) {
            Some((key, _)) => Err(format!("the resin printer {}: {key} is outside what its archive takes", self.name)),
            None => Ok(()),
        }
    }

    /// The printer as the plate sees it: the screen's area for a bed, its height, no nozzles.
    pub fn machine(&self) -> Machine {
        Machine::resin(&self.name, self.screen, self.max_height)
    }

    /// Seconds of light for layer `k` (0 is on the plate).
    pub fn exposure_of(&self, k: u32) -> f64 {
        if k < self.bottom_layers { self.bottom_exposure } else { self.exposure }
    }

    /// The peel after layer `k`.
    pub fn lift_of(&self, k: u32) -> Lift {
        if k < self.bottom_layers { self.bottom_lift } else { self.lift }
    }
}

/// The simulated resin printer the tests print on: made-up, generic numbers, no real machine's
/// profile. Its printer preset's values (name, key, value).
pub const SIMULATED_PRINTER: (&str, &[(&str, &str)]) = (
    "Simulated resin 0.05",
    &[
        ("printer_technology", "SLA"),
        ("printer_model", "Simulated resin"),
        ("fab_sla_resolution", "2400x1600"),
        ("fab_sla_pixel_pitch", "0.05x0.05"),
        ("printable_area", "0x0,120x0,120x80,0x80"),
        ("printable_height", "150"),
        ("fab_sla_mirror", "y"),
        ("fab_sla_rotation", "0"),
        ("fab_sla_grey_levels", "16"),
    ],
);

/// The simulated resin printer's process preset (name, key, value).
pub const SIMULATED_PROCESS: (&str, &[(&str, &str)]) = (
    "Simulated resin 0.05 mm",
    &[
        ("layer_height", "0.05"),
        ("fab_sla_bottom_layers", "4"),
        ("fab_sla_bottom_exposure", "25"),
        ("fab_sla_exposure", "2.5"),
        ("fab_sla_light_off_delay", "0.5"),
        ("fab_sla_bottom_lift_height", "6"),
        ("fab_sla_bottom_lift_speed", "1"),
        ("fab_sla_lift_height", "5"),
        ("fab_sla_lift_speed", "2"),
        ("fab_sla_retract_speed", "3"),
    ],
);

/// The simulated printer's configuration: its printer preset with its process over it.
pub fn simulated_config() -> Config {
    config_of(&[SIMULATED_PRINTER.1, SIMULATED_PROCESS.1])
}

/// A configuration of presets' values (key, value), each over the ones before.
pub fn config_of(presets: &[&[(&str, &str)]]) -> Config {
    let mut cfg = Config::new();
    for (k, v) in presets.iter().flat_map(|p| p.iter()) {
        cfg.map.insert(k.to_string(), parse_text(k, v));
    }
    cfg
}

/// The resin printer Fab carries a preset for itself, from the numbers in its manual (no file of
/// its maker's is shipped): a 14K screen of 13312 × 5120 pixels, 16.8 µm along X and 24.8 µm
/// along Y, 230 mm tall, read from a USB stick. Its printer preset (name, key, value).
pub const BUILTIN_PRINTER: (&str, &[(&str, &str)]) = (
    "Anycubic Photon Mono M7 Pro",
    &[
        ("printer_technology", "SLA"),
        ("printer_model", "Anycubic Photon Mono M7 Pro"),
        ("fab_sla_resolution", "13312x5120"),
        ("fab_sla_pixel_pitch", "0.0168x0.0248"),
        ("printable_area", "0x0,223.6416x0,223.6416x126.976,0x126.976"),
        ("printable_height", "230"),
        // the plate seen from above, mirrored once along X (to be confirmed by the first print)
        ("fab_sla_mirror", "x"),
        ("fab_sla_rotation", "0"),
        ("fab_sla_grey_levels", "16"),
        ("fab_sla_container", "archive"),
        ("fab_sla_layer_encoding", "raster"),
        ("fab_sla_route", "drive"),
    ],
);

/// The built-in printer's process presets, one per settings group of its manual: Normal, Fast
/// (for its fast-release film and hollowed parts) and High-speed (for its maker's high-speed
/// resin). Every layer lifts alike, the bottom ones too, and the plate comes back down at the
/// lift's speed.
pub const BUILTIN_PROCESSES: [(&str, &[(&str, &str)]); 3] = [
    (
        "Resin 0.05 mm normal",
        &[
            ("layer_height", "0.05"),
            ("fab_sla_bottom_layers", "5"),
            ("fab_sla_bottom_exposure", "25"),
            ("fab_sla_exposure", "1.8"),
            ("fab_sla_light_off_delay", "0.5"),
            ("fab_sla_bottom_lift_height", "8"),
            ("fab_sla_bottom_lift_speed", "6"),
            ("fab_sla_lift_height", "8"),
            ("fab_sla_lift_speed", "6"),
            ("fab_sla_retract_speed", "6"),
            ("fab_sla_anti_alias", "16"),
            ("fab_sla_group", "normal"),
        ],
    ),
    (
        "Resin 0.10 mm fast",
        &[
            ("layer_height", "0.1"),
            ("fab_sla_bottom_layers", "3"),
            ("fab_sla_bottom_exposure", "25"),
            ("fab_sla_exposure", "2.2"),
            ("fab_sla_light_off_delay", "0.5"),
            ("fab_sla_bottom_lift_height", "4"),
            ("fab_sla_bottom_lift_speed", "15"),
            ("fab_sla_lift_height", "4"),
            ("fab_sla_lift_speed", "15"),
            ("fab_sla_retract_speed", "15"),
            ("fab_sla_anti_alias", "16"),
            ("fab_sla_group", "fast"),
        ],
    ),
    (
        "Resin 0.10 mm high-speed",
        &[
            ("layer_height", "0.1"),
            ("fab_sla_bottom_layers", "3"),
            ("fab_sla_bottom_exposure", "15"),
            ("fab_sla_exposure", "1.4"),
            ("fab_sla_light_off_delay", "0.5"),
            ("fab_sla_bottom_lift_height", "4"),
            ("fab_sla_bottom_lift_speed", "15"),
            ("fab_sla_lift_height", "4"),
            ("fab_sla_lift_speed", "15"),
            ("fab_sla_retract_speed", "15"),
            ("fab_sla_anti_alias", "16"),
            ("fab_sla_group", "high_speed"),
        ],
    ),
];

/// The built-in printer's preset and its processes as presets a profile store offers
/// ([`makepad_fab_settings::profiles::ProfileStore::with_builtin`]), each process made for it.
pub fn builtin_presets() -> Vec<RawPreset> {
    let preset = |kind: &str, (name, values): (&str, &[(&str, &str)])| {
        let mut values = config_of(&[values]);
        if kind == "process" {
            values.map.insert("compatible_printers".into(), makepad_fab_settings::Value::Vector(vec![BUILTIN_PRINTER.0.into()]));
        }
        RawPreset { kind: kind.into(), name: name.into(), from: "system".into(), instantiation: true, values, ..RawPreset::default() }
    };
    std::iter::once(preset("machine", BUILTIN_PRINTER)).chain(BUILTIN_PROCESSES.iter().map(|p| preset("process", *p))).collect()
}

/// The simulated resin printer's profile.
pub fn simulated() -> ResinProfile {
    ResinProfile::from_config(SIMULATED_PRINTER.0, &simulated_config()).expect("the simulated resin printer's profile reads")
}

#[cfg(test)]
mod tests {
    use super::*;
    use fab_plate::machine::{Rect, Technology};

    /// The simulated printer reads whole, its machine is a resin printer whose bed is the
    /// screen, and each key that is wrong or missing is named.
    #[test]
    fn the_simulated_resin_printer_reads_and_bad_keys_are_named() {
        let p = simulated();
        assert_eq!(p.screen.pixels, [2400, 1600]);
        assert_eq!(p.screen.pitch, [0.05, 0.05]);
        assert_eq!(p.screen.mirror, [false, true]);
        assert_eq!(p.screen.grey_levels, 16);
        assert_eq!((p.layer_height, p.bottom_layers, p.exposure_of(3), p.exposure_of(4)), (0.05, 4, 25.0, 2.5));
        assert_eq!(p.lift_of(0), Lift { height: 6.0, speed: 1.0 });
        let m = p.machine();
        assert!(matches!(m.technology, Technology::Resin(_)));
        assert_eq!(m.bed, Rect::new(0.0, 0.0, 120.0, 80.0));
        assert!(m.nozzles.is_empty());
        for (key, bad) in [("fab_sla_resolution", "2400"), ("fab_sla_pixel_pitch", "0x0.05"), ("fab_sla_mirror", "z"), ("fab_sla_rotation", "45"), ("fab_sla_grey_levels", "1"), ("printable_area", "0x0,100x0,100x80,0x80"), ("layer_height", "0"), ("fab_sla_exposure", "")] {
            let mut cfg = simulated_config();
            cfg.set_str(key, bad);
            let e = ResinProfile::from_config("p", &cfg).expect_err(key);
            assert!(e.contains(key) || key == "printable_area" && e.contains("printable_area"), "{key}: {e}");
        }
        let mut cfg = simulated_config();
        cfg.set_str("printer_technology", "FFF");
        assert!(ResinProfile::from_config("p", &cfg).is_err());
        assert_eq!(p.output, Output { container: Container::Folder, encoding: Encoding::Raster, route: Route::Link, anti_alias: 16, group: Group::Normal, bottle: Bottle { price: 220.0, ml: 1000.0, density: 1.2 } }, "left out: a folder over the link");
    }

    /// The built-in printer reads with each of its processes: its screen of non-square pixels
    /// is its bed, pixel for pixel, its job writes the printer's archive to a drive, and each
    /// process is its manual's group. A key it doesn't take, or a value outside what its archive
    /// takes, is named.
    #[test]
    fn the_builtin_resin_printer_reads_with_its_groups_and_bad_keys_are_named() {
        use crate::grid::PixelGrid;
        let groups = [(Group::Normal, 0.05, 5, 25.0, 1.8, 8.0, 6.0), (Group::Fast, 0.1, 3, 25.0, 2.2, 4.0, 15.0), (Group::HighSpeed, 0.1, 3, 15.0, 1.4, 4.0, 15.0)];
        for ((name, process), (group, h, bottom, bottom_s, s, lift, speed)) in BUILTIN_PROCESSES.iter().zip(groups) {
            let cfg = config_of(&[BUILTIN_PRINTER.1, *process]);
            let p = ResinProfile::from_config(BUILTIN_PRINTER.0, &cfg).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert_eq!((p.screen.pixels, p.screen.pitch, p.screen.mirror, p.screen.quarter_turns, p.screen.grey_levels), ([13312, 5120], [0.0168, 0.0248], [true, false], 0, 16), "{name}");
            assert_eq!((p.layer_height, p.bottom_layers, p.exposure_of(bottom - 1), p.exposure_of(bottom), p.light_off_delay), (h, bottom, bottom_s, s, 0.5), "{name}");
            assert_eq!((p.lift_of(0), p.lift_of(bottom), p.retract_speed), (Lift { height: lift, speed }, Lift { height: lift, speed }, speed), "{name}: the bottom layers lift like the others");
            assert_eq!((p.output.container, p.output.encoding, p.output.route, p.output.anti_alias, p.output.group), (Container::Archive, Encoding::Raster, Route::Drive, 16, group), "{name}");
            let m = p.machine();
            assert!((m.bed.width() - 223.6416).abs() < 1e-9 && (m.bed.depth() - 126.976).abs() < 1e-9 && m.max_height == 230.0, "{name}: {:?}", m.bed);
            // a pixel is 16.8 µm wide and 24.8 µm deep, all the way across
            let grid = PixelGrid::of(&p.screen);
            assert_eq!(grid.rect_of([0.0, 0.0], [0.0168, 0.0248]), crate::grid::PixelRect { x0: 0, y0: 0, x1: 1, y1: 1 }, "{name}: one pixel");
            let far = grid.center(13311, 5119);
            assert!((far[0] - 223.6332).abs() < 1e-9 && (far[1] - 126.9636).abs() < 1e-9, "{name}: the last pixel's centre {far:?}");
            assert!((grid.pixel_area() - 0.0168 * 0.0248).abs() < 1e-15);
        }
        let presets = builtin_presets();
        assert_eq!(presets.iter().map(|p| (p.kind.as_str(), p.name.as_str())).collect::<Vec<_>>(), [("machine", BUILTIN_PRINTER.0), ("process", BUILTIN_PROCESSES[0].0), ("process", BUILTIN_PROCESSES[1].0), ("process", BUILTIN_PROCESSES[2].0)]);
        assert_eq!(presets[1].values.get("compatible_printers").map(|v| v.as_vec()), Some(vec![BUILTIN_PRINTER.0.to_string()]));
        for (key, bad) in [
            ("fab_sla_container", "zip"),
            ("fab_sla_layer_encoding", "bitmap"),
            ("fab_sla_route", "wifi"),
            ("fab_sla_group", "turbo"),
            ("fab_sla_anti_alias", "17"),
            ("fab_sla_anti_alias", "2.5"),
            ("fab_sla_resin_price", "-1"),
            ("layer_height", "0.2"),
            ("fab_sla_exposure", "130"),
            ("fab_sla_bottom_exposure", "301"),
            ("fab_sla_lift_height", "21"),
            ("fab_sla_lift_speed", "25"),
            ("fab_sla_light_off_delay", "61"),
        ] {
            let mut cfg = config_of(&[BUILTIN_PRINTER.1, BUILTIN_PROCESSES[0].1]);
            cfg.set_str(key, bad);
            let e = ResinProfile::from_config(BUILTIN_PRINTER.0, &cfg).expect_err(key);
            assert!(e.contains(key), "{key} {bad}: {e}");
        }
        // a drive carries the printer's archive, not Fab's own folder
        let mut cfg = config_of(&[BUILTIN_PRINTER.1, BUILTIN_PROCESSES[0].1]);
        cfg.set_str("fab_sla_container", "folder");
        assert!(ResinProfile::from_config(BUILTIN_PRINTER.0, &cfg).expect_err("a folder on a drive").contains("fab_sla_route"));
    }
}
