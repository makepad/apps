//! Points inside a text's letters (the targets `ParticleText` settles on):
//! the glyph outlines of the document's own text stack (`type_fns`: the
//! outlines `Text{stroke}` strokes and measures use), flattened to
//! polygons and filled by the non-zero rule on a fine grid, then a seeded
//! choice of `count` of them. Pure and deterministic.

/// Non-zero winding of `p` against closed polygons.
fn inside(contours: &[Vec<[f64; 2]>], p: [f64; 2]) -> bool {
    let mut winding = 0i32;
    for c in contours {
        let n = c.len();
        for i in 0..n {
            let (a, b) = (c[i], c[(i + 1) % n]);
            if a[1] <= p[1] {
                if b[1] > p[1] && (b[0] - a[0]) * (p[1] - a[1]) - (p[0] - a[0]) * (b[1] - a[1]) > 0.0 {
                    winding += 1;
                }
            } else if b[1] <= p[1] && (b[0] - a[0]) * (p[1] - a[1]) - (p[0] - a[0]) * (b[1] - a[1]) < 0.0 {
                winding -= 1;
            }
        }
    }
    winding != 0
}

/// `count` points inside `text` set in `font` (a Motion font name or a
/// path) at `size` units, centred on 0,0 (y down), a seeded pick of a fine
/// grid inside the letters (repeated when the letters hold fewer). Lines
/// break at `\n`.
pub fn glyph_points(text: &str, font: &str, weight: f64, italic: bool, size: f64, count: usize, seed: u32) -> Result<Vec<[f64; 2]>, String> {
    crate::font_spec::font_or_error(font)?;
    // A `Text`'s size is in points; the contours come in document units
    // with the em at `size`.
    let contours = crate::type_fns::text_outline_contours(text, font, size / (4.0 / 3.0), weight, italic, &[])
        .ok_or_else(|| format!("`{font}` is not a bundled font (`makepad-motion fonts` lists them)"))?;
    let contours: Vec<Vec<[f64; 2]>> = contours.iter().filter(|c| c.len() >= 3).cloned().collect();
    if contours.is_empty() {
        return Err(format!("`{text}` has no letters to fill in {font}"));
    }
    pick(&contours, count, seed, size * 0.002, 1.0, false).map(|(points, _)| points).ok_or_else(|| format!("`{text}` has no inside to fill in {font}"))
}

/// `count` points of polygons `contours`, centred on 0,0 and scaled by
/// `scale`: inside them (non-zero), or along their outlines with
/// `outline`, a seeded pick of a fine grid (or of even steps along the
/// outline), repeated when there are fewer, with how many distinct
/// candidates there were. `None` when there is nothing.
fn pick(contours: &[Vec<[f64; 2]>], count: usize, seed: u32, min_step: f64, scale: f64, outline: bool) -> Option<(Vec<[f64; 2]>, usize)> {
    let (mut min, mut max) = ([f64::MAX; 2], [f64::MIN; 2]);
    for p in contours.iter().flatten() {
        for i in 0..2 {
            min[i] = min[i].min(p[i]);
            max[i] = max[i].max(p[i]);
        }
    }
    let mut found = Vec::new();
    let mut step;
    if outline {
        // Even steps along every contour, about four per target.
        let length: f64 = contours.iter().map(|c| (0..c.len()).map(|i| dist(c[i], c[(i + 1) % c.len()])).sum::<f64>()).sum();
        step = (length / (count.max(1) as f64 * 4.0)).max(1e-9);
        for c in contours {
            let mut carry = 0.0;
            for i in 0..c.len() {
                let (a, b) = (c[i], c[(i + 1) % c.len()]);
                let l = dist(a, b);
                let mut u = carry;
                while u < l {
                    found.push([a[0] + (b[0] - a[0]) * u / l, a[1] + (b[1] - a[1]) * u / l]);
                    u += step;
                }
                carry = u - l;
            }
        }
    } else {
        // A grid fine enough for about three candidates per target.
        let area = (max[0] - min[0]) * (max[1] - min[1]);
        step = (area / (count.max(1) as f64 * 6.0)).sqrt().max(min_step);
        for _ in 0..3 {
            found.clear();
            let mut y = min[1] + step * 0.5;
            let mut row = 0usize;
            while y < max[1] {
                let mut x = min[0] + step * (0.5 + 0.5 * (row % 2) as f64);
                while x < max[0] {
                    if inside(contours, [x, y]) {
                        found.push([x, y]);
                    }
                    x += step;
                }
                y += step;
                row += 1;
            }
            if found.len() >= count {
                break;
            }
            step *= 0.6;
        }
    }
    if found.is_empty() {
        return None;
    }
    // A seeded pick: order the candidates by a hash of their index.
    let mut order: Vec<(u64, usize)> = (0..found.len()).map(|i| ((crate::crowd::hash01(seed, i, 7) * u64::MAX as f64) as u64, i)).collect();
    order.sort();
    let jitter = step * if outline { 0.15 } else { 0.35 };
    let centre = [(min[0] + max[0]) * 0.5, (min[1] + max[1]) * 0.5];
    let points = (0..count)
        .map(|k| {
            let p = found[order[k % order.len()].1];
            let jx = (crate::crowd::hash01(seed, k, 8) - 0.5) * jitter;
            let jy = (crate::crowd::hash01(seed, k, 9) - 0.5) * jitter;
            [(p[0] + jx - centre[0]) * scale, (p[1] + jy - centre[1]) * scale]
        })
        .collect();
    Some((points, found.len()))
}

