//! A parameter table describes a part's editable fields. The same table supplies inspector rows,
//! file text, parameter sweeps and catalog actions, with shared limits and material rules.

use fab_materials::finish::FINISHES;
use fab_materials::material::MATERIALS;
use fab_materials::swatch::{colour_by_name, hex_string, SWATCHES};
use fab_materials::{Colour, Finish, FinishId, FinishStandard, Material, MaterialId, Processes};
use std::path::{Component, Path, PathBuf};

/// Define labelled choices with clamped index conversion.
macro_rules! choices {
    ($(#[$doc:meta])* $name:ident { $($variant:ident => $label:literal),+ $(,)? }) => {
        $(#[$doc])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        #[repr(i32)]
        pub enum $name { $($variant),+ }

        impl $name {
            pub const NAMES: &'static [&'static str] = &[$($label),+];

            /// Clamp a stored index to the available choices.
            pub fn from_index(index: i32) -> Self {
                const VALUES: &[$name] = &[$($name::$variant),+];
                VALUES[index.clamp(0, VALUES.len() as i32 - 1) as usize]
            }

            pub const fn index(self) -> i32 { self as i32 }
        }
    };
}
pub(crate) use choices;

/// A field's accessors, limits and picker choices.
pub enum Kind<P> {
    Float {
        get: fn(&P) -> f64,
        set: fn(&mut P, f64),
        lo: f64,
        hi: f64,
        fmt: &'static str,
        hard_lo: f64,
        hard_hi: f64,
    },
    Int {
        get: fn(&P) -> i32,
        set: fn(&mut P, i32),
        lo: i32,
        hi: i32,
    },
    Bool {
        get: fn(&P) -> bool,
        set: fn(&mut P, bool),
    },
    Enum {
        get: fn(&P) -> i32,
        set: fn(&mut P, i32),
        names: &'static [&'static str],
        notes: Option<&'static [&'static str]>,
    },
    Colour {
        get: fn(&P) -> Colour,
        set: fn(&mut P, Colour),
    },
    Material {
        get: fn(&P) -> MaterialId,
        set: fn(&mut P, MaterialId),
        allowed: Option<fn(&Material) -> bool>,
        featured: Option<fn(&Material) -> bool>,
        aliases: &'static [(&'static str, &'static str)],
    },
    Finish {
        get: fn(&P) -> FinishId,
        set: fn(&mut P, FinishId),
        allowed: Option<fn(&Finish) -> bool>,
        aliases: &'static [(&'static str, &'static str)],
    },
    Path {
        get: fn(&P) -> String,
        set: fn(&mut P, String),
        ext: Option<&'static str>,
        dir: bool,
    },
}

/// A named field in a group of inspector rows and file entries.
pub struct ParamDesc<P> {
    pub key: &'static str,
    pub label: &'static str,
    pub group: &'static str,
    pub tip: Option<&'static str>,
    pub kind: Kind<P>,
}

impl<P> ParamDesc<P> {
    pub fn tip(mut self, tip: &'static str) -> Self {
        self.tip = Some(tip);
        self
    }

    /// Set a float's typed limits independently of its slider limits.
    pub fn hard(mut self, lo: f64, hi: f64) -> Self {
        if let Kind::Float { hard_lo, hard_hi, .. } = &mut self.kind {
            *hard_lo = lo;
            *hard_hi = hi;
        }
        self
    }

    pub fn notes(mut self, notes: &'static [&'static str]) -> Self {
        if let Kind::Enum { notes: target, .. } = &mut self.kind {
            *target = Some(notes);
        }
        self
    }

    pub fn sweepable(&self) -> bool {
        !matches!(self.kind, Kind::Bool { .. } | Kind::Path { .. })
    }

    pub fn list_type(&self) -> bool {
        matches!(self.kind, Kind::Material { .. } | Kind::Finish { .. })
    }

    /// Material or finish ids in picker order, with featured materials first. Without an `allowed`
    /// filter a part picks among the moulding materials and the process finishes (no board rows,
    /// mask grades or as-made surface).
    pub fn options(&self) -> Vec<&'static str> {
        let mut options = Vec::new();
        match &self.kind {
            Kind::Material { allowed, featured, .. } => {
                let allowed = |m: &Material| allowed.map_or(m.family.is_moulding(), |allowed| allowed(m));
                if let Some(featured) = featured {
                    for m in MATERIALS {
                        if allowed(m) && featured(m) {
                            options.push(m.id);
                        }
                    }
                }
                for m in MATERIALS {
                    if allowed(m) && featured.is_none_or(|featured| !featured(m)) {
                        options.push(m.id);
                    }
                }
            }
            Kind::Finish { allowed, .. } => {
                for f in FINISHES {
                    if allowed.map_or(f.processes != Processes::NONE, |allowed| allowed(f)) {
                        options.push(f.id);
                    }
                }
            }
            _ => {}
        }
        options
    }

    pub fn option_index(&self, id: &str) -> usize {
        self.options().iter().position(|option| *option == id).unwrap_or(0)
    }

    /// Resolve a catalog spelling, then an exact legacy alias, to its canonical id.
    pub fn resolve(&self, value: &str) -> Option<&'static str> {
        match &self.kind {
            Kind::Material { aliases, .. } => MaterialId::lookup(value)
                .or_else(|| {
                    aliases.iter().find(|(alias, _)| *alias == value)
                        .and_then(|(_, id)| MaterialId::lookup(id))
                })
                .map(MaterialId::name),
            Kind::Finish { aliases, .. } => FinishId::lookup(value)
                .or_else(|| {
                    aliases.iter().find(|(alias, _)| *alias == value)
                        .and_then(|(_, id)| FinishId::lookup(id))
                })
                .map(FinishId::name),
            _ => None,
        }
    }
}

