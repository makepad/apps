//! The shapes: Rect, Ellipse, Path, Svg, Vector, read from their components.

use crate::plain::{EllipseProps, PathProps, RectProps};
use super::{check_path_data, points_of, points_path, At, Reader};
use crate::layers::{kind, Ellipse, Field, Path, Rect, Svg, Vector};
use crate::model::*;
use crate::obj::Look;

impl Rect {
    pub(super) fn read(&self, c: &mut Reader, at: &At, path: &str) -> LayerKind {
        let (fill, stroke) = paint(c, at, path, &self.fill, &self.stroke, "Rect");
        LayerKind::Rect(RectProps {
            size: size(c, at, &self.size, "Rect"),
            radius: self.radius.prop(c, at, "radius", 0.0),
            fill,
            stroke,
            width: self.width.prop(c, at, "width", 2.0),
            trim_start: self.trim_start.prop(c, at, "trim_start", 0.0),
            trim_end: self.trim_end.prop(c, at, "trim_end", 1.0),
            cap: self.cap.pick(c, at, "cap", Cap::Butt),
        })
    }
}

impl Ellipse {
    pub(super) fn read(&self, c: &mut Reader, at: &At, path: &str) -> LayerKind {
        let (fill, stroke) = paint(c, at, path, &self.fill, &self.stroke, "Ellipse");
        LayerKind::Ellipse(EllipseProps {
            size: size(c, at, &self.size, "Ellipse"),
            fill,
            stroke,
            width: self.width.prop(c, at, "width", 2.0),
            trim_start: self.trim_start.prop(c, at, "trim_start", 0.0),
            trim_end: self.trim_end.prop(c, at, "trim_end", 1.0),
            cap: self.cap.pick(c, at, "cap", Cap::Butt),
        })
    }
}

/// A shape's `size`, which it needs.
fn size(c: &mut Reader, at: &At, size: &Field<kind::Vec2>, kind: &str) -> Prop<2> {
    if !size.is_set() {
        c.err(at, "size", format!("a {kind} needs its `size`"), Some("`size: vec2(400, 120)`"));
        return Prop::Const([0.0, 0.0]);
    }
    size.prop(c, at, "size", [0.0, 0.0])
}

/// A shape's `fill` and `stroke`: a mask shape without either shows its
/// whole area; any other draws nothing, which is worth a warning.
fn paint(c: &mut Reader, at: &At, path: &str, fill: &Field<kind::Color>, stroke: &Field<kind::Color>, kind: &str) -> (Option<Prop<4>>, Option<Prop<4>>) {
    let mut fill = fill.opt(c, at, "fill");
    let stroke = stroke.opt(c, at, "stroke");
    if fill.is_none() && stroke.is_none() {
        if path.ends_with(".mask") {
            fill = Some(Prop::Const([1.0, 1.0, 1.0, 1.0]));
        } else {
            c.warn(at, "fill", format!("this {kind} has neither `fill` nor `stroke`, so it draws nothing"), Some("`fill: #ffffff` or `stroke: #ffffff width: 4`"));
        }
    }
    (fill, stroke)
}

impl Path {
    pub(super) fn read(&self, c: &mut Reader, at: &At, path: &str) -> LayerKind {
        let data = match (self.svg.look(c), self.points.look(c)) {
            (Some(_), Some(_)) => {
                c.err(at, "points", "a Path has `svg` path data or `points`, not both".into(), None);
                PathShape::Static(Default::default())
            }
            (None, Some(points)) => {
                let closed = self.closed.is_true(c);
                let smooth = self.smooth.is_true(c);
                match points_of(&points) {
                    Some(points) if points.len() >= 2 => PathShape::Static(crate::geometry::parse_path(&points_path(&points, closed, smooth)).into()),
                    _ => {
                        c.err(at, "points", format!("`points` is an array of at least two `vec2(x, y)`, not {}", points.kind()), Some("`points: [vec2(0, 0), vec2(100, 40), vec2(200, 0)] smooth: true`"));
                        PathShape::Static(Default::default())
                    }
                }
            }
            (Some(value @ Look::Obj(..)), None) if value.anim().is_some() => c.morph(at, &value).unwrap_or_else(|| PathShape::Static(Default::default())),
            (Some(value), None) => match c.string(at, "svg", &value) {
                Some(data) => {
                    if let Err(message) = check_path_data(&data) {
                        c.err(at, "svg", message, Some("path data as in SVG's `d`: `M0 0 L400 0 C 450 0 450 80 500 80 Z`"));
                        PathShape::Static(Default::default())
                    } else {
                        PathShape::Static(crate::geometry::parse_path(&data).into())
                    }
                }
                None => PathShape::Static(Default::default()),
            },
            (None, None) => {
                c.err(at, "svg", "a Path needs its `svg` path data or `points`".into(), Some("`svg: \"M0 0 L400 0\"` or `points: [vec2(0, 0), vec2(400, 0)]`"));
                PathShape::Static(Default::default())
            }
        };
        let mut fill = self.fill.opt(c, at, "fill");
        let stroke = self.stroke.opt(c, at, "stroke");
        if fill.is_none() && stroke.is_none() {
            if path.ends_with(".mask") {
                // A mask shape shows its whole area.
                fill = Some(Prop::Const([1.0, 1.0, 1.0, 1.0]));
            } else {
                c.warn(at, "stroke", "this Path has neither `fill` nor `stroke`, so it draws nothing".into(), Some("`stroke: #ffffff width: 6`"));
            }
        }
        LayerKind::Path(PathProps {
            data: data.into(),
            fill,
            stroke,
            width: self.width.prop(c, at, "width", 2.0),
            trim_start: self.trim_start.prop(c, at, "trim_start", 0.0),
            trim_end: self.trim_end.prop(c, at, "trim_end", 1.0),
            cap: self.cap.pick(c, at, "cap", Cap::Round),
            join: self.join.pick(c, at, "join", Join::Round),
        })
    }
}

