//! `@probs`: the next-token UI's probability table. Each dataset is one
//! step (`data`, then `next` at `switch` times, e.g. the words of a lyric):
//! the `top_k` tokens ranked by p, a mono token cell, a bar and "0.61".
//! The sampled token (`pick: true`, else the top one) is lit in the accent.
//! M1's TokenStream embeds it with `at` / `size`.

use super::build::*;
use makepad_script::*;
use super::scale::text_width;
use super::Chart;

pub(super) fn build(c: &mut Chart) -> Result<(), String> {
    let b = c.b;
    let sets = c.sets.clone();
    let k = c.top_k.unwrap_or(5).max(1);
    let steps: Vec<Vec<super::Datum>> = sets.iter().map(|t| {
        let mut rows = t.rows.clone();
        rows.sort_by(|a, b| b.value.total_cmp(&a.value));
        rows.truncate(k);
        rows
    }).collect();
    c.keys = steps[0].iter().map(|d| d.label.clone()).collect();
    let header = c.header_text.clone().unwrap_or_else(|| "p(next | context)".into());
    c.header(&[]);
    let ls = c.ls;
    let [px0, py0, px1, py1] = c.plot;
    let mono = "JetBrains Mono";
    let head_h = ls * 1.7;
    let row_h = (py1 - py0 - head_h) / k as f64;
    let size = (row_h * 0.5).min(ls.max(row_h * 0.42));
    let token_w = steps.iter().flatten().map(|d| text_width(&format!("\"{}\"", d.label), size, 500.0) * 1.05).fold(ls * 3.0, f64::max).min((px1 - px0) * 0.4);
    let pct_w = text_width("0.00", size, 700.0) + ls * 0.6;
    let (bx0, bx1) = (px0 + token_w + ls * 0.6, px1 - pct_w);
    let max = c.max.unwrap_or(1.0).max(1e-9);
    let start = c.start.clone();
    let head = c.text("title", &header, [px0, py0 + head_h * 0.45], "left", "center", (size * 0.8).max(ls * 0.75), 600.0, c.th.muted, start, Some(px1 - px0));
    set(b, head, "font", font_face(b, mono, 600.0));
    c.out.push(head);
    c.out.push(c.rule([px0, py0 + head_h - 1.0], [px1 - px0, 1.0], c.th.grid, start));
    // Each step's table shows from its time until the next step's.
    let begins: Vec<ScriptValue> = (0..steps.len()).map(|s| if s == 0 { c.start.clone() } else { c.switch[s - 1].clone() }).collect();
    let ease = c.ease("ease_out_cubic");
    for (step, rows) in steps.iter().enumerate() {
        let picked = rows.iter().position(|d| d.pick).unwrap_or(0);
        let on = begins[step].clone();
        let window = |node: ScriptValue| {
            let mut points = vec![(on.clone(), num(0.0)), (shift(b, on, 0.15), num(1.0))];
            if let Some(next) = begins.get(step + 1) {
                points.push((next.clone(), num(1.0)));
                points.push((shift(b, *next, 0.01), num(0.0)));
            }
            set(b, node, "opacity", keys(b, points, id("linear")));
        };
        for (r, d) in rows.iter().enumerate() {
            let y = py0 + head_h + row_h * (r as f64 + 0.5);
            let lit = r == picked;
            let ink = if lit { c.th.accent } else { c.th.ink };
            let grow_at = shift(b, on, 0.05 + 0.04 * r as f64);
            if lit {
                let band = layer(b, "Rect", vec![("name", s(b, "picked")), ("at", v2(b, px0 - ls * 0.3, y - row_h * 0.46)), ("size", v2(b, px1 - px0 + ls * 0.6, row_h * 0.92)), ("radius", num(6.0)), ("fill", color(alpha(c.th.accent, 0x26)))]);
                window(band);
                c.out.push(band);
            }
            let token = c.text("label", &format!("\"{}\"", d.label), [px0, y], "left", "center", size, if lit { 800.0 } else { 500.0 }, ink, on, Some(token_w));
            set(b, token, "font", font_face(b, mono, if lit { 800.0 } else { 500.0 }));
            window(token);
            c.out.push(token);
            let w = (bx1 - bx0) * (d.value / max).clamp(0.0, 1.0);
            let bar = layer(b, "Rect", vec![
                ("name", s(b, &format!("p {}", d.label))), ("at", v2(b, bx0, y - row_h * 0.22)), ("radius", num(3.0)), ("fill", color(if lit { c.th.accent } else { alpha(c.th.ink, 0x66) })),
                ("size", tween(b, v2(b, 0.0, row_h * 0.44), v2(b, w.max(1.0), row_h * 0.44), grow_at.clone(), shift(b, grow_at, 0.45), ease)),
            ]);
            window(bar);
            c.out.push(bar);
            let pct = c.text("value", &crate::format_number(d.value, 2, None), [px1, y], "right", "center", size * 0.9, 700.0, ink, on, None);
            set(b, pct, "font", font_face(b, mono, 700.0));
            window(pct);
            c.out.push(pct);
            c.mark(step, r, [bx0 + w, y - row_h * 0.22]);
        }
    }
    Ok(())
}

