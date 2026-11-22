//! The Fab slicing pipeline: placed models + job setup → toolpaths →
//! G-code → `.gcode.3mf`, with the per-move preview records.

pub mod bench;
pub mod bench_report;
pub mod fab_doc;
pub mod golden;
pub mod field_given;
pub mod field_regions;
pub mod gate;
pub mod gcode_text;
pub mod glue;
pub mod stage_cases;
#[cfg(test)]
mod infill_golden;
pub mod live;
pub mod live_paths;
pub mod parity;
pub mod piece;
pub mod plate;
pub mod sdf_diff;
pub mod mesh_diff;

/// The SDF types the gate's shared fixtures build parts from, in both the CPU and the GPU gate.
pub use makepad_csg_sdf;

use makepad_fab_gcode::process::Timeline;
use std::sync::Arc;
use makepad_fab_gcode::writer::{self, Stats};
use makepad_fab_package::{PackageInput, PlateObject};
use makepad_fab_settings::profiles::JobSetup;
use makepad_fab_settings::machine::Job;
use makepad_fab_slicer::mesh::Mesh;
use makepad_fab_slicer::source::MeshSource;
use makepad_fab_toolpath::{generate, ObjectInput};

pub const GENERATOR: &str = "Makepad Fab 0.1.0";

/// Today's date as YYYY-MM-DD (UTC).
pub fn today() -> String {
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0) as i64;
    let days = secs.div_euclid(86400);
    // Civil date from days since 1970-01-01.
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}")
}

pub struct PlacedObject {
    pub name: String,
    /// In bed coordinates (mm).
    pub mesh: Mesh,
    /// 0-based filament slot.
    pub filament: usize,
}

pub struct SliceResult {
    /// Original regions and support paths, retained for worker-side analysis.
    pub toolpaths: Arc<makepad_fab_toolpath::Toolpaths>,
    pub gcode: String,
    pub timeline: Timeline,
    pub stats: Stats,
    /// What the slice tells beside its paths: an object that prints its bodies apart though it
    /// prints them as one, and why.
    pub warnings: Vec<String>,
    pub job: Job,
    /// The package's object list and per-layer filaments, for [`SliceResult::package`].
    pub objects: Vec<PlateObject>,
    pub layer_filaments: Vec<Vec<usize>>,
}

impl SliceResult {
    /// The layers that were written.
    pub fn layers(&self) -> &[makepad_fab_toolpath::Layer] {
        &self.toolpaths.layers
    }

    /// The printer package (`.gcode.3mf`) of this slice, dated today. Built
    /// on demand (Print and Export), never as part of slicing: it is the
    /// G-code's MD5, the plate renders and the archive, seconds on big
    /// plates. Call it on a worker.
    pub fn package(&self) -> Result<Vec<u8>, String> {
        self.package_with(&|_| {}, &|| false)
    }

    /// [`Self::package`], telling `progress` the share done (0..1); it ends early, as an
    /// error, once `stop` says the package is no longer wanted.
    pub fn package_with(&self, progress: &(dyn Fn(f64) + Sync), stop: &(dyn Fn() -> bool + Sync)) -> Result<Vec<u8>, String> {
        let moves = self.timeline.parts.iter().map(|part| makepad_fab_gcode::packed::MovesRef::Records(part)).collect();
        package_of(&self.job, &self.gcode, &self.stats, moves, &self.objects, &self.layer_filaments, progress, stop)
    }
}

