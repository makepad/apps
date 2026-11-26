//! `MatchMorph{steps: ["a² + b²", "b² + a²", …] times: [0.5, 2.5, …] by: @word}`:
//! a layout turns into the next by flying the pieces they share, once per
//! transition of a chain of states: `steps` are the states, `times` the
//! second each transition starts (one fewer than `steps`), `duration` (or a
//! list, `durations`) how long it runs. Between transitions a state holds
//! still. Pieces are cut by `by` (`@char`, `@word`, `@line` for text; `@char`,
//! `@id` for `math: true`, where `steps` are TeX and an `\id{name}{…}` part
//! is one piece); `by: [@id, @id, @char]` keys each transition on its own; the n-th piece of a key in one state pairs with the n-th of
//! that key in the next (`crate::pieces::pair_pieces`). Pairs travel from their
//! place to their new one while their outlines morph (aligned,
//! `crate::align`) and their colour changes; a pair whose pieces show
//! different glyphs (a 5 becoming a 6) cross-fades instead, as it would
//! morph into a blob; pieces with no partner fade: the old state's out
//! during the first half, the new state's in during the second. It lowers
//! to ordinary Paths with tweens in a group per transition and per held
//! state, so every frame is a pure function of time.
//!
//! Colours: `color` is what unstyled pieces wear, or `colors` one per step;
//! `parts`, `paths` and `chars` colour pieces as the Math layer's do
//! (`parts: {t7: {color: #ff7a1a}}`, colour only; names that a state does
//! not have are ignored), and a pair's colours mix. `enter_color` is what
//! arriving pieces wear as they fade in; it settles to the piece's own
//! colour over `settle` seconds after the fade (the next transition may not
//! start before that is over).
//!
//! Fields: `steps times durations by font size tracking color colors
//! enter_color settle parts paths chars align duration ease` (`math display`
//! for formulas), beside the common ones.
//!
//! `Morph{from: Text{…} to: Text{…} time duration}` (what `Text{… morph: {…}}`
//! lowers to) is the same morph between two texts, each with its own size,
//! font, colour, anchor and alignment, matched `by: @char`: there is one
//! morph path, and every letter's outline is morphed, never swapped.

use super::*;
use crate::math;
use crate::pieces::{self, Keying, MathKeying, Piece};

fn data(piece: &Piece) -> String {
    crate::icons::path_data(&piece.outline).trim().to_string()
}

/// One side of a text morph: what to set, in which face, placed where.
struct Side {
    text: String,
    face: crate::font_spec::FontSpec,
    /// Points (a `Text`'s size).
    size: f64,
    /// Ems.
    tracking: f64,
    line_spacing: f64,
    color: u32,
    /// 0 left, 0.5 centre, 1 right: how the lines sit in the block, and the
    /// block about the anchor.
    align: f64,
    /// 0 top, 0.5 middle, 1 bottom: the block about the anchor.
    valign: f64,
    /// The anchor in the layer's space.
    at: [f64; 2],
}

fn pieces_of_text(side: &Side, keying: Keying) -> Result<Vec<Piece>, (String, String)> {
    let mut laid = crate::type_fns::glyph_inks(&side.text, &side.face, side.size, side.tracking, side.line_spacing).map_err(|m| ("font".to_string(), m))?;
    let dy = side.at[1] - laid.height * side.valign;
    for g in &mut laid.glyphs {
        let dx = side.at[0] - side.align * laid.line_widths.get(g.line).copied().unwrap_or(0.0);
        for p in g.contours.iter_mut().flatten() {
            *p = [p[0] + dx, p[1] + dy];
        }
    }
    Ok(pieces::text_pieces(&laid.glyphs, keying))
}

fn pieces_of(text: String, field: &str, math_mode: bool, by: &str, face: &crate::font_spec::FontSpec, size: f64, tracking: f64, display: bool, align: f64) -> Result<Vec<Piece>, (String, String)> {
    if math_mode {
        let keying = match by {
            "char" | "letter" => MathKeying::Char,
            "id" => MathKeying::Id,
            other => return Err(("by".into(), format!("a formula's pieces are matched `by: @char` or `@id`, not `{other}`"))),
        };
        let set = math::typeset(&text, face.family, size * 4.0 / 3.0, display).map_err(|m| (field.to_string(), m))?;
        let width = set.width;
        let mut pieces = pieces::math_pieces(set, keying);
        for p in &mut pieces {
            p.origin[0] -= width * align;
        }
        return Ok(pieces);
    }
    let keying = match by {
        "char" | "letter" => Keying::Char,
        "word" => Keying::Word,
        "line" => Keying::Line,
        other => return Err(("by".into(), format!("text is matched `by: @char`, `@word` or `@line`, not `{other}`"))),
    };
    let side = Side { text, face: face.clone(), size, tracking, line_spacing: 1.0, color: 0, align, valign: 0.0, at: [0.0, 0.0] };
    pieces_of_text(&side, keying)
}

