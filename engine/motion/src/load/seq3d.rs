//! Reading a `Seq3D{…}` layer (lane R3): a ready-made 3D sequence named by
//! `template`, built by its Splash template (motion3d's seq3d.splash, the
//! built-in module `mod.motion.modules.seq3d`) as an ordinary Scene3D that
//! draws like a Scene3D layer: the layer's scene is seq3d_scene.splash with
//! the Seq3D read into its `seq3d` input. A template can be copied out of
//! the module and edited as a Scene3D.
//!
//! Pictures and clips arrive as assets: `image` becomes the scene texture
//! `"seq_image"` and `video` the video texture `"seq_video"`; `product` (a
//! GLB) and `logo` (an SVG asset, markup or a path `d`) go into the scene
//! as they are.

use crate::plain::GroupProps;
use super::{At, Reader};
use crate::layers::Seq3D;
use crate::model::*;
use crate::obj::Look;
use makepad_stage_motion3d::script::InputValue as M3Input;
use makepad_stage_motion3d::seq3d::{self, CropKeys, Logo, Media as SeqMedia, Seq3dSpec};

impl Seq3D {
    pub(super) fn read(&self, c: &mut Reader, at: &At) -> LayerKind {
        let mut spec = Seq3dSpec::default();
        spec.template = self.template.ident(c).unwrap_or_else(|| "logo_sting".into());
        spec.font = self.font.face(c, at, "font").map(|f| f.family.to_string());
        spec.text = self.text.text(c, at, "text");
        spec.subtitle = self.subtitle.text(c, at, "subtitle");
        let doc_bpm = c.doc_bpm;
        spec.bpm = self.bpm.konst_opt(c, at, "bpm").or(doc_bpm);
        spec.duration = self.duration.konst_opt(c, at, "duration");
        spec.seed = self.seed.konst_opt(c, at, "seed").map(|s| s.max(1.0) as u64).unwrap_or(1);
        spec.from = self.from.konst_opt(c, at, "from").map(|v| v.round() as i64);
        spec.to = self.to.konst_opt(c, at, "to").map(|v| v.round() as i64);
        let rgb = |c: Color| [c[0] as f32, c[1] as f32, c[2] as f32];
        spec.color = self.color.konst(c, at, "color").map(rgb);
        spec.accent = self.accent.konst(c, at, "accent").map(rgb);
        spec.background = match self.background.look(c) {
            None => None,
            Some(Look::Id(s)) if s == "none" || s == "transparent" => Some(None),
            Some(value) => Some(c.color_const(at, "background", &value).map(rgb)),
        };
        if let Some(Look::Arr(items)) = self.points.look(c) {
            for p in &items {
                if let Some([lat, lon]) = super::vec_n::<2>(p) {
                    spec.points.push([lat, lon]);
                }
            }
        }
        let mut textures = Vec::new();
        let mut videos = Vec::new();
        let seq_asset = |a: &AssetRef| SeqMedia::Asset { id: a.id.clone(), rev: a.rev.clone() };
        if self.image.is_set() {
            if let Some(m) = self.image.media(c, at, "image", false) {
                textures.push(("seq_image".to_string(), m));
                spec.image = Some(SeqMedia::Texture("seq_image".into()));
            }
        }
        if self.video.is_set() {
            if let Some(m) = self.video.media(c, at, "video", false) {
                videos.push(("seq_video".to_string(), m));
                spec.video = Some(SeqMedia::Texture("seq_video".into()));
            }
        }
        for (field, media, texture) in [("phone_image", &self.phone_image, "seq_phone_image"), ("phone_video", &self.phone_video, "seq_phone_video")] {
            if media.is_set() {
                if let Some(m) = media.media(c, at, field, false) {
                    if field == "phone_image" {
                        textures.push((texture.to_string(), m));
                        spec.phone_image = Some(SeqMedia::Texture(texture.into()));
                    } else {
                        videos.push((texture.to_string(), m));
                        spec.phone_video = Some(SeqMedia::Texture(texture.into()));
                    }
                }
            }
        }
        spec.fit = self.fit.ident(c);
        spec.phone_aspect = self.phone_aspect.konst_opt(c, at, "phone_aspect");
        for (field, crop) in [("crop", &self.crop), ("phone_crop", &self.phone_crop)] {
            let prop = match crop.look(c) {
                None => None,
                Some(value) => c.prop_value::<4>(at, field, &value),
            };
            let keys = match prop {
                None => None,
                Some(Prop::Const(v)) => Some(CropKeys(vec![(0.0, v, "linear".into())])),
                Some(Prop::Track(track)) => Some(CropKeys(track.keys.iter().map(|k| (k.t.value, k.v, if k.ease == crate::Ease::Hold { "hold".to_string() } else { "linear".to_string() })).collect())),
                Some(_) => {
                    c.err(at, field, format!("`{field}` is [x, y, w, h] in the clip's pixels or `keys(...)` of them (a reframe's crop)"), None);
                    None
                }
            };
            if field == "crop" { spec.crop = keys } else { spec.phone_crop = keys }
        }
        if self.product.is_set() {
            if let Some(Media::Asset(a)) = self.product.media(c, at, "product", false) {
                spec.product = Some(seq_asset(&a));
            }
        }
        spec.logo = match self.logo.look(c) {
            None => None,
            Some(Look::Str(s)) => Some(Logo::from_text(&s)),
            Some(_) => match self.logo.media(c, at, "logo", true) {
                Some(Media::Asset(a)) => Some(Logo::Src(seq_asset(&a))),
                Some(Media::Inline(text)) => Some(Logo::from_text(&text)),
                _ => None,
            },
        };
        let size = self.size.size(c, at, "size");
        // The assets the scene reads (the host fetches them first).
        let mut assets = Vec::new();
        for m in [spec.product.as_ref(), match &spec.logo { Some(Logo::Src(m)) => Some(m), _ => None }].into_iter().flatten() {
            if let SeqMedia::Asset { id, rev } = m {
                let a = AssetRef { id: id.clone(), rev: rev.clone() };
                if !assets.contains(&a) {
                    assets.push(a);
                }
            }
        }
        match seq3d::record(&spec) {
            Ok(record) => LayerKind::Scene3dDoc { assets, scene: SceneSource::Template { source: SCENE.into(), inputs: vec![("seq3d".into(), input(&record))] }, loc: at.loc, written: false, size, textures, videos, layers: Vec::new() },
            Err(message) => {
                c.err(at, "template", message, Some(&format!("`Seq3D{{template: @logo_sting text: \"BRAND\"}}`; templates: {}", seq3d::template_names())));
                LayerKind::Group { layers: Vec::new(), mask: None, props: GroupProps { mask_mode: MaskMode::Alpha, stagger: crate::model::Stagger::none(), size: None }, post: Default::default(), glyphs: false }
            }
        }
    }
}

/// The scene a Seq3D layer draws: its template run for the `seq3d` input.
const SCENE: &str = include_str!("../seq3d_scene.splash");

/// A scene input as a document input.
fn input(v: &M3Input) -> InputValue {
    match v {
        M3Input::Number(n) => InputValue::Number(*n),
        M3Input::Bool(b) => InputValue::Bool(*b),
        M3Input::Text(t) => InputValue::Text(t.clone()),
        M3Input::Color(c) => InputValue::Color([c[0] as f64, c[1] as f64, c[2] as f64, c[3] as f64]),
        M3Input::Vec3(v) => InputValue::List(v.iter().map(|x| InputValue::Number(*x as f64)).collect()),
        M3Input::List(items) => InputValue::List(items.iter().map(input).collect()),
        M3Input::Record(fields) => InputValue::Record(fields.iter().map(|(k, v)| (k.clone(), input(v))).collect()),
    }
}
