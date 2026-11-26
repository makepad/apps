//! The catalog: every effect the kit renders, its parameters (kind, range,
//! default, unit, meaning) and how it composes. One table drives parameter
//! validation with teaching diagnostics, resolution at `t`, and the
//! authoring guide (`makepad-render-kit doc`, design/RENDER-KIT.md).
//!
//! Conventions every effect shares:
//! - Lengths are pixels of the frame the effect renders into (the comp or
//!   clip size); positions named `center`, `anchor`, `start`, `end` and
//!   `position` of emitters are fractions of the frame (0..1, y down).
//! - Light angles follow the Photoshop convention: degrees
//!   counter-clockwise from the right, the direction the light comes FROM
//!   (120 = upper left, so a shadow falls to the lower right).
//! - Colours are straight RGBA; every texture is premultiplied.
//! - `mix` (0..1) blends the effect's result with its input.

use crate::anim::{parse_hex_color, Anim, Value};

/// Every effect.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum EffectKind {
    GaussianBlur,
    DirectionalBlur,
    RadialBlur,
    ZoomBlur,
    Glow,
    Bloom,
    DropShadow,
    InnerShadow,
    Stroke,
    Bevel,
    Fill,
    Gradient,
    Noise,
    Displace,
    Turbulence,
    Ripple,
    Slices,
    Blend,
    Mask,
    Matte,
    Transform3d,
    Particles,
    StrokePath,
    Echo,
    MotionBlur,
    Curves,
    Levels,
    HueSaturation,
    WhiteBalance,
    Lut,
    ChromaticAberration,
    Grain,
    Vignette,
    Halation,
    Exposure,
    BrightnessContrast,
    Tint,
    Invert,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Category {
    Blur,
    Light,
    Style,
    Generate,
    Distort,
    Composite,
    Space,
    Time,
    Color,
}

impl Category {
    pub fn title(self) -> &'static str {
        match self {
            Category::Blur => "Blur",
            Category::Light => "Glow and light",
            Category::Style => "Layer styles",
            Category::Generate => "Generators",
            Category::Distort => "Distortion",
            Category::Composite => "Blend modes, masks and mattes",
            Category::Space => "2.5D",
            Category::Time => "Time (motion blur, echo)",
            Category::Color => "Colour",
        }
    }
}

/// A parameter's kind, range and default.
#[derive(Clone, Copy, Debug)]
pub enum ParamKind {
    Num { min: f64, max: f64, default: f64 },
    Vec2 { default: [f64; 2] },
    Vec3 { default: [f64; 3] },
    Color { default: [f64; 4] },
    Bool { default: bool },
    Choice { choices: &'static [&'static str], default: &'static str },
    /// Text (a file path, SVG path data); empty by default.
    Text,
    /// A host layer by name; unset by default.
    Layer,
    /// Pixel points; empty by default.
    Points,
    Stops { default: &'static [(f64, [f64; 4])] },
    /// A curve through `[x, y]` points in 0..1, identity by default.
    Curve,
}

impl ParamKind {
    pub fn default_value(&self) -> Value {
        match *self {
            ParamKind::Num { default, .. } => Value::Num(default),
            ParamKind::Vec2 { default } => Value::Vec2(default),
            ParamKind::Vec3 { default } => Value::Vec3(default),
            ParamKind::Color { default } => Value::Color(default),
            ParamKind::Bool { default } => Value::Bool(default),
            ParamKind::Choice { default, .. } => Value::Choice(default.into()),
            ParamKind::Text => Value::Text(String::new()),
            ParamKind::Layer => Value::Layer(String::new()),
            ParamKind::Points => Value::Points(Vec::new()),
            ParamKind::Stops { default } => Value::Stops(default.to_vec()),
            ParamKind::Curve => Value::Points(vec![[0.0, 0.0], [1.0, 1.0]]),
        }
    }

