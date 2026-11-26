//! What the AI reads to check its own work. `built`: what an edit built, read in the model's own
//! frame, so the AI holds it against what was asked before anything else. [`Check`]: every check
//! answers pass, fail or not checked, and a check that did not run says why. The person's numbers
//! (`m.expect`): how each build judges them, and what an edit changed of them.

use crate::out::{arr, int, line, mm3, n, name, obj, s, S};
use fab_model::eval::{Engine, Evaluated};
use std::collections::HashMap;

/// Placed part `i` of `after` against placed part `j` of `before` (printed, then excluded):
/// whether its size, volume, triangle count or field moved.
pub(crate) fn part_moved(before: &Evaluated, j: usize, after: &Evaluated, i: usize) -> bool {
    let (p, q) = (after.placed_part(i), before.placed_part(j));
    let (sp, sq) = (p.bounds().size(), q.bounds().size());
    let (vp, vq) = (super::part_volume(after, i), super::part_volume(before, j));
    let volume_moved = match (vp, vq) {
        (Some(Some(vp)), Some(Some(vq))) => (vp - vq).abs() > 1e-3,
        _ => vp != vq,
    };
    let key = |ev: &Evaluated, k: usize| ev.placed_field(k).map(|f| f.key);
    (0..3).any(|k| (sp[k] - sq[k]).abs() > 1e-3) || volume_moved || p.mesh.triangles.len() != q.mesh.triangles.len() || key(after, i) != key(before, j)
}

/// What a build made: the plate's part and joint counts, then each object (script instance) with
/// its parts, joints and size, and each body with its engine, size and volume. Sizes are in the
/// model frame (Y up), as the script writes them. `before`: the build this one follows; only the
/// bodies it added or changed are listed, and the objects they are in (an edit's answer stays
/// small on a large plate). None, or `all`: every one (`fab.inspect`).
pub fn built_value(before: Option<&Evaluated>, ev: &Evaluated, all: bool) -> S {
    let placed: Vec<&fab_plate::Part> = ev.plate.placed_parts().collect();
    let printed = ev.plate.parts.len();
    let joints_of = |instance: &str| ev.mech.iter().filter(|m| m.instance == instance).map(|m| m.joint_ids.len()).sum::<usize>();
    let infos: HashMap<&str, &fab_model::eval::ObjectInfo> = ev.objects.iter().map(|o| (o.name.as_str(), o)).collect();
    let listed: Vec<usize> = match before.filter(|_| !all) {
        None => (0..placed.len()).collect(),
        Some(b) => {
            let was: HashMap<&str, usize> = b.plate.placed_parts().enumerate().map(|(j, q)| (q.object.as_str(), j)).collect();
            (0..placed.len()).filter(|&i| was.get(placed[i].object.as_str()).is_none_or(|&j| part_moved(b, j, ev, i))).collect()
        }
    };
    let instance_of = |p: &fab_plate::Part| fab_model::program::split_id(&p.object).map_or(p.object.clone(), |(inst, _)| inst.to_string());
    let model_box = |p: &fab_plate::Part| infos.get(p.object.as_str()).and_then(|o| o.bounds);
    // the objects the listed bodies are in, in plate order, each with all its bodies
    let mut instances: Vec<(String, Vec<usize>)> = Vec::new();
    for (i, p) in placed.iter().enumerate() {
        let inst = instance_of(p);
        match instances.iter_mut().find(|(known, _)| *known == inst) {
            Some((_, bodies)) => bodies.push(i),
            None => instances.push((inst, vec![i])),
        }
    }
    instances.retain(|(_, bodies)| bodies.iter().any(|i| listed.contains(i)));
    let objects = instances.iter().map(|(inst, bodies)| {
        let boxes: Vec<([f64; 3], [f64; 3])> = bodies.iter().filter_map(|&i| model_box(placed[i])).collect();
        let size = boxes.iter().copied().reduce(|a, b| (std::array::from_fn(|k| a.0[k].min(b.0[k])), std::array::from_fn(|k| a.1[k].max(b.1[k])))).map(|(lo, hi)| mm3(std::array::from_fn(|k| hi[k] - lo[k])));
        let mut row = vec![("instance", s(inst)), ("parts", int(bodies.len())), ("joints", int(joints_of(inst)))];
        if let Some(size) = size {
            row.push(("size", size));
        }
        // a placement's scale is the bed's (printer x y z of the unturned part), not the model's
        if let Some(scale) = bodies.first().and_then(|&i| ev.placed_handles(i)).map(|h| h.scale).filter(|k| k.iter().any(|f| (f - 1.0).abs() > 1e-9)) {
            row.push(("placed_scale", arr(scale.iter().map(|f| n(*f)))));
        }
        line(obj(row))
    });
    let mut pending = false;
    let bodies: Vec<S> = listed.iter().map(|&i| {
        let p = placed[i];
        let engine = ev.engine(i);
        let exact = infos.get(p.object.as_str()).and_then(|o| o.exact.as_ref()).filter(|_| matches!(engine, Engine::Exact)).map(|m| m.volume);
        let mut row = vec![("part", s(&p.object)), ("engine", name(engine.name()))];
        if let Some((lo, hi)) = model_box(p) {
            row.push(("size", mm3(std::array::from_fn(|k| hi[k] - lo[k]))));
        }
        match exact.map(Some).or_else(|| super::part_volume(ev, i)) {
            Some(Some(v)) => row.push(("volume_mm3", n(v))),
            Some(None) => {
                pending = true;
                row.push(("volume_mm3", s("pending")));
            }
            None => {}
        }
        if i >= printed {
            row.push(("print", crate::out::flag(false)));
        }
        line(obj(row))
    }).collect();
    let mut fields = vec![
        ("parts", int(placed.len())),
        ("joints", int(ev.mech.iter().map(|m| m.joint_ids.len()).sum::<usize>())),
        ("objects", arr(objects)),
        (if all || before.is_none() { "bodies" } else { "changed" }, arr(bodies)),
    ];
    if let Some(expects) = expects_value(ev, false) {
        fields.push(("expects", expects));
    }
    if pending {
        fields.push(("volume_pending", s("a field body's volume is read from its field in the background once the build is in; fab.inspect has it then. Pending is not a failure.")));
    }
    obj(fields)
}

