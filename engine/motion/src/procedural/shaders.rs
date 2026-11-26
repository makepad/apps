//! The looks drawn by shaders: `Pattern` and its kinds,
//! the `slices` slit-scan, `GridFloor`, `Metaballs`, `Rays` and `GridWarp`.
//! The layer types lower to a `Shader` running their pixel function from
//! `procedural.splash`, with their dials as params so they animate.

use super::*;

/// The pixel functions' module, loaded into the document's VM once.
const MODULE: &str = include_str!("procedural.splash");

fn module_fn(cx: &mut Cx, name: &str) -> Result<ScriptValue, (String, String)> {
    let exports = crate::modules::load_internal(cx.vm, "procedural", MODULE).map_err(|e| (String::new(), e.iter().map(|e| e.message()).collect::<Vec<_>>().join("; ")))?;
    Ok(obj::get(cx.vm, exports, name))
}

/// A `Shader` layer running the module's `pixel` (and `helpers`), with `o`'s
/// common fields.
fn shader_layer(cx: &mut Cx, o: Option<ScriptObject>, name: &str, pixel: &str, helpers: &[(&str, &str)], size: [f64; 2], params: Vec<(&str, Lit)>) -> Made {
    let pixel = module_fn(cx, pixel)?;
    let params = record(cx, params);
    let mut fields: Vec<(&str, Lit)> = vec![("name", name.into()), ("pixel", pixel.into()), ("size", v2(size[0], size[1])), ("params", params.into())];
    if !helpers.is_empty() {
        let mut hs = Vec::new();
        for (field, f) in helpers {
            let f = module_fn(cx, f)?;
            hs.push((*field, Lit::Value(f)));
        }
        fields.push(("helpers", record(cx, hs).into()));
    }
    let made = layer(cx, "Shader", fields);
    Ok(match o {
        Some(o) => with_common(cx, o, made, &[]),
        None => made,
    })
}

/// Seconds per slices wave: `@beat` (default), `@half`, `@bar`, or a
/// period in seconds.
fn period(cx: &Cx, on: Option<ScriptValue>) -> Result<f64, (String, String)> {
    let beat = 60.0 / cx.bpm.max(1.0);
    let Some(on) = on.filter(|v| !v.is_nil()) else { return Ok(beat) };
    if let Some(id) = obj::ident(on) {
        return match id.as_str() {
            "beat" => Ok(beat),
            "half" => Ok(beat * 0.5),
            "bar" => Ok(beat * 4.0),
            other => Err(("slices".into(), format!("`slices.on` is @beat, @half, @bar or a period in seconds, not @{other}"))),
        };
    }
    match on.as_number() {
        Some(p) if p > 0.0 => Ok(p),
        _ => Err(("slices".into(), format!("`slices.on` is @beat, @half, @bar or a period in seconds, not {}", kind(cx, on)))),
    }
}

/// A number, or a `tween`/`keys` of numbers, times `k`.
fn scaled(cx: &mut Cx, v: ScriptValue, k: f64) -> ScriptValue {
    if let Some(n) = v.as_number() {
        return (n * k).into();
    }
    let Some(o) = v.as_object() else { return v };
    if cx.vm.bx.heap_ref().is_fn(o) || obj::items(cx.vm, v).is_some() {
        return v;
    }
    let copy = obj::copy_without(cx.vm, o, &[]);
    for (key, value, _) in obj::fields(cx.vm, copy) {
        let new = match key.as_str() {
            "from" | "to" | "inner" => scaled(cx, value, k),
            "points" if obj::items(cx.vm, value).is_some() => {
                let points = obj::items(cx.vm, value).unwrap_or_default();
                let mut out = Vec::with_capacity(points.len());
                for p in points {
                    match obj::items(cx.vm, p).filter(|parts| parts.len() >= 2) {
                        Some(mut parts) => {
                            parts[1] = scaled(cx, parts[1], k);
                            out.push(obj::array(cx.vm, parts));
                        }
                        None => out.push(p),
                    }
                }
                obj::array(cx.vm, out)
            }
            _ => continue,
        };
        obj::set(cx.vm, copy, &key, new);
    }
    copy.into()
}

