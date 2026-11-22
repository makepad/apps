//! Optimise, its script side: the cuts a stress study plans ([`fab_force::opt`], printer mm)
//! written into the part's object script the way a person writes them. One `let` per cut
//! (`let upright_holes = m.hole_row({..})`; slots that differ in one coordinate as a `for`
//! loop), each joined to the part's cutters (`m.subtract(body, [holes bores upright_holes])`).
//! Every number is a site the part panel shows, and the edits are one undo step. A variant's
//! label says what it saves as printed (walls and skins solid, infill at its density), which is
//! what the scale reads.

pub mod grade;
pub mod grow;
pub mod grown;

use fab_force::opt::cuts::{cross, dot, unit, Cut, ROOF_COS};
use fab_force::opt::search::Score;
use fab_force::opt::variants::{Family, Size, Variant};
use fab_force::V3;
use makepad_model_script::lang::edit::Edit;
use makepad_model_script::lang::Program;

/// Printer mm to a part's model frame: the inverse of its `model_to_plate` (a rotation, a
/// uniform scale and a move).
#[derive(Clone, Copy, Debug)]
pub struct Frame {
    m: [[f64; 4]; 3],
    scale: f64,
}

impl Frame {
    pub fn of(model_to_plate: &[[f64; 4]; 3]) -> Frame {
        let m = *model_to_plate;
        let scale = (m[0][0] * m[0][0] + m[1][0] * m[1][0] + m[2][0] * m[2][0]).sqrt().max(1e-12);
        Frame { m, scale }
    }

    /// `M^T v`: a printer vector turned back (and scaled by the part's scale).
    fn back(&self, v: V3) -> V3 {
        std::array::from_fn(|c| (0..3).map(|r| self.m[r][c] * v[r]).sum::<f64>())
    }

    pub fn point(&self, p: V3) -> V3 {
        let d = [p[0] - self.m[0][3], p[1] - self.m[1][3], p[2] - self.m[2][3]];
        self.back(d).map(|v| v / (self.scale * self.scale))
    }

    pub fn dir(&self, d: V3) -> V3 {
        unit(self.back(d))
    }

    pub fn len(&self, l: f64) -> f64 {
        l / self.scale
    }

    /// The print's up (printer +Z) in the model frame.
    pub fn up(&self) -> V3 {
        self.dir([0.0, 0.0, 1.0])
    }
}

/// `v` to `decimals` places, without trailing zeros (and no `-0`).
fn num(v: f64, decimals: i32) -> String {
    let k = 10f64.powi(decimals);
    let r = (v * k).round() / k;
    if r == 0.0 {
        return "0".into();
    }
    let s = format!("{r:.*}", decimals as usize);
    match s.contains('.') {
        true => s.trim_end_matches('0').trim_end_matches('.').to_string(),
        false => s,
    }
}

/// A length as Splash writes it: `12.5mm`, `0`.
fn mm(v: f64) -> String {
    match num(v, 2) {
        z if z == "0" => z,
        s => format!("{s}mm"),
    }
}

/// A point or a size (commas: `[-8mm -20mm]` would subtract).
fn point(p: &[f64]) -> String {
    format!("[{}]", p.iter().map(|v| mm(*v)).collect::<Vec<_>>().join(", "))
}

fn direction(d: V3) -> String {
    format!("[{}]", d.iter().map(|v| num(*v, 4)).collect::<Vec<_>>().join(", "))
}

fn same_dir(a: V3, b: V3) -> bool {
    dot(a, b) > 1.0 - 1e-6
}

/// `up` across unit `axis` as the builders take it (any vector across when parallel).
fn across(axis: V3, up: V3) -> V3 {
    let d = dot(axis, up);
    let a: V3 = std::array::from_fn(|k| up[k] - d * axis[k]);
    if dot(a, a).sqrt() > 1e-9 {
        return unit(a);
    }
    let other = if axis[0].abs() < 0.9 { [1.0, 0.0, 0.0] } else { [0.0, 0.0, 1.0] };
    unit(cross(axis, other))
}

/// The fields a builder call takes past the ones the cut sets: `up` when the print's up is
/// not the model's +Y.
fn up_field(f: &Frame) -> String {
    match f.up() {
        up if same_dir(up, [0.0, 1.0, 0.0]) => String::new(),
        up => format!(" up: {}", direction(up)),
    }
}

