//! What the calibration found, per printer and filament.
//!
//! One text file per pair, `~/Documents/Fab/calibration/<printer>/<filament>.conf`:
//! `name = value` lines (mm, degrees, ratios), each step's date as `date.<step> = YYYY-MM-DD`,
//! `#` comments. A value is there only once its step was done; Fab's defaults hold until then.
//! `FAB_CALIBRATION_DIR` moves the folder (test runs).

use crate::coupon::Coupon;
use crate::Step;
use std::collections::{BTreeMap, BTreeSet};
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

/// The file that supplied a measured value or a completion date.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ValueSource { Owner, Preset }

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EffectiveValue {
    pub value: f64,
    pub source: ValueSource,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EffectiveDate {
    pub date: String,
    pub source: ValueSource,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct EffectiveResults {
    pub owner: Option<String>,
    pub preset: String,
    pub filament: String,
    pub values: BTreeMap<String, EffectiveValue>,
    pub dates: BTreeMap<String, EffectiveDate>,
    pub clearable_steps: BTreeSet<String>,
}

impl EffectiveResults {
    pub fn get(&self, name: &str) -> Option<f64> { self.value(name).map(|v| v.value) }
    pub fn value(&self, name: &str) -> Option<EffectiveValue> { self.values.get(name).copied() }
    pub fn source(&self, name: &str) -> Option<ValueSource> { self.value(name).map(|v| v.source) }
    pub fn done(&self, step: Step) -> Option<&EffectiveDate> { self.dates.get(step.key()) }
    pub fn can_clear(&self, step: Step) -> bool { self.clearable_steps.contains(step.key()) }
    pub fn step_from_preset(&self, step: Step) -> bool {
        self.owner.is_some() && step_names(step).iter().any(|name| self.source(name) == Some(ValueSource::Preset))
    }
}

/// The measurements recorded by each calibration step.
pub fn step_names(step: Step) -> &'static [&'static str] {
    match step {
        Step::Clearance => &[names::C_XY],
        Step::ZGap => &[names::C_Z_LAYERS],
        Step::SlipFit => &[names::SLIP_FIT, names::PRESS_FIT],
        Step::Snap => &[names::SNAP_MAX, names::SNAP_HOLD],
        Step::Friction => &[names::MU],
        Step::Flow => &[names::FLOW_RATIO],
        Step::Temperature => &[names::NOZZLE_TEMP],
        Step::PressureAdvance => &[names::PRESSURE_ADVANCE],
        Step::HeadEnvelope => &[],
    }
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

/// Lowercase literals and fixed-width byte escapes stay distinct on case-insensitive volumes.
fn owner_slug(s: &str) -> String {
    use std::fmt::Write;
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' {
            out.push(b as char);
        } else {
            let _ = write!(out, "~{b:02x}");
        }
    }
    out
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
        Self::load_at(&Self::path_in(dir, printer, filament), printer, filament)
    }

    pub fn owner_path_in(dir: &Path, owner_id: &str, filament: &str) -> PathBuf {
        dir.join(format!("@{}", owner_slug(owner_id))).join(format!("{}.conf", slug(filament)))
    }

    pub fn load_owner_in(dir: &Path, owner_id: &str, filament: &str) -> Self {
        Self::load_at(&Self::owner_path_in(dir, owner_id, filament), owner_id, filament)
    }

    pub fn load_owner(owner_id: &str, filament: &str) -> Self {
        Self::load_owner_in(&default_dir(), owner_id, filament)
    }

    fn load_at(path: &Path, printer: &str, filament: &str) -> Self {
        if let Ok(text) = std::fs::read_to_string(path) {
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

    pub fn load_effective_in(dir: &Path, owner_id: Option<&str>, preset: &str, filament: &str) -> EffectiveResults {
        use ValueSource::{Owner, Preset};
        let base = Self::load_in(dir, preset, filament);
        let owner = owner_id.map(|id| Self::load_owner_in(dir, id, filament));
        let active = owner.as_ref().unwrap_or(&base);
        let mut effective = EffectiveResults {
            owner: owner_id.map(str::to_owned), preset: preset.to_string(), filament: filament.to_string(),
            ..Default::default()
        };
        for step in Step::ALL {
            let has_data = active.done(step).is_some() || step_names(step).iter().any(|n| active.values.contains_key(*n));
            let (date, source) = if has_data {
                effective.clearable_steps.insert(step.key().to_string());
                (active.done(step), if owner.is_some() { Owner } else { Preset })
            } else {
                (base.done(step), Preset)
            };
            if let Some(date) = date {
                effective.dates.insert(step.key().to_string(), EffectiveDate { date: date.to_string(), source });
            }
        }
        for (file, source) in std::iter::once((base, Preset)).chain(owner.map(|r| (r, Owner))) {
            effective.values.extend(file.values.into_iter().map(|(k, value)| (k, EffectiveValue { value, source })));
        }
        effective
    }

    pub fn load_effective(owner_id: Option<&str>, preset: &str, filament: &str) -> EffectiveResults {
        Self::load_effective_in(&default_dir(), owner_id, preset, filament)
    }

    pub fn clear_step(&mut self, step: Step) -> bool {
        let mut changed = self.dates.remove(step.key()).is_some();
        for name in step_names(step) {
            changed |= self.values.remove(*name).is_some();
        }
        changed
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
        self.save_at(Self::path_in(dir, &self.printer, &self.filament))
    }

    pub fn save_owner_in(&self, dir: &Path) -> Result<PathBuf, String> {
        if self.printer.is_empty() {
            return Err("missing calibration owner".into());
        }
        self.save_at(Self::owner_path_in(dir, &self.printer, &self.filament))
    }

    pub fn save_owner(&self) -> Result<PathBuf, String> {
        self.save_owner_in(&default_dir())
    }

    fn save_at(&self, path: PathBuf) -> Result<PathBuf, String> {
        match std::fs::read_to_string(&path) {
            Ok(text) => {
                let stored = Self::parse(&text).map_err(|e| format!("refusing to overwrite calibration results: {e}"))?;
                if stored.printer != self.printer || stored.filament != self.filament {
                    return Err("calibration file belongs to a different printer or filament; refusing to overwrite it".into());
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.to_string()),
        }
        let parent = path.parent().ok_or("no folder")?;
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        let tmp = path.with_extension("conf.new");
        std::fs::write(&tmp, self.to_text()).map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, &path).map_err(|e| e.to_string())?;
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

    fn in_store(name: &str, test: impl FnOnce(&Path)) {
        let time = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let dir = std::env::temp_dir().join(format!("fab-calibration-{name}-{}-{time}", std::process::id()));
        test(&dir);
        std::fs::remove_dir_all(dir).unwrap();
    }

    fn seed_pair(dir: &Path) {
        let mut preset = Results::new("Sim 0.4", "PLA");
        for (step, key, value) in [(Step::Flow, names::FLOW_RATIO, 0.96), (Step::PressureAdvance, names::PRESSURE_ADVANCE, 0.030), (Step::Clearance, names::C_XY, 0.30)] {
            preset.values.insert(key.into(), value);
            preset.dates.insert(step.key().into(), "2026-11-25".into());
        }
        preset.save_in(dir).unwrap();
        let mut owner = Results::new("printer:P", "PLA");
        owner.values.insert(names::FLOW_RATIO.into(), 0.91);
        owner.dates.insert(Step::Flow.key().into(), "2026-11-27".into());
        owner.save_owner_in(dir).unwrap();
    }

    #[test]
    fn owner_path_never_collides_with_preset_or_case_variants() {
        in_store("paths", |dir| {
            let mut seen = BTreeSet::new();
            for id in ["printer:Workshop", "printer:workshop", "printer:A B", "printer:A-B", "printer:A/B", "printer:A~2fB"] {
                let r = Results::new(id, "PLA");
                let path = r.save_owner_in(dir).unwrap();
                let folder = path.parent().unwrap().file_name().unwrap().to_str().unwrap();
                assert!(folder.starts_with('@') && seen.insert(folder.to_ascii_lowercase()), "separate owner folder for {id}");
                assert_eq!(Results::load_owner_in(dir, id, "PLA"), r);
            }
            for name in ["@printer:Workshop", "printer:Workshop", "Workshop"] {
                let path = Results::path_in(dir, name, "PLA");
                assert!(!path.parent().unwrap().file_name().unwrap().to_str().unwrap().contains('@'));
            }
        });
    }

    #[test]
    fn effective_results_merges_per_measurement_and_tracks_source() {
        use ValueSource::{Owner, Preset};
        in_store("merge", |dir| {
            seed_pair(dir);
            let read = || Results::load_effective_in(dir, Some("printer:P"), "Sim 0.4", "PLA");
            let r = read();
            for (key, value, source) in [(names::FLOW_RATIO, 0.91, Owner), (names::PRESSURE_ADVANCE, 0.03, Preset), (names::C_XY, 0.30, Preset)] {
                assert_eq!(r.value(key), Some(EffectiveValue { value, source }), "{key}");
            }
            assert!(r.can_clear(Step::Flow) && !r.step_from_preset(Step::Flow));
            assert!(!r.can_clear(Step::PressureAdvance) && r.step_from_preset(Step::PressureAdvance));
            for (step, date, source) in [(Step::Flow, "2026-11-27", Owner), (Step::PressureAdvance, "2026-11-25", Preset)] {
                assert_eq!(r.done(step), Some(&EffectiveDate { date: date.into(), source }));
            }
            let mut owner = Results::load_owner_in(dir, "printer:P", "PLA");
            owner.dates.clear();
            owner.save_owner_in(dir).unwrap();
            assert_eq!(read().done(Step::Flow), None, "an undated owner measurement cannot inherit a preset date");
        });
    }

    #[test]
    fn clearing_owner_step_preserves_preset_fallback() {
        in_store("clear", |dir| {
            seed_pair(dir);
            let mut r = Results::load_owner_in(dir, "printer:P", "PLA");
            assert!(r.clear_step(Step::Flow));
            assert!(!r.clear_step(Step::Flow));
            r.save_owner_in(dir).unwrap();
            assert_eq!(Results::load_in(dir, "Sim 0.4", "PLA").get(names::FLOW_RATIO), Some(0.96));
            let r = Results::load_effective_in(dir, Some("printer:P"), "Sim 0.4", "PLA");
            assert_eq!(r.value(names::FLOW_RATIO), Some(EffectiveValue { value: 0.96, source: ValueSource::Preset }));
            assert!(!r.can_clear(Step::Flow));
        });
    }

    #[test]
    fn unowned_effective_reads_and_clears_preset_only() {
        in_store("unowned", |dir| {
            seed_pair(dir);
            let r = Results::load_effective_in(dir, None, "Sim 0.4", "PLA");
            assert_eq!(r.value(names::FLOW_RATIO), Some(EffectiveValue { value: 0.96, source: ValueSource::Preset }));
            assert!(r.can_clear(Step::Flow));
            let mut preset = Results::load_in(dir, "Sim 0.4", "PLA");
            assert!(preset.clear_step(Step::Flow));
            preset.save_in(dir).unwrap();
            assert_eq!(Results::load_owner_in(dir, "printer:P", "PLA").get(names::FLOW_RATIO), Some(0.91));
        });
    }

    #[test]
    fn owner_file_contains_no_secret_fields() {
        in_store("fields", |dir| {
            let mut r = Results::new("printer:Shop 1", "PLA");
            r.values.insert(names::FLOW_RATIO.into(), 0.94);
            let path = r.save_owner_in(dir).unwrap();
            assert!(path.ends_with("@printer~3a~53hop~201/PLA.conf"));
            let text = std::fs::read_to_string(path).unwrap();
            assert_eq!(text.lines().filter(|l| !l.starts_with('#')).collect::<Vec<_>>(),
                ["printer = printer:Shop 1", "filament = PLA", "flow_ratio = 0.94"]);
        });
    }

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