/// `slices: {dir: @vertical|@horizontal|@diagonal|@cycle count: 12 amp: 80
/// on: @beat spread: 0.35 length: 0.45 edge: @wrap|@clamp|@clear seed}` on
/// a layer: its picture cut into strips that jump along their length and
/// snap back, one wave a beat running across the strips (`@cycle` turns
/// the strips vertical, horizontal, diagonal on successive waves). The
/// layer lowers to a Group running the render kit's `slices` effect on its
/// own picture, so it is exact over transparency. The strips are in frame
/// space; the waves count document seconds at the document's bpm.
pub(super) fn slices(cx: &mut Cx, inner: ScriptObject, spec: ScriptValue) -> Made {
    const FIELDS: &[&str] = &["dir", "count", "amp", "on", "spread", "edge", "seed", "length"];
    let o = match settings(cx, spec, "slices", FIELDS)? {
        Some(o) => Some(o),
        None if spec.as_bool() == Some(true) => None,
        None => return Err(("slices".into(), format!("`slices` is `{{dir: @vertical count: 12 amp: 80 on: @beat}}`, not {}", kind(cx, spec)))),
    };
    let field = |cx: &Cx, k: &str| o.and_then(|o| obj::opt(cx.vm, o, k)).filter(|v| !v.is_nil());
    let num = |cx: &Cx, k: &str, d: f64| -> Result<f64, (String, String)> {
        match o {
            Some(o) => get_num(cx, o, k, d),
            None => Ok(d),
        }
    };
    let dir: &'static str = match field(cx, "dir") {
        None => "vertical",
        Some(v) => match obj::ident(v).as_deref() {
            Some("vertical") => "vertical",
            Some("horizontal") => "horizontal",
            Some("diagonal") => "diagonal",
            Some("cycle") => "cycle",
            _ => return Err(("slices".into(), format!("`slices.dir` is @vertical, @horizontal, @diagonal or @cycle, not {}", kind(cx, v)))),
        },
    };
    let edge: &'static str = match field(cx, "edge") {
        None => "wrap",
        Some(v) => match obj::ident(v).as_deref() {
            Some("wrap") => "wrap",
            Some("clamp") => "extend",
            Some("clear") => "transparent",
            _ => return Err(("slices".into(), format!("`slices.edge` is @wrap, @clamp or @clear, not {}", kind(cx, v)))),
        },
    };
    // `amp` in document units; the effect takes a fraction of the height.
    let amp = match field(cx, "amp") {
        None => 0.08.into(),
        Some(v) if v.as_number().is_some() || (v.as_object().is_some() && obj::items(cx.vm, v).is_none() && !obj::is_fn(cx.vm, v)) => {
            let k = 1.0 / cx.doc[1].max(1.0);
            scaled(cx, v, k)
        }
        Some(v) => return Err(("slices".into(), format!("`slices.amp` is a number (document units), not {}", kind(cx, v)))),
    };
    let on = field(cx, "on");
    let effect = layer(cx, "Effect", vec![
        ("name", Lit::Id("slices")),
        ("dir", Lit::Id(dir)),
        ("count", num(cx, "count", 12.0)?.clamp(1.0, 400.0).round().into()),
        ("amp", amp.into()),
        ("period", period(cx, on)?.into()),
        ("spread", num(cx, "spread", 0.35)?.clamp(0.0, 1.0).into()),
        ("length", num(cx, "length", 0.45)?.clamp(0.02, 1.0).into()),
        ("seed", num(cx, "seed", 1.0)?.into()),
        ("edge", Lit::Id(edge)),
    ]);
    // The layer (keeping its own placement and look) inside a group whose
    // picture the effect resamples.
    let layer_only = obj::copy_without(cx.vm, inner, &["span"]);
    let effects = array(cx, vec![effect]);
    Ok(wrapper(cx, inner, "slices", vec![("effects", effects.into())], vec![layer_only.into()]))
}

