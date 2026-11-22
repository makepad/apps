//! fab-gate: the print safety check on a sliced file, with the printer profile taken from flat
//! preset files, as the app takes it from the installed system presets.
//!
//!   fab-gate --machine M.json [--filament F.json]... [--tree FOLDER] --gcode FILE
//!
//! A plain G-code file is checked as the job its family prints (the second family's whole
//! check, the default family's G-code check). Prints the facts or every refusal; exits 0 when
//! the file passes, 1 when it is refused, 2 on a usage or input error.

use fab_printer_link::safety::{self, MachineProfile};
use makepad_fab_settings::family::Flavor;
use makepad_fab_settings::profiles::{load_preset_file, Preset};
use std::path::Path;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    std::process::exit(match run(&args) {
        Ok(true) => 0,
        Ok(false) => 1,
        Err(e) => {
            eprintln!("fab-gate: {e}");
            2
        }
    });
}

fn run(args: &[String]) -> Result<bool, String> {
    let all = |k: &str| args.windows(2).filter(|w| w[0] == k).map(|w| w[1].clone()).collect::<Vec<_>>();
    let one = |k: &str| all(k).into_iter().next();
    let tree = one("--tree").unwrap_or_default();
    let preset = |p: &str| -> Result<Preset, String> { Ok(Preset::from_flat(load_preset_file(Path::new(p))?).read_from(&tree)) };
    let machine = preset(&one("--machine").ok_or("--machine missing")?)?;
    let filaments = all("--filament").iter().map(|p| preset(p)).collect::<Result<Vec<_>, _>>()?;
    let path = one("--gcode").ok_or("--gcode missing")?;
    let bytes = std::fs::read(&path).map_err(|e| format!("{path}: {e}"))?;
    // The machine's own value, else the filaments' first values (as the app builds it).
    let get = |k: &str| {
        let m = machine.values.get(k).map(|v| v.as_vec()).unwrap_or_default();
        if !m.is_empty() {
            return m;
        }
        filaments.iter().filter_map(|f| f.values.get(k).map(|v| v.as_vec()).and_then(|v| v.into_iter().next())).collect()
    };
    let templates = filaments.iter().flat_map(|f| ["filament_start_gcode", "filament_end_gcode"].into_iter().map(move |k| (k.to_string(), f.values.raw(k, 0).to_string()))).collect();
    let profile = MachineProfile::from_settings(&get, templates).map_err(|e| e.message())?;
    let name = Path::new(&path).file_name().and_then(|n| n.to_str()).unwrap_or("job.gcode").to_string();
    let facts = match profile.flavor {
        Flavor::Named => safety::validate_gcode(bytes, &profile, &name).map(|job| job.facts().clone()),
        Flavor::Mask => safety::check_gcode(&String::from_utf8(bytes).map_err(|_| "not UTF-8")?, &profile),
    };
    match facts {
        Ok(facts) => {
            println!("passed: {facts:?}");
            Ok(true)
        }
        Err(refusal) => {
            for v in &refusal.0 {
                println!("line {}: {}: {}", v.line, v.rule, v.detail);
            }
            Ok(false)
        }
    }
}
