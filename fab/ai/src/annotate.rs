//! `fab.annotations`: what the person marked on the 3D view with the marker, for the AI to read.
//!
//! A mark is a stroke drawn on the view: a dot (a tap), an open stroke (what is under the
//! line) or a circle (a stroke whose ends meet: everything visible inside it, not the line). The
//! app sends the stroke with the camera it was drawn with; this module resolves it on the model
//! worker with the rays `fab.pick` uses, so a pixel inside a mark's outline picks into the
//! mark's solid by construction. A mark belongs to what it was drawn on: its points are kept in
//! the model frame of the solid they hit, so it follows the part when the part moves. Points
//! of a stroke that miss the surface (a circle round an arm misses for most of its length)
//! sit on the plane through the mark's own hits that faces the view it was drawn from; the
//! plate is the frame only for a mark that hit nothing.
//!
//! The marks live in the session for as long as the model worker does; they are not part of the
//! project, not an edit, and not saved.

use crate::out::{arr, int, mm3, n, obj, s, S};
use crate::surface::Hit;
use crate::{point_arg, Reply, Session};
use fab_model::params::xf_point;
use makepad_model::json::Value;

type P3 = [f64; 3];

fn dot(a: P3, b: P3) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
fn len(a: P3) -> f64 {
    dot(a, a).sqrt()
}
fn sub(a: P3, b: P3) -> P3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
fn add_scaled(a: P3, b: P3, k: f64) -> P3 {
    [a[0] + b[0] * k, a[1] + b[1] * k, a[2] + b[2] * k]
}
fn unit(a: P3) -> Option<P3> {
    let l = len(a);
    (l > 1e-12).then(|| a.map(|x| x / l))
}
fn number(v: &Value) -> Option<f64> {
    match v {
        Value::F64(x) if x.is_finite() => Some(*x),
        Value::Int(i) => Some(*i as f64),
        _ => None,
    }
}

/// A camera as `fab.render` answers its `cameras` ([`crate::camera::RAYS`]), for a picture of any
/// shape: `px` wide and, when `py` is given, `py` high. A picture that is not square has `right`
/// as long as its aspect (width over height) and `up` unit, `fov` the vertical field and an
/// orthographic `across` the vertical span.
#[derive(Clone, Debug, PartialEq)]
pub struct Cam {
    pub eye: P3,
    pub forward: P3,
    pub right: P3,
    pub up: P3,
    pub fov: Option<f64>,
    pub across: Option<f64>,
    pub w: f64,
    pub h: f64,
}

impl Cam {
    pub fn from_value(camera: &Value) -> Result<Cam, String> {
        const WHAT: &str = "'camera' is one of the cameras a render answers, as it gives it: {view sheet cell px eye forward right up fov}, or `across` for an orthographic one";
        let v3 = |k: &str| -> Result<P3, String> {
            let at = camera.get(k).ok_or_else(|| format!("{WHAT}; it has no '{k}'"))?;
            point_arg::<3>(at).map_err(|e| format!("'camera.{k}': {e}"))
        };
        let (eye, forward, right, up) = (v3("eye")?, v3("forward")?, v3("right")?, v3("up")?);
        let w = camera.get("px").and_then(number).filter(|p| *p >= 1.0).ok_or_else(|| format!("{WHAT}; it has no 'px'"))?;
        let h = match camera.get("py") {
            Some(py) => number(py).filter(|p| *p >= 1.0).ok_or("'camera.py' is the picture's height in pixels")?,
            None => w,
        };
        let (fov, across) = (camera.get("fov").and_then(number), camera.get("across").and_then(number));
        match (fov, across) {
            (Some(f), None) if f > 0.0 && f < 180.0 => {}
            (None, Some(a)) if a > 0.0 => {}
            _ => return Err(format!("{WHAT}; it needs one of 'fov' (degrees, 0..180) or 'across' (mm)")),
        }
        unit(forward).ok_or("'camera.forward' is no direction")?;
        Ok(Cam { eye, forward, right, up, fov, across, w, h })
    }

