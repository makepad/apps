//! Point maps and complex arithmetic shared by Plot and Draw2D.
use crate::obj;
use crate::plotting::{self, Point};
use makepad_script::*;

/// Unlike `vecn`, keep non-finite coordinates: a map's pole is a line break,
/// not a type error. Arrays keep double precision through complex arithmetic.
fn point(vm: &ScriptVm, value: ScriptValue) -> Option<Point> {
    let values = obj::items(vm, value)?;
    if values.len() != 2 { return None; }
    Some([values[0].as_number().or_else(|| obj::num(vm, values[0]))?,
        values[1].as_number().or_else(|| obj::num(vm, values[1]))?])
}

#[derive(Clone, Copy)]
pub(super) enum PointMap {
    Matrix([[f64; 3]; 2]),
    Function(ScriptValue),
}

impl PointMap {
    pub(super) fn parse(vm: &ScriptVm, value: ScriptValue) -> Result<Self, String> {
        if obj::is_fn(vm, value) { return Ok(Self::Function(value)); }
        let error = || "`map` is [[a, b], [c, d]], [[a, b, tx], [c, d, ty]], or fn(p) { vec2(p.x, p.y) }".to_string();
        let rows = obj::items(vm, value).filter(|r| r.len() == 2).ok_or_else(error)?;
        let mut matrix = [[0.0; 3]; 2];
        let mut columns = None;
        for (row, value) in matrix.iter_mut().zip(rows) {
            let values = obj::items(vm, value).filter(|r| r.len() == 2 || r.len() == 3).ok_or_else(error)?;
            if columns.is_some_and(|n| n != values.len()) { return Err(error()); }
            columns = Some(values.len());
            for (entry, value) in row.iter_mut().zip(values) {
                *entry = obj::num(vm, value).filter(|n| n.is_finite()).ok_or_else(error)?;
            }
        }
        Ok(Self::Matrix(matrix))
    }

    pub(super) fn apply(self, vm: &mut ScriptVm, p: Point) -> Result<Point, String> {
        if !plotting::finite(p) { return Ok([f64::NAN; 2]); }
        match self {
            Self::Matrix(m) => Ok(m.map(|r| r[0] * p[0] + r[1] * p[1] + r[2])),
            Self::Function(f) => {
                // Pass a real vec2 so .x/.y and vector arithmetic work in f.
                let ty = vm.pod_builtins().pod_vec2f;
                let pod = vm.bx.heap_mut().new_pod(ty);
                let data = vm.bx.heap_mut().pod_data_mut(pod);
                data[0] = (p[0] as f32).to_bits();
                data[1] = (p[1] as f32).to_bits();
                let value = vm.call(f, &[pod.into()]);
                point(vm, value).ok_or_else(|| format!("`map` returned {}; expected vec2(x, y) or [x, y]: map: fn(p) {{ vec2(p.x, p.y) }}", obj::kind(vm, value)))
            }
        }
    }
}

fn multiply(a: Point, b: Point) -> Point {
    [a[0] * b[0] - a[1] * b[1], a[0] * b[1] + a[1] * b[0]]
}

fn divide(a: Point, b: Point) -> Point {
    // Scale before squaring to avoid overflowing the divisor's norm.
    let scale = b[0].abs().max(b[1].abs());
    if scale == 0.0 { return [f64::NAN; 2]; }
    let [x, y] = [b[0] / scale, b[1] / scale];
    let norm = x * x + y * y;
    let [r, i] = [a[0] / scale, a[1] / scale];
    [(r * x + i * y) / norm, (i * x - r * y) / norm]
}

fn power(z: Point, n: f64) -> Point {
    if n == 0.0 { return [1.0, 0.0]; }
    let radius = z[0].hypot(z[1]).powf(n);
    let angle = z[1].atan2(z[0]) * n;
    [radius * angle.cos(), radius * angle.sin()]
}

