//! What the calibration found, per printer and filament.
//!
//! One text file per pair, `~/Documents/Fab/calibration/<printer>/<filament>.conf`:
//! `name = value` lines (mm, degrees, ratios), each step's date as `date.<step> = YYYY-MM-DD`,
//! `#` comments. A value is there only once its step was done; Fab's defaults hold until then.
//! `FAB_CALIBRATION_DIR` moves the folder (test runs).

use crate::coupon::Coupon;
use crate::Step;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// What the user entered for a step, in the coupon's numbers.
#[derive(Clone, Debug, PartialEq)]
pub enum Answer {
    /// The variant number (the smallest spinner that turns, the best step, band or block ...).
    Number(usize),
    /// Two variant numbers: slip and press fit, or the largest snap that went in and the
    /// smallest that held.
    Two(usize, usize),
    /// An angle in degrees (friction: how far the arm stays off vertical).
    Angle(f64),
}

/// The stored values of one printer and filament.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Results {
    pub printer: String,
    pub filament: String,
    pub values: BTreeMap<String, f64>,
    /// Step key -> the date it was done.
    pub dates: BTreeMap<String, String>,
}

/// The names of the stored values.
pub mod names {
    /// In-plane clearance of print-in-place joints (mm).
    pub const C_XY: &str = "c_xy";
    /// Empty layers between print-in-place parts across layers.
    pub const C_Z_LAYERS: &str = "c_z_layers";
    /// Per-side gap for a free slip fit between parts printed apart (mm).
    pub const SLIP_FIT: &str = "slip_fit";
    /// Per-side gap a press fit goes in at (mm, may be 0).
    pub const PRESS_FIT: &str = "press_fit";
    /// The largest snap interference that went in without breaking (mm).
    pub const SNAP_MAX: &str = "snap_delta_max";
    /// The smallest snap interference that held (mm).
    pub const SNAP_HOLD: &str = "snap_delta_hold";
    /// Joint friction coefficient.
    pub const MU: &str = "mu";
    pub const FLOW_RATIO: &str = "flow_ratio";
    /// Nozzle temperature (°C).
    pub const NOZZLE_TEMP: &str = "nozzle_temp";
    pub const PRESSURE_ADVANCE: &str = "pressure_advance";
}

/// Where calibration files live.
pub fn default_dir() -> PathBuf {
    if let Some(d) = std::env::var_os("FAB_CALIBRATION_DIR").filter(|d| !d.is_empty()) {
        return PathBuf::from(d);
    }
    let home = makepad_fab_settings::home::user_home();
    home.join("Documents").join("Fab").join("calibration")
}

/// Preserve ordinary file names and escape other UTF-8 bytes, including a leading dot.
fn slug(s: &str) -> String {
    let mut out = String::new();
    for (i, b) in s.bytes().enumerate() {
        match b {
            b' ' => out.push('-'),
            b'-' | b'_' => out.push(b as char),
            b'.' if i != 0 => out.push('.'),
            b if b.is_ascii_alphanumeric() => out.push(b as char),
            b => out.push_str(&format!("~{b:02x}")),
        }
    }
    if out.is_empty() { "default".into() } else { out }
}

fn name_text(name: &str) -> String {
    name.replace('\\', "\\\\").replace('\n', "\\n").replace('\r', "\\r")
}

fn read_name(text: &str) -> Result<String, String> {
    let mut name = String::new();
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        name.push(if c == '\\' {
            match chars.next() {
                Some('\\') => '\\',
                Some('n') => '\n',
                Some('r') => '\r',
                _ => return Err("invalid escape in a calibration name".into()),
            }
        } else { c });
    }
    Ok(name)
}

impl Results {
    pub fn new(printer: &str, filament: &str) -> Self {
        Results { printer: printer.to_string(), filament: filament.to_string(), ..Default::default() }
    }

    pub fn path_in(dir: &Path, printer: &str, filament: &str) -> PathBuf {
        dir.join(slug(printer)).join(format!("{}.conf", slug(filament)))
    }

    /// The stored results of a pair (empty when none are stored or the file does not read).
    pub fn load_in(dir: &Path, printer: &str, filament: &str) -> Self {
        if let Ok(text) = std::fs::read_to_string(Self::path_in(dir, printer, filament)) {
            if let Ok(r) = Self::parse(&text) {
                if r.printer == printer && r.filament == filament {
                    return r;
                }
            }
        }
        Results::new(printer, filament)
    }

