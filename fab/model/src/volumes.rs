//! Print settings volumes: a solid of an object's script with `m.settings` on it, where the part
//! prints with infill and walls of its own. A volume's shape is what it says it is:
//!
//! ```text
//! let left_arm_settings = m.offset(left_arm, 1mm)      // follows a feature, grown past its blend
//! m.settings(left_arm_settings, {sparse_infill_density: "100%" wall_loops: 4})
//! let settings1 = m.box({size: [8mm 8mm 8mm] at: [0 10mm 0]})
//! m.settings(settings1, {sparse_infill_density: "100%"})
//! let elbow_settings = m.capsule({a: [0mm, 0mm, 0mm] b: [20mm, 12mm, 0mm] radius: 5mm})
//! m.settings(elbow_settings, {sparse_infill_density: "100%"})
//! ```
//!
//! Each is a solid of its own at the script's end, one undo step however many a batch writes, so
//! its name, its row, its rows' edits and its delete are one path whatever its shape; the feature
//! it follows stays what it was, and deleting that feature takes the volume with it. Fits are
//! written from points of what they fit round, on the plate as the part stands.

use crate::program::{part_id, Emitted, Instance};
use crate::{Change, Edit};
use makepad_model_script::lang::Program;

type M3 = [[f64; 3]; 3];

/// A new volume's settings: a solid infill to start.
pub const DEFAULT_SETTINGS: &[(&str, &str)] = &[("sparse_infill_density", "100%")];

/// How far a volume over a whole part reaches past its sides and top (mm): its box shows clear
/// of the part's skin, its handles off it.
pub const OVER_PART: f64 = 1.0;

/// What a fitted shape takes round what it fits (mm): more than a slicer's rounding, so its
/// surface stays clear of the feature's own.
pub const FIT_MARGIN: f64 = 0.5;

/// How far a volume that follows a feature grows past it to start with (mm): into the fillet of
/// the blend it joins by, which the feature's own solid does not hold.
pub const DEFAULT_GROW: f64 = 1.0;

/// The shape of a print settings volume.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum VolumeShape {
    /// The feature itself, grown ([`DEFAULT_GROW`]).
    Follows,
    #[default]
    Box,
    Sphere,
    Cylinder,
    /// A rod with round ends from A to B, at any angle: what a limb on a diagonal fits.
    Capsule,
}

impl VolumeShape {
    pub const ALL: [VolumeShape; 5] = [VolumeShape::Follows, VolumeShape::Box, VolumeShape::Sphere, VolumeShape::Cylinder, VolumeShape::Capsule];

    pub fn name(self) -> &'static str {
        match self {
            VolumeShape::Follows => "follows",
            VolumeShape::Box => "box",
            VolumeShape::Sphere => "sphere",
            VolumeShape::Cylinder => "cylinder",
            VolumeShape::Capsule => "capsule",
        }
    }

    /// The numbered stem of a volume of this shape on no feature: a box `settings1`, a sphere
    /// `sphere_settings1`.
    pub fn stem(self) -> &'static str {
        match self {
            VolumeShape::Follows | VolumeShape::Box => "settings",
            VolumeShape::Sphere => "sphere_settings",
            VolumeShape::Cylinder => "cylinder_settings",
            VolumeShape::Capsule => "capsule_settings",
        }
    }

    pub fn parse(name: &str) -> Option<VolumeShape> {
        Self::ALL.into_iter().find(|s| s.name() == name)
    }
}

/// Primitives placed on the plate (mm), written in an object's model frame.
pub struct ModelWriter {
    frame: [[f64; 4]; 3],
    /// The frame's scale.
    pub k: f64,
    /// Plate to model: the frame's turn back.
    to_model_rot: M3,
    /// A box's or cylinder's own frame in the model's: local Y is the plate's Z (the model's
    /// up), local Z its -Y.
    local: M3,
}

impl ModelWriter {
    pub fn new(frame: &[[f64; 4]; 3]) -> ModelWriter {
        let f = frame;
        let lin: M3 = std::array::from_fn(|i| [f[i][0], f[i][1], f[i][2]]);
        let k = (lin[0][0].powi(2) + lin[1][0].powi(2) + lin[2][0].powi(2)).sqrt().max(1e-9);
        let to_model_rot: M3 = std::array::from_fn(|i| std::array::from_fn(|j| lin[j][i] / k));
        let q: M3 = [[1.0, 0.0, 0.0], [0.0, 0.0, -1.0], [0.0, 1.0, 0.0]];
        let local = std::array::from_fn(|i| std::array::from_fn(|j| (0..3).map(|n| to_model_rot[i][n] * q[n][j]).sum()));
        ModelWriter { frame: *frame, k, to_model_rot, local }
    }

