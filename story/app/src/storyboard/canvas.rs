//! The storyboard canvas (the board's 2D view): time runs across under a
//! ruler (seconds, and bars and beats when the board has a tempo), rows run
//! down under their headers, and the items are cards: scene cards with
//! their number, title, intent and badges; notes as stickies with their
//! words; references as pictures, colour swatches, fonts and links; the
//! music reference as a bar with its beats. Links are dotted curves.
//!
//! Drawn on the CUT page's audio-style display (`wave_*` roles), with the
//! page's role hues: scene cards indigo, voiceover cyan, visuals blue,
//! music green, references neutral, free notes a sticky amber.
//!
//! The widget owns only gestures and view state (zoom, pan, the drag in
//! progress). A click selects; shift adds, cmd toggles; a drag moves the
//! selection (to another row too) or resizes an item from either end; a
//! drag on empty space draws a marquee; a double click on empty space adds
//! a note there (a scene card on a story row) and on an item edits its
//! words in place. Each gesture sends one action when it ends; the parent
//! answers with the next board. Moves snap to item edges and seconds, or
//! beats when the board has a tempo; the snap is drawn and named.
//!
//! Main-loop cost: the layout is recomputed per draw (linear in the items:
//! tens), text is measured only for what is on screen.

use makepad_story_storyboard::layout::{self, Hit as BoardHit, Layout, Part, SnapTo, View, HEADER, RULER};
use makepad_story_storyboard::{Body, Item, Media, Role, RowKind, Storyboard};
use makepad_widgets::badge::advance;
use makepad_widgets::*;
use std::collections::HashMap;

script_mod! {
    use mod.prelude.widgets_internal.*
    use mod.widgets.*
    let stage = mod.stage

    set_type_default() do #(DrawBoardShape::script_shader(vm)){
        ..mod.draw.DrawQuad
        pixel: fn() {
            let sdf = Sdf2d.viewport(self.pos * self.rect_size)
            let w = self.rect_size.x
            let h = self.rect_size.y
            let p = self.pos * self.rect_size
            if self.shape > 3.5 {
                // A soft shadow under a card.
                let d = max(abs(p.x - w * 0.5) - (w * 0.5 - 8.), abs(p.y - h * 0.5) - (h * 0.5 - 8.))
                let a = 1. - smoothstep(-6., 8., d)
                return vec4(0., 0., 0., self.fill.w * a * a)
            }
            if self.shape > 2.5 {
                // A dashed rounded outline (drop ghosts, the marquee).
                sdf.box(1., 1., w - 2., h - 2., self.radius)
                sdf.fill_keep(self.fill)
                let dash = step(0.5, fract((p.x + p.y) / 8.))
                sdf.stroke(vec4(self.edge.xyz, self.edge.w * dash), 1.5)
                return sdf.result
            }
            if self.shape > 1.5 {
                // A circle.
                sdf.circle(w * 0.5, h * 0.5, min(w, h) * 0.5 - 0.5)
                sdf.fill_keep(self.fill)
                sdf.stroke(self.edge, self.stroke)
                return sdf.result
            }
            if self.shape > 0.5 {
                // A pentagon pointing down (the playhead's handle).
                sdf.move_to(0.5, 0.5)
                sdf.line_to(w - 0.5, 0.5)
                sdf.line_to(w - 0.5, h * 0.55)
                sdf.line_to(w * 0.5, h - 0.5)
                sdf.line_to(0.5, h * 0.55)
                sdf.close_path()
                sdf.fill(self.fill)
                return sdf.result
            }
            sdf.box(0.5, 0.5, w - 1., h - 1., self.radius)
            sdf.fill_keep(self.fill)
            sdf.stroke(self.edge, self.stroke)
            return sdf.result
        }
    }

    set_type_default() do #(DrawBoardPicture::script_shader(vm)){
        ..mod.draw.DrawQuad
        image: texture_2d(float)
        pixel: fn() {
            let c = self.image.sample(mix(self.uv.xy, self.uv.zw, self.pos))
            return vec4(c.xyz * self.opacity, self.opacity)
        }
    }

    mod.widgets.StoryboardLegendBase = #(StoryboardLegend::register_widget(vm))
    mod.widgets.StoryboardLegend = set_type_default() do mod.widgets.StoryboardLegendBase{
        width: Fill height: 196
        draw_bg +: {color: stage.panel}
        draw_label +: {color: stage.text_bright text_style: theme.font_bold{font_size: 8.5}}
        draw_text +: {color: stage.muted text_style: theme.font_regular{font_size: 7.5}}
    }

    mod.widgets.StoryboardFrameBase = #(StoryboardFrame::register_widget(vm))
    mod.widgets.StoryboardFrame = set_type_default() do mod.widgets.StoryboardFrameBase{
        width: Fill height: 400
        draw_bg +: {color: stage.panel}
        draw_label +: {color: stage.glass_ink text_style: theme.font_bold{font_size: 6.5}}
        draw_text +: {color: stage.glass_ink text_style: theme.font_regular{font_size: 7}}
    }

    mod.widgets.StoryboardCanvasBase = #(StoryboardCanvas::register_widget(vm))
    mod.widgets.StoryboardCanvas = set_type_default() do mod.widgets.StoryboardCanvasBase{
        width: Fill height: Fill
        draw_bg +: {color: stage.wave_bg}
        draw_rect +: {color: stage.wave_rim}
        draw_text +: {color: stage.wave_label text_style: theme.font_regular{font_size: 7.5}}
        draw_title +: {color: stage.glass_ink text_style: theme.font_bold{font_size: 9}}
        draw_body +: {color: stage.glass_ink text_style: theme.font_regular{font_size: 8.5}}
        draw_meta +: {color: stage.glass_ink_soft text_style: theme.font_regular{font_size: 7.5}}
        draw_badge +: {color: stage.on_accent text_style: theme.font_bold{font_size: 7}}
        draw_row +: {color: stage.wave_label text_style: theme.font_bold{font_size: 8}}
        draw_big +: {color: stage.glass_ink text_style: theme.font_bold{font_size: 18}}
        icon_image +: {svg: crate_resource("self:resources/icons/image.svg")}
        icon_video +: {svg: crate_resource("self:resources/icons/video.svg")}
        icon_audio +: {svg: crate_resource("self:resources/icons/audio.svg")}
        icon_link +: {svg: crate_resource("self:resources/icons/http.svg")}
        icon_text +: {svg: crate_resource("self:resources/icons/text.svg")}
        icon_gen +: {svg: crate_resource("self:resources/icons/gen.svg")}
        editor: TextInput{
            width: Fill height: Fit
            is_multiline: true
            submit_on_enter: true
            padding: Inset{left: 6 right: 6 top: 5 bottom: 5}
            draw_bg +: {color: stage.well color_hover: stage.well color_focus: stage.well border_size: 1.0 border_color: stage.accent}
            draw_text +: {color: stage.text_bright text_style: theme.font_regular{font_size: 9}}
        }
    }
}

#[derive(Script, ScriptHook)]
#[repr(C)]
pub struct DrawBoardShape {
    #[deref]
    draw_super: DrawQuad,
    #[live]
    pub fill: Vec4f,
    #[live]
    pub edge: Vec4f,
    #[live]
    pub stroke: f32,
    #[live]
    pub radius: f32,
    /// 0 a rounded box, 1 a pentagon, 2 a circle, 3 a dashed box, 4 a shadow.
    #[live]
    pub shape: f32,
}

#[derive(Script, ScriptHook)]
#[repr(C)]
pub struct DrawBoardPicture {
    #[deref]
    draw_super: DrawQuad,
    #[live]
    pub opacity: f32,
    #[live]
    pub uv: Vec4f,
}

/// Role hues (EDIT design §2.1), and a sticky amber for free notes.
#[derive(Clone, Copy)]
pub struct Hue {
    pub hue: u32,
    pub wash: u32,
    pub strip: u32,
}

impl Hue {
    pub const SCENE: Hue = Hue { hue: 0x7c83f5ff, wash: 0x1c2038ff, strip: 0x363b73ff };
    pub const VOICE: Hue = Hue { hue: 0x3cc2dcff, wash: 0x14313aff, strip: 0x236877ff };
    pub const VISUAL: Hue = Hue { hue: 0x4f8feaff, wash: 0x18273dff, strip: 0x2c4e7eff };
    pub const MUSIC: Hue = Hue { hue: 0x2fb894ff, wash: 0x112f2cff, strip: 0x1c6253ff };
    pub const REF: Hue = Hue { hue: 0xc2ccd8ff, wash: 0x22262dff, strip: 0x4a5059ff };
    pub const NOTE: Hue = Hue { hue: 0xf0c75eff, wash: 0x3a3220ff, strip: 0x7a6431ff };
    pub const CHAPTER: Hue = Hue { hue: 0xff8a3dff, wash: 0x2a1d14ff, strip: 0x6e3f1fff };
    pub const TREATMENT: Hue = Hue { hue: 0xe07bb5ff, wash: 0x2c1b27ff, strip: 0x683a57ff };
    pub const BIBLE: Hue = Hue { hue: 0xd6c7a1ff, wash: 0x26241eff, strip: 0x5e5746ff };
    pub const SECTION: Hue = Hue { hue: 0x5fc9a8ff, wash: 0x142a26ff, strip: 0x2a5d51ff };
    pub const LYRIC: Hue = Hue { hue: 0xf4f0e6ff, wash: 0x1d1c1aff, strip: 0x4a4740ff };
    pub const DEVICE: Hue = Hue { hue: 0x8fb4ffff, wash: 0x16202fff, strip: 0x2d4368ff };
    pub const GENERATE: u32 = 0xa77bffff;
    pub const STALE: u32 = 0xf0b34dff;

    pub fn of_row(kind: RowKind) -> Hue {
        match kind {
            RowKind::Story => Hue::SCENE,
            RowKind::Visuals => Hue::VISUAL,
            RowKind::Voice => Hue::VOICE,
            RowKind::Music => Hue::MUSIC,
            RowKind::Refs => Hue::REF,
            RowKind::Notes => Hue::NOTE,
            RowKind::Chapters => Hue::CHAPTER,
            RowKind::Treatments => Hue::TREATMENT,
            RowKind::Bible => Hue::BIBLE,
            RowKind::Sections => Hue::SECTION,
            RowKind::Lyrics => Hue::LYRIC,
            RowKind::Devices => Hue::DEVICE,
        }
    }
}

/// Dark or light ink for type on `ground` (0xRRGGBBAA).
fn ink_on(ground: u32) -> u32 {
    let (r, g, b) = ((ground >> 24) & 0xff, (ground >> 16) & 0xff, (ground >> 8) & 0xff);
    if r * 299 + g * 587 + b * 114 > 150_000 { 0x14171cff } else { 0xf4f7faff }
}