impl Svg {
    pub(super) fn read(&self, c: &mut Reader, at: &At) -> LayerKind {
        let src = match (self.src.is_set(), self.svg.is_set()) {
            (true, false) => self.src.media(c, at, "src", true),
            (false, true) => match self.svg.text(c, at, "svg") {
                Some(text) if text.trim_start().starts_with('<') => Some(Media::Inline(text.into())),
                Some(_) => {
                    c.err(at, "svg", "`svg` is SVG markup (`<svg …>…</svg>`)".into(), Some("for path data alone use a Path layer"));
                    None
                }
                None => None,
            },
            (true, true) => {
                c.err(at, "svg", "an Svg has `src` (an asset) or `svg` (markup), not both".into(), None);
                None
            }
            (false, false) => {
                c.err(at, "src", "an Svg needs `src: asset(id, rev)` or `svg: \"<svg …>\"`".into(), None);
                None
            }
        };
        LayerKind::Svg { src: src.unwrap_or(Media::Empty), size: self.size.size(c, at, "size") }
    }
}

impl Vector {
    pub(super) fn read(&self, c: &mut Reader, at: &At) -> LayerKind {
        let src = match (self.src.is_set(), self.svg.is_set(), self.icon.is_set()) {
            (true, false, false) => self.src.media(c, at, "src", true),
            (false, true, false) => match self.svg.text(c, at, "svg") {
                Some(text) if text.trim_start().starts_with('<') => Some(Media::Inline(text.into())),
                _ => {
                    c.err(at, "svg", "`svg` is SVG markup (`<svg …>…</svg>`); `icon(\"name\")` gives a bundled icon's".into(), None);
                    None
                }
            },
            (false, false, true) => {
                let name = self.icon.text(c, at, "icon").unwrap_or_default();
                match crate::icons::icon(&name) {
                    Some(svg) => Some(Media::Inline(svg.into())),
                    None => {
                        let near = crate::icons::similar_icons(&name, 5).join(", ");
                        c.err(at, "icon", format!("no bundled icon \"{name}\""), Some(&format!("closest: {near}; `makepad-motion icons` lists all {}", crate::icons::ICONS.len())));
                        None
                    }
                }
            }
            (false, false, false) => {
                c.err(at, "src", "a Vector needs `svg: \"<svg…>\"`, `icon: \"stage/play\"` or `src: asset(id, rev)`".into(), None);
                None
            }
            _ => {
                c.err(at, "src", "a Vector has one of `svg`, `icon` or `src`".into(), None);
                None
            }
        };
        // Its own size by default (the markup's, when it is known here).
        let size = self.size.size(c, at, "size").or_else(|| match &src {
            Some(Media::Inline(markup)) => {
                let (w, h) = makepad_svg::parse_svg(markup).logical_size();
                (w > 0.0 && h > 0.0).then(|| Prop::Const([w as f64, h as f64]))
            }
            _ => None,
        });
        let mut parts = Vec::new();
        match self.parts.look(c) {
            None => {}
            Some(Look::Obj(map, _)) => {
                for (id, value) in c.fields(map) {
                    if id.starts_with("__") {
                        continue;
                    }
                    let Look::Obj(part, _) = value else {
                        c.err(at, "parts", format!("part \"{id}\" is {}; write `{{x: … opacity: …}}`", value.kind()), None);
                        continue;
                    };
                    let pat = c.at(part, format!("{}.{id}", at.field("parts")), at.loc);
                    c.unknown_fields(&pat, &["x", "y", "scale", "rotate", "opacity", "fill", "stroke", "width", "draw_on"], "part");
                    parts.push((id.clone(), VectorPart {
                        opacity: c.prop::<1>(&pat, "opacity", [1.0]),
                        x: c.prop::<1>(&pat, "x", [0.0]),
                        y: c.prop::<1>(&pat, "y", [0.0]),
                        scale: c.prop::<2>(&pat, "scale", [1.0, 1.0]),
                        rotate: c.prop::<1>(&pat, "rotate", [0.0]),
                        fill: c.opt_prop::<4>(&pat, "fill"),
                        stroke: c.opt_prop::<4>(&pat, "stroke"),
                        width: c.opt_prop::<1>(&pat, "width"),
                        draw_on: c.opt_prop::<1>(&pat, "draw_on"),
                    }));
                }
            }
            Some(other) => c.err(at, "parts", format!("`parts` maps element ids to offsets (`{{\"wheel\": {{rotate: …}}}}`), not {}", other.kind()), None),
        }
        LayerKind::Vector {
            src: src.unwrap_or(Media::Empty),
            size,
            parts,
            draw_on: self.draw_on.opt(c, at, "draw_on"),
            stagger: self.stagger.konst(c, at, "stagger", 0.6).clamp(0.0, 0.99),
            trace: self.trace.konst(c, at, "trace", 1.5).max(0.0),
            fill: self.fill.opt(c, at, "fill"),
            stroke: self.stroke.opt(c, at, "stroke"),
        }
    }
}
