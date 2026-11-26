//! The library browser's card list (design §4.2; Story's CUT and DECK, Stage's SCORE): one lane's items (media,
//! edits, app runs) as a grid of cards or a list. A card is the picture at
//! 16:9 with its caption under it (name, meta), a duration badge, the
//! accent "used" marker over the ranges the open cut uses, and a render
//! state badge on edits. Hovering a video card skims its filmstrip. A click
//! picks a row; a drag carries it: the list reports where the pointer is
//! while it moves and where it was let go, and the page decides what that
//! means on the timeline. The pointer stays captured by the list for the
//! whole drag, so nothing depends on the platform's drag-and-drop.

use crate::draw::{wave_height, CutRole, DrawEditBlock, DrawEditPicture, Filmstrip, Peaks};
use std::sync::Arc;
use makepad_widgets::badge::advance;
use makepad_widgets::*;

const CARD_MIN: f64 = 132.0;
const GAP: f64 = 8.0;
const CAPTION: f64 = 34.0;
const LIST_ROW: f64 = 44.0;
const DRAG_START: f64 = 4.0;

script_mod! {
    use mod.prelude.widgets_internal.*
    use mod.widgets.*
    let stage = mod.stage

    mod.widgets.CutAssetPreviewBase = #(CutAssetPreview::register_widget(vm))
    mod.widgets.CutAssetPreview = set_type_default() do mod.widgets.CutAssetPreviewBase{
        width: Fill height: 176
        draw_bg +: {color: stage.video}
        draw_bar +: {color: stage.glass_white}
        draw_time +: {color: stage.glass_ink text_style: theme.font_regular{font_size: 7.5}}
    }

    mod.widgets.CutMediaListBase = #(CutMediaList::register_widget(vm))
    mod.widgets.CutMediaList = set_type_default() do mod.widgets.CutMediaListBase{
        width: Fill height: Fill
        draw_bg +: {color: stage.panel}
        draw_mark +: {color: stage.accent}
        draw_title +: {color: stage.text text_style: theme.font_regular{font_size: 9}}
        draw_detail +: {color: stage.muted text_style: theme.font_regular{font_size: 8}}
        draw_badge +: {color: stage.glass_ink text_style: theme.font_regular{font_size: 7.5}}
        draw_empty +: {color: stage.secondary text_style: theme.font_regular{font_size: 10}}
        icon_video +: {svg: crate_resource("self:resources/icons/video.svg")}
        icon_image +: {svg: crate_resource("self:resources/icons/image.svg")}
        icon_audio +: {svg: crate_resource("self:resources/icons/audio.svg")}
        icon_app +: {svg: crate_resource("self:resources/icons/edit/app_run.svg")}
        icon_edit +: {svg: crate_resource("self:resources/icons/edit/timeline.svg")}
    }
}

/// What a card's picture stands for.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CardKind {
    #[default]
    Video,
    Image,
    Audio,
    AppRun,
    Cut,
    Deck,
}

#[derive(Clone, Debug, Default)]
pub struct ListRow {
    pub title: String,
    pub detail: String,
    pub kind: CardKind,
    pub texture: Option<Texture>,
    /// Document aspect, known before its still has loaded.
    pub aspect: Option<f64>,
    /// Pictures across the item, for skimming.
    pub film: Option<Filmstrip>,
    /// The length the duration badge shows.
    pub seconds: Option<f64>,
    /// Source ranges the open cut uses, as fractions of the length.
    pub used: Vec<(f64, f64)>,
    /// A state badge at the top right: text, fill, ink.
    pub badge: Option<(String, u32, u32)>,
    /// A sound's loudness, drawn as its card, and whether it is a voice take.
    pub peaks: Option<Arc<Peaks>>,
    pub voice: bool,
}

#[derive(Clone, Debug, Default)]
pub enum MediaListAction {
    #[default]
    None,
    Click(usize),
    /// A row is being dragged; the pointer is at `abs`.
    Drag(usize, Vec2d),
    /// A dragged row was let go at `abs`.
    Drop(usize, Vec2d),
    /// The drag ended without a drop (cancelled by the platform).
    Cancel,
    /// A double click: open it (a cut).
    Open(usize),
    /// A click on empty space: nothing selected.
    Deselect,
    /// A key while the list has the focus.
    Key(KeyEvent),
}

/// `m:ss` for badges.
pub fn short_time(seconds: f64) -> String {
    let s = seconds.max(0.0).floor() as u64;
    if s >= 3600 { format!("{}:{:02}:{:02}", s / 3600, (s / 60) % 60, s % 60) } else { format!("{}:{:02}", s / 60, s % 60) }
}

/// A card's duration badge (§4.8): `1.6 s` under ten seconds, `M:SS`
/// above; never rounded up.
pub fn badge_time(seconds: f64) -> String {
    if seconds < 10.0 { format!("{:.1} s", (seconds.max(0.0) * 10.0).floor() / 10.0) } else { short_time(seconds) }
}