/// A check's answer: it ran and passed, it ran and failed, or it did not run, with why.
#[derive(Clone, Debug, PartialEq)]
pub enum Check {
    Pass,
    Fail,
    NotChecked(String),
}

impl Check {
    /// Pass or fail.
    pub fn of(pass: bool) -> Check {
        if pass { Check::Pass } else { Check::Fail }
    }

    /// `check: @pass` or `@fail`, or `check: @not_checked` and its `why`.
    pub fn fields(&self) -> Vec<(&'static str, S)> {
        match self {
            Check::Pass => vec![("check", name("pass"))],
            Check::Fail => vec![("check", name("fail"))],
            Check::NotChecked(why) => vec![("check", name("not_checked")), ("why", s(why))],
        }
    }
}

/// What did not run, each with why: `[{check why}]`.
pub fn not_checked(rows: &[(String, String)]) -> S {
    arr(rows.iter().map(|(check, why)| line(obj(vec![("check", s(check)), ("why", s(why))]))))
}

/// `answer` (an object) led by `fields`: a check's answer says its verdict first. A field it
/// already has of the same name moves to the front with the new value.
pub fn lead(answer: &mut S, fields: Vec<(&str, S)>) {
    let S::Object(_, have) = answer else { return };
    for (key, value) in fields.into_iter().rev() {
        have.retain(|(k, _)| k != key);
        have.insert(0, (key.to_string(), value));
    }
}

/// Whether any of a check's findings (`{severity ..}` rows) is an error.
pub fn has_error(findings: &[S]) -> bool {
    findings.iter().any(|f| matches!(f, S::Object(_, fields) if fields.iter().any(|(k, v)| k == "severity" && matches!(v, S::Str(x) | S::Name(x) if x == "error"))))
}

/// A judged expect's verdict as a check's.
pub fn of_verdict(v: fab_model::expect::Verdict) -> Check {
    match v {
        fab_model::expect::Verdict::Pass => Check::Pass,
        fab_model::expect::Verdict::Fail => Check::Fail,
        fab_model::expect::Verdict::NotChecked(why) => Check::NotChecked(why),
    }
}

/// One judged expect: its check, its name, where it is written and what each want saw.
pub fn expect_row(j: &fab_model::expect::Judged) -> S {
    let mut row = of_verdict(j.verdict()).fields();
    row.push(("expect", s(&j.expect.name)));
    row.push(("instance", s(&j.instance)));
    row.push(("at", s(format!("{}:{}", j.file, j.expect.loc.map_or(1, |l| l.0)))));
    row.push(("seen", arr((0..j.expect.wants.len()).map(|k| s(j.seen_words(k))))));
    line(obj(row))
}

/// The person's numbers (`m.expect`) as `ev` judged them; None without any. `all` (fab.check): a
/// row each. Else (an edit's `built`): the counts, and a row for each that did not pass.
pub fn expects_value(ev: &Evaluated, all: bool) -> Option<S> {
    use fab_model::expect::Verdict;
    if ev.expects.is_empty() {
        return None;
    }
    if all {
        return Some(arr(ev.expects.iter().map(expect_row)));
    }
    let verdicts: Vec<Verdict> = ev.expects.iter().map(|j| j.verdict()).collect();
    let mut fields = vec![
        ("pass", int(verdicts.iter().filter(|v| **v == Verdict::Pass).count())),
        ("fail", int(verdicts.iter().filter(|v| **v == Verdict::Fail).count())),
        ("not_checked", int(verdicts.iter().filter(|v| matches!(v, Verdict::NotChecked(_))).count())),
    ];
    let open: Vec<S> = ev.expects.iter().zip(&verdicts).filter(|(_, v)| **v != Verdict::Pass).map(|(j, _)| expect_row(j)).collect();
    if !open.is_empty() {
        fields.push(("not_passing", arr(open)));
    }
    Some(obj(fields))
}

/// What a batch did to the person's numbers (`m.expect`), script by script, one plain line each
/// ([`fab_model::expect::changes`]): the AI says so in its reply.
pub fn expects_changed(before: &fab_model::program::Compiled, after: &fab_model::program::Compiled) -> Vec<String> {
    let scripts = |c: &fab_model::program::Compiled| {
        let mut by_file: Vec<(String, Vec<fab_model::expect::Expect>)> = Vec::new();
        for i in &c.instances {
            if !by_file.iter().any(|(f, _)| *f == i.script.file) {
                by_file.push((i.script.file.clone(), i.script.expects.clone()));
            }
        }
        by_file
    };
    let (was, now) = (scripts(before), scripts(after));
    let mut lines = Vec::new();
    for (file, expects) in &now {
        let old = was.iter().find(|(f, _)| f == file).map_or(&[][..], |(_, e)| e.as_slice());
        lines.extend(fab_model::expect::changes(old, expects));
    }
    // a script the batch removed takes its expects with it
    for (_, expects) in was.iter().filter(|(f, _)| !now.iter().any(|(g, _)| g == f)) {
        lines.extend(fab_model::expect::changes(expects, &[]));
    }
    lines
}