/// The printer package of a slice's G-code, its move records (`moves`, in print order) and its
/// plate, dated today.
#[allow(clippy::too_many_arguments)]
pub(crate) fn package_of(job: &Job, gcode: &str, stats: &Stats, moves: Vec<makepad_fab_gcode::packed::MovesRef<'_>>, objects: &[PlateObject], layer_filaments: &[Vec<usize>], progress: &(dyn Fn(f64) + Sync), stop: &(dyn Fn() -> bool + Sync)) -> Result<Vec<u8>, String> {
    let family = makepad_fab_settings::family::of(&job.cfg).ok_or(makepad_fab_settings::family::UNSUPPORTED)?;
    use makepad_fab_settings::family::Package;
    let version = match family.package { Package::GcodeThreeMf { version, .. } => version, Package::PlainGcode => "" };
    let inp = PackageInput {
        job,
        gcode,
        stats,
        moves,
        objects,
        layer_filaments,
        client_version: version,
        application: GENERATOR,
        printer_model_id: family.package.model_id(job.s("printer_model")),
        date: &today(),
    };
    match family.package {
        Package::GcodeThreeMf { .. } => makepad_fab_package::build_with(&inp, progress, stop),
        Package::PlainGcode => makepad_fab_package::plain_gcode(&inp),
    }
}

/// Centre a mesh's footprint at (x, y) with its base on the bed.
pub fn place(mesh: &mut Mesh, x: f64, y: f64) {
    let (lo, hi) = mesh.bbox();
    mesh.translate([x - (lo[0] + hi[0]) / 2.0, y - (lo[1] + hi[1]) / 2.0, -lo[2]]);
}

pub fn slice(setup: &JobSetup, objects: Vec<PlacedObject>) -> Result<SliceResult, String> {
    let mut inputs = Vec::new();
    let mut boxes = Vec::new();
    for o in objects {
        boxes.push(o.mesh.bbox());
        inputs.push(ObjectInput { overrides: Vec::new(), name: o.name, source: Box::new(MeshSource::new(o.mesh)), filament: o.filament });
    }
    slice_inputs(setup, inputs, boxes)
}

/// [`slice`], reporting its stages to `progress`.
pub fn slice_with(setup: &JobSetup, objects: Vec<PlacedObject>, progress: Progress) -> Result<SliceResult, String> {
    let mut inputs = Vec::new();
    let mut boxes = Vec::new();
    for o in objects {
        boxes.push(o.mesh.bbox());
        inputs.push(ObjectInput { overrides: Vec::new(), name: o.name, source: Box::new(MeshSource::new(o.mesh)), filament: o.filament });
    }
    slice_inputs_with(setup, inputs, boxes, progress)
}

/// Where a slice is: the fraction done (0..1) and the stage's name, called
/// from the slicing thread at each stage boundary.
pub type Progress<'a> = &'a (dyn Fn(f32, &str) + Sync);

/// Slice objects given as contour sources; `boxes` are their bounds (for
/// the package's object list). Object `i` is the records' `object` `i`.
pub fn slice_inputs(setup: &JobSetup, inputs: Vec<ObjectInput>, boxes: Vec<([f64; 3], [f64; 3])>) -> Result<SliceResult, String> {
    slice_inputs_with(setup, inputs, boxes, &|_, _| {})
}

/// [`slice_inputs`], reporting its stages to `progress`.
pub fn slice_inputs_with(setup: &JobSetup, inputs: Vec<ObjectInput>, boxes: Vec<([f64; 3], [f64; 3])>, progress: Progress) -> Result<SliceResult, String> {
    progress(0.0, "Preparing the job");
    let job = prepare_job(setup)?;
    check_inputs(&job, &inputs)?;
    progress(0.05, "Layers, walls and infill");
    let tp = generate(&inputs, &job);
    let names: Vec<String> = inputs.iter().map(|o| o.name.clone()).collect();
    finish(job, Arc::new(tp), &names, &boxes, progress, &|| false)
}