/// A number dial of a shader layer: a constant or an animation, as written.
fn dial(cx: &Cx, o: ScriptObject, k: &str, d: f64) -> Result<Lit, (String, String)> {
    match obj::opt(cx.vm, o, k).filter(|v| !v.is_nil()) {
        None => Ok(d.into()),
        Some(v) if v.as_number().is_some() || (v.as_object().is_some() && obj::items(cx.vm, v).is_none() && !obj::is_fn(cx.vm, v)) => Ok(v.into()),
        Some(v) => Err((k.to_string(), format!("`{k}` is a number, not {}", kind(cx, v)))),
    }
}

/// `GridFloor{color horizon cells speed glow fade width sky size}`: a
/// synthwave floor, grid lines running to a vanishing point on a glowing
/// horizon (`horizon` is its height, 0..1 of the box), scrolling toward the
/// viewer at `speed` cells a second; fades to dark near the horizon.
pub(super) fn grid_floor(cx: &mut Cx, o: ScriptObject) -> Made {
    let size = get_vec2(cx, o, "size", cx.doc)?;
    let c = crate::load::unpack(get_color(cx, o, "color", 0xff5a1fff)?);
    let sky = get_num(cx, o, "sky", 0.35)?;
    let params = vec![
        ("gf_r", c[0].into()),
        ("gf_g", c[1].into()),
        ("gf_b", c[2].into()),
        ("gf_horizon", dial(cx, o, "horizon", 0.4)?),
        ("gf_cells", dial(cx, o, "cells", 16.0)?),
        ("gf_speed", dial(cx, o, "speed", 2.0)?),
        ("gf_glow", dial(cx, o, "glow", 1.0)?),
        ("gf_fade", dial(cx, o, "fade", 0.35)?),
        ("gf_width", dial(cx, o, "width", 1.5)?),
        ("gf_sky", sky.into()),
    ];
    shader_layer(cx, Some(o), "grid floor", "grid_floor", &[], size, params)
}

/// `Metaballs{color colors satellites radius orbit k speed wobble
/// background caption size}`: a glossy blob, ray-marched, whose satellites
/// (`colors`, up to four) orbit, drift in and melt into it by a smooth
/// minimum of radius `k`, lit with a specular highlight and a rim.
pub(super) fn metaballs(cx: &mut Cx, o: ScriptObject) -> Made {
    let size = get_vec2(cx, o, "size", cx.doc)?;
    let c = crate::load::unpack(get_color(cx, o, "color", 0xff5a1fff)?);
    let colors = get_colors(cx, o, "colors", &[0xf4f1eaff, 0x3b6cffff, 0xc6ff3aff, 0xff8a3aff])?;
    let count = get_num(cx, o, "satellites", colors.len().min(4) as f64)?.clamp(0.0, 4.0) as usize;
    let mut params: Vec<(&str, Lit)> = vec![
        ("mb_r", c[0].into()),
        ("mb_g", c[1].into()),
        ("mb_b", c[2].into()),
        ("mb_radius", dial(cx, o, "radius", 0.34)?),
        ("mb_orbit", dial(cx, o, "orbit", 0.9)?),
        ("mb_k", dial(cx, o, "k", 0.22)?),
        ("mb_speed", dial(cx, o, "speed", 1.0)?),
        ("mb_wobble", dial(cx, o, "wobble", 1.0)?),
        ("mb_n", (count as f64).into()),
    ];
    // Satellite i's colour: `colors` round and round.
    const NAMES: [[&str; 3]; 4] = [["mb_s0r", "mb_s0g", "mb_s0b"], ["mb_s1r", "mb_s1g", "mb_s1b"], ["mb_s2r", "mb_s2g", "mb_s2b"], ["mb_s3r", "mb_s3g", "mb_s3b"]];
    for (i, names) in NAMES.iter().enumerate() {
        let sc = crate::load::unpack(colors[i % colors.len()]);
        for (n, v) in names.iter().zip(sc) {
            params.push((n, v.into()));
        }
    }
    let mut layers = Vec::new();
    if let Some(bg) = obj::opt(cx.vm, o, "background").filter(|v| !v.is_nil()) {
        let Some(bg) = bg.as_color() else {
            return Err(("background".into(), format!("`background` is a colour, not {}", kind(cx, bg))));
        };
        // A radial light behind the blob: the colour, lifted in the middle.
        layers.push(layer(cx, "Rect", vec![("size", v2(size[0], size[1])), ("fill", Lit::Color(bg))]));
        let d = size[1] * 1.1;
        layers.push(layer(cx, "Ellipse", vec![
            ("size", v2(d, d)),
            ("at", v2(size[0] * 0.5 - d * 0.5, size[1] * 0.5 - d * 0.5)),
            ("fill", Lit::Color(mix_color(bg, 0x6f8cffff, 0.22))),
            ("feather", (d * 0.45).into()),
        ]));
    }
    layers.push(shader_layer(cx, None, "metaballs", "metaballs", &[("mb_map", "metaballs_map")], size, params)?);
    if get_bool(cx, o, "caption", false) {
        let ink = get_color(cx, o, "ink", 0xf0ebe0cc)?;
        let font = get_face(cx, o, "font", "JetBrains Mono")?;
        let k = get_num(cx, o, "k", 0.22).unwrap_or(0.22);
        layers.push(label(cx, &format!("r = smin(a, b, k = {k:.2})"), size[1] * 0.03, ink, [size[0] * 0.5, size[1] * 0.88], "center", &font));
    }
    Ok(group(cx, o, layers, vec![("size", v2(size[0], size[1]))]))
}

