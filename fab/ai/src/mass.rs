//! A part's mass and balance (`fab.measure {what: "mass"}`): its grams as the job prints it (the
//! filament's density, the walls, the top and bottom shells and the infill), its centre of mass,
//! its inertia about that centre, and whether it stands on its base as modelled (Y up, resting on
//! its lowest points) with the tilt that tips it over.
//!
//! The figures are an estimate before a slice: the inside counts at the infill's share of the
//! filament, a skin as thick as the walls (or the shells, on faces that face the print's up or
//! down) counts solid. Regions with print settings of their own (`m.settings`) count at the
//! part's fill. Computed on demand, O(triangles), on the model worker; nothing per frame.

use crate::out::{arr, deg, flag, int, mm, mm2, mm3, n, obj, s, S};
use crate::{Reply, Session};
use fab_model::eval::Evaluated;
use makepad_csg_graph::TriMesh;
use makepad_fab_settings::profiles::JobSetup;
use makepad_model::json::Value;

/// How the job prints a part: what its grams and balance are worked out from.
#[derive(Clone, Debug, PartialEq)]
pub struct Fill {
    pub material: String,
    /// g/mm³.
    pub density: f64,
    /// The inside's share of the filament, 0 to 1.
    pub infill: f64,
    pub walls: u32,
    /// The walls' thickness, mm.
    pub wall: f64,
    /// The top and bottom shells' thickness, mm.
    pub top: f64,
    pub bottom: f64,
    /// The print's up in the model frame.
    pub up: [f64; 3],
    /// Read from the job's printer, process and filament (false: Fab's defaults).
    pub from_job: bool,
}

/// What a part weighs and how it balances, in the model frame (mm, Y up).
#[derive(Clone, Debug, PartialEq)]
pub struct Mass {
    pub volume: f64,
    pub grams: f64,
    pub com: [f64; 3],
    /// About the centre of mass along the model's axes, g·mm².
    pub inertia: [[f64; 3]; 3],
    /// The principal moments (smallest first) and their axes.
    pub principal: [f64; 3],
    pub axes: [[f64; 3]; 3],
    pub base: Base,
}

/// What the part stands on as modelled: its lowest points' outline (x, z at height `y`).
#[derive(Clone, Debug, PartialEq)]
pub struct Base {
    pub y: f64,
    pub outline: Vec<[f64; 2]>,
    pub area: f64,
    /// The centre of mass is over the outline.
    pub stands: bool,
    /// How far the centre of mass is inside the outline's nearest edge (negative: past it), mm.
    pub margin: f64,
    /// The tilt over that edge that tips it, degrees (0 when it does not stand).
    pub tip: f64,
    /// The way it tips: out of that edge, along the ground.
    pub toward: [f64; 2],
}

fn sub(a: [f64; 3], b: [f64; 3]) -> [f64; 3] { [a[0] - b[0], a[1] - b[1], a[2] - b[2]] }
fn dot(a: [f64; 3], b: [f64; 3]) -> f64 { a[0] * b[0] + a[1] * b[1] + a[2] * b[2] }
fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] { [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]] }

