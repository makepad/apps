//! `Text3D{...}` Motion layers: 3D kinetic typography in one line.
//!
//! A Text3D layer names a preset (`preset: @chrome_title`) and the words.
//! The presets are Splash: the module `mod.motion.modules.text3d`, which a
//! host installs with its text pack ([`crate::modules::install`]; Stage's
//! kinetic text pack), each preset its look (the defaults a layer
//! overrides) and a `build` function that fills a motion3d `Scene3D`
//! round motion3d's `Text3D` node (per-letter meshes): the camera, the
//! preset's lights, environment, floor and post, and the per-unit
//! animation (`each` / `spread` / `alternate` keyed on the layer's clock,
//! staggered per letter, word, line or shard). The layer lowers to a
//! Scene3D layer (`LayerKind::Scene3dDoc`) whose scene is
//! `text3d_scene(in(@text3d))` (text3d_scene.splash), with the layer
//! resolved here into that input: its words, font file, colours, sizes
//! and the block measured from the font. So frames are a pure function of
//! the layer's time, `span: [beat(4), …]` binds the entrance to the music
//! like any layer, and `pulse` binds the look to the beat through motion3d's
//! `music(@beat)`.
//!
//! The presets port the looks of the kinetic type kits (letter flip, gold
//! ogee, facet toss, neon tube, chrome text, wave line, text tunnel, letter
//! ring, shear poster, glass slab, puffy pop, holo foil, letter snap, soft
//! plastic, cascade drop) as timed entrances for edits instead of
//! beat-looped scenes.

use crate::model::{Color, InputValue};
use crate::obj::{self, Look as L};
use makepad_script::{ScriptObject, ScriptVm};
#[cfg(feature = "scene3d")]
use makepad_text_mesh::letters::{build_text3d, FontSource, Text3dParams, TextAlign};
use std::path::PathBuf;

/// The scene a Text3D layer draws: the preset run for the `text3d` input.
pub const SCENE: &str = include_str!("text3d_scene.splash");

/// What a preset is for (the families the AI picks from).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Family {
    /// Entrances: the words arrive and hold.
    Reveal,
    /// Seamless motion for beds and backgrounds (loops on `loop_period`).
    Loop,
    /// Hero titles and end cards.
    Title,
    /// Names and captions in the lower left.
    LowerThird,
    /// Figures, prices and counters.
    Number,
    /// Type laid out in space: paths, rings, helices, grids, tunnels.
    Space,
    /// A material look first (neon, glass, foil, ice, clay, voxels…).
    Material,
    /// Type that forms from, or dissolves into, particles.
    Particle,
}

impl Family {
    pub const ALL: [Family; 8] = [Family::Reveal, Family::Loop, Family::Title, Family::LowerThird, Family::Number, Family::Space, Family::Material, Family::Particle];
    pub fn name(self) -> &'static str {
        match self {
            Family::Reveal => "reveal",
            Family::Loop => "loop",
            Family::Title => "title",
            Family::LowerThird => "lower_third",
            Family::Number => "number",
            Family::Space => "space",
            Family::Material => "material",
            Family::Particle => "particle",
        }
    }
    fn of(name: &str) -> Option<Family> {
        Family::ALL.into_iter().find(|f| f.name() == name)
    }
}

/// A preset as the module declares it: its name, its family, the look in
/// one line, what it is best for, a good layer length, the kinetic kit
/// whose look it ports, and its look (the defaults a layer overrides).
#[derive(Clone, Debug)]
pub struct Preset {
    pub name: String,
    pub family: Family,
    /// The look and the motion, one line.
    pub about: String,
    /// Where it works best.
    pub best: String,
    /// A good layer length in seconds (entrance, hold and room for an exit).
    pub length: f64,
    pub after: String,
    pub look: Look,
}

impl Preset {
    /// One line for the AI: `@name (family, ~Ns): look — best for …`.
    pub fn describe(&self) -> String {
        format!("@{} ({}, ~{} s): {} — best for {} (after {})", self.name, self.family.name(), crate::value::fmt_num(self.length), self.about, self.best, self.after)
    }
}

