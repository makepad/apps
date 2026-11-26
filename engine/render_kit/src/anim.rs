//! Keyable parameter values: every effect parameter is a [`Value`], either
//! constant or keyed over time ([`Anim`]), so a frame is a pure function of
//! `t`. Key semantics follow the edit model's `keys(...)`:
//! a key's ease shapes the segment that ARRIVES at it, the first key's value
//! holds before it and the last key's after it.

use std::fmt;
use std::sync::Arc;

/// One parameter value.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Num(f64),
    Vec2([f64; 2]),
    Vec3([f64; 3]),
    /// Straight (not premultiplied) RGBA, 0..1.
    Color([f64; 4]),
    Bool(bool),
    /// One of a parameter's named choices (`@gaussian`, `@outside`, …).
    Choice(String),
    /// Free text: a file path (`lut.file`), SVG path data.
    Text(String),
    /// Another layer of the host, by name (a track matte, a displacement
    /// map, a particle emitter's shape). The host hands its texture in
    /// [`crate::FxInputs`].
    Layer(String),
    /// Points in pixels (a path, an emitter's points).
    Points(Vec<[f64; 2]>),
    /// Gradient stops: position 0..1 and a straight RGBA colour.
    Stops(Vec<(f64, [f64; 4])>),
}

impl Value {
    pub fn kind_name(&self) -> &'static str {
        match self {
            Value::Num(_) => "number",
            Value::Vec2(_) => "vec2",
            Value::Vec3(_) => "vec3",
            Value::Color(_) => "color",
            Value::Bool(_) => "bool",
            Value::Choice(_) => "choice",
            Value::Text(_) => "text",
            Value::Layer(_) => "layer",
            Value::Points(_) => "points",
            Value::Stops(_) => "stops",
        }
    }

    /// `a` → `b` at `u` (0..1): numbers, vectors, colours, and points and
    /// stops of equal length interpolate component-wise; everything else
    /// holds `a` until `u` reaches 1.
    pub fn lerp(a: &Value, b: &Value, u: f64) -> Value {
        let mix = |x: f64, y: f64| x + (y - x) * u;
        match (a, b) {
            (Value::Num(x), Value::Num(y)) => Value::Num(mix(*x, *y)),
            (Value::Vec2(x), Value::Vec2(y)) => Value::Vec2([mix(x[0], y[0]), mix(x[1], y[1])]),
            (Value::Vec3(x), Value::Vec3(y)) => Value::Vec3([mix(x[0], y[0]), mix(x[1], y[1]), mix(x[2], y[2])]),
            (Value::Color(x), Value::Color(y)) => Value::Color([mix(x[0], y[0]), mix(x[1], y[1]), mix(x[2], y[2]), mix(x[3], y[3])]),
            (Value::Points(x), Value::Points(y)) if x.len() == y.len() => {
                Value::Points(x.iter().zip(y).map(|(p, q)| [mix(p[0], q[0]), mix(p[1], q[1])]).collect())
            }
            (Value::Stops(x), Value::Stops(y)) if x.len() == y.len() => Value::Stops(
                x.iter()
                    .zip(y)
                    .map(|((p, c), (q, d))| (mix(*p, *q), [mix(c[0], d[0]), mix(c[1], d[1]), mix(c[2], d[2]), mix(c[3], d[3])]))
                    .collect(),
            ),
            _ => {
                if u >= 1.0 {
                    b.clone()
                } else {
                    a.clone()
                }
            }
        }
    }
}

/// How a segment arrives at its key. The first five are the edit model's
/// (`ease_in` etc. are quadratic, exactly as `video_cut::Ease`); the rest
/// are the standard motion-design set.
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub enum Ease {
    #[default]
    Linear,
    EaseIn,
    EaseOut,
    EaseInOut,
    /// The previous value until this key's time, then a jump.
    Hold,
    InCubic,
    OutCubic,
    InOutCubic,
    InQuint,
    OutQuint,
    InOutQuint,
    InExpo,
    OutExpo,
    InOutExpo,
    InBack,
    OutBack,
    InOutBack,
    OutElastic,
    OutBounce,
    /// A damped spring settling on the key: stiffness `k`, damping `d`
    /// (`@spring(k, d)`; critically damped at `d = 2·sqrt(k)`).
    Spring { k: f64, d: f64 },
    /// CSS `cubic-bezier(x1, y1, x2, y2)`.
    Bezier([f64; 4]),
}

