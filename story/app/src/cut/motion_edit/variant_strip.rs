//! The variant strip's cards: each variant the AI made of the open graphic
//! (`variants.rs`), drawn as its own frame at the playhead by its own
//! `MotionDraw` (the renderer the viewer uses), with its number, label and
//! note. A click selects a card: the viewer shows that variant (played and
//! scrubbed as the graphic is) until the bar's Choose applies it or Cancel
//! goes back. The first card is the graphic as it is.
//!
//! Main-loop cost: a card's picture is recorded only when its frame
//! changes (a new variant, a new playhead), at the card's pixel size;
//! otherwise drawing is a few quads and labels per card.

use makepad_library_browser::draw::DrawEditPicture;
use makepad_motion::MotionFrame;
use makepad_motion_draw::MotionDraw;
use makepad_widgets::*;

/// One card as the host gives it.
#[derive(Clone, Debug, PartialEq)]
pub struct CardInfo {
    pub id: u64,
    pub index: usize,
    pub label: String,
    pub note: String,
    /// "writing", "loading", "" (ready) or the reason it failed.
    pub state: String,
    pub failed: bool,
    /// The one the viewer shows.
    pub selected: bool,
    /// The colours the variant writes (a strip of swatches under it).
    pub swatches: Vec<[f32; 4]>,
}

struct Card {
    info: CardInfo,
    frame: Option<MotionFrame>,
    key: Option<u64>,
    motion: Option<MotionDraw>,
    picture: Option<Texture>,
    recorded: Option<u64>,
    shown: bool,
}

#[derive(Clone, Debug, Default)]
pub enum VariantAction {
    #[default]
    None,
    /// A card clicked: the viewer shows its variant.
    Select(u64),
}

#[derive(Script, ScriptHook, Widget)]
pub struct MotionVariantThumbs {
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
    draw_picture: DrawEditPicture,
    #[live]
    draw_text: DrawText,
    #[live]
    draw_small: DrawText,
    #[rust]
    cards: Vec<Card>,
    #[rust]
    hits: Vec<(Rect, usize)>,
    /// The document's size (all variants share it).
    #[rust]
    size: [f64; 2],
    /// Where the strip was drawn, in its own space.
    #[rust]
    origin: Vec2d,
}

