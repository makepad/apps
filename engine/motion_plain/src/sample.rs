use crate::model::*;

/// The value a property function gave.
pub enum FnOut {
    Nums(Vec<f64>),
    Color(Color),
}

/// The full document implements this for property functions stored in its VM.
pub trait PropertyCalls {
    fn call_fn(&self, index: usize, t: f64, i: usize, edit: f64) -> Result<FnOut, String>;
}

pub const NEEDS_VM: &str = "a property function needs the document's VM; this document was read as data";

pub struct Sampler<'a> {
    pub calls: Option<&'a dyn PropertyCalls>,
    pub errors: &'a mut Vec<Diagnostic>,
    pub instance: usize,
    pub edit: f64,
}

impl Sampler<'_> {
    pub fn sample<const N: usize>(&mut self, prop: &Prop<N>, t: f64, i: usize) -> [f64; N] {
        self.sample_kind(prop, t, i, None)
    }

    /// A blend also checks function results: ordinary field shorthand must
    /// not silently truncate a vector control or hide differing value kinds.
    pub fn sample_kind<const N: usize>(&mut self, prop: &Prop<N>, t: f64, i: usize, kind: Option<usize>) -> [f64; N] {
        match prop {
            Prop::Const(value) => *value,
            Prop::Track(track) => sample_track(track, t),
            Prop::Blend(blend) => {
                let x = self.sample_kind(&blend.x, t, i, Some(1))[0];
                let y = blend.y.as_ref().map_or(0.0, |y| self.sample_kind(y, t, i, Some(1))[0]);
                let kind = Some(blend.value_kind.unwrap_or(N));
                blend.sample(x, y, |p| self.sample_kind(p, t, i, kind), |a, b, w| std::array::from_fn(|k| a[k] + (b[k] - a[k]) * w))
            }
            Prop::Fn(f) => match self.calls.ok_or_else(|| NEEDS_VM.to_owned()).and_then(|calls| calls.call_fn(f.index, t, i + self.instance, self.edit)) {
                Ok(FnOut::Nums(values)) => {
                    if kind.is_some_and(|kind| kind != values.len()) {
                        self.fn_error(f, format!("mismatched blend value kinds: returned {} components, expected {}", values.len(), kind.unwrap()));
                        return [0.0; N];
                    }
                    let mut out = [0.0; N];
                    if values.len() == 1 {
                        out = [values[0]; N];
                        if N == 4 {
                            out[3] = 1.0;
                        }
                    } else if values.len() >= N {
                        out.copy_from_slice(&values[..N]);
                    } else if N == 4 && values.len() == 3 {
                        out[..3].copy_from_slice(&values);
                        out[3] = 1.0;
                    } else if N == 4 && values.len() == 2 {
                        // A vec2 uniform (a post pass's `fn(t)`).
                        out[..2].copy_from_slice(&values);
                    } else {
                        self.fn_error(f, format!("returned {} numbers where {N} are needed", values.len()));
                    }
                    out
                }
                Ok(FnOut::Color(color)) => {
                    if N == 4 && kind.is_none_or(|kind| kind == 4) {
                        std::array::from_fn(|k| color[k])
                    } else {
                        self.fn_error(f, "returned a colour where a number is needed".into());
                        [0.0; N]
                    }
                }
                Err(message) => {
                    self.fn_error(f, message);
                    [0.0; N]
                }
            },
        }
    }

    fn fn_error(&mut self, f: &FnProp, message: String) {
        let mut diag = Diagnostic::error(f.loc, "", format!("property function: {message}"));
        if message.contains("cannot index into pod type Vec") {
            diag = diag.hint("vectors have fields: write `p.x` and `p.y` (not `p[0]`), and compute with them directly (`p - size * 0.5`)");
        }
        if !self.errors.contains(&diag) {
            self.errors.push(diag);
        }
    }

    pub fn trim(&mut self, start: &Prop<1>, end: &Prop<1>, t: f64) -> [f64; 2] {
        let a = self.sample(start, t, 0)[0].clamp(0.0, 1.0);
        let b = self.sample(end, t, 0)[0].clamp(0.0, 1.0);
        [a.min(b), a.max(b)]
    }

    pub fn opt4(&mut self, prop: &Option<Prop<4>>, t: f64) -> Option<Color> {
        prop.as_ref().map(|prop| self.sample(prop, t, 0))
    }
}