/// Produce a `(get, set)` function-pointer pair for a field, including owned strings.
/// Destructure the pair before passing its accessors to a descriptor builder.
#[macro_export]
macro_rules! field {
    ($part:ty, $member:ident) => {{
        fn pair<P, V>(get: fn(&P) -> V, set: fn(&mut P, V)) -> (fn(&P) -> V, fn(&mut P, V)) {
            (get, set)
        }
        pair(|p: &$part| p.$member.clone(), |p: &mut $part, value| p.$member = value)
    }};
}

pub use crate::field;

fn head<P>(key: &'static str, label: &'static str, group: &'static str, kind: Kind<P>) -> ParamDesc<P> {
    ParamDesc { key, label, group, tip: None, kind }
}

pub fn float<P>(
    key: &'static str, label: &'static str, group: &'static str,
    get: fn(&P) -> f64, set: fn(&mut P, f64), lo: f64, hi: f64, fmt: &'static str,
) -> ParamDesc<P> {
    head(key, label, group, Kind::Float { get, set, lo, hi, fmt, hard_lo: lo, hard_hi: hi })
}

pub fn int<P>(
    key: &'static str, label: &'static str, group: &'static str,
    get: fn(&P) -> i32, set: fn(&mut P, i32), lo: i32, hi: i32,
) -> ParamDesc<P> {
    head(key, label, group, Kind::Int { get, set, lo, hi })
}

pub fn enumeration<P>(
    key: &'static str, label: &'static str, group: &'static str,
    get: fn(&P) -> i32, set: fn(&mut P, i32), names: &'static [&'static str],
) -> ParamDesc<P> {
    head(key, label, group, Kind::Enum { get, set, names, notes: None })
}

pub fn boolean<P>(
    key: &'static str, label: &'static str, group: &'static str,
    get: fn(&P) -> bool, set: fn(&mut P, bool),
) -> ParamDesc<P> {
    head(key, label, group, Kind::Bool { get, set })
}

pub fn colour<P>(
    key: &'static str, label: &'static str, group: &'static str,
    get: fn(&P) -> Colour, set: fn(&mut P, Colour),
) -> ParamDesc<P> {
    head(key, label, group, Kind::Colour { get, set })
}

pub fn material<P>(
    key: &'static str, label: &'static str, group: &'static str,
    get: fn(&P) -> MaterialId, set: fn(&mut P, MaterialId),
    allowed: Option<fn(&Material) -> bool>, featured: Option<fn(&Material) -> bool>,
    aliases: &'static [(&'static str, &'static str)],
) -> ParamDesc<P> {
    head(key, label, group, Kind::Material { get, set, allowed, featured, aliases })
}

pub fn finish<P>(
    key: &'static str, label: &'static str, group: &'static str,
    get: fn(&P) -> FinishId, set: fn(&mut P, FinishId),
    allowed: Option<fn(&Finish) -> bool>, aliases: &'static [(&'static str, &'static str)],
) -> ParamDesc<P> {
    head(key, label, group, Kind::Finish { get, set, allowed, aliases })
}

pub fn path<P>(
    key: &'static str, label: &'static str, group: &'static str,
    get: fn(&P) -> String, set: fn(&mut P, String), ext: Option<&'static str>, dir: bool,
) -> ParamDesc<P> {
    head(key, label, group, Kind::Path { get, set, ext, dir })
}

/// The suffix after a printf conversion, with leading spaces removed.
pub fn unit_of(fmt: &str) -> &str {
    let Some(start) = fmt.find('%') else { return "" };
    let Some(end) = fmt[start + 1..].find(['f', 'd', 'g', 'i', 'e']) else { return "" };
    fmt[start + end + 2..].trim_start_matches(' ')
}

/// Format a float with `%.Nf`, `%d` or `%g`, preserving any prefix and suffix.
/// `%g` writes the shortest decimal that round trips to the same float.
pub fn format_value(v: f64, fmt: &str) -> String {
    let Some(start) = fmt.find('%') else { return v.to_string() };
    let Some(offset) = fmt[start + 1..].find(['f', 'd', 'g']) else { return v.to_string() };
    let end = start + offset + 1;
    let number = match fmt.as_bytes()[end] {
        b'f' => {
            let spec = &fmt[start + 1..end];
            let precision = if spec.is_empty() { Some(6) } else {
                spec.strip_prefix('.').and_then(|digits| digits.parse::<usize>().ok())
            };
            match precision {
                Some(precision) => format!("{v:.precision$}"),
                None => v.to_string(),
            }
        }
        b'd' => (v as i32).to_string(),
        _ => v.to_string(),
    };
    format!("{}{number}{}", &fmt[..start], &fmt[end + 1..])
}