fn dist(a: [f64; 2], b: [f64; 2]) -> f64 {
    (b[0] - a[0]).hypot(b[1] - a[1])
}

/// `count` points of SVG path data `d`, fitted (aspect kept) into a `size`
/// square and centred on 0,0: inside its fill, or along its outline with
/// `outline` (and when the path encloses nothing, like a stroke-only line).
pub fn shape_points(d: &str, size: f64, count: usize, seed: u32, outline: bool) -> Result<Vec<[f64; 2]>, String> {
    let mut path = makepad_svg::path::VectorPath::new();
    makepad_svg::path_data::parse_path_data(d, &mut path);
    let mut contours: Vec<Vec<[f64; 2]>> = Vec::new();
    let mut at = [0.0, 0.0];
    for cmd in &path.cmds {
        use makepad_svg::path::PathCmd::*;
        match *cmd {
            MoveTo(x, y) => {
                at = [x as f64, y as f64];
                contours.push(vec![at]);
            }
            LineTo(x, y) => {
                at = [x as f64, y as f64];
                if let Some(c) = contours.last_mut() {
                    c.push(at);
                }
            }
            BezierTo(x1, y1, x2, y2, x, y) => {
                let (p0, p1, p2, p3) = (at, [x1 as f64, y1 as f64], [x2 as f64, y2 as f64], [x as f64, y as f64]);
                if let Some(c) = contours.last_mut() {
                    for k in 1..=10 {
                        let t = k as f64 / 10.0;
                        let u = 1.0 - t;
                        let b = |i: usize| u * u * u * p0[i] + 3.0 * u * u * t * p1[i] + 3.0 * u * t * t * p2[i] + t * t * t * p3[i];
                        c.push([b(0), b(1)]);
                    }
                }
                at = p3;
            }
            Close | Winding(_) => {}
        }
    }
    contours.retain(|c| c.len() >= 2);
    if contours.is_empty() {
        return Err("the shape's path data draws nothing".into());
    }
    let (mut min, mut max) = ([f64::MAX; 2], [f64::MIN; 2]);
    for p in contours.iter().flatten() {
        for i in 0..2 {
            min[i] = min[i].min(p[i]);
            max[i] = max[i].max(p[i]);
        }
    }
    let span = (max[0] - min[0]).max(max[1] - min[1]).max(1e-9);
    let scale = size / span;
    let filled = if outline { None } else { pick(&contours, count, seed, span * 0.002, scale, false) };
    // Too little inside (a line drawing): its outline instead.
    match filled {
        Some((points, distinct)) if distinct >= count / 4 => Ok(points),
        _ => pick(&contours, count, seed, span * 0.002, scale, true).map(|(points, _)| points).ok_or_else(|| "the shape has no outline".into()),
    }
}
