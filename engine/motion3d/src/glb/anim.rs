//! Poses: clips sampled at a time and blended, world matrices, and meshes
//! morphed and skinned on the CPU. Everything is a pure function of the
//! pose, so a frame at `t` is the same on every run.

use super::{Channel, ChannelPath, GlbModel, Interpolation};
use makepad_geom3d::math::{compose, mul, quat_nlerp, quat_normalize, quat_slerp, transform_dir, transform_point, Quat4};
use makepad_geom3d::mesh::{normalize_or_up, Mesh};
use makepad_widgets::*;

/// Every node's local transform and morph weights at one moment.
#[derive(Clone, Debug, PartialEq)]
pub struct GlbPose {
    pub t: Vec<[f32; 3]>,
    pub r: Vec<Quat4>,
    pub s: Vec<[f32; 3]>,
    /// Morph weights per node (empty for nodes without morph targets).
    pub weights: Vec<Vec<f32>>,
}

/// Which space [`GlbModel::posed_primitive`] returned its mesh in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MeshSpace {
    /// Skinned: already in world space (the node's own transform is ignored,
    /// as glTF specifies).
    World,
    /// Not skinned: in the node's local space; multiply by `world[node]`.
    NodeLocal,
}

/// Where `t` falls in `times`: key index `k` and the fraction toward `k + 1`.
fn locate(times: &[f32], t: f32) -> (usize, f32, f32) {
    let last = times.len() - 1;
    if t <= times[0] {
        return (0, 0.0, 0.0);
    }
    if t >= times[last] {
        return (last, 0.0, 0.0);
    }
    // Largest k with times[k] <= t.
    let k = times.partition_point(|&x| x <= t).saturating_sub(1).min(last - 1);
    let dt = times[k + 1] - times[k];
    let f = if dt > 0.0 { (t - times[k]) / dt } else { 0.0 };
    (k, f, dt)
}

/// A channel's value at `t` (`n` numbers).
pub(crate) fn sample(ch: &Channel, t: f32, out: &mut Vec<f32>) {
    let n = ch.width();
    out.clear();
    out.resize(n, 0.0);
    if ch.times.is_empty() {
        return;
    }
    let (k, f, dt) = locate(&ch.times, t);
    let cubic = ch.interpolation == Interpolation::CubicSpline;
    let value = |key: usize, part: usize, i: usize| -> f32 {
        let at = if cubic { (key * 3 + part) * n + i } else { key * n + i };
        ch.values.get(at).copied().unwrap_or(0.0)
    };
    let last = ch.times.len() - 1;
    if f == 0.0 || k >= last || ch.interpolation == Interpolation::Step {
        for (i, slot) in out.iter_mut().enumerate() {
            *slot = value(k, 1, i);
        }
    } else if cubic {
        let (s2, s3) = (f * f, f * f * f);
        let (h00, h10, h01, h11) = (2.0 * s3 - 3.0 * s2 + 1.0, s3 - 2.0 * s2 + f, -2.0 * s3 + 3.0 * s2, s3 - s2);
        for (i, slot) in out.iter_mut().enumerate() {
            let v0 = value(k, 1, i);
            let b0 = value(k, 2, i);
            let v1 = value(k + 1, 1, i);
            let a1 = value(k + 1, 0, i);
            *slot = h00 * v0 + h10 * dt * b0 + h01 * v1 + h11 * dt * a1;
        }
    } else if ch.path == ChannelPath::Rotation && n == 4 {
        let q0 = [value(k, 1, 0), value(k, 1, 1), value(k, 1, 2), value(k, 1, 3)];
        let q1 = [value(k + 1, 1, 0), value(k + 1, 1, 1), value(k + 1, 1, 2), value(k + 1, 1, 3)];
        out.copy_from_slice(&quat_slerp(q0, q1, f));
    } else {
        for (i, slot) in out.iter_mut().enumerate() {
            let (a, b) = (value(k, 1, i), value(k + 1, 1, i));
            *slot = a + (b - a) * f;
        }
    }
    if ch.path == ChannelPath::Rotation && n == 4 {
        let q = quat_normalize([out[0], out[1], out[2], out[3]]);
        out.copy_from_slice(&q);
    }
}

