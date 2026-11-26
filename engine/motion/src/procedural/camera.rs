//! World-space cameras and magnified details, lowered to ordinary groups.

use super::anim::{anim_components, components, keyed, sample_times, window};
use super::*;
use crate::frame::{Affine, Local};
use crate::plotting::{self, Bounds, Point};

type ResultOf<T> = Result<T, (String, String)>;

fn bad(field: &str, message: impl Into<String>) -> (String, String) {
    (field.into(), message.into())
}

fn given(cx: &Cx, o: ScriptObject, field: &str) -> Option<ScriptValue> {
    obj::opt(cx.vm, o, field).filter(|v| !v.is_nil())
}

fn negative_literal<const N: usize>(cx: &mut Cx, value: ScriptValue) -> Option<ScriptValue> {
    let values = components::<N>(cx, value).filter(|v| v.iter().all(|n| n.is_finite()))?;
    Some(if N == 1 { (-values[0]).into() } else { obj::array(cx.vm, values.map(|n| (-n).into())) })
}

/// Copy tracks, never mutate an author's shared Tween/Keys. Times, easing and
/// extrapolation survive exactly; scalar Bezier handles negate value, not time.
fn negative_track<const N: usize>(cx: &mut Cx, value: ScriptValue) -> Option<ScriptValue> {
    if let Some(v) = negative_literal::<N>(cx, value) { return Some(v); }
    let o = value.as_object()?;
    match obj::type_name(cx.vm, value) {
        Some("Tween") => {
            let (a, b) = (obj::get(cx.vm, o, "from"), obj::get(cx.vm, o, "to"));
            let a = negative_literal::<N>(cx, a)?;
            let b = negative_literal::<N>(cx, b)?;
            let copy = obj::copy_without(cx.vm, o, &[]);
            obj::set(cx.vm, copy, "from", a);
            obj::set(cx.vm, copy, "to", b);
            Some(copy.into())
        }
        Some("Keys") => {
            let points = obj::items(cx.vm, obj::get(cx.vm, o, "points"))?;
            let mut out = Vec::new();
            for (i, point) in points.iter().copied().enumerate() {
                if i + 1 == points.len() && obj::record_of(cx.vm, point).is_some() {
                    out.push(point); // The immutable before/after record.
                    continue;
                }
                let mut p = obj::items(cx.vm, point).filter(|p| p.len() == 2 || p.len() == 3)?;
                p[1] = negative_literal::<N>(cx, p[1])?;
                if let Some(rec) = p.get(2).and_then(|v| obj::record_of(cx.vm, *v)) {
                    let copy = obj::copy_without(cx.vm, rec, &[]);
                    for side in ["in", "out"] {
                        if let Some(handle) = given(cx, rec, side) {
                            let [dt, dv] = components::<2>(cx, handle)?;
                            let handle = obj::v2(cx.vm, dt, -dv);
                            obj::set(cx.vm, copy, side, handle);
                        }
                    }
                    p[2] = copy.into();
                }
                out.push(obj::array(cx.vm, p));
            }
            let points = obj::array(cx.vm, out);
            let copy = obj::copy_without(cx.vm, o, &[]);
            obj::set(cx.vm, copy, "points", points);
            Some(copy.into())
        }
        _ => None,
    }
}

fn negative<const N: usize>(cx: &mut Cx, value: ScriptValue, field: &str) -> ResultOf<ScriptValue> {
    if let Some(value) = negative_track::<N>(cx, value) { return Ok(value); }
    let mut samples = Vec::new();
    for t in sample_times(Some([0.0, cx.duration]), 30.0) {
        let v = anim_components::<N>(cx, value, t).map_err(|(_, m)| bad(field, m))?;
        let value = if N == 1 { (-v[0]).into() } else { obj::array(cx.vm, v.map(|n| (-n).into())) };
        samples.push((t, Lit::Value(value)));
    }
    Ok(keyed(cx, samples, true))
}

fn origin_group(cx: &mut Cx, children: Vec<ScriptValue>, mut fields: Vec<(&str, Lit)>) -> ScriptValue {
    // Local::affine multiplies anchor by size; eval uses [0, 0] for a Group
    // without size, not its children's bounds. Explicit zero anchor also keeps
    // the pivot at the origin when a sized viewport/mask is added to a group.
    fields.push(("anchor", v2(0.0, 0.0)));
    let children = array(cx, children);
    fields.push(("layers", children.into()));
    layer(cx, "Group", fields)
}