pub(super) fn install(vm: &mut ScriptVm, module: ScriptObject) {
    fn fail(vm: &mut ScriptVm, message: &str) -> ScriptValue { script_err_invalid_args!(vm.trap(), "{message}") }
    type Binary = fn(Point, Point) -> Point;
    for (id, name, f) in [(id_lut!(cmul), "cmul", multiply as Binary), (id_lut!(cdiv), "cdiv", divide as Binary)] {
        vm.add_method(module, id, script_args!(a = NIL, b = NIL), move |vm, args| {
            let (Some(a), Some(b)) = (point(vm, obj::get(vm, args, "a")), point(vm, obj::get(vm, args, "b"))) else {
                return fail(vm, &format!("{name}(a, b) takes complex points vec2(real, imaginary) or [real, imaginary]"));
            };
            let [x, y] = f(a, b);
            obj::v2(vm, x, y)
        });
    }
    type Unary = fn(Point) -> Point;
    for (id, name, f) in [
        (id_lut!(cexp), "cexp", (|z: Point| { let r = z[0].exp(); [r * z[1].cos(), r * z[1].sin()] }) as Unary),
        (id_lut!(clog), "clog", (|z: Point| [z[0].hypot(z[1]).ln(), z[1].atan2(z[0])]) as Unary),
        (id_lut!(csqrt), "csqrt", (|z| power(z, 0.5)) as Unary),
        (id_lut!(conj), "conj", (|z: Point| [z[0], -z[1]]) as Unary),
    ] {
        vm.add_method(module, id, script_args!(z = NIL), move |vm, args| {
            let Some(z) = point(vm, obj::get(vm, args, "z")) else {
                return fail(vm, &format!("{name}(z) takes vec2(real, imaginary) or [real, imaginary]"));
            };
            let [x, y] = f(z);
            obj::v2(vm, x, y)
        });
    }
    vm.add_method(module, id_lut!(cpow), script_args!(z = NIL, n = NIL), |vm, args| {
        let (Some(z), Some(n)) = (point(vm, obj::get(vm, args, "z")), obj::num(vm, obj::get(vm, args, "n"))) else {
            return fail(vm, "cpow(z, n) takes a complex point and a real exponent");
        };
        let [x, y] = power(z, n);
        obj::v2(vm, x, y)
    });
    for (id, angle) in [(id_lut!(cabs), false), (id_lut!(carg), true)] {
        vm.add_method(module, id, script_args!(z = NIL), move |vm, args| {
            let Some(z) = point(vm, obj::get(vm, args, "z")) else {
                return fail(vm, "cabs(z) / carg(z) takes vec2(real, imaginary) or [real, imaginary]");
            };
            (if angle { z[1].atan2(z[0]) } else { z[0].hypot(z[1]) }).into()
        });
    }
    vm.add_method(module, id_lut!(map_points), script_args!(points = NIL, f = NIL, tolerance = 0.35), |vm, args| {
        let Some(values) = obj::items(vm, obj::get(vm, args, "points")) else {
            return fail(vm, "map_points(points, f): points is [[x, y], ...] or a list of polylines");
        };
        let map = match PointMap::parse(vm, obj::get(vm, args, "f")) { Ok(map) => map, Err(e) => return fail(vm, &e) };
        let Some(tolerance) = obj::num(vm, obj::get(vm, args, "tolerance")).filter(|v| *v > 0.0) else {
            return fail(vm, "map_points(points, f, tolerance): tolerance is a positive finite distance in your drawing units");
        };
        let lines = if values.first().is_some_and(|v| point(vm, *v).is_some()) {
            Some(vec![values])
        } else { values.into_iter().map(|v| obj::items(vm, v)).collect::<Option<Vec<_>>>() };
        let Some(lines) = lines else { return fail(vm, "map_points: each polyline is [[x, y], ...]"); };
        let lines: Option<Vec<Vec<Point>>> = lines.into_iter().map(|line| line.into_iter().map(|v| point(vm, v)).collect()).collect();
        let Some(lines) = lines else { return fail(vm, "map_points: each point is vec2(x, y) or [x, y]"); };
        let mut lo = [f64::INFINITY; 2];
        let mut hi = [f64::NEG_INFINITY; 2];
        for p in lines.iter().flatten().filter(|p| plotting::finite(**p)) {
            for axis in 0..2 { lo[axis] = lo[axis].min(p[axis]); hi[axis] = hi[axis].max(p[axis]); }
        }
        // A vertical input has no x extent; use its height for the initial split.
        let width = if hi[0] > lo[0] { hi[0] - lo[0] } else { hi[1] - lo[1] };
        let result = (|| -> Result<Vec<Vec<Point>>, String> {
            let mut out = Vec::new();
            for line in lines {
                let mut mapped = Vec::new();
                if let Some(&p) = line.first() { mapped.push(map.apply(vm, p)?); }
                for pair in line.windows(2) {
                    let fractions = plotting::map_subdivision(pair[0], pair[1], &mut |p| map.apply(vm, p), tolerance, width / 16.0)?;
                    for u in fractions.into_iter().skip(1) { mapped.push(map.apply(vm, plotting::mix(pair[0], pair[1], u))?); }
                }
                out.extend(mapped.split(|p| !plotting::finite(*p)).filter(|line| !line.is_empty()).map(<[Point]>::to_vec));
            }
            Ok(out)
        })();
        match result {
            Err(e) => fail(vm, &e),
            Ok(lines) => {
                let lines: Vec<_> = lines.into_iter().map(|line| {
                    let points: Vec<_> = line.into_iter().map(|[x, y]| obj::v2(vm, x, y)).collect();
                    obj::array(vm, points)
                }).collect();
                obj::array(vm, lines)
            }
        }
    });
}