/// A key track's value at `t` (seconds on its layer's clock).
pub fn sample_track<const N: usize>(track: &Track<N>, t: f64) -> [f64; N] {
    let keys = &track.keys;
    let Some(first) = keys.first() else { return [0.0; N] };
    let mut t = t;
    if let Some(repeat) = &track.repeat {
        let period = repeat.period.value.max(1e-9);
        let start = first.t.value;
        if t > start {
            let elapsed = t - start;
            let cycle = (elapsed / period).floor();
            let within = repeat.count.map_or(true, |count| cycle < count as f64);
            if within {
                let mut u = elapsed - cycle * period;
                if repeat.mode == LoopMode::PingPong && (cycle as i64) % 2 == 1 {
                    u = period - u;
                }
                t = start + u;
            } else {
                // Finished: rest where the last cycle ended.
                let count = repeat.count.unwrap_or(1) as i64;
                t = if repeat.mode == LoopMode::PingPong && count % 2 == 0 { start } else { start + period };
            }
        }
    }
    let last = &keys[keys.len() - 1];
    let (t0, t1) = (first.t.value, last.t.value);
    // Past the keys: the extrapolation.
    if t < t0 || t > t1 {
        let (mode, end) = if t < t0 { (track.before, 0) } else { (track.after, keys.len() - 1) };
        match mode {
            Extrap::Constant => return keys[end].v,
            Extrap::Cycle if t1 > t0 => {
                let span = t1 - t0;
                t = t0 + (t - t0).rem_euclid(span);
            }
            Extrap::Cycle => return keys[end].v,
            Extrap::Linear => {
                let slope = end_slope(keys, end);
                let dt = t - keys[end].t.value;
                return std::array::from_fn(|i| keys[end].v[i] + slope[i] * dt);
            }
        }
    }
    if !(t > first.t.value) {
        return first.v;
    }
    let next = keys.partition_point(|key| key.t.value <= t);
    if next >= keys.len() {
        return keys[keys.len() - 1].v;
    }
    let (a, b) = (&keys[next - 1], &keys[next]);
    let span = b.t.value - a.t.value;
    if b.bez.is_some_and(|h| h.bezier) {
        let (_, out) = key_handles(keys, next - 1);
        let (inn, _) = key_handles(keys, next);
        return std::array::from_fn(|i| bezier_at([a.t.value, a.v[i]], [a.t.value + out.0, a.v[i] + out.1[i]], [b.t.value + inn.0, b.v[i] + inn.1[i]], [b.t.value, b.v[i]], t));
    }
    let u = b.ease.apply((t - a.t.value) / span.max(1e-12), span);
    std::array::from_fn(|i| a.v[i] + (b.v[i] - a.v[i]) * u)
}

/// The slope a track leaves its end key `k` with (per second, each
/// component): its handle's for a bezier end, else its segment's.
fn end_slope<const N: usize>(keys: &[Key<N>], k: usize) -> [f64; N] {
    if keys.len() < 2 {
        return [0.0; N];
    }
    let (a, b) = if k == 0 { (0, 1) } else { (k - 1, k) };
    let bezier = keys[b].bez.is_some_and(|h| h.bezier);
    if bezier {
        let (inn, out) = key_handles(keys, k);
        let h = if k == 0 { out } else { inn };
        if h.0.abs() > 1e-9 {
            return std::array::from_fn(|i| h.1[i] / h.0);
        }
        return [0.0; N];
    }
    let dt = (keys[b].t.value - keys[a].t.value).max(1e-12);
    std::array::from_fn(|i| (keys[b].v[i] - keys[a].v[i]) / dt)
}