/// A preset's defaults; every one can be overridden on the layer.
#[derive(Clone, Debug)]
pub struct Look {
    pub font: String,
    pub weight: f64,
    /// 0xrrggbb.
    pub background: u32,
    pub color: u32,
    pub accent: u32,
    pub material: String,
    pub depth: f64,
    pub bevel: f64,
    pub bevel_type: String,
    /// Entrance length in seconds (per unit, before the stagger).
    pub duration: f64,
    pub by: String,
    pub order: String,
    pub fov: f64,
    pub environment: String,
    pub floor: bool,
    /// Share of the width the block spans by default (0 = 0.78).
    pub fit: f64,
    pub align: String,
}

/// Every preset, in the module's order; none when no host installed the
/// `text3d` module.
pub fn presets() -> Vec<Preset> {
    let Some(module) = crate::modules::builtin("text3d") else { return Vec::new() };
    let text = module.text;
    let mut evaluated = crate::value::evaluate(&crate::model::DocSource::file(&text), crate::MotionContext::default().signals, 0.0, &[], &crate::modules::NoModules, None).unwrap_or_else(|diags| panic!("mod.motion.modules.text3d: {diags:?}"));
    assert!(evaluated.errors.is_empty(), "mod.motion.modules.text3d: {:?}", evaluated.errors);
    let root = evaluated.root;
    evaluated.with_vm(|vm| {
        let Some(L::Obj(all, _)) = root.as_object().and_then(|o| obj::look_field(vm, o, "text3d_presets")) else { panic!("mod.motion.modules.text3d has no text3d_presets") };
        obj::fields(vm, all).into_iter().filter(|(name, _, _)| !name.starts_with("__")).map(|(name, preset, _)| read_preset(vm, &name, &obj::look(vm, preset)).unwrap_or_else(|e| panic!("mod.motion.modules.text3d: {e}"))).collect()
    })
}

pub fn preset(name: &str) -> Option<Preset> {
    let key = name.trim().trim_start_matches('@').replace('-', "_").to_ascii_lowercase();
    presets().into_iter().find(|p| p.name == key)
}

fn read_preset(vm: &ScriptVm, name: &str, value: &L) -> Result<Preset, String> {
    let bad = |field: &str| format!("preset `{name}`: `{field}` is missing or of the wrong kind");
    let o = value.obj().ok_or_else(|| bad("{…}"))?;
    let get = |o: ScriptObject, field: &str| obj::look_field(vm, o, field);
    let text = |o: ScriptObject, field: &str| match get(o, field) {
        Some(L::Str(s)) | Some(L::Id(s)) => Ok(s),
        _ => Err(bad(field)),
    };
    let num = |o: ScriptObject, field: &str| get(o, field).and_then(|v| v.num()).ok_or_else(|| bad(field));
    let rgb = |o: ScriptObject, field: &str| match get(o, field) {
        Some(L::Color(c)) => Ok(c >> 8),
        _ => Err(bad(field)),
    };
    let l = get(o, "look").and_then(|v| v.obj()).ok_or_else(|| bad("look"))?;
    Ok(Preset {
        name: name.into(),
        family: text(o, "family").ok().and_then(|f| Family::of(&f)).ok_or_else(|| bad("family"))?,
        about: text(o, "about")?,
        best: text(o, "best")?,
        length: num(o, "length")?,
        after: text(o, "after")?,
        look: Look {
            font: text(l, "font")?,
            weight: num(l, "weight")?,
            background: rgb(l, "background")?,
            color: rgb(l, "color")?,
            accent: rgb(l, "accent")?,
            material: text(l, "material")?,
            depth: num(l, "depth")?,
            bevel: num(l, "bevel")?,
            bevel_type: text(l, "bevel_type")?,
            duration: num(l, "duration")?,
            by: text(l, "by")?,
            order: text(l, "order")?,
            fov: num(l, "fov")?,
            environment: text(l, "environment")?,
            floor: matches!(get(l, "floor"), Some(L::Bool(true))),
            fit: num(l, "fit")?,
            align: text(l, "align")?,
        },
    })
}

/// The material names a layer's `material` can take: motion3d's presets
/// plus `neon` (emissive tubes), `holo` (iridescent foil) and `soft` (a
/// satin plastic).
pub const MATERIALS: &[&str] = &["chrome", "gold", "copper", "brushed_metal", "car_paint", "glass", "frosted_glass", "plastic", "rubber", "ceramic", "velvet", "matte", "neon", "holo", "soft", "wire", "flat"];

