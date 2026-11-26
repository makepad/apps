//! `Seq3D{template: @logo_sting …}`: whole ready-made 3D sequences (5–10 s)
//! in one line. A template is parameterised with text, colours, a logo SVG,
//! an image or video, a product model, numbers, map points and a bpm, and
//! builds an ordinary `Scene3D{…}` from the effects library (`Fx`), the kits
//! and 3D type (`Text3D`). The templates are Splash (`seq3d.splash`, Motion's
//! built-in module `mod.motion.modules.seq3d`): `seq3d_scene(s)` builds the
//! scene of the Seq3D `s`, as [`record`] gives it. Nothing is opaque: a
//! template can be copied out of the module and edited like any scene.
//! Times are seconds, or `beat(n)` when a bpm is given, so the key moments
//! land on the beat grid.

use crate::script::InputValue;
use makepad_widgets::*;

/// A picture or clip for screens and transitions.
#[derive(Clone, Debug, PartialEq)]
pub enum Media {
    /// A texture the host supplies by name (a video, an app run, a 2D layer).
    Texture(String),
    File(String),
    Asset { id: String, rev: String },
}


/// Crop keys `(t, [x, y, w, h], ease id)` in a clip's pixels.
#[derive(Clone, Debug, PartialEq)]
pub struct CropKeys(pub Vec<(f64, [f64; 4], String)>);

/// The logo mark: an SVG path `d`, an inline `<svg …>` document, or a file.
#[derive(Clone, Debug, PartialEq)]
pub enum Logo {
    Path(String),
    Svg(String),
    Src(Media),
}

impl Logo {
    /// A string: `<svg` = a document, else a path `d`.
    pub fn from_text(s: &str) -> Logo {
        if s.trim_start().starts_with('<') { Logo::Svg(s.to_string()) } else { Logo::Path(s.to_string()) }
    }
}

/// Everything a Seq3D takes. Unset fields fall back to the template's.
#[derive(Clone, Debug, PartialEq)]
pub struct Seq3dSpec {
    pub template: String,
    pub text: Option<String>,
    pub subtitle: Option<String>,
    /// sRGB 0..1.
    pub color: Option<[f32; 3]>,
    pub accent: Option<[f32; 3]>,
    /// `None` = the template's; `Some(None)` = transparent (over 2D layers).
    pub background: Option<Option<[f32; 3]>>,
    pub logo: Option<Logo>,
    /// A picture for screens (device showcase) or the product label.
    pub image: Option<Media>,
    /// A clip for screens; in Motion a `file()`/`asset()` video is fed to
    /// the scene as the texture `"seq_video"`.
    pub video: Option<Media>,
    /// The phone's own picture or clip in the device showcase (else the
    /// laptop's `image` / `video`).
    pub phone_image: Option<Media>,
    pub phone_video: Option<Media>,
    /// How pictures fit screens: `cover` (default: crop, never stretch),
    /// `contain` (letterbox) or `fill`.
    pub fit: Option<String>,
    /// What part of the clip each screen shows: crop keys in the clip's
    /// pixels (a reframe's `crop: keys(...)`, following the action).
    pub crop: Option<CropKeys>,
    pub phone_crop: Option<CropKeys>,
    /// The phone screen's width / height (`9 / 16` shows a 1080×1920 app
    /// run whole); default 9:19.5.
    pub phone_aspect: Option<f64>,
    /// A GLB for the product reveal.
    pub product: Option<Media>,
    pub font: Option<String>,
    pub bpm: Option<f64>,
    pub duration: Option<f64>,
    pub seed: u64,
    /// Countdown: from → to (inclusive). Number reveal: the figure is `text`.
    pub from: Option<i64>,
    pub to: Option<i64>,
    /// Data globe: (latitude, longitude) in degrees; arcs join them in order.
    pub points: Vec<[f64; 2]>,
}

impl Default for Seq3dSpec {
    fn default() -> Self {
        Self {
            template: "logo_sting".into(),
            text: None,
            subtitle: None,
            color: None,
            accent: None,
            background: None,
            logo: None,
            image: None,
            video: None,
            phone_image: None,
            phone_video: None,
            fit: None,
            crop: None,
            phone_crop: None,
            phone_aspect: None,
            product: None,
            font: None,
            bpm: None,
            duration: None,
            seed: 1,
            from: None,
            to: None,
            points: Vec::new(),
        }
    }
}

pub const FIELDS: &[&str] = &[
    "template", "text", "subtitle", "color", "accent", "background", "logo", "image", "video", "product", "font", "bpm", "duration", "seed", "from", "to", "points",
    "phone_image", "phone_video", "fit", "crop", "phone_crop", "phone_aspect",
];

/// The templates: `mod.motion.modules.seq3d` (and, in a scene, the Seq3D's
/// own `seq3d_scene`).
pub const MODULE: &str = include_str!("seq3d.splash");