    /// How a value of this kind is written, for diagnostics and the guide.
    pub fn syntax(&self) -> String {
        match self {
            ParamKind::Num { min, max, default } => format!("a number {} to {} (default {})", num(*min), num(*max), num(*default)),
            ParamKind::Vec2 { default } => format!("vec2(x, y) (default vec2({}, {}))", num(default[0]), num(default[1])),
            ParamKind::Vec3 { default } => format!("vec3(x, y, z) (default vec3({}, {}, {}))", num(default[0]), num(default[1]), num(default[2])),
            ParamKind::Color { default } => format!("a colour like #ff8800 or #ff880080 (default {})", hex(default)),
            ParamKind::Bool { default } => format!("true or false (default {default})"),
            ParamKind::Choice { choices, default } => format!(
                "one of {} (default @{default})",
                choices.iter().map(|c| format!("@{c}")).collect::<Vec<_>>().join(", ")
            ),
            ParamKind::Text => "a string".into(),
            ParamKind::Layer => "a layer name, e.g. @logo".into(),
            ParamKind::Points => "a list of points in pixels, [vec2(x, y), ...]".into(),
            ParamKind::Stops { .. } => "gradient stops, [[0.0, #000000], [1.0, #ffffff]] (position 0..1, colour)".into(),
            ParamKind::Curve => "curve points, [vec2(0, 0), vec2(0.5, 0.6), vec2(1, 1)] (input, output in 0..1)".into(),
        }
    }
}

fn num(v: f64) -> String {
    if v >= 1e8 {
        "∞".into()
    } else {
        let s = format!("{v:.3}");
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    }
}

pub fn hex(c: &[f64; 4]) -> String {
    let b = |v: f64| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    if c[3] >= 1.0 {
        format!("#{:02x}{:02x}{:02x}", b(c[0]), b(c[1]), b(c[2]))
    } else {
        format!("#{:02x}{:02x}{:02x}{:02x}", b(c[0]), b(c[1]), b(c[2]), b(c[3]))
    }
}

#[derive(Clone, Copy, Debug)]
pub struct ParamInfo {
    pub name: &'static str,
    pub kind: ParamKind,
    pub doc: &'static str,
}

#[derive(Clone, Copy, Debug)]
pub struct EffectInfo {
    pub kind: EffectKind,
    pub name: &'static str,
    pub category: Category,
    /// Renders the layer at other times (motion blur, echo): the host must
    /// apply the stack through [`crate::RenderKit::apply_layer`].
    pub temporal: bool,
    pub doc: &'static str,
    pub params: &'static [ParamInfo],
}

impl EffectInfo {
    pub fn param(&self, name: &str) -> Option<(usize, &ParamInfo)> {
        self.params.iter().enumerate().find(|(_, p)| p.name == name)
    }
}

const BLACK: [f64; 4] = [0.0, 0.0, 0.0, 1.0];
const WHITE: [f64; 4] = [1.0, 1.0, 1.0, 1.0];
const CENTER: [f64; 2] = [0.5, 0.5];
const HUGE: f64 = 1e9;

pub const BLEND_MODES: &[&str] = &[
    "normal", "multiply", "screen", "overlay", "darken", "lighten", "color_dodge", "color_burn", "hard_light", "soft_light",
    "difference", "exclusion", "add", "subtract", "divide", "hue", "saturation", "color", "luminosity",
];
const EDGE: &[&str] = &["transparent", "extend"];

const fn n(name: &'static str, min: f64, max: f64, default: f64, doc: &'static str) -> ParamInfo {
    ParamInfo { name, kind: ParamKind::Num { min, max, default }, doc }
}
const fn v2(name: &'static str, default: [f64; 2], doc: &'static str) -> ParamInfo {
    ParamInfo { name, kind: ParamKind::Vec2 { default }, doc }
}
const fn v3(name: &'static str, default: [f64; 3], doc: &'static str) -> ParamInfo {
    ParamInfo { name, kind: ParamKind::Vec3 { default }, doc }
}
const fn c(name: &'static str, default: [f64; 4], doc: &'static str) -> ParamInfo {
    ParamInfo { name, kind: ParamKind::Color { default }, doc }
}
const fn b(name: &'static str, default: bool, doc: &'static str) -> ParamInfo {
    ParamInfo { name, kind: ParamKind::Bool { default }, doc }
}
const fn ch(name: &'static str, choices: &'static [&'static str], default: &'static str, doc: &'static str) -> ParamInfo {
    ParamInfo { name, kind: ParamKind::Choice { choices, default }, doc }
}
const fn layer(name: &'static str, doc: &'static str) -> ParamInfo {
    ParamInfo { name, kind: ParamKind::Layer, doc }
}
const fn curve(name: &'static str, doc: &'static str) -> ParamInfo {
    ParamInfo { name, kind: ParamKind::Curve, doc }
}

const MIX: ParamInfo = n("mix", 0.0, 1.0, 1.0, "How much of the effect: 0 is the input unchanged, 1 the full effect.");
const BLEND: ParamInfo = ch("blend", BLEND_MODES, "normal", "How the generated picture combines with the input.");
const OPACITY: ParamInfo = n("opacity", 0.0, 1.0, 1.0, "Opacity of what the effect draws.");
const CLIP: ParamInfo = b("clip", false, "Keep the result inside the input's alpha (a layer-style overlay) instead of covering the frame.");

pub static EFFECTS: &[EffectInfo] = &[
    // ── Blur ────────────────────────────────────────────────────────────
    EffectInfo {
        kind: EffectKind::GaussianBlur, name: "gaussian_blur", category: Category::Blur, temporal: false,
        doc: "A true gaussian blur (separable; large radii run on a downsampled copy with the downsampling's own blur subtracted, so the result stays gaussian).",
        params: &[
            n("radius", 0.0, 2000.0, 8.0, "Standard deviation in pixels (the same number as CSS blur())."),
            ch("direction", &["both", "horizontal", "vertical"], "both", "Blur both axes or one."),
            ch("edge", EDGE, "transparent", "Outside the frame: transparent (soft edges fade out) or the edge pixels extended (a full-frame picture keeps solid edges)."),
            MIX,
        ],
    },
    EffectInfo {
        kind: EffectKind::DirectionalBlur, name: "directional_blur", category: Category::Blur, temporal: false,
        doc: "A straight smear along one direction: the look of fast linear motion.",
        params: &[
            n("angle", -360.0, 360.0, 0.0, "Direction in degrees, counter-clockwise from the right."),
            n("length", 0.0, 4000.0, 24.0, "Total smear length in pixels."),
            MIX,
        ],
    },
    EffectInfo {
        kind: EffectKind::RadialBlur, name: "radial_blur", category: Category::Blur, temporal: false,
        doc: "A spin blur around a centre.",
        params: &[
            v2("center", CENTER, "Centre of rotation, 0..1 of the frame."),
            n("angle", 0.0, 360.0, 8.0, "Total spin in degrees."),
            MIX,
        ],
    },
    EffectInfo {
        kind: EffectKind::ZoomBlur, name: "zoom_blur", category: Category::Blur, temporal: false,
        doc: "A zoom (radial streak) blur towards a centre.",
        params: &[
            v2("center", CENTER, "Centre of the zoom, 0..1 of the frame."),
            n("amount", 0.0, 1.0, 0.15, "Streak length as a fraction of each pixel's distance to the centre."),
            MIX,
        ],
    },
    // ── Light ───────────────────────────────────────────────────────────
    EffectInfo {
        kind: EffectKind::Glow, name: "glow", category: Category::Light, temporal: false,
        doc: "The bright parts, blurred and added back: neon, hot highlights.",
        params: &[
            n("threshold", 0.0, 1.0, 0.6, "Luma above which pixels glow."),
            n("knee", 0.0, 1.0, 0.2, "Softness of the threshold."),
            n("radius", 0.0, 2000.0, 16.0, "Glow spread in pixels (gaussian standard deviation)."),
            n("intensity", 0.0, 20.0, 1.0, "Strength of the added glow."),
            c("color", WHITE, "Tint multiplied into the glow (white keeps the source colours)."),
            MIX,
        ],
    },
    EffectInfo {
        kind: EffectKind::Bloom, name: "bloom", category: Category::Light, temporal: false,
        doc: "Multi-scale bloom: the bright parts blurred at four radii (radius/8 … radius) and summed, like a camera lens. Wider and softer than glow.",
        params: &[
            n("threshold", 0.0, 1.0, 0.75, "Luma above which pixels bloom."),
            n("knee", 0.0, 1.0, 0.3, "Softness of the threshold."),
            n("radius", 1.0, 2000.0, 48.0, "The widest scale's radius in pixels."),
            n("intensity", 0.0, 20.0, 0.8, "Strength of the bloom."),
            c("tint", WHITE, "Colour multiplied into the bloom."),
            MIX,
        ],
    },
    EffectInfo {
        kind: EffectKind::Halation, name: "halation", category: Category::Light, temporal: false,
        doc: "Film halation: highlights bleed a warm red-orange halo, as light scatters back through film.",
        params: &[
            n("threshold", 0.0, 1.0, 0.7, "Luma above which highlights halate."),
            n("radius", 0.0, 2000.0, 10.0, "Halo spread in pixels."),
            c("color", [1.0, 0.23, 0.12, 1.0], "Halo colour."),
            n("amount", 0.0, 4.0, 0.6, "Halo strength."),
            MIX,
        ],
    },
    // ── Layer styles ────────────────────────────────────────────────────
    EffectInfo {
        kind: EffectKind::DropShadow, name: "drop_shadow", category: Category::Style, temporal: false,
        doc: "A soft shadow of the layer's alpha, cast away from the light, under the layer.",
        params: &[
            c("color", BLACK, "Shadow colour."),
            n("opacity", 0.0, 1.0, 0.6, "Shadow opacity."),
            n("angle", -360.0, 360.0, 120.0, "Light angle (degrees counter-clockwise from the right, where the light comes from); the shadow falls opposite."),
            n("distance", 0.0, 2000.0, 12.0, "Shadow offset in pixels."),
            n("radius", 0.0, 2000.0, 10.0, "Softness in pixels (gaussian standard deviation)."),
            n("spread", 0.0, 1.0, 0.0, "Hardens the shadow's edge before blurring (0 soft, 1 solid)."),
            b("shadow_only", false, "Draw only the shadow, not the layer."),
            MIX,
        ],
    },
    EffectInfo {
        kind: EffectKind::InnerShadow, name: "inner_shadow", category: Category::Style, temporal: false,
        doc: "A shadow inside the layer's edge, as if the layer were cut into the surface.",
        params: &[
            c("color", BLACK, "Shadow colour."),
            n("opacity", 0.0, 1.0, 0.6, "Shadow opacity."),
            n("angle", -360.0, 360.0, 120.0, "Light angle, Photoshop convention."),
            n("distance", 0.0, 2000.0, 6.0, "Offset in pixels."),
            n("radius", 0.0, 2000.0, 8.0, "Softness in pixels."),
            n("choke", 0.0, 1.0, 0.0, "Hardens the shadow's edge before blurring."),
            MIX,
        ],
    },
    EffectInfo {
        kind: EffectKind::Stroke, name: "stroke", category: Category::Style, temporal: false,
        doc: "An outline of exact width around the layer's alpha (a distance field, so any width stays round).",
        params: &[
            n("width", 0.0, 1000.0, 4.0, "Outline width in pixels."),
            c("color", WHITE, "Outline colour."),
            n("opacity", 0.0, 1.0, 1.0, "Outline opacity."),
            ch("position", &["outside", "inside", "center"], "outside", "Where the outline sits relative to the edge."),
            MIX,
        ],
    },
    EffectInfo {
        kind: EffectKind::Bevel, name: "bevel", category: Category::Style, temporal: false,
        doc: "Bevel and emboss: the layer's edge lit as a raised, rounded surface.",
        params: &[
            n("size", 0.0, 500.0, 10.0, "Bevel width in pixels."),
            n("depth", 0.0, 20.0, 1.0, "Height of the bevel (steeper light and shade)."),
            n("angle", -360.0, 360.0, 120.0, "Light angle, Photoshop convention."),
            n("altitude", 0.0, 90.0, 30.0, "Light height above the surface in degrees."),
            c("highlight", WHITE, "Highlight colour (screened)."),
            n("highlight_opacity", 0.0, 1.0, 0.75, "Highlight opacity."),
            c("shadow", BLACK, "Shade colour (multiplied)."),
            n("shadow_opacity", 0.0, 1.0, 0.75, "Shade opacity."),
            n("soften", 0.0, 100.0, 0.0, "Extra blur of the lighting in pixels."),
            MIX,
        ],
    },
    EffectInfo {
        kind: EffectKind::Fill, name: "fill", category: Category::Style, temporal: false,
        doc: "A solid colour: over the layer's alpha (clip, a colour overlay) or over the whole frame.",
        params: &[c("color", WHITE, "Fill colour."), BLEND, OPACITY, ParamInfo { name: "clip", kind: ParamKind::Bool { default: true }, doc: "Keep the fill inside the input's alpha." }],
    },
    // ── Generators ──────────────────────────────────────────────────────
    EffectInfo {
        kind: EffectKind::Gradient, name: "gradient", category: Category::Generate, temporal: false,
        doc: "A multi-stop gradient, dithered so it never bands. Over the whole frame, or inside the input's alpha with clip: true (a gradient overlay).",
        params: &[
            ch("kind", &["linear", "radial", "conic", "diamond"], "linear", "Gradient shape."),
            v2("start", [0.0, 0.5], "Linear: the start point. Radial, conic, diamond: the centre. 0..1 of the frame."),
            v2("end", [1.0, 0.5], "Linear: the end point. Radial and diamond: a point on the outer edge. Conic: the direction of angle 0."),
            ParamInfo { name: "stops", kind: ParamKind::Stops { default: &[(0.0, BLACK), (1.0, WHITE)] }, doc: "Colour stops, position 0..1 along the gradient." },
            ch("repeat", &["pad", "repeat", "mirror"], "pad", "Beyond the ends: hold the end colours, repeat, or mirror."),
            BLEND, OPACITY, CLIP,
        ],
    },
    EffectInfo {
        kind: EffectKind::Noise, name: "noise", category: Category::Generate, temporal: false,
        doc: "Procedural noise, evolving smoothly with time: value, perlin (gradient), fbm, turbulent, ridged or cellular (Worley). Deterministic: the same seed and t give the same pixels.",
        params: &[
            ch("kind", &["value", "perlin", "fbm", "turbulent", "ridged", "cellular"], "fbm", "Noise type."),
            n("scale", 1.0, 8000.0, 120.0, "Feature size in pixels."),
            n("octaves", 1.0, 8.0, 4.0, "Layers of detail (fbm, turbulent, ridged)."),
            n("lacunarity", 1.0, 4.0, 2.0, "Scale step between octaves."),
            n("gain", 0.0, 1.0, 0.5, "Amplitude step between octaves."),
            n("evolution", 0.0, 100.0, 0.3, "How fast the pattern changes, per second."),
            v2("offset", [0.0, 0.0], "Scroll in pixels (key it to drift)."),
            n("seed", 0.0, 1e6, 0.0, "Pattern seed."),
            n("contrast", 0.0, 8.0, 1.0, "Contrast around mid grey."),
            n("brightness", -1.0, 1.0, 0.0, "Offset."),
            c("color_a", BLACK, "Colour at 0."),
            c("color_b", WHITE, "Colour at 1."),
            BLEND, OPACITY, CLIP,
        ],
    },
    EffectInfo {
        kind: EffectKind::StrokePath, name: "stroke_path", category: Category::Generate, temporal: false,
        doc: "A stroke drawn along a path, trimmed (draw-on) and tapered at either end. The path is `points` (pixels) or SVG path data in `svg` (M, L, H, V, C, S, Q, T, Z, absolute or relative).",
        params: &[
            ParamInfo { name: "points", kind: ParamKind::Points, doc: "Path points in pixels (a polyline)." },
            ParamInfo { name: "svg", kind: ParamKind::Text, doc: "SVG path data in pixels, used when `points` is empty." },
            b("closed", false, "Close the path (points only; SVG closes with Z)."),
            n("width", 0.0, 1000.0, 8.0, "Stroke width in pixels."),
            n("taper_start", 0.0, 1.0, 0.0, "Fraction of the drawn length over which the width grows from 0."),
            n("taper_end", 0.0, 1.0, 0.0, "Fraction of the drawn length over which the width shrinks to 0."),
            n("trim_start", 0.0, 1.0, 0.0, "Start of the drawn part, 0..1 of the path length."),
            n("trim_end", 0.0, 1.0, 1.0, "End of the drawn part (key 0 → 1 to draw the path on)."),
            n("offset", -10.0, 10.0, 0.0, "Shifts the trimmed window along the path (wraps on closed paths)."),
            ch("cap", &["round", "butt"], "round", "Stroke ends."),
            c("color", WHITE, "Stroke colour."),
            OPACITY,
            ch("layer", &["over", "under", "only"], "over", "Draw the stroke over the input, under it, or alone."),
        ],
    },
    EffectInfo {
        kind: EffectKind::Particles, name: "particles", category: Category::Generate, temporal: false,
        doc: "Deterministic particles: every particle's birth, path and look is a closed-form function of its index, the seed and t, so any frame renders on its own. Emit from a point, a line, a circle, a rectangle, a list of points (a path), or the alpha of a layer (text: particles are born only inside the glyphs).",
        params: &[
            ch("emitter", &["point", "line", "circle", "rect", "points", "layer"], "point", "Emitter shape."),
            v2("position", CENTER, "Emitter centre, 0..1 of the frame."),
            v2("size", [0.2, 0.2], "Emitter extent as a fraction of the frame: line (x = length, y ignored, along `angle`), circle (x = radius), rect (width, height)."),
            n("angle", -360.0, 360.0, 0.0, "Rotation of a line or rect emitter, degrees."),
            ParamInfo { name: "points", kind: ParamKind::Points, doc: "Emitter points in pixels (`emitter: @points`); births are spread along the polyline through them." },
            layer("from", "The layer whose alpha emits (`emitter: @layer`), e.g. a text layer."),
            n("rate", 0.0, 100000.0, 60.0, "Births per second."),
            n("burst", 0.0, 100000.0, 0.0, "Extra particles born at once at `burst_time`."),
            n("burst_time", -HUGE, HUGE, 0.0, "When the burst happens (seconds)."),
            n("start", -HUGE, HUGE, 0.0, "Emission starts (seconds)."),
            n("end", -HUGE, HUGE, HUGE, "Emission ends (seconds)."),
            n("life", 0.01, 120.0, 2.0, "Lifetime in seconds."),
            n("life_var", 0.0, 1.0, 0.3, "Lifetime variation (fraction)."),
            n("speed", 0.0, 20000.0, 200.0, "Initial speed in pixels per second."),
            n("speed_var", 0.0, 1.0, 0.5, "Speed variation (fraction)."),
            n("direction", -360.0, 360.0, -90.0, "Emission direction in degrees (0 right, -90 up)."),
            n("spread", 0.0, 360.0, 30.0, "Cone of directions in degrees (360 all around)."),
            v2("gravity", [0.0, 300.0], "Acceleration in pixels per second² (y down)."),
            n("drag", 0.0, 50.0, 0.5, "Air drag per second."),
            n("turbulence", 0.0, 5000.0, 0.0, "Noise wander in pixels."),
            n("particle_size", 0.0, 1000.0, 8.0, "Size at birth in pixels."),
            n("size_end", 0.0, 1000.0, 2.0, "Size at death in pixels."),
            c("color", [1.0, 0.82, 0.48, 1.0], "Colour at birth."),
            c("color_end", [1.0, 0.29, 0.11, 1.0], "Colour at death."),
            n("fade_in", 0.0, 1.0, 0.1, "Fraction of the life spent fading in."),
            n("fade_out", 0.0, 1.0, 0.4, "Fraction of the life spent fading out."),
            n("spin", -10000.0, 10000.0, 0.0, "Rotation speed in degrees per second (random sign)."),
            ch("shape", &["circle", "square", "spark", "ring", "star"], "circle", "Particle shape (spark stretches along the velocity)."),
            ch("particle_blend", &["add", "normal", "screen"], "add", "How particles combine with each other."),
            n("seed", 0.0, 1e6, 0.0, "Random seed."),
            ch("layer", &["over", "under", "only"], "over", "Particles over the input, under it, or alone."),
        ],
    },
    // ── Distortion ──────────────────────────────────────────────────────
    EffectInfo {
        kind: EffectKind::Displace, name: "displace", category: Category::Distort, temporal: false,
        doc: "Displacement map: each pixel moves by a channel of a map layer (0.5 grey = no move).",
        params: &[
            layer("map", "The map layer; unset uses the input itself."),
            v2("amount", [20.0, 20.0], "Largest move in pixels on x and y."),
            ch("channel_x", &["red", "green", "blue", "alpha", "luma"], "red", "Channel that moves pixels horizontally."),
            ch("channel_y", &["red", "green", "blue", "alpha", "luma"], "green", "Channel that moves pixels vertically."),
            ch("edge", EDGE, "transparent", "What shows where a pixel moves in from outside the frame."),
            MIX,
        ],
    },
    // Slit-scan strips (lane FX2's `slices:` modifier): exact over
    // transparency because it resamples the layer's own picture.
    EffectInfo {
        kind: EffectKind::Slices, name: "slices", category: Category::Distort, temporal: false,
        doc: "Slit-scan: the picture cut into strips that jump along their length and snap back, one wave every `period` seconds running across the strips.",
        params: &[
            ch("dir", &["vertical", "horizontal", "diagonal", "cycle"], "vertical", "Strip direction; cycle turns vertical, horizontal, diagonal on successive waves."),
            n("count", 1.0, 400.0, 12.0, "Number of strips."),
            n("amp", 0.0, 4.0, 0.08, "Largest jump, as a fraction of the frame height (resolution independent)."),
            n("period", 0.01, 100.0, 0.5, "Seconds per wave (0.5 is a beat at 120 BPM)."),
            n("offset", -100.0, 100.0, 0.0, "Seconds added to the clock before the waves are counted."),
            n("spread", 0.0, 1.0, 0.35, "How far the wave lags across the strips, as a fraction of a wave."),
            n("length", 0.02, 1.0, 0.45, "How long a strip stays out, as a fraction of a wave."),
            n("seed", 0.0, 1e6, 1.0, "Which strips jump which way."),
            ch("edge", &["wrap", "transparent", "extend"], "wrap", "What shows where a strip moves in from outside the frame."),
        ],
    },
    EffectInfo {
        kind: EffectKind::Turbulence, name: "turbulence", category: Category::Distort, temporal: false,
        doc: "Turbulent displacement: the picture wobbles through evolving fbm noise (heat haze, water, liquid type).",
        params: &[
            n("amount", 0.0, 2000.0, 16.0, "Largest move in pixels."),
            n("size", 1.0, 8000.0, 120.0, "Size of the swirls in pixels."),
            n("octaves", 1.0, 8.0, 3.0, "Detail layers."),
            n("evolution", 0.0, 100.0, 0.5, "How fast it changes, per second."),
            n("seed", 0.0, 1e6, 0.0, "Pattern seed."),
            ch("edge", EDGE, "extend", "What shows where a pixel moves in from outside the frame."),
            MIX,
        ],
    },
    EffectInfo {
        kind: EffectKind::Ripple, name: "ripple", category: Category::Distort, temporal: false,
        doc: "Concentric ripples travelling out from a centre.",
        params: &[
            v2("center", CENTER, "Ripple centre, 0..1 of the frame."),
            n("wavelength", 1.0, 8000.0, 60.0, "Distance between crests in pixels."),
            n("amplitude", 0.0, 1000.0, 8.0, "Displacement in pixels."),
            n("speed", -10000.0, 10000.0, 120.0, "Outward speed in pixels per second."),
            n("decay", 0.0, 20.0, 1.0, "Fall-off with distance (per 1000 pixels)."),
            ch("edge", EDGE, "extend", "What shows where a pixel moves in from outside the frame."),
            MIX,
        ],
    },
    // ── Composite ───────────────────────────────────────────────────────
    EffectInfo {
        kind: EffectKind::Blend, name: "blend", category: Category::Composite, temporal: false,
        doc: "Combines another layer with this one in a blend mode (the W3C compositing set: separable and non-separable modes, premultiplied, exact).",
        params: &[
            layer("layer", "The other layer."),
            ch("mode", BLEND_MODES, "normal", "Blend mode."),
            n("opacity", 0.0, 1.0, 1.0, "Opacity of the other layer."),
            ch("order", &["over", "under"], "over", "The other layer over this one, or under it."),
        ],
    },
    EffectInfo {
        kind: EffectKind::Mask, name: "mask", category: Category::Composite, temporal: false,
        doc: "A shape mask (rectangle, rounded rectangle, ellipse) with an exact feathered edge (analytic, no blur pass).",
        params: &[
            ch("shape", &["rect", "rounded_rect", "ellipse"], "rect", "Mask shape."),
            v2("center", CENTER, "Centre, 0..1 of the frame."),
            v2("size", [0.5, 0.5], "Width and height, 0..1 of the frame."),
            n("radius", 0.0, 4000.0, 24.0, "Corner radius in pixels (rounded_rect)."),
            n("rotation", -360.0, 360.0, 0.0, "Rotation in degrees."),
            n("feather", 0.0, 4000.0, 0.0, "Edge softness in pixels (a gaussian edge of this standard deviation)."),
            n("expand", -4000.0, 4000.0, 0.0, "Grows (or shrinks) the shape in pixels."),
            b("invert", false, "Keep the outside instead."),
            n("opacity", 0.0, 1.0, 1.0, "Mask strength."),
        ],
    },
    EffectInfo {
        kind: EffectKind::Matte, name: "matte", category: Category::Composite, temporal: false,
        doc: "A track matte: this layer shows through another layer's alpha or luma, optionally inverted and feathered.",
        params: &[
            layer("layer", "The matte layer."),
            ch("mode", &["alpha", "luma"], "alpha", "Use the matte's alpha or its luminance."),
            b("invert", false, "Show where the matte is empty instead."),
            n("feather", 0.0, 4000.0, 0.0, "Softens the matte's edges in pixels (gaussian)."),
        ],
    },
    // ── 2.5D ────────────────────────────────────────────────────────────
    EffectInfo {
        kind: EffectKind::Transform3d, name: "transform_3d", category: Category::Space, temporal: false,
        doc: "Places the layer as a plane in 3D and renders it through the comp's camera (perspective, exact per pixel), with depth of field from the camera's focus and aperture.",
        params: &[
            v3("position", [0.0, 0.0, 0.0], "Offset in pixels: x right, y down, z away from the camera."),
            v3("rotation", [0.0, 0.0, 0.0], "Rotation in degrees about x (tilt), y (turn) and z (roll), applied in that order."),
            v3("scale", [1.0, 1.0, 1.0], "Scale."),
            v2("anchor", CENTER, "Pivot, 0..1 of the frame."),
            b("dof", true, "Blur by the camera's depth of field (no effect while the camera's aperture is 0)."),
        ],
    },
    // ── Time ────────────────────────────────────────────────────────────
    EffectInfo {
        kind: EffectKind::MotionBlur, name: "motion_blur", category: Category::Time, temporal: true,
        doc: "True motion blur: the layer (and the effects before this one) rendered at sub-frame times across the shutter and averaged. Exact, since a frame is a pure function of t. Samples adapt to the layer's motion when the host knows it; in preview a velocity blur stands in when the budget is short.",
        params: &[
            n("shutter_angle", 0.0, 720.0, 180.0, "Shutter angle in degrees (360 = the whole frame interval)."),
            n("phase", -360.0, 360.0, -90.0, "Shutter open offset in degrees (-90 with 180 centres the shutter on the frame)."),
            n("samples", 0.0, 1024.0, 0.0, "Sub-frames; 0 adapts to motion (between min_samples and max_samples)."),
            n("min_samples", 1.0, 1024.0, 4.0, "Fewest sub-frames when adapting."),
            n("max_samples", 1.0, 1024.0, 64.0, "Most sub-frames when adapting."),
            n("max_step", 0.1, 64.0, 1.0, "Adaptive: the largest move in pixels between sub-frames."),
            n("fps", 0.0, 1000.0, 0.0, "Frame rate the shutter refers to; 0 uses the host's."),
        ],
    },
    EffectInfo {
        kind: EffectKind::Echo, name: "echo", category: Category::Time, temporal: true,
        doc: "Echoes / trails: the layer at earlier times, fading, behind (or added to) the current one.",
        params: &[
            n("count", 1.0, 256.0, 6.0, "Number of echoes."),
            n("delay", -10.0, 10.0, 0.05, "Seconds between echoes (positive looks back in time)."),
            n("decay", 0.0, 1.0, 0.6, "Opacity of each echo relative to the one before."),
            ch("mode", &["over", "add", "screen"], "over", "How echoes combine: newest over older, added light, or screened."),
        ],
    },
    // ── Colour ──────────────────────────────────────────────────────────
    EffectInfo {
        kind: EffectKind::Curves, name: "curves", category: Category::Color, temporal: false,
        doc: "Tone curves (monotone cubic through the points), a master curve then per channel.",
        params: &[curve("master", "Curve applied to all channels."), curve("red", "Red channel curve."), curve("green", "Green channel curve."), curve("blue", "Blue channel curve."), MIX],
    },
    EffectInfo {
        kind: EffectKind::Levels, name: "levels", category: Category::Color, temporal: false,
        doc: "Input and output levels with gamma.",
        params: &[
            n("in_black", 0.0, 1.0, 0.0, "Input black point."),
            n("in_white", 0.0, 1.0, 1.0, "Input white point."),
            n("gamma", 0.05, 10.0, 1.0, "Midtone gamma (>1 brightens)."),
            n("out_black", 0.0, 1.0, 0.0, "Output black."),
            n("out_white", 0.0, 1.0, 1.0, "Output white."),
            ch("channel", &["rgb", "red", "green", "blue"], "rgb", "Channels affected."),
            MIX,
        ],
    },
    EffectInfo {
        kind: EffectKind::HueSaturation, name: "hue_saturation", category: Category::Color, temporal: false,
        doc: "Hue rotation, saturation and lightness, or colorize.",
        params: &[
            n("hue", -180.0, 180.0, 0.0, "Hue rotation in degrees."),
            n("saturation", -1.0, 1.0, 0.0, "-1 greyscale, 0 unchanged, 1 double."),
            n("lightness", -1.0, 1.0, 0.0, "-1 black, 1 white."),
            b("colorize", false, "Tint everything to one hue instead."),
            n("colorize_hue", 0.0, 360.0, 30.0, "Colorize hue in degrees."),
            n("colorize_saturation", 0.0, 1.0, 0.5, "Colorize saturation."),
            MIX,
        ],
    },
    EffectInfo {
        kind: EffectKind::WhiteBalance, name: "white_balance", category: Category::Color, temporal: false,
        doc: "Temperature (blue ↔ amber) and tint (green ↔ magenta), luminance-preserving.",
        params: &[n("temperature", -100.0, 100.0, 0.0, "Negative cooler, positive warmer."), n("tint", -100.0, 100.0, 0.0, "Negative green, positive magenta."), MIX],
    },
    EffectInfo {
        kind: EffectKind::Lut, name: "lut", category: Category::Color, temporal: false,
        doc: "A 3D colour LUT from a .cube file (trilinear).",
        params: &[ParamInfo { name: "file", kind: ParamKind::Text, doc: "Path to the .cube file." }, n("intensity", 0.0, 1.0, 1.0, "How much of the LUT.")],
    },
    EffectInfo {
        kind: EffectKind::ChromaticAberration, name: "chromatic_aberration", category: Category::Color, temporal: false,
        doc: "Red and blue split apart from green, radially (lens fringing) or along a direction.",
        params: &[
            n("amount", -500.0, 500.0, 4.0, "Split in pixels at the frame corners (radial) or everywhere (linear)."),
            v2("center", CENTER, "Lens centre, 0..1 of the frame."),
            ch("mode", &["radial", "linear"], "radial", "Radial or linear split."),
            n("angle", -360.0, 360.0, 0.0, "Direction of a linear split in degrees."),
            MIX,
        ],
    },
    EffectInfo {
        kind: EffectKind::Grain, name: "grain", category: Category::Color, temporal: false,
        doc: "Film grain, strongest in the midtones, deterministic per frame (seeded by time).",
        params: &[
            n("amount", 0.0, 1.0, 0.08, "Grain strength."),
            n("size", 0.5, 32.0, 1.4, "Grain size in pixels."),
            b("color", false, "Coloured grain instead of monochrome."),
            n("rate", 0.0, 240.0, 24.0, "New grain per second (0 = frozen)."),
            n("seed", 0.0, 1e6, 0.0, "Pattern seed."),
            MIX,
        ],
    },
    EffectInfo {
        kind: EffectKind::Vignette, name: "vignette", category: Category::Color, temporal: false,
        doc: "Darkened (or lightened) frame edges.",
        params: &[
            n("amount", -1.0, 1.0, 0.5, "Strength; negative lightens."),
            n("midpoint", 0.0, 1.0, 0.5, "Where the fall-off starts (0 centre, 1 corners)."),
            n("roundness", -1.0, 1.0, 0.0, "-1 follows the frame's shape, 1 a circle."),
            n("feather", 0.0, 1.0, 0.5, "Softness of the fall-off."),
            c("color", BLACK, "Vignette colour."),
            MIX,
        ],
    },
    EffectInfo {
        kind: EffectKind::Exposure, name: "exposure", category: Category::Color, temporal: false,
        doc: "Exposure in stops, offset and gamma.",
        params: &[n("exposure", -10.0, 10.0, 0.0, "Stops (+1 doubles)."), n("offset", -1.0, 1.0, 0.0, "Added after exposure."), n("gamma", 0.05, 10.0, 1.0, "Gamma correction."), MIX],
    },
    EffectInfo {
        kind: EffectKind::BrightnessContrast, name: "brightness_contrast", category: Category::Color, temporal: false,
        doc: "Brightness and contrast.",
        params: &[n("brightness", -1.0, 1.0, 0.0, "Brightness."), n("contrast", -1.0, 1.0, 0.0, "Contrast around mid grey."), MIX],
    },
    EffectInfo {
        kind: EffectKind::Tint, name: "tint", category: Category::Color, temporal: false,
        doc: "Maps luminance onto a two-colour ramp (duotone).",
        params: &[c("black", BLACK, "Colour for black."), c("white", WHITE, "Colour for white."), n("amount", 0.0, 1.0, 1.0, "How much of the tint.")],
    },
    EffectInfo {
        kind: EffectKind::Invert, name: "invert", category: Category::Color, temporal: false,
        doc: "Inverts the colours (alpha kept).",
        params: &[MIX],
    },
];

/// The comp camera's parameters (a Motion document's `camera`), keyable like
/// any effect's. See [`crate::space::Camera`].
pub static CAMERA: &[ParamInfo] = &[
    n("fov", 1.0, 170.0, 39.6, "Vertical field of view in degrees (39.6 is a 50 mm lens on 36 mm film)."),
    v3("target", [0.5, 0.5, 0.0], "Point of interest: x, y as 0..1 of the frame, z in pixels."),
    v2("orbit", [0.0, 0.0], "Orbit around the target in degrees: yaw (left/right), pitch (up/down)."),
    n("distance", 0.0, 1e6, 0.0, "Distance from the target in pixels; 0 is the distance at which z = 0 is 1:1 with the frame."),
    n("roll", -360.0, 360.0, 0.0, "Camera roll in degrees."),
    v3("offset", [0.0, 0.0, 0.0], "Moves camera and target together in pixels (a dolly or truck)."),
    n("focus", 0.0, 1e6, 0.0, "Focus distance in pixels from the camera; 0 focuses on the target."),
    n("aperture", 0.0, 2000.0, 0.0, "Blur diameter in pixels of something infinitely far away; 0 disables depth of field."),
];

pub fn info(kind: EffectKind) -> &'static EffectInfo {
    EFFECTS.iter().find(|e| e.kind == kind).expect("every kind is in the catalog")
}

pub fn by_name(name: &str) -> Option<&'static EffectInfo> {
    let name = name.trim().trim_start_matches('@').trim_start_matches("kit/");
    EFFECTS.iter().find(|e| e.name == name)
}

