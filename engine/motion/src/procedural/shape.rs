//! `Shape{op: @union layers: [Rect{…}, Ellipse{…}, Path{…}, Text{…}]}`: the
//! children's filled outlines combined by a boolean operation
//! ([`crate::boolean`]) into one Path, made at load. The first child is the
//! start; each next one is combined with what is there so far (`@difference`
//! cuts every later child out of the first). Children are placed by their
//! `at`, `size`, `rotate` and `scale` (static numbers) in the Shape's own
//! units; their looks are not used: the Shape's `fill stroke width` (and the
//! Path fields `trim_start trim_end cap join`) draw the result, so the result
//! animates like any Path.
//!
//! `path_boolean(op, a, b)` does the same on point lists for Draw2D and the
//! paths module: `a` and `b` are a contour `[[x, y], …]` or a list of them.

use super::*;
use crate::boolean::{boolean, BoolOp, Contour};
use crate::frame::Affine;
use crate::geometry;

/// The fields a Shape passes on to the Path it makes.
const LOOK: &[&str] = &["fill", "stroke", "width", "trim_start", "trim_end", "cap", "join"];

fn static_num(cx: &Cx, o: ScriptObject, key: &str, default: f64, ty: &str) -> Result<f64, (String, String)> {
    match obj::opt(cx.vm, o, key).filter(|v| !v.is_nil()) {
        None => Ok(default),
        Some(v) => obj::num(cx.vm, v).ok_or_else(|| (key.to_string(), format!("a {ty} inside a Shape is placed once: `{key}` is a number here, not {} (animate the Shape itself)", kind(cx, v)))),
    }
}

fn static_vec2(cx: &Cx, o: ScriptObject, key: &str, default: [f64; 2], ty: &str) -> Result<[f64; 2], (String, String)> {
    match obj::opt(cx.vm, o, key).filter(|v| !v.is_nil()) {
        None => Ok(default),
        Some(v) => obj::vecn::<2>(cx.vm, v).or_else(|| v.as_number().map(|n| [n, n])).ok_or_else(|| (key.to_string(), format!("a {ty} inside a Shape is placed once: `{key}` is `vec2(x, y)` here, not {} (animate the Shape itself)", kind(cx, v)))),
    }
}

/// One child's outline, in the Shape's units.
fn outline(cx: &Cx, child: ScriptValue, index: usize) -> Result<Vec<Contour>, (String, String)> {
    let field = format!("layers[{index}]");
    let (Some(o), Some(ty)) = (child.as_object(), obj::type_name(cx.vm, child)) else {
        return Err((field, format!("a Shape's `layers` are Rect, Ellipse, Path or Text layers, not {}", kind(cx, child))));
    };
    let at = static_vec2(cx, o, "at", [0.0, 0.0], ty)?;
    let (path, size) = match ty {
        "Rect" => {
            let size = static_vec2(cx, o, "size", [100.0, 100.0], ty)?;
            (geometry::rect_path(size, static_num(cx, o, "radius", 0.0, ty)?), size)
        }
        "Ellipse" => {
            let size = static_vec2(cx, o, "size", [100.0, 100.0], ty)?;
            (geometry::ellipse_path(size), size)
        }
        "Path" => {
            let data = obj::opt(cx.vm, o, "svg").and_then(|v| obj::string(cx.vm, v)).ok_or_else(|| (field.clone(), "a Path inside a Shape needs its outline: `svg: \"M 0 0 L 100 0 L 50 80 Z\"` (a keyed svg cannot be combined)".to_string()))?;
            let path = geometry::parse_path(&data);
            let size = geometry::bounds(&path).map_or([0.0, 0.0], |b| [b[2].max(0.0), b[3].max(0.0)]);
            (path, size)
        }
        "Text" => {
            let text = get_text(cx, o, "text").ok_or_else(|| (field.clone(), "a Text inside a Shape needs `text: \"…\"`".to_string()))?;
            let face = get_face(cx, o, "font", "Inter")?;
            let size = static_num(cx, o, "size", 72.0, ty)?;
            let contours = crate::type_fns::text_outline_contours(&text, face.family, size, face.weight, face.italic, &face.axes)
                .ok_or_else(|| (field.clone(), format!("the font `{}` has no outlines here", face.family)))?;
            let width = crate::type_fns::text_width(&text, face.family, size * 4.0 / 3.0, face.weight, face.italic, &face.axes).map_err(|e| (field.clone(), e))?;
            let shift = match obj::opt(cx.vm, o, "align").and_then(|v| obj::name(cx.vm, v)).as_deref() {
                None | Some("left") => 0.0,
                Some("center") => 0.5,
                Some("right") => 1.0,
                Some(other) => return Err((field, format!("`align` is @left, @center or @right, not `{other}`"))),
            } * width;
            let polys: Vec<Vec<[f64; 2]>> = contours.iter().map(|c| c.iter().map(|p| [p[0] - shift, p[1]]).collect()).collect();
            (geometry::polygons_path(&polys), [width, size * 4.0 / 3.0])
        }
        other => return Err((field, format!("a Shape combines Rect, Ellipse, Path and Text outlines, not a {other}"))),
    };
    // The child's own scale and rotation turn about its anchor (a fraction
    // of its box, the middle by default), as they do when it is drawn.
    let anchor = static_vec2(cx, o, "anchor", [0.5, 0.5], ty)?;
    let scale = static_vec2(cx, o, "scale", [1.0, 1.0], ty)?;
    let rotate = static_num(cx, o, "rotate", 0.0, ty)?;
    let x = static_num(cx, o, "x", 0.0, ty)?;
    let y = static_num(cx, o, "y", 0.0, ty)?;
    let pivot = [size[0] * anchor[0], size[1] * anchor[1]];
    let map = Affine::translate(at[0] + x + pivot[0], at[1] + y + pivot[1])
        .then(&Affine::rotate(rotate))
        .then(&Affine::scale(scale[0], scale[1]))
        .then(&Affine::translate(-pivot[0], -pivot[1]));
    let path = geometry::transformed(&path, &map);
    Ok(geometry::flatten(&path, 0.2).into_iter().map(|(mut c, _)| {
        if c.len() > 1 && c.first() == c.last() {
            c.pop();
        }
        c
    }).filter(|c| c.len() >= 3).collect())
}

