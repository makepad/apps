//! Bundled icons by name (see `icon_data.rs` for where they come from),
//! and SVG markup flattened into one path (for `Path` layers and morphs).

use crate::geometry::{PathCmd, VectorPath};
use makepad_svg::document::{SvgNode, Transform2d};

#[path = "icon_data.rs"]
mod icon_data;
use std::sync::LazyLock;

/// Every icon a document can name, `(name, SVG markup)` sorted by name:
/// the desktop styles' app identities as `<style>/<app>` (the one table in
/// `makepad-app-icons`, which widgets-core draws too) and Motion's own
/// sets (`icon_data.rs`).
pub static ICONS: LazyLock<Vec<(String, &'static str)>> = LazyLock::new(|| {
    let mut all: Vec<(String, &'static str)> = makepad_app_icons::ICONS
        .iter()
        .flat_map(|icon| makepad_app_icons::SETS.iter().zip(icon.variants).map(move |(set, svg)| (format!("{set}/{}", icon.name), svg)))
        .chain(icon_data::BUNDLED.iter().map(|(name, svg)| (name.to_string(), *svg)))
        .collect();
    all.sort_by(|a, b| a.0.cmp(&b.0));
    all
});

/// Sets searched, in order, for a bare name (`"play"`).
const PREFERRED: &[&str] = &["stage", "stage-app", "widgets", "ios", "macos", "android", "windows"];

fn key(name: &str) -> String {
    name.trim().to_lowercase().replace(['_', ' '], "-")
}

/// An icon's markup by `<set>/<name>` (`"stage/play"`), or by its bare name
/// (the first set that has it, Stage's own first). Case, `_`, `-` and
/// spaces compare alike.
pub fn icon(name: &str) -> Option<&'static str> {
    let wanted = key(name);
    let full = |n: &str| key(n) == wanted;
    if let Some((_, svg)) = ICONS.iter().find(|(n, _)| full(n)) {
        return Some(svg);
    }
    let stem = |n: &str| key(n.rsplit('/').next().unwrap_or(n)) == wanted;
    for set in PREFERRED {
        if let Some((_, svg)) = ICONS.iter().find(|(n, _)| n.starts_with(&format!("{set}/")) && stem(n)) {
            return Some(svg);
        }
    }
    ICONS.iter().find(|(n, _)| stem(n)).map(|(_, svg)| *svg)
}

/// The bundled icon names, sorted.
pub fn icon_names() -> impl Iterator<Item = &'static str> {
    ICONS.iter().map(|(name, _)| name.as_str())
}

/// Icon names close to `name`, for a teaching message.
pub fn similar_icons(name: &str, count: usize) -> Vec<&'static str> {
    let wanted = key(name);
    let stem = wanted.rsplit('/').next().unwrap_or(&wanted).to_string();
    let mut scored: Vec<(usize, &'static str)> = ICONS.iter().map(|(n, _)| {
        let s = key(n.rsplit('/').next().unwrap_or(n));
        let score = if s.contains(&stem) || stem.contains(&s) { 0 } else { makepad_script::suggest::levenshtein(&s, &stem) };
        (score, n.as_str())
    }).collect();
    scored.sort();
    scored.into_iter().take(count).map(|(_, n)| n).collect()
}

/// Every shape of an SVG document as one path (transforms applied), fitted
/// into a `size` x `size` box (the viewBox, centred, aspect kept), as path
/// data. What a `Path` layer draws or morphs from an icon.
pub fn svg_path_data(markup: &str, size: f64) -> String {
    let doc = makepad_svg::parse_svg(markup);
    let (w, h) = doc.logical_size();
    let (vx, vy, vw, vh) = match &doc.viewbox {
        Some(vb) => (vb.x, vb.y, vb.width, vb.height),
        None => (0.0, 0.0, w.max(1.0), h.max(1.0)),
    };
    let s = (size as f32) / vw.max(vh).max(1e-3);
    let base = Transform2d::translate(-vx, -vy)
        .then(&Transform2d::scale(s, s))
        .then(&Transform2d::translate((size as f32 - vw * s) * 0.5, (size as f32 - vh * s) * 0.5));
    let mut out = VectorPath::new();
    collect(&doc.root, &base, &mut out);
    path_data(&out)
}