/// `Rays{color center count spread speed burst intensity size}`: light
/// rays streaming out of a point (`center`, 0..1 of the box), slowly
/// turning, with a hot core. `burst: <time>` makes them shoot out from the
/// core then (a starburst drop); `reach` (0..1+, animatable) sets how far
/// they reach without it.
pub(super) fn rays(cx: &mut Cx, o: ScriptObject) -> Made {
    let size = get_vec2(cx, o, "size", cx.doc)?;
    let c = crate::load::unpack(get_color(cx, o, "color", 0xff5a1fff)?);
    let center = get_vec2(cx, o, "center", [0.5, 0.5])?;
    let reach: Lit = match obj::opt(cx.vm, o, "burst").filter(|v| !v.is_nil()) {
        Some(t) => {
            // Dark until the burst: a key at 0, unless the burst is at 0 (or
            // before), where that key would repeat the burst's own time.
            let mut points = Vec::new();
            if obj::num(cx.vm, t).map_or(true, |t| t > 0.0) {
                points.push((0.0.into(), 0.0.into(), None));
            }
            points.push((t, 0.0.into(), None));
            let peak = at_time(cx, t, 0.6);
            points.push((peak, 1.6.into(), Some("ease_out_expo")));
            obj::keys(cx.vm, points).into()
        }
        None => dial(cx, o, "reach", 1.6)?,
    };
    let params = vec![
        ("ry_r", c[0].into()),
        ("ry_g", c[1].into()),
        ("ry_b", c[2].into()),
        ("ry_cx", center[0].into()),
        ("ry_cy", center[1].into()),
        ("ry_count", dial(cx, o, "count", 18.0)?),
        ("ry_spread", dial(cx, o, "spread", 0.35)?),
        ("ry_speed", dial(cx, o, "speed", 0.6)?),
        ("ry_reach", reach),
        ("ry_intensity", dial(cx, o, "intensity", 1.2)?),
    ];
    shader_layer(cx, Some(o), "rays", "rays", &[], size, params)
}

