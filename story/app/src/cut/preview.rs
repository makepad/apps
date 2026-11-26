//! The CUT page's viewer (design §4.3): the frame at the playhead at the
//! edit's output aspect on video black, drawn by the cut renderer's own
//! preview (`makepad_story_edit_render::PreviewSource`), so effects,
//! transitions, titles, captions and reframes show exactly as they will
//! render.
//!
//! The renderer records its passes from this widget's draw and takes every
//! event through it. While it loads (media resolving, effects building) or
//! when it fails, the viewer draws a sketch of `frame_plan(t)` instead:
//! each source's library thumbnail at its place, titles as text. Over the
//! frame: a preview-state chip (PREVIEW / SKETCH / LOADING), the safe-zone
//! and platform overlays when switched on, a skim border, and when the
//! renderer refuses the cut a danger banner with the reason wrapped and a
//! button that selects the node it names.

use super::project::SketchItem;
use makepad_library_browser::draw::{DrawEditBlock, DrawEditPicture};
use makepad_story_edit_render::{CutThumbnail, PreviewSource, PreviewStatus, Thumbnail};
use makepad_story_cut::reframe::SafeZone;
use makepad_story_cut::MediaRef;
use makepad_widgets::badge::advance;
use makepad_widgets::*;
use std::collections::{HashMap, VecDeque};
use crate::still_cache::{Demand, Stills};

script_mod! {
    use mod.prelude.widgets_internal.*
    use mod.widgets.*
    let stage = mod.stage

    mod.widgets.CutPreviewBase = #(CutPreview::register_widget(vm))
    mod.widgets.CutPreview = set_type_default() do mod.widgets.CutPreviewBase{
        width: Fill height: Fill
        draw_bg +: {color: stage.video}
        draw_frame +: {color: #000}
        draw_box +: {color: stage.ghost}
        draw_caption +: {color: stage.glass_ink text_style: theme.font_regular{font_size: 9}}
        draw_title +: {color: #fff text_style: theme.font_bold{font_size: 12}}
        draw_note +: {color: stage.text text_style: theme.font_regular{font_size: 9}}
        draw_chip +: {color: stage.glass_ink text_style: theme.font_bold{font_size: 7}}
        icon_alert +: {svg: crate_resource("self:resources/icons/alert.svg")}
    }
}

#[derive(Clone, Debug, Default)]
pub enum PreviewAction {
    #[default]
    None,
    /// The banner's button: select this node.
    Select(String),
}

/// A platform's covered bands in `frame`, with their captions: the
/// reframe solver's own (`SafeZone::bands`).
fn platform_bands(safe: SafeZone, frame: Rect) -> Vec<(Rect, &'static str)> {
    if safe == SafeZone::None {
        return Vec::new();
    }
    let b = safe.bands();
    let (w, h) = (frame.size.x, frame.size.y);
    let middle = (frame.pos.y + h * b.top, h * (1.0 - b.top - b.bottom));
    vec![
        (Rect { pos: frame.pos, size: dvec2(w, h * b.top) }, ""),
        (Rect { pos: dvec2(frame.pos.x, frame.pos.y + h * (1.0 - b.bottom)), size: dvec2(w, h * b.bottom) }, "UI · CAPTION"),
        (Rect { pos: dvec2(frame.pos.x, middle.0), size: dvec2(w * b.left, middle.1) }, ""),
        (Rect { pos: dvec2(frame.pos.x + w * (1.0 - b.right), middle.0), size: dvec2(w * b.right, middle.1) }, "BUTTONS"),
    ]
}

/// A renderer message for people: `name` becomes “name”, and `fx1: :`
/// runs collapse.
/// Asset ids in a message replaced by their titles (“name”), or dropped
/// (review 2, B2): people never read `ast_…:arev_…`.
pub(super) fn named_reason(text: &str, names: &HashMap<String, String>) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(at) = rest.find("ast_") {
        out.push_str(&rest[..at]);
        let tail = &rest[at..];
        let end = tail.find(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '_' || ch == ':')).unwrap_or(tail.len());
        let token = &tail[..end];
        let id = token.split(':').next().unwrap_or(token);
        match names.get(id) {
            Some(title) => out.push_str(&format!("“{title}”")),
            None => out.push_str("an asset"),
        }
        rest = &tail[end..];
    }
    out.push_str(rest);
    out
}

pub(super) fn plain_reason(text: &str) -> String {
    let mut out = String::new();
    let mut open = true;
    for ch in text.chars() {
        if ch == '`' {
            out.push(if open { '“' } else { '”' });
            open = !open;
        } else {
            out.push(ch);
        }
    }
    out.replace("”: :", "”:").replace(" : ", ": ").replace("render: “", "render “")
}

/// The words of `text` in lines no wider than `width` (at most `max`, the
/// last one ellipsized).
fn wrap(draw: &DrawText, cx: &mut Cx2d, text: &str, width: f64, max: usize) -> Vec<String> {
    wrap_measured(text, width, max, &mut |s| advance(draw, cx, s))
}

