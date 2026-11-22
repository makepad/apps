//! Calibrated filament sets: a set as its swatch photo read it back ([`fab_colour::read`]),
//! kept by name, one Splash file each in the documents' `calibration` folder
//! (`<name>.colour.splash`):
//!
//! ```splash
//! // Fab calibration: a filament set as its swatch photo read it.
//! use mod.fab.*
//! filament("Cyan", colour: [0.0 0.3 0.62], scatter: [1.8 1.8 1.8])
//! stack([["Cyan" 0.16mm] ["White" 0.48mm]], over: [0.8 0.8 0.8], colour: [0.02 0.3 0.6])
//! ```
//!
//! A `filament` is its optics (its colour when thick, linear, and its scattering per mm); a
//! `stack` one patch as read: its filament runs from the face, the background it was seen over
//! and the colour it showed (linear).

use crate::program::UNIT;
use fab_colour::palette::{Filament, FilamentSet, Measured};
use makepad_model_script::lang::{self, Arg, Options, RootFn, V};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// The calibrated sets kept, by name.
pub type Sets = BTreeMap<String, Arc<FilamentSet>>;

/// A calibrated set's file extension (`Matte CMYW.colour.splash`).
pub const EXTENSION: &str = "colour.splash";
/// The documents' folder the sets are kept in.
const FOLDER: &str = "calibration";

const ROOTS: &[RootFn] = &[
    RootFn { name: "filament", args: &["name", "colour", "scatter"], model: false },
    RootFn { name: "stack", args: &["runs", "over", "colour"], model: false },
];

/// Where the set `name` is kept.
pub fn path(documents: &Path, name: &str) -> Result<PathBuf, String> {
    if name.trim().is_empty() || name.trim() != name || name.starts_with('.') || name.contains(['/', '\\', ':']) {
        return Err(format!("'{name}' cannot name a filament set: a name without slashes or colons, not starting with a dot"));
    }
    Ok(documents.join(FOLDER).join(format!("{name}.{EXTENSION}")))
}

/// The set as its file holds it.
pub fn write(set: &FilamentSet) -> String {
    let num = |x: f32| format!("{x}");
    let rgb = |c: [f32; 3]| format!("[{} {} {}]", num(c[0]), num(c[1]), num(c[2]));
    let mut out = String::from("// Fab calibration: a filament set as its swatch photo read it.\nuse mod.fab.*\n\n");
    for f in &set.filaments {
        out += &format!("filament({}, colour: {}, scatter: {})\n", makepad_script::syntax::write::quote(&f.name), rgb(f.colour), rgb(f.scatter));
    }
    if !set.measured.is_empty() {
        out += "\n";
    }
    for m in &set.measured {
        let runs: Vec<String> = m.stack.iter().map(|(f, mm)| format!("[{} {}mm]", makepad_script::syntax::write::quote(&set.filaments[*f as usize].name), mm)).collect();
        out += &format!("stack([{}], over: {}, colour: {})\n", runs.join(" "), rgb(m.background), rgb(m.colour));
    }
    out
}