    pub fn load(printer: &str, filament: &str) -> Self {
        Self::load_in(&default_dir(), printer, filament)
    }

    fn parse(text: &str) -> Result<Self, String> {
        let mut r = Self::default();
        let (mut printer, mut filament) = (None, None);
        for line in text.lines() {
            if line.trim().is_empty() || line.trim_start().starts_with('#') {
                continue;
            }
            let Some((k, v)) = line.split_once('=') else { continue };
            let k = k.trim();
            if k == "printer" || k == "filament" {
                let name = read_name(v.strip_prefix(' ').unwrap_or(v))?;
                let field = if k == "printer" { &mut printer } else { &mut filament };
                if field.replace(name).is_some() {
                    return Err(format!("duplicate {k} in calibration results"));
                }
            } else if let Some(step) = k.strip_prefix("date.") {
                r.dates.insert(step.to_string(), v.trim().to_string());
            } else if let Ok(x) = v.trim().parse::<f64>() {
                if x.is_finite() {
                    r.values.insert(k.to_string(), x);
                }
            }
        }
        r.printer = printer.ok_or("calibration results have no printer name")?;
        r.filament = filament.ok_or("calibration results have no filament name")?;
        Ok(r)
    }

    pub fn to_text(&self) -> String {
        let mut s = String::from("# Fab calibration results (Calibrate panel). Values in mm, degrees and ratios.\n");
        s.push_str(&format!("printer = {}\nfilament = {}\n", name_text(&self.printer), name_text(&self.filament)));
        for (k, v) in &self.values {
            s.push_str(&format!("{k} = {}\n", trim_num(*v)));
        }
        for (k, d) in &self.dates {
            s.push_str(&format!("date.{k} = {d}\n"));
        }
        s
    }

