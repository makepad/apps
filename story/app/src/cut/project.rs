//! The timeline and the preview sketch as projections of an `CutDoc`
//! (the source is the truth; the timeline is a projection of it). Pure.

use super::ops::Lane;
use makepad_story_cut::{Animated, AudioShape, DrawKind, CutDoc, CutEval, CutNode, CutNodeKind, EffectRef, FramePlan, MediaRef, Slot, SourceTime, Span};
use std::collections::HashMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlockKind {
    Clip,
    Still,
    Title,
    Audio,
    /// Drawn as a band over the top of the block it wraps.
    Effect,
    /// One voiceover line.
    Voice,
    /// One caption of a Subtitles node.
    Caption,
}

/// A block's sound, for waveforms, fades and ducking.
#[derive(Clone, Debug, PartialEq)]
pub struct Sound {
    /// The asset id whose waveform plays here, and its source time at the
    /// block's start and speed.
    pub media: Option<String>,
    pub source_in: f64,
    pub speed: f64,
    pub fade_in: f64,
    pub fade_out: f64,
    /// Edit-time intervals it is ducked in, and by how much (dB).
    pub ducked: Vec<(f64, f64)>,
    pub duck_db: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Block {
    /// What selecting it selects: a node id, or `node#line` for a
    /// voiceover line and `node#caption` for a caption.
    pub node: String,
    /// The cut node it belongs to.
    pub owner: String,
    /// A voiceover line's or caption's index.
    pub line: Option<usize>,
    pub kind: BlockKind,
    pub label: String,
    pub lane: Lane,
    pub track: u32,
    pub start: f64,
    pub length: f64,
    /// Absolute times of this block's keyframes.
    pub keys: Vec<f64>,
    /// Its media is not in the library any more (retired
    /// media shows red).
    pub broken: bool,
    /// A voiceover line whose take is missing or made for other words.
    pub stale: bool,
    /// Its sound: an audio block's, or a clip's own linked audio.
    pub sound: Option<Sound>,
    /// The asset whose pictures its filmstrip shows, its source time at the
    /// block's start, and its speed.
    pub picture: Option<(String, f64, f64)>,
    /// Wrapped by an Fx (the FX badge).
    pub fx: bool,
    /// An app run's render.
    pub app_run: bool,
    /// A compound clip: another cut nested (`Clip{src: cut(…)}`).
    pub nested: bool,
    /// A motion graphic (`Graphic{src: asset(…)}`): opens
    /// into its layers like a compound clip.
    pub graphic: bool,
    /// An effect's clip, and its place in the clip's effects (0 = next to
    /// the clip): its band's row in the rail.
    pub wraps: Option<String>,
    pub level: u32,
    /// A Generate's band: drawn violet, with its state; `progress`
    /// while it generates (the page fills these in).
    pub generate: bool,
    pub progress: Option<f64>,
}

/// A transition over the overlap of two video tracks.
#[derive(Clone, Debug, PartialEq)]
pub struct Band {
    pub node: String,
    /// The clips it joins, first then second.
    pub clips: (String, String),
    pub label: String,
    pub start: f64,
    pub length: f64,
    pub tracks: (u32, u32),
    pub keys: Vec<f64>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Projection {
    pub duration: f64,
    /// Where the last block ends: what "fit" shows.
    pub content_end: f64,
    pub caption_tracks: u32,
    pub video_tracks: u32,
    pub audio_tracks: u32,
    pub voice_tracks: u32,
    pub blocks: Vec<Block>,
    pub bands: Vec<Band>,
}

impl Projection {
    /// How many effect rows `track`'s rail has.
    pub fn fx_levels(&self, track: u32) -> u32 {
        self.blocks.iter().filter(|block| block.kind == BlockKind::Effect && block.track == track).map(|block| block.level + 1).max().unwrap_or(0)
    }

