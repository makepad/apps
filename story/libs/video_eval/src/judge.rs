//! The visual score: a vision review of the contact sheet and key frames
//! against the written rubric (the file `--rubric` names, the part between
//! its `judge` markers). The reviewer is a chat provider that accepts
//! images; the default is the Claude Code CLI with Opus 5.5, the user's
//! configured and logged-in backend, so no credential passes through here.

use makepad_ai_hub::chat_wire::{ChatMessage, ChatRole};
use makepad_ai_hub::providers::claude::ClaudeCodeChatProvider;
use makepad_ai_hub::providers::provider::{ChatProvider, ProviderEvent, ToolImage, TurnInput};
use makepad_ai_hub::testpattern::{decode_png_rgb8, encode_png_rgb8};
use makepad_micro_serde::*;
use std::time::{Duration, Instant};

/// The rubric's criteria, in table order.
pub const CRITERIA: &[&str] = &["hierarchy", "legibility", "pacing", "easing", "brand", "originality"];

/// The provider limits images to 1024 px on the long side and 4 per turn.
pub const MAX_IMAGE_SIDE: u32 = 1024;
pub const MAX_IMAGES: usize = 4;

pub struct Rubric {
    /// What the judge reads for the six criteria.
    pub text: String,
    /// sha256 (16 hex) of `text`: runs are comparable only under one hash.
    /// Only the judged section counts, so editing the rest of the file (or
    /// the separate richness section) keeps earlier runs comparable.
    pub hash: String,
    /// The separate visual-richness review, when the file has one. Scored
    /// on its own and never folded into the six criteria or the overall.
    pub richness: Option<String>,
    pub richness_hash: String,
    /// The acceptance checklist (REF-SYNTHESIS §2), judged for the
    /// acceptance set only, in its own turn.
    pub acceptance: Option<String>,
    pub acceptance_hash: String,
    /// The lyric-film checklist (REF-SYNTHESIS §2, the three.js film).
    pub lyric_film: Option<String>,
    pub lyric_film_hash: String,
}

fn section(file: &str, name: &str) -> Option<String> {
    let (begin, end) = (format!("<!-- {name}:begin -->"), format!("<!-- {name}:end -->"));
    match (file.find(&begin), file.find(&end)) {
        (Some(a), Some(b)) if b > a => Some(file[a + begin.len()..b].trim().to_string()),
        _ => None,
    }
}

fn short_hash(text: &str) -> String {
    makepad_ai_hub::sha256::sha256_hex(text.as_bytes())[..16].to_string()
}

impl Rubric {
    pub fn parse(file: &str) -> Rubric {
        let text = section(file, "judge").unwrap_or_else(|| file.trim().to_string());
        let richness = section(file, "richness");
        let acceptance = section(file, "acceptance");
        let lyric_film = section(file, "lyric-film");
        Rubric {
            hash: short_hash(&text),
            richness_hash: richness.as_deref().map_or(String::new(), short_hash),
            acceptance_hash: acceptance.as_deref().map_or(String::new(), short_hash),
            lyric_film_hash: lyric_film.as_deref().map_or(String::new(), short_hash),
            lyric_film,
            text,
            richness,
            acceptance,
        }
    }
}

#[derive(Clone, Debug, Default, SerJson, DeJson)]
pub struct CriterionScore {
    pub name: String,
    /// 1..5.
    pub score: f64,
    pub note: String,
}

#[derive(Clone, Debug, Default, SerJson, DeJson)]
pub struct Verdict {
    /// In [`CRITERIA`] order.
    pub criteria: Vec<CriterionScore>,
    pub fulfils_brief: String,
    pub top_fix: String,
    /// Mean of the criteria scores.
    pub mean: f64,
    pub judge: String,
}

impl Verdict {
    pub fn score(&self, criterion: &str) -> Option<f64> {
        self.criteria.iter().find(|c| c.name == criterion).map(|c| c.score)
    }
}