/// [`wrap`] with its measure. Only what can show is measured (each word,
/// a line with its next word): a reason may run to thousands of words
/// (every problem of a document that does not load), and every string
/// measured is shaped and kept by the text cache.
fn wrap_measured(text: &str, width: f64, max: usize, measure: &mut dyn FnMut(&str) -> f64) -> Vec<String> {
    let max = max.max(1);
    let mut lines: Vec<String> = Vec::new();
    // One line past `max` says the text goes on.
    'words: for word in text.split_whitespace() {
        // A word wider than the line (an id) is cut into pieces that fit,
        // each grown a character at a time.
        let mut pieces: Vec<String> = Vec::new();
        let mut piece = String::new();
        let wide = measure(word) > width;
        for ch in word.chars().filter(|_| wide) {
            piece.push(ch);
            if piece.chars().count() > 1 && measure(&piece) > width {
                piece.pop();
                pieces.push(std::mem::take(&mut piece));
                piece.push(ch);
                if lines.len() + pieces.len() > max {
                    break;
                }
            }
        }
        pieces.push(if wide { piece } else { word.to_string() });
        for piece in pieces {
            match lines.last_mut() {
                Some(line) if measure(&format!("{line} {piece}")) <= width => {
                    line.push(' ');
                    line.push_str(&piece);
                }
                _ => lines.push(piece),
            }
            if lines.len() > max {
                break 'words;
            }
        }
    }
    if lines.len() > max {
        lines.truncate(max);
        let mut last = lines.pop().unwrap_or_default();
        while !last.is_empty() && measure(&format!("{last}…")) > width {
            last.pop();
        }
        lines.push(format!("{}…", last.trim_end()));
    }
    lines
}

#[derive(Script, ScriptHook, Widget)]
pub struct CutPreview {
    #[uid]
    uid: WidgetUid,
    #[walk]
    walk: Walk,
    #[redraw]
    #[live]
    draw_bg: DrawColor,
    #[live]
    draw_frame: DrawColor,
    #[live]
    draw_box: DrawColor,
    #[live]
    draw_shape: DrawEditBlock,
    #[live]
    draw_picture: DrawEditPicture,
    #[live]
    draw_caption: DrawText,
    #[live]
    draw_title: DrawText,
    #[live]
    draw_note: DrawText,
    #[live]
    draw_chip: DrawText,
    #[live]
    icon_alert: DrawSvg,
    /// Output size in pixels.
    #[rust]
    output: (u32, u32),
    #[rust]
    items: Vec<SketchItem>,
    /// The renderer of this cut revision, and the time it is asked for.
    #[rust]
    source: Option<Box<dyn PreviewSource>>,
    /// A renderer of a newer revision loading behind the one shown, which
    /// stays until the new one's picture is whole ([`Self::set_source_held`]).
    #[rust]
    pending: Option<Box<dyn PreviewSource>>,
    #[rust]
    want: f64,
    #[rust]
    thumbs: HashMap<String, Texture>,
    #[rust]
    labels: HashMap<String, String>,
    /// The cut's node ids, effects first: what the banner may name.
    #[rust]
    nodes: Vec<String>,
    #[rust]
    safe_zones: bool,
    #[rust]
    platform: SafeZone,
    #[rust]
    skimming: bool,
    /// The banner's button, and the node it selects.
    #[rust]
    select: Option<(Rect, String)>,
    #[rust]
    select_hover: bool,
    #[rust]
    next_frame: NextFrame,
    /// A saved revision's thumbnail being drawn by the renderer (L5 round 8):
    /// it draws in this widget's pass.
    #[rust]
    thumbnail: Option<CutThumbnail>,
    #[rust]
    stills: Stills,
    #[rust]
    still_demands: Vec<Demand>,
    #[rust]
    thumbnail_timer: Timer,
    #[rust]
    presented: bool,

    // While the cut plays: its frame accounting (see `Playback`); and
    // when frames were dropped, within the last `DROPS_SHOWN` seconds.
    #[rust]
    playback: Option<Playback>,
    #[rust]
    drops: VecDeque<f64>,
    #[rust]
    drops_timer: Timer,
    // Since when the frame asked for has been waiting, and the reason last
    // logged (a wait over `WAIT_NOTED` seconds is shown and logged).
    #[rust]
    waiting_since: Option<f64>,
    #[rust]
    waiting_logged: String,
}

/// A frame this long in coming is a wait the viewer names.
const WAIT_NOTED: f64 = 1.0;

/// What a playing viewer showed: the frame each display frame asked for
/// is due on screen the display frame after (one frame of recording), so
/// a frame that is not there then was dropped. Frames a fast shuttle
/// passes between two display frames are skipped by design, not dropped.
#[derive(Clone, Copy)]
struct Playback {
    rate: f64,
    fps: f64,
    /// The grid frame asked for at the previous display frame.
    due: Option<i64>,
    /// The last frame counted as dropped (each is counted once).
    counted: Option<i64>,
}

/// How long the dropped-frames note stays after the last drop.
const DROPS_SHOWN: f64 = 10.0;

impl CutPreview {
    pub fn library_stills(&self) -> &crate::still_cache::Stills { &self.stills }

    pub fn set_library_stills(&mut self, cx: &mut Cx, demands: Vec<Demand>) {
        let changed = self.still_demands.len() != demands.len() || self.still_demands.iter().zip(&demands)
            .any(|(a, b)| a.id != b.id || a.size != b.size || !std::sync::Arc::ptr_eq(&a.spec, &b.spec));
        self.still_demands = demands;
        if changed { self.redraw(cx); }
    }

    fn still_idle(&self) -> bool {
        self.presented && self.playback.is_none() && self.pending.is_none()
            && self.source.as_ref().is_none_or(|source| !source.busy())
    }

    /// How graphic `node` loads in the cut shown ([`makepad_story_edit_render::graphic::FilmLoad`]),
    /// once its renderer has loaded.
    pub fn film_load(&mut self, node: &str) -> Option<makepad_story_edit_render::graphic::FilmLoad> {
        self.source.as_mut()?.film_load(node)
    }

    pub fn set_output(&mut self, cx: &mut Cx, width: u32, height: u32) {
        if self.output != (width, height) {
            self.output = (width, height);
            self.redraw(cx);
        }
    }

    /// The sketch shown while the renderer has no frame.
    pub fn set_sketch(&mut self, cx: &mut Cx, items: Vec<SketchItem>) {
        if self.items != items {
            self.items = items;
            self.redraw(cx);
        }
    }