pub fn trimmed(value: &str) -> &str {
    value.trim_matches([' ', '\t', '\r', '\n'])
}

pub fn palette_index(name: &str) -> Option<usize> {
    SWATCHES.iter().position(|swatch| swatch.name.eq_ignore_ascii_case(name))
}

/// The part identity and its main colour; translucency belongs to the colour.
pub trait PartParams: Clone + Default {
    fn name(&self) -> &str;
    fn set_name(&mut self, name: String);
    fn colour(&self) -> Colour;
    fn set_colour(&mut self, c: Colour);
}

pub struct ParamTable<P: PartParams> {
    pub descs: Vec<ParamDesc<P>>,
    pub header: &'static str,
    pub after_set: Option<fn(&mut P, &ParamDesc<P>)>,
    pub legacy: Option<fn(&mut P, &str, &str) -> bool>,
}

impl<P: PartParams> ParamTable<P> {
    pub fn material_desc(&self) -> Option<&ParamDesc<P>> {
        self.descs.iter().find(|d| matches!(d.kind, Kind::Material { .. }))
    }

    pub fn finish_desc(&self, key: &str) -> Option<&ParamDesc<P>> {
        self.find(key).filter(|d| matches!(d.kind, Kind::Finish { .. }))
    }

    /// The selected material, or ABS when the descriptor or id is missing.
    pub fn material(&self, p: &P) -> MaterialId {
        if let Some(ParamDesc { kind: Kind::Material { get, .. }, .. }) = self.material_desc() {
            let id = get(p);
            if MATERIALS.get(id.0 as usize).is_some() {
                return id;
            }
        }
        MaterialId::lookup("abs").expect("ABS is in the material table")
    }

    pub fn finish(&self, p: &P, key: &str) -> FinishId {
        if let Some(ParamDesc { kind: Kind::Finish { get, .. }, .. }) = self.finish_desc(key) {
            let id = get(p);
            if FINISHES.get(id.0 as usize).is_some() {
                return id;
            }
        }
        self.material(p).get().default_finish()
    }

    /// Prefer the material's default, then an allowed finish sharing its process.
    pub fn finish_for(&self, m: &Material, fd: &ParamDesc<P>) -> FinishId {
        let default = m.default_finish();
        let Kind::Finish { allowed, .. } = &fd.kind else { return default };
        if allowed.is_none_or(|allowed| allowed(default.get())) {
            return default;
        }
        let options = fd.options();
        for id in &options {
            if let Some(id) = FinishId::lookup(id) {
                if id.get().processes.intersects(m.processes) {
                    return id;
                }
            }
        }
        options.first().and_then(|id| FinishId::lookup(id)).unwrap_or(default)
    }

    /// Repair the primary finish on material changes and enforce the material's colour rules.
    pub fn apply_rules(&self, p: &mut P, changed: Option<&ParamDesc<P>>) {
        let Some(md) = self.material_desc() else { return };
        let Kind::Material { get, .. } = &md.kind else { return };
        let Some(m) = MATERIALS.get(get(p).0 as usize) else { return };
        let mut c = p.colour();
        if changed.is_some_and(|d| std::ptr::eq(d, md)) {
            if let Some(fd) = self.finish_desc("finish") {
                if let Kind::Finish { get, set, allowed, .. } = &fd.kind {
                    let compatible = FINISHES.get(get(p).0 as usize).is_some_and(|f| {
                        f.processes.intersects(m.processes) && allowed.is_none_or(|allowed| allowed(f))
                    });
                    if !compatible {
                        set(p, self.finish_for(m, fd));
                    }
                }
            }
            if !m.translucency_allowed() {
                c.translucency = 0.0;
            }
        }
        if m.electrical.dark_only {
            let srgb = m.clamp_colour(c.srgb);
            if srgb != c.srgb {
                c.ral = None;
            }
            c.srgb = srgb;
            c.translucency = 0.0;
        }
        if c != p.colour() {
            p.set_colour(c);
        }
    }

    /// Translate an old roughness into a nearby VDI or SPI A-C finish.
    pub fn legacy_roughness(&self, p: &mut P, r: f64) {
        let Some(fd) = self.finish_desc("finish") else { return };
        let Kind::Finish { set, .. } = &fd.kind else { return };
        let m = self.material(p).get();
        if (f64::from(self.finish(p, "finish").get().render_roughness()) - r).abs() <= 0.08 {
            return;
        }
        let mut best = None;
        let mut distance = 1e9;
        for id in fd.options().iter().filter_map(|id| FinishId::lookup(id)) {
            let f = id.get();
            if f.processes.0 & m.processes.0 & Processes::INJECTION.0 == 0 {
                continue;
            }
            if f.standard != FinishStandard::Vdi3400
                && !(f.standard == FinishStandard::Spi
                    && f.grade.as_bytes().first().is_some_and(|grade| *grade <= b'C'))
            {
                continue;
            }
            let error = (f64::from(f.render_roughness()) - r).abs();
            if error < distance - 1e-4 {
                distance = error;
                best = Some(id);
            }
        }
        if let Some(best) = best {
            set(p, best);
        }
    }

    pub fn find(&self, key: &str) -> Option<&ParamDesc<P>> {
        self.descs.iter().find(|d| d.key == key)
    }