    /// A plate direction in the model frame.
    pub fn turned(&self, d: [f64; 3]) -> [f64; 3] {
        std::array::from_fn(|i| self.to_model_rot[i][0] * d[0] + self.to_model_rot[i][1] * d[1] + self.to_model_rot[i][2] * d[2])
    }

    /// A plate point in the model frame.
    pub fn to_model(&self, q: [f64; 3]) -> [f64; 3] {
        let f = &self.frame;
        let d = [q[0] - f[0][3], q[1] - f[1][3], q[2] - f[2][3]];
        std::array::from_fn(|i| (self.to_model_rot[i][0] * d[0] + self.to_model_rot[i][1] * d[1] + self.to_model_rot[i][2] * d[2]) / self.k)
    }

    /// The `at` (and `rotate`, for a turned object) of a primitive centred at plate point `c`.
    pub fn place(&self, c: [f64; 3]) -> String {
        let m = self.to_model(c);
        let mut s = format!("at: [{}, {}, {}]", mm(m[0]), mm(m[1]), mm(m[2]));
        let turned = (0..3).any(|i| (0..3).any(|j| (self.local[i][j] - if i == j { 1.0 } else { 0.0 }).abs() > 1e-6));
        if turned {
            let e = crate::eval::euler_xyz(&self.local);
            s += &format!(" rotate: [{}, {}, {}]", deg(e[0]), deg(e[1]), deg(e[2]));
        }
        s
    }

    /// A box of plate size `sz` (along the plate's X, Y and Z) centred at `c`.
    pub fn boxed(&self, sz: [f64; 3], c: [f64; 3]) -> String {
        let k = self.k;
        format!("m.box({{size: [{}, {}, {}] {}}})", mm(sz[0] / k), mm(sz[2] / k), mm(sz[1] / k), self.place(c))
    }

    /// A cylinder standing along the plate's Z, centred at `c`.
    pub fn cylinder(&self, radius: f64, height: f64, c: [f64; 3]) -> String {
        format!("m.cylinder({{radius: {} height: {} {}}})", mm(radius / self.k), mm(height / self.k), self.place(c))
    }

    /// A sphere centred at `c` (no turn: it has none to give).
    pub fn sphere(&self, radius: f64, c: [f64; 3]) -> String {
        let m = self.to_model(c);
        format!("m.sphere({{radius: {} at: [{}, {}, {}]}})", mm(radius / self.k), mm(m[0]), mm(m[1]), mm(m[2]))
    }
}

pub fn mm(v: f64) -> String {
    let r = (v * 10.0).round() / 10.0;
    format!("{}mm", if r == 0.0 { 0.0 } else { r })
}

pub fn deg(v: f64) -> String {
    let r = (v * 10.0).round() / 10.0;
    format!("{}°", if r == 0.0 { 0.0 } else { r })
}

pub fn taken(p: &Program, n: &str) -> bool {
    p.solids.iter().any(|s| s.id == n || s.id.split(['.', '[']).next() == Some(n)) || p.names.syntax.find_let(n).is_some() || p.params.iter().any(|q| q.name.as_deref() == Some(n))
}

/// Whether a solid's name is one a statement can write: a top-level `let`, or an item of one's
/// list.
pub fn writable(p: &Program, id: &str) -> bool {
    match id.split_once('[') {
        Some((head, rest)) => rest.strip_suffix(']').is_some_and(|n| n.parse::<usize>().is_ok()) && p.names.syntax.find_let(head).is_some(),
        None => p.solid(id).is_some_and(|s| s.named),
    }
}

/// The solids of `p` a person can name as features of its printed objects: named so a statement can
/// write them, inside an object and not the object itself, no marked area.
pub fn named_features(p: &Program, objects: &[Emitted]) -> Vec<usize> {
    let areas = crate::program::area_solids(p);
    (0..p.solids.len())
        .filter(|i| !areas.contains(i) && writable(p, &p.solids[*i].id))
        .filter(|&i| !objects.iter().any(|o| o.bodies.contains(&i)) && objects.iter().any(|o| o.bodies.iter().any(|&b| crate::paint::within(p, i, b))))
        .collect()
}