/// Key `k`'s handles (in, out) as offsets (seconds, value per component):
/// written ones as written, the rest from the neighbours by its mode. A
/// handle never reaches past the next key in time.
pub fn key_handles<const N: usize>(keys: &[Key<N>], k: usize) -> ((f64, [f64; N]), (f64, [f64; N])) {
    let key = &keys[k];
    let h = key.bez.unwrap_or_else(|| {
        // A key without a record whose next segment is a bezier: smooth.
        KeyHandles::default()
    });
    let dt_prev = if k > 0 { key.t.value - keys[k - 1].t.value } else { 0.0 };
    let dt_next = if k + 1 < keys.len() { keys[k + 1].t.value - key.t.value } else { 0.0 };
    let dv_prev: [f64; N] = std::array::from_fn(|i| if k > 0 { key.v[i] - keys[k - 1].v[i] } else { 0.0 });
    let dv_next: [f64; N] = std::array::from_fn(|i| if k + 1 < keys.len() { keys[k + 1].v[i] - key.v[i] } else { 0.0 });
    let clamp_t = |dt: f64, limit: f64| if limit > 0.0 { dt.clamp(-limit, limit) } else { 0.0 };
    match h.mode {
        HandleMode::Aligned | HandleMode::Free if N == 1 => {
            let inn = h.r#in.unwrap_or([-dt_prev / 3.0, 0.0]);
            let out = h.out.unwrap_or([dt_next / 3.0, 0.0]);
            let it = clamp_t(inn[0].min(0.0), dt_prev);
            let ot = clamp_t(out[0].max(0.0), dt_next);
            // A handle cut short in time keeps its direction.
            let iv = if inn[0].abs() > 1e-12 { inn[1] * it / inn[0] } else { inn[1] };
            let ov = if out[0].abs() > 1e-12 { out[1] * ot / out[0] } else { out[1] };
            ((it, std::array::from_fn(|_| iv)), (ot, std::array::from_fn(|_| ov)))
        }
        HandleMode::Vector => ((-dt_prev / 3.0, std::array::from_fn(|i| -dv_prev[i] / 3.0)), (dt_next / 3.0, std::array::from_fn(|i| dv_next[i] / 3.0))),
        mode => {
            // A slope through the key: the neighbours' (one-sided at the ends).
            let slope: [f64; N] = std::array::from_fn(|i| {
                let s = match (k > 0, k + 1 < keys.len()) {
                    (true, true) => (dv_prev[i] + dv_next[i]) / (dt_prev + dt_next).max(1e-12),
                    (false, true) => dv_next[i] / dt_next.max(1e-12),
                    (true, false) => dv_prev[i] / dt_prev.max(1e-12),
                    (false, false) => 0.0,
                };
                if mode != HandleMode::Clamped {
                    return s;
                }
                // Clamped: flat at a peak or an end, and never past a neighbour.
                let ends = k == 0 || k + 1 == keys.len();
                if ends || dv_prev[i] * dv_next[i] <= 0.0 {
                    return 0.0;
                }
                let lim_prev = 3.0 * dv_prev[i].abs() / dt_prev.max(1e-12);
                let lim_next = 3.0 * dv_next[i].abs() / dt_next.max(1e-12);
                s.signum() * s.abs().min(lim_prev).min(lim_next)
            });
            ((-dt_prev / 3.0, std::array::from_fn(|i| -slope[i] * dt_prev / 3.0)), (dt_next / 3.0, std::array::from_fn(|i| slope[i] * dt_next / 3.0)))
        }
    }
}