#[derive(Script, ScriptHook, Widget)]
pub struct CutMediaList {
    #[uid]
    uid: WidgetUid,
    #[walk]
    walk: Walk,
    #[redraw]
    #[live]
    draw_bg: DrawColor,
    #[live]
    draw_mark: DrawColor,
    #[live]
    draw_thumb: DrawEditPicture,
    #[live]
    draw_frame: DrawEditBlock,
    #[live]
    draw_title: DrawText,
    #[live]
    draw_detail: DrawText,
    #[live]
    draw_badge: DrawText,
    #[live]
    draw_empty: DrawText,
    #[live]
    icon_video: DrawSvg,
    #[live]
    icon_image: DrawSvg,
    #[live]
    icon_audio: DrawSvg,
    #[live]
    icon_app: DrawSvg,
    #[live]
    icon_edit: DrawSvg,
    #[rust]
    rows: Vec<ListRow>,
    #[rust]
    aspects: Vec<f64>,
    #[rust]
    band_prefix: Vec<f64>,
    #[rust]
    band_columns: usize,
    /// The rows the last paint drew (those in view).
    #[rust]
    drawn_from: usize,
    #[rust]
    drawn_to: usize,
    #[rust]
    selected: Option<usize>,
    #[rust]
    hover: Option<usize>,
    /// The pointer's x over the hovered card's picture, 0..1.
    #[rust]
    skim: Option<f64>,
    #[rust]
    scroll: f64,
    /// The row pressed, where, and whether it has become a drag.
    #[rust]
    press: Option<(usize, Vec2d, bool)>,
    #[rust]
    rect: Rect,
    #[rust]
    empty: String,
    #[rust]
    empty_kind: CardKind,
    #[rust]
    list_mode: bool,
    #[rust]
    focused: bool,
    /// Focus shown only after keyboard use, until the next click (C5).
    #[rust]
    focus_visible: bool,
}

impl CutMediaList {
    pub fn set_rows(&mut self, cx: &mut Cx, rows: Vec<ListRow>, empty: String, empty_kind: CardKind) {
        self.aspects = rows.iter().map(|row| row.aspect.or_else(|| row.texture.as_ref().and_then(|texture| texture.get_format(cx).vec_width_height()).map(|(w,h)| w as f64 / h.max(1) as f64)).filter(|a| a.is_finite() && *a > 0.0).unwrap_or(16.0/9.0)).collect();
        self.rows = rows;
        self.band_columns = 0;
        self.layout_bands();
        self.empty = empty;
        self.empty_kind = empty_kind;
        if self.selected.is_some_and(|index| index >= self.rows.len()) {
            self.selected = None;
        }
        self.redraw(cx);
    }

    /// The row under the pointer, and the rows in view.
    pub fn wanted(&self) -> (Option<usize>, std::ops::Range<usize>) {
        (self.hover.filter(|index| *index < self.rows.len()), self.drawn_from.min(self.rows.len())..self.drawn_to.min(self.rows.len()))
    }

    pub fn set_selected(&mut self, cx: &mut Cx, selected: Option<usize>) {
        if self.selected != selected {
            self.selected = selected;
            self.redraw(cx);
        }
    }

    pub fn set_list_mode(&mut self, cx: &mut Cx, list: bool) {
        if self.list_mode != list {
            self.list_mode = list;
            self.band_columns = 0;
            self.layout_bands();
            self.scroll = 0.0;
            self.redraw(cx);
        }
    }

    /// Scroll so card `index` is whole in view (a moved selection).
    pub fn ensure_visible(&mut self, cx: &mut Cx, index: usize) {
        if self.rect.size.y <= 0.0 || index >= self.rows.len() {
            return;
        }
        let tile = self.tile_rect(index);
        let (top, bottom) = (tile.pos.y - self.rect.pos.y, tile.pos.y + tile.size.y - self.rect.pos.y);
        if top < 0.0 {
            self.scroll += top - GAP;
        } else if bottom > self.rect.size.y {
            self.scroll += bottom - self.rect.size.y + GAP;
        } else {
            return;
        }
        self.clamp_scroll();
        self.redraw(cx);
    }

    pub fn reset_scroll(&mut self, cx: &mut Cx) {
        self.scroll = 0.0;
        self.press = None;
        self.redraw(cx);
    }

    /// Columns, card width and card height for the current width.
    fn grid(&self) -> (usize, f64) {
        if self.list_mode { return (1, (self.rect.size.x - 2.0 * GAP).max(1.0)); }
        let inner = (self.rect.size.x - GAP).max(1.0);
        let columns = ((inner / (CARD_MIN + GAP)).floor() as usize).max(1);
        (columns, (inner / columns as f64 - GAP).max(1.0))
    }

    // Group heights depend on aspects and column count, not the continuously
    // changing width. Splitter motion only scales these cached prefix sums.
    fn layout_bands(&mut self) {
        let (columns, _) = self.grid();
        if columns == self.band_columns { return; }
        self.band_columns = columns;
        self.band_prefix.clear();
        self.band_prefix.push(0.0);
        for band in self.aspects.chunks(columns) {
            let ratio = band.iter().map(|aspect| 1.0 / aspect).fold(0.0, f64::max);
            self.band_prefix.push(self.band_prefix.last().copied().unwrap() + ratio);
        }
    }

    fn band_top(&self, band: usize) -> f64 {
        if self.list_mode { GAP + band as f64 * (LIST_ROW + 4.0) }
        else { GAP + self.grid().1 * self.band_prefix[band] + band as f64 * (CAPTION + GAP) }
    }

    fn band_at(&self, y: f64) -> usize {
        if self.list_mode { return ((y - GAP).max(0.0) / (LIST_ROW + 4.0)) as usize; }
        let (mut low, mut high) = (0, self.band_prefix.len().saturating_sub(1));
        while low < high {
            let mid = (low + high).div_ceil(2);
            if self.band_top(mid) <= y { low = mid; } else { high = mid - 1; }
        }
        low
    }

    /// The distance from one row's top to the next.
    

    fn tile_rect(&self, index: usize) -> Rect {
        let (columns, w) = self.grid();
        let (col, band) = (index % columns, index / columns);
        let h = if self.list_mode { LIST_ROW } else { w / self.aspects[index] + CAPTION };
        Rect { pos: dvec2(self.rect.pos.x + GAP + col as f64 * (w + GAP), self.rect.pos.y + self.band_top(band) - self.scroll), size: dvec2(w, h) }
    }

    pub fn card_width(&self, index: usize) -> f64 { self.picture_rect(self.tile_rect(index)).size.x }