#[derive(Clone, Debug, Default)]
pub enum BoardAction {
    #[default]
    None,
    /// The selection changed (by a click or a marquee).
    Select(Vec<String>),
    /// Items moved or resized: `(id, start, length, row)`.
    Place(Vec<(String, f64, f64, Option<String>)>),
    /// A double click on empty space: a new item on `row` at `t`.
    Add { row: String, t: f64 },
    /// An item's words, edited in place.
    Text { id: String, text: String },
    /// A click or drag on the ruler: the cut's playhead goes there.
    Scrub(f64),
    /// What a drag snapped to, for the status line.
    Snapped(String),
    /// A key the canvas does not use itself (delete, undo, paste, …).
    Key(KeyEvent),
    /// Text pasted or typed into the canvas (a URL, a note).
    Paste(String),
    PasteClipboard,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Grab {
    Body,
    Start,
    End,
    Ruler,
    Marquee,
    /// A middle-button or space drag: pan.
    Pan,
}

#[derive(Clone, Debug)]
struct Drag {
    grab: Grab,
    /// The item grabbed.
    id: Option<String>,
    origin: Vec2d,
    corner: Vec2d,
    moved: bool,
    /// The dragged items' places when it began: `(id, start, length, row)`.
    from: Vec<(String, f64, f64, String)>,
    /// The preview: where they are now.
    preview: Vec<(String, f64, f64, String)>,
    /// The selection a marquee started from, and 0 replace / 1 add / 2 toggle.
    base: (Vec<String>, u8),
    /// The view when a pan began.
    view: (f64, f64),
}

/// What the parent tells the canvas about each item beyond the board.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Badges {
    /// Items whose part of the cut is out of date.
    pub stale: Vec<String>,
    /// Items the cut was not built from yet.
    pub unbuilt: Vec<String>,
}

#[derive(Script, ScriptHook, Widget)]
pub struct StoryboardCanvas {
    #[uid]
    uid: WidgetUid,
    #[walk]
    walk: Walk,
    #[redraw]
    #[live]
    draw_bg: DrawColor,
    #[live]
    draw_rect: DrawColor,
    #[live]
    draw_shape: DrawBoardShape,
    #[live]
    draw_picture: DrawBoardPicture,
    #[live]
    draw_text: DrawText,
    #[live]
    draw_title: DrawText,
    #[live]
    draw_body: DrawText,
    #[live]
    draw_meta: DrawText,
    #[live]
    draw_badge: DrawText,
    #[live]
    draw_row: DrawText,
    #[live]
    draw_big: DrawText,
    #[live]
    icon_image: DrawSvg,
    #[live]
    icon_video: DrawSvg,
    #[live]
    icon_audio: DrawSvg,
    #[live]
    icon_link: DrawSvg,
    #[live]
    icon_text: DrawSvg,
    #[live]
    icon_gen: DrawSvg,
    #[live]
    editor: TextInput,
    #[rust]
    board: Storyboard,
    #[rust]
    badges: Badges,
    /// Pictures by asset id.
    #[rust]
    pictures: HashMap<String, Texture>,
    #[rust]
    selected: Vec<String>,
    #[rust]
    view: View,
    #[rust]
    rect: Rect,
    #[rust]
    drag: Option<Drag>,
    #[rust]
    hovered: Option<(String, Part)>,
    #[rust]
    snap: Option<(f64, String)>,
    #[rust]
    snap_mode: Option<SnapTo>,
    #[rust]
    playhead: f64,
    /// A library item dragged over the board: its landing `(row, t, length, label)`.
    #[rust]
    drop_hint: Option<(String, f64, f64, String)>,
    /// The item whose words are being edited in place.
    #[rust]
    editing: Option<String>,
    /// An item to cut in place once the board has it (a new note).
    #[rust]
    cut_next: Option<String>,
    /// The editor takes the keys once it is drawn.
    #[rust]
    focus_editor: bool,
    #[rust]
    fit_pending: bool,
    #[rust]
    initialized: bool,
    #[rust]
    focused: bool,
    #[rust]
    empty_text: String,
}

fn color(value: u32, alpha: f32) -> Vec4f {
    let mut color = Vec4f::from_u32(value);
    color.w *= alpha;
    color
}

fn r(x: f64, y: f64, w: f64, h: f64) -> Rect {
    Rect { pos: dvec2(x, y), size: dvec2(w, h) }
}

impl StoryboardCanvas {
    // ------------------------------------------------------------ parent API

    pub fn set_board(&mut self, cx: &mut Cx, board: &Storyboard, badges: Badges) {
        let first = self.board.items.is_empty() && self.board.rows.is_empty();
        if self.board != *board || self.badges != badges {
            self.board = board.clone();
            self.badges = badges;
            // The drag's landing is the board now.
            if self.drag.as_ref().is_some_and(|drag| drag.moved) {
                self.drag = None;
            }
            self.selected.retain(|id| board.item(id).is_some());
            if first {
                self.fit_pending = true;
            }
            self.redraw(cx);
        }
        if let Some(id) = self.cut_next.take() {
            if self.board.item(&id).is_some() {
                self.cut_text(cx, &id);
            }
        }
    }

    pub fn set_pictures(&mut self, cx: &mut Cx, pictures: HashMap<String, Texture>) {
        if pictures.len() != self.pictures.len() || pictures.keys().any(|key| !self.pictures.contains_key(key)) {
            self.pictures = pictures;
            self.redraw(cx);
        }
    }

    pub fn set_selected(&mut self, cx: &mut Cx, selected: Vec<String>) {
        if self.selected != selected {
            self.selected = selected;
            self.redraw(cx);
        }
    }

    pub fn set_playhead(&mut self, cx: &mut Cx, t: f64) {
        if (self.playhead - t).abs() > 1e-6 {
            self.playhead = t;
            self.redraw(cx);
        }
    }

    pub fn set_snap(&mut self, cx: &mut Cx, on: bool) {
        let mode = if !on { SnapTo::Off } else if self.board.bpm.is_some() { SnapTo::Beats } else { SnapTo::Seconds };
        if self.snap_mode != Some(mode) {
            self.snap_mode = Some(mode);
            self.redraw(cx);
        }
    }

    pub fn set_empty_text(&mut self, cx: &mut Cx, text: &str) {
        if self.empty_text != text {
            self.empty_text = text.to_string();
            self.redraw(cx);
        }
    }

    pub fn fit(&mut self, cx: &mut Cx) {
        self.fit_pending = true;
        self.redraw(cx);
    }

    pub fn zoom(&mut self, cx: &mut Cx, factor: f64) -> f64 {
        let anchor = self.view.x_of(self.playhead).clamp(HEADER, self.rect.size.x.max(HEADER + 1.0));
        self.view.zoom_at(factor, anchor);
        self.redraw(cx);
        self.view.px_per_sec
    }

    pub fn focus(&self, cx: &mut Cx) {
        cx.set_key_focus(self.draw_bg.area());
    }

    /// Where a library item dropped at `abs` lands: its row and time.
    pub fn drop_at(&self, abs: Vec2d) -> Option<(String, f64)> {
        if !self.rect.contains(abs) || self.rect.size.x <= HEADER {
            return None;
        }
        let (x, y) = (abs.x - self.rect.pos.x, abs.y - self.rect.pos.y);
        let layout = self.layout();
        let row = layout::row_at(&layout, y)?;
        Some((row.id.clone(), self.view.t_of(x).max(0.0)))
    }

    pub fn set_drop_hint(&mut self, cx: &mut Cx, hint: Option<(String, f64, f64, String)>) {
        if self.drop_hint != hint {
            self.drop_hint = hint;
            self.redraw(cx);
        }
    }

    /// Edit an item's words in place (a new note, a double click, Enter).
    pub fn cut_text(&mut self, cx: &mut Cx, id: &str) {
        let Some(item) = self.board.item(id) else {
            self.cut_next = Some(id.to_string());
            return;
        };
        let text = match &item.body {
            Body::Note { text } => text.clone(),
            Body::Scene(scene) => scene.title.clone(),
            Body::Ref(reference) => reference.label.clone(),
            Body::Chapter(chapter) => chapter.skill.clone(),
            Body::Treatment(treatment) => treatment.title.clone(),
            Body::Page(page) => page.title.clone(),
            Body::Lyric(lyric) => lyric.text.clone(),
            Body::Device(device) => device.value.clone(),
        };
        self.editing = Some(id.to_string());
        self.editor.set_text(cx, &text);
        self.focus_editor = true;
        self.redraw(cx);
    }


    // --------------------------------------------------------------- helpers

    fn layout(&self) -> Layout {
        let mut view = self.view;
        view.width = self.rect.size.x;
        view.height = self.rect.size.y;
        layout::layout(&self.board, &view)
    }

    /// The board with the drag's preview applied.
    fn shown_board(&self) -> Storyboard {
        let mut board = self.board.clone();
        if let Some(drag) = &self.drag {
            for (id, start, length, row) in &drag.preview {
                if let Some(item) = board.item_mut(id) {
                    item.start = *start;
                    item.length = *length;
                    item.row = row.clone();
                }
            }
        }
        board
    }

    fn local(&self, abs: Vec2d) -> (f64, f64) {
        (abs.x - self.rect.pos.x, abs.y - self.rect.pos.y)
    }

    fn snap_time(&mut self, t: f64, except: &[String], invert: bool) -> f64 {
        let mode = self.snap_mode.unwrap_or(SnapTo::Seconds);
        let mode = if invert { if mode == SnapTo::Off { if self.board.bpm.is_some() { SnapTo::Beats } else { SnapTo::Seconds } } else { SnapTo::Off } } else { mode };
        let snapped = layout::snap(t, mode, &self.board, self.view.px_per_sec, &layout::edges(&self.board, except));
        if let Some(what) = snapped.what.clone() {
            self.snap = Some((snapped.t, what));
        }
        snapped.t
    }

    fn fit_text(draw: &DrawText, cx: &mut Cx2d, text: &str, width: f64) -> Option<String> {
        if width < 10.0 {
            return None;
        }
        if advance(draw, cx, text) <= width {
            return Some(text.to_string());
        }
        let chars: Vec<char> = text.chars().collect();
        let (mut lo, mut hi) = (0usize, chars.len());
        while lo < hi {
            let mid = (lo + hi).div_ceil(2);
            let candidate = format!("{}…", chars[..mid].iter().collect::<String>());
            if advance(draw, cx, &candidate) <= width { lo = mid } else { hi = mid - 1 }
        }
        (lo > 0).then(|| format!("{}…", chars[..lo].iter().collect::<String>()))
    }

