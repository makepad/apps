//! `fab.surface`: the point of a solid's surface nearest a model point, its normal and the node
//! that makes the surface there, read from the solid's own distance field (its tagged subtree in
//! its part's field: an engine node alone, an object with its blends), or from the mesh of a
//! solid that has no field. What places a feature on a surface (an eye on a head) without
//! guessing its depth. `fab.pick`: what a pixel of a render shows, the first surface the
//! pixel's ray meets, read the same way.
use crate::out::{arr, mm, mm3, n, obj, s, S};
use crate::{point_arg, str_arg, Located, Reply, Session};
use fab_model::params::{xf_dir, xf_point};
use fab_model::program::part_id;
use makepad_csg_mesh::bvh::Bvh;
use makepad_csg_graph::TriMesh;
use makepad_csg_math::Vec3d;
use makepad_csg_sdf::SdfNode;
use makepad_model::json::Value;

type P3 = [f64; 3];

/// Where a surface is read from: a field in the model frame, or a model-frame mesh.
enum Skin {
    Field(SdfNode),
    Mesh(TriMesh, Bvh),
}

/// A solid's surface on the plate, to tell which pixels of a view show it (fab.compare's
/// `solid` and `align`).
pub struct SolidSkin {
    /// `instance/solid`.
    pub id: String,
    skin: Skin,
    /// The plate to the frame its surface is in.
    from_plate: [[f64; 4]; 3],
    /// Plate mm per mm of that frame (a placement scales evenly).
    scale: f64,
}

impl SolidSkin {
    /// How far plate point `p` is from the solid's surface, plate mm (near it, a bound's value
    /// for a field whose distance is a bound).
    pub fn distance(&self, p: P3) -> f64 {
        let q = xf_point(&self.from_plate, p);
        let d = match &self.skin {
            Skin::Field(f) => makepad_csg_sdf::distance(f, v(q)).abs(),
            Skin::Mesh(_, bvh) => bvh.closest(v(q)).map_or(f64::INFINITY, |(d, _, _)| d),
        };
        d * self.scale
    }
}

/// One point found on a surface (model mm).
pub struct Found {
    pub point: P3,
    pub normal: P3,
    /// From the asked point to the surface: negative inside.
    pub distance: f64,
    /// The node whose surface it is (`instance/solid`), when the field names one.
    pub node: Option<String>,
}

/// A prepared surface query, shared by inspection and placement tools.
pub(crate) struct SurfaceQuery {
    id: String,
    skin: Skin,
    to_plate: [[f64; 4]; 3],
    names: Vec<(u32, String)>,
}

impl SurfaceQuery {
    fn nearest(&self, near: P3) -> Found {
        self.skin.nearest(near, &|tag| self.names.iter().find(|(t, _)| *t == tag).map(|(_, n)| n.clone()))
    }

    /// The surface point nearest model point `p` and its outward normal, in the model frame;
    /// none where the solid has no surface.
    pub(crate) fn nearest_model(&self, p: P3) -> Option<(P3, P3)> {
        let f = self.nearest(p);
        f.distance.is_finite().then_some((f.point, f.normal))
    }

    /// The nearest point and outward normal in plate coordinates, without text rounding.
    pub(crate) fn nearest_plate(&self, near: P3) -> Result<Found, String> {
        let from_plate = fab_model::weave::invert(&self.to_plate);
        let f = self.nearest(xf_point(&from_plate, near));
        if !f.distance.is_finite() {
            return Err(format!("'{}' has no surface near this point; pick a face with fab.surface or fab.pick", self.id));
        }
        let point = xf_point(&self.to_plate, f.point);
        let normal = unit(xf_dir(&self.to_plate, f.normal)).ok_or("the surface has no outward normal at this point; pick a point inside a face")?;
        let off = sub(near, point);
        Ok(Found { point, normal, distance: dot(off, off).sqrt().copysign(f.distance), node: f.node })
    }
}

