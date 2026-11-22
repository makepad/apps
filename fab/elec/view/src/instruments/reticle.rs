//! Capture features as hover readouts and the geometry behind an answer. No drawing context.

use super::simfeed::{Feature, FeatureKind};
use std::fmt::Write;

/// Nearest feature within the radius. One binary search, then the neighbours (including ties).
/// At equal distances a transition wins over a level; otherwise the earlier feature wins.
pub fn snap<'a>(features: &'a [Feature], t: f64, radius_s: f64) -> Option<&'a Feature> {
    if !t.is_finite() || radius_s.is_nan() || radius_s < 0.0 { return None; }
    let at = features.partition_point(|f| f.t < t);
    let mut best: Option<&Feature> = None;
    let mut distance = radius_s;
    let edge = |f: &Feature| matches!(f.kind, FeatureKind::Rise | FeatureKind::Fall);
    let mut consider = |f: &'a Feature| {
        let d = (f.t - t).abs();
        if d > distance { return false; }
        if best.is_none() || d < distance || best.is_some_and(|old| edge(f) && !edge(old)) {
            best = Some(f);
            distance = d;
        }
        true
    };
    for f in features[..at].iter().rev() { if !consider(f) { break; } }
    for f in &features[at..] { if !consider(f) { break; } }
    best
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Geometry {
    Period { t0: f64, t1: f64 },
    High { t0: f64, t1: f64 },
    Levels { top: f64, base: f64 },
    Edge { t10: f64, t90: f64, v10: f64, v90: f64 },
}

/// The first complete instance starting at/after zero, or the first complete one in the record.
/// Duty needs a complete rise/fall/rise cycle, just as the measured duty answer does.
pub fn answer_geometry(name: &str, features: &[Feature]) -> Option<Geometry> {
    let levels = if name == "rise" {
        let Geometry::Levels { top, base } = answer_geometry("Vpp", features)? else { return None };
        Some((top, base))
    } else { None };
    let mut first = None;
    let (mut rise, mut fall, mut top, mut base) = (None::<&Feature>, None::<&Feature>, None::<&Feature>, None::<&Feature>);
    for f in features {
        let candidate = match name {
            "freq" | "duty" => match f.kind {
                FeatureKind::Rise => {
                    let candidate = rise.filter(|r| r.t < f.t).and_then(|r| {
                        if name == "freq" { Some((r.t, Geometry::Period { t0: r.t, t1: f.t })) }
                        else { fall.filter(|end| end.t > r.t && end.t < f.t).map(|end| (r.t, Geometry::High { t0: r.t, t1: end.t })) }
                    });
                    rise = Some(f); fall = None;
                    candidate
                }
                FeatureKind::Fall => { if fall.is_none() { fall = Some(f); } None }
                _ => None,
            },
            "Vpp" => {
                match f.kind { FeatureKind::Top => top = Some(f), FeatureKind::Base => base = Some(f), _ => continue }
                top.zip(base).map(|(top, base)| (top.t.min(base.t), Geometry::Levels { top: top.v, base: base.v }))
            }
            "rise" => if f.kind == FeatureKind::Rise {
                f.crossings.and_then(|[t10, _, t90]| {
                    let (top, base) = levels?;
                    (t90 > t10).then(|| (t10, Geometry::Edge { t10, t90, v10: base + (top - base) * 0.1, v90: base + (top - base) * 0.9 }))
                })
            } else { None },
            _ => return None,
        };
        if let Some((start, geometry)) = candidate {
            if start >= 0.0 { return Some(geometry); }
            if first.is_none() { first = Some(geometry); }
        }
    }
    first
}

/// Same engineering prefixes and trimmed four-digit precision as the answer's `si_label`.
pub fn reticle_label(feature: Option<&Feature>, t: f64, v: f64, unit: &str) -> String {
    let mut label = ReticleLabel::default();
    label.update(feature, t, v, unit);
    label.text
}

