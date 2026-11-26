//! Layout templates that lower to plain layers before compiling, like a
//! Chart: `Comic{…}` (a comic page whose panels slam in on words, with
//! caption tabs, halftone fills and action bursts) and `Polaroid{…}` (the
//! video's own picture frozen at a time, flashed, desaturated and framed
//! as a tilted instant photo with a handwritten caption).

use crate::build::*;
use crate::obj::{self, face, Lit};
use makepad_script::*;

pub const PANEL_FIELDS: &[&str] = &["time", "fill", "caption", "burst", "burst_time", "burst_color", "layers", "span"];

/// The common fields most templates carry over onto the Group they make.
const BOX_KEEP: &[&str] = &["name", "at", "x", "y", "scale", "rotate", "opacity", "span", "blend", "visible", "locked", "effects"];

fn effect(cx: &mut Cx, name: &'static str, params: Vec<(&str, Lit)>) -> ScriptValue {
    let mut fields: Vec<(&str, Lit)> = vec![("name", Lit::Id(name))];
    fields.extend(params);
    layer(cx, "Effect", fields)
}

/// `keys(…)` of numbers: (time, value, ease).
fn nkeys(cx: &mut Cx, points: Vec<(f64, f64, Option<&str>)>) -> ScriptValue {
    let list = points.into_iter().map(|(t, v, e)| (ScriptValue::from(t), ScriptValue::from(v), e)).collect();
    obj::keys(cx.vm, list)
}

/// A `Loop` of `inner` restarting every `period` forever.
fn forever(cx: &mut Cx, inner: ScriptValue, period: f64) -> ScriptValue {
    obj::make(cx.vm, "Loop", &[("inner", inner), ("period", period.into()), ("mode", obj::id("restart")), ("count", NIL)])
}

/// A number field of `o`.
fn onum(cx: &Cx, o: ScriptObject, key: &str) -> Option<f64> {
    obj::opt(cx.vm, o, key).and_then(|v| obj::num(cx.vm, v))
}

/// The items of an array field of `o`.
fn oitems(cx: &Cx, o: ScriptObject, key: &str) -> Option<Vec<ScriptValue>> {
    obj::opt(cx.vm, o, key).and_then(|v| obj::items(cx.vm, v))
}

fn color_of(v: Option<ScriptValue>, default: u32) -> u32 {
    v.and_then(|v| v.as_color()).unwrap_or(default)
}

fn ocolor(cx: &Cx, o: ScriptObject, key: &str, default: u32) -> u32 {
    color_of(obj::opt(cx.vm, o, key), default)
}

/// A non-array, non-function object.
fn plain(cx: &Cx, v: ScriptValue) -> Option<ScriptObject> {
    v.as_object().filter(|o| obj::items(cx.vm, v).is_none() && !cx.vm.bx.heap_ref().is_fn(*o))
}

/// A two-number field (each part `default`'s where it is not a number);
/// `None` where the field is not a pair.
fn pair(cx: &Cx, o: ScriptObject, key: &str, default: [f64; 2]) -> Option<[f64; 2]> {
    match oitems(cx, o, key) {
        Some(s) if s.len() == 2 => Some([obj::num(cx.vm, s[0]).unwrap_or(default[0]), obj::num(cx.vm, s[1]).unwrap_or(default[1])]),
        _ => None,
    }
}

/// The Group a template lowers to: `o`'s fields in `keep`, then `extra`,
/// made where `o` was.
fn out_group(cx: &mut Cx, o: ScriptObject, keep: &[&str], extra: Vec<(&str, ScriptValue)>) -> Made {
    let g = crate::layers::make(cx.vm, "Group", &[]);
    for (k, v, _) in obj::fields(cx.vm, o) {
        if keep.contains(&k.as_str()) {
            obj::set(cx.vm, g, &k, v);
        }
    }
    for (k, v) in extra {
        obj::set(cx.vm, g, k, v);
    }
    let at = cx.vm.bx.heap_ref().made_at(o);
    cx.vm.bx.heap_mut().set_made_at(g, at);
    Ok(g.into())
}

fn hash01(seed: u32, i: usize, k: u32) -> f64 {
    crate::crowd::hash01(seed, i, k)
}

/// A comic starburst: `spikes` points round an ellipse of `size`, the
/// inner corners at 58–72% of the outer (seeded), as path data from 0,0.
pub fn starburst(size: [f64; 2], spikes: usize, seed: u32) -> String {
    let (cx, cy) = (size[0] * 0.5, size[1] * 0.5);
    let mut out = String::new();
    for k in 0..spikes * 2 {
        let a = k as f64 / (spikes * 2) as f64 * std::f64::consts::TAU - std::f64::consts::FRAC_PI_2;
        let r = if k % 2 == 0 { 0.92 + 0.08 * hash01(seed, k, 1) } else { 0.58 + 0.14 * hash01(seed, k, 2) };
        let (x, y) = (cx + a.cos() * cx * r, cy + a.sin() * cy * r);
        out.push_str(&format!("{} {:.1} {:.1} ", if k == 0 { "M" } else { "L" }, x, y));
    }
    out.push('Z');
    out
}

/// A darker tone of a colour (for halftone dots on a flat fill).
fn shade(c: u32, k: f64) -> u32 {
    let ch = |s: u32| ((((c >> s) & 0xff) as f64) * k).round().clamp(0.0, 255.0) as u32;
    (ch(24) << 24) | (ch(16) << 16) | (ch(8) << 8) | (c & 0xff)
}

/// The comic's panel boxes `[x, y, w, h]` inside `size`.
pub fn comic_boxes(n: usize, size: [f64; 2], layout: &str, columns: Option<usize>, gutter: f64) -> Vec<[f64; 4]> {
    let [w, h] = size;
    if n == 0 {
        return Vec::new();
    }
    if layout == "hero" && n >= 2 && h > w {
        // Portrait: the first panel across the top half, the rest below
        // two to a row.
        let top = (h - gutter * 3.0) * 0.52;
        let mut out = vec![[gutter, gutter, w - gutter * 2.0, top]];
        let rest = comic_boxes(n - 1, [w, h - top - gutter], "grid", Some(2), gutter);
        out.extend(rest.into_iter().map(|[x, y, bw, bh]| [x, y + top + gutter, bw, bh]));
        return out;
    }
    if layout == "hero" && n >= 3 {
        // A tall centre panel (the first), the rest in two side columns.
        let side = n - 1;
        let left = side.div_ceil(2);
        let right = side - left;
        let cw = (w - gutter * 4.0) * 0.25;
        let hero_w = w - gutter * 4.0 - cw * 2.0;
        let mut out = vec![[gutter * 2.0 + cw, gutter, hero_w, h - gutter * 2.0]];
        for (col, count, x) in [(0, left, gutter), (1, right, w - gutter - cw)] {
            let _ = col;
            if count == 0 {
                continue;
            }
            let ph = (h - gutter * (count as f64 + 1.0)) / count as f64;
            for k in 0..count {
                out.push([x, gutter + k as f64 * (ph + gutter), cw, ph]);
            }
        }
        // Alternate sides in panel order: left, right, left, …
        let mut ordered = vec![out[0]];
        let (l, r): (Vec<_>, Vec<_>) = out[1..].iter().enumerate().partition(|(i, _)| *i < left);
        let (mut l, mut r) = (l.into_iter().map(|(_, b)| *b), r.into_iter().map(|(_, b)| *b));
        loop {
            match (l.next(), r.next()) {
                (None, None) => break,
                (a, b) => {
                    ordered.extend(a);
                    ordered.extend(b);
                }
            }
        }
        return ordered;
    }
    let cols = columns.unwrap_or(if w > h { n.min(3) } else if n <= 2 { 1 } else { 2 }).max(1);
    let rows = n.div_ceil(cols);
    let ph = (h - gutter * (rows as f64 + 1.0)) / rows as f64;
    let mut out = Vec::new();
    for r in 0..rows {
        // The last row's panels share its width.
        let in_row = if r + 1 == rows { n - r * cols } else { cols };
        let pw = (w - gutter * (in_row as f64 + 1.0)) / in_row as f64;
        for c in 0..in_row {
            out.push([gutter + c as f64 * (pw + gutter), gutter + r as f64 * (ph + gutter), pw, ph]);
        }
    }
    out
}

/// A word that pops out of a starburst at `time`.
fn burst(cx: &mut Cx, text: &str, at: [f64; 2], size: [f64; 2], time: ScriptValue, fill: u32, ink: u32, seed: u32) -> ScriptValue {
    let tilt = (hash01(seed, 7, 3) - 0.5) * 20.0;
    let t1 = obj::shift(cx.vm, time, 0.12);
    let t2 = obj::shift(cx.vm, time, 0.24);
    let pop = obj::keys(cx.vm, vec![
        (time, 0.0.into(), None),
        (t1, 1.18.into(), Some("ease_out_cubic")),
        (t2, 1.0.into(), Some("ease_in_out_sine")),
    ]);
    let opacity = tween(cx, 0.0.into(), 1.0.into(), time, 0.02, "linear");
    let star = layer(cx, "Path", vec![("svg", starburst(size, 14, seed).into()), ("fill", Lit::Color(fill)), ("stroke", Lit::Color(ink)), ("width", (size[0] * 0.025).max(3.0).into()), ("join", Lit::Id("miter"))]);
    let word = layer(cx, "Text", vec![
        ("text", text.into()),
        ("font", face("Anton", 400.0, false)),
        ("size", (size[1] * 0.26).into()),
        ("color", Lit::Color(ink)),
        ("at", v2(size[0] * 0.5, size[1] * 0.5)),
        ("align", Lit::Id("center")),
        ("valign", Lit::Id("center")),
        ("fit", (size[0] * 0.62).into()),
    ]);
    let inner = array(cx, vec![star, word]);
    layer(cx, "Group", vec![
        ("name", format!("burst {text}").into()),
        ("at", v2(at[0], at[1])),
        ("size", v2(size[0], size[1])),
        ("rotate", tilt.into()),
        ("scale", pop.into()),
        ("opacity", opacity.into()),
        ("layers", inner.into()),
    ])
}

