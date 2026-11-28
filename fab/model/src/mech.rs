//! The program's joints applied to its solids, and the assembled piece for the
//! checks and the sim (`makepad_csg_mech`).
//!
//! Joints are declarations (`m.hinge`, `m.ball`, `m.chain`, `m.bond`, `m.anchor`, `m.string`);
//! the evaluator sizes them from the parts' meshes and the printer profile,
//! writes each joint's halves into its two parts (new graph nodes over the
//! parts' own), and builds the jointed parts. A bond cuts the part it is to out
//! of the parts it bonds; bonded to a part of a flexible material (the
//! evaluator knows its colour), they bend about it where it runs free between
//! them, measured after the build ([`Mech::measure_flexes`]). Parts joined in
//! place print as one group (one offset, one drop); a string joins no group, and its holes are
//! checked against its parts' outlines as the piece is planned. The checks and the sim run later,
//! on demand, on the jointed parts.

use makepad_csg_graph::{Graph, NodeId};
use makepad_csg_mech as mech;
use makepad_csg_mech::dyn_world::{check_string_parts, Outline};
use makepad_csg_mech::sim::{self, SimOptions, SimReport, StringFault, StringSpec};
use makepad_csg_mech::flex::Flex;
use makepad_csg_mech::{FinalPart, Geometry, JointCheck, JointDecl, Plan, Profile, SolidField, P3};
use makepad_csg_mesh::TriMesh;
use makepad_csg_sdf::mesh_field::{MeshData, MeshField};
use makepad_model_script::lang::{JointCall, Program};
use std::sync::{Arc, OnceLock};

/// The program's mechanism after an evaluation.
pub struct Mech {
    /// Its joints and bonds (its `flexes` empty: [`Self::measure_flexes`] has them).
    pub plan: Plan,
    /// Plan part -> program solid index.
    pub solids: Vec<usize>,
    /// The instance it belongs to, and its script (where its bonds are written).
    pub instance: String,
    pub file: String,
    /// Plan part -> plate part id (`instance/solid`).
    pub names: Vec<String>,
    /// Per joint (plan order): its id and source location.
    pub joint_ids: Vec<String>,
    pub joint_locs: Vec<Option<(u32, u32)>>,
    /// Per bond (plan order): its id and source location.
    pub bond_calls: Vec<(String, Option<(u32, u32)>)>,
    /// The strings between its parts (plan parts, model frame mm), and the anchors (plan order) a hand holds.
    pub strings: Vec<StringSpec>,
    pub held: Vec<usize>,
    /// Per plan part: its mesh before and after the joints (model frame, mm).
    pub originals: Vec<Arc<TriMesh>>,
    pub finals: Vec<Arc<TriMesh>>,
    /// Per program solid: its node in `graph` (jointed parts get their new nodes).
    pub graph: Graph,
    pub nodes: Vec<NodeId>,
    /// Per plan part: its in-place group (parts joined in place print together).
    pub group: Vec<usize>,
    fields: OnceLock<Vec<MeshField>>,
    flexes: OnceLock<Result<Vec<Flex>, crate::eval::Failure>>,
    /// Its bends were sent to be measured ([`Self::ask_measure`]).
    asked: std::sync::atomic::AtomicBool,
}

impl std::fmt::Debug for Mech {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Mech({} parts, {} joints)", self.names.len(), self.plan.joints.len())
    }
}

/// A hinge's motor's torque when its script gives none (N mm): a micro servo's.
pub const MOTOR_TORQUE: f64 = 150.0;
/// A slider's motor's force when its script gives none (N).
pub const MOTOR_FORCE: f64 = 10.0;

/// A joint the evaluator could not build: where it was written and why.
pub struct MechError {
    pub loc: Option<(u32, u32)>,
    pub id: String,
    pub message: String,
}

fn bounds(m: &TriMesh) -> mech::PartBox {
    let b = m.bounding_box();
    mech::PartBox { min: [b.min.x, b.min.y, b.min.z], max: [b.max.x, b.max.y, b.max.z] }
}