    fn picture_rect(&self, tile: Rect) -> Rect {
        if self.list_mode {
            Rect { pos: dvec2(tile.pos.x, tile.pos.y + 4.0), size: dvec2(64.0, 36.0) }
        } else {
            Rect { pos: tile.pos, size: dvec2(tile.size.x, tile.size.y - CAPTION) }
        }
    }

    fn row_at(&self, abs: Vec2d) -> Option<usize> {
        if !self.rect.contains(abs) || self.rows.is_empty() { return None; }
        let (columns, w) = self.grid();
        let band = self.band_at(abs.y - self.rect.pos.y + self.scroll);
        let col = if self.list_mode { 0 } else { ((abs.x - self.rect.pos.x - GAP).max(0.0) / (w + GAP)) as usize };
        let index = band * columns + col;
        (col < columns && index < self.rows.len() && self.tile_rect(index).contains(abs)).then_some(index)
    }

    fn clamp_scroll(&mut self) {
        self.layout_bands();
        let bands = self.rows.len().div_ceil(self.grid().0);
        self.scroll = self.scroll.clamp(0.0, (self.band_top(bands) - self.rect.size.y).max(0.0));
    }

    fn glyph(&mut self, kind: CardKind) -> &mut DrawSvg {
        match kind {
            CardKind::Video => &mut self.icon_video,
            CardKind::Image => &mut self.icon_image,
            CardKind::Audio => &mut self.icon_audio,
            CardKind::AppRun => &mut self.icon_app,
            CardKind::Cut | CardKind::Deck => &mut self.icon_edit,
        }
    }

    fn fit_text(draw: &DrawText, cx: &mut Cx2d, text: &str, width: f64) -> String {
        if advance(draw, cx, text) <= width {
            return text.to_string();
        }
        let chars: Vec<char> = text.chars().collect();
        let mut n = chars.len();
        while n > 0 && advance(draw, cx, &format!("{}…", chars[..n].iter().collect::<String>())) > width {
            n -= 1;
        }
        format!("{}…", chars[..n].iter().collect::<String>())
    }

