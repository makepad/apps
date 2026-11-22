//! Host-generated parts: pass one, cached panel base meshes, generators cached by
//! request key, then pass two. An edit that changes no request reruns no generator.
//! Resolution runs in the evaluator's worker; there is no work per frame.

use crate::eval::Failure;
use crate::program::{self, Compiled, Script};
use makepad_core_util::sha256::Sha256;
use makepad_csg_graph::{Evaluator, TriMesh};
use makepad_csg_math::{dvec3, Vec3d};
use makepad_model_script::lang::{self, GenPart, GenRequest};
use std::collections::HashMap;
use std::sync::Arc;

pub type Generator = Arc<dyn Fn(&GenInput<'_>) -> Result<GenPart, String> + Send + Sync>;

pub struct GenInput<'a> {
    pub kind: &'a str,
    pub part: &'a str,
    pub params: &'a str,
    pub panel: Option<&'a PanelBase>,
    pub inputs: &'a lang::Inputs,
}

/// A panel before host additions and cuts, in model coordinates, millimetres.
#[derive(Clone, Debug)]
pub struct PanelBase {
    pub name: String,
    pub pose: String,
    pub mesh: Arc<TriMesh>,
    bvh: Arc<Bvh>,
}

#[derive(Clone, Copy, Debug)]
pub struct PanelHit {
    /// Distance along the normalized ray to its first face, millimetres.
    pub back: f64,
    /// Distance from that face to the exit, millimetres (zero if already exiting).
    pub thickness: f64,
    pub normal: [f64; 3],
}

impl PanelBase {
    pub fn new(name: String, pose: String, mesh: Arc<TriMesh>) -> Self {
        let bvh = Arc::new(Bvh::new(&mesh));
        Self { name, pose, mesh, bvh }
    }

    /// First face and the solid interval after it. A BVH, built once for this mesh,
    /// visits only triangles whose bounds meet the ray; a shared edge counts once.
    pub fn ray(&self, from: [f64; 3], dir: [f64; 3]) -> Option<PanelHit> {
        if !from.iter().chain(&dir).all(|x| x.is_finite()) { return None; }
        let length = dir.iter().map(|x| x * x).sum::<f64>().sqrt();
        if !length.is_finite() || length == 0.0 { return None; }
        let dir = dir.map(|x| x / length);
        let mut hits = Vec::new();
        self.bvh.hits(&self.mesh, from, dir, &mut hits);
        hits.sort_by(|a, b| a.0.total_cmp(&b.0));
        let mut first = None;
        let mut depth = 0i32;
        let mut i = 0;
        while i < hits.len() {
            let (at, normal) = hits[i];
            let tolerance = 1e-8 * at.abs().max(1.0);
            let (mut enters, mut leaves) = (false, false);
            let mut entry_normal = normal;
            while i < hits.len() && (hits[i].0 - at).abs() <= tolerance {
                let n = hits[i].1;
                if n.dot(dvec3(dir[0], dir[1], dir[2])) < 0.0 {
                    enters = true;
                    entry_normal = n;
                } else {
                    leaves = true;
                }
                i += 1;
            }
            // Coincident entering and leaving faces touch the ray without crossing it.
            if enters == leaves { continue; }
            if first.is_none() {
                if leaves {
                    return Some(PanelHit { back: at, thickness: 0.0, normal: [normal.x, normal.y, normal.z] });
                }
                first = Some((at, entry_normal));
            }
            depth += if enters { 1 } else { -1 };
            if depth == 0 {
                let (back, n) = first?;
                return Some(PanelHit { back, thickness: at - back, normal: [n.x, n.y, n.z] });
            }
        }
        None
    }
}

#[derive(Debug)]
struct Bvh {
    nodes: Vec<BvhNode>,
    triangles: Vec<usize>,
}

#[derive(Debug)]
struct BvhNode {
    min: [f64; 3],
    max: [f64; 3],
    range: std::ops::Range<usize>,
    children: Option<[usize; 2]>,
}

impl Bvh {
    fn new(mesh: &TriMesh) -> Self {
        let mut out = Self { nodes: Vec::new(), triangles: (0..mesh.triangles.len()).collect() };
        if !out.triangles.is_empty() { out.build(mesh, 0..out.triangles.len()); }
        out
    }

