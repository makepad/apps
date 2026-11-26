//! Scene kits: ready-made setups an author (or a model) calls by name —
//! `Device{kind: @phone screen: ...}`, `Turntable{...}`, `Field{preset:
//! @galaxy}`, `ShapeSet{preset: @floating}`. Each kit is a Splash template
//! (below) that builds ordinary nodes from the author's object; its
//! parameters' defaults live on the kit's prototype, so `p.count` is the
//! author's value or the default. Templates are plain Splash on purpose:
//! they double as worked examples of the node vocabulary.

use crate::script::{Reader, TYPES};
use makepad_widgets::*;

/// The kits' prototypes (with defaults), the screen helper material and the
/// template functions, evaluated once into `mod.scene`.
pub const KIT_PRELUDE: &str = include_str!("kits.splash");

/// Register the kits' templates and prototypes (after `script::register`).
/// The render graph's kits (`Glow`, `Halation`, ...) are `mod.post`'s.
pub fn install(vm: &mut ScriptVm) {
    let scene = vm.bx.heap_ref().value(vm.modules(), id!(scene).into(), NoTrap).as_object();
    let Some(scene) = scene else { return };
    // `screen_material(v)`: a device screen's material from what the author
    // gave — @demo, a media ref (image), a texture name (video/app-run
    // frames from the host), or a material; nothing = a dark screen.
    vm.add_method(scene, id_lut!(screen_material), script_args!(v = NIL, base = NIL, fit = NIL, crop = NIL, aspect = NIL), |vm, args| {
        let v = vm.bx.heap_ref().value(args, id!(v).into(), NoTrap);
        // `base`: the material the picture goes on (default unlit).
        let base_arg = vm.bx.heap_ref().value(args, id!(base).into(), NoTrap);
        let module = vm.bx.heap_ref().value(vm.modules(), id!(scene).into(), NoTrap).as_object().unwrap();
        let get = |vm: &ScriptVm, k: LiveId| vm.bx.heap_ref().value(module, k.into(), NoTrap);
        if v.is_nil() || v.is_err() {
            return get(vm, id!(ScreenOff));
        }
        if let Some(id) = v.as_id() {
            if id == id!(demo) {
                return get(vm, id!(DemoScreen));
            }
            if id == id!(off) {
                return get(vm, id!(ScreenOff));
            }
        }
        let basic = if base_arg.is_nil() || base_arg.is_err() { get(vm, id!(MeshBasicMaterial)) } else { base_arg };
        let o = vm.bx.heap_mut().new_with_proto(basic);
        // A picture on a screen never stretches: `fit` (default @cover),
        // an optional `crop` (a reframe's keys) and the screen's aspect.
        for name in ["fit", "crop", "aspect"] {
            let k = LiveId::from_str_with_lut(name).unwrap_or(LiveId::from_str(name));
            let a = vm.bx.heap_ref().value(args, LiveId::from_str(name).into(), NoTrap);
            if !a.is_nil() && !a.is_err() {
                vm.bx.heap_mut().set_value_def(o, k.into(), a);
            } else if name == "fit" {
                vm.bx.heap_mut().set_value_def(o, k.into(), LiveId::from_str_with_lut("cover").unwrap_or(LiveId::from_str("cover")).into());
            }
        }
        if v.is_string_like() {
            vm.bx.heap_mut().set_value_def(o, LiveId::from_str_with_lut("texture").unwrap_or(id!(texture)).into(), v);
            return o.into();
        }
        // A material object passes through; a media ref becomes a map.
        if v.as_object().is_some() && !crate::script::is_animation_value(vm.bx.heap_ref(), v) {
            return v;
        }
        vm.bx.heap_mut().set_value_def(o, LiveId::from_str_with_lut("map").unwrap_or(id!(map)).into(), v);
        o.into()
    });
    vm.capture_errors();
    let v = vm.eval(ScriptMod::new("", "", "motion3d://kits", 0, 0, &(KIT_PRELUDE), &[]));
    let errors = vm.take_errors();
    if cfg!(test) {
        assert!(errors.is_empty() && !v.is_err(), "motion3d kits failed to evaluate: {errors:?}");
    }
    if !errors.is_empty() {
        log!("motion3d kits: {}", errors.join("; "));
    }
}

