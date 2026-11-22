//! What kind of printer a profile is for, beyond its numbers: the G-code its firmware reads and
//! the file a job goes to it in.
//!
//! A printer is a profile (its bed, nozzles, limits and templates) plus the family it belongs
//! to. The families are data ([`FAMILIES`]): a printer of a known family needs no code, only its
//! profile, and at most a row of its family's model ids.

use crate::config::Config;

/// The G-code dialect of a printer's firmware: the commands it takes for what standard G-code
/// leaves out (object labels, progress, the idle nozzle's temperature, pressure advance, the
/// file's header).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Flavor {
    /// Objects labelled by bit mask, a change counter after each tool select, timed pre-heat
    /// brackets for the idle nozzle, and a header and settings block ahead of the moves.
    Mask,
    /// Named objects, logical filament tools, and settings after the moves.
    Named,
}

/// The file a job goes to the printer in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Package {
    /// The G-code in a 3MF package with the plate's renders and settings. `version`: the package
    /// format the printer reads; `models`: the id the package names a printer model by, keyed by
    /// a part of its profile's `printer_model`.
    GcodeThreeMf { version: &'static str, models: &'static [(&'static str, &'static str)] },
    /// Plain G-code with PNG thumbnails at the sizes in the machine's `thumbnails` key.
    PlainGcode,
}

impl Package {
    /// The id the package names `printer_model` by: the first listed model whose key it contains,
    /// "" when it contains none.
    pub fn model_id(&self, printer_model: &str) -> &'static str {
        match self {
            Package::PlainGcode => "",
            Package::GcodeThreeMf { models, .. } => models.iter().find(|(m, _)| printer_model.contains(m)).map_or("", |(_, id)| id),
        }
    }
}

/// How a family's prime rows after a tool change speed up, as its own slicer lays them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PrimeRows {
    /// Four rows at set speeds, then every later row at the fifth row's.
    HoldFifth,
    /// Four rows at set shares of the layer's target speed, then 50 mm/min faster each row up to
    /// the target.
    Climb,
}

/// The protocol Fab talks to the printer over.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Link { Mqtt, Moonraker }

/// How a family's spools feed its heads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Spools { Units, OnePerHead }

/// One family of printers.
pub struct Family {
    /// The maker's name the printer's own report of its model leaves out.
    pub maker: &'static str,
    /// The `printer_model` prefixes that identify this family.
    pub models: &'static [&'static str],
    pub flavor: Flavor,
    pub link: Link,
    pub spools: Spools,
    /// The dialect its profiles' templates are written in.
    pub templates: crate::template::Dialect,
    pub package: Package,
    /// The `filament_vendor` of the maker's own filaments.
    pub own_filament_vendor: &'static str,
    /// How its prime rows after a change speed up.
    pub prime_rows: PrimeRows,
}

/// Fab's own default printer's family, independent of the registry order.
pub const DEFAULT: &Family = &DEFAULT_FAMILY;

static DEFAULT_FAMILY: Family = Family {
    maker: "Bambu Lab ",
    models: &["Bambu Lab "],
    flavor: Flavor::Mask,
    link: Link::Mqtt,
    spools: Spools::Units,
    templates: crate::template::Dialect::Compact,
    package: Package::GcodeThreeMf { version: "02.08.02.61", models: &[("H2D", "O1D")] },
    own_filament_vendor: "Bambu Lab",
    prime_rows: PrimeRows::HoldFifth,
};

/// Every family Fab prints for.
pub const FAMILIES: &[&Family] = &[DEFAULT, &Family {
    models: &["Snapmaker U1"],
    maker: "Snapmaker ",
    flavor: Flavor::Named,
    link: Link::Moonraker,
    spools: Spools::OnePerHead,
    package: Package::PlainGcode,
    own_filament_vendor: "Snapmaker",
    prime_rows: PrimeRows::Climb,
    templates: crate::template::Dialect::Merged,
}];

/// The family of a job's, or a printer preset's, printer.
pub fn of(cfg: &Config) -> Option<&'static Family> {
    of_model(cfg.str("printer_model"))
}

/// The family of the printer model a profile names (`printer_model`).
/// An empty model means only Fab's own defaults, never an installed or imported profile.
pub fn of_model(printer_model: &str) -> Option<&'static Family> {
    if printer_model.is_empty() { return Some(DEFAULT); }
    FAMILIES.iter().copied().find(|f| f.models.iter().any(|prefix| printer_model.starts_with(prefix)))
}