/// Whether the program's joints are only hands holding its parts (`m.anchor(part, {at: [..] held: true})`):
/// that is no mechanism, so its parts stay free parts that start in those hands ([`hands`]).
fn hands_only(p: &Program) -> bool {
    p.joints.iter().all(|j| j.proto == "Anchor" && j.value("held").is_some() && j.value("fixed").is_none())
}

/// Whether the program is a mechanism: it has joints other than hands holding its parts.
pub(crate) fn jointed(p: &Program) -> bool {
    !hands_only(p)
}

/// Where hands hold the parts of a program whose joints are only that ([`hands_only`]): per anchor, its
/// solid and point (model frame, mm). Empty for a program with joints, whose anchors are its mechanism's.
pub fn hands(p: &Program) -> Vec<(usize, P3)> {
    if !hands_only(p) {
        return Vec::new();
    }
    p.joints.iter().map(|j| (j.a, j.at.unwrap())).collect()
}

/// Plan the program's joints: `mesh` builds a solid's original mesh. None without joints, or with only
/// hands holding its parts.
pub fn plan(p: &Program, profile: &Profile, mesh: &mut dyn FnMut(usize) -> Result<Arc<TriMesh>, String>) -> Result<Option<Mech>, MechError> {
    if !jointed(p) {
        return Ok(None);
    }
    let fail = |j: &JointCall, message: String| MechError { loc: j.loc, id: j.id.clone(), message };
    // the parts the joints name, in first-use order
    let mut solids: Vec<usize> = Vec::new();
    for j in &p.joints {
        for s in std::iter::once(j.a).chain(j.b).chain(j.shoe) {
            if !solids.contains(&s) {
                solids.push(s);
            }
        }
    }
    let part_of = |s: usize| solids.iter().position(|x| *x == s).unwrap();
    let mut originals = Vec::new();
    for &s in &solids {
        let m = mesh(s).map_err(|e| MechError { loc: p.solids[s].loc, id: p.solids[s].id.clone(), message: e })?;
        if m.triangles.is_empty() {
            return Err(MechError { loc: p.solids[s].loc, id: p.solids[s].id.clone(), message: "a jointed part is empty".into() });
        }
        originals.push(m);
    }
    let boxes: Vec<mech::PartBox> = originals.iter().map(|m| bounds(m)).collect();
    let up = profile.up;
    let mut decls = Vec::new();
    let mut anchors = Vec::new();
    let mut joint_ids = Vec::new();
    let mut joint_locs = Vec::new();
    let mut bonds: Vec<mech::BondDecl> = Vec::new();
    let mut bond_calls: Vec<(String, Option<(u32, u32)>)> = Vec::new();
    let mut strings = Vec::new();
    let mut string_calls = Vec::new();
    let mut held = Vec::new();
    for j in &p.joints {
        let a = part_of(j.a);
        match j.proto {
            // a bond's calls come in a row, one per part; the evaluator says if it is flexible
            "Bond" => {
                let b = part_of(j.b.unwrap());
                if bond_calls.last().is_some_and(|(id, _)| *id == j.id) {
                    bonds.last_mut().unwrap().parts.push(b);
                } else {
                    bonds.push(mech::BondDecl { to: a, parts: vec![b], flexible: false });
                    bond_calls.push((j.id.clone(), j.loc));
                }
            }
            "Anchor" => {
                let axis = j.axis.unwrap_or(up);
                let by_hand = j.value("held").is_some();
                if by_hand && j.value("fixed").is_some() {
                    return Err(fail(j, "an anchor holds its part fixed or by hand, not both".into()));
                }
                let hole = if j.value("fixed").is_some() || by_hand { None } else { Some((axis, j.value("hole").unwrap_or(1.2))) };
                if by_hand {
                    held.push(anchors.len());
                }
                anchors.push(mech::AnchorDecl { part: a, at: j.at.unwrap(), hole });
            }
            "String" => {
                strings.push(StringSpec { a, from: j.at.unwrap(), b: part_of(j.b.unwrap()), to: j.to.unwrap(), through: Vec::new(), length: j.value("length").unwrap() });
                string_calls.push(j);
            }
            proto => {
                let b = part_of(j.b.unwrap());
                // a chain's joint sits midway between the two links' centres
                let at = j.at.unwrap_or_else(|| {
                    let (ca, cb) = (boxes[a].center(), boxes[b].center());
                    [(ca[0] + cb[0]) * 0.5, (ca[1] + cb[1]) * 0.5, (ca[2] + cb[2]) * 0.5]
                });
                // a hinge's motor pushes with N mm, a slider's with N
                let motor = |limit: &str, default: f64| {
                    j.motor.map(|kind| mech::MotorDecl { kind: if kind == "speed" { mech::MotorKind::Speed } else { mech::MotorKind::Position }, limit: j.value(limit).unwrap_or(default) })
                };
                let d = match proto {
                    "Hinge" => {
                        let limits = match (j.value("lower"), j.value("upper")) {
                            (None, None) => None,
                            (l, u) => Some((l.unwrap_or(-std::f64::consts::PI), u.unwrap_or(std::f64::consts::PI))),
                        };
                        let brake = j.shoe.map(|s| mech::BrakeDecl { shoe: part_of(s), hold: j.value("hold") });
                        JointDecl::Hinge(mech::HingeDecl { a, b, at, axis: j.axis.unwrap_or(up), limits, pin: j.value("pin"), plate: j.value("plate"), clearance: j.value("clearance"), clearance_axial: j.value("clearance_axial"), brake, motor: motor("torque", MOTOR_TORQUE) })
                    }
                    "Slider" => {
                        // an end not given slides to its end stop
                        let travel = (j.value("lower"), j.value("upper"));
                        let axis = j.axis.ok_or_else(|| fail(j, "a slider needs axis: the way it slides".into()))?;
                        JointDecl::Slider(mech::SliderDecl { a, b, at, axis, travel, width: j.value("width"), depth: j.value("depth"), clearance: j.value("clearance"), motor: motor("force", MOTOR_FORCE) })
                    }
                    "Bearing" => JointDecl::Bearing(mech::BearingDecl { a, b, at, radius: j.value("radius"), depth: j.value("depth"), clearance: j.value("clearance") }),
                    _ => {
                        let twist = match (j.value("twist_lower"), j.value("twist_upper")) {
                            (None, None) => None,
                            (l, u) => Some((l.unwrap_or(-std::f64::consts::PI), u.unwrap_or(std::f64::consts::PI))),
                        };
                        JointDecl::Ball(mech::BallDecl { a, b, at, radius: j.value("radius"), neck: j.value("neck"), axis: j.axis, cone: j.value("cone").unwrap_or(35f64.to_radians()), twist, clearance: j.value("clearance"), wall: j.value("wall") })
                    }
                };
                decls.push(d);
                joint_ids.push(j.id.clone());
                joint_locs.push(j.loc);
            }
        }
    }
    // a string's holes inside its parts and its length no shorter than its path: plain errors at the string
    let names: Vec<&str> = solids.iter().map(|s| p.solids[*s].id.as_str()).collect();
    let outlines: Vec<std::cell::OnceCell<Outline>> = solids.iter().map(|_| Default::default()).collect();
    check_string_parts(&strings, &names, &|part, at| outlines[part].get_or_init(|| Outline::new(&verts(&originals[part]))).contains(at)).map_err(|f| {
        let (StringFault::NoPart { string, .. } | StringFault::Outside { string, .. } | StringFault::TooShort { string, .. }) = &f;
        fail(string_calls[*string], f.what())
    })?;
    mech::check_bonds(solids.len(), &bonds).map_err(|e| {
        let (id, loc) = bond_calls[e.bond].clone();
        MechError { loc, id, message: e.message }
    })?;
    let plan = mech::plan(&boxes, &decls, &bonds, &anchors, profile).map_err(|e| {
        let call = p.joints.iter().filter(|j| !matches!(j.proto, "Anchor" | "Bond" | "String")).nth(e.joint).unwrap();
        fail(call, e.message)
    })?;
    // the jointed parts as new nodes over the parts' own
    let mut graph = p.graph.clone();
    let part_nodes: Vec<NodeId> = solids.iter().map(|s| p.solids[*s].node).collect();
    let mut nodes: Vec<NodeId> = p.solids.iter().map(|s| s.node).collect();
    for (k, solid) in plan.solids.iter().enumerate() {
        let n = solid.lower(&mut graph, &part_nodes, 96).map_err(|e| MechError { loc: p.solids[solids[k]].loc, id: p.solids[solids[k]].id.clone(), message: e })?;
        nodes[solids[k]] = n;
    }
    // in-place groups: parts connected by joints and bonds, a brake's shoes with the part they
    // print in
    let mut group: Vec<usize> = (0..solids.len()).collect();
    fn root(g: &mut Vec<usize>, i: usize) -> usize {
        let mut r = i;
        while g[r] != r {
            r = g[r];
        }
        g[i] = r;
        r
    }
    let shoes = plan.decls.iter().filter_map(|d| d.shoe().map(|s| (d.parts().1, s)));
    let pairs = plan.decls.iter().map(|d| d.parts()).chain(shoes).chain(plan.bonds.iter().flat_map(|b| b.parts.iter().map(|p| (b.to, *p))));
    for (a, b) in pairs {
        let (ra, rb) = (root(&mut group, a), root(&mut group, b));
        group[ra.max(rb)] = ra.min(rb);
    }
    for i in 0..group.len() {
        group[i] = root(&mut group, i);
    }
    Ok(Some(Mech {
        instance: String::new(),
        file: String::new(),
        names: solids.iter().map(|s| p.solids[*s].id.clone()).collect(),
        solids,
        plan,
        joint_ids,
        joint_locs,
        bond_calls,
        strings,
        held,
        originals,
        finals: Vec::new(),
        graph,
        nodes,
        group,
        fields: OnceLock::new(),
        flexes: OnceLock::new(),
        asked: Default::default(),
    }))
}

