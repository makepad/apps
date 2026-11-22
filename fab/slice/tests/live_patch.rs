//! Under the JIT, a live patch of the support code plans the supports again: the live engine keeps
//! what it makes under the code that made it (`LoadedCode`), and `code_patched` runs the newest
//! input again by the patched code. A test binary of its own: the patch moves the code every engine
//! in the process runs under, which no other test may see.
#![cfg(relax)]

use fab_plate::{Part, PartMesh, Plate};
use makepad_fab_settings::profiles::{JobSetup, Preset};
use makepad_fab_slice::live::{loaded_code, LiveEngine, LiveEvent, LiveInput, Next, Settled, Spawn, Urgency};
use std::path::{Component, Path, PathBuf};
use std::sync::{atomic::{AtomicU64, Ordering}, mpsc, Arc, Mutex};

fn cube(min: [f64; 3], size: [f64; 3]) -> PartMesh {
    let positions = (0..8u32).map(|i| [min[0] + size[0] * (i & 1) as f64, min[1] + size[1] * ((i >> 1) & 1) as f64, min[2] + size[2] * ((i >> 2) & 1) as f64]).collect();
    let triangles = vec![[0, 2, 1], [1, 2, 3], [4, 5, 6], [5, 7, 6], [0, 1, 4], [1, 5, 4], [2, 6, 3], [3, 6, 7], [0, 4, 2], [2, 4, 6], [1, 3, 5], [3, 7, 5]];
    PartMesh { positions, triangles }
}

/// A cap on a stem: the cap's underside stands on trees.
fn plate() -> Plate {
    let mut stem = cube([96.0, 96.0, 0.0], [8.0, 8.0, 8.0]);
    let cap = cube([88.0, 88.0, 8.0], [24.0, 24.0, 3.0]);
    let b = stem.positions.len() as u32;
    stem.positions.extend(cap.positions);
    stem.triangles.extend(cap.triangles.iter().map(|t| t.map(|v| v + b)));
    let part = Part { body_of: None, print: Vec::new(), print_mesh: None, paint: None, object: "cap".into(), name: "cap".into(), filament: 1, color: [1.0; 4], nozzle: None, mesh: stem, field_bounds: None, field_floor: None, ..Default::default() };
    Plate { excluded: Vec::new(), settings: Vec::new(), parts: vec![part] }
}

/// Tree supports whose branches are 4.321 mm across: a diameter no other slice takes, which the
/// patch below widens alone.
fn setup() -> JobSetup {
    let mut machine = Preset::default();
    machine.values.set_vec("nozzle_diameter", vec!["0.4".into(), "0.4".into()]);
    machine.values.set_vec("printable_area", fab_plate::machine::Machine::dual_nozzle_350().bed.corners());
    let mut process = Preset::default();
    for (k, v) in [("enable_support", "1"), ("support_type", "tree(auto)"), ("tree_support_branch_diameter_organic", "4.321")] {
        process.values.set_str(k, v);
    }
    JobSetup {
        machine,
        process,
        filaments: vec![Preset::default(), Preset::default()],
        filament_map: vec![1, 2],
        nozzle_volume_type: vec!["Standard".into(), "Standard".into()],
        bed_type: "Textured PEI Plate".into(),
        filament_map_mode: "Manual".into(),
        filament_colours: vec![None; 2],
    }
}

/// An engine on std threads whose continuations skip a stale input's work, as the app's pool
/// tickets do.
fn engine() -> (LiveEngine, mpsc::Receiver<LiveEvent>) {
    let (tx, rx) = mpsc::channel();
    let tx = Mutex::new(tx);
    let spawn: Spawn = Arc::new(|_, job| {
        std::thread::spawn(job);
    });
    let version = Arc::new(AtomicU64::new(0));
    let next: Next = Arc::new(move || {
        let mine = version.fetch_add(1, Ordering::AcqRel) + 1;
        let version = version.clone();
        Arc::new(move || version.load(Ordering::Acquire) != mine)
    });
    let engine = LiveEngine::new(spawn.clone(), Arc::new(move |e| {
        let _ = tx.lock().unwrap().send(e);
    }), next);
    engine.set_resume(Arc::new(move |checkpoint| {
        let spawn = spawn.clone();
        Arc::new(move |urgency, job| {
            let checkpoint = checkpoint.clone();
            spawn(urgency, Box::new(move || if urgency == Urgency::Viewed || !checkpoint() { job() }));
        })
    }));
    (engine, rx)
}

fn settled(rx: &mpsc::Receiver<LiveEvent>, want: u64) -> Arc<Settled> {
    loop {
        match rx.recv().expect("a settled slice") {
            LiveEvent::Settled { tag, result, .. } if tag == want => return result.expect("sliced"),
            LiveEvent::Failed { error, .. } => panic!("{error}"),
            _ => {}
        }
    }
}