    /// Video tracks that carry an Fx band.
    pub fn fx_tracks(&self) -> Vec<u32> {
        let mut tracks = self.blocks.iter().filter(|block| block.kind == BlockKind::Effect).map(|block| block.track).collect::<Vec<_>>();
        tracks.sort();
        tracks.dedup();
        tracks
    }

    pub fn block(&self, node: &str) -> Option<&Block> {
        self.blocks.iter().find(|block| block.node == node)
    }
}

/// A short readable name for a media reference: the library's title, or the
/// id's tail.
pub fn media_label(media: &MediaRef, titles: &HashMap<String, String>) -> String {
    let id = match media {
        MediaRef::Asset(asset) => &asset.id,
        MediaRef::Run(run) => &run.id,
        MediaRef::MissingOptional(path) => return format!("media not present: {path} (plays silent)"),
    };
    titles.get(id).cloned().unwrap_or_else(|| format!("…{}", &id[id.len().saturating_sub(6)..]))
}

pub fn effect_label(effect: &EffectRef) -> String {
    match effect {
        EffectRef::Preset(name) => preset_label(name),
        EffectRef::Asset(asset) => format!("fx …{}", &asset.id[asset.id.len().saturating_sub(6)..]),
    }
}

/// A preset's display name, as the pickers show it (`09_synthwave` →
/// `Synthwave Grid`); an unknown one from its id (`15_acid_bloom` → `Acid
/// bloom`).
pub fn preset_label(name: &str) -> String {
    let stem = name.rsplit('/').next().unwrap_or(name);
    if let Some((pack, found, source)) = makepad_content_packs::effects::resolve(stem) {
        return pack.preset_title(found, source);
    }
    let bare = stem.split_once('_').filter(|(head, _)| head.bytes().all(|byte| byte.is_ascii_digit())).map_or(stem, |(_, rest)| rest);
    let bare = bare.strip_prefix("trans_").unwrap_or(bare);
    let words = if bare.is_empty() { "dissolve".to_string() } else { bare.replace('_', " ") };
    let mut chars = words.chars();
    chars.next().map(|first| first.to_uppercase().chain(chars).collect()).unwrap_or_default()
}

fn key_times<const N: usize>(value: &Animated<N>, start: f64, out: &mut Vec<f64>) {
    if let Animated::Keys(keys) = value {
        out.extend(keys.points.iter().map(|point| start + point.t));
    }
}

fn keys_of(node: &CutNode) -> Vec<f64> {
    let start = node.span.map_or(0.0, |span| span.start);
    let mut keys = Vec::new();
    match &node.kind {
        CutNodeKind::Clip { opacity, gain, .. } => {
            key_times(opacity, start, &mut keys);
            key_times(gain, start, &mut keys);
        }
        CutNodeKind::Still { opacity, .. } => key_times(opacity, start, &mut keys),
        CutNodeKind::Title { font_size, .. } => key_times(font_size, start, &mut keys),
        CutNodeKind::Fx { params, .. } => params.iter().for_each(|param| key_times(param, start, &mut keys)),
        CutNodeKind::Transition { params, mix, .. } => {
            params.iter().for_each(|param| key_times(param, start, &mut keys));
            if let Some(mix) = mix {
                key_times(mix, start, &mut keys);
            }
        }
        CutNodeKind::AudioClip { gain, pan, .. } => {
            key_times(gain, start, &mut keys);
            key_times(pan, start, &mut keys);
        }
        CutNodeKind::Reframe { crop: Some(crop), .. } => key_times(crop, start, &mut keys),
        _ => {}
    }
    keys.sort_by(f64::total_cmp);
    keys.dedup();
    keys
}

/// The track a chain of effects and transitions sits on: its base block's.
fn base_track(doc: &CutDoc, id: &str) -> u32 {
    let mut current = id;
    for _ in 0..doc.nodes.len() {
        let Some(node) = doc.node(current) else { break };
        match &node.kind {
            CutNodeKind::Fx { input: Some(input), .. } => current = &input.node,
            CutNodeKind::Transition { a: Some(a), .. } => current = &a.node,
            _ => return node.track,
        }
    }
    doc.node(id).map_or(0, |node| node.track)
}

fn media_asset(media: &MediaRef) -> String {
    match media {
        MediaRef::Asset(asset) => asset.id.clone(),
        MediaRef::Run(run) => run.render_id.clone(),
        MediaRef::MissingOptional(path) => path.clone(),
    }
}

/// The intervals `node` is ducked in: its duck's voices speaking, widened
/// by attack and release.
fn ducked(eval: &CutEval, shape: &AudioShape) -> (Vec<(f64, f64)>, f64) {
    let Some(duck) = &shape.duck else { return (Vec::new(), 0.0) };
    let mut spans = Vec::new();
    for under in &duck.under {
        let lines = eval.voice_lines(under);
        if lines.is_empty() {
            if let Some(span) = eval.doc().node(under).and_then(|node| node.span) {
                spans.push((span.start - duck.attack, span.end() + duck.release));
            }
        }
        spans.extend(lines.iter().map(|line| (line.start - duck.attack, line.end + duck.release)));
    }
    spans.sort_by(|a, b| a.0.total_cmp(&b.0));
    (spans, duck.depth_db)
}

/// The timeline of `doc`, with voiceover lines placed and captions made by
/// `eval`. `titles` names media by asset id; `retired` says which media
/// the library no longer has.
pub fn project(eval: &CutEval, titles: &HashMap<String, String>, retired: &dyn Fn(&MediaRef) -> bool) -> Projection {
    let doc = eval.doc();
    let mut projection = Projection { duration: doc.duration, ..Default::default() };
    let block = |node: &CutNode, kind: BlockKind, label: String, lane: Lane, track: u32, span: Span| Block {
        node: node.id.clone(), owner: node.id.clone(), line: None, kind, label, lane, track,
        start: span.start, length: span.length, keys: keys_of(node), broken: false, stale: false, sound: None,
        picture: None, fx: false, app_run: false, nested: false, graphic: false, wraps: None, level: 0, generate: false, progress: None,
    };
    for node in &doc.nodes {
        match &node.kind {
            CutNodeKind::VoiceOver { voice, lines, shape, .. } => {
                for placed in eval.voice_lines(&node.id) {
                    let length = (placed.end - placed.start).max(0.2);
                    let mut line = block(node, BlockKind::Voice, placed.text.clone(), Lane::Voice, node.track, Span { start: placed.start, length });
                    line.node = format!("{}#{}", node.id, placed.index);
                    line.line = Some(placed.index);
                    line.stale = lines.get(placed.index).is_some_and(|line| line.stale(voice));
                    line.keys.clear();
                    line.sound = Some(Sound {
                        media: placed.take.as_ref().map(|take| take.id.clone()), source_in: 0.0, speed: 1.0,
                        fade_in: shape.fade_in, fade_out: shape.fade_out, ducked: Vec::new(), duck_db: 0.0,
                    });
                    projection.blocks.push(line);
                }
                continue;
            }
            CutNodeKind::Subtitles { source, .. } => {
                let lines = source.as_ref().map_or(&[][..], |source| eval.voice_lines(&source.node));
                for (index, caption) in eval.captions(&node.id).iter().enumerate() {
                    let mut item = block(node, BlockKind::Caption, caption.text(), Lane::Captions, 0, Span { start: caption.start, length: (caption.end - caption.start).max(0.1) });
                    item.node = format!("{}#{}", node.id, index);
                    item.line = Some(index);
                    // Provisional: its words' times are estimates until the
                    // line they belong to has a take.
                    item.stale = lines.iter().any(|line| line.take.is_none() && caption.start >= line.start - 1e-6 && caption.start < line.sound_end.max(line.end));
                    projection.blocks.push(item);
                }
                projection.caption_tracks = 1;
                continue;
            }
            _ => {}
        }
        let Some(span) = node.span else { continue };
        let sound = |src: &MediaRef, source_in: &SourceTime, speed: f64, shape: &AudioShape| {
            let (ducked, duck_db) = ducked(eval, shape);
            Sound {
                media: Some(media_asset(src)),
                source_in: match source_in { SourceTime::Seconds(seconds) => *seconds, SourceTime::Marker { .. } => eval.source_in(node).unwrap_or(0.0) },
                speed, fade_in: shape.fade_in, fade_out: shape.fade_out, ducked, duck_db,
            }
        };
        let item = match &node.kind {
            CutNodeKind::Clip { src, source_in, speed, audio, shape, nested, .. } => {
                let mut item = block(node, BlockKind::Clip, media_label(src, titles), Lane::Video, node.track, span);
                item.broken = retired(src);
                item.nested = *nested;
                // A compound clip's sound is its cut's mix: no one waveform.
                item.sound = (*audio && !*nested).then(|| sound(src, source_in, *speed, shape));
                let start_in = match source_in { SourceTime::Seconds(seconds) => *seconds, SourceTime::Marker { .. } => eval.source_in(node).unwrap_or(0.0) };
                item.picture = Some((media_asset(src), start_in, *speed));
                item.app_run = matches!(src, MediaRef::Run(_));
                item.fx = doc.wiring().any(|(from, _, to, _)| from == node.id && doc.node(to).is_some_and(|to| matches!(to.kind, CutNodeKind::Fx { .. })));
                item
            }
            CutNodeKind::Still { src, .. } => {
                let mut item = block(node, BlockKind::Still, media_label(src, titles), Lane::Video, node.track, span);
                item.broken = retired(src);
                item.picture = Some((media_asset(src), 0.0, 0.0));
                item.fx = doc.wiring().any(|(from, _, to, _)| from == node.id && doc.node(to).is_some_and(|to| matches!(to.kind, CutNodeKind::Fx { .. })));
                item
            }
            CutNodeKind::Title { text, .. } => block(node, BlockKind::Title, text.clone(), Lane::Video, node.track, span),
            CutNodeKind::AudioClip { src, source_in, shape, .. } => {
                let mut item = block(node, BlockKind::Audio, media_label(src, titles), Lane::Audio, node.track, span);
                item.broken = retired(src);
                item.sound = Some(sound(src, source_in, 1.0, shape));
                item
            }
            CutNodeKind::Fx { effect, input, .. } => {
                let track = input.as_ref().map_or(node.track, |input| base_track(doc, &input.node));
                let mut item = block(node, BlockKind::Effect, effect_label(effect), Lane::Video, track, span);
                // Its depth in the clip's chain of effects.
                let mut current = input.as_ref().map(|input| input.node.clone());
                while let Some(id) = current {
                    match doc.node(&id).map(|node| &node.kind) {
                        Some(CutNodeKind::Fx { input, .. }) => {
                            item.level += 1;
                            current = input.as_ref().map(|input| input.node.clone());
                        }
                        _ => {
                            item.wraps = Some(id);
                            break;
                        }
                    }
                    if item.level as usize > doc.nodes.len() {
                        break;
                    }
                }
                item
            }
            CutNodeKind::Transition { effect, a, b, .. } => {
                let track_of = |id: String| doc.node(&id).map_or(node.track, |node| node.track);
                let track_a = a.as_ref().map_or(node.track, |a| track_of(super::ops::side_base(doc, &a.node, true)));
                let track_b = b.as_ref().map_or(node.track, |b| track_of(super::ops::side_base(doc, &b.node, false)));
                projection.bands.push(Band {
                    node: node.id.clone(),
                    clips: (a.as_ref().map(|a| super::ops::side_base(doc, &a.node, true)).unwrap_or_default(), b.as_ref().map(|b| super::ops::side_base(doc, &b.node, false)).unwrap_or_default()),
                    label: preset_label(effect), start: span.start, length: span.length,
                    tracks: (track_a.min(track_b), track_a.max(track_b)), keys: keys_of(node),
                });
                continue;
            }
            _ => continue,
        };
        projection.blocks.push(item);
    }
    // Motion graphics: blocks on their track (title role), opened by a
    // double click into their layers (`motion_edit.rs`).
    for graphic in &doc.graphics {
        let Some(span) = graphic.span else { continue };
        let label = match &graphic.src {
            makepad_story_cut::GraphicSrc::Asset(asset) => titles.get(&asset.id).cloned().unwrap_or_else(|| format!("Graphic {}", graphic.id)),
            makepad_story_cut::GraphicSrc::Builtin(name) => format!("Graphic · {name}"),
            makepad_story_cut::GraphicSrc::Tree(tree) => tree.document.clone(),
        };
        projection.blocks.push(Block {
            node: graphic.id.clone(), owner: graphic.id.clone(), line: None, kind: BlockKind::Title, label, lane: Lane::Video, track: graphic.track,
            start: span.start, length: span.length, keys: Vec::new(), broken: false, stale: false, sound: None,
            picture: match &graphic.src {
                makepad_story_cut::GraphicSrc::Asset(asset) => Some((asset.id.clone(), 0.0, 0.0)),
                makepad_story_cut::GraphicSrc::Builtin(_) | makepad_story_cut::GraphicSrc::Tree(_) => None,
            },
            fx: false, app_run: false, nested: false, graphic: true, wraps: None, level: 0, generate: false, progress: None,
        });
    }
    // Generate segments: a band on the rail over the clip they
    // wrap (above its effects), or over the range on V1.
    for gen in &doc.generates {
        let base = super::ops::chain_base(doc, &gen.id);
        let clip = super::ops::timed_block(doc, &base);
        let mut level = 0;
        let mut current = gen.input.as_ref().map(|wire| wire.node.clone());
        while let Some(id) = current {
            match doc.node(&id).map(|node| &node.kind) {
                Some(CutNodeKind::Fx { input, .. }) => {
                    level += 1;
                    current = input.as_ref().map(|wire| wire.node.clone());
                }
                _ => break,
            }
            if level as usize > doc.nodes.len() {
                break;
            }
        }
        let prompt: String = gen.prompt.chars().take(28).collect();
        let takes = match gen.takes.len() {
            0 => "no take yet".to_string(),
            n => format!("take {} of {n}", gen.take.min(n - 1) + 1),
        };
        let label = if prompt.is_empty() { format!("Generate · {takes}") } else { format!("Generate “{prompt}” · {takes}") };
        projection.blocks.push(Block {
            node: gen.id.clone(), owner: gen.id.clone(), line: None, kind: BlockKind::Effect, label, lane: Lane::Video,
            track: clip.as_ref().map_or(0, |clip| clip.track), start: gen.span.start, length: gen.span.length, keys: Vec::new(),
            broken: false, stale: false, sound: None, picture: None, fx: false, app_run: false, nested: false, graphic: false,
            wraps: clip.map(|clip| clip.id), level, generate: true, progress: None,
        });
    }
    let max_track = |lane: Lane| projection.blocks.iter().filter(|block| block.lane == lane).map(|block| block.track + 1).max().unwrap_or(0);
    // One empty track above the picture and below the sound to drop onto.
    projection.video_tracks = max_track(Lane::Video).max(projection.bands.iter().map(|band| band.tracks.1 + 1).max().unwrap_or(0)) + 1;
    projection.audio_tracks = max_track(Lane::Audio) + 1;
    projection.voice_tracks = max_track(Lane::Voice).max(1);
    projection.content_end = projection.blocks.iter().map(|block| block.start + block.length).fold(0.0, f64::max);
    projection
}

/// One thing the placeholder preview draws, in output pixels.
#[derive(Clone, Debug, PartialEq)]
pub enum SketchItem {
    /// A source's picture (its library thumbnail) at `dst` with `opacity`;
    /// `uv` is the part of the source it shows, in fractions.
    Picture { node: String, media: MediaRef, dst: [f64; 4], uv: [f64; 4], opacity: f64, still: bool, source_time: Option<f64> },
    Text { text: String, position: [f64; 2], font_size: f64 },
    /// A named effect or transition, drawn as a caption.
    Caption { text: String },
}

/// Flatten a frame plan into what a picture-less preview can show: every
/// source it reads at its place, text, and the names of the effects and
/// transitions on the way. A transition crossfades its inputs by `mix`.
/// `size_of` answers the source sizes the plan was made with.
pub fn sketch(plan: &FramePlan, size_of: &dyn Fn(&MediaRef) -> Option<(u32, u32)>) -> Vec<SketchItem> {
    let mut items = Vec::new();
    if let Some(result) = plan.result {
        walk(plan, result, 1.0, size_of, &mut items);
    }
    items
}

fn walk(plan: &FramePlan, slot: Slot, opacity: f64, size_of: &dyn Fn(&MediaRef) -> Option<(u32, u32)>, items: &mut Vec<SketchItem>) {
    let Some(op) = plan.ops.get(slot) else { return };
    // Every op of a plan reads only earlier slots, so this terminates.
    match &op.kind {
        DrawKind::Source { media, still, source_time, src_rect, dst_rect, opacity: own, .. } => {
            let uv = match (src_rect, size_of(media)) {
                (Some(src), Some((w, h))) if w > 0 && h > 0 => {
                    let (w, h) = (w as f64, h as f64);
                    [src.x / w, src.y / h, (src.x + src.w) / w, (src.y + src.h) / h]
                }
                _ => [0.0, 0.0, 1.0, 1.0],
            };
            let dst = dst_rect.map_or([0.0, 0.0, op.size.0 as f64, op.size.1 as f64], |rect| [rect.x, rect.y, rect.w, rect.h]);
            // Sources are drawn at the plan's output size unless a Reframe
            // asked for native ones; scale those into the output.
            let (sx, sy) = (plan.size.0 as f64 / op.size.0.max(1) as f64, plan.size.1 as f64 / op.size.1.max(1) as f64);
            items.push(SketchItem::Picture {
                node: op.node.clone(), media: media.clone(), dst: [dst[0] * sx, dst[1] * sy, dst[2] * sx, dst[3] * sy], uv,
                opacity: own * opacity, still: *still, source_time: *source_time,
            });
        }
        DrawKind::Text { text, position, font_size, .. } => items.push(SketchItem::Text { text: text.clone(), position: *position, font_size: *font_size }),
        // A motion graphic: its name, as the sketch shows effects.
        DrawKind::Graphic { .. } => items.push(SketchItem::Caption { text: "GRAPHIC".into() }),
        DrawKind::App { app, .. } => items.push(SketchItem::Caption { text: format!("APP {app}") }),
        DrawKind::Code { label, .. } => items.push(SketchItem::Caption { text: format!("CODE {label}") }),
        DrawKind::Camera { spec } => items.push(SketchItem::Caption { text: format!("CAMERA {}", spec.device.label()) }),
        DrawKind::Effect { effect, input, .. } => {
            if let Some(input) = input.filter(|input| *input < slot) {
                walk(plan, input, opacity, size_of, items);
            }
            items.push(SketchItem::Caption { text: format!("FX {}", effect_label(effect)) });
        }
        DrawKind::Transition { effect, a, b, mix, .. } => {
            if let Some(a) = a.filter(|a| *a < slot) {
                walk(plan, a, opacity * (1.0 - mix), size_of, items);
            }
            if let Some(b) = b.filter(|b| *b < slot) {
                walk(plan, b, opacity * mix, size_of, items);
            }
            items.push(SketchItem::Caption { text: format!("{} {:.0}%", preset_label(effect).to_uppercase(), mix * 100.0) });
        }
        DrawKind::Stack { layers } => {
            for (layer, _) in layers.iter().filter(|(layer, _)| *layer < slot) {
                walk(plan, *layer, opacity, size_of, items);
            }
        }
        DrawKind::Reframe { input, .. } => {
            if *input < slot {
                walk(plan, *input, opacity, size_of, items);
            }
            items.push(SketchItem::Caption { text: "REFRAME".into() });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use makepad_story_cut::CutEval;

    const EDIT: &str = r#"use mod.flow.*
use mod.cut.*

let a = Clip{src: asset("ast_aeaqcaibaeaqcaibaeaqcaibae", "arev_1111111111111111111111111111111111111111111111111111111111111111") span: [0.0, 6.0] track: 0}
let b = Clip{src: asset("ast_aibaeaqcaibaeaqcaibaeaqcai", "arev_2222222222222222222222222222222222222222222222222222222222222222") span: [5.0, 5.0] track: 1}
let t = Transition{a: a.out(@frame) b: b.out(@frame) effect: "13_pixel_dissolve" span: [5.0, 1.0]}
let fx = Fx{input: t.out(@frame) effect: "15_acid_bloom" p1: keys([0.0, 0.2], [2.0, 0.8]) span: [4.0, 4.0]}
let main = Stack{layers: [conn(1, fx.out(@frame))]}
let music = AudioClip{src: asset("ast_aeaqcaibaeaqcaibaeaqcaibae", "arev_1111111111111111111111111111111111111111111111111111111111111111") span: [0.0, 10.0] track: 0}
let out = CutOutput{frame: main.out(@frame) audio: [conn(1, music.out(@audio))]}

Flow{
    kind: @cut
    label: "P"
    output: {width: 1080 height: 1920 fps: 60}
    duration: 10.0
    a, b, t, fx, main, music, out
}
"#;

    #[test]
    fn blocks_bands_tracks_and_keys() {
        let doc = CutDoc::load(EDIT).unwrap_or_else(|diags| panic!("{}", super::super::patch::diagnostics(diags)));
        let mut titles = HashMap::new();
        titles.insert("ast_aeaqcaibaeaqcaibaeaqcaibae".to_string(), "Intro".to_string());
        let eval = CutEval::new(&doc);
        let projection = project(&eval, &titles, &|_| false);
        assert_eq!((projection.video_tracks, projection.audio_tracks, projection.voice_tracks, projection.content_end), (3, 2, 1, 10.0));
        let a = projection.block("a").unwrap();
        assert_eq!((a.label.as_str(), a.track, a.lane), ("Intro", 0, Lane::Video));
        let fx = projection.block("fx").unwrap();
        assert_eq!((fx.kind, fx.track, fx.keys.clone()), (BlockKind::Effect, 0, vec![4.0, 6.0]));
        assert_eq!(projection.bands[0].tracks, (0, 1));
        // No effect pack is installed: the band's label comes from the preset's id.
        assert_eq!(projection.bands[0].label, "Pixel dissolve");
    }

    #[test]
    fn the_sketch_crossfades_inside_a_transition() {
        let doc = CutDoc::load(EDIT).unwrap();
        let eval = CutEval::new(&doc);
        let items = sketch(&eval.frame_plan(5.5), &|_| None);
        let pictures = items.iter().filter_map(|item| match item {
            SketchItem::Picture { node, opacity, .. } => Some((node.clone(), (*opacity * 100.0).round())),
            _ => None,
        }).collect::<Vec<_>>();
        assert_eq!(pictures, vec![("a".to_string(), 50.0), ("b".to_string(), 50.0)]);
        assert!(items.iter().any(|item| matches!(item, SketchItem::Caption { text } if text.to_uppercase().contains("DISSOLVE"))));
        assert!(items.iter().any(|item| matches!(item, SketchItem::Caption { text } if text.to_lowercase() == "fx acid bloom")));
        let before = sketch(&eval.frame_plan(4.5), &|_| None);
        assert!(matches!(&before[0], SketchItem::Picture { node, .. } if node == "a"));
    }
}