// A rounded numeric key avoids formatting again when the pointer moves within one displayed
// digit. Writing directly into the retained String avoids temporary SI strings on hover.
#[derive(Clone, Copy, Debug)]
struct Number { value: f64, decimals: usize, group: i32, negative: bool }

impl PartialEq for Number {
    fn eq(&self, other: &Self) -> bool {
        // After trailing zeroes are trimmed, the same rounded value has the same text even
        // across a precision boundary (9.9999 and 10 both display as 10).
        self.value == other.value && self.group == other.group && self.negative == other.negative
    }
}

impl Number {
    fn new(value: f64, unit: &str) -> Self {
        if value == 0.0 || !value.is_finite() { return Self { value: 0.0, decimals: 0, group: 0, negative: false }; }
        let fixed = matches!(unit, "%" | "dBV");
        let mut group = if fixed { 0 } else { ((value.abs().log10() / 3.0).floor() as i32).clamp(-10, 10) };
        let mut scaled = value.abs() / 10.0_f64.powi(group * 3);
        if !fixed && scaled >= 999.95 && group < 10 { group += 1; scaled /= 1000.0; }
        let decimals = (3.0 - scaled.log10().floor()).clamp(0.0, 15.0) as usize;
        let scale = 10.0_f64.powi(decimals as i32);
        Self { value: (scaled * scale).round_ties_even() / scale, decimals, group, negative: value < 0.0 }
    }

    fn write(self, text: &mut String, unit: &str) {
        const PREFIXES: [&str; 21] = ["q", "r", "y", "z", "a", "f", "p", "n", "µ", "m", "", "k", "M", "G", "T", "P", "E", "Z", "Y", "R", "Q"];
        if self.negative { text.push('−'); }
        let _ = write!(text, "{:.*}", self.decimals, self.value);
        if self.decimals > 0 {
            while text.ends_with('0') { text.pop(); }
            if text.ends_with('.') { text.pop(); }
        }
        text.push(' ');
        text.push_str(PREFIXES[(self.group + 10) as usize]);
        text.push_str(unit);
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum LabelKey { Edge(FeatureKind, Number), Level(FeatureKind, Number), Free(Number, Number) }

pub(super) struct ReticleLabel {
    key: Option<LabelKey>,
    unit: String,
    pub text: String,
}

impl Default for ReticleLabel {
    fn default() -> Self { Self { key: None, unit: String::with_capacity(8), text: String::with_capacity(96) } }
}

impl ReticleLabel {
    pub fn update(&mut self, feature: Option<&Feature>, t: f64, v: f64, unit: &str) {
        let key = match feature {
            Some(f) => match f.kind {
                FeatureKind::Rise | FeatureKind::Fall if f.crossings.is_some() => {
                    let [ten, _, ninety] = f.crossings.unwrap();
                    LabelKey::Edge(f.kind, Number::new((ninety - ten).abs(), "s"))
                }
                FeatureKind::Top | FeatureKind::Base => LabelKey::Level(f.kind, Number::new(f.v, unit)),
                _ => LabelKey::Free(Number::new(t, "s"), Number::new(v, unit)),
            },
            None => LabelKey::Free(Number::new(t, "s"), Number::new(v, unit)),
        };
        if self.key == Some(key) && self.unit == unit { return; }
        self.key = Some(key);
        if self.unit != unit { self.unit.clear(); self.unit.push_str(unit); }
        self.text.clear();
        match key {
            LabelKey::Edge(kind, value) => {
                self.text.push_str(if kind == FeatureKind::Rise { "rise " } else { "fall " });
                value.write(&mut self.text, "s");
            }
            LabelKey::Level(kind, value) => {
                self.text.push_str(if kind == FeatureKind::Top { "top " } else { "base " });
                value.write(&mut self.text, unit);
            }
            LabelKey::Free(time, value) => {
                self.text.push_str("t "); time.write(&mut self.text, "s");
                self.text.push_str("  "); value.write(&mut self.text, unit);
            }
        }
    }
}
