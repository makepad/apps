//! `fab-parity`: slice a matrix of test jobs with Fab and with a reference
//! slicer, from the same profiles, and rank the differences by print risk.
//!
//! fab-parity run --out DIR [--reference EXE] [--profiles DIR] [--printer NAME] [--only CASE]
//!     prepare every case (presets resolved from the installed profiles and
//!     written flattened, the test parts written as STL), run the reference
//!     slicer's command line on them (when `--reference` names it), slice them
//!     with Fab, and write `DIR/report.txt`.
//! fab-parity fab --out DIR [--only CASE]
//!     slice the prepared cases with Fab again and compare against the
//!     reference slices already in DIR (the regression run).
//! `--own-flush`: Fab's purge matrix from the colours instead of the
//! reference's (its command line uses a fixed default).
//! fab-parity cases
//!     list the matrix.
//!
//! The reference slicer runs in its own process with a data folder inside
//! DIR, so the user's own configuration is never read or written. Nothing
//! is sent to a printer. Profiles and reference output stay in DIR (local
//! data, never part of the repository).

use makepad_fab_settings::import::preset_json;
use makepad_fab_settings::profiles::{load_preset_file, JobSetup, Preset, ProfileStore, RawPreset};
use makepad_fab_slice::{parity, place, slice, PlacedObject};
use makepad_fab_slicer::mesh::{load_model, Mesh};
use std::path::{Path, PathBuf};

/// One test job.
struct Case {
    name: &'static str,
    /// Test parts, one object each (`two` places two copies, one per filament).
    part: Part,
    process: &'static str,
    /// Filament preset per slot, with an optional colour override.
    filaments: &'static [(&'static str, Option<&'static str>)],
    /// 1-based nozzle per filament (1 = left).
    map: &'static [u32],
    /// Filament settings changed for a one-variable sweep (every filament).
    set: &'static [(&'static str, &'static str)],
}

#[derive(Clone, Copy, PartialEq)]
enum Part {
    Cube,
    Bracket,
    /// Two cubes, filament 1 and filament 2.
    TwoColour,
}

const PLA: &str = "Generic PLA @BBL H2D";
const PETG: &str = "Generic PETG @BBL H2D";
const ABS: &str = "Generic ABS @BBL H2D";
const STD: &str = "0.20mm Standard @BBL H2D";
const FINE: &str = "0.12mm Fine @BBL H2D";

const CASES: &[Case] = &[
    Case { name: "cube_pla_020", part: Part::Cube, process: STD, filaments: &[(PLA, None)], map: &[1], set: &[] },
    Case { name: "cube_pla_012", part: Part::Cube, process: FINE, filaments: &[(PLA, None)], map: &[1], set: &[] },
    Case { name: "cube_petg_020", part: Part::Cube, process: STD, filaments: &[(PETG, None)], map: &[2], set: &[] },
    Case { name: "cube_petg_012", part: Part::Cube, process: FINE, filaments: &[(PETG, None)], map: &[1], set: &[] },
    Case { name: "bracket_pla_020", part: Part::Bracket, process: STD, filaments: &[(PLA, None)], map: &[1], set: &[] },
    Case { name: "bracket_petg_012", part: Part::Bracket, process: FINE, filaments: &[(PETG, None)], map: &[2], set: &[] },
    Case { name: "twocolour_pla_020", part: Part::TwoColour, process: STD, filaments: &[(PLA, Some("#FFFFFF")), (PLA, Some("#C12E1F"))], map: &[1, 2], set: &[] },
    Case { name: "twocolour_pla_petg_020", part: Part::TwoColour, process: STD, filaments: &[(PLA, Some("#FFFFFF")), (PETG, Some("#1F1F1F"))], map: &[1, 2], set: &[] },
    // First layer and the rest at different temperatures.
    Case { name: "cube_abs_020", part: Part::Cube, process: STD, filaments: &[(ABS, None)], map: &[1], set: &[] },
    // One-variable sweeps: what the reference's time estimate charges for heating.
    Case { name: "sweep_pla_bed90", part: Part::Cube, process: STD, filaments: &[(PLA, None)], map: &[1], set: &[("textured_plate_temp", "90"), ("textured_plate_temp_initial_layer", "90")] },
    Case { name: "sweep_pla_chamber45", part: Part::Cube, process: STD, filaments: &[(PLA, None)], map: &[1], set: &[("chamber_temperatures", "45")] },
    Case { name: "sweep_pla_chamber60", part: Part::Cube, process: STD, filaments: &[(PLA, None)], map: &[1], set: &[("chamber_temperatures", "60")] },
    Case { name: "twocolour_one_nozzle_020", part: Part::TwoColour, process: STD, filaments: &[(PLA, Some("#FFFFFF")), (PLA, Some("#C12E1F"))], map: &[1, 1], set: &[] },
];