/// A mesh prepared for distance queries.
pub(crate) fn mesh_field(m: &TriMesh) -> MeshField {
    MeshField::new(MeshData { positions: m.vertices.iter().map(|p| [p.x as f32, p.y as f32, p.z as f32]).collect(), triangles: m.triangles.clone() })
}

fn verts(m: &TriMesh) -> Vec<P3> {
    m.vertices.iter().map(|v| [v.x, v.y, v.z]).collect()
}

impl Mech {
    fn originals_fields(&self) -> &[MeshField] {
        self.fields.get_or_init(|| self.originals.iter().map(|m| mesh_field(m)).collect())
    }

    /// The final parts (jointed meshes, their fields from the original parts) for `f`.
    pub fn with_parts<R>(&self, f: impl FnOnce(&[FinalPart]) -> R) -> R {
        let originals = self.originals_fields();
        let fields: Vec<SolidField> = self.plan.solids.iter().map(|s| SolidField { solid: s, parts: originals.iter().map(|f| f as &(dyn makepad_csg_sdf::Sdf3 + Sync)).collect() }).collect();
        let vs: Vec<Vec<P3>> = self.finals.iter().map(|m| verts(m)).collect();
        let parts: Vec<FinalPart> = (0..self.names.len()).map(|i| FinalPart { name: &self.names[i], field: &fields[i], vertices: &vs[i], triangles: &self.finals[i].triangles }).collect();
        f(&parts)
    }

