//! `fab.align`'s answer: a board fitted onto a printed part's bosses, hole by hole, and the
//! mount that seats it as script edits (`board_mount`). It reads only: the caller applies the
//! edits (`fab.apply`), so the AI and the viewport's Align tool commit the same way. Cost:
//! O(the part's triangles) to find its bosses plus the fit ([`crate::align`]), once per call on a
//! worker, never per frame.

use crate::align::{compose, find_bosses, fit, invert, mount_of, Boss, Fit, TOL};
use crate::board_mech::board_mech_with;
use fab_elec_core::{to_mm, BBox};
use fab_elec_design::Design;
use fab_model::eval::Evaluated;
use std::fmt::Write;

/// A fit with what it was fitted from, for an answer or a view.
pub struct Aligned {
    /// The board unit's name and its holes' names and places (board mm).
    pub board: String,
    pub holes: Vec<(String, [f64; 2])>,
    /// The part's object (`instance/solid`), its name and its bosses (the plate's frame).
    pub part: String,
    pub part_name: String,
    pub bosses: Vec<Boss>,
    /// The board now on the plate (board mm to plate mm), when the part holds it.
    pub now: Option<[[f64; 4]; 3]>,
    /// The fit, its pose board mm to plate mm.
    pub fit: Fit,
    /// The part's model frame from the plate's.
    pub model_from_plate: [[f64; 4]; 3],
    /// The mount that seats it, in the part's model frame: its middle's place and its turn
    /// (degrees); none when the board would not lie flat facing the part's up (+y).
    pub mount: Option<([f64; 3], f64)>,
}

/// Fit `board` (a board unit's name; the design's only one when none) onto `part` (a plate part,
/// by object or name; the part that holds the board when none).
pub fn align(design: &Design, ev: &Evaluated, board: Option<&str>, part: Option<&str>) -> Result<Aligned, String> {
    let boards: Vec<_> = design.units.iter().filter_map(|unit| unit.board.as_deref().map(|b| (unit, b))).collect();
    let names = || boards.iter().map(|(unit, _)| format!("{:?}", unit.name)).collect::<Vec<_>>().join(", ");
    let (unit, pcb) = match board {
        Some(name) => boards.iter().find(|(unit, _)| unit.name == name).copied().ok_or_else(|| format!("no board {name:?}; pass board as one of: {}", names()))?,
        None if boards.len() == 1 => boards[0],
        None if boards.is_empty() => return Err("the design has no board".into()),
        None => return Err(format!("the design has several boards; pass board as one of: {}", names())),
    };
    let mech = board_mech_with(pcb, &unit.circuit, &pcb.drills());
    if mech.holes.is_empty() {
        return Err(format!("{:?} has no mounting holes to align by", unit.name));
    }
    let bounds = mech.outline.iter().fold(BBox::EMPTY, |b, c| b.union(c.bbox()));
    let middle = [(to_mm(bounds.min.x) + to_mm(bounds.max.x)) / 2.0, (to_mm(bounds.min.y) + to_mm(bounds.max.y)) / 2.0];

    // The board as parts hold it (inserts linked `board:<name>`).
    let key = format!("board:{}", unit.name);
    let linked: Vec<_> = ev.plate.excluded.iter().filter(|p| p.link.as_deref() == Some(key.as_str())).collect();
    let host_of = |object: &str| ev.plate.parts.iter().position(|p| p.object == object);
    let ix = match part {
        Some(want) => host_of(want).or_else(|| ev.plate.parts.iter().position(|p| p.name == want))
            .ok_or_else(|| format!("no plate part {want:?} (the plate's parts: {})", ev.plate.parts.iter().map(|p| format!("{:?}", p.name)).collect::<Vec<_>>().join(", ")))?,
        None => linked.iter().find_map(|p| host_of(&p.insert.as_ref()?.into))
            .ok_or_else(|| format!("no part holds {:?}; pass part: the plate part to seat it on", unit.name))?,
    };
    let target = &ev.plate.parts[ix];
    // Where the target holds it now; a board another part holds says nothing about this one.
    let now = linked.iter().find(|p| p.insert.as_ref().is_some_and(|i| i.into == target.object)).and_then(|p| {
        let (_, frame) = p.frame.as_ref()?;
        Some(compose(&ev.model_to_plate[ix], frame))
    });
    if target.mesh.triangles.is_empty() {
        return Err(format!("{:?} has no mesh to find bosses on", target.object));
    }
    let to_plate = ev.model_to_plate[ix];
    let model_from_plate = invert(&to_plate).ok_or_else(|| format!("{:?}'s placement flattens it", target.object))?;
    let up = match &now {
        Some(p) => [p[0][2], p[1][2], p[2][2]],
        None => {
            let y = [to_plate[0][1], to_plate[1][1], to_plate[2][1]];
            let n = (y[0] * y[0] + y[1] * y[1] + y[2] * y[2]).sqrt();
            [y[0] / n, y[1] / n, y[2] / n]
        }
    };
    let bosses = find_bosses(&target.mesh.positions, &target.mesh.triangles, up);
    let holes: Vec<(String, [f64; 2])> = mech.holes.iter().map(|h| (h.name.clone(), [to_mm(h.at.x), to_mm(h.at.y)])).collect();
    let at: Vec<[f64; 2]> = holes.iter().map(|h| h.1).collect();
    let fit = fit(&at, &bosses, up, now.as_ref())
        .ok_or_else(|| format!("{:?} has no bosses facing the board: no round column top or hole in a face looking its way", target.object))?;
    let on: Vec<[f64; 2]> = holes.iter().zip(&fit.holes).filter(|(_, h)| h.boss.is_some() && h.residual <= TOL).map(|(h, _)| h.1).collect();
    let mount = mount_of(&compose(&model_from_plate, &fit.pose), middle).map(|(at, turn)| (at, snap_turn(turn, &on)));
    Ok(Aligned { board: unit.name.clone(), holes, part: target.object.clone(), part_name: target.name.clone(), bosses, now, fit, model_from_plate, mount })
}