/// A plate's side by its cut's axis in the printer frame: an axis near the build direction
/// cuts a level plate.
fn side_of(axis: V3) -> &'static str {
    if unit(axis)[2].abs() >= ROOF_COS {
        "level"
    } else {
        "upright"
    }
}

/// One builder call, in the model frame, for a cut (`center` in place of a pocket's own).
fn call(f: &Frame, cut: &Cut, center: Option<String>) -> String {
    match cut {
        Cut::Holes { from, to, count, diameter, length, axis, .. } => {
            let (a, b) = (f.point(*from), f.point(*to));
            let place = match count {
                1 => format!("from: {}", point(&std::array::from_fn::<f64, 3, _>(|k| 0.5 * (a[k] + b[k])))),
                n => format!("from: {} to: {} count: {n}", point(&a), point(&b)),
            };
            format!("m.hole_row({{{place} diameter: {} length: {} axis: {}{}}})", mm(f.len(*diameter)), mm(f.len(*length)), direction(f.dir(*axis)), up_field(f))
        }
        Cut::Pocket { at, size, along, axis, depth, corner, roof } => {
            let (axis_m, along_m, up) = (f.dir(*axis), f.dir(*along), f.up());
            let mut extra = String::new();
            let default_along = cross(across(axis_m, up), axis_m);
            if dot(along_m, default_along).abs() < 1.0 - 1e-6 {
                extra.push_str(&format!(" along: {}", direction(along_m)));
            }
            extra.push_str(&up_field(f));
            if *roof != (dot(axis_m, up).abs() < ROOF_COS) {
                extra.push_str(&format!(" roof: {roof}"));
            }
            let center = center.unwrap_or_else(|| point(&f.point(*at)));
            format!(
                "m.pocket({{center: {center} size: {} depth: {} axis: {} corner: {}{extra}}})",
                point(&size.map(|s| f.len(s))),
                mm(f.len(*depth)),
                direction(axis_m),
                mm(f.len(*corner))
            )
        }
    }
}

/// Pockets alike but for their centres, which differ in one model coordinate only: that
/// coordinate's values.
fn alike(f: &Frame, cuts: &[Cut]) -> Option<(usize, Vec<f64>)> {
    let shape = |c: &Cut| match c {
        Cut::Pocket { size, along, axis, depth, corner, roof, .. } => Some((*size, *along, *axis, *depth, *corner, *roof)),
        _ => None,
    };
    let first = shape(cuts.first()?)?;
    if cuts.len() < 2 || cuts.iter().any(|c| shape(c) != Some(first)) {
        return None;
    }
    let centres: Vec<V3> = cuts.iter().map(|c| f.point(c.centres()[0])).collect();
    let moves: Vec<usize> = (0..3).filter(|&k| centres.iter().any(|c| (c[k] - centres[0][k]).abs() > 1e-3)).collect();
    match moves[..] {
        [k] => Some((k, centres.iter().map(|c| c[k]).collect())),
        _ => None,
    }
}

