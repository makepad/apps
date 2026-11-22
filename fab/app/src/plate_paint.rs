//! Pictures of the plates and demos the view has not taken one of: each one built away from the
//! bed (a session of its own) and traced on the CPU from the view it opens with, its parts
//! framed, on a background worker. A picture's time is its plate's build, the one opening the
//! plate takes (its joints' exact mesh booleans, the exact bodies' steps, a grown pattern's
//! simulation); the trace is a few percent of it. Several are painted at once, each shown as it
//! ends: one slow build (a plate of many printed-in-place joints takes tens of seconds) holds no
//! other picture back ([`crate::model_host`] starts them); one whose build stops at the memory
//! budget beside others is painted again alone. A file beside each picture (`<picture>.of`)
//! names its look and what its build read: the plate's object and electronics documents and
//! assets, printer, calibrated colour sets, script libraries and host input values. A picture is
//! painted again when any of them moved; a picture the view takes (its own frame of the plate)
//! drops the name.
//!
//! A picture is a cache: it and its name are written without waiting on the disk. One painted
//! at a time, a picture that waited for a flush held every row behind it, for over ten seconds
//! when other programs kept the disk busy; one a crash cuts short is empty and painted again
//! ([`fab_ai::plates::has_picture`]).

use fab::api::{Aabb, Camera, PartSdf};
use fab::model::query::Ray;
use fab_model::eval::Evaluated;
use fab_model::program::ScriptEnv;
use fab_plate::machine::Machine;
use makepad_core_util::sha256::{sha256_hex, to_hex};
use makepad_widgets::*;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// The painter's look: a picture painted by an earlier one is painted again.
const LOOK: u32 = 2;

/// The painted picture's size (the list shows it at about half).
const WIDTH: u32 = 192;
const HEIGHT: u32 = 144;

