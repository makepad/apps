//! `@treemap`: a squarified tiling, tiles scaling in largest first, their
//! labels and values inside when they fit, re-tiling between datasets.

use super::build::*;
use makepad_script::*;
use super::scale::text_width;
use super::Chart;

pub(super) fn build(c: &mut Chart) -> Result<(), String> {
    let b = c.b;
    let sets = c.sets.clone();
    let mut cats: Vec<String> = Vec::new();
    for t in &sets {
        for (i, d) in t.rows.iter().enumerate() {
            let key = if d.label.is_empty() { format!("{}", i + 1) } else { d.label.clone() };
            if !cats.contains(&key) {
                cats.push(key);
            }
        }
    }
    let n = cats.len();
    let row = |k: usize, i: usize| sets[k].rows.iter().enumerate().find(|(j, d)| (if d.label.is_empty() { format!("{}", j + 1) } else { d.label.clone() }) == cats[i]).map(|(_, d)| d.clone());
    let mut groups: Vec<String> = Vec::new();
    for t in &sets {
        for d in &t.rows {
            if let Some(g) = &d.group {
                if !groups.contains(g) {
                    groups.push(g.clone());
                }
            }
        }
    }
    c.keys = cats.clone();
    let legend: Vec<(String, u32)> = groups.iter().enumerate().map(|(k, g)| (g.clone(), c.th.color(k))).collect();
    c.header(&legend);
    let [px0, py0, px1, py1] = c.plot;
    let ls = c.ls;
    let tiles: Vec<Vec<Option<[f64; 4]>>> = (0..sets.len()).map(|k| {
        let values: Vec<f64> = (0..n).map(|i| row(k, i).map_or(0.0, |d| d.value.max(0.0))).collect();
        let mut out = vec![None; n];
        for (i, r) in squarify(&values, [px0, py0, px1 - px0, py1 - py0]) {
            out[i] = Some(r);
        }
        out
    }).collect();
    // Tiles land largest first.
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by(|a, b| row(0, *b).map_or(0.0, |d| d.value).total_cmp(&row(0, *a).map_or(0.0, |d| d.value)).then(a.cmp(b)));
    let mut rank = vec![0; n];
    for (r, i) in order.into_iter().enumerate() {
        rank[i] = r;
    }
    let all: Vec<f64> = sets.iter().flat_map(|t| t.rows.iter().map(|d| d.value)).collect();
    let ease = c.ease("ease_in_out_cubic");
    let gap = 2.0;
    for i in 0..n {
        let t = c.land(rank[i]);
        let d0 = row(0, i).unwrap_or_default();
        let fill = if !c.highlight.is_empty() {
            if c.highlighted(i) { c.th.accent } else { mix(c.th.neutral, c.th.muted, (i % 3) as f64 * 0.2) }
        } else {
            match &d0.group {
                Some(g) => mix(c.th.color(groups.iter().position(|x| x == g).unwrap_or(0)), 0x000000ff, (i % 3) as f64 * 0.12),
                None => c.th.color(i),
            }
        };
        let tile = |k: usize| tiles[k][i].unwrap_or([0.0; 4]);
        let track = |c: &Chart, f: &dyn Fn([f64; 4]) -> ScriptValue| -> ScriptValue {
            let mut points = vec![(t.clone(), f(tile(0)))];
            for k in 1..sets.len() {
                points.push((c.switch[k - 1].clone(), f(tile(k - 1))));
                points.push((c.set_time(k, i), f(tile(k))));
            }
            keys(b, points, ease)
        };
        let at = track(c, &|r| v2(b, r[0] + gap, r[1] + gap));
        let size = track(c, &|r| v2(b, (r[2] - 2.0 * gap).max(0.0), (r[3] - 2.0 * gap).max(0.0)));
        c.out.push(layer(b, "Rect", vec![
            ("name", s(b, &format!("tile {}", cats[i]))), ("at", at), ("size", size), ("fill", color(fill)), ("radius", num(4.0)),
            ("scale", tween(b, num(0.0), num(1.0), c.enter(rank[i]), t.clone(), id("ease_out_cubic"))),
        ]));
        let r = tile(0);
        let ink = c.th.ink_on(fill);
        let labels = c.labels.unwrap_or(true);
        // Only what fits the tile in every dataset.
        let fits = |w: f64, h: f64| (0..sets.len()).all(|k| { let r = tile(k); r[2] - 20.0 >= w && r[3] - 12.0 >= h });
        let label_w = text_width(&cats[i], ls * 0.85, 600.0).min(r[2] - 20.0);
        if labels && fits(ls * 2.5, ls * 1.3) {
            let at = track(c, &|r| v2(b, r[0] + 10.0, r[1] + 8.0));
            let label = c.text("label", &cats[i], [0.0, 0.0], "left", "top", ls * 0.85, 600.0, ink, shift(b, t, 0.1), Some(label_w.max(ls * 2.5)));
            set(b, label, "at", at);
            c.out.push(label);
            if c.values && fits(ls * 2.5, ls * 2.8) {
                let values: Vec<f64> = (0..sets.len()).map(|k| row(k, i).map_or(0.0, |d| d.value)).collect();
                let times: Vec<ScriptValue> = (0..sets.len()).map(|k| if k == 0 { t.clone() } else { c.set_time(k, i) }).collect();
                let shown = c.fmt.shown(all.iter().fold(0.0f64, |a, v| a.max(v.abs())), 0.0, &all);
                let at = track(c, &|r| v2(b, r[0] + 10.0, r[1] + 8.0 + ls * 1.15));
                let min_w = (0..sets.len()).map(|k| tile(k)[2] - 20.0).fold(f64::MAX, f64::min);
                let label = c.number("value", &values, &times, &shown, at, "left", "top", ls * 1.1, 700.0, ink, Some(min_w));
                c.out.push(label);
            }
        }
        for k in 0..sets.len() {
            let r = tile(k);
            c.mark(k, i, [r[0] + r[2] * 0.5, r[1] + r[3] * 0.3]);
        }
    }
    Ok(())
}

