//! A part's force study between its script and the Force render mode: the script's
//! `m.fixed` / `m.force` / `m.pressure` / `m.torque` statements (`fab_model::study`, model
//! frame) as the solver and the panel take them (printer frame), and the Force tab's edits
//! written back as those statements, assembled on the model worker against its latest
//! document (one undo step each).

use fab::api::{ForceKind, ForceLoad, ForcePick, StudyEdit};
use fab_force::bc::Kind;
use fab_force::study::{Load, Spot, Within};
use fab_force::surface::Pick;
use fab_force::V3;
use fab_model::program::Compiled;
use fab_model::study::{self, Study, Where};
use fab_model::{Change, Edit};
use makepad_script::syntax;
use std::sync::Arc;

/// How near a hole click must lie to a feature's own surface to name it (model mm).
const FEATURE_REACH: f64 = 0.6;

/// A placed part's frame: model mm to printer mm (a turn, a uniform scale, a move).
#[derive(Clone, Copy, Debug)]
pub struct Frame {
    m: [[f64; 4]; 3],
    inv: [[f64; 4]; 3],
    scale: f64,
}

impl Frame {
    pub fn new(m: &[[f64; 4]; 3]) -> Option<Frame> {
        let inv = crate::paint_edit::inverse(m)?;
        let scale = (0..3).map(|i| m[i][0] * m[i][0]).sum::<f64>().sqrt();
        (scale > 0.0).then_some(Frame { m: *m, inv, scale })
    }
    fn apply(m: &[[f64; 4]; 3], p: V3, w: f64) -> V3 {
        std::array::from_fn(|i| m[i][0] * p[0] + m[i][1] * p[1] + m[i][2] * p[2] + w * m[i][3])
    }
    /// A model point in the printer frame, and back.
    pub fn point(&self, p: V3) -> V3 {
        Self::apply(&self.m, p, 1.0)
    }
    pub fn model_point(&self, p: V3) -> V3 {
        Self::apply(&self.inv, p, 1.0)
    }
    /// A model direction in the printer frame (its length kept), and back.
    pub fn turn(&self, v: V3) -> V3 {
        Self::apply(&self.m, v, 0.0).map(|x| x / self.scale)
    }
    pub fn model_turn(&self, v: V3) -> V3 {
        Self::apply(&self.inv, v, 0.0).map(|x| x * self.scale)
    }
}

fn kind_of(k: &study::Kind) -> ForceKind {
    match k {
        study::Kind::Fixed => ForceKind::Fixed,
        study::Kind::Force(_) => ForceKind::Force,
        study::Kind::Pressure(_) => ForceKind::Pressure,
        study::Kind::Torque(_) => ForceKind::Torque,
    }
}

/// A region's identity for the solver's loads: what it is and where the part is.
fn key_of(at: &Where, frame: &Frame) -> u64 {
    format!("{at:?}|{:?}", frame.m).bytes().fold(0xcbf2_9ce4_8422_2325u64, |h, b| (h ^ b as u64).wrapping_mul(0x100_0000_01b3))
}

/// The part's study as the solver and the panel take it, in the printer frame: one load and
/// one panel row per statement. A feature keeps the surface within about a cell of its own
/// surface and facing along it; a region solid the surface inside it.
pub fn loads(study: &Study, frame: &Frame) -> Vec<(Load, ForceLoad)> {
    let f = *frame;
    study
        .loads
        .iter()
        .map(|l| {
            let (kind, value) = match l.kind {
                study::Kind::Fixed => (Kind::Fixed, [0.0; 3]),
                study::Kind::Force(v) => {
                    let v = f.turn(v);
                    (Kind::Force(v), v)
                }
                study::Kind::Pressure(p) => (Kind::Pressure(p), [p, 0.0, 0.0]),
                study::Kind::Torque(t) => (Kind::Torque(t), [t, 0.0, 0.0]),
            };
            let within = |keep: Arc<dyn Fn(V3, V3, f64) -> bool + Send + Sync>| Spot::Within(Within { key: key_of(&l.at, &f), keep });
            let spot = match &l.at {
                Where::Face(p) => Spot::Seed { pick: Pick::Face, at: f.point(*p) },
                Where::HoleWall(p) => Spot::Seed { pick: Pick::Hole, at: f.point(*p) },
                Where::All => within(Arc::new(|_, _, _| true)),
                Where::Inside(r) => {
                    let r = r.clone();
                    within(Arc::new(move |c, _, h| r.distance(f.model_point(c)) <= 0.5 * h / f.scale))
                }
                Where::Feature(r) => {
                    let r = r.clone();
                    let grow = match &r {
                        fab_paint_field::Region::Field { grow, .. } => *grow,
                        _ => 0.0,
                    };
                    within(Arc::new(move |c, n, h| {
                        let (q, h) = (f.model_point(c), h / f.scale);
                        let d = |p: V3| r.distance(p) + grow;
                        if d(q).abs() > 0.75 * h {
                            return false;
                        }
                        // the feature's own normal there against the part's
                        let e = 0.01 * h;
                        let g: V3 = std::array::from_fn(|k| {
                            let (mut a, mut b) = (q, q);
                            a[k] += e;
                            b[k] -= e;
                            d(a) - d(b)
                        });
                        let n = f.model_turn(n);
                        let (gl, nl) = (g.iter().map(|v| v * v).sum::<f64>().sqrt(), n.iter().map(|v| v * v).sum::<f64>().sqrt());
                        gl > 0.0 && nl > 0.0 && ((g[0] * n[0] + g[1] * n[1] + g[2] * n[2]) / (gl * nl)).abs() >= 0.7
                    }))
                }
            };
            let row = ForceLoad { kind: kind_of(&l.kind), on: l.on.clone(), top: l.top, value, ..Default::default() };
            (Load { kind, spot, rigid: l.rigid }, row)
        })
        .collect()
}