fn lerp3(a: [f32; 3], b: [f32; 3], w: f32) -> [f32; 3] {
    [a[0] + (b[0] - a[0]) * w, a[1] + (b[1] - a[1]) * w, a[2] + (b[2] - a[2]) * w]
}

impl GlbModel {
    /// The rest pose: every node's own TRS and default morph weights.
    pub fn rest_pose(&self) -> GlbPose {
        GlbPose {
            t: self.nodes.iter().map(|n| n.translation).collect(),
            r: self.nodes.iter().map(|n| n.rotation).collect(),
            s: self.nodes.iter().map(|n| n.scale).collect(),
            weights: self
                .nodes
                .iter()
                .map(|n| {
                    let targets = n
                        .mesh
                        .and_then(|m| self.meshes.get(m))
                        .map(|m| m.primitives.iter().map(|p| p.targets.len()).max().unwrap_or(0))
                        .unwrap_or(0);
                    if targets == 0 {
                        return Vec::new();
                    }
                    let mut w = if !n.weights.is_empty() {
                        n.weights.clone()
                    } else {
                        n.mesh.and_then(|m| self.meshes.get(m)).map(|m| m.weights.clone()).unwrap_or_default()
                    };
                    w.resize(targets, 0.0);
                    w
                })
                .collect(),
        }
    }

    /// Sample clip `clip` at `time` (clamped to the clip) into `pose`:
    /// `weight` 1 replaces what the pose holds, less blends toward the clip
    /// (lerp, and nlerp for rotations), so several clips layer by calling
    /// this in turn. Looping is the caller's choice of `time`.
    pub fn apply_clip(&self, pose: &mut GlbPose, clip: usize, time: f32, weight: f32) {
        let Some(clip) = self.clips.get(clip) else { return };
        let t = time.clamp(0.0, clip.duration.max(0.0));
        let w = weight.clamp(0.0, 1.0);
        if w <= 0.0 {
            return;
        }
        let mut v = Vec::with_capacity(4);
        for ch in &clip.channels {
            if ch.node >= pose.t.len() {
                continue;
            }
            sample(ch, t, &mut v);
            let node = ch.node;
            match ch.path {
                ChannelPath::Translation if v.len() >= 3 => pose.t[node] = lerp3(pose.t[node], [v[0], v[1], v[2]], w),
                ChannelPath::Scale if v.len() >= 3 => pose.s[node] = lerp3(pose.s[node], [v[0], v[1], v[2]], w),
                ChannelPath::Rotation if v.len() >= 4 => {
                    let q = [v[0], v[1], v[2], v[3]];
                    pose.r[node] = if w >= 1.0 { q } else { quat_nlerp(pose.r[node], q, w) };
                }
                ChannelPath::Weights => {
                    let target = &mut pose.weights[node];
                    if target.len() < v.len() {
                        target.resize(v.len(), 0.0);
                    }
                    for (slot, &x) in target.iter_mut().zip(&v) {
                        *slot += (x - *slot) * w;
                    }
                }
                _ => {}
            }
        }
    }

    /// World matrix of every node in `pose` (roots at the identity).
    /// Robust to any node order; a node in a parent cycle is treated as a root.
    pub fn world_matrices(&self, pose: &GlbPose) -> Vec<Mat4f> {
        let count = self.nodes.len();
        let mut world = vec![Mat4f::identity(); count];
        // 0 = todo, 1 = in progress, 2 = done.
        let mut state = vec![0u8; count];
        for start in 0..count {
            if state[start] == 2 {
                continue;
            }
            // Walk up to the first finished (or root) ancestor, then down.
            let mut chain = Vec::new();
            let mut n = Some(start);
            while let Some(i) = n {
                if state[i] != 0 {
                    break;
                }
                state[i] = 1;
                chain.push(i);
                n = self.nodes[i].parent;
            }
            for &i in chain.iter().rev() {
                let local = compose(pose.t[i], pose.r[i], pose.s[i]);
                world[i] = match self.nodes[i].parent {
                    Some(p) if state[p] == 2 => mul(&world[p], &local),
                    _ => local,
                };
                state[i] = 2;
            }
        }
        world
    }