/// Read a set's file (`name`: the set's name, its file's stem). Errors name the line.
pub fn parse(name: &str, text: &str) -> Result<FilamentSet, String> {
    let assets = crate::program::Assets::new();
    let program = lang::evaluate(text, &Options { generated: &Default::default(), assets: &assets, images: &Default::default(), extension: "fab", libraries: &[], inputs: &lang::NO_INPUTS, unit: UNIT, roots: ROOTS, overrides: Default::default(), weave: None, solved: Default::default() })
        .map_err(|e| e.iter().map(|d| format!("{name}:{}:{}: {}", d.line, d.col, d.message)).collect::<Vec<_>>().join("\n"))?;
    let mut set = FilamentSet::new(name, Vec::new());
    for root in &program.roots {
        let at = |m: String| {
            let (line, col) = root.loc.unwrap_or((1, 1));
            format!("{name}:{line}:{col}: {}: {m}", root.kind)
        };
        let value = |k: &str| match root.get(k) {
            Some(Arg::Value(v)) => Some(v),
            _ => None,
        };
        let rgb = |k: &str| -> Result<[f32; 3], String> {
            match value(k) {
                Some(V::Arr(a)) => match a.as_slice() {
                    [V::Num(r), V::Num(g), V::Num(b)] => Ok([*r as f32, *g as f32, *b as f32]),
                    _ => Err(at(format!("'{k}' is three numbers [r g b]"))),
                },
                _ => Err(at(format!("'{k}' is three numbers [r g b]"))),
            }
        };
        match root.kind {
            "filament" => {
                let Some(V::Str(f)) = value("name") else { return Err(at("the first argument is the filament's name".into())) };
                if set.filaments.iter().any(|g| g.name == *f) {
                    return Err(at(format!("two filaments are named '{f}'")));
                }
                set.filaments.push(Filament { name: f.clone(), colour: rgb("colour")?, scatter: rgb("scatter")? });
            }
            "stack" => {
                let Some(V::Arr(runs)) = value("runs") else { return Err(at("the first argument is the runs from the face: [[\"Cyan\" 0.16mm] ..]".into())) };
                let stack = runs
                    .iter()
                    .map(|r| match r {
                        V::Arr(r) => match r.as_slice() {
                            [V::Str(f), V::Num(mm)] => match set.filaments.iter().position(|g| g.name == *f) {
                                Some(i) => Ok((i as u8, mm * UNIT)),
                                None => Err(at(format!("no filament(\"{f}\") above it"))),
                            },
                            _ => Err(at("a run is [\"Cyan\" 0.16mm]".into())),
                        },
                        _ => Err(at("a run is [\"Cyan\" 0.16mm]".into())),
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                set.measured.push(Measured { stack, background: rgb("over")?, colour: rgb("colour")? });
            }
            _ => {}
        }
    }
    if set.filaments.is_empty() {
        return Err(format!("{name}: no filament(...) in it"));
    }
    Ok(set)
}

/// Keep `set` under its name in the documents' calibration folder; its path.
pub fn save(documents: &Path, set: &FilamentSet) -> Result<PathBuf, String> {
    let path = path(documents, &set.name)?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    std::fs::write(&path, write(set)).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(path)
}

/// The set kept as `name`.
pub fn load(documents: &Path, name: &str) -> Result<FilamentSet, String> {
    let path = path(documents, name)?;
    let text = std::fs::read_to_string(&path).map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => format!("no calibrated filament set '{name}': print the colour swatch and read its photo first"),
        _ => format!("{}: {e}", path.display()),
    })?;
    parse(name, &text)
}

/// The names of the sets kept, sorted.
pub fn names(documents: &Path) -> Vec<String> {
    let suffix = format!(".{EXTENSION}");
    let mut out: Vec<String> = std::fs::read_dir(documents.join(FOLDER))
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| e.file_name().to_str().and_then(|n| n.strip_suffix(&suffix)).map(str::to_string))
        .collect();
    out.sort();
    out
}

/// Every set kept, and what could not be read (a line each).
pub fn load_all(documents: &Path) -> (Sets, Vec<String>) {
    let (mut sets, mut errors) = (Sets::new(), Vec::new());
    for name in names(documents) {
        match load(documents, &name) {
            Ok(set) => {
                sets.insert(name, Arc::new(set));
            }
            Err(e) => errors.push(e),
        }
    }
    (sets, errors)
}