    /// The ray through pixel (`x`, `y`), (0, 0) the top left corner: from a point, a unit
    /// direction, plate mm.
    pub fn ray(&self, x: f64, y: f64) -> Result<(P3, P3), String> {
        let (u, w) = (2.0 * x / self.w - 1.0, 1.0 - 2.0 * y / self.h);
        let aside: P3 = std::array::from_fn(|k| self.right[k] * u + self.up[k] * w);
        match (self.fov, self.across) {
            (Some(fov), _) => {
                let t = (fov.to_radians() * 0.5).tan();
                Ok((self.eye, unit(add_scaled(self.forward, aside, t)).ok_or("the pixel's ray has no direction")?))
            }
            (_, Some(across)) => Ok((add_scaled(self.eye, aside, across * 0.5), unit(self.forward).ok_or("'camera.forward' is no direction")?)),
            _ => Err("the camera has no lens".into()),
        }
    }

    /// Where plate point `p` shows in the picture, in pixels; none behind a perspective eye.
    pub fn project(&self, p: P3) -> Option<[f64; 2]> {
        let (f, r, u) = (unit(self.forward)?, unit(self.right)?, unit(self.up)?);
        let rel = sub(p, self.eye);
        let (a, b) = match (self.fov, self.across) {
            (Some(fov), _) => {
                let depth = dot(rel, f);
                if depth <= 1e-9 {
                    return None;
                }
                let t = (fov.to_radians() * 0.5).tan() * depth;
                (dot(rel, r) / (t * len(self.right)), dot(rel, u) / (t * len(self.up)))
            }
            (_, Some(across)) => (dot(rel, r) / (across * 0.5 * len(self.right)), dot(rel, u) / (across * 0.5 * len(self.up))),
            _ => return None,
        };
        Some([(a + 1.0) * 0.5 * self.w, (1.0 - b) * 0.5 * self.h])
    }

    /// The camera as a render's answer tells it (view `live`).
    pub fn value(&self) -> S {
        let axis = |v: P3| arr(v.iter().map(|c| S::Num((c * 1e5).round() / 1e5 + 0.0)));
        let mut fields = vec![("view", s("live")), ("px", int(self.w.round() as i64))];
        if (self.w - self.h).abs() > 0.5 {
            fields.push(("py", int(self.h.round() as i64)));
        }
        fields.push(("eye", arr(self.eye.iter().map(|c| n(*c)))));
        fields.push(("forward", axis(self.forward)));
        fields.push(("right", axis(self.right)));
        fields.push(("up", axis(self.up)));
        fields.push(match (self.fov, self.across) {
            (Some(f), _) => ("fov", n(f)),
            (_, a) => ("across", n(a.unwrap_or(0.0))),
        });
        obj(fields)
    }
}

/// What a stroke is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Dot,
    Open,
    Circle,
}

impl Kind {
    pub fn word(self) -> &'static str {
        match self {
            Kind::Dot => "dot",
            Kind::Open => "open stroke",
            Kind::Circle => "circle",
        }
    }
}

type P2 = [f64; 2];

fn bbox(line: &[P2]) -> (P2, P2) {
    let (mut lo, mut hi) = ([f64::INFINITY; 2], [f64::NEG_INFINITY; 2]);
    for p in line {
        for k in 0..2 {
            lo[k] = lo[k].min(p[k]);
            hi[k] = hi[k].max(p[k]);
        }
    }
    (lo, hi)
}

fn dist2(a: P2, b: P2) -> f64 {
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2)).sqrt()
}

/// A stroke's meaning: a dot when it spans under `dot_px` pixels, a circle when its ends meet
/// (closer than a third of its size) round a body, else an open stroke.
pub fn classify(line: &[P2], dot_px: f64) -> Kind {
    if line.len() < 2 {
        return Kind::Dot;
    }
    let (lo, hi) = bbox(line);
    let (w, h) = (hi[0] - lo[0], hi[1] - lo[1]);
    let size = w.max(h);
    if size < dot_px {
        return Kind::Dot;
    }
    if dist2(line[0], line[line.len() - 1]) <= size / 3.0 && w.min(h) >= size * 0.2 {
        Kind::Circle
    } else {
        Kind::Open
    }
}

