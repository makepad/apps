//! The numbers a person wants a build to meet, written beside the solids they check:
//! `m.expect("8 mm hole", hole, {x: 8mm z: 8mm})`. The evaluator judges each on every build
//! ([`Judged`], `Evaluated::expects`); an AI's edit that makes one fail is refused
//! ([`newly_failing`]), a person's commits with it as a warning.

use crate::program::UNIT;
use makepad_model_script::lang::{Arg, Diag, Obj, Program, V};
use std::collections::{HashMap, VecDeque};

/// What an expect measures.
#[derive(Clone, Debug, PartialEq)]
pub enum Measure {
    /// Width along the model's X axis, in millimetres.
    X,
    /// Width along the model's Y axis, in millimetres.
    Y,
    /// Width along the model's Z axis, in millimetres.
    Z,
    /// Volume in cubic millimetres.
    VolumeMm3,
    /// Number of solids.
    Count,
    /// Wall thickness at a point.
    Wall {
        /// The point in the model frame, in millimetres.
        at: [f64; 3],
    },
}

impl Measure {
    /// The measure's key as written in the script.
    pub fn key(&self) -> &'static str {
        match self {
            Self::X => "x",
            Self::Y => "y",
            Self::Z => "z",
            Self::VolumeMm3 => "volume_mm3",
            Self::Count => "count",
            Self::Wall { .. } => "wall",
        }
    }
}

/// What the person wants of one measure.
#[derive(Clone, Debug, PartialEq)]
pub struct Want {
    /// What to measure.
    pub measure: Measure,
    /// A target value, when the person gives one.
    pub value: Option<f64>,
    /// The least allowed value of a range.
    pub min: Option<f64>,
    /// The greatest allowed value of a range.
    pub max: Option<f64>,
    /// How far a measured value may be from a target, in the measure's units.
    pub tol: f64,
}

/// One `m.expect(name, solid, {wants})` statement as written.
#[derive(Clone, Debug, PartialEq)]
pub struct Expect {
    /// The person's words for the check.
    pub name: String,
    /// Indices in the program's solids, in the order given.
    pub solids: Vec<usize>,
    /// The measures the person asks to check.
    pub wants: Vec<Want>,
    /// The call's line and column in the script, counted from one.
    pub loc: Option<(u32, u32)>,
}

/// The result of checking one measure.
#[derive(Clone, Debug, PartialEq)]
pub enum Verdict {
    /// The measured value meets the check.
    Pass,
    /// The measured value falls outside the check.
    Fail,
    /// Why the value could not be measured.
    NotChecked(String),
}

/// The margin in millimetres for a length target with no written tolerance.
const DEFAULT_TOL: f64 = 0.05;

