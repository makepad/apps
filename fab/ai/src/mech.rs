//! `fab.joints` and `fab.simulate`: the program's joints checked and the
//! assembled piece run in box3d (a flexing one on the dynamics engine),
//! answered first with the verdicts and in a few numbers per joint, so the AI
//! can model, check, run, read and fix in a loop.

use crate::out::{arr, deg, flag, int, mm, mm3, n, obj, opt, s, S};
use makepad_csg_mech::sim::{Scenario, SimOptions, SimReport};
use makepad_csg_mech::{JointCheck, Risk};
use makepad_model::json::Value;

/// One-line fixes for a joint that fails its check.
fn fixes(c: &JointCheck, layer: f64) -> Vec<String> {
    let mut out = Vec::new();
    let [x, y, z] = c.gap.at;
    if c.gap.gap < 0.0 {
        // the gap search stops at the first overlap: its depth is a bound
        out.push(format!("the parts overlap by at least {:.2} mm near [{x:.1}, {y:.1}, {z:.1}]: move the joint or trim a part there", -c.gap.gap));
    } else if c.gap.gap < 0.5 * c.clearance {
        out.push(format!("the parts come within {:.2} mm near [{x:.1}, {y:.1}, {z:.1}]: move the joint or trim a part there", c.gap.gap));
    }
    if c.layers.tack[0] > 0.5 || c.layers.tack[1] > 2.0 {
        out.push(format!("{:.0} mm2 print one layer or less over the other part (they fuse): clearance_axial: {:.2}mm, or turn the joint so its pin is along the print Z", c.layers.tack[0] + c.layers.tack[1], 3.0 * layer));
    }
    if c.layers.layer_gap < 0.7 * c.clearance {
        out.push(format!("in-layer gap {:.2} mm: clearance: {:.2}mm", c.layers.layer_gap, c.clearance + 0.1));
    }
    let slider = c.kind == "slider";
    if c.effective.1 - c.effective.0 < if slider { 1.0 } else { 10f64.to_radians() } {
        out.push(if slider { "the carriage hardly slides: make the rail longer than the carriage, or trim what it carries where it meets the rail".into() } else { "the parts block each other almost at once: round or shorten the bodies next to the joint".into() });
    }
    if c.retention.is_some_and(|r| r < 0.3) {
        out.push(match c.kind {
            "ball" => "the ball is not held: a smaller cone or a larger radius".into(),
            _ => "the ridge is not held in its groove: make the parts taller where they meet, or give depth:".into(),
        });
    }
    out
}

/// The joint checks of one jointed object (`mech`; none: the plate has no joints).
pub fn check_value(mech: Option<&fab_model::mech::Mech>, checks: &[JointCheck]) -> S {
    let Some(mech) = mech else { return obj(vec![("joints", arr(Vec::new())), ("note", s("no object has joints: m.hinge, m.ball, m.slider, m.bearing, m.chain"))]) };
    let layer = mech.plan.profile.layer;
    let all_ok = checks.iter().all(|c| c.ok());
    let worst = checks.iter().map(|c| c.risk).max().unwrap_or(Risk::Low);
    let joints = checks.iter().map(|c| {
        let id = mech.joint_ids.get(c.joint).cloned().unwrap_or_default();
        // a slider's limits in millimetres, the others' in degrees; a hinge's, a slider's and a
        // bearing's touch range as measured each way
        let lim = |p: (f64, f64)| if c.kind == "slider" { arr([mm(p.0), mm(p.1)]) } else { arr([deg(p.0.to_degrees()), deg(p.1.to_degrees())]) };
        let field = if c.kind == "ball" { c.field } else { (-c.field.0, c.field.1) };
        obj(vec![
            ("joint", s(id)),
            ("ok", flag(c.ok())),
            ("kind", s(c.kind)),
            ("parts", arr([s(&mech.names[c.a]), s(&mech.names[c.b])])),
            ("clearance", mm(c.clearance)),
            // overlapping parts: how deep is a bound (the gap search stops at the first overlap)
            if c.gap.gap < 0.0 { ("overlap_at_least", mm(-c.gap.gap)) } else { ("min_gap", mm(c.gap.gap)) },
            (if c.gap.gap < 0.0 { "overlap_at" } else { "min_gap_at" }, mm3(c.gap.at)),
            ("layer_gap", mm(c.layers.layer_gap)),
            ("tack_mm2", obj(vec![("on", n(c.layers.tack[0])), ("over_1_layer", n(c.layers.tack[1])), ("over_2", n(c.layers.tack[2])), ("over_3", n(c.layers.tack[3]))])),
            ("fuse_risk", s(c.risk.name())),
            ("limits", obj(vec![("declared", opt(c.declared.map(lim))), ("parts_touch_at", lim(field)), ("used", lim(c.effective))])),
            ("captive_mm", opt(c.retention.map(mm))),
            ("notes", arr(c.notes.iter().map(|x| s(&x.text)))),
            ("fix", arr(fixes(c, layer).into_iter().map(s))),
            ("ms", n(c.millis.round())),
        ])
    });
    obj(vec![
        ("ok", flag(all_ok)),
        ("geometry", s("design")),
        ("worst_fuse_risk", s(worst.name())),
        ("profile", obj(vec![("nozzle", mm(mech.plan.profile.nozzle)), ("layer", mm(layer)), ("material", s(&mech.plan.profile.material.name))])),
        ("joints", arr(joints)),
    ])
}