/// The judge's reply: the one JSON object in it (code fences and stray
/// prose tolerated). Every criterion must be scored 1..5.
pub fn parse_verdict(reply: &str, judge: &str) -> Result<Verdict, String> {
    let start = reply.find('{').ok_or("no JSON object in the judge's reply")?;
    let end = reply.rfind('}').ok_or("no JSON object in the judge's reply")?;
    let v = JsonValue::deserialize_json_lenient(&reply[start..=end]).map_err(|e| format!("judge reply: {e:?}"))?;
    let scores = v.get("scores").ok_or("judge reply has no `scores`")?;
    let mut out = Verdict { judge: judge.to_string(), ..Default::default() };
    for c in CRITERIA {
        let n = scores
            .get(c)
            .and_then(|s| s.as_f64().or_else(|| s.as_i64().map(|i| i as f64)).or_else(|| s.as_u64().map(|u| u as f64)))
            .ok_or_else(|| format!("judge reply has no score for `{c}`"))?;
        if !(1.0..=5.0).contains(&n) {
            return Err(format!("judge scored `{c}` {n}, outside 1..5"));
        }
        let note = v.get("notes").and_then(|n| n.get(c)).and_then(JsonValue::as_str).unwrap_or("");
        out.criteria.push(CriterionScore { name: c.to_string(), score: n, note: note.to_string() });
    }
    out.mean = out.criteria.iter().map(|c| c.score).sum::<f64>() / CRITERIA.len() as f64;
    out.fulfils_brief = v.get("fulfils_brief").and_then(JsonValue::as_str).unwrap_or("").to_string();
    out.top_fix = v.get("top_fix").and_then(JsonValue::as_str).unwrap_or("").to_string();
    Ok(out)
}

/// The user turn: the brief and what each image is.
pub fn brief_prompt(prompt: &str, format: &str, length_s: f64, labels: &[String]) -> String {
    let mut out = format!("The brief the video was made from ({format}, {length_s:.0} s):\n\"{prompt}\"\n\nImages, in order:\n");
    for (i, l) in labels.iter().enumerate() {
        out.push_str(&format!("{}. {l}\n", i + 1));
    }
    out.push_str("\nScore the video per the rubric. Reply with the JSON object only.");
    out
}

/// Box-filter an RGB8 image down so neither side exceeds `max` (never up).
pub fn downscale_rgb(rgb: &[u8], w: u32, h: u32, max: u32) -> (Vec<u8>, u32, u32) {
    if w <= max && h <= max {
        return (rgb.to_vec(), w, h);
    }
    let scale = max as f64 / w.max(h) as f64;
    let (nw, nh) = (((w as f64 * scale).round() as u32).max(1), ((h as f64 * scale).round() as u32).max(1));
    let mut out = vec![0u8; (nw * nh * 3) as usize];
    for y in 0..nh {
        let y0 = (y as u64 * h as u64 / nh as u64) as u32;
        let y1 = (((y + 1) as u64 * h as u64 / nh as u64) as u32).max(y0 + 1).min(h);
        for x in 0..nw {
            let x0 = (x as u64 * w as u64 / nw as u64) as u32;
            let x1 = (((x + 1) as u64 * w as u64 / nw as u64) as u32).max(x0 + 1).min(w);
            let mut acc = [0u64; 3];
            for sy in y0..y1 {
                for sx in x0..x1 {
                    let i = ((sy * w + sx) * 3) as usize;
                    for c in 0..3 {
                        acc[c] += rgb[i + c] as u64;
                    }
                }
            }
            let n = ((y1 - y0) * (x1 - x0)) as u64;
            let o = ((y * nw + x) * 3) as usize;
            for c in 0..3 {
                out[o + c] = (acc[c] / n) as u8;
            }
        }
    }
    (out, nw, nh)
}

/// A PNG the provider accepts: at most [`MAX_IMAGE_SIDE`] on each side.
pub fn fit_png(png: &[u8]) -> Result<Vec<u8>, String> {
    let (rgb, w, h) = decode_png_rgb8(png).map_err(|e| e.to_string())?;
    if w <= MAX_IMAGE_SIDE && h <= MAX_IMAGE_SIDE {
        return Ok(png.to_vec());
    }
    let (small, nw, nh) = downscale_rgb(&rgb, w, h, MAX_IMAGE_SIDE);
    encode_png_rgb8(&small, nw as usize, nh as usize).map_err(|e| e.to_string())
}

/// Ask the judge. `judge` is `claude-cli` or `claude-cli:<model>` (the
/// Claude Code CLI's model id or alias; default `claude-opus-5-5`, pinned so runs compare).
pub fn review(judge: &str, rubric: &Rubric, user: &str, images: Vec<(String, Vec<u8>)>, timeout: Duration) -> Result<(Verdict, String), String> {
    let (label, reply) = ask(judge, &rubric.text, user, images, timeout)?;
    let verdict = parse_verdict(&reply, &label).map_err(|e| format!("{e}; reply: {}", reply.chars().take(400).collect::<String>()))?;
    Ok((verdict, reply))
}