/// Weigh `mesh` (a closed solid in the model frame) as `fill` prints it.
pub fn weigh(mesh: &TriMesh, fill: &Fill) -> Result<Mass, String> {
    if mesh.triangles.is_empty() {
        return Err("the part is empty".into());
    }
    let bb = mesh.bounding_box();
    let o = [(bb.min.x + bb.max.x) * 0.5, (bb.min.y + bb.max.y) * 0.5, (bb.min.z + bb.max.z) * 0.5];
    let at = |i: u32| { let v = mesh.vertices[i as usize]; [v.x - o[0], v.y - o[1], v.z - o[2]] };
    let up = { let l = dot(fill.up, fill.up).sqrt(); if l > 0.0 { fill.up.map(|x| x / l) } else { [0.0, 1.0, 0.0] } };
    // The solid's volume moments (tetrahedra from the box's centre) and the skin's (area times
    // its thickness), about the box's centre for precision.
    let (mut v, mut v1, mut v2) = (0.0, [0.0; 3], [[0.0; 3]; 3]);
    let (mut s, mut s1, mut s2) = (0.0, [0.0; 3], [[0.0; 3]; 3]);
    for t in &mesh.triangles {
        let (a, b, c) = (at(t[0]), at(t[1]), at(t[2]));
        let sum = [a[0] + b[0] + c[0], a[1] + b[1] + c[1], a[2] + b[2] + c[2]];
        let second = |i: usize, j: usize| a[i] * a[j] + b[i] * b[j] + c[i] * c[j] + sum[i] * sum[j];
        let vol = dot(a, cross(b, c)) / 6.0;
        v += vol;
        for i in 0..3 {
            v1[i] += vol * sum[i] / 4.0;
            for j in 0..3 { v2[i][j] += vol / 20.0 * second(i, j); }
        }
        let normal = cross(sub(b, a), sub(c, a));
        let twice = dot(normal, normal).sqrt();
        if twice > 0.0 {
            let area = twice * 0.5;
            let facing = dot(normal, up) / twice;
            let skin = if facing > 0.7 { fill.top } else if facing < -0.7 { fill.bottom } else { fill.wall };
            s += area * skin;
            for i in 0..3 {
                s1[i] += area * skin * sum[i] / 3.0;
                for j in 0..3 { s2[i][j] += skin * area / 12.0 * second(i, j); }
            }
        }
    }
    // a mesh wound inside out still weighs what it holds
    if v < 0.0 {
        v = -v;
        v1 = v1.map(|x| -x);
        v2 = v2.map(|r| r.map(|x| -x));
    }
    if v <= 1e-9 {
        return Err("the part holds no volume (is its surface closed?)".into());
    }
    // A part thinner than its skin prints solid.
    let infill = fill.infill.clamp(0.0, 1.0);
    let (kv, ks) = if s >= v { (1.0, 0.0) } else { (infill, 1.0 - infill) };
    let rho = fill.density;
    let grams = rho * (kv * v + ks * s);
    let first: [f64; 3] = std::array::from_fn(|i| rho * (kv * v1[i] + ks * s1[i]));
    let c = first.map(|x| x / grams);
    let mut inertia = [[0.0; 3]; 3];
    let central: [[f64; 3]; 3] = std::array::from_fn(|i| std::array::from_fn(|j| rho * (kv * v2[i][j] + ks * s2[i][j]) - grams * c[i] * c[j]));
    let trace = central[0][0] + central[1][1] + central[2][2];
    for i in 0..3 {
        for j in 0..3 { inertia[i][j] = if i == j { trace } else { 0.0 } - central[i][j]; }
    }
    let (principal, axes) = principal(inertia);
    let com = [c[0] + o[0], c[1] + o[1], c[2] + o[2]];
    Ok(Mass { volume: v, grams, com, inertia, principal, axes, base: base(mesh, com) })
}

/// The eigenvalues (smallest first) and unit eigenvectors of a symmetric 3x3 (Jacobi rotations).
fn principal(m: [[f64; 3]; 3]) -> ([f64; 3], [[f64; 3]; 3]) {
    let mut a = m;
    let mut v = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
    for _ in 0..64 {
        let (p, q) = [(0, 1), (0, 2), (1, 2)].into_iter().max_by(|x, y| a[x.0][x.1].abs().total_cmp(&a[y.0][y.1].abs())).unwrap();
        let scale = a[0][0].abs() + a[1][1].abs() + a[2][2].abs();
        if a[p][q].abs() <= 1e-13 * scale.max(f64::MIN_POSITIVE) {
            break;
        }
        let theta = (a[q][q] - a[p][p]) / (2.0 * a[p][q]);
        let t = theta.signum() / (theta.abs() + (theta * theta + 1.0).sqrt());
        let c = 1.0 / (t * t + 1.0).sqrt();
        let s = t * c;
        for k in 0..3 {
            let (kp, kq) = (a[k][p], a[k][q]);
            a[k][p] = c * kp - s * kq;
            a[k][q] = s * kp + c * kq;
        }
        for k in 0..3 {
            let (pk, qk) = (a[p][k], a[q][k]);
            a[p][k] = c * pk - s * qk;
            a[q][k] = s * pk + c * qk;
        }
        for k in 0..3 {
            let (kp, kq) = (v[k][p], v[k][q]);
            v[k][p] = c * kp - s * kq;
            v[k][q] = s * kp + c * kq;
        }
    }
    let mut order = [0, 1, 2];
    order.sort_by(|&i, &j| a[i][i].total_cmp(&a[j][j]));
    (order.map(|i| a[i][i]), order.map(|i| [v[0][i], v[1][i], v[2][i]]))
}

