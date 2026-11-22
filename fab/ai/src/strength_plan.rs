//! `fab.strength_plan`: "optimize this part's printed strength" in one reply, built from the other
//! tools' own answers and changing nothing: the tubes and weak spots, the best way up, the limb turns
//! that would help, the infill and thickening still worth doing with their gains and costs, and what
//! stays weak after all of it.

use crate::out::{arr, n, s, S};
use crate::strengthen::{Spot, WeakMemo, WeakSpotJob};
use crate::tubes::{off_segment, with_field, Bead, OrientTable, Oriented, Tube, TubesMemo, STAYS_DEG};
use makepad_ai_services::wire::MAX_RESULT_BYTES;
use crate::{RenderRequest, Reply, Session};
use fab_model::eval::Evaluated;
use fab_model::Head;
use makepad_fab_slice::live::PlatePaths;
use makepad_model::json::Value;
use std::sync::{Arc, Mutex};

pub const TOOL: &str = "strength_plan";

/// The plan's slow half, off the model worker: the list of weak spots, and what's left to say with
/// it. The table was read on the worker, as `fab.orient` reads it.
pub struct PlanJob {
    plan: Plan,
    /// Every row, however long the reply: for a caller that reads it as data, as the Slicer tab does.
    whole: bool,
}

enum Plan {
    /// Still to find: the weak spots, with the way up the worker searched.
    Fresh { weak: Arc<WeakSpotJob>, table: OrientTable, bead: Bead, asked: Asked, tubes: Option<Arc<TubesMemo>>, memo: Arc<Mutex<Option<Arc<PlanMemo>>>> },
    /// Asked as the last plan was: its answer.
    Again(Arc<PlanMemo>),
}

/// What a plan is of: the plate, its slice, the program's head and the words asked, but for how
/// much of it the reply carries.
#[derive(Clone)]
struct Asked {
    ev: Arc<Evaluated>,
    paths: Arc<PlatePaths>,
    head: Head,
    args: Value,
}

impl Asked {
    fn is(&self, other: &Asked) -> bool {
        Arc::ptr_eq(&self.ev, &other.ev) && Arc::ptr_eq(&self.paths, &other.paths) && self.head == other.head && self.args == other.args
    }
}

/// The last plan: its answer before a reply's bound, and the lists its ids name. The Slicer tab's
/// whole list after the AI's reply, or the same plan asked twice, is this answer, not the same find
/// and way-up search again.
pub(crate) struct PlanMemo {
    asked: Asked,
    answer: S,
    weak: Arc<WeakMemo>,
    tubes: Option<Arc<TubesMemo>>,
}

impl std::fmt::Debug for PlanJob {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PlanJob")
    }
}

impl PartialEq for PlanJob {
    fn eq(&self, other: &Self) -> bool {
        std::ptr::eq(self, other)
    }
}

/// The weak spot on a tube, worst first in the list: what its fixes are offered through.
fn spot_on<'a>(spots: &'a [Spot], tube: &Tube) -> Option<&'a Spot> {
    spots.iter().find(|spot| {
        spot.part == tube.part && (spot.feature.is_some() && spot.feature == tube.feature || off_segment(spot.found.at, tube.start, tube.end) <= tube.reach + 0.5 * spot.found.thickness)
    })
}

impl PlanJob {
    pub fn answer(&self, stop: &(dyn Fn() -> bool + Sync)) -> Result<S, String> {
        let answer = match &self.plan {
            Plan::Fresh { weak, table, bead, asked, tubes, memo } => {
                let answer = fresh(weak, table, bead, stop)?;
                if let Some(found) = weak.memo() {
                    makepad_error_log::flight::mark_ab("fab.strength.plan", "plan", table.tubes.len() as u64, found.spots.len() as u64);
                    *memo.lock().unwrap() = Some(Arc::new(PlanMemo { asked: asked.clone(), answer: answer.clone(), weak: found, tubes: tubes.clone() }));
                }
                answer
            }
            Plan::Again(memo) => {
                makepad_error_log::flight::mark_ab("fab.strength.plan", "again", memo.tubes.as_ref().map_or(0, |t| t.tubes.len()) as u64, memo.weak.spots.len() as u64);
                memo.answer.clone()
            }
        };
        Ok(bounded(answer, if self.whole { usize::MAX } else { MAX_RESULT_BYTES }))
    }
}