const PRINTER: &str = "Bambu Lab H2D 0.4 nozzle";
const BED: &str = "Textured PEI Plate";

struct Args(Vec<String>);

impl Args {
    fn one(&self, flag: &str) -> Option<String> {
        self.0.windows(2).filter(|w| w[0] == flag).map(|w| w[1].clone()).last()
    }
}

fn cube(m: &mut Mesh, lo: [f64; 3], hi: [f64; 3]) {
    let b = m.verts.len() as u32;
    for z in [lo[2], hi[2]] {
        m.verts.extend_from_slice(&[[lo[0], lo[1], z], [hi[0], lo[1], z], [hi[0], hi[1], z], [lo[0], hi[1], z]]);
    }
    for k in [[0, 2, 1], [0, 3, 2], [4, 5, 6], [4, 6, 7], [0, 1, 5], [0, 5, 4], [1, 2, 6], [1, 6, 5], [2, 3, 7], [2, 7, 6], [3, 0, 4], [3, 4, 7]] {
        m.tris.push([b + k[0], b + k[1], b + k[2]]);
    }
}

/// The STL files of a case's objects (written once into `models/`).
fn models(dir: &Path, part: Part) -> Result<Vec<PathBuf>, String> {
    let models = dir.join("models");
    std::fs::create_dir_all(&models).map_err(|e| e.to_string())?;
    let write = |name: &str, m: &Mesh| -> Result<PathBuf, String> {
        let p = models.join(name);
        if !p.exists() {
            std::fs::write(&p, makepad_fab_slice::bench::stl_bytes(m)).map_err(|e| format!("{}: {e}", p.display()))?;
        }
        Ok(p)
    };
    let mut c = Mesh::default();
    cube(&mut c, [0.0, 0.0, 0.0], [20.0, 20.0, 20.0]);
    Ok(match part {
        Part::Cube => vec![write("cube20.stl", &c)?],
        Part::Bracket => vec![write("bracket.stl", &makepad_fab_slice::bench::bracket())?],
        // One file per object: the reference command line takes a filament per file.
        Part::TwoColour => vec![write("cube20.stl", &c)?, write("cube20b.stl", &c)?],
    })
}

fn flat(p: &Preset, from: &str) -> RawPreset {
    RawPreset { kind: p.kind.clone(), name: p.name.clone(), from: from.into(), instantiation: true, setting_id: p.setting_id.clone(), filament_id: p.filament_id.clone(), values: p.values.clone(), ..Default::default() }
}

/// Resolve and write the case's presets, flattened (the same files go to
/// both slicers). Returns (machine, process, filaments) paths.
fn prepare(store: &ProfileStore, dir: &Path, case: &Case, printer: &str) -> Result<(PathBuf, PathBuf, Vec<PathBuf>), String> {
    let inputs = dir.join("inputs").join(case.name);
    std::fs::create_dir_all(&inputs).map_err(|e| e.to_string())?;
    let write = |file: &str, p: &RawPreset| -> Result<PathBuf, String> {
        let path = inputs.join(file);
        std::fs::write(&path, preset_json(p)).map_err(|e| format!("{}: {e}", path.display()))?;
        Ok(path)
    };
    let m = write("machine.json", &flat(&store.resolve(printer)?, "system"))?;
    let p = write("process.json", &flat(&store.resolve(case.process)?, "system"))?;
    let mut fils = Vec::new();
    for (i, (name, colour)) in case.filaments.iter().enumerate() {
        let mut f = flat(&store.resolve(name)?, "system");
        for (k, v) in case.set {
            let n = f.values.vec_len(k).max(1);
            f.values.set_vec(k, vec![v.to_string(); n]);
            f.from = "User".into();
        }
        if let Some(c) = colour {
            f.values.set_vec("filament_colour", vec![c.to_string()]);
            f.name = format!("{} {}", f.name, i + 1);
            f.from = "User".into();
        }
        fils.push(write(&format!("filament_{}.json", i + 1), &f)?);
    }
    Ok((m, p, fils))
}

