//! `fab-slice`: slice models to a printer package from the command line,
//! and compare against reference slices.
//!
//! fab-slice slice --model a.stl [--model b.stl] --machine NAME|FILE --process NAME|FILE
//!                 --filament NAME|FILE [--filament ...] [--map 1,2] [--object-filament 1,2]
//!                 [--at X,Y ...] [--bed "Textured PEI Plate"] [--nozzle-volume Standard,Standard]
//!                 [--profiles DIR] --out plate.gcode.3mf [--gcode plate.gcode] [--records plate.fabrec]
//! fab-slice golden CASE --golden DIR [--out DIR] [--layer N] [--layer-times] [--own-flush] [--installed DIR]
//! fab-slice estimate FILE.gcode --machine ... --process ... --filament ...
//! fab-slice import FILE [--profiles DIR] [--into DIR]
//! fab-slice fab --doc FILE.fab.splash|FILE.cad.splash --machine P --process P --filament P [--filament ...]
//!               [--map 1,2,..] [--colors NAME,NAME,..] [--flat [--tree DIR]] [--set k=v]
//!               [--documents DIR] [--preview picture.png] --out plate.gcode.3mf [--gcode plate.gcode]

use makepad_fab_settings::profiles::{load_preset_file, JobSetup, Preset, ProfileStore};
use makepad_fab_settings::machine::Job;
use makepad_fab_slice::{golden, place, slice, PlacedObject};
use makepad_fab_slicer::mesh::load_model;
use std::path::{Path, PathBuf};

fn usage() -> ! {
    eprintln!("usage:\n  fab-slice slice --model FILE [--model FILE] --machine P --process P --filament P [--filament P] [--map 1,2] [--object-filament 1,2] [--at X,Y] [--bed NAME] [--nozzle-volume A,B] [--profiles DIR] [--flat [--tree DIR]] --out FILE.gcode.3mf [--gcode FILE] [--records FILE]\n  fab-slice golden CASE --golden DIR [--out DIR] [--layer N]\n  fab-slice estimate FILE.gcode --machine P --process P --filament P\n  fab-slice import FILE [--profiles DIR] [--into DIR]\n  fab-slice fab --doc FILE.fab.splash|FILE.cad.splash --machine P --process P --filament P [--filament P] [--map 1,2] [--colors A,B] [--flat [--tree DIR]] [--documents DIR] [--preview FILE.png] --out FILE.gcode.3mf [--gcode FILE]");
    std::process::exit(2)
}

struct Args {
    v: Vec<String>,
}

impl Args {
    fn all(&self, flag: &str) -> Vec<String> {
        let mut out = Vec::new();
        let mut i = 0;
        while i < self.v.len() {
            if self.v[i] == flag && i + 1 < self.v.len() {
                out.push(self.v[i + 1].clone());
                i += 2;
            } else {
                i += 1;
            }
        }
        out
    }
    fn one(&self, flag: &str) -> Option<String> {
        self.all(flag).into_iter().last()
    }
}

fn preset(store: Option<&ProfileStore>, spec: &str) -> Result<Preset, String> {
    let p = Path::new(spec);
    if p.extension().map(|e| e == "json").unwrap_or(false) && p.exists() {
        let mut raw = load_preset_file(p)?;
        if raw.inherits.is_empty() {
            return Ok(Preset::from_flat(raw));
        }
        let store = store.ok_or("a preset file with a parent needs --profiles")?;
        // Resolve the parent chain from the store, then apply the file.
        let mut parent = store.resolve(&raw.inherits)?;
        // The file's keys read as its parent's slicer names them, then land on the parent's.
        makepad_fab_settings::trees::read_names(&parent.tree, &mut raw.values);
        makepad_fab_settings::profiles::merge_child(&mut parent.values, &raw.values);
        parent.system_parent = if parent.system_parent.is_empty() { raw.inherits.clone() } else { parent.system_parent };
        parent.name = raw.name;
        if !raw.filament_id.is_empty() {
            parent.filament_id = raw.filament_id;
        }
        return Ok(parent);
    }
    store.ok_or_else(|| format!("{spec}: not a file, and no profile store"))?.resolve(spec)
}

fn csv_u32(s: &str) -> Vec<u32> {
    s.split(',').filter_map(|x| x.trim().parse().ok()).collect()
}