/// Squarified treemap: `(datum index, [x, y, w, h])`, largest first.
pub(crate) fn squarify(values: &[f64], rect: [f64; 4]) -> Vec<(usize, [f64; 4])> {
    let mut order: Vec<usize> = (0..values.len()).filter(|&i| values[i] > 0.0).collect();
    order.sort_by(|a, b| values[*b].total_cmp(&values[*a]).then(a.cmp(b)));
    let total: f64 = order.iter().map(|&i| values[i]).sum();
    if total <= 0.0 {
        return Vec::new();
    }
    let scale = rect[2] * rect[3] / total;
    let mut out = Vec::new();
    let mut r = rect;
    let mut row: Vec<usize> = Vec::new();
    let worst = |row: &[usize], side: f64| -> f64 {
        let areas: Vec<f64> = row.iter().map(|&i| values[i] * scale).collect();
        let sum: f64 = areas.iter().sum();
        let (mx, mn) = areas.iter().fold((0.0f64, f64::MAX), |(a, b), &x| (a.max(x), b.min(x)));
        ((side * side * mx) / (sum * sum)).max((sum * sum) / (side * side * mn))
    };
    let mut rest = order.into_iter().peekable();
    while let Some(&i) = rest.peek() {
        let side = r[2].min(r[3]);
        let mut with = row.clone();
        with.push(i);
        if row.is_empty() || worst(&with, side) <= worst(&row, side) {
            row = with;
            rest.next();
        } else {
            lay_row(&row, values, scale, &mut r, &mut out);
            row.clear();
        }
    }
    if !row.is_empty() {
        lay_row(&row, values, scale, &mut r, &mut out);
    }
    out
}

fn lay_row(row: &[usize], values: &[f64], scale: f64, r: &mut [f64; 4], out: &mut Vec<(usize, [f64; 4])>) {
    let sum: f64 = row.iter().map(|&i| values[i] * scale).sum();
    if r[2] >= r[3] {
        let w = sum / r[3].max(1e-9);
        let mut y = r[1];
        for &i in row {
            let h = values[i] * scale / w.max(1e-9);
            out.push((i, [r[0], y, w, h]));
            y += h;
        }
        r[0] += w;
        r[2] -= w;
    } else {
        let h = sum / r[2].max(1e-9);
        let mut x = r[0];
        for &i in row {
            let w = values[i] * scale / h.max(1e-9);
            out.push((i, [x, r[1], w, h]));
            x += w;
        }
        r[1] += h;
        r[3] -= h;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_treemap_tiles_its_box() {
        let rects = squarify(&[6.0, 6.0, 4.0, 3.0, 2.0, 2.0, 1.0], [0.0, 0.0, 600.0, 400.0]);
        let area: f64 = rects.iter().map(|(_, r)| r[2] * r[3]).sum();
        assert!((area - 240000.0).abs() < 1.0, "{area}");
        assert!(rects.iter().all(|(_, r)| r[0] >= -1e-6 && r[1] >= -1e-6 && r[0] + r[2] <= 600.0 + 1e-6 && r[1] + r[3] <= 400.0 + 1e-6));
    }
}