/// A fitted turn (degrees) as the fit can state it: the nearest exact quarter turn when the turn lies within what the
/// holes that sit on their bosses resolve (the fit places each within [`TOL`] mm, so across the widest pair of them a
/// turn is known to atan(TOL / span)), else the turn as fitted. The mesh's facets leave about 1e-4 degrees on a true
/// quarter turn, and the edit writes the turn into the user's script. With fewer than two holes on there is no
/// resolution to speak of and the turn is kept.
pub fn snap_turn(turn: f64, on: &[[f64; 2]]) -> f64 {
    let span = on.iter().flat_map(|a| on.iter().map(move |b| ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2)).sqrt())).fold(0.0, f64::max);
    if span <= TOL {
        return turn;
    }
    let quarter = (turn / 90.0).round() * 90.0;
    if (turn - quarter).abs() > (TOL / span).atan().to_degrees() {
        return turn;
    }
    // (a turn of half a revolution reads 180 either way round)
    if quarter == -180.0 { 180.0 } else { quarter + 0.0 }
}

/// The answer as text, with the edits that write the mount into the part's script (`script_of`:
/// an instance's script file and source).
pub fn align_answer(design: &Design, ev: &Evaluated, script_of: &dyn Fn(&str) -> Option<(String, String)>, board: Option<&str>, part: Option<&str>) -> Result<String, String> {
    let a = align(design, ev, board, part)?;
    let mut out = format!("{{board: {:?} part: {:?} on: {} of: {} tolerance: {}mm holes: [", a.board, a.part_name, a.fit.on, a.holes.len(), num(TOL));
    let mut misses = Vec::new();
    for ((name, at), h) in a.holes.iter().zip(&a.fit.holes) {
        write!(out, "{{name: {name:?} at: [{} {}]", num(at[0]), num(at[1])).unwrap();
        let Some(k) = h.boss else {
            out.push_str(" boss: nil} ");
            misses.push(format!("{name} has no boss left to sit on"));
            continue;
        };
        let b = a.bosses[k].axis;
        write!(out, " boss: [{} {} {}] residual: {} off: [{} {}] gap: {}}} ", num(b[0]), num(b[1]), num(b[2]), num(h.residual), num(h.off[0]), num(h.off[1]), num(h.gap)).unwrap();
        if h.residual > TOL {
            // The boss's move in the part's model frame: from its axis to where the hole lands.
            let p = apply(&a.fit.pose, [at[0], at[1], 0.0]);
            let d = [p[0] - b[0], p[1] - b[1], p[2] - b[2]];
            let m = &a.model_from_plate;
            let dm: [f64; 3] = std::array::from_fn(|r| m[r][0] * d[0] + m[r][1] * d[1] + m[r][2] * d[2]);
            misses.push(format!("{name} misses its boss by {} mm: move the hole by [{} {}] mm on the board, or the boss by [{} {} {}] mm in the part's model frame",
                num(h.residual), num(h.off[0]), num(h.off[1]), num(dm[0]), num(dm[1]), num(dm[2])));
        }
    }
    out.push_str("]");
    let instance = a.part.split('/').next().unwrap_or(&a.part);
    let ident: String = a.board.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }).collect();
    let let_name = format!("{ident}_mount");
    let mut note = None;
    match (a.mount, script_of(instance)) {
        (Some((at, turn)), Some((file, source))) => {
            let mount = format!("board_mount(board({:?}), {{at: [{}mm, {}mm, {}mm] turn: {}deg}})", a.board, num(at[0]), num(at[1]), num(at[2]), num(turn));
            write!(out, " mount: {{at: [{}mm {}mm {}mm] turn: {}deg}} script: {file:?}", num(at[0]), num(at[1]), num(at[2]), num(turn)).unwrap();
            let edits = if source.lines().any(|l| l.trim_start().starts_with(&format!("let {let_name} ="))) {
                Some(format!("replace({let_name}, {mount})"))
            } else if a.now.is_some() && source.contains(&format!("board({:?})", a.board)) {
                note = Some(format!("{file} builds its own place for {} (a case round it follows the board's holes): nothing to write", a.board));
                None
            } else {
                Some(format!("let {let_name} = {mount}\ninsert(board_solid(board({b:?}), board_bodies({b:?}), {let_name}), name: {b:?}, into: {:?})", a.part_name, b = a.board))
            };
            if let Some(edits) = edits {
                write!(out, " edits: {edits:?}").unwrap();
            }
        }
        (None, _) => note = Some(format!("{} would not lie flat facing {}'s up (+y): a mount cannot say that", a.board, a.part_name)),
        (_, None) => note = Some(format!("no script for {instance:?}")),
    }
    if !misses.is_empty() {
        write!(out, " misses: [{}]", misses.iter().map(|m| format!("{m:?}")).collect::<Vec<_>>().join(" ")).unwrap();
    }
    if let Some(note) = note {
        write!(out, " note: {note:?}").unwrap();
    }
    out.push('}');
    Ok(out)
}

fn apply(m: &[[f64; 4]; 3], p: [f64; 3]) -> [f64; 3] {
    std::array::from_fn(|r| m[r][0] * p[0] + m[r][1] * p[1] + m[r][2] * p[2] + m[r][3])
}

/// A length or an angle to a ten-thousandth, trailing zeros dropped.
fn num(v: f64) -> String {
    let s = format!("{:.4}", v);
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s == "-0" { "0".into() } else { s.into() }
}
