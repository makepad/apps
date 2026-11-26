//! `fab.interference`: whether a jointed object's parts collide anywhere in their travel. Every pair
//! of its parts at rest, then with each joint at each end of its travel (the others at rest), each
//! overlapping pair answered with how deep, where and in which pose. `fab.joints` checks a joint's
//! own two parts; this checks the others: a lid swung into a latch, a finger into the palm.

use crate::checks::{lead, not_checked, Check};
use crate::out::{arr, int, line, mm, mm3, obj, s, S};
use crate::{str_arg, Reply, Session};
use fab_model::eval::Evaluated;
use fab_model::mech::Mech;
use makepad_csg_mech::check::{deepest_overlap, rigid};
use makepad_csg_mech::sim::{pose_bodies, Assembly, JointKind};
use makepad_csg_mech::{JointCheck, GAP_TOL, P3};
use makepad_model::json::Value;
use std::sync::Arc;

pub const TOOL: &str = "interference";

/// Shallower than this (mm), two parts touch rather than overlap: the search's tolerance and a
/// mesh's facets.
const TOUCH: f64 = 0.05;

/// A body's pose: its centre of mass and its rotation from as built ([`pose_bodies`]).
type Placed = (P3, [[f64; 3]; 3]);

/// A pose: what it is in words, and each of the sim's joints' angle (radians) or slide (mm).
struct Pose {
    words: String,
    angles: Vec<f64>,
}

/// Two parts (plan indexes) that overlap in a pose: by how much, and where (model frame, mm, the
/// pose applied).
pub struct Overlap {
    pub a: usize,
    pub b: usize,
    pub depth: f64,
    pub at: P3,
    pub pose: usize,
}

