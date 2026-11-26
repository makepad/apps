//! Named style kits: `Motion{style: @swiss …}` in one line. A kit bundles
//! a palette, a type pairing (bundled fonts), a motion language (the
//! typography animators and their pace), background plates, ornaments
//! (icons, SVG paths, Repeat), post effects (render-kit grain, glow,
//! vignette, …, and a halftone screen) and the matching Text3D and
//! KineticText looks.
//!
//! The kits are Splash: the built-in module `mod.motion.modules.style`
//! (`modules/style.splash`), each kit an object of data and functions of
//! the frame. `style:` is lowered in the document's VM right after it
//! evaluates ([`expand`], `style_apply.splash`), to plain layers:
//! `style.plate` under the author's layers, `style.ornaments` and
//! `style.screen` over them, all inside `style.post` (a Group carrying the
//! post effects); and to fields on the author's Text / Text3D /
//! KineticText layers, set only where the author has not set them. Every
//! piece is overridable: a field on a layer wins, and
//! `style: {kit: @swiss accent: #ff0000 display: "Anton" plate: false …}`
//! changes the kit itself. A document can use the module directly
//! (`style_kits.swiss.plate(k)`; `makepad-motion style <kit>` prints how),
//! and edit a copy of a kit as ordinary layers.

use crate::obj::{self, Look};
use makepad_script::*;

/// A face: family, weight, letter spacing (a fraction of the size),
/// italic, width (`stretch`, Roboto Flex only).
#[derive(Clone, Debug)]
pub struct Face {
    pub font: String,
    pub weight: f64,
    pub tracking: f64,
    pub italic: bool,
    pub stretch: f64,
}

/// A style kit's data, as `modules/style.splash` declares it (its layer
/// functions stay in Splash).
#[derive(Clone, Debug)]
pub struct Kit {
    pub name: String,
    pub look: String,
    pub suits: String,
    /// bg, ink, accent, accent2, muted as 0xrrggbb.
    pub palette: [u32; 5],
    /// Titles (a Text of size 56 or more, or no size).
    pub display: Face,
    /// Everything smaller.
    pub body: Face,
    /// Display titles in capitals.
    pub upper: bool,
    /// The face of `role: @label` Texts (tiny tracked capitals).
    pub label: String,
    /// Display line height (a multiple of the size).
    pub leading: f64,
    pub animate: String,
    pub body_animate: String,
    /// The animators' pace (1 = their own).
    pub speed: f64,
    /// Text3D preset for a `Text3D` without one.
    pub text3d: String,
    /// Kinetic type kit for a `KineticText` without one.
    pub kinetic: String,
    /// vj_fx background effect (`style: {kit: … fx: true}`), or "".
    pub fx: String,
}

pub const PALETTE_NAMES: [&str; 5] = ["bg", "ink", "accent", "accent2", "muted"];

/// The style lowering's module (it uses the kits' module).
const APPLY: &str = include_str!("style_apply.splash");

fn read_kit(vm: &ScriptVm, name: &str, value: &Look) -> Result<Kit, String> {
    let bad = |field: &str| format!("style kit `{name}`: `{field}` is missing or of the wrong kind");
    let o = value.obj().ok_or_else(|| bad("{…}"))?;
    let get = |o: ScriptObject, field: &str| obj::look_field(vm, o, field);
    let text = |field: &str| match get(o, field) {
        Some(Look::Str(s)) | Some(Look::Id(s)) => Ok(s),
        _ => Err(bad(field)),
    };
    let num = |field: &str| get(o, field).and_then(|v| v.num()).ok_or_else(|| bad(field));
    let font_of = |o: ScriptObject, field: &str| -> Result<crate::font_spec::FontSpec, String> {
        let v = obj::opt(vm, o, field).ok_or_else(|| bad(field))?;
        crate::font_spec::font_spec(vm, v)
    };
    let face = |field: &str| -> Result<Face, String> {
        let face = get(o, field).and_then(|v| v.obj()).ok_or_else(|| bad(field))?;
        Ok(Face {
            font: font_of(face, "font").map_err(|_| bad(&format!("{field}.font")))?.family.to_string(),
            weight: font_of(face, "font").map_err(|_| bad(&format!("{field}.font")))?.weight,
            tracking: get(face, "tracking").and_then(|v| v.num()).unwrap_or(0.0),
            italic: font_of(face, "font").map_err(|_| bad(&format!("{field}.font")))?.italic,
            stretch: get(face, "stretch").and_then(|v| v.num()).unwrap_or(100.0),
        })
    };
    let palette = get(o, "palette").and_then(|v| v.obj()).ok_or_else(|| bad("palette"))?;
    let mut colors = [0; 5];
    for (slot, key) in colors.iter_mut().zip(PALETTE_NAMES) {
        match get(palette, key) {
            Some(Look::Color(c)) => *slot = c >> 8,
            _ => return Err(bad(&format!("palette.{key}"))),
        }
    }
    Ok(Kit {
        name: name.into(),
        look: text("look")?,
        suits: text("suits")?,
        palette: colors,
        display: face("display")?,
        body: face("body")?,
        upper: matches!(get(o, "upper"), Some(Look::Bool(true))),
        label: font_of(o, "label").map_err(|_| bad("label"))?.family.to_string(),
        leading: num("leading")?,
        animate: text("animate")?,
        body_animate: text("body_animate")?,
        speed: num("speed")?,
        text3d: text("text3d")?,
        kinetic: text("kinetic")?,
        fx: text("fx")?,
    })
}