fn collect(nodes: &[SvgNode], parent: &Transform2d, out: &mut VectorPath) {
    for node in nodes {
        let (xf, path) = match node {
            SvgNode::Group(g) => {
                collect(&g.children, &g.transform.then(parent), out);
                continue;
            }
            other => match node_path(other) {
                Some((transform, path)) => (transform.then(parent), path),
                None => continue,
            },
        };
        for cmd in &path.cmds {
            out.cmds.push(match cmd {
                PathCmd::MoveTo(x, y) => { let (x, y) = xf.apply(*x, *y); PathCmd::MoveTo(x, y) }
                PathCmd::LineTo(x, y) => { let (x, y) = xf.apply(*x, *y); PathCmd::LineTo(x, y) }
                PathCmd::BezierTo(a, b, c, d, x, y) => {
                    let (a, b) = xf.apply(*a, *b);
                    let (c, d) = xf.apply(*c, *d);
                    let (x, y) = xf.apply(*x, *y);
                    PathCmd::BezierTo(a, b, c, d, x, y)
                }
                other => other.clone(),
            });
        }
    }
}

/// A shape node's own transform and outline (in its local coordinates).
pub fn node_path(node: &SvgNode) -> Option<(Transform2d, VectorPath)> {
    let mut p = VectorPath::new();
    let xf = match node {
        SvgNode::Path(n) => {
            p = n.path.clone();
            n.transform.clone()
        }
        SvgNode::Rect(n) => {
            if n.rx > 0.0 || n.ry > 0.0 {
                p.rounded_rect(n.x, n.y, n.width, n.height, n.rx.max(n.ry));
            } else {
                p.rect(n.x, n.y, n.width, n.height);
            }
            n.transform.clone()
        }
        SvgNode::Circle(n) => {
            p.circle(n.cx, n.cy, n.r);
            n.transform.clone()
        }
        SvgNode::Ellipse(n) => {
            p.ellipse(n.cx, n.cy, n.rx, n.ry);
            n.transform.clone()
        }
        SvgNode::Line(n) => {
            p.move_to(n.x1, n.y1);
            p.line_to(n.x2, n.y2);
            n.transform.clone()
        }
        SvgNode::Polyline(n) => {
            for (i, (x, y)) in n.points.iter().enumerate() {
                if i == 0 { p.move_to(*x, *y) } else { p.line_to(*x, *y) }
            }
            n.transform.clone()
        }
        SvgNode::Polygon(n) => {
            for (i, (x, y)) in n.points.iter().enumerate() {
                if i == 0 { p.move_to(*x, *y) } else { p.line_to(*x, *y) }
            }
            p.close();
            n.transform.clone()
        }
        SvgNode::Group(_) | SvgNode::Use(_) => return None,
    };
    Some((xf, p))
}

/// Path data of a path, absolute commands, 3 decimals.
pub fn path_data(path: &VectorPath) -> String {
    let mut s = String::new();
    for cmd in &path.cmds {
        match cmd {
            PathCmd::MoveTo(x, y) => s.push_str(&format!("M {x:.3} {y:.3} ")),
            PathCmd::LineTo(x, y) => s.push_str(&format!("L {x:.3} {y:.3} ")),
            PathCmd::BezierTo(a, b, c, d, x, y) => s.push_str(&format!("C {a:.3} {b:.3} {c:.3} {d:.3} {x:.3} {y:.3} ")),
            PathCmd::Close => s.push_str("Z "),
            PathCmd::Winding(_) => {}
        }
    }
    s.trim_end().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn icons_resolve_by_full_or_bare_name_and_flatten_into_a_box() {
        assert!(ICONS.len() > 300, "{}", ICONS.len());
        assert!(ICONS.windows(2).all(|w| w[0].0 < w[1].0), "sorted and unique");
        let (name, _) = ICONS.iter().find(|(n, _)| n.starts_with("stage/")).unwrap();
        assert!(icon(name).is_some());
        let stem = name.rsplit('/').next().unwrap();
        assert_eq!(icon(stem), icon(name), "a bare name finds Stage's own first");
        assert!(icon("no_such_icon_anywhere").is_none());
        assert!(!similar_icons("chevron", 3).is_empty());
        for (name, svg) in ICONS.iter().take(200) {
            let data = svg_path_data(svg, 100.0);
            if data.is_empty() {
                continue;
            }
            let b = crate::geometry::bounds(&crate::geometry::parse_path(&data)).unwrap_or_else(|| panic!("{name}"));
            assert!(b[0] > -60.0 && b[2] < 160.0 && b[1] > -60.0 && b[3] < 160.0, "{name}: {b:?}");
        }
    }
}