    /// Words wrapped into at most `lines` lines of `width`; the last one
    /// ellipsized.
    fn wrap(draw: &DrawText, cx: &mut Cx2d, text: &str, width: f64, lines: usize) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        if width < 12.0 || lines == 0 {
            return out;
        }
        for paragraph in text.split('\n') {
            let mut line = String::new();
            for word in paragraph.split_whitespace() {
                let candidate = if line.is_empty() { word.to_string() } else { format!("{line} {word}") };
                if advance(draw, cx, &candidate) <= width || line.is_empty() {
                    line = candidate;
                } else {
                    out.push(std::mem::take(&mut line));
                    line = word.to_string();
                }
                if out.len() >= lines {
                    break;
                }
            }
            if out.len() >= lines {
                break;
            }
            out.push(line);
        }
        let truncated = out.len() > lines || (out.len() == lines && out.iter().map(|l| l.len() + 1).sum::<usize>() < text.len());
        out.truncate(lines);
        if truncated {
            if let Some(last) = out.last_mut() {
                let longer = format!("{last} …");
                *last = Self::fit_text(draw, cx, &longer, width).unwrap_or_default();
            }
        }
        for line in &mut out {
            if advance(draw, cx, line) > width {
                *line = Self::fit_text(draw, cx, line, width).unwrap_or_default();
            }
        }
        out
    }

    fn rect(&mut self, cx: &mut Cx2d, rect: Rect, c: Vec4f) {
        if rect.size.x > 0.0 && rect.size.y > 0.0 {
            self.draw_rect.color = c;
            self.draw_rect.draw_abs(cx, rect);
        }
    }

    fn shape(&mut self, cx: &mut Cx2d, rect: Rect, shape: f32, fill: Vec4f, edge: Vec4f, stroke: f32, radius: f32) {
        if rect.size.x <= 0.0 || rect.size.y <= 0.0 {
            return;
        }
        self.draw_shape.fill = fill;
        self.draw_shape.edge = edge;
        self.draw_shape.stroke = stroke;
        self.draw_shape.radius = radius;
        self.draw_shape.shape = shape;
        self.draw_shape.draw_abs(cx, rect);
    }

    fn text_with(draw: &mut DrawText, cx: &mut Cx2d, pos: Vec2d, text: &str, width: f64, c: Vec4f) -> f64 {
        let Some(text) = Self::fit_text(draw, cx, text, width) else { return 0.0 };
        draw.color = c;
        draw.draw_abs(cx, pos, &text);
        advance(draw, cx, &text)
    }

    fn badge(&mut self, cx: &mut Cx2d, x: f64, y: f64, text: &str, fill: u32, ink: u32) -> f64 {
        let w = advance(&self.draw_badge, cx, text) + 8.0;
        self.shape(cx, r(x, y, w, 14.0), 0.0, color(fill, 1.0), color(fill, 1.0), 0.0, 2.0);
        self.draw_badge.color = color(ink, 1.0);
        self.draw_badge.draw_abs(cx, dvec2(x + 4.0, y + 1.5), text);
        w
    }

    /// A picture covering `rect` (cropped to its aspect).
    fn picture(&mut self, cx: &mut Cx2d, texture: &Texture, rect: Rect, opacity: f32) {
        let (tw, th) = texture.get_format(cx).vec_width_height().unwrap_or((16, 9));
        let (tw, th) = (tw.max(1) as f64, th.max(1) as f64);
        let (rw, rh) = (rect.size.x.max(1.0), rect.size.y.max(1.0));
        let (mut u0, mut v0, mut u1, mut v1) = (0.0, 0.0, 1.0, 1.0);
        if tw / th > rw / rh {
            let visible = (rw / rh) / (tw / th);
            u0 = (1.0 - visible) * 0.5;
            u1 = 1.0 - u0;
        } else {
            let visible = (tw / th) / (rw / rh);
            v0 = (1.0 - visible) * 0.5;
            v1 = 1.0 - v0;
        }
        self.draw_picture.draw_vars.set_texture(0, texture);
        self.draw_picture.opacity = opacity;
        self.draw_picture.uv = vec4(u0 as f32, v0 as f32, u1 as f32, v1 as f32);
        self.draw_picture.draw_abs(cx, rect);
    }

    // ------------------------------------------------------------------ draw

    fn draw_ruler(&mut self, cx: &mut Cx2d, p: &makepad_look::palette::Palette) {
        let o = self.rect.pos;
        let width = self.rect.size.x;
        self.rect(cx, r(o.x, o.y, width, RULER), color(p.wave_ruler_bg, 1.0));
        self.rect(cx, r(o.x, o.y + RULER - 1.0, width, 1.0), color(p.wave_rim, 1.0));
        let t0 = self.view.t_of(HEADER);
        let t1 = self.view.t_of(width);
        let seconds = layout::second_ticks(self.view.px_per_sec, t0, t1, 56.0);
        let beats = self.board.bpm.map(|bpm| layout::beat_ticks(bpm, self.board.beat_offset, self.board.beats_per_bar, self.view.px_per_sec, t0, t1)).unwrap_or_default();
        // Seconds on the upper half, bars and beats below.
        let split = if beats.is_empty() { RULER } else { RULER * 0.55 };
        for tick in &seconds {
            let x = o.x + self.view.x_of(tick.t);
            if x < o.x + HEADER {
                continue;
            }
            let h = if tick.major { split * 0.45 } else { split * 0.22 };
            self.rect(cx, r(x, o.y + split - h, 1.0, h), color(if tick.major { p.wave_label } else { p.wave_muted }, if tick.major { 0.7 } else { 0.5 }));
            if let Some(label) = &tick.label {
                Self::text_with(&mut self.draw_text, cx, dvec2(x + 3.0, o.y + 2.0), label, 60.0, color(p.wave_label, 1.0));
            }
        }
        for tick in &beats {
            let x = o.x + self.view.x_of(tick.t);
            if x < o.x + HEADER {
                continue;
            }
            let h = if tick.major { RULER - split - 2.0 } else { (RULER - split) * 0.35 };
            self.rect(cx, r(x, o.y + RULER - 1.0 - h, 1.0, h), color(Hue::MUSIC.hue, if tick.major { 0.8 } else { 0.45 }));
            if let Some(label) = &tick.label {
                Self::text_with(&mut self.draw_text, cx, dvec2(x + 3.0, o.y + split + 1.0), label, 40.0, color(Hue::MUSIC.hue, 1.0));
            }
        }
        // The board's end.
        let end = o.x + self.view.x_of(self.board.duration);
        if end > o.x + HEADER && end < o.x + width {
            self.rect(cx, r(end, o.y, 1.0, RULER), color(p.wave_label, 0.8));
        }
        // The cut's playhead handle.
        let x = o.x + self.view.x_of(self.playhead);
        if x >= o.x + HEADER && x <= o.x + width {
            self.shape(cx, r(x - 6.0, o.y + RULER - 12.0, 12.0, 12.0), 1.0, color(p.accent, 1.0), color(p.accent, 1.0), 0.0, 0.0);
        }
        // The header corner.
        self.rect(cx, r(o.x, o.y, HEADER, RULER), color(p.wave_ruler_bg, 1.0));
        let label = if let Some(bpm) = self.board.bpm { format!("{} BPM · {}/4", makepad_story_storyboard::splash::number(bpm).trim_end_matches(".0"), self.board.beats_per_bar) } else { "SECONDS".to_string() };
        Self::text_with(&mut self.draw_text, cx, dvec2(o.x + 12.0, o.y + 9.0), &label, HEADER - 16.0, color(p.wave_muted, 1.0));
        self.rect(cx, r(o.x + HEADER - 1.0, o.y, 1.0, RULER), color(p.wave_rim, 1.0));
    }

    fn draw_item(&mut self, cx: &mut Cx2d, board: &Storyboard, item: &Item, rect: Rect, p: &makepad_look::palette::Palette) {
        let kind = board.row(&item.row).map_or(RowKind::Notes, |row| row.kind);
        let selected = self.selected.contains(&item.id);
        let hovered = self.hovered.as_ref().is_some_and(|(id, _)| *id == item.id);
        let stale = self.badges.stale.contains(&item.id);
        let unbuilt = self.badges.unbuilt.contains(&item.id);
        let clip_x0 = self.rect.pos.x + HEADER;
        // Cards cast a soft shadow onto the board.
        self.shape(cx, r(rect.pos.x - 4.0, rect.pos.y - 2.0, rect.size.x + 8.0, rect.size.y + 8.0), 4.0, color(0x000000ff, 0.35), color(0, 0.0), 0.0, 0.0);
        match &item.body {
            Body::Scene(scene) => {
                let hue = Hue::SCENE;
                let wash = if hovered { 0x232848ff } else { hue.wash };
                self.shape(cx, rect, 0.0, color(wash, 1.0), color(hue.hue, 0.7), 1.0, 5.0);
                // The strip: number and title.
                let strip = r(rect.pos.x + 1.0, rect.pos.y + 1.0, rect.size.x - 2.0, 20.0);
                self.shape(cx, strip, 0.0, color(hue.strip, 1.0), color(hue.strip, 1.0), 0.0, 4.0);
                self.rect(cx, r(strip.pos.x, strip.pos.y + 12.0, strip.size.x, 8.0), color(hue.strip, 1.0));
                let mut x = strip.pos.x.max(clip_x0) + 6.0;
                let number = board.scene_number(&item.id).unwrap_or(0);
                if strip.size.x > 60.0 {
                    x += self.badge(cx, x, strip.pos.y + 3.0, &format!("{number}"), hue.hue, 0x10122aff) + 6.0;
                }
                let title_w = strip.pos.x + strip.size.x - x - 6.0;
                Self::text_with(&mut self.draw_title, cx, dvec2(x, strip.pos.y + 4.0), &scene.title, title_w, color(p.glass_ink, 1.0));
                // Body: the intent, then mood and look.
                let bx = rect.pos.x.max(clip_x0) + 8.0;
                let bw = rect.pos.x + rect.size.x - bx - 8.0;
                let mut y = rect.pos.y + 26.0;
                let bottom = rect.pos.y + rect.size.y - 20.0;
                if !scene.intent.is_empty() {
                    let lines = Self::wrap(&self.draw_body, cx, &scene.intent, bw, ((bottom - y) / 13.0).max(0.0) as usize).into_iter().take(2).collect::<Vec<_>>();
                    for line in lines {
                        Self::text_with(&mut self.draw_body, cx, dvec2(bx, y), &line, bw, color(p.glass_ink, 1.0));
                        y += 13.0;
                    }
                }
                for (label, value) in [("MOOD", &scene.mood), ("LOOK", &scene.look), ("MUSIC", &scene.music)] {
                    if value.is_empty() || y + 11.0 > bottom {
                        continue;
                    }
                    let w = Self::text_with(&mut self.draw_badge, cx, dvec2(bx, y + 1.0), label, 40.0, color(hue.hue, 1.0));
                    Self::text_with(&mut self.draw_meta, cx, dvec2(bx + w + 5.0, y), value, bw - w - 5.0, color(p.glass_ink_soft, 1.0));
                    y += 12.0;
                }
                // Badges along the bottom.
                let mut bx2 = bx;
                let by = rect.pos.y + rect.size.y - 18.0;
                if !scene.kit.is_empty() && rect.size.x > 90.0 {
                    bx2 += self.badge(cx, bx2, by, &scene.kit.trim_start_matches('@').to_uppercase(), 0x2c3150ff, p.glass_ink) + 4.0;
                }
                if scene.gen.from.is_some() && rect.size.x > 130.0 {
                    bx2 += self.badge(cx, bx2, by, "GEN", Hue::GENERATE, 0x1a1030ff) + 4.0;
                }
                if (stale || unbuilt) && rect.size.x > 170.0 {
                    let (text, fill) = if stale { ("OUT OF DATE", Hue::STALE) } else { ("NOT BUILT", 0x6f7b87ff) };
                    self.badge(cx, bx2, by, text, fill, 0x1c1206ff);
                }
            }
            Body::Chapter(chapter) => {
                // The chapter's own ground, its number in mono, the skill big.
                let ground = item.color.unwrap_or(Hue::CHAPTER.wash);
                let ink = ink_on(ground);
                let edge = if hovered { p.text_hi } else { Hue::CHAPTER.hue };
                self.shape(cx, rect, 0.0, color(ground, 1.0), color(edge, 0.8), 1.0, 5.0);
                let x = rect.pos.x.max(clip_x0) + 8.0;
                let w = rect.pos.x + rect.size.x - x - 8.0;
                let number = format!("{:02}/{:02}", board.chapter_number(&item.id).unwrap_or(0), board.chapters().len());
                let nw = Self::text_with(&mut self.draw_badge, cx, dvec2(x, rect.pos.y + 6.0), &number, w, color(ink, 0.7));
                if !board.on_beat(item.start) && w > 90.0 {
                    self.badge(cx, x + nw + 6.0, rect.pos.y + 4.0, "OFF BEAT", Hue::STALE, 0x1c1206ff);
                }
                // The skill big when it fits, else in the title face.
                if advance(&self.draw_big, cx, &chapter.skill) <= w {
                    Self::text_with(&mut self.draw_big, cx, dvec2(x, rect.pos.y + 18.0), &chapter.skill, w, color(ink, 1.0));
                } else {
                    Self::text_with(&mut self.draw_title, cx, dvec2(x, rect.pos.y + 22.0), &chapter.skill, w, color(ink, 1.0));
                }
                let mut y = rect.pos.y + 44.0;
                let bottom = rect.pos.y + rect.size.y - 6.0;
                for (label, value) in [("LOOK", &chapter.look), ("HERO", &chapter.hero), ("OUT →", &chapter.out)] {
                    if value.is_empty() || y + 11.0 > bottom {
                        continue;
                    }
                    let lw = Self::text_with(&mut self.draw_badge, cx, dvec2(x, y + 1.0), label, 44.0, color(ink, 0.6));
                    Self::text_with(&mut self.draw_meta, cx, dvec2(x + lw + 5.0, y), value, w - lw - 5.0, color(ink, 0.95));
                    y += 12.0;
                }
                if (stale || unbuilt) && rect.size.x > 60.0 {
                    self.shape(cx, r(rect.pos.x + rect.size.x - 12.0, rect.pos.y + 5.0, 7.0, 7.0), 2.0, color(Hue::STALE, 1.0), color(Hue::STALE, 1.0), 0.0, 0.0);
                }
            }
            Body::Treatment(treatment) => {
                let hue = Hue::TREATMENT;
                let edge = if treatment.chosen { p.accent } else { hue.hue };
                self.shape(cx, rect, 0.0, color(if hovered { hue.strip } else { hue.wash }, 1.0), color(edge, if treatment.chosen { 1.0 } else { 0.7 }), if treatment.chosen { 2.0 } else { 1.0 }, 5.0);
                let x = rect.pos.x.max(clip_x0) + 8.0;
                let w = rect.pos.x + rect.size.x - x - 8.0;
                let mut tx = x;
                if treatment.chosen && w > 80.0 {
                    tx += self.badge(cx, x, rect.pos.y + 6.0, "CHOSEN", p.accent, p.on_accent) + 6.0;
                }
                Self::text_with(&mut self.draw_title, cx, dvec2(tx, rect.pos.y + 6.0), &treatment.title, w - (tx - x), color(p.glass_ink, 1.0));
                let swatch_h = if treatment.palette.is_empty() { 0.0 } else { 14.0 };
                let lines = ((rect.size.y - 32.0 - swatch_h) / 13.0).max(0.0) as usize;
                let body = if treatment.look.is_empty() { treatment.idea.clone() } else { format!("{}\n{}", treatment.idea, treatment.look) };
                let mut y = rect.pos.y + 24.0;
                for line in Self::wrap(&self.draw_body, cx, body.trim(), w, lines) {
                    Self::text_with(&mut self.draw_body, cx, dvec2(x, y), &line, w, color(p.glass_ink_soft, 1.0));
                    y += 13.0;
                }
                if !treatment.palette.is_empty() {
                    let n = treatment.palette.len() as f64;
                    let sw = (w / n).min(28.0);
                    for (index, c) in treatment.palette.iter().enumerate() {
                        self.shape(cx, r(x + index as f64 * (sw + 2.0), rect.pos.y + rect.size.y - 18.0, sw, 12.0), 0.0, color(*c, 1.0), color(0xffffff30, 1.0), 1.0, 2.0);
                    }
                }
            }
            Body::Lyric(lyric) => {
                // The sung line in its voice's colour, its number and voice
                // on top, the pun, the set and the device under it.
                let voice = lyric.voice.as_deref().and_then(|id| board.voice(id)).cloned();
                let ink = voice.as_ref().and_then(|voice| voice.color).unwrap_or(Hue::LYRIC.hue);
                let edge = if hovered { p.text_hi } else { ink };
                self.shape(cx, rect, 0.0, color(if hovered { Hue::LYRIC.strip } else { Hue::LYRIC.wash }, 1.0), color(edge, 0.6), 1.0, 4.0);
                self.rect(cx, r(rect.pos.x + 1.0, rect.pos.y + 3.0, 3.0, rect.size.y - 6.0), color(ink, 1.0));
                let x = rect.pos.x.max(clip_x0) + 10.0;
                let w = rect.pos.x + rect.size.x - x - 6.0;
                let number = format!("LINE {:02}", board.line_number(&item.id).unwrap_or(0));
                let mut hx = x + Self::text_with(&mut self.draw_badge, cx, dvec2(x, rect.pos.y + 6.0), &number, w, color(p.glass_ink_soft, 1.0)) + 6.0;
                if let (Some(id), true) = (&lyric.voice, w > 110.0) {
                    let label = voice.as_ref().map_or(id.to_uppercase(), |voice| voice.label.to_uppercase());
                    hx += self.badge(cx, hx, rect.pos.y + 4.0, &label, ink, ink_on(ink)) + 4.0;
                }
                let _ = hx;
                let mut y = rect.pos.y + 20.0;
                for line in Self::wrap(&self.draw_title, cx, &lyric.text, w, 2) {
                    Self::text_with(&mut self.draw_title, cx, dvec2(x, y), &line, w, color(ink, 1.0));
                    y += 14.0;
                }
                let bottom = rect.pos.y + rect.size.y - 5.0;
                for (label, value) in [("PUN", &lyric.pun), ("SET", &lyric.set), ("DEVICE", &lyric.device)] {
                    if value.is_empty() || y + 11.0 > bottom {
                        continue;
                    }
                    let lw = Self::text_with(&mut self.draw_badge, cx, dvec2(x, y + 1.0), label, 48.0, color(Hue::CHAPTER.hue, 0.9));
                    Self::text_with(&mut self.draw_meta, cx, dvec2(x + lw + 5.0, y), value, w - lw - 5.0, color(p.glass_ink_soft, 1.0));
                    y += 12.0;
                }
                if (stale || unbuilt) && rect.size.x > 60.0 {
                    self.shape(cx, r(rect.pos.x + rect.size.x - 12.0, rect.pos.y + 5.0, 7.0, 7.0), 2.0, color(Hue::STALE, 1.0), color(Hue::STALE, 1.0), 0.0, 0.0);
                }
            }
            Body::Device(device) => {
                // One occurrence: the device's name small, its value big.
                let hue = Hue::DEVICE;
                self.shape(cx, rect, 0.0, color(if hovered { hue.strip } else { hue.wash }, 1.0), color(hue.hue, 0.7), 1.0, 4.0);
                let x = rect.pos.x.max(clip_x0) + 8.0;
                let w = rect.pos.x + rect.size.x - x - 6.0;
                Self::text_with(&mut self.draw_badge, cx, dvec2(x, rect.pos.y + 5.0), &device.name.to_uppercase(), w, color(hue.hue, 1.0));
                if advance(&self.draw_big, cx, &device.value) <= w {
                    Self::text_with(&mut self.draw_big, cx, dvec2(x, rect.pos.y + 16.0), &device.value, w, color(p.glass_ink, 1.0));
                } else {
                    Self::text_with(&mut self.draw_title, cx, dvec2(x, rect.pos.y + 19.0), &device.value, w, color(p.glass_ink, 1.0));
                }
            }
            Body::Page(page) => {
                // A page of the bible: paper, with its title and text.
                let paper = if hovered { 0xe9e2d0ff } else { 0xe2dbc8ff };
                self.shape(cx, rect, 0.0, color(paper, 1.0), color(Hue::BIBLE.hue, 1.0), 1.0, 3.0);
                let x = rect.pos.x.max(clip_x0) + 8.0;
                let w = rect.pos.x + rect.size.x - x - 8.0;
                Self::text_with(&mut self.draw_title, cx, dvec2(x, rect.pos.y + 6.0), &page.title, w, color(0x14171cff, 1.0));
                let lines = ((rect.size.y - 34.0) / 13.0).max(0.0) as usize;
                let mut y = rect.pos.y + 23.0;
                for line in Self::wrap(&self.draw_body, cx, &page.body, w, lines) {
                    Self::text_with(&mut self.draw_body, cx, dvec2(x, y), &line, w, color(0x2b2f36ff, 1.0));
                    y += 13.0;
                }
                if !page.refs.is_empty() {
                    Self::text_with(&mut self.draw_badge, cx, dvec2(x, rect.pos.y + rect.size.y - 13.0), &format!("{} REFERENCES", page.refs.len()), w, color(0x5e5746ff, 1.0));
                }
            }
            Body::Note { text } => {
                let hue = if item.color.is_some() { Hue::NOTE } else { Hue::of_row(kind) };
                let edge = item.color.unwrap_or(hue.hue);
                let wash = if hovered { hue.strip } else { hue.wash };
                self.shape(cx, rect, 0.0, color(wash, 1.0), color(edge, 0.75), 1.0, 4.0);
                self.rect(cx, r(rect.pos.x + 1.0, rect.pos.y + 3.0, 3.0, rect.size.y - 6.0), color(edge, 1.0));
                let tx = rect.pos.x.max(clip_x0) + 10.0;
                let tw = rect.pos.x + rect.size.x - tx - 6.0;
                let lines = ((rect.size.y - 10.0) / 13.0).max(1.0) as usize;
                let quote = kind == RowKind::Voice;
                let shown = if quote { format!("“{}”", text.trim()) } else { text.clone() };
                let wrapped = Self::wrap(&self.draw_body, cx, &shown, tw, lines);
                let mut y = rect.pos.y + 6.0;
                for line in wrapped {
                    Self::text_with(&mut self.draw_body, cx, dvec2(tx, y), &line, tw, color(p.glass_ink, 1.0));
                    y += 13.0;
                }
                if stale && rect.size.x > 60.0 {
                    let x = rect.pos.x + rect.size.x - 12.0;
                    self.shape(cx, r(x, rect.pos.y + 4.0, 7.0, 7.0), 2.0, color(Hue::STALE, 1.0), color(Hue::STALE, 1.0), 0.0, 0.0);
                }
            }
            Body::Ref(reference) => {
                let hue = if kind == RowKind::Music { Hue::MUSIC } else { Hue::REF };
                let generate = reference.role == Role::Generate;
                let edge = if generate { Hue::GENERATE } else { hue.hue };
                self.shape(cx, rect, 0.0, color(if hovered { hue.strip } else { hue.wash }, 1.0), color(edge, 0.7), 1.0, 5.0);
                let inner = r(rect.pos.x + 3.0, rect.pos.y + 3.0, rect.size.x - 6.0, rect.size.y - 22.0);
                let picture = reference.src.as_ref().and_then(|src| self.pictures.get(&src.asset_id).cloned());
                match (reference.media, picture) {
                    (Some(Media::Swatch), _) => {
                        let n = reference.colors.len().max(1) as f64;
                        for (index, c) in reference.colors.iter().enumerate() {
                            let w = inner.size.x / n;
                            self.shape(cx, r(inner.pos.x + index as f64 * w, inner.pos.y, w + 0.5, inner.size.y), 0.0, color(*c, 1.0), color(*c, 1.0), 0.0, 3.0);
                        }
                    }
                    (Some(Media::Font), _) => {
                        self.draw_big.color = color(p.glass_ink, 1.0);
                        self.draw_big.draw_abs(cx, dvec2(inner.pos.x.max(clip_x0) + 8.0, inner.pos.y + inner.size.y * 0.5 - 12.0), "Aa");
                    }
                    (Some(Media::Audio), _) | (_, _) if kind == RowKind::Music => {
                        // The music: a band with its beats.
                        let band = r(inner.pos.x, inner.pos.y + inner.size.y * 0.25, inner.size.x, inner.size.y * 0.5);
                        self.shape(cx, band, 0.0, color(hue.strip, 0.6), color(hue.hue, 0.0), 0.0, 3.0);
                        if let Some(bpm) = board.bpm {
                            let beat = 60.0 / bpm;
                            let mut n = ((item.start - board.beat_offset) / beat).ceil() as i64;
                            loop {
                                let t = board.beat_offset + n as f64 * beat;
                                if t > item.end() || n > 100_000 {
                                    break;
                                }
                                let x = self.rect.pos.x + self.view.x_of(t);
                                if x >= clip_x0 && x < self.rect.pos.x + self.rect.size.x {
                                    let bar = n.rem_euclid(board.beats_per_bar.max(1) as i64) == 0;
                                    let h = if bar { band.size.y + 8.0 } else { band.size.y * 0.5 };
                                    self.rect(cx, r(x, band.pos.y + (band.size.y - h) * 0.5, if bar { 2.0 } else { 1.0 }, h), color(hue.hue, if bar { 0.95 } else { 0.55 }));
                                }
                                n += 1;
                            }
                        }
                        self.icon_audio.color = color(hue.hue, 1.0);
                        self.icon_audio.draw_abs(cx, r(inner.pos.x.max(clip_x0) + 4.0, inner.pos.y + 2.0, 11.0, 11.0));
                    }
                    (_, Some(texture)) => {
                        self.picture(cx, &texture, inner, 1.0);
                        if reference.media == Some(Media::Frame) {
                            let at = reference.at.unwrap_or(0.0);
                            self.badge(cx, inner.pos.x + 4.0, inner.pos.y + 4.0, &format!("FRAME {}", layout::time_label(at, true)), 0x000000c0, p.glass_ink);
                        }
                    }
                    (media, None) => {
                        let icon = match media {
                            Some(Media::Video | Media::Frame | Media::Cut) => &mut self.icon_video,
                            Some(Media::Url) => &mut self.icon_link,
                            Some(Media::Audio) => &mut self.icon_audio,
                            Some(Media::Font) => &mut self.icon_text,
                            _ => &mut self.icon_image,
                        };
                        icon.color = color(hue.hue, 0.8);
                        let s = 18.0f64.min(inner.size.y - 4.0).max(8.0);
                        icon.draw_abs(cx, r(inner.pos.x.max(clip_x0) + (inner.size.x.min(80.0) - s) * 0.5, inner.pos.y + (inner.size.y - s) * 0.5, s, s));
                        if media == Some(Media::Url) && !reference.url.is_empty() {
                            Self::text_with(&mut self.draw_meta, cx, dvec2(inner.pos.x.max(clip_x0) + 6.0, inner.pos.y + inner.size.y - 14.0), &reference.url, inner.size.x - 12.0, color(p.glass_ink_soft, 1.0));
                        }
                    }
                }
                // The caption strip.
                let cx0 = rect.pos.x.max(clip_x0) + 6.0;
                let cw = rect.pos.x + rect.size.x - cx0 - 6.0;
                let mut x = cx0;
                if generate && cw > 60.0 {
                    self.icon_gen.color = color(Hue::GENERATE, 1.0);
                    self.icon_gen.draw_abs(cx, r(x, rect.pos.y + rect.size.y - 15.0, 10.0, 10.0));
                    x += 14.0;
                }
                let role = match reference.role {
                    Role::Reference => "",
                    Role::Use => "USE · ",
                    Role::Generate => "GENERATE · ",
                };
                Self::text_with(&mut self.draw_meta, cx, dvec2(x, rect.pos.y + rect.size.y - 16.0), &format!("{role}{}", item.headline()), cw - (x - cx0), color(p.glass_ink, 1.0));
                if (stale || unbuilt) && reference.role != Role::Reference && rect.size.x > 40.0 {
                    self.shape(cx, r(rect.pos.x + rect.size.x - 12.0, rect.pos.y + rect.size.y - 13.0, 7.0, 7.0), 2.0, color(Hue::STALE, 1.0), color(Hue::STALE, 1.0), 0.0, 0.0);
                }
            }
        }
        // Where in the frame it goes.
        if let Some(band) = item.band.as_ref().filter(|_| rect.size.x > 110.0) {
            let label = board.frame().and_then(|frame| frame.band(band)).map_or(band.as_str(), |band| band.label).to_uppercase();
            let w = advance(&self.draw_badge, cx, &label) + 8.0;
            self.badge(cx, rect.pos.x + rect.size.x - w - 6.0, rect.pos.y + rect.size.y - 18.0, &label, 0x000000a0, p.glass_ink);
        }
        if selected {
            self.shape(cx, r(rect.pos.x - 2.0, rect.pos.y - 2.0, rect.size.x + 4.0, rect.size.y + 4.0), 0.0, color(0, 0.0), color(p.accent, 1.0), 2.0, 6.0);
        }
        // Resize grips on the hovered end.
        if let Some((_, part @ (Part::Start | Part::End))) = self.hovered.as_ref().filter(|(id, _)| *id == item.id) {
            let x = if *part == Part::Start { rect.pos.x + 2.0 } else { rect.pos.x + rect.size.x - 5.0 };
            self.shape(cx, r(x, rect.pos.y + rect.size.y * 0.5 - 9.0, 3.0, 18.0), 0.0, color(p.glass_ink, 0.9), color(0, 0.0), 0.0, 1.5);
        }
    }

    /// A dotted curve from one card to another.
    fn draw_link(&mut self, cx: &mut Cx2d, a: Rect, b: Rect, label: &str, p: &makepad_look::palette::Palette) {
        let (from, to) = if a.pos.y <= b.pos.y {
            (dvec2(a.pos.x + a.size.x * 0.5, a.pos.y + a.size.y), dvec2(b.pos.x + b.size.x * 0.5, b.pos.y))
        } else {
            (dvec2(a.pos.x + a.size.x * 0.5, a.pos.y), dvec2(b.pos.x + b.size.x * 0.5, b.pos.y + b.size.y))
        };
        let bend = ((to.y - from.y) * 0.5).abs().max(20.0) * if to.y >= from.y { 1.0 } else { -1.0 };
        let (c1, c2) = (dvec2(from.x, from.y + bend), dvec2(to.x, to.y - bend));
        let length = (to - from).length() + bend.abs();
        let steps = ((length / 6.0) as usize).clamp(4, 400);
        let clip_x0 = self.rect.pos.x + HEADER;
        let top = self.rect.pos.y + RULER;
        let mut mid = from;
        for step in 0..=steps {
            let t = step as f64 / steps as f64;
            let u = 1.0 - t;
            let q = from * (u * u * u) + c1 * (3.0 * u * u * t) + c2 * (3.0 * u * t * t) + to * (t * t * t);
            if step == steps / 2 {
                mid = q;
            }
            if q.x < clip_x0 || q.y < top {
                continue;
            }
            self.shape(cx, r(q.x - 1.5, q.y - 1.5, 3.0, 3.0), 2.0, color(p.wave_label, 0.55), color(0, 0.0), 0.0, 0.0);
        }
        for end in [from, to] {
            if end.x >= clip_x0 && end.y >= top {
                self.shape(cx, r(end.x - 3.5, end.y - 3.5, 7.0, 7.0), 2.0, color(p.wave_bg, 1.0), color(p.wave_label, 0.9), 1.5, 0.0);
            }
        }
        if !label.is_empty() && mid.x >= clip_x0 && mid.y >= top {
            let w = advance(&self.draw_badge, cx, &label.to_uppercase()) + 8.0;
            self.badge(cx, mid.x - w * 0.5, mid.y - 7.0, &label.to_uppercase(), 0x2f333aff, p.wave_label);
        }
    }

    fn draw_board(&mut self, cx: &mut Cx2d) {
        let p = makepad_look::palette::get(cx);
        let o = self.rect.pos;
        let (width, height) = (self.rect.size.x, self.rect.size.y);
        let board = self.shown_board();
        let mut view = self.view;
        view.width = width;
        view.height = height;
        let layout = layout::layout(&board, &view);
        let bottom = o.y + height;
        // Row grounds, alternate rows a little lighter.
        for (index, row) in layout.rows.iter().enumerate() {
            let y = o.y + row.rect.y;
            if y > bottom || y + row.rect.h < o.y + RULER {
                continue;
            }
            let ground = if index % 2 == 1 { 0x0f1217ff } else { p.wave_bg };
            self.rect(cx, r(o.x + HEADER, y - layout::ROW_GAP * 0.5, width - HEADER, row.rect.h + layout::ROW_GAP), color(ground, 1.0));
        }
        // Grid: seconds (or bars) through the rows; past the end darkened.
        let t0 = view.t_of(HEADER);
        let t1 = view.t_of(width);
        let grid: Vec<(f64, bool)> = match board.bpm {
            Some(bpm) => layout::beat_ticks(bpm, board.beat_offset, board.beats_per_bar, view.px_per_sec, t0, t1).into_iter().map(|tick| (tick.t, tick.major)).collect(),
            None => layout::second_ticks(view.px_per_sec, t0, t1, 56.0).into_iter().map(|tick| (tick.t, tick.major)).collect(),
        };
        for (t, major) in grid {
            let x = o.x + view.x_of(t);
            if x > o.x + HEADER {
                self.rect(cx, r(x, o.y + RULER, 1.0, height - RULER), color(p.wave_grid, if major { 0.9 } else { 0.35 }));
            }
        }
        let end = o.x + view.x_of(board.duration);
        if end < o.x + width {
            let x = end.max(o.x + HEADER);
            self.rect(cx, r(x, o.y + RULER, o.x + width - x, height - RULER), color(0x000000ff, 0.3));
            if end > o.x + HEADER {
                self.rect(cx, r(end, o.y + RULER, 1.0, height - RULER), color(p.wave_label, 0.35));
            }
        }
        // A running device's occurrences, joined in time order.
        for (_, occurrences) in board.devices() {
            for pair in occurrences.windows(2) {
                if let (Some(a), Some(b)) = (layout.item(&pair[0].0), layout.item(&pair[1].0)) {
                    let (x0, x1) = (o.x + a.rect.x + a.rect.w, o.x + b.rect.x);
                    let y = o.y + a.rect.y + a.rect.h * 0.5;
                    let x0c = x0.max(o.x + HEADER);
                    if x1 > x0c && (o.y + b.rect.y + b.rect.h * 0.5 - y).abs() < 1.0 {
                        self.rect(cx, r(x0c, y, x1 - x0c, 1.0), color(Hue::DEVICE.hue, 0.45));
                        self.shape(cx, r(x1 - 4.0, y - 3.0, 6.0, 6.0), 2.0, color(Hue::DEVICE.hue, 0.8), color(0, 0.0), 0.0, 0.0);
                    }
                }
            }
        }
        // Links under the cards.
        for link in board.links.clone() {
            if let (Some(a), Some(b)) = (layout.item(&link.from), layout.item(&link.to)) {
                let shift = |rect: &layout::Rect| r(o.x + rect.x, o.y + rect.y, rect.w, rect.h);
                self.draw_link(cx, shift(&a.rect), shift(&b.rect), &link.label, &p);
            }
        }
        // Cards.
        let mut drawn = 0;
        for item_box in &layout.items {
            let rect = r(o.x + item_box.rect.x, o.y + item_box.rect.y, item_box.rect.w, item_box.rect.h);
            if rect.pos.x > o.x + width || rect.pos.x + rect.size.x < o.x + HEADER || rect.pos.y > bottom || rect.pos.y + rect.size.y < o.y + RULER {
                continue;
            }
            if self.editing.as_deref() == Some(item_box.id.as_str()) {
                continue;
            }
            if let Some(item) = board.item(&item_box.id).cloned() {
                self.draw_item(cx, &board, &item, rect, &p);
                drawn += 1;
            }
        }
        let _ = drawn;
        // A library item's landing place.
        if let Some((row, t, length, label)) = self.drop_hint.clone() {
            if let Some(row_box) = layout.rows.iter().find(|row_box| row_box.id == row) {
                let x = o.x + view.x_of(t);
                let rect = r(x, o.y + row_box.rect.y + layout::PAD, (length * view.px_per_sec).max(24.0), row_box.rect.h - 2.0 * layout::PAD);
                self.shape(cx, rect, 3.0, color(0x14171cff, 0.92), color(p.accent, 1.0), 1.5, 5.0);
                Self::text_with(&mut self.draw_meta, cx, dvec2(rect.pos.x + 6.0, rect.pos.y + 6.0), &label, rect.size.x - 12.0, color(p.glass_ink, 1.0));
            }
        }
        // The snap line.
        if let (Some((t, what)), Some(drag)) = (self.snap.clone(), &self.drag) {
            if drag.moved {
                let x = o.x + view.x_of(t);
                self.rect(cx, r(x, o.y + RULER, 1.0, height - RULER), color(0xf5c542ff, 0.9));
                let w = advance(&self.draw_badge, cx, &what) + 8.0;
                self.badge(cx, (x - w * 0.5).max(o.x + HEADER), o.y + RULER + 4.0, &what, 0xf5c542ff, 0x1c1206ff);
            }
        }
        // The marquee.
        if let Some(drag) = self.drag.as_ref().filter(|drag| drag.grab == Grab::Marquee && drag.moved) {
            let (a, b) = (drag.origin, drag.corner);
            let rect = r(a.x.min(b.x), a.y.min(b.y), (a.x - b.x).abs(), (a.y - b.y).abs());
            self.shape(cx, rect, 3.0, color(p.accent, 0.08), color(p.accent, 0.9), 1.0, 2.0);
        }
        // The cut's playhead through the rows.
        let x = o.x + view.x_of(self.playhead);
        if x >= o.x + HEADER && x <= o.x + width {
            self.rect(cx, r(x, o.y + RULER, 1.0, height - RULER), color(p.accent, 0.85));
        }
        // Row headers, over everything that scrolled under them.
        self.rect(cx, r(o.x, o.y + RULER, HEADER, height - RULER), color(p.wave_ruler_bg, 1.0));
        for row in layout.rows.iter() {
            let y = o.y + row.rect.y;
            if y > bottom || y + row.rect.h < o.y + RULER {
                continue;
            }
            let hue = Hue::of_row(row.kind);
            let label = board.row(&row.id).map(|row| row.label.to_uppercase()).unwrap_or_default();
            self.rect(cx, r(o.x + 8.0, y + 6.0, 3.0, row.rect.h - 12.0), color(hue.hue, 1.0));
            Self::text_with(&mut self.draw_row, cx, dvec2(o.x + 18.0, y + 8.0), &label, HEADER - 26.0, color(p.wave_label, 1.0));
            let count = board.items.iter().filter(|item| item.row == row.id).count();
            let meta = match row.kind {
                RowKind::Story => format!("{count} scenes"),
                RowKind::Voice => format!("{count} lines"),
                RowKind::Refs => format!("{count} references"),
                _ => format!("{count} items"),
            };
            Self::text_with(&mut self.draw_meta, cx, dvec2(o.x + 18.0, y + 22.0), &meta, HEADER - 26.0, color(p.wave_muted, 1.0));
            self.rect(cx, r(o.x, y + row.rect.h + layout::ROW_GAP * 0.5, width, 1.0), color(p.wave_rim, 0.6));
        }
        self.rect(cx, r(o.x + HEADER - 1.0, o.y + RULER, 1.0, height - RULER), color(p.wave_rim, 1.0));
        self.draw_ruler(cx, &p);
        // The empty board's hint.
        if board.items.is_empty() && !self.empty_text.is_empty() {
            let text = self.empty_text.clone();
            let w = advance(&self.draw_body, cx, &text);
            let y = layout.rows.first().map_or(o.y + height * 0.5, |row| o.y + row.rect.y + row.rect.h * 0.5 - 6.0);
            Self::text_with(&mut self.draw_body, cx, dvec2(o.x + HEADER + ((width - HEADER - w) * 0.5).max(12.0), y), &text, width - HEADER - 24.0, color(p.secondary, 1.0));
        }
        // The in-place editor over its item.
        if let Some(id) = self.editing.clone() {
            if let Some(item_box) = layout.item(&id) {
                let rect = r(o.x + item_box.rect.x.max(HEADER), o.y + item_box.rect.y, item_box.rect.w.max(220.0), item_box.rect.h.max(40.0));
                let walk = Walk::abs_rect(rect);
                let _ = self.editor.draw_walk(cx, &mut Scope::empty(), walk);
                if std::mem::take(&mut self.focus_editor) {
                    self.editor.set_key_focus(cx);
                    self.editor.select_all(cx);
                }
            } else {
                self.editing = None;
            }
        }
    }

    // ---------------------------------------------------------------- input

    fn commit_editor(&mut self, cx: &mut Cx) {
        if let Some(id) = self.editing.take() {
            let text = self.editor.text();
            cx.widget_action(self.widget_uid(), BoardAction::Text { id, text });
            self.focus(cx);
            self.redraw(cx);
        }
    }

    fn places_of(&self, ids: &[String]) -> Vec<(String, f64, f64, String)> {
        ids.iter().filter_map(|id| self.board.item(id)).map(|item| (item.id.clone(), item.start, item.length, item.row.clone())).collect()
    }
}

