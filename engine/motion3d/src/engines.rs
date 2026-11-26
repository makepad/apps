//! The effect pack's procedural 3D engines as motion3d geometry: L-system
//! plants, metaball blobs and flow-field ribbons, built by the same kernels
//! the VJ graph runs (the `lsystem`, `metaballs` and `ribbons` kits, through
//! makepad-content-packs). A frame stays a pure function of `t`: static
//! engines build once, metaballs are evaluated at `t`, and ribbons (a
//! stateful simulation) are re-run from 0 to `t` on a fixed step. With no
//! effect pack installed an engine grows nothing.

use makepad_geom3d::mesh::{self, Mesh};
use makepad_geom3d::fx_mesh::{FxMesh, VERT_FLOATS};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EngineKind {
    /// An L-system plant (vj_fx `lsystem`).
    Tree,
    /// Metaball blobs merging and splitting (vj_fx `metaballs`).
    Metaballs,
    /// Ribbons streaming through a curl flow field (vj_fx `ribbons`).
    Ribbons,
}

impl EngineKind {
    pub const NAMES: &'static [&'static str] = &["tree", "metaballs", "ribbons"];
    pub fn by_name(s: &str) -> Option<Self> {
        Some(match s {
            "tree" | "lsystem" | "plant" => Self::Tree,
            "metaballs" | "blobs" => Self::Metaballs,
            "ribbons" | "flow" => Self::Ribbons,
            _ => return None,
        })
    }
    /// Whether the geometry changes with time.
    pub fn animated(self) -> bool {
        !matches!(self, Self::Tree)
    }
}

/// The dials an author sets (`EngineGeometry{engine seed count size iterations speed}`).
#[derive(Clone, Debug, PartialEq)]
pub struct EngineParams {
    pub kind: EngineKind,
    pub seed: u64,
    /// Tree: plants in a ring; metaballs: blobs; ribbons: ribbons.
    pub count: usize,
    /// Overall scale of the result (the engines' own units are kept).
    pub size: f32,
    /// Tree: L-system iterations.
    pub iterations: usize,
    /// Metaballs: orbit speed; ribbons: flow speed.
    pub speed: f32,
    /// Ribbons: width in engine units.
    pub width: f32,
}

impl Default for EngineParams {
    fn default() -> Self {
        Self { kind: EngineKind::Tree, seed: 1, count: 1, size: 1.0, iterations: 5, speed: 1.0, width: 0.1 }
    }
}

/// Run the effect pack's engine `engine` over `steps` (`(dt, t)`) into `fx`.
fn run(engine: &str, params: &[(&str, f32)], texts: &[(&str, &str)], seed: u32, steps: &[(f32, f32)], fx: &mut FxMesh) {
    let Some(pack) = makepad_content_packs::effects::library() else { return };
    let _ = pack.engine_geometry(engine, params, texts, seed, steps, fx);
}

/// The classic plant the tree kind grows (the lsystem kit's default).
const TREE: &[(&str, &str)] = &[("axiom", "X"), ("rules", "X=F[+X][-X]FX\nF=FF")];

/// The geometry at time `t` (static engines ignore `t`).
pub fn build(p: &EngineParams, t: f32) -> Mesh {
    let mut fx = FxMesh::default();
    let seed = (p.seed as u32).wrapping_add(1);
    match p.kind {
        EngineKind::Tree => {
            let params = [("iterations", p.iterations.clamp(1, 8) as f32), ("copies", p.count.clamp(1, 8) as f32)];
            run("lsystem", &params, TREE, seed, &[(0.0, 0.0)], &mut fx);
        }
        EngineKind::Metaballs => {
            let params = [("blobs", p.count.clamp(2, 12) as f32), ("orbit_speed", 0.7 * p.speed)];
            run("metaballs", &params, &[], seed, &[(0.0, t.max(0.0))], &mut fx);
        }
        EngineKind::Ribbons => {
            let params = [("ribbons", p.count.clamp(2, 96) as f32), ("flow_speed", 2.0 * p.speed), ("width", p.width)];
            // The flow simulation from 0 to t on a fixed step (the first
            // run warms the trails).
            let dt = 1.0 / 60.0;
            let steps = (t.max(0.0) / dt).round() as usize;
            let steps: Vec<(f32, f32)> = (0..=steps).map(|k| (if k == 0 { 0.0 } else { dt }, k as f32 * dt)).collect();
            run("ribbons", &params, &[], seed, &steps, &mut fx);
        }
    }
    convert(&fx, p)
}

/// FxMesh (vj_fx's 12-float stream) to a Mesh. Ribbons carry their centre
/// line and side (a_r1 = ±1) with the width applied in vj_fx's shader;
/// here the side is baked across the tangent and up.
fn convert(fx: &FxMesh, p: &EngineParams) -> Mesh {
    let mut m = Mesh::new();
    let k = p.size;
    let used = fx.idx.iter().copied().max().map(|i| i as usize + 1).unwrap_or(0).min(fx.verts.len() / VERT_FLOATS);
    for v in fx.verts.chunks_exact(VERT_FLOATS).take(used) {
        let mut pos = [v[0], v[1], v[2]];
        let mut n = mesh::normalize_or_up([v[4], v[5], v[6]]);
        if p.kind == EngineKind::Ribbons {
            let tangent = n;
            let side = mesh::normalize_or_up(mesh::cross3(tangent, [0.0, 1.0, 0.0]));
            pos = mesh::add3(pos, mesh::scale3(side, v[11] * p.width * 0.5));
            n = mesh::normalize_or_up(mesh::cross3(side, tangent));
        }
        m.vertex(mesh::scale3(pos, k), n, [v[8], v[9]]);
    }
    for t in fx.idx.chunks_exact(3) {
        // Padding triangles (0, 0, 0) draw nothing; drop them.
        if t[0] == t[1] && t[1] == t[2] {
            continue;
        }
        if (t[0] as usize) < used && (t[1] as usize) < used && (t[2] as usize) < used {
            m.tri(t[0], t[1], t[2]);
        }
    }
    m
}