/// `Comic{panels: [{time fill caption burst burst_time burst_color layers}…]
/// size layout(@grid|@hero) columns gutter border ink paper halftone seed}`.
pub(crate) fn comic(cx: &mut Cx, o: ScriptObject) -> Made {
    let doc = cx.doc;
    let size = pair(cx, o, "size", doc).unwrap_or(doc);
    let panels = match oitems(cx, o, "panels") {
        Some(items) if !items.is_empty() => items,
        _ => return Err(("panels".into(), "a Comic has `panels: [{time: … caption: \"…\" layers: […]}, …]`".into())),
    };
    let layout = match obj::opt(cx.vm, o, "layout") {
        None => "grid".to_string(),
        Some(v) if v.is_nil() => "grid".to_string(),
        Some(v) => match obj::ident(v) {
            Some(l) if l == "grid" || l == "hero" => l,
            _ => return Err(("layout".into(), format!("`layout` is @grid or @hero, not {}", kind(cx, v)))),
        },
    };
    let columns = onum(cx, o, "columns").map(|n| n.max(1.0) as usize);
    let unit = size[0].min(size[1]);
    let gutter = onum(cx, o, "gutter").unwrap_or(unit * 0.025);
    let border = onum(cx, o, "border").unwrap_or(unit * 0.008);
    let ink = ocolor(cx, o, "ink", 0x111111ff);
    let paper = ocolor(cx, o, "paper", 0xf3ecd8ff);
    let halftone = get_bool(cx, o, "halftone", true);
    let seed = onum(cx, o, "seed").unwrap_or(1.0) as u32;
    let fills = [0xffd400ff, 0x2ec27eff, 0xff7a1aff, 0x3aa0ffff, 0xff4fa0ff, 0xb6ff3bff];
    let boxes = comic_boxes(panels.len(), size, &layout, columns, gutter);
    let mut layers = vec![layer(cx, "Rect", vec![("name", "page".into()), ("size", v2(size[0], size[1])), ("fill", Lit::Color(paper))])];
    for (i, (panel, b)) in panels.iter().zip(&boxes).enumerate() {
        let panel = *panel;
        let Some(p) = plain(cx, panel) else {
            return Err(("panels".into(), format!("panel {i} is {}; a panel is `{{time: … caption: \"…\" layers: […]}}`", kind(cx, panel))));
        };
        if let Some((key, _, _)) = obj::fields(cx.vm, p).into_iter().find(|(k, _, _)| !PANEL_FIELDS.contains(&k.as_str())) {
            return Err((format!("panels[{i}].{key}"), format!("a panel has no `{key}`; it takes {}", PANEL_FIELDS.join(" "))));
        }
        let time = obj::opt(cx.vm, p, "time").unwrap_or((0.3 + i as f64 * 0.6).into());
        let fill = ocolor(cx, p, "fill", fills[i % fills.len()]);
        let [x, y, w, h] = *b;
        let tilt = (hash01(seed, i, 1) - 0.5) * 10.0;
        let mut inner = vec![layer(cx, "Rect", vec![("size", v2(w, h)), ("fill", Lit::Color(fill))])];
        if halftone {
            inner.push(layer(cx, "Pattern", vec![("kind", Lit::Id("dots")), ("color", Lit::Color(shade(fill, 0.78))), ("cell", (unit * 0.018).into()), ("weight", 0.42.into()), ("angle", 45.0.into()), ("size", v2(w, h)), ("opacity", 0.55.into())]));
        }
        if let Some(own) = oitems(cx, p, "layers") {
            inner.extend(own);
        }
        let mask = layer(cx, "Rect", vec![("size", v2(w, h))]);
        let inner = array(cx, inner);
        let clip = layer(cx, "Group", vec![("size", v2(w, h)), ("mask", mask.into()), ("layers", inner.into())]);
        let frame = layer(cx, "Rect", vec![("name", "border".into()), ("size", v2(w, h)), ("stroke", Lit::Color(ink)), ("width", border.into())]);
        let mut parts = vec![clip, frame];
        if let Some(caption) = get_text(cx, p, "caption") {
            let th = unit * 0.05;
            let tw = (caption.chars().count() as f64 * th * 0.42 + th * 0.9).min(w * 0.9);
            let tab = layer(cx, "Rect", vec![("size", v2(tw, th)), ("fill", Lit::Color(0xfffbeaff)), ("stroke", Lit::Color(ink)), ("width", (border * 0.8).into())]);
            let words = layer(cx, "Text", vec![
                ("text", caption.to_uppercase().into()),
                ("font", face("LXGW WenKai", 700.0, true)),
                ("size", (th * 0.52).into()),
                ("color", Lit::Color(ink)),
                ("at", v2(tw * 0.5, th * 0.5)),
                ("align", Lit::Id("center")),
                ("valign", Lit::Id("center")),
                ("fit", (tw * 0.88).into()),
            ]);
            let tab_layers = array(cx, vec![tab, words]);
            parts.push(layer(cx, "Group", vec![
                ("name", "caption".into()),
                ("at", v2(-border * 0.5, -border * 0.5)),
                ("rotate", (-tilt * 0.3).into()),
                ("layers", tab_layers.into()),
            ]));
        }
        if let Some(word) = get_text(cx, p, "burst") {
            let bt = match obj::opt(cx.vm, p, "burst_time") {
                Some(v) => v,
                None => obj::shift(cx.vm, time, 0.35),
            };
            let bw = (w * 0.8).min(unit * 0.42);
            let bs = [bw, bw * 0.72];
            let corner = [w - bs[0] * 0.8, h - bs[1] * 0.85];
            let burst_color = ocolor(cx, p, "burst_color", 0xff3b2fff);
            parts.push(burst(cx, &word, corner, bs, bt, burst_color, ink, seed.wrapping_add(i as u32)));
        }
        // The slam: in from big and turned, overshooting flat, a hard cut in.
        let t14 = obj::shift(cx.vm, time, 0.14);
        let t26 = obj::shift(cx.vm, time, 0.26);
        let slam = obj::keys(cx.vm, vec![
            (time, 1.35.into(), None),
            (t14, 0.97.into(), Some("ease_in_cubic")),
            (t26, 1.0.into(), Some("ease_out_sine")),
        ]);
        let turn = obj::keys(cx.vm, vec![(time, (tilt * 2.5).into(), None), (t14, 0.0.into(), Some("ease_in_cubic"))]);
        let opacity = tween(cx, 0.0.into(), 1.0.into(), time, 0.02, "linear");
        let parts = array(cx, parts);
        let mut panel_fields: Vec<(&str, Lit)> = vec![
            ("name", format!("panel {i}").into()),
            ("at", v2(x, y)),
            ("size", v2(w, h)),
            ("scale", slam.into()),
            ("rotate", turn.into()),
            ("opacity", opacity.into()),
            ("layers", parts.into()),
        ];
        if let Some(span) = obj::opt(cx.vm, p, "span") {
            panel_fields.push(("span", span.into()));
        }
        layers.push(layer(cx, "Group", panel_fields));
    }
    let layers = array(cx, layers);
    let page = obj::v2(cx.vm, size[0], size[1]);
    let effects = match obj::opt(cx.vm, o, "effects") {
        None => {
            let grain = effect(cx, "grain", vec![("amount", 0.06.into()), ("size", 1.5.into())]);
            array(cx, vec![grain])
        }
        Some(e) => e,
    };
    out_group(cx, o, &["name", "at", "x", "y", "scale", "rotate", "anchor", "opacity", "span", "blend", "visible", "locked"], vec![("size", page), ("layers", layers), ("effects", effects)])
}

/// `Polaroid{time size caption caption_time tilt flash mono burst ink card}`:
/// at the layer's start a white flash and a starburst, then the picture
/// under it as it was at `time`, desaturated, in a tilted instant-photo
/// card, its caption written word by word.
pub(crate) fn polaroid(cx: &mut Cx, o: ScriptObject) -> Made {
    let doc = cx.doc;
    let Some(time) = obj::opt(cx.vm, o, "time") else {
        return Err(("time".into(), "a Polaroid freezes the picture at its `time` (seconds, or `beat(…)`, `word(@vo, …)`)".into()));
    };
    let photo = match pair(cx, o, "size", [doc[0] * 0.44, doc[1] * 0.44]) {
        Some(p) => p,
        None => {
            let w = doc[0].min(doc[1] * 1.2) * 0.44;
            [w, w * doc[1] / doc[0].max(1.0)]
        }
    };
    let pad = photo[0] * 0.06;
    let card = [photo[0] + pad * 2.0, photo[1] + pad * 1.5 + photo[0] * 0.22];
    let tilt = onum(cx, o, "tilt").unwrap_or(-6.0);
    let ink = ocolor(cx, o, "ink", 0x1a1a1aff);
    let card_color = ocolor(cx, o, "card", 0xf7f5efff);
    let flash = get_bool(cx, o, "flash", true);
    let mono = get_bool(cx, o, "mono", true);
    let with_burst = get_bool(cx, o, "burst", true);
    let at = pair(cx, o, "at", [0.0, 0.0]).unwrap_or([(doc[0] - card[0]) * 0.5, (doc[1] - card[1]) * 0.5]);
    let zero: ScriptValue = 0.0.into();
    let frozen = layer(cx, "Freeze", vec![("time", time.into()), ("size", v2(photo[0], photo[1]))]);
    let frozen = array(cx, vec![frozen]);
    let mut photo_group: Vec<(&str, Lit)> = vec![
        ("name", "photo".into()),
        ("at", v2(pad, pad)),
        ("size", v2(photo[0], photo[1])),
        ("layers", frozen.into()),
    ];
    if mono {
        let tint = effect(cx, "tint", vec![("black", Lit::Color(0x151515ff)), ("white", Lit::Color(0xf2f0eaff))]);
        let contrast = effect(cx, "brightness_contrast", vec![("contrast", 0.15.into())]);
        let grain = effect(cx, "grain", vec![("amount", 0.08.into()), ("size", 1.4.into())]);
        let effects = array(cx, vec![tint, contrast, grain]);
        photo_group.push(("effects", effects.into()));
    }
    let shadow = effect(cx, "drop_shadow", vec![("distance", (pad * 0.5).into()), ("radius", (pad * 0.9).into()), ("opacity", 0.5.into())]);
    let shadow = array(cx, vec![shadow]);
    let card_rect = layer(cx, "Rect", vec![("name", "card".into()), ("size", v2(card[0], card[1])), ("fill", Lit::Color(card_color)), ("radius", (pad * 0.15).into()), ("effects", shadow.into())]);
    let photo_group = layer(cx, "Group", photo_group);
    let mut card_layers = vec![card_rect, photo_group];
    if let Some(caption) = get_text(cx, o, "caption") {
        let ct = obj::opt(cx.vm, o, "caption_time").unwrap_or(0.55.into());
        let words = caption.split_whitespace().count().max(1);
        let _ = words;
        let opacity = tween(cx, 0.0.into(), 1.0.into(), ct, 0.08, "linear");
        let nudge = tween(cx, (-card[0] * 0.02).into(), 0.0.into(), ct, 0.18, "ease_out");
        card_layers.push(layer(cx, "Text", vec![
            ("name", "caption".into()),
            ("text", caption.into()),
            ("font", face("LXGW WenKai", 700.0, false)),
            ("size", (card[0] * 0.075).into()),
            ("color", Lit::Color(ink)),
            ("at", v2(card[0] * 0.5, pad + photo[1] + (card[1] - pad - photo[1]) * 0.5)),
            ("align", Lit::Id("center")),
            ("valign", Lit::Id("center")),
            ("fit", (card[0] * 0.86).into()),
            ("rotate", (-1.5).into()),
            ("by", Lit::Id("word")),
            ("stagger", 0.38.into()),
            ("opacity", opacity.into()),
            ("x", nudge.into()),
        ]));
    }
    // The card lands: dropped in turned further and bigger, settling.
    let rotate = nkeys(cx, vec![(0.0, tilt - 9.0, None), (0.38, tilt, Some("ease_out_back"))]);
    let scale = nkeys(cx, vec![(0.0, 1.18, None), (0.38, 1.0, Some("ease_out_cubic"))]);
    let card_layers = array(cx, card_layers);
    let card_group = layer(cx, "Group", vec![
        ("name", "polaroid".into()),
        ("at", v2(at[0], at[1])),
        ("size", v2(card[0], card[1])),
        ("rotate", rotate.into()),
        ("scale", scale.into()),
        ("layers", card_layers.into()),
    ]);
    let mut layers = Vec::new();
    // Under the card: the frozen picture full frame for the first beat of
    // the flash, so the cut into the photo reads as the same moment.
    let opacity = nkeys(cx, vec![(0.0, 1.0, None), (0.2, 1.0, None), (0.45, 0.0, Some("ease_in_out_sine"))]);
    let full = layer(cx, "Freeze", vec![("time", time.into()), ("size", v2(doc[0], doc[1]))]);
    let full = array(cx, vec![full]);
    layers.push(layer(cx, "Group", vec![("name", "freeze".into()), ("opacity", opacity.into()), ("layers", full.into())]));
    layers.push(card_group);
    if with_burst {
        let bs = doc[0].min(doc[1]) * 0.9;
        let scale = nkeys(cx, vec![(0.0, 0.3, None), (0.14, 1.1, Some("ease_out_cubic"))]);
        let opacity = nkeys(cx, vec![(0.0, 0.9, None), (0.1, 0.9, None), (0.3, 0.0, Some("ease_out"))]);
        layers.push(layer(cx, "Path", vec![
            ("name", "flash burst".into()),
            ("svg", starburst([bs, bs], 18, 5).into()),
            ("fill", Lit::Color(0xffffffff)),
            ("at", v2(at[0] + card[0] * 0.5 - bs * 0.5, at[1] + card[1] * 0.4 - bs * 0.5)),
            ("scale", scale.into()),
            ("opacity", opacity.into()),
        ]));
    }
    if flash {
        let opacity = nkeys(cx, vec![(0.0, 1.0, None), (0.06, 1.0, None), (0.32, 0.0, Some("ease_out_cubic"))]);
        layers.push(layer(cx, "Rect", vec![
            ("name", "flash".into()),
            ("size", v2(doc[0], doc[1])),
            ("fill", Lit::Color(0xffffffff)),
            ("opacity", opacity.into()),
        ]));
    }
    let _ = zero;
    let layers = array(cx, layers);
    out_group(cx, o, &["name", "opacity", "span", "blend", "visible", "locked", "effects"], vec![("layers", layers)])
}