impl Widget for StoryboardCanvas {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        let uid = self.widget_uid();
        if self.editing.is_some() {
            for action in cx.capture_actions(|cx| self.editor.handle_event(cx, event, scope)) {
                match action.as_widget_action().cast() {
                    TextInputAction::Returned(_, modifiers) if !modifiers.shift => self.commit_editor(cx),
                    TextInputAction::Escaped => {
                        self.editing = None;
                        self.focus(cx);
                        self.redraw(cx);
                    }
                    TextInputAction::KeyFocusLost => self.commit_editor(cx),
                    _ => {}
                }
            }
        }
        match event.hits(cx, self.draw_bg.area()) {
            Hit::KeyFocus(_) => self.focused = true,
            Hit::KeyFocusLost(_) => self.focused = false,
            Hit::FingerHoverIn(fe) | Hit::FingerHoverOver(fe) => {
                let (x, y) = self.local(fe.abs);
                let layout = self.layout();
                let hovered = match layout::hit(&layout, &self.view, x, y) {
                    BoardHit::Item { id, part } => Some((id, part)),
                    _ => None,
                };
                cx.set_cursor(match &hovered {
                    Some((_, Part::Start | Part::End)) => MouseCursor::EwResize,
                    Some(_) => MouseCursor::Hand,
                    None if y < RULER => MouseCursor::Default,
                    None => MouseCursor::Default,
                });
                if hovered != self.hovered {
                    self.hovered = hovered;
                    self.redraw(cx);
                }
            }
            Hit::FingerHoverOut(_) => {
                if self.hovered.take().is_some() {
                    self.redraw(cx);
                }
            }
            Hit::FingerDown(fe) => {
                if self.editing.is_some() {
                    self.commit_editor(cx);
                }
                cx.set_key_focus(self.draw_bg.area());
                let (x, y) = self.local(fe.abs);
                let layout = self.layout();
                let origin = fe.abs;
                let empty = Drag { grab: Grab::Marquee, id: None, origin, corner: origin, moved: false, from: Vec::new(), preview: Vec::new(), base: (Vec::new(), 0), view: (self.view.t0, self.view.y0) };
                if !fe.is_primary_hit() || fe.modifiers.alt {
                    // Pan with the other buttons (or alt-drag).
                    self.drag = Some(Drag { grab: Grab::Pan, ..empty });
                    return;
                }
                match layout::hit(&layout, &self.view, x, y) {
                    BoardHit::Ruler { t } => {
                        let t = t.clamp(0.0, self.board.duration);
                        self.drag = Some(Drag { grab: Grab::Ruler, ..empty });
                        cx.widget_action(uid, BoardAction::Scrub(t));
                    }
                    BoardHit::RowHeader { .. } => {}
                    BoardHit::Item { id, part } => {
                        if fe.tap_count >= 2 {
                            self.selected = vec![id.clone()];
                            cx.widget_action(uid, BoardAction::Select(self.selected.clone()));
                            self.cut_text(cx, &id);
                            return;
                        }
                        if fe.modifiers.logo || fe.modifiers.control {
                            if let Some(at) = self.selected.iter().position(|s| *s == id) {
                                self.selected.remove(at);
                            } else {
                                self.selected.push(id.clone());
                            }
                            cx.widget_action(uid, BoardAction::Select(self.selected.clone()));
                            self.redraw(cx);
                            return;
                        }
                        if fe.modifiers.shift {
                            if !self.selected.contains(&id) {
                                self.selected.push(id.clone());
                            }
                        } else if !self.selected.contains(&id) {
                            self.selected = vec![id.clone()];
                        }
                        cx.widget_action(uid, BoardAction::Select(self.selected.clone()));
                        let grab = match part {
                            Part::Body => Grab::Body,
                            Part::Start => Grab::Start,
                            Part::End => Grab::End,
                        };
                        let ids = if grab == Grab::Body { self.selected.clone() } else { vec![id.clone()] };
                        let from = self.places_of(&ids);
                        self.drag = Some(Drag { grab, id: Some(id), from, ..empty });
                    }
                    BoardHit::Empty { row, t } => {
                        if fe.tap_count >= 2 {
                            if let Some(row) = row {
                                let t = self.snap_time(t.max(0.0), &[], false);
                                self.snap = None;
                                cx.widget_action(uid, BoardAction::Add { row, t });
                            }
                            return;
                        }
                        let base: Vec<String> = self.selected.clone();
                        let mode = if fe.modifiers.shift { 1 } else if fe.modifiers.logo || fe.modifiers.control { 2 } else { 0 };
                        if mode == 0 && !self.selected.is_empty() {
                            self.selected.clear();
                            cx.widget_action(uid, BoardAction::Select(Vec::new()));
                        }
                        self.drag = Some(Drag { base: (base, mode), ..empty });
                    }
                }
                self.redraw(cx);
            }
            Hit::FingerMove(fe) => {
                let Some(mut drag) = self.drag.clone() else { return };
                let delta = fe.abs - drag.origin;
                if !drag.moved && delta.x.abs() < 3.0 && delta.y.abs() < 3.0 {
                    return;
                }
                drag.moved = true;
                drag.corner = fe.abs;
                self.snap = None;
                match drag.grab {
                    Grab::Pan => {
                        let content = self.layout().content_height;
                        self.view.t0 = drag.view.0;
                        self.view.y0 = drag.view.1;
                        self.view.pan(delta.x, delta.y, self.board.duration, content);
                    }
                    Grab::Ruler => {
                        let t = self.view.t_of(fe.abs.x - self.rect.pos.x).clamp(0.0, self.board.duration);
                        cx.widget_action(uid, BoardAction::Scrub(t));
                    }
                    Grab::Marquee => {
                        let layout = self.layout();
                        let (a, b) = (self.local(drag.origin), self.local(fe.abs));
                        let hits = layout::marquee(&layout, &layout::Rect::from_corners(a, b));
                        let (base, mode) = &drag.base;
                        self.selected = match mode {
                            1 => base.iter().cloned().chain(hits.into_iter().filter(|id| !base.contains(id))).collect(),
                            2 => base.iter().filter(|id| !hits.contains(id)).cloned().chain(hits.iter().filter(|id| !base.contains(id)).cloned()).collect(),
                            _ => hits,
                        };
                    }
                    Grab::Body | Grab::Start | Grab::End => {
                        let invert = fe.modifiers.logo || fe.modifiers.control;
                        let dt = delta.x / self.view.px_per_sec;
                        let except: Vec<String> = drag.from.iter().map(|(id, ..)| id.clone()).collect();
                        let lead = drag.id.clone().unwrap_or_default();
                        let Some((_, start, length, row)) = drag.from.iter().find(|(id, ..)| *id == lead).cloned() else { return };
                        match drag.grab {
                            Grab::Body => {
                                // The lead item snaps (by its start, else its end);
                                // the rest keep their offsets.
                                let head = self.snap_time((start + dt).max(0.0), &except, invert);
                                let head_snap = self.snap.clone();
                                let tail = self.snap_time(start + length + dt, &except, invert) - length;
                                let new_start = if head_snap.is_some() || self.snap.is_none() {
                                    self.snap = head_snap;
                                    head
                                } else {
                                    tail
                                }.max(0.0);
                                let shift = new_start - start;
                                // Rows: the lead goes to the row under the pointer;
                                // the others move by as many rows.
                                let layout = self.layout();
                                let rows: Vec<String> = self.board.rows.iter().map(|row| row.id.clone()).collect();
                                let target = layout::row_at(&layout, fe.abs.y - self.rect.pos.y).map(|row| row.id.clone());
                                let from_index = rows.iter().position(|id| *id == row).unwrap_or(0) as i64;
                                let to_index = target.as_ref().and_then(|target| rows.iter().position(|id| id == target)).map_or(from_index, |i| i as i64);
                                let row_shift = to_index - from_index;
                                drag.preview = drag.from.iter().map(|(id, s, l, r)| {
                                    let index = rows.iter().position(|id| id == r).unwrap_or(0) as i64 + row_shift;
                                    let new_row = rows.get(index.clamp(0, rows.len() as i64 - 1) as usize).cloned().unwrap_or_else(|| r.clone());
                                    (id.clone(), (s + shift).max(0.0), *l, new_row)
                                }).collect();
                            }
                            Grab::Start => {
                                let end = start + length;
                                let new_start = self.snap_time(start + dt, &except, invert).clamp(0.0, end - 0.1);
                                drag.preview = vec![(lead.clone(), new_start, end - new_start, row.clone())];
                            }
                            _ => {
                                let end = self.snap_time(start + length + dt, &except, invert).max(start + 0.1);
                                drag.preview = vec![(lead.clone(), start, end - start, row.clone())];
                            }
                        }
                    }
                }
                self.drag = Some(drag);
                self.redraw(cx);
            }
            Hit::FingerUp(fe) => {
                let Some(drag) = self.drag.take() else { return };
                if let Some((_, what)) = self.snap.take() {
                    if drag.moved {
                        cx.widget_action(uid, BoardAction::Snapped(what));
                    }
                }
                match drag.grab {
                    Grab::Marquee if drag.moved => cx.widget_action(uid, BoardAction::Select(self.selected.clone())),
                    Grab::Marquee => {}
                    Grab::Body | Grab::Start | Grab::End if drag.moved && !fe.cancelled => {
                        let changed: Vec<(String, f64, f64, Option<String>)> = drag.preview.iter().filter(|(id, s, l, r)| {
                            drag.from.iter().find(|(from, ..)| from == id).is_some_and(|(_, s0, l0, r0)| (s - s0).abs() > 1e-9 || (l - l0).abs() > 1e-9 || r != r0)
                        }).map(|(id, s, l, r)| (id.clone(), *s, *l, Some(r.clone()))).collect();
                        if !changed.is_empty() {
                            // Shown at its landing place until the board answers.
                            for (id, s, l, r) in &drag.preview {
                                if let Some(item) = self.board.item_mut(id) {
                                    item.start = *s;
                                    item.length = *l;
                                    item.row = r.clone();
                                }
                            }
                            cx.widget_action(uid, BoardAction::Place(changed));
                        }
                    }
                    _ => {}
                }
                self.redraw(cx);
            }
            Hit::FingerScroll(fe) => {
                let (x, _) = self.local(fe.abs);
                if fe.modifiers.control || fe.modifiers.logo {
                    let step = if fe.scroll.y.abs() >= fe.scroll.x.abs() { fe.scroll.y } else { fe.scroll.x };
                    self.view.zoom_at((-step * 0.01).exp(), x.max(HEADER));
                } else {
                    let (dx, dy) = if fe.modifiers.shift { (fe.scroll.y, 0.0) } else { (fe.scroll.x, fe.scroll.y) };
                    let content = self.layout().content_height;
                    self.view.pan(-dx, -dy, self.board.duration, content);
                }
                self.redraw(cx);
            }
            Hit::KeyDown(ke) => {
                let command = ke.modifiers.logo || ke.modifiers.control;
                match ke.key_code {
                    KeyCode::Equals | KeyCode::NumpadAdd if !command => {
                        self.zoom(cx, 1.25);
                    }
                    KeyCode::Minus | KeyCode::NumpadSubtract if !command => {
                        self.zoom(cx, 0.8);
                    }
                    KeyCode::KeyZ if ke.modifiers.shift && !command => {
                        self.fit(cx);
                    }
                    KeyCode::KeyA if command => {
                        self.selected = self.board.items.iter().map(|item| item.id.clone()).collect();
                        cx.widget_action(uid, BoardAction::Select(self.selected.clone()));
                        self.redraw(cx);
                    }
                    KeyCode::ReturnKey if self.selected.len() == 1 => {
                        let id = self.selected[0].clone();
                        self.cut_text(cx, &id);
                    }
                    KeyCode::ArrowLeft | KeyCode::ArrowRight if !self.selected.is_empty() => {
                        // Nudge: a beat (or a tenth of a second), ten with shift.
                        let step = self.board.bpm.map_or(0.1, |bpm| 60.0 / bpm) * if ke.modifiers.shift { 10.0 } else { 1.0 };
                        let sign = if ke.key_code == KeyCode::ArrowLeft { -1.0 } else { 1.0 };
                        let places: Vec<(String, f64, f64, Option<String>)> = self.places_of(&self.selected.clone()).into_iter().map(|(id, s, l, _)| (id, (s + sign * step).max(0.0), l, None)).collect();
                        cx.widget_action(uid, BoardAction::Place(places));
                    }
                    KeyCode::Escape if !self.selected.is_empty() => {
                        self.selected.clear();
                        cx.widget_action(uid, BoardAction::Select(Vec::new()));
                        self.redraw(cx);
                    }
                    _ => cx.widget_action(uid, BoardAction::Key(ke)),
                }
            }
            Hit::TextInput(te) => {
                if !te.input.is_empty() && self.editing.is_none() {
                    if te.was_paste && cfg!(any(target_os = "macos", target_os = "windows")) {
                        cx.widget_action(uid, BoardAction::PasteClipboard);
                    } else {
                        cx.widget_action(uid, BoardAction::Paste(te.input.clone()));
                    }
                }
            }
            _ => {}
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, _scope: &mut Scope, walk: Walk) -> DrawStep {
        let rect = cx.walk_turtle(walk);
        self.rect = rect;
        self.view.width = rect.size.x;
        self.view.height = rect.size.y;
        if !self.initialized {
            self.initialized = true;
            self.fit_pending = true;
            if self.snap_mode.is_none() {
                self.snap_mode = Some(SnapTo::Seconds);
            }
        }
        if self.fit_pending && rect.size.x > HEADER + 40.0 {
            self.fit_pending = false;
            self.view.fit(self.board.duration.max(1.0));
            self.view.y0 = 0.0;
        }
        let p = makepad_look::palette::get(cx);
        self.draw_bg.color = color(p.wave_bg, 1.0);
        self.draw_bg.draw_abs(cx, rect);
        // Everything the board draws stays inside it (cards run past the
        // right edge when zoomed in).
        cx.begin_turtle(Walk { abs_pos: Some(rect.pos), width: Size::Fixed(rect.size.x), height: Size::Fixed(rect.size.y), ..Default::default() }, makepad_widgets::Layout { clip_x: true, clip_y: true, ..makepad_widgets::Layout::default() });
        self.draw_board(cx);
        cx.end_turtle();
        DrawStep::done()
    }
}


