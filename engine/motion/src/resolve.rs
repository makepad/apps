//! Musical and spoken time in a document resolved into seconds (EDITS.md
//! §10.3). Every [`Time`] and [`Dur`] keeps what was written; resolution
//! writes their `value` on the clock of the layer they belong to, so
//! evaluation reads plain seconds. Resolving again with another context
//! (the music re-analysed, a take regenerated) re-syncs the document.

use crate::plain::{EllipseProps, GroupProps, PathProps, RectProps, TextProps};
use crate::model::*;
use crate::timing::{Length, TimeExpr};

impl MotionDoc {
    /// The document loaded again against `context` (its source and input
    /// values kept): musical and spoken time re-synced, audio features
    /// replaced, and what it computes from them computed again. Gives the
    /// warnings of references the context cannot place (they resolve to the
    /// clock's start, plus their offset).
    pub fn with_context(&self, context: MotionContext) -> Result<(MotionDoc, Vec<Diagnostic>), Vec<Diagnostic>> {
        let doc = MotionDoc::load_source(self.source.clone(), &self.slots(), context)?;
        let warnings = doc.warnings.iter().filter(|w| w.message.starts_with(RESOLVE_TAG)).cloned().collect();
        Ok((doc, warnings))
    }

    /// The same document with a VM of its own, for another thread to
    /// evaluate alongside this one (a document's VM runs on one thread at a
    /// time).
    pub fn fork(&self) -> Result<MotionDoc, Vec<Diagnostic>> {
        MotionDoc::load_source(self.source.clone(), &self.slots(), (*self.context).clone())
    }

}

const RESOLVE_TAG: &str = "time: ";

struct Resolver<'a> {
    context: &'a MotionContext,
    bpm: f64,
    warnings: Vec<Diagnostic>,
    used_grid: Option<Loc>,
}