/// The separate visual-richness review: `(score 1..5, note, reply)`.
pub fn review_richness(judge: &str, rubric: &Rubric, user: &str, images: Vec<(String, Vec<u8>)>, timeout: Duration) -> Result<(f64, String, String), String> {
    let system = rubric.richness.as_deref().ok_or("the rubric has no richness section")?;
    let (_, reply) = ask(judge, system, user, images, timeout)?;
    let (score, note) = parse_richness(&reply).map_err(|e| format!("{e}; reply: {}", reply.chars().take(400).collect::<String>()))?;
    Ok((score, note, reply))
}

/// `{"richness": N, "note": "…"}` from the reply, N in 1..5.
pub fn parse_richness(reply: &str) -> Result<(f64, String), String> {
    let start = reply.find('{').ok_or("no JSON object in the reply")?;
    let end = reply.rfind('}').ok_or("no JSON object in the reply")?;
    let v = JsonValue::deserialize_json_lenient(&reply[start..=end]).map_err(|e| format!("richness reply: {e:?}"))?;
    let n = v
        .get("richness")
        .and_then(|s| s.as_f64().or_else(|| s.as_i64().map(|i| i as f64)).or_else(|| s.as_u64().map(|u| u as f64)))
        .ok_or("reply has no `richness` score")?;
    if !(1.0..=5.0).contains(&n) {
        return Err(format!("richness {n} is outside 1..5"));
    }
    Ok((n, v.get("note").and_then(JsonValue::as_str).unwrap_or("").to_string()))
}

/// The acceptance checklist's items, in rubric order.
pub const ACCEPTANCE: &[&str] = &["chapters", "hud", "beat_cuts", "word_montage", "motivated_transitions", "logo_sting", "shorts_frame", "type_band"];

/// The lyric-film checklist's items, in rubric order.
pub const LYRIC_FILM: &[&str] = &["word_sync", "set_per_line", "one_world", "palette", "running_device", "footnotes", "outro"];

/// The acceptance review of `items` (a subset of [`ACCEPTANCE`], each
/// judged from the pictures and data that can show it): one
/// `(item, pass, note)` per item.
pub fn review_acceptance(
    judge: &str,
    system: &str,
    items: &[&str],
    user: &str,
    images: Vec<(String, Vec<u8>)>,
    timeout: Duration,
) -> Result<(Vec<(String, bool, String)>, String), String> {
    let user = format!("{user}\n\nAnswer ONLY these items in `checks`: {}.", items.join(", "));
    let (_, reply) = ask(judge, system, &user, images, timeout)?;
    let checks = parse_acceptance(&reply, items).map_err(|e| format!("{e}; reply: {}", reply.chars().take(400).collect::<String>()))?;
    Ok((checks, reply))
}

/// Tile PNGs into one sheet, `columns` per row, on a dark ground with a
/// small gap (for several strips or trails in one judge picture).
pub fn tile_pngs(pngs: &[Vec<u8>], columns: usize) -> Result<Vec<u8>, String> {
    let tiles: Vec<(Vec<u8>, u32, u32)> = pngs.iter().map(|p| decode_png_rgb8(p).map_err(|e| e.to_string())).collect::<Result<_, _>>()?;
    if tiles.is_empty() {
        return Err("no pictures to tile".into());
    }
    let columns = columns.max(1);
    let gap = 8u32;
    let cell_w = tiles.iter().map(|t| t.1).max().unwrap_or(1);
    let rows = tiles.len().div_ceil(columns);
    let row_h: Vec<u32> = (0..rows).map(|r| tiles[r * columns..((r + 1) * columns).min(tiles.len())].iter().map(|t| t.2).max().unwrap_or(1)).collect();
    let w = columns.min(tiles.len()) as u32 * (cell_w + gap) + gap;
    let h = row_h.iter().map(|h| h + gap).sum::<u32>() + gap;
    let mut out = vec![18u8; (w * h * 3) as usize];
    let mut y0 = gap;
    for (r, rh) in row_h.iter().enumerate() {
        for (c, (rgb, tw, th)) in tiles.iter().skip(r * columns).take(columns).enumerate() {
            let x0 = gap + c as u32 * (cell_w + gap);
            for y in 0..*th {
                let src = (y * tw * 3) as usize;
                let dst = (((y0 + y) * w + x0) * 3) as usize;
                out[dst..dst + (*tw * 3) as usize].copy_from_slice(&rgb[src..src + (*tw * 3) as usize]);
            }
        }
        y0 += rh + gap;
    }
    encode_png_rgb8(&out, w as usize, h as usize).map_err(|e| e.to_string())
}

