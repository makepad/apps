//! Solder on one outer copper face. Fields, proxies and meshes use joint-local millimetres.
use fab_elec_core::{poly::ring_area2, to_mm, PolySet, Side, Xform3, P};
use fab_elec_mech::mesh::{BodyMesh, BODY_VERTEX_FLOATS};
use fab_elec_mech::scene::{ElecScene, MeshRef, SceneBoard, SceneBody, SceneDrill, ScenePad, SolderPadKind};
use fab_elec_parts::footprint::PAD_POLY_ERR;
use makepad_csg_math::{BBox3d, Mat4d, Vec3d};
use makepad_csg_sdf::{
    sketches::{Outline, P2}, surface_mesh::mesh_surface, Clearance, Sdf3, SdfBox, SdfCylinder,
};
use makepad_math::Mat4f;
use makepad_widgets_core::log;
use makepad_widgets_core::makepad_platform::thread::checkpoint;
use std::{collections::{HashMap, HashSet}, sync::{atomic::{AtomicU64, Ordering}, Arc, LazyLock, Mutex, OnceLock}, time::Instant};

pub const SOLDER_BASE: [f32; 3] = [0.92, 0.92, 0.90];
pub const SOLDER_ROUGHNESS: f32 = 0.08;
pub const SOLDER_METAL: f32 = 1.0;

const PROXY_MARGIN: f64 = 0.10;
const LEAD_INSET: f64 = 0.01;
const FOOT_SEPARATION: f64 = 0.05;
const GRID: usize = 32;
const VOLUME_TOLERANCE: f64 = 0.02;

type Regions = Vec<Vec<Vec<P2>>>;

fn pad_pose(pad: &ScenePad, thickness: f64) -> Mat4d {
    let (s, c) = pad.rot.sin_cos();
    let up = if pad.side == Side::Top { 1.0 } else { -1.0 };
    let z = if pad.side == Side::Top { thickness } else { 0.0 };
    Mat4d { v: [
        c, s, 0.0, 0.0, -up * s, up * c, 0.0, 0.0, 0.0, 0.0, up, 0.0,
        to_mm(pad.centre.x), to_mm(pad.centre.y), z, 1.0,
    ] }
}

fn local_xy(p: P, from_board: &Mat4d) -> P2 {
    let q = from_board.transform_point(Vec3d::new(to_mm(p.x), to_mm(p.y), 0.0));
    [q.x, q.y]
}

fn regions(pad: &ScenePad, from_board: &Mat4d) -> Regions {
    let mut regions = Regions::new();
    for contour in std::iter::once(&pad.outline).chain(&pad.other_rings) {
        let ring = contour.to_ring(PAD_POLY_ERR);
        if ring_area2(&ring) > 0 { regions.push(Vec::new()); }
        let mut points: Vec<_> = ring.into_iter().map(|p| local_xy(p, from_board)).collect();
        if pad.side == Side::Bottom { points.reverse(); }
        if let Some(region) = regions.last_mut() { region.push(points); }
    }
    regions
}

/// A round drill or slot, as a centre segment and radius.
#[derive(Clone, Copy)]
struct Bore {
    a: P2,
    b: P2,
    radius: f64,
}

impl Bore {
    fn new(drill: &SceneDrill, from_board: &Mat4d) -> Self {
        let c = local_xy(drill.c, from_board);
        let (s, co) = drill.rot.sin_cos();
        let long = if drill.size.x >= drill.size.y { Vec3d::new(co, s, 0.0) } else { Vec3d::new(-s, co, 0.0) };
        let half = (to_mm(drill.size.x) - to_mm(drill.size.y)).abs() * 0.5;
        let axis = from_board.transform_vector(long) * half;
        Self { a: [c[0] - axis.x, c[1] - axis.y], b: [c[0] + axis.x, c[1] + axis.y], radius: to_mm(drill.width()) * 0.5 }
    }

    fn centre(self) -> P2 {
        [(self.a[0] + self.b[0]) * 0.5, (self.a[1] + self.b[1]) * 0.5]
    }

    fn distance(self, p: P2) -> f64 {
        makepad_csg_sdf::strokes::capsule([p[0], p[1], 0.0],
            [self.a[0], self.a[1], 0.0], [self.b[0], self.b[1], 0.0]) - self.radius
    }
}

#[derive(Clone, Copy, Debug)]
pub enum LeadProxy {
    None,
    Foot { bounds: BBox3d, thickness: f64 },
    Chip { bounds: BBox3d, height: f64 },
    Pin { radius: f64, z0: f64, z1: f64 },
}

/// Clip in XY only. Every intersecting face contributes its full Z range.
fn prism_box(tri: [Vec3d; 3], prism: BBox3d) -> Option<BBox3d> {
    let [a, b, c] = tri;
    let bounds = BBox3d::from_triangle(a, b, c);
    if !bounds.intersects(prism) { return None; }
    Some(BBox3d::from_min_max(
        Vec3d::new(bounds.min.x.max(prism.min.x), bounds.min.y.max(prism.min.y), bounds.min.z),
        Vec3d::new(bounds.max.x.min(prism.max.x), bounds.max.y.min(prism.max.y), bounds.max.z)))
}

fn fit_pin(outline: &Outline, bore: Bore, metal: &[[Vec3d; 3]], from_board: &Mat4d, thickness: f64) -> LeadProxy {
    let [x, y] = bore.centre();
    let (mut z0, mut z1) = (f64::INFINITY, f64::NEG_INFINITY);
    let mut radius2: f64 = 0.0;
    for &vertex in metal.iter().flatten() {
        let p = from_board.transform_point(vertex);
        if !(-thickness..=0.6).contains(&p.z) { continue; }
        let r2 = (p.x - x).powi(2) + (p.y - y).powi(2);
        if r2 > bore.radius * bore.radius || outline.distance([p.x, p.y]) > PROXY_MARGIN { continue; }
        radius2 = radius2.max(r2);
        z0 = z0.min(p.z);
        z1 = z1.max(p.z);
    }
    if z0 <= z1 { LeadProxy::Pin { radius: radius2.sqrt(), z0, z1 } } else { LeadProxy::None }
}