    /// Whether the sim hangs it on its first anchor's pin (one held fixed or by hand, or with no
    /// anchor, stays on the bed as it printed).
    pub fn hangs(&self) -> bool {
        self.plan.anchors.first().is_some_and(|a| a.hole.is_some()) && !self.held.contains(&0)
    }

    /// Whether parts are bonded to a flexible part: their bends need measuring.
    pub fn flexing(&self) -> bool {
        self.plan.bonds.iter().any(|b| b.flexible)
    }

    /// Where the parts bonded to a flexible part bend about it, measured on the final parts:
    /// once, by the first to ask (the app measures right after the build, in the background;
    /// a caller meanwhile waits for that). A bend that cannot be measured fails it at its bond.
    pub fn measure_flexes(&self) -> &Result<Vec<Flex>, crate::eval::Failure> {
        self.flexes.get_or_init(|| {
            if !self.flexing() {
                return Ok(Vec::new());
            }
            self.with_parts(|parts| mech::flex::measure_all(&self.plan.bonds, parts)).map_err(|(i, message)| {
                let (id, loc) = self.bond_calls[i].clone();
                let (line, col) = loc.unwrap_or((1, 1));
                crate::eval::Failure { lost: None, file: self.file.clone(), line, col, id, op: "joint".into(), message }
            })
        })
    }