/// One object's search: its poses in words, how many pair-poses were searched (pairs whose boxes
/// meet), what overlaps (deepest first), and the pairs left out by design, with why.
pub struct Found {
    pub poses: Vec<String>,
    pub searched: usize,
    pub overlaps: Vec<Overlap>,
    pub skipped: Vec<(usize, usize, &'static str)>,
}

/// Rest, then each joint at each end of the travel the sim gives it (its declared limits, else
/// where `fab.joints` found its parts touch), the others at rest; a hinge that turns all the way
/// round at its quarter turns. A soft joint (a flexible part's bend) and a weld have no ends.
fn poses(mech: &Mech, asm: &Assembly) -> Vec<Pose> {
    let rest = vec![0.0; asm.joints.len()];
    let mut out = vec![Pose { words: "rest".into(), angles: rest.clone() }];
    for j in 0..mech.plan.joints.len().min(asm.joints.len()) {
        let id = mech.joint_ids.get(j).map_or("a joint", |s| s.as_str());
        let turn = |a: f64| (a, format!("{id} at {}°", num(a.to_degrees())));
        let ends: Vec<(f64, String)> = match &asm.joints[j].kind {
            k if k.turns_freely() => [90f64, 180.0, -90.0].iter().map(|d| turn(d.to_radians())).collect(),
            JointKind::Revolute { lower, upper } => vec![turn(*lower), turn(*upper)],
            JointKind::Prismatic { lower, upper } => [*lower, *upper].iter().map(|x| (*x, format!("{id} slid {}mm", num(*x)))).collect(),
            JointKind::Spherical { cone, .. } => [*cone, -*cone].iter().map(|a| (*a, format!("{id} tilted {}°", num(a.to_degrees())))).collect(),
            JointKind::Flex { .. } | JointKind::Weld => Vec::new(),
        };
        for (angle, words) in ends.into_iter().filter(|(a, _)| a.abs() > 1e-9) {
            let mut angles = rest.clone();
            angles[j] = angle;
            out.push(Pose { words, angles });
        }
    }
    out
}

/// A number in words, to a tenth.
fn num(x: f64) -> String {
    let r = (x * 10.0).round() / 10.0;
    if r == r.trunc() { format!("{}", r as i64) } else { format!("{r}") }
}

/// The pairs that touch or overlap by design, each with why: a joint's own two parts (`fab.joints`
/// checks those, with their clearance and limits), a brake's shoe with its joint's parts, a part
/// with what it is bonded to.
fn by_design(mech: &Mech) -> Vec<(usize, usize, &'static str)> {
    let mut out: Vec<(usize, usize, &'static str)> = Vec::new();
    let mut add = |a: usize, b: usize, why: &'static str| {
        let (a, b) = (a.min(b), a.max(b));
        if a != b && !out.iter().any(|(x, y, _)| (*x, *y) == (a, b)) {
            out.push((a, b, why));
        }
    };
    for d in &mech.plan.decls {
        let (a, b) = d.parts();
        add(a, b, "a joint's own two parts: fab.joints checks them");
        if let Some(shoe) = d.shoe() {
            add(shoe, b, "a brake's shoe, in its eye");
            add(shoe, a, "a brake's shoe, on its pin");
        }
    }
    for bond in &mech.plan.bonds {
        for &p in &bond.parts {
            add(bond.to, p, "bonded: they print fused");
        }
    }
    out
}

/// The box `(lo, hi)` of a body as built (centre of mass `com`) once it is placed as `placed` says.
fn posed_box((lo, hi): (P3, P3), com: P3, (c, r): &Placed) -> (P3, P3) {
    let (mut min, mut max) = ([f64::INFINITY; 3], [f64::NEG_INFINITY; 3]);
    for k in 0..8 {
        let p = [if k & 1 == 0 { lo[0] } else { hi[0] }, if k & 2 == 0 { lo[1] } else { hi[1] }, if k & 4 == 0 { lo[2] } else { hi[2] }];
        let q = [p[0] - com[0], p[1] - com[1], p[2] - com[2]];
        for i in 0..3 {
            let x = c[i] + r[i][0] * q[0] + r[i][1] * q[1] + r[i][2] * q[2];
            min[i] = min[i].min(x);
            max[i] = max[i].max(x);
        }
    }
    (min, max)
}

/// Whether a body's pose is another's (to a micrometre, a millionth of a turn).
fn same(a: &Placed, b: &Placed) -> bool {
    (0..3).all(|i| (a.0[i] - b.0[i]).abs() < 1e-6 && (0..3).all(|k| (a.1[i][k] - b.1[i][k]).abs() < 1e-6))
}

/// Every pair of `mech`'s parts searched for overlap in each pose (`checks`: its joint checks, whose
/// limits the sim's joints take): at rest all of them, in a joint's pose the pairs it moves apart
/// (one part on its moving side, one not). A pair whose boxes do not meet has a gap and is not
/// searched; one that does, by the deepest overlap of their fields there. Stops between pairs when
/// `cancelled`.
pub fn find(mech: &Mech, checks: &[JointCheck], cancelled: &dyn Fn() -> bool) -> Result<Found, String> {
    let asm = mech.assembly(checks, 1.0).map_err(|f| format!("its bends can't be measured: {f}"))?;
    let poses = poses(mech, &asm);
    let skipped = by_design(mech);
    let count = mech.names.len().min(asm.bodies.len()).min(mech.finals.len());
    let boxes: Vec<(P3, P3)> = mech.finals[..count].iter().map(|m| {
        let b = m.bounding_box();
        ([b.min.x, b.min.y, b.min.z], [b.max.x, b.max.y, b.max.z])
    }).collect();
    let coms: Vec<P3> = asm.bodies[..count].iter().map(|b| b.mass.com).collect();
    const STILL: [[f64; 3]; 3] = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
    let rest: Vec<Placed> = coms.iter().map(|c| (*c, STILL)).collect();
    mech.with_parts(|parts| {
        let mut found = Found { poses: poses.iter().map(|p| p.words.clone()).collect(), searched: 0, overlaps: Vec::new(), skipped: skipped.clone() };
        for (k, pose) in poses.iter().enumerate() {
            let placed = if k == 0 { rest.clone() } else { pose_bodies(&asm, &pose.angles) };
            let moved: Vec<bool> = (0..count).map(|i| !same(&placed[i], &rest[i])).collect();
            for i in 0..count {
                for j in i + 1..count {
                    // by design; or moved together, as they were at rest
                    if skipped.iter().any(|(a, b, _)| (*a, *b) == (i, j)) || (k > 0 && moved[i] == moved[j]) {
                        continue;
                    }
                    if cancelled() {
                        return Err("cancelled".to_string());
                    }
                    let (bi, bj) = (posed_box(boxes[i], coms[i], &placed[i]), posed_box(boxes[j], coms[j], &placed[j]));
                    let lo: P3 = std::array::from_fn(|d| bi.0[d].max(bj.0[d]) - TOUCH);
                    let hi: P3 = std::array::from_fn(|d| bi.1[d].min(bj.1[d]) + TOUCH);
                    if (0..3).any(|d| lo[d] > hi[d]) {
                        continue;
                    }
                    found.searched += 1;
                    let fa = rigid(parts[i].field, coms[i], placed[i].0, placed[i].1);
                    let fb = rigid(parts[j].field, coms[j], placed[j].0, placed[j].1);
                    let gap = deepest_overlap(&fa, &fb, lo, hi, GAP_TOL);
                    if gap.gap < -TOUCH {
                        found.overlaps.push(Overlap { a: i, b: j, depth: -gap.gap, at: gap.at, pose: k });
                    }
                }
            }
        }
        found.overlaps.sort_by(|x, y| y.depth.total_cmp(&x.depth));
        Ok(found)
    })
}

/// The answer: check (fail on any overlap; not checked when an object could not be searched), each
/// object's poses and the pair-poses searched, the overlaps (deepest first) and the pairs left out
/// by design; what could not be searched, with why.
fn answer(found: &[(Arc<Mech>, Found)], open: &[(String, String)]) -> S {
    let name = |m: &Mech, i: usize| s(m.names.get(i).cloned().unwrap_or_default());
    let overlaps: Vec<S> = found.iter().flat_map(|(m, f)| f.overlaps.iter().map(move |o| line(obj(vec![
        ("a", name(m, o.a)),
        ("b", name(m, o.b)),
        ("depth", mm(o.depth)),
        ("at", mm3(o.at)),
        ("pose", s(&f.poses[o.pose])),
    ])))).collect();
    let skipped: Vec<S> = found.iter().flat_map(|(m, f)| f.skipped.iter().map(move |(a, b, why)| line(obj(vec![("a", name(m, *a)), ("b", name(m, *b)), ("why", s(*why))])))).collect();
    let objects: Vec<S> = found.iter().map(|(m, f)| line(obj(vec![
        ("object", s(&m.instance)),
        ("parts", int(m.names.len())),
        ("poses", arr(f.poses.iter().map(s))),
        ("searched", int(f.searched)),
    ]))).collect();
    let check = if !overlaps.is_empty() {
        Check::Fail
    } else if let Some((object, why)) = open.first() {
        Check::NotChecked(format!("{object}: {why}"))
    } else {
        Check::Pass
    };
    let mut fields = vec![("objects", arr(objects)), ("overlaps", arr(overlaps))];
    if !skipped.is_empty() {
        fields.push(("skipped", arr(skipped)));
    }
    if !open.is_empty() {
        let rows: Vec<(String, String)> = open.iter().map(|(object, why)| (format!("interference of {object}"), why.clone())).collect();
        fields.push(("not_checked", not_checked(&rows)));
    }
    let mut v = obj(fields);
    lead(&mut v, check.fields());
    v
}

impl Session {
    /// `last`'s joint checks of `mech`, from the cache when `last` is the build they were made of.
    pub(crate) fn joint_checks_of(&mut self, ev: &Evaluated, mech: &Mech) -> Arc<Vec<JointCheck>> {
        match &self.joint_checks {
            Some((content, instance, checks)) if *content == ev.content && *instance == mech.instance => checks.clone(),
            _ => {
                let checks = Arc::new(mech.check(&|| false));
                self.joint_checks = Some((ev.content, mech.instance.clone(), checks.clone()));
                checks
            }
        }
    }

