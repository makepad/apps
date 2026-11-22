use makepad_model::{
    character::{rest_joints, CHARACTER_JOINTS},
    templates::humanoid_skeleton,
    transform::{add, quat_axis_angle, quat_mul, quat_normalize, quat_rotate, sub},
    Joint, Operation, ProgramOp, RigOperation, RigState, Skeleton,
};

/// A built rig's rest skeleton, known when `m.build()` records the program.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct RigRest {
    pub names: Vec<String>,
    /// Parent index, -1 for a root.
    pub parent: Vec<i16>,
    /// Local rest translation in the parent's frame (metres).
    pub pos: Vec<[f32; 3]>,
    /// Local rest rotation, quaternion x y z w.
    pub rot: Vec<[f32; 4]>,
}

impl RigRest {
    pub fn index(&self, name: &str) -> Option<usize> { self.names.iter().position(|n| n == name) }

    pub fn model_space(&self) -> Vec<[f32; 3]> {
        let mut world = vec![None; self.names.len()];
        let mut path = Vec::new();
        for i in 0..self.names.len() {
            // A reparent can put the parent later in joint order. Resolve its chain first.
            let mut next = Some(i);
            while let Some(j) = next {
                if world[j].is_some() { break; }
                path.push(j);
                next = usize::try_from(self.parent[j]).ok();
            }
            while let Some(j) = path.pop() {
                let pos = self.pos[j].map(f64::from);
                let rot = self.rot[j].map(f64::from);
                world[j] = Some(if let Ok(p) = usize::try_from(self.parent[j]) {
                    let (parent_pos, parent_rot) = world[p].expect("parent resolved before child");
                    (add(parent_pos, quat_rotate(parent_rot, pos)), quat_mul(parent_rot, rot))
                } else { (pos, rot) });
            }
        }
        world.into_iter().map(|joint| joint.expect("resolved joint").0.map(|v| v as f32)).collect()
    }
}

fn edit_rest(skeleton: &mut Skeleton, state: &mut RigState, names: Option<&[String]>, op: &RigOperation) -> Option<()> {
    // Only a ProgramOp::Rig carries the name table for script-side rest edits.
    let index = |joint: u32| {
        let name = names?.get(joint as usize)?;
        skeleton.joints.iter().position(|j| j.name == *name)
    };
    match op {
        RigOperation::Rest { joint, transform } => {
            transform.validate().ok()?;
            state.rests.insert(index(*joint)? as u32, *transform);
        }
        RigOperation::Roll { joint, angle } => {
            let joint = index(*joint)?;
            let mut rest = state.local_rest(skeleton, joint).ok()?;
            let turn = quat_axis_angle([0., 1., 0.], *angle).ok()?;
            rest.rotation = quat_normalize(quat_mul(rest.rotation, turn)).ok()?;
            state.rests.insert(joint as u32, rest);
        }
        RigOperation::RenameJoint { joint, name } => {
            let joint = index(*joint)?;
            if skeleton.joints.iter().enumerate().any(|(i, j)| i != joint && j.name == *name) { return None; }
            skeleton.joints[joint].name.clone_from(name);
        }
        RigOperation::ReparentJoint { joint, parent } => {
            let joint = index(*joint)?;
            let parent = match parent { Some(p) => Some(index(*p)? as u32), None => None };
            let mut ancestor = parent;
            while let Some(p) = ancestor {
                if p as usize == joint { return None; }
                ancestor = skeleton.joints[p as usize].parent;
            }
            skeleton.joints[joint].parent = parent;
        }
        // Rig operations that leave the rest skeleton alone.
        RigOperation::Pose(_) | RigOperation::DeletePose { .. } | RigOperation::Constraint(_)
        | RigOperation::DeleteConstraint { .. } | RigOperation::SolvePose { .. } | RigOperation::Ik { .. }
        | RigOperation::BakeClip { .. } | RigOperation::BlendClip { .. } | RigOperation::RetargetClip { .. }
        | RigOperation::ClipOptions { .. } | RigOperation::WeightLocks { .. } | RigOperation::Weights { .. }
        | RigOperation::Morph { .. } | RigOperation::CaptureMorph { .. } | RigOperation::DeleteMorph { .. }
        | RigOperation::MorphWeight { .. } => {}
        // MirrorJoints adds joints this walk does not model. An operation libs/model
        // adds later reads as an unknown rig until it is classified here: unknown,
        // never wrong, and this crate keeps building.
        _ => return None,
    }
    Some(())
}

pub fn rig_rest(program: &[ProgramOp]) -> Option<RigRest> {
    let mut skeleton = Skeleton { joints: Vec::new() };
    // SetSkeleton replaces the joint table, but the document keeps indexed rest overrides.
    let mut state = RigState::default();
    for step in program {
        match step {
            ProgramOp::HumanoidRig { height, .. } => {
                if !height.is_finite() || *height <= 0. { return None; }
                skeleton = humanoid_skeleton(*height);
            }
            ProgramOp::Character(spec) => {
                let world = rest_joints(spec);
                skeleton.joints = CHARACTER_JOINTS.iter().enumerate().map(|(i, &(name, parent))| {
                    let base = parent.map_or([0.; 3], |p| world[p].1);
                    Joint { name: name.into(), parent: parent.map(|p| p as u32), translation: sub(world[i].1, base) }
                }).collect();
            }
            ProgramOp::Joint { name, parent, translation } => {
                if name.is_empty() || skeleton.joints.iter().any(|j| j.name == *name) { return None; }
                let parent = if parent.is_empty() {
                    if !skeleton.joints.is_empty() { return None; }
                    None
                } else { Some(skeleton.joints.iter().position(|j| j.name == *parent)? as u32) };
                skeleton.joints.push(Joint { name: name.clone(), parent, translation: *translation });
            }
            ProgramOp::Rig { joints, op } => edit_rest(&mut skeleton, &mut state, Some(joints), op)?,
            ProgramOp::Op(Operation::Rig(op)) => edit_rest(&mut skeleton, &mut state, None, op)?,
            ProgramOp::ImportGlb { .. } | ProgramOp::FpsArms(..)
            | ProgramOp::Op(Operation::SoftBody(_) | Operation::SetSkeleton { .. }) => return None,
            ProgramOp::Bind { .. } | ProgramOp::BindAll | ProgramOp::Clip { .. } | ProgramOp::Weathering { .. }
            | ProgramOp::Impostor { .. } | ProgramOp::Mesh(_) | ProgramOp::ImageLayer { .. } | ProgramOp::TextLayer { .. } => {}
            // Every other document operation (meshes, materials, scene, clips) leaves the
            // skeleton alone: a wildcard, so one libs/model adds later cannot break this crate.
            ProgramOp::Op(_) => {}
        }
    }
    if skeleton.joints.is_empty() { return None; }
    let mut rig = RigRest::default();
    for (i, joint) in skeleton.joints.iter().enumerate() {
        let rest = state.local_rest(&skeleton, i).ok()?;
        // A scaled rest cannot be represented by a rigid translation and rotation.
        if rest.scale != [1.; 3] { return None; }
        rig.parent.push(match joint.parent { Some(p) => i16::try_from(p).ok()?, None => -1 });
        rig.pos.push(rest.translation.map(|v| v as f32));
        rig.rot.push(rest.rotation.map(|v| v as f32));
    }
    rig.names = skeleton.joints.into_iter().map(|joint| joint.name).collect();
    Some(rig)
}