/// The plan of `table`'s way up and the weak spots `weak` finds, whole.
fn fresh(weak: &WeakSpotJob, table: &OrientTable, bead: &Bead, stop: &(dyn Fn() -> bool + Sync)) -> Result<S, String> {
    let answer = weak.answer(stop)?;
    let memo = weak.memo();
    let spots: &[Spot] = memo.as_ref().map_or(&[], |memo| &memo.spots);
    let after = |tube: &Tube| table.best.map_or_else(|| tube.angle(), |turn| crate::tubes::angle_after(tube, &turn));
    // what's left standing after the best way up, and what still helps it
    let mut stays = Vec::new();
    let mut words = Vec::new();
    for tube in &table.tubes {
        let angle = after(tube);
        if angle <= STAYS_DEG {
            continue;
        }
        let mut row = vec![("tube", s(&tube.id)), ("name", s(&tube.called)), ("angle_after", n(angle)), ("gain_lost_estimate", n(bead.gain(angle, 0.0)))];
        if let Some(spot) = spot_on(spots, tube) {
            row.push(("spot", s(&spot.id)));
            words.push(format!("{} stays at {angle:.0} deg: {} is the spot to fix", tube.called, spot.id));
        } else {
            words.push(format!("{} stays at {angle:.0} deg", tube.called));
        }
        stays.push(crate::out::obj(row));
    }
    let mut says = table.says.clone().map_or_else(Vec::new, |says| vec![says]);
    if words.is_empty() {
        says.push("nothing stays standing after that way up".to_string());
    } else {
        says.push(words.join("; "));
    }
    let rows = table.tubes.iter().map(|tube| {
        crate::out::line(crate::out::obj(vec![
            ("tube", s(&tube.id)),
            ("name", s(&tube.called)),
            ("angle_now", n(tube.angle())),
            ("angle_after", n(after(tube))),
            ("gain_estimate", n(bead.gain(tube.angle(), after(tube)))),
            ("flat_gain_estimate", n(bead.gain(tube.angle(), 0.0))),
            ("band", crate::out::obj(vec![("a", arr(tube.start.iter().map(|x| n(*x)))), ("b", arr(tube.end.iter().map(|x| n(*x)))), ("radius", n(tube.reach))])),
            ("length", n(tube.length)),
            ("thinnest_radius", n(tube.thinnest)),
        ]))
    });
    let mut answer = with_field(answer, "says", s(says.join(". ")));
    answer = with_field(answer, "changes", s("nothing: a read. Say this in plain words with a picture (fab.render {tubes: true}), ask before turning the part or a limb, then fab.strengthen the picks as one undo step"));
    answer = with_field(answer, "orientation", S::Object(None, table.fields.clone()));
    answer = with_field(answer, "tubes", arr(rows));
    answer = with_field(answer, "stays_weak", arr(stays));
    Ok(answer)
}

/// The plan's lists, each worst first, and the tool that has all of it.
const LISTS: [(&str, &str); 3] = [("tubes", "fab.tubes"), ("stays_weak", "fab.tubes"), ("spots", "fab.weak_spots")];
/// What's kept back from a reply's bound for saying what was left out, bytes.
const ROOM_TO_SAY: usize = 256;

/// A reply fits in `room` bytes, what an AI caller is sent: the way up and the words whole, and of the
/// lists as much as fits, off the end of the longest, saying what it left out. `fab.strengthen` takes every spot's id
/// all the same.
fn bounded(answer: S, room: usize) -> S {
    let S::Object(name, mut fields) = answer else { return answer };
    if let Some(i) = fields.iter().position(|(key, _)| key == "spots") {
        let spots = fields.remove(i);
        fields.push(spots);
    }
    let mut left_out = [0usize; LISTS.len()];
    while crate::out::text(&S::Object(name.clone(), fields.clone())).len() > room.saturating_sub(ROOM_TO_SAY) {
        let longest = fields.iter_mut().filter_map(|(key, value)| match value {
            S::Array(rows) if !rows.is_empty() => LISTS.iter().position(|(list, _)| list == key).map(|i| (i, rows)),
            _ => None,
        }).max_by_key(|(_, rows)| rows.len());
        let Some((i, rows)) = longest else { break };
        rows.pop();
        left_out[i] += 1;
    }
    let said: Vec<String> = LISTS.iter().zip(left_out).filter(|(_, count)| *count > 0).map(|((list, tool), count)| format!("{count} of {list}, less at risk: {tool} lists them")).collect();
    if !said.is_empty() {
        fields.push(("left_out".to_string(), s(said.join("; "))));
    }
    S::Object(name, fields)
}

impl Session {
    pub(crate) fn strength_plan_tool(&mut self, args: &Value, cancelled: Option<&dyn Fn() -> bool>) -> Reply {
        let weak = self.weak_spots_tool(args, cancelled);
        let Some(RenderRequest::WeakSpots(job)) = &weak.render else { return weak };
        let whole = args.get("whole").and_then(Value::as_bool) == Some(true);
        let asked = Asked { ev: job.ev.clone(), paths: job.paths().clone(), head: self.model.head(), args: without(&without(args, "apply"), "whole") };
        let last = self.plan_memo.lock().unwrap().clone();
        let plan = match last.filter(|last| last.asked.is(&asked)) {
            Some(last) => {
                // the lists its ids name, as they were when it was found
                *self.weak_memo.lock().unwrap() = Some(last.weak.clone());
                if let Some(tubes) = &last.tubes {
                    *self.tubes_memo.lock().unwrap() = Some(tubes.clone());
                }
                Plan::Again(last)
            }
            None => {
                let table = match self.orient_answer(&without(args, "apply"), cancelled) {
                    Ok(Oriented::Table(table)) => table,
                    Ok(Oriented::Moved(reply)) => return reply,
                    Err(why) => return Reply::err(why),
                };
                let tubes = self.tubes_memo.lock().unwrap().clone();
                Plan::Fresh { weak: job.clone(), table, bead: self.bead(&job.ev), asked, tubes, memo: self.plan_memo.clone() }
            }
        };
        let job = PlanJob { plan, whole };
        Reply { images: Vec::new(), ok: true, text: String::new(), update: None, render: Some(RenderRequest::StrengthPlan(Arc::new(job))) }
    }
}

fn without(args: &Value, key: &str) -> Value {
    match args {
        Value::Obj(fields) => Value::Obj(fields.iter().filter(|(k, _)| k != key).cloned().collect()),
        other => other.clone(),
    }
}

#[cfg(test)]
#[path = "strength_plan_tests.rs"]
mod tests;