    /// True the first time it is asked: whoever gets it measures the bends (the app, once a piece,
    /// whichever plate shows it).
    pub fn ask_measure(&self) -> bool {
        self.flexing() && !self.asked.swap(true, std::sync::atomic::Ordering::Relaxed)
    }

    /// The bends once measured, without waiting.
    pub fn flexes(&self) -> Option<&Result<Vec<Flex>, crate::eval::Failure>> {
        self.flexes.get()
    }

    /// The design check of every joint.
    pub fn check(&self, sink: &dyn mech::CheckSink) -> Vec<JointCheck> {
        self.with_parts(|parts| mech::check_joints(&self.plan, parts, Geometry::Design, sink))
    }

    /// The sim's assembly (bodies from the jointed meshes, joints with the checks' effective limits;
    /// after them, the bonds' soft joints and welds; the anchors, the strings): once its bends are
    /// measured (it waits for them), none when they cannot be.
    pub fn assembly(&self, checks: &[JointCheck], fill: f64) -> Result<sim::Assembly, crate::eval::Failure> {
        let flexes = self.measure_flexes().as_ref().map_err(Clone::clone)?;
        let measured;
        let plan = if flexes.is_empty() {
            &self.plan
        } else {
            measured = Plan { flexes: flexes.clone(), ..self.plan.clone() };
            &measured
        };
        let mut asm = self.with_parts(|parts| mech::assembly(plan, parts, checks, fill));
        for (j, id) in asm.joints.iter_mut().zip(&self.joint_ids) {
            j.name = id.clone();
        }
        // a held part plans as a fixed one (no hole); in the sim a hand holds it
        for &k in &self.held {
            asm.anchors[k].kind = sim::AnchorKind::Held;
        }
        asm.strings = self.strings.clone();
        Ok(asm)
    }

    pub fn simulate(&self, asm: &sim::Assembly, opts: &SimOptions, cancelled: &dyn Fn() -> bool) -> Result<SimReport, String> {
        sim::simulate(asm, opts, cancelled)
    }

    /// The plan part a program solid is, if it is jointed.
    pub fn part_of_solid(&self, solid: usize) -> Option<usize> {
        self.solids.iter().position(|s| *s == solid)
    }
}

/// What the slicer will lay down near one joint: its beads, part by part.
pub struct PrintedJoint {
    pub a: mech::bead::BeadField,
    /// B's beads, with its brake's shoes' (they print in it).
    pub b: mech::bead::BeadField,
    /// A braked hinge's B without its shoes: what turns clear of A (the shoes give).
    pub b_turns: Option<mech::bead::BeadField>,
    /// Support beads in the joint box (they must not reach both parts).
    pub support: Option<mech::bead::BeadField>,
    /// Exact smallest in-layer gap between A's and B's beads (edge to edge) and its print z.
    pub bead_gap: f64,
    pub bead_gap_z: f64,
    /// Smallest air gap between a bead of one part and a stacked bead of the other (0: printed onto it).
    pub stacked_gap: f64,
    /// Support touches both parts.
    pub support_bridges: bool,
}

fn affine(m: &[[f64; 4]; 3], p: P3) -> P3 {
    std::array::from_fn(|i| m[i][0] * p[0] + m[i][1] * p[1] + m[i][2] * p[2] + m[i][3])
}
fn linear(m: &[[f64; 4]; 3], p: P3) -> P3 {
    std::array::from_fn(|i| m[i][0] * p[0] + m[i][1] * p[1] + m[i][2] * p[2])
}