    /// The renderer of a new cut revision (the old one is dropped and its
    /// workers stop); `None` shows the sketch only.
    pub fn set_source(&mut self, cx: &mut Cx, source: Option<Box<dyn PreviewSource>>) {
        self.pending = None;
        self.source = source;
        if let Some(source) = &mut self.source {
            source.seek(self.want);
        }
        self.redraw(cx);
    }

    /// The renderer of a new revision whose picture is the one shown already
    /// (the cut's panel saved what the shown renderer holds live): it loads
    /// behind the shown one and takes its place once its picture is whole,
    /// so the viewer never falls back to the sketch meanwhile.
    pub fn set_source_held(&mut self, cx: &mut Cx, mut source: Box<dyn PreviewSource>) {
        if !self.has_frame() {
            return self.set_source(cx, Some(source));
        }
        source.seek(self.want);
        self.pending = Some(source);
        self.redraw(cx);
    }

    /// Draw `thumbnail` in this pass until it has its frame.
    pub fn set_thumbnail(&mut self, cx: &mut Cx, thumbnail: CutThumbnail) {
        self.thumbnail = Some(thumbnail);
        self.redraw(cx);
    }

    /// The thumbnail's frame, once it is done (taken).
    pub fn take_thumbnail(&mut self) -> Option<Result<Thumbnail, String>> {
        let done = self.thumbnail.as_ref()?.result()?.clone();
        self.thumbnail = None;
        Some(done)
    }

    /// The renderer's graphics load as an editor's (the cut's panel is
    /// open: its values are patched into them live).
    pub fn set_live_literals(&mut self, on: bool) {
        for source in self.source.iter_mut().chain(self.pending.iter_mut()) {
            source.set_live_literals(on);
        }
    }

    /// A held gesture moved the cut's clips: the frames follow `doc` now.
    pub fn retime(&mut self, cx: &mut Cx, doc: std::sync::Arc<makepad_story_cut::CutDoc>) {
        for source in self.source.iter_mut().chain(self.pending.iter_mut()) {
            source.retime(doc.clone());
        }
        self.redraw(cx);
    }

    /// Library documents' texts changed during a held gesture: those that
    /// read them load again behind the ones that ran.
    pub fn replace_texts(&mut self, cx: &mut Cx, graphics: Vec<(makepad_story_cut::AssetRef, String)>, modules: Vec<(makepad_story_cut::AssetRef, String)>) {
        for source in self.source.iter_mut().chain(self.pending.iter_mut()) {
            source.replace_texts(graphics.clone(), modules.clone());
        }
        self.redraw(cx);
    }

    /// A held recolour's step on the copies the graphics' loads made.
    pub fn recolour_copies(&mut self, map: &[([f32; 4], [f32; 4])]) -> usize {
        self.source.iter_mut().chain(self.pending.iter_mut()).map(|s| s.recolour_copies(map)).sum()
    }

    /// A held gesture ended.
    pub fn gesture_end(&mut self) {
        for source in self.source.iter_mut().chain(self.pending.iter_mut()) {
            source.gesture_end();
        }
    }

    /// Literals of the graphics were patched: the frame is drawn again.
    pub fn literals_patched(&mut self, cx: &mut Cx) {
        for source in self.source.iter_mut().chain(self.pending.iter_mut()) {
            source.literals_patched();
        }
        self.redraw(cx);
    }

    /// Pictures the source shows for a clip it has not read yet.
    pub fn set_stand_ins(&mut self, pictures: HashMap<makepad_story_cut::MediaRef, (Texture, u32, u32)>) {
        if let Some(source) = &mut self.source {
            source.set_stand_ins(pictures);
        }
    }

    /// Whether the frame shown is exact for its time (drawn again after a
    /// patch: not until it is whole).
    pub fn frame_exact(&self) -> bool {
        self.source.as_ref().is_some_and(|source| source.frame().is_some() && source.exact())
    }

    /// The renderer's frame as it shows (output size), and its time: what
    /// the film's chat reads back to send.
    pub fn shown_frame(&self) -> Option<(f64, Texture)> {
        self.source.as_ref()?.frame()
    }

    /// Whether the renderer has a frame to show (the sketch is not needed).
    pub fn has_frame(&self) -> bool {
        self.source.as_ref().is_some_and(|source| source.frame().is_some())
    }

    /// `Some((rate, fps, t))` while the cut plays at `rate` with its
    /// playhead at `t`: the viewer plays the sound and counts the frames it
    /// could not show in time.
    pub fn set_playing(&mut self, cx: &mut Cx, playing: Option<(f64, f64, f64)>) {
        for source in self.source.iter_mut().chain(self.pending.iter_mut()) {
            source.set_playing(playing.map(|(rate, _, t)| (rate, t)));
        }
        let playing = playing.map(|(rate, fps, _)| (rate, fps));
        match (playing, &mut self.playback) {
            (Some((rate, fps)), Some(playback)) => {
                playback.rate = rate;
                playback.fps = fps;
            }
            (Some((rate, fps)), None) => self.playback = Some(Playback { rate, fps, due: None, counted: None }),
            (None, Some(_)) => {
                self.playback = None;
                self.redraw(cx);
            }
            (None, None) => {}
        }
    }

    pub fn seek(&mut self, cx: &mut Cx, t: f64) {
        self.want = t;
        for source in self.source.iter_mut().chain(self.pending.iter_mut()) {
            source.seek(t);
        }
        self.redraw(cx);
    }

    /// Thumbnails by asset id, and the names to write on sources without one.
    pub fn set_media(&mut self, cx: &mut Cx, thumbs: HashMap<String, Texture>, labels: HashMap<String, String>) {
        self.thumbs = thumbs;
        self.labels = labels;
        self.redraw(cx);
    }