/// The board's frame layout drawn as the frame (the Shorts framing): the
/// platform's covered margins shaded, each band outlined and named, and
/// what the board puts in each band at the playhead written into it, so the
/// person and the AI plan a vertical video against the platform's UI.
#[derive(Script, ScriptHook, Widget)]
pub struct StoryboardFrame {
    #[uid]
    uid: WidgetUid,
    #[walk]
    walk: Walk,
    #[redraw]
    #[live]
    draw_bg: DrawColor,
    #[live]
    draw_shape: DrawBoardShape,
    #[live]
    draw_label: DrawText,
    #[live]
    draw_text: DrawText,
    #[rust]
    board: Storyboard,
    #[rust]
    t: f64,
}

impl StoryboardFrame {
    pub fn set(&mut self, cx: &mut Cx, board: &Storyboard, t: f64) {
        if self.board != *board || (self.t - t).abs() > 1e-6 {
            self.board = board.clone();
            self.t = t;
            self.redraw(cx);
        }
    }

    fn shape(&mut self, cx: &mut Cx2d, rect: Rect, shape: f32, fill: Vec4f, edge: Vec4f, stroke: f32, radius: f32) {
        if rect.size.x <= 0.0 || rect.size.y <= 0.0 {
            return;
        }
        self.draw_shape.fill = fill;
        self.draw_shape.edge = edge;
        self.draw_shape.stroke = stroke;
        self.draw_shape.radius = radius;
        self.draw_shape.shape = shape;
        self.draw_shape.draw_abs(cx, rect);
    }
}