    /// Numeric view for inspectors and sweeps; colours use the nearest sRGB swatch.
    pub fn get(&self, p: &P, d: &ParamDesc<P>) -> f64 {
        match &d.kind {
            Kind::Float { get, .. } => get(p),
            Kind::Int { get, .. } | Kind::Enum { get, .. } => get(p) as f64,
            Kind::Bool { get, .. } => if get(p) { 1.0 } else { 0.0 },
            Kind::Colour { get, .. } => {
                let c = get(p).srgb;
                let mut best = 0;
                let mut distance = 1e9;
                for (index, swatch) in SWATCHES.iter().enumerate() {
                    let [r, g, b] = std::array::from_fn(|i| swatch.colour.srgb[i] - c[i]);
                    let error = r * r + g * g + b * b;
                    if error < distance {
                        distance = error;
                        best = index;
                    }
                }
                best as f64
            }
            Kind::Material { get, .. } => MATERIALS.get(get(p).0 as usize)
                .map_or(0, |m| d.option_index(m.id)) as f64,
            Kind::Finish { get, .. } => FINISHES.get(get(p).0 as usize)
                .map_or(0, |f| d.option_index(f.id)) as f64,
            Kind::Path { get, .. } => if get(p).is_empty() { 0.0 } else { 1.0 },
        }
    }

    /// Clamp numeric input, round integer fields, then run the cross-field and material rules.
    pub fn set(&self, p: &mut P, d: &ParamDesc<P>, v: f64) {
        match &d.kind {
            Kind::Float { set, hard_lo, hard_hi, .. } => set(p, clamp(v, *hard_lo, *hard_hi)),
            Kind::Int { set, lo, hi, .. } => set(p, clamp(v, *lo as f64, *hi as f64).round() as i32),
            Kind::Enum { set, names, .. } => {
                set(p, clamp(v, 0.0, names.len().saturating_sub(1) as f64).round() as i32);
            }
            Kind::Bool { set, .. } => set(p, v > 0.5),
            Kind::Colour { set, .. } => {
                let index = clamp(v, 0.0, SWATCHES.len().saturating_sub(1) as f64).round() as usize;
                if let Some(swatch) = SWATCHES.get(index) {
                    set(p, swatch.colour);
                }
            }
            Kind::Material { set, .. } => {
                let options = d.options();
                let index = clamp(v, 0.0, options.len().saturating_sub(1) as f64).round() as usize;
                if let Some(id) = options.get(index).and_then(|id| MaterialId::lookup(id)) {
                    set(p, id);
                }
            }
            Kind::Finish { set, .. } => {
                let options = d.options();
                let index = clamp(v, 0.0, options.len().saturating_sub(1) as f64).round() as usize;
                if let Some(id) = options.get(index).and_then(|id| FinishId::lookup(id)) {
                    set(p, id);
                }
            }
            Kind::Path { .. } => return,
        }
        self.after_set(p, d);
        self.apply_rules(p, Some(d));
    }

    fn after_set(&self, p: &mut P, d: &ParamDesc<P>) {
        if let Some(after_set) = self.after_set {
            after_set(p, d);
        }
    }

    /// File spelling: unadorned numbers, enum names, booleans, hex colours and catalog ids.
    pub fn value_text(&self, p: &P, d: &ParamDesc<P>) -> String {
        match &d.kind {
            Kind::Float { get, .. } => format_value(get(p), "%g"),
            Kind::Int { get, .. } => get(p).to_string(),
            Kind::Enum { get, names, .. } => names.get(get(p) as usize).unwrap_or(&"").to_string(),
            Kind::Bool { get, .. } => get(p).to_string(),
            Kind::Colour { get, .. } => hex_string(get(p).srgb),
            Kind::Material { get, .. } => MATERIALS.get(get(p).0 as usize).map_or("", |m| m.id).to_owned(),
            Kind::Finish { get, .. } => FINISHES.get(get(p).0 as usize).map_or("", |f| f.id).to_owned(),
            Kind::Path { get, .. } => get(p),
        }
    }

    /// Read file syntax or a named swatch. Hex colours preserve the existing translucency.
    pub fn set_text(&self, p: &mut P, d: &ParamDesc<P>, value: &str) -> bool {
        match &d.kind {
            Kind::Float { .. } | Kind::Int { .. } => {
                self.set(p, d, float_prefix(value));
                return true;
            }
            Kind::Bool { set, .. } => {
                set(p, matches!(value, "true" | "1" | "yes"));
                self.apply_rules(p, Some(d));
                return true;
            }
            Kind::Enum { names, .. } => {
                let index = names.iter().rposition(|name| *name == value)
                    .or_else(|| {
                        let value = compact(value);
                        names.iter().position(|name| compact(name) == value)
                    })
                    .map_or_else(|| int_prefix(value), |index| index as i32);
                self.set(p, d, index as f64);
                return true;
            }
            Kind::Colour { get, set } => {
                if let Some(hex) = value.strip_prefix('#') {
                    if hex.len() == 6 && hex.bytes().all(|c| c.is_ascii_hexdigit()) {
                        if let Ok(hex) = u32::from_str_radix(hex, 16) {
                            let c = Colour::hex(hex).with_translucency(get(p).translucency);
                            set(p, c);
                            self.apply_rules(p, Some(d));
                            return true;
                        }
                    }
                }
                if let Some(index) = palette_index(value) {
                    self.set(p, d, index as f64);
                    return true;
                }
                let Some(c) = colour_by_name(value) else { return false };
                set(p, c);
            }
            Kind::Path { set, .. } => {
                set(p, value.to_owned());
                self.after_set(p, d);
                return true;
            }
            Kind::Material { set, .. } => {
                let Some(id) = resolve_file(d, value).and_then(MaterialId::lookup) else { return false };
                set(p, id);
            }
            Kind::Finish { set, .. } => {
                let Some(id) = resolve_file(d, value).and_then(FinishId::lookup) else { return false };
                set(p, id);
            }
        }
        self.after_set(p, d);
        self.apply_rules(p, Some(d));
        true
    }