    fn build(&mut self, mesh: &TriMesh, range: std::ops::Range<usize>) -> usize {
        let (mut min, mut max) = ([f64::INFINITY; 3], [f64::NEG_INFINITY; 3]);
        for &i in &self.triangles[range.clone()] {
            for &v in &mesh.triangles[i] {
                let v = mesh.vertices[v as usize];
                for (k, x) in [v.x, v.y, v.z].into_iter().enumerate() {
                    min[k] = min[k].min(x);
                    max[k] = max[k].max(x);
                }
            }
        }
        let id = self.nodes.len();
        self.nodes.push(BvhNode { min, max, range: range.clone(), children: None });
        if range.len() > 8 {
            let axis = (0..3).max_by(|&a, &b| (max[a] - min[a]).total_cmp(&(max[b] - min[b]))).unwrap();
            let center = |i: usize| mesh.triangles[i].iter().map(|&v| {
                let p = mesh.vertices[v as usize];
                [p.x, p.y, p.z][axis]
            }).sum::<f64>();
            let mid = range.start + range.len() / 2;
            self.triangles[range.clone()].select_nth_unstable_by(mid - range.start, |&a, &b| center(a).total_cmp(&center(b)));
            let left = self.build(mesh, range.start..mid);
            let right = self.build(mesh, mid..range.end);
            self.nodes[id].children = Some([left, right]);
        }
        id
    }

    fn hits(&self, mesh: &TriMesh, from: [f64; 3], dir: [f64; 3], out: &mut Vec<(f64, Vec3d)>) {
        if self.nodes.is_empty() { return; }
        let origin = dvec3(from[0], from[1], from[2]);
        let direction = dvec3(dir[0], dir[1], dir[2]);
        let mut todo = vec![0];
        while let Some(i) = todo.pop() {
            let node = &self.nodes[i];
            let (mut near, mut far) = (0.0f64, f64::INFINITY);
            for k in 0..3 {
                if dir[k] == 0.0 {
                    if from[k] < node.min[k] || from[k] > node.max[k] { far = -1.0; }
                } else {
                    let a = (node.min[k] - from[k]) / dir[k];
                    let b = (node.max[k] - from[k]) / dir[k];
                    near = near.max(a.min(b));
                    far = far.min(a.max(b));
                }
            }
            if near > far { continue; }
            if let Some(children) = node.children {
                todo.extend(children);
                continue;
            }
            for &t in &self.triangles[node.range.clone()] {
                let [a, b, c] = mesh.triangles[t].map(|v| mesh.vertices[v as usize]);
                let (ab, ac) = (b - a, c - a);
                let p = direction.cross(ac);
                let det = ab.dot(p);
                let normal = ab.cross(ac);
                if det.abs() <= normal.length() * 1e-12 { continue; }
                let q = origin - a;
                let u = q.dot(p) / det;
                if u < -1e-10 || u > 1.0 + 1e-10 { continue; }
                let q = q.cross(ab);
                let v = direction.dot(q) / det;
                if v < -1e-10 || u + v > 1.0 + 1e-10 { continue; }
                let at = ac.dot(q) / det;
                if at >= 0.0 { out.push((at, normal.normalize())); }
            }
        }
    }
}

type Key = [u8; 32];

/// The current and previous evaluation's asks, so edits and undo reuse their work.
#[derive(Default)]
pub(crate) struct Cache {
    round: u64,
    parts: HashMap<Key, (Generator, GenPart, u64)>,
    bases: HashMap<(Key, u64), (Arc<TriMesh>, Arc<Bvh>, u64)>,
    scripts: HashMap<(usize, Key), (Arc<Script>, Arc<Script>, u64)>,
}

pub(crate) fn failure(script: &Script, request: &GenRequest, message: String) -> Failure {
    let (line, col) = script.program.solid(&request.part).and_then(|s| s.loc).unwrap_or((1, 1));
    Failure { lost: None, file: script.file.clone(), line, col, id: request.part.clone(), op: "generated".into(), message }
}

impl Cache {
    pub fn begin(&mut self) {
        self.round += 1;
        let keep = self.round.saturating_sub(1);
        self.parts.retain(|_, (_, _, used)| *used >= keep);
        self.bases.retain(|_, (_, _, used)| *used >= keep);
        self.scripts.retain(|_, (_, _, used)| *used >= keep);
    }

