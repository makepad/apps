//! `Branches{…}`: a branching growth drawing itself. `style: @tree` is a
//! recursive tree (an L-system's bracketed turtle, seeded: explosive FOOM
//! growth from a spark); `style: @loom` is a tree of continuations growing
//! left to right in smooth curves, with word choices on its branches (the
//! Loom). Every branch is a `Path` drawing on at its depth's time, so the
//! whole is a pure function of the seed and time.

use super::*;

/// One branch: from `a` to `b` (box units), bent through `c`, at `depth`,
/// and which child of its parent it is.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Branch {
    pub a: [f64; 2],
    pub c: [f64; 2],
    pub b: [f64; 2],
    pub depth: usize,
    pub index: usize,
}

/// The shape of a growth.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Growth {
    pub from: [f64; 2],
    /// Degrees, screen space (0 is right, -90 up).
    pub angle: f64,
    pub length: f64,
    pub depth: usize,
    pub spread: f64,
    pub shrink: f64,
    pub seed: u32,
    pub loom: bool,
}

/// Every branch, parents before children (breadth first). A tree node
/// forks in two, sometimes three (seeded); a loom node in two or three
/// fanned vertically, reaching right.
pub(crate) fn grow(g: &Growth) -> Vec<Branch> {
    let mut out = Vec::new();
    // (tip, heading in degrees, length, depth)
    let mut frontier = vec![(g.from, g.angle, g.length, 0usize)];
    let mut serial = 0usize;
    while let Some((tip, heading, len, depth)) = (!frontier.is_empty()).then(|| frontier.remove(0)) {
        if depth >= g.depth || out.len() >= 2000 {
            continue;
        }
        let me = serial;
        let h = |k: u32| hash01(g.seed, me, k);
        let (b, c) = if g.loom {
            // A curve heading right, fanning up or down.
            let b = [tip[0] + len, tip[1] + (heading.to_radians().sin()) * len];
            ([b[0], b[1]], [tip[0] + len * 0.55, tip[1]])
        } else {
            let a = heading.to_radians();
            let b = [tip[0] + a.cos() * len, tip[1] + a.sin() * len];
            // Bent a little to one side, like a twig.
            let bend = (h(1) - 0.5) * len * 0.25;
            let mid = [(tip[0] + b[0]) * 0.5 - a.sin() * bend, (tip[1] + b[1]) * 0.5 + a.cos() * bend];
            (b, mid)
        };
        out.push(Branch { a: tip, c, b, depth, index: me });
        serial += 1;
        let forks = if h(2) < 0.22 { 3 } else { 2 };
        for k in 0..forks {
            let u = if forks == 1 { 0.0 } else { k as f64 / (forks - 1) as f64 * 2.0 - 1.0 };
            let turn = if g.loom {
                // Loom: a vertical fan, closing as it goes.
                u * g.spread * (0.6 + 0.4 * h(3 + k as u32)) / (1.0 + depth as f64 * 0.35)
            } else {
                heading + u * g.spread + (h(3 + k as u32) - 0.5) * g.spread * 0.5
            };
            let next = len * g.shrink * (0.85 + 0.3 * h(7 + k as u32));
            frontier.push((b, turn, next, depth + 1));
        }
    }
    out
}

/// When depth `d` of `depth` starts growing: later generations come
/// faster (explosive growth), all within `grow` seconds.
pub(crate) fn depth_start(d: usize, depth: usize, grow: f64) -> f64 {
    grow * (d as f64 / depth.max(1) as f64).powf(0.7)
}