    /// Make an absolute asset path relative to a file's directory, without accessing the asset.
    pub fn relative_path(value: &str, base_dir: Option<&Path>) -> String {
        let Some(base_dir) = base_dir.filter(|base| !base.as_os_str().is_empty()) else {
            return value.to_owned();
        };
        let path = Path::new(value);
        if value.is_empty() || !path.is_absolute() {
            return value.to_owned();
        }
        let Some(base) = absolute_base(base_dir) else { return value.to_owned() };
        let target: Vec<_> = path.components().collect();
        let base: Vec<_> = base.components().collect();
        if target.first() != base.first() {
            return generic_path(path);
        }
        let shared = target.iter().zip(&base).take_while(|(a, b)| a == b).count();
        let parents = base[shared..].iter().fold(0isize, |count, component| {
            count + match component {
                Component::Normal(_) => 1,
                Component::ParentDir => -1,
                _ => 0,
            }
        });
        if parents < 0 {
            return generic_path(path);
        }
        let mut relative = PathBuf::new();
        for _ in 0..parents {
            relative.push("..");
        }
        for component in &target[shared..] {
            relative.push(component.as_os_str());
        }
        if relative.as_os_str().is_empty() {
            relative.push(".");
        }
        generic_path(&relative)
    }

    /// Resolve an asset path against a file's directory and remove lexical dot components.
    pub fn absolute_path(value: &str, base_dir: Option<&Path>) -> String {
        let Some(base_dir) = base_dir.filter(|base| !base.as_os_str().is_empty()) else {
            return value.to_owned();
        };
        if value.is_empty() {
            return String::new();
        }
        let path = Path::new(value);
        let path = if path.is_absolute() { path.to_owned() } else {
            let Some(base) = absolute_base(base_dir) else { return value.to_owned() };
            base.join(path)
        };
        normal_path(&path).to_string_lossy().into_owned()
    }

    /// Grouped `key = value` text. Expose translucency as a float field if it must be saved.
    pub fn to_text(&self, p: &P, base_dir: Option<&Path>) -> String {
        let mut text = format!("{}\nname = {}\n", self.header, p.name());
        let mut group = "";
        for d in &self.descs {
            if group != d.group {
                group = d.group;
                text.push_str(&format!("\n# {group}\n"));
            }
            let value = match &d.kind {
                Kind::Path { get, .. } => Self::relative_path(&get(p), base_dir),
                _ => self.value_text(p, d),
            };
            text.push_str(&format!("{} = {value}\n", d.key));
        }
        text
    }

    /// Apply lines in order; missing fields retain their values and unknown keys go to `legacy`.
    /// All understood lines are applied before returning any malformed-line diagnostic.
    pub fn from_text(&self, text: &str, p: &mut P, base_dir: Option<&Path>) -> Result<(), String> {
        let mut found = 0;
        let mut bad = 0;
        for line in text.lines().map(trimmed) {
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                bad += 1;
                continue;
            };
            let (key, value) = (trimmed(key), trimmed(value));
            if key == "name" {
                p.set_name(value.to_owned());
                found += 1;
                continue;
            }
            let Some(d) = self.find(key) else {
                if self.legacy.is_some_and(|legacy| legacy(p, key, value)) {
                    found += 1;
                }
                continue;
            };
            let understood = if matches!(d.kind, Kind::Path { .. }) {
                self.set_text(p, d, &Self::absolute_path(value, base_dir))
            } else {
                self.set_text(p, d, value)
            };
            if !understood {
                bad += 1;
            }
            found += 1;
        }
        if found == 0 {
            Err("no parameters found".to_owned())
        } else if bad > 0 {
            Err(format!("{bad} line(s) not understood"))
        } else {
            Ok(())
        }
    }
}

fn clamp(value: f64, lo: f64, hi: f64) -> f64 {
    value.min(hi).max(lo)
}

fn compact(value: &str) -> String {
    value.chars().filter(|c| !c.is_ascii_whitespace()).map(|c| c.to_ascii_lowercase()).collect()
}

fn resolve_file<P>(d: &ParamDesc<P>, value: &str) -> Option<&'static str> {
    d.resolve(value).or_else(|| {
        value.as_bytes().first().filter(|c| c.is_ascii_digit())
            .and_then(|_| d.resolve(&int_prefix(value).to_string()))
    })
}

