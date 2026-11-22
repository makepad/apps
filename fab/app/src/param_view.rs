//! The model's parameter model ([`fab_model::params`]) as the shell's view of it
//! ([`fab::params`]): handles and object boxes taken from the model frame through each part's
//! model-to-plate transform into printer millimetres on the plate, where the view, the
//! headset and the panel work.

use fab::params::{ObjectBox, ParamHandle, ParamKind, ParamValue, ParamView, PartParamsView, Wire};
use fab_model::eval::Evaluated;
use fab_model::params::{xf_point, Anchor, Kind, PValue, PartParams};
use std::sync::Arc;

pub fn kind(k: Kind) -> ParamKind {
    match k {
        Kind::Length => ParamKind::Length,
        Kind::Radius => ParamKind::Radius,
        Kind::Diameter => ParamKind::Diameter,
        Kind::Angle => ParamKind::Angle,
        Kind::Count => ParamKind::Count,
        Kind::Number => ParamKind::Number,
        Kind::Ratio => ParamKind::Ratio,
        Kind::Bool => ParamKind::Bool,
        Kind::Choice => ParamKind::Choice,
        Kind::Point => ParamKind::Point,
        Kind::Profile => ParamKind::Profile,
        Kind::Seed => ParamKind::Seed,
        Kind::Growth => ParamKind::Growth,
    }
}

pub fn value(v: &PValue) -> ParamValue {
    match v {
        PValue::Num(x) => ParamValue::Num(*x),
        PValue::Bool(b) => ParamValue::Bool(*b),
        PValue::Choice(i) => ParamValue::Choice(*i),
        PValue::Point(c) => ParamValue::Point(c.clone()),
        PValue::Profile(p) => ParamValue::Profile(p.clone()),
    }
}

pub fn model_value(v: &ParamValue) -> PValue {
    match v {
        ParamValue::Num(x) => PValue::Num(*x),
        ParamValue::Bool(b) => PValue::Bool(*b),
        ParamValue::Choice(i) => PValue::Choice(*i),
        ParamValue::Point(c) => PValue::Point(c.clone()),
        ParamValue::Profile(p) => PValue::Profile(p.clone()),
    }
}

fn handle(a: &Anchor) -> ParamHandle {
    match a.clone() {
        Anchor::Linear { from, to, rate } => ParamHandle::Linear { from, to, rate },
        Anchor::Radius { center, axis, radius, rate } => ParamHandle::Radius { center, axis, radius, rate },
        Anchor::Angle { center, axis, from, radius, angle, rate } => ParamHandle::Angle { center, axis, from, radius, angle, rate },
        Anchor::Point { at, axes } => ParamHandle::Point { at, axes },
        Anchor::Profile { origin, u, v, points } => ParamHandle::Profile { origin, u, v, points },
        Anchor::Count { at, axis } => ParamHandle::Count { at, axis },
    }
}

/// The density an exact body's readout weighs it at.
const PLA_G_PER_CM3: f64 = 1.24;

/// One view per plate part, same order.
pub fn views(model: &[PartParams], ev: &Evaluated) -> Vec<Arc<PartParamsView>> {
    let mut out = vec![Arc::new(PartParamsView::default()); ev.plate.parts.len() + ev.plate.excluded.len()];
    for pp in model {
        let Some(m) = ev.placed_frame(pp.part) else { continue };
        let m: &[[f64; 4]] = &m[..];
        let boxes = pp
            .objects
            .iter()
            .filter_map(|o| {
                let info = ev.objects.iter().find(|i| &i.name == o)?;
                let (lo, hi) = info.bounds?;
                let (mut min, mut max) = ([f64::MAX; 3], [f64::MIN; 3]);
                for c in 0..8 {
                    let p = [if c & 1 == 0 { lo[0] } else { hi[0] }, if c & 2 == 0 { lo[1] } else { hi[1] }, if c & 4 == 0 { lo[2] } else { hi[2] }];
                    let q = xf_point(m, p);
                    for k in 0..3 {
                        min[k] = min[k].min(q[k]);
                        max[k] = max[k].max(q[k]);
                    }
                }
                Some(ObjectBox { object: o.clone(), min, max, origin: xf_point(m, [info.world[0][3], info.world[1][3], info.world[2][3]]) })
            })
            .collect();
        let params = pp
            .params
            .iter()
            .map(|p| ParamView {
                key: p.key.clone(),
                label: p.label.clone(),
                group: p.group.clone(),
                kind: kind(p.kind),
                unit: p.unit.clone(),
                value: value(&p.value),
                min: p.min,
                max: p.max,
                step: p.step,
                choices: p.choices.iter().map(|(l, _)| l.clone()).collect(),
                declared: p.declared,
                derived: p.derived,
                detail: p.detail,
                hint: p.hint.clone(),
                objects: p.objects.clone(),
                handle: p.anchor.as_ref().map(|a| handle(&a.transformed(m))),
                site: Some(p.site.clone()),
                setting: fab_model::params::region_setting(&p.key),
            })
            .collect();
        if let Some(slot) = out.get_mut(pp.part) {
            let exact = pp.objects.iter().find_map(|o| ev.objects.iter().find(|i| &i.name == o).and_then(|i| i.exact));
            let readout = exact.map_or_else(String::new, |m| {
                let cm3 = m.volume / 1000.0;
                format!("Exact: {cm3:.2} cm³ · {:.2} cm² · {:.1} g in PLA · {} faces", m.area / 100.0, cm3 * PLA_G_PER_CM3, m.faces)
            });
            let wires = pp.wires.iter().map(|w| Wire { object: w.object.clone(), points: w.points.iter().map(|q| xf_point(m, *q)).collect(), edges: w.edges.clone(), keys: w.keys.clone() }).collect();
            *slot = Arc::new(PartParamsView { object: pp.object.clone(), readout, params, boxes, wires });
        }
    }
    out
}