/// The job a setup slices with: its settings resolved, plus purge volumes
/// from Fab's colour model when the profile carries no matrix for this
/// filament set.
/// The job of a setup; a pattern Fab does not print is an error here
/// (never silently another).
pub fn prepare_job(setup: &JobSetup) -> Result<Job, String> {
    let mut cfg = setup.build_config();
    // The printer is the machine preset's: no setting laid over it names another model.
    if cfg.str("printer_model") != setup.machine.values.str("printer_model") {
        return Err("printer_model is the printer profile's: a setting cannot change it".into());
    }
    makepad_fab_settings::family::of(&cfg).ok_or(makepad_fab_settings::family::UNSUPPORTED)?;
    let nf = setup.filaments.len();
    let nozzles = cfg.vec_len("nozzle_diameter").max(1);
    if nf > 1 && cfg.vec_len("flush_volumes_matrix") != nozzles * nf * nf {
        let colours: Vec<String> = (0..nf).map(|f| cfg.raw("filament_colour", f).to_string()).collect();
        let m = makepad_fab_settings::flush::matrix(&colours, nozzles);
        cfg.set_vec("flush_volumes_matrix", m.iter().map(|v| format!("{v}")).collect());
    }
    let job = Job::new(cfg);
    // The plate sets the bed temperatures: one Fab does not know never heats as another.
    if job.bed_temp_key().is_none() {
        return Err(if setup.bed_type.is_empty() { "no bed plate is picked".to_string() } else { format!("unknown bed plate {:?}", setup.bed_type) });
    }
    makepad_fab_toolpath::check(&job)?;
    Ok(job)
}

/// Every object's filament exists in the job.
pub fn check_inputs(job: &Job, inputs: &[ObjectInput]) -> Result<(), String> {
    for o in inputs {
        if o.filament >= job.filaments() {
            return Err(format!("object {} uses filament {} but the job has {}", o.name, o.filament + 1, job.filaments()));
        }
        if !o.overrides.is_empty() {
            makepad_fab_toolpath::check(&job.with_overrides(&o.overrides)).map_err(|e| format!("{}: {e}", o.name))?;
        }
    }
    Ok(())
}

/// The final, exact step of a slice: toolpaths → G-code and print time
/// (the printer package is built on demand, [`SliceResult::package`]). `names` and `boxes` are the objects' (record `object`
/// order).
pub fn finish(job: Job, tp: Arc<makepad_fab_toolpath::Toolpaths>, names: &[String], boxes: &[([f64; 3], [f64; 3])], progress: Progress, stop: &(dyn Fn() -> bool + Sync)) -> Result<SliceResult, String> {
    // The plate has one bed temperature: refuse a filament set that cannot share it.
    let mut used: Vec<u32> = tp.layers.iter().flat_map(|l| l.objects.iter().filter(|ol| ol.has_paths()).map(|ol| ol.filament as u32 + 1)).collect();
    used.sort();
    used.dedup();
    if let Some(why) = fab_printability::Materials::from_job(&job).bed_conflict(&used) {
        return Err(why);
    }
    progress(0.55, "G-code and print time");
    if makepad_fab_gcode::sequential::active(&tp, &job) { makepad_fab_gcode::sequential::check(&tp, &job)?; }
    let out = writer::write(&tp, &job, GENERATOR, stop);
    if !out.warnings.is_empty() {
        return Err(format!("G-code generation failed:\n{}", out.warnings.join("\n")));
    }
    let by_object = makepad_fab_gcode::sequential::active(&tp, &job);
    let sequence = if by_object { makepad_fab_gcode::sequential::layer_order(&tp) }
        else { (0..tp.layers.len()).map(|li| (li, usize::MAX, li)).collect() };
    let layer_filaments: Vec<Vec<usize>> = sequence.iter()
        .map(|&(li, object, _)| {
            let mut v: Vec<usize> = tp.layers[li].objects.iter().filter(|ol| ol.has_paths() && (!by_object || ol.object == object)).map(|ol| ol.filament).collect();
            if by_object {
                v.extend(tp.np_blocks.iter().filter(|b| b.layer == li && b.object == object && !b.paths.is_empty()).map(|b| tp.objects[b.object].filament));
            }
            v.sort();
            v.dedup();
            v
        })
        .collect();
    let ids = if out.stats.label_ids.is_empty() { vec![100] } else { out.stats.label_ids.clone() };
    let objs: Vec<PlateObject> = names
        .iter()
        .enumerate()
        .map(|(i, n)| {
            let (lo, hi) = boxes[i];
            let area = tp.layers.first().map(|l| l.objects.iter().filter(|o| o.object == i && !o.support_only).flat_map(|o| o.slices.iter()).map(|s| s.area_mm2()).sum()).unwrap_or(0.0);
            PlateObject { identify_id: ids.get(i).copied().unwrap_or(100 + i as u32), name: n.clone(), bbox: [lo[0], lo[1], hi[0], hi[1]], area }
        })
        .collect();
    progress(1.0, "Sliced");
    Ok(SliceResult { toolpaths: tp, gcode: out.text, timeline: out.timeline, stats: out.stats, warnings: Vec::new(), job, objects: objs, layer_filaments })
}