/// What a picture is painted of.
pub enum Subject {
    /// A plate: its working copy, else its file.
    Plate { store: fab_ai::plates::PlateStore, id: String, file: Option<PathBuf> },
    Demo(&'static crate::demo_part::Demo),
}

/// One picture to paint: of what, where it goes, and the printer the plate is built for.
pub struct Order {
    /// The plate's id or the demo's key, as the list names it.
    pub key: String,
    pub subject: Subject,
    pub out: PathBuf,
    pub machine: fab_plate::machine::Machine,
    /// No other picture builds beside this one: its build stopping at the memory budget is its end.
    pub alone: bool,
}

impl Order {
    /// What the host keeps it as among those looked at: a plate's id, a demo's key after `demo:`.
    pub fn seen(&self) -> String {
        match &self.subject {
            Subject::Plate { id, .. } => id.clone(),
            Subject::Demo(demo) => format!("demo:{}", demo.key),
        }
    }
}

/// How a picture's paint ended ([`crate::model_host::HostAction::Painted`]).
#[derive(Clone, Debug)]
pub enum Painted {
    /// Written (true), or found as it is or not painted (false: its line in the log says why).
    Done(bool),
    /// Its build stopped at the process's memory budget while other pictures built beside it: it
    /// is painted again once none does (its key among those looked at, [`Order::seen`]).
    Again(String),
}

/// Why a picture was not painted.
#[derive(Debug)]
pub struct Unpainted {
    pub why: String,
    /// The build stopped at the process's memory budget.
    pub memory: bool,
}

impl From<String> for Unpainted {
    fn from(why: String) -> Unpainted {
        Unpainted { why, memory: false }
    }
}

/// Paint `order` on a background worker; its end comes as [`crate::model_host::HostAction::Painted`].
/// False when the pool takes no work (the app is closing).
pub fn start(pool: &TaskPool, order: Order) -> bool {
    let key = order.key.clone();
    let task = pool.submit_named(Class::Background, "fab plate picture", move || {
        let started = std::time::Instant::now();
        let painted = match paint(&order) {
            Ok(true) => {
                log!("fab: painted the picture of {} in {:.0} ms", order.key, started.elapsed().as_secs_f64() * 1e3);
                Painted::Done(true)
            }
            Ok(false) => Painted::Done(false),
            Err(Unpainted { why, memory: true }) if !order.alone => {
                log!("fab: the picture of {} waits until no other picture builds: {why}", order.key);
                Painted::Again(order.seen())
            }
            Err(Unpainted { why, .. }) => {
                log!("fab: no picture of {}: {why}", order.key);
                Painted::Done(false)
            }
        };
        Cx::post_action(crate::model_host::HostAction::Painted(painted));
    });
    match task {
        Ok(task) => {
            task.detach();
            true
        }
        Err(_) => {
            log!("fab: no picture of {key}: the app is closing");
            false
        }
    }
}

/// True when a picture was written; false when its `.of` still names this look and what its
/// build read: the object and electronics documents and assets, printer, calibrated colour
/// sets, script libraries and host input values. A change to any paints it again. A plate is
/// the person's file, perhaps in an earlier Fab's format: building it for its picture is no run
/// of it and files no script error (what it meets is this painter's line, and its row says why
/// it does not read), as the list's reads do. A demo is the app's own: its errors are filed.
fn paint(order: &Order) -> Result<bool, Unpainted> {
    let shared = crate::elec_host::shared_env();
    let env_for: fab_ai::EnvFor = Arc::new(crate::elec_host::doc_env);
    match order.subject {
        Subject::Plate { .. } => makepad_script::inspect_registry::unreported(|| paint_built(order, shared, env_for)),
        Subject::Demo(_) => paint_built(order, shared, env_for),
    }
}

/// What the picture's build read, in order. A read with a line or field separator has no name
/// this file can keep, so its picture is painted each time.
fn key_text(content: &[u8; 32], machine: &Machine, calibrated: &fab_model::colour_sets::Sets, env: &ScriptEnv, reads: &[(String, String)]) -> Option<String> {
    if reads.iter().any(|(kind, name)| kind.contains(['\t', '\n', '\r']) || name.contains(['\t', '\n', '\r'])) {
        return None;
    }
    let mut text = format!(
        "look\t{LOOK}\ncontent\t{}\nmachine\t{}\ncalibrated\t{}\n",
        fab_ai::plates::content_text(content),
        sha256_hex(format!("{machine:?}").as_bytes()),
        sha256_hex(format!("{calibrated:?}").as_bytes()),
    );
    for library in env.libraries.iter() {
        text.push_str(&format!("library\t{}\t{}\n", library.file, sha256_hex(library.source.as_bytes())));
    }
    for key in reads {
        let (kind, name) = key;
        let hash = env.inputs.values.get(key).map(|i| i.hash).or_else(|| {
            let lazy = env.inputs.lazy.as_ref().filter(|_| env.inputs.kinds.contains(kind))?;
            lazy.provider.provide(kind, name).map(|i| i.hash)
        });
        let hash = hash.map(|h| to_hex(&h)).unwrap_or_else(|| "none".into());
        text.push_str(&format!("input\t{kind}\t{name}\t{hash}\n"));
    }
    Some(text)
}

fn paint_built(order: &Order, shared: ScriptEnv, env_for: fab_ai::EnvFor) -> Result<bool, Unpainted> {
    let model = match &order.subject {
        Subject::Plate { store, id, file } => match store.working_model(id) {
            Some(model) => model,
            None => match file {
                Some(file) if file.is_file() => fab_model::project::load(file, &shared)?,
                _ => return Err(String::from("the plate has nothing to show").into()),
            },
        },
        Subject::Demo(demo) => demo.model()?,
    };
    paint_model(model, &order.out, &order.machine, shared, env_for)
}

/// [`paint_built`] for a loaded `model`: the picture at `out` for the plate built for `machine`,
/// its scripts reading `shared` (a plate's documents name their own env, `env_for`).
fn paint_model(model: fab_model::FabModel, out: &Path, machine: &Machine, shared: ScriptEnv, env_for: fab_ai::EnvFor) -> Result<bool, Unpainted> {
    let content = model.content();
    let calibrated = Arc::new(fab_model::colour_sets::load_all(&crate::model_host::documents_dir()).0);
    let named = fab_ai::plates::painted_content(out);
    if fab_ai::plates::has_picture(out) && std::fs::read_to_string(&named).is_ok_and(|text| {
        let reads: Vec<_> = text.lines().filter_map(|line| {
            let (kind, rest) = line.strip_prefix("input\t")?.split_once('\t')?;
            let (name, _) = rest.split_once('\t')?;
            Some((kind.to_string(), name.to_string()))
        }).collect();
        let env = env_for(model.docs(), model.elec_assets());
        key_text(&content, machine, &calibrated, &env, &reads).as_deref() == Some(text.as_str())
    }) {
        return Ok(false);
    }
    let name = model.name.clone();
    let mut session = fab_ai::Session::new(machine.clone(), std::env::temp_dir());
    // the process's memory budget stops the build, and says so to the host
    let stopped = Arc::new(std::sync::atomic::AtomicBool::new(false));
    session.eval.memory_guard = Some({
        let (guard, stopped) = (crate::live_slice::model_memory_guard(), stopped.clone());
        Arc::new(move || guard().inspect(|_| stopped.store(true, std::sync::atomic::Ordering::Relaxed)))
    });
    session.eval.picture = true;
    // its pictures in the colours the calibrated sets kept print them
    session.eval.calibrated = calibrated.clone();
    // The scripts' env as the model host's session has it: a case plate reads its board, a plate
    // with electronics documents its own.
    session.model.set_env(shared);
    session.env_for = Some(env_for);
    let ev = session.replace_model(model, None).map_err(|why| Unpainted { why, memory: stopped.load(std::sync::atomic::Ordering::Relaxed) })?;
    let mut reads = Vec::new();
    if let Some(compiled) = session.compiled.as_deref() {
        for inst in &compiled.instances {
            for read in &inst.script.program.inputs_read {
                if !reads.contains(read) {
                    reads.push(read.clone());
                }
            }
        }
    }
    let key = key_text(&content, machine, &calibrated, session.model.env(), &reads);
    let rgba = render(&ev, session.compiled.as_deref(), &name);
    let png = fab_ai::plates::thumbnail_png(WIDTH, HEIGHT, &rgba)?;
    fab_model::project::write_unflushed(out, &png)?;
    match key {
        Some(key) => fab_model::project::write_unflushed(&named, key.as_bytes())?,
        None => match std::fs::remove_file(&named) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(format!("{}: {e}", named.display()).into()),
        },
    }
    Ok(true)
}