    fn draw_card(&mut self, cx: &mut Cx2d, index: usize, tile: Rect, p: &makepad_look::palette::Palette) {
        let row = self.rows[index].clone();
        let selected = self.selected == Some(index);
        let hover = self.hover == Some(index);
        let dragging = self.press.is_some_and(|(pressed, _, dragging)| pressed == index && dragging);
        let pic = self.picture_rect(tile);
        let alpha = if dragging { 0.45 } else { 1.0 };
        if self.list_mode && (selected || hover) {
            self.draw_frame.fill = Vec4f::from_u32(if selected { p.accent_wash } else { p.control_hover });
            self.draw_frame.edge = self.draw_frame.fill;
            self.draw_frame.stroke = 0.0;
            self.draw_frame.radius = 3.0;
            self.draw_frame.shape = 0.0;
            self.draw_frame.draw_abs(cx, Rect { pos: tile.pos - dvec2(4.0, 2.0), size: tile.size + dvec2(8.0, 4.0) });
            if selected {
                self.draw_mark.color = Vec4f::from_u32(p.accent);
                self.draw_mark.draw_abs(cx, Rect { pos: dvec2(tile.pos.x - 4.0, tile.pos.y + (tile.size.y - 20.0) * 0.5), size: dvec2(2.0, 20.0) });
            }
        }
        // The picture's ground, then the picture at its aspect (a skimmed
        // frame while hovered).
        self.draw_frame.fill = Vec4f::from_u32(if row.kind == CardKind::Audio { p.wave_bg } else { p.cell });
        self.draw_frame.edge = self.draw_frame.fill;
        self.draw_frame.stroke = 0.0;
        self.draw_frame.radius = 4.0;
        self.draw_frame.shape = 0.0;
        self.draw_frame.draw_abs(cx, pic);
        let skim_t = self.skim.filter(|_| hover && !self.list_mode && !matches!(row.kind, CardKind::Cut | CardKind::Deck)).zip(row.seconds).map(|(x, seconds)| x * seconds);
        let texture = match (&row.film, skim_t) {
            (Some(film), Some(t)) if film.len() > 1 => film.iter().min_by(|a, b| (a.0 - t).abs().total_cmp(&(b.0 - t).abs())).map(|(_, texture)| texture.clone()),
            // Without a picture of its own, the film's frame at 10 % (a
            // render's first frame is often black).
            _ => row.texture.clone().or_else(|| (!matches!(row.kind, CardKind::Cut | CardKind::Deck)).then_some(()).and_then(|_| row.film.as_ref()).and_then(|film| film.get(film.len() / 10).or(film.first()).map(|(_, texture)| texture.clone()))),
        };
        // A sound's card is its dB waveform in its role's hue (review 3,
        // C6); the store's picture shows only under the pointer.
        let waveform = row.peaks.clone().filter(|_| row.kind == CardKind::Audio && !hover);
        if let Some(peaks) = waveform {
            let seconds = row.seconds.unwrap_or(peaks.values.len() as f64 / peaks.per_second.max(1.0)).max(0.1);
            let hue = makepad_look::palette::on_wave(cx, if row.voice { CutRole::VOICE.hue } else { CutRole::AUDIO.hue });
            let inner = Rect { pos: pic.pos + dvec2(4.0, 4.0), size: pic.size - dvec2(8.0, 8.0) };
            let mid = inner.pos.y + inner.size.y * 0.5;
            let per_px = seconds / inner.size.x.max(1.0);
            let half = inner.size.y * 0.46;
            let mut x = 0.0;
            while x < inner.size.x {
                let (t0, t1) = (x * per_px, (x + 1.0) * per_px);
                let peak = wave_height(peaks.over(t0, t1)) * half;
                let rms = wave_height(peaks.rms_over(t0, t1)) * half;
                let mut c = Vec4f::from_u32(hue);
                c.w = 0.45 * alpha;
                self.draw_mark.color = c;
                self.draw_mark.draw_abs(cx, Rect { pos: dvec2(inner.pos.x + x, mid - peak), size: dvec2(1.0, (peak * 2.0).max(1.0)) });
                c.w = alpha;
                self.draw_mark.color = c;
                self.draw_mark.draw_abs(cx, Rect { pos: dvec2(inner.pos.x + x, mid - rms), size: dvec2(1.0, (rms * 2.0).max(1.0)) });
                x += 1.0;
            }
        }
        match texture {
            _ if row.kind == CardKind::Audio && row.peaks.is_some() && !hover => {}
            Some(texture) => {
                let (tw, th) = texture.get_format(cx).vec_width_height().unwrap_or((16, 9));
                let aspect = tw.max(1) as f64 / th.max(1) as f64;
                let (w, h) = if aspect > pic.size.x / pic.size.y { (pic.size.x, pic.size.x / aspect) } else { (pic.size.y * aspect, pic.size.y) };
                self.draw_thumb.draw_vars.set_texture(0, &texture);
                self.draw_thumb.opacity = alpha;
                self.draw_thumb.uv = vec4(0.0, 0.0, 1.0, 1.0);
                self.draw_thumb.draw_abs(cx, Rect { pos: dvec2(pic.pos.x + (pic.size.x - w) * 0.5, pic.pos.y + (pic.size.y - h) * 0.5), size: dvec2(w, h) });
            }
            None => {
                let size = if self.list_mode { 16.0 } else { 24.0 };
                let color = if row.kind == CardKind::Audio { p.glass_wave } else { p.faint };
                let glyph = self.glyph(if row.detail.starts_with("App · ") { CardKind::AppRun } else { row.kind });
                glyph.color = Vec4f::from_u32(color);
                glyph.draw_abs(cx, Rect { pos: dvec2(pic.pos.x + (pic.size.x - size) * 0.5, pic.pos.y + (pic.size.y - size) * 0.5), size: dvec2(size, size) });
            }
        }
        // The skim line.
        if let (Some(x), true) = (self.skim, hover && !self.list_mode && row.film.is_some() && !matches!(row.kind, CardKind::Cut | CardKind::Deck)) {
            self.draw_mark.color = Vec4f::from_u32(p.glass_white);
            self.draw_mark.draw_abs(cx, Rect { pos: dvec2(pic.pos.x + x * pic.size.x, pic.pos.y), size: dvec2(1.0, pic.size.y) });
        }
        // Used in this cut: an accent bar over the ranges.
        for (a, b) in &row.used {
            self.draw_mark.color = Vec4f::from_u32(p.accent);
            let h = if self.list_mode { 2.0 } else { 3.0 };
            self.draw_mark.draw_abs(cx, Rect { pos: dvec2(pic.pos.x + a * pic.size.x, pic.pos.y + pic.size.y - h), size: dvec2(((b - a) * pic.size.x).max(3.0), h) });
        }
        if !self.list_mode {
            // Duration (or skim time) badge, bottom right.
            if let Some(seconds) = row.seconds.filter(|_| row.kind != CardKind::Image) {
                let text = badge_time(skim_t.unwrap_or(seconds));
                let w = advance(&self.draw_badge, cx, &text) + 8.0;
                let r = Rect { pos: dvec2(pic.pos.x + pic.size.x - 6.0 - w, pic.pos.y + pic.size.y - 6.0 - 16.0), size: dvec2(w, 16.0) };
                self.draw_frame.fill = vec4(0.0, 0.0, 0.0, 0.7);
                self.draw_frame.edge = self.draw_frame.fill;
                self.draw_frame.radius = 2.0;
                self.draw_frame.draw_abs(cx, r);
                self.draw_badge.color = Vec4f::from_u32(p.glass_ink);
                self.draw_badge.draw_abs(cx, dvec2(r.pos.x + 4.0, r.pos.y + 2.5), &text);
            }
            if let Some((text, fill, ink)) = &row.badge {
                let w = advance(&self.draw_badge, cx, text) + 10.0;
                let r = Rect { pos: dvec2(pic.pos.x + pic.size.x - 6.0 - w, pic.pos.y + 6.0), size: dvec2(w, 16.0) };
                self.draw_frame.fill = Vec4f::from_u32(*fill);
                self.draw_frame.edge = self.draw_frame.fill;
                self.draw_frame.radius = 2.0;
                self.draw_frame.draw_abs(cx, r);
                self.draw_badge.color = Vec4f::from_u32(*ink);
                self.draw_badge.draw_abs(cx, dvec2(r.pos.x + 5.0, r.pos.y + 2.5), text);
            }
            if row.kind == CardKind::AppRun {
                let r = Rect { pos: dvec2(pic.pos.x + 6.0, pic.pos.y + 6.0), size: dvec2(18.0, 18.0) };
                self.draw_frame.fill = Vec4f::from_u32(CutRole::APP_RUN.strip);
                self.draw_frame.edge = self.draw_frame.fill;
                self.draw_frame.radius = 3.0;
                self.draw_frame.draw_abs(cx, r);
                self.icon_app.color = Vec4f::from_u32(p.glass_ink);
                self.icon_app.draw_abs(cx, Rect { pos: dvec2(r.pos.x + 3.0, r.pos.y + 3.0), size: dvec2(12.0, 12.0) });
            }
        }
        // Rings: selected (accent, outside), hovered (border_hi); the list
        // marks a row instead.
        if (selected || hover) && !self.list_mode {
            self.draw_frame.fill = vec4(0.0, 0.0, 0.0, 0.0);
            self.draw_frame.edge = Vec4f::from_u32(if selected { p.accent } else { p.border_hi });
            self.draw_frame.stroke = if selected { 2.0 } else { 1.0 };
            self.draw_frame.radius = 5.0;
            let grow = if selected { 2.0 } else { 1.0 };
            self.draw_frame.draw_abs(cx, Rect { pos: pic.pos - dvec2(grow, grow), size: pic.size + dvec2(2.0 * grow, 2.0 * grow) });
        }
        // Keyboard focus (K2): the `cursor` ring, 2 px out, round the
        // selected card or row while the list has the keys.
        if selected && self.focused && self.focus_visible {
            let ring = if self.list_mode { Rect { pos: tile.pos - dvec2(6.0, 4.0), size: tile.size + dvec2(12.0, 8.0) } } else { Rect { pos: pic.pos - dvec2(4.0, 4.0), size: pic.size + dvec2(8.0, 8.0) } };
            self.draw_frame.fill = vec4(0.0, 0.0, 0.0, 0.0);
            self.draw_frame.edge = Vec4f::from_u32(p.cursor);
            self.draw_frame.stroke = 1.0;
            self.draw_frame.radius = 6.0;
            self.draw_frame.draw_abs(cx, ring);
        }
        // The caption: under the picture (or beside it in the list).
        let (x, y, width) = if self.list_mode {
            (pic.pos.x + pic.size.x + 10.0, tile.pos.y + 7.0, tile.size.x - pic.size.x - 60.0)
        } else {
            (tile.pos.x + 1.0, pic.pos.y + pic.size.y + 4.0, tile.size.x - 2.0)
        };
        self.draw_title.color = Vec4f::from_u32(if selected || hover { p.text_hi } else { p.text });
        let title = Self::fit_text(&self.draw_title, cx, &row.title, width);
        self.draw_title.draw_abs(cx, dvec2(x, y), &title);
        let detail = Self::fit_text(&self.draw_detail, cx, &row.detail, width);
        self.draw_detail.draw_abs(cx, dvec2(x, y + 15.0), &detail);
        if self.list_mode {
            if let Some((text, _, _)) = &row.badge {
                // A dot and a word: live Rendered, amber Stale, dim No render.
                let word = text.trim_start_matches("● ").to_string();
                let dot = if word.starts_with("Rendered") { p.live } else if word.starts_with("Stale") { p.amber } else { p.dim };
                let w = advance(&self.draw_detail, cx, &word);
                let x = tile.pos.x + tile.size.x - w - 6.0;
                let y = tile.pos.y + (tile.size.y - 11.0) * 0.5;
                self.draw_frame.fill = Vec4f::from_u32(dot);
                self.draw_frame.edge = self.draw_frame.fill;
                self.draw_frame.stroke = 0.0;
                self.draw_frame.radius = 3.5;
                self.draw_frame.draw_abs(cx, Rect { pos: dvec2(x - 12.0, y + 2.0), size: dvec2(7.0, 7.0) });
                self.draw_detail.draw_abs(cx, dvec2(x, y), &word);
            } else if let Some(seconds) = row.seconds {
                let text = badge_time(seconds);
                let w = advance(&self.draw_detail, cx, &text);
                self.draw_detail.draw_abs(cx, dvec2(tile.pos.x + tile.size.x - w - 4.0, tile.pos.y + 11.0), &text);
            }
        }
    }
}