/// `cut1`, `cut2`, ... (`numbered`), or `cut_bottom`, `cut_bottom2`, ...
pub fn free_name(p: &Program, stem: &str, numbered: bool) -> String {
    (1..).map(|i| if numbered { format!("{stem}{i}") } else if i == 1 { stem.to_string() } else { format!("{stem}{i}") }).find(|n| !taken(p, n)).unwrap()
}

/// The statements of a volume `name` of shape `solid` with `settings`, at the end of `src`.
fn statement_text(src: &str, name: &str, solid: &str, settings: &[(&str, &str)]) -> String {
    let values: Vec<String> = settings.iter().map(|(k, v)| if v.parse::<f64>().is_ok() { format!("{k}: {v}") } else { format!("{k}: \"{v}\"") }).collect();
    format!("{}let {name} = {solid}\nm.settings({name}, {{{}}})\n", if src.is_empty() || src.ends_with('\n') { "" } else { "\n" }, values.join(" "))
}

/// `solid` as `inst`'s next print settings volume (`stem`, numbered or not: [`free_name`]) with
/// `settings`, at the script's end, one undo step; and the plate part id of the volume.
pub fn settings_statement(inst: &Instance, stem: &str, numbered: bool, solid: &str, settings: &[(&str, &str)]) -> (Vec<Change>, String) {
    let p = &inst.script.program;
    let name = free_name(p, stem, numbered);
    let src = p.source();
    let edit = Edit::Splice { range: src.len()..src.len(), text: statement_text(src, &name, solid, settings) };
    (vec![Change::Script { file: inst.script.file.clone(), edit }], part_id(&inst.name, &name))
}

/// The stem of a volume on `feature`'s name: `left_arm_settings`, `holes_1_settings`.
pub fn volume_stem(feature: &str) -> String {
    let ident: String = feature.chars().map(|c| if c.is_ascii_alphanumeric() || c == '_' { c } else { '_' }).collect();
    format!("{}_settings", ident.trim_matches('_'))
}

/// One volume of a batch ([`batch_statements`]): its stem (numbered or not) and its shape's code.
pub struct NewVolume {
    pub stem: String,
    pub numbered: bool,
    pub solid: String,
}

/// Several volumes on `inst` with the same `settings`: one splice at the script's end, so one
/// change of a transaction however many; and their plate part ids.
pub fn batch_statements(inst: &Instance, volumes: &[NewVolume], settings: &[(&str, &str)]) -> (Vec<Change>, Vec<String>) {
    let p = &inst.script.program;
    let src = p.source();
    let mut text = String::new();
    let mut names: Vec<String> = Vec::new();
    for v in volumes {
        let free = |n: &String| !taken(p, n) && !names.contains(n);
        let name = (1..).map(|i| if v.numbered || i > 1 { format!("{}{i}", v.stem) } else { v.stem.clone() }).find(free).unwrap();
        // what the splice has written so far ends in a line, and so does what it follows
        let before = if text.is_empty() { src } else { "\n" };
        text += &statement_text(before, &name, &v.solid, settings);
        names.push(name);
    }
    let edit = Edit::Splice { range: src.len()..src.len(), text };
    (vec![Change::Script { file: inst.script.file.clone(), edit }], names.iter().map(|n| part_id(&inst.name, n)).collect())
}

/// A list of three numbers in mm, as a primitive's fields write it.
fn point_code(p: [f64; 3]) -> String {
    format!("[{}, {}, {}]", mm(p[0]), mm(p[1]), mm(p[2]))
}

/// The shapes placed by numbers in the model frame (mm), the frame `fab.pick`'s `point` is in: at
/// any angle, what a limb on a diagonal takes. Each is a shape's code, or why its numbers are no
/// shape.
pub fn capsule_code(a: [f64; 3], b: [f64; 3], radius: f64) -> Result<String, String> {
    if radius <= 0.0 {
        return Err(format!("a capsule's radius is more than 0, not {radius}"));
    }
    Ok(format!("m.capsule({{a: {} b: {} radius: {}}})", point_code(a), point_code(b), mm(radius)))
}