impl Widget for StoryboardFrame {
    fn handle_event(&mut self, _cx: &mut Cx, _event: &Event, _scope: &mut Scope) {}

    fn draw_walk(&mut self, cx: &mut Cx2d, _scope: &mut Scope, walk: Walk) -> DrawStep {
        let rect = cx.walk_turtle(walk);
        let p = makepad_look::palette::get(cx);
        self.draw_bg.color = color(p.panel, 1.0);
        self.draw_bg.draw_abs(cx, rect);
        let Some(frame) = self.board.frame() else { return DrawStep::done() };
        // The frame at its aspect, centred.
        let aspect = frame.width as f64 / frame.height as f64;
        let h = (rect.size.y - 24.0).max(40.0);
        let w = (h * aspect).min(rect.size.x - 24.0);
        let h = w / aspect;
        let f = r(rect.pos.x + (rect.size.x - w) * 0.5, rect.pos.y + 4.0, w, h);
        let at = |x: f64, y: f64| dvec2(f.pos.x + x * w, f.pos.y + y * h);
        self.shape(cx, f, 0.0, color(0x0b0d12ff, 1.0), color(p.wave_rim, 1.0), 1.0, 3.0);
        // The platform's covered margins.
        let s = frame.safe;
        let covered = color(Hue::STALE, 0.10);
        for band in [r(f.pos.x, f.pos.y, w, s.top * h), r(f.pos.x, f.pos.y + (1.0 - s.bottom) * h, w, s.bottom * h), r(f.pos.x, f.pos.y + s.top * h, s.left * w, (1.0 - s.top - s.bottom) * h), r(f.pos.x + (1.0 - s.right) * w, f.pos.y + s.top * h, s.right * w, (1.0 - s.top - s.bottom) * h)] {
            self.shape(cx, band, 0.0, covered, color(0, 0.0), 0.0, 0.0);
        }
        let safe = r(f.pos.x + s.left * w, f.pos.y + s.top * h, (1.0 - s.left - s.right) * w, (1.0 - s.top - s.bottom) * h);
        self.shape(cx, safe, 3.0, color(0, 0.0), color(Hue::STALE, 0.8), 1.0, 1.0);
        // The bands, and what the board puts in each at the playhead.
        let t = self.t;
        let board = self.board.clone();
        for band in frame.bands {
            let b = r(at(band.x0, band.y0).x, at(band.x0, band.y0).y, (band.x1 - band.x0) * w, (band.y1 - band.y0) * h);
            let hue = if band.text { p.accent } else { p.wave_label };
            self.shape(cx, b, 0.0, color(hue, 0.08), color(hue, 0.55), 1.0, 2.0);
            self.draw_label.color = color(hue, 0.9);
            self.draw_label.draw_abs(cx, dvec2(b.pos.x + 3.0, b.pos.y + 2.0), &band.label.to_uppercase());
            let here: Vec<String> = board.items.iter().filter(|item| item.band.as_deref() == Some(band.id) && item.start <= t && t < item.end()).map(|item| item.headline()).collect();
            let mut y = b.pos.y + 12.0;
            for text in here {
                if y + 9.0 > b.pos.y + b.size.y.max(20.0) {
                    break;
                }
                if let Some(text) = StoryboardCanvas::fit_text(&self.draw_text, cx, &text, b.size.x - 6.0) {
                    self.draw_text.color = color(p.glass_ink, 1.0);
                    self.draw_text.draw_abs(cx, dvec2(b.pos.x + 3.0, y), &text);
                }
                y += 10.0;
            }
        }
        // The caption: the safe zones' owner and the rule.
        self.draw_text.color = color(p.muted, 1.0);
        let caption = format!("{} · safe area dashed", frame.label);
        self.draw_text.draw_abs(cx, dvec2(rect.pos.x + 12.0, f.pos.y + h + 5.0), &caption);
        DrawStep::done()
    }
}