impl Widget for CutMediaList {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, _scope: &mut Scope) {
        let uid = self.widget_uid();
        let focused = cx.has_key_focus(self.draw_bg.area());
        if focused != self.focused {
            self.focused = focused;
            self.redraw(cx);
        }
        // Where it really is: a parent's alignment moves it after it drew.
        let area = self.draw_bg.area().rect(cx);
        if area.size.x > 0.0 {
            self.rect = area;
        }
        match event.hits(cx, self.draw_bg.area()) {
            Hit::FingerHoverIn(fe) | Hit::FingerHoverOver(fe) => {
                let hover = self.row_at(fe.abs);
                let skim = hover.map(|index| {
                    let pic = self.picture_rect(self.tile_rect(index));
                    ((fe.abs.x - pic.pos.x) / pic.size.x.max(1.0)).clamp(0.0, 1.0)
                });
                if hover != self.hover || skim != self.skim {
                    self.hover = hover;
                    self.skim = skim;
                    self.redraw(cx);
                }
                cx.set_cursor(if hover.is_some() { MouseCursor::Hand } else { MouseCursor::Default });
            }
            Hit::FingerHoverOut(_) => {
                if self.hover.take().is_some() {
                    self.skim = None;
                    self.redraw(cx);
                }
            }
            Hit::FingerDown(fe) if fe.is_primary_hit() => {
                cx.set_key_focus(self.draw_bg.area());
                self.focus_visible = false;
                self.press = self.row_at(fe.abs).map(|index| (index, fe.abs, false));
                if self.press.is_none() && self.selected.take().is_some() {
                    cx.widget_action(uid, MediaListAction::Deselect);
                    self.redraw(cx);
                }
            }
            Hit::FingerMove(fe) => {
                if let Some((index, origin, dragging)) = self.press {
                    if !dragging && (fe.abs - origin).length() >= DRAG_START {
                        self.press = Some((index, origin, true));
                        self.selected = Some(index);
                        self.redraw(cx);
                    }
                    if self.press.is_some_and(|(_, _, dragging)| dragging) {
                        cx.set_cursor(MouseCursor::Grabbing);
                        cx.widget_action(uid, MediaListAction::Drag(index, fe.abs));
                    }
                }
            }
            Hit::FingerUp(fe) => {
                if let Some((index, _, dragging)) = self.press.take() {
                    if fe.cancelled {
                        if dragging {
                            cx.widget_action(uid, MediaListAction::Cancel);
                        }
                    } else if dragging {
                        cx.widget_action(uid, MediaListAction::Drop(index, fe.abs));
                    } else if self.row_at(fe.abs) == Some(index) {
                        self.selected = Some(index);
                        cx.widget_action(uid, if fe.tap_count >= 2 { MediaListAction::Open(index) } else { MediaListAction::Click(index) });
                    }
                    self.redraw(cx);
                }
            }
            Hit::KeyDown(ke) => {
                // Arrows walk the cards; the ring shows.
                let (columns, _) = self.grid();
                let step: Option<isize> = match ke.key_code {
                    KeyCode::ArrowLeft => Some(-1),
                    KeyCode::ArrowRight => Some(1),
                    KeyCode::ArrowUp => Some(-(columns as isize)),
                    KeyCode::ArrowDown => Some(columns as isize),
                    KeyCode::Tab => Some(if ke.modifiers.shift { -1 } else { 1 }),
                    _ => None,
                };
                self.focus_visible = true;
                match step {
                    Some(step) if !self.rows.is_empty() => {
                        let at = self.selected.map_or(0, |index| (index as isize + step).clamp(0, self.rows.len() as isize - 1) as usize);
                        self.selected = Some(at);
                        cx.widget_action(uid, MediaListAction::Click(at));
                    }
                    _ => cx.widget_action(uid, MediaListAction::Key(ke)),
                }
                self.redraw(cx);
            }
            Hit::FingerScroll(fe) => {
                self.scroll += fe.scroll.y;
                self.clamp_scroll();
                self.redraw(cx);
            }
            _ => {}
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, _scope: &mut Scope, walk: Walk) -> DrawStep {
        let rect = cx.walk_turtle(walk);
        self.rect = rect;
        self.clamp_scroll();
        self.draw_bg.draw_abs(cx, rect);
        let p = makepad_look::palette::get(cx);
        if self.rows.is_empty() {
            self.drawn_from = 0;
            self.drawn_to = 0;
            // The lane's glyph and a sentence at 40 % height.
            let y = rect.pos.y + rect.size.y * 0.4;
            let kind = self.empty_kind;
            let glyph = self.glyph(kind);
            glyph.color = Vec4f::from_u32(p.faint);
            glyph.draw_abs(cx, Rect { pos: dvec2(rect.pos.x + (rect.size.x - 32.0) * 0.5, y - 40.0), size: dvec2(32.0, 32.0) });
            let empty = Self::fit_text(&self.draw_empty, cx, &self.empty.clone(), rect.size.x - 24.0);
            let w = advance(&self.draw_empty, cx, &empty);
            self.draw_empty.draw_abs(cx, dvec2(rect.pos.x + (rect.size.x - w) * 0.5, y), &empty);
            return DrawStep::done();
        }
        // Every row the view touches, partly scrolled ones included; the
        // turtle's clip cuts them at the view's edges.
        let (columns, _) = self.grid();
        let bands = self.rows.len().div_ceil(columns);
        let rows = if self.list_mode {
            visible_rows(bands, GAP, LIST_ROW, LIST_ROW + 4.0, self.scroll, rect.size.y)
        } else { self.band_at(self.scroll).min(bands)..(self.band_at(self.scroll + rect.size.y) + 1).min(bands) };
        cx.begin_turtle(Walk::abs_rect(rect), Layout::flow_overlay().with_clip(true, true));
        self.drawn_from = rows.start * columns;
        self.drawn_to = (rows.end * columns).min(self.rows.len());
        for index in self.drawn_from..self.drawn_to {
            let tile = self.tile_rect(index);
            self.draw_card(cx, index, tile, &p);
        }
        cx.end_turtle();
        DrawStep::done()
    }
}

