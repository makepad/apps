//! An opened graphic's code, read and typed in: the graphic and the modules
//! it imports as tabs (a film's plates often keep their drawing there; built-in
//! modules are read, not typed in). The code is a plain text editor; the
//! scene's editor panel is what edits values, and the code folds tightly
//! around what that panel binds, so a slider moving shows its line and its
//! value changing. With nothing bound it folds to what moves in time, or
//! shows it all. At each line's end, the value it holds at the playhead
//! (`= 0.42  ×12`), read live.
//!
//! Over the code a toolbar: the tabs, the way back through the jumps, and a
//! compact scrubber (play/pause, the time, the graphic as a filmstrip of
//! small frames with the song's beats and where the panel's controls act,
//! the playhead; dragged, it scrubs).
//!
//! Beside the code, foldable by its splitter: the pick. A click on the
//! picture lands here: what was drawn, the stack of code that drew it
//! (innermost first) and where its text comes from, each a jump; the code
//! unfolds to the line and scrolls there.

use super::super::timeline::timecode_full;
use makepad_library_browser::draw::DrawEditBlock;
use super::filmstrip::{FilmSource, Filmstrip};
use makepad_code_editor::code_view::CodeView;
use makepad_code_editor::text::Position;
use makepad_code_editor::{CodeDocument, CodeSession};
use makepad_motion::schema::code::{CodeOutline, RegionKind};
use makepad_motion::schema::CodeFile;
use makepad_edit_values::Edit;
use makepad_widgets::badge::advance;
use makepad_widgets::*;

/// A value the scene's editor panel binds: its path (what it holds), and
/// where the path resolved in the text (a live anchor until resolved again).
#[derive(Clone, Debug, PartialEq)]
pub struct Binding {
    pub path: String,
    pub doc: usize,
    pub range: std::ops::Range<usize>,
}

/// One document the view shows: the graphic, or a module it imports.
#[derive(Clone, Debug)]
pub struct CodeDoc {
    pub title: String,
    /// Shipped with Stage (a built-in module): read, not edited.
    pub read_only: bool,
    pub file: CodeFile,
    pub text: String,
    pub code: CodeOutline,
    /// Values at the playhead, shown at the end of the line they are
    /// written on: (byte, text).
    pub live: Vec<(usize, String)>,
    /// What its `let`s computed as it loaded: (byte, name, value).
    pub bound: Vec<(usize, String, makepad_motion::schema::Watched)>,
    /// What its `let`s held over the film: (byte, a value per sample, NaN
    /// where unbound).
    pub series: Vec<(usize, Vec<f32>)>,
    /// What its `let`s holding vectors or colours held over the film:
    /// (byte, components per sample, 2..4 components or 5 a colour).
    pub paths: Vec<(usize, Vec<[f32; 4]>, u8)>,
    /// Where each line starts (filled by the view).
    pub lines: Vec<usize>,
}

impl CodeDoc {
    /// The line of byte `at` (0-based).
    fn line(&self, at: usize) -> usize {
        self.lines.partition_point(|&start| start <= at).saturating_sub(1)
    }
}

#[derive(Clone, Debug, Default)]
pub enum CodeTimelineAction {
    #[default]
    None,
    /// The code typed in: edits of document `doc`.
    Edits { doc: usize, edits: Vec<Edit> },
    /// The playhead moved on the scrubber.
    Scrub(f64),
    /// Play or pause (the scrubber's button).
    PlayPause,
    /// A lane's close mark: the lane of this path closes.
    LaneClose(String),
    /// A key the code view does not take (undo, play, …).
    Key(KeyEvent),
    /// The code selected (the caret's block): its drawing lights up on the
    /// picture. `None`: nothing.
    Select(Option<(usize, usize)>),
}

/// A frame of a pick's stack trace: a place in the code that drew it.
#[derive(Clone, Debug, PartialEq)]
pub struct TraceFrame {
    pub doc: usize,
    pub at: usize,
    /// Its index in the pick's chain (0: innermost).
    pub frame: usize,
    /// "line 805 in helper o_draw_pcb".
    pub place: String,
    /// Its call as written (first line).
    pub call: String,
}

/// Where something acts on the film, shown on the scrubber: a panel
/// control's or module's spans (film seconds), keyed by the panel's own id
/// for it (`key`), with a label for its tip.
#[derive(Clone, Debug, PartialEq)]
pub struct ScrubSpans {
    pub key: String,
    pub label: String,
    pub spans: Vec<(f64, f64)>,
}

/// An animated value's curve lane: its path, its label, the file its keys
/// are written in (doc index) and its keys (`makepad_motion::lane`).
#[derive(Clone, Debug, PartialEq)]
pub struct Lane {
    pub path: String,
    pub label: String,
    pub doc: usize,
    pub keys: makepad_motion::lane::Keys,
}

/// A key being dragged on a lane: which, where the drag began, the value
/// range of the lane, its time and value bytes as the text is now, and the
/// gesture.
#[derive(Clone, Debug)]
struct KeyDrag {
    lane: usize,
    key: usize,
    from: DVec2,
    t0: f64,
    v0: f64,
    lo: f64,
    hi: f64,
    height: f64,
    t_now: Option<std::ops::Range<usize>>,
    v_now: std::ops::Range<usize>,
    gesture: u64,
    moved: bool,
}

/// A place in the code a jump left or went to: its document, the byte of
/// its cursor, and (once left) where its view was scrolled.
#[derive(Clone, Copy, Debug, PartialEq)]
struct NavSpot {
    doc: usize,
    at: usize,
    scroll: Option<DVec2>,
}

/// A number literal as written, its value and its unit suffix (`0.08s`:
/// 0.08 and "s"; `90deg`, `120ms`, `12px`, `0.3rad`).
pub fn written_number(text: &str) -> Option<(f64, &str)> {
    let text = text.trim();
    ["ms", "deg", "rad", "px", "s", ""].into_iter().find_map(|unit| {
        let digits = text.strip_suffix(unit)?;
        digits.parse::<f64>().ok().filter(|_| unit.is_empty() || digits.ends_with(|c: char| c.is_ascii_digit() || c == '.')).map(|v| (v, unit))
    })
}

#[derive(Clone, Debug, PartialEq)]
enum Hot {
    Tab(usize),
    /// The current file: opens the list of the scene's files.
    FileMenu,
    /// What the code folds to: what the panel binds / what moves / all.
    FoldToggle,
    /// A frame of the pick's stack trace: goes to its code.
    Trace(usize),
    /// Where the pick's text comes from: goes there, its words selected.
    Origin,
    /// The pick pane's scrollbar.
    PaneBar,
    /// Back or forward through the jumps, or a crumb of the trail.
    NavBack,
    NavForward,
    Crumb(usize),
    PlayPause,
    /// The scrubber's strip: pressed and dragged, it scrubs.
    Scrub,
    /// A span where a panel control acts: the playhead jumps to its start.
    Span(f64),
    /// A key on a lane (lane, key): dragged in time and value; selected.
    LaneKey(usize, usize),
    /// A lane's body: a double click adds a key there.
    Lane(usize),
    /// The ease into key (lane, key): a click steps it.
    LaneEase(usize, usize),
    /// A lane's close mark.
    LaneClose(usize),
}