/// What a Text3D layer asks for; `None` takes the preset's default.
#[derive(Clone, Debug, Default)]
pub struct Text3dSpec {
    pub text: String,
    pub preset: String,
    /// A Motion font name or a font file.
    pub font: Option<String>,
    pub weight: Option<f64>,
    pub italic: bool,
    pub material: Option<String>,
    pub color: Option<Color>,
    pub accent: Option<Color>,
    /// `Some(None)`: transparent (`background: @none`).
    pub background: Option<Option<Color>>,
    pub depth: Option<f64>,
    pub bevel: Option<f64>,
    pub bevel_type: Option<String>,
    pub tracking: Option<f64>,
    pub line_height: Option<f64>,
    pub align: Option<String>,
    pub by: Option<String>,
    pub order: Option<String>,
    pub stagger: Option<f64>,
    /// Entrance length per unit (seconds).
    pub duration: Option<f64>,
    /// When the exit starts (layer seconds); None = no exit.
    pub exit: Option<f64>,
    /// Beat reaction 0..1 (scale and glow on `music(@beat)`).
    pub pulse: f64,
    pub music: Option<String>,
    /// The share of the layer's width the text spans (0..1).
    pub fit: f64,
    /// The layer's box in document units.
    pub size: [f64; 2],
    pub environment: Option<String>,
    pub floor: Option<bool>,
    pub motion_blur: bool,
    pub seed: u64,
    /// Karaoke: each word's (start, end) on the layer clock; empty = one
    /// word per `stagger` from `duration`'s start.
    pub word_times: Vec<[f64; 2]>,
    /// Karaoke state colours: the not-yet-sung ghost (its alpha is its
    /// opacity) and the word being sung (default: the accent).
    pub ghost: Option<Color>,
    pub highlight: Option<Color>,
}

/// The font file behind a Motion font name (see `makepad_doc_fonts`: a
/// family's face), or the name itself
/// when it is a path.
pub fn font_file(name: &str, weight: f64, italic: bool) -> Option<PathBuf> {
    if name.contains('.') || name.contains('/') {
        return Some(PathBuf::from(name));
    }
    crate::font_spec::font_roots();
    makepad_doc_fonts::font_file(name, weight, italic)
}

/// A block's extent and its unit counts, from motion3d's flat-cap build.
struct Measured {
    min: [f32; 3],
    max: [f32; 3],
    letters: usize,
    words: usize,
    lines: usize,
}

#[cfg(feature = "scene3d")]
fn measure(text: &str, file: &std::path::Path, weight: f64, tracking: f64, line_height: f64, align: &str) -> Result<Measured, String> {
    let m = build_text3d(&Text3dParams {
        text: text.to_string(),
        font: FontSource::Path(file.to_path_buf()),
        weight: Some(weight as f32),
        depth: 0.0,
        bevel: 0.0,
        detail: 0.05,
        tracking: tracking as f32,
        line_height: line_height as f32,
        align: match align {
            "left" => TextAlign::Left,
            "right" => TextAlign::Right,
            _ => TextAlign::Center,
        },
        ..Text3dParams::default()
    })?;
    Ok(Measured { min: m.bounds.0, max: m.bounds.1, letters: m.letters.len(), words: m.words, lines: m.lines })
}

/// Without the GPU side there is no font outline to measure: a Text3D
/// lowers only where motion3d is built.
#[cfg(not(feature = "scene3d"))]
fn measure(_text: &str, _file: &std::path::Path, _weight: f64, _tracking: f64, _line_height: f64, _align: &str) -> Result<Measured, String> {
    Err("Text3D needs makepad-motion's `scene3d` feature (motion3d measures its letters)".into())
}

fn unpack(c: u32) -> Color {
    [((c >> 16) & 255) as f64 / 255.0, ((c >> 8) & 255) as f64 / 255.0, (c & 255) as f64 / 255.0, 1.0]
}