/// A cylinder from `a` to `b`: see [`capsule_code`].
pub fn cylinder_code(a: [f64; 3], b: [f64; 3], radius: f64) -> Result<String, String> {
    let d = sub(b, a);
    let height = dot(d, d).sqrt();
    if radius <= 0.0 || height <= 0.0 {
        return Err(format!("a cylinder has two ends apart and a radius more than 0, not {height} long and {radius} round"));
    }
    let at: [f64; 3] = std::array::from_fn(|k| 0.5 * (a[k] + b[k]));
    Ok(format!("m.cylinder({{radius: {} height: {} at: {} rotate: {{to: [{:.4}, {:.4}, {:.4}]}}}})", mm(radius), mm(height), point_code(at), d[0] / height, d[1] / height, d[2] / height))
}

/// A sphere: see [`capsule_code`].
pub fn sphere_code(at: [f64; 3], radius: f64) -> Result<String, String> {
    if radius <= 0.0 {
        return Err(format!("a sphere's radius is more than 0, not {radius}"));
    }
    Ok(format!("m.sphere({{radius: {} at: {}}})", mm(radius), point_code(at)))
}

/// A box of sides `size` about its middle `at`, its height along direction `up` if given: see
/// [`capsule_code`].
pub fn box_code(at: [f64; 3], size: [f64; 3], up: Option<[f64; 3]>) -> Result<String, String> {
    if size.iter().any(|d| *d <= 0.0) {
        return Err(format!("a box's sides are more than 0, not {size:?}"));
    }
    let turn = match up {
        None => String::new(),
        Some(d) => {
            let l = dot(d, d).sqrt();
            if l <= 0.0 {
                return Err("a box's `up` is a direction, not zero".to_string());
            }
            format!(" rotate: {{to: [{:.4}, {:.4}, {:.4}]}}", d[0] / l, d[1] / l, d[2] / l)
        }
    };
    Ok(format!("m.box({{size: {} at: {}{turn}}})", point_code(size), point_code(at)))
}

/// A volume that follows `feature` (as a statement writes it), grown by `grow` mm.
pub fn follows_code(feature: &str, grow: f64) -> String {
    format!("m.offset({feature}, {})", mm(grow))
}

/// What a volume follows, and how far it grows: `m.offset(left_arm, 1mm)` reads `left_arm`, 1.
pub fn followed(p: &Program, i: usize) -> Option<(String, f64)> {
    let solid = &p.solids[i];
    if solid.proto != "Offset" {
        return None;
    }
    let call = makepad_model_script::lang::edit::part_call(p, solid).ok()?;
    let proto = makepad_model_script::lang::protos::proto("Offset")?;
    let fields = makepad_model_script::lang::edit::call_fields(&p.names.syntax, &call, proto);
    let text = |key: &str| -> Option<String> {
        let (_, at) = fields.iter().find(|(name, _)| name == key)?;
        Some(p.names.syntax.node(at.loc())?.text(p.source()).to_string())
    };
    let grow = text("distance").and_then(|t| t.trim().trim_end_matches("mm").trim().parse::<f64>().ok()).unwrap_or(0.0);
    Some((text("source")?, grow))
}

/// A box over the whole of a part with `bounds` on the plate: [`OVER_PART`] past its sides and
/// top, from its bottom.
pub fn over_part(w: &ModelWriter, min: [f64; 3], max: [f64; 3]) -> String {
    let d = OVER_PART;
    let (lo, hi) = ([min[0] - d, min[1] - d, min[2]], [max[0] + d, max[1] + d, max[2] + d]);
    w.boxed(std::array::from_fn(|k| hi[k] - lo[k]), std::array::from_fn(|k| 0.5 * (lo[k] + hi[k])))
}

fn sub(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    std::array::from_fn(|k| a[k] - b[k])
}

fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    (0..3).map(|k| a[k] * b[k]).sum()
}

/// A sphere round `points` (Ritter's: the farthest pair, then grown over what lies outside).
fn round_sphere(points: &[[f64; 3]]) -> ([f64; 3], f64) {
    let far = |from: [f64; 3]| points.iter().copied().max_by(|a, b| dot(sub(*a, from), sub(*a, from)).total_cmp(&dot(sub(*b, from), sub(*b, from)))).unwrap_or(from);
    let a = far(points[0]);
    let b = far(a);
    let mut c: [f64; 3] = std::array::from_fn(|k| 0.5 * (a[k] + b[k]));
    let mut r = 0.5 * dot(sub(a, b), sub(a, b)).sqrt();
    for &p in points {
        let d = dot(sub(p, c), sub(p, c)).sqrt();
        if d > r {
            let grown = 0.5 * (r + d);
            let t = (grown - r) / d;
            c = std::array::from_fn(|k| c[k] + (p[k] - c[k]) * t);
            r = grown;
        }
    }
    (c, r)
}