fn int_prefix(value: &str) -> i32 {
    let value = value.trim_start_matches(|c: char| c.is_ascii_whitespace());
    let sign = usize::from(value.starts_with(['+', '-']));
    let end = sign + value[sign..].bytes().take_while(u8::is_ascii_digit).count();
    if end == sign {
        return 0;
    }
    value[..end].parse().unwrap_or(if value.starts_with('-') { i32::MIN } else { i32::MAX })
}

// Numeric file values may have a suffix; an unrecognised prefix reads as zero.
fn float_prefix(value: &str) -> f64 {
    let value = value.trim_start_matches(|c: char| c.is_ascii_whitespace());
    let sign = usize::from(value.starts_with(['+', '-']));
    let tail = &value[sign..];
    if tail.get(..3).is_some_and(|s| s.eq_ignore_ascii_case("inf")) {
        return if sign == 1 && value.starts_with('-') { f64::NEG_INFINITY } else { f64::INFINITY };
    }
    if tail.get(..3).is_some_and(|s| s.eq_ignore_ascii_case("nan")) {
        return f64::NAN;
    }
    let bytes = value.as_bytes();
    let mut end = sign;
    while bytes.get(end).is_some_and(u8::is_ascii_digit) { end += 1; }
    let mut digits = end - sign;
    if bytes.get(end) == Some(&b'.') {
        end += 1;
        let start = end;
        while bytes.get(end).is_some_and(u8::is_ascii_digit) { end += 1; }
        digits += end - start;
    }
    if digits == 0 {
        return 0.0;
    }
    if matches!(bytes.get(end), Some(b'e' | b'E')) {
        let mut exponent = end + 1;
        if matches!(bytes.get(exponent), Some(b'+' | b'-')) { exponent += 1; }
        let start = exponent;
        while bytes.get(exponent).is_some_and(u8::is_ascii_digit) { exponent += 1; }
        if exponent > start { end = exponent; }
    }
    value[..end].parse().unwrap_or(0.0)
}

fn absolute_base(path: &Path) -> Option<PathBuf> {
    if path.is_absolute() { Some(path.to_owned()) } else {
        std::env::current_dir().ok().map(|base| base.join(path))
    }
}

fn generic_path(path: &Path) -> String {
    let path = path.to_string_lossy();
    if std::path::MAIN_SEPARATOR == '/' { path.into_owned() } else {
        path.replace(std::path::MAIN_SEPARATOR, "/")
    }
}