/// Every kit, in the module's order.
pub fn kits() -> Vec<Kit> {
    let text = crate::modules::builtin("style").expect("mod.motion.modules.style").text;
    let mut evaluated = crate::value::evaluate(&crate::model::DocSource::file(&text), crate::MotionContext::default().signals, 0.0, &[], &crate::modules::NoModules, None).unwrap_or_else(|diags| panic!("mod.motion.modules.style: {diags:?}"));
    assert!(evaluated.errors.is_empty(), "mod.motion.modules.style: {:?}", evaluated.errors);
    let root = evaluated.root;
    evaluated.with_vm(|vm| {
        let Some(Look::Obj(all, _)) = root.as_object().and_then(|o| obj::look_field(vm, o, "style_kits")) else { panic!("mod.motion.modules.style has no style_kits") };
        obj::fields(vm, all).into_iter().filter(|(name, _, _)| !name.starts_with("__")).map(|(name, kit, _)| read_kit(vm, &name, &obj::look(vm, kit)).unwrap_or_else(|e| panic!("mod.motion.modules.style: {e}"))).collect()
    })
}

pub fn kit(name: &str) -> Option<Kit> {
    kits().into_iter().find(|kit| kit.name == name)
}

/// The fields of `style: {kit: @… …}`.
const STYLE_FIELDS: &[&str] = &[
    "kit", "bg", "ink", "accent", "accent2", "muted", "display", "body", "animate", "body_animate", "speed", "upper",
    "plate", "ornaments", "screen", "post", "fx", "text3d", "kinetic",
];

/// A field's value, nil where `o` has none (a component's unset field).
fn get(vm: &ScriptVm, o: ScriptObject, key: &str) -> ScriptValue {
    let v = vm.bx.heap_ref().value(o, LiveId::from_str(key).into(), NoTrap);
    if v.is_err() { NIL } else { v }
}

fn names_of(vm: &ScriptVm, o: ScriptObject) -> Vec<String> {
    let mut out = Vec::new();
    vm.bx.heap_ref().map_entries(o).iter().for_each(|e| {
        if let Some(id) = e.key.as_id() {
            out.push(id.to_string());
        }
    });
    out
}

fn kind(v: ScriptValue) -> &'static str {
    if v.is_nil() { "nil" } else if v.as_number().is_some() { "a number" } else if v.as_bool().is_some() { "a bool" } else if v.as_color().is_some() { "a colour" } else if v.as_id().is_some() { "an id" } else if v.is_string_like() { "text" } else if v.as_array().is_some() { "a list" } else { "an object" }
}