/// Rows (of `count`, the first `top` below the view's top, each `h` tall,
/// `pitch` apart) that intersect a view `view_h` tall scrolled by `scroll`:
/// every row with any part visible, not only whole ones.
pub fn visible_rows(count: usize, top: f64, h: f64, pitch: f64, scroll: f64, view_h: f64) -> std::ops::Range<usize> {
    if count == 0 || pitch <= 0.0 || view_h <= 0.0 {
        return 0..0;
    }
    // Row r spans [top + r*pitch - scroll, … + h) in view space.
    let first = (((scroll - top - h) / pitch).floor() + 1.0).max(0.0) as usize;
    let end = ((scroll + view_h - top) / pitch).ceil().max(0.0) as usize;
    first.min(count)..end.min(count)
}

#[cfg(test)]
mod visible_tests {
    use super::visible_rows;

    /// Brute force: every row whose span meets the view.
    fn expected(count: usize, top: f64, h: f64, pitch: f64, scroll: f64, view_h: f64) -> Vec<usize> {
        (0..count).filter(|r| {
            let y0 = top + *r as f64 * pitch - scroll;
            y0 < view_h && y0 + h > 0.0
        }).collect()
    }

    #[test]
    fn partly_visible_rows_are_drawn() {
        // Half a row scrolled away at the top and cut at the bottom.
        assert_eq!(visible_rows(10, 8.0, 100.0, 108.0, 58.0, 250.0), 0..3);
        for scroll in [0.0, 0.5, 7.9, 8.0, 50.0, 107.9, 108.0, 108.1, 300.3, 900.0] {
            for view_h in [1.0, 99.0, 100.0, 250.0, 1000.0] {
                let range = visible_rows(12, 8.0, 100.0, 108.0, scroll, view_h);
                assert_eq!(range.collect::<Vec<_>>(), expected(12, 8.0, 100.0, 108.0, scroll, view_h), "scroll {scroll} view {view_h}");
            }
        }
        assert_eq!(visible_rows(0, 8.0, 100.0, 108.0, 0.0, 500.0), 0..0);
    }
}