    /// The overlays and states the page switches: safe zones, the platform
    /// whose UI bands to show (`""` for none), skimming, and the node ids.
    pub fn set_overlays(&mut self, cx: &mut Cx, safe_zones: bool, platform: SafeZone, skimming: bool, nodes: Vec<String>) {
        if (self.safe_zones, self.platform, self.skimming, &self.nodes) != (safe_zones, platform, skimming, &nodes) {
            self.safe_zones = safe_zones;
            self.platform = platform;
            self.skimming = skimming;
            self.nodes = nodes;
            self.redraw(cx);
        }
    }

    fn sketch(&mut self, cx: &mut Cx2d, origin: Vec2d, frame: Rect, scale: f64, dim: f32) {
        for item in self.items.clone() {
            match item {
                SketchItem::Picture { media, dst, uv, opacity, .. } => {
                    let r = Rect { pos: dvec2(origin.x + dst[0] * scale, origin.y + dst[1] * scale), size: dvec2(dst[2] * scale, dst[3] * scale) };
                    let (x0, y0) = (r.pos.x.max(frame.pos.x), r.pos.y.max(frame.pos.y));
                    let (x1, y1) = ((r.pos.x + r.size.x).min(frame.pos.x + frame.size.x), (r.pos.y + r.size.y).min(frame.pos.y + frame.size.y));
                    if x1 <= x0 || y1 <= y0 {
                        continue;
                    }
                    let id = media_id(&media).to_string();
                    match self.thumbs.get(&id).cloned() {
                        Some(texture) => {
                            // What overhangs the frame is cut from the
                            // picture's texture coordinates too.
                            let (fx0, fy0) = ((x0 - r.pos.x) / r.size.x, (y0 - r.pos.y) / r.size.y);
                            let (fx1, fy1) = ((x1 - r.pos.x) / r.size.x, (y1 - r.pos.y) / r.size.y);
                            let (uw, vh) = (uv[2] - uv[0], uv[3] - uv[1]);
                            self.draw_picture.uv = vec4((uv[0] + fx0 * uw) as f32, (uv[1] + fy0 * vh) as f32, (uv[0] + fx1 * uw) as f32, (uv[1] + fy1 * vh) as f32);
                            self.draw_picture.draw_vars.set_texture(0, &texture);
                            self.draw_picture.opacity = opacity as f32 * dim;
                            self.draw_picture.draw_abs(cx, Rect { pos: dvec2(x0, y0), size: dvec2(x1 - x0, y1 - y0) });
                        }
                        None => {
                            let mut color = self.draw_box.color;
                            color.w = (0.25 + 0.5 * opacity) as f32;
                            self.draw_box.color = color;
                            self.draw_box.draw_abs(cx, Rect { pos: dvec2(x0, y0), size: dvec2(x1 - x0, y1 - y0) });
                            let label = self.labels.get(&id).cloned().unwrap_or_default();
                            self.draw_caption.draw_abs(cx, dvec2(x0 + 6.0, y0 + 6.0), &label);
                        }
                    }
                }
                SketchItem::Text { text, position, font_size } => {
                    self.draw_title.text_style.font_size = ((font_size * scale * 0.75) as f32).clamp(6.0, 64.0);
                    let width = advance(&self.draw_title, cx, &text);
                    self.draw_title.draw_abs(cx, dvec2(origin.x + position[0] * scale - width * 0.5, origin.y + position[1] * scale - font_size * scale * 0.5), &text);
                }
                SketchItem::Caption { .. } => {}
            }
        }
    }

    /// A flat rect over the picture (the shape shader batches after it).
    fn line(&mut self, cx: &mut Cx2d, rect: Rect, color: Vec4f) {
        // A hairline is a plain rect (the rounded box has no inside at 1 px).
        if rect.size.x < 2.0 || rect.size.y < 2.0 {
            self.draw_box.color = color;
            self.draw_box.draw_abs(cx, rect);
            return;
        }
        self.draw_shape.fill = color;
        self.draw_shape.edge = color;
        self.draw_shape.stroke = 0.0;
        self.draw_shape.radius = 0.0;
        self.draw_shape.shape = 0.0;
        self.draw_shape.draw_abs(cx, rect);
    }

    /// A dashed 1 px line from `from` along `along` for `length`.
    fn dashes(&mut self, cx: &mut Cx2d, from: Vec2d, along: Vec2d, length: f64, color: Vec4f) {
        let mut d = 0.0;
        while d < length {
            let l = 3.0f64.min(length - d);
            self.line(cx, Rect { pos: from + along * d, size: dvec2(along.x * l + along.y, along.y * l + along.x) }, color);
            d += 6.0;
        }
    }

    /// A 1 px outline of `r`, dashed when `dash` is set.
    fn outline(&mut self, cx: &mut Cx2d, r: Rect, color: Vec4f, dash: bool) {
        let step = if dash { 6.0 } else { 0.0 };
        let mut segments = |from: Vec2d, along: Vec2d, length: f64, this: &mut Self| {
            if step == 0.0 {
                this.line(cx, Rect { pos: from, size: dvec2(along.x * length + along.y, along.y * length + along.x) }, color);
                return;
            }
            let mut d = 0.0;
            while d < length {
                let l = 3.0f64.min(length - d);
                this.line(cx, Rect { pos: from + along * d, size: dvec2(along.x * l + along.y, along.y * l + along.x) }, color);
                d += step;
            }
        };
        segments(r.pos, dvec2(1.0, 0.0), r.size.x, self);
        segments(dvec2(r.pos.x, r.pos.y + r.size.y - 1.0), dvec2(1.0, 0.0), r.size.x, self);
        segments(r.pos, dvec2(0.0, 1.0), r.size.y, self);
        segments(dvec2(r.pos.x + r.size.x - 1.0, r.pos.y), dvec2(0.0, 1.0), r.size.y, self);
    }