/// The templates' names and what each is, in the module's order.
pub fn templates() -> Vec<(String, String)> {
    let mut vm = crate::script::DocVm::new(Default::default());
    vm.with(|vm| {
        let Ok(exports) = exports(vm) else { return Vec::new() };
        let all = vm.bx.heap_ref().value(exports, LiveId::from_str("seq3d_templates").into(), NoTrap).as_object();
        let mut out = Vec::new();
        if let Some(all) = all {
            let mut names = Vec::new();
            vm.bx.heap_ref().map_entries(all).iter().for_each(|e| names.push((e.key, e.value)));
            for (k, v) in names {
                let (Some(name), Some(t)) = (k.as_id(), v.as_object()) else { continue };
                let about = vm.bx.heap_ref().value(t, LiveId::from_str("about").into(), NoTrap);
                let about = vm.bx.heap_ref().string_with(about, |_, s| s.to_string()).unwrap_or_default();
                out.push((name.to_string(), about));
            }
        }
        out
    })
}

pub fn template_names() -> String {
    templates().iter().map(|t| format!("@{}", t.0)).collect::<Vec<_>>().join(" ")
}

/// The module evaluated in `vm`: its exports.
fn exports(vm: &mut ScriptVm) -> Result<ScriptObject, String> {
    let named = vm.bx.heap_mut().new_object();
    let scene = vm.bx.heap_ref().value(vm.modules(), id!(scene).into(), NoTrap).as_object().ok_or("no mod.scene")?;
    let module = makepad_script_imports::LoadedModule {
        reference: makepad_script_imports::ModuleRef::Named("seq3d".into()),
        source: makepad_script_imports::ModuleSource::new("seq3d", "1", MODULE),
    };
    let core = makepad_script_imports::language_core(vm);
    let errors = makepad_script_imports::load(vm, scene, &core, &[module], Some(named), 20_000_000);
    if !errors.is_empty() {
        return Err(errors.iter().map(|e| e.text()).collect::<Vec<_>>().join("; "));
    }
    vm.bx.heap_ref().value(named, LiveId::from_str("seq3d").into(), NoTrap).as_object().ok_or_else(|| "the seq3d module has no exports".into())
}

/// The Scene3D value of the Seq3D `s`, built in `vm` (a scene document's).
pub fn scene(vm: &mut ScriptVm, s: &Seq3dSpec) -> Result<ScriptValue, String> {
    let record = record(s)?;
    let exports = exports(vm)?;
    let build = vm.bx.heap_ref().value(exports, LiveId::from_str("seq3d_scene").into(), NoTrap);
    let arg = crate::script::input_to_script(vm, &record);
    vm.capture_errors();
    let value = vm.call(build, &[arg]);
    let errors = vm.take_errors();
    if !errors.is_empty() || value.is_err() {
        return Err(format!("the Seq3D template did not build: {}", errors.iter().map(|e| e.to_string()).collect::<Vec<_>>().join("; ")));
    }
    Ok(value)
}

/// The Seq3D as `seq3d_scene` reads it (what it leaves out is the
/// template's); an unknown template is an error naming them.
pub fn record(s: &Seq3dSpec) -> Result<InputValue, String> {
    let all = templates();
    if !all.iter().any(|t| t.0 == s.template) {
        return Err(format!("no Seq3D template `@{}`; templates: {}", s.template, all.iter().map(|t| format!("@{}", t.0)).collect::<Vec<_>>().join(" ")));
    }
    let text = |v: &str| InputValue::Text(v.to_string());
    let num = |v: f64| InputValue::Number(v);
    let color = |c: [f32; 3]| InputValue::Color([c[0], c[1], c[2], 1.0]);
    let media = |m: &Media| InputValue::Record(match m {
        Media::Texture(name) => vec![("kind".into(), text("texture")), ("name".into(), text(name))],
        Media::File(path) => vec![("kind".into(), text("file")), ("path".into(), text(path))],
        Media::Asset { id, rev } => vec![("kind".into(), text("asset")), ("id".into(), text(id)), ("rev".into(), text(rev))],
    });
    let crop = |k: &Option<CropKeys>| InputValue::List(k.iter().flat_map(|k| k.0.iter()).take(64).map(|(t, c, e)| InputValue::List(vec![num(*t), InputValue::List(c.iter().map(|v| num(*v)).collect()), text(e)])).collect());
    let mut r: Vec<(String, InputValue)> = vec![
        ("template".into(), text(&s.template)),
        ("transparent".into(), InputValue::Bool(s.background == Some(None))),
        ("fit".into(), text(s.fit.as_deref().unwrap_or("cover"))),
        ("crop".into(), crop(&s.crop)),
        ("phone_crop".into(), crop(&s.phone_crop)),
        ("phone_aspect".into(), num(s.phone_aspect.unwrap_or(0.0))),
        ("font".into(), text(s.font.as_deref().unwrap_or("bold"))),
        ("bpm".into(), num(s.bpm.filter(|b| *b > 0.0).unwrap_or(0.0))),
        ("duration".into(), num(s.duration.filter(|d| *d > 0.0).unwrap_or(0.0))),
        ("seed".into(), num(s.seed as f64)),
        ("from".into(), num(s.from.unwrap_or(5) as f64)),
        ("to".into(), num(s.to.unwrap_or(1) as f64)),
        ("points".into(), InputValue::List(s.points.iter().map(|p| InputValue::List(vec![num(p[0]), num(p[1])])).collect())),
    ];
    let mut opt = |name: &str, v: Option<InputValue>| {
        if let Some(v) = v {
            r.push((name.into(), v));
        }
    };
    opt("text", s.text.as_deref().map(text));
    opt("subtitle", s.subtitle.as_deref().map(text));
    opt("color", s.color.map(color));
    opt("accent", s.accent.map(color));
    opt("background", s.background.flatten().map(color));
    opt("image", s.image.as_ref().map(media));
    opt("video", s.video.as_ref().map(media));
    opt("phone_image", s.phone_image.as_ref().map(media));
    opt("phone_video", s.phone_video.as_ref().map(media));
    opt("product", s.product.as_ref().map(media));
    opt("logo", s.logo.as_ref().map(|l| InputValue::Record(match l {
        Logo::Path(d) => vec![("kind".into(), text("path")), ("text".into(), text(d))],
        Logo::Svg(svg) => vec![("kind".into(), text("svg")), ("text".into(), text(svg))],
        Logo::Src(m) => vec![("kind".into(), text("src")), ("media".into(), media(m))],
    })));
    Ok(InputValue::Record(r))
}