/// The explorer's preview of the selected item (design §4.8): the media fit
/// in a black well with a rim. A video skims under the pointer from its
/// filmstrip and plays (Space) through the platform player, which the page
/// lays over the picture; a sound shows its waveform with a playhead and
/// seeks on a click; an image or a cut shows its picture.
#[derive(Script, ScriptHook, Widget)]
pub struct CutAssetPreview {
    #[uid]
    uid: WidgetUid,
    #[walk]
    walk: Walk,
    #[redraw]
    #[live]
    draw_bg: DrawColor,
    #[live]
    draw_bar: DrawColor,
    #[live]
    draw_well: DrawEditBlock,
    #[live]
    draw_picture: DrawEditPicture,
    #[live]
    draw_time: DrawText,
    #[rust]
    kind: CardKind,
    #[rust]
    film: Option<Filmstrip>,
    #[rust]
    texture: Option<Texture>,
    #[rust]
    peaks: Option<Arc<Peaks>>,
    #[rust]
    seconds: f64,
    /// Where playback is (or was left), and whether it plays.
    #[rust]
    t: f64,
    #[rust]
    playing: bool,
    /// The pointer's x over the picture, 0..1.
    #[rust]
    skim: Option<f64>,
    /// A sentence over the picture ("No render").
    #[rust]
    note: String,
    /// The picture's aspect, when known before a frame is.
    #[rust]
    aspect: Option<f64>,
    /// A voice take: the voice role's hue.
    #[rust]
    pub voice: bool,
    #[rust]
    rect: Rect,
}

#[derive(Clone, Debug, Default)]
pub enum AssetPreviewAction {
    #[default]
    None,
    /// A click on a sound: play from here.
    Seek(f64),
}

impl CutAssetPreview {
    #[allow(clippy::too_many_arguments)]
    pub fn set_item(&mut self, cx: &mut Cx, kind: CardKind, film: Option<Filmstrip>, texture: Option<Texture>, peaks: Option<Arc<Peaks>>, seconds: f64, note: &str, aspect: Option<f64>) {
        let film = if matches!(kind, CardKind::Cut | CardKind::Deck) { None } else { film };
        let same = self.kind == kind && self.seconds == seconds && self.film.as_ref().map(Arc::as_ptr) == film.as_ref().map(Arc::as_ptr)
            && self.peaks.as_ref().map(Arc::as_ptr) == peaks.as_ref().map(Arc::as_ptr) && self.texture == texture && self.note == note && self.aspect == aspect;
        if !same {
            self.kind = kind;
            self.film = film;
            self.texture = texture;
            self.peaks = peaks;
            self.seconds = seconds;
            self.note = note.to_string();
            self.aspect = aspect;
            self.redraw(cx);
        }
    }

    pub fn set_position(&mut self, cx: &mut Cx, t: f64, playing: bool) {
        if (self.t - t).abs() > 1e-3 || self.playing != playing {
            self.t = t;
            self.playing = playing;
            self.redraw(cx);
        }
    }

    /// Where the picture sits in `room` at `aspect`.
    pub fn fit(room: Rect, aspect: f64) -> Rect {
        let (w, h) = if aspect > room.size.x / room.size.y.max(1.0) { (room.size.x, room.size.x / aspect) } else { (room.size.y * aspect, room.size.y) };
        Rect { pos: dvec2((room.pos.x + (room.size.x - w) * 0.5).round(), (room.pos.y + (room.size.y - h) * 0.5).round()), size: dvec2(w.round(), h.round()) }
    }

    fn texture_aspect(cx: &mut Cx2d, texture: &Texture) -> f64 {
        let (tw, th) = texture.get_format(cx).vec_width_height().unwrap_or((16, 9));
        tw.max(1) as f64 / th.max(1) as f64
    }
}