fn fit_proxy(kind: SolderPadKind, outline: &Outline, bore: Option<Bore>, metal: &[[Vec3d; 3]],
    other: &[[Vec3d; 3]], from_board: &Mat4d, thickness: f64) -> LeadProxy {
    if kind == SolderPadKind::Thru {
        return bore.map_or(LeadProxy::None, |b| fit_pin(outline, b, metal, from_board, thickness));
    }
    let prism = BBox3d::from_min_max(
        Vec3d::new(outline.min[0] - PROXY_MARGIN, outline.min[1] - PROXY_MARGIN, -0.01),
        Vec3d::new(outline.max[0] + PROXY_MARGIN, outline.max[1] + PROXY_MARGIN, f64::INFINITY));
    let mut bounds = BBox3d::empty();
    let mut faces = Vec::new();
    for tri in metal {
        let [a, b, c] = tri.map(|p| from_board.transform_point(p));
        if let Some(face) = prism_box([a, b, c], prism) {
            bounds = bounds.union(face);
            let n = (b - a).cross(c - a);
            faces.push((face, n.z > 0.9 * n.length()));
        }
    }
    if faces.is_empty() { return LeadProxy::None; }

    // Use the high vertex of a nearly horizontal face so all three vertices fit under its top.
    let foot_top = faces.iter()
        .filter(|(face, up)| *up && face.min.z >= bounds.min.z + FOOT_SEPARATION)
        .map(|(face, _)| face.max.z).fold(f64::INFINITY, f64::min);
    if foot_top < bounds.max.z - FOOT_SEPARATION {
        let foot = faces.iter().filter(|(face, _)| face.max.z <= foot_top + 0.01)
            .fold(BBox3d::empty(), |foot, (face, _)| foot.union(*face));
        return LeadProxy::Foot { bounds: foot, thickness: foot_top - bounds.min.z };
    }

    // The chip height is measured from copper to the highest face of any material over this pad.
    let height = other.iter().filter_map(|tri| prism_box(tri.map(|p| from_board.transform_point(p)), prism))
        .fold(bounds.max.z, |top, face| top.max(face.max.z));
    LeadProxy::Chip { bounds, height }
}

/// Fit an unclassified body, using all of its triangles as potential contacts.
pub fn lead_proxy(pad: &ScenePad, tris_board: &[[Vec3d; 3]], board_to_joint: &Mat4d, thickness_mm: f64) -> LeadProxy {
    let Ok(outline) = Outline::new(&regions(pad, board_to_joint)) else { return LeadProxy::None };
    let bore = pad.drill.as_ref().map(|d| Bore::new(d, board_to_joint));
    fit_proxy(pad.kind, &outline, bore, tris_board, &[], board_to_joint, thickness_mm)
}

enum Lead {
    None,
    Foot(SdfBox, f64),
    Chip(SdfBox, f64),
    Pin(SdfCylinder),
}

/// A vertical section through an extruded box or pin. XY work is shared by all volume samples.
#[derive(Clone, Copy)]
struct LeadColumn { lateral: f64, z0: f64, z1: f64 }

impl LeadColumn {
    fn distance(self, z: f64) -> f64 {
        let dz = (self.z0 - z).max(z - self.z1);
        if dz > 0.0 && self.lateral > 0.0 {
            (dz * dz + self.lateral * self.lateral).sqrt()
        } else {
            self.lateral.max(dz)
        }
    }
}

impl Lead {
    fn new(proxy: LeadProxy, bore: Option<Bore>) -> Self {
        let box_sdf = |b: BBox3d| SdfBox::new(b.center(), b.size());
        match proxy {
            LeadProxy::None => Self::None,
            LeadProxy::Foot { bounds, thickness } => Self::Foot(box_sdf(bounds), thickness),
            LeadProxy::Chip { bounds, height } => Self::Chip(box_sdf(bounds), height),
            LeadProxy::Pin { radius, z0, z1 } => {
                let [x, y] = bore.map_or([0.0; 2], Bore::centre);
                Self::Pin(SdfCylinder::new(Vec3d::new(x, (z0 + z1) * 0.5, y), radius, z1 - z0))
            }
        }
    }

    fn column(&self, [x, y]: P2) -> Option<LeadColumn> {
        match self {
            Self::None => None,
            Self::Foot(b, _) | Self::Chip(b, _) => {
                let dx = (x - b.center.x).abs() - b.half_size.x;
                let dy = (y - b.center.y).abs() - b.half_size.y;
                let lateral = if dx > 0.0 && dy > 0.0 { (dx * dx + dy * dy).sqrt() } else { dx.max(dy) };
                Some(LeadColumn { lateral, z0: b.center.z - b.half_size.z, z1: b.center.z + b.half_size.z })
            }
            Self::Pin(c) => Some(LeadColumn { lateral: (x - c.center.x).hypot(y - c.center.z) - c.radius,
                z0: c.center.y - c.half_height, z1: c.center.y + c.half_height }),
        }
    }

    fn has_core(&self) -> bool {
        match self {
            Self::None => false,
            Self::Foot(b, _) | Self::Chip(b, _) => b.half_size.x.min(b.half_size.y).min(b.half_size.z) > LEAD_INSET,
            Self::Pin(c) => c.radius.min(c.half_height) > LEAD_INSET,
        }
    }