impl Resolver<'_> {
    /// Seconds of `expr` on a clock that starts at `origin` (absolute).
    fn point(&mut self, expr: &TimeExpr, origin: f64, loc: Loc, path: &str) -> f64 {
        match expr {
            TimeExpr::Seconds(s) => *s,
            TimeExpr::At { at, offset } => {
                if at.is_grid() && self.context.timing.grid.is_none() && self.used_grid.is_none() {
                    self.used_grid = Some(loc);
                }
                // `word(.., after)` counts from a second of this clock: the
                // host's is where the lookup runs.
                let shifted;
                let at = match at {
                    crate::timing::TimeRef::WordText { voice, text, nth, after: Some(after) } => {
                        shifted = crate::timing::TimeRef::WordText { voice: voice.clone(), text: text.clone(), nth: *nth, after: Some(after + self.context.offset + origin) };
                        &shifted
                    }
                    at => at,
                };
                match self.context.timing.resolve_ref(at, self.bpm) {
                    Ok(host) => host + offset - self.context.offset - origin,
                    Err(message) => {
                        self.warnings.push(Diagnostic::warning(loc, path.to_string(), format!("{RESOLVE_TAG}{message}"))
                            .hint(format!("until it resolves, {} is taken as the start of its clock", at.describe())));
                        *offset
                    }
                }
            }
        }
    }

    fn length(&mut self, length: Length, at_abs: f64) -> f64 {
        self.context.timing.length(length, at_abs + self.context.offset, self.bpm)
    }

    fn time(&mut self, time: &mut Time, origin: f64, loc: Loc, path: &str) {
        time.value = self.point(&time.expr, origin, loc, path);
    }

    fn dur(&mut self, dur: &mut Dur, at_abs: f64) {
        dur.value = match dur.expr {
            // A loop without its own period: filled in from its keys.
            Length::Seconds(s) if s < 0.0 => -1.0,
            length => self.length(length, at_abs),
        };
    }

    fn prop<const N: usize>(&mut self, prop: &mut Prop<N>, origin: f64, loc: Loc, path: &str) {
        if let Prop::Blend(blend) = prop {
            self.blend_axes(blend, origin, loc, path);
            for value in &mut blend.values { self.prop(value, origin, loc, path); }
            return;
        }
        let Prop::Track(track) = prop else { return };
        for key in &mut track.keys {
            self.time(&mut key.t, origin, loc, path);
        }
        if let Some(repeat) = &mut track.repeat {
            let first = track.keys.first().map_or(0.0, |key| key.t.value);
            self.dur(&mut repeat.period, origin + first);
            if repeat.period.value <= 0.0 {
                let last = track.keys.last().map_or(0.0, |key| key.t.value);
                repeat.period.value = (last - first).max(1e-6);
            }
        }
    }

    fn blend_axes<T>(&mut self, blend: &mut BlendForm<T>, origin: f64, loc: Loc, path: &str) {
        self.prop(&mut blend.x, origin, loc, path);
        if let Some(y) = &mut blend.y { self.prop(y, origin, loc, path); }
    }

    fn arg(&mut self, arg: &mut EffectArg, origin: f64, loc: Loc, path: &str) {
        match arg {
            EffectArg::Num(p) => self.prop(p, origin, loc, path),
            EffectArg::Vec2(p) => self.prop(p, origin, loc, path),
            EffectArg::Vec3(p) => self.prop(p, origin, loc, path),
            EffectArg::Color(p) => self.prop(p, origin, loc, path),
            _ => {}
        }
    }

    fn opt4(&mut self, prop: &mut Option<Prop<4>>, origin: f64, loc: Loc, path: &str) {
        if let Some(prop) = prop {
            self.prop(prop, origin, loc, path);
        }
    }

    fn post(&mut self, passes: &mut [PostPassSpec], origin: f64, loc: Loc, path: &str) {
        for pass in passes {
            for value in &mut pass.uniforms {
                match value {
                    PostUniform::One(p) => self.prop(p, origin, loc, path),
                    PostUniform::Two(p) => self.prop(p, origin, loc, path),
                    PostUniform::Three(p) => self.prop(p, origin, loc, path),
                    PostUniform::Four(p) => self.prop(p, origin, loc, path),
                }
            }
        }
    }

    fn layer(&mut self, layer: &mut Layer, parent_origin: f64) {
        let loc = layer.loc;
        let path = layer.path.clone();
        let origin = match &mut layer.span {
            Some(span) => {
                self.time(&mut span.start, parent_origin, loc, &path);
                let start_abs = parent_origin + span.start.value;
                self.dur(&mut span.length, start_abs);
                start_abs
            }
            None => parent_origin,
        };
        let t = &mut layer.transform;
        self.prop(&mut t.at, origin, loc, &path);
        self.prop(&mut t.x, origin, loc, &path);
        self.prop(&mut t.y, origin, loc, &path);
        self.prop(&mut t.scale, origin, loc, &path);
        self.prop(&mut t.rotate, origin, loc, &path);
        if let Some(along) = &mut t.along {
            self.prop(&mut along.progress, origin, loc, &path);
        }
        self.prop(&mut layer.opacity, origin, loc, &path);
        self.prop(&mut layer.blur, origin, loc, &path);
        self.prop(&mut layer.feather, origin, loc, &path);
        for effect in &mut layer.effects {
            for (_, arg) in &mut effect.params {
                self.arg(arg, origin, loc, &path);
            }
        }
        match &mut layer.kind {
            LayerKind::Rect(RectProps { size, radius, fill, stroke, width, trim_start, trim_end, .. }) => {
                self.prop(size, origin, loc, &path);
                self.prop(radius, origin, loc, &path);
                self.opt4(fill, origin, loc, &path);
                self.opt4(stroke, origin, loc, &path);
                self.prop(width, origin, loc, &path);
                self.prop(trim_start, origin, loc, &path);
                self.prop(trim_end, origin, loc, &path);
            }
            LayerKind::Ellipse(EllipseProps { size, fill, stroke, width, trim_start, trim_end, .. }) => {
                self.prop(size, origin, loc, &path);
                self.opt4(fill, origin, loc, &path);
                self.opt4(stroke, origin, loc, &path);
                self.prop(width, origin, loc, &path);
                self.prop(trim_start, origin, loc, &path);
                self.prop(trim_end, origin, loc, &path);
            }
            LayerKind::Path(PathProps { data, fill, stroke, width, trim_start, trim_end, .. }) => {
                if let PathShape::Morph { track, .. } = data {
                    let mut prop = Prop::Track(track.clone());
                    self.prop(&mut prop, origin, loc, &path);
                    if let Prop::Track(resolved) = prop {
                        *track = resolved;
                    }
                }
                self.opt4(fill, origin, loc, &path);
                self.opt4(stroke, origin, loc, &path);
                self.prop(width, origin, loc, &path);
                self.prop(trim_start, origin, loc, &path);
                self.prop(trim_end, origin, loc, &path);
            }
            LayerKind::Svg { size, .. } => {
                if let Some(size) = size {
                    self.prop(size, origin, loc, &path);
                }
            }
            LayerKind::Image { size, zoom, focus, .. } => {
                if let Some(size) = size {
                    self.prop(size, origin, loc, &path);
                }
                self.prop(zoom, origin, loc, &path);
                if let Some(focus) = focus {
                    self.prop(focus, origin, loc, &path);
                }
            }
            LayerKind::Video { size, source_in, .. } => {
                if let Some(size) = size {
                    self.prop(size, origin, loc, &path);
                }
                // The source's own time: a musical `in` means the source
                // time of that moment of the music (both on this clock).
                self.time(source_in, origin, loc, &path);
            }
            LayerKind::Freeze { time: FreezeTime::Moment(time), .. } => self.time(time, origin, loc, &path),
            LayerKind::Freeze { time: FreezeTime::Moving(time), .. } => self.prop(time, origin, loc, &path),
            LayerKind::Anchored { layer, .. } => self.layer(layer, origin),
            LayerKind::FlyThrough { layers, time, .. } => {
                self.time(time, origin, loc, &path);
                for child in layers {
                    self.layer(child, origin);
                }
            }
            LayerKind::Bones(rig) => {
                for bone in &mut rig.bones { self.prop(&mut bone.rotate, origin, loc, &path); }
                for follow in &mut rig.follow { self.prop(&mut follow.along, origin, loc, &path); }
                for ik in &mut rig.ik {
                    self.prop(&mut ik.target, origin, ik.loc, &ik.path);
                    self.prop(&mut ik.mix, origin, ik.loc, &ik.path);
                }
                for child in &mut rig.layers { self.layer(child, origin); }
            }
            LayerKind::Character(ch) => {
                for (_, target) in &mut ch.reach { self.prop(target, origin, loc, &path); }
                if let Some(blend) = &mut ch.pose_blend { self.blend_axes(blend, origin, loc, &path); }
                for time in ch.times_mut() {
                    self.time(time, origin, loc, &path);
                }
                for (_, part) in &mut ch.parts {
                    self.prop(&mut part.rotate, origin, loc, &path);
                    self.prop(&mut part.x, origin, loc, &path);
                    self.prop(&mut part.y, origin, loc, &path);
                    self.prop(&mut part.scale, origin, loc, &path);
                    self.prop(&mut part.opacity, origin, loc, &path);
                }
            }
            LayerKind::Crowd(crowd) => {
                self.prop(&mut crowd.color, origin, loc, &path);
                for prop in [&mut crowd.color2, &mut crowd.outline].into_iter().flatten() {
                    self.prop(prop, origin, loc, &path);
                }
                for prop in [&mut crowd.bob, &mut crowd.hop, &mut crowd.nod, &mut crowd.arms, &mut crowd.sign_up] {
                    self.prop(prop, origin, loc, &path);
                }
                self.time(&mut crowd.start, origin, loc, &path);
                if let Some(member) = &mut crowd.member {
                    for (_, target) in &mut member.reach { self.prop(target, origin, loc, &path); }
                    if let Some(blend) = &mut member.pose_blend { self.blend_axes(blend, origin, loc, &path); }
                    for time in member.times_mut() {
                        self.time(time, origin, loc, &path);
                    }
                }
            }
            LayerKind::Text { source, props: TextProps { size, color, stagger, reveal, tracking, .. }, ranges, plate, caret, axes, path: on_path, flicker, .. } => {
                if let Some((amount, _)) = flicker {
                    self.prop(amount, origin, loc, &path);
                }
                if let TextSource::Number { value, .. } = source {
                    self.prop(value, origin, loc, &path);
                }
                if let Some((_, offset)) = on_path {
                    self.prop(offset, origin, loc, &path);
                }
                self.prop(size, origin, loc, &path);
                self.prop(color, origin, loc, &path);
                for (_, axis) in axes.iter_mut() {
                    self.prop(axis, origin, loc, &path);
                }
                for range in ranges {
                    for (field, prop) in [("from", &mut range.from), ("to", &mut range.to), ("slide", &mut range.slide), ("soft", &mut range.soft)] {
                        self.prop(prop, origin, range.loc, &format!("{}.{field}", range.path));
                    }
                    for (field, prop) in [("x", &mut range.x), ("y", &mut range.y), ("rotate", &mut range.rotate), ("blur", &mut range.blur), ("opacity", &mut range.opacity)] {
                        if let Some(prop) = prop {
                            self.prop(prop, origin, range.loc, &format!("{}.{field}", range.path));
                        }
                    }
                    if let Some(prop) = &mut range.scale {
                        self.prop(prop, origin, range.loc, &format!("{}.scale", range.path));
                    }
                    if let Some(prop) = &mut range.color {
                        self.prop(prop, origin, range.loc, &format!("{}.color", range.path));
                    }
                    for axis in &mut range.axes {
                        self.prop(&mut axis.value, origin, range.loc, &format!("{}.axes.{}", range.path, axis.tag));
                    }
                }
                self.prop(tracking, origin, loc, &path);
                self.dur(&mut stagger.step, origin);
                if let Some(plate) = plate {
                    self.prop(&mut plate.color, origin, loc, &path);
                    self.prop(&mut plate.reveal, origin, loc, &path);
                    self.opt4(&mut plate.stroke, origin, loc, &path);
                }
                if let Some(reveal) = reveal {
                    self.prop(reveal, origin, loc, &path);
                }
                if let Some((opacity, color, _)) = caret {
                    self.prop(opacity, origin, loc, &path);
                    self.prop(color, origin, loc, &path);
                }
            }
            LayerKind::Vector { size, parts, draw_on, fill, stroke, .. } => {
                if let Some(size) = size {
                    self.prop(size, origin, loc, &path);
                }
                if let Some(draw_on) = draw_on {
                    self.prop(draw_on, origin, loc, &path);
                }
                self.opt4(fill, origin, loc, &path);
                self.opt4(stroke, origin, loc, &path);
                for (_, part) in parts {
                    self.prop(&mut part.opacity, origin, loc, &path);
                    self.prop(&mut part.x, origin, loc, &path);
                    self.prop(&mut part.y, origin, loc, &path);
                    self.prop(&mut part.scale, origin, loc, &path);
                    self.prop(&mut part.rotate, origin, loc, &path);
                    self.opt4(&mut part.fill, origin, loc, &path);
                    self.opt4(&mut part.stroke, origin, loc, &path);
                    if let Some(w) = &mut part.width {
                        self.prop(w, origin, loc, &path);
                    }
                    if let Some(d) = &mut part.draw_on {
                        self.prop(d, origin, loc, &path);
                    }
                }
            }
            LayerKind::Repeat { layer, items, .. } => {
                self.layer(layer, origin);
                for item in items {
                    self.layer(item, origin);
                }
            }
            LayerKind::Group { layers, mask, props: GroupProps { stagger, size, .. }, post, .. } => {
                self.post(&mut post.0, origin, loc, &path);
                self.dur(&mut stagger.step, origin);
                if let Some(size) = size {
                    self.prop(size, origin, loc, &path);
                }
                for child in layers {
                    self.layer(child, origin);
                }
                if let Some(mask) = mask {
                    self.layer(mask, origin);
                }
            }
            LayerKind::Shader { size, params, .. } => {
                if let Some(size) = size {
                    self.prop(size, origin, loc, &path);
                }
                for (_, param) in params {
                    self.prop(param, origin, loc, &path);
                }
            }
            LayerKind::Scene3D { size, camera, lights, nodes, materials, background, exposure, .. } => {
                if let Some(size) = size {
                    self.prop(size, origin, loc, &path);
                }
                self.prop(&mut camera.position, origin, loc, &path);
                self.prop(&mut camera.target, origin, loc, &path);
                self.prop(&mut camera.yaw, origin, loc, &path);
                self.prop(&mut camera.pitch, origin, loc, &path);
                self.prop(&mut camera.distance, origin, loc, &path);
                self.prop(&mut camera.fov, origin, loc, &path);
                for light in lights {
                    self.prop(&mut light.direction, origin, loc, &path);
                    self.prop(&mut light.position, origin, loc, &path);
                    self.prop(&mut light.color, origin, loc, &path);
                    self.prop(&mut light.intensity, origin, loc, &path);
                }
                for node in nodes {
                    self.prop(&mut node.translate, origin, loc, &path);
                    self.prop(&mut node.rotate, origin, loc, &path);
                    self.prop(&mut node.scale, origin, loc, &path);
                    self.prop(&mut node.visible, origin, loc, &path);
                }
                for material in materials {
                    self.opt4(&mut material.base_color, origin, loc, &path);
                    self.opt4(&mut material.emissive, origin, loc, &path);
                    if let Some(p) = &mut material.metallic {
                        self.prop(p, origin, loc, &path);
                    }
                    if let Some(p) = &mut material.roughness {
                        self.prop(p, origin, loc, &path);
                    }
                }
                self.opt4(background, origin, loc, &path);
                self.prop(exposure, origin, loc, &path);
            }
            LayerKind::Widget { size, props, .. } => {
                self.prop(size, origin, loc, &path);
                for (_, prop) in props {
                    match prop {
                        WidgetProp::Num(p) => self.prop(p, origin, loc, &path),
                        WidgetProp::Color(p) => self.prop(p, origin, loc, &path),
                        WidgetProp::Text(_) => {}
                    }
                }
            }
            LayerKind::Scene3dDoc { size, layers, .. } => {
                if let Some(size) = size {
                    self.prop(size, origin, loc, &path);
                }
                for (_, layer) in layers {
                    self.layer(layer, origin);
                }
            }
            LayerKind::Draw2d { size, .. } => {
                if let Some(size) = size {
                    self.prop(size, origin, loc, &path);
                }
            }
        }
    }
}