impl Ease {
    /// Every named ease, as written after `@`.
    pub const NAMES: &'static [&'static str] = &[
        "linear", "ease_in", "ease_out", "ease_in_out", "hold", "ease_in_cubic", "ease_out_cubic", "ease_in_out_cubic",
        "ease_in_quint", "ease_out_quint", "ease_in_out_quint", "ease_in_expo", "ease_out_expo", "ease_in_out_expo",
        "ease_in_back", "ease_out_back", "ease_in_out_back", "ease_out_elastic", "ease_out_bounce",
    ];

    pub fn from_id(id: &str) -> Option<Self> {
        Some(match id {
            "linear" => Self::Linear,
            "ease_in" => Self::EaseIn,
            "ease_out" => Self::EaseOut,
            "ease_in_out" => Self::EaseInOut,
            "hold" => Self::Hold,
            "ease_in_cubic" => Self::InCubic,
            "ease_out_cubic" => Self::OutCubic,
            "ease_in_out_cubic" => Self::InOutCubic,
            "ease_in_quint" => Self::InQuint,
            "ease_out_quint" => Self::OutQuint,
            "ease_in_out_quint" => Self::InOutQuint,
            "ease_in_expo" => Self::InExpo,
            "ease_out_expo" => Self::OutExpo,
            "ease_in_out_expo" => Self::InOutExpo,
            "ease_in_back" => Self::InBack,
            "ease_out_back" => Self::OutBack,
            "ease_in_out_back" => Self::InOutBack,
            "ease_out_elastic" => Self::OutElastic,
            "ease_out_bounce" => Self::OutBounce,
            _ => return None,
        })
    }

    /// The segment fraction `u` (0..1) through the ease. Exactly 0 at 0 and
    /// 1 at 1 for every ease (a spring is normalised to land on its key).
    pub fn apply(self, u: f64) -> f64 {
        let u = if u.is_nan() { 0.0 } else { u.clamp(0.0, 1.0) };
        let inout = |f: fn(f64) -> f64, u: f64| if u < 0.5 { f(2.0 * u) * 0.5 } else { 1.0 - f(2.0 - 2.0 * u) * 0.5 };
        const C1: f64 = 1.70158;
        match self {
            Self::Linear => u,
            Self::EaseIn => u * u,
            Self::EaseOut => 1.0 - (1.0 - u) * (1.0 - u),
            Self::EaseInOut => inout(|u| u * u, u),
            Self::Hold => if u >= 1.0 { 1.0 } else { 0.0 },
            Self::InCubic => u * u * u,
            Self::OutCubic => 1.0 - (1.0 - u).powi(3),
            Self::InOutCubic => inout(|u| u * u * u, u),
            Self::InQuint => u.powi(5),
            Self::OutQuint => 1.0 - (1.0 - u).powi(5),
            Self::InOutQuint => inout(|u| u.powi(5), u),
            Self::InExpo => expo_in(u),
            Self::OutExpo => 1.0 - expo_in(1.0 - u),
            Self::InOutExpo => inout(expo_in, u),
            Self::InBack => u * u * ((C1 + 1.0) * u - C1),
            Self::OutBack => {
                let v = 1.0 - u;
                1.0 - v * v * ((C1 + 1.0) * v - C1)
            }
            Self::InOutBack => inout(|u| u * u * ((C1 * 1.525 + 1.0) * u - C1 * 1.525), u),
            Self::OutElastic => {
                if u <= 0.0 || u >= 1.0 {
                    u
                } else {
                    2f64.powf(-10.0 * u) * ((u * 10.0 - 0.75) * (2.0 * std::f64::consts::PI / 3.0)).sin() + 1.0
                }
            }
            Self::OutBounce => bounce_out(u),
            Self::Spring { k, d } => spring(k, d, u),
            Self::Bezier(p) => bezier(p, u),
        }
    }
}

fn expo_in(u: f64) -> f64 {
    if u <= 0.0 { 0.0 } else { 2f64.powf(10.0 * u - 10.0) }
}

fn bounce_out(u: f64) -> f64 {
    const N: f64 = 7.5625;
    const D: f64 = 2.75;
    if u < 1.0 / D {
        N * u * u
    } else if u < 2.0 / D {
        let u = u - 1.5 / D;
        N * u * u + 0.75
    } else if u < 2.5 / D {
        let u = u - 2.25 / D;
        N * u * u + 0.9375
    } else {
        let u = u - 2.625 / D;
        N * u * u + 0.984375
    }
}