/// The type-voice legend: each voice's colour, name, face and what it is
/// for, the way the lyric cards name them.
#[derive(Script, ScriptHook, Widget)]
pub struct StoryboardLegend {
    #[uid]
    uid: WidgetUid,
    #[walk]
    walk: Walk,
    #[redraw]
    #[live]
    draw_bg: DrawColor,
    #[live]
    draw_shape: DrawBoardShape,
    #[live]
    draw_label: DrawText,
    #[live]
    draw_text: DrawText,
    #[rust]
    voices: Vec<makepad_story_storyboard::TypeVoice>,
    /// How many lines use each voice.
    #[rust]
    counts: Vec<usize>,
}

impl StoryboardLegend {
    pub fn set(&mut self, cx: &mut Cx, board: &Storyboard) {
        let counts: Vec<usize> = board.voices.iter().map(|voice| board.items.iter().filter(|item| matches!(&item.body, Body::Lyric(line) if line.voice.as_deref() == Some(voice.id.as_str()))).count()).collect();
        if self.voices != board.voices || self.counts != counts {
            self.voices = board.voices.clone();
            self.counts = counts;
            self.redraw(cx);
        }
    }
}

impl Widget for StoryboardLegend {
    fn handle_event(&mut self, _cx: &mut Cx, _event: &Event, _scope: &mut Scope) {}