/// The family of a printer named as it reports its model, or as a printer configuration names
/// it (`model`), by [`Family::same_model`]'s rule: the full name, or the name after the family's
/// maker, either case. A name that starts with a maker is a full name; a shorter one goes to the
/// family whose models name the most of it after the maker.
pub fn of_reported(model: &str) -> Option<&'static Family> {
    let model = model.trim();
    if model.is_empty() { return None; }
    let starts = |name: &str, prefix: &str| name.get(..prefix.len()).is_some_and(|p| p.eq_ignore_ascii_case(prefix));
    let full = FAMILIES.iter().any(|f| starts(model, f.maker));
    let mut best: Option<(usize, &'static Family)> = None;
    for &f in FAMILIES {
        let name = if full { model.to_string() } else { format!("{}{model}", f.maker) };
        let named = f.models.iter().filter(|p| starts(&name, p)).map(|p| p.len().saturating_sub(f.maker.len())).max();
        if let Some(named) = named.filter(|n| best.is_none_or(|(b, _)| *n > b)) {
            best = Some((named, f));
        }
    }
    best.map(|(_, f)| f)
}

pub const NO_MODEL: &str = "This printer profile names no printer model: Fab cannot tell which G-code it reads.";
pub const UNSUPPORTED: &str = "Fab reads this printer's profile, but does not write G-code for it yet.";
/// A printer configuration's `model` that no family claims ([`of_reported`]).
pub const UNKNOWN_PRINTER: &str = "printer configuration: model must name a printer Fab prints for, e.g. Bambu Lab H2D";

/// Resolve an installed or imported printer preset, which must name its model.
pub fn of_profile(cfg: &Config) -> Result<&'static Family, &'static str> {
    if cfg.str("printer_model").is_empty() { return Err(NO_MODEL); }
    of(cfg).ok_or(UNSUPPORTED)
}

impl Family {
    /// Whether the model a printer reports (`reported`) is the profile's `printer_model`: the
    /// same name, or the name after the family's maker.
    pub fn same_model(&self, reported: &str, printer_model: &str) -> bool {
        let (reported, printer_model) = (reported.trim(), printer_model.trim());
        let maker = self.maker;
        !reported.is_empty()
            && (reported.eq_ignore_ascii_case(printer_model)
                || (printer_model.get(..maker.len()).is_some_and(|prefix| prefix.eq_ignore_ascii_case(maker)) && reported.eq_ignore_ascii_case(&printer_model[maker.len()..])))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_prefix_selects_the_family() {
        let mut cfg = Config::new();
        cfg.set_str("printer_model", &format!("{} (0.4 nozzle)", FAMILIES[1].models[0]));
        let second = of(&cfg).expect("claimed model");
        assert_eq!(second.flavor, Flavor::Named);
        assert_eq!(second.templates, crate::template::Dialect::Merged);
        assert_eq!(second.prime_rows, PrimeRows::Climb);
        assert_eq!(second.spools, Spools::OnePerHead);
        assert_eq!(Package::PlainGcode.model_id(cfg.str("printer_model")), "");
        cfg.set_str("printer_model", "Bambu Lab H2D");
        let first = of(&cfg).expect("claimed model");
        assert_eq!(first.flavor, Flavor::Mask);
        assert_eq!(first.prime_rows, PrimeRows::HoldFifth);
        assert_eq!(first.spools, Spools::Units);
        assert!(std::ptr::eq(DEFAULT, first));
        assert!(std::ptr::eq(of_model("").expect("own defaults"), first));
        let reversed: Vec<_> = FAMILIES.iter().rev().copied().collect();
        let claimed = reversed.into_iter().find(|f| f.models.iter().any(|p| cfg.str("printer_model").starts_with(p))).expect("claimed in either order");
        assert!(std::ptr::eq(DEFAULT, claimed));
        assert!(of_model("unclaimed").is_none());
    }

    #[test]
    fn a_printer_is_named_by_its_full_model_or_the_name_after_its_maker() {
        let second = FAMILIES[1];
        let short = &second.models[0][second.maker.len()..];
        for name in [second.models[0].to_string(), second.models[0].to_uppercase(), short.to_lowercase(), format!(" {short} ")] {
            assert!(of_reported(&name).is_some_and(|f| std::ptr::eq(f, second)), "{name}");
        }
        for name in [format!("{}Fixture", DEFAULT.maker), format!("{}fixture", DEFAULT.maker.to_lowercase()), "Fixture".into()] {
            assert!(of_reported(&name).is_some_and(|f| std::ptr::eq(f, DEFAULT)), "{name}");
            assert!(DEFAULT.same_model(&name, &format!("{}Fixture", DEFAULT.maker)), "{name}");
        }
        // Another model of a maker's is no family's.
        assert!(of_reported(&format!("{}Fixture", second.maker)).is_none());
        assert!(of_reported(" ").is_none());
    }

    #[test]
    fn installed_profiles_must_name_a_claimed_model() {
        let mut cfg = Config::new();
        assert!(matches!(of_profile(&cfg), Err(NO_MODEL)));
        cfg.set_str("printer_model", "Unclaimed");
        assert!(matches!(of_profile(&cfg), Err(UNSUPPORTED)));
    }
}