/// `count` points along the line at equal arc lengths (the closed line back to its start).
fn resample(line: &[P2], closed: bool, count: usize) -> Vec<P2> {
    let mut path = line.to_vec();
    if closed {
        path.push(line[0]);
    }
    let total: f64 = path.windows(2).map(|w| dist2(w[0], w[1])).sum();
    if total < 1e-9 || count < 2 {
        return vec![line[0]];
    }
    let mut out = Vec::with_capacity(count);
    let (mut seg, mut walked) = (0, 0.0);
    for i in 0..count {
        let target = total * i as f64 / (count - 1) as f64;
        while seg + 2 < path.len() && walked + dist2(path[seg], path[seg + 1]) < target {
            walked += dist2(path[seg], path[seg + 1]);
            seg += 1;
        }
        let l = dist2(path[seg], path[seg + 1]).max(1e-12);
        let k = ((target - walked) / l).clamp(0.0, 1.0);
        out.push([path[seg][0] + (path[seg + 1][0] - path[seg][0]) * k, path[seg][1] + (path[seg + 1][1] - path[seg][1]) * k]);
    }
    out
}

/// Whether `p` is inside the closed polygon `poly` (even-odd).
pub fn inside(poly: &[P2], p: P2) -> bool {
    let mut c = false;
    let mut j = poly.len() - 1;
    for i in 0..poly.len() {
        let (a, b) = (poly[i], poly[j]);
        if (a[1] > p[1]) != (b[1] > p[1]) && p[0] < (b[0] - a[0]) * (p[1] - a[1]) / (b[1] - a[1]) + a[0] {
            c = !c;
        }
        j = i;
    }
    c
}

/// Sample points of a circle: a grid of about twenty a side over its box, the ones inside.
fn grid_inside(poly: &[P2]) -> Vec<P2> {
    let (lo, hi) = bbox(poly);
    let size = (hi[0] - lo[0]).max(hi[1] - lo[1]);
    let step = (size / 20.0).max(1.0);
    let (nx, ny) = (((hi[0] - lo[0]) / step).ceil().max(1.0) as usize, ((hi[1] - lo[1]) / step).ceil().max(1.0) as usize);
    let mut out = Vec::new();
    for j in 0..ny {
        for i in 0..nx {
            let p = [lo[0] + (i as f64 + 0.5) * step, lo[1] + (j as f64 + 0.5) * step];
            if inside(poly, p) {
                out.push(p);
            }
        }
    }
    out
}

/// A mark as a render draws it: its outline on the plate (printer mm), closed for a circle, with the
/// number drawn beside it.
#[derive(Clone, Debug, PartialEq)]
pub struct RenderMark {
    pub points: Vec<[f64; 3]>,
    pub closed: bool,
    pub number: u32,
}

/// The layers (1-based, first and last) a z range of the plate (mm) falls in, given each layer's top
/// z and height: the layer whose span holds the point, the nearest when it lies outside all.
pub fn layer_range(layers: &[(f64, f64)], z_lo: f64, z_hi: f64) -> Option<(usize, usize)> {
    let at = |z: f64| -> Option<usize> {
        layers.iter().position(|(top, h)| z <= *top + 1e-6 && z > *top - *h - 1e-6).or_else(|| {
            layers.iter().enumerate().min_by(|a, b| (a.1 .0 - z).abs().total_cmp(&(b.1 .0 - z).abs())).map(|(i, _)| i)
        })
    };
    let (a, b) = (at(z_lo)?, at(z_hi)?);
    Some((a.min(b) + 1, a.max(b) + 1))
}

/// A point of a mark: on a solid (its model frame) or, for a mark that hit nothing, on the
/// plate.
#[derive(Clone, Debug)]
pub struct Anchor {
    pub solid: Option<String>,
    pub p: P3,
    /// On the surface the ray met, not on the plane that stands in for a miss.
    pub surface: bool,
}

/// What a mark covers: a solid, the node of it, and the share of the mark's hits.
#[derive(Clone, Debug, PartialEq)]
pub struct Cover {
    pub solid: String,
    pub node: Option<String>,
    pub hits: usize,
    pub share: f64,
}