    /// Primitive `prim` of the mesh on `node`, morphed by the pose's weights
    /// and, when the node has a skin, linear-blend skinned by `world`
    /// (from [`GlbModel::world_matrices`]) into world space.
    pub fn posed_primitive(&self, node: usize, prim: usize, pose: &GlbPose, world: &[Mat4f]) -> (Mesh, MeshSpace) {
        let n = &self.nodes[node];
        let Some(p) = n.mesh.and_then(|m| self.meshes.get(m)).and_then(|m| m.primitives.get(prim)) else {
            return (Mesh::default(), MeshSpace::NodeLocal);
        };
        let mut mesh = p.mesh.clone();
        if let Some(weights) = pose.weights.get(node) {
            let mut moved_normals = false;
            for (target, &w) in p.targets.iter().zip(weights) {
                if w == 0.0 {
                    continue;
                }
                for (pos, d) in mesh.positions.iter_mut().zip(&target.positions) {
                    for k in 0..3 {
                        pos[k] += d[k] * w;
                    }
                }
                for (nrm, d) in mesh.normals.iter_mut().zip(&target.normals) {
                    moved_normals = true;
                    for k in 0..3 {
                        nrm[k] += d[k] * w;
                    }
                }
            }
            if moved_normals {
                mesh.normals.iter_mut().for_each(|v| *v = normalize_or_up(*v));
            }
        }
        let skin = n.skin.and_then(|s| self.skins.get(s));
        let (Some(skin), false) = (skin, p.joints.is_empty()) else {
            return (mesh, MeshSpace::NodeLocal);
        };
        let joint_m: Vec<Mat4f> = skin
            .joints
            .iter()
            .enumerate()
            .map(|(j, &node)| {
                let ib = skin.inverse_bind.get(j).copied().unwrap_or_else(Mat4f::identity);
                mul(world.get(node).unwrap_or(&Mat4f::identity()), &ib)
            })
            .collect();
        let blend = |v: usize| -> Mat4f {
            let mut m = Mat4f { v: [0.0; 16] };
            for k in 0..4 {
                let w = p.weights[v][k];
                if w == 0.0 {
                    continue;
                }
                let jm = joint_m.get(p.joints[v][k] as usize).copied().unwrap_or_else(Mat4f::identity);
                for e in 0..16 {
                    m.v[e] += jm.v[e] * w;
                }
            }
            m
        };
        for v in 0..mesh.positions.len() {
            let m = blend(v);
            mesh.positions[v] = transform_point(&m, mesh.positions[v]);
            if let Some(nrm) = mesh.normals.get_mut(v) {
                *nrm = normalize_or_up(transform_dir(&m, *nrm));
            }
            if let Some(t) = mesh.tangents.get_mut(v) {
                let d = normalize_or_up(transform_dir(&m, [t[0], t[1], t[2]]));
                *t = [d[0], d[1], d[2], t[3]];
            }
        }
        (mesh, MeshSpace::World)
    }

    /// World-space bounds of every posed mesh of the scene, for framing a
    /// camera; `None` when the model draws nothing.
    pub fn bounds(&self, pose: &GlbPose) -> Option<([f32; 3], [f32; 3])> {
        let world = self.world_matrices(pose);
        let mut out: Option<([f32; 3], [f32; 3])> = None;
        for (node, mesh) in self.scene_nodes() {
            for prim in 0..self.meshes[mesh].primitives.len() {
                let (m, space) = self.posed_primitive(node, prim, pose, &world);
                for p in &m.positions {
                    let p = if space == MeshSpace::NodeLocal { transform_point(&world[node], *p) } else { *p };
                    let b = out.get_or_insert((p, p));
                    for k in 0..3 {
                        b.0[k] = b.0[k].min(p[k]);
                        b.1[k] = b.1[k].max(p[k]);
                    }
                }
            }
        }
        out
    }
}