pub(super) fn camera(cx: &mut Cx, o: ScriptObject) -> Made {
    // Camera also serves as an open comp/scene record. Check the 2D vocabulary
    // here, while record uses retain their own readers and fields.
    let allowed = crate::layers::fields("Camera");
    for (field, _, _) in obj::fields(cx.vm, o) {
        if !field.starts_with("__") && !allowed.contains(&field) { return Err(bad(&field, format!("Camera layer has no `{field}`; use center, zoom, roll, screen and layers (plus common layer fields)"))); }
    }
    let center = match given(cx, o, "center") {
        Some(v) => v,
        None => obj::v2(cx.vm, cx.doc[0] * 0.5, cx.doc[1] * 0.5),
    };
    let roll = given(cx, o, "roll").unwrap_or(0.0.into());
    let center = negative::<2>(cx, center, "center")?;
    let roll = negative::<1>(cx, roll, "roll")?;
    let zoom = given(cx, o, "zoom").unwrap_or(1.0.into());
    let screen = match given(cx, o, "screen") {
        Some(v) => v,
        None => obj::v2(cx.vm, cx.doc[0] * 0.5, cx.doc[1] * 0.5),
    };
    let children = match given(cx, o, "layers") {
        Some(v) => obj::items(cx.vm, v).ok_or_else(|| bad("layers", "Camera `layers` is a list of world-space layers: layers: [Rect{…}]"))?,
        None => Vec::new(),
    };
    let world = origin_group(cx, children, vec![("at", center.into())]);
    Ok(group(cx, o, vec![world], vec![
        ("at", screen.into()), ("scale", zoom.into()), ("rotate", roll.into()),
        ("anchor", v2(0.0, 0.0)), ("x", 0.0.into()), ("y", 0.0.into()),
    ]))
}

#[derive(Clone)]
struct Located {
    object: ScriptObject,
    ancestors: Vec<ScriptObject>,
}

/// Traverse layer-bearing fields only: a style record's name is not a layer.
fn locations(cx: &Cx, root: ScriptObject) -> Vec<Located> {
    fn visit(cx: &Cx, value: ScriptValue, parents: &mut Vec<ScriptObject>, out: &mut Vec<Located>) {
        let Some(o) = value.as_object() else { return };
        if parents.contains(&o) || parents.len() > 128 || obj::is_fn(cx.vm, value) { return; }
        let document = obj::is(cx.vm, value, "Motion");
        if !document {
            out.push(Located { object: o, ancestors: parents.clone() });
            parents.push(o);
        }
        for field in ["layers", "layer", "mask"] {
            let v = obj::get(cx.vm, o, field);
            if let Some(items) = obj::items(cx.vm, v) {
                for item in items { visit(cx, item, parents, out); }
            } else if let Some(map) = obj::record_of(cx.vm, v) {
                for (_, v, _) in obj::fields(cx.vm, map) { visit(cx, v, parents, out); }
            } else { visit(cx, v, parents, out); }
        }
        if !document { parents.pop(); }
    }
    let mut out = Vec::new();
    visit(cx, root.into(), &mut Vec::new(), &mut out);
    out
}

/// Arrays and object fields are independent, including all descendant layers
/// and animation tracks. Functions retain their immutable code and captures.
fn deep_copy(vm: &mut ScriptVm, value: ScriptValue, copies: &mut Vec<(ScriptValue, ScriptValue)>) -> ScriptValue {
    if let Some((_, copy)) = copies.iter().find(|(v, _)| *v == value) { return *copy; }
    if value.as_array().is_some() || (value.as_object().is_some() && obj::items(vm, value).is_some()) {
        let out = vm.bx.heap_mut().new_array();
        copies.push((value, out.into()));
        for v in obj::items(vm, value).unwrap_or_default() {
            let v = deep_copy(vm, v, copies);
            vm.bx.heap_mut().array_push(out, v, NoTrap);
        }
        return out.into();
    }
    let Some(o) = value.as_object().filter(|_| !obj::is_fn(vm, value)) else { return value };
    let out = obj::copy_without(vm, o, &[]);
    copies.push((value, out.into()));
    for (key, v, _) in obj::fields(vm, out) {
        if v.is_nil() { continue; }
        let copy = deep_copy(vm, v, copies);
        if v != copy { obj::set(vm, out, &key, copy); }
    }
    out.into()
}

