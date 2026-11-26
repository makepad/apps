//! A layer's `effects: [...]` and the document's `camera` checked against
//! the render kit's catalog (R2's `makepad-stage-render-kit`, without its
//! GPU side): the load reports what the kit would refuse, and the drawing
//! side builds its effects with [`kit_effect`].

use crate::frame::{EffectFrame, EffectValue};
use makepad_stage_render_kit::catalog::{by_name as kit_by_name, ParamKind as KitParamKind};
use makepad_stage_render_kit::{Anim as KitAnim, CameraSpec, Effect as KitEffect, Value as KitValue};

/// A render-kit effect from its values at a frame.
pub fn kit_effect(effect: &EffectFrame) -> Result<KitEffect, String> {
    let info = kit_by_name(&effect.name).ok_or_else(|| {
        let names: Vec<&str> = makepad_stage_render_kit::EFFECTS.iter().map(|e| e.name).collect();
        format!("`{}` is not a render-kit effect (effects: {})", effect.name, names.join(", "))
    })?;
    let params = effect.params.iter().map(|(name, value)| {
        let layer = info.param(name).is_some_and(|(_, p)| matches!(p.kind, KitParamKind::Layer));
        let value = match value {
            EffectValue::Num(n) => KitValue::Num(*n),
            EffectValue::Vec2(v) => KitValue::Vec2(*v),
            EffectValue::Vec3(v) => KitValue::Vec3(*v),
            EffectValue::Color(c) => KitValue::Color(*c),
            EffectValue::Bool(b) => KitValue::Bool(*b),
            EffectValue::Choice(c) if layer => KitValue::Layer(c.clone()),
            EffectValue::Choice(c) => KitValue::Choice(c.clone()),
            EffectValue::Text(s) => KitValue::Text(s.clone()),
            EffectValue::Points(p) => KitValue::Points(p.clone()),
        };
        (name.as_str(), KitAnim::Const(value))
    });
    KitEffect::build(&effect.name, params).map(|(effect, _)| effect).map_err(|d| d.errors.join("; "))
}

/// Every problem the render kit finds in a document's effects and camera
/// (unknown names, parameters, values), with where they are written: what
/// a host with the drawing side checks at load.
pub fn check_effects(doc: &crate::model::MotionDoc) -> Vec<crate::model::Diagnostic> {
    let mut out = Vec::new();
    let frame = doc.eval(0.0);
    for (_, item) in frame.walk() {
        let Some(layer) = doc.all_layers().into_iter().find(|l| l.path == item.path) else { continue };
        for (effect, spec) in item.effects.iter().zip(&layer.effects) {
            if let Err(error) = kit_effect(effect) {
                out.push(crate::model::Diagnostic { severity: crate::model::Severity::Error, file: None, loc: spec.loc, path: format!("{}.effects", layer.path), message: error, hint: None });
            }
        }
    }
    if let Some(camera) = &frame.camera {
        if let Err(errors) = camera_spec(camera) {
            out.push(crate::model::Diagnostic { severity: crate::model::Severity::Error, file: None, loc: crate::model::Loc { line: 1, col: 1 }, path: "camera".into(), message: errors, hint: None });
        }
    }
    out
}

pub fn camera_spec(camera: &[(String, EffectValue)]) -> Result<CameraSpec, String> {
    let params = camera.iter().map(|(name, value)| {
        let value = match value {
            EffectValue::Num(n) => KitValue::Num(*n),
            EffectValue::Vec2(v) => KitValue::Vec2(*v),
            EffectValue::Vec3(v) => KitValue::Vec3(*v),
            EffectValue::Color(c) => KitValue::Color(*c),
            EffectValue::Bool(b) => KitValue::Bool(*b),
            EffectValue::Choice(c) => KitValue::Choice(c.clone()),
            EffectValue::Text(s) => KitValue::Text(s.clone()),
            EffectValue::Points(p) => KitValue::Points(p.clone()),
        };
        (name.as_str(), KitAnim::Const(value))
    });
    CameraSpec::build(params).map(|(spec, _)| spec).map_err(|errors| errors.join("; "))
}