fn seg_seg(a0: [f64; 2], a1: [f64; 2], b0: [f64; 2], b1: [f64; 2]) -> f64 {
    let pt = |p: [f64; 2], s0: [f64; 2], s1: [f64; 2]| {
        let (dx, dy) = (s1[0] - s0[0], s1[1] - s0[1]);
        let l2 = dx * dx + dy * dy;
        let t = if l2 > 0.0 { (((p[0] - s0[0]) * dx + (p[1] - s0[1]) * dy) / l2).clamp(0.0, 1.0) } else { 0.0 };
        ((p[0] - s0[0] - dx * t).powi(2) + (p[1] - s0[1] - dy * t).powi(2)).sqrt()
    };
    let cross = |o: [f64; 2], a: [f64; 2], b: [f64; 2]| (a[0] - o[0]) * (b[1] - o[1]) - (a[1] - o[1]) * (b[0] - o[0]);
    let (d1, d2, d3, d4) = (cross(b0, b1, a0), cross(b0, b1, a1), cross(a0, a1, b0), cross(a0, a1, b1));
    if (d1 > 0.0) != (d2 > 0.0) && (d3 > 0.0) != (d4 > 0.0) {
        return 0.0;
    }
    pt(a0, b0, b1).min(pt(a1, b0, b1)).min(pt(b0, a0, a1)).min(pt(b1, a0, a1))
}

