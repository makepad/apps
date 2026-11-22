//! `m.clip(name, {...})`: keyframed animation authored in script for a
//! humanoid rig (`m.rig_humanoid`).
//!
//! A clip is a list of key POSES at frame times. A pose names joints and
//! their rotation in degrees (`vec3(x, y, z)`, the same XYZ order as every
//! `rot:` in the builder; +x swings a hanging limb forward), plus `root`, an
//! offset of the hips from their rest position (crouch, lunge, hop). Keys are
//! sticky: a joint a key does not mention keeps its previous value, and the
//! first key starts from `base`. Each key says how motion EASES into it —
//! `snap` (fast out, the strike), `out`, `in`, `inout` (default), `linear`,
//! `hold` — which is what makes a punch read as a punch.
//!
//! The clip is baked at 60 frames per second into ordinary rotation and
//! translation keys, so a fighting game can sample it frame-exactly: move
//! frame N is clip time N/60.

use super::*;
use makepad_model::templates::humanoid_joints;

#[derive(Clone, Copy)]
enum Ease {
    Linear,
    In,
    Out,
    InOut,
    Snap,
    Hold,
}

fn ease(e: Ease, u: f64) -> f64 {
    let u = u.clamp(0.0, 1.0);
    match e {
        Ease::Linear => u,
        Ease::In => u * u * u,
        Ease::Out => 1.0 - (1.0 - u).powi(3),
        Ease::InOut => u * u * (3.0 - 2.0 * u),
        Ease::Snap => 1.0 - (1.0 - u).powi(5),
        Ease::Hold => 0.0,
    }
}

fn slerp(a: [f64; 4], b: [f64; 4], t: f64) -> [f64; 4] {
    let mut d = a[0] * b[0] + a[1] * b[1] + a[2] * b[2] + a[3] * b[3];
    let b = if d < 0.0 { d = -d; b.map(|v| -v) } else { b };
    if d > 0.9995 {
        let q = [0, 1, 2, 3].map(|i| a[i] + (b[i] - a[i]) * t);
        let n = (q.iter().map(|v| v * v).sum::<f64>()).sqrt().max(1e-12);
        return q.map(|v| v / n);
    }
    let th = d.clamp(-1.0, 1.0).acos();
    let s = th.sin();
    let (wa, wb) = (((1.0 - t) * th).sin() / s, (t * th).sin() / s);
    [0, 1, 2, 3].map(|i| a[i] * wa + b[i] * wb)
}

/// A resolved pose: rotation per joint (quaternion) and the hips offset.
#[derive(Clone)]
struct Pose {
    rot: Vec<Option<[f64; 4]>>,
    root: [f64; 3],
}

impl Recorder<'_, '_> {
    fn read_pose(&mut self, v: ScriptValue, into: &mut Pose, names: &[&str]) -> Result<(), String> {
        let Some(o) = v.as_object() else {
            return if v.is_nil() { Ok(()) } else { Err("a pose is an object {joint: vec3(deg)}".into()) };
        };
        let n = self.vm.bx.heap_ref().iter_len(o);
        for k in 0..n {
            let kv = self.vm.bx.heap_ref().iter_key_value(o, k, NoTrap);
            let Some(key) = kv.key.as_id() else { continue };
            let key = key.to_string();
            let p = value_vec3(self.vm, kv.value);
            let p = [p.x as f64, p.y as f64, p.z as f64];
            if key == "root" {
                into.root = p;
                continue;
            }
            let Some(j) = names.iter().position(|x| *x == key) else {
                return Err(format!("unknown joint '{key}' (hips spine chest neck head shoulder_l upper_arm_l lower_arm_l hand_l … upper_leg_r lower_leg_r foot_r toe_r, or root)"));
            };
            into.rot[j] = Some(quat_euler_deg(p));
        }
        Ok(())
    }