    fn overlays(&mut self, cx: &mut Cx2d, frame: Rect, p: &makepad_look::palette::Palette) {
        let white = |alpha: f32| {
            let mut c = Vec4f::from_u32(p.glass_white);
            c.w *= alpha;
            c
        };
        {
            for (band, caption) in platform_bands(self.platform, frame) {
                if band.size.x < 1.0 || band.size.y < 1.0 {
                    continue;
                }
                self.line(cx, band, vec4(0.0, 0.0, 0.0, 0.45));
                // Dashed only on the edges inside the frame; the frame's own
                // edge stays the solid rim.
                let inner = |a: f64, b: f64| (a - b).abs() > 0.5;
                let dash = white(0.5);
                let (l, t, r, b) = (band.pos.x, band.pos.y, band.pos.x + band.size.x, band.pos.y + band.size.y);
                let (fl, ft, fr, fb) = (frame.pos.x, frame.pos.y, frame.pos.x + frame.size.x, frame.pos.y + frame.size.y);
                if inner(t, ft) { self.dashes(cx, dvec2(l, t), dvec2(1.0, 0.0), band.size.x, dash); }
                if inner(b, fb) { self.dashes(cx, dvec2(l, b - 1.0), dvec2(1.0, 0.0), band.size.x, dash); }
                if inner(l, fl) { self.dashes(cx, dvec2(l, t), dvec2(0.0, 1.0), band.size.y, dash); }
                if inner(r, fr) { self.dashes(cx, dvec2(r - 1.0, t), dvec2(0.0, 1.0), band.size.y, dash); }
                if !caption.is_empty() {
                    let text = if caption.starts_with("UI") { format!("{} {caption}", self.platform.id().to_uppercase()) } else { caption.to_string() };
                    let w = advance(&self.draw_chip, cx, &text);
                    // Inside the band when it fits, else inside the frame at
                    // the band's top-inner corner.
                    let x = if w + 12.0 <= band.size.x { l + 6.0 } else if inner(r, fr) { r + 6.0 } else { (l - 6.0 - w).max(fl + 4.0) };
                    self.draw_chip.color = Vec4f::from_u32(p.glass_ink);
                    self.draw_chip.draw_abs(cx, dvec2(x, t + 5.0), &text);
                }
            }
        }
        if self.safe_zones {
            for (fraction, alpha) in [(0.93, 0.35), (0.9, 0.35)] {
                let inset = dvec2(frame.size.x * (1.0 - fraction) * 0.5, frame.size.y * (1.0 - fraction) * 0.5);
                let r = Rect { pos: frame.pos + inset, size: frame.size - inset * 2.0 };
                self.outline(cx, r, white(alpha), false);
                // Corner ticks.
                for (cx0, cy0, dx, dy) in [(r.pos.x, r.pos.y, 1.0, 1.0), (r.pos.x + r.size.x, r.pos.y, -1.0, 1.0), (r.pos.x, r.pos.y + r.size.y, 1.0, -1.0), (r.pos.x + r.size.x, r.pos.y + r.size.y, -1.0, -1.0)] {
                    self.line(cx, Rect { pos: dvec2(cx0.min(cx0 + dx * 8.0), cy0 - if dy < 0.0 { 2.0 } else { 0.0 }), size: dvec2(8.0, 2.0) }, white(0.7));
                    self.line(cx, Rect { pos: dvec2(cx0 - if dx < 0.0 { 2.0 } else { 0.0 }, cy0.min(cy0 + dy * 8.0)), size: dvec2(2.0, 8.0) }, white(0.7));
                }
            }
            let c = frame.pos + frame.size * 0.5;
            self.line(cx, Rect { pos: dvec2(c.x - 6.0, c.y), size: dvec2(12.0, 1.0) }, white(0.35));
            self.line(cx, Rect { pos: dvec2(c.x, c.y - 6.0), size: dvec2(1.0, 12.0) }, white(0.35));
        }
    }

    fn chip(&mut self, cx: &mut Cx2d, at: Vec2d, text: &str, ink: u32, p: &makepad_look::palette::Palette) {
        let w = advance(&self.draw_chip, cx, text) + 12.0;
        let r = Rect { pos: at, size: dvec2(w, 18.0) };
        self.draw_shape.fill = Vec4f::from_u32(p.control_glass);
        self.draw_shape.edge = self.draw_shape.fill;
        self.draw_shape.stroke = 0.0;
        self.draw_shape.radius = 3.0;
        self.draw_shape.shape = 0.0;
        self.draw_shape.draw_abs(cx, r);
        self.draw_chip.color = Vec4f::from_u32(ink);
        self.draw_chip.draw_abs(cx, dvec2(r.pos.x + 6.0, r.pos.y + 4.0), text);
    }