script_mod! {
    use mod.prelude.widgets_internal.*
    use mod.widgets.*
    let stage = mod.stage

    let PaneSplit = Splitter{size: 6 axis: SplitterAxis.Horizontal
        draw_bg +: {color_bg: stage.bg color: stage.border_hi color_hover: stage.accent color_drag: stage.accent splitter_pad: 2 bar_size: 24}}

    mod.widgets.MotionCodeTimelineBase = #(MotionCodeTimeline::register_widget(vm))
    mod.widgets.MotionCodeTimeline = set_type_default() do mod.widgets.MotionCodeTimelineBase{
        width: Fill height: Fill flow: Down
        draw_text +: {color: stage.text text_style: theme.font_regular{font_size: 8.5}}
        draw_small +: {color: stage.secondary text_style: theme.font_regular{font_size: 7.5}}
        // The tabs, the way back, what the code folds to; then the
        // scrubber.
        bar := View{width: Fill height: 30}
        scrub := View{width: Fill height: 40}
        // The curve lanes of animated values, under the scrubber on its
        // time axis; they push the code down (folded away with none open).
        split_lanes := PaneSplit{axis: SplitterAxis.Vertical align: SplitterAlign.FromA(72) collapse: SplitterCollapse.A fold: SplitterCollapse.A min_horizontal: 48 max_horizontal: 120
            a: View{width: Fill height: Fill lanes := SolidView{width: Fill height: Fill draw_bg +: {color: stage.wave_bg}}}
            b: View{width: Fill height: Fill
                // The pick (foldable: dragged shut or a double click on the bar)
                // beside the code.
                split_pane := PaneSplit{align: SplitterAlign.FromA(300) fold: SplitterCollapse.A min_vertical: 160 max_vertical: 320
                    a: View{width: Fill height: Fill
                        gather_pane := SolidView{width: Fill height: Fill draw_bg +: {color: stage.well}}}
                    b: View{width: Fill height: Fill
                        // Its own draw list: what is drawn over it (the readouts,
                        // the found line) changes every frame; the code does not.
                        code_box := View{width: Fill height: Fill new_batch: true
                            code := CodeView{
                                editor +: {
                                    width: Fill height: Fill
                                    read_only: false show_gutter: true word_wrap: false
                                    // Its ground is drawn under it.
                                    draw_bg +: {color: #0000}
                                    draw_text +: {text_style: theme.font_code{font_size: 9}}
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// Room at the header's left for the pane's Code | Curves switch.
const LEAD: f64 = 132.0;

#[derive(Script, ScriptHook, Widget)]
pub struct MotionCodeTimeline {
    #[deref]
    view: View,
    #[live]
    draw_block: DrawEditBlock,
    #[live]
    draw_solid: DrawColor,
    #[live]
    draw_text: DrawText,
    #[live]
    draw_small: DrawText,
    #[live]
    draw_film: DrawImage,
    /// Shared, so a frame reads them without copying.
    #[rust]
    docs: Vec<std::rc::Rc<CodeDoc>>,
    #[rust]
    active: usize,
    /// Which text the code view holds (doc, its text, the generation).
    #[rust]
    shown: Option<(usize, String, u64)>,
    /// Bumped when what is shown changes (text, tab, folds asked for).
    #[rust]
    gen: u64,
    #[rust]
    code_rev: u64,
    #[rust]
    code_drawn: Option<(usize, u64, u64)>,
    #[rust]
    editor_read_only: bool,
    #[rust]
    folds_of: Option<u64>,
    #[rust]
    folded_of: Option<(u64, std::rc::Rc<Vec<usize>>)>,
    /// What the code folds to: `Bound` (what the scene's panel binds),
    /// `Moving` (what moves in time), `All`.
    #[rust]
    fold_mode: FoldMode,
    /// The byte ranges the scene's editor panel binds: (doc, bytes).
    #[rust]
    bindings: Vec<Binding>,
    /// The line a jump went to: it stays unfolded.
    #[rust]
    keep: Option<(usize, usize)>,
    #[rust]
    reveal: Option<(usize, usize)>,
    #[rust]
    reveal_wait: u32,
    #[rust]
    reveal_select: Option<std::ops::Range<usize>>,
    #[rust]
    nav_restore: Option<DVec2>,
    #[rust]
    nav: Vec<NavSpot>,
    #[rust]
    nav_at: usize,
    #[rust]
    found_at: f64,
    #[rust]
    found_frame: NextFrame,
    /// The pick: what was drawn, its stack trace, the frame gone to.
    #[rust]
    trace: Option<(String, Vec<TraceFrame>, usize)>,
    /// Where the pick's text comes from: (doc, its words' bytes, said).
    #[rust]
    origin: Option<(usize, std::ops::Range<usize>, String)>,
    #[rust]
    pane_scroll: f64,
    #[rust]
    pane_content: f64,
    #[rust]
    pane_drag: Option<(f64, f64)>,
    #[rust]
    hots: Vec<(Rect, Hot)>,
    #[rust]
    gestures: u64,
    /// The list of files is open (the file chip pressed).
    #[rust]
    file_menu: bool,
    #[rust]
    typing_gesture: u64,
    #[rust]
    typed_at: f64,
    #[rust]
    selected: Option<(usize, usize)>,
    #[rust]
    duration: f64,
    #[rust]
    playhead: f64,
    #[rust]
    playing: bool,
    /// The graphic along its time, small (`filmstrip`).
    #[rust]
    film: Filmstrip,
    /// The scrubber's strip, and a scrub under way.
    #[rust]
    strip: Rect,
    #[rust]
    scrubbing: bool,
    /// What acts where on the film (the scene's editor panel's controls),
    /// and the one lit (hovered or selected in the panel).
    #[rust]
    spans: Vec<ScrubSpans>,
    #[rust]
    span_focus: Option<String>,
    /// The curve lanes shown, a key being dragged, the key selected, the
    /// last click on a lane (for a double click), the lanes' rect.
    #[rust]
    lanes: Vec<Lane>,
    #[rust]
    key_drag: Option<KeyDrag>,
    #[rust]
    key_sel: Option<(usize, usize)>,
    #[rust]
    lane_click: Option<(f64, usize)>,
    #[rust]
    lanes_rect: Rect,
    #[rust]
    code_rect: Rect,
    #[rust]
    gather_rect: Rect,
}

/// What the code folds to: none by default (the whole code, readable);
/// the bar's chip folds it to what the panel binds or to what moves.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
enum FoldMode {
    Bound,
    Moving,
    #[default]
    All,
}

fn color(value: u32, alpha: f32) -> Vec4f {
    let mut color = Vec4f::from_u32(value);
    color.w *= alpha;
    color
}

fn line_of(text: &str, at: usize) -> usize {
    text[..at.min(text.len())].matches('\n').count()
}

impl MotionCodeTimeline {
    pub fn set_docs(&mut self, cx: &mut Cx, mut docs: Vec<CodeDoc>) {
        if self.active >= docs.len() {
            self.active = 0;
        }
        for doc in &mut docs {
            doc.lines = std::iter::once(0).chain(doc.text.match_indices('\n').map(|(i, _)| i + 1)).collect();
        }
        self.docs = docs.into_iter().map(std::rc::Rc::new).collect();
        self.touch();
        self.redraw_over(cx);
    }

    /// What the scene's editor panel binds, each path resolved in the
    /// current text (doc 0 the graphic, `k + 1` module `k`): the code folds
    /// to their lines. Between resolutions each range is a live anchor:
    /// typed text and changes put in from outside move it.
    pub fn set_bindings(&mut self, cx: &mut Cx, bindings: Vec<Binding>) {
        if self.bindings != bindings {
            self.bindings = bindings;
            self.touch();
            self.redraw_over(cx);
        }
    }

    /// The code view's text typed into: the change (one splice from the
    /// text it showed) written to the document. A pause of over a second
    /// starts a new undo step.
    fn take_typed(&mut self, cx: &mut Cx, uid: WidgetUid) {
        let Some((doc, old, gen)) = self.shown.clone() else { return };
        if doc != self.active {
            return;
        }
        let code = self.view.widget(cx, ids!(code));
        let Some(now) = code.borrow::<CodeView>().and_then(|v| v.session.as_ref().map(|s| s.document().as_text().to_string())) else { return };
        if now == old {
            return;
        }
        let p = old.bytes().zip(now.bytes()).take_while(|(a, b)| a == b).count();
        let mut p = p.min(old.len()).min(now.len());
        while !old.is_char_boundary(p) || !now.is_char_boundary(p) {
            p -= 1;
        }
        let room = old.len().min(now.len()) - p;
        let mut q = old.bytes().rev().zip(now.bytes().rev()).take(room).take_while(|(a, b)| a == b).count();
        while !old.is_char_boundary(old.len() - q) || !now.is_char_boundary(now.len() - q) {
            q -= 1;
        }
        let t = Cx::time_now();
        if t - self.typed_at > 1.0 {
            self.gestures += 1;
            self.typing_gesture = self.gestures;
        }
        self.typed_at = t;
        let edit = Edit::splice(p..old.len() - q, now[p..now.len() - q].to_string(), self.typing_gesture, true);
        let delta = (now.len() - q - p) as isize - (old.len() - q - p) as isize;
        self.shift_refs(doc, old.len() - q, delta);
        self.shown = Some((doc, now, gen));
        cx.widget_action(uid, CodeTimelineAction::Edits { doc, edits: vec![edit] });
    }

    /// Document `doc`'s text grew by `delta` bytes at `at`: the places the
    /// pick, the kept line, the bound ranges and the history point to after
    /// it move with it.
    fn shift_refs(&mut self, doc: usize, at: usize, delta: isize) {
        if delta == 0 {
            return;
        }
        let mv = |b: usize| if b >= at { (b as isize + delta).max(at as isize) as usize } else { b };
        if let Some((_, frames, _)) = &mut self.trace {
            for f in frames.iter_mut().filter(|f| f.doc == doc) {
                f.at = mv(f.at);
            }
        }
        if let Some((d, range, _)) = &mut self.origin {
            if *d == doc {
                *range = mv(range.start)..mv(range.end);
            }
        }
        for b in self.bindings.iter_mut().filter(|b| b.doc == doc) {
            b.range = mv(b.range.start)..mv(b.range.end);
        }
        if let Some((d, b)) = self.keep {
            if d == doc {
                self.keep = Some((d, mv(b)));
            }
        }
        for spot in self.nav.iter_mut().filter(|s| s.doc == doc) {
            spot.at = mv(spot.at);
        }
    }

    /// The pick's stack trace (empty: none), and where its text comes from.
    pub fn set_trace(&mut self, cx: &mut Cx, drawn: String, frames: Vec<TraceFrame>, current: usize, origin: Option<(usize, std::ops::Range<usize>, String)>) {
        self.trace = (!frames.is_empty()).then_some((drawn, frames, current));
        self.origin = origin;
        self.pane_scroll = 0.0;
        self.redraw_over(cx);
    }

    /// What is drawn over and beside the code draws again. The code keeps
    /// its own draw list and draws again only when it changed.
    fn redraw_over(&mut self, cx: &mut Cx) {
        self.view.area().redraw(cx);
    }

    fn touch(&mut self) {
        self.gen += 1;
    }

    /// The values at the playhead, per document.
    pub fn set_now(&mut self, cx: &mut Cx, live: Vec<Vec<(usize, String)>>) {
        let mut changed = false;
        for (doc, live) in self.docs.iter_mut().zip(live) {
            if doc.live != live {
                std::rc::Rc::make_mut(doc).live = live;
                changed = true;
            }
        }
        if changed {
            self.redraw_over(cx);
        }
    }

    /// What the scrubber's filmstrip is made from (`None`: the graphic is
    /// not placed in the edit, no strip).
    pub fn set_film(&mut self, cx: &mut Cx, source: Option<FilmSource>) {
        self.film.set_source(cx, source);
        self.redraw_over(cx);
    }

    pub fn set_time(&mut self, cx: &mut Cx, duration: f64, playhead: f64, playing: bool) {
        if (self.duration - duration).abs() > 1e-9 || (self.playhead - playhead).abs() > 1e-9 || self.playing != playing {
            self.duration = duration.max(0.1);
            self.playhead = playhead;
            self.playing = playing;
            self.redraw_over(cx);
        }
    }

    /// Where each of the scene panel's controls (or modules) acts on the
    /// film, shown on the scrubber as blocks; a click on one puts the
    /// playhead at its start.
    pub fn set_spans(&mut self, cx: &mut Cx, spans: Vec<ScrubSpans>) {
        if self.spans != spans {
            self.spans = spans;
            self.redraw_over(cx);
        }
    }

    /// The curve lanes to show (the desk reads their keys from the code
    /// each time it changes). With none the lanes fold away; opened, they
    /// take room from the code.
    pub fn set_lanes(&mut self, cx: &mut Cx, lanes: Vec<Lane>) {
        if self.lanes != lanes {
            let opened = self.lanes.len() != lanes.len();
            self.lanes = lanes;
            if self.key_sel.is_some_and(|(l, k)| self.lanes.get(l).is_none_or(|lane| k >= lane.keys.keys.len())) {
                self.key_sel = None;
            }
            if opened {
                if let Some(mut split) = self.view.widget(cx, ids!(split_lanes)).borrow_mut::<Splitter>() {
                    if self.lanes.is_empty() {
                        split.set_collapse(cx, SplitterCollapse::A);
                    } else {
                        split.align = SplitterAlign::FromA(8.0 + 64.0 * self.lanes.len().min(4) as f64);
                        split.set_collapse(cx, SplitterCollapse::None);
                    }
                }
            }
            self.redraw_over(cx);
        }
    }

    /// The curve lanes: each one's label and close mark at its left, its
    /// keys over the scrubber's time axis joined by their eased curve, the
    /// ease of each segment a small mark, the playhead through them.
    fn draw_lanes(&mut self, cx: &mut Cx2d, p: &makepad_look::palette::Palette) {
        let area = self.view.widget(cx, ids!(lanes)).area().rect(cx);
        self.lanes_rect = area;
        if area.size.y < 8.0 || self.lanes.is_empty() {
            return;
        }
        cx.begin_turtle(Walk::abs_rect(area), Layout::default().with_clip(true, true));
        let n = self.lanes.len();
        let h = (area.size.y / n as f64).max(24.0);
        let lanes = self.lanes.clone();
        for (li, lane) in lanes.iter().enumerate() {
            let row = Rect { pos: dvec2(area.pos.x, area.pos.y + li as f64 * h), size: dvec2(area.size.x, h) };
            self.rect(cx, Rect { pos: dvec2(row.pos.x, row.pos.y + row.size.y - 1.0), size: dvec2(row.size.x, 1.0) }, color(p.wave_rim, 0.6));
            let close = self.chip(cx, row.pos.x + 6.0, row.pos.y + 4.0, "×", false, p);
            self.hots.push((close, Hot::LaneClose(li)));
            self.draw_small.color = color(p.label, 1.0);
            let label: String = lane.label.chars().take(28).collect();
            self.draw_small.draw_abs(cx, dvec2(close.pos.x + close.size.x + 6.0, row.pos.y + 9.0), &label);
            let plot = Rect { pos: dvec2(self.strip.pos.x, row.pos.y + 6.0), size: dvec2(self.strip.size.x, row.size.y - 12.0) };
            self.hots.push((plot, Hot::Lane(li)));
            // The lane's value range (a little room above and below).
            let vals: Vec<f64> = lane.keys.keys.iter().filter_map(|k| k.v).collect();
            let (lo, hi) = vals.iter().fold((f64::MAX, f64::MIN), |(a, b), v| (a.min(*v), b.max(*v)));
            let (lo, hi) = if vals.is_empty() { (0.0, 1.0) } else if (hi - lo).abs() < 1e-9 { (lo - 1.0, hi + 1.0) } else { (lo - (hi - lo) * 0.1, hi + (hi - lo) * 0.1) };
            let y_of = |v: f64| plot.pos.y + plot.size.y - (v - lo) / (hi - lo) * plot.size.y;
            // The eased curve between the keys.
            let mut prev: Option<DVec2> = None;
            let steps = (plot.size.x / 3.0).max(8.0) as usize;
            for s in 0..=steps {
                let t = s as f64 / steps as f64 * self.duration;
                let Some(v) = makepad_motion::lane::value_at(&lane.keys, t) else { break };
                let pt = dvec2(self.x_of(t), y_of(v));
                if let Some(a) = prev {
                    let r = Rect { pos: dvec2(a.x, a.y.min(pt.y) - 0.75), size: dvec2((pt.x - a.x).max(1.0), (pt.y - a.y).abs() + 1.5) };
                    self.rect(cx, r, color(p.accent, 0.55));
                }
                prev = Some(pt);
            }
            // The keys, and the ease into each.
            for (ki, key) in lane.keys.keys.iter().enumerate() {
                let (Some(t), Some(v)) = (key.t, key.v) else { continue };
                let c = dvec2(self.x_of(t), y_of(v));
                let sel = self.key_sel == Some((li, ki));
                self.draw_block.fill = color(if sel { p.accent } else { p.glass_ink }, 1.0);
                self.draw_block.edge = color(p.accent, 1.0);
                self.draw_block.stroke = 1.0;
                self.draw_block.radius = 0.0;
                self.draw_block.shape = 1.0;
                let r = Rect { pos: c - dvec2(5.0, 5.0), size: dvec2(10.0, 10.0) };
                self.draw_block.draw_abs(cx, r);
                self.hots.insert(0, (Rect { pos: r.pos - dvec2(3.0, 3.0), size: r.size + dvec2(6.0, 6.0) }, Hot::LaneKey(li, ki)));
                if ki > 0 {
                    let pa = lane.keys.keys[ki - 1].t.map(|ta| self.x_of(ta));
                    if let Some(xa) = pa {
                        let name = key.ease.as_ref().map_or("linear".to_string(), |(_, e)| e.trim_start_matches('@').replace("ease_", ""));
                        let w = advance(&self.draw_small, cx, &name) + 6.0;
                        let mid = Rect { pos: dvec2((xa + c.x) * 0.5 - w * 0.5, plot.pos.y + plot.size.y - 11.0), size: dvec2(w, 12.0) };
                        self.draw_small.color = color(p.dim, 1.0);
                        self.draw_small.draw_abs(cx, dvec2(mid.pos.x + 3.0, mid.pos.y), &name);
                        self.hots.insert(0, (mid, Hot::LaneEase(li, ki)));
                    }
                }
            }
        }
        let px = self.x_of(self.playhead);
        self.rect(cx, Rect { pos: dvec2(px - 0.5, area.pos.y), size: dvec2(1.0, area.size.y) }, color(p.accent, 0.8));
        cx.end_turtle();
    }

    /// The splices of a lane's file that write `t` and `v` over a dragged
    /// key's time and value (`done`: the release).
    fn move_key(&mut self, cx: &mut Cx, uid: WidgetUid, at: DVec2, free: bool, done: bool) {
        let Some(mut drag) = self.key_drag.take() else { return };
        let Some(lane) = self.lanes.get(drag.lane).cloned() else { return };
        let d = at - drag.from;
        let mut edits = Vec::new();
        if d.x.abs() > 2.0 || d.y.abs() > 2.0 {
            drag.moved = true;
        }
        if drag.moved {
            let keys = &lane.keys.keys;
            if let Some(t_now) = drag.t_now.clone() {
                let px_per_sec = (self.strip.size.x - 8.0).max(1.0) / self.duration.max(0.1);
                let mut t = drag.t0 + d.x / px_per_sec;
                // The song's beats catch it (shift: free).
                if !free {
                    if let Some(b) = self.beats().into_iter().min_by(|a, b| (a - t).abs().total_cmp(&(b - t).abs())).filter(|b| (b - t).abs() * px_per_sec < 6.0) {
                        t = b;
                    }
                }
                let lo = drag.key.checked_sub(1).and_then(|k| keys.get(k)?.t).map_or(0.0, |x| x + 1e-3);
                let hi = keys.get(drag.key + 1).and_then(|k| k.t).map_or(self.duration, |x| x - 1e-3);
                let text = makepad_motion::patch::fmt_num((t.clamp(lo, hi.max(lo)) * 100.0).round() / 100.0);
                let grow = text.len() as isize - t_now.len() as isize;
                edits.push(Edit::splice(t_now.start..t_now.end, text.clone(), drag.gesture, done));
                drag.t_now = Some(t_now.start..t_now.start + text.len());
                drag.v_now = (drag.v_now.start as isize + grow) as usize..(drag.v_now.end as isize + grow) as usize;
            }
            let v = drag.v0 - d.y / drag.height.max(1.0) * (drag.hi - drag.lo);
            let text = makepad_motion::patch::fmt_num((v * 1000.0).round() / 1000.0);
            edits.push(Edit::splice(drag.v_now.start..drag.v_now.end, text.clone(), drag.gesture, done));
            drag.v_now = drag.v_now.start..drag.v_now.start + text.len();
            cx.widget_action(uid, CodeTimelineAction::Edits { doc: lane.doc, edits });
        }
        if !done {
            self.key_drag = Some(drag);
        }
        self.redraw_over(cx);
    }

    /// The control whose spans are lit (hovered or selected in the panel);
    /// `None`: none, all shown faintly.
    pub fn set_span_focus(&mut self, cx: &mut Cx, key: Option<String>) {
        if self.span_focus != key {
            self.span_focus = key;
            self.redraw_over(cx);
        }
    }

    /// Show the code at byte `at` of document `doc`: its tab, unfolded to
    /// its line, the caret on it, scrolled into view (a jump in the
    /// history).
    pub fn reveal(&mut self, cx: &mut Cx, doc: usize, at: usize) {
        if doc >= self.docs.len() {
            return;
        }
        self.push_nav(cx, NavSpot { doc, at, scroll: None });
        self.reveal_to(cx, doc, at);
    }

    /// A value changed from elsewhere (the panel): its line brought into
    /// view and lit a moment, without a jump in the history.
    pub fn show_edit(&mut self, cx: &mut Cx, doc: usize, at: usize) {
        self.reveal_to(cx, doc, at);
    }

    /// A jump to a string's words: they are selected, ready to type over.
    pub fn reveal_words(&mut self, cx: &mut Cx, doc: usize, range: std::ops::Range<usize>) {
        self.reveal(cx, doc, range.start);
        self.reveal_select = Some(range);
    }

    /// Where the code view is now: its document, cursor and scroll.
    fn here(&self, cx: &mut Cx) -> Option<NavSpot> {
        let code = self.view.widget(cx, ids!(code));
        let view = code.borrow::<CodeView>()?;
        let session = view.session.as_ref()?;
        let p = session.selections().last()?.cursor.position;
        let text = session.document().as_text().to_string();
        let at = text.split('\n').take(p.line_index).map(|l| l.len() + 1).sum::<usize>() + p.byte_index;
        Some(NavSpot { doc: self.active, at, scroll: Some(view.editor.scroll_pos()) })
    }

    /// A jump: where the view was, then where it goes, at the history's
    /// head (what was ahead of the walk is dropped).
    fn push_nav(&mut self, cx: &mut Cx, to: NavSpot) {
        if !self.nav.is_empty() {
            self.nav.truncate(self.nav_at + 1);
        }
        if let Some(here) = self.here(cx) {
            match self.nav.last_mut() {
                Some(last) if last.doc == here.doc && last.at == here.at => last.scroll = here.scroll,
                _ => self.nav.push(here),
            }
        }
        if self.nav.last().is_none_or(|l| l.doc != to.doc || l.at != to.at) {
            self.nav.push(to);
        }
        if self.nav.len() > 64 {
            self.nav.remove(0);
        }
        self.nav_at = self.nav.len() - 1;
    }

    /// Back (`-1`) or forward (`1`) through the jumps: the view goes back
    /// to where it was (scroll and cursor).
    fn nav_step(&mut self, cx: &mut Cx, step: isize) {
        let to = self.nav_at as isize + step;
        if to < 0 || to as usize >= self.nav.len() {
            return;
        }
        self.nav_go(cx, to as usize);
    }

    fn nav_go(&mut self, cx: &mut Cx, k: usize) {
        if k >= self.nav.len() {
            return;
        }
        if let Some(here) = self.here(cx) {
            if let Some(cur) = self.nav.get_mut(self.nav_at) {
                if cur.doc == here.doc {
                    *cur = here;
                }
            }
        }
        self.nav_at = k;
        let spot = self.nav[k];
        self.nav_restore = spot.scroll;
        self.reveal_to(cx, spot.doc, spot.at);
        self.redraw_over(cx);
    }

    /// The name under byte `at` of document `doc` and where it is defined:
    /// a `let` or function in this file (its own function's first), or,
    /// read off a module (`song.lines`), in that module's file.
    fn definition(&self, doc: usize, at: usize) -> Option<(usize, usize)> {
        let d = self.docs.get(doc)?;
        let bytes = d.text.as_bytes();
        let is_id = |c: u8| c.is_ascii_alphanumeric() || c == b'_';
        if at > bytes.len() {
            return None;
        }
        let mut a = at.min(bytes.len());
        while a > 0 && is_id(bytes[a - 1]) {
            a -= 1;
        }
        let mut b = at;
        while b < bytes.len() && is_id(bytes[b]) {
            b += 1;
        }
        if a == b {
            return None;
        }
        let name = &d.text[a..b];
        if a > 1 && bytes[a - 1] == b'.' {
            let mut c = a - 1;
            while c > 0 && is_id(bytes[c - 1]) {
                c -= 1;
            }
            let owner = &d.text[c..a - 1];
            if let Some(m) = self.docs.iter().position(|x| x.title == owner) {
                let md = &self.docs[m];
                if let Some(r) = md.code.regions.iter().find(|r| r.parent.is_none() && r.name == name) {
                    return Some((m, r.range.start));
                }
                return Some((m, 0));
            }
        }
        if let Some(m) = self.docs.iter().position(|x| x.title == name) {
            if m != doc {
                return Some((m, 0));
            }
        }
        let regions = &d.code.regions;
        let enclosing_fn = |i: usize| {
            let mut p = regions[i].parent;
            while let Some(x) = p {
                if regions[x].kind == RegionKind::Fn {
                    return Some(x);
                }
                p = regions[x].parent;
            }
            None
        };
        let here_fn = d.code.region_at(at).and_then(|r| if regions[r].kind == RegionKind::Fn { Some(r) } else { enclosing_fn(r) });
        let lets: Vec<usize> = (0..regions.len()).filter(|&i| matches!(regions[i].kind, RegionKind::Let | RegionKind::Fn) && regions[i].name == name && !(regions[i].range.start <= at && at < regions[i].range.start + name.len() + 4)).collect();
        let own = lets.iter().copied().filter(|&i| here_fn.is_some() && enclosing_fn(i) == here_fn && regions[i].range.start < at).max_by_key(|&i| regions[i].range.start);
        let top = lets.iter().copied().find(|&i| regions[i].parent.is_none());
        if let Some(i) = own.or(top).or(lets.first().copied()) {
            return Some((doc, regions[i].range.start));
        }
        self.docs.iter().enumerate().filter(|(m, _)| *m != doc).find_map(|(m, md)| {
            md.code.regions.iter().find(|r| r.parent.is_none() && r.name == name).map(|r| (m, r.range.start))
        })
    }

    fn reveal_to(&mut self, cx: &mut Cx, doc: usize, at: usize) {
        if doc >= self.docs.len() {
            return;
        }
        if self.active != doc {
            self.active = doc;
        }
        self.reveal = Some((doc, at));
        self.keep = Some((doc, at));
        self.found_at = Cx::time_now();
        self.touch();
        self.redraw_over(cx);
    }

    /// The lines folded away in the shown document (made once per
    /// generation): around what the panel binds (each bound line and the
    /// first line of every block holding it), else around what moves in
    /// time, else none.
    fn folded(&mut self) -> std::rc::Rc<Vec<usize>> {
        if let Some((gen, folded)) = &self.folded_of {
            if *gen == self.gen {
                return folded.clone();
            }
        }
        let folded = std::rc::Rc::new(self.make_folded());
        self.folded_of = Some((self.gen, folded.clone()));
        folded
    }

    fn make_folded(&self) -> Vec<usize> {
        let Some(doc) = self.docs.get(self.active) else { return Vec::new() };
        let total = doc.text.lines().count().max(1);
        let mut shows = vec![false; total];
        let mut mark = |line: usize| {
            if line < total {
                shows[line] = true;
            }
        };
        // The first line of every block holding byte `at`, and `at`'s own
        // lines: what a bound value needs around it to be read.
        let with_heads = |at: std::ops::Range<usize>, mark: &mut dyn FnMut(usize)| {
            for l in doc.line(at.start)..=doc.line(at.end.saturating_sub(1).max(at.start)) {
                mark(l);
            }
            let mut r = doc.code.region_at(at.start);
            while let Some(i) = r {
                mark(doc.line(doc.code.regions[i].range.start));
                r = doc.code.regions[i].parent;
            }
        };
        let bound: Vec<std::ops::Range<usize>> = self.bindings.iter().filter(|b| b.doc == self.active).map(|b| b.range.clone()).collect();
        match self.fold_mode {
            FoldMode::All => return Vec::new(),
            FoldMode::Bound if !bound.is_empty() => {
                for r in bound {
                    with_heads(r, &mut mark);
                }
            }
            // Nothing bound: all of it.
            FoldMode::Bound => return Vec::new(),
            // What moves in time (the graphic's; a module is read whole):
            // values measured to change over the film, keys and tweens.
            FoldMode::Moving if self.active == 0 => {
                let moving: Vec<usize> = doc.series.iter().filter(|(_, v)| {
                    let (lo, hi) = v.iter().filter(|x| x.is_finite()).fold((f32::MAX, f32::MIN), |(lo, hi), x| (lo.min(*x), hi.max(*x)));
                    hi - lo > 1e-6 * hi.abs().max(1.0)
                }).map(|(b, _)| *b).chain(doc.paths.iter().filter(|(_, v, _)| v.windows(2).any(|w| w[0] != w[1] && w[0][0].is_finite() && w[1][0].is_finite())).map(|(b, _, _)| *b)).collect();
                if moving.is_empty() {
                    return Vec::new();
                }
                for b in moving {
                    with_heads(b..b + 1, &mut mark);
                }
                for t in &doc.code.times {
                    with_heads(t.range.clone(), &mut mark);
                }
            }
            _ => return Vec::new(),
        }
        // The line a jump went to stays open, and the pick's frames.
        if let Some((_, at)) = self.keep.filter(|(d, _)| *d == self.active) {
            with_heads(at..at + 1, &mut mark);
        }
        if let Some((_, frames, _)) = &self.trace {
            for f in frames.iter().filter(|f| f.doc == self.active) {
                with_heads(f.at..f.at + 1, &mut mark);
            }
        }
        (0..total).filter(|&l| !shows[l]).collect()
    }

    /// Put the active document's text in the code view, folded as the view
    /// says, and bring a revealed place into view.
    fn sync_code(&mut self, cx: &mut Cx) {
        let Some(doc) = self.docs.get(self.active) else { return };
        let text = doc.text.clone();
        let code = self.view.widget(cx, ids!(code));
        let Some(mut view) = code.borrow_mut::<CodeView>() else { return };
        let gen = self.gen;
        let read_only = doc.read_only;
        if self.editor_read_only != read_only {
            self.editor_read_only = read_only;
            view.editor.set_read_only(cx, read_only);
        }
        let apply = self.shown.as_ref().map(|(d, _, g)| (*d, *g)) != Some((self.active, gen)) && self.shown.as_ref().is_none_or(|(d, t, _)| *d != self.active || *t != text);
        if apply {
            match (&self.shown, view.session.as_mut()) {
                // The same document edited: the change goes into the text
                // in place (its folds and scroll stay put).
                (Some((shown_doc, old, _)), Some(session)) if *shown_doc == self.active => {
                    let p = old.bytes().zip(text.bytes()).take_while(|(a, b)| a == b).count();
                    let mut p = p.min(old.len()).min(text.len());
                    while !old.is_char_boundary(p) || !text.is_char_boundary(p) {
                        p -= 1;
                    }
                    let room = old.len().min(text.len()) - p;
                    let mut q = old.bytes().rev().zip(text.bytes().rev()).take(room).take_while(|(a, b)| a == b).count();
                    while !old.is_char_boundary(old.len() - q) || !text.is_char_boundary(text.len() - q) {
                        q -= 1;
                    }
                    let pos = |at: usize| {
                        let before = &old[..at];
                        Position { line_index: before.matches('\n').count(), byte_index: at - before.rfind('\n').map_or(0, |nl| nl + 1) }
                    };
                    // As given: typing's insert would autoindent the
                    // changed lines and the view would show (and could
                    // hand back) a re-indented text.
                    makepad_value_pickers::panel::host::timed("code.replace_range", || session.document().replace_range(pos(p), pos(old.len() - q), text[p..text.len() - q].into()));
                    makepad_value_pickers::panel::host::timed("code.handle_changes", || session.handle_changes());
                    self.code_rev += 1;
                    let delta = (text.len() - q - p) as isize - (old.len() - q - p) as isize;
                    self.shift_refs(self.active, old.len() - q, delta);
                }
                _ => {
                    makepad_value_pickers::panel::host::time_add("code.new_session", 0.0001);
                    let prepared = CodeDocument::prepare_for_path(if doc.file == CodeFile::Doc { "graphic.splash" } else { "module.splash" }, text.as_str().into());
                    view.session = Some(CodeSession::new(CodeDocument::from_prepared(prepared)));
                    self.folds_of = None;
                }
            }
        }
        // What the view holds now: the document's text when it was put in;
        // else what it held (typed text the document has not taken yet).
        let held = if apply { text.clone() } else { self.shown.as_ref().filter(|(d, _, _)| *d == self.active).map_or_else(|| text.clone(), |(_, t, _)| t.clone()) };
        self.shown = Some((self.active, held, gen));
        drop(view);
        let folded = makepad_value_pickers::panel::host::timed("code.folded", || self.folded());
        let code = self.view.widget(cx, ids!(code));
        let Some(mut view) = code.borrow_mut::<CodeView>() else { return };
        if self.folds_of != Some(gen) {
            self.folds_of = Some(gen);
            if let Some(session) = view.session.as_mut() {
                let count = session.document().as_text().as_lines().len();
                let unfold: Vec<usize> = (0..count).filter(|l| session.is_folded(*l) && folded.binary_search(l).is_err()).collect();
                let fold: Vec<usize> = folded.iter().copied().filter(|l| !session.is_folded(*l)).collect();
                if self.reveal.is_some() {
                    // Going to a line: the folds move at once, so it is
                    // where it will stay when the view goes there.
                    session.set_folds_now(&fold, &unfold);
                    self.reveal_wait = 2;
                } else {
                    if !unfold.is_empty() {
                        session.unfold_lines(&unfold);
                    }
                    if !fold.is_empty() {
                        session.fold_lines(&fold, 0);
                    }
                }
            }
        }
        // A place to bring into view, once laid out (the folds have moved).
        let laid_out = self.reveal_wait == 0;
        self.reveal_wait = self.reveal_wait.saturating_sub(1);
        let settled = laid_out && view.session.as_ref().is_some_and(|session| !session.folds_animating());
        let again = self.reveal.is_some() && !settled;
        if let Some((doc, at)) = if settled { self.reveal.take() } else { None } {
            if doc == self.active {
                let line = line_of(&text, at);
                let CodeView { editor, session, .. } = &mut *view;
                if let Some(session) = session {
                    let col = at - text[..at.min(text.len())].rfind('\n').map_or(0, |n| n + 1);
                    if let Some(scroll) = self.nav_restore.take() {
                        use makepad_code_editor::history::NewGroup;
                        use makepad_code_editor::selection::Affinity;
                        use makepad_code_editor::session::SelectionMode;
                        session.set_selection(Position { line_index: line, byte_index: col }, Affinity::Before, SelectionMode::Simple, NewGroup::Yes);
                        editor.set_scroll_pos(cx, scroll);
                    } else if let Some(range) = self.reveal_select.take().filter(|r| r.start == at) {
                        // A string's words, selected (inside its quotes).
                        let inner = text.get(range.clone()).map_or(range.clone(), |t| {
                            let open = t.find('"').map_or(0, |i| i + 1);
                            let close = t.rfind('"').filter(|c| *c >= open).unwrap_or(t.len());
                            range.start + open..range.start + close
                        });
                        let pos = |b: usize| Position { line_index: line_of(&text, b), byte_index: b - text[..b.min(text.len())].rfind('\n').map_or(0, |n| n + 1) };
                        editor.set_selection_and_scroll(cx, pos(inner.start), pos(inner.end), session);
                        editor.set_key_focus(cx);
                    } else {
                        editor.set_cursor_and_center(cx, Position { line_index: line, byte_index: col }, session);
                    }
                    // (This runs in a draw: a plain redraw would be dropped.)
                    cx.redraw_area_in_draw(editor.area());
                    self.found_at = Cx::time_now();
                }
            }
        }
        drop(view);
        let drawn = (self.active, gen, self.code_rev);
        if self.code_drawn != Some(drawn) || again {
            self.code_drawn = Some(drawn);
            cx.redraw_area_in_draw(code.area());
        }
        if again {
            cx.redraw_area_in_draw(self.view.area());
        }
    }

    /// The song's beats on the graphic's clock: the rising list of times
    /// within the film that the graphic's code computed with the most of
    /// them (its `beats`).
    fn beats(&self) -> Vec<f64> {
        let Some(doc) = self.docs.first() else { return Vec::new() };
        let within = |t: &f64| (0.0..=self.duration + 1e-9).contains(t);
        doc.bound.iter().map(|(_, _, w)| w.ticks.iter().copied().filter(within).collect::<Vec<f64>>()).filter(|t| t.len() >= 4).max_by_key(|t| t.len()).unwrap_or_default()
    }

    /// The pick: what was drawn, the stack of code that drew it (innermost
    /// first), and where its text comes from; each a jump.
    fn draw_pick(&mut self, cx: &mut Cx2d, p: &makepad_look::palette::Palette) {
        let pane = self.view.widget(cx, ids!(gather_pane)).area().rect(cx);
        self.gather_rect = pane;
        if pane.size.x < 20.0 {
            return;
        }
        cx.begin_turtle(Walk::abs_rect(pane), Layout::default().with_clip(true, true));
        let hots_before = self.hots.len();
        let mut y = pane.pos.y + 8.0 - self.pane_scroll;
        let Some((drawn, frames, current)) = self.trace.clone() else {
            self.draw_small.color = color(p.dim, 1.0);
            self.draw_small.draw_abs(cx, dvec2(pane.pos.x + 10.0, y), "SELECTION · click the picture (shift adds)");
            cx.end_turtle();
            return;
        };
        self.draw_small.color = color(p.wave_muted, 1.0);
        self.draw_small.draw_abs(cx, dvec2(pane.pos.x + 10.0, y), "SELECTION");
        y += 15.0;
        let drawn: String = if drawn.chars().count() > 44 { format!("{}…", drawn.chars().take(43).collect::<String>()) } else { drawn };
        self.draw_small.color = color(p.text_hi, 1.0);
        self.draw_small.draw_abs(cx, dvec2(pane.pos.x + 10.0, y), &drawn);
        y += 18.0;
        // Where its text comes from (a lyric's word in the song's timing).
        if let Some((_, _, said)) = self.origin.clone() {
            let r = Rect { pos: dvec2(pane.pos.x + 4.0, y - 2.0), size: dvec2(pane.size.x - 8.0, 16.0) };
            self.draw_small.color = color(p.accent, 1.0);
            self.draw_small.draw_abs(cx, dvec2(r.pos.x + 8.0, y), &format!("text {said}"));
            self.hots.push((r, Hot::Origin));
            y += 20.0;
        }
        for (k, f) in frames.iter().enumerate() {
            // The frame gone to, with its call; the others one line each.
            let tall = f.frame == current;
            let r = Rect { pos: dvec2(pane.pos.x + 4.0, y - 2.0), size: dvec2(pane.size.x - 8.0, if tall { 30.0 } else { 16.0 }) };
            if tall {
                self.rect(cx, r, color(p.accent, 0.14));
                self.rect(cx, Rect { pos: r.pos, size: dvec2(3.0, r.size.y) }, color(p.accent, 1.0));
            }
            self.draw_small.color = color(if tall { p.accent } else { p.label }, 1.0);
            self.draw_small.draw_abs(cx, dvec2(r.pos.x + 8.0, y), &format!("{}{}", if k == 0 { "" } else { "← " }, f.place));
            if tall {
                self.draw_small.color = color(p.dim, 1.0);
                self.draw_small.draw_abs(cx, dvec2(r.pos.x + 20.0, y + 13.0), &f.call);
            }
            self.hots.push((r, Hot::Trace(k)));
            y += if tall { 32.0 } else { 17.0 };
        }
        // Its scroll: a thumb when it runs past the pane.
        let content = y + self.pane_scroll - pane.pos.y + 8.0;
        self.pane_content = content;
        let max = (content - pane.size.y).max(0.0);
        if self.pane_scroll > max {
            self.pane_scroll = max;
        }
        if content > pane.size.y + 1.0 {
            let h = (pane.size.y * pane.size.y / content).max(24.0);
            let top = pane.pos.y + (pane.size.y - h) * (self.pane_scroll / max.max(1.0));
            let thumb = Rect { pos: dvec2(pane.pos.x + pane.size.x - 7.0, top), size: dvec2(5.0, h) };
            self.draw_block.fill = color(p.dim, 0.8);
            self.draw_block.edge = color(p.dim, 0.0);
            self.draw_block.stroke = 0.0;
            self.draw_block.radius = 2.5;
            self.draw_block.shape = 0.0;
            self.draw_block.draw_abs(cx, thumb);
            self.hots.push((Rect { pos: dvec2(thumb.pos.x - 4.0, pane.pos.y), size: dvec2(13.0, pane.size.y) }, Hot::PaneBar));
        }
        let inside = |r: &Rect| r.pos.y + r.size.y > pane.pos.y && r.pos.y < pane.pos.y + pane.size.y;
        let mut k = hots_before;
        while k < self.hots.len() {
            if !inside(&self.hots[k].0) {
                self.hots.remove(k);
            } else {
                k += 1;
            }
        }
        cx.end_turtle();
    }

    /// The line gone to: bright for 1.5 s, fading to a steady mark.
    fn draw_found(&mut self, cx: &mut Cx2d, p: &makepad_look::palette::Palette) {
        let Some((doc_i, at)) = self.keep.filter(|(d, _)| *d == self.active) else { return };
        let Some(doc) = self.docs.get(doc_i).cloned() else { return };
        let band = {
            let code = self.view.widget(cx, ids!(code));
            let Some(view) = code.borrow::<CodeView>() else { return };
            let Some(session) = view.session.as_ref() else { return };
            let area = view.editor.area().rect(cx);
            let line = doc.line(at);
            let Some(cell) = view.editor.position_rect(session, Position { line_index: line, byte_index: 0 }) else { return };
            Rect { pos: dvec2(area.pos.x, area.pos.y + cell.pos.y), size: dvec2(area.size.x, cell.size.y) }
        };
        let clip = self.code_rect;
        if band.pos.y + band.size.y < clip.pos.y || band.pos.y > clip.pos.y + clip.size.y {
            return;
        }
        let age = Cx::time_now() - self.found_at;
        let fade = ((age - 1.5) / 0.5).clamp(0.0, 1.0);
        let wash = 0.32 + (0.12 - 0.32) * fade;
        if age < 2.0 {
            self.found_frame = cx.new_next_frame();
        }
        self.rect(cx, band, color(p.accent, wash as f32));
        self.rect(cx, Rect { pos: band.pos, size: dvec2(3.0, band.size.y) }, color(p.accent, 1.0));
    }

    /// The live values at the end of their lines (what each holds at the
    /// playhead, read every frame from its samples over the film; else as
    /// last read paused).
    fn draw_readouts(&mut self, cx: &mut Cx2d, p: &makepad_look::palette::Palette) {
        let Some(doc) = self.docs.get(self.active).cloned() else { return };
        let spots: Vec<(DVec2, String)> = {
            let code = self.view.widget(cx, ids!(code));
            let view = code.borrow::<CodeView>();
            let mut out = Vec::new();
            if let Some(view) = view.as_ref() {
                if let Some(session) = view.session.as_ref() {
                    let origin = view.editor.area().rect(cx).pos;
                    let lines: Vec<usize> = session.document().as_text().as_lines().iter().map(|l| l.len()).collect();
                    let shown = session.layout().view_line_range();
                    let mut values: std::collections::BTreeMap<usize, String> = doc.live.iter().cloned().collect();
                    for (b, v) in &doc.series {
                        if !shown.contains(&doc.line(*b)) {
                            continue;
                        }
                        let Some(text) = series_text(v, self.playhead, self.duration) else { continue };
                        let count = values.get(b).and_then(|t| t.split_once("  ×").map(|(_, n)| n.to_string()));
                        values.insert(*b, match count { Some(n) => format!("{text}  ×{n}"), None => text });
                    }
                    for (b, v, lanes) in &doc.paths {
                        if !shown.contains(&doc.line(*b)) {
                            continue;
                        }
                        let Some(text) = path_text(v, *lanes, self.playhead, self.duration) else { continue };
                        let count = values.get(b).and_then(|t| t.split_once("  ×").map(|(_, n)| n.to_string()));
                        values.insert(*b, match count { Some(n) => format!("{text}  ×{n}"), None => text });
                    }
                    // One readout per line, on the line its `let` is
                    // written on in the text shown (a value whose place is
                    // not a `let` of this text, or a comment, shows none).
                    let shown_text = session.document().as_text().to_string();
                    let shown_lines: Vec<&str> = shown_text.split('\n').collect();
                    let mut placed: std::collections::BTreeMap<usize, String> = Default::default();
                    for (at, text) in &values {
                        let line = match doc.code.region_at(*at).map(|r| &doc.code.regions[r]).filter(|r| r.kind == RegionKind::Let) {
                            Some(r) => doc.line(r.range.start),
                            None => doc.line(*at),
                        };
                        let written = shown_lines.get(line).map_or("", |l| l.trim_start());
                        if !(written.starts_with("let ") || written.starts_with("var ")) {
                            continue;
                        }
                        placed.entry(line).or_insert_with(|| text.clone());
                    }
                    for (line, text) in placed {
                        if !shown.contains(&line) || session.is_folded(line) {
                            continue;
                        }
                        // Long values end in an ellipsis, not over the next.
                        let text: String = if text.chars().count() > 48 { format!("{}…", text.chars().take(47).collect::<String>()) } else { text };
                        let at = &line;
                        let Some(end) = lines.get(*at) else { continue };
                        let Some(cell) = view.editor.position_rect(session, Position { line_index: *at, byte_index: *end }) else { continue };
                        out.push((origin + cell.pos + dvec2(14.0, 2.0), text.clone()));
                    }
                }
            }
            out
        };
        cx.begin_turtle(Walk::abs_rect(self.code_rect), Layout::default().with_clip(true, true));
        for (at, text) in spots {
            self.draw_small.color = color(p.accent_soft, 1.0);
            self.draw_small.draw_abs(cx, at, &format!("= {text}"));
        }
        cx.end_turtle();
    }

    /// The scrubber: play/pause, the time, and the film on a thin strip
    /// (its sound, the song's beats, each second, the playhead).
    fn draw_scrubber(&mut self, cx: &mut Cx2d, p: &makepad_look::palette::Palette) {
        let row = self.view.widget(cx, ids!(scrub)).area().rect(cx);
        let play = self.chip(cx, row.pos.x + 8.0, row.pos.y + (row.size.y - 20.0) * 0.5, if self.playing { "❚❚" } else { "▶" }, self.playing, p);
        self.hots.push((play, Hot::PlayPause));
        let time = timecode_full(self.playhead, 60.0);
        self.draw_text.color = color(p.text_hi, 1.0);
        self.draw_text.draw_abs(cx, dvec2(play.pos.x + play.size.x + 10.0, play.pos.y + 3.0), &time);
        let x0 = play.pos.x + play.size.x + 10.0 + advance(&self.draw_text, cx, &time) + 14.0;
        self.strip = Rect { pos: dvec2(x0, row.pos.y + 4.0), size: dvec2((row.pos.x + row.size.x - 10.0 - x0).max(40.0), row.size.y - 8.0) };
        let strip = self.strip;
        self.rect(cx, strip, color(p.wave_bg, 1.0));
        // The graphic along its time: small frames, whole or filling in.
        if let Some(texture) = self.film.draw(cx, strip, self.playing) {
            self.draw_film.draw_vars.set_texture(0, &texture);
            self.draw_film.draw_abs(cx, strip);
        }
        if self.film.busy() {
            self.redraw_over(cx.cx);
        }
        // Each second along its foot; the song's beats as ticks over it.
        let mut t = 0.0;
        while t <= self.duration + 1e-9 {
            let tx = self.x_of(t);
            self.rect(cx, Rect { pos: dvec2(tx, strip.pos.y + strip.size.y - 4.0), size: dvec2(1.0, 4.0) }, color(p.wave_rim, 1.0));
            t += 1.0;
        }
        for b in self.beats() {
            let tx = self.x_of(b);
            self.rect(cx, Rect { pos: dvec2(tx, strip.pos.y), size: dvec2(1.0, 5.0) }, color(p.label, 0.7));
        }
        // Where the panel's controls act: a thin line along the strip's
        // foot (never over the frames: they stay visible), the lit control's
        // bright and its label beside the time. A click anywhere on the
        // lower part of the strip over a span goes to it.
        let band = Rect { pos: dvec2(strip.pos.x, strip.pos.y + strip.size.y * 0.55), size: dvec2(strip.size.x, strip.size.y * 0.45) };
        let spans = self.spans.clone();
        let focus = self.span_focus.clone();
        for s in spans.iter().filter(|s| Some(&s.key) != focus.as_ref()).chain(spans.iter().filter(|s| Some(&s.key) == focus.as_ref())) {
            let lit = Some(&s.key) == focus.as_ref();
            for &(a, b) in &s.spans {
                let (xa, xb) = (self.x_of(a.max(0.0)), self.x_of(b.min(self.duration)));
                let r = Rect { pos: dvec2(xa, band.pos.y), size: dvec2((xb - xa).max(2.0), band.size.y) };
                let h = if lit { 3.0 } else { 2.0 };
                let line = Rect { pos: dvec2(xa, strip.pos.y + strip.size.y - h), size: dvec2((xb - xa).max(2.0), h) };
                self.rect(cx, line, color(p.accent, if lit { 1.0 } else if focus.is_some() { 0.25 } else { 0.5 }));
                self.hots.push((r, Hot::Span(a.max(0.0))));
            }
        }
        if let Some(s) = focus.and_then(|k| spans.iter().find(|s| s.key == k)) {
            // Its name as a small chip in the corner (the frames stay seen).
            let w = advance(&self.draw_small, cx, &s.label) + 10.0;
            let chip = Rect { pos: dvec2(strip.pos.x + strip.size.x - w - 4.0, strip.pos.y + 3.0), size: dvec2(w, 14.0) };
            self.rect(cx, chip, color(p.panel, 0.85));
            self.draw_small.color = color(p.accent, 1.0);
            self.draw_small.draw_abs(cx, dvec2(chip.pos.x + 5.0, chip.pos.y + 1.5), &s.label);
        }
        let px = self.x_of(self.playhead);
        self.rect(cx, Rect { pos: dvec2(px - 1.0, strip.pos.y - 2.0), size: dvec2(2.0, strip.size.y + 4.0) }, color(p.accent, 1.0));
        self.hots.push((strip, Hot::Scrub));
    }

    /// The scene's files under the file chip: the scene, its modules, then
    /// the built-ins (dimmed).
    fn draw_file_menu(&mut self, cx: &mut Cx2d, p: &makepad_look::palette::Palette, at: Rect) {
        let mut order: Vec<usize> = Vec::new();
        order.push(0);
        order.extend((1..self.docs.len()).filter(|&i| !self.docs[i].read_only));
        order.extend((1..self.docs.len()).filter(|&i| self.docs[i].read_only));
        let w = order.iter().map(|&i| advance(&self.draw_small, cx, &self.docs[i].title)).fold(140.0, f64::max) + 70.0;
        let h = order.len() as f64 * 20.0 + 8.0 + if order.iter().any(|&i| self.docs[i].read_only) { 6.0 } else { 0.0 };
        let r = Rect { pos: dvec2(at.pos.x, at.pos.y + at.size.y + 3.0), size: dvec2(w, h) };
        self.draw_block.fill = color(p.menu, 1.0);
        self.draw_block.edge = color(p.border_hi, 1.0);
        self.draw_block.stroke = 1.0;
        self.draw_block.radius = 6.0;
        self.draw_block.shape = 0.0;
        self.draw_block.draw_abs(cx, r);
        let mut y = r.pos.y + 4.0;
        let mut prev_ro = false;
        for i in order {
            let (title, read_only) = (self.docs[i].title.clone(), self.docs[i].read_only);
            if read_only && !prev_ro && i != 0 {
                self.rect(cx, Rect { pos: dvec2(r.pos.x + 8.0, y + 2.0), size: dvec2(w - 16.0, 1.0) }, color(p.separator, 1.0));
                y += 6.0;
            }
            prev_ro = read_only;
            let row = Rect { pos: dvec2(r.pos.x + 3.0, y), size: dvec2(w - 6.0, 19.0) };
            if i == self.active {
                self.rect(cx, row, color(p.accent_wash, 1.0));
            }
            let kind = if i == 0 { "scene" } else if read_only { "built-in" } else { "module" };
            self.draw_small.color = color(if read_only { p.muted } else { p.text }, 1.0);
            self.draw_small.draw_abs(cx, dvec2(row.pos.x + 8.0, row.pos.y + 4.0), &title);
            let kw = advance(&self.draw_small, cx, kind);
            self.draw_small.color = color(p.dim, 1.0);
            self.draw_small.draw_abs(cx, dvec2(row.pos.x + row.size.x - kw - 8.0, row.pos.y + 4.0), kind);
            self.hots.insert(0, (row, Hot::Tab(i)));
            y += 20.0;
        }
    }

    fn chip(&mut self, cx: &mut Cx2d, x: f64, y: f64, text: &str, on: bool, p: &makepad_look::palette::Palette) -> Rect {
        let w = advance(&self.draw_small, cx, text) + 14.0;
        let r = Rect { pos: dvec2(x, y), size: dvec2(w, 20.0) };
        let look = p.pick(on, false, p.raised);
        self.draw_block.fill = color(look.fill, 1.0);
        self.draw_block.edge = color(if on { look.edge } else { p.line }, 1.0);
        self.draw_block.stroke = 1.0;
        self.draw_block.radius = 4.0;
        self.draw_block.shape = 0.0;
        self.draw_block.draw_abs(cx, r);
        self.draw_small.color = color(if on { p.text_hi } else { p.label }, 1.0);
        self.draw_small.draw_abs(cx, dvec2(x + 7.0, y + 5.0), text);
        r
    }

    fn rect(&mut self, cx: &mut Cx2d, r: Rect, c: Vec4f) {
        if r.size.x > 0.0 && r.size.y > 0.0 {
            self.draw_solid.color = c;
            self.draw_solid.draw_abs(cx, r);
        }
    }

    fn x_of(&self, t: f64) -> f64 {
        self.strip.pos.x + 4.0 + (t / self.duration.max(0.1)) * (self.strip.size.x - 8.0)
    }

    fn t_of(&self, x: f64) -> f64 {
        ((x - self.strip.pos.x - 4.0) / (self.strip.size.x - 8.0).max(1.0) * self.duration).clamp(0.0, self.duration)
    }
}

impl Widget for MotionCodeTimeline {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        let uid = self.widget_uid();
        if self.found_frame.is_event(event).is_some() {
            self.redraw_over(cx);
        }
        if self.film.handle_event(cx, event) {
            self.redraw_over(cx);
        }
        match event {
            Event::MouseDown(e) => {
                let hit = self.hots.iter().find(|(r, _)| r.contains(e.abs)).map(|(_, h)| h.clone());
                // The open file list: a file, or anywhere else, closes it.
                if self.file_menu && !matches!(hit, Some(Hot::Tab(_)) | Some(Hot::FileMenu)) {
                    self.file_menu = false;
                    self.redraw_over(cx);
                    return;
                }
                if let Some(hot) = hit {
                    match hot {
                        Hot::FileMenu => {
                            self.file_menu = !self.file_menu;
                            self.redraw_over(cx);
                        }
                        Hot::Tab(i) => {
                            self.file_menu = false;
                            self.redraw_over(cx);
                            if self.active != i {
                                self.active = i;
                                self.touch();
                                self.redraw_over(cx);
                            }
                        }
                        Hot::FoldToggle => {
                            self.fold_mode = match self.fold_mode {
                                FoldMode::Bound => FoldMode::Moving,
                                FoldMode::Moving => FoldMode::All,
                                FoldMode::All => FoldMode::Bound,
                            };
                            self.touch();
                            self.redraw_over(cx);
                        }
                        Hot::Trace(k) => {
                            if let Some((drawn, frames, _)) = self.trace.clone() {
                                if let Some(f) = frames.get(k) {
                                    self.trace = Some((drawn, frames.clone(), f.frame));
                                    self.reveal(cx, f.doc, f.at);
                                }
                            }
                        }
                        Hot::Origin => {
                            if let Some((doc, range, _)) = self.origin.clone() {
                                self.reveal_words(cx, doc, range);
                            }
                        }
                        Hot::PaneBar => self.pane_drag = Some((e.abs.y, self.pane_scroll)),
                        Hot::NavBack => self.nav_step(cx, -1),
                        Hot::NavForward => self.nav_step(cx, 1),
                        Hot::Crumb(k) => self.nav_go(cx, k),
                        Hot::PlayPause => cx.widget_action(uid, CodeTimelineAction::PlayPause),
                        Hot::Span(t) => cx.widget_action(uid, CodeTimelineAction::Scrub(t)),
                        Hot::LaneClose(li) => {
                            if let Some(lane) = self.lanes.get(li) {
                                cx.widget_action(uid, CodeTimelineAction::LaneClose(lane.path.clone()));
                            }
                        }
                        Hot::LaneKey(li, ki) => {
                            self.key_sel = Some((li, ki));
                            if let Some(lane) = self.lanes.get(li) {
                                let vals: Vec<f64> = lane.keys.keys.iter().filter_map(|k| k.v).collect();
                                let (lo, hi) = vals.iter().fold((f64::MAX, f64::MIN), |(a, b), v| (a.min(*v), b.max(*v)));
                                let (lo, hi) = if (hi - lo).abs() < 1e-9 { (lo - 1.0, hi + 1.0) } else { (lo - (hi - lo) * 0.1, hi + (hi - lo) * 0.1) };
                                let key = &lane.keys.keys[ki];
                                if let (Some(t), Some(v)) = (key.t.or(Some(0.0)), key.v) {
                                    self.gestures += 1;
                                    let height = self.lanes_rect.size.y / self.lanes.len().max(1) as f64 - 12.0;
                                    self.key_drag = Some(KeyDrag { lane: li, key: ki, from: e.abs, t0: t, v0: v, lo, hi, height, t_now: key.t.map(|_| key.t_range.clone()), v_now: key.v_range.clone(), gesture: self.gestures, moved: false });
                                    cx.widget_action(uid, CodeTimelineAction::Scrub(key.t.unwrap_or(self.playhead)));
                                }
                            }
                            self.redraw_over(cx);
                        }
                        Hot::LaneEase(li, ki) => {
                            // linear → in-out → out → in → hold → linear.
                            const EASES: [Option<&str>; 5] = [None, Some("ease_in_out_cubic"), Some("ease_out_cubic"), Some("ease_in_cubic"), Some("hold")];
                            if let Some(lane) = self.lanes.get(li).cloned() {
                                let now = lane.keys.keys.get(ki).and_then(|k| k.ease.as_ref().map(|(_, e)| e.trim_start_matches('@').to_string()));
                                let at = EASES.iter().position(|e| e.map(str::to_string) == now).unwrap_or(0);
                                let next = EASES[(at + 1) % EASES.len()];
                                let text = self.docs.get(lane.doc).map(|d| d.text.clone()).unwrap_or_default();
                                if let Some((range, with)) = makepad_motion::lane::set_ease(&text, &lane.keys, ki, next) {
                                    self.gestures += 1;
                                    cx.widget_action(uid, CodeTimelineAction::Edits { doc: lane.doc, edits: vec![Edit::splice(range.start..range.end, with, self.gestures, true)] });
                                }
                            }
                        }
                        Hot::Lane(li) => {
                            // A double click adds a key there, on the curve.
                            let now = Cx::time_now();
                            let double = self.lane_click.is_some_and(|(t, l)| now - t < 0.4 && l == li);
                            self.lane_click = Some((now, li));
                            self.key_sel = None;
                            if double {
                                if let Some(lane) = self.lanes.get(li).cloned() {
                                    let t = self.t_of(e.abs.x);
                                    let v = makepad_motion::lane::value_at(&lane.keys, t).unwrap_or(0.0);
                                    let (range, text) = makepad_motion::lane::add_key(&lane.keys, t, v);
                                    self.gestures += 1;
                                    cx.widget_action(uid, CodeTimelineAction::Edits { doc: lane.doc, edits: vec![Edit::splice(range.start..range.end, text, self.gestures, true)] });
                                }
                            }
                            self.redraw_over(cx);
                        }
                        Hot::Scrub => {
                            self.scrubbing = true;
                            cx.widget_action(uid, CodeTimelineAction::Scrub(self.t_of(e.abs.x)));
                        }
                    }
                    return;
                }
            }
            Event::MouseMove(e) if self.key_drag.is_some() => {
                self.move_key(cx, uid, e.abs, e.modifiers.shift, false);
                return;
            }
            Event::MouseUp(e) if self.key_drag.is_some() => {
                self.move_key(cx, uid, e.abs, e.modifiers.shift, true);
                return;
            }
            Event::MouseMove(e) if self.scrubbing => {
                cx.widget_action(uid, CodeTimelineAction::Scrub(self.t_of(e.abs.x)));
                return;
            }
            Event::MouseUp(_) if self.scrubbing => {
                self.scrubbing = false;
                return;
            }
            Event::MouseMove(e) if self.pane_drag.is_some() => {
                let (y0, s0) = self.pane_drag.unwrap();
                let pane = self.gather_rect;
                let max = (self.pane_content - pane.size.y).max(0.0);
                let per = self.pane_content / pane.size.y.max(1.0);
                self.pane_scroll = (s0 + (e.abs.y - y0) * per).clamp(0.0, max);
                self.redraw_over(cx);
                return;
            }
            Event::MouseUp(_) if self.pane_drag.is_some() => {
                self.pane_drag = None;
                return;
            }
            Event::Scroll(e) if self.gather_rect.contains(e.abs) && self.trace.is_some() => {
                let max = (self.pane_content - self.gather_rect.size.y).max(0.0);
                self.pane_scroll = (self.pane_scroll + e.scroll.y).clamp(0.0, max);
                self.redraw_over(cx);
                return;
            }
            _ => {}
        }
        // The desk's keys (undo, play, …), only when no text field has the
        // keys; while the code is typed in, undo and redo still go to the
        // desk (the document's undo).
        if let Event::KeyDown(key) = event {
            let command = key.modifiers.logo || key.modifiers.control;
            if command && matches!(key.key_code, KeyCode::LBracket | KeyCode::RBracket) {
                self.nav_step(cx, if key.key_code == KeyCode::LBracket { -1 } else { 1 });
                return;
            }
            let typing = cx.text_input_has_focus();
            // A selected key: Delete or Backspace takes it away.
            if !typing && matches!(key.key_code, KeyCode::Delete | KeyCode::Backspace) {
                if let Some((li, ki)) = self.key_sel.take() {
                    if let Some(lane) = self.lanes.get(li).cloned() {
                        if let Some((range, text)) = makepad_motion::lane::remove_key(&lane.keys, ki) {
                            self.gestures += 1;
                            cx.widget_action(uid, CodeTimelineAction::Edits { doc: lane.doc, edits: vec![Edit::splice(range.start..range.end, text, self.gestures, true)] });
                        }
                    }
                    self.redraw_over(cx);
                    return;
                }
            }
            let desk_key = (command && matches!(key.key_code, KeyCode::KeyZ)) || (!typing && ((command && key.key_code == KeyCode::KeyD) || matches!(key.key_code, KeyCode::Space | KeyCode::Escape)));
            if desk_key {
                cx.widget_action(uid, CodeTimelineAction::Key(key.clone()));
                return;
            }
        }
        self.view.handle_event(cx, event, scope);
        // Typed in the code: the change goes to the document (one undo step
        // per run of typing).
        if matches!(event, Event::TextInput(_) | Event::KeyDown(_) | Event::TextCut(_)) {
            self.take_typed(cx, uid);
        }
        if matches!(event, Event::Scroll(_)) {
            self.redraw_over(cx);
        }
        // The caret in the code selects its block (its drawing lights up);
        // cmd-click on a name goes to its definition.
        if let Event::MouseUp(e) = event {
            let code = self.view.widget(cx, ids!(code));
            let at = code.borrow::<CodeView>().and_then(|view| {
                let session = view.session.as_ref()?;
                if !view.editor.area().rect(cx).contains(e.abs) {
                    return None;
                }
                let p = session.selections().last()?.cursor.position;
                let text = session.document().as_text().to_string();
                Some(text.split('\n').take(p.line_index).map(|l| l.len() + 1).sum::<usize>() + p.byte_index)
            });
            if let (Some(at), true) = (at, e.modifiers.logo || e.modifiers.control) {
                if let Some((doc, def)) = self.definition(self.active, at) {
                    self.reveal(cx, doc, def);
                    return;
                }
            }
            if let Some(at) = at {
                let region = self.docs.get(self.active).and_then(|d| d.code.region_at(at));
                if region.is_some() {
                    self.selected = region.map(|r| (self.active, r));
                    cx.widget_action(uid, CodeTimelineAction::Select(self.selected));
                    self.redraw_over(cx);
                }
            }
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let step_t0 = makepad_value_pickers::panel::host::timing().then(Cx::time_now);
        let step = self.draw_walk_timed(cx, scope, walk);
        if let Some(t0) = step_t0 {
            makepad_value_pickers::panel::host::time_add("draw.code_timeline", Cx::time_now() - t0);
        }
        step
    }
}

impl MotionCodeTimeline {
    fn draw_walk_timed(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        makepad_value_pickers::panel::host::timed("code.sync", || self.sync_code(cx.cx));
        // Under the code: its ground (the editor's own is clear).
        if self.code_rect.size.x > 0.0 {
            let p = makepad_look::palette::get(cx);
            let r = self.code_rect;
            self.rect(cx, r, color(p.well, 1.0));
        }
        makepad_value_pickers::panel::host::timed("code.view_draw", || while self.view.draw_walk(cx, scope, walk).is_step() {});
        self.hots.clear();
        let p = makepad_look::palette::get(cx);
        self.code_rect = self.view.widget(cx, ids!(code_box)).area().rect(cx);
        let bar = self.view.widget(cx, ids!(bar)).area().rect(cx);
        // One header line: the page switch is laid over its left end (the
        // pane's own), then the current file as a chip that lists the
        // scene's files, the way back, and what the code folds to as a
        // small icon at the right.
        let mut x = bar.pos.x + LEAD;
        let doc_title = |i: usize, d: &CodeDoc| if i == 0 { d.title.clone() } else if d.read_only { format!("{} · built-in", d.title) } else { format!("{} · module", d.title) };
        let current = self.docs.get(self.active).map(|d| doc_title(self.active, d)).unwrap_or_default();
        let file = self.chip(cx, x, bar.pos.y + 5.0, &format!("{current}     "), self.file_menu, &p);
        // Its chevron.
        let (cx0, cy0) = (file.pos.x + file.size.x - 14.0, file.pos.y + 8.0);
        for k in 0..4 {
            let k = k as f64;
            self.rect(cx, Rect { pos: dvec2(cx0 + k, cy0 + k), size: dvec2(1.3, 1.3) }, color(p.muted, 1.0));
            self.rect(cx, Rect { pos: dvec2(cx0 + 6.0 - k, cy0 + k), size: dvec2(1.3, 1.3) }, color(p.muted, 1.0));
        }
        self.hots.push((file, Hot::FileMenu));
        x = file.pos.x + file.size.x + 6.0;
        let label = match self.fold_mode {
            FoldMode::Bound if self.bindings.iter().any(|b| b.doc == self.active) => "folded to the panel",
            FoldMode::Bound => "all code",
            FoldMode::Moving => "folded to what moves",
            FoldMode::All => "all code",
        };
        // The fold as an icon: lines with gaps (folded) or whole (all).
        let toggle = Rect { pos: dvec2(bar.pos.x + bar.size.x - 30.0, bar.pos.y + 5.0), size: dvec2(22.0, 20.0) };
        let folded = label != "all code";
        let look = p.pick(folded, false, p.raised);
        self.draw_block.fill = color(look.fill, 1.0);
        self.draw_block.edge = color(if folded { look.edge } else { p.line }, 1.0);
        self.draw_block.stroke = 1.0;
        self.draw_block.radius = 4.0;
        self.draw_block.shape = 0.0;
        self.draw_block.draw_abs(cx, toggle);
        for (k, w) in [10.0, 6.0, 10.0, 8.0].into_iter().enumerate() {
            let gap = if folded && k >= 2 { 3.0 } else { 0.0 };
            self.rect(cx, Rect { pos: dvec2(toggle.pos.x + 6.0, toggle.pos.y + 4.5 + k as f64 * 3.0 + gap), size: dvec2(w, 1.2) }, color(p.text_soft, 1.0));
        }
        self.hots.push((toggle, Hot::FoldToggle));
        // The way back: the jumps' arrows and the trail to here.
        if !self.nav.is_empty() {
            x += 6.0;
            let back = self.chip(cx, x, bar.pos.y + 5.0, "‹", false, &p);
            self.hots.push((back, Hot::NavBack));
            x = back.pos.x + back.size.x + 3.0;
            let fwd = self.chip(cx, x, bar.pos.y + 5.0, "›", false, &p);
            self.hots.push((fwd, Hot::NavForward));
            x = fwd.pos.x + fwd.size.x + 8.0;
            let limit = toggle.pos.x - 12.0;
            let first = self.nav_at.saturating_sub(3);
            for k in first..self.nav.len().min(self.nav_at + 2) {
                let spot = self.nav[k];
                let Some(doc) = self.docs.get(spot.doc) else { continue };
                let line = doc.line(spot.at) + 1;
                let words = doc.code.literal_at(spot.at).map(|l| &doc.code.literals[l]).filter(|l| l.range.start == spot.at).and_then(|l| l.words()).map(|w| {
                    let w: String = w.chars().take(14).collect();
                    format!(" “{w}”")
                }).unwrap_or_default();
                let file = if spot.doc == 0 { String::new() } else { format!("{} ", doc.title) };
                let label = format!("{}{file}{line}{words}", if k == first { "" } else { "› " });
                let w = advance(&self.draw_small, cx, &label) + 10.0;
                if x + w > limit {
                    break;
                }
                let r = Rect { pos: dvec2(x, bar.pos.y + 8.0), size: dvec2(w, 14.0) };
                self.draw_small.color = color(if k == self.nav_at { p.accent } else if k > self.nav_at { p.dim } else { p.label }, 1.0);
                self.draw_small.draw_abs(cx, dvec2(x + 3.0, bar.pos.y + 9.0), &label);
                self.hots.push((r, Hot::Crumb(k)));
                x += w;
            }
        }
        makepad_value_pickers::panel::host::timed("code.scrubber", || self.draw_scrubber(cx, &p));
        makepad_value_pickers::panel::host::timed("code.lanes", || self.draw_lanes(cx, &p));
        makepad_value_pickers::panel::host::timed("code.readouts", || self.draw_readouts(cx, &p));
        makepad_value_pickers::panel::host::timed("code.found", || self.draw_found(cx, &p));
        makepad_value_pickers::panel::host::timed("code.pick", || self.draw_pick(cx, &p));
        if self.file_menu {
            self.draw_file_menu(cx, &p, file);
        }
        DrawStep::done()
    }
}

/// Where `t` of a film `dur` long falls among `n` samples (sample k sits at
/// (k+0.5)/n of the film): two neighbours and the share of the second.
fn sample_at(n: usize, t: f64, dur: f64) -> (usize, usize, f32) {
    let u = (t / dur.max(1e-6) * n as f64 - 0.5).clamp(0.0, n.saturating_sub(1) as f64);
    let k = u.floor() as usize;
    (k, (k + 1).min(n.saturating_sub(1)), (u - u.floor()) as f32)
}

/// A sampled number's value at `t`, as a readout.
fn series_text(v: &[f32], t: f64, dur: f64) -> Option<String> {
    if v.is_empty() {
        return None;
    }
    let (k, j, f) = sample_at(v.len(), t, dur);
    let (a, c) = (v[k], v[j]);
    let x = if a.is_finite() && c.is_finite() { a + (c - a) * f } else if a.is_finite() { a } else { return None };
    Some(makepad_motion::schema::show_number(x as f64))
}

/// A sampled vector's or colour's value at `t`, as a readout (`(x, y)` or
/// `#rrggbb`).
fn path_text(v: &[[f32; 4]], lanes: u8, t: f64, dur: f64) -> Option<String> {
    if v.is_empty() {
        return None;
    }
    let (k, j, f) = sample_at(v.len(), t, dur);
    let (a, c) = (v[k], v[j]);
    if !a[0].is_finite() {
        return None;
    }
    let x: Vec<f32> = (0..4).map(|i| if c[i].is_finite() { a[i] + (c[i] - a[i]) * f } else { a[i] }).collect();
    if lanes == 5 {
        let b = |x: f32| (x.clamp(0.0, 1.0) * 255.0).round() as u32;
        return Some(format!("#{:02x}{:02x}{:02x}", b(x[0]), b(x[1]), b(x[2])));
    }
    let n = (lanes as usize).clamp(2, 4);
    Some(format!("({})", x[..n].iter().map(|x| makepad_motion::schema::show_number(*x as f64)).collect::<Vec<_>>().join(", ")))
}