fn num(v: &Value) -> Option<f64> {
    match v {
        Value::F64(f) if f.is_finite() => Some(*f),
        Value::Int(i) => Some(*i as f64),
        _ => None,
    }
}

/// The sim's options from the call; `slides`: which joints are sliders (a pose gives their
/// places in mm, the others' angles in degrees).
pub fn options(args: &Value, slides: &[bool]) -> Result<SimOptions, String> {
    let scenario = match args.get("scenario").and_then(Value::as_str).unwrap_or("hang") {
        "hang" => Scenario::Hang,
        "shake" => Scenario::Shake,
        "drop" => Scenario::Drop,
        "pose" => {
            let angles = match args.get("angles") {
                Some(Value::Arr(a)) => a
                    .iter()
                    .enumerate()
                    .map(|(i, v)| num(v).map(|x| if slides.get(i).copied().unwrap_or(false) { x } else { x.to_radians() }).ok_or("angles are degrees, one per joint (a slider's: mm)"))
                    .collect::<Result<Vec<_>, _>>()?,
                _ => return Err("pose needs angles: [deg per joint (a slider's: mm), in fab.joints order]".into()),
            };
            Scenario::Pose(angles)
        }
        other => return Err(format!("scenario is hang, shake, drop or pose, not {other}")),
    };
    let seconds = args.get("seconds").and_then(num).unwrap_or(if matches!(scenario, Scenario::Hang) { 15.0 } else { 8.0 });
    let seed = args.get("seed").and_then(num).unwrap_or(1.0) as u64;
    Ok(SimOptions { scenario, seconds, seed, ..Default::default() })
}

pub fn sim_value(r: &SimReport) -> S {
    obj(vec![
        ("scenario", s(r.scenario)),
        ("geometry", s("design")),
        ("articulates", flag(r.articulates)),
        ("hangs_right", flag(r.hangs_right)),
        ("binding", arr(r.binding.iter().map(s))),
        ("settled_at_s", opt(r.settle.map(n))),
        ("limit_hits", arr(r.joints.iter().filter(|j| j.limit_hits > 0).map(|j| obj(vec![("joint", s(&j.name)), ("hits", int(j.limit_hits))])))),
        ("self_contacts", arr(r.self_contacts.iter().map(|(a, b, k)| obj(vec![("parts", arr([s(a), s(b)])), ("times", int(*k))])))),
        ("floor_contacts", int(r.floor_contacts)),
        ("pose_drift", opt(r.drift.map(|d| deg(d.to_degrees())))),
        (
            "joints",
            // a weld holds two parts as one (a bond): no joint to report
            arr(r.joints.iter().filter(|j| !j.holds).map(|j| {
                // a slider's places in millimetres, the others' angles in degrees
                let at = |x: f64| if j.slide { mm(x) } else { deg(x.to_degrees()) };
                obj(vec![
                    ("joint", s(&j.name)),
                    ("moved", flag(j.moved)),
                    ("range", arr([at(j.min), at(j.max)])),
                    ("final", at(j.last)),
                    ("off_hanging", deg(j.hang_error.to_degrees())),
                    ("stick_band", deg(j.dead_band.to_degrees())),
                    ("peak_force_n", n(j.peak_force)),
                ])
            })),
        ),
        ("final_pose", arr(r.final_pose.iter().map(|(name, c, rot)| obj(vec![("part", s(name)), ("com", mm3(*c)), ("turn", arr(rot.iter().map(|d| deg(*d))))])))),
        ("simulated_s", n(r.seconds)),
        ("cancelled", flag(r.cancelled)),
        ("ms", n(r.wall_ms.round())),
    ])
}