    /// The loading panel at the foot of the picture (the cut and its first
    /// picture): what loads, the percentage, and a bar in the accent.
    /// `foot`: where the card's bottom may reach (the frame's foot, or the
    /// top of the error card under it).
    fn progress_bar(&mut self, cx: &mut Cx2d, frame: Rect, foot: f64, fraction: f64, what: &str, p: &makepad_look::palette::Palette) {
        let fraction = fraction.clamp(0.0, 1.0);
        let width = (frame.size.x - 32.0).min(420.0);
        let top = (foot.min(frame.pos.y + frame.size.y - 12.0) - 44.0).round();
        if width < 80.0 || top < frame.pos.y + 4.0 {
            return;
        }
        let r = Rect { pos: dvec2((frame.pos.x + (frame.size.x - width) * 0.5).round(), top), size: dvec2(width, 44.0) };
        self.draw_shape.fill = Vec4f::from_u32(p.control_glass);
        self.draw_shape.edge = Vec4f::from_u32(p.glass_rim);
        self.draw_shape.stroke = 1.0;
        self.draw_shape.radius = 4.0;
        self.draw_shape.shape = 0.0;
        self.draw_shape.draw_abs(cx, r);
        let percent = format!("{:.0}%", fraction * 100.0);
        let percent_w = advance(&self.draw_chip, cx, &percent);
        self.draw_chip.color = Vec4f::from_u32(p.glass_ink);
        self.draw_chip.draw_abs(cx, dvec2(r.pos.x + 10.0, r.pos.y + 8.0), "LOADING");
        self.draw_chip.draw_abs(cx, dvec2(r.pos.x + r.size.x - 10.0 - percent_w, r.pos.y + 8.0), &percent);
        let text = named_reason(&plain_reason(what), &self.labels);
        let line = wrap(&self.draw_note, cx, &text, r.size.x - 20.0, 1).into_iter().next().unwrap_or_default();
        let (note, chip) = (self.draw_note.color, self.draw_chip.color);
        self.draw_note.color = Vec4f::from_u32(p.glass_label);
        self.draw_note.draw_abs(cx, dvec2(r.pos.x + 10.0, r.pos.y + 20.0), &line);
        (self.draw_note.color, self.draw_chip.color) = (note, chip);
        let track = Rect { pos: dvec2(r.pos.x + 10.0, r.pos.y + r.size.y - 8.0), size: dvec2(r.size.x - 20.0, 3.0) };
        self.draw_shape.stroke = 0.0;
        self.draw_shape.radius = 1.5;
        self.draw_shape.fill = Vec4f::from_u32(p.glass_grid_hi);
        self.draw_shape.edge = self.draw_shape.fill;
        self.draw_shape.draw_abs(cx, track);
        if fraction > 0.0 {
            self.draw_shape.fill = Vec4f::from_u32(p.glass_accent);
            self.draw_shape.edge = self.draw_shape.fill;
            self.draw_shape.draw_abs(cx, Rect { pos: track.pos, size: dvec2((track.size.x * fraction).max(3.0), track.size.y) });
        }
    }

    /// The error card at the foot of the picture; answers its top.
    fn banner(&mut self, cx: &mut Cx2d, rect: Rect, frame: Rect, reason: &str, p: &makepad_look::palette::Palette) -> f64 {
        // Name the node the reason is about: a node id in its words, else
        // the first effect when it speaks of an effect.
        let words: Vec<&str> = reason.split(|ch: char| !(ch.is_alphanumeric() || ch == '_')).collect();
        let node = self.nodes.iter().find(|id| words.contains(&id.as_str())).cloned()
            .or_else(|| reason.contains("effect").then(|| self.nodes.first().cloned()).flatten());
        let width = (rect.size.x - 32.0).min(560.0);
        let button_w = node.as_ref().map_or(0.0, |node| advance(&self.draw_note, cx, &format!("Select {node}")) + 20.0);
        let text_w = width - 20.0 - 12.0 - 8.0 - if button_w > 0.0 { button_w + 12.0 } else { 0.0 };
        let text = if reason.starts_with("Cannot") { reason.to_string() } else { format!("Cannot render {reason}") };
        let text = named_reason(&plain_reason(&text), &self.labels);
        let lines = wrap(&self.draw_note, cx, &text, text_w, 3);
        let h = lines.len() as f64 * 16.0 + 16.0;
        let r = Rect { pos: dvec2(frame.pos.x + (frame.size.x - width) * 0.5, frame.pos.y + frame.size.y - 8.0 - h), size: dvec2(width, h) };
        let mut fill = Vec4f::from_u32(p.danger_wash);
        fill.w = 0.92;
        self.draw_shape.fill = fill;
        self.draw_shape.edge = Vec4f::from_u32(p.danger);
        self.draw_shape.stroke = 1.0;
        self.draw_shape.radius = 4.0;
        self.draw_shape.shape = 0.0;
        self.draw_shape.draw_abs(cx, r);
        self.icon_alert.color = Vec4f::from_u32(p.danger);
        self.icon_alert.draw_abs(cx, Rect { pos: dvec2(r.pos.x + 10.0, r.pos.y + 10.0), size: dvec2(12.0, 12.0) });
        self.draw_note.color = Vec4f::from_u32(p.text);
        for (index, line) in lines.iter().enumerate() {
            self.draw_note.draw_abs(cx, dvec2(r.pos.x + 30.0, r.pos.y + 8.0 + index as f64 * 16.0), line);
        }
        self.select = node.map(|node| {
            let b = Rect { pos: dvec2(r.pos.x + r.size.x - 10.0 - button_w, r.pos.y + (h - 22.0) * 0.5), size: dvec2(button_w, 22.0) };
            self.draw_shape.fill = Vec4f::from_u32(if self.select_hover { p.control_hover } else { p.control });
            self.draw_shape.edge = Vec4f::from_u32(p.border_hi);
            self.draw_shape.radius = 2.5;
            self.draw_shape.draw_abs(cx, b);
            self.draw_note.color = Vec4f::from_u32(p.label);
            self.draw_note.draw_abs(cx, dvec2(b.pos.x + 10.0, b.pos.y + 4.0), &format!("Select {node}"));
            (b, node)
        });
        r.pos.y
    }
}

fn media_id(media: &MediaRef) -> &str {
    match media {
        MediaRef::Asset(asset) => &asset.id,
        MediaRef::Run(run) => &run.id,
        MediaRef::MissingOptional(path) => path,
    }
}