    fn draw_walk(&mut self, cx: &mut Cx2d, _scope: &mut Scope, walk: Walk) -> DrawStep {
        let rect = cx.walk_turtle(walk);
        let p = makepad_look::palette::get(cx);
        self.draw_bg.color = color(p.panel, 1.0);
        self.draw_bg.draw_abs(cx, rect);
        self.draw_text.color = color(p.secondary, 1.0);
        self.draw_label.color = color(p.secondary, 1.0);
        self.draw_label.draw_abs(cx, dvec2(rect.pos.x + 12.0, rect.pos.y + 4.0), "TYPE VOICES");
        let mut y = rect.pos.y + 22.0;
        for (index, voice) in self.voices.clone().iter().enumerate() {
            let c = voice.color.unwrap_or(p.text_bright);
            self.draw_shape.fill = color(c, 1.0);
            self.draw_shape.edge = color(c, 1.0);
            self.draw_shape.stroke = 0.0;
            self.draw_shape.radius = 2.0;
            self.draw_shape.shape = 0.0;
            self.draw_shape.draw_abs(cx, r(rect.pos.x + 12.0, y + 2.0, 10.0, 10.0));
            let count = self.counts.get(index).copied().unwrap_or(0);
            self.draw_label.color = color(c, 1.0);
            let head = format!("{}  ·  {count} {}", voice.label, if count == 1 { "line" } else { "lines" });
            if let Some(head) = StoryboardCanvas::fit_text(&self.draw_label, cx, &head, rect.size.x - 44.0) {
                self.draw_label.draw_abs(cx, dvec2(rect.pos.x + 28.0, y), &head);
            }
            let detail = [voice.face.as_str(), voice.role.as_str()].iter().filter(|s| !s.is_empty()).copied().collect::<Vec<_>>().join(" — ");
            self.draw_text.color = color(p.muted, 1.0);
            if let Some(detail) = StoryboardCanvas::fit_text(&self.draw_text, cx, &detail, rect.size.x - 44.0) {
                self.draw_text.draw_abs(cx, dvec2(rect.pos.x + 28.0, y + 13.0), &detail);
            }
            y += 34.0;
        }
        DrawStep::done()
    }
}