#[derive(Clone, Copy)]
struct Placement {
    at: Point,
    scale: Point,
    rotate: f64,
    anchor: Point,
    size: Point,
    span: Option<ScriptValue>,
}

impl Placement {
    fn identity() -> Self { Self { at: [0.0; 2], scale: [1.0; 2], rotate: 0.0, anchor: [0.0; 2], size: [0.0; 2], span: None } }
    fn affine(self) -> Affine {
        Affine::translate(self.at[0], self.at[1]).then(&Local { anchor: self.anchor, scale: self.scale, rotate: self.rotate }.affine(self.size))
    }
    fn wrap(self, cx: &mut Cx, picture: ScriptValue) -> ScriptValue {
        let children = array(cx, vec![picture]);
        let mut fields = vec![
            ("at", v2(self.at[0], self.at[1])), ("scale", v2(self.scale[0], self.scale[1])),
            ("rotate", self.rotate.into()), ("anchor", v2(self.anchor[0], self.anchor[1])),
            ("size", v2(self.size[0], self.size[1])), ("layers", children.into()),
        ];
        // The child's animation uses its original ancestors' local clocks.
        if let Some(span) = self.span { fields.push(("span", span.into())); }
        layer(cx, "Group", fields)
    }
    fn inverse_wrap(self, cx: &mut Cx, picture: ScriptValue) -> ResultOf<ScriptValue> {
        if self.scale.contains(&0.0) { return Err(bad("of", "Zoomed needs invertible ancestor scales; zoom a top-level layer")); }
        let pivot = [self.anchor[0] * self.size[0], self.anchor[1] * self.size[1]];
        let translated = origin_group(cx, vec![picture], vec![("at", v2(-pivot[0] - self.at[0], -pivot[1] - self.at[1]))]);
        let rotated = origin_group(cx, vec![translated], vec![("rotate", (-self.rotate).into())]);
        Ok(origin_group(cx, vec![rotated], vec![("at", v2(pivot[0], pivot[1])), ("scale", v2(1.0 / self.scale[0], 1.0 / self.scale[1]))]))
    }
}

fn static_error(cx: &Cx, o: ScriptObject, field: &str) -> (String, String) {
    let name = get_text(cx, o, "name").unwrap_or_else(|| obj::type_name(cx.vm, o.into()).unwrap_or("ancestor").into());
    bad("of", format!("Zoomed cannot freeze ancestor `{name}`'s animated or non-numeric `{field}`; zoom a top-level layer (name the whole moving group), or use static ancestor transforms"))
}

fn static_vec(cx: &Cx, o: ScriptObject, field: &str, default: Point, scalar: bool) -> ResultOf<Point> {
    let Some(v) = given(cx, o, field) else { return Ok(default) };
    components::<2>(cx, v).or_else(|| if scalar { obj::num(cx.vm, v).map(|n| [n; 2]) } else { None })
        .filter(|p| p.iter().all(|n| n.is_finite())).ok_or_else(|| static_error(cx, o, field))
}

fn static_num(cx: &Cx, o: ScriptObject, field: &str, default: f64) -> ResultOf<f64> {
    given(cx, o, field).map_or(Ok(default), |v| obj::num(cx.vm, v).filter(|n| n.is_finite()).ok_or_else(|| static_error(cx, o, field)))
}

fn placements(cx: &Cx, ancestors: &[ScriptObject]) -> ResultOf<Vec<Placement>> {
    let mut out = Vec::new();
    for &o in ancestors {
        let ty = obj::type_name(cx.vm, o.into());
        if ty == Some("Camera") {
            let center = static_vec(cx, o, "center", [cx.doc[0] * 0.5, cx.doc[1] * 0.5], false)?;
            out.push(Placement { at: static_vec(cx, o, "screen", [cx.doc[0] * 0.5, cx.doc[1] * 0.5], false)?,
                scale: [static_num(cx, o, "zoom", 1.0)?; 2], rotate: -static_num(cx, o, "roll", 0.0)?, span: given(cx, o, "span"), ..Placement::identity() });
            out.push(Placement { at: [-center[0], -center[1]], ..Placement::identity() });
            continue;
        }
        if !matches!(ty, Some("Group" | "Plot")) {
            return Err(bad("of", "Zoomed reads static Group/Plot/Camera ancestors; zoom a top-level layer for repeated, projected or procedural placement"));
        }
        let plot = ty == Some("Plot");
        let size = static_vec(cx, o, "size", if plot { [cx.doc[0] * 0.8, cx.doc[1] * 0.8] } else { [0.0; 2] }, false)?;
        let mut at = static_vec(cx, o, "at", if plot { [(cx.doc[0] - size[0]) * 0.5, (cx.doc[1] - size[1]) * 0.5] } else { [0.0; 2] }, false)?;
        if !plot { at[0] += static_num(cx, o, "x", 0.0)?; at[1] += static_num(cx, o, "y", 0.0)?; }
        out.push(Placement { at, size, scale: static_vec(cx, o, "scale", [1.0; 2], true)?,
            rotate: static_num(cx, o, "rotate", 0.0)?, anchor: static_vec(cx, o, "anchor", [0.5; 2], false)?, span: given(cx, o, "span") });
    }
    Ok(out)
}