    pub fn resolve(&mut self, c: &Compiled, graph: &mut Evaluator, cancelled: Option<&dyn Fn() -> bool>) -> Result<Compiled, Failure> {
        let mut c = c.clone();
        for inst in &mut c.instances {
            let script = &inst.script;
            if script.program.gen_requests.is_empty() || !script.generated.is_empty() { continue; }
            let mut generated = HashMap::new();
            let mut combined = Sha256::new();
            for r in &script.program.gen_requests {
                let fail = |message| failure(script, r, message);
                if cancelled.is_some_and(|c| c()) { return Err(fail("cancelled".into())); }
                let generator = script.env.generators.get(&r.kind).ok_or_else(|| fail(format!("no generator for '{}'", r.kind)))?;
                let panel = r.panel.as_ref().map(|name| {
                    let panel = script.program.panels.iter().find(|p| &p.name == name).ok_or_else(|| fail(format!("no panel '{name}'")))?;
                    let node = script.program.solids[panel.base].node;
                    let key = script.program.graph.node(node).key;
                    // Queries always use full-resolution base meshes, regardless of view draft.
                    let mesh_key = (key, graph.brep_chord.to_bits());
                    let (mesh, bvh, used) = match self.bases.entry(mesh_key) {
                        std::collections::hash_map::Entry::Occupied(e) => e.into_mut(),
                        std::collections::hash_map::Entry::Vacant(e) => {
                            let mesh = graph.mesh(&script.program.graph, node, cancelled).map_err(|e| fail(e.message))?;
                            let bvh = Arc::new(Bvh::new(&mesh));
                            e.insert((mesh, bvh, self.round))
                        }
                    };
                    *used = self.round;
                    Ok::<_, Failure>((key, PanelBase { name: name.clone(), pose: panel.pose.splash().map_err(&fail)?, mesh: mesh.clone(), bvh: bvh.clone() }))
                }).transpose()?;
                let mut h = Sha256::new();
                let mut text = |s: &str| { h.update(&(s.len() as u64).to_le_bytes()); h.update(s.as_bytes()); };
                text(&r.kind);
                text(&r.part);
                text(&r.params);
                if let Some((_, p)) = &panel { text(&p.name); text(&p.pose); }
                drop(text);
                h.update(&(Arc::as_ptr(generator) as *const () as usize).to_le_bytes());
                if let Some((key, _)) = &panel {
                    h.update(key);
                    h.update(&graph.brep_chord.to_bits().to_le_bytes());
                }
                let mut reads = script.program.inputs_read.iter().collect::<Vec<_>>();
                reads.sort();
                for (kind, name) in reads {
                    h.update(&(kind.len() as u64).to_le_bytes());
                    h.update(kind.as_bytes());
                    h.update(&(name.len() as u64).to_le_bytes());
                    h.update(name.as_bytes());
                    match script.env.inputs.stamp(&(kind.clone(), name.clone())) {
                        Some(stamp) => { h.update(&[1]); h.update(&stamp); }
                        None => h.update(&[0]),
                    }
                }
                let key = h.finish();
                combined.update(&key);
                let (_, part, used) = match self.parts.entry(key) {
                    std::collections::hash_map::Entry::Occupied(e) => e.into_mut(),
                    std::collections::hash_map::Entry::Vacant(e) => {
                        let part = generator(&GenInput { kind: &r.kind, part: &r.part, params: &r.params, panel: panel.as_ref().map(|(_, p)| p), inputs: &script.env.inputs }).map_err(&fail)?;
                        if cancelled.is_some_and(|c| c()) { return Err(fail("cancelled".into())); }
                        e.insert((generator.clone(), part, self.round))
                    }
                };
                *used = self.round;
                generated.insert(r.part.clone(), part.clone());
            }
            let key = combined.finish();
            let compile_key = (Arc::as_ptr(script) as usize, key);
            let (_, complete, used) = match self.scripts.entry(compile_key) {
                std::collections::hash_map::Entry::Occupied(e) => e.into_mut(),
                std::collections::hash_map::Entry::Vacant(e) => {
                    let mut complete = program::compile_generated(&script.file, script.source(), Some(script.program.names.clone()), &script.assets, &script.facets, &script.pictures, &script.context, &script.env, script.overrides.clone(), &script.solved, &script.skip, &generated, &|f| script.paint_files.get(f).cloned()).map_err(|d| crate::first(&script.file, d))?;
                    complete.generated_key = key;
                    if cancelled.is_some_and(|c| c()) {
                        return Err(failure(script, &script.program.gen_requests[0], "cancelled".into()));
                    }
                    e.insert((script.clone(), Arc::new(complete), self.round))
                }
            };
            *used = self.round;
            inst.script = complete.clone();
        }
        Ok(c)
    }
}