/// A unit-mass spring from 0 to 1 over the segment, time scaled so the
/// segment spans the spring's settling (about 8 time constants), then
/// blended onto exactly 1 at the key so the value never jumps.
fn spring(k: f64, d: f64, u: f64) -> f64 {
    let k = if k.is_finite() && k > 0.0 { k } else { 100.0 };
    let d = if d.is_finite() && d >= 0.0 { d } else { 10.0 };
    let w0 = k.sqrt();
    let zeta = d / (2.0 * w0);
    // Settling time scale: underdamped decays at zeta·w0, overdamped at the
    // slow root.
    let rate = if zeta < 1.0 { zeta * w0 } else { w0 * (zeta - (zeta * zeta - 1.0).sqrt()) };
    let span = if rate > 1e-9 { 8.0 / rate } else { 8.0 / w0 };
    let t = u * span;
    let x = if zeta < 1.0 {
        let wd = w0 * (1.0 - zeta * zeta).sqrt();
        1.0 - (-zeta * w0 * t).exp() * ((wd * t).cos() + zeta * w0 / wd * (wd * t).sin())
    } else if (zeta - 1.0).abs() < 1e-9 {
        1.0 - (-w0 * t).exp() * (1.0 + w0 * t)
    } else {
        let s = (zeta * zeta - 1.0).sqrt();
        let r1 = -w0 * (zeta - s);
        let r2 = -w0 * (zeta + s);
        1.0 - (r2 * (r1 * t).exp() - r1 * (r2 * t).exp()) / (r2 - r1)
    };
    // The residue at the key (e^-8 of the start) blended out.
    let smooth = u * u * (3.0 - 2.0 * u);
    x + (1.0 - x) * smooth.powi(8)
}

/// CSS cubic-bezier: solve x(s) = u by Newton with a bisection fallback.
fn bezier(p: [f64; 4], u: f64) -> f64 {
    let (x1, y1, x2, y2) = (p[0].clamp(0.0, 1.0), p[1], p[2].clamp(0.0, 1.0), p[3]);
    let curve = |a: f64, b: f64, s: f64| {
        let v = 1.0 - s;
        3.0 * v * v * s * a + 3.0 * v * s * s * b + s * s * s
    };
    let slope = |a: f64, b: f64, s: f64| {
        let v = 1.0 - s;
        3.0 * v * v * a + 6.0 * v * s * (b - a) + 3.0 * s * s * (1.0 - b)
    };
    let mut s = u;
    for _ in 0..8 {
        let dx = curve(x1, x2, s) - u;
        let m = slope(x1, x2, s);
        if dx.abs() < 1e-9 || m.abs() < 1e-9 {
            break;
        }
        s = (s - dx / m).clamp(0.0, 1.0);
    }
    if (curve(x1, x2, s) - u).abs() > 1e-6 {
        let (mut lo, mut hi) = (0.0, 1.0);
        for _ in 0..60 {
            s = (lo + hi) * 0.5;
            if curve(x1, x2, s) < u { lo = s } else { hi = s }
        }
    }
    curve(y1, y2, s)
}

/// One key: at `t` (seconds, in the host's time base) the value is `v`;
/// `ease` shapes the segment arriving here.
#[derive(Clone, Debug, PartialEq)]
pub struct Key {
    pub t: f64,
    pub v: Value,
    pub ease: Ease,
}

/// A host-evaluated curve: hosts with their own animation system (the
/// Motion document's `tween`, `sequence`, `loop`, …) hand their sampler in
/// instead of converting keys. It must be a pure function of `t`.
#[derive(Clone)]
pub struct Sampler(pub Arc<dyn Fn(f64) -> Value + Send + Sync>);

impl fmt::Debug for Sampler {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Sampler(..)")
    }
}