#[derive(Clone, Debug)]
pub struct Mark {
    pub uid: u32,
    /// The number drawn beside it (1, 2...) among the marks now.
    pub number: u32,
    pub kind: Kind,
    /// The camera and the line (that camera's pixels) it was drawn with.
    pub cam: Cam,
    pub line: Vec<P2>,
    /// The stroke's outline on the model: the line itself, closed for a circle.
    pub outline: Vec<Anchor>,
    /// A circle's covered surface, one point to a hit inside it.
    pub covered: Vec<Anchor>,
    pub covers: Vec<Cover>,
    /// Rays that found nothing, of those asked.
    pub missed: usize,
    pub asked: usize,
    pub active: bool,
    /// The lines (the person's, by count) this mark went with.
    pub sent: Vec<u32>,
}

/// The session's marks.
#[derive(Default)]
pub struct Marks {
    pub marks: Vec<Mark>,
    next_uid: u32,
}

impl Marks {
    fn active(&self) -> impl Iterator<Item = &Mark> {
        self.marks.iter().filter(|m| m.active)
    }
}

fn plate_center(ev: &fab_model::eval::Evaluated) -> Option<P3> {
    let b = ev.plate.placed_parts().map(|p| p.bounds()).reduce(|a, b| a.union(&b))?;
    Some(std::array::from_fn(|k| (b.min[k] + b.max[k]) * 0.5))
}

impl Session {
    /// The plate-frame matrix of solid `solid`'s model frame, as the rays use it.
    fn frame_of(&self, solid: &str) -> Option<[[f64; 4]; 3]> {
        let ev = self.last.as_ref()?;
        let k = ev.plate.parts.iter().position(|p| p.object == solid)?;
        Some(ev.fields.get(k).and_then(|f| f.as_ref()).map_or(ev.model_to_plate[k], |f| f.to_printer))
    }

    /// An anchor on the plate now (printer mm).
    pub fn anchor_plate(&self, a: &Anchor) -> Option<P3> {
        match &a.solid {
            Some(solid) => Some(xf_point(&self.frame_of(solid)?, a.p)),
            None => Some(a.p),
        }
    }