impl Widget for CutPreview {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, _scope: &mut Scope) {
        for source in self.source.iter_mut().chain(self.pending.iter_mut()) {
            source.handle_event(cx, event);
        }
        if let Some(thumbnail) = &mut self.thumbnail { thumbnail.handle_event(cx, event); }
        if self.stills.event(cx, event) { self.redraw(cx); }
        if self.thumbnail_timer.is_event(event).is_some() {
            self.thumbnail_timer = Timer::default();
            self.redraw(cx);
        }
        if self.next_frame.is_event(event).is_some() {
            self.redraw(cx);
        }
        if self.drops_timer.is_event(event).is_some() {
            self.drops_timer = Timer::empty();
            self.redraw(cx);
        }
        match event.hits(cx, self.draw_bg.area()) {
            Hit::FingerHoverIn(fe) | Hit::FingerHoverOver(fe) => {
                let over = self.select.as_ref().is_some_and(|(rect, _)| rect.contains(fe.abs));
                if over != self.select_hover {
                    self.select_hover = over;
                    self.redraw(cx);
                }
                cx.set_cursor(if over { MouseCursor::Hand } else { MouseCursor::Default });
            }
            Hit::FingerUp(fe) if fe.is_over && !fe.cancelled => {
                if let Some((rect, node)) = self.select.clone() {
                    if rect.contains(fe.abs) {
                        cx.widget_action(self.widget_uid(), PreviewAction::Select(node));
                    }
                }
            }
            _ => {}
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, _scope: &mut Scope, walk: Walk) -> DrawStep {
        let rect = cx.walk_turtle(walk);
        self.draw_bg.draw_abs(cx, rect);
        self.select = None;
        let still_idle = self.still_idle();
        self.presented = true;
        self.stills.begin();
        for demand in &self.still_demands { self.stills.want(demand.id, demand.spec.clone(), demand.size); }
        self.stills.draw(cx, still_idle && self.thumbnail.is_none());
        if still_idle {
            if let Some(thumbnail) = &mut self.thumbnail {
                if thumbnail.busy() {
                    thumbnail.draw(cx);
                    if self.thumbnail_timer.0 == 0 { self.thumbnail_timer = cx.start_timeout(1.0 / 30.0); }
                }
            }
        }
        let p = makepad_look::palette::get(cx);
        let (ow, oh) = (self.output.0.max(1) as f64, self.output.1.max(1) as f64);
        let room = dvec2(rect.size.x - 16.0, rect.size.y - 16.0);
        if room.x <= 4.0 || room.y <= 4.0 {
            return DrawStep::done();
        }
        let scale = (room.x / ow).min(room.y / oh);
        let size = dvec2((ow * scale).floor(), (oh * scale).floor());
        let origin = dvec2((rect.pos.x + (rect.size.x - size.x) * 0.5).round(), (rect.pos.y + (rect.size.y - size.y) * 0.5).round());
        let frame = Rect { pos: origin, size };
        self.draw_frame.draw_abs(cx, frame);
        // The renderer records inside this pass; its frame (once it has
        // one) is the picture, else the sketch stands in.
        let mut rendered = None;
        let mut status = None;
        let mut shown = None;
        // A newer renderer loading behind: drawn too, shown once whole.
        if let Some(pending) = &mut self.pending {
            pending.draw(cx);
            self.next_frame = cx.new_next_frame();
            if pending.frame().is_some() && pending.exact() {
                self.source = self.pending.take();
            }
        }
        if let Some(source) = &mut self.source {
            source.draw(cx);
            shown = source.frame().map(|(t, _)| (t, source.exact()));
            rendered = source.frame().map(|(_, texture)| texture);
            status = Some(source.status());
            if source.busy() {
                self.next_frame = cx.new_next_frame();
            }
        }
        let now = Cx::time_now();
        let waiting = self.source.as_ref().and_then(|source| source.waiting_for());
        let waited = match (&waiting, self.waiting_since) {
            (None, _) => {
                self.waiting_since = None;
                None
            }
            (Some(_), None) => {
                self.waiting_since = Some(now);
                None
            }
            (Some(reason), Some(since)) => (now - since > WAIT_NOTED).then(|| (now - since, reason.clone())),
        };
        if let Some((seconds, reason)) = &waited {
            if *reason != self.waiting_logged {
                log!("edit viewer: waiting {seconds:.1} s for {reason}");
                self.waiting_logged = reason.clone();
            }
        }
        // Drops are counted only while the renderer shows the cut (not
        // over a refusal or the sketch).
        let rendering = shown.is_some() && !matches!(status, Some(PreviewStatus::Failed(_)) | Some(PreviewStatus::Loading) | None);
        if let Some(playback) = self.playback.as_mut().filter(|_| rendering) {
            let grid = |t: f64| (t * playback.fps + 1e-6).floor() as i64;
            let on_screen = shown.filter(|(_, exact)| *exact).map(|(t, _)| grid(t));
            if let Some(due) = playback.due {
                let late = on_screen.map_or(true, |n| if playback.rate >= 0.0 { n < due } else { n > due });
                if late && playback.counted != Some(due) {
                    playback.counted = Some(due);
                    self.drops.push_back(now);
                }
            }
            playback.due = Some(grid(self.want));
        }
        while self.drops.front().is_some_and(|at| now - at > DROPS_SHOWN) {
            self.drops.pop_front();
        }
        let failed = match &status {
            Some(PreviewStatus::Failed(reason)) => Some(reason.clone()),
            _ => None,
        };
        // A graphic that cannot draw (its document does not load) is said,
        // never shown blank: its first problem, and how many more.
        let graphic_error = self.source.as_ref().map(|source| source.graphic_errors()).filter(|errors| !errors.is_empty()).map(|errors| {
            let first = errors[0].lines().take(2).collect::<Vec<_>>().join(" ");
            match errors.len() {
                1 => first,
                n => format!("{first} (and {} more)", n - 1),
            }
        });
        // The last good frame dims under a refusal.
        let dim = if failed.is_some() { 0.4 } else { 1.0 };
        match rendered {
            Some(texture) => {
                self.draw_picture.draw_vars.set_texture(0, &texture);
                self.draw_picture.opacity = dim;
                self.draw_picture.uv = vec4(0.0, 0.0, 1.0, 1.0);
                self.draw_picture.draw_abs(cx, frame);
            }
            None => self.sketch(cx, origin, frame, scale, dim),
        }
        self.overlays(cx, frame, &p);
        // The frame's edge: the rim, or the skim border.
        let mut rim = Vec4f::from_u32(if self.skimming { p.glass_white } else { p.glass_rim });
        if self.skimming {
            rim.w *= 0.45;
        }
        self.outline(cx, Rect { pos: frame.pos - dvec2(1.0, 1.0), size: frame.size + dvec2(2.0, 2.0) }, rim, false);
        let (chip, ink) = match (&status, rendered_is_none(&self.source)) {
            (Some(PreviewStatus::Loading), true) => ("LOADING", p.glass_ink),
            (Some(PreviewStatus::Failed(_)), _) | (None, _) => ("SKETCH", p.amber),
            (_, true) => ("SKETCH", p.amber),
            _ => ("PREVIEW", p.glass_ink),
        };
        let chip = if self.skimming { "SKIM" } else { chip };
        self.chip(cx, dvec2(rect.pos.x + 8.0, rect.pos.y + 8.0), chip, ink, &p);
        // Getting the whole edit ready (its load, first picture, the frames
        // drawn ahead): a progress bar with what it prepares, in place of
        // the waiting note, until done.
        let progress = self.source.as_ref().and_then(|source| source.progress());
        // The error card sits at the foot of the picture and the loading
        // card stacks above it, so both read; with no room for both the
        // error wins.
        let mut foot = frame.pos.y + frame.size.y;
        if let Some(reason) = failed.or(graphic_error) {
            foot = self.banner(cx, rect, frame, &reason, &p) - 6.0;
        }
        if let Some((fraction, what)) = &progress {
            self.progress_bar(cx, frame, foot, *fraction, what, &p);
        }
        if let Some((_, reason)) = waited.as_ref().filter(|_| progress.is_none()) {
            // A frame long in coming says what it waits for, not nothing.
            let reason = plain_reason(reason);
            let lines = wrap(&self.draw_note, cx, &format!("Waiting for {reason}"), (frame.size.x - 16.0).max(200.0), 2);
            for (index, line) in lines.iter().enumerate() {
                self.draw_note.draw_abs(cx, dvec2(rect.pos.x + 8.0, rect.pos.y + 32.0 + index as f64 * 13.0), line);
            }
        }
        if !self.drops.is_empty() {
            // Playback could not show every frame in time: say so quietly,
            // for as long as drops are recent.
            let note = format!("{} DROPPED", self.drops.len());
            let w = advance(&self.draw_chip, cx, &note) + 12.0;
            self.chip(cx, dvec2(rect.pos.x + rect.size.x - 8.0 - w, rect.pos.y + 8.0), &note, p.amber, &p);
            if self.drops_timer.is_empty() {
                // Redraw when the note's time is up.
                self.drops_timer = cx.start_timeout(DROPS_SHOWN);
            }
        }
        DrawStep::done()
    }
}

