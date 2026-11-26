//! `@network`: nodes and links placed by a deterministic force layout
//! (computed once at load, so every frame is seekable), revealed outward
//! from a root node in breadth-first order, links drawing on as the node
//! they lead to arrives.

use super::build::*;
use super::scale::text_box;
use super::{Chart, Land};
use std::f64::consts::PI;

pub(super) fn build(c: &mut Chart) -> Result<(), String> {
    let b = c.b;
    let rows = c.sets[0].rows.clone();
    let n = rows.len();
    c.keys = rows.iter().map(|d| d.label.clone()).collect();
    let mut links: Vec<(usize, usize, f64)> = Vec::new();
    for (a, b, w) in c.links.clone() {
        match (c.index_of(&a), c.index_of(&b)) {
            (Some(a), Some(b)) if a < n && b < n && a != b => links.push((a, b, w)),
            _ => c.warnings.push(("links".into(), format!("the link {a:?} → {b:?} names a node the data does not have"))),
        }
    }
    let mut groups: Vec<String> = Vec::new();
    for d in &rows {
        if let Some(g) = &d.group {
            if !groups.contains(g) {
                groups.push(g.clone());
            }
        }
    }
    let legend: Vec<(String, u32)> = groups.iter().enumerate().map(|(k, g)| (g.clone(), c.th.color(k))).collect();
    c.header(&legend);
    let ls = c.ls;
    let [px0, py0, px1, py1] = c.plot;
    let degree: Vec<usize> = (0..n).map(|i| links.iter().filter(|(a, b, _)| *a == i || *b == i).count()).collect();
    let vmax = rows.iter().map(|d| d.value.abs()).fold(0.0, f64::max);
    let sized_by_value = vmax > 0.0 && rows.iter().any(|d| (d.value - rows[0].value).abs() > 1e-9);
    let dmax = degree.iter().copied().max().unwrap_or(1).max(1) as f64;
    let radius: Vec<f64> = (0..n).map(|i| {
        let f = if sized_by_value { (rows[i].value.abs() / vmax).sqrt() } else { (degree[i] as f64 / dmax).sqrt() };
        ls * 0.4 + f * ls * 1.1
    }).collect();
    let label_size = ls * 0.8;
    let labels = c.labels.unwrap_or(n <= 40);
    let boxes: Vec<[f64; 2]> = rows.iter().map(|d| if labels && !d.label.is_empty() { text_box(&d.label, label_size, 600.0, None, Some(c.w * 0.2), 1.0) } else { [0.0, 0.0] }).collect();
    // Room each node claims: its disc and its label under it.
    let claims: Vec<f64> = (0..n).map(|i| radius[i].max(boxes[i][0] * 0.5) + ls * 0.4).collect();
    let margin_x = claims.iter().copied().fold(0.0, f64::max);
    let margin_bottom = (0..n).map(|i| radius[i] + boxes[i][1] + 6.0).fold(0.0, f64::max);
    let area = [px0 + margin_x, py0 + radius.iter().copied().fold(0.0, f64::max) + if c.notes.is_empty() { 0.0 } else { ls * 3.0 }, px1 - margin_x, py1 - margin_bottom];
    let given: Option<Vec<[f64; 2]>> = if rows.iter().all(|d| d.x.is_some() && d.y.is_some()) { Some(rows.iter().map(|d| [d.x.unwrap(), d.y.unwrap()]).collect()) } else { None };
    // `layout: @layers`: a neural net's columns, one per group in order.
    let pos = if c.layout == "layers" && groups.len() > 1 {
        let cols = groups.len();
        (0..n).map(|i| {
            let g = rows[i].group.as_ref().and_then(|g| groups.iter().position(|x| x == g)).unwrap_or(0);
            let members: Vec<usize> = (0..n).filter(|&j| rows[j].group.as_ref().and_then(|x| groups.iter().position(|y| y == x)).unwrap_or(0) == g).collect();
            let k = members.iter().position(|&j| j == i).unwrap_or(0);
            let x = area[0] + (area[2] - area[0]) * if cols > 1 { g as f64 / (cols - 1) as f64 } else { 0.5 };
            let y = area[1] + (area[3] - area[1]) * (k as f64 + 0.5) / members.len() as f64;
            [x, y]
        }).collect()
    } else {
        layout(n, &links, &claims, area, c.layout == "circle", given)
    };
    // Reveal order: breadth first from the root (the best-connected node).
    let root = c.root.as_ref().and_then(|p| c.index_of(p)).filter(|r| *r < n).unwrap_or_else(|| (0..n).max_by_key(|&i| (degree[i], std::cmp::Reverse(i))).unwrap_or(0));
    let order = bfs(n, &links, root, &degree);
    let mut rank = vec![0; n];
    for (r, &i) in order.iter().enumerate() {
        rank[i] = r;
    }
    let explicit = !matches!(c.land, Land::Auto);
    let land = |c: &Chart, i: usize| if explicit { c.land(i) } else { c.land(rank[i]) };
    let ease = c.ease("ease_out_cubic");
    for (la, lb, w) in &links {
        // Drawn from the node that arrives first to the one that follows.
        let (from, to) = if rank[*la] <= rank[*lb] { (*la, *lb) } else { (*lb, *la) };
        let t = land(c, to);
        let (pa, pb) = (pos[from], pos[to]);
        let lit = c.highlighted(*la) && c.highlighted(*lb);
        let stroke = if lit { c.th.accent } else { alpha(c.th.muted, 0x80) };
        c.out.push(layer(b, "Path", vec![
            ("name", s(b, &format!("link {} {}", c.keys[from], c.keys[to]))),
            ("svg", s(b, &format!("M {:.2} {:.2} L {:.2} {:.2}", pa[0], pa[1], pb[0], pb[1]))), ("stroke", color(stroke)),
            ("width", num((1.2 + w.sqrt() * 1.3).min(6.0))), ("cap", id("round")),
            ("trim_end", tween(b, num(0.0), num(1.0), shift(b, t, -c.grow), shift(b, t, -0.1), ease)),
        ]));
    }
    let mut placed: Vec<[f64; 4]> = Vec::new();
    for i in 0..n {
        let t = land(c, i);
        let r = radius[i];
        let fill = if !c.highlight.is_empty() {
            if c.highlighted(i) { c.th.accent } else { c.th.neutral }
        } else {
            match &rows[i].group {
                Some(g) => c.th.color(groups.iter().position(|x| x == g).unwrap_or(0)),
                None => c.th.color(0),
            }
        };
        c.out.push(layer(b, "Ellipse", vec![
            ("name", s(b, &format!("node {}", c.keys[i]))), ("at", v2(b, pos[i][0] - r, pos[i][1] - r)), ("size", v2(b, 2.0 * r, 2.0 * r)), ("fill", color(fill)),
            ("scale", tween(b, num(0.0), num(1.0), shift(b, t, -0.35), shift(b, t, 0.1), id("ease_out_back"))),
        ]));
        if labels && !rows[i].label.is_empty() {
            // Under the node, or above it, or beside it: the first place no
            // other label or node takes.
            let [bw, bh] = boxes[i];
            let candidates = [
                ([pos[i][0], pos[i][1] + r + 4.0], "center", "top", [pos[i][0] - bw * 0.5, pos[i][1] + r + 4.0]),
                ([pos[i][0], pos[i][1] - r - 4.0], "center", "bottom", [pos[i][0] - bw * 0.5, pos[i][1] - r - 4.0 - bh]),
                ([pos[i][0] + r + 6.0, pos[i][1]], "left", "center", [pos[i][0] + r + 6.0, pos[i][1] - bh * 0.5]),
                ([pos[i][0] - r - 6.0, pos[i][1]], "right", "center", [pos[i][0] - r - 6.0 - bw, pos[i][1] - bh * 0.5]),
            ];
            let free = |b: [f64; 4]| {
                b[0] >= 0.0 && b[0] + b[2] <= c.w && b[1] >= 0.0 && b[1] + b[3] <= c.h
                    && placed.iter().all(|p| b[0] + b[2] <= p[0] || p[0] + p[2] <= b[0] || b[1] + b[3] <= p[1] || p[1] + p[3] <= b[1])
                    && (0..n).filter(|&j| j != i).all(|j| {
                        let (cx, cy) = (pos[j][0].clamp(b[0], b[0] + b[2]), pos[j][1].clamp(b[1], b[1] + b[3]));
                        (cx - pos[j][0]).powi(2) + (cy - pos[j][1]).powi(2) > radius[j].powi(2)
                    })
            };
            let pick = candidates.iter().find(|(_, _, _, tl)| free([tl[0], tl[1], bw, bh])).unwrap_or(&candidates[0]);
            placed.push([pick.3[0], pick.3[1], bw, bh]);
            let label = c.text("label", &rows[i].label, pick.0, pick.1, pick.2, label_size, 600.0, if c.highlighted(i) { c.th.ink } else { c.th.muted }, shift(b, t, 0.1), Some(c.w * 0.2));
            c.out.push(label);
        }
        c.mark(0, i, [pos[i][0], pos[i][1] - r]);
    }
    // Activation waves: pulses run along the links, hop by hop, and each
    // node lights (and stays faintly lit) as a pulse reaches it.
    for wave in c.activate.clone() {
        let mut sources: Vec<usize> = wave.from.iter().filter_map(|p| c.index_of(p)).filter(|i| *i < n).collect();
        if sources.is_empty() {
            sources.push(root);
        }
        let mut depth = vec![usize::MAX; n];
        let mut queue = std::collections::VecDeque::new();
        for &s in &sources {
            depth[s] = 0;
            queue.push_back(s);
        }
        let step = |u: usize| -> Vec<usize> {
            links.iter().filter_map(|(a, b, _)| if wave.backward { (*b == u).then_some(*a) } else { (*a == u).then_some(*b) }).collect()
        };
        while let Some(u) = queue.pop_front() {
            for v in step(u) {
                if depth[v] == usize::MAX {
                    depth[v] = depth[u] + 1;
                    queue.push_back(v);
                }
            }
        }
        let arrive = |i: usize| shift(b, wave.at, wave.hop * depth[i] as f64);
        let pulse = ls * 0.3;
        for (la, lb, _) in &links {
            let (u, v) = if wave.backward { (*lb, *la) } else { (*la, *lb) };
            if depth[u] == usize::MAX || depth[v] != depth[u] + 1 {
                continue;
            }
            let (pu, pv) = (pos[u], pos[v]);
            let (tu, tv) = (arrive(u), arrive(v));
            c.out.push(layer(b, "Path", vec![
                ("name", s(b, "link lit")), ("svg", s(b, &format!("M {:.2} {:.2} L {:.2} {:.2}", pu[0], pu[1], pv[0], pv[1]))), ("stroke", color(c.th.accent)), ("width", num(3.0)), ("cap", id("round")),
                ("opacity", keys(b, vec![(tu.clone(), num(0.0)), (tv.clone(), num(0.9)), (shift(b, tv, 0.8), num(0.3))], id("ease_out_cubic"))),
            ]));
            c.out.push(layer(b, "Ellipse", vec![
                ("name", s(b, "pulse")), ("size", v2(b, 2.0 * pulse, 2.0 * pulse)), ("fill", color(c.th.ink)),
                ("at", tween(b, v2(b, pu[0] - pulse, pu[1] - pulse), v2(b, pv[0] - pulse, pv[1] - pulse), tu.clone(), tv.clone(), id("ease_in_out_sine"))),
                ("opacity", keys(b, vec![(shift(b, tu, -0.01), num(0.0)), (tu.clone(), num(1.0)), (tv.clone(), num(1.0)), (shift(b, tv, 0.05), num(0.0))], id("linear"))),
            ]));
        }
        for i in (0..n).filter(|&i| depth[i] != usize::MAX) {
            let t = arrive(i);
            let r = radius[i];
            c.out.push(layer(b, "Ellipse", vec![
                ("name", s(b, "node lit")), ("at", v2(b, pos[i][0] - r, pos[i][1] - r)), ("size", v2(b, 2.0 * r, 2.0 * r)), ("fill", color(c.th.accent)),
                ("opacity", keys(b, vec![(shift(b, t, -0.01), num(0.0)), (t.clone(), num(1.0)), (shift(b, t, 0.9), num(0.45))], id("ease_out_cubic"))),
            ]));
            c.out.push(layer(b, "Ellipse", vec![
                ("name", s(b, "node ring")), ("at", v2(b, pos[i][0] - r, pos[i][1] - r)), ("size", v2(b, 2.0 * r, 2.0 * r)), ("stroke", color(c.th.accent)), ("width", num(2.5)),
                ("scale", tween(b, num(1.0), num(2.0), t.clone(), shift(b, t, 0.5), id("ease_out_cubic"))),
                ("opacity", keys(b, vec![(shift(b, t, -0.01), num(0.0)), (t.clone(), num(0.9)), (shift(b, t, 0.5), num(0.0))], id("ease_out_cubic"))),
            ]));
        }
    }
    Ok(())
}