impl Mech {
    /// The beads the slicer lays near joint `j` (printer frame), from the sliced layers, a brake's shoes' with B's.
    /// `plate_part[k]` is plan part k's plate part (its `ObjectLayer.object`), `to_plate` the joint's model-to-plate
    /// map (the group's).
    pub fn printed_joint(&self, j: usize, layers: &[makepad_fab_toolpath::Layer], plate_part: &[Option<usize>], to_plate: &[[f64; 4]; 3]) -> Option<PrintedJoint> {
        use makepad_fab_toolpath::Role;
        let (a, b) = self.plan.decls[j].parts();
        let (pa, pb) = (plate_part[a]?, plate_part[b]?);
        let ps = match self.plan.joints[j].shoe() {
            Some(s) => Some(plate_part[s]?),
            None => None,
        };
        let (lo, hi) = self.plan.joints[j].region();
        let mut blo = [f64::INFINITY; 3];
        let mut bhi = [f64::NEG_INFINITY; 3];
        for c in 0..8 {
            let q = affine(to_plate, [if c & 1 == 0 { lo[0] } else { hi[0] }, if c & 2 == 0 { lo[1] } else { hi[1] }, if c & 4 == 0 { lo[2] } else { hi[2] }]);
            for k in 0..3 {
                blo[k] = blo[k].min(q[k]);
                bhi[k] = bhi[k].max(q[k]);
            }
        }
        let (mut ba, mut bb, mut bs, mut bt) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
        let mut layer_h = f64::INFINITY;
        // per layer, A's and B's beads for the exact in-layer gap
        let mut per_layer: Vec<(f64, Vec<mech::bead::Bead>, Vec<mech::bead::Bead>)> = Vec::new();
        for l in layers {
            let (z0, z1) = (l.print_z - l.height, l.print_z);
            if z1 < blo[2] || z0 > bhi[2] {
                continue;
            }
            layer_h = layer_h.min(l.height);
            let (mut la, mut lb) = (Vec::new(), Vec::new());
            for ol in &l.objects {
                let which = if ol.object == pa {
                    1
                } else if ol.object == pb {
                    2
                } else if Some(ol.object) == ps {
                    3
                } else {
                    continue;
                };
                for isl in &ol.islands {
                    for path in isl.walls.iter().chain(isl.fills.iter()) {
                        let support = matches!(path.role, Role::Support | Role::SupportInterface | Role::SupportTransition);
                        // a bead printed over air sags about half a layer
                        let sag = if matches!(path.role, Role::Bridge | Role::OverhangWall) { 0.5 * path.height } else { 0.0 };
                        let n = path.pts.len();
                        let segs = if path.closed { n } else { n.saturating_sub(1) };
                        for k in 0..segs {
                            let (p, q) = (path.pts[k], path.pts[(k + 1) % n]);
                            let (pa2, qa2) = ([p.x_mm(), p.y_mm()], [q.x_mm(), q.y_mm()]);
                            let r = 0.5 * path.width + 0.1;
                            if pa2[0].max(qa2[0]) + r < blo[0] || pa2[0].min(qa2[0]) - r > bhi[0] || pa2[1].max(qa2[1]) + r < blo[1] || pa2[1].min(qa2[1]) - r > bhi[1] {
                                continue;
                            }
                            let bead = mech::bead::Bead { a: pa2, b: qa2, z0: z1 - path.height - sag, z1: z1 - sag, width: path.width };
                            if support || ol.support_only {
                                bs.push(bead);
                            } else if which == 1 {
                                ba.push(bead);
                                la.push(bead);
                            } else {
                                if which == 2 {
                                    bt.push(bead);
                                }
                                bb.push(bead);
                                lb.push(bead);
                            }
                        }
                    }
                }
            }
            per_layer.push((l.print_z, la, lb));
        }
        if ba.is_empty() || bb.is_empty() {
            return None;
        }
        let mut bead_gap = f64::INFINITY;
        let mut bead_gap_z = 0.0;
        // Across layers: a bead of one part over (or under) a bead of the other, overlapping in plan, up to three
        // layers apart: the air between them (0: printed onto it).
        let mut stacked_gap = f64::INFINITY;
        for (i, (_, la, lb)) in per_layer.iter().enumerate() {
            for (_, ua, ub) in per_layer.iter().skip(i + 1).take(3) {
                for (lower, upper) in [(la, ub), (lb, ua)] {
                    for x in lower.iter() {
                        for y in upper.iter() {
                            if seg_seg(x.a, x.b, y.a, y.b) < 0.5 * (x.width + y.width) {
                                stacked_gap = stacked_gap.min(y.z0 - x.z1);
                            }
                        }
                    }
                }
            }
        }
        for (z, la, lb) in &per_layer {
            for x in la {
                for y in lb {
                    let d = seg_seg(x.a, x.b, y.a, y.b) - 0.5 * (x.width + y.width);
                    if d < bead_gap {
                        bead_gap = d;
                        bead_gap_z = *z;
                    }
                }
            }
        }
        let layer = if layer_h.is_finite() { layer_h } else { self.plan.profile.layer };
        let field = |v: Vec<mech::bead::Bead>| mech::bead::BeadField::new(v, 0.0, layer, 1.0);
        let fa = field(ba);
        let fb = field(bb);
        let b_turns = ps.map(|_| field(bt));
        let (support, support_bridges) = if bs.is_empty() {
            (None, false)
        } else {
            let fs = field(bs);
            let touches = |f: &mech::bead::BeadField| mech::check::closer_than(&fs, f, blo, bhi, 0.05);
            let both = touches(&fa) && touches(&fb);
            (Some(fs), both)
        };
        Some(PrintedJoint { a: fa, b: fb, b_turns, support, bead_gap, bead_gap_z, stacked_gap, support_bridges })
    }

    /// The joint checks on the sliced beads (geometry: as printed): gaps, layers, fused risk and limits from what
    /// the printer lays down. `plate_part[k]` / `to_plate[k]`: plan part k's plate part and model-to-plate map.
    pub fn check_as_printed(&self, layers: &[makepad_fab_toolpath::Layer], plate_part: &[Option<usize>], to_plate: &[[[f64; 4]; 3]]) -> Vec<(JointCheck, PrintedJointSummary)> {
        // one index per joint, each on its own beads, on the slicer's maps: as many threads as the run's
        // CPU budget gives them (`par::set_threads`), on the app's pool where it set one, and on the
        // caller alone inside another map
        let checked = makepad_fab_toolpath::par::map(self.plan.joints.len(), |j| self.check_printed_joint(j, layers, plate_part, to_plate));
        let mut v: Vec<_> = checked.into_iter().flatten().collect();
        v.sort_by_key(|(c, _)| c.joint);
        v
    }

