//! One part layer's paths as the live view keeps them: the planned layers' own islands, shared
//! with the plan that made them (never copied), and what makes their preview records. The
//! records are made only while a view reads them (a layer's tile, its walls' steps, a replan's
//! check), then go: a big print's records are gigabytes, its paths a fraction of that.

use crate::live::TreeDisc;
use makepad_fab_gcode::record::{flags, MoveKind, MoveRecord};
use makepad_fab_settings::machine::Job;
use makepad_fab_toolpath::{ExtrusionPath, ObjectLayer, Role};
use std::hash::{Hash, Hasher};
use std::sync::Arc;

/// Which of a planned layer's paths a [`LivePaths`] shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kept {
    All,
    /// Its support paths alone (a model layer's plan holds both).
    Supports,
    /// All but its support paths.
    Model,
}

#[derive(Clone)]
struct Planned {
    index: u32,
    z: f64,
    layer: ObjectLayer,
    nozzle: u8,
    /// The filament's cross-section (mm²): a record's filament length is its bead's volume over it.
    filament_area: f64,
    kept: Kept,
    /// Support records on these discs are left out: trees the person deleted.
    without: Arc<[TreeDisc]>,
}

impl Planned {
    fn keeps(&self, p: &ExtrusionPath) -> bool {
        match self.kept {
            Kept::All => true,
            Kept::Supports => self.layer.support_only || support(p.role),
            Kept::Model => !self.layer.support_only && !support(p.role),
        }
    }

    fn paths(&self) -> impl Iterator<Item = &ExtrusionPath> {
        self.layer.islands.iter().flat_map(|island| island.walls.iter().chain(&island.fills)).filter(|p| self.keeps(p))
    }

    fn segments(&self) -> usize {
        self.paths().map(|p| p.pts.len().saturating_sub(1)).sum()
    }

    /// Its records after `out`'s (times from 0), in the plan's frame. A 3D path (spiral vase,
    /// scarf seam, non-planar top) rises from the layer's z by its per-point offsets, as the
    /// writer emits it, and every record keeps its path's experimental mode.
    fn records_into(&self, out: &mut Vec<MoveRecord>) {
        let (index, z) = (self.index as usize, self.z);
        let zf = z as f32;
        let mut t = 0.0f64;
        for p in self.paths() {
            let speed = p.speed.max(1.0);
            let dz = |i: usize| p.z.get(i).copied().unwrap_or(0.0);
            for (k, w) in p.pts.windows(2).enumerate() {
                let (za, zb) = (z + dz(k), z + dz(k + 1));
                let (p0, p1) = (at(w[0], za), at(w[1], zb));
                // The record's length is that of the endpoints it keeps, and its filament and
                // time follow it: a toolpath segment can be a few nm, shorter than an f32
                // position far out on the bed resolves, and its flow still reads as its path's.
                let len = length(p0, p1);
                if len <= 0.0 {
                    continue;
                }
                let dry = p.dry.get(k).copied().unwrap_or(false);
                let dt = len / speed;
                let sloped = !dry && (zb - za).abs() > 1e-7;
                let record = MoveRecord {
                    t_start: t,
                    t_end: t + dt,
                    kind: if dry { MoveKind::Travel } else { MoveKind::Extrude },
                    role: p.role,
                    p0,
                    p1,
                    nozzle: self.nozzle,
                    tool: self.nozzle,
                    filament: self.layer.filament as u8 + 1,
                    width: p.width as f32,
                    height: p.height as f32,
                    e: if dry { 0.0 } else { (p.area * len / self.filament_area) as f32 },
                    v_cruise: speed as f32,
                    f_cmd: speed as f32,
                    layer: index as u32,
                    z_layer: zf,
                    object: self.layer.object as u16,
                    flags: (if index == 0 { flags::FIRST_LAYER } else { 0 }) | (if sloped { flags::SLOPED } else { 0 }),
                    mode: p.mode,
                    ..Default::default()
                };
                t += dt;
                if !self.without.iter().any(|d| on_disc(&record, d)) {
                    out.push(record);
                }
            }
        }
    }

    /// Whether it makes a record at all: a path with a segment whose kept endpoints differ.
    fn has_record(&self) -> bool {
        self.paths().any(|p| {
            let dz = |i: usize| p.z.get(i).copied().unwrap_or(0.0);
            p.pts.windows(2).enumerate().any(|(k, w)| at(w[0], self.z + dz(k)) != at(w[1], self.z + dz(k + 1)))
        }) && self.without.is_empty()
            || !self.without.is_empty() && {
                let mut out = Vec::new();
                self.records_into(&mut out);
                !out.is_empty()
            }
    }

    /// The same layer at the same place, but for its islands.
    fn same_place(&self, other: &Planned) -> bool {
        self.index == other.index
            && self.z.to_bits() == other.z.to_bits()
            && self.nozzle == other.nozzle
            && self.filament_area.to_bits() == other.filament_area.to_bits()
            && self.kept == other.kept
            && self.without == other.without
            && self.layer.object == other.layer.object
            && self.layer.filament == other.layer.filament
            && self.layer.support_only == other.layer.support_only
    }

    fn same(&self, other: &Planned) -> bool {
        self.same_place(other)
            && self.layer.islands.len() == other.layer.islands.len()
            && self.layer.islands.iter().zip(&other.layer.islands).all(|(a, b)| Arc::ptr_eq(a, b))
    }