pub(super) fn lower(cx: &mut Cx, o: ScriptObject) -> Made {
    let [w, h] = get_vec2(cx, o, "size", cx.doc)?;
    let loom = match obj::opt(cx.vm, o, "style").filter(|v| !v.is_nil()) {
        None => false,
        Some(v) => match obj::ident(v).as_deref() {
            Some("tree") => false,
            Some("loom") => true,
            _ => return Err(("style".into(), format!("`style` is @tree or @loom, not {}", kind(cx, v)))),
        },
    };
    let depth = get_num(cx, o, "depth", if loom { 4.0 } else { 7.0 })?.clamp(1.0, 11.0) as usize;
    let g = Growth {
        from: get_vec2(cx, o, "from", if loom { [w * 0.06, h * 0.5] } else { [w * 0.5, h * 0.95] })?,
        angle: get_num(cx, o, "angle", if loom { 0.0 } else { -90.0 })?,
        length: get_num(cx, o, "length", if loom { w * 0.2 } else { h * 0.26 })?,
        depth,
        spread: get_num(cx, o, "spread", if loom { 38.0 } else { 26.0 })?,
        shrink: get_num(cx, o, "shrink", if loom { 0.9 } else { 0.74 })?.clamp(0.2, 1.2),
        seed: get_num(cx, o, "seed", 1.0)? as u32,
        loom,
    };
    let color = get_color(cx, o, "color", 0xff5a1fff)?;
    let width = get_num(cx, o, "width", h * if loom { 0.004 } else { 0.012 })?;
    let start = get_start(cx, o);
    let grow_time = get_num(cx, o, "grow", 1.4)?.max(0.05);
    let labels: Vec<String> = match obj::opt(cx.vm, o, "labels").filter(|v| !v.is_nil()) {
        None => Vec::new(),
        Some(v) if obj::items(cx.vm, v).is_some() => obj::items(cx.vm, v).unwrap_or_default().into_iter().filter_map(|x| obj::string(cx.vm, x)).collect(),
        Some(other) => return Err(("labels".into(), format!("`labels` is a list of words, not {}", kind(cx, other)))),
    };
    let ink = get_color(cx, o, "ink", 0xf0ebe0ff)?;
    let font = get_face(cx, o, "font", "Inter")?;
    let branches = grow(&g);
    let seg = grow_time / depth as f64 * 1.15;
    let mut layers = Vec::with_capacity(branches.len());
    for br in &branches {
        let t0 = depth_start(br.depth, depth, grow_time);
        let bw = (width * 0.78f64.powi(br.depth as i32)).max(0.8);
        let when = at_time(cx, start, t0);
        let trim = tween(cx, 0.0.into(), 1.0.into(), when, seg, if br.depth == 0 { "ease_out_cubic" } else { "ease_out_quad" });
        layers.push(layer(cx, "Path", vec![
            ("svg", format!("M {:.1} {:.1} Q {:.1} {:.1}, {:.1} {:.1}", br.a[0], br.a[1], br.c[0], br.c[1], br.b[0], br.b[1]).into()),
            ("stroke", Lit::Color(color)),
            ("width", bw.into()),
            ("cap", Lit::Id("round")),
            ("trim_end", trim.into()),
        ]));
        // Word choices on the branches (after the root), fading in as each
        // branch completes.
        if !labels.is_empty() && br.depth > 0 {
            let word = &labels[(br.index - 1) % labels.len()];
            let size = h * 0.028 * 0.92f64.powi(br.depth as i32);
            let when = at_time(cx, start, t0 + seg * 0.8);
            let fade = tween(cx, 0.0.into(), 1.0.into(), when, 0.2, "ease_out");
            layers.push(layer(cx, "Text", vec![
                ("text", word.clone().into()),
                ("font", obj::Lit::Font(font.clone())),
                ("size", size.into()),
                ("color", Lit::Color(ink)),
                ("at", v2(br.b[0] + size * 0.4, br.b[1] - size * 0.35)),
                ("opacity", fade.into()),
            ]));
        }
    }
    // A spark at the root.
    let r = width * 1.6;
    let pop = tween(cx, 0.0.into(), 1.0.into(), start, 0.2, "ease_out_back");
    layers.push(layer(cx, "Ellipse", vec![
        ("size", v2(r * 2.0, r * 2.0)),
        ("at", v2(g.from[0] - r, g.from[1] - r)),
        ("fill", Lit::Color(mix_color(color, 0xffffffff, 0.35))),
        ("scale", pop.into()),
    ]));
    Ok(group(cx, o, layers, vec![("size", v2(w, h))]))
}