script_mod! {
    use mod.prelude.widgets_internal.*
    use mod.widgets.*
    let stage = mod.stage

    mod.widgets.MotionVariantThumbsBase = #(MotionVariantThumbs::register_widget(vm))
    mod.widgets.MotionVariantThumbs = set_type_default() do mod.widgets.MotionVariantThumbsBase{
        width: Fill height: 104
        draw_bg +: {color: #0000}
        draw_rect +: {color: stage.raised}
        draw_text +: {color: stage.text_hi text_style: theme.font_bold{font_size: 8}}
        draw_small +: {color: stage.muted text_style: theme.font_regular{font_size: 7.5}}
    }
}

/// The card width at this height (the frame's aspect, a label row below).
const LABEL_ROWS: f64 = 34.0;
const GAP: f64 = 8.0;

impl MotionVariantThumbs {
    /// The cards, in order; ones already shown keep their renderer.
    pub fn set_cards(&mut self, cx: &mut Cx, size: [f64; 2], cards: Vec<CardInfo>) {
        self.size = size;
        let mut old: Vec<Card> = std::mem::take(&mut self.cards);
        for info in cards {
            match old.iter().position(|card| card.info.id == info.id) {
                Some(at) => {
                    let mut card = old.remove(at);
                    card.info = info;
                    self.cards.push(card);
                }
                None => self.cards.push(Card { info, frame: None, key: None, motion: None, picture: None, recorded: None, shown: false }),
            }
        }
        self.draw_bg.redraw(cx);
    }

    /// A card's frame (its variant at the playhead); `key`: equal keys,
    /// equal frames.
    pub fn set_frame(&mut self, cx: &mut Cx, id: u64, frame: MotionFrame, key: u64) {
        let Some(card) = self.cards.iter_mut().find(|card| card.info.id == id) else { return };
        if card.key == Some(key) {
            return;
        }
        card.key = Some(key);
        card.frame = Some(frame);
        self.draw_bg.redraw(cx);
    }

    /// Whether a card's picture has painted (for the host's timing).
    pub fn painted(&self, id: u64) -> bool {
        self.cards.iter().any(|card| card.info.id == id && card.shown)
    }

    fn card_width(&self, height: f64) -> f64 {
        let aspect = if self.size[1] > 0.0 { self.size[0] / self.size[1] } else { 16.0 / 9.0 };
        ((height - LABEL_ROWS) * aspect).max(40.0)
    }
}

fn rgba(value: u32) -> Vec4f {
    Vec4f::from_u32(value)
}

impl Widget for MotionVariantThumbs {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, _scope: &mut Scope) {
        let uid = self.widget_uid();
        for card in &mut self.cards {
            if let Some(motion) = &mut card.motion {
                motion.handle_event(cx, event);
            }
        }
        // The cards' rects are where they were drawn (the strip's own
        // space); the pointer is on the window.
        let shift = self.origin - self.draw_bg.area().rect(cx).pos;
        let hit_at = |hits: &[(Rect, usize)], at: Vec2d| hits.iter().rev().find(|(rect, _)| rect.contains(at + shift)).map(|(_, index)| *index);
        if let Hit::FingerUp(fe) = event.hits(cx, self.draw_bg.area()) {
            if fe.is_over && fe.was_tap() {
                if let Some(card) = hit_at(&self.hits, fe.abs).and_then(|index| self.cards.get(index)).filter(|card| !card.info.failed && card.frame.is_some()) {
                    cx.widget_action(uid, VariantAction::Select(card.info.id));
                }
            }
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, _scope: &mut Scope, walk: Walk) -> DrawStep {
        let rect = cx.walk_turtle(walk);
        self.origin = rect.pos;
        self.hits.clear();
        let p = makepad_look::palette::get(cx);
        let height = rect.size.y;
        let width = self.card_width(height);
        let pic_h = height - LABEL_ROWS;
        cx.push_clip_rect(rect);
        let mut x = rect.pos.x;
        let dpi = cx.current_dpi_factor();
        for index in 0..self.cards.len() {
            let card_rect = Rect { pos: dvec2(x, rect.pos.y), size: dvec2(width, height) };
            x += width + GAP;
            let pic = Rect { pos: card_rect.pos, size: dvec2(width, pic_h) };
            // The frame well, and the frame when there is one.
            self.draw_rect.color = rgba(p.video);
            self.draw_rect.draw_abs(cx, pic);
            let card = &mut self.cards[index];
            if let Some(frame) = &card.frame {
                let pixels = (((width * dpi).round() as u32).max(2), ((pic_h * dpi).round() as u32).max(2));
                let key = card.key.unwrap_or(0) ^ ((pixels.0 as u64) << 40) ^ ((pixels.1 as u64) << 20);
                let motion = card.motion.get_or_insert_with(|| MotionDraw::new(cx));
                if card.recorded != Some(key) || !motion.exact(cx) {
                    if motion.ready() || card.recorded.is_none() {
                        let drawn = motion.draw(cx, frame, pixels);
                        card.picture = Some(drawn.texture);
                        card.recorded = Some(key);
                        if !drawn.exact {
                            self.draw_bg.redraw(cx);
                        }
                    }
                } else if motion.painted(cx) {
                    card.shown = true;
                } else {
                    motion.reattach(cx);
                    self.draw_bg.redraw(cx);
                }
            }
            if let Some(texture) = card.picture.clone() {
                self.draw_picture.draw_vars.set_texture(0, &texture);
                self.draw_picture.opacity = 1.0;
                self.draw_picture.uv = vec4(0.0, 0.0, 1.0, 1.0);
                self.draw_picture.draw_abs(cx, pic);
            }
            let info = card.info.clone();
            // The colours it writes, as a row of swatches over the frame's foot.
            let sw = 10.0;
            for (k, color) in info.swatches.iter().enumerate() {
                self.draw_rect.color = vec4(color[0], color[1], color[2], 1.0);
                self.draw_rect.draw_abs(cx, Rect { pos: dvec2(pic.pos.x + 4.0 + k as f64 * (sw + 2.0), pic.pos.y + pic_h - sw - 4.0), size: dvec2(sw, sw) });
            }
            // State over the frame while it is not there.
            if !info.state.is_empty() {
                let color = if info.failed { p.danger } else { p.muted };
                self.draw_small.color = rgba(color);
                self.draw_small.draw_abs(cx, dvec2(pic.pos.x + 6.0, pic.pos.y + 6.0), &short(&info.state, (width / 5.5) as usize));
            }
            // Its outline: the one shown lit.
            let edge = if info.selected { p.accent } else { p.separator };
            let line = if info.selected { 2.0 } else { 1.0 };
            self.draw_rect.color = rgba(edge);
            for r in [
                Rect { pos: pic.pos, size: dvec2(width, line) },
                Rect { pos: dvec2(pic.pos.x, pic.pos.y + pic_h - line), size: dvec2(width, line) },
                Rect { pos: pic.pos, size: dvec2(line, pic_h) },
                Rect { pos: dvec2(pic.pos.x + width - line, pic.pos.y), size: dvec2(line, pic_h) },
            ] {
                self.draw_rect.draw_abs(cx, r);
            }
            // Number and label, the note under them.
            self.draw_text.color = rgba(if info.failed { p.muted } else { p.text_hi });
            let chars = (width / 6.0) as usize;
            self.draw_text.draw_abs(cx, dvec2(card_rect.pos.x, pic.pos.y + pic_h + 4.0), &short(&if info.index == 0 { info.label.clone() } else { format!("{}  {}", info.index, info.label) }, chars));
            self.draw_small.color = rgba(p.muted);
            self.draw_small.draw_abs(cx, dvec2(card_rect.pos.x, pic.pos.y + pic_h + 19.0), &short(&info.note, (width / 5.0) as usize));
            self.hits.push((card_rect, index));
        }
        cx.pop_clip_rect();
        // Over the cards (clear): what the pointer hits, so hovering and
        // clicking are the strip's, card by card (`hits`).
        self.draw_bg.draw_abs(cx, rect);
        DrawStep::done()
    }
}

fn short(text: &str, chars: usize) -> String {
    let chars = chars.max(4);
    if text.chars().count() <= chars {
        text.to_string()
    } else {
        format!("{}…", text.chars().take(chars - 1).collect::<String>())
    }
}
