use super::Evaluator;
use crate::frame::{Affine, ItemKind};
use crate::model::Diagnostic;
use crate::rig::{Bones, Solver};

impl Evaluator<'_> {
    pub(super) fn bones(&mut self, rig: &Bones, t: f64, origin: f64, placed: &Affine, is_static: bool) -> ItemKind {
        let mut solver = Solver::new(rig, |i| self.sample(&rig.bones[i].rotate, t, 0)[0]);
        for follow in &rig.follow {
            let along = self.sample(&follow.along, t, 0)[0];
            solver.follow(follow, along);
        }
        for ik in &rig.ik {
            let target = self.sample(&ik.target, t, 0);
            let mix = self.sample(&ik.mix, t, 0)[0];
            if solver.ik(ik, target, mix) {
                self.errors.push(Diagnostic::warning(ik.loc, ik.path.clone(), "IK.target is out of reach; using the nearest reachable point (full extension for a distant target)"));
            }
        }
        let maps = solver.skin_maps();
        let mut children = Vec::with_capacity(rig.layers.len());
        for (layer, skin) in rig.layers.iter().zip(&rig.skins) {
            if let Some(mut item) = self.layer(layer, t, origin, &Affine::IDENTITY, is_static) {
                skin.deform(&mut item, layer, &rig.rest, &maps, placed);
                children.push(item);
            }
        }
        ItemKind::Group { children, mask: None, mask_mode: Default::default(), size: None }
    }
}