/// The lowest points' outline and how the centre of mass stands over it.
fn base(mesh: &TriMesh, com: [f64; 3]) -> Base {
    let bb = mesh.bounding_box();
    let y = bb.min.y;
    let size = bb.max - bb.min;
    let tol = (1e-4 * (size.x * size.x + size.y * size.y + size.z * size.z).sqrt()).max(0.05);
    let points: Vec<[f64; 2]> = mesh.vertices.iter().filter(|p| p.y <= y + tol).map(|p| [p.x, p.z]).collect();
    let outline = hull(points);
    let cross2 = |o: [f64; 2], a: [f64; 2], b: [f64; 2]| (a[0] - o[0]) * (b[1] - o[1]) - (a[1] - o[1]) * (b[0] - o[0]);
    let area = (0..outline.len()).map(|i| cross2([0.0, 0.0], outline[i], outline[(i + 1) % outline.len()])).sum::<f64>().abs() * 0.5;
    let q = [com[0], com[2]];
    let height = com[1] - y;
    // signed distance from each edge (inside positive: the outline runs counter-clockwise in x, z)
    let nearest = (0..outline.len()).filter_map(|i| {
        let (a, b) = (outline[i], outline[(i + 1) % outline.len()]);
        let d = [b[0] - a[0], b[1] - a[1]];
        let len = (d[0] * d[0] + d[1] * d[1]).sqrt();
        (len > 0.0).then(|| (cross2(a, b, q) / len, [d[1] / len, -d[0] / len]))
    }).min_by(|x, y| x.0.total_cmp(&y.0));
    match nearest.filter(|_| outline.len() >= 3 && area > 1e-6) {
        Some((margin, toward)) => {
            let stands = margin > 0.0;
            let tip = if stands { margin.atan2(height.max(0.0)).to_degrees() } else { 0.0 };
            Base { y, outline, area, stands, margin, tip, toward }
        }
        // on a point or a line: it rolls or falls
        None => Base { y, outline, area, stands: false, margin: 0.0, tip: 0.0, toward: [0.0, 0.0] },
    }
}

/// The convex hull, counter-clockwise (monotone chain).
fn hull(mut p: Vec<[f64; 2]>) -> Vec<[f64; 2]> {
    p.sort_by(|a, b| a[0].total_cmp(&b[0]).then(a[1].total_cmp(&b[1])));
    p.dedup();
    if p.len() < 3 {
        return p;
    }
    let turn = |o: [f64; 2], a: [f64; 2], b: [f64; 2]| (a[0] - o[0]) * (b[1] - o[1]) - (a[1] - o[1]) * (b[0] - o[0]);
    let mut out: Vec<[f64; 2]> = Vec::with_capacity(p.len() * 2);
    for pass in 0..2 {
        let start = out.len();
        let points: Box<dyn Iterator<Item = &[f64; 2]>> = if pass == 0 { Box::new(p.iter()) } else { Box::new(p.iter().rev()) };
        for &q in points {
            while out.len() >= start + 2 && turn(out[out.len() - 2], out[out.len() - 1], q) <= 0.0 {
                out.pop();
            }
            out.push(q);
        }
        out.pop();
    }
    out
}

/// A setting's number: `20%` is 0.2 of `of` when `of` is given, else 20; a bare number is itself.
fn number(text: &str, of: Option<f64>) -> Option<f64> {
    let t = text.trim();
    match t.strip_suffix('%') {
        Some(pct) => pct.trim().parse::<f64>().ok().map(|x| of.map_or(x, |of| x / 100.0 * of)),
        None => t.parse::<f64>().ok(),
    }
}

impl Session {
    /// How the job prints `object`: the job's process, the plate's settings over it, the part's
    /// own over those (as the slicer reads them); Fab's defaults without a job.
    fn fill_of(&self, ev: &Evaluated, setup: Option<&JobSetup>, object: &str) -> Fill {
        let part = ev.plate.parts.iter().find(|p| p.object == object);
        let setting = |key: &str| -> Option<String> {
            let mut value = setup.and_then(|s| s.process.values.get(key)).map(|v| v.at(0).to_string())
                .or_else(|| fab_plate::print_setting(key).map(|row| row.default.to_string()));
            if let Some((_, v)) = ev.plate.settings.iter().rev().find(|(k, _)| k == key) { value = Some(v.clone()); }
            if let Some((_, v)) = part.and_then(|p| p.print.iter().rev().find(|(k, _)| k == key)) { value = Some(v.clone()); }
            value.filter(|v| !v.trim().is_empty())
        };
        let profile = &self.eval.profile;
        let nozzle = setup.map(|s| s.machine.values.f64_at("nozzle_diameter", 0)).filter(|n| *n > 0.0).unwrap_or(profile.nozzle);
        let layer = setting("layer_height").and_then(|v| number(&v, None)).filter(|l| *l > 0.0).unwrap_or(profile.layer);
        let width = |key: &str| setting(key).and_then(|v| number(&v, Some(nozzle))).filter(|w| *w > 0.0).unwrap_or(nozzle * 1.125);
        let walls = setting("wall_loops").and_then(|v| number(&v, None)).map_or(2, |w| w.max(0.0).round() as u32);
        let wall = if walls == 0 { 0.0 } else { width("outer_wall_line_width") + (walls - 1) as f64 * width("inner_wall_line_width") };
        let shell = |layers: &str, thickness: &str, default: f64| {
            let by_layers = setting(layers).and_then(|v| number(&v, None)).unwrap_or(default) * layer;
            by_layers.max(setting(thickness).and_then(|v| number(&v, None)).unwrap_or(0.0))
        };
        let filament = setup.and_then(|s| {
            let k = part.map_or(0, |p| p.filament.saturating_sub(1) as usize);
            s.filaments.get(k).or(s.filaments.first())
        });
        let density = filament.map(|f| f.values.f64_at("filament_density", 0)).filter(|d| *d > 0.0).map(|d| d * 1e-3).unwrap_or(profile.material.density);
        let material = filament.map(|f| f.values.raw("filament_type", 0).to_string()).filter(|t| !t.is_empty()).unwrap_or_else(|| profile.material.name.clone());
        Fill {
            material,
            density,
            // `15%`, or a bare 15 (a share above 1 is a percentage)
            infill: setting("sparse_infill_density").and_then(|v| number(&v, Some(1.0))).map(|x| if x > 1.0 { x / 100.0 } else { x }).unwrap_or(0.15),
            walls,
            wall,
            top: shell("top_shell_layers", "top_shell_thickness", 5.0),
            bottom: shell("bottom_shell_layers", "bottom_shell_thickness", 3.0),
            up: profile.up,
            from_job: setup.is_some(),
        }
    }

