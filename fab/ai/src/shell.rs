//! `fab.shell`: an exact part hollowed, open at its picked faces, by one number, the wall. The
//! view's Shell drags it at a handle on the face picked last; the AI gives it. Both write the same
//! edit: the part's body becomes `m.brep.shell(body, wall, {open: [faces]})` right after what it
//! was (`Edit::After`, the part keeping its name), and once that is written, its thickness. A
//! part whose body is a shell already has that shell's thickness set instead, from where it is. The plan is read
//! from the picks and the committed program; nothing is built for it.
use crate::picks::{Kind, Named, Pick};
use crate::round::{mm_arg, site_or_param, unit, written, Plan};
use crate::hoist::Own;
use crate::{Reply, Session};
use fab_model::params::{Kind as PKind, Template, Writes, AT_VALUE};
use makepad_model_script::lang::edit;

/// The tool's name, for the AI and for the view's own shells.
pub const TOOL: &str = "shell";

/// What a pick must be for a shell.
const NOTHING: &str = "pick the faces to leave open first (E): faces of one exact part";

/// The thinnest and thickest wall.
const LEAST: f64 = 0.1;
const MOST: f64 = 50.0;

impl Session {
    /// The shell of the faces among picks `ids` (none: all of them), open at them.
    pub fn shell_plan(&mut self, ids: &[String]) -> Result<Plan, String> {
        let picks = self.chosen_picks(ids)?;
        let faces: Vec<&Pick> = picks.iter().filter(|p| p.kind == Kind::Face).collect();
        let last = *faces.last().ok_or(NOTHING)?;
        let object = last.object.clone();
        if faces.iter().any(|p| p.object != object) {
            return Err("the picked faces are on different parts: shell one part at a time".into());
        }
        let b = self.exact_body(&last.on)?.ok_or_else(|| format!("`{}` is not an exact part: shell takes an exact part's faces", last.on))?;
        let params = self.params()?;
        let (inst, k) = self.build_on(&faces)?;
        // a wall past half the part's thinnest extent leaves nothing inside (its inner faces would
        // cross), so the drag stops short of that; a box's is exact, any other part's an upper bound
        let thinnest = (0..3).map(|i| b.measure.max[i] - b.measure.min[i]).fold(f64::INFINITY, f64::min);
        let most = (thinnest / 2.0 - 0.1).min(MOST);
        if most < LEAST {
            return Err(format!("`{}` is {thinnest:.1} mm at its thinnest: too thin to shell", last.on));
        }
        let program = &inst.script.program;
        let solid = &program.solids[k];
        if !solid.named || solid.call.is_some() {
            return Err(format!("`{}` has no `let` of its own at the top level: name its body to shell it", solid.id));
        }
        let sid = solid.id.clone();
        let (at, dir) = match last.at {
            Some((q, n)) => (q, unit(n).unwrap_or([0.0, 1.0, 0.0])),
            None => (std::array::from_fn(|i| (b.measure.min[i] + b.measure.max[i]) / 2.0), [0.0, 1.0, 0.0]),
        };
        let file = inst.script.file.clone();
        let plan = |template: Template, then: Option<Template>, says: String, value: f64| Plan {
            object: object.clone(),
            label: "Shell".into(),
            says,
            template,
            then,
            value,
            min: LEAST,
            max: most,
            step: 0.1,
            at,
            dir,
            picks: faces.iter().map(|p| p.id.clone()).collect(),
        };
        // a shell already: its wall
        if solid.proto == "BrepShell" {
            let proto = makepad_model_script::lang::protos::proto(solid.proto).ok_or_else(|| format!("`{sid}` has no builder"))?;
            let call = edit::part_call(program, solid)?;
            let syn = &program.names.syntax;
            let fields = edit::call_fields(syn, &call, proto);
            let node = fields.iter().find(|(f, _)| f == "thickness").and_then(|(_, n)| syn.node(n.loc()));
            let (writes, base) = site_or_param(&params, inst, &sid, "thickness", node)?.ok_or_else(|| format!("`{sid}`'s thickness is not written in its call"))?;
            let template = Template { file, kind: PKind::Length, writes };
            return Ok(plan(template, None, format!("`{sid}` is a shell already: its wall"), base));
        }
        // (each by its copy's own name, where the body holds the part twice)
        let mut own = Own::new(program, b);
        let names: Vec<String> = faces
            .iter()
            .map(|p| match &p.named {
                Some(Named::Face(name)) => own.face(name, p.at.map(|(q, _)| q)).map(|(_, name)| name),
                _ => Err(format!("pick {} has no name a script can write", p.id)),
            })
            .collect::<Result<_, _>>()?;
        let open = if names.len() == 1 { names[0].clone() } else { format!("[{}]", names.join(" ")) };
        let count = names.len();
        let says = format!("hollows `{sid}`, open at {count} face{}: `{sid}` becomes its shell", if count == 1 { "" } else { "s" });
        let template = Template { file: file.clone(), kind: PKind::Length, writes: own.writes(Writes::After { solid: sid.clone(), code: format!("m.brep.shell({sid}, {AT_VALUE}, {{open: {open}}})") }) };
        let then = Template { file, kind: PKind::Length, writes: Writes::Set { site: format!("{sid}.thickness") } };
        Ok(plan(template, Some(then), says, 0.0))
    }

    /// The AI's shell: the picked faces (`picks`, default all) left open, walls `thickness` mm,
    /// committed as one undo step; without `thickness`, what it would write.
    pub(crate) fn shell_tool(&mut self, args: &makepad_model::json::Value) -> Reply {
        use crate::out::{arr, n, obj, s};
        let ids = crate::round::pick_ids(args);
        let plan = match self.shell_plan(&ids) {
            Ok(plan) => plan,
            Err(e) => return Reply::err(e),
        };
        let Some(t) = args.get("thickness").and_then(mm_arg) else {
            let writes = written(&plan.template, "<thickness>");
            return Reply::ok(obj(vec![("label", s(&plan.label)), ("says", s(&plan.says)), ("object", s(&plan.object)), ("min", n(plan.min)), ("max", n(plan.max)), ("writes", s(writes)), ("picks", arr(plan.picks.iter().map(s)))]));
        };
        if !(plan.min..=plan.max).contains(&t) {
            return Reply::err(format!("shell takes {:.1} to {:.1} mm here", plan.min, plan.max));
        }
        self.commit(TOOL, plan.template.changes(t))
    }
}
