//! Procedural motion design (lane FX2, REF-Ac7SuDYTQDc §5–6): the looks a
//! motion-design showreel is built from, each a one-line layer with a few
//! dials that lowers at load to ordinary layers (like `crate::templates`),
//! so evaluation and drawing stay as they are and every frame is a pure
//! function of time.
//!
//! - `Pattern{kind: @stripes | … | @truchet | @truchet_fill | @truchet_diag | @truchet_hex seed}`:
//!   tilings and seeded generative tilings, Splash pixel functions in
//!   `procedural.splash` ([`pattern_kinds`]).
//! - `slices: {dir count amp on}` on any layer: slit-scan strips of its
//!   picture displaced and snapping back, one wave per beat (the render
//!   kit's `slices` effect on the layer's own picture).
//! - `EaseGraph{curve: "cubic-bezier(…)"}`: a curve editor drawing itself.
//! - `ParticleText{text: "flow"}`: a vortex of particles that settles into
//!   the letters and blows away.
//! - `Bounce{…}`: a ball bouncing along a baseline with automatic squash
//!   and stretch; `onion: {count spacing}` on any layer: an onion-skin
//!   trail of its earlier selves.
//! - `GridFloor{…}`: a synthwave perspective grid fading into a glowing
//!   horizon.
//! - `Metaballs{…}`: a glossy ray-marched blob whose satellites orbit and
//!   merge into it (smooth-min SDF).
//! - `Pattern{kind: @voronoi | @scales | @guilloche | @guilloche_band | @leds}`:
//!   cellular edges, reptile skin, banknote rosettes and ropes, an LED wall.
//! - `Rays{…}` (light rays and starbursts), `GridWarp{…}` (a grid bent by a
//!   point mass into a black hole, or rippled), `Branches{…}` (a branching
//!   tree or a Loom of word choices growing).
//! - `tear: {time}` and `droste: {depth scale period}` on any layer: the
//!   layer rips in two and falls away; the layer nested inside itself,
//!   zooming forever.

mod anim;
mod camera;
mod bounce;
mod branches;
mod ease_graph;
mod glyphs;
mod match_morph;
mod math;
mod particles;
mod plot;
mod shape;
mod point_map;
mod shaders;
#[cfg(test)]
mod tests;
mod transitions;

pub use ease_graph::EaseCurve;
pub use glyphs::{glyph_points, shape_points};
pub use shaders::pattern_kinds;
pub(crate) use plot::install as install_plotting;
pub(crate) use shape::install as install_boolean;

pub(crate) use crate::build::*;
use crate::lower::Pass;
use crate::obj::{self, Lit};
use makepad_script::*;

/// The layer types this module adds (`Morph`, two texts matched letter by
/// letter, lowers here too).
pub const LAYER_TYPES: &[&str] = &["EaseGraph", "ParticleText", "Bounce", "GridFloor", "Metaballs", "Rays", "GridWarp", "Branches", "Math", "MatchMorph", "Shape", "Plot", "Graph", "Parametric", "Implicit", "VectorField", "Riemann", "Area", "Tangent", "Dot", "Camera", "Zoomed"];

/// Fields any layer takes that this module lowers (beside the common ones).
pub const MODIFIERS: &[&str] = &["slices", "onion", "tear", "droste"];