fn setup_from(store: Option<&ProfileStore>, a: &Args) -> Result<JobSetup, String> {
    // `--flat`: preset files taken as complete (the golden inputs), parents ignored.
    let flat = a.v.iter().any(|x| x == "--flat");
    // `--tree DIR`: the slicer data folder name the flat files come from, whose defaults fill
    // what they leave out.
    let tree = a.one("--tree").unwrap_or_default();
    if !tree.is_empty() && !makepad_fab_settings::trees::is_known(&tree) {
        return Err(format!("--tree {tree}: not a slicer data folder Fab knows the defaults of"));
    }
    let preset = |store: Option<&ProfileStore>, spec: &str| -> Result<Preset, String> {
        if flat {
            return Ok(Preset::from_flat(load_preset_file(Path::new(spec))?).read_from(&tree));
        }
        preset(store, spec)
    };
    let machine = preset(store, &a.one("--machine").ok_or("--machine missing")?)?;
    makepad_fab_settings::family::of_profile(&machine.values)?;
    let process = preset(store, &a.one("--process").ok_or("--process missing")?)?;
    let mut filaments = Vec::new();
    for f in a.all("--filament") {
        filaments.push(preset(store, &f)?);
    }
    if filaments.is_empty() {
        return Err("--filament missing".into());
    }
    let nf = filaments.len();
    let map = a.one("--map").map(|s| csv_u32(&s)).unwrap_or_else(|| vec![1; nf]);
    let nv: Vec<String> = a.one("--nozzle-volume").map(|s| s.split(',').map(|x| x.trim().to_string()).collect()).unwrap_or_else(|| vec!["Standard".into(), "Standard".into()]);
    Ok(JobSetup {
        machine,
        process,
        filaments,
        filament_map: map,
        nozzle_volume_type: nv,
        bed_type: a.one("--bed").unwrap_or_else(|| "Textured PEI Plate".into()),
        filament_map_mode: "Manual".into(),
        filament_colours: vec![None; nf],
    })
}

fn cmd_slice(a: &Args) -> Result<(), String> {
    let data = a.one("--profiles").map(PathBuf::from).or_else(ProfileStore::default_data_dir);
    let store = data.map(|d| ProfileStore::open(&[d], &[]));
    let mut setup = setup_from(store.as_ref(), a)?;
    apply_sets(&mut setup, a);
    let models = a.all("--model");
    if models.is_empty() {
        return Err("--model missing".into());
    }
    let ofil = a.one("--object-filament").map(|s| csv_u32(&s)).unwrap_or_default();
    let at: Vec<(f64, f64)> = a.all("--at").iter().filter_map(|s| s.split_once(',').and_then(|(x, y)| Some((x.trim().parse().ok()?, y.trim().parse().ok()?)))).collect();
    let mut objects = Vec::new();
    let mut k = 0;
    for m in &models {
        let model = load_model(m)?;
        for o in model.objects {
            let mut mesh = o.mesh;
            let (cx, cy) = at.get(k).copied().unwrap_or((175.0, 160.0 + k as f64 * 25.0));
            if at.get(k).is_some() || m.to_ascii_lowercase().ends_with(".stl") {
                place(&mut mesh, cx, cy);
            }
            let fil = ofil.get(k).copied().map(|f| f.max(1) as usize - 1).unwrap_or(if o.extruder > 0 { o.extruder as usize - 1 } else { 0 });
            objects.push(PlacedObject { name: if o.name.is_empty() { m.clone() } else { o.name }, mesh, filament: fil });
            k += 1;
        }
    }
    let t0 = std::time::Instant::now();
    let res = slice(&setup, objects)?;
    let out = a.one("--out").ok_or("--out missing")?;
    std::fs::write(&out, res.package()?).map_err(|e| format!("{out}: {e}"))?;
    if let Some(r) = a.one("--records") {
        std::fs::write(&r, makepad_fab_gcode::record::encode_records(&res.timeline.flat_moves(), &res.timeline.nozzle_temps)).map_err(|e| format!("{r}: {e}"))?;
    }
    if let Some(g) = a.one("--gcode") {
        std::fs::write(&g, &res.gcode).map_err(|e| format!("{g}: {e}"))?;
    }
    println!("sliced in {:.2} s: {} layers, {:.0} s estimated ({:.0} s model), {:.2} g, {} records -> {out}", t0.elapsed().as_secs_f64(), res.stats.layers, res.stats.total_time, res.stats.model_time, res.stats.filament_g.iter().sum::<f64>(), res.timeline.move_count());
    Ok(())
}

struct Case {
    name: &'static str,
    models: &'static [&'static str],
    process: &'static str,
    filaments: &'static [&'static str],
    map: &'static [u32],
}

const CASES: &[Case] = &[
    Case { name: "cube_left", models: &["cube20.stl"], process: "process_0.20_standard.json", filaments: &["filament_generic_pla.json"], map: &[1] },
    Case { name: "cube_right", models: &["cube20.stl"], process: "process_0.20_standard.json", filaments: &["filament_generic_pla.json"], map: &[2] },
    Case { name: "cube_right_pla_hs", models: &["cube20.stl"], process: "process_0.20_standard.json", filaments: &["filament_generic_pla_hs.json"], map: &[2] },
    Case { name: "two_obj_dual_nozzle", models: &["cube20.stl", "cube20.stl"], process: "process_0.20_standard.json", filaments: &["filament_generic_pla_white.json", "filament_bambu_pla_basic_red.json"], map: &[1, 2] },
    Case { name: "two_obj_same_nozzle_left", models: &["cube20.stl", "cube20.stl"], process: "process_0.20_standard.json", filaments: &["filament_generic_pla_white.json", "filament_bambu_pla_basic_red.json"], map: &[1, 1] },
    Case { name: "two_obj_pla_petg_dual_nozzle", models: &["cube20.stl", "cube20.stl"], process: "process_0.20_standard.json", filaments: &["filament_generic_pla_white.json", "filament_generic_petg_black.json"], map: &[1, 2] },
    Case { name: "two_obj_dual_nozzle_3mf", models: &["two_obj.3mf"], process: "process_0.20_standard_tower165.json", filaments: &["filament_generic_pla_white.json", "filament_bambu_pla_basic_red.json"], map: &[1, 2] },
    Case { name: "painted_dual_nozzle", models: &["painted.3mf"], process: "process_0.20_standard_tower165.json", filaments: &["filament_generic_pla_white.json", "filament_bambu_pla_basic_red.json"], map: &[1, 2] },
    Case { name: "painted_same_nozzle_right", models: &["painted.3mf"], process: "process_0.20_standard_tower165.json", filaments: &["filament_generic_pla_white.json", "filament_bambu_pla_basic_red.json"], map: &[2, 2] },
    Case { name: "overhang_tree_organic", models: &["overhang_t.stl"], process: "process_tree_organic.json", filaments: &["filament_generic_pla.json"], map: &[1] },
    Case { name: "overhang_normal_grid", models: &["overhang_t.stl"], process: "process_normal_grid.json", filaments: &["filament_generic_pla.json"], map: &[1] },
];