    fn check_printed_joint(&self, j: usize, layers: &[makepad_fab_toolpath::Layer], plate_part: &[Option<usize>], to_plate: &[[[f64; 4]; 3]]) -> Option<(JointCheck, PrintedJointSummary)> {
        let started = std::time::Instant::now();
        let (a, b) = self.plan.decls[j].parts();
        let m = to_plate[a];
        let pj = self.printed_joint(j, layers, plate_part, &m)?;
        let g = &self.plan.joints[j];
        let (lo, hi) = g.region();
        let mut blo = [f64::INFINITY; 3];
        let mut bhi = [f64::NEG_INFINITY; 3];
        for c in 0..8 {
            let q = affine(&m, [if c & 1 == 0 { lo[0] } else { hi[0] }, if c & 2 == 0 { lo[1] } else { hi[1] }, if c & 4 == 0 { lo[2] } else { hi[2] }]);
            for k in 0..3 {
                blo[k] = blo[k].min(q[k]);
                bhi[k] = bhi[k].max(q[k]);
            }
        }
        // The gap straight from the beads: edge to edge in a layer, and air between stacked beads.
        let gap_mm = pj.bead_gap.min(pj.stacked_gap);
        let gap = mech::Gap { gap: gap_mm, at: [0.0, 0.0, pj.bead_gap_z], evals: 0 };
        let t_gap = started.elapsed().as_secs_f64() * 1000.0;
        let layer = self.plan.profile.layer;
        let mut layers_r = mech::check::layer_check(&pj.a, &pj.b, mech::check::LayerSetup { lo: blo, hi: bhi, up: [0.0, 0.0, 1.0], layer, res: 0.1, bed: 0.0 });
        let t_layers = started.elapsed().as_secs_f64() * 1000.0 - t_gap;
        layers_r.layer_gap = layers_r.layer_gap.min(pj.bead_gap);
        if pj.stacked_gap <= 1e-6 {
            layers_r.tack[0] = layers_r.tack[0].max(1.0);
        }
        let (mut risk, risk_score) = mech::check::risk(gap.gap, layers_r.layer_gap, &layers_r.tack, self.plan.profile.c_xy());
        if pj.support_bridges {
            risk = mech::Risk::Fused;
        }
        let (pivot, axis) = g.pivot();
        let (pivot, axis) = (affine(&m, pivot), mech::geom::norm(linear(&m, axis)));
        let mech::Swept { kind, declared, field, effective, turns } =
            mech::sweep_joint(g, pivot, axis, (blo, bhi), 0.05, &mut |s| mech::check::sweep_steps(&pj.a, pj.b_turns.as_ref().unwrap_or(&pj.b), s, &mut |_| true)).expect("a sweep that never stops ends");
        let mut binds = mech::binds(&gap, &layers_r, risk_score, g.clearance(), self.plan.profile.c_xy(), mech::short_range(kind, effective), g.retention());
        if pj.support_bridges {
            binds.push(mech::Bind::Supports);
        }
        let millis = started.elapsed().as_secs_f64() * 1000.0;
        let check = JointCheck {
            joint: j,
            kind,
            a,
            b,
            clearance: g.clearance(),
            gap,
            layers: layers_r,
            risk,
            risk_score,
            declared,
            field,
            effective,
            retention: g.retention(),
            geometry: Geometry::AsPrinted,
            notes: self.plan.notes[j].clone(),
            millis,
            phases: (t_gap, t_layers, millis - t_gap - t_layers),
            turns,
            binds,
        };
        Some((check, PrintedJointSummary { bead_gap: pj.bead_gap, bead_gap_z: pj.bead_gap_z, support_bridges: pj.support_bridges, beads: (pj.a.beads().len(), pj.b.beads().len()) }))
    }
}

/// The slicer's numbers for one joint, beside its check.
#[derive(Clone, Debug, PartialEq)]
pub struct PrintedJointSummary {
    pub bead_gap: f64,
    pub bead_gap_z: f64,
    pub support_bridges: bool,
    pub beads: (usize, usize),
}