/// A layer of this module's types, or one carrying a modifier (`slices`,
/// `onion`, `tear`, `droste`), as the ordinary layers it draws as (`None`:
/// none of this module's; `Some(None)`: an error, reported).
pub(crate) fn lower(pass: &mut Pass, vm: &mut ScriptVm, o: ScriptObject, path: &str, kind: &str) -> Option<Option<ScriptValue>> {
    let live = |vm: &ScriptVm, k: &str| obj::opt(vm, o, k).filter(|v| !v.is_nil());
    let doc = [pass.doc.w, pass.doc.h];
    let bpm = pass.doc.bpm;
    // Modifiers first: the layer without the field, wrapped.
    // (A Bounce trails its ball, not the whole graphic.)
    let modifier = if let Some(spec) = live(vm, "onion").filter(|_| kind != "Bounce") {
        Some(("onion", spec))
    } else if let Some(spec) = live(vm, "slices") {
        Some(("slices", spec))
    } else if let Some(spec) = live(vm, "tear") {
        Some(("tear", spec))
    } else {
        live(vm, "droste").map(|spec| ("droste", spec))
    };
    let made = {
        let duration = pass.doc.duration;
        let root = pass.root;
        let mut cx = Cx { vm, doc, bpm, duration };
        match modifier {
            Some((field, spec)) => {
                let inner = obj::copy_without(cx.vm, o, &[field]);
                match field {
                    "onion" => bounce::onion(&mut cx, inner, spec),
                    "slices" => shaders::slices(&mut cx, inner, spec),
                    "tear" => transitions::tear(&mut cx, inner, spec),
                    _ => transitions::droste(&mut cx, inner, spec),
                }
            }
            None => match kind {
                "EaseGraph" => ease_graph::lower(&mut cx, o),
                "ParticleText" => particles::lower(&mut cx, o),
                "Bounce" => bounce::lower(&mut cx, o),
                "Pattern" => shaders::pattern(&mut cx, o),
                "GridFloor" => shaders::grid_floor(&mut cx, o),
                "Metaballs" => shaders::metaballs(&mut cx, o),
                "Rays" => shaders::rays(&mut cx, o),
                "GridWarp" => shaders::grid_warp(&mut cx, o),
                "Branches" => branches::lower(&mut cx, o),
                "Math" => math::lower(&mut cx, o),
                "MatchMorph" => match_morph::lower(&mut cx, o),
                "Plot" => plot::lower(&mut cx, o),
                "Camera" => camera::camera(&mut cx, o),
                "Zoomed" => camera::zoomed(&mut cx, o, root, &mut pass.zoom_copies),
                "Shape" => shape::lower(&mut cx, o),
                item if plot::ITEMS.contains(&item) => Err((String::new(), format!("`{item}` goes inside a Plot's `layers` (it draws in the plot's coordinates): Plot{{x: [-3, 3] y: [-1, 9] layers: [Graph{{f: fn(x) {{ x * x }}}}]}}"))),
                "Morph" => match_morph::lower_morph(&mut cx, o),
                _ => return None,
            },
        }
    };
    Some(match made {
        Ok(v) => Some(v),
        Err((field, message)) => {
            pass.err(vm, o, path, &field, message, None);
            None
        }
    })
}

fn with_alpha(c: u32, a: f64) -> u32 {
    (c & 0xffff_ff00) | ((a.clamp(0.0, 1.0) * 255.0).round() as u32)
}

/// Linear mix of two colours.
fn mix_color(a: u32, b: u32, t: f64) -> u32 {
    let (x, y) = (crate::load::unpack(a), crate::load::unpack(b));
    let ch = |i: usize| ((x[i] + (y[i] - x[i]) * t).clamp(0.0, 1.0) * 255.0).round() as u32;
    (ch(0) << 24) | (ch(1) << 16) | (ch(2) << 8) | ch(3)
}

/// A colour along a gradient of `stops` (evenly spaced) at `t` in 0..1.
fn gradient(stops: &[u32], t: f64) -> u32 {
    if stops.len() == 1 {
        return stops[0];
    }
    let x = t.clamp(0.0, 1.0) * (stops.len() - 1) as f64;
    let i = (x.floor() as usize).min(stops.len() - 2);
    mix_color(stops[i], stops[i + 1], x - i as f64)
}

fn hash01(seed: u32, i: usize, k: u32) -> f64 {
    crate::crowd::hash01(seed, i, k)
}