/// A cubic bezier through (time, value) points p0..p3, read at time `t`
/// (time runs forward along it).
pub fn bezier_at(p0: [f64; 2], p1: [f64; 2], p2: [f64; 2], p3: [f64; 2], t: f64) -> f64 {
    let x = |u: f64| {
        let m = 1.0 - u;
        m * m * m * p0[0] + 3.0 * m * m * u * p1[0] + 3.0 * m * u * u * p2[0] + u * u * u * p3[0]
    };
    let y = |u: f64| {
        let m = 1.0 - u;
        m * m * m * p0[1] + 3.0 * m * m * u * p1[1] + 3.0 * m * u * u * p2[1] + u * u * u * p3[1]
    };
    // Bisection on x(u) = t (x is monotone: the handles stay within the segment).
    let (mut lo, mut hi) = (0.0f64, 1.0f64);
    for _ in 0..40 {
        let mid = 0.5 * (lo + hi);
        if x(mid) < t {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    y(0.5 * (lo + hi))
}

#[cfg(test)]
mod tests {
    use crate::*;

    #[test]
    fn model_constructors_refuse_what_sampling_assumes() {
        let key = |time, value| Key { t: Time::seconds(time), v: [value], ease: Ease::Linear, bez: None };
        let track = |keys, repeat| Track::new(keys, repeat, Extrap::Constant, Extrap::Constant);
        assert!(track(vec![], None).is_err(), "empty track");
        assert!(track(vec![key(0.0, 1.0), key(0.0, 2.0)], None).is_err(), "equal key times");
        assert!(track(vec![key(0.0, f64::NAN)], None).is_err(), "NaN key value");
        assert!(track(vec![key(f64::INFINITY, 0.0)], None).is_err(), "infinite key time");
        for (incoming, outgoing) in [(Some([f64::NAN, 0.0]), None), (None, Some([0.0, f64::INFINITY]))] {
            let bez = Some(KeyHandles { r#in: incoming, out: outgoing, ..Default::default() });
            assert!(track(vec![Key { bez, ..key(0.0, 0.0) }], None).is_err(), "nonfinite handle {bez:?}");
        }
        for period in [0.0, -1.0, f64::INFINITY, f64::NAN] {
            let repeat = Repeat { period: Dur::seconds(period), mode: LoopMode::Restart, count: None };
            assert!(track(vec![key(0.0, 1.0)], Some(repeat)).is_err(), "invalid repeat period {period}");
        }
        let valid = track(vec![key(0.0, 2.0), key(2.0, 6.0)], None).expect("valid linear track");
        assert_eq!(sample_track(&valid, 0.5), [3.0], "interpolation between keys");

        let grid = |cols, rows, values| BlendForm::new(Prop::scalar(0.0), Some(Prop::scalar(0.0)), cols, rows, values, None, Ease::Linear);
        assert!(grid(vec![], vec![0.0], vec![]).is_err(), "empty column axis");
        assert!(grid(vec![0.0], vec![], vec![]).is_err(), "empty row axis");
        assert!(grid(vec![1.0, 0.0], vec![0.0], vec![1.0, 2.0]).is_err(), "decreasing axis");
        assert!(grid(vec![0.0], vec![f64::NAN], vec![1.0]).is_err(), "NaN stop");
        assert!(grid(vec![0.0, 1.0], vec![0.0, 1.0], vec![0.0, 1.0]).is_err(), "incomplete grid");
        assert!(BlendForm::new(Prop::scalar(1.0), None, vec![0.0], vec![1.0], vec![1.0], None, Ease::Linear).is_err(), "1D row must be at zero");
        let valid = grid(vec![0.0, 1.0], vec![0.0, 2.0], vec![0.0, 4.0, 8.0, 12.0]).expect("valid grid");
        assert_eq!(valid.sample(0.5, 0.5, |v| *v, |a, b, w| a + (b - a) * w), 4.0, "bilinear interpolation");

        assert!(Prop::finite([f64::NAN]).is_err(), "NaN constant");
        assert!(Prop::finite([1.0, f64::NEG_INFINITY]).is_err(), "infinite vector component");
        assert_eq!(Prop::finite([1.0, 2.0]).expect("finite constant").constant(), Some([1.0, 2.0]), "constant components");
    }
}