/// A list of numbers (`times`, `durations`) of exactly `len`.
fn numbers(cx: &Cx, o: ScriptObject, key: &str, len: usize) -> Result<Option<Vec<f64>>, (String, String)> {
    let Some(v) = obj::opt(cx.vm, o, key).filter(|v| !v.is_nil()) else { return Ok(None) };
    let items = obj::items(cx.vm, v).ok_or_else(|| (key.to_string(), format!("`{key}` is a list of seconds [t, …], not {}", kind(cx, v))))?;
    let nums: Vec<f64> = items.iter().map(|x| obj::num(cx.vm, *x)).collect::<Option<_>>().ok_or_else(|| (key.to_string(), format!("`{key}` is a list of numbers")))?;
    if nums.len() != len {
        return Err((key.to_string(), format!("`{key}` has one entry per transition: {len} (one fewer than `steps`), not {}", nums.len())));
    }
    Ok(Some(nums))
}

pub(super) fn lower(cx: &mut Cx, o: ScriptObject) -> Made {
    let math_mode = get_bool(cx, o, "math", false);
    let face = get_face(cx, o, "font", if math_mode { "STIX Two Math" } else { "Inter" })?;
    let size = get_num(cx, o, "size", 64.0)?.max(0.1);
    let tracking = get_num(cx, o, "tracking", 0.0)?;
    let display = get_bool(cx, o, "display", true);
    let color = get_color(cx, o, "color", 0xffffffff)?;
    let duration = get_num(cx, o, "duration", 1.0)?.max(1e-3);
    let ease = obj::opt(cx.vm, o, "ease").and_then(|v| obj::name(cx.vm, v)).unwrap_or_else(|| "ease_in_out_cubic".to_string());
    let settle = get_num(cx, o, "settle", 0.0)?.max(0.0);
    let align = match obj::opt(cx.vm, o, "align").and_then(|v| obj::name(cx.vm, v)).as_deref() {
        None | Some("left") => 0.0,
        Some("center") => 0.5,
        Some("right") => 1.0,
        Some(other) => return Err(("align".into(), format!("`align` is @left, @center or @right, not `{other}`"))),
    };
    let steps_value = obj::opt(cx.vm, o, "steps").filter(|v| !v.is_nil()).ok_or_else(|| ("steps".to_string(), "a MatchMorph needs `steps`: the texts (or, with `math: true`, formulas) it goes through, `steps: [\"a\", \"b\", …]`".to_string()))?;
    let texts: Vec<String> = obj::items(cx.vm, steps_value)
        .ok_or_else(|| ("steps".to_string(), format!("`steps` is a list of texts, not {}", kind(cx, steps_value))))?
        .into_iter()
        .map(|v| obj::string(cx.vm, v))
        .collect::<Option<_>>()
        .ok_or_else(|| ("steps".to_string(), "`steps` is a list of texts (formula source with `math: true`)".to_string()))?;
    if texts.len() < 2 {
        return Err(("steps".into(), "`steps` has at least two states: it morphs from each to the next".into()));
    }
    let transitions = texts.len() - 1;
    // `by` is one keying for every transition, or a list with one keying per transition.
    let bys: Vec<String> = match obj::opt(cx.vm, o, "by").filter(|v| !v.is_nil()) {
        None => vec!["char".to_string(); transitions],
        Some(v) => match obj::items(cx.vm, v) {
            Some(items) => {
                let names: Vec<String> = items.iter().map(|x| obj::name(cx.vm, *x)).collect::<Option<_>>().ok_or_else(|| ("by".to_string(), "`by` is a keying (`@id`) or a list of keyings, one per transition".to_string()))?;
                if names.len() != transitions {
                    return Err(("by".into(), format!("`by` as a list has one keying per transition: {transitions} (one fewer than `steps`), not {}", names.len())));
                }
                names
            }
            None => vec![obj::name(cx.vm, v).ok_or_else(|| ("by".to_string(), "`by` is a keying (`@id`) or a list of keyings, one per transition".to_string()))?; transitions],
        },
    };
    let times = numbers(cx, o, "times", transitions)?.ok_or_else(|| ("times".to_string(), format!("`times` says when each transition starts: {transitions} seconds, one per step after the first")))?;
    let durations = numbers(cx, o, "durations", transitions)?.unwrap_or_else(|| vec![duration; transitions]);
    for i in 0..transitions {
        if durations[i] <= 0.0 {
            return Err(("durations".into(), "a transition takes some time".into()));
        }
        // The next transition may not begin while this one (and its colour settle) still runs.
        if i + 1 < transitions && times[i + 1] < times[i] + durations[i] + settle - 1e-9 {
            return Err(("times".into(), format!("transition {} starts at {} s, before transition {} (from {} s, {} s long + {} s settle) is done", i + 2, times[i + 1], i + 1, times[i], durations[i], settle)));
        }
    }
    let state_colors = match get_colors(cx, o, "colors", &[color])? {
        c if c.len() == 1 => vec![c[0]; texts.len()],
        c if c.len() == texts.len() => c,
        c => return Err(("colors".into(), format!("`colors` is one colour per step: {}, not {}", texts.len(), c.len()))),
    };
    let styles = read_styles(cx, o)?;
    let enter = obj::opt(cx.vm, o, "enter_color").filter(|v| !v.is_nil()).map(|v| v.as_color().ok_or(("enter_color".to_string(), "`enter_color` is a colour (#rrggbb)".to_string()))).transpose()?;
    // Transition i matches state i against state i+1 by bys[i], so a state is cut once per keying a
    // neighbouring transition asks of it (the held state wears the keying of the transition out of it).
    let mut made: Vec<(usize, String, State)> = Vec::new();
    for (i, (text, base)) in texts.iter().zip(&state_colors).enumerate() {
        let wanted: Vec<&String> = [i.checked_sub(1).map(|k| &bys[k]), bys.get(i)].into_iter().flatten().collect();
        for by in wanted {
            if !made.iter().any(|(k, b, _)| *k == i && b == by) {
                let pieces = pieces_of(text.clone(), "steps", math_mode, by, &face, size, tracking, display, align)?;
                made.push((i, by.clone(), State { colors: styles.colors(&pieces, *base), pieces }));
            }
        }
    }
    let cut = |i: usize, by: &str| &made.iter().find(|(k, b, _)| *k == i && b == by).expect("cut above").2;
    // Each state holds still from the end of the transition into it (and its settle) until
    // the transition out of it begins; each transition is a group with its own clock.
    let span = |cx: &mut Cx, start: f64, length: f64| {
        let (a, b) = (ScriptValue::from(start), ScriptValue::from(length));
        obj::array(cx.vm, vec![a, b])
    };
    let mut layers = Vec::new();
    for i in 0..=transitions {
        let state = cut(i, &bys[i.min(transitions - 1)]);
        let from = if i == 0 { 0.0 } else { times[i - 1] + durations[i - 1] + settle };
        let until = if i == transitions { f64::MAX } else { times[i] };
        if until - from > 1e-9 {
            let held: Vec<ScriptValue> = state.pieces.iter().zip(&state.colors).enumerate().map(|(k, (p, c))| layer(cx, "Path", vec![("svg", data(p).into()), ("at", v2(p.origin[0], p.origin[1])), ("fill", Lit::Color(*c)), ("name", format!("hold {k}").into())])).collect();
            let held = array(cx, held);
            let length = if i == transitions { 1.0e6 } else { until - from };
            let span = span(cx, from, length);
            layers.push(layer(cx, "Group", vec![("name", format!("state {i}").into()), ("span", Lit::Value(span)), ("layers", Lit::Value(held))]));
        }
        if i < transitions {
            let moving = morph(cx, state, cut(i + 1, &bys[i]), enter, settle, 0.0.into(), durations[i], &ease)?;
            let moving = array(cx, moving);
            let span = span(cx, times[i], durations[i] + settle);
            layers.push(layer(cx, "Group", vec![("name", format!("step {}", i + 1).into()), ("span", Lit::Value(span)), ("layers", Lit::Value(moving))]));
        }
    }
    Ok(group(cx, o, layers, vec![("glyphs", Lit::Bool(true))]))
}