/// The reference slicer's command line for a case (its own data folder in DIR).
fn run_reference(exe: &str, dir: &Path, case: &Case, presets: &(PathBuf, PathBuf, Vec<PathBuf>), models: &[PathBuf]) -> Result<PathBuf, String> {
    let out = dir.join("ref").join(case.name);
    std::fs::create_dir_all(&out).map_err(|e| e.to_string())?;
    let data = dir.join("ref-data");
    std::fs::create_dir_all(&data).map_err(|e| e.to_string())?;
    let join = |v: &[&PathBuf]| v.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join(";");
    let map = case.map.iter().map(|m| m.to_string()).collect::<Vec<_>>().join(",");
    let mut cmd = std::process::Command::new(exe);
    cmd.arg("--datadir").arg(&data);
    cmd.arg("--load-settings").arg(join(&[&presets.0, &presets.1]));
    cmd.arg("--load-filaments").arg(join(&presets.2.iter().collect::<Vec<_>>()));
    cmd.args(["--nozzle-volume-type", "Standard,Standard", "--curr-bed-type", BED, "--filament-map-mode", "Manual", "--filament-map", &map]);
    cmd.arg("--outputdir").arg(&out);
    cmd.arg("--export-settings").arg(out.join("resolved_settings.json"));
    cmd.args(["--export-3mf", &format!("{}.gcode.3mf", case.name)]);
    if models.len() > 1 {
        cmd.args(["--load-filament-ids", &(1..=models.len()).map(|i| i.to_string()).collect::<Vec<_>>().join(","), "--allow-multicolor-oneplate", "--arrange", "1"]);
    }
    cmd.args(["--slice", "0"]);
    for m in models {
        cmd.arg(m);
    }
    let t0 = std::time::Instant::now();
    let o = cmd.output().map_err(|e| format!("{exe}: {e}"))?;
    std::fs::write(out.join("stdout.txt"), &o.stdout).ok();
    std::fs::write(out.join("stderr.txt"), &o.stderr).ok();
    let pkg = out.join(format!("{}.gcode.3mf", case.name));
    let bytes = std::fs::read(&pkg).map_err(|_| format!("the reference slice failed (exit {:?}, {:.1} s): no {}", o.status.code(), t0.elapsed().as_secs_f64(), pkg.display()))?;
    let mut cur = std::io::Cursor::new(&bytes[..]);
    let zdir = makepad_zip_file::zip_read_central_directory(&mut cur).map_err(|e| format!("{}: {e:?}", pkg.display()))?;
    let h = zdir.file_headers.iter().find(|h| h.file_name == "Metadata/plate_1.gcode").ok_or("the reference package has no plate G-code")?;
    let gcode = h.extract(&mut cur).map_err(|e| format!("{e:?}"))?;
    let path = out.join("plate_1.gcode");
    std::fs::write(&path, gcode).map_err(|e| e.to_string())?;
    Ok(path)
}

/// Object centres the reference placed the parts at (its run summary).
fn reference_centres(dir: &Path) -> Vec<(f64, f64)> {
    let Ok(bytes) = std::fs::read(dir.join("result.json")) else { return Vec::new() };
    let Ok(v) = makepad_strict_json::parse_depth(&bytes, 32) else { return Vec::new() };
    let num = |o: &makepad_strict_json::Value, k: &str| -> f64 {
        match o.get(k) {
            Some(makepad_strict_json::Value::F64(f)) => *f,
            Some(makepad_strict_json::Value::Int(i)) => *i as f64,
            _ => 0.0,
        }
    };
    let mut out = Vec::new();
    for p in v.get("sliced_plates").and_then(|p| p.as_arr()).unwrap_or(&[]) {
        for o in p.get("objects").and_then(|o| o.as_arr()).unwrap_or(&[]) {
            if let Some(b) = o.get("bbox") {
                out.push((num(b, "x") + num(b, "width") / 2.0, num(b, "y") + num(b, "depth") / 2.0));
            }
        }
    }
    out
}