pub(crate) fn v(p: P3) -> Vec3d {
    Vec3d::new(p[0], p[1], p[2])
}
pub(crate) fn dot(a: P3, b: P3) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
fn sub(a: P3, b: P3) -> P3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
pub(crate) fn add_scaled(a: P3, b: P3, k: f64) -> P3 {
    [a[0] + b[0] * k, a[1] + b[1] * k, a[2] + b[2] * k]
}
pub(crate) fn unit(a: P3) -> Option<P3> {
    let l = dot(a, a).sqrt();
    (l > 1e-12).then(|| a.map(|x| x / l))
}

/// A field's gradient at `p` by central differences (mm per mm).
pub(crate) fn gradient(f: &SdfNode, p: P3) -> P3 {
    const H: f64 = 1e-4;
    std::array::from_fn(|k| {
        let (mut a, mut b) = (p, p);
        a[k] += H;
        b[k] -= H;
        (makepad_csg_sdf::distance(f, v(a)) - makepad_csg_sdf::distance(f, v(b))) / (2.0 * H)
    })
}

/// A number of a tool's arguments.
pub(crate) fn number(v: &Value) -> Option<f64> {
    match v {
        Value::F64(x) if x.is_finite() => Some(*x),
        Value::Int(i) => Some(*i as f64),
        _ => None,
    }
}

/// The ray through pixel (`x`, `y`) of a render's view, its camera as the render's answer tells
/// it ([`crate::camera::RAYS`]): from a point, a unit direction, plate mm.
fn told_ray(camera: &Value, x: f64, y: f64) -> Result<(P3, P3), String> {
    let cam = crate::annotate::Cam::from_value(camera)?;
    if !(0.0..=cam.w).contains(&x) || !(0.0..=cam.h).contains(&y) {
        return Err(if cam.w == cam.h { format!("pixel ({x}, {y}) is outside the view, {} pixels square", cam.w) } else { format!("pixel ({x}, {y}) is outside the view, {} by {} pixels", cam.w, cam.h) });
    }
    cam.ray(x, y)
}

/// Where the ray `o + t d` (t >= 0) is inside the box `lo..hi`: its entry and exit t.
pub(crate) fn slab(o: P3, d: P3, lo: P3, hi: P3) -> Option<(f64, f64)> {
    let (mut t0, mut t1) = (0.0f64, f64::INFINITY);
    for k in 0..3 {
        if d[k].abs() < 1e-15 {
            if o[k] < lo[k] || o[k] > hi[k] {
                return None;
            }
            continue;
        }
        let (a, b) = ((lo[k] - o[k]) / d[k], (hi[k] - o[k]) / d[k]);
        t0 = t0.max(a.min(b));
        t1 = t1.min(a.max(b));
    }
    (t0 <= t1).then_some((t0, t1))
}

/// How close to its surface a traced ray stops (mm).
const HIT: f64 = 1e-4;

/// Where the unit ray `o + t d` first reaches field `f`'s surface inside the field's box `lo..hi`:
/// sphere tracing, each step what the field says is clear (its value never overstates the way to
/// the surface), then onto the surface along the ray.
pub(crate) fn trace_field(f: &SdfNode, o: P3, d: P3, lo: P3, hi: P3) -> Option<f64> {
    let (mut t, end) = slab(o, d, lo, hi)?;
    while t <= end {
        let dist = makepad_csg_sdf::distance(f, v(add_scaled(o, d, t)));
        if dist < HIT {
            // a step back out and Newton's along the ray: the crossing itself
            for _ in 0..8 {
                let at = add_scaled(o, d, t);
                let (value, slope) = (makepad_csg_sdf::distance(f, v(at)), dot(gradient(f, at), d));
                if value.abs() < ON || slope.abs() < 1e-9 {
                    break;
                }
                t = (t - value / slope).max(0.0);
            }
            return Some(t);
        }
        t += dist.max(HIT);
    }
    None
}

enum Traced {
    Field { root: SdfNode, min: P3, max: P3 },
    Mesh(TriMesh, Bvh),
}

struct PartTrace {
    object: String,
    to_plate: [[f64; 4]; 3],
    to_model: [[f64; 4]; 3],
    /// The tags of the part's solids, to name the node a field hit lands on.
    names: Vec<(u32, String)>,
    what: Traced,
}