fn rendered_is_none(source: &Option<Box<dyn PreviewSource>>) -> bool {
    source.as_ref().is_none_or(|source| source.frame().is_none())
}

#[cfg(test)]
mod tests {
    use super::{platform_bands, SafeZone};
    use makepad_widgets::*;

    #[test]
    fn ids_become_names() {
        let mut names = std::collections::HashMap::new();
        names.insert("ast_abc".to_string(), "Notes".to_string());
        assert_eq!(super::named_reason("“clip1” nests edit ast_abc:arev_x1, which cannot be read", &names), "“clip1” nests edit “Notes”, which cannot be read");
        assert_eq!(super::named_reason("ast_zzz failed", &names), "an asset failed");
    }

    #[test]
    fn a_long_reason_wraps_measuring_only_what_shows() {
        // One unit per character; a reason of 20000 words.
        let text = (0..20000).map(|i| format!("word{i}")).collect::<Vec<_>>().join(" ");
        let mut measured = 0usize;
        let mut longest = 0usize;
        let lines = super::wrap_measured(&text, 30.0, 3, &mut |s| {
            measured += 1;
            longest = longest.max(s.chars().count());
            s.chars().count() as f64
        });
        assert_eq!(lines.len(), 3, "{lines:?}");
        assert_eq!(lines[0], "word0 word1 word2 word3 word4");
        assert!(lines[2].ends_with('…') && lines[2].chars().count() <= 30, "{lines:?}");
        assert!(measured < 50 && longest <= 40, "{measured} measures, the longest {longest} characters");
        // A short reason is whole; a word wider than a line is cut to fit.
        assert_eq!(super::wrap_measured("a b", 30.0, 3, &mut |s| s.chars().count() as f64), ["a b"]);
        assert_eq!(super::wrap_measured("abcdefgh", 3.0, 3, &mut |s| s.chars().count() as f64), ["abc", "def", "gh"]);
        let id = "x".repeat(100000);
        let mut measured = 0usize;
        let cut = super::wrap_measured(&id, 10.0, 2, &mut |s| { measured += 1; s.chars().count() as f64 });
        assert_eq!(cut, ["xxxxxxxxxx", "xxxxxxxxx…"]);
        assert!(measured < 100, "{measured} measures");
    }

    #[test]
    fn reasons_read_as_sentences() {
        assert_eq!(super::plain_reason("Cannot render: `fx1`: effect `Fireworks` cannot be seeked"), "Cannot render “fx1”: effect “Fireworks” cannot be seeked");
    }

    #[test]
    fn platforms_cover_their_bands() {
        let frame = Rect { pos: dvec2(0.0, 0.0), size: dvec2(100.0, 200.0) };
        let bands = platform_bands(SafeZone::Reels, frame);
        assert_eq!(bands.len(), 4);
        assert!((bands[1].0.pos.y + bands[1].0.size.y - 200.0).abs() < 1e-9);
        assert!(platform_bands(SafeZone::None, frame).is_empty());
    }
}