/// Edit distance with adjacent transpositions (optimal string alignment),
/// for "did you mean".
fn distance(a: &str, b: &str) -> usize {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    let mut d = vec![vec![0usize; b.len() + 1]; a.len() + 1];
    for (i, row) in d.iter_mut().enumerate() {
        row[0] = i;
    }
    for j in 0..=b.len() {
        d[0][j] = j;
    }
    for i in 1..=a.len() {
        for j in 1..=b.len() {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            d[i][j] = (d[i - 1][j] + 1).min(d[i][j - 1] + 1).min(d[i - 1][j - 1] + cost);
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                d[i][j] = d[i][j].min(d[i - 2][j - 2] + 1);
            }
        }
    }
    d[a.len()][b.len()]
}

pub fn closest<'a>(name: &str, candidates: impl Iterator<Item = &'a str>) -> Option<&'a str> {
    candidates
        .map(|c| (distance(name, c) + usize::from(!c.contains(name) && !name.contains(c)), c))
        .filter(|(d, c)| *d <= c.len().max(3) / 2 + 1)
        .min_by_key(|(d, _)| *d)
        .map(|(_, c)| c)
}

/// Converts a written value to the parameter's kind where that is
/// unambiguous (a number for a vec2 param broadcasts; a hex string is a
/// colour; a string names a choice), or explains what was expected.
pub fn coerce(param: &ParamInfo, value: Value) -> Result<Value, String> {
    let wrong = |value: &Value| format!("`{}` takes {}, not a {}", param.name, param.kind.syntax(), value.kind_name());
    Ok(match (&param.kind, value) {
        (ParamKind::Num { .. }, Value::Num(v)) => Value::Num(v),
        (ParamKind::Num { .. }, Value::Bool(v)) => Value::Num(if v { 1.0 } else { 0.0 }),
        (ParamKind::Vec2 { .. }, Value::Vec2(v)) => Value::Vec2(v),
        (ParamKind::Vec2 { .. }, Value::Num(v)) => Value::Vec2([v, v]),
        (ParamKind::Vec3 { .. }, Value::Vec3(v)) => Value::Vec3(v),
        (ParamKind::Vec3 { .. }, Value::Vec2(v)) => Value::Vec3([v[0], v[1], 0.0]),
        (ParamKind::Vec3 { .. }, Value::Num(v)) => Value::Vec3([v, v, v]),
        (ParamKind::Color { .. }, Value::Color(v)) => Value::Color(v),
        (ParamKind::Color { .. }, Value::Text(text) | Value::Choice(text)) => {
            Value::Color(parse_hex_color(&text).ok_or_else(|| format!("`{}`: `{text}` is not a colour; write #rrggbb or #rrggbbaa", param.name))?)
        }
        (ParamKind::Bool { .. }, Value::Bool(v)) => Value::Bool(v),
        (ParamKind::Bool { .. }, Value::Num(v)) => Value::Bool(v != 0.0),
        (ParamKind::Choice { choices, .. }, Value::Choice(text) | Value::Text(text)) => {
            let text = text.trim_start_matches('@').to_string();
            if !choices.contains(&text.as_str()) {
                let hint = closest(&text, choices.iter().copied()).map(|c| format!(" Did you mean @{c}?")).unwrap_or_default();
                return Err(format!("`{}`: @{text} is not a choice; it takes {}.{hint}", param.name, param.kind.syntax()));
            }
            Value::Choice(text)
        }
        (ParamKind::Text, Value::Text(text) | Value::Choice(text)) => Value::Text(text),
        (ParamKind::Layer, Value::Layer(text) | Value::Text(text) | Value::Choice(text)) => Value::Layer(text.trim_start_matches('@').to_string()),
        (ParamKind::Points | ParamKind::Curve, Value::Points(points)) => Value::Points(points),
        (ParamKind::Stops { .. }, Value::Stops(stops)) => Value::Stops(stops),
        (_, value) => return Err(wrong(&value)),
    })
}