/// The plate's surfaces, ready to take many rays ([`Session::tracer`]).
pub(crate) struct Tracer {
    parts: Vec<PartTrace>,
}

impl Tracer {
    /// The first surface the plate-frame ray `from + t dir` meets.
    pub(crate) fn hit(&self, from: P3, dir: P3) -> Option<Hit> {
        let mut best: Option<Hit> = None;
        for (k, part) in self.parts.iter().enumerate() {
            let (o, d) = (xf_point(&part.to_model, from), xf_dir(&part.to_model, dir));
            let scale = dot(d, d).sqrt();
            let d = d.map(|x| x / scale);
            let found = match &part.what {
                Traced::Field { root, min, max } => trace_field(root, o, d, *min, *max).map(|t| {
                    let point = add_scaled(o, d, t);
                    let normal = unit(gradient(root, point)).unwrap_or([0.0, 1.0, 0.0]);
                    let tag = makepad_csg_sdf::distance_id(root, v(point)).1;
                    (t, point, normal, tag)
                }),
                Traced::Mesh(m, bvh) => bvh.ray(v(o), v(d), f64::INFINITY, None).map(|(t, tri)| {
                    let n = m.triangle_normal(tri as usize);
                    (t, add_scaled(o, d, t), [n.x, n.y, n.z], None)
                }),
            };
            let Some((t, point, normal, tag)) = found else { continue };
            if best.as_ref().is_some_and(|b| b.depth <= t / scale) {
                continue;
            }
            let node = tag.and_then(|tag| part.names.iter().find(|(id, _)| *id == tag).map(|(_, n)| n.clone()));
            best = Some(Hit { part: k, object: part.object.clone(), node, point, normal, depth: t / scale });
        }
        best
    }

    /// Part `k`'s model frame to the plate.
    pub(crate) fn to_plate(&self, k: usize) -> &[[f64; 4]; 3] {
        &self.parts[k].to_plate
    }

    /// These surfaces to trace, each with its name and the map from its frame onto the plate.
    pub(crate) fn of(parts: Vec<(String, [[f64; 4]; 3], Surface)>) -> Tracer {
        let parts = parts.into_iter().map(|(object, to_plate, of)| {
            let what = match of {
                Surface::Field(f) => Traced::Field { root: f.root, min: f.min, max: f.max },
                Surface::Mesh(m) => {
                    let bvh = Bvh::build(&m);
                    Traced::Mesh(m, bvh)
                }
            };
            PartTrace { object, to_model: fab_model::weave::invert(&to_plate), to_plate, names: Vec::new(), what }
        });
        Tracer { parts: parts.collect() }
    }

    /// Part `k`'s surface normal `n` (its frame's, as a hit gives it) on the plate, unit.
    pub(crate) fn plate_normal(&self, k: usize, n: P3) -> Option<P3> {
        let m = &self.parts[k].to_model;
        unit(std::array::from_fn(|c| (0..3).map(|r| m[r][c] * n[r]).sum()))
    }

    /// The surface point nearest plate point `p`: its part, the point and its unit normal on the
    /// plate, and how far it is (plate mm).
    pub(crate) fn nearest(&self, p: P3) -> Option<(usize, P3, P3, f64)> {
        let mut best: Option<(usize, P3, P3, f64)> = None;
        for (k, part) in self.parts.iter().enumerate() {
            let q = xf_point(&part.to_model, p);
            let (point, normal) = match &part.what {
                Traced::Field { root, .. } => nearest_on_field(root, q),
                Traced::Mesh(m, bvh) => {
                    let Some((_, at, tri)) = bvh.closest(v(q)) else { continue };
                    let n = m.triangle_normal(tri as usize);
                    ([at.x, at.y, at.z], [n.x, n.y, n.z])
                }
            };
            let (Some(normal), point) = (self.plate_normal(k, normal), xf_point(&part.to_plate, point)) else { continue };
            let off = sub(p, point);
            let d = dot(off, off).sqrt();
            if best.is_none_or(|b| d < b.3) {
                best = Some((k, point, normal, d));
            }
        }
        best
    }
}