/// `--set key=value` (repeatable): override a process setting, e.g. an
/// experimental mode (`--set fab_wall_texture=knurl`).
fn apply_sets(setup: &mut JobSetup, a: &Args) {
    for kv in a.all("--set") {
        if let Some((k, v)) = kv.split_once('=') {
            setup.process.values.set_str(k.trim(), v.trim());
        }
    }
}

/// The reference run's folder, from the required `--golden DIR`. There is no
/// default: the reference files are not part of the repository.
fn golden_root(a: &Args) -> Result<PathBuf, String> {
    a.one("--golden").map(PathBuf::from).ok_or_else(|| "missing --golden DIR: the folder holding the reference run (inputs/presets/, the reference gcode and result.json)".to_string())
}

/// Object centres from the reference run's summary (`result.json`).
fn reference_centres(dir: &Path) -> Vec<(f64, f64)> {
    let Ok(bytes) = std::fs::read(dir.join("result.json")) else { return Vec::new() };
    let Ok(v) = makepad_strict_json::parse_depth(&bytes, 32) else { return Vec::new() };
    let mut out = Vec::new();
    let num = |o: &makepad_strict_json::Value, k: &str| -> f64 {
        match o.get(k) {
            Some(makepad_strict_json::Value::F64(f)) => *f,
            Some(makepad_strict_json::Value::Int(i)) => *i as f64,
            _ => 0.0,
        }
    };
    if let Some(plates) = v.get("sliced_plates").and_then(|p| p.as_arr()) {
        for p in plates {
            if let Some(objs) = p.get("objects").and_then(|o| o.as_arr()) {
                for o in objs {
                    if let Some(b) = o.get("bbox") {
                        out.push((num(b, "x") + num(b, "width") / 2.0, num(b, "y") + num(b, "depth") / 2.0));
                    }
                }
            }
        }
    }
    out
}