    // Linear edge crossings and QEF vertices can cut a corner of the subtracted solid.
    // Move those vertices to its closest face, leaving room for conversion to f32.
    fn outside_core(&self, p: Vec3d) -> Vec3d {
        let inset = LEAD_INSET - 1e-6;
        match self {
            Self::None => p,
            Self::Foot(b, _) | Self::Chip(b, _) => {
                let offset = p - b.center;
                let depth = b.half_size - offset.abs() - Vec3d::all(inset);
                let d = depth.x.min(depth.y).min(depth.z);
                if d <= 0.0 { return p; }
                if d == depth.x { p + Vec3d::new(d.copysign(offset.x), 0.0, 0.0) }
                else if d == depth.y { p + Vec3d::new(0.0, d.copysign(offset.y), 0.0) }
                else { p + Vec3d::new(0.0, 0.0, d.copysign(offset.z)) }
            }
            Self::Pin(c) => {
                let dx = p.x - c.center.x;
                let dy = p.y - c.center.z;
                let r = dx.hypot(dy);
                let side = c.radius - inset - r;
                let end = c.half_height - inset - (p.z - c.center.y).abs();
                if side.min(end) <= 0.0 { return p; }
                if end <= side { p + Vec3d::new(0.0, 0.0, end.copysign(p.z - c.center.y)) }
                else if r == 0.0 { p + Vec3d::new(side, 0.0, 0.0) }
                else { p + Vec3d::new(dx, dy, 0.0) * (side / r) }
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum JointKind { Smd, Dome, ThruSolder, ThruComponent }

struct JointBase {
    outline: Outline,
    bore: Option<Bore>,
    lead: Lead,
    kind: JointKind,
}

impl JointBase {
    fn wet_distance(&self, xy: P2, copper: f64) -> f64 {
        self.bore.map_or(copper, |b| copper.max(-b.distance(xy)))
    }

    fn smaller_width(&self) -> f64 {
        (self.outline.max[0] - self.outline.min[0]).min(self.outline.max[1] - self.outline.min[1])
    }

    fn field(self: &Arc<Self>, t: f64) -> JointField {
        let k = match self.kind {
            JointKind::Dome => 0.0,
            JointKind::ThruComponent => 0.1,
            JointKind::Smd | JointKind::ThruSolder => 2.0 * t,
        };
        let cap = match &self.lead {
            Lead::Foot(_, thickness) => t + 0.5 * thickness,
            Lead::Chip(_, height) => t.max(height / 3.0),
            Lead::Pin(_) | Lead::None => t + self.bore.map_or(0.0, |b| (self.smaller_width() * 0.5 - b.radius).max(0.0)),
        };
        JointField { base: self.clone(), t, k, cap, subtract_lead: self.lead.has_core() }
    }
}

struct JointField {
    base: Arc<JointBase>,
    t: f64,
    k: f64,
    cap: f64,
    subtract_lead: bool,
}

/// Polynomial smooth minimum, exact outside the blend band (also for k = 0).
fn smooth_min(a: f64, b: f64, k: f64) -> f64 {
    let h = k - (a - b).abs();
    if h > 0.0 { a.min(b) - 0.25 * h * h / k } else { a.min(b) }
}

impl JointField {
    fn film_inset2(&self, wet: f64) -> f64 {
        (wet + self.t).max(0.0).powi(2)
    }

    /// The outer surface, shared with the volume integral before the lead is deducted.
    fn envelope(&self, z: f64, inset2: f64, copper: f64, lead: Option<f64>) -> f64 {
        let film = (inset2 + z * z).sqrt() - self.t;
        let Some(lead) = lead else { return film };
        let union = smooth_min(film, lead, self.k);
        // Taper the added fillet at 45 degrees into the existing round film toe. At the
        // outer copper edge both reach zero, even where a fitted contact extends past it.
        let edge = (z + copper) * std::f64::consts::FRAC_1_SQRT_2;
        let tapered = union.max(edge);
        let capped = -smooth_min(-tapered, self.cap - z, 0.5 * self.t);
        film.min(capped)
    }

    fn top(&self) -> f64 { self.cap }

    fn bounds(&self) -> BBox3d {
        let outline = &self.base.outline;
        BBox3d::from_min_max(Vec3d::new(outline.min[0], outline.min[1], 0.0),
            Vec3d::new(outline.max[0], outline.max[1], self.top()))
    }

    fn mesh_cell(&self) -> f64 {
        (self.base.smaller_width() / 8.0).clamp(0.03, 0.08).min(1.5 * self.t)
    }
}

impl Sdf3 for JointField {
    fn distance(&self, p: Vec3d) -> f64 {
        let xy = [p.x, p.y];
        let copper = self.base.outline.distance(xy);
        let wet = self.base.wet_distance(xy, copper);
        let lead = self.base.lead.column(xy).map(|c| c.distance(p.z));
        let outer = self.envelope(p.z, self.film_inset2(wet), copper, lead);
        // Offsetting the plain lead inward puts the seam under the visible metal.
        let core_cut = lead.filter(|_| self.subtract_lead).map_or(outer, |d| outer.max(-d - LEAD_INSET));
        core_cut.max(-p.z)
    }
}

struct SampleColumn {
    copper: f64,
    wet: f64,
    lead: Option<LeadColumn>,
}

struct VolumeGrid {
    columns: Vec<SampleColumn>,
    cell_area: f64,
    dz: f64,
}

impl VolumeGrid {
    /// Keep all 32³ cells fixed throughout bisection, including the XY samples of the copper.
    fn new(base: &JointBase, bounds: BBox3d) -> Self {
        let step = (bounds.max - bounds.min) * (1.0 / GRID as f64);
        let mut columns = Vec::with_capacity(GRID * GRID);
        for y in 0..GRID {
            for x in 0..GRID {
                let xy = [bounds.min.x + (x as f64 + 0.5) * step.x, bounds.min.y + (y as f64 + 0.5) * step.y];
                let copper = base.outline.distance(xy);
                if copper <= 0.0 {
                    columns.push(SampleColumn { copper, wet: base.wet_distance(xy, copper), lead: base.lead.column(xy) });
                }
            }
        }
        Self { columns, cell_area: step.x * step.y, dz: step.z }
    }
}

/// Solder in mm³. Interpolate each vertical zero crossing so a flat film top can fill part of a cell.
fn solder_volume(field: &JointField, grid: &VolumeGrid) -> f64 {
    let top = field.top();
    let mut height = 0.0;
    for sample in &grid.columns {
        let inset2 = field.film_inset2(sample.wet);
        // Away from the contact the rounded film has an analytic height.
        if sample.lead.is_none_or(|c| c.lateral > 0.0 && c.lateral >= field.k) {
            height += (field.t * field.t - inset2).max(0.0).sqrt();
            continue;
        }
        // Exclude the plain metal span. The 0.01 mm overlap only hides the rendered seam.
        let occupied = sample.lead.filter(|c| c.lateral <= 0.0);
        if occupied.is_some_and(|c| c.z0 <= 0.0 && c.z1 >= top) { continue; }
        let distance = |z| field.envelope(z, inset2, sample.copper, sample.lead.map(|c| c.distance(z)));
        let mut prev = distance(0.0);
        for iz in 0..GRID {
            let z = iz as f64 * grid.dz;
            if z >= top { break; }
            let end = ((iz + 1) as f64 * grid.dz).min(top);
            let next = distance(end);
            if prev < 0.0 || next < 0.0 {
                let mut lo = z;
                let mut hi = end;
                if (prev < 0.0) != (next < 0.0) {
                    let cross = z + (end - z) * prev / (prev - next);
                    if prev < 0.0 { hi = cross; } else { lo = cross; }
                }
                if hi > lo {
                    let lead = occupied.map_or(0.0, |c| (hi.min(c.z1) - lo.max(c.z0)).max(0.0));
                    height += hi - lo - lead;
                }
            }
            prev = next;
        }
    }
    height * grid.cell_area
}

struct Fit {
    field: JointField,
    volume: f64,
    limited: bool,
}

fn fit_volume(base: &Arc<JointBase>, target: Option<f64>) -> Fit {
    let hi = target.map_or(0.03, |_| base.smaller_width() * 0.5);
    let field = base.field(hi);
    let grid = VolumeGrid::new(base, field.bounds());
    let volume = solder_volume(&field, &grid);
    let Some(target) = target else { return Fit { field, volume, limited: false } };
    if target <= 0.0 { return Fit { field: base.field(0.0), volume: 0.0, limited: false }; }
    let mut best = Fit { field, volume, limited: volume < target };
    if volume < target || (volume - target).abs() <= VOLUME_TOLERANCE * target { return best; }
    // Volume rises smoothly and monotonically with t from none at t = 0, so the Illinois
    // false-position step meets the tolerance in a handful of volume evaluations.
    let (mut lo, mut f_lo, mut hi, mut f_hi) = (0.0, -target, hi, volume - target);
    let mut last = 0;
    for _ in 0..24 {
        let t = (lo * f_hi - hi * f_lo) / (f_hi - f_lo);
        let field = base.field(t);
        let volume = solder_volume(&field, &grid);
        let f = volume - target;
        if f.abs() < (best.volume - target).abs() {
            best = Fit { field, volume, limited: false };
        }
        if f.abs() <= VOLUME_TOLERANCE * target { break; }
        if f < 0.0 {
            (lo, f_lo) = (t, f);
            if last < 0 { f_hi *= 0.5; }
            last = -1;
        } else {
            (hi, f_hi) = (t, f);
            if last > 0 { f_lo *= 0.5; }
            last = 1;
        }
    }
    best
}

fn joint_mesh(field: JointField) -> BodyMesh {
    // A film thinner than half a cell would fall between the sample planes and vanish.
    let cell = field.mesh_cell();
    // Sample planes at odd half cells above the copper, so every film over half a cell is caught.
    let mut joint_box = field.bounds();
    joint_box.min.z -= 0.5 * cell;
    let field = Arc::new(field);
    let mesh = mesh_surface(field.clone(), joint_box.min, joint_box.max, cell, Clearance::uniform(1.0));
    let mut out = BodyMesh::default();
    let mut remap = vec![u32::MAX; mesh.vertices.len()];
    // Both proxies are convex. A face with all three corners in the metal is fully hidden,
    // including a straight side where the smooth union meets the plain lead exactly.
    let in_metal: Vec<_> = mesh.vertices.iter().map(|p| {
        field.subtract_lead && field.base.lead.column([p.x, p.y])
            .is_some_and(|c| c.lateral <= 0.0 && (c.z0..=c.z1).contains(&p.z))
    }).collect();
    let h = cell * 0.25;
    for tri in &mesh.triangles {
        if tri.iter().all(|&i| in_metal[i as usize]) { continue; }
        // The face on the copper (where the clip at z = 0 wins) is never seen: outward winding
        // makes it face down, within half a cell of the copper.
        let [a, b, c] = tri.map(|i| mesh.vertices[i as usize]);
        let n = (b - a).cross(c - a);
        if n.z < -0.7 * n.length() && a.z.max(b.z).max(c.z) < 0.5 * cell { continue; }
        for &i in tri {
            let index = &mut remap[i as usize];
            if *index == u32::MAX {
                let p = mesh.vertices[i as usize];
                let p = if in_metal[i as usize] { field.base.lead.outside_core(p) } else { p };
                let diff = |axis| field.distance(p + axis) - field.distance(p - axis);
                let n = Vec3d::new(diff(Vec3d::new(h, 0.0, 0.0)), diff(Vec3d::new(0.0, h, 0.0)), diff(Vec3d::new(0.0, 0.0, h))).normalize();
                *index = (out.vertices.len() / BODY_VERTEX_FLOATS) as u32;
                out.vertices.extend([p.x, p.y, p.z, 0.0, n.x, n.y, n.z, 0.0].map(|v| v as f32));
            }
            out.indices.push(*index);
        }
    }
    out
}

pub struct JointMesh {
    pub mesh: BodyMesh,
    /// Solder outside the proxy and above the copper, in mm³.
    pub achieved_volume: f64,
    /// None for the component side of a through joint, which has a fixed film thickness.
    pub target_volume: Option<f64>,
}

#[derive(Hash, PartialEq, Eq)]
enum ProxyKey { None, Foot([i64; 7]), Chip([i64; 7]), Pin([i64; 3]) }

#[derive(Hash, PartialEq, Eq)]
struct JointKey {
    kind: JointKind,
    side: Side,
    regions: Vec<Vec<Vec<[i64; 2]>>>,
    bore: Option<([[i64; 2]; 2], i64)>,
    proxy: ProxyKey,
    target: Option<i64>,
}

fn micron(mm: f64) -> i64 { (mm * 1000.0).round() as i64 }

impl JointKey {
    fn new(kind: JointKind, side: Side, regions: &Regions, bore: Option<Bore>, proxy: LeadProxy, target: Option<f64>) -> Self {
        // Polygon sets start rings in board order. Moving the pad can change the first vertex.
        let mut regions: Vec<Vec<_>> = regions.iter().map(|region| {
            let mut rings: Vec<Vec<_>> = region.iter().map(|ring| {
                let mut points: Vec<_> = ring.iter().map(|p| p.map(micron)).collect();
                if let Some((i, _)) = points.iter().enumerate().min_by_key(|(_, p)| **p) {
                    points.rotate_left(i);
                }
                points
            }).collect();
            rings[1..].sort_unstable();
            rings
        }).collect();
        regions.sort_unstable();
        let bore = bore.map(|b| {
            let mut ends = [b.a.map(micron), b.b.map(micron)];
            ends.sort_unstable();
            (ends, micron(b.radius))
        });
        let box_key = |b: BBox3d, height| [b.min.x, b.min.y, b.min.z, b.max.x, b.max.y, b.max.z, height].map(micron);
        let proxy = match proxy {
            LeadProxy::None => ProxyKey::None,
            LeadProxy::Foot { bounds, thickness } => ProxyKey::Foot(box_key(bounds, thickness)),
            LeadProxy::Chip { bounds, height } => ProxyKey::Chip(box_key(bounds, height)),
            LeadProxy::Pin { radius, z0, z1 } => ProxyKey::Pin([radius, z0, z1].map(micron)),
        };
        // Consecutive nonzero volume bins differ by one part in a thousand.
        let target = target.map(|v| if v > 0.0 { (v.ln() / 0.001_f64.ln_1p()).round() as i64 } else { i64::MIN });
        Self { kind, side, regions, bore, proxy, target }
    }
}

static JOINTS: LazyLock<Mutex<HashMap<JointKey, (Arc<OnceLock<Arc<JointMesh>>>, u64)>>> = LazyLock::new(|| Mutex::new(HashMap::new()));
static BUILD_SERIAL: AtomicU64 = AtomicU64::new(0);

fn target_volume(pad: &ScenePad, kind: JointKind) -> Option<f64> {
    match kind {
        JointKind::Smd | JointKind::Dome => Some(pad.paste_area * 1e-12 * 0.12 * 0.5),
        JointKind::ThruComponent => None,
        JointKind::ThruSolder => {
            let rings: Vec<_> = std::iter::once(&pad.outline).chain(&pad.other_rings)
                .map(|c| c.to_ring(PAD_POLY_ERR)).collect();
            let copper = PolySet::from_rings_even_odd(&rings);
            // Only the part of an offset hole or slot that overlaps copper reduces the annulus.
            let area = pad.drill.as_ref().map_or_else(|| copper.area(), |d| {
                copper.difference(&d.shape().to_polyset(PAD_POLY_ERR)).area()
            });
            Some(area * 1e-12 * 0.15)
        }
    }
}

pub struct SolderJoint {
    pub mesh: Arc<JointMesh>,
    /// Index in SceneBoard::bodies: joints follow their component's drags and live poses.
    pub body: usize,
    /// Joint-local mm to the body's own frame.
    pub body_from_joint: Mat4f,
    /// SOLDER_ROUGHNESS plus a position-hashed step.
    pub roughness: f32,
}

/// The same basis and nm-to-mm translation as bodies::xform_mat, kept in f64 for inversion.
fn body_pose64(x: &Xform3) -> Mat4d {
    let a = x.rotate([1.0, 0.0, 0.0]);
    let b = x.rotate([0.0, 1.0, 0.0]);
    let c = x.rotate([0.0, 0.0, 1.0]);
    Mat4d { v: [
        a[0], a[1], a[2], 0.0, b[0], b[1], b[2], 0.0, c[0], c[1], c[2], 0.0,
        to_mm(x.t[0]), to_mm(x.t[1]), to_mm(x.t[2]), 1.0,
    ] }
}

/// Board-space triangles, partitioned once per body. Other materials supply only the chip height.
#[derive(Default)]
pub struct BodyTriangles {
    contacts: Vec<[Vec3d; 3]>,
    other: Vec<[Vec3d; 3]>,
}

impl From<Vec<[Vec3d; 3]>> for BodyTriangles {
    fn from(contacts: Vec<[Vec3d; 3]>) -> Self { Self { contacts, other: Vec::new() } }
}

/// Already-prepared primitives, including actuators at rest, transformed into board mm.
pub(super) fn body_triangles<'a>(body: &SceneBody, meshes: impl Iterator<Item = (&'a BodyMesh, bool)>) -> BodyTriangles {
    let pose = Mat4d { v: super::bodies::xform_mat(&body.pose).v.map(f64::from) };
    let mut tris = BodyTriangles::default();
    for (mesh, metal) in meshes {
        let dest = if metal { &mut tris.contacts } else { &mut tris.other };
        let points: Vec<_> = mesh.vertices.chunks_exact(BODY_VERTEX_FLOATS).map(|p| {
            pose.transform_point(Vec3d::new(p[0] as f64, p[1] as f64, p[2] as f64))
        }).collect();
        dest.extend(mesh.indices.chunks_exact(3).map(|tri| {
            [tri[0], tri[1], tri[2]].map(|i| points[i as usize])
        }));
    }
    // A model that states no metal may carry its leads anywhere, so all of it may touch the solder.
    // A generated block (a part without a model, or its courtyard extruded) has no leads: taken
    // for one, it covers its pads from the copper up and leaves the paste no room.
    if tris.contacts.is_empty() && matches!(body.mesh, MeshRef::Mesh(_) | MeshRef::Script(_)) {
        std::mem::swap(&mut tris.contacts, &mut tris.other);
    }
    tris
}

fn roughness(centre: P) -> f32 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in centre.x.to_le_bytes().into_iter().chain(centre.y.to_le_bytes()) {
        hash = (hash ^ byte as u64).wrapping_mul(0x100_0000_01b3);
    }
    [-0.02, -0.01, 0.0, 0.01, 0.02][(hash % 5) as usize] + SOLDER_ROUGHNESS
}

/// One list per board, in scene order. None when the pool task is cancelled.
pub fn scene_joints<T: Into<BodyTriangles>>(scene: &ElecScene, body_tris: &mut dyn FnMut(usize, usize) -> T) -> Option<Vec<Vec<SolderJoint>>> {
    let serial = BUILD_SERIAL.fetch_add(1, Ordering::Relaxed) + 1;
    let mut result = Vec::with_capacity(scene.boards.len());
    for (board_index, board) in scene.boards.iter().enumerate() {
        let start = Instant::now();
        let thickness_mm = to_mm(board.thickness);
        let mut bodies = HashMap::new();
        for (i, body) in board.bodies.iter().enumerate() { bodies.entry(&body.sel).or_insert(i); }
        let mut triangles = HashMap::<usize, BodyTriangles>::new();
        let mut distinct = HashSet::new();
        let mut joints = Vec::new();
        let mut drawn_triangles = 0;
        let mut worst: f64 = 0.0;
        for pad in &board.pads {
            checkpoint().ok()?;
            if pad.kind == SolderPadKind::Smd && pad.paste_area == 0.0 { continue; }
            let Some(&body) = bodies.get(&pad.sel) else { continue };
            let pose = pad_pose(pad, thickness_mm);
            let Some(inverse) = pose.invert() else { continue };
            let Some(body_inverse) = body_pose64(&board.bodies[body].pose).invert() else { continue };
            let body_from_joint = Mat4d::mul(&body_inverse, &pose);
            let regions = regions(pad, &inverse);
            let Ok(outline) = Outline::new(&regions) else { continue };
            let bore = pad.drill.as_ref().map(|d| Bore::new(d, &inverse));
            let tris = triangles.entry(body).or_insert_with(|| body_tris(board_index, body).into());
            let proxy = fit_proxy(pad.kind, &outline, bore, &tris.contacts, &tris.other, &inverse, thickness_mm);
            let kind = match pad.kind {
                SolderPadKind::Smd if matches!(proxy, LeadProxy::None) => JointKind::Dome,
                SolderPadKind::Smd => JointKind::Smd,
                SolderPadKind::Thru if pad.side == pad.component_side => JointKind::ThruComponent,
                SolderPadKind::Thru => JointKind::ThruSolder,
            };
            let target = target_volume(pad, kind);
            if target == Some(0.0) { continue; }
            let key = JointKey::new(kind, pad.side, &regions, bore, proxy, target);
            let cell = {
                let mut cache = JOINTS.lock().unwrap_or_else(|p| p.into_inner());
                let (cell, used) = cache.entry(key).or_insert_with(|| (Arc::new(OnceLock::new()), serial));
                // Concurrent older builds must not overwrite a newer use's stamp.
                *used = (*used).max(serial);
                cell.clone()
            };
            let mesh = cell.get_or_init(|| {
                let base = Arc::new(JointBase { outline, bore, lead: Lead::new(proxy, bore), kind });
                let fit = fit_volume(&base, target);
                if fit.limited {
                    log!("fab solder: {:?} pad {} at film limit, {:.6} of {:.6} mm³", kind, pad.no, fit.volume, target.unwrap_or_default());
                }
                Arc::new(JointMesh { mesh: joint_mesh(fit.field), achieved_volume: fit.volume, target_volume: target })
            });
            if let Some(v) = target { worst = worst.max((mesh.achieved_volume - v).abs() / v); }
            if mesh.mesh.indices.is_empty() { continue; }
            distinct.insert(Arc::as_ptr(mesh));
            drawn_triangles += mesh.mesh.indices.len() / 3;
            joints.push(SolderJoint { mesh: mesh.clone(), body,
                body_from_joint: Mat4f { v: body_from_joint.v.map(|v| v as f32) }, roughness: roughness(pad.centre) });
        }
        log!("fab solder: board {}: {} joints, {} distinct meshes, {} triangles, worst volume error {:.2}%, {} ms",
            board_index, joints.len(), distinct.len(), drawn_triangles, worst * 100.0, start.elapsed().as_millis());
        result.push(joints);
    }
    let mut cache = JOINTS.lock().unwrap_or_else(|p| p.into_inner());
    let latest = BUILD_SERIAL.load(Ordering::Relaxed);
    cache.retain(|_, (_, used)| latest.saturating_sub(*used) < 4);
    Some(result)
}

/// Joint inputs equal up to component poses; contour comparisons use the pad's own nm frame.
pub fn same_pads(a: &SceneBoard, b: &SceneBoard) -> bool {
    a.thickness == b.thickness && a.pads.len() == b.pads.len()
        && a.pads.iter().zip(&b.pads).all(|(a, b)| {
            if a.sel != b.sel || a.no != b.no || a.kind != b.kind || a.side != b.side
                || a.component_side != b.component_side || a.paste_area != b.paste_area
                || a.other_rings.len() != b.other_rings.len()
                || !match (&a.drill, &b.drill) {
                    (Some(a), Some(b)) => a.size == b.size && a.plated == b.plated,
                    (None, None) => true,
                    _ => false,
                }
            { return false; }
            let local = |pad: &ScenePad| {
                let (s, c) = pad.rot.sin_cos();
                let centre = pad.centre;
                move |p: P| {
                    let x = (p.x - centre.x) as f64;
                    let y = (p.y - centre.y) as f64;
                    [c * x + s * y, -s * x + c * y]
                }
            };
            let (local_a, local_b) = (local(a), local(b));
            let close = |a: [f64; 2], b: [f64; 2]| (a[0] - b[0]).abs() <= 2.0 && (a[1] - b[1]).abs() <= 2.0;
            std::iter::once(&a.outline).chain(&a.other_rings)
                .zip(std::iter::once(&b.outline).chain(&b.other_rings)).all(|(a, b)| {
                    let (a, b) = (a.to_ring(PAD_POLY_ERR), b.to_ring(PAD_POLY_ERR));
                    if a.len() != b.len() { return false; }
                    let Some(&first) = a.first() else { return true };
                    // Polygon operations may rotate the starting vertex when a part moves.
                    let Some(start) = b.iter().position(|&p| close(local_a(first), local_b(p))) else { return false };
                    a.iter().zip(b[start..].iter().chain(&b[..start])).all(|(&a, &b)| close(local_a(a), local_b(b)))
                })
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use fab_elec_board::ConnItem;
    use fab_elec_core::{mm, Angle, BoardId, CompId, CompRef, Contour, ItemId, PadIx, Sel};

    #[test]
    fn film_converges_to_printed_and_annular_volumes() {
        let thickness = 1.6;
        let rect = |w: f64, h: f64| Contour::rect(P::mm(-w / 2.0, -h / 2.0), P::mm(w / 2.0, h / 2.0), 0);
        let drill = SceneDrill { item: ConnItem::Pad { placed: ItemId(0), pad: PadIx(0) }, net: None,
            c: P::ZERO, size: P::mm(1.0, 1.0), rot: Angle::ZERO, plated: true };
        let cases = [
            ("0603 end", rect(0.9, 0.95), 0.9 * 0.95, None,
                [Vec3d::new(0.05, -0.40, 0.0), Vec3d::new(0.45, 0.40, 0.0), Vec3d::new(0.45, 0.40, 0.45)]),
            ("gull-wing lead", rect(0.3, 1.5), 0.3 * 1.5, None,
                [Vec3d::new(-0.10, -0.40, 0.0), Vec3d::new(0.10, 0.40, 0.0), Vec3d::new(0.10, 0.40, 0.15)]),
            ("THT pin", Contour::circle(P::ZERO, mm(0.85)), 0.0, Some(drill),
                [Vec3d::new(0.32, 0.0, -thickness), Vec3d::new(-0.32, 0.0, 0.50), Vec3d::new(0.0, 0.32, 0.50)]),
        ];
        for (name, outline, paste_mm2, drill, vertices) in cases {
            let thru = drill.is_some();
            let pad = ScenePad {
                sel: Sel::Comp(CompRef { board: BoardId(0), comp: CompId(0) }), no: "1".into(),
                kind: if thru { SolderPadKind::Thru } else { SolderPadKind::Smd },
                side: if thru { Side::Bottom } else { Side::Top }, component_side: Side::Top,
                outline, other_rings: Vec::new(), centre: P::ZERO, rot: Angle::ZERO, drill,
                paste_area: paste_mm2 * 1e12,
            };
            let pose = pad_pose(&pad, thickness);
            let inverse = pose.invert().unwrap();
            let tris = [vertices.map(|v| pose.transform_point(v))];
            let proxy = lead_proxy(&pad, &tris, &inverse, thickness);
            let kind = if thru { JointKind::ThruSolder } else { JointKind::Smd };
            let target = target_volume(&pad, kind).unwrap();
            let expected = if thru { std::f64::consts::PI * (0.85 * 0.85 - 0.5 * 0.5) * 0.15 } else { paste_mm2 * 0.06 };
            let bore = pad.drill.as_ref().map(|d| Bore::new(d, &inverse));
            let base = Arc::new(JointBase { outline: Outline::new(&regions(&pad, &inverse)).unwrap(),
                bore, lead: Lead::new(proxy, bore), kind });
            let fit = fit_volume(&base, Some(target));
            let grid = VolumeGrid::new(&base, base.field(base.smaller_width() * 0.5).bounds());
            let achieved = solder_volume(&fit.field, &grid);
            // The round copper and drill are polygonised within one micrometre.
            assert!(!matches!(proxy, LeadProxy::None) && !fit.limited && (target - expected).abs() <= expected * 0.005
                && (achieved - target).abs() <= target * 0.02,
                "{name}: achieved {achieved:.6} mm³, target {target:.6} mm³ (nominal {expected:.6})");
        }
    }

    #[test]
    fn reflowed_fillets_stay_below_the_cap_and_outside_the_lead() {
        let pad = ScenePad {
            sel: Sel::Comp(CompRef { board: BoardId(0), comp: CompId(0) }), no: "1".into(),
            kind: SolderPadKind::Smd, side: Side::Top, component_side: Side::Top,
            outline: Contour::rect(P::mm(-0.4, -0.3), P::mm(0.4, 0.3), 0),
            other_rings: Vec::new(), centre: P::ZERO, rot: Angle::ZERO, drill: None,
            paste_area: 0.8 * 0.6 * 1e12,
        };
        let pose = pad_pose(&pad, 1.6);
        let inverse = pose.invert().unwrap();
        let target = target_volume(&pad, JointKind::Smd).unwrap();
        for (name, metal_height) in [("gull-wing", 0.15), ("chip", 0.5)] {
            // Closed strip crossing the copper edge. The gull-wing's last section rises off the foot.
            let section = |x, z| [
                Vec3d::new(x, -0.12, z), Vec3d::new(x, 0.12, z),
                Vec3d::new(x, 0.12, z + metal_height), Vec3d::new(x, -0.12, z + metal_height),
            ];
            let mut sections = vec![section(-0.65, 0.0), section(0.04, 0.0)];
            if name == "gull-wing" { sections.push(section(0.22, 0.45)); }
            let mut tris = Vec::new();
            let mut quad = |[a, b, c, d]: [Vec3d; 4]| {
                tris.extend([[a, b, c], [a, c, d]].map(|tri| tri.map(|p| pose.transform_point(p))));
            };
            let [a, b, c, d] = sections[0];
            quad([d, c, b, a]);
            for pair in sections.windows(2) {
                for i in 0..4 {
                    let j = (i + 1) % 4;
                    quad([pair[0][i], pair[0][j], pair[1][j], pair[1][i]]);
                }
            }
            quad(*sections.last().unwrap());
            let flat_face = |x, z| [Vec3d::new(x, 0.0, z), Vec3d::new(x + 0.02, 0.0, z),
                Vec3d::new(x, 0.02, z)].map(|p| pose.transform_point(p));
            tris.push(flat_face(-0.25, 0.01)); // Low metal sliver on the drum16 resistor.
            let case = [flat_face(0.20, 0.08)]; // Moulding below the height of the foot.
            let outline = Outline::new(&regions(&pad, &inverse)).unwrap();
            let proxy = fit_proxy(pad.kind, &outline, None, &tris, &case, &inverse, 1.6);
            let bounds = match (name, proxy) {
                ("gull-wing", LeadProxy::Foot { bounds, thickness }) => {
                    assert!((thickness - metal_height).abs() < 1e-8, "{name}: incorrect foot thickness {thickness}");
                    bounds
                }
                ("chip", LeadProxy::Chip { bounds, height }) => {
                    assert!((height - metal_height).abs() < 1e-8, "{name}: incorrect part height {height}");
                    bounds
                }
                _ => panic!("{name}: incorrect contact classification {proxy:?}"),
            };
            assert!((bounds.min.x + 0.5).abs() < 1e-8 && (bounds.max.x - 0.04).abs() < 1e-8,
                "{name}: clipped contact includes the bend or misses the pad overhang: {bounds:?}");
            let base = Arc::new(JointBase { outline, bore: None, lead: Lead::new(proxy, None), kind: JointKind::Smd });
            let fit = fit_volume(&base, Some(target));
            assert!(!fit.limited && (fit.volume - target).abs() <= target * VOLUME_TOLERANCE,
                "{name}: solder volume {} misses target {target}", fit.volume);
            let t = fit.field.t;
            let cap = if name == "gull-wing" { t + 0.075 } else { t.max(0.5 / 3.0) };
            assert!((fit.field.top() - cap).abs() < 1e-8, "{name}: incorrect field cap {}, expected {cap}", fit.field.top());
            assert!(fit.field.distance(Vec3d::new(-0.15, 0.122, t + 0.01)) < 0.0,
                "{name}: fillet fails to rise along the metal");
            let cell = fit.field.mesh_cell();
            assert!(fit.field.distance(Vec3d::new(-0.4, 0.12, cell)) > 0.0,
                "{name}: the overhanging metal leaves a wall at the copper edge");
            let mesh = joint_mesh(fit.field);
            assert!(!mesh.indices.is_empty(), "{name}: missing fillet mesh");
            let core = bounds.grow(-LEAD_INSET);
            let mut highest = f64::NEG_INFINITY;
            for v in mesh.vertices.chunks_exact(BODY_VERTEX_FLOATS) {
                let p = Vec3d::new(v[0].into(), v[1].into(), v[2].into());
                assert!(!core.contains(p), "{name}: vertex {p:?} inside shrunk lead {core:?}");
                assert!(base.outline.distance([p.x, p.y]) < -1e-6 || p.z <= cell,
                    "{name}: copper-edge wall reaches {} mm", p.z);
                highest = highest.max(p.z);
            }
            assert!(highest <= cap + cell, "{name}: highest vertex {highest} exceeds cap {cap} plus cell {cell}");
        }
    }
}