    /// Resolve a stroke drawn with `cam` into a mark, and keep it: the app's `annotate add`.
    pub fn annotate_add(&mut self, cam: Cam, line: Vec<P2>, dot_px: f64) -> Result<usize, String> {
        if line.is_empty() {
            return Err("the stroke has no points".into());
        }
        let tracer = self.tracer()?;
        let kind = classify(&line, dot_px);
        let outline_px: Vec<P2> = match kind {
            Kind::Dot => {
                let (lo, hi) = bbox(&line);
                vec![[(lo[0] + hi[0]) * 0.5, (lo[1] + hi[1]) * 0.5]]
            }
            Kind::Open => resample(&line, false, 48),
            Kind::Circle => resample(&line, true, 48),
        };
        // the pixels whose rays say what the mark means
        let asked_px: Vec<P2> = if kind == Kind::Circle { grid_inside(&outline_px) } else { outline_px.clone() };
        let ray = |p: P2| -> Result<Option<Hit>, String> {
            let (from, dir) = cam.ray(p[0], p[1])?;
            Ok(tracer.hit(from, dir))
        };
        let mut asked_hits: Vec<(P2, Option<Hit>)> = Vec::with_capacity(asked_px.len());
        for p in &asked_px {
            asked_hits.push((*p, ray(*p)?));
        }
        let outline_hits: Vec<Option<Hit>> = if kind == Kind::Circle { outline_px.iter().map(|p| ray(*p)).collect::<Result<_, _>>()? } else { asked_hits.iter().map(|(_, h)| h.clone()).collect() };
        // what the mark means: a circle's inside; else what is under the line
        let meant: Vec<&Hit> = asked_hits.iter().filter_map(|(_, h)| h.as_ref()).collect();
        let mut covers: Vec<Cover> = Vec::new();
        for h in &meant {
            match covers.iter_mut().find(|c| c.solid == h.object && c.node == h.node) {
                Some(c) => c.hits += 1,
                None => covers.push(Cover { solid: h.object.clone(), node: h.node.clone(), hits: 1, share: 0.0 }),
            }
        }
        for c in &mut covers {
            c.share = c.hits as f64 / meant.len() as f64;
        }
        covers.sort_by(|a, b| b.hits.cmp(&a.hits));
        let dominant = covers.first().map(|c| c.solid.clone());
        // the plane that stands in for a miss: through the hits of the mark, facing the view
        let facing = unit(cam.forward).ok_or("'camera.forward' is no direction")?;
        let plane_through: Option<P3> = {
            let plate_hits: Vec<P3> = if !meant.is_empty() {
                meant.iter().map(|h| xf_point(tracer.to_plate(h.part), h.point)).collect()
            } else {
                outline_hits.iter().flatten().map(|h| xf_point(tracer.to_plate(h.part), h.point)).collect()
            };
            if plate_hits.is_empty() {
                None
            } else {
                let c = plate_hits.iter().fold([0.0; 3], |a, p| add_scaled(a, *p, 1.0 / plate_hits.len() as f64));
                Some(c)
            }
        };
        let plane_at = match plane_through {
            Some(c) => c,
            // nothing hit: the plate's middle, or ahead of the eye
            None => {
                let around = self.last.as_deref().and_then(plate_center);
                let depth = around.map_or(100.0, |c| dot(sub(c, cam.eye), facing)).max(1.0);
                add_scaled(cam.eye, facing, depth)
            }
        };
        let to_model_of_dominant = dominant.as_ref().and_then(|d| self.frame_of(d)).map(|m| fab_model::weave::invert(&m));
        let mut outline = Vec::with_capacity(outline_px.len());
        for (p, hit) in outline_px.iter().zip(&outline_hits) {
            match hit {
                Some(h) => outline.push(Anchor { solid: Some(h.object.clone()), p: h.point, surface: true }),
                None => {
                    let (from, dir) = cam.ray(p[0], p[1])?;
                    let denom = dot(dir, facing);
                    if denom.abs() < 1e-9 {
                        continue;
                    }
                    let t = dot(sub(plane_at, from), facing) / denom;
                    if t <= 0.0 {
                        continue;
                    }
                    let plate = add_scaled(from, dir, t);
                    match (&dominant, &to_model_of_dominant) {
                        (Some(d), Some(inv)) => outline.push(Anchor { solid: Some(d.clone()), p: xf_point(inv, plate), surface: false }),
                        _ => outline.push(Anchor { solid: None, p: plate, surface: false }),
                    }
                }
            }
        }
        let covered: Vec<Anchor> = if kind == Kind::Circle { meant.iter().map(|h| Anchor { solid: Some(h.object.clone()), p: h.point, surface: true }).collect() } else { Vec::new() };
        let missed = asked_hits.iter().filter(|(_, h)| h.is_none()).count();
        let asked = asked_hits.len();
        let number = self.marks.active().map(|m| m.number).max().unwrap_or(0) + 1;
        self.marks.next_uid += 1;
        let uid = self.marks.next_uid;
        self.marks.marks.push(Mark { uid, number, kind, cam, line, outline, covered, covers, missed, asked, active: true, sent: Vec::new() });
        Ok(self.marks.marks.len() - 1)
    }

    /// The person took back the last stroke: the newest mark that is on the view goes.
    pub fn annotate_pop(&mut self) -> Option<u32> {
        let at = self.marks.marks.iter().rposition(|m| m.active)?;
        let m = self.marks.marks.remove(at);
        Some(m.number)
    }

    /// The marks left the view: those never sent are gone, the sent stay readable.
    pub fn annotate_clear(&mut self) {
        self.marks.marks.retain(|m| !m.active || !m.sent.is_empty());
        for m in &mut self.marks.marks {
            m.active = false;
        }
    }

    /// The marks on the view went with the person's line number `line`.
    pub fn annotate_sent(&mut self, line: u32) {
        for m in self.marks.marks.iter_mut().filter(|m| m.active) {
            if !m.sent.contains(&line) {
                m.sent.push(line);
            }
        }
    }