/// Closed contours as path data.
pub(crate) fn contours_svg(contours: &[Contour]) -> String {
    let mut out = String::new();
    for c in contours {
        for (i, p) in c.iter().enumerate() {
            out.push_str(&format!("{}{} {} ", if i == 0 { "M" } else { "L" }, round(p[0]), round(p[1])));
        }
        out.push_str("Z ");
    }
    out.trim_end().to_string()
}

fn round(v: f64) -> f64 {
    (v * 1000.0).round() / 1000.0
}

fn op_of(cx: &Cx, o: ScriptObject) -> Result<BoolOp, (String, String)> {
    match obj::opt(cx.vm, o, "op").filter(|v| !v.is_nil()) {
        None => Ok(BoolOp::Union),
        Some(v) => obj::ident(v).and_then(|id| BoolOp::from_id(&id)).ok_or_else(|| ("op".to_string(), format!("`op` is @union, @intersection, @difference or @xor, not {}", kind(cx, v)))),
    }
}

pub(super) fn lower(cx: &mut Cx, o: ScriptObject) -> Made {
    let op = op_of(cx, o)?;
    let children = obj::opt(cx.vm, o, "layers").and_then(|v| obj::items(cx.vm, v)).unwrap_or_default();
    if children.is_empty() {
        return Err(("layers".into(), "a Shape combines its `layers`: `Shape{op: @difference layers: [Rect{size: vec2(200, 200)}, Ellipse{at: vec2(50, 50) size: vec2(100, 100)}]}`".into()));
    }
    let mut result = outline(cx, children[0], 0)?;
    // One child alone is its outline cleaned up (overlaps merged).
    if children.len() == 1 {
        result = boolean(&result, &[], BoolOp::Union);
    }
    for (i, child) in children.iter().enumerate().skip(1) {
        let next = outline(cx, *child, i)?;
        result = boolean(&result, &next, op);
    }
    let mut fields: Vec<(&str, Lit)> = vec![("svg", contours_svg(&result).into())];
    let has = |cx: &Cx, k: &str| obj::opt(cx.vm, o, k).is_some_and(|v| !v.is_nil());
    if !has(cx, "fill") && !has(cx, "stroke") {
        fields.push(("fill", Lit::Color(0xffffffff)));
    }
    let made = layer(cx, "Path", fields);
    if let Some(m) = made.as_object() {
        for k in LOOK {
            if let Some(v) = obj::opt(cx.vm, o, k).filter(|v| !v.is_nil()) {
                obj::set(cx.vm, m, k, v);
            }
        }
    }
    Ok(with_common(cx, o, made, &[]))
}

/// A contour or a list of contours from a Splash value.
fn contours_of(vm: &ScriptVm, v: ScriptValue) -> Option<Vec<Contour>> {
    let items = obj::items(vm, v)?;
    let point = |p: ScriptValue| obj::vecn::<2>(vm, p).or_else(|| obj::items(vm, p).filter(|xy| xy.len() == 2).and_then(|xy| Some([obj::num(vm, xy[0])?, obj::num(vm, xy[1])?])));
    if items.first().is_some_and(|first| point(*first).is_some()) {
        return Some(vec![items.into_iter().map(point).collect::<Option<Vec<_>>>()?]);
    }
    items.into_iter().map(|c| obj::items(vm, c)?.into_iter().map(point).collect::<Option<Vec<_>>>()).collect()
}

/// `path_boolean(op, a, b)` in `mod.motion`.
pub(crate) fn install(vm: &mut ScriptVm, module: ScriptObject) {
    vm.add_method(module, id_lut!(path_boolean), script_args!(op = NIL, a = NIL, b = NIL), |vm, args| {
        let op = obj::ident(obj::get(vm, args, "op")).and_then(|id| BoolOp::from_id(&id));
        let (a, b) = (contours_of(vm, obj::get(vm, args, "a")), contours_of(vm, obj::get(vm, args, "b")));
        let (Some(op), Some(a), Some(b)) = (op, a, b) else {
            return script_err_invalid_args!(vm.trap(), "path_boolean(op, a, b): op is @union, @intersection, @difference or @xor; a and b are [[x, y], …] or lists of them");
        };
        let out = boolean(&a, &b, op);
        let mut contours = Vec::with_capacity(out.len());
        for c in out {
            let points: Vec<ScriptValue> = c.into_iter().map(|[x, y]| obj::array(vm, [x.into(), y.into()])).collect();
            contours.push(obj::array(vm, points));
        }
        obj::array(vm, contours)
    });
}
