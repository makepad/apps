//! The Shader layer, read from its component.

use super::{At, Reader};
use crate::layers::Shader;
use crate::model::*;
use crate::obj::Look;
use crate::RESERVED_UNIFORMS;

impl Shader {
    pub(super) fn read(&self, c: &mut Reader, at: &At) -> LayerKind {
        let program = match (self.effect.look(c), self.pixel.look(c)) {
            (Some(Look::Str(name)), None) if !name.is_empty() => ShaderProgram::Preset(name),
            (Some(value @ Look::Obj(..)), None) => match c.media(at, "effect", &value, false) {
                Some(Media::Asset(asset)) => ShaderProgram::Effect(asset),
                _ => ShaderProgram::Preset(String::new()),
            },
            (None, Some(Look::Fn(pixel_fn))) => {
                // The document's (or a module's) function, compiled by the
                // drawing side (moved there by its tokens); its `helpers`
                // beside it.
                let mut helpers = Vec::new();
                match self.helpers.look(c) {
                    None => {}
                    Some(Look::Obj(h, _)) => {
                        for (name, value) in c.fields(h) {
                            match &value {
                                Look::Fn(i) => match c.fn_code(*i) {
                                    Some((_, code)) => helpers.push((name.clone(), code)),
                                    None => c.err(at, "helpers", format!("helper `{name}` could not be read"), None),
                                },
                                other => c.err(at, "helpers", format!("helper `{name}` is a shader function `fn(x: float) -> float {{ … }}`, not {}", other.kind()), None),
                            }
                        }
                    }
                    Some(other) => c.err(at, "helpers", format!("`helpers` is an object of shader functions, not {}", other.kind()), None),
                }
                let origin = if crate::value::loading_editable() { c.fn_site(pixel_fn) } else { None };
                match c.fn_code(pixel_fn) {
                    Some((loc, code)) => ShaderProgram::Inline { source: code, helpers: helpers.into(), loc: loc.unwrap_or_else(|| c.loc_of(at, "pixel")), origin },
                    None => {
                        c.err(at, "pixel", "the `pixel` function could not be read".into(), None);
                        ShaderProgram::Preset(String::new())
                    }
                }
            }
            (None, Some(other)) => {
                c.err(at, "pixel", format!("`pixel` is `fn() {{ … return vec4(…) }}` in Makepad's shader language, not {}", other.kind()), None);
                ShaderProgram::Preset(String::new())
            }
            (Some(_), Some(_)) => {
                c.err(at, "effect", "a Shader runs an `effect` or its own `pixel` function, not both".into(), None);
                ShaderProgram::Preset(String::new())
            }
            (Some(other), None) => {
                c.err(at, "effect", format!("`effect` names a render-kit effect (\"kit/grain\") or an effect pack's preset, or is `asset(id, rev)` of a VjEffect, not {}", other.kind()), None);
                ShaderProgram::Preset(String::new())
            }
            (None, None) => {
                c.err(at, "effect", "a Shader needs `effect: \"kit/…\"` (or an effect pack's preset) or `pixel: fn() { … }`".into(),
                    Some("inline: `pixel: fn() { let d = length(self.pos - vec2(0.5)); return vec4(1.0, 0.5, 0.2, 1.0) * smoothstep(0.5, 0.2, d) }`"));
                ShaderProgram::Preset(String::new())
            }
        };
        let dials = [("p1", &self.p1), ("p2", &self.p2), ("p3", &self.p3), ("p4", &self.p4)];
        let mut params = Vec::new();
        match &program {
            ShaderProgram::Inline { .. } => {
                if let Some(value) = self.params.look(c) {
                    match &value {
                        Look::Obj(pobj, _) => {
                            let pat = c.at(*pobj, at.field("params"), at.loc);
                            for (name, _) in c.fields(*pobj) {
                                if RESERVED_UNIFORMS.contains(&name.as_str()) {
                                    c.err(&pat, &name, format!("`{name}` is already a shader uniform ({}); name the param otherwise", RESERVED_UNIFORMS.join(", ")), None);
                                    continue;
                                }
                                let prop = c.prop::<1>(&pat, &name, [0.0]);
                                params.push((name, prop));
                            }
                        }
                        other => c.err(at, "params", format!("`params` is an object of name: value (each a uniform), not {}", other.kind()), None),
                    }
                }
                for (p, field) in dials {
                    if field.is_set() {
                        c.err(at, p, format!("`{p}` is a vj_fx dial; an inline shader takes `params: {{name: value}}`"), None);
                    }
                }
            }
            _ => {
                super::text::dials(c, at, &mut params, dials);
                if self.params.is_set() {
                    c.err(at, "params", "a vj_fx effect takes its dials as `p1`..`p4` (0..1)".into(), None);
                }
            }
        }
        let below = match self.input.look(c) {
            None => matches!(program, ShaderProgram::Preset(_) | ShaderProgram::Effect(_)),
            Some(Look::Id(id)) if id == "below" => true,
            Some(Look::Id(id)) if id == "none" => false,
            Some(other) => {
                c.err(at, "input", format!("`input` is @below (the frame under the layer) or @none, not {}", other.kind()), None);
                false
            }
        };
        // `data: [f, …]`: floats the pixel function reads from a texture.
        let data = match self.data.look(c) {
            None => None,
            Some(Look::Arr(items)) if matches!(program, ShaderProgram::Inline { .. }) => {
                let mut out = Vec::with_capacity(items.len());
                for (i, item) in items.iter().enumerate() {
                    match item.num() {
                        Some(x) => out.push(x as f32),
                        None => {
                            c.err(at, "data", format!("`data` is a list of numbers; item {i} is {}", item.kind()), None);
                            out.clear();
                            break;
                        }
                    }
                }
                (!out.is_empty()).then(|| ShaderData(out.into()))
            }
            Some(Look::Arr(_)) => {
                c.err(at, "data", "`data` is read by an inline `pixel` function; a vj_fx effect does not take it".into(), None);
                None
            }
            Some(other) => {
                c.err(at, "data", format!("`data` is a list of numbers `[0.5, 1.0, …]`, not {}", other.kind()), None);
                None
            }
        };
        let speed = self.speed.konst(c, at, "speed", 1.0);
        LayerKind::Shader { program, size: self.size.size(c, at, "size"), params, speed, below, data, text: None, font: None }
    }
}