/// One side of a `Morph` from its `Text` layer (the fields that place and
/// set a text; others that change its layout are errors, not ignored).
fn side_of(cx: &Cx, field: &str, o: ScriptObject) -> Result<Side, (String, String)> {
    const UNSUPPORTED: &[&str] = &["wrap", "fit", "animate", "path", "plate", "stagger", "by", "reveal", "highlight", "karaoke", "wave"];
    for k in UNSUPPORTED {
        if obj::opt(cx.vm, o, k).is_some_and(|v| !v.is_nil()) {
            return Err((field.to_string(), format!("a text morph takes the text's `text font size color tracking line_height align valign at`, not `{k}`")));
        }
    }
    let text = get_text(cx, o, "text").ok_or_else(|| (field.to_string(), format!("a Morph's `{field}` is a Text layer with its `text`")))?;
    let face = get_face(cx, o, "font", "Inter")?;
    let size = get_num(cx, o, "size", 72.0)?.max(0.1);
    // A Text's tracking is in document units after each glyph; the layout's is in ems.
    let tracking = get_num(cx, o, "tracking", 0.0)? / (size * 4.0 / 3.0);
    let align = match obj::opt(cx.vm, o, "align").and_then(|v| obj::name(cx.vm, v)).as_deref() {
        None | Some("left") => 0.0,
        Some("center") => 0.5,
        Some("right") => 1.0,
        Some(other) => return Err((field.to_string(), format!("`align` is @left, @center or @right, not `{other}`"))),
    };
    let valign = match obj::opt(cx.vm, o, "valign").and_then(|v| obj::name(cx.vm, v)).as_deref() {
        None | Some("top") => 0.0,
        Some("center") | Some("middle") => 0.5,
        Some("bottom") => 1.0,
        Some(other) => return Err((field.to_string(), format!("`valign` is @top, @center or @bottom, not `{other}`"))),
    };
    Ok(Side {
        text,
        face,
        size,
        tracking,
        line_spacing: get_num(cx, o, "line_height", 1.2)?,
        color: get_color(cx, o, "color", 0xffffffff)?,
        align,
        valign,
        at: get_vec2(cx, o, "at", [0.0, 0.0])?,
    })
}