/// The kept set measured with these filaments (name and linear colour, in slot order: the same
/// spools, each within a just-seen difference) at this layer height, if one was.
pub fn matching<'a>(sets: &'a Sets, loaded: &[(String, [f32; 3])], layer: f64) -> Option<&'a Arc<FilamentSet>> {
    let same = |a: [f32; 3], b: [f32; 3]| fab_colour::oklab::delta_e(fab_colour::oklab::oklab(a), fab_colour::oklab::oklab(b)) < 0.02;
    sets.values().find(|s| {
        s.filaments.len() == loaded.len()
            && s.filaments.iter().zip(loaded).all(|(f, (n, c))| f.name == *n && same(f.colour, *c))
            && s.measured.first().and_then(|m| m.stack.first()).is_some_and(|r| (r.1 - layer).abs() < 1e-3)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A set written and read again is the same set, its stacks' thicknesses within a µm; a
    /// stack naming a filament the file has not is an error at its line. Pictures find it.
    #[test]
    fn a_calibrated_set_round_trips_through_its_file() {
        let mut set = FilamentSet::new("Matte CMYW", ["Cyan", "Magenta", "Yellow", "White"].iter().zip([[0u8, 160, 224], [224, 0, 122], [255, 224, 0], [242, 242, 242]]).map(|(n, c)| Filament { scatter: [1.8, 2.1, 2.4], ..Filament::guess(n, c) }).collect());
        set.measured.push(Measured { stack: vec![(0, 0.16), (3, 0.48)], background: [0.8; 3], colour: [0.012_345_678, 0.3, 0.61] });
        set.measured.push(Measured { stack: vec![(1, 0.16), (2, 0.16), (3, 0.32)], background: [0.8; 3], colour: [0.5, 0.05, 0.2] });
        let back = parse("Matte CMYW", &write(&set)).unwrap();
        assert_eq!(back.filaments, set.filaments);
        assert_eq!(back.measured.len(), 2);
        for (a, b) in back.measured.iter().zip(&set.measured) {
            assert_eq!((a.background, a.colour), (b.background, b.colour));
            assert!(a.stack.len() == b.stack.len() && a.stack.iter().zip(&b.stack).all(|(x, y)| x.0 == y.0 && (x.1 - y.1).abs() < 1e-6), "{:?}", a.stack);
        }
        let bad = write(&set).replace("[\"Magenta\" 0.16mm]", "[\"Black\" 0.16mm]");
        let e = parse("Matte CMYW", &bad).unwrap_err();
        assert!(e.contains("no filament(\"Black\")") && e.starts_with("Matte CMYW:"), "{e}");
        assert!(path(Path::new("/docs"), "a/b").is_err() && path(Path::new("/docs"), ".hidden").is_err());
        // a picture naming the set prints with it, as does one with no set or filaments of its
        // own while those filaments are loaded at its layer height; at another, a first guess
        let arg = |set: Option<&str>, layer: f64| crate::picture::Arg { swatch: false, size: [20.0, 20.0], layers: 4, layer, face_down: true, min_run: 2, smooth: 0.3, set: set.map(str::to_string) };
        let spec = |a| crate::picture::Spec::new(a, None, Vec::new(), [0.0; 3]);
        let loaded: Vec<(String, [f32; 3])> = set.filaments.iter().map(|f| (f.name.clone(), f.colour)).collect();
        let sets: Sets = [("Matte CMYW".to_string(), Arc::new(set.clone()))].into();
        assert_eq!(crate::picture::set_of(&spec(arg(Some("Matte CMYW"), 0.16)), &[], &sets).unwrap(), set);
        assert_eq!(crate::picture::set_of(&spec(arg(None, 0.16)), &loaded, &sets).unwrap(), set);
        assert!(crate::picture::set_of(&spec(arg(None, 0.2)), &loaded, &sets).unwrap().measured.is_empty());
        assert!(crate::picture::set_of(&spec(arg(Some("Matte CMYK"), 0.16)), &loaded, &sets).unwrap_err().contains("no calibrated filament set 'Matte CMYK'"));
        assert_eq!(path(Path::new("/docs"), "Matte CMYW").unwrap(), Path::new("/docs/calibration/Matte CMYW.colour.splash"));
    }
}
