//! The document root, read from its `Motion` component: the frame, timing,
//! inputs, post, and the layers.

use super::{bind_text_path, At, Reader};
use crate::layers::Motion;
use crate::model::*;
use crate::obj::Look;
use crate::value::fmt_num;
use crate::Diagnostic;
use makepad_script::LiveId;

impl Motion {
    pub(super) fn read(&self, c: &mut Reader, at: &At, doc: &mut MotionDoc, params: &[(LiveId, InputValue)]) {
        let root_loc = at.loc;
        // Inputs first: the header may use them (`size: in(@frame)`).
        let inputs = self.inputs.look(c);
        c.declare_inputs(at, inputs.as_ref(), params);
        match self.label.look(c) {
            None => {}
            Some(Look::Str(label)) => doc.label = label,
            Some(other) => c.err(at, "label", format!("`label` is a string, not {}", other.kind()), None),
        }
        match self.camera.look(c) {
            None => {}
            Some(Look::Obj(cam, _)) => {
                let cat = c.at(cam, "camera".into(), root_loc);
                let mut args = Vec::new();
                for (name, value) in c.fields(cam).into_iter().filter(|(k, _)| !k.starts_with("__")) {
                    if let Some(arg) = c.effect_arg(&cat, &name, &value) {
                        args.push((name, arg));
                    }
                }
                doc.camera = Some(args);
            }
            Some(other) => c.err(at, "camera", format!("`camera` is `Camera{{orbit: … fov: … focus: …}}`, not {}", other.kind()), None),
        }
        match self.brief.look(c) {
            None => {}
            Some(Look::Str(brief)) => doc.brief = brief,
            Some(other) => c.err(at, "brief", format!("`brief` is a string, not {}", other.kind()), None),
        }
        match self.tags.look(c) {
            None => {}
            Some(Look::Arr(tags)) if tags.iter().all(|t| matches!(t, Look::Str(_) | Look::Id(_))) => {
                doc.tags = tags.into_iter().filter_map(|t| match t {
                    Look::Str(s) | Look::Id(s) => Some(s),
                    _ => None,
                }).collect();
            }
            Some(other) => c.err(at, "tags", format!("`tags` is an array of strings, not {}", other.kind()), None),
        }
        // Left out: 1920x1080, the loader's own frame (`doc.size` and
        // `c.width`/`c.height` start there).
        match self.size.konst_opt(c, at, "size") {
            Some(size) if size[0] >= 1.0 && size[1] >= 1.0 && size[0] <= 16384.0 && size[1] <= 16384.0 => {
                doc.size = size;
                c.width = size[0];
                c.height = size[1];
            }
            Some(size) => c.err(at, "size", format!("`size` {}x{} is outside 1..16384 units a side", fmt_num(size[0]), fmt_num(size[1])), None),
            None => {}
        }
        match self.duration.look(c) {
            Some(value) => match value.num() {
                Some(duration) if duration > 0.0 => doc.duration = duration,
                _ => c.err(at, "duration", format!("`duration` is a positive number of seconds, not {}", value.kind()), Some("`duration: 6.0`")),
            },
            None => c.err(at, "duration", "a Motion document declares its `duration` in seconds".into(), Some("`duration: 6.0`")),
        }
        c.duration = doc.duration;
        if let Some(value) = self.fps.look(c) {
            match value.num() {
                Some(fps) if fps > 0.0 && fps <= 1000.0 => doc.fps = fps,
                _ => c.err(at, "fps", format!("`fps` is a positive frame rate, not {}", value.kind()), None),
            }
        }
        let bpm = self.bpm.look(c);
        c.doc_bpm = bpm.as_ref().and_then(|v| v.num());
        if let Some(value) = bpm {
            match value.num() {
                Some(bpm) if bpm > 0.0 => {
                    doc.bpm = bpm;
                    c.bpm = bpm;
                }
                _ => c.err(at, "bpm", format!("`bpm` is a positive tempo, not {}", value.kind()), None),
            }
        }
        doc.background = self.background.konst(c, at, "background");
        match self.color.look(c) {
            None => {}
            Some(Look::Id(id)) if id == "hdr" => doc.hdr = true,
            Some(Look::Id(id)) if id == "display" => doc.hdr = false,
            Some(other) => c.err(at, "color", format!("`color` is @hdr (linear light, tone mapped once) or @display (8-bit, the default), not {}", other.kind()), None),
        }
        doc.post = c.post_passes(at, self.post.0);
        if let Some(value) = self.motion_blur.look(c) {
            doc.motion_blur = c.motion_blur(at, &value);
        }
        match self.layers.look(c) {
            Some(Look::Arr(items)) => {
                doc.layers = c.layers(at, "layers", &items);
                for (text, name, series, loc) in std::mem::take(&mut c.pending_paths) {
                    if let Err(message) = bind_text_path(&mut doc.layers, &text, &name, series.as_deref()) {
                        c.diags.push(Diagnostic::error(loc, format!("{text}.path"), message).hint("`path: layer(\"curve\")` rides a Path named \"curve\"; `layer(\"chart\", \"loss\")` rides the chart's series \"loss\""));
                    }
                }
                if items.is_empty() {
                    c.warn(at, "layers", "the document has no layers: every frame is empty".into(), None);
                }
            }
            Some(other) => c.err(at, "layers", format!("`layers` is an array of layers, not {}", other.kind()), Some("`layers: [Rect{…} Text{…}]`")),
            None => c.err(at, "layers", "a Motion document has `layers: [...]`".into(), Some("`layers: [Rect{size: vec2(1920, 1080) fill: #0b0d12}]`")),
        }
        doc.inputs = std::mem::take(&mut c.inputs);
        doc.input_values = std::mem::take(&mut c.values);
        doc.has_functions = c.has_functions;
    }
}
