//! A `Character` layer at one moment (lane CH1): its `parts` offsets
//! sampled, the rig solved on the document's beat grid and the voice's
//! words, and lowered to plain shape items.

use super::Evaluator;
use crate::character::{self, CharacterLayer, Moment, PartOffset};
use crate::frame::{Affine, ItemKind};

impl Evaluator<'_> {
    pub(super) fn pose_blend(&mut self, ch: &CharacterLayer, t: f64, i: usize) -> Option<character::pose::Pose> {
        let blend = ch.pose_blend.as_ref()?;
        let x = self.sample_kind(&blend.x, t, i, Some(1))[0];
        let y = blend.y.as_ref().map_or(0.0, |y| self.sample_kind(y, t, i, Some(1))[0]);
        Some(blend.sample(x, y, |pose| *pose, |a, b, w| a.mix(&b, w)))
    }

    pub(super) fn character(&mut self, path: &str, loc: crate::Loc, ch: &CharacterLayer, lt: f64, origin: f64, placed: &Affine) -> ItemKind {
        let empty = ItemKind::Group { children: Vec::new(), mask: None, mask_mode: Default::default(), size: None };
        let Some(rig) = ch.rig() else { return empty };
        let parts: Vec<PartOffset> = ch.parts.iter().map(|(_, keys)| PartOffset {
            rotate: self.sample(&keys.rotate, lt, 0)[0],
            offset: [self.sample(&keys.x, lt, 0)[0], self.sample(&keys.y, lt, 0)[0]],
            scale: self.sample(&keys.scale, lt, 0),
            opacity: self.sample(&keys.opacity, lt, 0)[0],
        }).collect();
        let reach: Vec<_> = ch.reach.iter().map(|(joint, target)| (*joint, self.sample(target, lt, 0))).collect();
        let pose = self.pose_blend(ch, lt, 0);
        let this = &*self;
        let beat = move |u: f64| this.beat_at(origin + u);
        let signals = this.doc.context.signals.clone();
        let voice = ch.lip_sync.as_deref().and_then(|id| signals.voice(id));
        let moment = Moment { t: lt, beat: &beat, voice, host: origin + this.doc.context.offset, fps: this.doc.fps, parts: &parts, reach: &reach, pose, instance: this.instance, arms: 0.0, hop: 0.0, nod: 0.0 };
        let solved = character::solve(ch, rig, &moment);
        for joint in &solved.unreachable {
            self.errors.push(crate::Diagnostic::warning(loc, format!("{path}.reach.{}", joint.name()), "Character reach target is out of reach; using the nearest reachable point"));
        }
        ItemKind::Group { children: character::items(rig, &solved, path, placed), mask: None, mask_mode: Default::default(), size: None }
    }
}