/// A number as a statement writes it: two decimals at most.
fn num(v: f64) -> String {
    let r = (v * 100.0).round() / 100.0;
    format!("{}", if r == 0.0 { 0.0 } else { r })
}

/// A model point as a statement writes it (mm, commas: a space before a minus subtracts).
fn point(p: V3) -> String {
    let mm = |v: f64| match num(v).as_str() {
        "0" => "0".to_string(),
        s => format!("{s}mm"),
    };
    format!("[{}, {}, {}]", mm(p[0]), mm(p[1]), mm(p[2]))
}

/// A load's value as its statement writes it (model frame); none for a fixture.
fn value_text(kind: ForceKind, value: &[f64; 3], frame: &Frame) -> Option<String> {
    match kind {
        ForceKind::Fixed => None,
        ForceKind::Force => {
            let v = frame.model_turn(*value);
            Some(format!("[{}, {}, {}]", num(v[0]), num(v[1]), num(v[2])))
        }
        ForceKind::Pressure | ForceKind::Torque => Some(num(value[0])),
    }
}

fn call(kind: ForceKind) -> &'static str {
    match kind {
        ForceKind::Fixed => "fixed",
        ForceKind::Force => "force",
        ForceKind::Pressure => "pressure",
        ForceKind::Torque => "torque",
    }
}

/// A statement's bytes with its line when it stands alone on it (its indent and newline).
pub fn whole_line(src: &str, r: &std::ops::Range<usize>) -> std::ops::Range<usize> {
    let start = src[..r.start].rfind('\n').map_or(0, |i| i + 1);
    let end = src[r.end..].find('\n').map_or(src.len(), |i| r.end + i + 1);
    let alone = src[start..r.start].trim().is_empty() && src[r.end..end].trim().is_empty();
    if alone { start..end } else { r.clone() }
}

/// The script edits a Force tab change makes, against the worker's latest compile:
/// `object` is the plate part's id, `frame` its model-to-printer frame.
pub fn edits(c: &Compiled, object: &str, edit: &StudyEdit, frame: Option<&[[f64; 4]; 3]>) -> Result<Vec<Change>, String> {
    let (inst, emitted) = c.object(object).ok_or("The part is no longer on the plate")?;
    let script = &inst.script;
    let src = script.source();
    let file = script.file.clone();
    let frame = Frame::new(frame.ok_or("The part has no model frame")?).ok_or("The part has a singular model frame")?;
    let study = script.study.get(&emitted.name);
    let load = |index: usize| study.and_then(|s| s.loads.get(index)).ok_or("That load is no longer in the script");
    let splice = |range: std::ops::Range<usize>, text: String| Change::Script { file: file.clone(), edit: Edit::Splice { range, text } };
    match edit {
        StudyEdit::Add { kind, pick, at, value } => {
            if !at.iter().chain(value.iter()).all(|v| v.is_finite()) {
                return Err("The click has no valid point".into());
            }
            let q = frame.model_point(*at);
            let on = match pick {
                // a hole of a named feature holds by its name, so it follows the feature
                ForcePick::Hole => study::feature_at(&script.program, &script.objects, emitted, q, FEATURE_REACH).unwrap_or_else(|| format!("HoleWall{{at: {}}}", point(q))),
                ForcePick::Face => format!("Face{{at: {}}}", point(q)),
            };
            let line = match value_text(*kind, value, &frame) {
                Some(v) => format!("m.{}({on}, {v})\n", call(*kind)),
                None => format!("m.{}({on})\n", call(*kind)),
            };
            let text = if src.ends_with('\n') { line } else { format!("\n{line}") };
            Ok(vec![splice(src.len()..src.len(), text)])
        }
        StudyEdit::Set { index, value } => {
            let l = load(*index)?;
            let text = value_text(kind_of(&l.kind), value, &frame).ok_or("A fixture has no value")?;
            let span = l.value_span.clone().ok_or("That load's value is not written in its call")?;
            if !value.iter().all(|v| v.is_finite()) {
                return Err("The value is not a number".into());
            }
            Ok(vec![splice(span, text)])
        }
        StudyEdit::Remove { index } => {
            let l = load(*index)?;
            let span = l.span.as_ref().filter(|_| l.top).ok_or("That load is written inside a loop or a function: remove it in the code")?;
            Ok(vec![splice(whole_line(src, span), String::new())])
        }
        StudyEdit::Clear => {
            // from the last statement back, so each splice's bytes hold
            let mut spans: Vec<_> = study.iter().flat_map(|s| s.loads.iter()).filter(|l| l.top).filter_map(|l| l.span.clone()).collect();
            spans.sort_by_key(|r| std::cmp::Reverse(r.start));
            spans.dedup();
            if spans.is_empty() {
                return Err("No fixture or load the Force tab can remove".into());
            }
            Ok(spans.iter().map(|r| splice(whole_line(src, r), String::new())).collect())
        }
    }
}