/// Resolve every time of `doc` against `context`; the warnings of what did
/// not resolve.
pub(crate) fn resolve_times(doc: &mut MotionDoc, context: &MotionContext) -> Vec<Diagnostic> {
    let mut resolver = Resolver { context, bpm: doc.bpm, warnings: Vec::new(), used_grid: None };
    for layer in &mut doc.layers {
        resolver.layer(layer, 0.0);
    }
    resolver.post(&mut doc.post, 0.0, Loc { line: 1, col: 1 }, "post");
    if let Some(blur) = &mut doc.motion_blur {
        resolver.prop(&mut blur.shutter, 0.0, Loc { line: 1, col: 1 }, "motion_blur.shutter");
    }
    if let Some(camera) = &mut doc.camera {
        for (_, arg) in camera.iter_mut() {
            resolver.arg(arg, 0.0, Loc { line: 1, col: 1 }, "camera");
        }
    }
    if let Some(loc) = resolver.used_grid {
        resolver.warnings.push(Diagnostic::warning(loc, "", format!("{RESOLVE_TAG}no beat grid is given, so beats count at the document's bpm ({}) from 0 s", crate::value::fmt_num(doc.bpm)))
            .hint("inside an edit the music's grid is used; standalone, set `bpm:` to the music's tempo"));
    }
    resolver.warnings
}