/// The expects in script order, or the errors at the calls that contain them.
pub fn compile(p: &Program) -> Result<Vec<Expect>, Vec<Diag>> {
    let mut expects = Vec::new();
    let mut errors = Vec::new();
    for root in p.roots.iter().filter(|r| r.kind == "expect") {
        let (line, col) = root.loc.unwrap_or((1, 1));
        let mut error = |message: String| errors.push(Diag { line, col, message: format!("m.expect: {message}") });
        let name = match root.get("name") {
            Some(Arg::Value(V::Str(name))) if !name.is_empty() => Some(name),
            _ => {
                error(r#"give it the person's words first: m.expect("8 mm hole", hole, {x: 8mm})"#.into());
                None
            }
        };
        let solids = match root.get("solid") {
            Some(Arg::Solid(i)) => Some(vec![*i]),
            Some(Arg::Solids(ids)) => Some(ids.clone()),
            _ => {
                error("the second argument is the solid it checks (a let name, or a list of them)".into());
                None
            }
        };
        let Some(Arg::Value(V::Obj(obj))) = root.get("want") else {
            error("give it an object of measures, such as {x: 8mm}".into());
            continue;
        };
        let wants = read_wants(obj, &mut error);
        if let (Some(name), Some(solids)) = (name, solids) {
            expects.push(Expect { name: name.clone(), solids, wants, loc: root.loc });
        }
    }
    if errors.is_empty() { Ok(expects) } else { Err(errors) }
}

/// A finite number in the units used to make the check.
fn number(v: &V, scale: f64) -> Option<f64> {
    v.num().map(|n| n * scale).filter(|n| n.is_finite())
}

/// Three lengths in the model frame, converted to millimetres.
fn point(v: &V) -> Option<[f64; 3]> {
    let V::Arr(coords) = v else { return None };
    let [x, y, z] = coords.as_slice() else { return None };
    Some([number(x, UNIT)?, number(y, UNIT)?, number(z, UNIT)?])
}

/// Read one number for a measure, naming the bound when it is part of a range.
fn target(key: &str, field: &str, raw: &V, error: &mut impl FnMut(String)) -> Option<f64> {
    let scale = if matches!(key, "count" | "volume_mm3") { 1.0 } else { UNIT };
    let Some(n) = number(raw, scale) else {
        error(format!("{key}{field} needs a finite number"));
        return None;
    };
    if key == "count" && n.fract() != 0.0 {
        error(format!("{key}{field} must be a whole number"));
        return None;
    }
    Some(n)
}

/// Read a target value or the lower and upper bounds the person gave.
fn targets(key: &str, raw: &V, error: &mut impl FnMut(String)) -> (Option<f64>, Option<f64>, Option<f64>) {
    let V::Obj(range) = raw else { return (target(key, "", raw, error), None, None) };
    for (field, _) in &range.fields {
        if !matches!(field.as_str(), "min" | "max") {
            error(format!("unknown key '{field}' in {key}. A range takes min and max"));
        }
    }
    if !range.items.is_empty() || (range.get("min").is_none() && range.get("max").is_none()) {
        error(format!("{key} needs a range with min, max, or both"));
    }
    let min = range.get("min").and_then(|v| target(key, ".min", v, error));
    let max = range.get("max").and_then(|v| target(key, ".max", v, error));
    if matches!((min, max), (Some(lo), Some(hi)) if lo > hi) {
        error(format!("{key}.min must be at most {key}.max"));
    }
    (None, min, max)
}

/// Read each measure in a stable order and report each bad field at the call.
fn read_wants(obj: &Obj, error: &mut impl FnMut(String)) -> Vec<Want> {
    /// The order the script's measures are listed in a change.
    const KEYS: [&str; 6] = ["x", "y", "z", "volume_mm3", "count", "wall"];
    for (key, _) in &obj.fields {
        if !KEYS.contains(&key.as_str()) && !matches!(key.as_str(), "tol" | "at") {
            error(format!("unknown key '{key}'. The check takes x, y, z, volume_mm3, count, wall, tol and at"));
        }
    }
    if !obj.items.is_empty() || !KEYS.iter().any(|key| obj.get(key).is_some()) {
        error("give it named measures from x, y, z, volume_mm3, count and wall".into());
    }
    let length_tol = match obj.get("tol") {
        None => DEFAULT_TOL,
        Some(v) => number(v, UNIT).filter(|n| *n >= 0.0).unwrap_or_else(|| {
            error("tol must be a non-negative length".into());
            DEFAULT_TOL
        }),
    };
    if obj.get("at").is_some() && obj.get("wall").is_none() {
        error("at gives the point for a wall check and needs wall".into());
    }
    let mut wants = Vec::new();
    for key in KEYS {
        let Some(raw) = obj.get(key) else { continue };
        let (value, min, max) = targets(key, raw, error);
        let measure = match key {
            "x" => Measure::X,
            "y" => Measure::Y,
            "z" => Measure::Z,
            "volume_mm3" => Measure::VolumeMm3,
            "count" => Measure::Count,
            "wall" => {
                let Some(at) = obj.get("at").and_then(point) else {
                    error("wall needs at: [x y z] with three lengths in the model frame".into());
                    continue;
                };
                Measure::Wall { at }
            }
            _ => unreachable!(),
        };
        let tol = match measure {
            Measure::VolumeMm3 => value.map_or(0.0, |v| v.abs() * 0.01),
            Measure::Count => 0.0,
            _ => length_tol,
        };
        wants.push(Want { measure, value, min, max, tol });
    }
    wants
}

/// Judge one want against a measurement or the reason it could not be made.
pub fn judge(want: &Want, seen: Result<f64, String>) -> Verdict {
    let seen = match seen {
        Ok(n) => n,
        Err(why) => return Verdict::NotChecked(why),
    };
    let holds = match want.value {
        Some(value) => match want.measure {
            Measure::Count => seen == value,
            Measure::VolumeMm3 => (seen - value).abs() <= value.abs() * 0.01,
            _ => (seen - value).abs() <= want.tol,
        },
        None => want.min.is_none_or(|min| seen >= min - 1e-9) && want.max.is_none_or(|max| seen <= max + 1e-9),
    };
    if holds { Verdict::Pass } else { Verdict::Fail }
}

/// A length in millimetres with up to three decimal places.
fn mm(n: f64) -> String {
    let rounded = format!("{n:.3}");
    let digits = rounded.trim_end_matches('0').trim_end_matches('.');
    if digits == "-0" { "0".into() } else { digits.into() }
}

/// The target or range in words, with the unit written once.
pub fn describe(want: &Want) -> String {
    let unit = match want.measure {
        Measure::Count => "",
        Measure::VolumeMm3 => " mm³",
        _ => " mm",
    };
    let digits = |n: f64| if unit == " mm" { mm(n) } else { n.to_string() };
    match (want.value, want.min, want.max) {
        (Some(v), _, _) => format!("{}{unit}", digits(v)),
        (_, Some(lo), Some(hi)) => format!("{} to {}{unit}", digits(lo), digits(hi)),
        (_, Some(lo), _) => format!("at least {}{unit}", digits(lo)),
        (_, _, Some(hi)) => format!("at most {}{unit}", digits(hi)),
        _ => "any value".into(),
    }
}

/// Include a changed tolerance or wall point so the words say what moved.
fn details(want: &Want, tolerance: bool, point: bool) -> String {
    let mut words = describe(want);
    if tolerance && !matches!(want.measure, Measure::Count | Measure::VolumeMm3) {
        words.push_str(&format!(" (tol {} mm)", mm(want.tol)));
    }
    if let Measure::Wall { at } = &want.measure {
        if point {
            words.push_str(&format!(" at [{} {} {}] mm", mm(at[0]), mm(at[1]), mm(at[2])));
        }
    }
    words
}

/// What an edit changed, added or removed, matching checks by name and wants by measure.
pub fn changes(before: &[Expect], after: &[Expect]) -> Vec<String> {
    let mut by_name: HashMap<&str, VecDeque<usize>> = HashMap::new();
    for (i, check) in after.iter().enumerate() {
        by_name.entry(&check.name).or_default().push_back(i);
    }
    let mut matched = vec![false; after.len()];
    let mut lines = Vec::new();
    for old in before {
        let name = &old.name;
        let Some(i) = by_name.get_mut(name.as_str()).and_then(VecDeque::pop_front) else {
            lines.push(format!("removed the person's check `{name}`"));
            continue;
        };
        matched[i] = true;
        let new = &after[i];
        let mut diff = Vec::new();
        for was in &old.wants {
            let key = was.measure.key();
            match new.wants.iter().find(|w| w.measure.key() == key) {
                Some(now) if was != now => {
                    let tol = was.tol != now.tol;
                    let at = was.measure != now.measure;
                    diff.push(format!("{key} {} → {}", details(was, tol, at), details(now, tol, at)));
                }
                None => diff.push(format!("{key} dropped")),
                _ => {}
            }
        }
        for want in &new.wants {
            let key = want.measure.key();
            if !old.wants.iter().any(|w| w.measure.key() == key) {
                diff.push(format!("{key} {} added", details(want, want.tol != DEFAULT_TOL, true)));
            }
        }
        if !diff.is_empty() {
            lines.push(format!("changed the person's check `{name}`: {}", diff.join(", ")));
        }
    }
    for (check, matched) in after.iter().zip(matched) {
        if !matched {
            let wants = check.wants.iter().map(|w| format!("{} {}", w.measure.key(), details(w, w.tol != DEFAULT_TOL, true))).collect::<Vec<_>>();
            lines.push(format!("added the check `{}`: {}", check.name, wants.join(", ")));
        }
    }
    lines
}

/// One expect of one instance judged on its build: what each want saw (mm, mm³ or a count, or why
/// it could not be read) and each want's verdict, in the order they are written.
#[derive(Clone, Debug, PartialEq)]
pub struct Judged {
    pub instance: String,
    /// The script it is written in.
    pub file: String,
    pub expect: Expect,
    pub seen: Vec<Result<f64, String>>,
    pub verdicts: Vec<Verdict>,
}

impl Judged {
    /// Fail when a want fails; else not checked when one could not be read (its why); else pass.
    pub fn verdict(&self) -> Verdict {
        if self.verdicts.contains(&Verdict::Fail) {
            return Verdict::Fail;
        }
        self.verdicts.iter().find(|v| matches!(v, Verdict::NotChecked(_))).cloned().unwrap_or(Verdict::Pass)
    }

    /// What want `k` saw, in words: "x 10 mm, asked 8 mm", or "x not read: why".
    pub fn seen_words(&self, k: usize) -> String {
        let want = &self.expect.wants[k];
        match &self.seen[k] {
            Ok(v) => format!("{} {}, asked {}", want.measure.key(), seen_text(&want.measure, *v), describe(want)),
            Err(why) => format!("{} not read: {why}", want.measure.key()),
        }
    }

    /// What a failing one saw, in words: `m.expect "8 mm hole" fails: x is 10 mm, asked 8 mm`.
    pub fn fails_with(&self) -> String {
        let failing: Vec<String> = self.expect.wants.iter().zip(&self.seen).zip(&self.verdicts).filter(|(_, v)| **v == Verdict::Fail).map(|((w, s), _)| match s {
            Ok(v) => format!("{} is {}, asked {}", w.measure.key(), seen_text(&w.measure, *v), describe(w)),
            Err(why) => format!("{}: {why}", w.measure.key()),
        }).collect();
        format!("m.expect \"{}\" fails: {}", self.expect.name, failing.join("; "))
    }

    /// [`Self::fails_with`] at its line: `box.cad.splash:7:1: m.expect "8 mm hole" fails: ..`.
    pub fn says(&self) -> String {
        let (line, col) = self.expect.loc.unwrap_or((1, 1));
        format!("{}:{line}:{col}: {}", self.file, self.fails_with())
    }
}

/// A measured value in words: lengths to 0.01 mm, a volume to the mm³, a count.
fn seen_text(m: &Measure, v: f64) -> String {
    match m {
        Measure::Count => format!("{}", v.round()),
        Measure::VolumeMm3 => format!("{} mm³", v.round()),
        _ => {
            let t = format!("{v:.2}");
            let t = t.trim_end_matches('0').trim_end_matches('.');
            format!("{t} mm")
        }
    }
}

/// The first expect of `after` that fails where it did not fail in `before` (the build it follows;
/// matched by instance and name), as a build error at its line: an AI's edit that breaks a number
/// the person gave is refused, and says what to do. One already failing stays a warning.
/// `one_shot`: the edit is a tool's (strengthen, orient, volumes), which cannot change an expect in
/// the same batch: the expect changes first, in an edit of its own.
pub fn newly_failing(before: Option<&crate::eval::Evaluated>, after: &crate::eval::Evaluated, one_shot: bool) -> Option<crate::eval::Failure> {
    let failed_before = |j: &Judged| before.is_some_and(|b| b.expects.iter().any(|k| k.instance == j.instance && k.expect.name == j.expect.name && k.verdict() == Verdict::Fail));
    let j = after.expects.iter().find(|j| j.verdict() == Verdict::Fail && !failed_before(j))?;
    let (line, col) = j.expect.loc.unwrap_or((1, 1));
    Some(crate::eval::Failure {
        file: j.file.clone(),
        line,
        col,
        id: String::new(),
        op: String::new(),
        lost: None,
        message: match one_shot {
            false => format!("{}. It is the person's number: keep it, or change the expect in the same batch only if they asked for that number to change, and say so", j.fails_with()),
            true => format!("{}. It is the person's number: change the expect first if the person asked for this (an edit of its own, which says so), then run the tool again", j.fails_with()),
        },
    })
}