/// The template to call for a kit object, and the parameters it takes.
pub fn template(r: &mut Reader, o: ScriptObject, kind: &str) -> Result<(String, &'static [&'static str]), String> {
    let pick = |r: &mut Reader, field: &str, default: &str, names: &[&str], what: &str| -> Result<String, String> {
        let name = r.id(o, field).unwrap_or_else(|| default.to_string());
        if names.contains(&name.as_str()) {
            Ok(name)
        } else {
            Err(format!(
                "no {what} `@{name}`{}; one of {}",
                crate::anim::closest(&name, names.iter().copied()).map(|c| format!(" (did you mean `@{c}`?)")).unwrap_or_default(),
                names.iter().map(|n| format!("@{n}")).collect::<Vec<_>>().join(" ")
            ))
        }
    };
    Ok(match kind {
        "device" => {
            let k = pick(r, "kind", "phone", DEVICES, "device")?;
            (format!("kit_device_{k}"), &["kind", "screen", "body", "bezel", "open", "fit", "crop", "aspect"])
        }
        "turntable" => ("kit_turntable".into(), &["speed", "plinth", "plinth_radius", "plinth_height", "plinth_material"]),
        "crowd" => ("kit_crowd".into(), &["count", "shape", "variants", "seed", "radius", "spacing", "tier", "arc", "color", "color2", "eyes", "eyes2", "react", "hop", "bob", "arms", "cheer", "signs", "sign_color", "stagger", "start"]),
        "callout" => ("kit_callout".into(), &["target", "offset", "text", "font", "size", "color", "color2", "start"]),
        "spotlights" => ("kit_spotlights".into(), &["count", "spread", "height", "angle", "color", "color2", "intensity", "react", "sweep", "start"]),
        "field" => {
            let k = pick(r, "preset", "galaxy", FIELDS, "field preset")?;
            (format!("kit_field_{k}"), &["count", "size", "color", "color2", "radius", "height", "grid", "spacing", "speed", "react", "start", "seed"])
        }
        "shape_set" => {
            let k = pick(r, "preset", "floating", SHAPE_SETS, "shape set")?;
            (format!("kit_shapes_{k}"), &["color", "color2", "color3", "spread", "speed", "start", "stagger"])
        }
        "fx" => {
            let k = pick(r, "preset", "burst", FX, "effect")?;
            (format!("kit_fx_{k}"), FX_PARAMS)
        }
        _ => return Err(format!("`{kind}` is not a kit")),
    })
}

pub const DEVICES: &[&str] = &["phone", "tablet", "laptop", "monitor"];
pub const FIELDS: &[&str] = &["galaxy", "swarm", "wave_grid", "pillars", "confetti", "dots"];
pub const SHAPE_SETS: &[&str] = &["floating", "stack", "rings", "blobs"];
/// The effects library (`Fx{preset: ...}`), by family.
pub const FX: &[&str] = &[
    // particles
    "burst", "sparks", "confetti", "trail", "swarm_path", "text_swarm", "text_sparks", "snow", "embers",
    // instanced fields
    "cube_grid", "cube_sphere", "wave", "helix", "voxel_morph", "voxel_text",
    // deformers
    "shatter", "ripple_pool", "twister", "bender",
    // procedural geometry
    "ribbons", "tree", "metaballs", "tube_draw", "ribbon_path", "vase", "svg_logo",
    // transitions
    "cube_flip", "page_curl", "portal", "depth_push",
    // round 7: a glowing wire maze, a mesh shattering into particles on a hit
    "wire_maze", "shatter_burst",
    // an extruded pattern slab (a heightfield of blocks, the picture on top)
    "pattern_slab",
    // line worlds, gravity and orbits, path-built instances
    "contour_terrain", "gravity_well", "accretion_disk", "paperclips", "braid",
];
pub const FX_PARAMS: &[&str] = &[
    "count", "size", "color", "color2", "start", "duration", "radius", "speed", "seed", "text", "font", "curve", "amount", "from", "to", "width", "height",
    "geometry", "material", "svg", "depth", "grid", "spacing", "resolution", "height",
];

#[allow(dead_code)]
fn _types_linked() -> usize {
    TYPES.len()
}

#[cfg(test)]
mod tests {
    use crate::script::DocVm;
    use makepad_widgets::*;
    use std::collections::HashMap;

    #[test]
    fn every_kit_template_is_defined() {
        let mut doc = DocVm::new(HashMap::new());
        doc.with(|vm| {
            let m3 = vm.bx.heap_ref().value(vm.modules(), id!(scene).into(), NoTrap).as_object().unwrap();
            let mut names: Vec<String> = super::DEVICES.iter().map(|d| format!("kit_device_{d}")).collect();
            names.extend(super::FIELDS.iter().map(|d| format!("kit_field_{d}")));
            names.extend(super::SHAPE_SETS.iter().map(|d| format!("kit_shapes_{d}")));
            names.push("kit_turntable".into());
            names.push("kit_crowd".into());
            names.push("kit_spotlights".into());
            names.push("kit_callout".into());
            names.extend(super::FX.iter().map(|d| format!("kit_fx_{d}")));
            for n in names {
                let v = vm.bx.heap_ref().value(m3, LiveId::from_str(&n).into(), NoTrap);
                assert!(!v.is_err() && !v.is_nil(), "{n} is not defined");
            }
        });
    }
}