/// Whether any time in the document is musical or spoken.
pub fn uses_musical_time(doc: &MotionDoc) -> bool {
    fn prop<const N: usize>(p: &Prop<N>) -> bool {
        if let Prop::Blend(blend) = p {
            return prop(&blend.x) || blend.y.as_ref().is_some_and(prop) || blend.values.iter().any(prop);
        }
        matches!(p, Prop::Track(track) if track.keys.iter().any(|k| k.t.expr.is_musical()) || track.repeat.as_ref().is_some_and(|r| matches!(r.period.expr, Length::Beats(_))))
    }
    doc.all_layers().iter().any(|layer| {
        layer.span.as_ref().is_some_and(|s| s.start.expr.is_musical() || matches!(s.length.expr, Length::Beats(_)))
            || prop(&layer.transform.at) || prop(&layer.transform.x) || prop(&layer.transform.y) || prop(&layer.transform.scale)
            || prop(&layer.transform.rotate) || prop(&layer.opacity) || prop(&layer.feather)
            || match &layer.kind {
                LayerKind::Text { props: TextProps { stagger, .. }, source, ranges, .. } => matches!(stagger.step.expr, Length::Beats(_)) || matches!(source, TextSource::Voice(_))
                    || ranges.iter().any(|r| [&r.from, &r.to, &r.slide, &r.soft].into_iter().any(prop)
                        || [&r.x, &r.y, &r.rotate, &r.blur, &r.opacity].into_iter().any(|p| p.as_ref().is_some_and(prop))
                        || r.scale.as_ref().is_some_and(prop) || r.color.as_ref().is_some_and(prop)
                        || r.axes.iter().any(|a| prop(&a.value))),
                LayerKind::Bones(rig) => rig.bones.iter().any(|b| prop(&b.rotate))
                    || rig.follow.iter().any(|f| prop(&f.along))
                    || rig.ik.iter().any(|ik| prop(&ik.target) || prop(&ik.mix)),
                LayerKind::Character(ch) => ch.reach.iter().any(|(_, p)| prop(p))
                    || ch.pose_blend.as_ref().is_some_and(|b| prop(&b.x) || b.y.as_ref().is_some_and(prop)),
                LayerKind::Crowd(crowd) => crowd.member.as_ref().is_some_and(|ch| ch.reach.iter().any(|(_, p)| prop(p))
                    || ch.pose_blend.as_ref().is_some_and(|b| prop(&b.x) || b.y.as_ref().is_some_and(prop))),
                LayerKind::Group { props: GroupProps { stagger, .. }, .. } => matches!(stagger.step.expr, Length::Beats(_)),
                _ => false,
            }
    })
}