/// What the person reads when a study edit is refused, from the worker's answer.
pub fn refusal(reply: &str) -> String {
    let why = syntax::parse_value(reply).ok().and_then(|v| v.field("error").and_then(|f| f.value.string()).map(str::to_string));
    format!("Not changed: {}", why.as_deref().unwrap_or(reply))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The Force tab writes its fixtures and loads as statements of the part's script and
    /// reads them back the same in the printer frame, whatever the part's placement: a click in
    /// a named hole holds by the hole's name, a click on a face writes the face's point; a
    /// value edit rewrites only the value, a removal only its line, each one undo step.
    #[test]
    fn the_force_tab_writes_the_study_into_the_script_and_reads_it_back() {
        let mut session = fab_ai::Session::new(fab_plate::machine::Machine::dual_nozzle_350(), std::env::temp_dir());
        session.model = fab_model::FabModel::with_script("t", "t.cad.splash", fab_model::program::EMPTY);
        let source = format!("{}let hole = m.cylinder({{radius: 3mm height: 30mm}})\nlet body = m.subtract(m.box({{size: [40mm 10mm 20mm]}}), hole)\nobject(body)\n", fab_model::program::EMPTY);
        let (reply, _) = session.set_source("t.cad.splash", &source);
        assert!(reply.ok, "{}", reply.text);
        let place = |arg| Change::Place { instance: "t".into(), object: "body".into(), arg };
        let reply = session.commit("place", vec![place(fab_model::PlaceArg::At(Some([120.0, 140.0]))), place(fab_model::PlaceArg::Rotate([0.0, 0.0, 30.0]))]);
        assert!(reply.ok, "{}", reply.text);
        let ev = session.last.clone().unwrap();
        let frame = Frame::new(&ev.model_to_plate[0]).unwrap();
        let object = ev.plate.parts[0].object.clone();
        let edit = |session: &mut fab_ai::Session, e: StudyEdit| {
            let ev = session.last.clone().unwrap();
            let batch = edits(session.compiled.as_ref().unwrap(), &object, &e, ev.model_to_plate.first()).unwrap_or_else(|why| panic!("{e:?}: {why}"));
            let reply = session.commit("study", batch);
            assert!(reply.ok, "{e:?}: {}", reply.text);
        };
        // the hole's wall (model x = 3 mm) and the top (model y = 5 mm), clicked in the printer frame
        edit(&mut session, StudyEdit::Add { kind: ForceKind::Fixed, pick: ForcePick::Hole, at: frame.point([3.0, 0.0, 0.0]), value: [0.0; 3] });
        let down = frame.turn([0.0, -40.0, 0.0]);
        edit(&mut session, StudyEdit::Add { kind: ForceKind::Force, pick: ForcePick::Face, at: frame.point([12.0, 5.0, 4.0]), value: down });
        let script = session.source("t.cad.splash").unwrap();
        assert!(script.ends_with("object(body)\nm.fixed(hole)\nm.force(Face{at: [12mm, 5mm, 4mm]}, [0, -40, 0])\n"), "{script}");
        let ev = session.last.clone().unwrap();
        let read = loads(ev.placed_study(0).expect("the study"), &frame);
        assert_eq!(read.iter().map(|(_, r)| (r.kind, r.on.as_str(), r.top)).collect::<Vec<_>>(), [(ForceKind::Fixed, "hole", true), (ForceKind::Force, "Face{at: [12mm, 5mm, 4mm]}", true)]);
        assert!((0..3).all(|a| (read[1].1.value[a] - down[a]).abs() < 0.01), "the force reads back in the printer frame: {:?} against {down:?}", read[1].1.value);
        assert!(matches!(read[1].0.spot, Spot::Seed { pick: Pick::Face, at } if (0..3).all(|a| (at[a] - frame.point([12.0, 5.0, 4.0])[a]).abs() < 1e-6)));
        // a value edit, then a removal: only their own bytes
        edit(&mut session, StudyEdit::Set { index: 1, value: frame.turn([5.0, -60.0, 0.0]) });
        assert!(session.source("t.cad.splash").unwrap().ends_with("m.force(Face{at: [12mm, 5mm, 4mm]}, [5, -60, 0])\n"));
        edit(&mut session, StudyEdit::Remove { index: 0 });
        assert_eq!(session.source("t.cad.splash").unwrap(), format!("{source}m.force(Face{{at: [12mm, 5mm, 4mm]}}, [5, -60, 0])\n"));
        let reply = session.undo_redo(fab_model::HistoryAction::Undo);
        assert!(reply.ok, "{}", reply.text);
        assert!(session.source("t.cad.splash").unwrap().contains("m.fixed(hole)\n"), "one undo step brings the fixture back");
        edit(&mut session, StudyEdit::Clear);
        assert_eq!(session.source("t.cad.splash").unwrap(), source);
        assert!(session.last.as_ref().unwrap().placed_study(0).is_none());
    }

    /// The demo bracket's feature tree lists its fixture and its load after its solids, each
    /// at its statement's line with its value; removing the load's row removes its line.
    #[test]
    fn the_feature_tree_lists_the_study_and_removes_a_load_by_its_line() {
        let mut session = fab_ai::Session::new(fab_plate::machine::Machine::dual_nozzle_350(), std::env::temp_dir());
        session.model = crate::demo_part::model();
        session.path = None;
        session.rebuild().expect("the demo builds");
        let c = session.compiled.clone().unwrap();
        let inst = crate::code_edit::instance_of(&c, "bracket.cad.splash").expect("the bracket");
        let rows = crate::code_edit::rows(inst);
        let study: Vec<_> = rows.iter().filter(|r| r.study.is_some()).collect();
        assert_eq!(study.iter().map(|r| (r.op.as_str(), r.id.as_str(), r.detail.as_str())).collect::<Vec<_>>(), [("fixed", "fixed [holes[1], holes[3]]", ""), ("force", "force Face{at: [0, thickness, shelf_z]}", "[0, -50, 0] N")]);
        let src = inst.script.source();
        assert!(study.iter().all(|r| src.lines().nth(r.line).is_some_and(|l| l.starts_with(&format!("m.{}(", r.op)))), "each row at its statement's line");
        assert!(rows.iter().filter(|r| r.study.is_none()).all(|r| !r.root || r.id == "bracket"), "a study's regions stay free to toggle");
        let state = fab::ui::code::SourceState { file: "bracket.cad.splash".into(), text: Arc::new(src.to_string()), features: rows.clone(), ..Default::default() };
        let removed = crate::code_edit::removed(&state, &study[1].id).expect("the load's row is removable");
        let reply = session.commit("remove", removed);
        assert!(reply.ok, "{}", reply.text);
        let after = session.source("bracket.cad.splash").unwrap();
        assert_eq!(after, src.replace("m.force(Face{at: [0, thickness, shelf_z]}, [0, -50, 0])\n", ""));
        let ev = session.last.clone().unwrap();
        let k = ev.plate.parts.iter().position(|p| p.object.starts_with("bracket/")).expect("the bracket on the plate");
        assert!(ev.placed_study(k).is_some_and(|s| !s.posed() && s.loads.len() == 1), "held, not loaded");
        // the geometry a fixture names renamed away: the script's problem is at the fixture's
        // line, the line the tree marks on its row
        let renamed = after.replace("let holes = ", "let screw_holes = ").replace("[holes bores]", "[screw_holes bores]");
        let (reply, problem) = session.set_source("bracket.cad.splash", &renamed);
        assert!(!reply.ok, "a fixture on a name that is gone does not build");
        let line = problem.and_then(|p| p.line).map(|(l, _)| l.saturating_sub(1) as usize);
        assert_eq!(line, Some(study[0].line), "the problem at the fixture's row: {}", reply.text);
    }
}