impl PartialEq for Sampler {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

/// A parameter over time.
#[derive(Clone, Debug, PartialEq)]
pub enum Anim {
    Const(Value),
    /// Times strictly increasing (checked by [`Anim::keys`]).
    Keys(Vec<Key>),
    Sampled(Sampler),
}

impl Anim {
    /// Keys, checked: at least one, times finite and strictly increasing,
    /// all of one value kind.
    pub fn keys(keys: Vec<Key>) -> Result<Self, String> {
        let Some(first) = keys.first() else { return Err("keys(...) needs at least one key, e.g. keys([0.0, 1.0], [1.0, 0.0])".into()) };
        for (i, key) in keys.iter().enumerate() {
            if !key.t.is_finite() {
                return Err(format!("key {i}: its time {} is not a finite number", key.t));
            }
            if key.v.kind_name() != first.v.kind_name() {
                return Err(format!("key {i} is a {} but key 0 is a {}; every key of one parameter has the same kind", key.v.kind_name(), first.v.kind_name()));
            }
            if i > 0 && key.t <= keys[i - 1].t {
                return Err(format!("key {i} at t={} does not come after key {} at t={}; key times must strictly increase", key.t, i - 1, keys[i - 1].t));
            }
        }
        Ok(Anim::Keys(keys))
    }

    /// The value at `t`.
    pub fn at(&self, t: f64) -> Value {
        match self {
            Anim::Const(v) => v.clone(),
            Anim::Sampled(sampler) => (sampler.0)(t),
            Anim::Keys(keys) => {
                let first = &keys[0];
                if !(t > first.t) {
                    return first.v.clone();
                }
                let next = keys.partition_point(|key| key.t <= t);
                if next >= keys.len() {
                    return keys[keys.len() - 1].v.clone();
                }
                let (a, b) = (&keys[next - 1], &keys[next]);
                let u = b.ease.apply((t - a.t) / (b.t - a.t));
                Value::lerp(&a.v, &b.v, u)
            }
        }
    }

