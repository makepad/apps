//! The DECK picture: the player's frame at the output's aspect on black,
//! drawn from this widget's pass (the renderers record inside it).

use super::frames::DeckFrames;
use super::player::{Command, Player, Pointer, PointerUse};
use super::playbar::{self, PlayBar, PlayButton};
use super::shapes::{DrawDeckUi, STRIP_GAP};
use super::thumbnails::{SharedThumbnails, Thumbnails};
use makepad_story_deck::DeckDoc;
use makepad_story_deck::sketch::SketchText;
use makepad_widgets::makepad_draw::text::selection::Cursor;
use makepad_widgets::*;
use std::cell::RefCell;
use std::rc::Rc;
use super::selection::Selection as ElementSelection;
use super::super::still_cache::Stills;
use super::sketch::{SketchAction, Typing};

script_mod! {
    use mod.prelude.widgets_internal.*
    use mod.widgets.*
    let stage = mod.stage

    set_type_default() do #(DrawDeckPicture::script_shader(vm)){
        ..mod.draw.DrawQuad
        image: texture_2d(float)
        pixel: fn() {
            let c = self.image.sample(self.pos)
            return vec4(c.xyz * self.opacity, self.opacity)
        }
    }

    // The demo's synthetic cursor over the app: an arrow whose tip is the
    // middle of the quad, and a ring that grows and fades at a press
    // (the Rust struct's `ripple`, 0..1, negative: none). Never drawn into the app itself.
    set_type_default() do #(DrawDeckCursor::script_shader(vm)){
        ..mod.draw.DrawQuad
        pixel: fn() {
            let sdf = Sdf2d.viewport(self.pos * self.rect_size)
            let c = self.rect_size * 0.5
            if self.ripple >= 0.0 {
                sdf.circle(c.x, c.y, 5.0 + self.ripple * 19.0)
                sdf.stroke(vec4(1.0, 1.0, 1.0, (1.0 - self.ripple) * 0.85), 2.5)
            }
            sdf.move_to(c.x, c.y)
            sdf.line_to(c.x, c.y + 20.0)
            sdf.line_to(c.x + 4.5, c.y + 15.5)
            sdf.line_to(c.x + 8.5, c.y + 23.5)
            sdf.line_to(c.x + 11.5, c.y + 22.0)
            sdf.line_to(c.x + 7.5, c.y + 14.5)
            sdf.line_to(c.x + 13.5, c.y + 14.5)
            sdf.close_path()
            sdf.fill_keep(vec4(1.0, 1.0, 1.0, 1.0))
            sdf.stroke(vec4(0.0, 0.0, 0.0, 0.9), 1.5)
            return sdf.result
        }
    }

    mod.widgets.DeckViewBase = #(DeckView::register_widget(vm))
    mod.widgets.DeckView = set_type_default() do mod.widgets.DeckViewBase{
        width: Fill height: Fill
        draw_bg +: {color: #000}
        draw_note +: {color: stage.glass_ink text_style: theme.font_regular{font_size: 10}}
        draw_ink +: {color: #fff text_style: theme.font_regular{font_size: 9}}
        draw_dim +: {color: #ccd text_style: theme.font_regular{font_size: 9}}
        draw_status +: {color: #dde text_style: theme.font_regular{font_size: 12.5}}
        // Typing a sketch on the slide: the title (its first line) and the
        // bullets (a line each) as two fields that act as one, at the sizes
        // the slide's layout shows them (set as the picture is drawn).
        sketch_title: TextInput{
            is_multiline: true
            submit_on_enter: true
            empty_text: "Title"
            padding: Inset{left: 0 right: 0 top: 0 bottom: 0}
            draw_bg +: {color: #0000 color_hover: #0000 color_focus: #0000 border_size: 0.0}
            draw_text +: {color: #f4f1ea text_style: theme.font_bold{font_size: 34}}
        }
        sketch_editor: TextInput{
            is_multiline: true
            empty_text: "A first point\nA second point"
            padding: Inset{left: 0 right: 0 top: 0 bottom: 0}
            draw_bg +: {color: #0000 color_hover: #0000 color_focus: #0000 border_size: 0.0}
            draw_text +: {color: #f4f1ea text_style: theme.font_regular{font_size: 18}}
        }
    }
}

#[derive(Script, ScriptHook)]
#[repr(C)]
pub struct DrawDeckPicture {
    #[deref]
    draw_super: DrawQuad,
    #[live]
    pub opacity: f32,
}

#[derive(Script, ScriptHook)]
#[repr(C)]
pub struct DrawDeckCursor {
    #[deref]
    draw_super: DrawQuad,
    #[live]
    pub ripple: f32,
}

/// A sketch button on the picture.
#[derive(Clone, Debug)]
enum SketchChip {
    /// Type on the slide (its name, its words now).
    Edit(String, String),
    /// Fancy it up, or regenerate (the slide's name).
    Fancy(String),
}

/// The cursor quad's side (the arrow's tip is its middle).
const CURSOR_QUAD: f64 = 56.0;

#[derive(Clone, Debug, Default)]
pub enum DeckAction {
    #[default]
    None,
    /// The shown slide changed.
    Moved,
    Selected,
}

#[derive(Script, ScriptHook, Widget)]
pub struct DeckView {
    #[rust]
    selection: ElementSelection,
    #[rust]
    pick_at: Option<[f64; 2]>,
    #[rust]
    presenting: bool,
    #[rust]
    preview: Option<super::preview::Preview>,
    #[uid]
    uid: WidgetUid,
    #[walk]
    walk: Walk,
    #[redraw]
    #[live]
    draw_bg: DrawColor,
    #[live]
    draw_picture: DrawDeckPicture,
    #[live]
    draw_note: DrawText,
    #[live]
    draw_cursor: DrawDeckCursor,
    #[live]
    draw_ui: DrawDeckUi,
    #[live]
    draw_ink: DrawText,
    #[live]
    draw_dim: DrawText,
    /// An app's build or launch status over the slide (UI-sized).
    #[live]
    draw_status: DrawText,
    /// The play bar: the ruler, the holds and the badges (see `playbar.rs`).
    #[rust]
    playbar: PlayBar,
    /// Whether the shown slide's picture cannot change with time, as last
    /// seen settled (its slide): the engine says so only between frames, and
    /// the bar's badge must not flicker while the slide plays.
    #[rust]
    known_independent: Option<(usize, bool)>,
    #[rust]
    player: Option<Player>,
    /// Where the picture was drawn, for the pointer, and in which window.
    #[rust]
    picture: Rect,
    #[rust]
    window: Option<WindowId>,
    #[rust]
    frames: Option<Rc<DeckFrames>>,
    #[rust]
    thumbnails: Option<SharedThumbnails>,
    #[rust]
    stills: Stills,
    #[rust]
    still_specs: Vec<Option<std::sync::Arc<super::super::still_cache::Spec>>>,
    #[rust]
    still_revision: u64,
    #[rust]
    library_demands: Vec<super::super::still_cache::Demand>,

    /// The picture alone fills the window: no margin around it.
    #[rust]
    bare: bool,
    #[rust]
    next_frame: NextFrame,
    /// A playing slide's next picture is due (`Player::next_picture_in`).
    #[rust]
    picture_timer: Timer,
    /// The desk is recording the player without a visible picture pane.
    #[rust]
    hidden_picture: bool,
    
    /// The last idle state logged, so the log shows each change once.
    #[rust]
    logged_idle: Option<bool>,
    /// The time left in the talk, in a corner of the picture while
    /// presenting (the speaker's, never in the output window's frames);
    /// true: over time.
    #[rust]
    clock_readout: Option<(String, bool)>,

    /// The sketch's title line, and its bullet lines (`sketch_editor`).
    #[live]
    sketch_title: TextInput,
    #[live]
    sketch_editor: TextInput,
    /// The sketch typing (the slide typed on) and its just-committed words.
    #[rust]
    typing: Typing,
    #[rust]
    focus_sketch: bool,
    /// Where the two fields were drawn.
    #[rust]
    sketch_rect: Rect,
    /// The sketch buttons on the picture, as drawn.
    #[rust]
    sketch_chips: Vec<(Rect, SketchChip)>,
    /// The slide "Fancy it up" is running on, and slides whose words
    /// changed since their graphic was made.
    #[rust]
    fancy_busy: Option<String>,
    #[rust]
    fancy_regenerate: Vec<String>,
}

impl DeckView {
    pub fn idle(&self) -> bool { self.player.as_ref().is_none_or(Player::idle) }

    /// Type on sketch slide `name` (its words now `text`) in the main view.
    pub fn edit_sketch(&mut self, cx: &mut Cx, name: &str, text: &str) {
        self.typing.begin(name, text);
        let (title, bullets) = split_sketch(text);
        self.sketch_title.set_text(cx, title);
        self.sketch_editor.set_text(cx, bullets);
        self.focus_sketch = true;
        self.redraw(cx);
    }

    /// Whether the person is typing on a sketch slide.
    pub fn sketch_typing(&self) -> bool { self.typing.is_typing() }

    /// Whether the shown deck has a slide named `name`.
    pub fn has_slide(&self, name: &str) -> bool {
        self.player.as_ref().is_some_and(|player| player.doc().slides.iter().any(|slide| slide.name == name))
    }

    pub fn set_sketch_marks(&mut self, cx: &mut Cx, busy: Option<String>, regenerate: Vec<String>) {
        if self.fancy_busy != busy || self.fancy_regenerate != regenerate {
            self.fancy_busy = busy;
            self.fancy_regenerate = regenerate;
            self.redraw(cx);
        }
    }

    /// The typed words go to the page (when they changed) and typing ends.
    fn commit_sketch(&mut self, cx: &mut Cx) {
        let typed = self.sketch_words();
        let deck = self.typing.slide().and_then(|slide| self.deck_sketch(slide));
        if let Some((slide, text)) = self.typing.commit(typed, deck.as_deref()) {
            cx.widget_action(self.uid, SketchAction::Commit { slide, text });
        }
        self.redraw(cx);
    }

    /// The words in the two fields, as the slide's `sketch`: the title
    /// line, then the bullet lines.
    fn sketch_words(&self) -> String {
        let title = self.sketch_title.text();
        let bullets = self.sketch_editor.text();
        if bullets.trim().is_empty() { title.trim_end().to_string() } else { format!("{}\n{}", title.trim_end(), bullets.trim_end()) }
    }

    /// One event for the two sketch fields: Enter (or Down where the caret
    /// cannot go further down) in the title goes on to the first bullet,
    /// Backspace at the start of the bullets joins their first line onto the
    /// title, Up where the caret cannot go further up goes back to the
    /// title's end -- one text in two sizes. Esc, Cmd/Ctrl+Enter or the keys leaving both
    /// fields writes the words.
    fn sketch_event(&mut self, cx: &mut Cx, event: &Event) {
        let title_had = cx.has_key_focus(self.sketch_title.area());
        let body_had = cx.has_key_focus(self.sketch_editor.area());
        let body_at = self.sketch_editor.cursor().index;
        let title_at = self.sketch_title.cursor().index;
        let mut commit = false;
        let mut to_body = false;
        for action in cx.capture_actions(|cx| self.sketch_title.handle_event(cx, event, &mut Scope::empty())) {
            match action.as_widget_action().cast() {
                TextInputAction::Returned(_, mods) if mods.is_primary() => commit = true,
                TextInputAction::Returned(..) => to_body = true,
                // A pasted or typed line break: what follows it is bullets.
                TextInputAction::Changed(text) if text.contains('\n') => {
                    let (title, rest) = text.split_once('\n').unwrap_or((&text, ""));
                    let (title, rest) = (title.to_string(), rest.to_string());
                    let bullets = self.sketch_editor.text();
                    self.sketch_title.set_text(cx, &title);
                    self.sketch_editor.set_text(cx, &if bullets.is_empty() { rest } else { format!("{rest}\n{bullets}") });
                    to_body = true;
                }
                TextInputAction::Escaped => commit = true,
                TextInputAction::KeyFocusLost => commit |= !cx.has_key_focus(self.sketch_editor.area()),
                _ => {}
            }
        }
        for action in cx.capture_actions(|cx| self.sketch_editor.handle_event(cx, event, &mut Scope::empty())) {
            match action.as_widget_action().cast() {
                TextInputAction::Returned(..) | TextInputAction::Escaped => commit = true,
                TextInputAction::KeyFocusLost => commit |= !cx.has_key_focus(self.sketch_title.area()),
                _ => {}
            }
        }
        if let Event::KeyDown(key) = event {
            let plain = !key.modifiers.any();
            let title_len = self.sketch_title.text().len();
            if title_had && plain && key.key_code == KeyCode::ArrowDown && self.sketch_title.cursor().index == title_at {
                to_body = true;
            }
            let body_now = self.sketch_editor.cursor().index;
            if body_had && plain && key.key_code == KeyCode::ArrowUp && body_now == body_at {
                self.focus_sketch_field(cx, false, title_len);
            }
            if body_had && plain && key.key_code == KeyCode::Backspace && body_at == 0 && body_now == 0 && self.sketch_editor.selected_text().is_empty() {
                let bullets = self.sketch_editor.text();
                let (first, rest) = bullets.split_once('\n').unwrap_or((&bullets, ""));
                let (first, rest) = (first.to_string(), rest.to_string());
                let title = self.sketch_title.text();
                self.sketch_title.set_text(cx, &format!("{title}{first}"));
                self.sketch_editor.set_text(cx, &rest);
                self.focus_sketch_field(cx, false, title.len());
            }
        }
        if to_body && !commit {
            self.focus_sketch_field(cx, true, 0);
        }
        if commit { self.commit_sketch(cx); }
    }

    /// The keys to the title (`body` false) or the bullets, the caret at byte `at`.
    fn focus_sketch_field(&mut self, cx: &mut Cx, body: bool, at: usize) {
        let field = if body { &mut self.sketch_editor } else { &mut self.sketch_title };
        field.take_key_focus(cx);
        field.set_cursor(cx, Cursor { index: at, prefer_next_row: false }, false);
        self.redraw(cx);
    }

    /// The shown slide, when it has a sketch: its name and whether it is
    /// still in the default layout.
    fn shown_sketch(&self) -> Option<(String, String, bool)> {
        let player = self.player.as_ref()?;
        let slide = player.doc().slides.get(player.current())?;
        let words = self.typing.words(&slide.name, slide.sketch.as_deref()?);
        Some((slide.name.clone(), words, slide.sketch_only()))
    }

    /// The deck's own words for sketch slide `name`.
    fn deck_sketch(&self, name: &str) -> Option<String> {
        self.player.as_ref()?.doc().slides.iter().find(|slide| slide.name == name)?.sketch.clone()
    }

    /// A press on the picture that is the sketch's: a button, typing on a
    /// sketch slide, or the end of typing. True when it was.
    fn sketch_press(&mut self, cx: &mut Cx, abs: Vec2d) -> bool {
        if self.presenting { return false; }
        if self.typing.is_typing() {
            // The fields take their own presses; one between them (by the
            // rule) keeps the typing as it is. Any other press has
            // committed the words already (`handle_event`).
            if self.sketch_rect.contains(abs) { return true; }
            self.commit_sketch(cx);
        }
        if let Some((_, chip)) = self.sketch_chips.iter().find(|(rect, _)| rect.contains(abs)).cloned() {
            match chip {
                SketchChip::Edit(slide, text) => self.edit_sketch(cx, &slide, &text),
                SketchChip::Fancy(slide) => cx.widget_action(self.uid, SketchAction::FancyUp { slide }),
            }
            return true;
        }
        match self.shown_sketch() {
            Some((name, text, true)) if self.picture.contains(abs) => {
                self.edit_sketch(cx, &name, &text);
                true
            }
            _ => false,
        }
    }

    /// The sketch buttons along the picture's bottom edge, right to left,
    /// and the editor over the picture while typing.
    fn draw_sketch(&mut self, cx: &mut Cx2d, origin: Vec2d, size: Vec2d) {
        self.sketch_chips.clear();
        if self.presenting { return; }
        if let Some(name) = self.typing.slide() {
            if !self.has_slide(name) || self.shown_sketch().is_none_or(|(shown, ..)| shown != name) {
                self.typing.cancel();
            }
        }
        if self.typing.is_typing() {
            // The slide as its layout will show it: the title at the
            // layout's title size, the accent rule, the bullets at theirs
            // (the layout's units are pixels of a 1080-line picture; text
            // sizes are points, 72 to 96 logical pixels).
            let metrics = SketchText::parse(&self.sketch_words()).metrics();
            let unit = size.y / 1080.0;
            let points = |px: f64| (px * unit * 0.75) as f32;
            self.sketch_title.set_font_size(cx, points(metrics.title_px));
            self.sketch_editor.set_font_size(cx, points(metrics.body_px));
            self.draw_ui.fill(cx, Rect { pos: origin, size }, vec4f(0.04, 0.05, 0.07, 0.94), 0.0);
            let accent = vec4f(0.49, 0.78, 0.9, 0.9);
            let hint = "Sketch: the title, Enter, then a line per bullet. Esc, Cmd+Enter or a click outside when done";
            self.draw_dim.draw_abs(cx, origin + dvec2(metrics.left * unit, 12.0), hint);
            let left = origin.x + metrics.left * unit;
            let width = (size.x - 2.0 * metrics.left * unit).max(40.0);
            let top = origin.y + metrics.top * unit;
            let title_walk = Walk { abs_pos: Some(dvec2(left, top)), width: Size::Fixed(width), height: Size::fit(), ..Walk::default() };
            let _ = self.sketch_title.draw_walk(cx, &mut Scope::empty(), title_walk);
            let title_rect = self.sketch_title.area().rect(cx);
            let rule = title_rect.pos.y + title_rect.size.y.max(metrics.title_px * unit) + metrics.rule_gap * unit;
            self.draw_ui.fill(cx, Rect { pos: dvec2(left, rule), size: dvec2(metrics.rule.0 * unit, metrics.rule.1 * unit) }, accent, 0.0);
            let body_top = rule + metrics.body_gap * unit;
            let mark = metrics.mark * unit;
            let body = Rect { pos: dvec2(left + mark, body_top), size: dvec2((width - mark).max(40.0), (origin.y + size.y - body_top - 44.0).max(metrics.body_px * unit * 1.5)) };
            let _ = self.sketch_editor.draw_walk(cx, &mut Scope::empty(), Walk::abs_rect(body));
            self.sketch_rect = Rect { pos: dvec2(left, top), size: dvec2(width, body.pos.y + body.size.y - top) };
            if std::mem::take(&mut self.focus_sketch) {
                self.sketch_title.take_key_focus(cx);
                let at = self.sketch_title.text().len();
                self.sketch_title.set_cursor(cx, Cursor { index: at, prefer_next_row: false }, false);
            }
            return;
        }
        let Some((name, text, plain)) = self.shown_sketch() else { return };
        let busy = self.fancy_busy.as_deref() == Some(name.as_str());
        let mut chips: Vec<(String, Option<SketchChip>)> = vec![("Edit sketch".into(), Some(SketchChip::Edit(name.clone(), text)))];
        let fancy = if busy { "Fancying up…" } else if plain { "Fancy it up" } else if self.fancy_regenerate.contains(&name) { "Regenerate" } else { "Fancy it up again" };
        chips.push((fancy.into(), (!busy).then_some(SketchChip::Fancy(name))));
        let mut right = origin.x + size.x - 10.0;
        let y = origin.y + size.y - 38.0;
        for (label, action) in chips.into_iter().rev() {
            let width = 22.0 + 7.2 * label.chars().count() as f64;
            let rect = Rect { pos: dvec2(right - width, y), size: dvec2(width, 28.0) };
            let lit = matches!(action, Some(SketchChip::Fancy(_)));
            let color = if lit { vec4f(0.27, 0.55, 0.85, 0.92) } else { vec4f(0.05, 0.06, 0.08, 0.82) };
            self.draw_ui.fill(cx, rect, color, 6.0);
            self.draw_ink.draw_abs(cx, rect.pos + dvec2(11.0, 7.0), &label);
            if let Some(chip) = action { self.sketch_chips.push((rect, chip)); }
            right -= width + 8.0;
        }
    }

    pub fn library_stills(&self) -> &super::super::still_cache::Stills { &self.stills }

    pub fn set_library_stills(&mut self, cx: &mut Cx, demands: Vec<super::super::still_cache::Demand>) {
        let changed = self.library_demands.len() != demands.len() || self.library_demands.iter().zip(&demands)
            .any(|(a,b)| a.id != b.id || a.size != b.size || !std::sync::Arc::ptr_eq(&a.spec, &b.spec));
        self.library_demands = demands;
        if changed { self.redraw(cx); }
    }

    fn reset_stills(&mut self, cx: &mut Cx, doc: Option<&DeckDoc>) {
        self.stills.clear(cx);
        self.still_revision = 0;
        self.still_specs = doc.map(|doc| doc.slides.iter().map(|slide| {
            let makepad_story_deck::SlideCut::Inline(cut) = &slide.cut else { return None };
            // Live inputs get an app/name card; a thumbnail never hosts an app.
            if super::thumbnails::live_label(slide).is_some() { return None; }
            let source = super::super::still_cache::declarations(&doc.source, slide.span);
            Some(std::sync::Arc::new(super::super::still_cache::Spec { source: source.into(), context: doc.tree.clone(), cut: std::sync::Arc::new((**cut).clone()), live_name: None }))
        }).collect()).unwrap_or_default();
    }

    pub fn first_still(&self) -> Option<makepad_library_browser::lanes::Thumb> { self.stills.thumb(0).or_else(|| self.stills.thumb(1)) }

    pub fn has_uncarried_stills(&self, tree: &makepad_asset_data::tree::TreeDraft) -> bool {
        self.stills.has_uncarried(tree, 0..self.still_specs.len() * 2)
    }

    fn draw_stills(&mut self, cx: &mut Cx2d) {
        self.stills.begin();
        let shared = self.thumbnails();
        {
            let state = shared.borrow();
            if state.sorter_visible {
                for index in &state.visible {
                    if let (Some(Some(spec)), Some(size)) = (self.still_specs.get(*index), state.sizes.get(index)) {
                        self.stills.want(*index * 2, spec.clone(), *size);
                    }
                }
            }
            let next = state.current + 1;
            if state.presenter_visible {
                if let (Some(Some(spec)), Some(size)) = (self.still_specs.get(next), state.presenter_size) {
                    self.stills.want(next * 2 + 1, spec.clone(), size);
                }
            }
        }
        for demand in &self.library_demands { self.stills.want(demand.id, demand.spec.clone(), demand.size); }
        // Editing can show a looping or live-reading graphic forever. Its
        // playback clock must not starve the finite card jobs. During a
        // presentation the audience picture still has priority.
        // The shown slide's first picture goes before every still (a deck
        // just opened, or a slide just entered).
        let allow_stills = (!self.presenting || self.player.as_ref().is_none_or(Player::idle))
            && !self.player.as_ref().is_some_and(Player::starting);
        self.stills.draw(cx, allow_stills && self.preview.is_none());
        if self.still_revision != self.stills.revision {
            self.still_revision = self.stills.revision;
            let mut state = shared.borrow_mut();
            for index in 0..self.still_specs.len() {
                if let Some(texture) = self.stills.texture(index * 2) { state.textures.insert(index, texture); }
                if let Some(texture) = self.stills.texture(index * 2 + 1) { state.presenter_textures.insert(index, texture); }
            }
            state.revision = state.revision.wrapping_add(1);
        }
    }

    pub fn selection(&self) -> &ElementSelection { &self.selection }

    pub fn set_presenting(&mut self, presenting: bool) {
        if presenting != self.presenting {
            if let Some(player) = &mut self.player {
                if presenting { player.start_presenting() } else { player.stop_presenting() }
            }
        }
        self.presenting = presenting;
    }

    /// The corner readout of the time left (None: none).
    pub fn set_clock_readout(&mut self, cx: &mut Cx, readout: Option<(String, bool)>) {
        if self.clock_readout != readout {
            self.clock_readout = readout;
            self.redraw(cx);
        }
    }

    pub fn select_element(&mut self, cx: &mut Cx, key: Option<String>) {
        self.selection.select(key);
        self.selection.ready = false;
        cx.widget_action(self.uid, DeckAction::Selected);
        self.redraw(cx);
    }

    pub fn fold_element(&mut self, cx: &mut Cx, key: String) {
        self.selection.fold(key);
        cx.widget_action(self.uid, DeckAction::Selected);
        self.redraw(cx);
    }

    fn reset_selection(&mut self) {
        let revision = self.selection.revision.wrapping_add(1);
        self.selection = ElementSelection::default();
        self.selection.revision = revision;
        self.pick_at = None;
    }

    pub fn start_preview(&mut self, cx: &mut Cx, doc: std::sync::Arc<DeckDoc>) -> Result<(), String> {
        if self.preview.is_some() { return Err("a preview is already rendering".into()); }
        if let Some(player) = &mut self.player { player.suspend_renderer(); }
        self.stills.clear(cx);
        self.preview = Some(super::preview::Preview::new(doc));
        self.redraw(cx);
        self.next_frame = cx.new_next_frame();
        Ok(())
    }

    pub fn take_preview(&mut self) -> Option<makepad_strict_json::Value> {
        if !self.preview.as_ref().is_some_and(super::preview::Preview::done) { return None; }
        self.preview.take().map(super::preview::Preview::result)
    }

    pub fn cancel_preview(&mut self) { self.preview = None; }
    /// The picture as a frame source, for the output window.
    pub fn frames(&mut self) -> Rc<DeckFrames> {
        self.frames.get_or_insert_with(DeckFrames::new).clone()
    }

    /// The picture the player shows now.
    pub fn shown_texture(&mut self) -> Option<Texture> {
        self.frames().texture()
    }

    pub fn thumbnails(&mut self) -> SharedThumbnails {
        self.thumbnails.get_or_insert_with(|| Rc::new(RefCell::new(Thumbnails::default()))).clone()
    }

    /// Play `doc` (None: nothing). The shown slide is the first.
    pub fn set_doc(&mut self, cx: &mut Cx, doc: Option<Rc<DeckDoc>>) {
        self.reset_selection();
        self.shutdown_apps(cx);
        let frames = self.frames();
        let thumbnails = self.thumbnails();
        {
            let mut state = thumbnails.borrow_mut();
            state.doc = doc.clone();
            state.current = 0;
            state.visible.clear();
            state.textures.clear();
            state.presenter_textures.clear();
            
            
            state.revision = state.revision.wrapping_add(1);
        }
        self.reset_stills(cx, doc.as_deref());
        self.known_independent = None;
        self.player = doc.map(|doc| Player::new(cx, doc, frames, thumbnails));
        if self.presenting {
            if let Some(player) = &mut self.player { player.start_presenting(); }
        }
        self.redraw(cx);
    }

    /// Another deck of the tree from slide `to`, entered through
    /// `transition` from the picture shown now (a link, or its return):
    /// the strip and stills take the new deck, the player its place.
    pub fn jump(&mut self, cx: &mut Cx, doc: Rc<DeckDoc>, to: usize, transition: makepad_story_deck::Transition) {
        if self.player.is_none() { return self.set_doc(cx, Some(doc)); }
        self.reset_selection();
        let thumbnails = self.thumbnails();
        {
            let mut state = thumbnails.borrow_mut();
            state.doc = Some(doc.clone());
            state.visible.clear();
            state.textures.clear();
            state.presenter_textures.clear();
            state.revision = state.revision.wrapping_add(1);
        }
        self.known_independent = None;
        if let Some(player) = &mut self.player { player.jump(cx, doc.clone(), to, transition); }
        self.reset_stills(cx, Some(&doc));
        cx.widget_action(self.uid, DeckAction::Moved);
        self.redraw(cx);
    }

    /// A link to follow or the deck's end, asked by Next.
    pub fn take_link(&mut self) -> Option<super::player::LinkRequest> {
        self.player.as_mut().and_then(super::player::Player::take_link)
    }

    /// The full deck after its first-slide load: the slide list and stills
    /// take every slide, the player keeps its engines and its place.
    pub fn complete_doc(&mut self, cx: &mut Cx, doc: Rc<DeckDoc>) {
        self.reset_stills(cx, Some(&doc));
        let thumbnails = self.thumbnails();
        {
            let mut state = thumbnails.borrow_mut();
            state.doc = Some(doc.clone());
            state.textures.clear();
            state.presenter_textures.clear();
            state.revision = state.revision.wrapping_add(1);
        }
        match &mut self.player {
            Some(player) => player.complete(doc),
            None => self.set_doc(cx, Some(doc)),
        }
        self.redraw(cx);
    }

    /// A source edit refreshes renderers but reuses compatible hosted apps,
    /// including builds that have not finished and children without a frame.
    pub fn replace_doc(&mut self, cx: &mut Cx, doc: Rc<DeckDoc>) {
        // A slide whose own source did not change keeps its card picture
        // (wherever it moved to) until its still is loaded again: a reorder
        // or an edit of one slide no longer blanks every card.
        let thumbnails = self.thumbnails();
        let kept: Vec<(std::sync::Arc<str>, Option<Texture>, Option<Texture>)> = {
            let state = thumbnails.borrow();
            self.still_specs.iter().enumerate().filter_map(|(index, spec)| {
                let spec = spec.as_ref()?;
                Some((spec.source.clone(), state.textures.get(&index).cloned(), state.presenter_textures.get(&index).cloned()))
            }).collect()
        };
        self.reset_stills(cx, Some(&doc));
        self.reset_selection();
        {
            let mut state = thumbnails.borrow_mut();
            state.doc = Some(doc.clone());
            state.visible.clear();
            state.textures.clear();
            state.presenter_textures.clear();
            for (index, spec) in self.still_specs.iter().enumerate() {
                let Some(spec) = spec else { continue };
                if let Some((_, card, presenter)) = kept.iter().find(|(source, ..)| *source == spec.source) {
                    if let Some(texture) = card { state.textures.insert(index, texture.clone()); }
                    if let Some(texture) = presenter { state.presenter_textures.insert(index, texture.clone()); }
                }
            }
            state.revision = state.revision.wrapping_add(1);
        }
        self.known_independent = None;
        if let Some(player) = &mut self.player {
            player.replace_doc(cx, doc);
            thumbnails.borrow_mut().current = player.current();
        } else {
            self.set_doc(cx, Some(doc));
        }
        self.redraw(cx);
    }

    /// The system's cameras as the platform lists them, and a number that
    /// changes when the list does (0 and empty before a deck with a camera
    /// asked the platform).
    pub fn camera_devices(&self) -> (u64, Vec<makepad_widgets::makepad_platform::video::VideoInputDesc>) {
        self.player.as_ref().map_or((0, Vec::new()), |player| {
            let (revision, devices) = player.camera_devices();
            (revision, devices.map(<[_]>::to_vec).unwrap_or_default())
        })
    }

    pub fn current(&self) -> Option<usize> {
        self.player.as_ref().filter(|player| !player.doc().slides.is_empty()).map(Player::current)
    }

    pub fn presenter(&self) -> Option<(f64, f64, (usize, usize))> {
        let player = self.player.as_ref()?;
        let (total, slide) = player.elapsed();
        Some((total, slide, player.beats()))
    }

    /// Quit every hosted app (the page closes, Stage shuts down).
    pub fn shutdown_apps(&mut self, cx: &mut Cx) {
        if let Some(player) = &mut self.player {
            player.shutdown(cx);
        }
    }

    /// Keys and text go to the app that holds the focus (in edit mode and
    /// in Present), unless a widget of Stage took the keys since; true when
    /// they did (Escape: the app let go of them).
    pub fn app_input(&mut self, cx: &mut Cx, event: &Event) -> bool {
        let Some(player) = &mut self.player else { return false };
        if cx.key_focus() != Area::Empty {
            if player.release_app_focus() { self.redraw(cx); }
            return false;
        }
        player.app_input(event)
    }

    pub fn command(&mut self, cx: &mut Cx, command: Command) {
        if let Some(player) = &mut self.player {
            let before = player.current();
            player.command(cx, command);
            if before != player.current() { self.reset_selection(); }
            // The play bar moves the clock of the slide shown, not the slide:
            // the page's slide sync (notes, list, source) has nothing to do.
            if !matches!(command, Command::Seek { .. } | Command::Play(_) | Command::Loop(_)) {
                cx.widget_action(self.uid, DeckAction::Moved);
            }
            self.redraw(cx);
        }
    }

    /// Whether the shown slide has a demo the play bar plays.
    pub fn has_demo(&self) -> bool {
        self.player.as_ref().is_some_and(Player::has_demo)
    }

    /// A press on the play bar: what it asks of the player. Dragging the
    /// ruler moves the editor's picture; while presenting it is locked (the
    /// output window shows the same picture), but a marker still jumps.
    fn playbar_action(&mut self, cx: &mut Cx, action: playbar::Action) {
        let Some(player) = &self.player else { return };
        let paused = player.playbar(self.presenting).is_some_and(|input| input.story == playbar::Story::Paused);
        let demo = player.has_demo();
        let marker_time = |bar: &PlayBar, index: usize| bar.marker_at(index);
        let command = match action {
            playbar::Action::Play(button) if demo => Some(Command::Demo { restart: button == PlayButton::Replay }),
            playbar::Action::Play(PlayButton::Pause) => Some(Command::Play(false)),
            playbar::Action::Play(PlayButton::Play) => Some(Command::Play(true)),
            playbar::Action::Play(PlayButton::Release) => Some(Command::Next),
            playbar::Action::Play(PlayButton::Replay) => Some(Command::Restart),
            playbar::Action::Loop(on) => Some(Command::Loop(on)),
            playbar::Action::Marker(index) => marker_time(&self.playbar, index).map(|(at, kind)| match kind {
                playbar::Kind::Step(step) => Command::DemoSeek(step),
                _ => Command::Seek { time: at, playing: !paused },
            }),
            playbar::Action::Scrub(_) if demo => None,
            playbar::Action::Scrub(at) => Some(Command::Seek { time: at, playing: false }),
            playbar::Action::Release { at, .. } if demo => {
                let total = player.demo_steps().len();
                Some(Command::DemoSeek(playbar::step_at(at, total)))
            }
            playbar::Action::Release { resume: true, .. } => Some(Command::Play(true)),
            playbar::Action::Release { .. } => None,
        };
        if let Some(command) = command { self.command(cx, command); } else { self.redraw(cx); }
    }

    /// What the player is waiting for or failed at; empty when all is well.
    pub fn status(&self) -> String {
        self.player.as_ref().map(Player::status).unwrap_or_default()
    }

    pub fn set_bare(&mut self, cx: &mut Cx, bare: bool) {
        if self.bare != bare {
            self.bare = bare;
            self.redraw(cx);
        }
    }

    pub(super) fn frame_event(&self, event: &Event) -> bool {
        self.next_frame.is_event(event).is_some() || self.picture_timer.is_event(event).is_some() || self.stills.frame_event(event)
    }

    pub(super) fn draw_hidden(&mut self, cx: &mut Cx2d) {
        if let Some(preview) = &mut self.preview { preview.draw(cx); }
        self.hidden_picture = true;
        self.picture = Rect::default();
        self.playbar.hide();
        if let Some(player) = &mut self.player { player.set_view_pixels((0.0, 0.0)); }
        if self.presenting && self.preview.is_none() {
            if let Some(player) = &mut self.player { player.draw(cx); }
            self.request_frame(cx, true);
        }
        self.draw_stills(cx);
    }

    /// `player_drawn`: the player recorded this frame (`Player::draw` ran).
    fn request_frame(&mut self, cx: &mut Cx, player_drawn: bool) {
        // A held story without live readers sleeps until input or a
        // resource event wakes it. The header's timers run independently.
        let idle = self.player.as_ref().is_none_or(Player::idle);
        // A playing slide whose picture is the frame of now: its next frame
        // at the slide's frame rate, by a timer, not every display frame.
        let due_in = (!idle && player_drawn && self.preview.is_none()).then(|| self.player.as_ref().and_then(Player::next_picture_in)).flatten();
        cx.stop_timer(self.picture_timer);
        self.picture_timer = match due_in {
            // just past the boundary, so the frame it asks for is the next one
            Some(delay) => cx.start_timeout(delay + 0.001),
            None => Timer::empty(),
        };
        let wanted = wants_next_frame(player_drawn, idle || due_in.is_some(), self.preview.is_some());
        if self.logged_idle != Some(!wanted) {
            self.logged_idle = Some(!wanted);
            makepad_widgets::trace!("stage.deck", "deck: picture {}", if wanted { "animating: frame requested each frame" } else { "idle: no frame requests" });
        }
        if wanted {
            self.next_frame = cx.new_next_frame();
        }
    }
}

impl Widget for DeckView {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, _scope: &mut Scope) {
        if self.stills.event(cx, event) { self.redraw(cx); }
        if let Some(preview) = &mut self.preview { preview.event(cx, event); }
        if self.player.as_mut().is_some_and(|player| player.handle_event(cx, event)) {
            self.redraw(cx);
        }
        if self.next_frame.is_event(event).is_some() {
            self.redraw(cx);
        }
        if self.picture_timer.is_event(event).is_some() {
            self.picture_timer = Timer::empty();
            self.redraw(cx);
        }
        if self.hidden_picture {
            return;
        }
        if self.typing.is_typing() {
            self.sketch_event(cx, event);
        }
        // Any press (the picture, the slide list, anywhere) first commits
        // the typed words as the one undo step, before the press is handled.
        if let Event::MouseDown(down) = event {
            if self.typing.is_typing() && !self.sketch_rect.contains(down.abs) {
                self.commit_sketch(cx);
            }
        }
        // A press anywhere else in this window (the side column, the
        // play bar) gives the keys back to the deck.
        if let Event::MouseDown(down) = event {
            if Some(down.window_id) == self.window && !self.picture.contains(down.abs)
                && self.player.as_mut().is_some_and(Player::release_app_focus) {
                self.redraw(cx);
            }
        }
        // The pointer on the picture: a click takes the keys back from a
        // text field (the page handles the keys itself), and a press on an
        // app's picture gives that app the focus.
        let hit = event.hits(cx, self.draw_bg.area());
        match &hit {
            Hit::FingerHoverOut(_) => {
                if self.playbar.hover(None) { self.redraw(cx); }
                if self.player.as_mut().is_some_and(|player| player.set_pointer(None)) {
                    self.redraw(cx);
                }
            }
            Hit::FingerHoverIn(fe) | Hit::FingerHoverOver(fe) => {
                // Lit parts of the play bar redraw the view; nothing else does.
                if self.playbar.hover(Some(fe.abs)) { self.redraw(cx); }
            }
            Hit::FingerMove(fe) => {
                if self.playbar.hover(Some(fe.abs)) { self.redraw(cx); }
            }
            Hit::FingerDown(fe) => {
                if self.playbar.hover(Some(fe.abs)) { self.redraw(cx); }
            }
            _ => {}
        }
        if let Hit::FingerDown(fe) = &hit {
            if fe.is_primary_hit() && self.sketch_press(cx, fe.abs) { return; }
        }
        if let Hit::FingerHoverIn(fe) | Hit::FingerHoverOver(fe) = &hit {
            if self.sketch_chips.iter().any(|(rect, _)| rect.contains(fe.abs)) {
                cx.set_cursor(MouseCursor::Hand);
                return;
            }
        }
        let pointer = match hit {
            Hit::FingerDown(fe) if fe.is_primary_hit() => {
                cx.set_key_focus(Area::Empty);
                Some((Pointer::Down, fe.abs, fe.time, fe.modifiers))
            }
            Hit::FingerMove(fe) => Some((Pointer::Move, fe.abs, fe.time, fe.modifiers)),
            Hit::FingerHoverIn(fe) | Hit::FingerHoverOver(fe) => Some((Pointer::Move, fe.abs, fe.time, fe.modifiers)),
            Hit::FingerUp(fe) if fe.is_primary_hit() => Some((Pointer::Up, fe.abs, fe.time, fe.modifiers)),
            Hit::FingerScroll(fe) => Some((Pointer::Scroll(fe.scroll), fe.abs, fe.time, fe.modifiers)),
            _ => None,
        };
        // The play bar takes its presses, drags and releases (and a press that
        // began on it is its own until the button is up).
        if let Some((kind, abs, _, _)) = &pointer {
            if let Some(cursor) = self.playbar.cursor(*abs) { cx.set_cursor(cursor); }
            // (taken, what it asks, whether the view redraws)
            let taken = match kind {
                Pointer::Down => match self.playbar.press(*abs) {
                    (true, action) => Some((action, true)),
                    (false, _) => None,
                },
                Pointer::Move if self.playbar.pressing() => Some((self.playbar.drag(*abs), false)),
                Pointer::Up if self.playbar.pressing() => Some((self.playbar.release(*abs), true)),
                // Hovering the bar redraws nothing (a lit part does, above).
                Pointer::Up | Pointer::Move | Pointer::Scroll(_) if self.playbar.captures(*abs) => Some((None, false)),
                _ => None,
            };
            if let Some((action, redraw)) = taken {
                match action {
                    Some(action) => self.playbar_action(cx, action),
                    None if redraw => self.redraw(cx),
                    None => {}
                }
                return;
            }
        }
        let mut repaint = false;
        if let (Some((kind, abs, time, modifiers)), Some(player)) = (pointer, &mut self.player) {
            let picture = self.picture;
            if matches!(kind, Pointer::Move | Pointer::Down | Pointer::Up) {
                // Graphics that declare pointer_x / pointer_y follow it (-1 off the picture).
                let over = (picture.size.x > 0.0 && picture.size.y > 0.0 && picture.contains(abs))
                    .then(|| [(abs.x - picture.pos.x) / picture.size.x, (abs.y - picture.pos.y) / picture.size.y]);
                repaint = player.set_pointer(over);
            }
            if picture.size.x > 0.0 && picture.size.y > 0.0 {
                // The apps take the pointer in edit mode and in Present: a
                // press on an app gives it the keys, a press elsewhere takes
                // them back (and does nothing else).
                let at = dvec2((abs.x - picture.pos.x) / picture.size.x, (abs.y - picture.pos.y) / picture.size.y);
                let used = player.pointer(kind, at, time, &modifiers);
                // Over an app: the cursor it asked for (text, hand, ...).
                if !self.playbar.captures(abs) {
                    cx.set_cursor(player.pointer_cursor());
                }
                if used != PointerUse::Deck {
                    if matches!(kind, Pointer::Down) { self.redraw(cx); }
                } else if !self.presenting && matches!(kind, Pointer::Down) {
                    if picture.contains(abs) { self.pick_at = Some([at.x, at.y]); }
                    else { self.selection.select(None); cx.widget_action(self.uid, DeckAction::Selected); }
                    self.redraw(cx);
                } else if self.presenting && matches!(kind, Pointer::Down) && picture.contains(abs) {
                    player.command(cx, Command::Next);
                    self.selection.ready = false;
                    self.selection.select(None);
                    cx.widget_action(self.uid, DeckAction::Moved);
                    self.redraw(cx);
                }
            }
        }
        if repaint {
            self.redraw(cx);
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, _scope: &mut Scope, walk: Walk) -> DrawStep {
        if let Some(preview) = &mut self.preview { preview.draw(cx); }
        self.hidden_picture = false;
        self.window = cx.get_current_window_id();
        let rect = cx.walk_turtle(walk);
        self.draw_bg.draw_abs(cx, rect);
        let Some(player) = &mut self.player else {
            self.draw_note.draw_abs(cx, rect.pos + dvec2(12.0, 12.0), "No deck open");
            self.draw_stills(cx);
            return DrawStep::done();
        };
        let (ow, oh) = (player.doc().output.0.max(1) as f64, player.doc().output.1.max(1) as f64);
        let margin = if self.bare { 0.0 } else { 8.0 };
        // The play bar sits under the picture.
        let bar_strip = if !self.bare { playbar::HEIGHT + STRIP_GAP } else { 0.0 };
        let strip = bar_strip;
        let room = dvec2(rect.size.x - 2.0 * margin, rect.size.y - 2.0 * margin - strip);
        let picture_drawn = room.x > 4.0 && room.y > 4.0;
        if picture_drawn {
            let scale = (room.x / ow).min(room.y / oh);
            let size = dvec2((ow * scale).floor(), (oh * scale).floor());
            let origin = dvec2((rect.pos.x + (rect.size.x - size.x) * 0.5).round(), (rect.pos.y + (rect.size.y - strip - size.y) * 0.5).round());
            // The slides are recorded at the picture's own pixels (and at
            // least at the output window's, which shows the same texture),
            // so the picture is never scaled up: crisp at the display's DPI.
            let dpi = cx.current_dpi_factor();
            let here = ((size.x * dpi).round().max(1.0) as u32, (size.y * dpi).round().max(1.0) as u32);
            let reader = self.frames.as_ref().and_then(|frames| frames.reader_pixels());
            let raster = reader.map_or(here, |(w, h)| {
                // The larger of the two, at the output's aspect (the output
                // window letterboxes the picture into its own size).
                let scale = (w as f64 / here.0 as f64).min(h as f64 / here.1 as f64).max(1.0);
                ((here.0 as f64 * scale).round() as u32, (here.1 as f64 * scale).round() as u32)
            });
            player.set_raster(Some(raster));
            // The renderers record inside this pass; their newest whole
            // frame (or the mix of two) is the picture.
            let shown = if self.preview.is_some() { self.frames.as_ref().and_then(|frames| frames.texture()) } else { player.draw(cx) };
            if shown.is_some() && player.inspection_ready() && !self.presenting && (!self.selection.ready || self.pick_at.is_some()) {
                let inspections = player.inspect_graphics(cx);
                self.selection.rebuild(player.doc(), player.current(), inspections, player.placed());
                if let Some(at) = self.pick_at.take() { self.selection.pick(at, 3.0 / size.x.max(size.y)); }
                cx.widget_action(self.uid, DeckAction::Selected);
            }
            if let Some(texture) = shown {
                self.draw_picture.draw_vars.set_texture(0, &texture);
                self.draw_picture.opacity = 1.0;
                self.draw_picture.draw_abs(cx, Rect { pos: origin, size });
            }
            self.picture = Rect { pos: origin, size };
            let dpi = cx.current_dpi_factor();
            player.set_view_pixels((size.x * dpi, size.y * dpi));
            if !self.presenting {
                if let Some(element) = self.selection.current() {
                    if let Some(quad) = element.quad {
                        let lo = [quad.iter().map(|p| p[0]).fold(f64::INFINITY, f64::min), quad.iter().map(|p| p[1]).fold(f64::INFINITY, f64::min)];
                        let hi = [quad.iter().map(|p| p[0]).fold(f64::NEG_INFINITY, f64::max), quad.iter().map(|p| p[1]).fold(f64::NEG_INFINITY, f64::max)];
                        let pos = origin + dvec2(lo[0] * size.x, lo[1] * size.y);
                        let edge = dvec2((hi[0] - lo[0]) * size.x, (hi[1] - lo[1]) * size.y);
                        let color = vec4f(0.65, 0.78, 0.95, 0.85);
                        for rect in [Rect { pos, size: dvec2(edge.x, 1.0) }, Rect { pos: pos + dvec2(0.0, edge.y), size: dvec2(edge.x, 1.0) }, Rect { pos, size: dvec2(1.0, edge.y) }, Rect { pos: pos + dvec2(edge.x, 0.0), size: dvec2(1.0, edge.y) }] { self.draw_ui.fill(cx, rect, color, 0.0); }
                        self.draw_dim.draw_abs(cx, dvec2(pos.x.max(origin.x), (pos.y - 15.0).max(origin.y)), &element.label);
                    }
                }
            }
            // The demo's pointer, over the picture.
            if let Some(cursor) = player.cursor() {
                let tip = origin + dvec2(cursor.at.x * size.x, cursor.at.y * size.y);
                self.draw_cursor.ripple = cursor.ripple.map_or(-1.0, |age| age as f32);
                self.draw_cursor.draw_abs(cx, Rect { pos: tip - dvec2(CURSOR_QUAD * 0.5, CURSOR_QUAD * 0.5), size: dvec2(CURSOR_QUAD, CURSOR_QUAD) });
            }
            // While a demo seek replays, the held picture is dimmed and says
            // so; the audience's own picture just holds its frame.
            let veil = if self.bare { None } else { player.veil() };
            if let Some(veil) = veil {
                self.draw_ui.fill(cx, Rect { pos: origin, size }, vec4f(0.0, 0.0, 0.0, 0.55 * veil), 0.5);
                if veil >= 1.0 {
                    let text = "seeking...";
                    let chip = Rect { pos: origin + dvec2(size.x * 0.5 - 52.0, size.y * 0.5 - 14.0), size: dvec2(104.0, 28.0) };
                    self.draw_ui.fill(cx, chip, vec4f(0.04, 0.04, 0.05, 0.85), 6.0);
                    self.draw_ink.draw_abs(cx, chip.pos + dvec2(16.0, 7.0), text);
                }
            }
            // The play bar, under the picture.
            let bar_x = rect.pos.x + margin;
            let bar_width = (rect.size.x - 2.0 * margin).max(0.0);
            let mut input = player.playbar(self.presenting).filter(|_| bar_strip > 0.0);
            if let Some(input) = &mut input {
                let slide = player.current();
                match input.independent {
                    Some(known) => self.known_independent = Some((slide, known)),
                    None => input.independent = self.known_independent.filter(|(shown, _)| *shown == slide).map(|(_, known)| known),
                }
            }
            match input {
                Some(input) => {
                    let bar = Rect { pos: dvec2(bar_x, origin.y + size.y + STRIP_GAP), size: dvec2(bar_width, playbar::HEIGHT) };
                    self.playbar.draw(cx, &mut self.draw_ui, &mut self.draw_ink, &mut self.draw_dim, bar, playbar::model(&input));
                }
                None => self.playbar.hide(),
            }
            // The time left, quiet in the top right corner while presenting (T).
            if let Some((line, over)) = &self.clock_readout {
                let width = line.chars().count() as f64 * 7.0 + 16.0;
                let chip = Rect { pos: origin + dvec2(size.x - width - 10.0, 10.0), size: dvec2(width, 22.0) };
                self.draw_ui.fill(cx, chip, vec4f(0.0, 0.0, 0.0, 0.45), 4.0);
                let color = self.draw_dim.color;
                self.draw_dim.color = if *over { vec4f(1.0, 0.45, 0.3, 0.95) } else { vec4f(0.85, 0.85, 0.9, 0.75) };
                self.draw_dim.draw_abs(cx, chip.pos + dvec2(8.0, 5.0), line);
                self.draw_dim.color = color;
            }
            // An app still building or starting: its status, small, low and
            // centred over the picture (this window only; the output window
            // shows the plain frame).
            if !self.bare && !self.presenting {
                if let Some(status) = player.status_overlay() { self.draw_app_status(cx, origin, size, &status); }
            }
            self.draw_sketch(cx, origin, size);
        }
        if !picture_drawn {
            self.playbar.hide();
            }
        self.draw_stills(cx);
        self.request_frame(cx, picture_drawn);
        DrawStep::done()
    }
}

/// Where the status chip sits on a picture of `size`: the width it may use
/// and its distance from the bottom. Its text is the view's own `draw_status`
/// (12.5 UI points); neither the app's render size nor the dpi reaches it.
pub(super) fn status_chip(size: Vec2d) -> (f64, f64) {
    ((size.x * 0.8).min(520.0).max(120.0).min(size.x), 28.0)
}

impl DeckView {
    fn draw_app_status(&mut self, cx: &mut Cx2d, origin: Vec2d, size: Vec2d, status: &super::apps::StatusOverlay) {
        let (width, lift) = status_chip(size);
        let lines: Vec<String> = [status.title.as_str(), status.note.lines().next().unwrap_or("")].iter().filter(|line| !line.is_empty()).map(|line| {
            let max = ((width - 24.0) / 8.4).max(8.0) as usize;
            if line.chars().count() > max { format!("{}…", line.chars().take(max.saturating_sub(1)).collect::<String>()) } else { line.to_string() }
        }).collect();
        let height = 14.0 + lines.len() as f64 * 18.0 + if status.progress.is_some() { 8.0 } else { 0.0 };
        let chip = Rect { pos: dvec2(origin.x + (size.x - width) * 0.5, origin.y + size.y - lift - height), size: dvec2(width, height) };
        self.draw_ui.fill(cx, chip, vec4f(0.0, 0.0, 0.0, 0.55), 6.0);
        let color = self.draw_status.color;
        self.draw_status.color = if status.error { vec4f(1.0, 0.5, 0.42, 1.0) } else { vec4f(0.86, 0.88, 0.93, 1.0) };
        let mut y = chip.pos.y + 6.0;
        for line in &lines {
            self.draw_status.draw_abs(cx, dvec2(chip.pos.x + 12.0, y), line);
            y += 18.0;
        }
        self.draw_status.color = color;
        if let Some(done) = status.progress {
            let track = Rect { pos: dvec2(chip.pos.x + 12.0, chip.pos.y + height - 10.0), size: dvec2(width - 24.0, 3.0) };
            self.draw_ui.fill(cx, track, vec4f(1.0, 1.0, 1.0, 0.18), 1.5);
            self.draw_ui.fill(cx, Rect { size: dvec2(track.size.x * done.clamp(0.0, 1.0) as f64, 3.0), ..track }, vec4f(0.45, 0.72, 1.0, 0.95), 1.5);
        }
    }
}

/// A sketch's words as the two fields show them: the title (its first line
/// that is not blank) and the bullet lines after it.
fn split_sketch(text: &str) -> (&str, &str) {
    let text = text.trim_start_matches(|c: char| c.is_whitespace());
    text.split_once('\n').unwrap_or((text, ""))
}

/// Whether the deck view asks for the next frame after this draw. Only a
/// player that recorded this frame can animate: one with no room for its
/// picture (a pane squeezed to nothing) never runs `Player::draw`, so its
/// slide never settles and it is never idle -- asking for frames for it
/// redrew the whole window every display frame (hosted Stage on DECK at a
/// small window size: 120 full draws a second, for nothing). The layout
/// change that gives the picture room redraws the view itself.
fn wants_next_frame(player_drawn: bool, player_idle: bool, preview: bool) -> bool {
    preview || (player_drawn && !player_idle)
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_status_chip_is_ui_sized_whatever_the_picture() {
        use super::status_chip;
        use makepad_widgets::*;
        let (small, lift) = status_chip(dvec2(400.0, 225.0));
        let (large, _) = status_chip(dvec2(3840.0, 2160.0));
        assert!(small <= 400.0 && small >= 120.0 && lift == 28.0);
        assert_eq!(large, 520.0);
    }

    use super::wants_next_frame;

    #[test]
    fn a_player_without_room_for_its_picture_requests_no_frames() {
        // never drawn, so never idle: still no frame requests
        assert!(!wants_next_frame(false, false, false));
        assert!(wants_next_frame(true, false, false), "a drawn, animating slide keeps asking");
        assert!(!wants_next_frame(true, true, false), "a drawn, held slide sleeps");
        assert!(wants_next_frame(false, true, true), "an editor preview animates on its own");
    }

    #[test]
    fn requests_stop_once_a_resize_settles() {
        // (room for the picture, the player idle after that draw), per draw: squeezed out,
        // then a resize gives it room, the slide records two frames and holds.
        let draws = [(false, false), (false, false), (true, false), (true, false), (true, true), (true, true)];
        let asked: Vec<bool> = draws.iter().map(|&(drawn, idle)| wants_next_frame(drawn, idle, false)).collect();
        assert_eq!(asked, [false, false, true, true, false, false]);
        // after the last request nothing asks again: the frames stop within the draws that record
        let last = asked.iter().rposition(|asked| *asked).unwrap();
        assert!(asked[last + 1..].iter().all(|asked| !asked));
    }
}