fn normal_path(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if matches!(out.components().next_back(), Some(Component::Normal(_))) {
                    out.pop();
                } else if !out.has_root() {
                    out.push("..");
                }
            }
            component => out.push(component.as_os_str()),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone)]
    struct Part {
        name: String,
        width: f64,
        count: i32,
        enabled: bool,
        preset: i32,
        colour: Colour,
        material: MaterialId,
        finish: FinishId,
        artwork: String,
        notifications: usize,
    }

    impl Default for Part {
        fn default() -> Self {
            Self {
                name: "Panel part".to_owned(),
                width: 2.5,
                count: 2,
                enabled: true,
                preset: 0,
                colour: Colour::hex(0x1A1A1C),
                material: MaterialId::lookup("pc").unwrap(),
                finish: FinishId::lookup("spi_a2").unwrap(),
                artwork: String::new(),
                notifications: 0,
            }
        }
    }

    impl PartParams for Part {
        fn name(&self) -> &str { &self.name }
        fn set_name(&mut self, name: String) { self.name = name; }
        fn colour(&self) -> Colour { self.colour }
        fn set_colour(&mut self, c: Colour) { self.colour = c; }
    }

    fn table() -> ParamTable<Part> {
        let (preset_get, preset_set) = field!(Part, preset);
        let (width_get, width_set) = field!(Part, width);
        let (count_get, count_set) = field!(Part, count);
        let (enabled_get, enabled_set) = field!(Part, enabled);
        let (colour_get, colour_set) = field!(Part, colour);
        let (material_get, material_set) = field!(Part, material);
        let (finish_get, finish_set) = field!(Part, finish);
        let (artwork_get, artwork_set) = field!(Part, artwork);
        ParamTable {
            descs: vec![
                enumeration("preset", "Preset", "Shape", preset_get, preset_set, &["Custom", "Wide part"])
                    .notes(&["Keep dimensions", "Set dimensions"]),
                float("width", "Width", "Shape", width_get, width_set, 1.0, 8.0, "%.2f mm")
                    .tip("Width across the part").hard(-2.0, 12.0),
                int("count", "Count", "Shape", count_get, count_set, -3, 8),
                boolean("enabled", "Enabled", "Shape", enabled_get, enabled_set),
                material("material", "Material", "Look", material_get, material_set, None, Some(|m| m.id == "pc"),
                    &[("old clear", "pc"), ("0", "pc"), ("PC", "abs")]),
                finish("finish", "Finish", "Look", finish_get, finish_set, None, &[("old polish", "spi_a2")]),
                colour("colour", "Colour", "Look", colour_get, colour_set),
                float("translucency", "Translucency", "Look", |p| f64::from(p.colour.translucency),
                    |p, v| p.colour.translucency = v as f32, 0.0, 0.999, "%.3f"),
                path("artwork", "Artwork", "Files", artwork_get, artwork_set, Some("svg"), false),
            ],
            header: "# Panel part",
            after_set: Some(|p, d| {
                p.notifications += 1;
                if d.key == "preset" && p.preset == 1 {
                    p.width = 6.0;
                    p.count = 4;
                }
            }),
            legacy: Some(|p, key, value| {
                if key == "old_count" {
                    p.count = int_prefix(value);
                    true
                } else { false }
            }),
        }
    }

    #[test]
    fn text_round_trip_and_ordered_updates() {
        let table = table();
        let mut p = Part::default();
        table.from_text("width = 3.25\nunknown = ignored\nold_count = 5", &mut p, None).unwrap();
        assert_eq!((p.width, p.count), (3.25, 5), "partial files apply known and legacy keys");
        assert!(p.enabled && p.preset == 0 && p.name == Part::default().name,
            "missing keys retain defaults");
        assert_eq!(p.material, Part::default().material, "missing material retains its default");

        let base = std::env::current_dir().unwrap().join("parts");
        table.from_text(
            "# part\nname = Example\npreset = widepart\nwidth = 4.125\ncount = 3\n\
             material = old clear\nfinish = old polish\ncolour = Smoke\nartwork = ../art/./legend.svg",
            &mut p, Some(&base),
        ).unwrap();
        assert_eq!((p.width, p.count), (4.125, 3), "later values override the preset");
        assert_eq!(p.colour.translucency, 0.6, "named swatches carry translucency");
        let text = table.to_text(&p, Some(&base));
        assert!(text.contains("material = pc\nfinish = spi_a2\n"), "files use canonical ids");
        assert!(text.contains("artwork = ../art/legend.svg\n"), "asset paths are relative and normalised");
        assert!(text.contains("colour = #3A3D44\n"), "colours use hex file syntax");
        let mut restored = Part::default();
        table.from_text(&text, &mut restored, Some(&base)).unwrap();
        assert_eq!(table.to_text(&restored, Some(&base)), text, "serialisation is stable");
        assert_eq!(restored.colour, p.colour, "a separate translucency field preserves transparent colours");
        assert_eq!(restored.artwork, p.artwork, "relative assets resolve back to their absolute path");
        assert_eq!(ParamTable::<Part>::relative_path(base.to_str().unwrap(), Some(&base)), ".",
            "the base directory is relative to itself");
        assert_eq!(ParamTable::<Part>::absolute_path("art.svg", None), "art.svg", "no base leaves paths alone");
        assert_eq!(table.from_text("unknown = ignored", &mut p, None), Err("no parameters found".to_owned()),
            "unknown-only files report no parameters");
        assert_eq!(table.from_text("bad line\ncolour = no such colour\nwidth = 7", &mut p, None),
            Err("2 line(s) not understood".to_owned()), "bad lines are counted");
        assert_eq!(p.width, 7.0, "a bad line does not stop later assignments");
    }

    #[test]
    fn material_rules_options_and_legacy_roughness() {
        let table = table();
        let md = table.material_desc().unwrap();
        let fd = table.finish_desc("finish").unwrap();
        let mut p = Part::default();
        let options = md.options();
        assert_eq!(options[0], "pc", "featured materials come first");
        assert_eq!(options.iter().filter(|id| **id == "pc").count(), 1, "featured materials occur once");
        assert!(options.iter().all(|id| MaterialId::lookup(id).unwrap().get().family.is_moulding()), "a part picks no board rows");
        assert!(fd.options().iter().all(|id| FinishId::lookup(id).unwrap().get().processes != Processes::NONE), "a part picks no mask grade or as-made surface");
        assert_eq!(md.resolve("PC"), Some("pc"), "catalog names take precedence over aliases");
        assert_eq!(md.resolve("PC-ABS"), Some("pc_abs"), "catalog spellings resolve");
        assert_eq!(fd.resolve("SPI A-2"), Some("spi_a2"), "finish labels resolve");
        assert_eq!(md.option_index("missing"), 0, "unknown option ids use the first index");
        assert!(table.set_text(&mut p, md, "0.0"), "numeric legacy aliases accept old float spellings");
        assert_eq!(p.material.name(), "pc", "legacy alias selects the typed material");
        p.finish = FinishId::lookup("sla").unwrap();
        p.colour = Colour::hex(0xFFCC88).with_translucency(0.8);
        assert!(table.set_text(&mut p, md, "abs"), "opaque material resolves");
        assert_eq!(p.finish, p.material.get().default_finish(), "a material change replaces an incompatible finish");
        assert!(p.finish.get().check(p.material.get(), None, false).is_empty(), "the replacement finish is compatible");
        assert_eq!(p.colour.translucency, 0.0, "an opaque material drops translucency");
        table.legacy_roughness(&mut p, 0.55);
        assert_eq!(p.finish.name(), "vdi_27", "roughness within 0.08 keeps the finish");
        table.legacy_roughness(&mut p, 0.10);
        assert_eq!(p.finish.name(), "spi_a2", "roughness selects the nearest eligible polish");
        table.legacy_roughness(&mut p, 0.90);
        assert_eq!(p.finish.name(), "vdi_45", "roughness selects the nearest eligible texture");

        p.colour = Colour::hex(0xFFCC88).with_translucency(0.8);
        assert!(table.set_text(&mut p, md, "pc_abs_cnt"), "conductive material resolves");
        assert!(p.material.get().colour_allowed(p.colour.srgb), "conductive grades clamp to dark colours");
        assert_eq!(p.colour.translucency, 0.0, "conductive grades are opaque");
        p.colour = Colour::hex(0xFF0000).with_translucency(0.5);
        table.apply_rules(&mut p, None);
        assert!(p.material.get().colour_allowed(p.colour.srgb) && p.colour.translucency == 0.0,
            "conductive rules also run without a changed material");

        let (get, set) = field!(Part, finish);
        let filtered = finish("finish", "Finish", "Look", get, set,
            Some(|f| f.standard == FinishStandard::Spi && f.clear_ok), &[]);
        assert_eq!(table.finish_for(MaterialId::lookup("abs").unwrap().get(), &filtered).name(), "spi_a1",
            "a disallowed default falls back to the first allowed compatible finish");
        let (get, set) = field!(Part, material);
        let filtered = material("material", "Material", "Look", get, set,
            Some(|m| m.electrical.dark_only), Some(|m| m.id == "pc"), &[]);
        assert!(filtered.options().iter().all(|id| MaterialId::lookup(id).unwrap().get().electrical.dark_only),
            "featured materials cannot bypass the allowed filter");
        let empty = ParamTable::<Part> { descs: Vec::new(), header: "", after_set: None, legacy: None };
        assert_eq!(empty.material(&p).name(), "abs", "tables without materials use ABS");
        assert_eq!(empty.finish(&p, "finish"), empty.material(&p).get().default_finish(),
            "tables without finishes use the material default");
    }

    #[test]
    fn numeric_edits_colours_and_formats() {
        let table = table();
        let mut p = Part::default();
        let width = table.find("width").unwrap();
        table.set(&mut p, width, 10.0);
        assert_eq!(p.width, 10.0, "hard limits can extend beyond slider limits");
        table.set(&mut p, width, 30.0);
        assert_eq!(p.width, 12.0, "upper hard limit clamps");
        table.set(&mut p, width, -30.0);
        assert_eq!(p.width, -2.0, "lower hard limit clamps");
        table.set(&mut p, table.find("count").unwrap(), 2.5);
        assert_eq!(p.count, 3, "integer ties round away from zero");
        table.set(&mut p, table.find("count").unwrap(), -2.5);
        assert_eq!(p.count, -3, "negative integer ties round away from zero");
        table.set(&mut p, table.find("count").unwrap(), 30.0);
        assert_eq!(p.count, 8, "integer limits clamp");
        table.set_text(&mut p, width, " 1.25e1 mm");
        assert_eq!(p.width, 12.0, "numeric prefixes are parsed before clamping");
        table.set_text(&mut p, width, "1.2345678");
        assert_eq!(table.value_text(&p, width).parse::<f64>().unwrap(), p.width,
            "file floats retain round-trip precision");

        let c = table.find("colour").unwrap();
        assert!(table.set_text(&mut p, c, "Smoke"), "swatches resolve");
        assert_eq!(table.get(&p, c), palette_index("smoke").unwrap() as f64, "numeric colours use nearest swatches");
        let notifications = p.notifications;
        assert!(table.set_text(&mut p, c, "#123456"), "hex colours resolve");
        assert_eq!(p.colour.translucency, 0.6, "hex colours keep translucency");
        table.set_text(&mut p, table.find("enabled").unwrap(), "yes");
        assert_eq!(p.notifications, notifications, "hex and bool text bypass after_set");
        assert_eq!(table.get(&p, table.find("enabled").unwrap()), 1.0, "bool numeric view is one or zero");
        assert!(table.set_text(&mut p, c, "RAL 9005"), "standard colour codes resolve");
        assert_eq!(p.colour.ral, Some(9005), "standard colour metadata is retained");
        assert!(table.set_text(&mut p, c, "jet black"), "standard colour names resolve");
        assert_eq!(p.colour.translucency, 0.0, "standard colours are opaque");
        assert!(!table.set_text(&mut p, c, "#12ZZ56"), "invalid hex colours are rejected");
        let artwork = table.find("artwork").unwrap();
        table.set_text(&mut p, artwork, "legend.svg");
        let notifications = p.notifications;
        table.set(&mut p, artwork, 0.0);
        assert_eq!((p.artwork.as_str(), p.notifications), ("legend.svg", notifications),
            "numeric path edits are ignored without callbacks");
        assert!(!artwork.sweepable() && table.material_desc().unwrap().list_type(),
            "descriptor categories distinguish paths and catalog pickers");

        assert_eq!(format_value(2.5, "%.2f mm"), "2.50 mm", "millimetre formatting");
        assert_eq!(format_value(89.6, "%.0f deg"), "90 deg", "degree formatting");
        assert_eq!(format_value(7.0, "%d"), "7", "integer formatting");
        assert_eq!(format_value(1.25, "%g"), "1.25", "file formatting omits trailing zeros");
        assert_eq!((unit_of("%.2f mm"), unit_of("%.0f deg"), unit_of("%d")), ("mm", "deg", ""),
            "units follow the conversion");
    }
}