/// A surface for a [`Tracer`] of its own: a field in its frame, or a mesh.
pub(crate) enum Surface {
    Field(makepad_csg_sdf::SdfShape),
    Mesh(TriMesh),
}

/// What a ray meets first on the plate.
#[derive(Clone)]
pub(crate) struct Hit {
    /// The plate part, its object (`instance/solid`).
    pub part: usize,
    pub object: String,
    /// The node whose surface it is, when its part's field names one.
    pub node: Option<String>,
    /// In the part's model frame.
    pub point: P3,
    pub normal: P3,
    /// Along the ray, plate mm.
    pub depth: f64,
}

/// How close to the surface a point found counts as on it (mm).
const ON: f64 = 1e-6;
/// The most steps a search takes: Newton's converges in a few, a slide along a curved surface in
/// tens.
const STEPS: usize = 100;

/// `p` moved onto the field's surface along its gradient (Newton's steps: one for an exact
/// distance, a few for a bound like an ellipsoid's), while each step brings it closer.
pub(crate) fn onto(f: &SdfNode, mut p: P3) -> P3 {
    let mut d = makepad_csg_sdf::distance(f, v(p));
    for _ in 0..STEPS {
        let g = gradient(f, p);
        let gg = dot(g, g);
        if d.abs() <= ON || gg < 1e-18 {
            break;
        }
        let next = add_scaled(p, g, -d / gg);
        let e = makepad_csg_sdf::distance(f, v(next));
        if e.abs() >= d.abs() {
            break;
        }
        (p, d) = (next, e);
    }
    p
}

/// The surface point of field `f` nearest `near` and its normal: onto the surface, then slid
/// along it (halving a slide that overshoots) until the way to `near` is along the normal (a
/// bound's gradient, an ellipsoid's, need not point at the nearest point).
fn nearest_on_field(f: &SdfNode, near: P3) -> (P3, P3) {
    let mut q = onto(f, near);
    let normal_at = |q: P3| unit(gradient(f, q)).unwrap_or([0.0, 1.0, 0.0]);
    for _ in 0..STEPS {
        let normal = normal_at(q);
        let off = sub(near, q);
        let along = add_scaled(off, normal, -dot(off, normal));
        if dot(along, along).sqrt() < ON {
            break;
        }
        let was = dot(off, off);
        let mut k = 1.0;
        let next = loop {
            let next = onto(f, add_scaled(q, along, k));
            let now = sub(near, next);
            if dot(now, now) < was {
                break Some(next);
            }
            k *= 0.5;
            if k < 1e-6 {
                break None;
            }
        };
        match next {
            Some(next) => q = next,
            None => break,
        }
    }
    (q, normal_at(q))
}

impl Skin {
    fn nearest(&self, near: P3, names: &dyn Fn(u32) -> Option<String>) -> Found {
        match self {
            Skin::Field(f) => {
                let (point, normal) = nearest_on_field(f, near);
                // a bound's value is no distance: the way to the point found is
                let way = sub(near, point);
                let distance = dot(way, way).sqrt().copysign(makepad_csg_sdf::distance(f, v(near)));
                let node = makepad_csg_sdf::distance_id(f, v(point)).1.and_then(names);
                Found { point, normal, distance, node }
            }
            Skin::Mesh(m, bvh) => {
                let Some((d, q, tri)) = bvh.closest(v(near)) else { return Found { point: near, normal: [0.0, 1.0, 0.0], distance: f64::INFINITY, node: None } };
                let (point, nm) = ([q.x, q.y, q.z], m.triangle_normal(tri as usize));
                let normal = [nm.x, nm.y, nm.z];
                // inside when the way out runs against the face's normal
                let sign = if dot(sub(near, point), normal) < 0.0 { -1.0 } else { 1.0 };
                Found { point, normal, distance: sign * d, node: None }
            }
        }
    }
}