/// Lower `style:` on the document `root` in its VM, in place (nothing
/// happens without one): the kit is checked here, then `style_apply`
/// builds the layers and sets the fields.
pub(crate) fn expand(vm: &mut ScriptVm, root: ScriptValue, errors: &mut Vec<ScriptErrorRecord>) {
    let Some(root) = root.as_object() else { return };
    let style = get(vm, root, "style");
    if style.is_nil() {
        return;
    }
    let at = style.as_object().unwrap_or(root);
    let err = |vm: &ScriptVm, errors: &mut Vec<ScriptErrorRecord>, message: String| errors.push(crate::modules::error_at(vm, at, &message));
    let name_value = match style.as_object() {
        Some(o) => get(vm, o, "kit"),
        None => style,
    };
    let name = match (name_value.as_id(), name_value.is_string_like()) {
        (Some(id), _) => id.to_string(),
        (None, true) => vm.bx.heap_ref().string_with(name_value, |_, s| s.to_string()).unwrap_or_default(),
        _ if style.as_object().is_some() => return err(vm, errors, "`style.kit`: `style: {…}` names its kit: `style: {kit: @swiss accent: #ff3b00}`".into()),
        _ => return err(vm, errors, format!("`style` is a kit (`style: @swiss`) or `{{kit: @swiss …}}`, not {}", kind(style))),
    };
    let exports = match crate::modules::load_internal(vm, "style_apply", APPLY) {
        Ok(exports) => exports,
        Err(e) => return errors.extend(e),
    };
    let kits = get(vm, exports, "style_kits").as_object().map(|k| names_of(vm, k)).unwrap_or_default();
    if !kits.contains(&name) {
        let names: Vec<&str> = kits.iter().map(|n| n.as_str()).collect();
        return err(vm, errors, format!("`@{name}` is not a style kit.{} The kits are {}", crate::load::did_you_mean(&name, &names), names.iter().map(|n| format!("@{n}")).collect::<Vec<_>>().join(" ")));
    }
    // The author's changes: known fields of the right kinds.
    if let Some(o) = style.as_object() {
        let animators: Vec<String> = crate::typography::animators().into_iter().map(|a| a.0).collect();
        let mut fields = Vec::new();
        vm.bx.heap_ref().map_entries(o).iter().for_each(|e| fields.push((e.key.as_id().map(|id| id.to_string()).unwrap_or_default(), e.value)));
        for (key, value) in fields {
            let ok = match key.as_str() {
                "kit" => true,
                "bg" | "ink" | "accent" | "accent2" | "muted" => value.as_color().is_some() || {
                    err(vm, errors, format!("`style.{key}` is a colour, not {}", kind(value)));
                    true
                },
                "display" | "body" => value.is_string_like() || {
                    err(vm, errors, format!("`style.{key}` is a font name, not {}", kind(value)));
                    true
                },
                "animate" | "body_animate" => match value.as_id() {
                    Some(id) => {
                        let id = id.to_string();
                        if id != "none" && !animators.contains(&id) {
                            err(vm, errors, format!("`style.{key}`: `@{id}` is not a typography animator (`makepad-motion animators` lists them), or @none"));
                        }
                        true
                    }
                    None => {
                        err(vm, errors, format!("`style.{key}` is an animator (`@rise`) or @none, not {}", kind(value)));
                        true
                    }
                },
                "speed" => value.as_number().is_some_and(|s| s > 0.0) || {
                    err(vm, errors, format!("`style.{key}` is a positive number, not {}", kind(value)));
                    true
                },
                "text3d" | "kinetic" => value.as_id().is_some() || value.is_string_like() || {
                    err(vm, errors, format!("`style.{key}` is a preset name, not {}", kind(value)));
                    true
                },
                "upper" | "plate" | "ornaments" | "screen" | "post" | "fx" => value.as_bool().is_some() || {
                    err(vm, errors, format!("`style.{key}` is true or false, not {}", kind(value)));
                    true
                },
                _ => false,
            };
            if !ok {
                err(vm, errors, format!("`style.{key}`: a style has no `{key}`.{} A style takes {}", crate::load::did_you_mean(&key, STYLE_FIELDS), STYLE_FIELDS.join(" ")));
            }
        }
    }
    // The frame: the document's size, 1920 x 1080 without one.
    let size = get(vm, root, "size");
    let size = if size.as_pod().is_some() {
        size
    } else {
        let ty = vm.pod_builtins().pod_vec2f;
        let pod = vm.bx.heap_mut().new_pod(ty);
        let data = vm.bx.heap_mut().pod_data_mut(pod);
        data[0] = 1920f32.to_bits();
        data[1] = 1080f32.to_bits();
        pod.into()
    };
    let apply = get(vm, exports, "style_apply");
    vm.capture_errors();
    vm.call(apply, &[root.into(), size]);
    errors.extend(vm.take_error_records());
}

fn hex(rgb: u32) -> String {
    format!("#x{rgb:06x}")
}

fn num(n: f64) -> String {
    crate::value::fmt_num((n * 1000.0).round() / 1000.0)
}

/// How a document uses a kit's parts directly, to copy and edit: the
/// module, the frame `k` for `size` and the kit's parts.
pub fn detached_source(name: &str, size: [f64; 2]) -> Option<String> {
    let kit = kit(name)?;
    let [bg, ink, acc, acc2, muted] = kit.palette;
    let d = &kit.display;
    let b = &kit.body;
    let [w, h] = size;
    let u = w.min(h);
    Some(format!(
        "// @{name}: {}\n// display  {} {} (tracking {} of the size{}{}), animate @{}\n// body     {} {} (tracking {}), animate @{}\n// speed {}  Text3D preset \"{}\"  KineticText preset \"vjfx/{}\"{}\n\
         // The kit is `{name}` in engine/motion/modules/style.splash: copy it to change its layers.\n\
         use mod.motion.modules.style.*\n\
         let k = {{w: {} h: {} cx: {} cy: {} m: {} u: {} s: {}\n         bg: {} ink: {} accent: {} accent2: {} muted: {}}}\n\
         let parts = {{plate: style_kits.{name}.plate(k) ornaments: style_kits.{name}.ornaments(k) screen: style_kits.{name}.screen(k) post: style_kits.{name}.post text_effects: style_kits.{name}.text_effects}}\n",
        kit.look,
        d.font, d.weight, d.tracking, if d.stretch != 100.0 { format!(", stretch {}", d.stretch) } else { String::new() }, if kit.upper { ", capitals" } else { "" }, kit.animate,
        b.font, b.weight, b.tracking, kit.body_animate, kit.speed, kit.text3d, kit.kinetic,
        if kit.fx.is_empty() { String::new() } else { format!("  fx \"{}\"", kit.fx) },
        num(w), num(h), num(w * 0.5), num(h * 0.5), num((u * 0.06).round()), num(u), num(u / 1080.0),
        hex(bg), hex(ink), hex(acc), hex(acc2), hex(muted),
    ))
}