    /// Write the file (through a temporary file, so a reader never sees half of it).
    pub fn save_in(&self, dir: &Path) -> Result<PathBuf, String> {
        let path = Self::path_in(dir, &self.printer, &self.filament);
        match std::fs::read_to_string(&path) {
            Ok(text) => {
                let stored = Self::parse(&text).map_err(|e| format!("refusing to overwrite {}: {e}", path.display()))?;
                if stored.printer != self.printer || stored.filament != self.filament {
                    return Err(format!("{} belongs to printer {:?}, filament {:?}; refusing to overwrite it with results for printer {:?}, filament {:?}", path.display(), stored.printer, stored.filament, self.printer, self.filament));
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(format!("{}: {e}", path.display())),
        }
        let parent = path.parent().ok_or("no folder")?;
        std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
        let tmp = path.with_extension("conf.new");
        std::fs::write(&tmp, self.to_text()).map_err(|e| format!("{}: {e}", tmp.display()))?;
        std::fs::rename(&tmp, &path).map_err(|e| format!("{}: {e}", path.display()))?;
        Ok(path)
    }

    pub fn save(&self) -> Result<PathBuf, String> {
        self.save_in(&default_dir())
    }

    pub fn get(&self, name: &str) -> Option<f64> {
        self.values.get(name).copied()
    }

    pub fn done(&self, step: Step) -> Option<&str> {
        self.dates.get(step.key()).map(|s| s.as_str())
    }

    /// Store what an answer about `coupon` means; returns the values it set.
    pub fn record(&mut self, coupon: &Coupon, answer: &Answer, today: &str) -> Result<Vec<(&'static str, f64)>, String> {
        use names::*;
        let var = |n: usize| coupon.variant(n).map(|v| v.value).ok_or_else(|| format!("{} has no number {n}: enter 1 to {}", coupon.name, coupon.variants.len()));
        let set: Vec<(&'static str, f64)> = match (coupon.step, answer) {
            (Step::Clearance, Answer::Number(n)) => vec![(C_XY, var(*n)?)],
            (Step::ZGap, Answer::Number(n)) => vec![(C_Z_LAYERS, var(*n)?)],
            (Step::SlipFit, Answer::Two(slip, press)) => {
                let (s, p) = (var(*slip)?, var(*press)?);
                if p > s {
                    return Err("the press fit cannot need a looser hole than the slip fit: check the numbers".into());
                }
                vec![(SLIP_FIT, s), (PRESS_FIT, p)]
            }
            (Step::Snap, Answer::Two(went_in, held)) => {
                let (m, h) = (var(*went_in)?, var(*held)?);
                if h > m {
                    return Err("no hook both went in and held: print the coupon in a tougher material or tell Fab to use pins instead of snaps".into());
                }
                vec![(SNAP_MAX, m), (SNAP_HOLD, h)]
            }
            (Step::Friction, Answer::Angle(deg)) => {
                if !(0.0..=60.0).contains(deg) {
                    return Err("enter the angle from straight down, 0 to 60 degrees".into());
                }
                let lever = coupon.fact("lever").ok_or("the coupon has no lever")?;
                let r = coupon.fact("bearing_radius").ok_or("the coupon has no bearing radius")?;
                // The arm rests where gravity's torque equals the friction torque. On the 45°
                // ridge a radial load presses the flanks with √2 times the load.
                let mu = (lever * deg.to_radians().sin() / (std::f64::consts::SQRT_2 * r)).clamp(0.0, 1.5);
                vec![(MU, mu)]
            }
            (Step::Flow, Answer::Number(n)) => vec![(FLOW_RATIO, coupon.fact("flow_ratio").unwrap_or(1.0) * var(*n)?)],
            (Step::Temperature, Answer::Number(n)) => vec![(NOZZLE_TEMP, var(*n)?)],
            (Step::PressureAdvance, Answer::Number(n)) => vec![(PRESSURE_ADVANCE, var(*n)?)],
            (step, a) => return Err(format!("{} takes a different answer than {a:?}", step.title())),
        };
        for (k, v) in &set {
            self.values.insert(k.to_string(), *v);
        }
        self.dates.insert(coupon.step.key().to_string(), today.to_string());
        Ok(set)
    }
}

/// A number without trailing zeros (0.35, 2, 0.0125).
fn trim_num(v: f64) -> String {
    let s = format!("{v:.5}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s.is_empty() || s == "-" { "0".into() } else { s.to_string() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coupon::{make, CouponSetup};

    #[test]
    fn answers_become_values() {
        let s = CouponSetup { flow_ratio: 0.98, ..Default::default() };
        let mut r = Results::new("H2D 0.4", "Generic PLA");
        r.record(&make(Step::Clearance, &s).unwrap(), &Answer::Number(5), "2026-10-29").unwrap();
        assert!((r.get(names::C_XY).unwrap() - 0.30).abs() < 1e-9);
        r.record(&make(Step::ZGap, &s).unwrap(), &Answer::Number(2), "2026-10-29").unwrap();
        assert_eq!(r.get(names::C_Z_LAYERS), Some(2.0));
        r.record(&make(Step::Flow, &s).unwrap(), &Answer::Number(3), "2026-10-29").unwrap();
        assert!((r.get(names::FLOW_RATIO).unwrap() - 0.98 * 0.98).abs() < 1e-9);
        assert!(r.record(&make(Step::SlipFit, &s).unwrap(), &Answer::Two(2, 4), "d").is_err(), "press looser than slip");
        assert!(r.record(&make(Step::Clearance, &s).unwrap(), &Answer::Number(12), "d").is_err(), "no such spinner");
        assert!(r.record(&make(Step::Clearance, &s).unwrap(), &Answer::Angle(3.0), "d").is_err(), "wrong kind of answer");
        let f = make(Step::Friction, &s).unwrap();
        r.record(&f, &Answer::Angle(5.0), "2026-10-29").unwrap();
        let mu = r.get(names::MU).unwrap();
        assert!(mu > 0.1 && mu < 0.6, "a plausible printed-joint friction from 5°: {mu}");
        assert_eq!(r.done(Step::Friction), Some("2026-10-29"));
    }

    #[test]
    fn file_round_trip() {
        let dir = std::env::temp_dir().join(format!("fabcal-test-{}", std::process::id()));
        let mut r = Results::new("Printer A / 0.4", "PLA Basic");
        r.values.insert(names::C_XY.into(), 0.3);
        r.values.insert(names::PRESSURE_ADVANCE.into(), 0.025);
        r.dates.insert("clearance".into(), "2026-10-29".into());
        let p = r.save_in(&dir).unwrap();
        assert!(p.ends_with("Printer-A-~2f-0.4/PLA-Basic.conf"), "{}", p.display());
        let back = Results::load_in(&dir, "Printer A / 0.4", "PLA Basic");
        assert_eq!(back, r);
        std::fs::remove_file(&p).ok();
        std::fs::remove_dir(p.parent().unwrap()).ok();
        std::fs::remove_dir(&dir).ok();
    }
}