impl Session {
    /// Solid `name`'s surface to read: its own field (`instance/solid`), the frame of the printed
    /// part it is in, and the names of the nodes its field tags.
    fn skin(&mut self, name: &str) -> Result<(String, Skin, [[f64; 4]; 3], Vec<(u32, String)>), String> {
        let Located { name: id, part, to_plate, .. } = self.locate(name)?;
        let (ev, c) = (self.last.clone().unwrap(), self.compiled.clone().unwrap());
        let (inst, i) = c.part(&id).unwrap();
        let p = &inst.script.program;
        let names: Vec<(u32, String)> = p.solids.iter().map(|s| (makepad_csg_sdf::part_id(&s.id), part_id(&inst.name, &s.id))).collect();
        let skin = match ev.fields.get(part).and_then(|f| f.as_ref()) {
            Some(f) if ev.plate.parts[part].object == id => Skin::Field(f.shape.root.clone()),
            Some(f) => Skin::Field(fab_model::eval::tagged_field(&f.shape.root, makepad_csg_sdf::part_id(&p.solids[i].id)).ok_or_else(|| format!("'{id}' has no surface of its own in its part's field"))?),
            None => {
                let m = self.eval.object_mesh(&id)?;
                let bvh = Bvh::build(&m);
                Skin::Mesh(m, bvh)
            }
        };
        Ok((id, skin, to_plate, names))
    }

    pub(crate) fn surface_query(&mut self, name: &str) -> Result<SurfaceQuery, String> {
        let (id, skin, to_plate, names) = self.skin(name)?;
        Ok(SurfaceQuery { id, skin, to_plate, names })
    }

    /// Solid `name`'s surface on the plate ([`SolidSkin`]).
    pub(crate) fn solid_skin(&mut self, name: &str) -> Result<SolidSkin, String> {
        let (id, skin, to_plate, _) = self.skin(name)?;
        Ok(SolidSkin { id, skin, from_plate: fab_model::weave::invert(&to_plate), scale: crate::frame_scale(&to_plate) })
    }

    /// `fab.surface {solid, near}`: per point asked (model mm), the nearest point of the solid's
    /// surface, its outward normal, how far the asked point is from it (negative inside), the
    /// node that makes the surface there, and the point on the plate.
    pub(crate) fn surface_tool(&mut self, args: &Value) -> Reply {
        let Some(name) = str_arg(args, "solid") else { return Reply::err("missing 'solid': a solid's name from fab.tree (head, or doll/head)") };
        let Some(near) = args.get("near") else { return Reply::err("missing 'near': a model point [x y z] in mm, or a list of them") };
        let points: Vec<P3> = match near.as_arr() {
            Some(list) if list.first().is_some_and(|p| p.as_arr().is_some()) => match list.iter().map(point_arg::<3>).collect() {
                Ok(p) => p,
                Err(e) => return Reply::err(format!("'near': {e}")),
            },
            _ => match point_arg::<3>(near) {
                Ok(p) => vec![p],
                Err(e) => return Reply::err(format!("'near': {e}")),
            },
        };
        let query = match self.surface_query(name) {
            Ok(x) => x,
            Err(e) => return Reply::err(e),
        };
        let rows: Vec<S> = points
            .iter()
            .map(|near| {
                let f = query.nearest(*near);
                let plate_normal = unit(xf_dir(&query.to_plate, f.normal)).unwrap_or(f.normal);
                obj(vec![
                    ("near", mm3(*near)),
                    ("point", mm3(f.point)),
                    ("normal", arr(f.normal.iter().map(|x| n(*x)))),
                    ("distance", mm(f.distance)),
                    ("node", f.node.map_or(S::Nil, s)),
                    ("plate", mm3(xf_point(&query.to_plate, f.point))),
                    ("plate_normal", arr(plate_normal.iter().map(|x| n(*x)))),
                ])
            })
            .collect();
        let engine = match query.skin {
            Skin::Field(_) => "field",
            Skin::Mesh(..) => "mesh",
        };
        Reply::ok(obj(vec![("solid", s(query.id)), ("frame", s("model mm, Y up")), ("surface", s(engine)), ("points", arr(rows))]))
    }