    /// The same records without making them: the same layer at the same place, path for path.
    fn same_paths(&self, other: &Planned) -> bool {
        if !self.same_place(other) {
            return false;
        }
        let (mut a, mut b) = (self.paths(), other.paths());
        loop {
            match (a.next(), b.next()) {
                (None, None) => return true,
                (Some(x), Some(y)) if same_path(x, y) => {}
                _ => return false,
            }
        }
    }
}

/// A toolpath point at height `z` as a record keeps it.
fn at(q: makepad_fab_slicer::Pt, z: f64) -> [f32; 3] {
    [q.x_mm() as f32, q.y_mm() as f32, z as f32]
}

/// The length between two record positions, as the checks measure a record.
fn length(a: [f32; 3], b: [f32; 3]) -> f64 {
    ((b[0] - a[0]) as f64).hypot((b[1] - a[1]) as f64).hypot((b[2] - a[2]) as f64)
}

/// Two paths that make the same records: every field [`Planned::records_into`] reads.
fn same_path(a: &ExtrusionPath, b: &ExtrusionPath) -> bool {
    a.role == b.role
        && a.pts == b.pts
        && a.width.to_bits() == b.width.to_bits()
        && a.height.to_bits() == b.height.to_bits()
        && a.area.to_bits() == b.area.to_bits()
        && a.speed.to_bits() == b.speed.to_bits()
        && a.dry == b.dry
        && a.z.len() == b.z.len()
        && a.z.iter().zip(&b.z).all(|(x, y)| x.to_bits() == y.to_bits())
        && a.mode == b.mode
}

fn support(role: Role) -> bool {
    matches!(role, Role::Support | Role::SupportInterface | Role::SupportTransition)
}

/// A record on a tree's disc: its middle within the disc grown by half its bead.
pub fn on_disc(r: &MoveRecord, d: &TreeDisc) -> bool {
    let (x, y) = ((r.p0[0] + r.p1[0]) / 2.0 - d.at[0], (r.p0[1] + r.p1[1]) / 2.0 - d.at[1]);
    x * x + y * y <= (d.r + r.width / 2.0).powi(2)
}

/// One part layer's paths: one or more planned layers at its index (a painted part's filaments,
/// a cached plan's layers), in print order.
#[derive(Clone)]
pub struct LivePaths {
    planned: Vec<Planned>,
    /// What tells these paths from others for the views ([`Self::identity`]): its own, or that
    /// of the paths it makes the same records as ([`Self::keep_identity`]).
    id: u64,
}

impl Default for LivePaths {
    fn default() -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        LivePaths { planned: Vec::new(), id: NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed) }
    }
}

impl LivePaths {
    /// Planned layer `layer`, at index `index` and print z `z`, whole.
    pub fn of(index: usize, z: f64, layer: &ObjectLayer, job: &Job) -> LivePaths {
        let mut paths = LivePaths::default();
        paths.add(index, z, layer, job, Kept::All);
        paths
    }

    /// The paths of `layer` that `kept` names, after these.
    pub fn add(&mut self, index: usize, z: f64, layer: &ObjectLayer, job: &Job, kept: Kept) {
        self.add_with(index, z, layer, job.ext_of(layer.filament) as u8, job.filament_area(layer.filament), kept);
    }

    /// As [`Self::add`], with the nozzle and filament cross-section (mm²) given.
    pub fn add_with(&mut self, index: usize, z: f64, layer: &ObjectLayer, nozzle: u8, filament_area: f64, kept: Kept) {
        self.planned.push(Planned { index: index as u32, z, layer: layer.clone(), nozzle, filament_area, kept, without: Arc::from([]) });
    }

    /// These less the support records on `discs`.
    pub fn without(mut self, discs: &[TreeDisc]) -> LivePaths {
        let discs: Arc<[TreeDisc]> = Arc::from(discs);
        for p in &mut self.planned {
            p.without = discs.clone();
        }
        self
    }

    /// The preview records, made now: each planned layer's from 0 s, in order.
    pub fn records(&self) -> Vec<MoveRecord> {
        let mut out = Vec::with_capacity(self.planned.iter().map(Planned::segments).sum());
        for p in &self.planned {
            p.records_into(&mut out);
        }
        out
    }

    /// The print z of its records (none: it makes none).
    pub fn z(&self) -> Option<f32> {
        self.planned.iter().find(|p| p.has_record()).map(|p| p.z as f32)
    }

    /// Hashes what tells these paths from others: each made apart hashes its own, and paths
    /// that took another's identity hash as those.
    pub fn identity(&self, h: &mut impl Hasher) {
        self.id.hash(h);
    }

    /// These take the identity of `other`, which makes the same records ([`Self::same_paths`]):
    /// a view that shows `other` shows these without building anything, and `other`'s islands
    /// can go while these hold the newer plan's.
    pub fn keep_identity(&mut self, other: &LivePaths) {
        self.id = other.id;
    }

    /// The same planned layers, island for island: their records are the same without making them.
    pub fn same(&self, other: &LivePaths) -> bool {
        self.planned.len() == other.planned.len() && self.planned.iter().zip(&other.planned).all(|(a, b)| a.same(b))
    }

    /// The same records without making them: the same planned layers, else the same paths at
    /// the same place (a plan made again of the same content).
    pub fn same_paths(&self, other: &LivePaths) -> bool {
        self.same(other) || self.planned.len() == other.planned.len() && self.planned.iter().zip(&other.planned).all(|(a, b)| a.same_paths(b))
    }

    /// No planned layer, or none that makes a record.
    pub fn is_empty(&self) -> bool {
        !self.planned.iter().any(Planned::has_record)
    }
}
