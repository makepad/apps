//! `character_new({...})`: a designed, rigged and animated character
//! from one spec, `fps_arms_new` and `character_presets()`.
//!
//! The spec is handed whole to the model worker as the `character` program
//! operation (libs/model `character.rs`), which generates the skinned body,
//! face rig, hair, clothing, gear and the full clip set. Like
//! `game.model_new`, the result is cached by content and installed as the
//! library alias `gen/mdl/char-<name>`, usable by `game.character`,
//! `game.player_character`, `game.avatar` and anything else that takes a
//! character model. The spec is validated here so typos surface at once.

use crate::value::*;
use crate::ModelHost;
use makepad_model::ProgramOp;
use makepad_script::*;

fn slug(name: &str) -> String {
    let slug: String = name.trim().to_ascii_lowercase().chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect();
    slug.split('-').filter(|p| !p.is_empty()).collect::<Vec<_>>().join("-")
}

pub fn v_character_new(vm: &mut ScriptVm, host: &dyn ModelHost, args: ScriptObject) -> ScriptValue {
    let v = arg(vm, args, 0);
    let spec = match crate::character_spec::read(vm, v) {
        Ok(spec) => spec,
        Err(e) => { host.log(format!("game.character_new: {e}")); return NIL; }
    };
    for w in &spec.warnings { host.log(format!("game.character_new: {w}")); }
    let slug = slug(&spec.name);
    let alias = format!("{}char-{}", crate::model_verbs::SCRIPT_MODEL_PREFIX, if slug.is_empty() { "character".into() } else { slug });
    let ao = ProgramOp::Weathering { ao: 0.55, distance: 0.08, edges: 0.25, samples: 12, objects: None };
    let program = vec![ProgramOp::Character(Box::new(spec)), ao];
    crate::model_verbs::request_model_for(host, &alias, "character", program);
    if let crate::model_verbs::ScriptModelState::Failed(e) = crate::model_verbs::script_model_state(&alias) {
        host.log(format!("game.character_new('{alias}'): build failed: {e}"));
    }
    vm.bx.heap_mut().new_string_from_str(&alias)
}

pub fn v_fps_arms_new(vm: &mut ScriptVm, host: &dyn ModelHost, args: ScriptObject) -> ScriptValue {
    let v = arg(vm, args, 0);
    let (spec, arms) = match crate::character_spec::read_arms(vm, v) {
        Ok(read) => read,
        Err(e) => { host.log(format!("game.fps_arms_new: {e}")); return NIL; }
    };
    let hold = format!("{:?}", arms.hold).to_ascii_lowercase();
    let alias = format!("{}arms-{}", crate::model_verbs::SCRIPT_MODEL_PREFIX, slug(&format!("{}-{hold}", spec.name)));
    let program = vec![ProgramOp::FpsArms(Box::new(spec), arms)];
    crate::model_verbs::request_model_for(host, &alias, "weapon", program);
    vm.bx.heap_mut().new_string_from_str(&alias)
}

pub fn v_character_presets(vm: &mut ScriptVm) -> ScriptValue {
    let list = vm.bx.heap_mut().new_array();
    for name in crate::character_spec::preset_names() {
        let s = vm.bx.heap_mut().new_string_from_str(&name);
        vm.array_push_trap(list, s);
    }
    list.into()
}