/// The direction `points` spread along most (their covariance's first eigenvector).
fn long_axis(points: &[[f64; 3]]) -> [f64; 3] {
    let n = points.len().max(1) as f64;
    let mean: [f64; 3] = std::array::from_fn(|k| points.iter().map(|p| p[k]).sum::<f64>() / n);
    let mut cov = [[0.0; 3]; 3];
    for p in points {
        let d = sub(*p, mean);
        for i in 0..3 {
            for j in 0..3 {
                cov[i][j] += d[i] * d[j];
            }
        }
    }
    // from the box's longest side: a start the eigenvector cannot be square to on a long shape
    let side = |k: usize| points.iter().map(|p| p[k]).fold(f64::MIN, f64::max) - points.iter().map(|p| p[k]).fold(f64::MAX, f64::min);
    let longest = (0..3).max_by(|a, b| side(*a).total_cmp(&side(*b))).unwrap_or(2);
    let mut v: [f64; 3] = std::array::from_fn(|k| if k == longest { 1.0 } else { 0.1 });
    for _ in 0..32 {
        let w: [f64; 3] = std::array::from_fn(|i| dot(cov[i], v));
        let l = dot(w, w).sqrt();
        if l <= 0.0 {
            break;
        }
        v = w.map(|x| x / l);
    }
    v
}

/// Under this angle from the plate's up (its cosine), a fitted cylinder stands up: its rows and
/// handles read as the plate's.
const STANDS: f64 = 0.97;

/// A shape fitted in some frame's mm: what `fab.fit` answers, and what a fitted volume writes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Fit {
    Box { min: [f64; 3], max: [f64; 3] },
    Sphere { at: [f64; 3], radius: f64 },
    Cylinder { a: [f64; 3], b: [f64; 3], radius: f64 },
    Capsule { a: [f64; 3], b: [f64; 3], radius: f64 },
}

impl Fit {
    /// Its shape's name.
    pub fn shape(&self) -> VolumeShape {
        match self {
            Fit::Box { .. } => VolumeShape::Box,
            Fit::Sphere { .. } => VolumeShape::Sphere,
            Fit::Cylinder { .. } => VolumeShape::Cylinder,
            Fit::Capsule { .. } => VolumeShape::Capsule,
        }
    }

    /// Its box.
    pub fn bounds(&self) -> ([f64; 3], [f64; 3]) {
        match *self {
            Fit::Box { min, max } => (min, max),
            Fit::Sphere { at, radius } => (at.map(|x| x - radius), at.map(|x| x + radius)),
            Fit::Cylinder { a, b, radius } | Fit::Capsule { a, b, radius } => (std::array::from_fn(|k| a[k].min(b[k]) - radius), std::array::from_fn(|k| a[k].max(b[k]) + radius)),
        }
    }

    /// The code of this shape, it being on the plate.
    pub fn code(&self, w: &ModelWriter) -> String {
        let k = w.k;
        match *self {
            Fit::Box { min, max } => w.boxed(std::array::from_fn(|i| max[i] - min[i]), std::array::from_fn(|i| 0.5 * (min[i] + max[i]))),
            Fit::Sphere { at, radius } => w.sphere(radius, at),
            Fit::Capsule { a, b, radius } => format!("m.capsule({{a: {} b: {} radius: {}}})", point_code(w.to_model(a)), point_code(w.to_model(b)), mm(radius / k)),
            Fit::Cylinder { a, b, radius } => {
                let d = sub(b, a);
                let height = dot(d, d).sqrt().max(1e-9);
                let c: [f64; 3] = std::array::from_fn(|i| 0.5 * (a[i] + b[i]));
                if d[2].abs() / height >= STANDS {
                    return w.cylinder(radius, height, c);
                }
                let at = w.to_model(c);
                let dir = w.turned(d.map(|x| x / height));
                format!("m.cylinder({{radius: {} height: {} at: {} rotate: {{to: [{:.3}, {:.3}, {:.3}]}}}})", mm(radius / k), mm(height / k), point_code(at), dir[0], dir[1], dir[2])
            }
        }
    }