/// `GridWarp{color cells mass center mode ring speed width size}`: a grid
/// bent by a point mass at `center` (0..1 of the box): `mode: @well` pulls
/// its lines into a black hole with an accretion ring (`ring: false` for
/// none), `@ripple` rings it outward like a dropped stone. `mass` (0..1,
/// animatable) is how hard.
pub(super) fn grid_warp(cx: &mut Cx, o: ScriptObject) -> Made {
    let size = get_vec2(cx, o, "size", cx.doc)?;
    let c = crate::load::unpack(get_color(cx, o, "color", 0xff5a1fff)?);
    let center = get_vec2(cx, o, "center", [0.5, 0.5])?;
    let ripple = match obj::opt(cx.vm, o, "mode").filter(|v| !v.is_nil()) {
        None => false,
        Some(v) => match obj::ident(v).as_deref() {
            Some("well") => false,
            Some("ripple") => true,
            _ => return Err(("mode".into(), format!("`mode` is @well or @ripple, not {}", kind(cx, v)))),
        },
    };
    let ring = get_bool(cx, o, "ring", !ripple);
    let params = vec![
        ("gw_r", c[0].into()),
        ("gw_g", c[1].into()),
        ("gw_b", c[2].into()),
        ("gw_cx", center[0].into()),
        ("gw_cy", center[1].into()),
        ("gw_cells", dial(cx, o, "cells", 14.0)?),
        ("gw_mass", dial(cx, o, "mass", 0.6)?),
        ("gw_width", dial(cx, o, "width", 1.2)?),
        ("gw_ring", (if ring { 1.0 } else { 0.0 }).into()),
        ("gw_ripple", (if ripple { 1.0 } else { 0.0 }).into()),
    ];
    let made = shader_layer(cx, Some(o), "grid warp", "grid_warp", &[], size, params)?;
    // `speed` runs the swirl and the ripple faster (the Shader's clock).
    if let (Some(m), Some(speed)) = (made.as_object(), obj::opt(cx.vm, o, "speed").and_then(|v| v.as_number())) {
        obj::set(cx.vm, m, "speed", speed.into());
    }
    Ok(made)
}

/// Pattern kinds drawn by `pattern` with the kind's coverage function
/// (`pattern_<kind>`), and the seed's hash offset they take (`0`: unseeded).
const PATTERN_COVERS: &[(&str, f64)] = &[
    ("stripes", 0.0), ("dots", 0.0), ("checker", 0.0), ("grid", 0.0), ("hatch", 0.0), ("waves", 0.0), ("noise", 0.0),
    ("truchet", 17.131), ("truchet_fill", 17.131), ("truchet_diag", 17.131), ("truchet_hex", 17.131),
    ("voronoi", 13.717), ("scales", 13.717), ("guilloche", 0.0), ("guilloche_band", 0.0), ("leds", 13.717),
];

/// Every Pattern kind: the tilings, then the two drawn by their own pixel.
pub fn pattern_kinds() -> impl Iterator<Item = &'static str> {
    PATTERN_COVERS.iter().map(|(k, _)| *k).chain(["halftone", "wash"])
}