/// A source of the running code patched live: its text with `inserted` after `at`, as it was
/// again once dropped.
struct LivePatch {
    path: String,
    text: String,
}

impl LivePatch {
    fn new(rel: &str, at: &str, inserted: &str) -> LivePatch {
        // The path from this crate's folder, its `..` taken out by name (the JIT resolves it).
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(rel).components().fold(PathBuf::new(), |mut p, c| {
            match c {
                Component::CurDir => {}
                Component::ParentDir => {
                    p.pop();
                }
                c => p.push(c),
            }
            p
        });
        let path = path.to_string_lossy().into_owned();
        let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"));
        assert_eq!(text.matches(at).count(), 1, "{rel} has `{at}` to patch after, once");
        let patched = text.replacen(at, &format!("{at}{inserted}"), 1);
        assert_eq!(std::relax::live_stream(&path, &patched), 0, "the patch of {rel} applies in place");
        LivePatch { path, text }
    }

    /// Its source as it was, live again.
    fn undo(mut self) {
        let text = std::mem::take(&mut self.text);
        assert_eq!(std::relax::live_stream(&self.path, &text), 0, "the source applies again as it was");
    }
}

impl Drop for LivePatch {
    fn drop(&mut self) {
        if !self.text.is_empty() {
            std::relax::live_stream(&self.path, &self.text);
        }
    }
}

/// A live patch of the tree supports' code plans the supports again on the part's slice as it is,
/// as a support setting does: no contour is traced and no model layer planned, nothing the sink
/// shows of the part's layers goes or comes again, the cap's supports are planned anew, and the
/// slice is a fresh engine's under the patched code, not the one before. Undone, the patch slices
/// as before it, again by the supports alone.
#[test]
fn a_live_patch_of_the_support_code_plans_the_supports_again() {
    let p = Arc::new(plate());
    let input = || LiveInput::ready(1, 1, setup(), p.clone(), Arc::new(Vec::new()));
    let (e, rx) = engine();
    e.settle(1, input());
    let first = settled(&rx, 1);
    assert!(!e.code_patched(), "no patch: nothing runs again");
    let (work, traced) = (e.part_work(), e.traced_count());
    // After a patch: the input runs again by the loaded code and, once its toolpaths are in,
    // settles as `tag`. What the sink was given on the way is the supports alone.
    let again = |tag: u64| -> Arc<Settled> {
        assert!(e.code_patched(), "a patch of the support code runs the input again");
        let rev = e.revisions().0;
        let (mut told, mut layers_sent, mut supports, mut settling) = (false, 0, None, false);
        let slice = loop {
            match rx.recv().expect("a settled slice") {
                LiveEvent::Settings => told = true,
                LiveEvent::PartCut { .. } | LiveEvent::PartRegions { .. } | LiveEvent::PartLayers { .. } => layers_sent += 1,
                LiveEvent::PartSupports { part: 0, layers, done: true, .. } => supports = Some(layers),
                LiveEvent::Toolpaths { revision, .. } if revision == rev && !settling => {
                    settling = true;
                    e.settle(tag, input());
                }
                LiveEvent::Settled { tag: t, result, .. } if t == tag => break result.expect("sliced"),
                LiveEvent::Failed { error, .. } => panic!("{error}"),
                _ => {}
            }
        };
        assert!(!told && layers_sent == 0, "the part's layers stay as shown: told {told}, {layers_sent} layer events");
        assert!(supports.is_some_and(|l| l.iter().any(|(_, r)| !r.is_empty())), "the cap's supports are planned again and shown");
        assert_eq!(e.traced_count(), traced, "no contour traced");
        assert_eq!(e.part_work(), work, "no model layer planned");
        assert!(!e.code_patched(), "the slice is the loaded code's: nothing runs again");
        slice
    };
    let before = loaded_code();
    let patch = LivePatch::new("../toolpath/src/tree/settings.rs", "    pub fn new(keys: Keys) -> Settings {\n",
        "        let keys = if keys.branch_diameter == 4.321 { Keys { branch_diameter: 6.5, ..keys } } else { keys };\n");
    let patched = loaded_code();
    assert!(patched.supports != before.supports && (patched.part, patched.output) == (before.part, before.output), "the patch moves the support code alone: {before:?} to {patched:?}");
    let widened = again(2);
    let fresh = {
        let (e, rx) = engine();
        e.settle(1, input());
        settled(&rx, 1)
    };
    assert!(widened.gcode == fresh.gcode, "the supports planned again are a fresh engine's under the patched code");
    assert!(widened.gcode != first.gcode, "the patched code widens the branches");
    patch.undo();
    let back = again(3);
    assert!(back.gcode == first.gcode, "undone, the patch slices as before it");
}