/// `{"checks": {item: {"pass": "yes|no|unclear", "note": "…"}}}`. Every
/// asked item must be answered; only "yes" passes ("unclear" fails, noted).
pub fn parse_acceptance(reply: &str, items: &[&str]) -> Result<Vec<(String, bool, String)>, String> {
    let start = reply.find('{').ok_or("no JSON object in the reply")?;
    let end = reply.rfind('}').ok_or("no JSON object in the reply")?;
    let v = JsonValue::deserialize_json_lenient(&reply[start..=end]).map_err(|e| format!("acceptance reply: {e:?}"))?;
    let checks = v.get("checks").ok_or("reply has no `checks`")?;
    items
        .iter()
        .map(|item| {
            // An item the judge left out fails, noted, without losing the
            // turn's other answers.
            let Some(c) = checks.get(item) else {
                return Ok((item.to_string(), false, "no answer from the judge".to_string()));
            };
            let verdict = c.get("pass").and_then(JsonValue::as_str).ok_or_else(|| format!("`{item}` has no pass"))?.to_ascii_lowercase();
            let note = c.get("note").and_then(JsonValue::as_str).unwrap_or("");
            let note = if verdict == "yes" || verdict == "no" { note.to_string() } else { format!("{verdict}: {note}") };
            Ok((item.to_string(), verdict == "yes", note))
        })
        .collect()
}