/// Lower a Text3D layer to the `text3d` input of [`SCENE`]: the layer
/// with the preset's look where it set nothing, the block measured from
/// the font, and the framing.
pub fn lower(spec: &Text3dSpec) -> Result<Vec<(String, InputValue)>, String> {
    let all = presets();
    if all.is_empty() {
        return Err(format!("no Text3D preset `@{}`: the Text3D presets come with a text pack (Stage's kinetic text pack), which this host has not installed", spec.preset.trim().trim_start_matches('@')));
    }
    let key = spec.preset.trim().trim_start_matches('@').replace('-', "_").to_ascii_lowercase();
    let preset = all.iter().find(|p| p.name == key).ok_or_else(|| {
        format!("no Text3D preset `@{}`; presets: {}", spec.preset, all.iter().map(|p| format!("@{}", p.name)).collect::<Vec<_>>().join(" "))
    })?;
    let look = &preset.look;
    if spec.text.trim().is_empty() {
        return Err("a Text3D needs `text`".into());
    }
    let font_name = spec.font.clone().unwrap_or_else(|| look.font.clone());
    let weight = spec.weight.unwrap_or(look.weight);
    let file = font_file(&font_name, weight, spec.italic).ok_or_else(|| format!("no font `{font_name}`; fonts: {}", makepad_doc_fonts::bundled().collect::<Vec<_>>().join(", ")))?;
    let font = file.to_string_lossy().replace('\\', "/");
    let material = spec.material.clone().unwrap_or_else(|| look.material.clone());
    if !MATERIALS.contains(&material.as_str()) {
        return Err(format!("no Text3D material `@{material}`; materials: {}", MATERIALS.iter().map(|m| format!("@{m}")).collect::<Vec<_>>().join(" ")));
    }
    let depth = spec.depth.unwrap_or(look.depth).max(0.0);
    let bevel = spec.bevel.unwrap_or(look.bevel).max(0.0);
    let bevel_type = spec.bevel_type.clone().unwrap_or_else(|| look.bevel_type.clone());
    let tracking = spec.tracking.unwrap_or(0.0);
    let line_height = spec.line_height.unwrap_or(1.0);
    let align = match spec.align.as_deref().unwrap_or(&look.align) {
        "left" => "left",
        "right" => "right",
        _ => "center",
    };
    // Measure the block (flat caps only: cheap).
    let measured = measure(&spec.text, &file, weight, tracking, line_height, align)?;
    let (mn, mx) = (measured.min, measured.max);
    let w = ((mx[0] - mn[0]) as f64).max(0.5);
    let h = ((mx[1] - mn[1]) as f64).max(0.5);
    let units = |by: &str| match by {
        "word" => measured.words.max(1),
        "line" => measured.lines.max(1),
        "whole" => 1,
        "piece" => measured.letters.max(1) * 7,
        _ => measured.letters.max(1),
    };
    let mut by = spec.by.clone().unwrap_or_else(|| look.by.clone());
    // A one-word text dropping by word drops as a whole: go by letter.
    if by == "word" && measured.words < 2 {
        by = if measured.lines > 1 { "line".into() } else { "letter".into() };
    }
    let dur = spec.duration.unwrap_or(look.duration).max(0.05);
    let count = units(&by) as f64;
    // Shards all land within half a second, so every letter closes (and is
    // welded whole) soon after the entrance.
    let stagger = spec.stagger.unwrap_or_else(|| if count <= 1.0 { 0.0 } else if by == "piece" { 0.5f64 / count } else { (1.0f64 / count).clamp(0.02, if by == "letter" { 0.08 } else { 0.22 }) });
    let aspect = (spec.size[0] / spec.size[1].max(1.0)).max(0.05);
    let fit = if spec.fit > 0.0 { spec.fit.min(1.0) } else if look.fit > 0.0 { look.fit } else { 0.78 };
    let fov = look.fov;
    let tan = (fov.to_radians() * 0.5).tan();
    // The distance at which the block spans `fit` of the width (and at
    // most 70% of the height).
    let d = (w / (fit * 2.0 * tan * aspect)).max(h / (0.7 * 2.0 * tan)) + depth * 0.5;
    let background = match spec.background {
        Some(bg) => bg,
        // A lower third sits over the picture: transparent unless asked.
        None if preset.family == Family::LowerThird => None,
        None => Some(unpack(look.background)),
    };
    let digit_advance = if preset.family == Family::Number {
        let one = measure("0", &file, weight, 0.0, 1.0, "center")?;
        let two = measure("00", &file, weight, 0.0, 1.0, "center")?;
        ((two.max[0] - two.min[0]) - (one.max[0] - one.min[0])) as f64
    } else {
        0.6
    };
    let num = |v: f64| InputValue::Number(v);
    let text = |s: &str| InputValue::Text(s.to_string());
    let flag = |b: bool| InputValue::Bool(b);
    // Opaque, as the scene's colours are written (an author's alpha is the layer's opacity).
    let color = |c: Color| InputValue::Color([c[0], c[1], c[2], 1.0]);
    let mut c: Vec<(&str, InputValue)> = vec![
        ("preset", text(&preset.name)),
        ("text", text(&spec.text)),
        ("one_line", text(&spec.text.replace('\n', " "))),
        // One cell a character, a space for any whitespace (a board's columns).
        ("cells", InputValue::List(spec.text.chars().filter(|ch| *ch != '\n' && *ch != '\r').map(|ch| text(&if ch.is_whitespace() { " ".to_string() } else { ch.to_string() })).collect())),
        ("font", text(&font)),
        ("weight", num(weight)),
        ("color", color(spec.color.unwrap_or_else(|| unpack(look.color)))),
        ("accent", color(spec.accent.unwrap_or_else(|| unpack(look.accent)))),
        ("has_bg", flag(background.is_some())),
        ("bg", color(background.unwrap_or([0.0, 0.0, 0.0, 1.0]))),
        ("material", text(&material)),
        ("depth", num(depth)),
        ("bevel", num(bevel)),
        ("bevel_type", text(&bevel_type)),
        ("tracking", num(tracking)),
        ("line_height", num(line_height)),
        ("align", text(align)),
        ("by", text(&by)),
        ("order", text(spec.order.as_deref().unwrap_or(&look.order))),
        ("stagger", num(stagger)),
        ("dur", num(dur)),
        ("has_exit", flag(spec.exit.is_some())),
        ("exit", num(spec.exit.unwrap_or(0.0))),
        ("w", num(w)),
        ("h", num(h)),
        ("bottom", num(mn[1] as f64)),
        ("lines", num(measured.lines as f64)),
        ("words", num(measured.words as f64)),
        ("letters", num(measured.letters as f64)),
        ("digit_advance", num(digit_advance)),
        // The duration the layer asked for (the landing instant for rolls).
        ("land", num(dur)),
        ("word_times", InputValue::List(spec.word_times.iter().map(|t| InputValue::List(vec![num(t[0]), num(t[1])])).collect())),
        ("has_ghost", flag(spec.ghost.is_some())),
        ("ghost", InputValue::List(spec.ghost.unwrap_or([0.0; 4]).iter().map(|v| num(*v)).collect())),
        ("has_highlight", flag(spec.highlight.is_some())),
        ("highlight", color(spec.highlight.unwrap_or([0.0, 0.0, 0.0, 1.0]))),
        ("d", num(d)),
        ("fit", num(fit)),
        ("fov", num(fov)),
        ("aspect", num(aspect)),
        ("pulse", num(spec.pulse.clamp(0.0, 1.0))),
        ("seed", num(spec.seed as f64)),
        ("environment", text(spec.environment.as_deref().unwrap_or(&look.environment))),
        ("floor", flag(spec.floor.unwrap_or(look.floor) && background.is_some())),
        ("floor_off", flag(spec.floor == Some(false))),
        ("has_music", flag(spec.music.is_some())),
        ("music", text(spec.music.as_deref().unwrap_or(""))),
        ("motion_blur", flag(spec.motion_blur)),
    ];
    // A mark ending in a full stop: where the word and the dot sit when
    // the whole mark is centred (a logo with an accent dot).
    let trimmed = spec.text.trim_end();
    let word = trimmed.trim_end_matches('.');
    let mut dot = None;
    if trimmed.ends_with('.') {
        if let (Ok(all), Ok(wm), Ok(dm)) = (measure(trimmed, &file, weight, tracking, 1.0, "center"), measure(word, &file, weight, tracking, 1.0, "center"), measure(".", &file, weight, 0.0, 1.0, "center")) {
            let (wa, ww, wd) = ((all.max[0] - all.min[0]) as f64, (wm.max[0] - wm.min[0]) as f64, (dm.max[0] - dm.min[0]) as f64);
            dot = Some((-(wa - ww) * 0.5, wa * 0.5 - wd * 0.5, all.min[1] as f64 + (dm.max[1] - dm.min[1]) as f64 * 0.5));
        }
    }
    c.push(("has_dot", flag(dot.is_some())));
    c.push(("dot_word", text(if dot.is_some() { word } else { &spec.text })));
    let (word_x, dx, dy) = dot.unwrap_or((0.0, 0.0, 0.0));
    c.push(("dot_word_x", num(word_x)));
    c.push(("dot_x", num(dx)));
    c.push(("dot_y", num(dy)));
    Ok(vec![("text3d".to_string(), InputValue::Record(c.into_iter().map(|(k, v)| (k.to_string(), v)).collect()))])
}