    /// The mark `index`, as the app's drawing wants it: JSON with the kind, what it covers and the
    /// outline on the plate now.
    pub fn annotate_json(&self, index: usize) -> makepad_model::json::Value {
        use makepad_model::json::{obj as jo, s as js, Value as J};
        let m = &self.marks.marks[index];
        let pt = |p: P3| J::Arr(p.iter().map(|c| J::F64(*c)).collect());
        let outline = m.outline.iter().filter_map(|a| Some(jo(vec![("solid", a.solid.clone().map_or(J::Null, js)), ("plate", pt(self.anchor_plate(a)?))]))).collect();
        let covers = m.covers.iter().map(|c| jo(vec![("solid", js(c.solid.clone())), ("node", c.node.clone().map_or(J::Null, js)), ("share", J::F64(c.share))])).collect();
        jo(vec![("uid", J::Int(m.uid as i64)), ("number", J::Int(m.number as i64)), ("kind", js(m.kind.word())), ("outline", J::Arr(outline)), ("covers", J::Arr(covers)), ("nothing", J::Bool(m.covers.is_empty()))])
    }

    /// `annotate {action}`: the app's calls that keep the marks ([`Marks`]). Answers JSON.
    pub(crate) fn annotate_tool(&mut self, args: &Value) -> Reply {
        use makepad_model::json::{obj as jo, Value as J};
        let ok = |v: J| Reply { ok: true, images: Vec::new(), text: v.to_json(), update: None, render: None };
        match args.get("action").and_then(Value::as_str) {
            Some("add") => {
                let Some(camera) = args.get("camera") else { return Reply::err("missing 'camera': the camera the stroke was drawn with") };
                let cam = match Cam::from_value(camera) {
                    Ok(c) => c,
                    Err(e) => return Reply::err(e),
                };
                let Some(points) = args.get("line").and_then(Value::as_arr) else { return Reply::err("missing 'line': the stroke's points [[x, y], ...] in the camera's pixels") };
                let line: Result<Vec<P2>, String> = points.iter().map(|p| point_arg::<2>(p)).collect();
                let line = match line {
                    Ok(l) => l,
                    Err(e) => return Reply::err(format!("'line': {e}")),
                };
                let dot_px = args.get("dot_px").and_then(number).unwrap_or(6.0);
                match self.annotate_add(cam, line, dot_px) {
                    Ok(index) => ok(self.annotate_json(index)),
                    Err(e) => Reply::err(e),
                }
            }
            Some("pop") => ok(jo(vec![("removed", self.annotate_pop().map_or(J::Null, |n| J::Int(n as i64)))])),
            Some("clear") => {
                self.annotate_clear();
                ok(jo(vec![]))
            }
            Some("sent") => {
                let Some(line) = args.get("line").and_then(number) else { return Reply::err("missing 'line': the number of the person's line") };
                self.annotate_sent(line as u32);
                ok(jo(vec![]))
            }
            _ => Reply::err("'action' is add, pop, clear or sent"),
        }
    }

    fn bounds_value(points: &[P3]) -> Option<S> {
        let mut lo = [f64::INFINITY; 3];
        let mut hi = [f64::NEG_INFINITY; 3];
        for p in points {
            for k in 0..3 {
                lo[k] = lo[k].min(p[k]);
                hi[k] = hi[k].max(p[k]);
            }
        }
        if !lo[0].is_finite() {
            return None;
        }
        let centre: P3 = std::array::from_fn(|k| (lo[k] + hi[k]) * 0.5);
        Some(obj(vec![("lo", mm3(lo)), ("hi", mm3(hi)), ("centre", mm3(centre))]))
    }