/// A numeric value outside its range, explained (the kit clamps it).
pub fn range_warning(param: &ParamInfo, value: &Value) -> Option<String> {
    if let (ParamKind::Num { min, max, .. }, Value::Num(v)) = (&param.kind, value) {
        if !v.is_finite() || *v < *min || *v > *max {
            return Some(format!("`{}` = {} is outside {} to {}; it is clamped", param.name, v, num(*min), num(*max)));
        }
    }
    None
}

/// Checks an animation against a parameter, converting each value.
pub fn coerce_anim(param: &ParamInfo, anim: Anim, warnings: &mut Vec<String>) -> Result<Anim, String> {
    Ok(match anim {
        Anim::Const(value) => {
            let value = coerce(param, value)?;
            warnings.extend(range_warning(param, &value));
            Anim::Const(value)
        }
        Anim::Keys(keys) => {
            let mut out = Vec::with_capacity(keys.len());
            for key in keys {
                let v = coerce(param, key.v).map_err(|e| format!("{e} (in the key at t={})", key.t))?;
                warnings.extend(range_warning(param, &v));
                out.push(crate::anim::Key { t: key.t, v, ease: key.ease });
            }
            Anim::keys(out).map_err(|e| format!("`{}`: {e}", param.name))?
        }
        sampled @ Anim::Sampled(_) => sampled,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_catalog_is_consistent() {
        let mut names = std::collections::HashSet::new();
        for effect in EFFECTS {
            assert!(names.insert(effect.name), "duplicate effect {}", effect.name);
            assert_eq!(info(effect.kind).name, effect.name);
            let mut params = std::collections::HashSet::new();
            for p in effect.params {
                assert!(params.insert(p.name), "{}: duplicate param {}", effect.name, p.name);
                assert!(!p.doc.is_empty());
                if let ParamKind::Num { min, max, default } = p.kind {
                    assert!(min <= default && default <= max, "{}.{} default out of range", effect.name, p.name);
                }
                if let ParamKind::Choice { choices, default } = p.kind {
                    assert!(choices.contains(&default), "{}.{}", effect.name, p.name);
                }
            }
        }
        assert!(EFFECTS.len() >= 37);
    }

    #[test]
    fn coercion_teaches() {
        let blur = by_name("gaussian_blur").unwrap();
        let (_, direction) = blur.param("direction").unwrap();
        let err = coerce(direction, Value::Choice("horizontl".into())).unwrap_err();
        assert!(err.contains("Did you mean @horizontal?"), "{err}");
        let (_, radius) = blur.param("radius").unwrap();
        assert!(coerce(radius, Value::Color([1.0; 4])).unwrap_err().contains("a number 0 to 2000"));
        assert!(range_warning(radius, &Value::Num(-1.0)).unwrap().contains("clamped"));
        let shadow = by_name("drop_shadow").unwrap();
        assert_eq!(coerce(shadow.param("color").unwrap().1, Value::Text("#ff0000".into())).unwrap(), Value::Color([1.0, 0.0, 0.0, 1.0]));
        assert_eq!(closest("gausian_blur", EFFECTS.iter().map(|e| e.name)), Some("gaussian_blur"));
        assert_eq!(closest("glw", EFFECTS.iter().map(|e| e.name)), Some("glow"));
        assert!(by_name("@kit/bloom").is_some());
    }
}