    /// `m.clip_rest({joint: vec3(deg)})`: the reference pose clips are
    /// keyed from, relative to the rig's own rest. Every later clip's
    /// rotation for that joint is applied on top of it, so one set of
    /// arms-hanging poses drives an A-pose rig (upper_arm_l: vec3(0, 0, 50),
    /// upper_arm_r: vec3(0, 0, -50)). `{}` clears it.
    pub(super) fn m_clip_rest(&mut self) -> ScriptValue {
        let v = self.arg(0);
        let mut rest = Vec::new();
        if let Some(o) = v.as_object() {
            let n = self.vm.bx.heap_ref().iter_len(o);
            for k in 0..n {
                let kv = self.vm.bx.heap_ref().iter_key_value(o, k, NoTrap);
                let Some(key) = kv.key.as_id() else { continue };
                let p = value_vec3(self.vm, kv.value);
                rest.push((key.to_string(), [p.x as f64, p.y as f64, p.z as f64]));
            }
        }
        self.b().clip_rest = rest;
        NIL
    }

    pub(super) fn m_clip(&mut self) -> ScriptValue {
        let name = self.text(0);
        let o = self.opts(1, &["base", "keys", "events", "looping", "length"]);
        if name.is_empty() {
            return self.fail("clip needs a name");
        }
        if !self.b().rigged {
            return self.fail("m.clip needs m.rig_humanoid() or m.character() first");
        }
        let height = self
            .b()
            .ops
            .iter()
            .rev()
            .find_map(|op| match op { makepad_model::ProgramOp::HumanoidRig { height, .. } => Some(*height), _ => None })
            .unwrap_or(1.8);
        let joints = humanoid_joints(height);
        let names: Vec<&str> = joints.iter().map(|j| j.0).collect();
        let hips_rest = self.b().rest_hips.unwrap_or(joints[0].2);

        let mut pose = Pose { rot: vec![None; names.len()], root: [0.0; 3] };
        let base = opt(self.vm, o, id!(base));
        if let Err(e) = self.read_pose(base, &mut pose, &names) {
            return self.fail(format!("clip '{name}' base: {e}"));
        }
        // Keys: (frame, pose, ease).
        let keys_v = opt(self.vm, o, id!(keys));
        let n = list_len(self.vm, keys_v);
        if n == 0 {
            return self.fail(format!("clip '{name}' needs keys: [{{f, pose}}]"));
        }
        let mut keys: Vec<(f64, Pose, Ease)> = Vec::new();
        for i in 0..n {
            let item = list_value(self.vm, keys_v, i).as_object();
            let frame = opt_num(self.vm, item, id!(f))
                .or_else(|| opt_num(self.vm, item, id!(t)).map(|t| t * 60.0))
                .unwrap_or(i as f64 * 6.0);
            let e = match opt_text(self.vm, item, id!(ease)).as_deref() {
                Some("linear") => Ease::Linear,
                Some("in") => Ease::In,
                Some("out") => Ease::Out,
                Some("snap") => Ease::Snap,
                Some("hold") => Ease::Hold,
                _ => Ease::InOut,
            };
            let pv = opt(self.vm, item, id!(pose));
            if let Err(e) = self.read_pose(pv, &mut pose, &names) {
                return self.fail(format!("clip '{name}' key {i}: {e}"));
            }
            keys.push((frame, pose.clone(), e));
        }
        keys.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
        let looping = o.is_some_and(|o| opt_bool(self.vm, o, id!(looping), false));
        let last = keys.last().map(|k| k.0).unwrap_or(0.0);
        let length = opt_num(self.vm, o, id!(length)).map(|l| l * 60.0).unwrap_or(last).max(last);
        if looping {
            // Close the loop on the first pose.
            let first = keys[0].1.clone();
            if length > last {
                keys.push((length, first, Ease::InOut));
            }
        }
        // Bake at 60 fps.
        let frames = length.ceil().max(1.0) as usize;
        let sample = |t: f64| -> (Vec<[f64; 4]>, [f64; 3]) {
            let mut i = 0;
            while i + 1 < keys.len() && keys[i + 1].0 <= t {
                i += 1;
            }
            let (a, b) = (&keys[i], &keys[(i + 1).min(keys.len() - 1)]);
            let span = (b.0 - a.0).max(1e-9);
            let u = if std::ptr::eq(a, b) { 0.0 } else { ease(b.2, (t - a.0) / span) };
            let rot = (0..names.len())
                .map(|j| {
                    let qa = a.1.rot[j].unwrap_or([0.0, 0.0, 0.0, 1.0]);
                    let qb = b.1.rot[j].unwrap_or([0.0, 0.0, 0.0, 1.0]);
                    slerp(qa, qb, u)
                })
                .collect();
            let root = [0, 1, 2].map(|d| a.1.root[d] + (b.1.root[d] - a.1.root[d]) * u);
            (rot, root)
        };
        // Reference pose: local r_j per joint and its accumulated global G_j
        // (parents precede children in the table). A key k_j is authored in
        // world-aligned frames AT the reference pose, so the rig-local
        // rotation is conj(G_parent) · k_j · G_parent · r_j.
        let rest: Vec<[f64; 4]> = names
            .iter()
            .map(|n| self.b().clip_rest.iter().find(|(j, _)| j == n).map_or([0.0, 0.0, 0.0, 1.0], |(_, e)| quat_euler_deg(*e)))
            .collect();
        let mut rest_global: Vec<[f64; 4]> = vec![[0.0, 0.0, 0.0, 1.0]; names.len()];
        for (j, joint) in joints.iter().enumerate() {
            let parent = joint.1.map_or([0.0, 0.0, 0.0, 1.0], |p| rest_global[p]);
            rest_global[j] = quat_mul(parent, rest[j]);
        }
        let parent_global: Vec<[f64; 4]> = joints.iter().map(|j| j.1.map_or([0.0, 0.0, 0.0, 1.0], |p| rest_global[p])).collect();
        let has_rest = !self.b().clip_rest.is_empty();
        use makepad_model::{AnimationChannel, AnimationClip, AnimationPath, ClipEvent, ClipOptions, Interpolation, Keyframe, Operation, RigOperation};
        let mut rot_keys: Vec<Vec<Keyframe>> = vec![Vec::new(); names.len()];
        let mut root_keys: Vec<Keyframe> = Vec::new();
        for fr in 0..=frames {
            let t = fr as f64;
            let (rot, root) = sample(t);
            let time = t / 60.0;
            for (j, q) in rot.iter().enumerate() {
                // The key rotates the limb FROM the reference pose: apply the
                // reference first (right-multiplied), then the key.
                let q = if has_rest {
                    let g = parent_global[j];
                    let gc = [-g[0], -g[1], -g[2], g[3]];
                    quat_mul(quat_mul(quat_mul(gc, *q), g), rest[j])
                } else {
                    *q
                };
                rot_keys[j].push(Keyframe { time, value: q });
            }
            let p = [hips_rest[0] + root[0], hips_rest[1] + root[1], hips_rest[2] + root[2], 0.0];
            root_keys.push(Keyframe { time, value: p });
        }
        let mut channels: Vec<AnimationChannel> = rot_keys
            .into_iter()
            .enumerate()
            .map(|(j, keys)| AnimationChannel { joint: j as u32, path: AnimationPath::Rotation, keys })
            .collect();
        channels.push(AnimationChannel { joint: 0, path: AnimationPath::Translation, keys: root_keys });
        self.push(makepad_model::ProgramOp::Op(Operation::SetClip { clip: AnimationClip { name: name.clone(), channels } }));
        // Markers (`on_animation`): {f, name}.
        let ev = opt(self.vm, o, id!(events));
        let en = list_len(self.vm, ev);
        if en > 0 {
            let mut events = Vec::new();
            for i in 0..en {
                let item = list_value(self.vm, ev, i).as_object();
                let t = opt_num(self.vm, item, id!(f)).unwrap_or(0.0) / 60.0;
                let n = opt_text(self.vm, item, id!(name)).unwrap_or_default();
                events.push(ClipEvent { time: t, name: n, payload: String::new() });
            }
            events.sort_by(|a, b| a.time.partial_cmp(&b.time).unwrap_or(std::cmp::Ordering::Equal));
            let options = ClipOptions { interpolation: Interpolation::Linear, events, ..ClipOptions::default() };
            self.push(makepad_model::ProgramOp::Op(Operation::Rig(RigOperation::ClipOptions { name: name.clone(), options })));
        }
        self.vm.bx.heap_mut().new_string_from_str(&name)
    }
}