    /// One mark as `fab.annotations` tells it. `told`: the camera of the picture it is read
    /// against (default: the one it was drawn with).
    fn mark_value(&self, m: &Mark, told: Option<&Cam>, full: bool, compact: bool) -> S {
        let cam = told.unwrap_or(&m.cam);
        let mut fields = vec![("mark", int(m.number as i64)), ("kind", s(m.kind.word()))];
        let covers: Vec<S> = m
            .covers
            .iter()
            .take(if compact { 3 } else { usize::MAX })
            .map(|c| {
                let mut f = vec![("solid", s(c.solid.clone()))];
                if let Some(node) = &c.node {
                    f.push(("node", s(node.clone())));
                }
                f.push(("share", n(c.share)));
                obj(f)
            })
            .collect();
        if m.covers.is_empty() {
            fields.push(("covers", s("nothing: the mark is on the empty bed or sky, no surface is under it")));
        } else {
            fields.push(("covers", arr(covers)));
        }
        // a circle takes in what is round the thing meant: the one with the most of it is the likely
        // meaning, the rest are neighbours (not equals)
        if let (Some(top), true) = (m.covers.first(), m.covers.len() > 1) {
            fields.push(("most_likely", s(top.solid.clone())));
        }
        if m.kind == Kind::Circle {
            fields.push(("empty_share", n(m.missed as f64 / m.asked.max(1) as f64)));
        }
        // plate bounds of the surface it means, and per solid in that solid's model frame
        let surface = |a: &&Anchor| a.surface;
        let meaning: Vec<&Anchor> = if m.kind == Kind::Circle { m.covered.iter().chain(m.outline.iter().filter(|a| a.surface)).collect() } else { m.outline.iter().filter(surface).collect() };
        let pool: Vec<&Anchor> = if meaning.is_empty() { m.outline.iter().collect() } else { meaning };
        let plate_pts: Vec<P3> = pool.iter().filter_map(|a| self.anchor_plate(a)).collect();
        if let Some(b) = Self::bounds_value(&plate_pts) {
            fields.push(("plate", b));
        }
        // under the Sliced view: the layers the mark's surface runs through, from the live slice
        // when it is made (what the view shows there is the print, not the model)
        if let (Some(top), Some(ev), Some(src)) = (m.covers.first(), self.last.as_ref(), self.slices.as_ref()) {
            let z_lo = plate_pts.iter().map(|p| p[2]).fold(f64::INFINITY, f64::min);
            let z_hi = plate_pts.iter().map(|p| p[2]).fold(f64::NEG_INFINITY, f64::max);
            let found = src.made(ev).zip(ev.plate.parts.iter().position(|p| p.object == top.solid)).and_then(|(paths, k)| {
                let plan = &paths.plans().get(k)?.0;
                let layers: Vec<(f64, f64)> = plan.layers.iter().filter(|(_, _, l)| !l.support_only).map(|(z, h, _)| (*z, *h)).collect();
                layer_range(&layers, z_lo, z_hi).map(|r| (r, layers.len()))
            });
            if let Some(((first, last), total)) = found {
                fields.push(("sliced", obj(vec![("part", s(top.solid.clone())), ("layers", arr([int(first as i64), int(last as i64)])), ("of", int(total as i64)), ("note", s("layers of the part's own slice under the mark; supports and brim are not told apart here"))])));
            }
        }
        let mut solids: Vec<&str> = Vec::new();
        for a in &pool {
            if let Some(sol) = a.solid.as_deref() {
                if !solids.contains(&sol) {
                    solids.push(sol);
                }
            }
        }
        let models: Vec<S> = solids
            .iter()
            .take(if compact { 1 } else { usize::MAX })
            .filter_map(|sol| {
                let pts: Vec<P3> = pool.iter().filter(|a| a.solid.as_deref() == Some(*sol)).map(|a| a.p).collect();
                let Some(S::Object(_, mut f)) = Self::bounds_value(&pts) else { return None };
                f.insert(0, ("solid".into(), s(*sol)));
                Some(S::Object(None, f))
            })
            .collect();
        if !models.is_empty() {
            fields.push(("model", arr(models)));
        }
        let mut shown: Vec<[f64; 2]> = m.outline.iter().filter_map(|a| cam.project(self.anchor_plate(a)?)).collect();
        if compact && shown.len() > 8 {
            shown = (0..8).map(|k| shown[k * (shown.len() - 1) / 7]).collect();
        }
        fields.push(("outline_px", arr(shown.iter().map(|p| arr([int(p[0].round() as i64), int(p[1].round() as i64)])))));
        if !compact && (full || told.is_some()) {
            fields.push(("camera", cam.value()));
        }
        if !m.sent.is_empty() {
            fields.push(("sent_with_lines", arr(m.sent.iter().map(|l| int(*l as i64)))));
        }
        obj(fields)
    }