/// A job from the printer, process and filaments the user has selected in
/// their installed slicer profiles: the first `filament_map.len()` selected
/// filaments (with their colours), mapped onto the nozzles as given
/// (1-based, 1 = left).
pub fn installed_setup(filament_map: &[u32]) -> Result<JobSetup, String> {
    let dir = makepad_fab_settings::profiles::ProfileStore::default_data_dir().ok_or("no slicer profile folder")?;
    installed_setup_in(&dir, filament_map)
}

/// [`installed_setup`] from the profile folder `dir`.
pub fn installed_setup_in(dir: &std::path::Path, filament_map: &[u32]) -> Result<JobSetup, String> {
    use makepad_fab_settings::profiles::{installed_selection, ProfileStore};
    let sel = installed_selection(dir).ok_or("no printer selected in the installed slicer profiles")?;
    let store = ProfileStore::open(&[dir.to_path_buf()], &[]);
    let n = filament_map.len().max(1);
    if sel.filaments.len() < n {
        return Err(format!("the installed profiles select {} filaments, the job needs {n}", sel.filaments.len()));
    }
    let filaments = sel.filaments[..n].iter().map(|f| store.resolve(f)).collect::<Result<Vec<_>, _>>()?;
    let machine = store.resolve(&sel.machine)?;
    makepad_fab_settings::family::of_profile(&machine.values)?;
    let nozzle_volume_type = makepad_fab_settings::profiles::nozzle_flow_types(&machine, Some(&sel));
    Ok(JobSetup {
        machine,
        process: store.resolve(&sel.process)?,
        filaments,
        filament_map: if filament_map.is_empty() { vec![1] } else { filament_map.to_vec() },
        nozzle_volume_type,
        bed_type: sel.plate,
        filament_map_mode: "Manual".into(),
        filament_colours: (0..n).map(|i| sel.colours.get(i).cloned()).collect(),
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use makepad_fab_gcode::record::MoveKind;
    use makepad_fab_settings::profiles::Preset;

    /// A closed box as a mesh (outward-facing triangles).
    fn cuboid(m: &mut Mesh, lo: [f64; 3], hi: [f64; 3]) {
        let b = m.verts.len() as u32;
        for z in [lo[2], hi[2]] {
            m.verts.extend_from_slice(&[[lo[0], lo[1], z], [hi[0], lo[1], z], [hi[0], hi[1], z], [lo[0], hi[1], z]]);
        }
        let t = [[0, 2, 1], [0, 3, 2], [4, 5, 6], [4, 6, 7], [0, 1, 5], [0, 5, 4], [1, 2, 6], [1, 6, 5], [2, 3, 7], [2, 7, 6], [3, 0, 4], [3, 4, 7]];
        for k in t {
            m.tris.push([b + k[0], b + k[1], b + k[2]]);
        }
    }

    /// Option defaults plus only the facts a plate needs: no profile file
    /// sets any speed.
    pub(crate) fn bare_setup(nf: usize, process: &[(&str, &str)]) -> JobSetup {
        let mut machine = Preset { name: "test machine".into(), kind: "machine".into(), ..Default::default() };
        // The bed and the nozzles' reach of the app's default printer, as the app lists them.
        let fab = fab_plate::machine::Machine::dual_nozzle_350();
        let v = &mut machine.values;
        v.set_vec("printable_area", fab.bed.corners());
        v.set_str("printable_height", "320");
        v.set_vec("extruder_printable_area", fab.nozzles.iter().map(|z| z.reach.corners().join(",")).collect());
        v.set_vec("extruder_printable_height", vec!["320".into(), "325".into()]);
        v.set_vec("nozzle_diameter", vec!["0.4".into(), "0.4".into()]);
        let mut proc_ = Preset { name: "test process".into(), kind: "process".into(), ..Default::default() };
        for (k, val) in process {
            proc_.values.set_str(k, *val);
        }
        let filaments = (0..nf).map(|i| Preset { name: format!("test filament {i}"), kind: "filament".into(), ..Default::default() }).collect();
        JobSetup {
            machine,
            process: proc_,
            filaments,
            filament_map: (0..nf).map(|i| (i % 2) as u32 + 1).collect(),
            nozzle_volume_type: vec!["Standard".into(), "Standard".into()],
            bed_type: "Textured PEI Plate".into(),
            filament_map_mode: "Manual".into(),
            filament_colours: vec![None; nf],
        }
    }

    /// A stem under a wide cap (bridges, floating shells, supports), its top
    /// half painted filament 2, beside a plain box on filament 2.
    fn plate() -> Vec<PlacedObject> {
        let mut t = Mesh::default();
        cuboid(&mut t, [-4.0, -4.0, 0.0], [4.0, 4.0, 8.2]);
        cuboid(&mut t, [-12.0, -12.0, 8.0], [12.0, 12.0, 12.0]);
        t.paint = t.tris.iter().map(|tr| if tr.iter().all(|&i| t.verts[i as usize][2] >= 8.0) { "8".to_string() } else { String::new() }).collect();
        place(&mut t, 150.0, 160.0);
        let mut b = Mesh::default();
        cuboid(&mut b, [0.0, 0.0, 0.0], [15.0, 15.0, 6.0]);
        place(&mut b, 200.0, 160.0);
        vec![PlacedObject { name: "cap".into(), mesh: t, filament: 0 }, PlacedObject { name: "box".into(), mesh: b, filament: 1 }]
    }

    #[test]
    fn setup_refuses_an_unclaimed_model() {
        let mut setup = bare_setup(1, &[]);
        setup.machine.values.set_str("printer_model", "Unclaimed");
        assert_eq!(prepare_job(&setup).err().as_deref(), Some(makepad_fab_settings::family::UNSUPPORTED));
        // A setting laid over a claimed profile cannot erase its model, or name another.
        let claimed = makepad_fab_settings::family::DEFAULT.models[0];
        for over in ["", "Unclaimed"] {
            let mut setup = bare_setup(1, &[("printer_model", over)]);
            setup.machine.values.set_str("printer_model", claimed);
            assert!(prepare_job(&setup).err().is_some_and(|e| e.contains("cannot change it")), "{over:?}");
        }
    }

    /// Whatever the preset source, every extrusion has a speed: a zero
    /// feed (G-code F0) would make the print take forever.
    #[test]
    fn no_extrusion_without_a_speed() {
        for process in [
            vec![],
            vec![("enable_support", "1"), ("support_type", "tree(auto)"), ("support_style", "tree_organic")],
            vec![("enable_support", "1"), ("support_type", "normal(auto)"), ("support_style", "grid")],
        ] {
            let setup = bare_setup(2, &process);
            let res = slice(&setup, plate()).unwrap_or_else(|e| panic!("{process:?}: {e}"));
            let mut extrusions = 0;
            for m in &res.timeline.flat_moves() {
                if m.kind == MoveKind::Extrude {
                    extrusions += 1;
                    assert!(m.f_cmd > 0.0 && m.v_cruise > 0.0, "{process:?}: {:?} at {:?} has feed {} cruise {}", m.role, m.p1, m.f_cmd, m.v_cruise);
                }
            }
            assert!(extrusions > 1000, "{process:?}: {extrusions} extrusions");
            assert!(res.gcode.lines().all(|l| !l.contains(" F0 ") && !l.ends_with(" F0")), "{process:?}: a G-code line with F0");
            assert!(res.stats.total_time.is_finite() && res.stats.total_time < 4.0 * 3600.0, "{process:?}: {} s", res.stats.total_time);
        }
    }
}