/// The plate as the view opens on it, its parts framed: the scene's triangles and the parts drawn
/// from their fields, each hit lit by one light over the shoulder. RGBA, rows top down; nothing
/// hit is clear.
fn render(ev: &Evaluated, compiled: Option<&fab_model::program::Compiled>, name: &str) -> Vec<u8> {
    let scene = crate::plate_scene::build(ev, name);
    let plate = crate::plate_edit::plate_info(compiled, ev, 0);
    let aspect = WIDTH as f32 / HEIGHT as f32;
    let mut camera = Camera::default();
    camera.frame_bounds(&parts_box(&plate).unwrap_or_else(|| plate.view_box()), aspect);
    let fields: Vec<(&PartSdf, [f32; 4], Aabb)> = plate
        .parts
        .iter()
        .filter_map(|p| {
            let sdf = p.sdf.as_ref()?;
            let bounds = scene.elements.iter().find(|e| e.name == p.object)?.bounds;
            Some((sdf, p.color, bounds))
        })
        .collect();
    // Each material's colour, a textured one's (the build plate's face) as its texture averages.
    let colors: Vec<_> = scene
        .materials
        .iter()
        .map(|m| {
            let mut c = m.base_color;
            if let Some(t) = m.texture.and_then(|t| scene.textures.get(t as usize)).filter(|t| !t.rgba.is_empty()) {
                let n = (t.rgba.len() / 4) as f32;
                for (i, c) in c.iter_mut().take(3).enumerate() {
                    *c *= t.rgba.chunks_exact(4).map(|p| p[i] as f32).sum::<f32>() / n / 255.0;
                }
            }
            (m.id, c)
        })
        .collect();
    let forward = (camera.target - camera.eye).normalize();
    let right = Vec3f::cross(forward, camera.up).normalize();
    let light = (camera.up * 0.9 - forward - right * 0.5).normalize();
    let mut rgba = vec![0u8; (WIDTH * HEIGHT * 4) as usize];
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            let ndc = vec2((x as f32 + 0.5) / WIDTH as f32 * 2.0 - 1.0, 1.0 - (y as f32 + 0.5) / HEIGHT as f32 * 2.0);
            let ray = camera.ray_at_ndc(ndc, aspect);
            let mut best: Option<(f32, Vec3f, [f32; 4])> = scene.bvh.raycast(&scene.batches, &ray, &|_| true).map(|h| {
                let material = scene.batches.get(h.batch as usize).map(|b| b.material);
                let color = colors.iter().find(|(id, _)| Some(*id) == material).map_or([0.7, 0.7, 0.7, 1.0], |(_, c)| *c);
                (h.t, h.normal, color)
            });
            for (sdf, color, bounds) in &fields {
                let Some(enter) = enters(&ray, bounds) else { continue };
                if best.is_some_and(|(t, _, _)| t < enter) {
                    continue;
                }
                let origin = [ray.origin.x as f64 * 1000.0, ray.origin.y as f64 * 1000.0, ray.origin.z as f64 * 1000.0];
                let dir = [ray.dir.x as f64, ray.dir.y as f64, ray.dir.z as f64];
                if let Some(h) = fab::viewport::sdf::pick_plate(sdf, origin, dir, 1.0) {
                    let t = (h.t * 0.001) as f32;
                    if best.is_none_or(|(b, _, _)| t < b) {
                        let n = vec3(h.normal[0] as f32, h.normal[1] as f32, h.normal[2] as f32);
                        best = Some((t, if n.dot(ray.dir) > 0.0 { -n } else { n }, *color));
                    }
                }
            }
            let Some((_, normal, color)) = best else { continue };
            let lit = 0.35 + 0.65 * normal.dot(light).max(0.0);
            let at = ((y * WIDTH + x) * 4) as usize;
            for c in 0..3 {
                rgba[at + c] = (color[c] * lit * 255.0).clamp(0.0, 255.0) as u8;
            }
            rgba[at + 3] = 255;
        }
    }
    rgba
}