/// The script edits that make `cuts` in the part `solid` (an index into `p`'s solids) of the
/// object script `p`, its frame `f`: the cuts' `let`s before the part's own, and each joined to
/// its cutters (a part that is no subtract becomes one). The names given, in order.
pub fn edits(p: &Program, solid: usize, f: &Frame, cuts: &[Cut]) -> Result<(Vec<Edit>, Vec<String>), String> {
    let target = p.solids.get(solid).ok_or("the part is not in its script")?;
    if !target.named {
        return Err(format!("'{}' has no `let` of its own to cut", target.id));
    }
    let taken = |name: &str| p.solids.iter().any(|s| s.id == name || s.id.starts_with(&format!("{name}[")) || s.id.starts_with(&format!("{name}."))) || p.params.iter().any(|q| q.name.as_deref() == Some(name)) || p.names.syntax.find_let(name).is_some();
    let mut names: Vec<String> = Vec::new();
    let mut fresh = |base: String| {
        let mut name = base.clone();
        let mut n = 2;
        while taken(&name) || names.contains(&name) {
            name = format!("{base}_{n}");
            n += 1;
        }
        names.push(name.clone());
        name
    };
    let mut lets = Vec::new();
    // pockets of one plate split into slots come in a run: one loop each
    let mut i = 0;
    while i < cuts.len() {
        let mut j = i + 1;
        while j < cuts.len() && alike(f, &cuts[i..=j]).is_some() {
            j += 1;
        }
        let run = &cuts[i..j];
        let (axis, kind) = match &run[0] {
            Cut::Holes { axis, .. } => (*axis, "holes"),
            Cut::Pocket { axis, .. } => (*axis, if run.len() > 1 { "slots" } else { "pocket" }),
        };
        let name = fresh(format!("{}_{kind}", side_of(axis)));
        let code = match alike(f, run) {
            Some((k, values)) => {
                let var = ["x", "y", "z"][k];
                let mut c = f.point(run[0].centres()[0]).map(|v| mm(v));
                c[k] = var.to_string();
                let list = values.iter().map(|v| mm(*v)).collect::<Vec<_>>().join(", ");
                format!("let {name} = for {var} in [{list}] {{ {} }}", call(f, &run[0], Some(format!("[{}]", c.join(", ")))))
            }
            None => format!("let {name} = {}", call(f, &run[0], None)),
        };
        lets.push(code);
        i = j;
    }
    let mut out: Vec<Edit> = lets.into_iter().map(|code| Edit::Add { code, before: Some(target.id.clone()) }).collect();
    if matches!(target.proto, "Difference") {
        out.extend(names.iter().map(|n| Edit::Operand { solid: target.id.clone(), code: n.clone() }));
    } else {
        let span = target.span.clone().ok_or_else(|| format!("'{}' is made by code: cut that code", target.id))?;
        let code = format!("m.subtract({}, [{}])", &p.source()[span], names.join(" "));
        out.push(Edit::Replace { solid: target.id.clone(), code });
    }
    Ok((out, names))
}

/// A model direction by its nearest axis, and the print's up or down when it is that: `+Z`,
/// `-Y (down)`.
fn facing(f: &Frame, d: V3) -> String {
    let k = (0..3).max_by(|&a, &b| d[a].abs().total_cmp(&d[b].abs())).unwrap();
    let mut s = format!("{}{}", if d[k] >= 0.0 { "+" } else { "-" }, ["X", "Y", "Z"][k]);
    let up = dot(d, f.up());
    if up.abs() >= ROOF_COS {
        s.push_str(if up > 0.0 { " (up)" } else { " (down)" });
    }
    s
}

/// A printer box in the model frame (the part turned by quarter turns keeps it a box).
fn model_box(f: &Frame, lo: V3, hi: V3) -> (V3, V3) {
    let (a, b) = (f.point(lo), f.point(hi));
    (std::array::from_fn(|k| a[k].min(b[k])), std::array::from_fn(|k| a[k].max(b[k])))
}

/// A study's answer for the AI and the panel, in the part's model frame (the frame its script
/// is written in), a few lines: what it weighs as printed and how safe it is, its most loaded
/// spots, and the lightly loaded bands of plate that may be cut and from where. `near` names
/// the part's feature at a model point (`holes[1]`), if any; `grams` the plastic's weight.
pub fn describe(s: &fab_force::opt::summary::Summary, f: &Frame, part: &str, grams: f64, near: &dyn Fn(V3) -> Option<String>) -> String {
    use fab_force::opt::summary::Access;
    let at = |p: V3| {
        let q = f.point(p);
        match near(q) {
            Some(n) => format!("{} by {n}", point(&q)),
            None => point(&q),
        }
    };
    let mut out = format!(
        "{part}: {:.1} g as printed; SF {} (at fixtures {}); max von Mises {} MPa at {}\n",
        grams,
        num(s.min_sf.0, 1),
        num(s.min_sf_fixed, 1),
        num(s.max_vm.0, 1),
        at(s.max_vm.1)
    );
    let high: Vec<String> = s.high.iter().map(|h| format!("SF {} at {}", num(h.sf, 1), at(h.at))).collect();
    if !high.is_empty() {
        out.push_str(&format!("most loaded: {}\n", high.join("; ")));
    }
    let low: Vec<String> = s
        .low
        .iter()
        .take(6)
        .map(|b| {
            let mut e = [0.0; 3];
            e[b.axis] = 1.0;
            let normal = f.dir(e);
            let from = match b.access {
                Access::Through => format!("through along {}", facing(f, normal).trim_start_matches(['+', '-'])),
                Access::FromHigh => format!("from {}", facing(f, normal)),
                Access::FromLow => format!("from {}", facing(f, normal.map(|v| -v))),
            };
            let (lo, hi) = model_box(f, b.lo, b.hi);
            // as printed: a pocket's floor is all walls and skins, so it saves only what the
            // plate prints over that
            let printed = format!("prints as {} solid", mm(f.len(b.printed)));
            let note = match b.access {
                Access::Through => printed,
                _ => {
                    let saves = (1.0 - b.floor / b.printed.max(1e-9)).max(0.0);
                    format!("{printed}; a pocket's {} floor prints solid, so it saves {}% of the plastic it opens", mm(f.len(b.floor)), num(100.0 * saves, 0))
                }
            };
            format!("{} plate {} thick, {from}: {} mm2 in {}..{} ({note})", side_of(e), mm(f.len(b.thickness)), num(b.area / (f.scale * f.scale), 0), point(&lo), point(&hi))
        })
        .collect();
    out.push_str(&match low.is_empty() {
        true => "lightly loaded, may be cut: none (no plate clear of the fixtures and loads)\n".to_string(),
        false => format!("lightly loaded, may be cut:\n  {}\n", low.join("\n  ")),
    });
    out
}