    /// `fab.measure {what: "mass"}`: each part's grams, centre of mass, inertia and footing.
    pub(crate) fn mass_tool(&mut self, args: &Value, ev: &Evaluated) -> Reply {
        let objects = crate::inspect::objects_arg(args, ev);
        if objects.is_empty() {
            return Reply::err("nothing is printed to weigh");
        }
        let outline = args.get("outline").and_then(Value::as_bool) == Some(true);
        // the job as the slicer would read it, once for every part weighed
        let setup = self.print_setup.as_ref().and_then(|f| f(ev).ok()).map(|(setup, _, _)| setup);
        let mut rows = Vec::new();
        let mut total = 0.0;
        for object in objects {
            let fill = self.fill_of(ev, setup.as_ref(), &object);
            let mass = match self.eval.object_mesh(&object).and_then(|mesh| weigh(&mesh, &fill)) {
                Ok(mass) => mass,
                Err(why) => return Reply::err(format!("{object}: {why}")),
            };
            total += mass.grams;
            rows.push(answer(&object, &fill, &mass, outline));
        }
        let mut fields = vec![("parts", arr(rows)), ("total_grams", n(total))];
        fields.push(("note", s("an estimate before a slice: the inside at the infill's share, a skin as thick as the walls and shells solid; regions with their own print settings count at the part's fill. Model frame: mm, Y up; the base is what it rests on as modelled.")));
        Reply::ok(obj(fields))
    }
}

fn answer(object: &str, fill: &Fill, mass: &Mass, outline: bool) -> S {
    let i = &mass.inertia;
    let b = &mass.base;
    let mut base = vec![
        ("stands", flag(b.stands)),
        ("margin", mm(b.margin)),
        ("area_mm2", n(b.area)),
        ("centre_height", mm(mass.com[1] - b.y)),
    ];
    if b.stands {
        base.push(("tip_angle", deg(b.tip)));
    }
    if b.toward != [0.0, 0.0] {
        base.push(("tips_toward", arr([n(b.toward[0]), n(0.0), n(b.toward[1])])));
    }
    if outline {
        base.push(("y", mm(b.y)));
        base.push(("outline", arr(b.outline.iter().map(|p| mm2(*p)))));
    }
    obj(vec![
        ("solid", s(object)),
        ("grams", n(mass.grams)),
        ("material", s(&fill.material)),
        ("fill", obj(vec![
            ("density_g_cm3", n(fill.density * 1e3)),
            ("infill", s(format!("{}%", (fill.infill * 100.0).round()))),
            ("walls", int(fill.walls)),
            ("wall", mm(fill.wall)),
            ("top", mm(fill.top)),
            ("bottom", mm(fill.bottom)),
            ("from", s(if fill.from_job { "the job's printer, process and filament" } else { "Fab's defaults: no print job set up" })),
        ])),
        ("volume_mm3", n(mass.volume)),
        ("centre_of_mass", mm3(mass.com)),
        ("inertia_g_mm2", obj(vec![
            ("about", s("the centre of mass, model axes")),
            ("xx", n(i[0][0])), ("yy", n(i[1][1])), ("zz", n(i[2][2])),
            ("xy", n(i[0][1])), ("xz", n(i[0][2])), ("yz", n(i[1][2])),
            ("principal", arr(mass.principal.map(n))),
            ("axes", arr(mass.axes.map(|a| arr(a.map(n))))),
        ])),
        ("base", obj(base)),
    ])
}