/// `Morph{from: Text{…} to: Text{…} time duration}`: the first text turns
/// into the second, letter by letter.
pub(super) fn lower_morph(cx: &mut Cx, o: ScriptObject) -> Made {
    let layer_of = |cx: &Cx, field: &str| match obj::opt(cx.vm, o, field).and_then(|v| v.as_object()) {
        Some(l) => Ok(l),
        None => Err((field.to_string(), format!("a Morph needs `{field}`: a Text layer"))),
    };
    let (from, to) = (side_of(cx, "from", layer_of(cx, "from")?)?, side_of(cx, "to", layer_of(cx, "to")?)?);
    let duration = get_num(cx, o, "duration", 0.6)?.max(0.01);
    let start = obj::opt(cx.vm, o, "time").filter(|v| !v.is_nil()).unwrap_or(1.0.into());
    let (a, b) = (pieces_of_text(&from, Keying::Char)?, pieces_of_text(&to, Keying::Char)?);
    let layers = morph(cx, &State::plain(a, from.color), &State::plain(b, to.color), None, 0.0, start, duration, "ease_in_out_cubic")?;
    Ok(group(cx, o, layers, vec![("glyphs", Lit::Bool(true))]))
}

/// A layout's pieces with the colour each is drawn in.
struct State {
    pieces: Vec<Piece>,
    colors: Vec<u32>,
}

impl State {
    fn plain(pieces: Vec<Piece>, color: u32) -> State {
        let colors = vec![color; pieces.len()];
        State { pieces, colors }
    }
}

/// The colours the author gave to parts of a formula (the Math layer's
/// `parts`, `paths` and `chars`, colour only).
struct Styles {
    ids: Vec<super::math::Part>,
    paths: Vec<super::math::Part>,
    chars: Vec<super::math::Part>,
}

fn read_styles(cx: &Cx, o: ScriptObject) -> Result<Styles, (String, String)> {
    let read = |field: &str| -> Result<Vec<super::math::Part>, (String, String)> {
        let parts = super::math::read_parts(cx, o, field)?;
        for p in &parts {
            for (k, v) in &p.fields {
                if k != "fill" || v.as_color().is_none() {
                    return Err((field.into(), format!("a MatchMorph colours its pieces: `{field}.{}` is `{{color: #rrggbb}}` (a plain colour; it settles from `enter_color`)", p.select)));
                }
            }
        }
        Ok(parts)
    };
    Ok(Styles { ids: read("parts")?, paths: read("paths")?, chars: read("chars")? })
}

impl Styles {
    /// Each piece's own colour: the first part that styles it, else `default`.
    fn colors(&self, pieces: &[Piece], default: u32) -> Vec<u32> {
        pieces.iter().map(|p| super::math::pick(p, &self.ids, &self.paths, &self.chars).and_then(|part| part.fields.iter().find(|(k, _)| k == "fill")).and_then(|(_, v)| v.as_color()).unwrap_or(default)).collect()
    }
}