/// A complete document showing one of this module's layer types (the
/// catalogue's `layer_example`).
pub fn example(kind: &str) -> Option<&'static str> {
    Some(match kind {
        "Camera" | "Zoomed" => include_str!("../../fixtures/camera.splash"),
        "Plot" | "Graph" | "Parametric" | "Implicit" | "VectorField" | "Riemann" | "Area" | "Tangent" | "Dot" => include_str!("../../fixtures/plot.splash"),
        "Shape" => r#"use mod.motion.*; Motion{size: vec2(1280, 720) duration: 2.0 background: #101418 layers: [
    Shape{op: @difference at: vec2(440, 160) fill: #ff7a1a layers: [
        Rect{size: vec2(400, 400) radius: 40}
        Ellipse{at: vec2(100, 100) size: vec2(200, 200)}
    ]}
]}"#,
        "EaseGraph" => r#"use mod.motion.*; Motion{size: vec2(1280, 720) duration: 2.0 background: #111111 layers: [
    EaseGraph{curve: "cubic-bezier(0.70, 0.00, 0.30, 1.00)" at: vec2(160, 60) size: vec2(960, 600)}
]}"#,
        "ParticleText" => r#"use mod.motion.*; Motion{size: vec2(1280, 720) duration: 3.0 background: #000000 layers: [
    ParticleText{text: "flow" count: 1500 converge: 1.0 release: 1.8}
]}"#,
        "Bounce" => r#"use mod.motion.*; Motion{size: vec2(1280, 720) duration: 2.0 background: #f0ebe0 layers: [
    Bounce{at: vec2(140, 200) size: vec2(1000, 420) bounces: 3 labels: true onion: {count: 6 spacing: 0.04 outline: true}}
]}"#,
        "GridFloor" => r#"use mod.motion.*; Motion{size: vec2(1280, 720) duration: 2.0 background: #0a0605 layers: [
    GridFloor{color: #ff5a1f horizon: 0.45 speed: 3}
]}"#,
        "Metaballs" => r#"use mod.motion.*; Motion{size: vec2(1280, 720) duration: 2.0 layers: [
    Metaballs{background: #0d1433 caption: true}
]}"#,
        "Rays" => r#"use mod.motion.*; Motion{size: vec2(1280, 720) duration: 2.0 background: #000000 layers: [
    Rays{color: #ff5a1f center: vec2(0.5, 0.5) count: 18 burst: 0.3}
]}"#,
        "GridWarp" => r#"use mod.motion.*; Motion{size: vec2(1280, 720) duration: 2.0 background: #050505 layers: [
    GridWarp{color: #ff5a1f mass: tween(0, 0.8, 0, 1.5, @ease_in_out_cubic)}
]}"#,
        "Branches" => r#"use mod.motion.*; Motion{size: vec2(1280, 720) duration: 2.0 background: #0b0b0b layers: [
    Branches{style: @tree depth: 7 seed: 3 grow: 1.5}
]}"#,
        "Math" => r#"use mod.motion.*; Motion{size: vec2(1280, 720) duration: 2.0 background: #101418 layers: [
    Math{tex: "S_N(x) = \\frac{4}{\\pi}\\sum_{k=1}^{N} \\frac{\\sin((2k-1)x)}{\\id{den}{2k-1}}" size: 56 color: #f0ebe0 at: vec2(160, 300)
        parts: {den: {color: tween(#f0ebe0, #ff6a00, 0.5, 1.2, @ease_in_out_cubic)}}}
]}"#,
        "MatchMorph" => r#"use mod.motion.*; Motion{size: vec2(1280, 720) duration: 3.0 background: #101418 layers: [
    MatchMorph{steps: ["a² + b² = c²", "c² = a² + b²"] times: [0.5] by: @word size: 96 colors: [#f0ebe0, #ff9a3c] at: vec2(240, 280) duration: 1.5}
]}"#,
        _ => return None,
    })
}