/// Read a `Seq3D{…}` object (motion3d's own documents; Motion reads its
/// layer fields itself).
pub(crate) fn read_spec(r: &mut crate::script::Reader, o: makepad_widgets::ScriptObject) -> Seq3dSpec {
    use crate::script::Semantic;
    r.check_fields(o, "", "Seq3D", FIELDS);
    let mut s = Seq3dSpec { template: r.id(o, "template").unwrap_or_else(|| "logo_sting".into()), ..Seq3dSpec::default() };
    s.text = r.string(o, "text");
    s.subtitle = r.string(o, "subtitle");
    s.font = r.id(o, "font");
    let rgb = |r: &crate::script::Reader, k: &str| r.plain::<4>(r.get(o, k), Semantic::Plain).map(|c| [c[0], c[1], c[2]]);
    s.color = rgb(r, "color");
    s.accent = rgb(r, "accent");
    if r.has(o, "background") {
        s.background = Some(match r.id(o, "background").as_deref() {
            Some("none") | Some("transparent") => None,
            _ => rgb(r, "background"),
        });
    }
    let num = |r: &crate::script::Reader, k: &str| r.number(r.get(o, k));
    s.bpm = num(r, "bpm");
    s.duration = num(r, "duration");
    s.seed = num(r, "seed").map(|v| v.max(1.0) as u64).unwrap_or(1);
    s.from = num(r, "from").map(|v| v.round() as i64);
    s.to = num(r, "to").map(|v| v.round() as i64);
    // Media: a plain string is a host texture; file()/asset() are files.
    let media = |r: &mut crate::script::Reader, k: &str| -> Option<Media> {
        let v = r.get(o, k);
        if v.is_nil() {
            return None;
        }
        if let Some(name) = r.string_of(v) {
            return Some(Media::Texture(name));
        }
        r.media(o, "", k).map(|m| match m {
            crate::script::MediaRef::File(p) => Media::File(p),
            crate::script::MediaRef::Asset { id, rev } => Media::Asset { id, rev },
        })
    };
    s.image = media(r, "image");
    s.video = media(r, "video");
    s.phone_image = media(r, "phone_image");
    s.phone_video = media(r, "phone_video");
    s.fit = r.id(o, "fit");
    s.phone_aspect = num(r, "phone_aspect");
    let crop = |r: &mut crate::script::Reader, k: &str| -> Option<CropKeys> {
        if !r.has(o, k) {
            return None;
        }
        let v = |c: [f32; 4]| [c[0] as f64, c[1] as f64, c[2] as f64, c[3] as f64];
        match r.anim::<4>(o, "", k, [0.0; 4], Semantic::Plain) {
            crate::anim::Anim::Const(c) => Some(CropKeys(vec![(0.0, v(c), "linear".into())])),
            crate::anim::Anim::Keys(keys) => Some(CropKeys(keys.iter().map(|key| (key.t as f64, v(key.v), if key.ease == crate::anim::Ease::Hold { "hold".into() } else { "linear".into() })).collect())),
            _ => None,
        }
    };
    s.crop = crop(r, "crop");
    s.phone_crop = crop(r, "phone_crop");
    s.product = media(r, "product").map(|m| match m {
        Media::Texture(p) => Media::File(p),
        m => m,
    });
    s.logo = match media(r, "logo") {
        Some(Media::Texture(t)) => Some(Logo::from_text(&t)),
        Some(m) => Some(Logo::Src(m)),
        None => None,
    };
    for v in r.list(r.get(o, "points")) {
        if let Some(p) = r.plain::<2>(v, Semantic::Plain) {
            s.points.push([p[0] as f64, p[1] as f64]);
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_template_names_the_list() {
        let e = record(&Seq3dSpec { template: "nope".into(), ..Seq3dSpec::default() }).unwrap_err();
        assert!(e.contains("@logo_sting") && e.contains("@countdown"), "{e}");
        assert_eq!(templates().len(), 8);
    }
}