/// The sizes the Thinner family may take down: the part's params whose value is the thickness
/// of one of its plates (`plates`, model mm), each floored at its own min and at `floor` (two
/// webs, mm). Param values are metres.
pub fn sizes_of(p: &Program, plates: &[f64], floor: f64) -> Vec<Size> {
    p.params
        .iter()
        .filter_map(|q| {
            let name = q.name.clone()?;
            let value = 1000.0 * q.call.value;
            if !plates.iter().any(|t| (t - value).abs() < 0.25) {
                return None;
            }
            let min = q.call.min.map_or(0.0, |m| 1000.0 * m).max(floor);
            (min < value).then_some(Size { name, value, min })
        })
        .collect()
}

/// The script edits of a variant's change to the shape: its sizes set (their params' values),
/// then its cuts ([`edits`]). The names its cuts take. Its print settings are the project's.
pub fn change_edits(p: &Program, solid: usize, f: &Frame, c: &fab_force::opt::variants::Candidate) -> Result<(Vec<Edit>, Vec<String>), String> {
    let mut out: Vec<Edit> = c.sizes.iter().map(|(name, v)| Edit::Set { site: name.clone(), value: mm(*v) }).collect();
    if c.cuts.is_empty() {
        return Ok((out, Vec::new()));
    }
    let (cuts, names) = edits(p, solid, f, &c.cuts)?;
    out.extend(cuts);
    Ok((out, names))
}

/// A share saved as a person reads the change: `-17%`, `+2%` (heavier).
fn percent(share: f64) -> String {
    match num(-100.0 * share, 0) {
        z if z == "0" || z.starts_with('-') => format!("{z}%"),
        up => format!("+{up}%"),
    }
}

/// The material a cut takes out of its plate, solid (mm3): a pocket's box to its depth, a row
/// of holes through the plate (their length less the planner's 1 mm out each side).
fn cut_volume(c: &Cut) -> f64 {
    match c {
        Cut::Holes { count, diameter, length, teardrop, .. } => {
            let r = 0.5 * diameter;
            let tip = if *teardrop { r * r * (1.0 - std::f64::consts::FRAC_PI_4) } else { 0.0 };
            *count as f64 * (std::f64::consts::PI * r * r + tip) * (length - 2.0).max(0.0)
        }
        Cut::Pocket { size, depth, .. } => size[0] * size[1] * depth,
    }
}