    /// True when the value never changes with `t`.
    pub fn is_static(&self) -> bool {
        match self {
            Anim::Const(_) => true,
            Anim::Keys(keys) => keys.windows(2).all(|w| w[0].v == w[1].v),
            Anim::Sampled(_) => false,
        }
    }
}

impl From<Value> for Anim {
    fn from(v: Value) -> Self {
        Anim::Const(v)
    }
}

impl From<f64> for Anim {
    fn from(v: f64) -> Self {
        Anim::Const(Value::Num(v))
    }
}

/// `#rrggbb` / `#rrggbbaa` / `#rgb` to straight RGBA 0..1.
pub fn parse_hex_color(text: &str) -> Option<[f64; 4]> {
    let hex = text.trim().trim_start_matches('#').trim_start_matches('x');
    let digit = |i: usize| u8::from_str_radix(hex.get(i..i + 1)?, 16).ok().map(|v| v as f64 / 15.0);
    let byte = |i: usize| u8::from_str_radix(hex.get(i..i + 2)?, 16).ok().map(|v| v as f64 / 255.0);
    match hex.len() {
        3 => Some([digit(0)?, digit(1)?, digit(2)?, 1.0]),
        4 => Some([digit(0)?, digit(1)?, digit(2)?, digit(3)?]),
        6 => Some([byte(0)?, byte(2)?, byte(4)?, 1.0]),
        8 => Some([byte(0)?, byte(2)?, byte(4)?, byte(6)?]),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_ease_starts_at_zero_and_lands_on_one() {
        let mut eases: Vec<Ease> = Ease::NAMES.iter().map(|n| Ease::from_id(n).unwrap()).collect();
        eases.push(Ease::Spring { k: 170.0, d: 26.0 });
        eases.push(Ease::Spring { k: 100.0, d: 4.0 });
        eases.push(Ease::Spring { k: 100.0, d: 40.0 });
        eases.push(Ease::Bezier([0.25, 0.1, 0.25, 1.0]));
        for ease in eases {
            assert!(ease.apply(0.0).abs() < 1e-9, "{ease:?} at 0: {}", ease.apply(0.0));
            assert!((ease.apply(1.0) - 1.0).abs() < 1e-9, "{ease:?} at 1: {}", ease.apply(1.0));
            assert!(ease.apply(f64::NAN).abs() < 1e-9);
        }
    }

    #[test]
    fn edit_model_eases_match_video_edit() {
        // video_cut::Ease: quadratic in / out / in-out.
        assert_eq!(Ease::EaseIn.apply(0.5), 0.25);
        assert_eq!(Ease::EaseOut.apply(0.5), 0.75);
        assert_eq!(Ease::EaseInOut.apply(0.25), 0.125);
        assert_eq!(Ease::EaseInOut.apply(0.75), 0.875);
        assert_eq!(Ease::Hold.apply(0.99), 0.0);
    }

    #[test]
    fn back_overshoots_and_spring_rings() {
        let peak = (0..=100).map(|i| Ease::OutBack.apply(i as f64 / 100.0)).fold(0.0, f64::max);
        assert!(peak > 1.05 && peak < 1.15, "{peak}");
        let ring = (0..=100).map(|i| Ease::Spring { k: 100.0, d: 4.0 }.apply(i as f64 / 100.0)).fold(0.0, f64::max);
        assert!(ring > 1.2, "an underdamped spring overshoots: {ring}");
        let critical = (0..=100).map(|i| Ease::Spring { k: 100.0, d: 20.0 }.apply(i as f64 / 100.0)).fold(0.0, f64::max);
        assert!(critical <= 1.0 + 1e-9, "a critically damped spring never overshoots: {critical}");
    }

    #[test]
    fn bezier_matches_css_ease() {
        // CSS `ease` at x = 0.5 is about 0.8024.
        let v = Ease::Bezier([0.25, 0.1, 0.25, 1.0]).apply(0.5);
        assert!((v - 0.8024).abs() < 1e-3, "{v}");
        assert!((Ease::Bezier([0.0, 0.0, 1.0, 1.0]).apply(0.3) - 0.3).abs() < 1e-6);
    }

    #[test]
    fn keys_hold_outside_and_ease_between() {
        let anim = Anim::keys(vec![
            Key { t: 1.0, v: Value::Num(0.0), ease: Ease::Linear },
            Key { t: 3.0, v: Value::Num(10.0), ease: Ease::EaseIn },
            Key { t: 4.0, v: Value::Num(20.0), ease: Ease::Hold },
        ])
        .unwrap();
        assert_eq!(anim.at(0.0), Value::Num(0.0));
        assert_eq!(anim.at(f64::NAN), Value::Num(0.0));
        assert_eq!(anim.at(2.0), Value::Num(2.5));
        assert_eq!(anim.at(3.0), Value::Num(10.0));
        assert_eq!(anim.at(3.9), Value::Num(10.0));
        assert_eq!(anim.at(4.0), Value::Num(20.0));
        assert_eq!(anim.at(99.0), Value::Num(20.0));
        assert!(!anim.is_static());
    }

    #[test]
    fn keys_are_checked_with_teaching_errors() {
        let err = Anim::keys(vec![
            Key { t: 1.0, v: Value::Num(0.0), ease: Ease::Linear },
            Key { t: 1.0, v: Value::Num(1.0), ease: Ease::Linear },
        ])
        .unwrap_err();
        assert!(err.contains("strictly increase"), "{err}");
        let err = Anim::keys(vec![
            Key { t: 0.0, v: Value::Num(0.0), ease: Ease::Linear },
            Key { t: 1.0, v: Value::Color([1.0; 4]), ease: Ease::Linear },
        ])
        .unwrap_err();
        assert!(err.contains("same kind"), "{err}");
    }

    #[test]
    fn values_interpolate_by_kind() {
        let a = Value::Stops(vec![(0.0, [0.0; 4]), (1.0, [1.0; 4])]);
        let b = Value::Stops(vec![(0.5, [1.0; 4]), (1.0, [0.0; 4])]);
        assert_eq!(Value::lerp(&a, &b, 0.5), Value::Stops(vec![(0.25, [0.5; 4]), (1.0, [0.5; 4])]));
        let c = Value::Choice("a".into());
        let d = Value::Choice("b".into());
        assert_eq!(Value::lerp(&c, &d, 0.99), c);
        assert_eq!(Value::lerp(&c, &d, 1.0), d);
        assert_eq!(parse_hex_color("#ff8000"), Some([1.0, 128.0 / 255.0, 0.0, 1.0]));
        assert_eq!(parse_hex_color("#f00"), Some([1.0, 0.0, 0.0, 1.0]));
        assert_eq!(parse_hex_color("#zz0000"), None);
    }

    #[test]
    fn sampled_params_are_host_curves() {
        let anim = Anim::Sampled(Sampler(Arc::new(|t| Value::Num(t * 2.0))));
        assert_eq!(anim.at(1.5), Value::Num(3.0));
        assert!(!anim.is_static());
    }
}