/// `Pattern{kind color color2 cell angle weight seed speed size}`: a
/// procedural fill of its box (put it in a Group with a `mask` for any
/// shape), a `Shader` running `pattern` with the kind's coverage, or the
/// kind's own pixel (`@halftone` reads the frame under it, `@wash`).
/// `cell` is in document units, `angle` in degrees; `seed` turns the seeded
/// kinds' tiles.
pub(super) fn pattern(cx: &mut Cx, o: ScriptObject) -> Made {
    let kind = match obj::opt(cx.vm, o, "kind").filter(|v| !v.is_nil()) {
        None => "stripes".to_string(),
        Some(v) => match obj::ident(v) {
            Some(id) => id,
            None => {
                let names: Vec<String> = pattern_kinds().map(|n| format!("@{n}")).collect();
                return Err(("kind".into(), format!("`kind` is one of {}, not {}", names.join(", "), kind(cx, v))));
            }
        },
    };
    let cover = PATTERN_COVERS.iter().find(|(k, _)| *k == kind);
    if cover.is_none() && kind != "halftone" && kind != "wash" {
        let names: Vec<&str> = pattern_kinds().collect();
        return Err(("kind".into(), format!("`@{kind}` is not a pattern.{}", crate::load::did_you_mean(&kind, &names))));
    }
    let size = obj::opt(cx.vm, o, "size").filter(|v| !v.is_nil());
    let box_size = match size {
        Some(_) => get_vec2(cx, o, "size", cx.doc)?,
        None => cx.doc,
    };
    let color = |cx: &Cx, field: &str, default: u32| -> Result<[f64; 4], (String, String)> {
        get_color(cx, o, field, default)
            .map(crate::load::unpack)
            .map_err(|(f, _)| (f, format!("a Pattern's `{field}` is a constant colour (animate the layer's opacity, or stack two)")))
    };
    let a = color(cx, "color", 0xffffffff)?;
    let b = color(cx, "color2", 0x00000000)?;
    let cell = match obj::opt(cx.vm, o, "cell").filter(|v| !v.is_nil()) {
        None => 40.0,
        Some(v) => v.as_number().ok_or_else(|| ("cell".to_string(), "a Pattern's `cell` is a constant size in document units".to_string()))?,
    };
    let angle: Lit = match obj::opt(cx.vm, o, "angle").filter(|v| !v.is_nil()) {
        None => 0.0.into(),
        Some(v) => match v.as_number() {
            Some(deg) => deg.to_radians().into(),
            None => {
                dial(cx, o, "angle", 0.0)?;
                scaled(cx, v, std::f64::consts::PI / 180.0).into()
            }
        },
    };
    let seed = get_num(cx, o, "seed", 1.0)? as u32;
    let mut params: Vec<(&str, Lit)> = vec![
        ("ar", a[0].into()), ("ag", a[1].into()), ("ab", a[2].into()), ("aa", a[3].into()),
        ("br", b[0].into()), ("bg", b[1].into()), ("bb", b[2].into()), ("ba", b[3].into()),
        ("cell", (cell / box_size[1].max(1.0)).into()),
        ("angle", angle),
        ("weight", dial(cx, o, "weight", 0.5)?),
        ("aspect", (box_size[0] / box_size[1].max(1.0)).into()),
    ];
    let (pixel, helpers): (String, Vec<(&str, String)>) = match cover {
        Some((k, offset)) => {
            if *offset != 0.0 {
                // The offset the tiles' hash adds, as the shaders always took it
                // (five decimals).
                let s = (seed as f64 * offset % 1000.0 * 1e5).round() / 1e5;
                params.push(("seed", s.into()));
            }
            let mut helpers = vec![("pattern_cover", format!("pattern_{k}"))];
            if matches!(*k, "voronoi" | "scales" | "leds") {
                helpers.push(("pattern_hash2", "pattern_hash2".into()));
            }
            if matches!(*k, "voronoi" | "scales") {
                helpers.push(("pattern_cells", "pattern_cells".into()));
            }
            ("pattern".into(), helpers)
        }
        None => (format!("pattern_{kind}"), Vec::new()),
    };
    let pixel = module_fn(cx, &pixel)?;
    let params = record(cx, params);
    let mut fields: Vec<(&str, Lit)> = vec![("pixel", pixel.into()), ("params", params.into())];
    if let Some(size) = size {
        fields.push(("size", size.into()));
    }
    if !helpers.is_empty() {
        let mut hs = Vec::new();
        for (field, f) in &helpers {
            let f = module_fn(cx, f)?;
            hs.push((*field, Lit::Value(f)));
        }
        fields.push(("helpers", record(cx, hs).into()));
    }
    if kind == "halftone" {
        fields.push(("input", Lit::Id("below")));
        fields.push(("speed", 0.0.into()));
    } else {
        fields.push(("speed", obj::opt(cx.vm, o, "speed").filter(|v| !v.is_nil()).map_or(0.0.into(), Lit::Value)));
    }
    let made = layer(cx, "Shader", fields);
    Ok(with_common(cx, o, made, &[]))
}
