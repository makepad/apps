//! Modelled props and characters in a Scene3D (KERNELS.md §3.10, PM2):
//! Sandbox's modelling pipeline, used from Stage scenes.
//!
//! ```text
//! use mod.model.*                       // model_new, character_new, character_presets, ...
//! use mod.libs.alien_flora.*            // a model kit, when the host installed its kits
//! let rio = character_new({preset: "adult" name: "stage-rio"})
//! let crate = fn(name) {
//!     let m = model_new(name, {class: "prop"})
//!     let wood = m.material({color: #x8a5a32, roughness: 0.8})
//!     let body = m.box({size: vec3(0.6, 0.6, 0.6) material: wood})
//!     m.collider(body, "box")
//!     return m.build()                  // the alias, "gen/mdl/<name>"
//! }
//! Scene3D{ children: [Model{src: rio animation: "walk"} Model{src: crate("c")}] }
//! ```
//!
//! `mod.model` is `makepad-model-script`, the builder `game.model_new` and
//! `game.character_new` are in Sandbox: the same program, hash and GLB. A
//! built alias placed as `Model{src}`, `Instances{model}` or a physics
//! `Body{model}` / `Spawn{model}` resolves through Stage's model host
//! (`makepad-stage-model-host`: the build service over Stage's own store).
//! While a model builds the document loads without it and says it is
//! waiting ([`crate::doc::Scene3dDoc::waiting_for_models`]); once the
//! product installs the host reloads the document
//! ([`crate::doc::Scene3dDoc::models_changed`]). Locked time never shows a
//! frame without its models (MustComplete).
//!
//! Kits are `<name>.splash` files in the kit folders a host installs
//! ([`install_kit_folder`]; Stage installs the game kits Sandbox's games
//! read): a document names one as `mod.libs.<name>` and gets its
//! functions (a kit's `game` is `mod.model`; its placement helpers are
//! Sandbox's and do nothing useful here). A host without one has no kits.

use crate::glb::GlbModel;
use crate::script::{Diagnostic, Loc};
use makepad_model_script::{character_verb, model_verbs, ModelHost};
#[cfg(feature = "models")]
use makepad_stage_model_host::{self as host, Built};
#[cfg(not(feature = "models"))]
use host::Built;
use makepad_widgets::makepad_script::EvalLimits;
use makepad_widgets::*;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

#[cfg(feature = "models")]
pub use makepad_stage_model_host::{building, generation, is_script_model, progress_text, pump};
#[cfg(not(feature = "models"))]
pub use host::{building, generation, is_script_model, progress_text, pump};

/// Without the `models` feature no model builds: a scene that places a
/// built model draws without it and says so (a player of finished
/// documents has no store to build into).
#[cfg(not(feature = "models"))]
mod host {
    pub use makepad_model_script::model_verbs::is_script_model;
    use makepad_widgets::Cx;
    use std::sync::Arc;

    #[allow(dead_code)]
    pub enum Built {
        Ready { hash: String, glb: Arc<[u8]> },
        Building,
        Failed(String),
        Unknown,
    }

    pub fn product(alias: &str) -> Built {
        if is_script_model(alias) {
            Built::Failed("this app does not build models (a Stage feature)".into())
        } else {
            Built::Unknown
        }
    }

    pub fn generation() -> u64 {
        0
    }

    pub fn building() -> bool {
        false
    }

    pub fn progress_text() -> Option<String> {
        None
    }

    pub fn pump(_cx: &mut Cx) -> bool {
        false
    }
}

/// The builder's messages (an unknown option, a spec warning): the
/// document's warnings.
struct DocModelHost {
    messages: Rc<RefCell<Vec<String>>>,
}

impl ModelHost for DocModelHost {
    fn log(&self, message: String) {
        self.messages.borrow_mut().push(message);
    }
}

fn doc_host(vm: &ScriptVm) -> Rc<dyn ModelHost> {
    let messages = crate::script::host(vm)
        .map(|h| h.model_messages.clone())
        .unwrap_or_default();
    Rc::new(DocModelHost { messages })
}