    /// fab.interference: the jointed object asked for (its instance), else every one.
    pub(crate) fn interference_tool(&mut self, args: &Value, cancelled: Option<&dyn Fn() -> bool>) -> Reply {
        let ev = self.last.clone().unwrap();
        let wanted = str_arg(args, "object").map(|o| o.split('/').next().unwrap_or(o).to_string());
        let mechs: Vec<Arc<Mech>> = ev.mech.iter().filter(|m| wanted.as_ref().is_none_or(|w| m.instance == *w)).cloned().collect();
        if mechs.is_empty() {
            if let Some(w) = wanted {
                return Reply::err(format!("'{w}' has no joints: none of its parts move"));
            }
            let mut v = obj(vec![]);
            lead(&mut v, Check::NotChecked("no object on the plate has joints: nothing moves".into()).fields());
            return Reply::ok(v);
        }
        let never = || false;
        let cancelled = cancelled.unwrap_or(&never);
        let (mut found, mut open) = (Vec::new(), Vec::new());
        for m in mechs {
            if cancelled() {
                return Reply::err("cancelled");
            }
            let checks = self.joint_checks_of(&ev, &m);
            match find(&m, &checks, cancelled) {
                Ok(f) => found.push((m, f)),
                Err(e) if e == "cancelled" => return Reply::err(e),
                Err(e) => open.push((m.instance.clone(), e)),
            }
        }
        Reply::ok(answer(&found, &open))
    }
}