fn check_references(cx: &Cx, source: ScriptObject, all: &[Located], active: &mut Vec<ScriptObject>) -> ResultOf<()> {
    if active.contains(&source) { return Err(bad("of", "Zoomed has a circular `of` reference; name a picture layer that does not contain or refer back to this inset")); }
    active.push(source);
    for node in locations(cx, source) {
        if obj::is(cx.vm, node.object.into(), "Zoomed") {
            if let Some(name) = get_text(cx, node.object, "of") {
                if let Some(next) = all.iter().find(|n| get_text(cx, n.object, "name").as_deref() == Some(&name)) {
                    check_references(cx, next.object, all, active)?;
                }
            }
        }
    }
    active.pop();
    Ok(())
}

fn bounds(at: Point, size: Point) -> Bounds {
    Bounds { x: [at[0], at[0] + size[0]], y: [at[1], at[1] + size[1]] }
}

fn corners(b: Bounds) -> [Point; 4] {
    [[b.x[0], b.y[0]], [b.x[1], b.y[0]], [b.x[1], b.y[1]], [b.x[0], b.y[1]]]
}

fn crosses(a: [Point; 2], b: [Point; 2]) -> bool {
    fn side(a: Point, b: Point, p: Point) -> f64 { (b[0] - a[0]) * (p[1] - a[1]) - (b[1] - a[1]) * (p[0] - a[0]) }
    let overlap = (0..2).all(|i| a[0][i].min(a[1][i]) <= b[0][i].max(b[1][i]) && b[0][i].min(b[1][i]) <= a[0][i].max(a[1][i]));
    overlap && side(a[0], a[1], b[0]) * side(a[0], a[1], b[1]) <= 0.0 && side(b[0], b[1], a[0]) * side(b[0], b[1], a[1]) <= 0.0
}

/// Minimise the two lengths over distinct corner pairs, excluding segments
/// through either box and crossing pairs. Overlapping boxes have no connectors.
fn connectors(source: Bounds, inset: Bounds) -> [[Point; 2]; 2] {
    let a = corners(source);
    let b = corners(inset);
    let empty = [[a[0]; 2]; 2];
    if source.x[0] < inset.x[1] && inset.x[0] < source.x[1] && source.y[0] < inset.y[1] && inset.y[0] < source.y[1] { return empty; }
    let inside = |p: Point, q: Point, box_: Bounds| {
        let eps = ((box_.x[1] - box_.x[0]).min(box_.y[1] - box_.y[0]) * 1e-6).min(1e-5);
        let inner = Bounds { x: [box_.x[0] + eps, box_.x[1] - eps], y: [box_.y[0] + eps, box_.y[1] - eps] };
        plotting::clip_segment(p, q, inner).is_some()
    };
    let mut candidates = Vec::new();
    for (i, &p) in a.iter().enumerate() {
        for (j, &q) in b.iter().enumerate() {
            if !inside(p, q, source) && !inside(p, q, inset) {
                candidates.push((i, j, [p, q], (p[0] - q[0]).hypot(p[1] - q[1])));
            }
        }
    }
    let mut best = (f64::INFINITY, empty);
    for (i, x) in candidates.iter().enumerate() {
        for y in candidates.iter().skip(i + 1) {
            if x.0 == y.0 || x.1 == y.1 || crosses(x.2, y.2) { continue; }
            let length = x.3 + y.3;
            if length < best.0 { best = (length, [x.2, y.2]); }
        }
    }
    best.1
}

fn connector_svg(lines: [[Point; 2]; 2]) -> String {
    // Two subpaths even when connectors collapse: keys retain correspondence.
    lines.into_iter().map(|[a, b]| format!("M {} {} L {} {}", a[0], a[1], b[0], b[1])).collect::<Vec<_>>().join(" ")
}

