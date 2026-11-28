//! Reading a `Text3D{…}` layer (lane T3): its fields into a
//! [`crate::text3d::Text3dSpec`], lowered to the `text3d` input of a
//! motion3d scene (the preset run in Splash) that draws like a Scene3D layer.

use crate::plain::GroupProps;
use super::{At, Reader};
use crate::layers::Text3D;
use crate::model::*;
use crate::text3d::{self, Text3dSpec};
use crate::obj::Look;

impl Text3D {
    pub(super) fn read(&self, c: &mut Reader, at: &At) -> LayerKind {
        let mut spec = Text3dSpec { seed: 1, ..Text3dSpec::default() };
        spec.text = self.text.text(c, at, "text").unwrap_or_default();
        spec.preset = self.preset.ident(c).unwrap_or_else(|| "extrude_in".into());
        let face = self.font.face(c, at, "font");
        spec.material = self.material.ident(c);
        spec.bevel_type = self.bevel_type.ident(c);
        spec.align = self.align.ident(c);
        spec.by = self.by.ident(c);
        spec.order = self.order.ident(c);
        spec.environment = self.environment.ident(c);
        spec.music = self.music.ident(c);
        spec.italic = face.as_ref().is_some_and(|f| f.italic);
        spec.motion_blur = self.motion_blur.is_true(c);
        spec.floor = match self.floor.look(c) {
            Some(Look::Bool(b)) => Some(b),
            _ => None,
        };
        // A Font without its own `weight` leaves the preset's.
        let weight_written = self.font.0.as_object().and_then(|f| crate::obj::opt(c.vm, f, "weight")).is_some_and(|w| !w.is_nil());
        spec.weight = face.as_ref().filter(|_| weight_written).map(|f| f.weight);
        spec.font = face.map(|f| f.family.to_string());
        spec.depth = self.depth.konst_opt(c, at, "depth");
        spec.bevel = self.bevel.konst_opt(c, at, "bevel");
        spec.tracking = self.tracking.konst_opt(c, at, "tracking");
        spec.line_height = self.line_height.konst_opt(c, at, "line_height");
        spec.stagger = self.stagger.konst_opt(c, at, "stagger");
        spec.duration = self.duration.konst_opt(c, at, "duration");
        // The 3D exit (the entrance reversed) starts at `exit_at`; the
        // common `exit: @defocus|@collapse|@fill` still wraps the layer.
        spec.exit = if self.common.exit.is_set() {
            None
        } else {
            self.common.exit_at.look(c).and_then(|v| c.num_const(at, "exit_at", &v))
        };
        spec.pulse = self.pulse.konst_opt(c, at, "pulse").unwrap_or(0.0);
        spec.seed = self.seed.konst_opt(c, at, "seed").map(|s| s.max(1.0) as u64).unwrap_or(1);
        // Vocal-locked karaoke: `words: voice(@vo) line: i` takes the sung
        // line's word times (M1's resolver, the same as 2D Text's), made
        // relative to the layer's start; the text defaults to the line.
        if let Some(words) = self.words.look(c).filter(|w| matches!(w, Look::Obj(..))) {
            let id = c.voice_id(&words);
            let line = self.line.look(c).and_then(|v| v.num()).map(|l| l.max(0.0) as u32);
            match (id, &c.context) {
                (Some(id), Some(context)) => match crate::lyrics::voice_words(context, &id, line) {
                    Ok(words) => {
                        let span = self.common.span.look(c);
                        let start = match &span {
                            Some(Look::Arr(parts)) => parts.first().and_then(|v| v.num()),
                            _ => None,
                        }
                        // A span on a musical or spoken time starts with its line.
                        .or_else(|| span.as_ref().map(|_| words.first().map_or(0.0, |w| w.1)))
                        .unwrap_or(0.0);
                        spec.word_times = words.iter().map(|w| [w.1 - start, w.2 - start]).collect();
                        if spec.text.is_empty() {
                            spec.text = words.iter().map(|w| w.0.as_str()).collect::<Vec<_>>().join(" ");
                        }
                    }
                    Err(message) => c.warn(at, "words", format!("{message}; the words fill evenly over the layer"), None),
                },
                (None, _) => c.err(at, "words", "`words` is `voice(@vo)` (with `line: i`)".into(), Some("`Text3D{preset: @karaoke_slab words: voice(@vo) line: 2}`")),
                (_, None) => {}
            }
        }
        if self.ghost.is_set() {
            spec.ghost = self.ghost.konst(c, at, "ghost");
        }
        if self.highlight_color.is_set() {
            spec.highlight = self.highlight_color.konst(c, at, "highlight_color");
        }
        // Karaoke word times: `word_times: [[start, end], …]` (seconds on
        // the layer clock; vec2s work too).
        if let Some(Look::Arr(items)) = self.word_times.look(c) {
            for item in &items {
                match super::vec_n::<2>(item) {
                    Some(t) => spec.word_times.push(t),
                    None => c.err(at, "word_times", format!("`word_times` is a list of [start, end] pairs, not {}", item.kind()), Some("`word_times: [[0.4, 0.9], [0.9, 1.3]]`")),
                }
            }
        }
        spec.color = self.color.konst(c, at, "color");
        spec.accent = self.accent.konst(c, at, "accent");
        spec.background = match self.background.look(c) {
            None => None,
            Some(Look::Id(s)) if s == "none" || s == "transparent" => Some(None),
            Some(value) => Some(c.color_const(at, "background", &value)),
        };
        // The layer's box: its `size`, else the document's.
        let doc_size = [c.width, c.height];
        let size = self.size.size(c, at, "size");
        spec.size = match &size {
            Some(Prop::Const(s)) => *s,
            _ => doc_size,
        };
        // `fit`: `@safe`, a width in document units, or (≤ 1) a share of the width.
        spec.fit = self.fit.width(c, at, "fit").map(|w| if w <= 1.0 { w } else { w / spec.size[0].max(1.0) }).unwrap_or(0.0);
        match text3d::lower(&spec) {
            Ok(inputs) => LayerKind::Scene3dDoc { scene: SceneSource::Template { source: text3d::SCENE.into(), inputs }, loc: at.loc, written: false, size, assets: Vec::new(), textures: Vec::new(), videos: Vec::new(), layers: Vec::new() },
            Err(message) => {
                let field = if message.contains("preset") { "preset" } else if message.contains("material") { "material" } else if message.contains("font") { "font" } else { "text" };
                let presets = text3d::presets().iter().map(|p| format!("@{}", p.name)).collect::<Vec<_>>().join(" ");
                let hint = (!presets.is_empty()).then(|| format!("`Text3D{{preset: @chrome_title text: \"TITLE\"}}`; presets: {presets}"));
                c.err(at, field, message, hint.as_deref());
                LayerKind::Group { layers: Vec::new(), mask: None, props: GroupProps { mask_mode: MaskMode::Alpha, stagger: crate::model::Stagger::none(), size: None }, post: Default::default(), glyphs: false }
            }
        }
    }
}