impl Widget for CutAssetPreview {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, _scope: &mut Scope) {
        match event.hits(cx, self.draw_bg.area()) {
            Hit::FingerHoverIn(fe) | Hit::FingerHoverOver(fe) => {
                let x = ((fe.abs.x - self.rect.pos.x) / self.rect.size.x.max(1.0)).clamp(0.0, 1.0);
                if self.skim != Some(x) && !self.playing {
                    self.skim = Some(x);
                    self.redraw(cx);
                }
            }
            Hit::FingerHoverOut(_) => {
                if self.skim.take().is_some() {
                    self.redraw(cx);
                }
            }
            Hit::FingerUp(fe) if fe.is_over && !fe.cancelled && self.kind == CardKind::Audio => {
                let x = ((fe.abs.x - self.rect.pos.x) / self.rect.size.x.max(1.0)).clamp(0.0, 1.0);
                cx.widget_action(self.widget_uid(), AssetPreviewAction::Seek(x * self.seconds));
            }
            _ => {}
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, _scope: &mut Scope, walk: Walk) -> DrawStep {
        let rect = cx.walk_turtle(walk);
        self.rect = rect;
        let p = makepad_look::palette::get(cx);
        let ground = if self.kind == CardKind::Audio { p.wave_bg } else { p.video };
        self.draw_well.fill = Vec4f::from_u32(ground);
        self.draw_well.edge = self.draw_well.fill;
        self.draw_well.stroke = 0.0;
        self.draw_well.radius = 4.0;
        self.draw_well.shape = 0.0;
        self.draw_well.draw_abs(cx, rect);
        // The background area answers the pointer.
        self.draw_bg.color = vec4(0.0, 0.0, 0.0, 0.0);
        self.draw_bg.draw_abs(cx, rect);
        let seconds = self.seconds.max(0.0);
        let skim_t = self.skim.filter(|_| !self.playing).map(|x| x * seconds);
        let shown_t = skim_t.unwrap_or(self.t);
        if self.kind == CardKind::Audio {
            if let Some(peaks) = self.peaks.clone() {
                let hue = makepad_look::palette::on_wave(cx, if self.voice { CutRole::VOICE.hue } else { CutRole::AUDIO.hue });
                let mid = rect.pos.y + rect.size.y * 0.5;
                let per_px = seconds.max(0.1) / rect.size.x.max(1.0);
                let half = rect.size.y * 0.42;
                let mut x = 0.0;
                while x < rect.size.x {
                    let (t0, t1) = (x * per_px, (x + 1.0) * per_px);
                    let peak = wave_height(peaks.over(t0, t1)) * half;
                    let rms = wave_height(peaks.rms_over(t0, t1)) * half;
                    let mut c = Vec4f::from_u32(hue);
                    c.w = 0.45;
                    self.draw_bar.color = c;
                    self.draw_bar.draw_abs(cx, Rect { pos: dvec2(rect.pos.x + x, mid - peak), size: dvec2(1.0, (peak * 2.0).max(1.0)) });
                    self.draw_bar.color = Vec4f::from_u32(hue);
                    self.draw_bar.draw_abs(cx, Rect { pos: dvec2(rect.pos.x + x, mid - rms), size: dvec2(1.0, (rms * 2.0).max(1.0)) });
                    x += 1.0;
                }
                if seconds > 0.0 {
                    self.draw_bar.color = Vec4f::from_u32(p.wave_head);
                    self.draw_bar.draw_abs(cx, Rect { pos: dvec2(rect.pos.x + rect.size.x * (shown_t / seconds).clamp(0.0, 1.0), rect.pos.y), size: dvec2(1.0, rect.size.y) });
                }
            }
        } else {
            let texture = match (&self.film, skim_t) {
                (Some(film), Some(t)) if film.len() > 1 => film.iter().min_by(|a, b| (a.0 - t).abs().total_cmp(&(b.0 - t).abs())).map(|(_, texture)| texture.clone()),
                _ => self.texture.clone().or_else(|| self.film.as_ref().and_then(|film| film.get(film.len() / 10).or(film.first()).map(|(_, texture)| texture.clone()))),
            };
            if let Some(texture) = texture {
                let aspect = self.aspect.unwrap_or_else(|| Self::texture_aspect(cx, &texture));
                let pic = Self::fit(Rect { pos: rect.pos + dvec2(1.0, 1.0), size: rect.size - dvec2(2.0, 2.0) }, aspect);
                self.draw_picture.draw_vars.set_texture(0, &texture);
                self.draw_picture.opacity = if self.note.is_empty() { 1.0 } else { 0.6 };
                self.draw_picture.uv = vec4(0.0, 0.0, 1.0, 1.0);
                self.draw_picture.draw_abs(cx, pic);
                // The rim.
                self.draw_well.fill = vec4(0.0, 0.0, 0.0, 0.0);
                self.draw_well.edge = Vec4f::from_u32(p.glass_rim);
                self.draw_well.stroke = 1.0;
                self.draw_well.radius = 0.0;
                self.draw_well.draw_abs(cx, Rect { pos: pic.pos - dvec2(1.0, 1.0), size: pic.size + dvec2(2.0, 2.0) });
                if let (Some(x), true) = (self.skim, self.film.as_ref().is_some_and(|film| film.len() > 1)) {
                    self.draw_bar.color = Vec4f::from_u32(p.glass_white);
                    self.draw_bar.draw_abs(cx, Rect { pos: dvec2(rect.pos.x + x * rect.size.x, pic.pos.y), size: dvec2(1.0, pic.size.y) });
                }
            }
            if !self.note.is_empty() {
                let note = self.note.clone();
                let w = advance(&self.draw_time, cx, &note);
                self.draw_time.color = Vec4f::from_u32(p.glass_ink);
                self.draw_time.draw_abs(cx, dvec2(rect.pos.x + (rect.size.x - w) * 0.5, rect.pos.y + rect.size.y * 0.5 - 6.0), &note);
            }
        }
        // Playable media: the progress line and the time chip.
        if matches!(self.kind, CardKind::Video | CardKind::AppRun | CardKind::Audio)  {
            if seconds > 0.0 {
                let mut track = Vec4f::from_u32(p.glass_white);
                track.w *= 0.3;
                self.draw_bar.color = track;
                let y = rect.pos.y + rect.size.y - 2.0;
                self.draw_bar.draw_abs(cx, Rect { pos: dvec2(rect.pos.x, y), size: dvec2(rect.size.x, 2.0) });
                self.draw_bar.color = Vec4f::from_u32(p.glass_white);
                self.draw_bar.draw_abs(cx, Rect { pos: dvec2(rect.pos.x, y), size: dvec2(rect.size.x * (shown_t / seconds).clamp(0.0, 1.0), 2.0) });
                // One formatter for both sides, chosen by the length (§4.8).
                let fmt = |t: f64| if seconds < 10.0 { badge_time(t) } else { short_time(t) };
                let text = format!("{} / {}", fmt(shown_t), fmt(seconds));
                let w = advance(&self.draw_time, cx, &text) + 10.0;
                let chip = Rect { pos: dvec2(rect.pos.x + 6.0, rect.pos.y + rect.size.y - 22.0), size: dvec2(w, 15.0) };
                self.draw_well.fill = Vec4f::from_u32(0x000000b3);
                self.draw_well.edge = self.draw_well.fill;
                self.draw_well.stroke = 0.0;
                self.draw_well.radius = 2.0;
                self.draw_well.draw_abs(cx, chip);
                self.draw_time.color = Vec4f::from_u32(p.glass_ink);
                self.draw_time.draw_abs(cx, dvec2(chip.pos.x + 5.0, chip.pos.y + 2.0), &text);
            }
        }
        DrawStep::done()
    }
}

#[cfg(test)]
mod tests {
    use super::short_time;

    #[test]
    fn badges_read_minutes_and_seconds() {
        assert_eq!(short_time(12.2), "0:12");
        assert_eq!(super::badge_time(1.62), "1.6 s");
        assert_eq!(super::badge_time(1.99), "1.9 s");
        assert_eq!(short_time(201.0), "3:21");
        assert_eq!(short_time(3725.0), "1:02:05");
    }
}