/// What a variant is, what it saves as printed and how safe it stays: `Pockets, infill 5%:
/// -17% as printed (24.9 g to 20.7 g), SF 2.4`. Cuts that save much less plastic than they
/// take away say why (a pocket's floor and walls print solid). `density`: the filament's
/// (g/cm3).
pub fn label(v: &Variant, base: &Score, density: f64) -> String {
    let what: Vec<String> = v
        .families
        .iter()
        .map(|f| match f {
            Family::Holes => "holes".to_string(),
            Family::Pockets => "pockets".to_string(),
            Family::Thinner => format!("thinner ({})", v.candidate.sizes.iter().map(|(n, s)| format!("{n} {}", mm(*s))).collect::<Vec<_>>().join(", ")),
            Family::Walls => match v.candidate.print.wall_loops {
                1 => "1 wall loop".to_string(),
                n => format!("{n} wall loops"),
            },
            Family::Infill => format!("infill {}%", num(100.0 * v.candidate.print.density, 0)),
        })
        .collect();
    let mut what = what.join(", ");
    if let Some(c) = what.get(..1) {
        what = c.to_uppercase() + &what[1..];
    }
    let g = |mm3: f64| fab_force::opt::mass::grams(mm3, density);
    let mut out = format!("{what}: {} as printed ({:.1} g to {:.1} g), SF {}", percent(v.saved), g(base.plastic), g(v.score.plastic), num(v.score.sf, 1));
    let cut: f64 = v.candidate.cuts.iter().map(cut_volume).sum();
    if let Some(shape) = v.shape.filter(|_| cut > 0.0) {
        let less = base.plastic - shape.plastic;
        if less < 0.5 * cut {
            let (what, why) = match v.candidate.cuts.iter().any(|c| matches!(c, Cut::Pocket { .. })) {
                true => ("pockets", "their floors and walls print solid"),
                false => ("holes", "their walls print solid"),
            };
            let change = match less >= 0.0 {
                true => format!("{} cm3 less plastic", num(less / 1000.0, 1)),
                false => format!("{} cm3 more plastic", num(-less / 1000.0, 1)),
            };
            out.push_str(&format!("; the {what} cut {} cm3 but {why}: {change}", num(cut / 1000.0, 1)));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use fab_force::bc::Kind;
    use fab_force::material::{Materials, Print, PLA};
    use fab_force::mesh::Mesh;
    use fab_force::opt::plan::{analyse, Ground, Rules};
    use fab_force::opt::variants::Candidate;
    use fab_force::solve::Options;
    use fab_force::study::{Load, Model, Spot};
    use fab_force::surface::Pick;
    use fab_model::eval::{Evaluated, Evaluator};
    use fab_model::{Change, FabModel};
    use fab_plate::machine::Machine;

    const FILE: &str = "bracket.cad.splash";
    const BRACKET: &str = include_str!("../../app/resources/bracket.cad.splash");

    fn volume(part: &fab_plate::Part) -> f64 {
        let v = &part.mesh.positions;
        part.mesh.triangles.iter().map(|t| {
            let [a, b, c] = t.map(|i| v[i as usize]);
            dot(a, cross(b, c))
        }).sum::<f64>() / 6.0
    }

    /// A model point or vector to the plate (`w` 1: a point).
    fn to_plate(m: &[[f64; 4]; 3], p: V3, w: f64) -> V3 {
        std::array::from_fn(|r| m[r][0] * p[0] + m[r][1] * p[1] + m[r][2] * p[2] + m[r][3] * w)
    }

    /// A row of teardrops through the bracket's wall and slots up into its shelf from below,
    /// planned in the printer frame: four lines of script (the slots a loop), the part's
    /// cutters taking both, and the part lighter by just the cuts' volume.
    #[test]
    fn cuts_become_builder_lines_joined_to_the_parts_cutters() {
        let model = FabModel::with_script("bracket", FILE, BRACKET);
        let mut e = Evaluator::new(Machine::dual_nozzle_350());
        let (c, ev) = model.evaluate(&mut e, None).unwrap();
        let (inst, solid) = c.part(&ev.plate.parts[0].object).unwrap_or_else(|| panic!("no part {}", ev.plate.parts[0].object));
        let p = &inst.script.program;
        let m = ev.model_to_plate[0];
        let frame = Frame::of(&m);
        // the wall at the back (z -23..-17 in the model), the shelf on the bed (y 0..6)
        let axis = unit(to_plate(&m, [0.0, 0.0, 1.0], 0.0));
        let holes = Cut::Holes { from: to_plate(&m, [-8.0, 22.0, -20.0], 1.0), to: to_plate(&m, [8.0, 22.0, -20.0], 1.0), count: 3, diameter: 8.0, length: 8.0, axis, teardrop: true };
        let slot = |x: f64| Cut::Pocket { at: to_plate(&m, [x, 0.0, 7.5], 1.0), size: [6.0, 20.0], along: unit(to_plate(&m, [1.0, 0.0, 0.0], 0.0)), axis: unit(to_plate(&m, [0.0, 1.0, 0.0], 0.0)), depth: 4.0, corner: 1.0, roof: false };
        let cuts = [holes, slot(-8.0), slot(0.0), slot(8.0)];
        let (edits, names) = edits(p, solid, &frame, &cuts).unwrap();
        assert_eq!(names, ["upright_holes", "level_slots"]);
        let code: Vec<String> = edits.iter().map(|e| format!("{e:?}")).collect();
        assert_eq!(
            edits[..2],
            [
                Edit::Add { code: "let upright_holes = m.hole_row({from: [-8mm, 22mm, -20mm] to: [8mm, 22mm, -20mm] count: 3 diameter: 8mm length: 8mm axis: [0, 0, 1]})".into(), before: Some("bracket".into()) },
                Edit::Add { code: "let level_slots = for x in [-8mm, 0, 8mm] { m.pocket({center: [x, 0, 7.5mm] size: [6mm, 20mm] depth: 4mm axis: [0, 1, 0] corner: 1mm}) }".into(), before: Some("bracket".into()) },
            ],
            "{code:#?}"
        );
        assert_eq!(edits[2..], [Edit::Operand { solid: "bracket".into(), code: "upright_holes".into() }, Edit::Operand { solid: "bracket".into(), code: "level_slots".into() }]);
        let changes: Vec<Change> = edits.into_iter().map(|edit| Change::Script { file: FILE.into(), edit }).collect();
        let (state, _, cut) = model.preview(&mut e, &changes, None).unwrap();
        assert!(state.scripts[FILE].contains("let bracket = m.subtract(body, [holes bores upright_holes level_slots])"), "{}", state.scripts[FILE]);
        // 48-sided teardrops through the 6 mm wall; slots of 6 by 20 by 4 rounded to 1 mm
        let s = (std::f64::consts::PI / 24.0).sin();
        let teardrop = 16.0 * (24.0 * s + 1.0 - 6.0 * s);
        let slot = 3.0 * (120.0 - (4.0 - std::f64::consts::PI)) + 4.0 * 18.0 + 44.0 * std::f64::consts::FRAC_PI_4 + 4.0 * std::f64::consts::PI / 6.0;
        let removed = volume(&ev.plate.parts[0]) - volume(&cut.plate.parts[0]);
        let want = 3.0 * 6.0 * teardrop + 3.0 * slot;
        assert!((removed - want).abs() < 0.02 * want, "removed {removed:.1} mm3, want {want:.1}");
    }

    /// The demo bracket bolted to a wall through its two wall holes, 100 N down on its shelf,
    /// solved from its script: the holes its study plans, written into the script, the edited
    /// script solved again. The variant is a line per row, lighter as printed, and it holds. At
    /// 40 % infill: at 15 % a hole's own walls print about what a hole under 3.7 mm across saves,
    /// and the bolts' clamps leave the wall a row only just.
    #[test]
    fn the_bracket_script_takes_the_holes_its_study_plans_and_holds() {
        let model = FabModel::with_script("bracket", FILE, BRACKET);
        let mut e = Evaluator::new(Machine::dual_nozzle_350());
        let (c, ev) = model.evaluate(&mut e, None).unwrap();
        let m = ev.model_to_plate[0];
        let at = |p: V3| to_plate(&m, p, 1.0);
        let (x, wall_y) = (0.27 * 64.0, 6.0 + 9.0 + (52.0 - 15.0) * 0.55);
        let loads = [
            Load::new(Kind::Fixed, Spot::Seed { pick: Pick::Hole, at: at([-x, wall_y + 2.3, -20.0]) }),
            Load::new(Kind::Fixed, Spot::Seed { pick: Pick::Hole, at: at([x, wall_y + 2.3, -20.0]) }),
            Load::new(Kind::Force([0.0, 0.0, -50.0]), Spot::Seed { pick: Pick::Face, at: at([0.0, 6.0, 10.0]) }),
        ];
        let print = Print { density: 0.4, ..Print::default() };
        let mats = Materials::new(PLA, &print, true);
        let sample = |ev: &Evaluated| -> Model {
            let part = &ev.plate.parts[0];
            let b = part.bounds();
            Model::of_mesh(b.min, b.max, 2.0, &Mesh::new(&part.mesh.positions, &part.mesh.triangles), &mats)
        };
        let base_model = sample(&ev);
        let base = base_model.solve(&mats, &base_model.conditions(&loads), &Options::default(), &mut |_, _| false).unwrap();
        let conditions = base_model.conditions(&loads);
        let g = Ground { model: &base_model, results: &base.results, conditions: &conditions, materials: &mats };
        let a = analyse(&g, &Rules::default(), 0.3);
        let summary = fab_force::opt::summary::summarise(&g, &a);
        let features = &ev.features[0];
        let near = |p: V3| {
            // the smallest feature within 3 mm of the point
            features.iter().filter_map(|ft| match &ft.region {
                fab_paint_field::Region::Operand(of) => {
                    let d = of.iter().map(|o| makepad_csg_sdf::distance_slot(&o.own, makepad_csg_math::Vec3d::new(p[0], p[1], p[2])).0).fold(f64::INFINITY, f64::min);
                    (d < 3.0).then_some((ft.volume, ft.name.clone()))
                }
                _ => None,
            }).min_by(|x, y| x.0.total_cmp(&y.0)).map(|x| x.1)
        };
        let text = describe(&summary, &Frame::of(&m), "bracket", fab_force::opt::mass::grams(summary.plastic, 1.24), &near);
        eprintln!("{text}");
        assert!(text.contains("upright plate 6mm thick, through along Z"), "{text}");
        let holes = a.holes(1.0);
        assert!(!holes.is_empty(), "the wall takes holes once its back is free");
        let frame = Frame::of(&m);
        let plates: Vec<f64> = a.plates.iter().map(|p| frame.len(p.faces[1] - p.faces[0])).collect();
        let sizes = sizes_of(&c.part(&ev.plate.parts[0].object).unwrap().0.script.program, &plates, 2.0 * a.web);
        assert!(matches!(&sizes[..], [Size { name, value, min }] if name == "thickness" && (value - 6.0).abs() < 1e-9 && (min - 2.0 * a.web).abs() < 1e-9), "the plates are the `thickness` param: {sizes:?} of {plates:?}");
        let (inst, solid) = c.part(&ev.plate.parts[0].object).unwrap();
        let (edits, names) = edits(&inst.script.program, solid, &Frame::of(&m), &holes).unwrap();
        assert_eq!(edits.len(), 2 * names.len(), "a line and an operand per row: {edits:?}");
        let changes: Vec<Change> = edits.iter().cloned().map(|edit| Change::Script { file: FILE.into(), edit }).collect();
        let (_, _, cut) = model.preview(&mut e, &changes, None).unwrap();
        // lighter as printed, and still one piece the bolts hold (the variants' solves are the
        // runner's)
        let variant = sample(&cut);
        let plastic = |m: &Model| fab_force::opt::mass::plastic(&m.grid, &mats);
        assert!(plastic(&variant) < plastic(&base_model), "lighter as printed: {} -> {}", plastic(&base_model), plastic(&variant));
        let posed = variant.pose(&variant.conditions(&loads)).unwrap();
        assert_eq!(posed.loose.pieces, base.loose.pieces, "no piece cut off");
        let lines: Vec<String> = edits.iter().filter_map(|e| match e { Edit::Add { code, .. } => Some(code.clone()), _ => None }).collect();
        eprintln!("{}", lines.join("\n"));
    }

    /// A project written before objects were named: an object known by its line (`box@7`)
    /// placed by that name, its code without one. An optimiser pass on the plate above it adds
    /// its cut's line before the plate's `let`; that write names the object, so it stays where
    /// it was placed.
    #[test]
    fn an_optimiser_pass_leaves_an_unnamed_object_in_place() {
        const T: &str = "t.cad.splash";
        let source = format!("{}let plate = m.box({{size: [40mm 6mm 30mm]}})\nobject(plate)\nobject(m.box({{size: [10mm 10mm 10mm]}}))\n", fab_model::program::EMPTY);
        let mut e = Evaluator::new(Machine::dual_nozzle_350());
        let mut first = FabModel::with_script("t", T, &source);
        let place = [Change::Place { instance: "t".into(), object: "box@7".into(), arg: fab_model::PlaceArg::At(Some([60.0, 70.0])) }];
        first.apply(&mut e, "place", first.head(), &place, None).unwrap();
        let mut model = FabModel::load("t", &first.project_text(), std::collections::BTreeMap::from([(T.to_string(), source.clone())])).unwrap();
        let (c, ev) = model.evaluate(&mut e, None).unwrap();
        let at = |ev: &Evaluated| ev.plate.parts.iter().find(|p| p.object == "t/box@7").map(|p| p.mesh.bounds().center());
        let placed = (model.project_text(), at(&ev).expect("loads placed"));
        let k = ev.plate.parts.iter().position(|p| p.object == "t/plate").unwrap();
        let m = ev.model_to_plate[k];
        let (inst, solid) = c.part("t/plate").unwrap();
        let holes = Cut::Holes { from: to_plate(&m, [-8.0, 0.0, 0.0], 1.0), to: to_plate(&m, [8.0, 0.0, 0.0], 1.0), count: 3, diameter: 4.0, length: 8.0, axis: unit(to_plate(&m, [0.0, 1.0, 0.0], 0.0)), teardrop: false };
        let (edits, _) = edits(&inst.script.program, solid, &Frame::of(&m), &[holes]).unwrap();
        let changes: Vec<Change> = edits.into_iter().map(|edit| Change::Script { file: T.into(), edit }).collect();
        let (_, built) = model.apply(&mut e, "cut", model.head(), &changes, None).unwrap();
        let (_, ev) = built.unwrap();
        let script = model.source(T).unwrap();
        assert!(script.contains("m.subtract(m.box"), "{script}");
        assert_eq!((model.project_text(), at(&ev).expect("still placed")), placed, "{script}");
        assert!(script.contains("object(m.box({size: [10mm 10mm 10mm]}), name: \"box@7\")"), "{script}");
    }

    /// A variant's label: what it changes, what it saves as printed (grams too), its SF.
    #[test]
    fn a_variant_label_reads_its_change_and_its_mass_as_printed() {
        let print = Print { density: 0.05, ..Print::default() };
        let base = Score { plastic: 20_000.0, sf: 5.9 };
        let v = |families: Vec<Family>, cuts: Vec<Cut>, sizes: Vec<(String, f64)>, shape: f64, plastic: f64| Variant {
            families,
            candidate: Candidate { cuts, sizes, print: print.clone() },
            score: Score { plastic, sf: 2.43 },
            saved: 1.0 - plastic / 20_000.0,
            met: true,
            shape: Some(Score { plastic: shape, sf: 2.5 }),
        };
        let pocket = Cut::Pocket { at: [0.0; 3], size: [30.0, 20.0], along: [1.0, 0.0, 0.0], axis: [0.0, 0.0, 1.0], depth: 4.0, corner: 1.0, roof: false };
        // 2.4 cm3 cut, 0.6 cm3 less plastic: the floor and walls it leaves print solid
        assert_eq!(
            label(&v(vec![Family::Pockets, Family::Infill], vec![pocket], Vec::new(), 19_400.0, 16_600.0), &base, 1.24),
            "Pockets, infill 5%: -17% as printed (24.8 g to 20.6 g), SF 2.4; the pockets cut 2.4 cm3 but their floors and walls print solid: 0.6 cm3 less plastic"
        );
        assert_eq!(label(&v(vec![Family::Thinner], Vec::new(), vec![("thickness".into(), 4.0)], 13_400.0, 13_400.0), &base, 1.24), "Thinner (thickness 4mm): -33% as printed (24.8 g to 16.6 g), SF 2.4");
        let mut walls = v(vec![Family::Walls, Family::Infill], Vec::new(), Vec::new(), 17_000.0, 14_000.0);
        walls.candidate.print = Print { wall_loops: 1, density: 0.1, ..Print::default() };
        assert_eq!(label(&walls, &base, 1.24), "1 wall loop, infill 10%: -30% as printed (24.8 g to 17.4 g), SF 2.4");
        // a row of holes whose walls weigh more than the infill they take
        let holes = Cut::Holes { from: [0.0; 3], to: [10.0, 0.0, 0.0], count: 2, diameter: 6.0, length: 8.0, axis: [0.0, 0.0, 1.0], teardrop: false };
        assert_eq!(
            label(&v(vec![Family::Holes], vec![holes], Vec::new(), 20_400.0, 20_400.0), &base, 1.24),
            "Holes: +2% as printed (24.8 g to 25.3 g), SF 2.4; the holes cut 0.3 cm3 but their walls print solid: 0.4 cm3 more plastic"
        );
    }
}