pub const WORD_FIELDS: &[&str] = &["text", "bg", "color", "color2", "font", "size", "look"];

/// The Text fields of the house mono label: tiny tracked capitals, half
/// the ink (`None` for no text).
fn label_fields(text: Option<&str>, at: [f64; 2], size: f64, ink: ScriptValue, align: &'static str) -> Vec<(&'static str, Lit)> {
    let mut fields: Vec<(&'static str, Lit)> = Vec::new();
    if let Some(t) = text {
        fields.push(("text", t.to_uppercase().into()));
    }
    fields.push(("font", face("JetBrains Mono", 400.0, false)));
    fields.push(("size", size.into()));
    fields.push(("tracking", (size * 0.18).into()));
    fields.push(("color", ink.into()));
    fields.push(("opacity", 0.6.into()));
    fields.push(("at", v2(at[0], at[1])));
    fields.push(("align", Lit::Id(align)));
    fields.push(("valign", Lit::Id("center")));
    fields
}

/// The house mono label: tiny tracked capitals, half the ink.
fn hud_label(cx: &mut Cx, text: &str, at: [f64; 2], size: f64, ink: ScriptValue, align: &'static str) -> ScriptValue {
    layer(cx, "Text", label_fields(Some(text), at, size, ink, align))
}

/// A time as seconds: a number, or `beats(n)` at `bpm`.
fn seconds(cx: &Cx, v: ScriptValue, bpm: f64) -> Option<f64> {
    if let Some(n) = v.as_number() {
        return Some(n);
    }
    let o = v.as_object()?;
    if obj::is(cx.vm, v, "Beats") {
        return onum(cx, o, "n").map(|n| n * 60.0 / bpm.max(1.0));
    }
    None
}

/// `Hud{title tag chapters bpm timecode clock ink inset brackets size label_size}`:
/// a showreel HUD over the frame (or over `size` at `at`): corner
/// brackets, a title and a tag at the top, the chapter counter
/// ("01/08 EASING") bottom left, "● 120 BPM" (the dot on the beat) and a
/// running timecode bottom right. A chapter may flip the ink:
/// `chapters: [[0, "EASING"], [2.0, "TYPOGRAPHY", #111111], …]`.
pub(crate) fn hud(cx: &mut Cx, o: ScriptObject) -> Made {
    let (doc, bpm) = (cx.doc, cx.bpm);
    let size = pair(cx, o, "size", doc).unwrap_or(doc);
    let unit = size[0].min(size[1]);
    let inset = onum(cx, o, "inset").unwrap_or(unit * 0.045);
    let ls = onum(cx, o, "label_size").unwrap_or(unit * 0.02);
    let base_ink = ocolor(cx, o, "ink", 0xf0ebe0ff);
    // Chapters: (start, name, ink).
    let mut chapters: Vec<(f64, String, u32)> = Vec::new();
    if let Some(list) = obj::opt(cx.vm, o, "chapters") {
        let Some(items) = obj::items(cx.vm, list) else { return Err(("chapters".into(), "`chapters` is `[[time, \"NAME\"], [time, \"NAME\", #ink], …]`".into())) };
        for (i, item) in items.iter().enumerate() {
            let item = *item;
            let (t, name, ink) = if let Some(p) = obj::items(cx.vm, item).filter(|p| p.len() >= 2) {
                (seconds(cx, p[0], bpm), obj::string(cx.vm, p[1]), p.get(2).map(|c| color_of(Some(*c), base_ink)).unwrap_or(base_ink))
            } else if let Some(po) = plain(cx, item) {
                (obj::opt(cx.vm, po, "time").and_then(|t| seconds(cx, t, bpm)), get_text(cx, po, "name"), ocolor(cx, po, "ink", base_ink))
            } else {
                (None, None, base_ink)
            };
            let (Some(t), Some(name)) = (t, name) else {
                return Err((format!("chapters[{i}]"), format!("chapter {i} is {}; a chapter is `[time, \"NAME\"]` or `[time, \"NAME\", #ink]`", kind(cx, item))));
            };
            chapters.push((t, name, ink));
        }
    }
    // The ink, flipping per chapter (held until the next).
    let ink: ScriptValue = if chapters.iter().any(|c| c.2 != base_ink) {
        let first = chapters.first().map_or(base_ink, |c| if c.0 <= 0.0 { c.2 } else { base_ink });
        let mut points: Vec<(ScriptValue, ScriptValue, Option<&str>)> = vec![(0.0.into(), obj::color(first), None)];
        for c in &chapters {
            if c.0 > 0.0 {
                points.push((c.0.into(), obj::color(c.2), Some("hold")));
            }
        }
        obj::keys(cx.vm, points)
    } else {
        obj::color(base_ink)
    };
    let [w, h] = size;
    let mut layers = Vec::new();
    if get_bool(cx, o, "brackets", true) {
        let b = unit * 0.035;
        let lw = (unit * 0.0025).max(1.5);
        for (x, y, sx, sy) in [(inset * 0.5, inset * 0.5, 1.0, 1.0), (w - inset * 0.5, inset * 0.5, -1.0, 1.0), (inset * 0.5, h - inset * 0.5, 1.0, -1.0), (w - inset * 0.5, h - inset * 0.5, -1.0, -1.0)] {
            layers.push(layer(cx, "Path", vec![
                ("name", "bracket".into()),
                ("svg", format!("M 0 {} L 0 0 L {} 0", b * sy, b * sx).into()),
                ("at", v2(x, y)),
                ("stroke", ink.into()),
                ("width", lw.into()),
                ("opacity", 0.7.into()),
            ]));
        }
    }
    let top = inset + ls * 0.5;
    let bottom = h - inset - ls * 0.5;
    if let Some(title) = get_text(cx, o, "title") {
        layers.push(hud_label(cx, &title, [inset + ls, top], ls, ink, "left"));
    }
    if let Some(tag) = get_text(cx, o, "tag") {
        layers.push(hud_label(cx, &tag, [w - inset - ls, top], ls, ink, "right"));
    }
    let n = chapters.len();
    for (k, (t, name, chapter_ink)) in chapters.iter().enumerate() {
        let end = chapters.get(k + 1).map_or(1e6, |c| c.0);
        // Its own ink: a spanned layer's clock starts at its span.
        let mut fields = label_fields(Some(&format!("{:02}/{:02}  {}", k + 1, n, name)), [inset + ls, bottom], ls, obj::color(*chapter_ink), "left");
        let span = obj::array(cx.vm, [ScriptValue::from(*t), ScriptValue::from((end - t).max(0.01))]);
        fields.push(("span", span.into()));
        layers.push(layer(cx, "Text", fields));
    }
    let mut right = w - inset - ls;
    if get_bool(cx, o, "timecode", true) {
        let clock = match obj::opt(cx.vm, o, "clock").and_then(obj::ident) {
            Some(c) if c == "doc" || c == "edit" || c == "time" => c,
            _ => "edit".to_string(),
        };
        let mut fields = label_fields(None, [right, bottom], ls, ink, "right");
        fields.push(("number", Lit::Value(obj::id(&clock))));
        fields.push(("format", Lit::Id("timecode")));
        fields.push(("tabular", true.into()));
        layers.push(layer(cx, "Text", fields));
        // A mono cell: 0.6 em (the em is 4/3 of the size) plus tracking.
        right -= ls * 0.98 * 11.0 + ls * 2.0;
    }
    if get_bool(cx, o, "bpm", true) {
        let tempo = onum(cx, o, "bpm").unwrap_or(bpm);
        layers.push(hud_label(cx, &format!("{} BPM", crate::value::fmt_num(tempo)), [right, bottom], ls, ink, "right"));
        // The dot, flashing on every beat.
        let period = 60.0 / tempo.max(1.0);
        let d = ls * 0.55;
        let text_w = (format!("{} BPM", crate::value::fmt_num(tempo)).chars().count() as f64) * ls * 0.98;
        let inner = nkeys(cx, vec![(0.0, 1.0, None), (period * 0.6, 0.25, Some("ease_out_cubic"))]);
        let blink = forever(cx, inner, period);
        layers.push(layer(cx, "Ellipse", vec![
            ("name", "beat".into()),
            ("at", v2(right - text_w - d * 2.2, bottom - d * 0.5)),
            ("size", v2(d, d)),
            ("fill", Lit::Color(0xff5a1fff)),
            ("opacity", blink.into()),
        ]));
    }
    let layers = array(cx, layers);
    out_group(cx, o, &["name", "at", "x", "y", "opacity", "span", "blend", "visible", "locked", "effects"], vec![("layers", layers)])
}

/// A word's look: what every layer of a word shares.
struct WordStyle {
    text: String,
    font: crate::font_spec::FontSpec,
    tsize: f64,
    center: [f64; 2],
    fit: f64,
}

/// One layer of the word, `color`, offset by `dx`, `dy`, with `extra` fields.
fn word_layer(cx: &mut Cx, st: &WordStyle, color: u32, dx: f64, dy: f64, extra: Vec<(&str, Lit)>) -> ScriptValue {
    let mut fields: Vec<(&str, Lit)> = vec![
        ("text", st.text.clone().into()),
        ("font", Lit::Font(st.font.clone())),
        ("size", st.tsize.into()),
        ("color", Lit::Color(color)),
        ("at", v2(st.center[0] + dx, st.center[1] + dy)),
        ("align", Lit::Id("center")),
        ("valign", Lit::Id("center")),
        ("fit", st.fit.into()),
    ];
    fields.extend(extra);
    layer(cx, "Text", fields)
}

/// The pop a word comes in with.
fn pop_keys(cx: &mut Cx, every: f64) -> ScriptValue {
    nkeys(cx, vec![(0.0, 1.12, None), (every.min(0.2), 1.0, Some("ease_out_cubic"))])
}

/// `Words{words every start look bg color font text_size size hold}`:
/// one word per step (`every`: seconds or `beats(½)`), each in its own
/// look: `words: ["TIMING", {text: "DEPTH" look: @echo bg: #1f3dff}, …]`.
/// Looks: @plain @echo (stacked extrusion echoes) @blur (a motion smear
/// with tick marks) @split (the frame split, the word inverted per side)
/// @halftone (dots behind) @clock (a face with a sweeping hand) @diamond
/// @outline.
pub(crate) fn words(cx: &mut Cx, o: ScriptObject) -> Made {
    let (doc, bpm) = (cx.doc, cx.bpm);
    let Some(list) = oitems(cx, o, "words") else {
        return Err(("words".into(), "`words: [\"TIMING\", {text: \"DEPTH\" look: @echo}, …]`".into()));
    };
    let size = pair(cx, o, "size", doc).unwrap_or(doc);
    let every = match obj::opt(cx.vm, o, "every") {
        None => 30.0 / bpm.max(1.0),
        Some(v) if v.is_nil() => 30.0 / bpm.max(1.0),
        Some(v) => seconds(cx, v, bpm).filter(|s| *s > 0.0).ok_or_else(|| ("every".to_string(), format!("`every` is seconds or `beats(0.5)`, not {}", kind(cx, v))))?,
    };
    let start = obj::opt(cx.vm, o, "start").and_then(|v| seconds(cx, v, bpm)).unwrap_or(0.0);
    let hold = onum(cx, o, "hold").unwrap_or(every);
    let default_look = obj::opt(cx.vm, o, "look").and_then(obj::ident).unwrap_or_else(|| "plain".into());
    let palette: [(u32, u32); 6] = [(0xf0ebe0ff, 0x111111ff), (0x0b0b0bff, 0xf0ebe0ff), (0x1f3dffff, 0xf0ebe0ff), (0xf0ebe0ff, 0x0b0b0bff), (0xb6ff3bff, 0x111111ff), (0xff5a1fff, 0x111111ff)];
    let looks = ["plain", "echo", "blur", "split", "halftone", "clock", "diamond", "outline"];
    let [w, h] = size;
    let unit = w.min(h);
    let mut slots = Vec::new();
    for (i, item) in list.iter().enumerate() {
        let item = *item;
        // The entry: a bare string, or an object of the word's fields.
        let (entry, bare): (Option<ScriptObject>, Option<String>) = if let Some(s) = obj::string(cx.vm, item) {
            (None, Some(s))
        } else if let Some(eo) = plain(cx, item) {
            (Some(eo), None)
        } else {
            return Err((format!("words[{i}]"), format!("a word is \"TEXT\" or `{{text: \"TEXT\" look: @echo bg: #…}}`, not {}", kind(cx, item))));
        };
        let eget = |cx: &Cx, key: &str| entry.and_then(|e| obj::opt(cx.vm, e, key));
        if let Some(e) = entry {
            if let Some((key, _, _)) = obj::fields(cx.vm, e).into_iter().find(|(k, _, _)| !WORD_FIELDS.contains(&k.as_str())) {
                return Err((format!("words[{i}].{key}"), format!("a word has no `{key}`; it takes {}", WORD_FIELDS.join(" "))));
            }
        }
        let text = match bare {
            Some(s) => s,
            None => match eget(cx, "text").and_then(|v| obj::string(cx.vm, v)) {
                Some(t) => t,
                None => return Err((format!("words[{i}]"), "a word needs its `text`".into())),
            },
        };
        let look = eget(cx, "look").and_then(obj::ident).unwrap_or_else(|| default_look.clone());
        if !looks.contains(&look.as_str()) {
            return Err((format!("words[{i}].look"), format!("`@{look}` is not a word look; they are {}", looks.iter().map(|l| format!("@{l}")).collect::<Vec<_>>().join(" "))));
        }
        let (bg0, ink0) = palette[i % palette.len()];
        let bg = color_of(eget(cx, "bg").or(obj::opt(cx.vm, o, "bg")), bg0);
        let ink = color_of(eget(cx, "color").or(obj::opt(cx.vm, o, "color")), ink0);
        let font = match eget(cx, "font").or(obj::opt(cx.vm, o, "font")).filter(|v| !v.is_nil()) {
            Some(v) => crate::font_spec::font_spec(cx.vm, v).map_err(|e| (format!("words[{i}].font"), e))?,
            None => crate::font_spec::FontSpec { family: "Anton", weight: 800.0, italic: false, axes: Vec::new() },
        };
        let tsize = eget(cx, "size").or(obj::opt(cx.vm, o, "text_size")).and_then(|v| obj::num(cx.vm, v)).unwrap_or(unit * 0.16);
        let center = [w * 0.5, h * 0.5];
        let st = WordStyle { text: text.clone(), font, tsize, center, fit: w * 0.86 };
        let mut parts = vec![layer(cx, "Rect", vec![("size", v2(w, h)), ("fill", Lit::Color(bg))])];
        match look.as_str() {
            "echo" => {
                for k in (1..=5).rev() {
                    let off = tsize * 0.035 * k as f64;
                    parts.push(word_layer(cx, &st, shade(ink, 1.0 - 0.14 * k as f64), off, off, vec![("opacity", (1.0 - 0.15 * k as f64).into())]));
                }
                let pop = pop_keys(cx, every);
                parts.push(word_layer(cx, &st, ink, 0.0, 0.0, vec![("scale", pop.into())]));
            }
            "blur" => {
                let tick = layer(cx, "Rect", vec![("size", v2((unit * 0.003).max(1.5), unit * 0.03)), ("fill", Lit::Color(ink)), ("opacity", 0.6.into())]);
                parts.push(layer(cx, "Repeat", vec![("count", 21.0.into()), ("layout", Lit::Id("grid")), ("columns", 21.0.into()), ("spacing", v2(w * 0.6 / 20.0, 0.0)), ("at", v2(w * 0.2, center[1] + tsize * 0.75)),
                    ("layer", tick.into())]));
                let zero: ScriptValue = 0.0.into();
                let x = tween(cx, (-w * 0.12).into(), 0.0.into(), zero, every * 0.8, "ease_out_expo");
                let length = obj::make(cx.vm, "Tween", &[("from", (unit * 0.08).into()), ("to", 0.0.into()), ("start", 0.0.into()), ("end", (every * 0.8).into()), ("ease", obj::id("ease_out_cubic"))]);
                let blur = effect(cx, "directional_blur", vec![("angle", 0.0.into()), ("length", length.into())]);
                let effects = array(cx, vec![blur]);
                parts.push(word_layer(cx, &st, ink, 0.0, 0.0, vec![("x", x.into()), ("effects", effects.into())]));
            }
            "split" => {
                let other = color_of(eget(cx, "color2"), ink);
                parts.push(layer(cx, "Rect", vec![("size", v2(w * 0.5, h)), ("at", v2(w * 0.5, 0.0)), ("fill", Lit::Color(other))]));
                for (x, color) in [(0.0, ink), (w * 0.5, bg)] {
                    let mask = layer(cx, "Rect", vec![("at", v2(x, 0.0)), ("size", v2(w * 0.5, h))]);
                    let pop = pop_keys(cx, every);
                    let half = word_layer(cx, &st, color, 0.0, 0.0, vec![("scale", pop.into())]);
                    let half = array(cx, vec![half]);
                    parts.push(layer(cx, "Group", vec![("size", v2(w, h)), ("mask", mask.into()), ("layers", half.into())]));
                }
            }
            "halftone" => {
                parts.push(layer(cx, "Pattern", vec![("kind", Lit::Id("dots")), ("color", Lit::Color(ink)), ("cell", (unit * 0.025).into()), ("weight", 0.45.into()), ("angle", 45.0.into()), ("size", v2(w, h)), ("opacity", 0.85.into())]));
                let pop = pop_keys(cx, every);
                parts.push(word_layer(cx, &st, ink, 0.0, 0.0, vec![("scale", pop.into()), ("plate", Lit::Color(bg)), ("plate_pad", v2(tsize * 0.2, tsize * 0.05))]));
            }
            "clock" => {
                let r = unit * 0.34;
                parts.push(layer(cx, "Ellipse", vec![("at", v2(center[0] - r, center[1] - r)), ("size", v2(r * 2.0, r * 2.0)), ("fill", Lit::Color(0xf7c9cfff)), ("opacity", 0.8.into())]));
                let zero: ScriptValue = 0.0.into();
                let sweep = tween(cx, 0.0.into(), 360.0.into(), zero, every.max(0.01) * 2.0, "linear");
                parts.push(layer(cx, "Rect", vec![("at", v2(center[0] - unit * 0.004, center[1] - r * 0.92)), ("size", v2(unit * 0.008, r * 0.92)), ("fill", Lit::Color(ink)), ("anchor", v2(0.5, 1.0)),
                    ("rotate", sweep.into())]));
                let pop = pop_keys(cx, every);
                parts.push(word_layer(cx, &st, ink, 0.0, 0.0, vec![("scale", pop.into())]));
            }
            "diamond" => {
                let d = unit * 0.2;
                let grow = nkeys(cx, vec![(0.0, 0.0, None), (every.min(0.2), 1.0, Some("ease_out_back"))]);
                parts.push(layer(cx, "Rect", vec![("at", v2(center[0] - d * 0.5, center[1] - d * 0.5)), ("size", v2(d, d)), ("fill", Lit::Color(0xff5a1fff)), ("rotate", 45.0.into()),
                    ("scale", grow.into())]));
                parts.push(word_layer(cx, &st, ink, 0.0, unit * 0.2, vec![("size", (tsize * 0.45).into())]));
            }
            "outline" => {
                parts.push(word_layer(cx, &st, ink, 0.0, 0.0, vec![("animate", Lit::Id("outline_fill")), ("speed", (0.6 / every).max(1.0).into())]));
            }
            _ => {
                let pop = pop_keys(cx, every);
                parts.push(word_layer(cx, &st, ink, 0.0, 0.0, vec![("scale", pop.into())]));
            }
        }
        let length = if i + 1 == list.len() { hold } else { every };
        let span = obj::array(cx.vm, [ScriptValue::from(start + every * i as f64), ScriptValue::from(length)]);
        let parts = array(cx, parts);
        slots.push(layer(cx, "Group", vec![
            ("name", format!("word {i} {text}").into()),
            ("span", span.into()),
            ("layers", parts.into()),
        ]));
    }
    let slots = array(cx, slots);
    out_group(cx, o, &["name", "at", "x", "y", "scale", "rotate", "anchor", "opacity", "span", "blend", "visible", "locked", "effects"], vec![("layers", slots)])
}

/// `Shorts{tag headline accent inset inset_size meta clip total floor ink
/// accent_color background}`: the vertical "screen within a screen" frame
/// (fractions of the height): a tiny tag at 0.02; a headline band at
/// 0.08–0.20 (`headline: ["ONE PROMPT.", "IT MADE THIS"]`, line `accent`
/// in the accent colour with a glow, slammed in); the 16:9 inset at
/// 0.36–0.66 (93% of the width, an accent border with a glow; `inset` is
/// any layer designed at `inset_size`, 1920×1080 by default, scaled in);
/// the meta band at 0.67–0.74 (a live dot, `meta` as a label, a progress
/// bar and an "x.x / N s" counter following the layer named `clip`, or
/// the document); a perspective grid floor below 0.78; a thin progress
/// line at 0.95. Type stays between 0.08 and 0.74 of the height.
pub(crate) fn shorts(cx: &mut Cx, o: ScriptObject) -> Made {
    let (doc, duration) = (cx.doc, cx.duration);
    let [w, h] = doc;
    let ink = ocolor(cx, o, "ink", 0xf0ebe0ff);
    let accent = ocolor(cx, o, "accent_color", 0xff5a1fff);
    let bg = ocolor(cx, o, "background", 0x0d0b0aff);
    let mut layers = vec![
        layer(cx, "Rect", vec![("name", "ground".into()), ("size", v2(w, h)), ("fill", Lit::Color(bg))]),
        layer(cx, "Ellipse", vec![("name", "glow".into()), ("at", v2(w * 0.5 - w * 0.7, h * 0.52 - w * 0.7)), ("size", v2(w * 1.4, w * 1.4)), ("fill", Lit::Color(0x5a2a14ff)), ("opacity", 0.55.into()), ("blur", (w * 0.22).into())]),
    ];
    if get_bool(cx, o, "floor", true) {
        layers.push(layer(cx, "GridFloor", vec![("at", v2(0.0, h * 0.78)), ("size", v2(w, h * 0.22)), ("color", Lit::Color(accent)), ("horizon", 0.04.into()), ("speed", 1.5.into())]));
    }
    if let Some(tag) = get_text(cx, o, "tag") {
        layers.push(hud_label(cx, &tag, [w * 0.5, h * 0.02 + w * 0.012], (w * 0.016).round(), obj::color(ink), "center"));
    }
    // The headline band.
    let lines: Vec<String> = match obj::opt(cx.vm, o, "headline") {
        None => Vec::new(),
        Some(v) if v.is_nil() => Vec::new(),
        Some(v) => {
            if let Some(s) = obj::string(cx.vm, v) {
                s.split('\n').map(str::to_string).collect()
            } else if let Some(items) = obj::items(cx.vm, v) {
                items.iter().filter_map(|v| obj::string(cx.vm, *v)).collect()
            } else {
                return Err(("headline".into(), format!("`headline` is a line or a list of lines, not {}", kind(cx, v))));
            }
        }
    };
    let accent_line = onum(cx, o, "accent").map(|n| n as usize);
    let hs = (h * 0.052).round();
    let band = (h * 0.08, h * 0.20);
    let step = hs * 1.3;
    let top = (band.0 + band.1) * 0.5 - step * (lines.len() as f64 - 1.0) * 0.5;
    for (i, line) in lines.iter().enumerate() {
        let hot = accent_line == Some(i);
        let mut fields: Vec<(&str, Lit)> = vec![
            ("text", line.to_uppercase().into()),
            ("font", face("Anton", 400.0, false)),
            ("size", hs.into()),
            ("color", Lit::Color(if hot { accent } else { ink })),
            ("at", v2(w * 0.5, top + step * i as f64)),
            ("align", Lit::Id("center")),
            ("valign", Lit::Id("center")),
            ("fit", (w * 0.86).into()),
            ("animate", Lit::Id("zoom_slam")),
            ("start", (0.1 + 0.25 * i as f64).into()),
        ];
        if hot {
            let glow = effect(cx, "glow", vec![("threshold", 0.2.into()), ("radius", (hs * 0.35).into()), ("intensity", 1.2.into())]);
            let effects = array(cx, vec![glow]);
            fields.push(("effects", effects.into()));
        }
        layers.push(layer(cx, "Text", fields));
    }
    // The inset: a 16:9 panel with an accent border and a glow.
    let iw = w * 0.93;
    let ih = iw * 9.0 / 16.0;
    let (ix, iy) = ((w - iw) * 0.5, h * 0.51 - ih * 0.5);
    let design = pair(cx, o, "inset_size", [1920.0, 1080.0]).unwrap_or([1920.0, 1080.0]);
    if let Some(content) = obj::opt(cx.vm, o, "inset") {
        let scale = (iw / design[0]).min(ih / design[1]);
        let mask = layer(cx, "Rect", vec![("size", v2(iw, ih)), ("radius", (w * 0.01).into())]);
        let content = array(cx, vec![content]);
        let scaled = layer(cx, "Group", vec![("scale", scale.into()), ("anchor", v2(0.0, 0.0)), ("size", v2(design[0], design[1])), ("layers", content.into())]);
        let scaled = array(cx, vec![scaled]);
        layers.push(layer(cx, "Group", vec![
            ("name", "inset".into()),
            ("at", v2(ix, iy)),
            ("size", v2(iw, ih)),
            ("mask", mask.into()),
            ("layers", scaled.into()),
        ]));
    }
    let glow = effect(cx, "glow", vec![("threshold", 0.1.into()), ("radius", (w * 0.02).into()), ("intensity", 1.0.into())]);
    let glow = array(cx, vec![glow]);
    layers.push(layer(cx, "Rect", vec![("name", "inset frame".into()), ("at", v2(ix, iy)), ("size", v2(iw, ih)), ("radius", (w * 0.01).into()), ("stroke", Lit::Color(accent)), ("width", (w * 0.002).max(2.0).into()),
        ("effects", glow.into())]));
    // The meta band: live dot, label, bar, counter.
    let my = h * 0.705;
    let ls = (w * 0.018).round();
    let dot = ls * 0.6;
    let inner = nkeys(cx, vec![(0.0, 1.0, None), (0.6, 0.3, Some("hold"))]);
    let blink = forever(cx, inner, 1.2);
    layers.push(layer(cx, "Ellipse", vec![("name", "live".into()), ("at", v2(ix, my - dot * 0.5)), ("size", v2(dot, dot)), ("fill", Lit::Color(0xff3b30ff)),
        ("opacity", blink.into())]));
    if let Some(meta) = get_text(cx, o, "meta") {
        let mut fields = label_fields(Some(&meta), [ix + dot * 1.8, my], ls, obj::color(ink), "left");
        fields.retain(|(k, _)| *k != "opacity");
        fields.push(("opacity", 0.75.into()));
        layers.push(layer(cx, "Text", fields));
    }
    let total = onum(cx, o, "total");
    let zero: ScriptValue = 0.0.into();
    let (count, bar) = match get_text(cx, o, "clip") {
        Some(name) => {
            let name = obj::text(cx.vm, &name);
            let from = obj::v2(cx.vm, 0.0, 1.0);
            let to = obj::v2(cx.vm, 1.0, 1.0);
            (
                obj::make(cx.vm, "Playhead", &[("name", name)]),
                obj::make(cx.vm, "Progress", &[("name", name), ("from", from), ("to", to)]),
            )
        }
        None => (obj::id("doc"), tween(cx, v2(0.0, 1.0), v2(1.0, 1.0), zero, duration, "linear")),
    };
    let bar_y = my + ls * 1.4;
    layers.push(layer(cx, "Rect", vec![("name", "track".into()), ("at", v2(ix, bar_y)), ("size", v2(iw * 0.7, (h * 0.003).max(3.0))), ("fill", Lit::Color(ink)), ("opacity", 0.15.into())]));
    layers.push(layer(cx, "Rect", vec![("name", "progress".into()), ("at", v2(ix, bar_y)), ("size", v2(iw * 0.7, (h * 0.003).max(3.0))), ("fill", Lit::Color(accent)), ("anchor", v2(0.0, 0.5)), ("scale", bar.into())]));
    let mut fields = label_fields(None, [ix + iw, bar_y], ls, obj::color(ink), "right");
    fields.push(("number", count.into()));
    fields.push(("format", Lit::Id("seconds")));
    fields.push(("total", total.unwrap_or(duration).into()));
    fields.push(("tabular", true.into()));
    layers.push(layer(cx, "Text", fields));
    // The whole Short's progress, along the bottom.
    let whole = tween(cx, v2(0.0, 1.0), v2(1.0, 1.0), zero, duration, "linear");
    layers.push(layer(cx, "Rect", vec![("name", "short progress".into()), ("at", v2(0.0, h * 0.95)), ("size", v2(w * 0.5, (h * 0.002).max(2.0))), ("fill", Lit::Color(accent)), ("anchor", v2(0.0, 0.5)),
        ("scale", whole.into())]));
    let layers = array(cx, layers);
    let vignette = effect(cx, "vignette", vec![("amount", 0.45.into()), ("feather", 0.7.into())]);
    let effects = array(cx, vec![vignette]);
    out_group(cx, o, &["name", "opacity", "span", "visible", "locked"], vec![("layers", layers), ("effects", effects)])
}

fn mono(cx: &mut Cx, text: &str, at: [f64; 2], size: f64, color: u32, extra: Vec<(&str, Lit)>) -> ScriptValue {
    let mut fields: Vec<(&str, Lit)> = vec![
        ("text", text.into()),
        ("font", face("JetBrains Mono", 400.0, false)),
        ("size", size.into()),
        ("color", Lit::Color(color)),
        ("at", v2(at[0], at[1])),
    ];
    fields.retain(|(k, _)| !extra.iter().any(|(e, _)| e == k));
    fields.extend(extra);
    layer(cx, "Text", fields)
}

fn hold_keys(cx: &mut Cx, points: Vec<(f64, f64)>) -> ScriptValue {
    nkeys(cx, points.into_iter().enumerate().map(|(i, (t, v))| (t, v, if i == 0 { None } else { Some("hold") })).collect())
}

/// `TokenStream{prompt tokens times|every|words+line start header candidates
/// decoys top_k accent ink panel size enter}`: the next-token UI. A prompt
/// line types itself token by token behind a `>` with a block caret; above
/// it a `p(next | context)` table ranks the candidates for each step, the
/// sampled one lit in the accent; `enter: true` ends on ↵.
/// `times` (seconds, one per token) come from the author, or from a voice
/// (`words: voice(@vo) line: i`, resolved by the loader), else `every`.
pub(crate) fn token_stream(cx: &mut Cx, o: ScriptObject, voice_times: Option<Vec<f64>>) -> Made {
    let doc = cx.doc;
    let Some(prompt) = get_text(cx, o, "prompt") else {
        return Err(("prompt".into(), "a TokenStream types its `prompt: \"Lanterns stitch the last boats home\"`".into()));
    };
    let tokens: Vec<String> = match oitems(cx, o, "tokens") {
        Some(items) => items.iter().filter_map(|v| obj::string(cx.vm, *v)).collect(),
        None => {
            // Words, each keeping the space before the next.
            let mut out = Vec::new();
            let words: Vec<&str> = prompt.split(' ').collect();
            for (i, w) in words.iter().enumerate() {
                out.push(if i + 1 < words.len() { format!("{w} ") } else { w.to_string() });
            }
            out
        }
    };
    if tokens.concat() != *prompt {
        return Err(("tokens".into(), "the `tokens` joined must spell the `prompt` exactly".into()));
    }
    let start = onum(cx, o, "start").unwrap_or(0.2);
    let every = onum(cx, o, "every").unwrap_or(0.32);
    let times: Vec<f64> = match (oitems(cx, o, "times"), voice_times) {
        (Some(items), _) => items.iter().filter_map(|v| obj::num(cx.vm, *v)).collect(),
        (_, Some(t)) => t,
        _ => (0..tokens.len()).map(|k| start + every * k as f64).collect(),
    };
    if times.len() < tokens.len() {
        return Err(("times".into(), format!("{} tokens but {} times", tokens.len(), times.len())));
    }
    let size = pair(cx, o, "size", [doc[0] * 0.62, doc[1] * 0.56]).unwrap_or([doc[0] * 0.62, doc[1] * 0.56]);
    let [w, h] = size;
    let unit = w.min(h);
    let accent = ocolor(cx, o, "accent", 0xff5a1fff);
    let ink = ocolor(cx, o, "ink", 0xf2efe8ff);
    let panel = ocolor(cx, o, "panel", 0x121214ee);
    let muted = (ink & 0xffffff00) | 0x80;
    let top_k = onum(cx, o, "top_k").unwrap_or(5.0).clamp(1.0, 10.0) as usize;
    let fs = (unit * 0.052).round();
    let pad = unit * 0.06;
    let decoys: Vec<String> = match oitems(cx, o, "decoys") {
        Some(items) => items.iter().filter_map(|v| obj::string(cx.vm, *v)).collect(),
        None => ["the", "a", "you", "please", "I", "it", "we", "now", "then", "so"].iter().map(|s| s.to_string()).collect(),
    };
    let header = get_text(cx, o, "header").unwrap_or_else(|| "p(next | context)".into());
    let mut layers = vec![
        layer(cx, "Rect", vec![("name", "panel".into()), ("size", v2(w, h)), ("radius", (unit * 0.02).into()), ("fill", Lit::Color(panel)), ("stroke", Lit::Color(accent)), ("width", (unit * 0.003).max(1.5).into())]),
        mono(cx, &header, [pad, pad], fs * 0.7, muted, vec![("tracking", (fs * 0.08).into())]),
    ];
    // The table: DV1's `Chart{type: @probs}`, one step per token, the
    // sampled token picked.
    let table_top = pad + fs * 1.6;
    let candidates = oitems(cx, o, "candidates");
    let mut steps: Vec<ScriptValue> = Vec::new();
    for (k, token) in tokens.iter().enumerate() {
        let given = candidates.as_ref().and_then(|c| c.get(k).copied()).and_then(|v| obj::items(cx.vm, v));
        let rows: Vec<ScriptValue> = match given {
            Some(pairs) => {
                let mut rows = Vec::new();
                for p in pairs {
                    let Some(kv) = obj::items(cx.vm, p).filter(|kv| kv.len() == 2) else { continue };
                    let (Some(label), Some(value)) = (obj::string(cx.vm, kv[0]), obj::num(cx.vm, kv[1])) else { continue };
                    let name = obj::text(cx.vm, &label);
                    rows.push(obj::record(cx.vm, &[("label", name), ("value", value.into()), ("pick", (label.trim() == token.trim()).into())]));
                }
                rows
            }
            None => {
                // Made up: the sampled token leads, seeded decoys follow.
                let lead = 0.38 + 0.45 * hash01(7, k, 1);
                let mut rest = 1.0 - lead;
                let name = obj::text(cx.vm, token.trim());
                let mut out = vec![obj::record(cx.vm, &[("label", name), ("value", ((lead * 100.0).round() / 100.0).into()), ("pick", true.into())])];
                for j in 0..top_k.saturating_sub(1) {
                    let share = rest * (0.35 + 0.3 * hash01(7, k, 2 + j as u32));
                    rest -= share;
                    let decoy = obj::text(cx.vm, &decoys[(k * 3 + j) % decoys.len()]);
                    out.push(obj::array(cx.vm, [decoy, ((share * 100.0).round() / 100.0).into()]));
                }
                out
            }
        };
        steps.push(array(cx, rows));
    }
    let mut chart: Vec<(&str, Lit)> = vec![
        ("type", Lit::Id("probs")),
        ("at", v2(pad, table_top)),
        ("size", v2(w - pad * 2.0, h - table_top - pad - fs * 2.6)),
        ("top_k", (top_k as f64).into()),
        ("header", "p(next | context)".into()),
        ("accent", Lit::Color(accent)),
        ("start", times[0].into()),
        ("data", steps[0].into()),
    ];
    if steps.len() > 1 {
        let next = array(cx, steps[1..].to_vec());
        let switch = array(cx, times[1..tokens.len()].iter().map(|t| ScriptValue::from(*t)).collect());
        chart.push(("next", next.into()));
        chart.push(("switch", switch.into()));
    }
    layers.push(layer(cx, "Chart", chart));
    // The prompt line: typed token by token, a block caret after it.
    let line_y = h - pad - fs * 1.2;
    layers.push(layer(cx, "Rect", vec![("at", v2(pad, line_y - fs * 0.5)), ("size", v2(w - pad * 2.0, 1.0)), ("fill", Lit::Color((ink & 0xffffff00) | 0x30))]));
    layers.push(mono(cx, ">", [pad, line_y], fs, accent, vec![]));
    // Nothing typed at 0, then each token at its time; a token at 0
    // replaces that key rather than repeating its time (keys must
    // strictly increase).
    let mut typed: Vec<(f64, f64)> = vec![(0.0, 0.0)];
    let mut count = 0usize;
    for (k, token) in tokens.iter().enumerate() {
        count += token.chars().count();
        let key = (times[k], count as f64 + 0.01);
        match typed.last_mut() {
            Some(last) if last.0 == key.0 => *last = key,
            _ => typed.push(key),
        }
    }
    let enter = get_bool(cx, o, "enter", false);
    let text = if enter { format!("{prompt} ↵") } else { prompt.clone() };
    if enter {
        let last = *times.last().unwrap_or(&0.0) + 0.35;
        typed.push((last, text.chars().count() as f64 + 0.01));
    }
    let blink = nkeys(cx, vec![(0.0, 1.0, None), (0.5, 0.0, Some("hold")), (1.0, 1.0, Some("hold"))]);
    let caret_blink = forever(cx, blink, 1.0);
    let reveal = hold_keys(cx, typed);
    layers.push(mono(cx, &text, [pad + fs * 1.3, line_y], fs, ink, vec![
        ("reveal", reveal.into()),
        ("caret", caret_blink.into()),
        ("caret_color", Lit::Color(accent)),
        ("caret_style", Lit::Id("block")),
        ("wrap", (w - pad * 2.0 - fs * 1.3).into()),
    ]));
    let page = obj::v2(cx.vm, w, h);
    let layers = array(cx, layers);
    out_group(cx, o, BOX_KEEP, vec![("size", page), ("layers", layers)])
}

/// A rubber stamp: a red double-outlined box with the word, slammed on
/// (big → overshoot → down) and slightly turned.
fn stamp(cx: &mut Cx, word: &str, at: [f64; 2], unit: f64, color: u32, t: f64, tilt: f64) -> ScriptValue {
    let fs = unit * 0.075;
    let w = word.chars().count() as f64 * fs * 0.62 + fs * 1.2;
    let h = fs * 1.7;
    let opacity = nkeys(cx, vec![(t, 0.0, None), (t + 0.02, 0.92, None)]);
    let scale = nkeys(cx, vec![(t, 1.8, None), (t + 0.12, 0.94, Some("ease_in_cubic")), (t + 0.22, 1.0, Some("ease_out_sine"))]);
    let outer = layer(cx, "Rect", vec![("size", v2(w, h)), ("radius", (fs * 0.12).into()), ("stroke", Lit::Color(color)), ("width", (fs * 0.12).into())]);
    let inner = layer(cx, "Rect", vec![("at", v2(fs * 0.16, fs * 0.16)), ("size", v2(w - fs * 0.32, h - fs * 0.32)), ("radius", (fs * 0.08).into()), ("stroke", Lit::Color(color)), ("width", (fs * 0.04).into())]);
    let text = layer(cx, "Text", vec![("text", word.to_uppercase().into()), ("font", face("Anton", 400.0, false)), ("size", fs.into()), ("color", Lit::Color(color)), ("at", v2(w * 0.5, h * 0.5)), ("align", Lit::Id("center")), ("valign", Lit::Id("center")), ("fit", (w * 0.86).into())]);
    let parts = array(cx, vec![outer, inner, text]);
    layer(cx, "Group", vec![
        ("name", format!("stamp {word}").into()),
        ("at", v2(at[0] - w * 0.5, at[1] - h * 0.5)),
        ("size", v2(w, h)),
        ("rotate", tilt.into()),
        ("opacity", opacity.into()),
        ("scale", scale.into()),
        ("layers", parts.into()),
    ])
}

/// `Form{title subtitle fields checks signature stamp stamp2 start every size
/// ink paper stamp_color}`: a paper form filled in by an invisible
/// typewriter, field by field; a row of check boxes with the chosen one
/// crossed; a handwritten signature; rubber stamps slammed on.
pub(crate) fn form(cx: &mut Cx, o: ScriptObject) -> Made {
    let doc = cx.doc;
    let size = pair(cx, o, "size", [doc[1] * 0.75, doc[1] * 0.92]).unwrap_or([doc[1] * 0.75, doc[1] * 0.92]);
    let [w, h] = size;
    let unit = w.min(h);
    let ink = ocolor(cx, o, "ink", 0x1b1b1bff);
    let paper = ocolor(cx, o, "paper", 0xf4efe2ff);
    let red = ocolor(cx, o, "stamp_color", 0xd2362bff);
    let start = onum(cx, o, "start").unwrap_or(0.3);
    let every = onum(cx, o, "every").unwrap_or(0.45);
    let pad = w * 0.08;
    let fs = unit * 0.03;
    let shadow = effect(cx, "drop_shadow", vec![("distance", (unit * 0.012).into()), ("radius", (unit * 0.03).into()), ("opacity", 0.45.into())]);
    let grain = effect(cx, "grain", vec![("amount", 0.05.into())]);
    let effects = array(cx, vec![shadow, grain]);
    let mut layers = vec![
        layer(cx, "Rect", vec![("name", "paper".into()), ("size", v2(w, h)), ("fill", Lit::Color(paper)), ("effects", effects.into())]),
        layer(cx, "Rect", vec![("at", v2(pad, pad)), ("size", v2(w - pad * 2.0, 3.0)), ("fill", Lit::Color(ink))]),
    ];
    // The title wraps: estimate its lines (a mono cell is 0.6 em, the em
    // 4/3 of the size) so what follows starts below it.
    let mut y = pad + fs * 0.9;
    if let Some(title) = get_text(cx, o, "title") {
        let cell = fs * 1.1 * 4.0 / 3.0 * 0.6;
        let per_line = ((w - pad * 2.0) / cell).floor().max(1.0);
        let lines = (title.chars().count() as f64 / per_line).ceil().max(1.0);
        layers.push(mono(cx, &title.to_uppercase(), [pad, y], fs * 1.1, ink, vec![("font", face("JetBrains Mono", 700.0, false)), ("wrap", (w - pad * 2.0).into())]));
        y += lines * fs * 1.1 * 4.0 / 3.0 * 1.25;
    }
    if let Some(sub) = get_text(cx, o, "subtitle") {
        layers.push(mono(cx, &sub, [pad, y + fs * 0.2], fs * 0.75, (ink & 0xffffff00) | 0x99, vec![]));
        y += fs * 1.6;
    }
    y += fs * 1.4;
    let mut t = start;
    if let Some(fields) = oitems(cx, o, "fields") {
        for f in fields {
            let Some(kv) = obj::items(cx.vm, f) else { continue };
            let (Some(label), Some(value)) = (kv.first().and_then(|v| obj::string(cx.vm, *v)), kv.get(1).and_then(|v| obj::string(cx.vm, *v))) else { continue };
            layers.push(mono(cx, &label.to_uppercase(), [pad, y], fs * 0.7, (ink & 0xffffff00) | 0x99, vec![("tracking", (fs * 0.1).into())]));
            layers.push(layer(cx, "Rect", vec![("at", v2(pad, y + fs * 2.3)), ("size", v2(w - pad * 2.0, 1.2)), ("fill", Lit::Color((ink & 0xffffff00) | 0x66))]));
            let chars = value.chars().count().max(1) as f64;
            let reveal = tween(cx, 0.0.into(), 1.0.into(), t.into(), (chars / 28.0).max(0.12), "linear");
            layers.push(mono(cx, &value, [pad + fs * 0.3, y + fs * 1.05], fs * 1.05, ink, vec![
                ("reveal", reveal.into()),
            ]));
            y += fs * 3.4;
            t += every.max(chars / 28.0 + 0.1);
        }
    }
    if let Some(checks) = obj::opt(cx.vm, o, "checks").and_then(|v| plain(cx, v)) {
        let label = get_text(cx, checks, "label").unwrap_or_else(|| "ASSESSMENT".into());
        let pick = onum(cx, checks, "pick").unwrap_or(0.0) as usize;
        layers.push(mono(cx, &label.to_uppercase(), [pad, y], fs * 0.7, (ink & 0xffffff00) | 0x99, vec![("tracking", (fs * 0.1).into())]));
        let options: Vec<String> = match oitems(cx, checks, "options") {
            Some(list) => list.iter().filter_map(|v| obj::string(cx.vm, *v)).collect(),
            None => Vec::new(),
        };
        let mut x = pad;
        let bx = fs * 1.1;
        for (i, option) in options.iter().enumerate() {
            let oy = y + fs * 1.4;
            layers.push(layer(cx, "Rect", vec![("at", v2(x, oy)), ("size", v2(bx, bx)), ("stroke", Lit::Color(ink)), ("width", 2.0.into())]));
            layers.push(mono(cx, &option.to_uppercase(), [x + bx * 1.35, oy + bx * 0.1], fs * 0.8, ink, vec![]));
            if i == pick {
                let trim = tween(cx, 0.0.into(), 1.0.into(), t.into(), 0.25, "ease_out_cubic");
                layers.push(layer(cx, "Path", vec![("svg", format!("M 0 0 L {bx} {bx} M {bx} 0 L 0 {bx}").into()), ("at", v2(x, oy)), ("stroke", Lit::Color(ink)), ("width", 3.0.into()), ("cap", Lit::Id("round")),
                    ("trim_end", trim.into())]));
            }
            x += bx * 1.6 + option.chars().count() as f64 * fs * 0.8 * 0.62 + fs * 1.2;
        }
        y += fs * 4.0;
        t += every + 0.25;
    }
    if let Some(sig) = get_text(cx, o, "signature") {
        layers.push(mono(cx, "SIGNATURE", [pad, y], fs * 0.7, (ink & 0xffffff00) | 0x99, vec![("tracking", (fs * 0.1).into())]));
        layers.push(layer(cx, "Rect", vec![("at", v2(pad, y + fs * 3.6)), ("size", v2(w * 0.5, 1.2)), ("fill", Lit::Color(ink))]));
        layers.push(layer(cx, "Text", vec![("text", sig.into()), ("font", face("LXGW WenKai", 700.0, true)), ("size", (fs * 2.2).into()), ("color", Lit::Color(0x1d3a8aff)),
            ("at", v2(pad + fs, y + fs * 0.9)), ("rotate", (-4.0).into()), ("animate", Lit::Id("wipe")), ("start", t.into())]));
        t += every + 0.4;
    }
    if let Some(word) = get_text(cx, o, "stamp") {
        layers.push(stamp(cx, &word, [w * 0.66, h * 0.72], unit, red, t, -9.0));
        t += every + 0.35;
    }
    if let Some(word) = get_text(cx, o, "stamp2") {
        layers.push(stamp(cx, &word, [w * 0.35, h * 0.86], unit * 0.8, red, t, 6.0));
    }
    let page = obj::v2(cx.vm, w, h);
    let layers = array(cx, layers);
    out_group(cx, o, BOX_KEEP, vec![("size", page), ("layers", layers)])
}

/// `CodeDraw{code layers every start split size ink muted panel}`: code
/// typed line by line on the left (comments muted), and on the right the
/// drawing it makes: `layers[i]` draws on (a stroke traces; anything else
/// pops in) as code line i is typed.
pub(crate) fn code_draw(cx: &mut Cx, o: ScriptObject) -> Made {
    let doc = cx.doc;
    let Some(code) = get_text(cx, o, "code") else {
        return Err(("code".into(), "a CodeDraw types its `code: \"\\\\draw (0,0) ellipse (2 and 1); % body\\n…\"`".into()));
    };
    let size = pair(cx, o, "size", doc).unwrap_or(doc);
    let [w, h] = size;
    let split = onum(cx, o, "split").unwrap_or(0.42);
    let start = onum(cx, o, "start").unwrap_or(0.2);
    let every = onum(cx, o, "every").unwrap_or(0.45);
    let ink = ocolor(cx, o, "ink", 0xf2efe8ff);
    let muted = ocolor(cx, o, "muted", 0x8a8f98ff);
    let unit = w.min(h);
    let fs = (unit * 0.026).round();
    let pad = unit * 0.06;
    let lines: Vec<&str> = code.lines().collect();
    let mut layers = Vec::new();
    if let Some(panel) = obj::opt(cx.vm, o, "panel") {
        layers.push(layer(cx, "Rect", vec![("size", v2(w * split, h)), ("fill", panel.into())]));
    }
    for (i, line) in lines.iter().enumerate() {
        let t = start + every * i as f64;
        // `%`, `//` and `#` start a comment.
        let cut = [line.find('%'), line.find("//"), line.find('#')].into_iter().flatten().min();
        let (body, comment) = match cut { Some(c) => (&line[..c], &line[c..]), None => (*line, "") };
        let y = pad + fs * 1.6 * i as f64;
        let chars = line.chars().count().max(1) as f64;
        let dur = (every * 0.8).min(chars / 30.0).max(0.1);
        let number = tween(cx, 0.0.into(), 1.0.into(), t.into(), 0.05, "linear");
        layers.push(mono(cx, &format!("{:>2}", i + 1), [pad * 0.4, y], fs * 0.8, (muted & 0xffffff00) | 0x80, vec![("opacity", number.into())]));
        let full = format!("{body}{comment}");
        let body_chars = body.chars().count() as f64;
        let reveal = tween(cx, 0.0.into(), (chars + 0.01).into(), t.into(), dur, "linear");
        layers.push(mono(cx, &full, [pad * 0.4 + fs * 2.2, y], fs, ink, vec![
            ("reveal", reveal.into()),
            ("by", Lit::Id("letter")),
            ("color", Lit::Color(ink)),
        ]));
        if !comment.is_empty() {
            // The comment in its own colour over the same letters.
            let reveal = tween(cx, 0.0.into(), (chars + 0.01).into(), t.into(), dur, "linear");
            let only = obj::array(cx.vm, (body_chars as usize..chars as usize).map(|k| ScriptValue::from(k as f64)).collect::<Vec<_>>());
            layers.push(layer(cx, "Text", vec![
                ("text", full.clone().into()),
                ("font", face("JetBrains Mono", 400.0, false)),
                ("size", fs.into()),
                ("at", v2(pad * 0.4 + fs * 2.2, y)),
                ("reveal", reveal.into()),
                ("by", Lit::Id("letter")),
                ("only", only.into()),
                ("color", Lit::Color(muted)),
            ]));
        }
    }
    if let Some(drawing) = oitems(cx, o, "layers") {
        let mut parts = Vec::new();
        for (i, part) in drawing.iter().enumerate() {
            let t = start + every * i as f64 + every * 0.5;
            let mut p = *part;
            if let Some(po) = plain(cx, p) {
                let traced = obj::type_name(cx.vm, p) == Some("Path") || obj::opt(cx.vm, po, "stroke").is_some();
                if traced && obj::opt(cx.vm, po, "trim_end").is_none() {
                    let trim = tween(cx, 0.0.into(), 1.0.into(), t.into(), every * 1.4, "ease_in_out_cubic");
                    let copy = obj::copy_without(cx.vm, po, &[]);
                    obj::set(cx.vm, copy, "trim_end", trim);
                    p = copy.into();
                } else if obj::opt(cx.vm, po, "opacity").is_none() {
                    let fade = tween(cx, 0.0.into(), 1.0.into(), t.into(), 0.2, "ease_out_quad");
                    let copy = obj::copy_without(cx.vm, po, &[]);
                    obj::set(cx.vm, copy, "opacity", fade);
                    p = copy.into();
                }
            }
            parts.push(p);
        }
        let parts = array(cx, parts);
        layers.push(layer(cx, "Group", vec![("name", "drawing".into()), ("at", v2(w * split, 0.0)), ("size", v2(w * (1.0 - split), h)), ("layers", parts.into())]));
    }
    let page = obj::v2(cx.vm, w, h);
    let layers = array(cx, layers);
    out_group(cx, o, BOX_KEEP, vec![("size", page), ("layers", layers)])
}

/// `UiWindow{title lines size start cps accent ink panel button}`: an OS
/// window (title bar with three dots) whose lines type in (`cps`
/// characters a second), and an optional button under them.
pub(crate) fn ui_window(cx: &mut Cx, o: ScriptObject) -> Made {
    let doc = cx.doc;
    let size = pair(cx, o, "size", [doc[0] * 0.4, doc[1] * 0.3]).unwrap_or([doc[0] * 0.4, doc[1] * 0.3]);
    let [w, h] = size;
    let unit = w.min(h);
    let ink = ocolor(cx, o, "ink", 0x1b1b1bff);
    let panel = ocolor(cx, o, "panel", 0xf6f5f2ff);
    let accent = ocolor(cx, o, "accent", 0xff5a1fff);
    let start = onum(cx, o, "start").unwrap_or(0.2);
    let cps = onum(cx, o, "cps").unwrap_or(30.0).max(1.0);
    let bar = unit * 0.16;
    let fs = (unit * 0.075).round();
    let shadow = effect(cx, "drop_shadow", vec![("distance", (unit * 0.02).into()), ("radius", (unit * 0.05).into()), ("opacity", 0.4.into())]);
    let shadow = array(cx, vec![shadow]);
    let mut layers = vec![
        layer(cx, "Rect", vec![("size", v2(w, h)), ("radius", (unit * 0.04).into()), ("fill", Lit::Color(panel)), ("effects", shadow.into())]),
        layer(cx, "Rect", vec![("size", v2(w, bar)), ("radius", (unit * 0.04).into()), ("fill", Lit::Color(0xe4e2dcff))]),
    ];
    for (k, c) in [0xff5f57ffu32, 0xfebc2eff, 0x28c840ff].iter().enumerate() {
        let d = bar * 0.34;
        layers.push(layer(cx, "Ellipse", vec![("at", v2(bar * 0.4 + k as f64 * d * 1.5, (bar - d) * 0.5)), ("size", v2(d, d)), ("fill", Lit::Color(*c))]));
    }
    if let Some(title) = get_text(cx, o, "title") {
        layers.push(layer(cx, "Text", vec![("text", title.into()), ("font", face("Inter", 600.0, false)), ("size", (fs * 0.8).into()), ("color", Lit::Color(ink)), ("at", v2(w * 0.5, bar * 0.5)), ("align", Lit::Id("center")), ("valign", Lit::Id("center")), ("fit", (w * 0.6).into())]));
    }
    let mut t = start;
    let mut y = bar + unit * 0.08;
    if let Some(lines) = oitems(cx, o, "lines") {
        for line in lines {
            let Some(line) = obj::string(cx.vm, line) else { continue };
            let chars = line.chars().count().max(1) as f64;
            let reveal = tween(cx, 0.0.into(), 1.0.into(), t.into(), chars / cps, "linear");
            layers.push(layer(cx, "Text", vec![("text", line.into()), ("font", face("Inter", 400.0, false)), ("size", fs.into()), ("color", Lit::Color(ink)), ("at", v2(unit * 0.08, y)), ("wrap", (w - unit * 0.16).into()),
                ("reveal", reveal.into())]));
            t += chars / cps + 0.15;
            y += fs * 1.6;
        }
    }
    if let Some(label) = get_text(cx, o, "button") {
        let bw = label.chars().count() as f64 * fs * 0.6 + fs * 2.0;
        let opacity = tween(cx, 0.0.into(), 1.0.into(), t.into(), 0.15, "linear");
        let pill = layer(cx, "Rect", vec![("size", v2(bw, fs * 1.9)), ("radius", (fs * 0.95).into()), ("fill", Lit::Color(accent))]);
        let text = layer(cx, "Text", vec![("text", label.into()), ("font", face("Inter", 600.0, false)), ("size", (fs * 0.85).into()), ("color", Lit::Color(0xffffffff)), ("at", v2(bw * 0.5, fs * 0.95)), ("align", Lit::Id("center")), ("valign", Lit::Id("center"))]);
        let inner = array(cx, vec![pill, text]);
        layers.push(layer(cx, "Group", vec![("at", v2(w - bw - unit * 0.08, h - fs * 2.6)), ("opacity", opacity.into()), ("layers", inner.into())]));
    }
    let page = obj::v2(cx.vm, w, h);
    let layers = array(cx, layers);
    out_group(cx, o, BOX_KEEP, vec![("size", page), ("layers", layers)])
}

/// `UiButton{label click size accent ink cursor}`: a pill button; a
/// pointer glides in and clicks it at `click` (the button presses and a
/// ring pulses).
pub(crate) fn ui_button(cx: &mut Cx, o: ScriptObject) -> Made {
    let Some(label) = get_text(cx, o, "label") else {
        return Err(("label".into(), "a UiButton needs its `label: \"⟲ Regenerate\"`".into()));
    };
    let fs = onum(cx, o, "size").unwrap_or(34.0);
    let accent = ocolor(cx, o, "accent", 0xff5a1fff);
    let ink = ocolor(cx, o, "ink", 0xf2efe8ff);
    let bw = label.chars().count() as f64 * fs * 0.6 + fs * 2.2;
    let bh = fs * 2.0;
    let click = onum(cx, o, "click");
    let press: ScriptValue = match click {
        Some(c) => nkeys(cx, vec![(c, 1.0, None), (c + 0.06, 0.93, Some("ease_out_quad")), (c + 0.2, 1.0, Some("ease_out_back"))]),
        None => 1.0.into(),
    };
    let mut layers = vec![
        layer(cx, "Rect", vec![("name", "button".into()), ("size", v2(bw, bh)), ("radius", (bh * 0.5).into()), ("stroke", Lit::Color(accent)), ("width", 2.0.into()), ("fill", Lit::Color((accent & 0xffffff00) | 0x22)),
            ("scale", press.into())]),
        layer(cx, "Text", vec![("text", label.into()), ("font", face("Inter", 600.0, false)), ("size", fs.into()), ("color", Lit::Color(ink)), ("at", v2(bw * 0.5, bh * 0.5)), ("align", Lit::Id("center")), ("valign", Lit::Id("center"))]),
    ];
    if let Some(c) = click {
        let scale = nkeys(cx, vec![(c + 0.05, 1.0, None), (c + 0.5, 1.35, Some("ease_out_cubic"))]);
        let opacity = nkeys(cx, vec![(c, 0.0, None), (c + 0.05, 0.9, None), (c + 0.5, 0.0, Some("ease_out_quad"))]);
        layers.push(layer(cx, "Rect", vec![("name", "pulse".into()), ("size", v2(bw, bh)), ("radius", (bh * 0.5).into()), ("stroke", Lit::Color(accent)), ("width", 2.0.into()),
            ("scale", scale.into()),
            ("opacity", opacity.into())]));
        if get_bool(cx, o, "cursor", true) {
            // An arrow pointer from below right to the button's middle.
            let arrow = "M 0 0 L 0 26 L 7 19 L 12 30 L 16 28 L 11 17 L 20 17 Z";
            let from = obj::v2(cx.vm, bw * 1.3, bh * 2.4);
            let to = obj::v2(cx.vm, bw * 0.55, bh * 0.55);
            let glide = obj::keys(cx.vm, vec![(ScriptValue::from(c - 0.8), from, None), (ScriptValue::from(c - 0.05), to, Some("ease_in_out_cubic"))]);
            layers.push(layer(cx, "Path", vec![("name", "cursor".into()), ("svg", arrow.into()), ("fill", Lit::Color(0xffffffff)), ("stroke", Lit::Color(0x111111ff)), ("width", 1.5.into()),
                ("scale", (fs / 22.0).into()), ("anchor", v2(0.0, 0.0)),
                ("at", glide.into())]));
        }
    }
    let page = obj::v2(cx.vm, bw, bh);
    let layers = array(cx, layers);
    out_group(cx, o, BOX_KEEP, vec![("size", page), ("layers", layers)])
}