/// The box round the plate's parts (metres), none without parts.
fn parts_box(plate: &fab::api::PlateInfo) -> Option<Aabb> {
    let first = plate.parts.first()?;
    let (mut min, mut max) = (first.min_mm, first.max_mm);
    for p in &plate.parts {
        for axis in 0..3 {
            min[axis] = min[axis].min(p.min_mm[axis]);
            max[axis] = max[axis].max(p.max_mm[axis]);
        }
    }
    let at = |p: [f64; 3]| vec3(p[0] as f32 * 0.001, p[1] as f32 * 0.001, p[2] as f32 * 0.001);
    Some(Aabb { min: at(min), max: at(max) })
}

/// Where a ray enters a box (metres along it), none when it misses.
fn enters(ray: &Ray, b: &Aabb) -> Option<f32> {
    let (mut near, mut far) = (0.0f32, f32::MAX);
    for (o, d, lo, hi) in [(ray.origin.x, ray.dir.x, b.min.x, b.max.x), (ray.origin.y, ray.dir.y, b.min.y, b.max.y), (ray.origin.z, ray.dir.z, b.min.z, b.max.z)] {
        if d.abs() < 1e-12 {
            if o < lo || o > hi {
                return None;
            }
            continue;
        }
        let (a, c) = ((lo - o) / d, (hi - o) / d);
        near = near.max(a.min(c));
        far = far.min(a.max(c));
    }
    (near <= far).then_some(near)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The paint-once logic, on the demo plate: the plates panel's test sees every demo painted,
    /// each once.
    #[test]
    fn a_demo_is_painted_once_for_its_content() {
        let dir = std::env::temp_dir().join(format!("fab-paint-{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        std::fs::create_dir_all(&dir).unwrap();
        let machine = crate::print::CheckSetup::fab_defaults().machine().unwrap();
        let demo = crate::demo_part::demo("demo").expect("the demo plate");
        let order = Order { key: demo.key.to_string(), subject: Subject::Demo(demo), out: dir.join(format!("{}.png", demo.key)), machine, alone: false };
        assert!(paint(&order).unwrap(), "{} is painted", demo.key);
        let png = std::fs::read(&order.out).unwrap();
        assert!(png.starts_with(b"\x89PNG"), "{}: the picture is a PNG", demo.key);
        assert!(!paint(&order).unwrap(), "{}: the same content is not painted again", demo.key);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// The same box is painted again when a library or a host input its script reads moved.
    #[test]
    fn a_picture_is_painted_again_when_a_library_or_input_its_plate_reads_changes() {
        use fab_model::lang;

        let dir = std::env::temp_dir().join(format!("fab-paint-input-{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        std::fs::create_dir_all(&dir).unwrap();
        let out = dir.join("box.png");
        let machine = crate::print::CheckSetup::fab_defaults().machine().unwrap();
        let wide = "fn wide() { return 4mm }\nlet lib = {wide: wide}\nlib";
        let paint_with = |source: &str, hash: u8| {
            let model = fab_model::FabModel::with_script("test", "t.cad.splash", "use mod.model.*\nuse mod.fab.*\nlet m = mod.model\nlet b = board(\"b1\")\nobject(m.box({size: [wide() b.width 2mm]}), name: \"a\")");
            let env = ScriptEnv::new(vec![lang::Library { file: "wide.splash".into(), source: source.into() }], lang::Inputs {
                kinds: vec!["board".into()],
                values: [(("board".into(), "b1".into()), lang::Input { source: "{width: 3mm}".into(), hash: [hash; 16] })].into_iter().collect(),
                lazy: None,
            });
            paint_model(model, &out, &machine, ScriptEnv::default(), Arc::new(move |_: &Arc<fab_model::Docs>, _: &Arc<fab_model::ElecAssets>| env.clone())).unwrap()
        };
        assert!(paint_with(wide, 1), "the picture is painted the first time");
        assert!(!paint_with(wide, 1), "the picture is not painted again with nothing changed");
        let wider = wide.replace("4mm", "6mm");
        assert!(paint_with(&wider, 1), "the picture is painted again after the library source changes");
        assert!(paint_with(&wider, 2), "the picture is painted again after the read board value's hash changes");
        let named = std::fs::read_to_string(fab_ai::plates::painted_content(&out)).unwrap();
        assert!(named.contains("input\tboard\tb1\t"), "the picture's name records the board input its build read");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A plate kept from an earlier Fab, its file in that Fab's format (`place` with the
    /// `filament:` and `nozzle:` it no longer takes), gets no picture and files no script error:
    /// the person's Fab filed them each session as it tried.
    #[test]
    fn an_earlier_formats_plate_files_no_script_error_for_its_picture() {
        crate::model_host::filed::listen();
        let dir = std::env::temp_dir().join(format!("fab-paint-old-{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        let (fab, home) = (dir.join("fab"), dir.join("Documents").join("Fab"));
        let file = fab.join("previous").join("1793600982-noise_vase").join("noise_vase.fab.splash");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, "use mod.fab.*\n\nscript(\"vase.cad.splash\")\n\nplace(\"vase\", object: \"vase\", filament: 3, color: #x3f89da, nozzle: @right)\n").unwrap();
        let store = fab_ai::plates::PlateStore::under(&fab, &home);
        let machine = crate::print::CheckSetup::fab_defaults().machine().unwrap();
        let order = Order { key: "m-old".into(), subject: Subject::Plate { store, id: "m-old".into(), file: Some(file) }, out: dir.join("m-old.png"), machine, alone: false };
        let why = paint(&order).expect_err("an earlier format's plate builds nothing").why;
        assert!(why.contains("unknown named arg"), "{why}");
        let heard = crate::model_host::filed::take();
        assert!(heard.is_empty(), "its picture's build filed {heard:?}");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