/// One vision turn: `(judge label, reply text)`.
fn ask(judge: &str, system: &str, user: &str, images: Vec<(String, Vec<u8>)>, timeout: Duration) -> Result<(String, String), String> {
    let (backend, model) = judge.split_once(':').unwrap_or((judge, "claude-opus-5-5"));
    if backend != "claude-cli" {
        return Err(format!("unknown judge backend `{backend}` (known: claude-cli)"));
    }
    let mut provider = ClaudeCodeChatProvider::new(Some(model.to_string()));
    let images: Vec<ToolImage> = images
        .into_iter()
        .take(MAX_IMAGES)
        .map(|(label, png)| Ok(ToolImage { label: short_label(&label), png: fit_png(&png)?.into() }))
        .collect::<Result<_, String>>()?;
    provider.attach_tool_images(images)?;
    let mut input = TurnInput::new(system.to_string(), vec![ChatMessage::new(ChatRole::User, user)]);
    input.tools_enabled = false;
    provider.begin_turn(&input)?;
    let started = Instant::now();
    let mut text = String::new();
    loop {
        for event in provider.poll() {
            match event {
                ProviderEvent::Delta(d) => text.push_str(&d),
                ProviderEvent::Done { text: full } => {
                    return Ok((format!("{backend}:{model}"), strip_think(if full.is_empty() { &text } else { &full })));
                }
                ProviderEvent::Error(e) => return Err(format!("judge: {e}")),
                _ => {}
            }
        }
        if started.elapsed() > timeout {
            provider.cancel();
            return Err(format!("judge timed out after {:.0} s", timeout.as_secs_f64()));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// The provider takes labels of at most 128 bytes; the full label is in
/// the prompt's image list anyway.
fn short_label(label: &str) -> String {
    if label.len() <= 120 {
        return label.to_string();
    }
    let mut end = 117;
    while !label.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}...", &label[..end])
}

/// Drop `<think>…</think>` blocks the provider forwards from reasoning.
fn strip_think(s: &str) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(a) = rest.find("<think>") {
        out.push_str(&rest[..a]);
        match rest[a..].find("</think>") {
            Some(b) => rest = &rest[a + b + "</think>".len()..],
            None => {
                rest = "";
            }
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rubric_takes_the_judge_section() {
        let r = Rubric::parse("# R\nintro\n<!-- judge:begin -->\nScore it.\n<!-- judge:end -->\ntail");
        assert_eq!(r.text, "Score it.");
        assert_eq!(r.hash.len(), 16);
        assert!(r.richness.is_none());
        assert_eq!(Rubric::parse("just text").text, "just text");
        // Only the judged section is hashed: a richness section or other
        // edits elsewhere keep runs comparable.
        let more = Rubric::parse("# R2\n<!-- judge:begin -->\nScore it.\n<!-- judge:end -->\n<!-- richness:begin -->\nRich?\n<!-- richness:end -->");
        assert_eq!(more.hash, r.hash);
        assert_eq!(more.richness.as_deref(), Some("Rich?"));
        assert_eq!(more.richness_hash.len(), 16);
    }

    #[test]
    fn acceptance_reply_parses() {
        let mut body: Vec<String> = ACCEPTANCE.iter().map(|i| format!("\"{i}\":{{\"pass\":\"yes\",\"note\":\"ok\"}}")).collect();
        body[2] = "\"beat_cuts\":{\"pass\":\"unclear\",\"note\":\"stills\"}".into();
        let checks = parse_acceptance(&format!("{{\"checks\":{{{}}}}}", body.join(",")), ACCEPTANCE).unwrap();
        assert_eq!(checks.len(), ACCEPTANCE.len());
        assert!(checks[0].1 && !checks[2].1);
        assert_eq!(checks[2].2, "unclear: stills");
        let partial = parse_acceptance("{\"checks\":{\"chapters\":{\"pass\":\"yes\"}}}", ACCEPTANCE).unwrap();
        assert!(partial[0].1 && !partial[1].1 && partial[1].2 == "no answer from the judge");
        assert!(parse_acceptance("{\"verdict\":1}", ACCEPTANCE).is_err());
        // A subset only needs its own items.
        assert_eq!(parse_acceptance("{\"checks\":{\"chapters\":{\"pass\":\"yes\"}}}", &["chapters"]).unwrap().len(), 1);
    }

    #[test]
    fn labels_fit_the_provider() {
        let long = "é".repeat(100);
        let l = short_label(&long);
        assert!(l.len() <= 120 && l.ends_with("..."));
        assert_eq!(short_label("short"), "short");
    }

    #[test]
    fn tiles_pack_rows() {
        let a = encode_png_rgb8(&vec![200u8; 10 * 4 * 3], 10, 4).unwrap();
        let b = encode_png_rgb8(&vec![100u8; 6 * 8 * 3], 6, 8).unwrap();
        let (_, w, h) = decode_png_rgb8(&tile_pngs(&[a.clone(), b, a], 2).unwrap()).unwrap();
        // 2 columns of the widest cell (10) + gaps; rows of 8 and 4.
        assert_eq!((w, h), (2 * (10 + 8) + 8, (8 + 8) + (4 + 8) + 8));
    }

    #[test]
    fn richness_reply_parses() {
        assert_eq!(parse_richness("```json\n{\"richness\":2,\"note\":\"flat\"}\n```").unwrap(), (2.0, "flat".to_string()));
        assert!(parse_richness("{\"richness\":7}").is_err());
        assert!(parse_richness("{\"note\":\"x\"}").is_err());
    }

    #[test]
    fn verdict_parses_fenced_json_and_checks_ranges() {
        let reply = "Here:\n```json\n{\"scores\":{\"hierarchy\":4,\"legibility\":5,\"pacing\":3,\"easing\":3.5,\"brand\":4,\"originality\":2},\"notes\":{\"pacing\":\"scene 2 too short\"},\"fulfils_brief\":\"partial\",\"top_fix\":\"hold scene 2 for 1 s more\"}\n```";
        let v = parse_verdict(reply, "claude-cli:opus").unwrap();
        assert_eq!(v.score("easing"), Some(3.5));
        assert!((v.mean - 21.5 / 6.0).abs() < 1e-9);
        assert_eq!(v.fulfils_brief, "partial");
        assert_eq!(v.criteria[2].note, "scene 2 too short");
        assert!(parse_verdict("{\"scores\":{\"hierarchy\":9}}", "x").is_err());
        assert!(parse_verdict("no json", "x").is_err());
    }

    #[test]
    fn think_blocks_are_dropped() {
        assert_eq!(strip_think("<think>hmm</think>{\"a\":1}"), "{\"a\":1}");
        assert_eq!(strip_think("x<think>open"), "x");
    }

    #[test]
    fn downscale_fits_and_averages() {
        let (w, h) = (2048u32, 512u32);
        let rgb: Vec<u8> = (0..w * h).flat_map(|i| if i % 2 == 0 { [0u8, 0, 0] } else { [200u8, 100, 50] }).collect();
        let (out, nw, nh) = downscale_rgb(&rgb, w, h, 1024);
        assert_eq!((nw, nh), (1024, 256));
        assert_eq!(&out[0..3], &[100, 50, 25]);
        let (same, sw, sh) = downscale_rgb(&rgb[..12], 2, 2, 1024);
        assert_eq!((sw, sh, same.len()), (2, 2, 12));
    }

    #[test]
    fn fit_png_shrinks_large_images() {
        let rgb = vec![128u8; 1600 * 900 * 3];
        let png = encode_png_rgb8(&rgb, 1600, 900).unwrap();
        let fitted = fit_png(&png).unwrap();
        let (_, w, h) = decode_png_rgb8(&fitted).unwrap();
        assert_eq!((w, h), (1024, 576));
    }
}