/// `mod.model`: the shared builders, as Sandbox's `game.*` verbs call them.
pub fn register(vm: &mut ScriptVm) {
    let m = vm.new_module(id!(model));
    vm.add_method(m, id_lut!(model_new), script_args!(), |vm, args| {
        let host = doc_host(vm);
        model_verbs::v_model_new(vm, &host, args)
    });
    vm.add_method(m, id_lut!(character_new), script_args!(), |vm, args| {
        let host = doc_host(vm);
        character_verb::v_character_new(vm, &*host, args)
    });
    vm.add_method(m, id_lut!(fps_arms_new), script_args!(), |vm, args| {
        let host = doc_host(vm);
        character_verb::v_fps_arms_new(vm, &*host, args)
    });
    vm.add_method(m, id_lut!(character_presets), script_args!(), |vm, _| character_verb::v_character_presets(vm));
    vm.add_method(m, id_lut!(build_progress), script_args!(), |vm, _| model_verbs::v_build_progress(vm));
    vm.add_method(m, id_lut!(model_status), script_args!(), |vm, args| model_verbs::v_model_status(vm, args));
}

// ── kits ────────────────────────────────────────────────────────────────

static KIT_FOLDERS: std::sync::RwLock<Vec<std::path::PathBuf>> = std::sync::RwLock::new(Vec::new());

/// A folder of model kits (`<name>.splash`) a host installs, read fresh at
/// each load: `use mod.libs.<name>.*` names one of its files. Installing a
/// folder again does nothing.
pub fn install_kit_folder(dir: std::path::PathBuf) {
    let mut folders = KIT_FOLDERS.write().unwrap_or_else(|e| e.into_inner());
    if !folders.contains(&dir) {
        folders.push(dir);
    }
}

/// How a document names a kit: `use mod.libs.<name>.*`.
const NAMED: &str = "mod.libs.";

/// Evaluates the kits `source` names (`use mod.libs.<name>.*`) as modules of
/// `makepad-script-imports`, dependencies first, into `mod.libs`, before the
/// document itself evaluates. A kit sees the language core and `game` as
/// `mod.model`.
pub(crate) fn load_kits(vm: &mut ScriptVm, source: &str) -> Result<(), Vec<Diagnostic>> {
    use makepad_script_imports as imports;
    // (The document's pinned library revisions are the host's, loaded already.)
    let wanted: Vec<imports::ModuleRef> = imports::scan(source, Some(NAMED)).into_iter().filter(|r| matches!(r, imports::ModuleRef::Named(_))).collect();
    if wanted.is_empty() {
        return Ok(());
    }
    let at = |message: String| Diagnostic::error(Loc { line: 1, col: 1 }, "kit".to_string(), message);
    let folders = KIT_FOLDERS.read().unwrap_or_else(|e| e.into_inner()).clone();
    if folders.is_empty() {
        let names = wanted.iter().map(|r| format!("`mod.libs.{}`", r.key())).collect::<Vec<_>>().join(", ");
        return Err(vec![at(format!("{names}: no model kits are installed here (a host installs its kit folders)"))]);
    }
    let folder = imports::FolderModules::new(folders, "kits");
    let loaded = imports::resolve(&wanted, &folder, Some(NAMED)).map_err(|errors| errors.into_iter().map(at).collect::<Vec<_>>())?;
    let libs = vm.new_module(id!(libs));
    let mut uses = imports::language_core(vm).to_vec();
    let game = vm.bx.heap_mut().new_object();
    let model = vm.module(id!(model));
    vm.bx.heap_mut().set_value_def(game, id!(game).into(), model.into());
    uses.push(game);
    let (errors, report) = vm.eval_bounded(&EvalLimits::DOC_LOAD, |vm| imports::load(vm, libs, &uses, &loaded, Some(libs), usize::MAX));
    if let Some(limit) = report.limit() {
        return Err(vec![at(format!("a kit did not load: {limit}"))]);
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.iter().map(|e| at(e.text().to_owned())).collect())
    }
}

// ── built products ──────────────────────────────────────────────────────

/// A built model a document placed, as the load found it.
#[derive(Clone, Debug, PartialEq)]
pub struct ModelUse {
    pub alias: String,
    /// The installed revision; `None` while it was building.
    pub hash: Option<String>,
}

pub(crate) enum BuiltGlb {
    Ready(Arc<GlbModel>, Arc<[u8]>),
    Pending,
    Failed(String),
}

