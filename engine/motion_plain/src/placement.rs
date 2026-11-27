use crate::{Affine, Local, Prop, RepeatLayout, Span, Transform};
use crate::geometry::{point_along, Polyline};
use crate::sample::Sampler;
use crate::plain::RepeatProps;

pub fn layer_clock(visible: bool, span: Option<&Span>, t: f64, origin: f64) -> Option<(f64, f64)> {
    if !visible {
        return None;
    }
    Some(match span {
        Some(span) => {
            if !(t >= span.start.value && t < span.start.value + span.length.value) {
                return None;
            }
            (t - span.start.value, origin + span.start.value)
        }
        None => (t, origin),
    })
}

pub struct Placement {
    pub world: Affine,
    pub local: Local,
    pub opacity: f64,
    pub blur: f64,
    pub feather: f64,
}

impl Sampler<'_> {
    pub fn place(&mut self, tr: &Transform, opacity: &Prop<1>, blur: &Prop<1>, feather: &Prop<1>,
                 lt: f64, parent: &Affine, per_unit: bool) -> Placement {
        let at = match &tr.along {
            Some(along) => point_along(&along.path, self.sample(&along.progress, lt, 0)[0]),
            None => self.sample(&tr.at, lt, 0),
        };
        let (x, y) = if per_unit { (0.0, 0.0) } else { (self.sample(&tr.x, lt, 0)[0], self.sample(&tr.y, lt, 0)[0]) };
        let world = parent.then(&Affine::translate(at[0] + x, at[1] + y));
        let local = if per_unit {
            Local { anchor: tr.anchor, scale: [1.0, 1.0], rotate: 0.0 }
        } else {
            let scale = self.sample(&tr.scale, lt, 0);
            Local { anchor: tr.anchor, scale: [scale[0], scale[1]], rotate: self.sample(&tr.rotate, lt, 0)[0] }
        };
        let opacity = if per_unit { 1.0 } else { self.sample(opacity, lt, 0)[0].clamp(0.0, 1.0) };
        let blur = if per_unit { 0.0 } else { self.sample(blur, lt, 0)[0].max(0.0) };
        let feather = self.sample(feather, lt, 0)[0].max(0.0);
        Placement { world, local, opacity, blur, feather }
    }
}

impl RepeatProps {
    /// Flatten the guide once for all copies in an along-path layout.
    pub fn guide(&self) -> Option<Polyline> {
        match &self.layout {
            RepeatLayout::Along { path, .. } => Some(Polyline::of(path)),
            _ => None,
        }
    }

    /// Placement, opacity and clock delay of one indexed copy.
    pub fn place_copy(&self, i: usize, placed: &Affine, along: Option<&Polyline>) -> (Affine, f64, f64) {
        let fi = i as f64;
        let rnd = |k: u32| -> f64 {
            let mut h = (self.seed as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ (i as u64).wrapping_mul(0xBF58_476D_1CE4_E5B9) ^ (k as u64).wrapping_mul(0x94D0_49BB_1331_11EB);
            h ^= h >> 31;
            h = h.wrapping_mul(0xD6E8_FEB8_6659_FD93);
            h ^= h >> 32;
            (h as f64 / u64::MAX as f64) * 2.0 - 1.0
        };
        let (mut pos, mut turn) = match &self.layout {
            RepeatLayout::Grid { columns, spacing } => ([(i % columns) as f64 * spacing[0], (i / columns) as f64 * spacing[1]], 0.0),
            RepeatLayout::Radial { radius, start, sweep, face } => {
                let full = (sweep.abs() - 360.0).abs() < 1e-9;
                let a = start + sweep * if full { fi / self.count.max(1) as f64 } else { fi / (self.count.max(2) - 1) as f64 };
                let r = a.to_radians();
                ([radius * r.sin(), -radius * r.cos()], if *face { a } else { 0.0 })
            }
            RepeatLayout::Along { face, .. } => {
                let path = along.expect("an along layout has its polyline");
                let u = if self.count > 1 { fi / (self.count - 1) as f64 } else { 0.0 };
                let p = path.point(u);
                let q = path.point((u + 0.001).min(1.0));
                let p0 = path.point((u - 0.001).max(0.0));
                let angle = (q[1] - p0[1]).atan2(q[0] - p0[0]).to_degrees();
                (p, if *face { angle } else { 0.0 })
            }
            RepeatLayout::Line => ([0.0, 0.0], 0.0),
        };
        pos[0] += self.step.x * fi + self.jitter.x * rnd(1);
        pos[1] += self.step.y * fi + self.jitter.y * rnd(2);
        turn += self.step.rotate * fi + self.jitter.rotate * rnd(3);
        let s = (1.0 + self.step.scale * fi + self.jitter.scale * rnd(4)).max(0.0);
        let o = (1.0 + self.step.opacity * fi + self.jitter.opacity * rnd(5)).clamp(0.0, 1.0);
        let copy = placed.then(&Affine::translate(pos[0], pos[1])).then(&Affine::rotate(turn)).then(&Affine::scale(s, s));
        let d = self.delay * fi;
        (copy, o, d)
    }
}
