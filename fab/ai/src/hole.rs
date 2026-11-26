//! `fab.hole`: a hole drilled where a flat face of an exact part was pointed at, by one number,
//! its diameter. The view's Hole drags it at a handle on the spot; the AI gives it (and a depth).
//! Both write the same edit: the part's body becomes
//! `m.brep.hole(body, {on: <face> at: [x y z] diameter: d})` right after what it was
//! (`Edit::After`, the part keeping its name), and once that is written, its diameter. The hole
//! starts on its face (`on:`) wherever the face goes; its point is in the body's own frame (a
//! placed body is drilled before its placement), so it moves with the body; it runs through the
//! part unless given a depth. The plan is read from the pick, the exact body and the committed program; nothing is
//! built for it.
use crate::picks::{Kind, Named};
use crate::round::{mm_arg, unit, written, Plan};
use crate::hoist::Own;
use crate::{Reply, Session};
use fab_model::params::{Kind as PKind, Template, Writes, AT_VALUE};

/// The tool's name, for the AI and for the view's own holes.
pub const TOOL: &str = "hole";

/// What a pick must be for a hole.
const NOTHING: &str = "pick the spot on a flat face of an exact part to drill first (E)";

/// The narrowest and widest hole.
const LEAST: f64 = 0.3;
const MOST: f64 = 100.0;

/// Point `w` that placement `m` put somewhere, back in the body's own frame (None: `m` flattens).
fn unplace(m: &[[f64; 4]; 3], w: [f64; 3]) -> Option<[f64; 3]> {
    let d = [w[0] - m[0][3], w[1] - m[1][3], w[2] - m[2][3]];
    // Cramer's rule on the matrix's 3x3, column `k` (if any) taken by `d`
    let det = |k: Option<usize>| {
        let a = |i: usize, j: usize| if Some(j) == k { d[i] } else { m[i][j] };
        a(0, 0) * (a(1, 1) * a(2, 2) - a(1, 2) * a(2, 1)) - a(0, 1) * (a(1, 0) * a(2, 2) - a(1, 2) * a(2, 0)) + a(0, 2) * (a(1, 0) * a(2, 1) - a(1, 1) * a(2, 0))
    };
    let whole = det(None);
    (whole.abs() > 1e-12).then(|| [det(Some(0)) / whole, det(Some(1)) / whole, det(Some(2)) / whole])
}

/// `v` mm as a script writes it, to a hundredth.
fn mm(v: f64) -> String {
    let t = format!("{:.2}", if v.abs() < 0.005 { 0.0 } else { v });
    format!("{}mm", t.trim_end_matches('0').trim_end_matches('.'))
}

impl Session {
    /// A hole at the face picked last among picks `ids` (none: all of them), where it was
    /// pointed at (no point: the face's own), `depth` deep (none: through).
    pub fn hole_plan(&mut self, ids: &[String], depth: Option<f64>) -> Result<Plan, String> {
        let picks = self.chosen_picks(ids)?;
        let faces: Vec<_> = picks.iter().filter(|p| p.kind == Kind::Face).collect();
        let p = *faces.last().ok_or(NOTHING)?;
        let Some(Named::Face(name)) = &p.named else { return Err(NOTHING.into()) };
        let b = self.exact_body(&p.on)?.ok_or_else(|| format!("`{}` is not an exact part: a hole from a pick drills an exact part's face", p.on))?;
        let (inst, k) = self.build_on(&faces)?;
        // (by its copy's own name, where the body holds the part twice)
        let mut copies = Own::new(&inst.script.program, b);
        let (face, name) = copies.face(name, p.at.map(|(q, _)| q))?;
        let face = copies.body.faces.iter().find(|f| f.id == face).expect("copies.face found it");
        let normal = face.normal.and_then(unit).ok_or_else(|| format!("a hole starts on a flat face: `{name}` is curved"))?;
        let solid = &inst.script.program.solids[k];
        if !solid.named || solid.call.is_some() {
            return Err(format!("`{}` has no `let` of its own at the top level: name its body to drill it", solid.id));
        }
        let sid = solid.id.clone();
        // the spot: where the face was pointed at, else a point of the face; written in the
        // body's own frame
        let at = p.at.map_or(face.at, |(q, _)| q);
        let frame = makepad_csg_graph::eval::drill_frame(&inst.script.program.graph, solid.node);
        let own = unplace(&frame, at).unwrap_or(at);
        let spot = makepad_script::syntax::write::list(own.map(mm));
        let deep = depth.map_or(String::new(), |d| format!(" depth: {}", mm(d)));
        let code = format!("m.brep.hole({sid}, {{on: {name} at: {spot} diameter: {AT_VALUE}{deep}}})");
        let file = inst.script.file.clone();
        Ok(Plan {
            object: p.object.clone(),
            label: "Hole".into(),
            says: format!("drills `{name}` at {spot}, {}: `{sid}` becomes its hole", depth.map_or("through".to_string(), |d| format!("{} deep", mm(d)))),
            template: Template { file: file.clone(), kind: PKind::Diameter, writes: copies.writes(Writes::After { solid: sid.clone(), code }) },
            then: Some(Template { file, kind: PKind::Diameter, writes: Writes::Set { site: format!("{sid}.diameter") } }),
            value: 0.0,
            min: LEAST,
            max: MOST,
            step: 0.1,
            at,
            dir: normal,
            picks: vec![p.id.clone()],
        })
    }

    /// The AI's hole: at the face picked last (`picks`, default all), `diameter` mm, `depth` mm
    /// (none: through), committed as one undo step; without `diameter`, what it would write.
    pub(crate) fn hole_tool(&mut self, args: &makepad_model::json::Value) -> Reply {
        use crate::out::{arr, n, obj, s};
        let ids = crate::round::pick_ids(args);
        let depth = args.get("depth").and_then(mm_arg);
        if depth.is_some_and(|d| d <= 0.0) {
            return Reply::err("a hole's depth is above 0 (leave it out for a hole through)");
        }
        let plan = match self.hole_plan(&ids, depth) {
            Ok(plan) => plan,
            Err(e) => return Reply::err(e),
        };
        let Some(d) = args.get("diameter").and_then(mm_arg) else {
            let writes = written(&plan.template, "<diameter>");
            return Reply::ok(obj(vec![("label", s(&plan.label)), ("says", s(&plan.says)), ("object", s(&plan.object)), ("min", n(plan.min)), ("max", n(plan.max)), ("writes", s(writes)), ("picks", arr(plan.picks.iter().map(s)))]));
        };
        if !(plan.min..=plan.max).contains(&d) {
            return Reply::err(format!("a hole takes {} to {} mm across", plan.min, plan.max));
        }
        self.commit(TOOL, plan.template.changes(d))
    }
}