thread_local! {
    /// Parsed products by revision: a document reloads (every edit, every
    /// arrival) without parsing a character again.
    static PARSED: RefCell<HashMap<String, (Arc<GlbModel>, u64)>> = RefCell::new(HashMap::new());
    static CLOCK: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// Parsed products kept for reloads; the least recently used go first.
const PARSED_KEPT: usize = 32;

fn parsed(hash: &str, glb: &[u8]) -> Result<Arc<GlbModel>, String> {
    let now = CLOCK.with(|c| {
        c.set(c.get() + 1);
        c.get()
    });
    if let Some(m) = PARSED.with(|p| p.borrow_mut().get_mut(hash).map(|e| {
        e.1 = now;
        e.0.clone()
    })) {
        return Ok(m);
    }
    let model = Arc::new(GlbModel::load(glb, None)?);
    PARSED.with(|p| {
        let mut p = p.borrow_mut();
        p.insert(hash.to_string(), (model.clone(), now));
        while p.len() > PARSED_KEPT {
            let Some(oldest) = p.iter().min_by_key(|(_, (_, used))| *used).map(|(k, _)| k.clone()) else { break };
            p.remove(&oldest);
        }
    });
    Ok(model)
}

/// A built model by alias (`None`: not one; the name is a file). The use
/// is recorded for the document's waiting and reloading.
pub(crate) fn built_glb(uses: &mut Vec<ModelUse>, alias: &str) -> Option<BuiltGlb> {
    if !is_script_model(alias) {
        return None;
    }
    let (out, hash) = match host::product(alias) {
        Built::Unknown => return None,
        Built::Building => (BuiltGlb::Pending, None),
        Built::Failed(e) => (BuiltGlb::Failed(e), None),
        Built::Ready { hash, glb } => match parsed(&hash, &glb) {
            Ok(model) => (BuiltGlb::Ready(model, glb), Some(hash)),
            Err(e) => (BuiltGlb::Failed(format!("the built GLB did not load: {e}")), Some(hash)),
        },
    };
    let entry = ModelUse { alias: alias.to_string(), hash };
    if !uses.contains(&entry) {
        uses.push(entry);
    }
    Some(out)
}

/// Some model a document placed is still building.
pub fn waiting(uses: &[ModelUse]) -> bool {
    uses.iter().any(|u| u.hash.is_none() && matches!(host::product(&u.alias), Built::Building))
}

/// Some model a document placed installed (or failed, or changed) since it
/// loaded: load it again.
pub fn changed(uses: &[ModelUse]) -> bool {
    uses.iter().any(|u| match host::product(&u.alias) {
        Built::Ready { hash, .. } => u.hash.as_deref() != Some(hash.as_str()),
        Built::Building => u.hash.is_some(),
        Built::Failed(_) | Built::Unknown => true,
    })
}

// ── colliders ───────────────────────────────────────────────────────────

/// A model's collider (its `m.collider` metadata, `MAKEPAD_collision` in
/// the GLB) in model coordinates: every proxy's points as one convex hull
/// for a dynamic body, their triangles as a mesh for a fixed or kinematic
/// one.
pub(crate) fn collider(glb: &[u8], dynamic: bool) -> Result<makepad_effect_sim::Shape, String> {
    let loaded = makepad_gltf::load_gltf_from_bytes(glb, None).map_err(|e| e.to_string())?;
    let proxies = makepad_render::asset_metadata::prepare_asset_collisions(&loaded)?;
    if proxies.is_empty() {
        return Err("the model declares no collider: add `m.collider(\"part\", \"box\")` (or \"sphere\", \"hull\", \"mesh\")".into());
    }
    let mut vertices: Vec<[f32; 3]> = Vec::new();
    let mut indices: Vec<u32> = Vec::new();
    for p in proxies.iter() {
        let base = vertices.len() as u32;
        vertices.extend_from_slice(&p.mesh.positions);
        indices.extend(p.mesh.indices.iter().map(|i| base + i));
    }
    Ok(if dynamic {
        makepad_effect_sim::Shape::Hull(Arc::new(vertices))
    } else {
        makepad_effect_sim::Shape::Mesh { vertices: Arc::new(vertices), indices: Arc::new(indices) }
    })
}

// ── a model as instance geometry ────────────────────────────────────────

/// The model's static meshes in model space, one mesh per material (the
/// rest pose; skinned parts at their bind pose).
pub(crate) fn meshes_by_material(model: &GlbModel) -> Vec<(Option<usize>, makepad_geom3d::mesh::Mesh)> {
    let pose = model.rest_pose();
    let world = model.world_matrices(&pose);
    let mut out: Vec<(Option<usize>, makepad_geom3d::mesh::Mesh)> = Vec::new();
    for (node, mesh) in model.scene_nodes() {
        for (k, prim) in model.meshes[mesh].primitives.iter().enumerate() {
            let (mut posed, space) = model.posed_primitive(node, k, &pose, &world);
            if space == crate::glb::MeshSpace::NodeLocal {
                let m = &world[node];
                for p in &mut posed.positions {
                    *p = makepad_geom3d::math::transform_point(m, *p);
                }
                for n in &mut posed.normals {
                    let d = makepad_geom3d::math::transform_dir(m, *n);
                    let l = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt().max(1e-12);
                    *n = [d[0] / l, d[1] / l, d[2] / l];
                }
                for t in &mut posed.tangents {
                    let d = makepad_geom3d::math::transform_dir(m, [t[0], t[1], t[2]]);
                    let l = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt().max(1e-12);
                    *t = [d[0] / l, d[1] / l, d[2] / l, t[3]];
                }
            }
            match out.iter_mut().find(|(m, _)| *m == prim.material) {
                Some((_, into)) => into.append(&posed),
                None => out.push((prim.material, posed)),
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use crate::doc::{FileResolver, LoadOptions, Scene3dDoc};
    use std::collections::HashMap;

    /// The builder's mailbox is process-wide: tests that drain it take turns.
    static MAILBOX: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn load(src: &str) -> Scene3dDoc {
        let resolver = FileResolver::default();
        match Scene3dDoc::load(src, LoadOptions { name: "models".into(), inputs: HashMap::new(), resolver: &resolver, first_line: 1, origin_file: None, song: None }) {
            Ok(d) => d,
            Err(e) => panic!("{}", e.iter().map(|d| d.to_string()).collect::<Vec<_>>().join("\n")),
        }
    }

    #[test]
    fn kit_names_come_from_code_not_strings_or_comments() {
        let src = "use mod.libs.alien_flora.*\n// mod.libs.station\nlet a = \"mod.libs.city_cars\"\nlet b = mod.libs.foliage.fol_opt\n";
        let names: Vec<String> = makepad_script_imports::scan(src, Some(super::NAMED)).iter().map(|r| r.key()).collect();
        assert_eq!(names, vec!["alien_flora".to_string(), "foliage".to_string()]);
    }

    /// `mod.model` in a Scene3D: the builders post their programs, and
    /// every placed built model waits for its build (no host pumps here:
    /// nothing installs). A kit with no kit folder installed is refused.
    #[test]
    fn a_scene_builds_with_the_shared_builders_and_waits_for_them() {
        let _turn = MAILBOX.lock().unwrap_or_else(|e| e.into_inner());
        let resolver = FileResolver::default();
        let kit = Scene3dDoc::load("use mod.scene.*\nuse mod.libs.alien_flora.*\nScene3D{children: []}", LoadOptions { name: "kit".into(), inputs: HashMap::new(), resolver: &resolver, first_line: 1, origin_file: None, song: None });
        let Err(diags) = kit else { panic!("a kit loaded with no kit folder installed") };
        assert!(diags.iter().any(|d| d.message.contains("`mod.libs.alien_flora`: no model kits are installed here")), "{diags:?}");
        let doc = load(
            r#"use mod.scene.*; use mod.model.*
let crate = fn(name) {
    let m = model_new(name, {class: "prop"})
    let body = m.box({size: vec3(0.6, 0.6, 0.6) material: "wood"})
    m.collider(body, "box")
    return m.build()
}
Scene3D{ lights: [AmbientLight{}]
    physics: Physics{spawn: [Spawn{at: 0 group: "c" count: 2 model: crate("pm2_test_crate") from: [Body{position: vec3(0, 1, 0)} Body{position: vec3(0, 2, 0)}]}]}
    children: [
        Model{src: crate("pm2_test_crate")}
        Instances{source: bodies("c") model: crate("pm2_test_crate")}
    ] }"#,
        );
        let aliases: Vec<&str> = doc.models.iter().map(|m| m.alias.as_str()).collect();
        assert_eq!(aliases, vec!["gen/mdl/pm2-test-crate"]);
        assert!(doc.models.iter().all(|m| m.hash.is_none()));
        assert!(doc.waiting_for_models());
        assert!(!doc.models_changed());
        // The nodes wait: only the light is placed until the models land.
        assert_eq!(doc.def.root.children.len(), 1);
        assert!(matches!(doc.def.root.children[0].kind, crate::scene::NodeKind::Light(_)));
        let posted = makepad_model_script::model_verbs::pending_script_models();
        assert!(posted.contains("gen/mdl/pm2-test-crate"));
        // Its collider is its `m.collider` metadata: the 0.6 m box, as a hull
        // for a dynamic body and as triangles for a fixed one.
        let request = makepad_model_script::model_verbs::take_script_model_requests().into_iter().find(|r| r.alias == "gen/mdl/pm2-test-crate").expect("posted");
        let mut fetch = |a: &str| -> Result<Vec<u8>, String> { Err(format!("no fetch for {a}")) };
        let glb = makepad_model::build_program(&request.program, makepad_model::Limits::default(), &mut fetch, None).map_err(|e| e.to_string()).unwrap().compiled.glb;
        match super::collider(&glb, true).unwrap() {
            makepad_effect_sim::Shape::Hull(points) => {
                assert_eq!(points.len(), 8);
                assert!(points.iter().all(|p| p.iter().all(|v| (v.abs() - 0.3).abs() < 1e-4)), "{points:?}");
            }
            other => panic!("a dynamic body's collider is a hull, not {other:?}"),
        }
        assert!(matches!(super::collider(&glb, false).unwrap(), makepad_effect_sim::Shape::Mesh { indices, .. } if indices.len() == 36));
        // As instance geometry: one mesh per material, in model space.
        let model = crate::glb::GlbModel::load(&glb, None).unwrap();
        let parts = super::meshes_by_material(&model);
        assert_eq!(parts.len(), 1);
        assert!(parts[0].1.positions.iter().all(|p| p.iter().all(|v| v.abs() <= 0.3 + 1e-4)));
    }

    /// The same clip time gives the same pose in Stage (motion3d's GLB
    /// sampling) as in Sandbox (the render core's `SkinnedModel`): a
    /// character-lab character built by the shared builder walks at the same
    /// clip times in both.
    #[test]
    fn a_character_lab_character_walks_at_the_same_clip_times_as_in_sandbox() {
        let _turn = MAILBOX.lock().unwrap_or_else(|e| e.into_inner());
        let doc = load("use mod.scene.*; use mod.model.*\nlet c = character_new(\"halcyon_warden\")\nScene3D{ lights: [AmbientLight{}] children: [Model{src: c animation: \"walk\"}] }");
        assert_eq!(doc.models.len(), 1);
        let alias = doc.models[0].alias.clone();
        let request = makepad_model_script::model_verbs::take_script_model_requests().into_iter().find(|r| r.alias == alias).expect("the character's program was posted");
        let mut fetch = |a: &str| -> Result<Vec<u8>, String> { Err(format!("no fetch for {a}")) };
        let limits = makepad_model::Limits::default();
        let built = makepad_model::build_program(&request.program, limits, &mut fetch, None).map_err(|e| e.to_string()).expect("the character builds");
        let glb = built.compiled.glb;
        let stage = crate::glb::GlbModel::load(&glb, None).expect("motion3d loads it");
        let sandbox = makepad_render::skin::SkinnedModel::parse_glb(&glb).expect("the render core loads it");
        let walk = stage.clip_by_name("walk").expect("a walk clip");
        let sandbox_walk = sandbox.clip_index_exact("walk").expect("a walk clip");
        assert_eq!(sandbox.clip_index("walk"), Some(sandbox_walk));
        let duration = stage.clips[walk].duration;
        assert!((duration - sandbox.clips[sandbox_walk].duration).abs() < 1e-5);
        let skin_node = stage.nodes.iter().position(|n| n.skin.is_some()).expect("a skinned node");
        let skin = &stage.skins[stage.nodes[skin_node].skin.unwrap()];
        let mut compared = 0;
        for &t in &[0.0f32, 0.1, 0.37, 0.5, 0.73, 1.0, 1.5, 2.9, 7.3] {
            // Stage: a looping clip at (t - offset) * speed, wrapped.
            let mut pose = stage.rest_pose();
            stage.apply_clip(&mut pose, walk, t.rem_euclid(duration.max(1e-6)), 1.0);
            let world = stage.world_matrices(&pose);
            let mesh_world = world[skin_node];
            let mut palette = Vec::new();
            let mut sandbox_pose = Vec::new();
            sandbox.sample_clip(sandbox_walk, t, &mut sandbox_pose);
            sandbox.palette(&sandbox_pose, &mut palette);
            assert_eq!(palette.len(), skin.joints.len());
            for (j, &node) in skin.joints.iter().enumerate() {
                let stage_m = makepad_geom3d::math::mul(&world[node], &skin.inverse_bind[j]);
                // The render core's palette is relative to the skinned mesh's
                // node; Stage's skinned vertices are in the model's space.
                let stage_m = makepad_geom3d::math::mul(&mesh_world.invert(), &stage_m);
                for k in 0..16 {
                    let (a, b) = (stage_m.v[k], palette[j].v[k]);
                    assert!((a - b).abs() <= 1e-3 * (1.0 + b.abs()), "t {t}: joint {j} element {k}: stage {a} sandbox {b}");
                }
                compared += 1;
            }
        }
        assert!(compared > 9 * 20, "compared {compared} joint matrices");
    }
}