    /// Every plate part's surface to trace rays against: a field part's field, any other's mesh
    /// with its BVH, built once for as many rays as are asked.
    pub(crate) fn tracer(&mut self) -> Result<Tracer, String> {
        let (ev, c) = (self.shown_last().ok_or("nothing is built yet")?, self.shown_compiled().ok_or("nothing is built yet")?);
        let mut parts = Vec::new();
        for (k, part) in ev.plate.parts.iter().enumerate() {
            let field = ev.fields.get(k).and_then(|f| f.as_ref());
            let to_plate = field.map_or(ev.model_to_plate[k], |f| f.to_printer);
            let names: Vec<(u32, String)> = c.part(&part.object).map(|(inst, _)| inst.script.program.solids.iter().map(|s| (makepad_csg_sdf::part_id(&s.id), part_id(&inst.name, &s.id))).collect()).unwrap_or_default();
            let what = match field {
                Some(f) => Traced::Field { root: f.shape.root.clone(), min: f.shape.min, max: f.shape.max },
                None => {
                    let m = self.eval.object_mesh(&part.object).map_err(|e| format!("'{}': {e}", part.object))?;
                    let bvh = Bvh::build(&m);
                    Traced::Mesh(m, bvh)
                }
            };
            parts.push(PartTrace { object: part.object.clone(), to_plate, to_model: fab_model::weave::invert(&to_plate), names, what });
        }
        Ok(Tracer { parts })
    }

    /// The first surface the plate-frame ray `from + t dir` meets, over every plate part: a field
    /// part traced in its field, any other on its mesh.
    fn first_hit(&mut self, from: P3, dir: P3) -> Result<Option<Hit>, String> {
        Ok(self.tracer()?.hit(from, dir))
    }

    /// `fab.pick {camera, x, y}`: what pixel (x, y) of a render's view shows, its camera as the
    /// render's answer gives it: the part and node there, the point and normal in the part's
    /// model frame and on the plate, and how far along the pixel's ray it is.
    pub(crate) fn pick_tool(&mut self, args: &Value) -> Reply {
        let Some(camera) = args.get("camera") else { return Reply::err("missing 'camera': one of the cameras a render answers (fab.render's `cameras`), as it gives it") };
        let (Some(x), Some(y)) = (args.get("x").and_then(number), args.get("y").and_then(number)) else { return Reply::err("missing 'x' or 'y': the pixel in the view's picture, from its top left corner") };
        let (from, dir) = match told_ray(camera, x, y) {
            Ok(r) => r,
            Err(e) => return Reply::err(e),
        };
        let ray = obj(vec![("from", mm3(from)), ("dir", arr(dir.iter().map(|x| n(*x))))]);
        let hit = match self.first_hit(from, dir) {
            Ok(Some(hit)) => hit,
            Ok(None) => return Reply::ok(obj(vec![("hit", S::Nil), ("note", s("the pixel's ray meets nothing on the plate")), ("ray", ray)])),
            Err(e) => return Reply::err(e),
        };
        let to_plate = self.shown_last().as_ref().map(|ev| ev.fields.get(hit.part).and_then(|f| f.as_ref()).map_or(ev.model_to_plate[hit.part], |f| f.to_printer)).unwrap();
        let plate_normal = unit(xf_dir(&to_plate, hit.normal)).unwrap_or(hit.normal);
        Reply::ok(obj(vec![
            ("hit", obj(vec![
                ("solid", s(hit.object)),
                ("node", hit.node.map_or(S::Nil, s)),
                ("point", mm3(hit.point)),
                ("normal", arr(hit.normal.iter().map(|x| n(*x)))),
                ("plate", mm3(xf_point(&to_plate, hit.point))),
                ("plate_normal", arr(plate_normal.iter().map(|x| n(*x)))),
                ("depth", mm(hit.depth)),
            ])),
            ("frame", s("point and normal: the solid's model mm, Y up; plate: printer mm, Z up")),
            ("ray", ray),
        ]))
    }
}