fn cmd_golden(a: &Args, name: &str) -> Result<(), String> {
    let case = CASES.iter().find(|c| c.name == name).ok_or_else(|| format!("unknown case {name}; cases: {}", CASES.iter().map(|c| c.name).collect::<Vec<_>>().join(", ")))?;
    let root = golden_root(a)?;
    let pre = |f: &str| -> Result<Preset, String> { Ok(Preset::from_flat(load_preset_file(&root.join("inputs/presets").join(f))?)) };
    // `--installed DIR`: the job from the user's installed profiles (their
    // selected printer, process and first filaments) instead of the
    // reference run's preset files.
    let setup = match a.one("--installed") {
        Some(dir) => makepad_fab_slice::installed_setup_in(Path::new(&dir), case.map)?,
        None => JobSetup {
            machine: pre("machine_H2D_0.4.json")?,
            process: pre(case.process)?,
            filaments: case.filaments.iter().map(|f| pre(f)).collect::<Result<_, _>>()?,
            filament_map: case.map.to_vec(),
            nozzle_volume_type: vec!["Standard".into(), "Standard".into()],
            bed_type: "Textured PEI Plate".into(),
            filament_map_mode: "Manual".into(),
            filament_colours: vec![None; case.filaments.len()],
        },
    };
    let ref_dir = root.join("slices").join(case.name);
    let centres = reference_centres(&ref_dir);
    // The reference run's plate placement of the prime tower is part of the
    // job (its command line places it for model files without a project).
    let mut setup = setup;
    makepad_fab_settings::family::of_profile(&setup.machine.values)?;
    apply_sets(&mut setup, a);
    if let Ok(text) = std::fs::read_to_string(ref_dir.join("plate_1.gcode")) {
        // The purge matrix too (Fab's colour model is compared with
        // `--own-flush`): the reference's purges are a job input here.
        let own_flush = a.v.iter().any(|x| x == "--own-flush");
        let keys: &[&str] = if own_flush || case.filaments.len() < 2 { &["wipe_tower_x", "wipe_tower_y"] } else { &["wipe_tower_x", "wipe_tower_y", "flush_volumes_matrix", "flush_multiplier", "flush_multiplier_fast"] };
        for key in keys.iter().copied() {
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
    let mut objects = Vec::new();
    let mut k = 0;
    for (i, m) in case.models.iter().enumerate() {
        let model = load_model(root.join("inputs/models").join(m).to_str().unwrap_or(""))?;
        for o in model.objects {
            let mut mesh = o.mesh;
            let (cx, cy) = centres.get(k).copied().unwrap_or((175.0, 160.0));
            place(&mut mesh, cx, cy);
            let filament = if o.extruder > 0 { o.extruder as usize - 1 } else { i }.min(case.filaments.len() - 1);
            objects.push(PlacedObject { name: m.to_string(), mesh, filament });
            k += 1;
        }
    }
    let t0 = std::time::Instant::now();
    let res = slice(&setup, objects)?;
    let dt = t0.elapsed().as_secs_f64();
    let out_dir = a.one("--out").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    let _ = std::fs::create_dir_all(&out_dir);
    let gpath = out_dir.join(format!("{}.gcode", case.name));
    std::fs::write(&gpath, &res.gcode).map_err(|e| e.to_string())?;
    std::fs::write(out_dir.join(format!("{}.gcode.3mf", case.name)), res.package()?).map_err(|e| e.to_string())?;
    let gold_text = std::fs::read_to_string(ref_dir.join("plate_1.gcode")).map_err(|e| format!("reference: {e}"))?;
    let ours = golden::analyse(&res.gcode);
    let gold = golden::analyse(&gold_text);
    println!("case {} (sliced in {:.2} s, {} records)", case.name, dt, res.timeline.move_count());
    print!("{}", golden::compare(&ours, &gold));
    if let Some(l) = a.one("--layer").and_then(|s| s.parse::<usize>().ok()) {
        print!("{}", golden::layer_detail(&ours, &gold, l.saturating_sub(1)));
    }
    // Time per feature against the reference run's own summary.
    if let Ok(bytes) = std::fs::read(ref_dir.join("result.json")) {
        if let Ok(v) = makepad_strict_json::parse_depth(&bytes, 32) {
            if let Some(ft) = v.get("sliced_plates").and_then(|p| p.as_arr()).and_then(|p| p.first()).and_then(|p| p.get("feature_type_times")) {
                let mut line = String::from("feature times s (ours/ref):");
                // The reference's summary has no rows of a head switch or a filament change:
                // ours are compared within its Flush.
                let mut times = res.timeline.role_times.clone();
                for c in res.timeline.change_rows() {
                    times.remove(c.name);
                    *times.entry("Flush".to_string()).or_insert(0.0) += c.seconds;
                }
                for (k, ours) in &times {
                    let r = match ft.get(k) {
                        Some(makepad_strict_json::Value::F64(f)) => *f,
                        Some(makepad_strict_json::Value::Int(i)) => *i as f64,
                        _ => continue,
                    };
                    line.push_str(&format!(" {k} {ours:.0}/{r:.0};"));
                }
                println!("{line}");
            }
        }
    }
    if a.v.iter().any(|x| x == "--layer-times") {
        let times = |text: &str| -> Vec<f64> {
            let mut p = makepad_fab_gcode::process::Processor::new(&res.job);
            for l in text.lines() {
                p.line(l);
            }
            let tl = p.finish();
            let idx = makepad_fab_gcode::record::layer_index(&tl.flat_moves());
            (0..idx.len()).map(|i| idx.get(i + 1).map_or(tl.total, |n| n.t_start) - idx[i].t_start).collect()
        };
        let (o, g) = (times(&res.gcode), times(&gold_text));
        println!("layer times (s), ours vs ref, layers differing by more than 0.5 s:");
        for i in 0..o.len().max(g.len()) {
            let (x, y) = (o.get(i).copied().unwrap_or(0.0), g.get(i).copied().unwrap_or(0.0));
            if (x - y).abs() > 0.5 {
                println!("  layer {}: {:.1} vs {:.1} ({:+.1})", i + 1, x, y, x - y);
            }
        }
    }
    println!("ours: {}", gpath.display());
    Ok(())
}

fn cmd_estimate(a: &Args, file: &str) -> Result<(), String> {
    let data = a.one("--profiles").map(PathBuf::from).or_else(ProfileStore::default_data_dir);
    let store = data.map(|d| ProfileStore::open(&[d], &[]));
    let setup = setup_from(store.as_ref(), a)?;
    let job = Job::new(setup.build_config());
    let text = std::fs::read_to_string(file).map_err(|e| format!("{file}: {e}"))?;
    let mut p = makepad_fab_gcode::process::Processor::new(&job);
    for l in text.lines() {
        p.line(l);
    }
    let tl = p.finish();
    println!("total {:.1} s, prepare {:.1} s, model {:.1} s, {} records", tl.total, tl.prepare, tl.total - tl.prepare, tl.move_count());
    let changes = tl.change_rows();
    for (k, v) in &tl.role_times {
        match changes.iter().find(|c| c.name == k) {
            Some(c) => println!("  {k}: {v:.1} s ({})", c.detail),
            None => println!("  {k}: {v:.1} s"),
        }
    }
    if let Some(f) = a.one("--line-times") {
        let mut o = String::new();
        for m in &tl.flat_moves() {
            o.push_str(&format!("{} {:.4} {:.4} {}\n", m.gcode_line, m.t_start, m.t_end, m.kind as u8));
        }
        std::fs::write(&f, o).map_err(|e| format!("{f}: {e}"))?;
    }
    if a.v.iter().any(|x| x == "--layers") {
        let idx = makepad_fab_gcode::record::layer_index(&tl.flat_moves());
        for (i, l) in idx.iter().enumerate() {
            let end = idx.get(i + 1).map_or(tl.total, |n| n.t_start);
            println!("  layer {}: {:.3} s", l.layer, end - l.t_start);
        }
    }
    Ok(())
}

/// `fab-slice bench --golden DIR --model FILE [--model FILE] [--process FILE.json] [--scale S] [--repeat N]`:
/// stage times of a full slice of the models (placed side by side) with the
/// reference machine and filament presets, plus the printability passes.
fn cmd_bench(a: &Args) -> Result<(), String> {
    use makepad_fab_toolpath::prof;
    let root = golden_root(a);
    let pre = |f: &str| -> Result<Preset, String> {
        let p = Path::new(f);
        let path = if p.exists() { p.to_path_buf() } else { root.clone()?.join("inputs/presets").join(f) };
        Ok(Preset::from_flat(load_preset_file(&path)?))
    };
    let process = a.one("--process").unwrap_or_else(|| "process_0.20_standard.json".into());
    // `--filaments a.json,b.json` (default generic PLA), `--map 1,2` (1-based nozzles).
    let fil_names: Vec<String> = a.one("--filaments").map(|s| s.split(',').map(|x| x.trim().to_string()).collect()).unwrap_or_else(|| vec!["filament_generic_pla.json".into()]);
    let filaments = fil_names.iter().map(|f| pre(f)).collect::<Result<Vec<_>, _>>()?;
    let nf = filaments.len();
    let map = a.one("--map").map(|s| csv_u32(&s)).filter(|m| m.len() == nf).unwrap_or_else(|| vec![1; nf]);
    let mut setup = JobSetup {
        machine: pre("machine_H2D_0.4.json")?,
        process: pre(&process)?,
        filaments,
        filament_map: map,
        nozzle_volume_type: vec!["Standard".into(), "Standard".into()],
        bed_type: "Textured PEI Plate".into(),
        filament_map_mode: "Manual".into(),
        filament_colours: vec![None; nf],
    };
    makepad_fab_settings::family::of_profile(&setup.machine.values)?;
    apply_sets(&mut setup, a);
    if let Some(t) = a.one("--tower") {
        if let Some((x, y)) = t.split_once(',') {
            setup.process.values.set_vec("wipe_tower_x", vec![x.trim().to_string()]);
            setup.process.values.set_vec("wipe_tower_y", vec![y.trim().to_string()]);
        }
    }
    let scale: f64 = a.one("--scale").and_then(|s| s.parse().ok()).unwrap_or(1.0);
    let repeat: usize = a.one("--repeat").and_then(|s| s.parse().ok()).unwrap_or(1).max(1);
    let threads: Vec<usize> = a.one("--threads").map(|s| s.split(',').filter_map(|x| x.trim().parse().ok()).collect()).unwrap_or_else(|| vec![1]);
    let mut meshes = Vec::new();
    let mut tris = 0;
    for m in a.all("--model") {
        for o in load_model(&m)?.objects {
            let filament = if o.extruder > 0 { (o.extruder as usize - 1).min(nf - 1) } else { 0 };
            let mut mesh = o.mesh;
            if scale != 1.0 {
                for v in &mut mesh.verts {
                    for c in v.iter_mut() {
                        *c *= scale;
                    }
                }
            }
            tris += mesh.tris.len();
            meshes.push((m.clone(), mesh, filament));
        }
    }
    if meshes.is_empty() {
        return Err("--model missing".into());
    }
    // In rows along X (8 to a row, 10 mm apart).
    let (mut x, mut y, mut row_d) = (20.0, 20.0, 0.0f64);
    let mut placed = Vec::new();
    for (k, (name, mut mesh, filament)) in meshes.into_iter().enumerate() {
        let (lo, hi) = mesh.bbox();
        let (w, d) = (hi[0] - lo[0], hi[1] - lo[1]);
        if k > 0 && k % 8 == 0 {
            x = 20.0;
            y += row_d + 10.0;
            row_d = 0.0;
        }
        place(&mut mesh, x + w / 2.0, y + d / 2.0);
        x += w + 10.0;
        row_d = row_d.max(d);
        placed.push(PlacedObject { name: format!("{name}#{k}"), mesh, filament });
    }
    let plate = fab_plate::Plate {
        excluded: Vec::new(), settings: Vec::new(),
        parts: placed.iter().map(|o| fab_plate::Part { body_of: None, print: Vec::new(), print_mesh: None, paint: None, object: o.name.clone(), name: o.name.clone(), filament: o.filament as u32 + 1, color: [1.0; 4], nozzle: None, mesh: fab_plate::PartMesh { positions: o.mesh.verts.clone(), triangles: o.mesh.tris.clone() }, field_bounds: None, field_floor: None, ..Default::default() }).collect(),
    };
    let b = plate.bounds();
    let (lo, hi) = (b.min, b.max);
    println!("bench: {} objects, {} triangles, plate {:.1}x{:.1}x{:.1} mm (from {:.1},{:.1})", placed.len(), tris, hi[0] - lo[0], hi[1] - lo[1], hi[2] - lo[2], lo[0], lo[1]);
    prof::enable(true);
    for (r, threads) in threads.iter().flat_map(|&t| std::iter::repeat(t).take(repeat)).enumerate() {
        makepad_fab_toolpath::par::set_threads(threads);
        let objects: Vec<PlacedObject> = placed.iter().map(|o| PlacedObject { name: o.name.clone(), mesh: o.mesh.clone(), filament: o.filament }).collect();
        prof::take();
        let marks = std::sync::Mutex::new(Vec::<(std::time::Instant, String)>::new());
        let t0 = std::time::Instant::now();
        let res = makepad_fab_slice::slice_with(&setup, objects, &|_, s| marks.lock().unwrap().push((std::time::Instant::now(), s.to_string())))?;
        let t_slice = t0.elapsed().as_secs_f64();
        let stages = prof::take();
        let t1 = std::time::Instant::now();
        let machine = fab_plate::machine::Machine::dual_nozzle_350();
        let materials = fab_printability::Materials::from_job(&res.job);
        let mut report = fab_printability::analyse_plate(&machine, &plate, &materials);
        let t_mesh = t1.elapsed().as_secs_f64();
        let t2 = std::time::Instant::now();
        // `--no-check`: the slice only (its G-code), no sliced printability.
        if !a.v.iter().any(|x| x == "--no-check") {
            fab_printability::analyse_slice(&machine, &plate, &materials, res.layers(), &res.timeline, &mut report);
        }
        let t_sliced = t2.elapsed().as_secs_f64();
        let marks = marks.into_inner().unwrap();
        let checks = prof::take();
        if let Some(path) = a.one("--report") {
            let text: String = report.issues.iter().map(|i| format!("{} {:?} {:?} {:?}-{:?} {:.6} {:.6} [{:.4},{:.4},{:.4}] {}\n", i.kind, i.severity, i.object, i.layer, i.layer_to, i.measure, i.area_mm2, i.at_mm[0], i.at_mm[1], i.at_mm[2], i.message)).collect();
            std::fs::write(&path, text).map_err(|e| format!("{path}: {e}"))?;
        }
        if let Some(g) = a.one("--gcode") {
            std::fs::write(&g, &res.gcode).map_err(|e| format!("{g}: {e}"))?;
        }
        println!("run {} ({threads} threads): slice {:.3} s, {} layers, {} moves, {} gcode bytes; printability mesh {:.3} s, sliced {:.3} s ({} issues)", r + 1, t_slice, res.layers().len(), res.timeline.move_count(), res.gcode.len(), t_mesh, t_sliced, report.issues.len());
        for w in marks.windows(2) {
            println!("  phase {:<28} {:8.3} s", w[0].1, (w[1].0 - w[0].0).as_secs_f64());
        }
        for (s, secs) in stages.into_iter().chain(checks) {
            if secs > 0.0 {
                println!("  stage {:<28} {:8.3} s  ({:.2} ms/layer)", s.name(), secs, secs * 1e3 / res.layers().len().max(1) as f64);
            }
        }
    }
    Ok(())
}

/// `fab-slice bench-edit --golden DIR --model FILE --edit top:DZ|band:Z0:Z1:S [--threads N] [--process P]`:
/// the live engine on one part, then a geometry edit of it: time to the first
/// re-planned layer, to all toolpaths, and to the settled G-code, and how many
/// layers were planned again.
fn cmd_bench_edit(a: &Args) -> Result<(), String> {
    use makepad_fab_slice::live::{LiveEngine, LiveEvent, LiveInput, Sink, Spawn};
    use std::sync::{mpsc, Arc, Mutex};
    let root = golden_root(a);
    let pre = |f: &str| -> Result<Preset, String> {
        let p = Path::new(f);
        let path = if p.exists() { p.to_path_buf() } else { root.clone()?.join("inputs/presets").join(f) };
        Ok(Preset::from_flat(load_preset_file(&path)?))
    };
    let process = a.one("--process").unwrap_or_else(|| "process_0.20_standard.json".into());
    let setup = JobSetup {
        machine: pre("machine_H2D_0.4.json")?,
        process: pre(&process)?,
        filaments: vec![pre("filament_generic_pla.json")?],
        filament_map: vec![1],
        nozzle_volume_type: vec!["Standard".into(), "Standard".into()],
        bed_type: "Textured PEI Plate".into(),
        filament_map_mode: "Manual".into(),
        filament_colours: vec![None],
    };
    makepad_fab_settings::family::of_profile(&setup.machine.values)?;
    let threads: usize = a.one("--threads").and_then(|s| s.parse().ok()).unwrap_or(16);
    makepad_fab_toolpath::par::set_threads(threads);
    let model = a.one("--model").ok_or("--model missing")?;
    let mut mesh = load_model(&model)?.objects.into_iter().next().ok_or("no object")?.mesh;
    place(&mut mesh, 175.0, 160.0);
    let (lo, hi) = mesh.bbox();
    let edit = a.one("--edit").unwrap_or_else(|| "top:5".into());
    let mut edited = mesh.clone();
    let f: Vec<f64> = edit.split(':').skip(1).filter_map(|x| x.parse().ok()).collect();
    let changed_z = if edit.starts_with("top") {
        // The top fifth raised: the part grows taller.
        let from = lo[2] + 0.8 * (hi[2] - lo[2]);
        for v in &mut edited.verts {
            if v[2] > from {
                v[2] += f.first().copied().unwrap_or(5.0);
            }
        }
        (from, hi[2] + f.first().copied().unwrap_or(5.0))
    } else {
        // A band scaled about the part's axis: a local feature resized.
        let (z0, z1, k) = (f.first().copied().unwrap_or(0.0), f.get(1).copied().unwrap_or(5.0), f.get(2).copied().unwrap_or(1.02));
        let c = [(lo[0] + hi[0]) / 2.0, (lo[1] + hi[1]) / 2.0];
        for v in &mut edited.verts {
            if v[2] > z0 && v[2] < z1 {
                v[0] = c[0] + (v[0] - c[0]) * k;
                v[1] = c[1] + (v[1] - c[1]) * k;
            }
        }
        (z0, z1)
    };
    let part = |m: &makepad_fab_slicer::mesh::Mesh| fab_plate::Part { body_of: None, print: Vec::new(), print_mesh: None, paint: None, object: "part".into(), name: "part".into(), filament: 1, color: [1.0; 4], nozzle: None, mesh: fab_plate::PartMesh { positions: m.verts.clone(), triangles: m.tris.clone() }, field_bounds: None, field_floor: None, ..Default::default() };
    let (tx, rx) = mpsc::channel::<(std::time::Instant, LiveEvent)>();
    let tx = Mutex::new(tx);
    let sink: Sink = Arc::new(move |e| {
        let _ = tx.lock().unwrap().send((std::time::Instant::now(), e));
    });
    let spawn: Spawn = Arc::new(|_, job| {
        std::thread::spawn(job);
    });
    let revision = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let next: makepad_fab_slice::live::Next = Arc::new(move || {
        let rev = revision.fetch_add(1, std::sync::atomic::Ordering::AcqRel) + 1;
        let revision = revision.clone();
        Arc::new(move || revision.load(std::sync::atomic::Ordering::Acquire) != rev)
    });
    let engine = LiveEngine::new(spawn, sink, next);
    let input = |id: u64, m: &makepad_fab_slicer::mesh::Mesh| LiveInput::ready(id, 1, setup.clone(), Arc::new(fab_plate::Plate { excluded: Vec::new(), settings: Vec::new(), parts: vec![part(m)] }), Arc::new(Vec::new()));
    let wait_settled = |tag: u64| -> std::time::Instant {
        loop {
            match rx.recv().expect("events") {
                (t, LiveEvent::Settled { tag: g, result, .. }) if g == tag => {
                    if let Err(e) = result {
                        panic!("{e}");
                    }
                    return t;
                }
                _ => {}
            }
        }
    };
    let t0 = std::time::Instant::now();
    engine.settle(1, input(1, &mesh));
    let t = wait_settled(1);
    println!("bench-edit {model}: first slice settled in {:.3} s ({} layers planned)", (t - t0).as_secs_f64(), engine.layers_computed());
    let before = engine.layers_computed();
    engine.set_focus_z(Some((changed_z.0 + changed_z.1) / 2.0));
    let t0 = std::time::Instant::now();
    engine.settle(2, input(2, &edited));
    let (mut first, mut contours, mut toolpaths) = (None, None, None);
    let done = loop {
        match rx.recv().expect("events") {
            (t, LiveEvent::Contours { .. }) => contours = contours.or(Some(t)),
            (t, LiveEvent::PartLayers { .. }) => first = first.or(Some(t)),
            (t, LiveEvent::Toolpaths { .. }) => toolpaths = toolpaths.or(Some(t)),
            (t, LiveEvent::Settled { tag: 2, result, .. }) => {
                result?;
                break t;
            }
            _ => {}
        }
    };
    let s = |t: Option<std::time::Instant>| t.map_or("-".to_string(), |t| format!("{:.3} s", (t - t0).as_secs_f64()));
    println!("bench-edit {edit} (z {:.1}..{:.1}): contours {}, first planned layer {}, toolpaths {}, settled {:.3} s; layers planned again {}", changed_z.0, changed_z.1, s(contours), s(first), s(toolpaths), (done - t0).as_secs_f64(), engine.layers_computed() - before);
    Ok(())
}

/// `fab-slice import FILE [--profiles DIR] [--into DIR]`: import the presets a
/// file holds (into Fab's profile folder, or `--into`) and print the report.
fn cmd_import(a: &Args, file: &str) -> Result<(), String> {
    use makepad_fab_settings::import::{fab_profile_dir, import_into};
    let data = a.one("--profiles").map(PathBuf::from).or_else(ProfileStore::default_data_dir).unwrap_or_default();
    let into = a.one("--into").map(PathBuf::from).unwrap_or_else(fab_profile_dir);
    let store = ProfileStore::open(std::slice::from_ref(&data), &[into.clone()]);
    let batch = import_into(Path::new(file), &store, &into);
    print!("{}", batch.report());
    let store = ProfileStore::open(std::slice::from_ref(&data), &[into]);
    for p in batch.presets.iter().filter(|p| !p.skip) {
        match store.resolve(&p.preset.name) {
            Ok(r) => println!("resolves: {} \"{}\" ({} keys)", r.kind, r.name, r.values.map.len()),
            Err(e) => println!("does not resolve: {e}"),
        }
    }
    Ok(())
}

/// `fab-slice fab`: slice a Fab document (a project, or an object script placed once) with the
/// files beside it (its paint, strokes and pictures) as Fab builds it: its print colours in
/// `--colors` order print with filaments 1, 2, .. (each on the nozzle `--map` gives it), every
/// other with filament 1; its pictures print with the calibrated sets of `--documents` (a Fab
/// documents folder). The parts are laid out on the machine preset's bed. Prints the
/// layers, the time, the grams per filament and the head swaps; `--preview` writes each picture
/// part's source beside the face it prints.
fn cmd_fab(a: &Args) -> Result<(), String> {
    use makepad_fab_slice::fab_doc;
    let data = a.one("--profiles").map(PathBuf::from).or_else(ProfileStore::default_data_dir);
    let store = data.map(|d| ProfileStore::open(&[d], &[]));
    let mut setup = setup_from(store.as_ref(), a)?;
    apply_sets(&mut setup, a);
    let model = fab_doc::load(Path::new(&a.one("--doc").ok_or("--doc missing")?))?;
    // `--documents DIR`: the Fab documents folder whose calibrated filament sets the pictures
    // print with, as in Fab
    let calibrated = match a.one("--documents") {
        Some(dir) => {
            let (sets, unread) = fab_model::colour_sets::load_all(Path::new(&dir));
            for e in unread {
                println!("warning: a calibrated set does not read: {e}");
            }
            std::sync::Arc::new(sets)
        }
        None => Default::default(),
    };
    let (compiled, mut ev, nozzle) = fab_doc::evaluate(&setup, &model, &calibrated)?;
    // `--preview FILE.png`: each picture part's source beside the face it prints, seen near and far
    if let Some(path) = a.one("--preview") {
        let loaded: Vec<(String, [f32; 3])> = compiled.colors.iter().filter_map(|c| Some((c.name.clone(), c.rgb?))).collect();
        let mut n = 0;
        for inst in &compiled.instances {
            for preview in fab_model::picture::previews(&inst.script, &loaded, &calibrated, nozzle, 3) {
                let preview = preview?;
                let file = if n == 0 { path.clone() } else { format!("{}-{n}.png", path.trim_end_matches(".png")) };
                std::fs::write(&file, preview.image.png()?).map_err(|e| format!("{file}: {e}"))?;
                match preview.delta {
                    Some([total, gamut, dither]) => println!("preview -> {file}: ΔE from {} mm (mean, p95) {:.1} {:.1}, gamut {:.1} {:.1}, dither {:.1} {:.1}", fab_colour::view::VIEWING, total.0, total.1, gamut.0, gamut.1, dither.0, dither.1),
                    None => println!("preview -> {file}"),
                }
                n += 1;
            }
        }
    }
    for w in &ev.warnings {
        println!("warning: {w}");
    }
    let colors: Vec<String> = a.one("--colors").map(|s| s.split(',').map(|c| c.trim().to_string()).collect()).unwrap_or_default();
    let t0 = std::time::Instant::now();
    let res = fab_doc::slice(&mut setup, &mut ev, &colors)?;
    let out = a.one("--out").ok_or("--out missing")?;
    std::fs::write(&out, res.package()?).map_err(|e| format!("{out}: {e}"))?;
    if let Some(g) = a.one("--gcode") {
        std::fs::write(&g, &res.gcode).map_err(|e| format!("{g}: {e}"))?;
    }
    let swaps = fab_doc::head_swaps(&res.gcode);
    let grams: Vec<String> = res.stats.filament_g.iter().enumerate().map(|(k, g)| format!("{}: {g:.2} g", colors.get(k).map_or(format!("filament {}", k + 1), String::clone))).collect();
    println!("sliced in {:.2} s: {} layers, {:.0} s estimated ({:.1} min), {} head swaps, {:.2} g ({}) -> {out}", t0.elapsed().as_secs_f64(), res.stats.layers, res.stats.total_time, res.stats.total_time / 60.0, swaps, res.stats.filament_g.iter().sum::<f64>(), grams.join(", "));
    Ok(())
}

fn main() {
    let v: Vec<String> = std::env::args().skip(1).collect();
    if v.is_empty() {
        usage();
    }
    let a = Args { v: v.clone() };
    let r = match v[0].as_str() {
        "slice" => cmd_slice(&a),
        "fab" => cmd_fab(&a),
        "bench" => cmd_bench(&a),
        "bench-edit" => cmd_bench_edit(&a),
        "golden" => match v.get(1) {
            Some(c) if c == "all" => {
                let mut r = Ok(());
                for c in CASES {
                    if let Err(e) = cmd_golden(&a, c.name) {
                        println!("case {}: {e}", c.name);
                        r = Err("some cases failed".to_string());
                    }
                }
                r
            }
            Some(c) => cmd_golden(&a, c),
            None => usage(),
        },
        "estimate" => match v.get(1) {
            Some(f) => cmd_estimate(&a, f),
            None => usage(),
        },
        "import" => match v.get(1) {
            Some(f) => cmd_import(&a, f),
            None => usage(),
        },
        _ => usage(),
    };
    if let Err(e) = r {
        eprintln!("fab-slice: {e}");
        std::process::exit(1);
    }
}