/// Breadth-first order from `root`; unreached components follow, each
/// from its best-connected node.
fn bfs(n: usize, links: &[(usize, usize, f64)], root: usize, degree: &[usize]) -> Vec<usize> {
    let mut seen = vec![false; n];
    let mut order = Vec::with_capacity(n);
    let mut next_root = Some(root);
    while let Some(r) = next_root {
        let mut queue = std::collections::VecDeque::from([r]);
        seen[r] = true;
        while let Some(i) = queue.pop_front() {
            order.push(i);
            let mut near: Vec<usize> = links.iter().filter_map(|(a, b, _)| if *a == i { Some(*b) } else if *b == i { Some(*a) } else { None }).filter(|j| !seen[*j]).collect();
            near.sort_by_key(|&j| (std::cmp::Reverse(degree[j]), j));
            near.dedup();
            for j in near {
                if !seen[j] {
                    seen[j] = true;
                    queue.push_back(j);
                }
            }
        }
        next_root = (0..n).filter(|&i| !seen[i]).max_by_key(|&i| (degree[i], std::cmp::Reverse(i)));
    }
    order
}

/// Node centres inside `area` (`[x0, y0, x1, y1]`): given (0..1 of the
/// area), on a circle, or a Fruchterman-Reingold layout started from the
/// circle. No randomness: the same data always lands in the same place.
pub(crate) fn layout(n: usize, links: &[(usize, usize, f64)], claims: &[f64], area: [f64; 4], circle: bool, given: Option<Vec<[f64; 2]>>) -> Vec<[f64; 2]> {
    let (w, h) = ((area[2] - area[0]).max(1.0), (area[3] - area[1]).max(1.0));
    let centre = [(area[0] + area[2]) * 0.5, (area[1] + area[3]) * 0.5];
    if let Some(given) = given {
        return given.iter().map(|p| [area[0] + p[0].clamp(0.0, 1.0) * w, area[1] + p[1].clamp(0.0, 1.0) * h]).collect();
    }
    let mut pos: Vec<[f64; 2]> = (0..n).map(|i| {
        let a = i as f64 / n.max(1) as f64 * 2.0 * PI;
        [centre[0] + w * 0.45 * a.sin(), centre[1] - h * 0.45 * a.cos()]
    }).collect();
    if circle || n < 2 {
        return pos;
    }
    let k = (w * h / n as f64).sqrt() * 0.75;
    let steps = 300;
    for step in 0..steps {
        let temperature = k * 0.5 * (1.0 - step as f64 / steps as f64) + 0.5;
        let mut disp = vec![[0.0f64; 2]; n];
        for i in 0..n {
            for j in (i + 1)..n {
                let d = [pos[i][0] - pos[j][0], pos[i][1] - pos[j][1]];
                let dist = (d[0] * d[0] + d[1] * d[1]).sqrt();
                // Coincident nodes part along a fixed direction per pair.
                let (d, dist) = if dist < 1e-6 { let a = (i * 7 + j * 13) as f64; ([a.sin(), a.cos()], 1.0) } else { (d, dist) };
                // Repulsion grows fast inside the two claims (no overlap).
                let room = claims[i] + claims[j];
                let f = k * k / dist + if dist < room { (room - dist) * 4.0 } else { 0.0 };
                disp[i][0] += d[0] / dist * f;
                disp[i][1] += d[1] / dist * f;
                disp[j][0] -= d[0] / dist * f;
                disp[j][1] -= d[1] / dist * f;
            }
        }
        for (a, b, _) in links {
            let d = [pos[*a][0] - pos[*b][0], pos[*a][1] - pos[*b][1]];
            let dist = (d[0] * d[0] + d[1] * d[1]).sqrt().max(0.01);
            let f = dist * dist / k;
            disp[*a][0] -= d[0] / dist * f;
            disp[*a][1] -= d[1] / dist * f;
            disp[*b][0] += d[0] / dist * f;
            disp[*b][1] += d[1] / dist * f;
        }
        for i in 0..n {
            // A gentle pull to the centre keeps components together.
            disp[i][0] += (centre[0] - pos[i][0]) * 0.02 * k / w.max(h) * 10.0;
            disp[i][1] += (centre[1] - pos[i][1]) * 0.02 * k / w.max(h) * 10.0;
            let len = (disp[i][0] * disp[i][0] + disp[i][1] * disp[i][1]).sqrt().max(1e-9);
            pos[i][0] = (pos[i][0] + disp[i][0] / len * len.min(temperature)).clamp(area[0], area[2]);
            pos[i][1] = (pos[i][1] + disp[i][1] / len * len.min(temperature)).clamp(area[1], area[3]);
        }
    }
    // Fill the area: stretch the layout's box onto it.
    let (lo, hi) = pos.iter().fold(([f64::MAX; 2], [f64::MIN; 2]), |(lo, hi), p| ([lo[0].min(p[0]), lo[1].min(p[1])], [hi[0].max(p[0]), hi[1].max(p[1])]));
    pos.iter().map(|p| {
        let fx = if hi[0] - lo[0] > 1e-6 { (p[0] - lo[0]) / (hi[0] - lo[0]) } else { 0.5 };
        let fy = if hi[1] - lo[1] > 1e-6 { (p[1] - lo[1]) / (hi[1] - lo[1]) } else { 0.5 };
        [area[0] + fx * w, area[1] + fy * h]
    }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_force_layout_is_deterministic_and_inside_its_area() {
        let links: Vec<(usize, usize, f64)> = vec![(0, 1, 1.0), (0, 2, 1.0), (0, 3, 1.0), (3, 4, 1.0), (4, 5, 1.0), (5, 6, 1.0), (6, 3, 1.0), (7, 8, 1.0)];
        let claims = vec![20.0; 9];
        let area = [50.0, 40.0, 850.0, 440.0];
        let a = layout(9, &links, &claims, area, false, None);
        let b = layout(9, &links, &claims, area, false, None);
        assert_eq!(a, b);
        assert!(a.iter().all(|p| p[0] >= area[0] - 1e-9 && p[0] <= area[2] + 1e-9 && p[1] >= area[1] - 1e-9 && p[1] <= area[3] + 1e-9));
        // No two nodes sit on each other.
        for i in 0..9 {
            for j in (i + 1)..9 {
                let d = ((a[i][0] - a[j][0]).powi(2) + (a[i][1] - a[j][1]).powi(2)).sqrt();
                assert!(d > 20.0, "{i} {j} {d}");
            }
        }
        assert_eq!(bfs(9, &links, 0, &[3, 1, 1, 3, 2, 2, 2, 1, 1])[..4], [0, 3, 1, 2]);
    }
}