/// The layers of a morph from one layout to another. Pairs travel and
/// change colour; a pair whose glyphs differ cross-fades; leaving pieces
/// fade out, arriving ones fade in and settle from `enter` (if any) to
/// their own colour over `settle` seconds more.
fn morph(cx: &mut Cx, from: &State, to: &State, enter: Option<u32>, settle: f64, start: ScriptValue, duration: f64, ease: &str) -> Result<Vec<ScriptValue>, (String, String)> {
    if makepad_tween::named::Ease::from_id(ease).is_none() {
        return Err(("ease".into(), format!("`@{ease}` is not an ease")));
    }
    let pairing = pieces::pair_pieces(&from.pieces, &to.pieces);
    let mut layers = Vec::with_capacity(from.pieces.len() + to.pieces.len());
    // An arriving piece's fill: its own colour, or `enter` settling into it from `begin`.
    let arriving = |cx: &mut Cx, own: u32, begin: ScriptValue, len: f64| -> Lit {
        match enter {
            Some(e) if e != own => Lit::Value(tween(cx, Lit::Color(e), Lit::Color(own), begin, len + settle, "ease_out_quad")),
            _ => Lit::Color(own),
        }
    };
    // Pairs: shape, place and colour move together.
    for &(i, j) in &pairing.matched {
        let (a, b) = (&from.pieces[i], &to.pieces[j]);
        let (ca, cb) = (from.colors[i], to.colors[j]);
        let (da, db) = (data(a), data(b));
        let at = if a.origin == b.origin { obj::lit(cx.vm, v2(a.origin[0], a.origin[1])) } else { tween(cx, v2(a.origin[0], a.origin[1]), v2(b.origin[0], b.origin[1]), start, duration, ease) };
        if a.content != b.content {
            // Different glyphs (a 5 becoming a 6) would morph into a blob: the old one fades out
            // while the new one fades in, both riding the pair's travel.
            let out = tween(cx, Lit::Num(1.0), Lit::Num(0.0), start, duration * 0.6, "ease_in_quad");
            layers.push(layer(cx, "Path", vec![("svg", da.into()), ("at", Lit::Value(at)), ("fill", Lit::Color(ca)), ("opacity", Lit::Value(out)), ("name", format!("swap out {}", j).into())]));
            let later = at_time(cx, start, duration * 0.4);
            let fade = tween(cx, Lit::Num(0.0), Lit::Num(1.0), later, duration * 0.6, "ease_out_quad");
            let fill = arriving(cx, cb, later, duration * 0.6);
            layers.push(layer(cx, "Path", vec![("svg", db.into()), ("at", Lit::Value(at)), ("fill", fill), ("opacity", Lit::Value(fade)), ("name", format!("swap in {}", j).into())]));
            continue;
        }
        let svg: ScriptValue = if da == db {
            obj::lit(cx.vm, Lit::Str(da))
        } else {
            let (x, y) = (obj::lit(cx.vm, Lit::Str(da)), obj::lit(cx.vm, Lit::Str(db)));
            obj::tween(cx.vm, x, y, start, duration, ease)
        };
        let fill = if ca == cb { obj::lit(cx.vm, Lit::Color(ca)) } else { tween(cx, Lit::Color(ca), Lit::Color(cb), start, duration, ease) };
        layers.push(layer(cx, "Path", vec![("svg", Lit::Value(svg)), ("at", Lit::Value(at)), ("fill", Lit::Value(fill)), ("name", format!("match {}", j).into())]));
    }
    // Leaving pieces fade over the first half, arriving ones over the second.
    for &i in &pairing.from_only {
        let p = &from.pieces[i];
        let opacity = tween(cx, Lit::Num(1.0), Lit::Num(0.0), start, duration * 0.5, "ease_in_quad");
        layers.push(layer(cx, "Path", vec![("svg", data(p).into()), ("at", v2(p.origin[0], p.origin[1])), ("fill", Lit::Color(from.colors[i])), ("opacity", Lit::Value(opacity)), ("name", format!("out {}", i).into())]));
    }
    for &j in &pairing.to_only {
        let p = &to.pieces[j];
        let later = at_time(cx, start, duration * 0.5);
        let opacity = tween(cx, Lit::Num(0.0), Lit::Num(1.0), later, duration * 0.5, "ease_out_quad");
        let fill = arriving(cx, to.colors[j], later, duration * 0.5);
        layers.push(layer(cx, "Path", vec![("svg", data(p).into()), ("at", v2(p.origin[0], p.origin[1])), ("fill", fill), ("opacity", Lit::Value(opacity)), ("name", format!("in {}", j).into())]));
    }
    Ok(layers)
}