pub(super) fn zoomed(cx: &mut Cx, o: ScriptObject, root: Option<ScriptObject>, copies: &mut Vec<ScriptObject>) -> Made {
    let name = get_text(cx, o, "of").ok_or_else(|| bad("of", "Zoomed needs a named picture: of: \"plot\" (put name: \"plot\" on its layer)"))?;
    let root = root.ok_or_else(|| bad("of", "Zoomed needs a Motion document to find its named layer"))?;
    let located = locations(cx, root);
    let own = located.iter().find(|n| n.object == o).map(|n| n.ancestors.clone()).unwrap_or_default();
    let all: Vec<_> = located.into_iter().filter(|n| !copies.contains(&n.object) && !n.ancestors.iter().any(|a| copies.contains(a))).collect();
    let found = all.iter().find(|n| get_text(cx, n.object, "name").as_deref() == Some(&name)).cloned().ok_or_else(|| {
        let mut names: Vec<_> = all.iter().filter_map(|n| get_text(cx, n.object, "name")).collect();
        names.sort(); names.dedup();
        bad("of", format!("Zoomed `of: {name:?}` names no layer; known names: {}. Set `name` on the picture to magnify, then use that name in `of`", if names.is_empty() { "none".into() } else { names.join(", ") }))
    })?;
    check_references(cx, found.object, &all, &mut Vec::new())?;
    let ancestors = placements(cx, &found.ancestors)?;
    let own_ancestors = placements(cx, &own)?;
    let mut picture = deep_copy(cx.vm, found.object.into(), &mut Vec::new());
    // A forward reference to a data item needs its Plot's coordinate conversion.
    // Lower an independent Plot copy, leaving the original for the normal pass.
    if super::plot::ITEMS.contains(&obj::type_name(cx.vm, picture).unwrap_or("")) {
        let plot = found.ancestors.last().copied().filter(|p| obj::is(cx.vm, (*p).into(), "Plot"))
            .ok_or_else(|| bad("of", "Zoomed's named plot item must be inside a Plot"))?;
        let copy = deep_copy(cx.vm, plot.into(), &mut Vec::new());
        let lowered = super::plot::lower(cx, copy.as_object().unwrap())?;
        let items = locations(cx, lowered.as_object().unwrap());
        picture = items.into_iter().find(|n| get_text(cx, n.object, "name").as_deref() == Some(&name))
            .ok_or_else(|| bad("of", "the named Plot item did not produce a picture; zoom the Plot itself"))?.object.into();
    }
    if let Some(picture) = picture.as_object() { copies.push(picture); }
    // The wrappers compose the full accumulated static transform, including
    // size-dependent pivots and non-uniform scales; the copy retains its own at.
    for placement in ancestors.into_iter().rev() {
        if placement.affine() != Affine::IDENTITY || placement.span.is_some() { picture = placement.wrap(cx, picture); }
    }

    let (region_at, region_size) = if let Some(region) = given(cx, o, "region") {
        if given(cx, o, "region_at").is_some() || given(cx, o, "region_size").is_some() {
            return Err(bad("region", "use either region: [x, y, w, h] or region_at plus region_size"));
        }
        let [x, y, w, h] = components::<4>(cx, region).ok_or_else(|| bad("region", "`region` is a static [x, y, w, h]; animate region_at: vec2(x, y) and region_size: vec2(w, h) with tween/keys/fn(t)"))?;
        (obj::v2(cx.vm, x, y), obj::v2(cx.vm, w, h))
    } else {
        (given(cx, o, "region_at").ok_or_else(|| bad("region", "give region: [x, y, w, h], or both region_at and region_size"))?,
            given(cx, o, "region_size").ok_or_else(|| bad("region_size", "region_at also needs region_size: vec2(width, height)"))?)
    };
    let at = get_vec2(cx, o, "at", [cx.doc[0] * 0.65, cx.doc[1] * 0.15])?;
    let size = get_vec2(cx, o, "size", [cx.doc[0] * 0.3, cx.doc[1] * 0.3])?;
    if !at.iter().chain(size.iter()).all(|n| n.is_finite()) || size.iter().any(|n| *n <= 0.0) {
        return Err(bad("size", "Zoomed needs a finite at and a positive size: size: vec2(480, 300)"));
    }
    let color = get_color(cx, o, "frame", 0xf2efe6ff)?;
    let background = get_color(cx, o, "background", 0x10141bff)?;
    let width = get_num(cx, o, "width", 2.0)?;
    let radius = get_num(cx, o, "radius", 0.0)?;
    if !width.is_finite() || !radius.is_finite() || width < 0.0 || radius < 0.0 { return Err(bad("width", "Zoomed width and radius must be finite and non-negative")); }
    let connect = get_bool(cx, o, "connect", true);
    let at_window = window(cx, region_at, cx.duration).map_err(|(_, m)| bad("region_at", m))?;
    let size_window = window(cx, region_size, cx.duration).map_err(|(_, m)| bad("region_size", m))?;
    let animated = at_window.is_some() || size_window.is_some();
    let mut places = Vec::new();
    let mut sizes = Vec::new();
    let mut centers = Vec::new();
    let mut scales = Vec::new();
    let mut lines = Vec::new();
    for t in sample_times(animated.then_some([0.0, cx.duration]), 30.0) {
        let p = anim_components::<2>(cx, region_at, t).map_err(|(_, m)| bad("region_at", m))?;
        let s = anim_components::<2>(cx, region_size, t).map_err(|(_, m)| bad("region_size", m))?;
        if s.iter().any(|n| *n <= 0.0) { return Err(bad("region_size", format!("Zoomed region width and height must stay positive (at {t} s); use region_size: vec2(200, 120)"))); }
        let scale = (size[0] / s[0]).min(size[1] / s[1]);
        let center = [-p[0] - s[0] * 0.5, -p[1] - s[1] * 0.5];
        if !scale.is_finite() || !center.iter().all(|n| n.is_finite()) { return Err(bad("region", "Zoomed region produces a non-finite transform; use finite coordinates and positive dimensions")); }
        places.push((t, v2(p[0], p[1])));
        sizes.push((t, v2(s[0], s[1])));
        centers.push((t, v2(center[0], center[1])));
        scales.push((t, scale.into()));
        if connect { lines.push((t, connector_svg(connectors(bounds(p, s), bounds(at, size))).into())); }
    }
    let place = keyed(cx, places, animated);
    let region_size = keyed(cx, sizes, animated);
    let center = keyed(cx, centers, animated);
    let scale = keyed(cx, scales, animated);
    let mut children = Vec::new();
    if connect {
        let svg = keyed(cx, lines, animated);
        children.push(layer(cx, "Path", vec![("name", "zoom connectors".into()), ("svg", svg.into()), ("stroke", Lit::Color(color)), ("width", width.into())]));
    }
    children.push(layer(cx, "Rect", vec![
        ("name", "zoom region".into()), ("at", place.into()), ("size", region_size.into()), ("stroke", Lit::Color(color)), ("width", width.into()),
    ]));
    children.push(layer(cx, "Rect", vec![
        ("name", "zoom background".into()), ("at", v2(at[0], at[1])), ("size", v2(size[0], size[1])), ("radius", radius.into()), ("fill", Lit::Color(background)),
    ]));
    let shifted = origin_group(cx, vec![picture], vec![("at", center.into())]);
    let scaled = origin_group(cx, vec![shifted], vec![("at", v2(size[0] * 0.5, size[1] * 0.5)), ("scale", scale.into())]);
    let mask = layer(cx, "Rect", vec![("size", v2(size[0], size[1])), ("radius", radius.into()), ("fill", Lit::Color(0xffffffff))]);
    children.push(origin_group(cx, vec![scaled], vec![
        ("name", "zoom clip".into()), ("at", v2(at[0], at[1])), ("size", v2(size[0], size[1])), ("mask", mask.into()),
    ]));
    children.push(layer(cx, "Rect", vec![
        ("name", "zoom border".into()), ("at", v2(at[0], at[1])), ("size", v2(size[0], size[1])), ("radius", radius.into()), ("stroke", Lit::Color(color)), ("width", width.into()),
    ]));
    // `at` belongs to the inset box, not the wrapper: source outline and
    // connectors remain in document coordinates. Common timing/effects stay here.
    let mut made = group(cx, o, children, vec![("at", v2(0.0, 0.0)), ("anchor", v2(0.0, 0.0))]);
    // Even a nested Zoomed's region and inset box are in document coordinates.
    for p in own_ancestors {
        if p.affine() != Affine::IDENTITY { made = p.inverse_wrap(cx, made)?; }
    }
    Ok(made)
}