/// Slice a prepared case with Fab, from the same preset files, the parts where the
/// reference placed them, the prime tower where it put it.
fn run_fab(dir: &Path, case: &Case, presets: &(PathBuf, PathBuf, Vec<PathBuf>), models: &[PathBuf], own_flush: bool) -> Result<PathBuf, String> {
    let read = |p: &Path| -> Result<Preset, String> { Ok(Preset::from_flat(load_preset_file(p)?)) };
    let mut setup = JobSetup {
        machine: read(&presets.0)?,
        process: read(&presets.1)?,
        filaments: presets.2.iter().map(|p| read(p)).collect::<Result<_, _>>()?,
        filament_map: case.map.to_vec(),
        nozzle_volume_type: vec!["Standard".into(), "Standard".into()],
        bed_type: BED.into(),
        filament_map_mode: "Manual".into(),
        filament_colours: vec![None; case.filaments.len()],
    };
    makepad_fab_settings::family::of_profile(&setup.machine.values)?;
    let ref_dir = dir.join("ref").join(case.name);
    // Where the reference put the prime tower is part of the plate layout, not of the slice.
    // Its purge matrix is a job input too: its command line does not derive one from the
    // colours as Fab does (`--own-flush` keeps Fab's).
    if let Ok(text) = std::fs::read_to_string(ref_dir.join("plate_1.gcode")) {
        let keys: &[&str] = if own_flush || case.filaments.len() < 2 { &["wipe_tower_x", "wipe_tower_y"] } else { &["wipe_tower_x", "wipe_tower_y", "flush_volumes_matrix", "flush_multiplier"] };
        for &key in keys {
            let tag = format!("; {key} = ");
            if let Some(v) = text.lines().find_map(|l| l.strip_prefix(tag.as_str())) {
                if key.starts_with("flush_") {
                    setup.process.values.set_vec(key, v.trim().split(',').map(|x| x.trim().to_string()).collect());
                } else {
                    setup.process.values.set_str(key, v.trim());
                }
            }
        }
    }
    let centres = reference_centres(&ref_dir);
    let mut objects = Vec::new();
    for (i, m) in models.iter().enumerate() {
        let model = load_model(m.to_str().unwrap_or(""))?;
        for o in model.objects {
            let mut mesh = o.mesh;
            let (cx, cy) = centres.get(i).copied().unwrap_or((175.0 + 30.0 * i as f64, 160.0));
            place(&mut mesh, cx, cy);
            objects.push(PlacedObject { name: m.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default(), mesh, filament: i.min(case.filaments.len() - 1) });
        }
    }
    let res = slice(&setup, objects)?;
    let out = dir.join("fab").join(case.name);
    std::fs::create_dir_all(&out).map_err(|e| e.to_string())?;
    let path = out.join("plate_1.gcode");
    std::fs::write(&path, &res.gcode).map_err(|e| e.to_string())?;
    Ok(path)
}

fn compare_case(dir: &Path, case: &Case) -> Result<String, String> {
    let read = |p: PathBuf| std::fs::read_to_string(&p).map_err(|e| format!("{}: {e}", p.display()));
    let ours = parity::read(&read(dir.join("fab").join(case.name).join("plate_1.gcode"))?);
    let r = parity::read(&read(dir.join("ref").join(case.name).join("plate_1.gcode"))?);
    Ok(parity::report(case.name, &ours, &r))
}

fn main() {
    let v: Vec<String> = std::env::args().skip(1).collect();
    let a = Args(v.clone());
    let cmd = v.first().map(|s| s.as_str()).unwrap_or("");
    if cmd == "cases" {
        for c in CASES {
            println!("{}: {} | {} | filaments {:?} | nozzles {:?}", c.name, match c.part { Part::Cube => "20 mm cube", Part::Bracket => "bracket", Part::TwoColour => "two cubes" }, c.process, c.filaments.iter().map(|f| f.0).collect::<Vec<_>>(), c.map);
        }
        return;
    }
    if cmd != "run" && cmd != "fab" {
        eprintln!("usage: fab-parity run --out DIR [--reference EXE] [--profiles DIR] [--printer NAME] [--only CASE] [--own-flush]\n       fab-parity fab --out DIR [--only CASE] [--own-flush]\n       fab-parity cases");
        std::process::exit(2);
    }
    let Some(dir) = a.one("--out").map(PathBuf::from) else {
        eprintln!("fab-parity: --out DIR missing");
        std::process::exit(2);
    };
    let only = a.one("--only");
    let printer = a.one("--printer").unwrap_or_else(|| PRINTER.into());
    let store = if cmd == "run" {
        let data = a.one("--profiles").map(PathBuf::from).or_else(ProfileStore::default_data_dir).unwrap_or_default();
        Some(ProfileStore::open(std::slice::from_ref(&data), &[]))
    } else {
        None
    };
    let reference = a.one("--reference");
    let own_flush = v.iter().any(|x| x == "--own-flush");
    let mut report = String::new();
    let mut failed = 0;
    for case in CASES.iter().filter(|c| only.as_deref().map_or(true, |o| o == c.name)) {
        let r = (|| -> Result<String, String> {
            let models = models(&dir, case.part)?;
            let inputs = dir.join("inputs").join(case.name);
            let presets = match &store {
                Some(store) => prepare(store, &dir, case, &printer)?,
                None => (inputs.join("machine.json"), inputs.join("process.json"), (1..=case.filaments.len()).map(|i| inputs.join(format!("filament_{i}.json"))).collect()),
            };
            if let (Some(exe), true) = (&reference, cmd == "run") {
                run_reference(exe, &dir, case, &presets, &models)?;
            }
            run_fab(&dir, case, &presets, &models, own_flush)?;
            compare_case(&dir, case)
        })();
        match r {
            Ok(s) => {
                print!("{s}");
                report.push_str(&s);
            }
            Err(e) => {
                failed += 1;
                let s = format!("== {}\nfailed: {e}\n", case.name);
                print!("{s}");
                report.push_str(&s);
            }
        }
    }
    std::fs::write(dir.join("report.txt"), &report).ok();
    if failed > 0 {
        std::process::exit(1);
    }
}