    /// The box (plate mm) round mark `number`'s meaning, with room round it: what `fab.render`
    /// frames for `focus: {mark: N}`.
    pub fn mark_box(&self, number: u32) -> Result<(P3, P3), String> {
        let m = self.marks.active().find(|m| m.number == number).ok_or_else(|| format!("no mark {number} is on the view now (fab.annotations lists them)"))?;
        let on = |surface_only: bool| -> Vec<P3> { m.outline.iter().chain(m.covered.iter()).filter(|a| a.surface || !surface_only).filter_map(|a| self.anchor_plate(a)).collect() };
        let mut pts = on(true);
        if pts.is_empty() {
            pts = on(false);
        }
        if pts.is_empty() {
            return Err(format!("mark {number} is on nothing in the plate now"));
        }
        let (mut lo, mut hi) = ([f64::INFINITY; 3], [f64::NEG_INFINITY; 3]);
        for p in &pts {
            for k in 0..3 {
                lo[k] = lo[k].min(p[k]);
                hi[k] = hi[k].max(p[k]);
            }
        }
        // room round it: a third of its largest side, at least 8 mm
        let room = ((0..3).map(|k| hi[k] - lo[k]).fold(0.0, f64::max) / 3.0).max(8.0);
        Ok((lo.map(|c| c - room), hi.map(|c| c + room)))
    }

    /// The marks on the view as a render draws them.
    pub fn render_marks(&self) -> Vec<RenderMark> {
        self.marks.active().map(|m| RenderMark { points: m.outline.iter().filter_map(|a| self.anchor_plate(a)).collect(), closed: m.kind == Kind::Circle, number: m.number }).filter(|m| !m.points.is_empty()).collect()
    }

    /// `fab.annotations {camera, mark, line}`.
    pub(crate) fn annotations_tool(&mut self, args: &Value) -> Reply {
        let told = match args.get("camera") {
            Some(c) => match Cam::from_value(c) {
                Ok(c) => Some(c),
                Err(e) => return Reply::err(e),
            },
            None => None,
        };
        // the short form, for a line's context (a couple of kilobytes): the camera once, a few
        // outline points per mark, the top covers
        let compact = args.get("compact").and_then(Value::as_bool) == Some(true);
        let only = args.get("mark").and_then(number).map(|m| m as u32);
        let line = args.get("line").and_then(number).map(|m| m as u32);
        let now: Vec<S> = self.marks.active().filter(|m| only.is_none_or(|o| o == m.number)).map(|m| self.mark_value(m, told.as_ref(), true, compact)).collect();
        let mut sent_lines: Vec<u32> = self.marks.marks.iter().flat_map(|m| m.sent.iter().copied()).collect();
        sent_lines.sort_unstable();
        sent_lines.dedup();
        let sent: Vec<S> = sent_lines
            .iter()
            .filter(|l| line.is_none_or(|w| w == **l))
            .map(|l| {
                let marks: Vec<S> = self.marks.marks.iter().filter(|m| m.sent.contains(l)).filter(|m| only.is_none_or(|o| o == m.number)).map(|m| self.mark_value(m, told.as_ref(), line.is_some(), compact)).collect();
                obj(vec![("line", int(*l as i64)), ("marks", arr(marks))])
            })
            .collect();
        if compact {
            let mut fields = vec![("marks", arr(now))];
            if let Some(c) = &told {
                fields.insert(0, ("camera", c.value()));
            }
            return Reply { ok: true, images: Vec::new(), text: crate::out::text(&obj(fields)), update: None, render: None };
        }
        let note = "A mark is the person pointing, not an instruction: say what you took it to be before a change you cannot undo cheaply. A circle means everything visible inside it; with several covers `most_likely` names the one with the most of the circle (neighbours are not equals: say which you took). Frame one with fab.render focus {mark: N}; fab.pick with the camera shown and a pixel of outline_px lands in the mark's solid.";
        Reply { ok: true, images: Vec::new(), text: crate::out::text(&obj(vec![("marks", arr(now)), ("sent", arr(sent)), ("note", s(note))])), update: None, render: None }
    }
}