    /// Written by its numbers, it being in the model frame already, as a target of
    /// `fab.settings_volume` takes them: `(key, numbers)`.
    pub fn numbers(&self) -> Vec<(&'static str, Vec<f64>)> {
        match *self {
            Fit::Box { min, max } => vec![("at", (0..3).map(|i| 0.5 * (min[i] + max[i])).collect()), ("size", (0..3).map(|i| max[i] - min[i]).collect())],
            Fit::Sphere { at, radius } => vec![("at", at.to_vec()), ("radius", vec![radius])],
            Fit::Cylinder { a, b, radius } | Fit::Capsule { a, b, radius } => vec![("a", a.to_vec()), ("b", b.to_vec()), ("radius", vec![radius])],
        }
    }
}

/// A cylinder's or capsule's axis through `points`' middle across `axis` (a unit vector): where
/// they stop along it and the farthest of them from it.
fn along(points: &[[f64; 3]], axis: [f64; 3]) -> ([f64; 3], f64, f64, f64) {
    let n = points.len().max(1) as f64;
    let mean: [f64; 3] = std::array::from_fn(|k| points.iter().map(|p| p[k]).sum::<f64>() / n);
    let off: [f64; 3] = { let t = dot(mean, axis); std::array::from_fn(|k| mean[k] - t * axis[k]) };
    let (mut lo, mut hi, mut r2) = (f64::MAX, f64::MIN, 0.0f64);
    for p in points {
        let d = sub(*p, off);
        let t = dot(d, axis);
        lo = lo.min(t);
        hi = hi.max(t);
        r2 = r2.max(dot(d, d) - t * t);
    }
    (off, lo, hi, r2.sqrt())
}

/// A shape fitted round `points`, [`FIT_MARGIN`] clear of them: a box along their frame's axes, a
/// sphere, or a cylinder or capsule along their long axis (`upright`: along the frame's Z). None
/// for no points, or for [`VolumeShape::Follows`], which fits nothing. A capsule's ends are a radius
/// in from where the points stop, so its round caps end there.
pub fn fit_points(shape: VolumeShape, points: &[[f64; 3]], upright: bool) -> Option<Fit> {
    if points.is_empty() {
        return None;
    }
    let axis = || if upright { [0.0, 0.0, 1.0] } else { long_axis(points) };
    let at = |off: [f64; 3], axis: [f64; 3], t: f64| -> [f64; 3] { std::array::from_fn(|k| off[k] + axis[k] * t) };
    Some(match shape {
        VolumeShape::Follows => return None,
        VolumeShape::Box => Fit::Box {
            min: std::array::from_fn(|k| points.iter().map(|p| p[k]).fold(f64::MAX, f64::min) - FIT_MARGIN),
            max: std::array::from_fn(|k| points.iter().map(|p| p[k]).fold(f64::MIN, f64::max) + FIT_MARGIN),
        },
        VolumeShape::Sphere => {
            let (at, r) = round_sphere(points);
            Fit::Sphere { at, radius: r + FIT_MARGIN }
        }
        VolumeShape::Cylinder => {
            let d = axis();
            let (off, lo, hi, r) = along(points, d);
            Fit::Cylinder { a: at(off, d, lo - FIT_MARGIN), b: at(off, d, hi + FIT_MARGIN), radius: r + FIT_MARGIN }
        }
        VolumeShape::Capsule => {
            let d = axis();
            let (off, lo, hi, r) = along(points, d);
            let radius = r + FIT_MARGIN;
            // a short shape's caps meet at its middle: a sphere's reach
            let (a, b) = if hi - lo > 2.0 * radius { (lo + radius, hi - radius) } else { (0.5 * (lo + hi), 0.5 * (lo + hi)) };
            Fit::Capsule { a: at(off, d, a), b: at(off, d, b), radius }
        }
    })
}

/// A shape's code fitted round `points` on the plate ([`fit_points`]).
pub fn fitted(shape: VolumeShape, w: &ModelWriter, points: &[[f64; 3]], upright: bool) -> Option<String> {
    fit_points(shape, points, upright).map(|f| f.code(w))
}
