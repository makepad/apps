//! `@donut` / `@pie`: slices sweep round in turn, labels sit outside on
//! leaders (spread so they never collide), the centre counts up, and a
//! `highlight`ed slice pulls out on the payoff with its share in the middle.

use super::build::*;
use makepad_script::*;
use super::scale::{spread, text_width, Shown};
use super::Chart;
use std::f64::consts::PI;

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
    let val = |k: usize, i: usize| sets[k].rows.iter().enumerate().find(|(j, d)| (if d.label.is_empty() { format!("{}", j + 1) } else { d.label.clone() }) == cats[i]).map_or(0.0, |(_, d)| d.value.max(0.0));
    let totals: Vec<f64> = (0..sets.len()).map(|k| (0..n).map(|i| val(k, i)).sum::<f64>().max(1e-9)).collect();
    let arc = |k: usize, i: usize| -> (f64, f64) {
        let before: f64 = (0..i).map(|j| val(k, j)).sum();
        (before / totals[k], (before + val(k, i)) / totals[k])
    };
    c.keys = cats.clone();
    let pie = c.kind == "pie";
    let outside = c.labels.unwrap_or(n <= 9);
    let legend: Vec<(String, u32)> = if c.legend.unwrap_or(!outside) { (0..n).map(|i| (cats[i].clone(), slice_color(c, i))).collect() } else { Vec::new() };
    c.header(&legend);
    let ls = c.ls;
    let [px0, py0, px1, py1] = c.plot;
    let headroom = if c.notes.is_empty() { 0.0 } else { ls * 3.0 };
    let (pw, ph) = (px1 - px0, py1 - py0 - headroom);
    let label_w = if outside { cats.iter().map(|l| text_width(l, ls * 0.85, 600.0)).fold(text_width("100%", ls * 0.95, 700.0), f64::max) + ls * 1.8 } else { 0.0 };
    let d = (ph * 0.94).min(pw - 2.0 * label_w).max(pw.min(ph) * 0.35);
    let (cx, cy) = ((px0 + px1) * 0.5, py0 + headroom + ph * 0.5);
    let thick = if pie { d * 0.5 } else if c.thickness < d * 0.05 { d * 0.18 } else { c.thickness };
    let ring = d - thick;
    let gap = if n > 1 && !pie { 0.004 } else { 0.0 };
    let ease = c.ease("ease_in_out_cubic");
    let payoff = c.payoff.clone().unwrap_or_else(|| shift(b, c.land(n.saturating_sub(1)), 0.5));
    let hl: Vec<usize> = (0..n).filter(|&i| c.highlighted(i)).collect();
    // A value per dataset: its first key, held until each switch.
    let morph = |c: &Chart, i: usize, first: Vec<(ScriptValue, ScriptValue)>, f: &dyn Fn(usize) -> ScriptValue| -> ScriptValue {
        let mut points = first;
        for k in 1..sets.len() {
            points.push((c.switch[k - 1].clone(), f(k - 1)));
            points.push((c.set_time(k, i), f(k)));
        }
        keys(b, points, ease)
    };
    for i in 0..n {
        let (a0, a1) = arc(0, i);
        let t = c.land(i);
        let fill = slice_color(c, i);
        let start_trim = morph(c, i, vec![(t.clone(), num(a0 + gap))], &|k| num(arc(k, i).0 + gap));
        let end_trim = morph(c, i, vec![(c.enter(i), num(a0 + gap)), (t.clone(), num((a1 - gap).max(a0 + gap)))], &|k| { let (a, b) = arc(k, i); num((b - gap).max(a + gap)) });
        let slice = layer(b, "Ellipse", vec![
            ("name", s(b, &format!("slice {}", cats[i]))), ("at", v2(b, cx - ring * 0.5, cy - ring * 0.5)), ("size", v2(b, ring, ring)), ("stroke", color(fill)),
            ("width", num(thick)), ("trim_start", start_trim), ("trim_end", end_trim),
        ]);
        if !hl.is_empty() {
            let last = sets.len() - 1;
            let (sa, sb) = arc(last, i);
            let mid = (sa + sb) * PI;
            if hl.contains(&i) {
                let out = ls * 0.6;
                set(b, slice, "x", tween(b, num(0.0), num(out * mid.sin()), payoff.clone(), shift(b, payoff, 0.5), id("ease_out_back")));
                set(b, slice, "y", tween(b, num(0.0), num(-out * mid.cos()), payoff.clone(), shift(b, payoff, 0.5), id("ease_out_back")));
            } else {
                set(b, slice, "opacity", tween(b, num(1.0), num(0.4), payoff.clone(), shift(b, payoff, 0.5), id("ease_in_out_cubic")));
            }
        }
        c.out.push(slice);
    }
    // Outside labels: a name and its share, on a leader, spread per side.
    let pct = Shown { div: 1.0, decimals: if (0..sets.len()).any(|k| (0..n).any(|i| { let (a, b) = arc(k, i); b - a > 0.0 && b - a < 0.01 })) { 1 } else { 0 }, prefix: String::new(), suffix: "%".into(), unit: String::new(), time: None, total: None };
    let place = |k: usize| -> Vec<([f64; 2], [f64; 2], bool)> {
        // Each label's anchor on the ring, its text point, and its side.
        let mids: Vec<f64> = (0..n).map(|i| { let (a, b) = arc(k, i); (a + b) * PI }).collect();
        let want: Vec<f64> = mids.iter().map(|m| cy - (d * 0.5 + ls * 0.9) * m.cos()).collect();
        let mut out = vec![([0.0; 2], [0.0; 2], true); n];
        for right in [true, false] {
            let idx: Vec<usize> = (0..n).filter(|&i| (mids[i].sin() >= 0.0) == right).collect();
            let ys = spread(&idx.iter().map(|&i| want[i]).collect::<Vec<_>>(), ls * 2.3, py0 + ls * 1.1, py1 - ls * 1.3);
            for (j, &i) in idx.iter().enumerate() {
                let anchor = [cx + (d * 0.5 + 4.0) * mids[i].sin(), cy - (d * 0.5 + 4.0) * mids[i].cos()];
                let x = if right { cx + d * 0.5 + ls * 1.2 } else { cx - d * 0.5 - ls * 1.2 };
                out[i] = (anchor, [x, ys[j]], right);
            }
        }
        out
    };
    if outside {
        let placed: Vec<Vec<([f64; 2], [f64; 2], bool)>> = (0..sets.len()).map(place).collect();
        for i in 0..n {
            let t = c.land(i);
            let (_, p, right) = placed[0][i];
            let (align, dx) = if right { ("left", 0.0) } else { ("right", 0.0) };
            let leader = |k: usize| { let (a, p, right) = placed[k][i]; s(b, &format!("M {:.2} {:.2} L {:.2} {:.2} L {:.2} {:.2}", a[0], a[1], p[0] + if right { -ls * 0.9 } else { ls * 0.9 }, p[1], p[0] + if right { -ls * 0.3 } else { ls * 0.3 }, p[1])) };
            let svg = morph(c, i, vec![(t.clone(), leader(0))], &leader);
            c.out.push(layer(b, "Path", vec![
                ("name", s(b, &format!("leader {}", cats[i]))), ("svg", svg), ("stroke", color(alpha(c.th.muted, 0xa0))), ("width", num(1.5)),
                ("trim_end", tween(b, num(0.0), num(1.0), shift(b, t, -0.1), shift(b, t, 0.3), id("ease_out_cubic"))),
            ]));
            let name_at = morph(c, i, vec![(t.clone(), v2(b, p[0] + dx, p[1]))], &|k| { let p = placed[k][i].1; v2(b, p[0], p[1]) });
            let name = c.text("label", &cats[i], [0.0, 0.0], align, "bottom", ls * 0.85, 600.0, c.th.ink, shift(b, t, 0.2), Some(label_w - ls * 1.4));
            set(b, name, "at", name_at);
            c.out.push(name);
            if c.values {
                let shares: Vec<f64> = (0..sets.len()).map(|k| { let (a, b) = arc(k, i); (b - a) * 100.0 }).collect();
                let times: Vec<ScriptValue> = (0..sets.len()).map(|k| c.set_time(k, i)).collect();
                let at = morph(c, i, vec![(t.clone(), v2(b, p[0], p[1] + 2.0))], &|k| { let p = placed[k][i].1; v2(b, p[0], p[1] + 2.0) });
                // Shares in their slice's colour; under a highlight, only its own.
                let ink = if c.highlight.is_empty() || c.highlighted(i) { slice_color(c, i) } else { c.th.muted };
                let label = c.number("value", &shares, &times, &pct, at, align, "top", ls * 0.95, 700.0, ink, None);
                c.out.push(label);
            }
        }
    }
    // The centre: the highlighted share on the payoff, or the total.
    let inner = if pie { 0.0 } else { d - 2.0 * thick };
    if inner > ls * 3.0 {
        let caption = c.center.clone().or_else(|| hl.first().map(|&i| cats[i].clone()));
        let big = (inner * 0.3).min(ls * 3.2);
        let lift = if caption.is_some() { big * 0.3 } else { 0.0 };
        if let Some(&i) = hl.first() {
            let share: Vec<f64> = (0..sets.len()).map(|k| { let (a, b) = arc(k, i); (b - a) * 100.0 }).collect();
            let times: Vec<ScriptValue> = (0..sets.len()).map(|k| if k == 0 { shift(b, payoff, c.grow) } else { c.set_time(k, i) }).collect();
            let label = c.number("center", &share, &times, &pct, v2(b, cx, cy - lift), "center", "center", big, 800.0, c.th.accent, Some(inner * 0.8));
            c.out.push(label);
        } else if c.values {
            let shown = c.fmt.shown(totals.iter().copied().fold(0.0, f64::max), 0.0, &totals);
            let times: Vec<ScriptValue> = (0..sets.len()).map(|k| c.set_time(k, n.saturating_sub(1))).collect();
            let label = c.number("center", &totals, &times, &shown, v2(b, cx, cy - lift), "center", "center", big, 800.0, c.th.ink, Some(inner * 0.8));
            c.out.push(label);
        }
        if let Some(caption) = caption {
            let at = if hl.is_empty() { shift(b, c.land(n.saturating_sub(1)), 0.2) } else { shift(b, payoff, 0.3) };
            let label = c.text("caption", &caption, [cx, cy + big * 0.45], "center", "top", ls * 0.85, 500.0, c.th.muted, at, Some(inner * 0.75));
            c.out.push(label);
        }
    }
    c.avoid.push([cx - d * 0.5, cy - d * 0.5, cx + d * 0.5, cy + d * 0.5]);
    for k in 0..sets.len() {
        for i in 0..n {
            let (a, b) = arc(k, i);
            let mid = (a + b) * PI;
            c.mark(k, i, [cx + d * 0.5 * mid.sin(), cy - d * 0.5 * mid.cos()]);
        }
    }
    Ok(())
}

fn slice_color(c: &Chart, i: usize) -> u32 {
    if c.highlight.is_empty() {
        c.th.color(i)
    } else if c.highlighted(i) {
        c.th.accent
    } else {
        mix(c.th.neutral, c.th.muted, (i % 3) as f64 * 0.22)
    }
}
