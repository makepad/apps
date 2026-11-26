//! The CURVES page: the scene's animated values as curves, in place of the
//! code (the bottom pane's other page), edited the way a graph editor
//! works: a channel list on the left (every `keys(…)` in the scene and its
//! modules, grouped by the node or `let` it is written in), a slim header
//! (Graph/Dopesheet, the View, Select, Key and Channel menus with their
//! shortcuts, snapping, normalise), the graph with its time ruler, and a
//! status line with the selected key's time and value.
//!
//! Most of it is keys and the pointer: I keys the hovered or selected
//! channel at the playhead, Ctrl/Cmd+click on or near a curve adds a key,
//! a double click on a curve adds one; G grabs, S scales, R turns handles,
//! X deletes, Shift+D duplicates, T interpolation, V handle type, Shift+E
//! extrapolation, A/Home frames all, F frames the selection; a right click
//! is the Key menu.
//!
//! The code is the truth: channels are read from the code each time it
//! changes (`makepad_motion::lane`), what the graph draws is what the
//! document evaluates (`lane::sample`), and every change is a text edit of
//! the curve's `keys(…)` through the desk's code edits: a drag's steps held
//! live (numbers patched in the running film where they can be), its
//! release one undo step.

use super::super::VideoCut;
use makepad_motion::lane::{self, Interp, Keys};
use makepad_motion::model::{Extrap, HandleMode, KeyHandles};
use makepad_edit_values::{Change, Edit};
use makepad_value_pickers::panel::control::{fit_text, DrawPanelShape, PanelControl, PanelControlAction, R_CONTROL, R_GROUP};
use makepad_value_pickers::panel::PanelValue;
use makepad_widgets::*;
use std::collections::{HashMap, HashSet};

script_mod! {
    use mod.prelude.widgets_internal.*
    use mod.widgets.*
    let stage = mod.stage

    mod.widgets.DrawCurveBase = #(DrawCurve::script_component(vm))
    /** A curve's piece: a line from `a` to `b` (`shape` 0), a key (1, a
     * rounded square; 2 a diamond), a handle's end (3, a circle), an eye
     * (4), a lock (5), in the quad's own pixels. */
    set_type_default() do #(DrawCurve::script_shader(vm)){
        ..mod.draw.DrawQuad
        pixel: fn() {
            let sdf = Sdf2d.viewport(self.pos * self.rect_size)
            let w = self.rect_size.x
            let h = self.rect_size.y
            if self.shape < 0.5 {
                sdf.move_to(self.a.x, self.a.y)
                sdf.line_to(self.b.x, self.b.y)
                sdf.stroke(self.color, self.width)
            } else if self.shape < 1.5 {
                sdf.box(1.0, 1.0, w - 2.0, h - 2.0, 1.5)
                sdf.fill_keep(self.color)
                sdf.stroke(self.edge, 1.0)
            } else if self.shape < 2.5 {
                sdf.move_to(w * 0.5, 0.5)
                sdf.line_to(w - 0.5, h * 0.5)
                sdf.line_to(w * 0.5, h - 0.5)
                sdf.line_to(0.5, h * 0.5)
                sdf.close_path()
                sdf.fill_keep(self.color)
                sdf.stroke(self.edge, 1.0)
            } else if self.shape < 3.5 {
                sdf.circle(w * 0.5, h * 0.5, min(w, h) * 0.5 - 1.0)
                sdf.fill_keep(self.color)
                sdf.stroke(self.edge, 1.0)
            } else if self.shape < 4.5 {
                sdf.move_to(1.0, h * 0.5)
                sdf.line_to(w * 0.25, h * 0.2)
                sdf.line_to(w * 0.75, h * 0.2)
                sdf.line_to(w - 1.0, h * 0.5)
                sdf.line_to(w * 0.75, h * 0.8)
                sdf.line_to(w * 0.25, h * 0.8)
                sdf.close_path()
                sdf.stroke(self.color, 1.1)
                sdf.circle(w * 0.5, h * 0.5, h * 0.17)
                sdf.fill(self.color)
            } else {
                sdf.box(w * 0.2, h * 0.45, w * 0.6, h * 0.45, 1.0)
                sdf.fill(self.color)
                sdf.move_to(w * 0.32, h * 0.47)
                sdf.line_to(w * 0.32, h * 0.25)
                sdf.line_to(w * 0.68, h * 0.25)
                sdf.line_to(w * 0.68, h * 0.47)
                sdf.stroke(self.color, 1.1)
            }
            return sdf.result
        }
    }

    mod.widgets.CurveEditorBase = #(CurveEditor::register_widget(vm))
    /** The channels, the header, the ruler and the graph or dopesheet. */
    mod.widgets.CurveEditor = set_type_default() do mod.widgets.CurveEditorBase{
        width: Fill height: Fill
        draw_text +: {text_style: theme.font_regular{font_size: 8}}
        draw_small +: {text_style: theme.font_regular{font_size: 7.5}}
        draw_bold +: {text_style: theme.font_bold{font_size: 8}}
    }

    /** The CURVES page: the editor, its channel filter over the list's
     * corner, and the status line. */
    mod.widgets.CurvesPage = View{width: Fill height: Fill flow: Down visible: false
        View{width: Fill height: Fill flow: Overlay
            cv_editor := mod.widgets.CurveEditor{}
            View{width: 222 height: Fit padding: Inset{left: 6 top: 31}
                cv_search := TextInput{width: Fill height: 20 margin: 0 padding: Inset{left: 6 right: 6 top: 2 bottom: 2} empty_text: "Filter channels"
                    draw_bg +: {color: stage.well color_hover: stage.well color_focus: stage.well border_color: stage.line_faint border_size: 1.0 border_radius: 4.0}
                    draw_text +: {text_style: theme.font_regular{font_size: 8}}}}
        }
        SolidView{width: Fill height: 1 draw_bg +: {color: stage.separator}}
        View{width: Fill height: 26 flow: Right spacing: 6 padding: Inset{left: 10 right: 10} align: Align{y: 0.5}
            cv_status := Label{width: Fill text: "" draw_text +: {color: stage.muted text_style: theme.font_regular{font_size: 7.5}}}
            cv_key := View{width: Fit height: Fit flow: Right spacing: 6 align: Align{y: 0.5} visible: false
                cv_t := mod.widgets.PanelNumber{width: 130 label: "Time"}
                cv_v := mod.widgets.PanelNumber{width: 130 label: "Value"}}
        }
    }
}

#[derive(Script, ScriptHook)]
#[repr(C)]
pub struct DrawCurve {
    #[deref]
    draw_super: DrawQuad,
    #[live]
    pub color: Vec4f,
    #[live]
    pub edge: Vec4f,
    #[live]
    pub a: Vec2f,
    #[live]
    pub b: Vec2f,
    #[live]
    pub width: f32,
    #[live]
    pub shape: f32,
}

impl DrawCurve {
    /// A line from `p` to `q` (window points), `w` wide.
    fn line(&mut self, cx: &mut Cx2d, p: DVec2, q: DVec2, w: f64, c: Vec4f) {
        let pad = w + 1.5;
        let lo = dvec2(p.x.min(q.x) - pad, p.y.min(q.y) - pad);
        let hi = dvec2(p.x.max(q.x) + pad, p.y.max(q.y) + pad);
        self.shape = 0.0;
        self.color = c;
        self.width = w as f32;
        self.a = Vec2f { x: (p.x - lo.x) as f32, y: (p.y - lo.y) as f32 };
        self.b = Vec2f { x: (q.x - lo.x) as f32, y: (q.y - lo.y) as f32 };
        self.draw_abs(cx, Rect { pos: lo, size: hi - lo });
    }

    fn mark(&mut self, cx: &mut Cx2d, c: DVec2, size: f64, shape: f32, fill: Vec4f, edge: Vec4f) {
        self.shape = shape;
        self.color = fill;
        self.edge = edge;
        self.draw_abs(cx, Rect { pos: c - dvec2(size * 0.5, size * 0.5), size: dvec2(size, size) });
    }

    fn icon(&mut self, cx: &mut Cx2d, r: Rect, shape: f32, c: Vec4f) {
        self.shape = shape;
        self.color = c;
        self.draw_abs(cx, r);
    }
}

/// One animated value: a `keys(…)` written in a file of the scene.
#[derive(Clone, Debug)]
pub struct Channel {
    /// `<doc>:<call start>` as the text is now.
    pub id: String,
    pub doc: usize,
    pub label: String,
    /// What it belongs to (a layer's name, a `let`), for grouping.
    pub group: String,
    /// The structure node it is written in, when one.
    pub node: Option<String>,
    pub keys: Keys,
    /// Its file's text (the keys' ranges are in it).
    pub text: std::rc::Rc<String>,
    pub hue: u32,
}

impl Channel {
    fn track(&self) -> (makepad_motion::model::Track<1>, Vec<usize>) {
        lane::track(&self.keys)
    }

    fn range(&self) -> (f64, f64) {
        let vals: Vec<f64> = self.keys.keys.iter().filter_map(|k| k.v).collect();
        let (lo, hi) = vals.iter().fold((f64::MAX, f64::MIN), |(a, b), v| (a.min(*v), b.max(*v)));
        if vals.is_empty() {
            (0.0, 1.0)
        } else if (hi - lo).abs() < 1e-9 {
            (lo - 1.0, hi + 1.0)
        } else {
            (lo, hi)
        }
    }
}

#[derive(Clone, Debug, Default)]
pub enum CurveAction {
    #[default]
    None,
    /// Text edits of a file of the scene (one gesture: held while `done`
    /// is false, written at the last).
    Edits { doc: usize, edits: Vec<Edit> },
    /// The playhead moved on the ruler.
    Seek(f64),
    /// The pointer is over a channel (its node lights in the picture).
    Hover(Option<String>),
    /// What the status line says, and the one selected key's time and
    /// value (its fields), when one is.
    Status { text: String, key: Option<(f64, f64)> },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Mode {
    #[default]
    Graph,
    Dopesheet,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Snap {
    Off,
    Frames,
    #[default]
    Beats,
    Words,
}

/// Every command, from a menu, a key or the context menu.
#[derive(Clone, Debug, PartialEq)]
enum Cmd {
    Mode(Mode),
    FrameAll,
    FrameSelected,
    Normalise,
    SelectAll,
    SelectNone,
    SelectInvert,
    BoxSelect,
    Insert,
    /// A key at this time and value on this channel (Ctrl+click, a double
    /// click, the context menu's "Insert key here").
    InsertAt(usize, f64, f64),
    Delete,
    Copy,
    Paste,
    Duplicate,
    Grab,
    Scale,
    Rotate,
    Interp(Interp),
    Handle(HandleMode),
    Extrap(Extrap),
    Snap(Snap),
    ShowAll,
    HideOthers,
    LockToggle,
    SoloToggle,
}

#[derive(Clone, Debug)]
struct Menu {
    at: DVec2,
    title: String,
    items: Vec<(String, String, Option<Cmd>)>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Spot {
    Group(usize),
    Channel(usize),
    Eye(usize),
    Lock(usize),
    Solo(usize),
    ModeTab(Mode),
    MenuOpen(usize),
    SnapMenu,
    Normalise,
    Ruler,
    MenuItem(usize),
}

/// A key, by channel and its index among the channel's numeric keys.
type KeyRef = (usize, usize);

#[derive(Clone, Debug)]
enum Drag {
    Keys { from: DVec2, base: Vec<(KeyRef, f64, f64)>, axis: Option<bool>, moved: bool },
    Handle { key: KeyRef, out: bool, base: ([f64; 2], [f64; 2]), moved: bool },
    Box { from: DVec2, to: DVec2, add: bool },
    Pan { from: DVec2, view: (f64, f64, f64, f64) },
    Scrub,
}

/// A modal gesture: the keys follow the pointer until a click (or Enter)
/// takes it, Esc or a right click puts them back.
#[derive(Clone, Debug)]
enum Modal {
    Grab { from: DVec2, base: Vec<(KeyRef, f64, f64)> },
    Scale { from: DVec2, pivot: f64, base: Vec<(KeyRef, f64, f64)> },
    Rotate { from: DVec2, base: Vec<(KeyRef, [f64; 2], [f64; 2])> },
    /// B: the next drag is a box.
    Box,
}

#[derive(Script, ScriptHook, Widget)]
pub struct CurveEditor {
    #[uid]
    uid: WidgetUid,
    #[walk]
    walk: Walk,
    #[redraw]
    #[live]
    draw_bg: DrawColor,
    #[live]
    draw_shape: DrawPanelShape,
    #[live]
    draw_curve: DrawCurve,
    #[live]
    draw_text: DrawText,
    #[live]
    draw_small: DrawText,
    #[live]
    draw_bold: DrawText,
    #[rust]
    channels: Vec<Channel>,
    #[rust]
    hidden: HashSet<String>,
    #[rust]
    locked: HashSet<String>,
    #[rust]
    solo: HashSet<String>,
    #[rust]
    folded: HashSet<String>,
    #[rust]
    filter: String,
    #[rust]
    node_filter: Option<String>,
    #[rust]
    mode: Mode,
    #[rust]
    normalise: bool,
    #[rust]
    snap: Snap,
    #[rust]
    view: Option<(f64, f64, f64, f64)>,
    #[rust]
    selected: HashSet<(String, usize)>,
    #[rust]
    active: Option<String>,
    #[rust]
    hover_channel: Option<usize>,
    #[rust]
    hover_key: Option<(KeyRef, Option<bool>)>,
    #[rust]
    playhead: f64,
    #[rust]
    duration: f64,
    #[rust]
    fps: f64,
    #[rust]
    drag: Option<Drag>,
    #[rust]
    modal: Option<Modal>,
    #[rust]
    menu: Option<Menu>,
    #[rust]
    menu_hot: Option<usize>,
    #[rust]
    clipboard: Vec<(f64, f64, Interp, KeyHandles)>,
    #[rust]
    gestures: u64,
    /// The selected key where a drag of its time or value field began.
    #[rust]
    field_base: Option<Vec<(KeyRef, f64, f64)>>,
    #[rust]
    spots: Vec<(Rect, Spot)>,
    #[rust]
    hot: Option<Spot>,
    #[rust]
    graph: Rect,
    #[rust]
    ruler: Rect,
    #[rust]
    pointer: DVec2,
    #[rust]
    reveal: Option<String>,
    #[rust]
    last_click: Option<(f64, DVec2)>,
    #[rust]
    tip: Option<(DVec2, String)>,
    /// Where each channel's row (and each group's) is in the list: the
    /// dopesheet's rows line up with them.
    #[rust]
    row_y: HashMap<usize, f64>,
    #[rust]
    group_y: Vec<(f64, Vec<usize>)>,
}

const LIST: f64 = 228.0;
const HEADER: f64 = 30.0;
const RULER: f64 = 22.0;
const ROW: f64 = 22.0;
const MENUS: [&str; 4] = ["View", "Select", "Key", "Channel"];

fn color(value: u32, alpha: f32) -> Vec4f {
    let mut c = Vec4f::from_u32(value);
    c.w *= alpha;
    c
}

/// A channel's colour: hues around the wheel, light enough on the dark well.
fn hue(i: u32) -> Vec4f {
    let h = (i as f32 * 0.61803 + 0.02) % 1.0;
    let f = |n: f32| {
        let k = (n + h * 6.0) % 6.0;
        0.86 - 0.6 * (k.min(4.0 - k).min(1.0)).max(0.0)
    };
    Vec4f { x: f(5.0), y: f(3.0), z: f(1.0), w: 1.0 }
}

fn interp_name(i: &Interp) -> String {
    match i {
        Interp::Constant => "constant".into(),
        Interp::Linear => "linear".into(),
        Interp::Bezier => "bezier".into(),
        Interp::Ease(e) => e.replace('_', " "),
    }
}

fn handle_name(m: HandleMode) -> &'static str {
    match m {
        HandleMode::Auto => "auto",
        HandleMode::Clamped => "auto-clamped",
        HandleMode::Aligned => "aligned",
        HandleMode::Free => "free",
        HandleMode::Vector => "vector",
    }
}

const EASES: [&str; 10] = ["ease_in_out", "ease_out", "ease_in", "ease_in_out_cubic", "ease_out_cubic", "ease_in_cubic", "ease_out_back", "ease_out_expo", "ease_out_elastic", "ease_out_bounce"];

impl CurveEditor {
    pub fn set_channels(&mut self, cx: &mut Cx, channels: Vec<Channel>, playhead: f64, duration: f64, fps: f64) {
        let first = self.channels.is_empty();
        self.channels = channels;
        self.playhead = playhead;
        self.duration = duration.max(0.1);
        self.fps = if fps > 0.0 { fps } else { 60.0 };
        let ids: HashSet<String> = self.channels.iter().map(|c| c.id.clone()).collect();
        let counts: HashMap<String, usize> = self.channels.iter().map(|c| (c.id.clone(), c.track().0.keys.len())).collect();
        self.selected.retain(|(id, k)| ids.contains(id) && counts.get(id).is_some_and(|n| k < n));
        if self.view.is_none() || first {
            self.frame(false);
        }
        if let Some(path) = self.reveal.take() {
            self.reveal_channel(&path);
        }
        self.say(cx);
        self.draw_bg.redraw(cx);
    }

    pub fn set_playhead(&mut self, cx: &mut Cx, t: f64) {
        if (self.playhead - t).abs() > 1e-9 {
            self.playhead = t;
            self.draw_bg.redraw(cx);
        }
    }

    pub fn set_filter(&mut self, cx: &mut Cx, filter: String) {
        self.filter = filter.to_lowercase();
        self.draw_bg.redraw(cx);
    }

    pub fn set_node_filter(&mut self, cx: &mut Cx, node: Option<String>) {
        if self.node_filter != node {
            self.node_filter = node;
            self.draw_bg.redraw(cx);
        }
    }

    /// Shown once the channels are read again (the byte is in the text
    /// that just changed).
    pub fn reveal(&mut self, cx: &mut Cx, at: String) {
        self.reveal = Some(at);
        self.draw_bg.redraw(cx);
    }

    fn reveal_channel(&mut self, at: &str) {
        let Some((doc, byte)) = at.split_once(':').and_then(|(d, b)| Some((d.parse::<usize>().ok()?, b.parse::<usize>().ok()?))) else { return };
        if let Some(ch) = self.channels.iter().find(|c| c.doc == doc && c.keys.call.start <= byte && byte <= c.keys.call.end) {
            let id = ch.id.clone();
            self.hidden.remove(&id);
            self.folded.remove(&ch.group);
            self.node_filter = None;
            self.active = Some(id.clone());
            self.selected = (0..ch.track().0.keys.len()).map(|k| (id.clone(), k)).collect();
            self.frame(true);
        }
    }

    fn visible(&self, i: usize) -> bool {
        let c = &self.channels[i];
        if self.hidden.contains(&c.id) {
            return false;
        }
        if !self.solo.is_empty() && !self.solo.contains(&c.id) {
            return false;
        }
        self.listed(i)
    }

    fn listed(&self, i: usize) -> bool {
        let c = &self.channels[i];
        if let Some(node) = &self.node_filter {
            if !c.node.as_deref().is_some_and(|n| n == node || n.starts_with(&format!("{node}."))) {
                return false;
            }
        }
        self.filter.is_empty() || c.label.to_lowercase().contains(&self.filter) || c.group.to_lowercase().contains(&self.filter)
    }

    fn active_index(&self) -> Option<usize> {
        self.active.as_ref().and_then(|a| self.channels.iter().position(|c| &c.id == a))
    }

    fn norm(&self, i: usize, v: f64) -> f64 {
        if !self.normalise {
            return v;
        }
        let (lo, hi) = self.channels[i].range();
        (v - lo) / (hi - lo)
    }

    fn denorm(&self, i: usize, n: f64) -> f64 {
        if !self.normalise {
            return n;
        }
        let (lo, hi) = self.channels[i].range();
        lo + n * (hi - lo)
    }

    fn denorm_dv(&self, i: usize, dv: f64) -> f64 {
        if !self.normalise {
            return dv;
        }
        let (lo, hi) = self.channels[i].range();
        dv * (hi - lo)
    }

    /// Frame the visible keys (or the selected ones) with a margin; a
    /// channel of one key, or keys close together, is seen over the scene.
    fn frame(&mut self, selected: bool) {
        let mut t = (f64::MAX, f64::MIN);
        let mut v = (f64::MAX, f64::MIN);
        for i in 0..self.channels.len() {
            if !self.visible(i) {
                continue;
            }
            let (track, _) = self.channels[i].track();
            for (k, key) in track.keys.iter().enumerate() {
                if selected && !self.selected.contains(&(self.channels[i].id.clone(), k)) {
                    continue;
                }
                let nv = self.norm(i, key.v[0]);
                t = (t.0.min(key.t.value), t.1.max(key.t.value));
                v = (v.0.min(nv), v.1.max(nv));
            }
        }
        if t.0 > t.1 {
            self.view = Some((-0.05 * self.duration, self.duration * 1.05, -0.2, 1.2));
            return;
        }
        let span = t.1 - t.0;
        let (t0, t1) = if span < 1.0 { (0.0f64.min(t.0), self.duration.max(t.1).max(2.0)) } else { (t.0, t.1) };
        let tm = (t1 - t0) * 0.06;
        let vspan = v.1 - v.0;
        let vm = if vspan < 1e-6 { (v.0.abs() * 0.5).max(if self.normalise { 0.2 } else { 0.5 }) } else { vspan * 0.15 };
        self.view = Some((t0 - tm, t1 + tm, v.0 - vm, v.1 + vm));
    }

    fn view(&self) -> (f64, f64, f64, f64) {
        self.view.unwrap_or((0.0, self.duration.max(1.0), 0.0, 1.0))
    }

    fn x_of(&self, t: f64) -> f64 {
        let (t0, t1, _, _) = self.view();
        self.graph.pos.x + (t - t0) / (t1 - t0).max(1e-9) * self.graph.size.x
    }

    fn t_of(&self, x: f64) -> f64 {
        let (t0, t1, _, _) = self.view();
        t0 + (x - self.graph.pos.x) / self.graph.size.x.max(1.0) * (t1 - t0)
    }

    fn y_of(&self, v: f64) -> f64 {
        let (_, _, v0, v1) = self.view();
        self.graph.pos.y + self.graph.size.y - (v - v0) / (v1 - v0).max(1e-12) * self.graph.size.y
    }

    fn v_of(&self, y: f64) -> f64 {
        let (_, _, v0, v1) = self.view();
        v0 + (self.graph.pos.y + self.graph.size.y - y) / self.graph.size.y.max(1.0) * (v1 - v0)
    }

    fn sec_per_px(&self) -> f64 {
        let (t0, t1, _, _) = self.view();
        (t1 - t0) / self.graph.size.x.max(1.0)
    }

    fn val_per_px(&self) -> f64 {
        let (_, _, v0, v1) = self.view();
        (v1 - v0) / self.graph.size.y.max(1.0)
    }

    fn snap_t(&self, t: f64, free: bool) -> f64 {
        if free || self.snap == Snap::Off {
            return t;
        }
        if self.snap == Snap::Frames {
            return (t * self.fps).round() / self.fps;
        }
        let reach = 8.0 * self.sec_per_px();
        let cands: Vec<f64> = match self.snap {
            Snap::Beats => makepad_value_pickers::panel::host::beats(t - reach, t + reach),
            Snap::Words => makepad_value_pickers::panel::host::words(t - reach, t + reach).into_iter().map(|w| w.0).collect(),
            _ => Vec::new(),
        };
        cands.into_iter().filter(|c| (c - t).abs() < reach).min_by(|a, b| (a - t).abs().total_cmp(&(b - t).abs())).unwrap_or((t * self.fps).round() / self.fps)
    }

    fn key_pos(&self, i: usize, t: f64, v: f64) -> DVec2 {
        if self.mode == Mode::Graph {
            dvec2(self.x_of(t), self.y_of(self.norm(i, v)))
        } else {
            dvec2(self.x_of(t), self.sheet_row_y(i))
        }
    }

    fn hit_key(&self, p: DVec2) -> Option<(KeyRef, Option<bool>)> {
        let mut best: Option<(f64, KeyRef, Option<bool>)> = None;
        for i in (0..self.channels.len()).filter(|&i| self.visible(i)) {
            let ch = &self.channels[i];
            let (track, _) = ch.track();
            let handles = lane::handles(&ch.keys);
            for (k, key) in track.keys.iter().enumerate() {
                let pos = self.key_pos(i, key.t.value, key.v[0]);
                let d = (pos - p).length();
                if d < 8.0 && best.as_ref().is_none_or(|b| d < b.0) {
                    best = Some((d, (i, k), None));
                }
                if self.mode == Mode::Graph && self.selected.contains(&(ch.id.clone(), k)) {
                    if let Some((hin, hout)) = handles.get(k) {
                        for (out, h) in [(false, hin), (true, hout)] {
                            if h[0].abs() < 1e-9 || !handle_shown(&track, k, out) {
                                continue;
                            }
                            let hp = dvec2(self.x_of(key.t.value + h[0]), self.y_of(self.norm(i, key.v[0] + h[1])));
                            let d = (hp - p).length();
                            if d < 7.0 && best.as_ref().is_none_or(|b| d < b.0) {
                                best = Some((d, (i, k), Some(out)));
                            }
                        }
                    }
                }
            }
        }
        best.map(|(_, k, h)| (k, h))
    }

    fn near_curve(&self, p: DVec2, reach: f64) -> Option<(usize, f64, f64)> {
        let t = self.t_of(p.x);
        let mut best: Option<(f64, usize, f64)> = None;
        for i in (0..self.channels.len()).filter(|&i| self.visible(i)) {
            let Some(v) = lane::sample(&self.channels[i].keys, t) else { continue };
            let y = if self.mode == Mode::Graph { self.y_of(self.norm(i, v)) } else { self.sheet_row_y(i) };
            let d = (y - p.y).abs();
            if d < reach && best.as_ref().is_none_or(|b| d < b.0) {
                best = Some((d, i, v));
            }
        }
        best.map(|(_, i, v)| (i, t, v))
    }

    /// The channels with a row in the dopesheet: shown, and in an open group.
    fn sheet_rows(&self) -> Vec<usize> {
        (0..self.channels.len()).filter(|&i| self.visible(i) && self.row_y.contains_key(&i)).collect()
    }

    /// A channel's row centre in the dopesheet: level with its list row.
    fn sheet_row_y(&self, i: usize) -> f64 {
        self.row_y.get(&i).map_or(-1000.0, |y| y + (ROW - 2.0) * 0.5)
    }

    fn selected_refs(&self) -> Vec<KeyRef> {
        let mut out = Vec::new();
        for (i, ch) in self.channels.iter().enumerate() {
            let n = ch.track().0.keys.len();
            for k in 0..n {
                if self.selected.contains(&(ch.id.clone(), k)) {
                    out.push((i, k));
                }
            }
        }
        out
    }

    fn say(&self, cx: &mut Cx) {
        let sel = self.selected_refs();
        let (text, key) = match sel.len() {
            0 if self.channels.is_empty() => ("No animated values yet. Hover a value in the panel and press I to key it.".to_string(), None),
            0 => ("Click a key or drag a box · I keys at the playhead · Ctrl+click on a curve adds a key · right click: Key menu".to_string(), None),
            1 => {
                let (i, k) = sel[0];
                let pts = self.points(i);
                let p = &pts[k];
                let next = pts.get(k + 1).map(|n| interp_name(&n.2)).unwrap_or_else(|| "—".into());
                (format!("{} · key {} of {} · out: {} · handles: {}", self.channels[i].label, k + 1, pts.len(), next, handle_name(p.3.mode)), Some((p.0, p.1)))
            }
            n => (format!("{n} keys selected · G grab · S scale · T interpolation · V handles · X delete"), None),
        };
        cx.widget_action(self.widget_uid(), CurveAction::Status { text, key });
    }

    /// The time or value of the one selected key from its field: a drag's
    /// steps held live (numbers patched in the running film), `done` its
    /// release or a typed entry, one undo step.
    pub fn set_key_field(&mut self, cx: &mut Cx, t: Option<f64>, v: Option<f64>, done: bool) {
        if self.selected_refs().len() != 1 {
            self.field_base = None;
            return;
        }
        if self.field_base.is_none() && done {
            // A typed entry (or a step): the key moves there, past its
            // neighbours if it must (the keys stay in time order).
            self.edit_selected(cx, move |pts, ks, _| {
                for &k in ks {
                    if let Some(t) = t {
                        pts[k].0 = t;
                    }
                    if let Some(v) = v {
                        pts[k].1 = v;
                    }
                }
                pts.sort_by(|a, b| a.0.total_cmp(&b.0));
                None
            });
            self.say(cx);
            return;
        }
        if self.field_base.is_none() {
            self.field_base = Some(self.base());
            self.next_gesture();
        }
        let Some(base) = self.field_base.clone() else { return };
        let Some(&((i, _), t0, v0)) = base.first() else { return };
        let dt = t.map_or(0.0, |t| t - t0);
        let dv = v.map_or(0.0, |v| v - v0);
        // The graph's units: a normalised channel moves in parts of its range.
        let dv_n = if self.normalise {
            let (lo, hi) = self.channels[i].range();
            if hi > lo { dv / (hi - lo) } else { dv }
        } else {
            dv
        };
        self.move_keys(cx, &base, dt, dv_n, done);
        if done {
            self.field_base = None;
            self.say(cx);
        }
    }

    // ------------------------------------------------------------ writing

    fn rewrite(&self, i: usize, points: &[(f64, f64, Interp, KeyHandles)], before: Extrap, after: Extrap) -> Edit {
        let ch = &self.channels[i];
        let text = ch.text.as_str();
        let n = |x: f64| makepad_motion::patch::fmt_num((x * 1e4).round() / 1e4);
        let mut parts: Vec<(f64, String)> = points.iter().map(|(t, v, interp, h)| (*t, lane::key_text(&n(*t), &n(*v), interp, h))).collect();
        // Keys written as code (a beat, a sum) keep their place by time.
        let (_, idx) = ch.track();
        for (k, key) in ch.keys.keys.iter().enumerate() {
            if idx.contains(&k) {
                continue;
            }
            parts.push((key.t.unwrap_or(f64::MAX), text[key.bracket.clone()].to_string()));
        }
        parts.sort_by(|a, b| a.0.total_cmp(&b.0));
        let mut all: Vec<String> = parts.into_iter().map(|(_, s)| s).collect();
        if before != Extrap::Constant || after != Extrap::Constant {
            all.push(extrap_text(before, after));
        }
        let call = if text[ch.keys.call.clone()].contains('\n') {
            let indent: String = ch.keys.keys.first().map(|k| {
                let ls = text[..k.bracket.start].rfind('\n').map_or(0, |p| p + 1);
                text[ls..k.bracket.start].to_string()
            }).unwrap_or_else(|| "    ".into());
            let close_ls = text[..ch.keys.call.end - 1].rfind('\n').map_or(0, |p| p + 1);
            let close_indent: String = text[close_ls..ch.keys.call.end - 1].chars().take_while(|c| c.is_whitespace()).collect();
            format!("keys(\n{indent}{}\n{close_indent})", all.join(&format!(",\n{indent}")))
        } else {
            format!("keys({})", all.join(", "))
        };
        Edit::splice(ch.keys.call.start..ch.keys.call.end, call, 0, false)
    }

    fn points(&self, i: usize) -> Vec<(f64, f64, Interp, KeyHandles)> {
        let ch = &self.channels[i];
        let (track, idx) = ch.track();
        track.keys.iter().zip(idx).map(|(k, ki)| (k.t.value, k.v[0], ch.keys.keys[ki].interp.clone(), ch.keys.keys[ki].handles)).collect()
    }

    /// Send edits as one gesture (`done`: its last step, in each file).
    fn send(&mut self, cx: &mut Cx, edits: Vec<(usize, Edit)>, gesture: u64, done: bool) {
        // One edit per file, its splices from the end (they apply in one pass).
        let mut by_doc: Vec<(usize, Vec<Change>)> = Vec::new();
        for (doc, e) in edits {
            match by_doc.iter_mut().find(|(d, _)| *d == doc) {
                Some((_, v)) => v.extend(e.changes),
                None => by_doc.push((doc, e.changes)),
            }
        }
        by_doc.sort_by_key(|(doc, _)| *doc);
        let start = |c: &Change| if let Change::Splice(s) = c { s.range.start } else { 0 };
        for (doc, mut changes) in by_doc {
            changes.sort_by(|a, b| start(b).cmp(&start(a)));
            cx.widget_action(self.widget_uid(), CurveAction::Edits { doc, edits: vec![Edit { changes, gesture, done }] });
        }
    }

    fn next_gesture(&mut self) -> u64 {
        self.gestures += 1;
        u64::MAX / 16 + self.gestures
    }

    /// Each channel with selected keys changed by `f` (its points, the
    /// selected keys' indices; it may give new extrapolation), one step.
    fn edit_selected(&mut self, cx: &mut Cx, f: impl Fn(&mut Vec<(f64, f64, Interp, KeyHandles)>, &[usize], usize) -> Option<(Extrap, Extrap)>) {
        let sel = self.selected_refs();
        let mut chans: Vec<usize> = sel.iter().map(|s| s.0).collect::<HashSet<_>>().into_iter().collect();
        chans.sort();
        let mut edits = Vec::new();
        for i in chans {
            if self.locked.contains(&self.channels[i].id) {
                continue;
            }
            let mut pts = self.points(i);
            let ks: Vec<usize> = sel.iter().filter(|s| s.0 == i).map(|s| s.1).collect();
            let (mut b, mut a) = (self.channels[i].keys.before, self.channels[i].keys.after);
            if let Some((nb, na)) = f(&mut pts, &ks, i) {
                b = nb;
                a = na;
            }
            if pts.is_empty() {
                continue;
            }
            edits.push((self.channels[i].doc, self.rewrite(i, &pts, b, a)));
        }
        if !edits.is_empty() {
            let g = self.next_gesture();
            self.send(cx, edits, g, true);
        }
    }

    /// Keys on channels at times (with values, or on the curve): the key
    /// there takes the value, else a new one carrying on its segment.
    fn insert(&mut self, cx: &mut Cx, keys_at: Vec<(usize, f64, Option<f64>)>) {
        let mut edits = Vec::new();
        let half = 0.5 / self.fps;
        let mut new_sel = Vec::new();
        for (i, t, v) in keys_at {
            let ch = &self.channels[i];
            if self.locked.contains(&ch.id) {
                continue;
            }
            let Some(v) = v.or_else(|| lane::sample(&ch.keys, t)) else { continue };
            let mut pts = self.points(i);
            match pts.iter_mut().find(|p| (p.0 - t).abs() < half) {
                Some(p) => p.1 = v,
                None => {
                    let interp = pts.iter().find(|p| p.0 > t).map(|p| p.2.clone()).unwrap_or(Interp::Bezier);
                    let bezier = interp == Interp::Bezier;
                    pts.push((t, v, interp, KeyHandles { bezier, ..Default::default() }));
                    pts.sort_by(|a, b| a.0.total_cmp(&b.0));
                }
            }
            if let Some(k) = pts.iter().position(|p| (p.0 - t).abs() < half) {
                new_sel.push((ch.id.clone(), k));
            }
            let (b, a) = (ch.keys.before, ch.keys.after);
            edits.push((ch.doc, self.rewrite(i, &pts, b, a)));
        }
        if !edits.is_empty() {
            self.selected = new_sel.into_iter().collect();
            let g = self.next_gesture();
            self.send(cx, edits, g, true);
        }
    }

    /// I: the selected channels (else the hovered or active one, else every
    /// one shown) keyed at the playhead from the curve's value.
    fn insert_at_playhead(&mut self, cx: &mut Cx) {
        let mut chans: Vec<usize> = self.selected_refs().iter().map(|s| s.0).collect::<HashSet<_>>().into_iter().collect();
        if chans.is_empty() {
            chans = self.hover_channel.or_else(|| self.active_index()).into_iter().collect();
        }
        if chans.is_empty() {
            chans = (0..self.channels.len()).filter(|&i| self.visible(i)).collect();
        }
        let t = (self.playhead * self.fps).round() / self.fps;
        self.insert(cx, chans.into_iter().map(|i| (i, t, None)).collect());
    }

    fn delete_selected(&mut self, cx: &mut Cx) {
        self.edit_selected(cx, |pts, ks, _| {
            // A curve keeps one key at least.
            if ks.len() < pts.len() {
                let keep: Vec<_> = pts.iter().enumerate().filter(|(k, _)| !ks.contains(k)).map(|(_, p)| p.clone()).collect();
                *pts = keep;
            }
            None
        });
        self.selected.clear();
    }

    fn copy(&mut self) {
        let sel = self.selected_refs();
        let Some(&(i, _)) = sel.first() else { return };
        let pts = self.points(i);
        let t0 = sel.iter().filter(|s| s.0 == i).map(|s| pts[s.1].0).fold(f64::MAX, f64::min);
        self.clipboard = sel.iter().filter(|s| s.0 == i).map(|s| {
            let p = &pts[s.1];
            (p.0 - t0, p.1, p.2.clone(), p.3)
        }).collect();
    }

    fn paste_at(&mut self, cx: &mut Cx, at: f64) {
        if self.clipboard.is_empty() {
            return;
        }
        let Some(i) = self.active_index().or_else(|| self.selected_refs().first().map(|s| s.0)) else { return };
        if self.locked.contains(&self.channels[i].id) {
            return;
        }
        let mut pts = self.points(i);
        let half = 0.5 / self.fps;
        let mut times = Vec::new();
        for (dt, v, interp, h) in self.clipboard.clone() {
            let t = at + dt;
            pts.retain(|p| (p.0 - t).abs() >= half);
            pts.push((t, v, interp, h));
            times.push(t);
        }
        pts.sort_by(|a, b| a.0.total_cmp(&b.0));
        let id = self.channels[i].id.clone();
        self.selected = pts.iter().enumerate().filter(|(_, p)| times.iter().any(|t| (p.0 - t).abs() < half)).map(|(k, _)| (id.clone(), k)).collect();
        let (b, a) = (self.channels[i].keys.before, self.channels[i].keys.after);
        let edit = (self.channels[i].doc, self.rewrite(i, &pts, b, a));
        let g = self.next_gesture();
        self.send(cx, vec![edit], g, true);
    }

    fn base(&self) -> Vec<(KeyRef, f64, f64)> {
        self.selected_refs().into_iter().map(|(i, k)| {
            let pts = self.points(i);
            ((i, k), pts[k].0, pts[k].1)
        }).collect()
    }

    /// The keys moved by (dt, dv in the graph's units), each kept between
    /// neighbours that do not move: numbers only (live in the film).
    fn move_keys(&mut self, cx: &mut Cx, base: &[(KeyRef, f64, f64)], dt: f64, dv_n: f64, done: bool) {
        let mut edits = Vec::new();
        let n = |x: f64| makepad_motion::patch::fmt_num((x * 1e4).round() / 1e4);
        for &((i, k), t, v) in base {
            let ch = &self.channels[i];
            if self.locked.contains(&ch.id) {
                continue;
            }
            let key = &ch.keys.keys[ch.track().1[k]];
            let pts = self.points(i);
            let lo = (0..k).rev().find(|&j| !base.iter().any(|b| b.0 == (i, j))).map_or(f64::MIN, |j| pts[j].0 + 1e-3);
            let hi = (k + 1..pts.len()).find(|&j| !base.iter().any(|b| b.0 == (i, j))).map_or(f64::MAX, |j| pts[j].0 - 1e-3);
            let nt = (t + dt).clamp(lo, hi);
            let nv = v + self.denorm_dv(i, dv_n);
            if key.t.is_some() {
                edits.push((ch.doc, Edit::splice(key.t_range.start..key.t_range.end, n(nt), 0, false)));
            }
            edits.push((ch.doc, Edit::splice(key.v_range.start..key.v_range.end, n(nv), 0, false)));
        }
        let g = u64::MAX / 16 + self.gestures;
        self.send(cx, edits, g, done);
    }

    fn scale_keys(&mut self, cx: &mut Cx, base: &[(KeyRef, f64, f64)], pivot: f64, f: f64, done: bool) {
        let mut edits = Vec::new();
        let n = |x: f64| makepad_motion::patch::fmt_num((x * 1e4).round() / 1e4);
        for &((i, k), t, _) in base {
            let ch = &self.channels[i];
            if self.locked.contains(&ch.id) {
                continue;
            }
            let key = &ch.keys.keys[ch.track().1[k]];
            if key.t.is_some() {
                edits.push((ch.doc, Edit::splice(key.t_range.start..key.t_range.end, n(pivot + (t - pivot) * f), 0, false)));
            }
        }
        let g = u64::MAX / 16 + self.gestures;
        self.send(cx, edits, g, done);
    }

    /// The keys' handles turned by `angle` (radians, on screen), aligned.
    fn rotate_handles(&mut self, cx: &mut Cx, base: &[(KeyRef, [f64; 2], [f64; 2])], angle: f64, done: bool) {
        let mut by: HashMap<usize, Vec<(usize, [f64; 2], [f64; 2])>> = HashMap::new();
        for (r, i, o) in base {
            by.entry(r.0).or_default().push((r.1, *i, *o));
        }
        let (spp, vpp) = (self.sec_per_px(), self.val_per_px());
        let turn = |h: [f64; 2]| {
            let (x, y) = (h[0] / spp, -h[1] / vpp);
            let (s, c) = angle.sin_cos();
            let (nx, ny) = (x * c - y * s, x * s + y * c);
            [nx * spp, -ny * vpp]
        };
        let mut edits = Vec::new();
        for (i, ks) in by {
            if self.locked.contains(&self.channels[i].id) {
                continue;
            }
            let mut pts = self.points(i);
            for (k, hin, hout) in ks {
                let (ni, no) = (turn(hin), turn(hout));
                let h = &mut pts[k].3;
                h.mode = HandleMode::Aligned;
                h.r#in = Some([ni[0].min(0.0), ni[1]]);
                h.out = Some([no[0].max(0.0), no[1]]);
            }
            let (b, a) = (self.channels[i].keys.before, self.channels[i].keys.after);
            edits.push((self.channels[i].doc, self.rewrite(i, &pts, b, a)));
        }
        let g = u64::MAX / 16 + self.gestures;
        self.send(cx, edits, g, done);
    }

    fn run(&mut self, cx: &mut Cx, cmd: Cmd) {
        self.menu = None;
        match cmd {
            Cmd::Mode(m) => self.mode = m,
            Cmd::FrameAll => self.frame(false),
            Cmd::FrameSelected => self.frame(!self.selected.is_empty()),
            Cmd::Normalise => {
                self.normalise = !self.normalise;
                self.frame(false);
            }
            Cmd::SelectAll | Cmd::SelectInvert => {
                let all: HashSet<(String, usize)> = (0..self.channels.len()).filter(|&i| self.visible(i)).flat_map(|i| {
                    let id = self.channels[i].id.clone();
                    (0..self.channels[i].track().0.keys.len()).map(move |k| (id.clone(), k))
                }).collect();
                self.selected = if cmd == Cmd::SelectAll { all } else { all.difference(&self.selected).cloned().collect() };
            }
            Cmd::SelectNone => self.selected.clear(),
            Cmd::BoxSelect => self.modal = Some(Modal::Box),
            Cmd::Insert => self.insert_at_playhead(cx),
            Cmd::InsertAt(i, t, v) => {
                let t = self.snap_t(t, false);
                self.active = Some(self.channels[i].id.clone());
                self.insert(cx, vec![(i, t, Some(v))]);
            }
            Cmd::Delete => self.delete_selected(cx),
            Cmd::Copy => self.copy(),
            Cmd::Paste => self.paste_at(cx, self.playhead),
            Cmd::Duplicate => {
                // A copy right after the selection.
                self.copy();
                let times: Vec<f64> = self.selected_refs().iter().map(|&(i, k)| self.points(i)[k].0).collect();
                if let (Some(start), Some(end)) = (times.iter().cloned().reduce(f64::min), times.iter().cloned().reduce(f64::max)) {
                    let at = end + ((end - start) * 0.25).max(4.0 / self.fps);
                    self.paste_at(cx, at);
                }
            }
            Cmd::Grab | Cmd::Scale => {
                let base = self.base();
                if !base.is_empty() {
                    self.next_gesture();
                    self.modal = Some(if cmd == Cmd::Grab { Modal::Grab { from: self.pointer, base } } else { Modal::Scale { from: self.pointer, pivot: self.playhead, base } });
                }
            }
            Cmd::Rotate => {
                let base: Vec<_> = self.selected_refs().into_iter().filter_map(|(i, k)| {
                    let (hin, hout) = *lane::handles(&self.channels[i].keys).get(k)?;
                    Some(((i, k), hin, hout))
                }).collect();
                if !base.is_empty() {
                    self.next_gesture();
                    self.modal = Some(Modal::Rotate { from: self.pointer, base });
                }
            }
            Cmd::Interp(interp) => {
                // The outgoing segment of each selected key: the next key's.
                self.edit_selected(cx, move |pts, ks, _| {
                    for &k in ks {
                        if let Some(next) = pts.get_mut(k + 1) {
                            next.2 = interp.clone();
                            next.3.bezier = interp == Interp::Bezier;
                        }
                    }
                    None
                });
            }
            Cmd::Handle(mode) => {
                let drawn: HashMap<KeyRef, ([f64; 2], [f64; 2])> = self.selected_refs().into_iter().filter_map(|(i, k)| Some(((i, k), *lane::handles(&self.channels[i].keys).get(k)?))).collect();
                self.edit_selected(cx, move |pts, ks, i| {
                    for &k in ks {
                        let (hin, hout) = drawn.get(&(i, k)).copied().unwrap_or(([0.0, 0.0], [0.0, 0.0]));
                        let p = &mut pts[k];
                        p.3.mode = mode;
                        p.3.r#in = mode.written().then_some(hin);
                        p.3.out = mode.written().then_some(hout);
                        // Handles shape bezier segments: the ones around it are.
                        if k > 0 && p.2 != Interp::Bezier {
                            p.2 = Interp::Bezier;
                            p.3.bezier = true;
                        }
                        if let Some(next) = pts.get_mut(k + 1) {
                            if next.2 != Interp::Bezier {
                                next.2 = Interp::Bezier;
                                next.3.bezier = true;
                            }
                        }
                    }
                    None
                });
            }
            Cmd::Extrap(e) => {
                // Per channel: the selected keys' channels, else the active one.
                if self.selected.is_empty() {
                    if let Some(i) = self.active_index() {
                        let id = self.channels[i].id.clone();
                        self.selected.insert((id, 0));
                    }
                }
                self.edit_selected(cx, move |_, _, _| Some((e, e)));
            }
            Cmd::Snap(s) => self.snap = s,
            Cmd::ShowAll => {
                self.hidden.clear();
                self.solo.clear();
            }
            Cmd::HideOthers => {
                if let Some(a) = self.active.clone() {
                    self.hidden = self.channels.iter().map(|c| c.id.clone()).filter(|id| *id != a).collect();
                }
            }
            Cmd::LockToggle | Cmd::SoloToggle => {
                if let Some(a) = self.active.clone() {
                    let set = if cmd == Cmd::LockToggle { &mut self.locked } else { &mut self.solo };
                    if !set.remove(&a) {
                        set.insert(a);
                    }
                }
            }
        }
        self.say(cx);
        self.draw_bg.redraw(cx);
    }

    fn open_menu(&mut self, at: DVec2, which: &str) {
        fn s(a: &str, b: &str, c: Cmd) -> (String, String, Option<Cmd>) {
            (a.to_string(), b.to_string(), Some(c))
        }
        fn head(a: &str, b: &str) -> (String, String, Option<Cmd>) {
            (a.to_string(), b.to_string(), None)
        }
        let interp = || -> Vec<(String, String, Option<Cmd>)> {
            let mut v = vec![s("Constant", "", Cmd::Interp(Interp::Constant)), s("Linear", "", Cmd::Interp(Interp::Linear)), s("Bezier", "", Cmd::Interp(Interp::Bezier)), head("Easing", "")];
            for e in EASES {
                v.push(s(&format!("   {}", e.replace('_', " ")), "", Cmd::Interp(Interp::Ease(e.to_string()))));
            }
            v
        };
        let handles = || -> Vec<(String, String, Option<Cmd>)> {
            [HandleMode::Auto, HandleMode::Clamped, HandleMode::Vector, HandleMode::Aligned, HandleMode::Free].into_iter().map(|m| s(handle_name(m), "", Cmd::Handle(m))).collect()
        };
        let extrap = || -> Vec<(String, String, Option<Cmd>)> {
            [Extrap::Constant, Extrap::Linear, Extrap::Cycle].into_iter().map(|e| s(e.name(), "", Cmd::Extrap(e))).collect()
        };
        let (title, items): (&str, Vec<(String, String, Option<Cmd>)>) = match which {
            "View" => ("View", vec![
                s("Frame all", "A · Home", Cmd::FrameAll),
                s("Frame selected", "F", Cmd::FrameSelected),
                s(if self.normalise { "Normalise ✓" } else { "Normalise" }, "", Cmd::Normalise),
                s(if self.mode == Mode::Graph { "Graph ✓" } else { "Graph" }, "Ctrl+Tab", Cmd::Mode(Mode::Graph)),
                s(if self.mode == Mode::Dopesheet { "Dopesheet ✓" } else { "Dopesheet" }, "Ctrl+Tab", Cmd::Mode(Mode::Dopesheet)),
            ]),
            "Select" => ("Select", vec![
                s("All", "Ctrl+A", Cmd::SelectAll),
                s("None", "Alt+A", Cmd::SelectNone),
                s("Invert", "Ctrl+I", Cmd::SelectInvert),
                s("Box select", "B · drag", Cmd::BoxSelect),
            ]),
            "Key" | "Context" => {
                let mut v = Vec::new();
                if which == "Context" {
                    let here = self.near_curve(self.pointer, 24.0).or_else(|| {
                        let i = self.hover_channel.or_else(|| self.active_index())?;
                        let t = self.t_of(self.pointer.x);
                        let vv = if self.mode == Mode::Graph { self.denorm(i, self.v_of(self.pointer.y)) } else { lane::sample(&self.channels[i].keys, t)? };
                        Some((i, t, vv))
                    });
                    if let Some((i, t, vv)) = here {
                        v.push(s("Insert key here", "Ctrl+click", Cmd::InsertAt(i, t, vv)));
                    }
                }
                v.extend([
                    s("Insert key at playhead", "I", Cmd::Insert),
                    s("Delete", "X · Del", Cmd::Delete),
                    s("Duplicate", "Shift+D", Cmd::Duplicate),
                    s("Copy", "Ctrl+C", Cmd::Copy),
                    s("Paste at playhead", "Ctrl+V", Cmd::Paste),
                    s("Grab", "G", Cmd::Grab),
                    s("Scale in time", "S", Cmd::Scale),
                    s("Rotate handles", "R", Cmd::Rotate),
                    head("Interpolation…", "T"),
                    head("Handle type…", "V"),
                    head("Extrapolation…", "Shift+E"),
                ]);
                ("Key", v)
            }
            "Channel" => ("Channel", vec![
                s("Show all", "Alt+H", Cmd::ShowAll),
                s("Hide others", "Shift+H", Cmd::HideOthers),
                s("Lock / unlock", "", Cmd::LockToggle),
                s("Solo / unsolo", "", Cmd::SoloToggle),
                head("Extrapolation…", "Shift+E"),
            ]),
            "Interp" => ("Interpolation", interp()),
            "Handle" => ("Handle type", handles()),
            "Extrap" => ("Extrapolation", extrap()),
            "Snap" => ("Snap to", [(Snap::Off, "off"), (Snap::Frames, "frames"), (Snap::Beats, "beats"), (Snap::Words, "sung words")].into_iter().map(|(sn, l)| s(&if self.snap == sn { format!("{l} ✓") } else { l.to_string() }, if sn == Snap::Off { "Ctrl held" } else { "" }, Cmd::Snap(sn))).collect()),
            _ => ("", Vec::new()),
        };
        self.menu = Some(Menu { at, title: title.to_string(), items });
        self.menu_hot = None;
    }

    fn handle_key(&mut self, cx: &mut Cx, ke: &KeyEvent) -> bool {
        let cmd = ke.modifiers.logo || ke.modifiers.control;
        let (shift, alt) = (ke.modifiers.shift, ke.modifiers.alt);
        let p = self.pointer;
        let c = match ke.key_code {
            KeyCode::Escape => {
                if self.menu.take().is_some() {
                    self.draw_bg.redraw(cx);
                    return true;
                }
                if self.modal.is_some() {
                    self.cancel_modal(cx);
                    return true;
                }
                Cmd::SelectNone
            }
            KeyCode::ReturnKey if self.modal.is_some() => {
                self.finish_modal(cx, p);
                return true;
            }
            KeyCode::Delete | KeyCode::Backspace => Cmd::Delete,
            KeyCode::KeyX if !cmd => Cmd::Delete,
            KeyCode::KeyI if cmd => Cmd::SelectInvert,
            KeyCode::KeyI => Cmd::Insert,
            KeyCode::KeyA if cmd => Cmd::SelectAll,
            KeyCode::KeyA if alt => Cmd::SelectNone,
            KeyCode::KeyA | KeyCode::Home => Cmd::FrameAll,
            KeyCode::KeyF | KeyCode::NumpadDecimal => Cmd::FrameSelected,
            KeyCode::KeyB => Cmd::BoxSelect,
            KeyCode::KeyC if cmd => Cmd::Copy,
            KeyCode::KeyV if cmd => Cmd::Paste,
            KeyCode::KeyD if shift => Cmd::Duplicate,
            KeyCode::KeyG => Cmd::Grab,
            KeyCode::KeyS => Cmd::Scale,
            KeyCode::KeyR => Cmd::Rotate,
            KeyCode::KeyT | KeyCode::KeyV | KeyCode::KeyE if !cmd && (ke.key_code != KeyCode::KeyE || shift) => {
                let which = match ke.key_code {
                    KeyCode::KeyT => "Interp",
                    KeyCode::KeyV => "Handle",
                    _ => "Extrap",
                };
                self.open_menu(p, which);
                self.draw_bg.redraw(cx);
                return true;
            }
            KeyCode::KeyH if alt => Cmd::ShowAll,
            KeyCode::KeyH if shift => Cmd::HideOthers,
            KeyCode::Tab if cmd => Cmd::Mode(if self.mode == Mode::Graph { Mode::Dopesheet } else { Mode::Graph }),
            _ => return false,
        };
        self.run(cx, c);
        true
    }

    fn modal_step(&mut self, cx: &mut Cx, at: DVec2, constrain: bool, done: bool) {
        let Some(modal) = self.modal.clone() else { return };
        match modal {
            Modal::Grab { from, base } => {
                let d = at - from;
                let (mut dt, mut dv) = (d.x * self.sec_per_px(), -d.y * self.val_per_px());
                if self.mode == Mode::Dopesheet {
                    dv = 0.0;
                }
                if constrain {
                    if d.x.abs() >= d.y.abs() {
                        dv = 0.0
                    } else {
                        dt = 0.0
                    }
                }
                if let Some(&(_, t0, _)) = base.first() {
                    if dt != 0.0 {
                        dt = self.snap_t(t0 + dt, false) - t0;
                    }
                }
                self.move_keys(cx, &base, dt, dv, done);
            }
            Modal::Scale { from, pivot, base } => {
                let px = self.x_of(pivot);
                let f = if (from.x - px).abs() < 4.0 { 1.0 } else { (at.x - px) / (from.x - px) };
                self.scale_keys(cx, &base, pivot, f, done);
            }
            Modal::Rotate { from, base } => {
                let Some(&((i, k), _, _)) = base.first() else { return };
                let pts = self.points(i);
                let c = self.key_pos(i, pts[k].0, pts[k].1);
                let a0 = (from.y - c.y).atan2(from.x - c.x);
                let a1 = (at.y - c.y).atan2(at.x - c.x);
                self.rotate_handles(cx, &base, a1 - a0, done);
            }
            Modal::Box => {}
        }
        if done {
            self.modal = None;
        }
    }

    fn finish_modal(&mut self, cx: &mut Cx, at: DVec2) {
        self.modal_step(cx, at, false, true);
        self.say(cx);
        self.draw_bg.redraw(cx);
    }

    /// Esc or a right click: the keys back where they were.
    fn cancel_modal(&mut self, cx: &mut Cx) {
        if let Some(modal) = self.modal.take() {
            match modal {
                Modal::Grab { base, .. } | Modal::Scale { base, .. } => self.move_keys(cx, &base, 0.0, 0.0, true),
                Modal::Rotate { base, .. } => self.rotate_handles(cx, &base, 0.0, true),
                Modal::Box => {}
            }
        }
        self.draw_bg.redraw(cx);
    }

    /// A key or handle dragged by the pointer (Alt locks an axis).
    fn drag_step(&mut self, cx: &mut Cx, at: DVec2, constrain: bool, free: bool, done: bool) {
        let Some(drag) = self.drag.clone() else { return };
        match drag {
            Drag::Keys { from, base, axis, mut moved } => {
                let d = at - from;
                if d.length() > 3.0 {
                    moved = true;
                }
                if !moved {
                    self.drag = if done { None } else { Some(Drag::Keys { from, base, axis, moved }) };
                    return;
                }
                let axis = if constrain { axis.or(Some(d.x.abs() >= d.y.abs())) } else { None };
                let (mut dt, mut dv) = (d.x * self.sec_per_px(), -d.y * self.val_per_px());
                if self.mode == Mode::Dopesheet || axis == Some(true) {
                    dv = 0.0;
                }
                if axis == Some(false) {
                    dt = 0.0;
                }
                if let Some(&(_, t0, _)) = base.first() {
                    if dt != 0.0 {
                        dt = self.snap_t(t0 + dt, free) - t0;
                    }
                }
                self.move_keys(cx, &base, dt, dv, done);
                self.drag = if done { None } else { Some(Drag::Keys { from, base, axis, moved }) };
            }
            Drag::Handle { key: (i, k), out, base, mut moved } => {
                if !done {
                    moved = true;
                }
                if self.locked.contains(&self.channels[i].id) || !moved {
                    self.drag = if done { None } else { Some(Drag::Handle { key: (i, k), out, base, moved }) };
                    return;
                }
                let mut pts = self.points(i);
                let (kt, kv) = (pts[k].0, pts[k].1);
                let ht = self.t_of(at.x) - kt;
                let hv = self.denorm(i, self.v_of(at.y)) - kv;
                let (spp, vpp) = (self.sec_per_px(), self.val_per_px());
                let h = &mut pts[k].3;
                h.mode = match h.mode {
                    HandleMode::Auto | HandleMode::Clamped => HandleMode::Aligned,
                    HandleMode::Vector => HandleMode::Free,
                    m => m,
                };
                let (bin, bout) = base;
                let mut hin = h.r#in.unwrap_or(bin);
                let mut hout = h.out.unwrap_or(bout);
                if out {
                    hout = [ht.max(0.0), hv];
                } else {
                    hin = [ht.min(0.0), hv];
                }
                if h.mode == HandleMode::Aligned {
                    // On one line on screen, the other keeping its length.
                    let (m, other) = if out { (hout, &mut hin) } else { (hin, &mut hout) };
                    let ol = ((other[0] / spp).powi(2) + (other[1] / vpp).powi(2)).sqrt();
                    let (mx, my) = (m[0] / spp, m[1] / vpp);
                    let ml = (mx * mx + my * my).sqrt().max(1e-9);
                    *other = [-mx / ml * ol * spp, -my / ml * ol * vpp];
                }
                h.r#in = Some(hin);
                h.out = Some(hout);
                if k > 0 {
                    pts[k].2 = Interp::Bezier;
                    pts[k].3.bezier = true;
                }
                if let Some(next) = pts.get_mut(k + 1) {
                    if next.2 != Interp::Bezier {
                        next.2 = Interp::Bezier;
                        next.3.bezier = true;
                    }
                }
                let (b, a) = (self.channels[i].keys.before, self.channels[i].keys.after);
                let edit = (self.channels[i].doc, self.rewrite(i, &pts, b, a));
                let g = u64::MAX / 16 + self.gestures;
                self.send(cx, vec![edit], g, done);
                self.drag = if done { None } else { Some(Drag::Handle { key: (i, k), out, base, moved }) };
            }
            _ => {}
        }
    }

    fn menu_rects(&self, cx: &mut Cx2d, m: &Menu, area: Rect) -> (Rect, Vec<Rect>) {
        let w = m.items.iter().map(|(a, b, _)| makepad_widgets::badge::advance(&self.draw_text, cx, a) + makepad_widgets::badge::advance(&self.draw_small, cx, b) + 48.0).fold(170.0, f64::max);
        let h = 26.0 + m.items.len() as f64 * 20.0 + 6.0;
        let x = m.at.x.min(area.pos.x + area.size.x - w - 4.0).max(area.pos.x + 4.0);
        let y = m.at.y.min(area.pos.y + area.size.y - h - 4.0).max(area.pos.y + 4.0);
        let r = Rect { pos: dvec2(x, y), size: dvec2(w, h) };
        let items = (0..m.items.len()).map(|j| Rect { pos: dvec2(x + 3.0, y + 26.0 + j as f64 * 20.0), size: dvec2(w - 6.0, 19.0) }).collect();
        (r, items)
    }
}

/// Whether a key's in (or out) handle shapes a bezier segment.
fn handle_shown(track: &makepad_motion::model::Track<1>, k: usize, out: bool) -> bool {
    if out {
        track.keys.get(k + 1).is_some_and(|n| n.bez.is_some_and(|h| h.bezier))
    } else {
        track.keys[k].bez.is_some_and(|h| h.bezier)
    }
}

fn extrap_text(before: Extrap, after: Extrap) -> String {
    let mut parts = Vec::new();
    if before != Extrap::Constant {
        parts.push(format!("before: @{}", before.name()));
    }
    if after != Extrap::Constant {
        parts.push(format!("after: @{}", after.name()));
    }
    format!("{{{}}}", parts.join(" "))
}

impl Widget for CurveEditor {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, _scope: &mut Scope) {
        // The header's left end over the list carries the pane's page
        // switch (laid over it): its presses are not the editor's.
        if let Event::MouseDown(e) = event {
            let r = self.draw_bg.area().rect(cx);
            if e.abs.x < r.pos.x + LIST && e.abs.y < r.pos.y + HEADER && self.menu.is_none() {
                return;
            }
        }
        let uid = self.widget_uid();
        let item_at = |s: &Self, p: DVec2| s.spots.iter().find(|(r, sp)| matches!(sp, Spot::MenuItem(_)) && r.contains(p)).and_then(|(_, sp)| if let Spot::MenuItem(j) = sp { Some(*j) } else { None });
        match event.hits(cx, self.draw_bg.area()) {
            Hit::KeyDown(ke) => {
                self.handle_key(cx, &ke);
            }
            Hit::FingerHoverIn(fe) | Hit::FingerHoverOver(fe) => {
                self.pointer = fe.abs;
                if self.modal.is_some() {
                    self.modal_step(cx, fe.abs, fe.modifiers.alt, false);
                    self.draw_bg.redraw(cx);
                    return;
                }
                if self.menu.is_some() {
                    let hot = item_at(self, fe.abs);
                    if hot != self.menu_hot {
                        self.menu_hot = hot;
                        self.draw_bg.redraw(cx);
                    }
                    cx.set_cursor(if hot.is_some() { MouseCursor::Hand } else { MouseCursor::Default });
                    return;
                }
                let hot = self.spots.iter().find(|(r, _)| r.contains(fe.abs)).map(|(_, s)| *s);
                let in_graph = self.graph.contains(fe.abs);
                let key = if in_graph { self.hit_key(fe.abs) } else { None };
                let near = if key.is_none() && in_graph { self.near_curve(fe.abs, 8.0).map(|c| c.0) } else { None };
                let hc = match hot {
                    Some(Spot::Channel(i)) | Some(Spot::Eye(i)) | Some(Spot::Lock(i)) | Some(Spot::Solo(i)) => Some(i),
                    _ => key.map(|k| k.0 .0).or(near),
                };
                let tip = match hot {
                    Some(Spot::Eye(_)) => Some((fe.abs, "Show or hide this channel".to_string())),
                    Some(Spot::Lock(_)) => Some((fe.abs, "Lock: its keys cannot be edited".to_string())),
                    Some(Spot::Solo(_)) => Some((fe.abs, "Solo: show only this channel".to_string())),
                    Some(Spot::Normalise) => Some((fe.abs, "Normalise: each channel 0..1, to compare shapes".to_string())),
                    _ => None,
                };
                if hot != self.hot || key != self.hover_key || hc != self.hover_channel || tip.is_some() != self.tip.is_some() {
                    if hc != self.hover_channel {
                        let node = hc.and_then(|i| self.channels.get(i)).and_then(|c| c.node.clone());
                        cx.widget_action(uid, CurveAction::Hover(node));
                    }
                    self.hot = hot;
                    self.hover_key = key;
                    self.hover_channel = hc;
                    self.tip = tip;
                    self.draw_bg.redraw(cx);
                }
                cx.set_cursor(if hot.is_some() || key.is_some() { MouseCursor::Hand } else if near.is_some() { MouseCursor::Crosshair } else { MouseCursor::Default });
            }
            Hit::FingerHoverOut(_) => {
                self.hot = None;
                self.hover_key = None;
                self.hover_channel = None;
                self.tip = None;
                cx.widget_action(uid, CurveAction::Hover(None));
                self.draw_bg.redraw(cx);
            }
            Hit::FingerScroll(fs) => {
                let (t0, t1, v0, v1) = self.view();
                if fs.modifiers.logo || fs.modifiers.control {
                    let dt = -fs.scroll.x * self.sec_per_px();
                    let dv = fs.scroll.y * self.val_per_px();
                    self.view = Some((t0 + dt, t1 + dt, v0 + dv, v1 + dv));
                } else if fs.modifiers.shift {
                    let k = (1.0 + fs.scroll.y * 0.004).clamp(0.5, 2.0);
                    let c = self.v_of(fs.abs.y);
                    self.view = Some((t0, t1, c + (v0 - c) * k, c + (v1 - c) * k));
                } else {
                    let k = (1.0 + fs.scroll.y * 0.004).clamp(0.5, 2.0);
                    let c = self.t_of(fs.abs.x);
                    self.view = Some((c + (t0 - c) * k, c + (t1 - c) * k, v0, v1));
                }
                self.draw_bg.redraw(cx);
            }
            Hit::FingerDown(fe) => {
                cx.set_key_focus(self.draw_bg.area());
                self.pointer = fe.abs;
                self.tip = None;
                // A modal gesture: a click takes it, a right click puts it back.
                if self.modal.is_some() && !matches!(self.modal, Some(Modal::Box)) {
                    if fe.is_primary_hit() {
                        self.finish_modal(cx, fe.abs);
                    } else {
                        self.cancel_modal(cx);
                    }
                    return;
                }
                // An open menu: an item runs (a heading opens its own menu),
                // elsewhere closes it.
                if let Some(m) = self.menu.clone() {
                    match item_at(self, fe.abs) {
                        Some(j) => match m.items[j].2.clone() {
                            Some(c) => self.run(cx, c),
                            None => {
                                let label = &m.items[j].0;
                                let sub = if label.starts_with("Interp") { "Interp" } else if label.starts_with("Handle") { "Handle" } else if label.starts_with("Extrap") { "Extrap" } else { "" };
                                if !sub.is_empty() {
                                    self.open_menu(fe.abs, sub);
                                }
                            }
                        },
                        None => self.menu = None,
                    }
                    self.draw_bg.redraw(cx);
                    return;
                }
                if !fe.is_primary_hit() {
                    if fe.mouse_button().is_some_and(|b| b == MouseButton::MIDDLE) {
                        self.drag = Some(Drag::Pan { from: fe.abs, view: self.view() });
                    } else if self.graph.contains(fe.abs) {
                        self.open_menu(fe.abs, "Context");
                        self.draw_bg.redraw(cx);
                    }
                    return;
                }
                if let Some((r, spot)) = self.spots.iter().find(|(r, _)| r.contains(fe.abs)).cloned() {
                    match spot {
                        Spot::ModeTab(m) => self.run(cx, Cmd::Mode(m)),
                        Spot::MenuOpen(j) => self.open_menu(dvec2(r.pos.x, r.pos.y + r.size.y + 2.0), MENUS[j]),
                        Spot::SnapMenu => self.open_menu(dvec2(r.pos.x, r.pos.y + r.size.y + 2.0), "Snap"),
                        Spot::Normalise => self.run(cx, Cmd::Normalise),
                        Spot::Group(i) => {
                            if let Some(g) = self.channels.get(i).map(|c| c.group.clone()) {
                                if !self.folded.remove(&g) {
                                    self.folded.insert(g);
                                }
                            }
                        }
                        Spot::Channel(i) => {
                            if let Some(c) = self.channels.get(i) {
                                let id = c.id.clone();
                                let n = c.track().0.keys.len();
                                self.active = Some(id.clone());
                                if !fe.modifiers.shift {
                                    self.selected.clear();
                                }
                                for k in 0..n {
                                    self.selected.insert((id.clone(), k));
                                }
                                if fe.tap_count >= 2 {
                                    self.frame(true);
                                }
                                self.say(cx);
                            }
                        }
                        Spot::Eye(i) | Spot::Lock(i) | Spot::Solo(i) => {
                            if let Some(id) = self.channels.get(i).map(|c| c.id.clone()) {
                                let set = match spot {
                                    Spot::Eye(_) => &mut self.hidden,
                                    Spot::Lock(_) => &mut self.locked,
                                    _ => &mut self.solo,
                                };
                                if !set.remove(&id) {
                                    set.insert(id);
                                }
                            }
                        }
                        Spot::Ruler => {
                            self.drag = Some(Drag::Scrub);
                            let t = self.snap_t(self.t_of(fe.abs.x).clamp(0.0, self.duration), fe.modifiers.control);
                            if makepad_widgets::makepad_platform::makepad_error_log::trace_enabled("curves") {
                                log!("curves: seek at x {} graph {:?} view {:?} -> {}", fe.abs.x, self.graph, self.view(), t);
                            }
                            cx.widget_action(uid, CurveAction::Seek(t));
                        }
                        Spot::MenuItem(_) => {}
                    }
                    self.draw_bg.redraw(cx);
                    return;
                }
                if !self.graph.contains(fe.abs) {
                    return;
                }
                let add = fe.modifiers.logo || fe.modifiers.control;
                let now = Cx::time_now();
                let double = self.last_click.is_some_and(|(t, p)| now - t < 0.4 && (p - fe.abs).length() < 6.0);
                self.last_click = Some((now, fe.abs));
                match self.hit_key(fe.abs) {
                    Some(((i, k), handle)) if !add => {
                        let id = self.channels[i].id.clone();
                        self.active = Some(id.clone());
                        self.next_gesture();
                        match handle {
                            Some(out) => {
                                let base = lane::handles(&self.channels[i].keys).get(k).copied().unwrap_or(([0.0, 0.0], [0.0, 0.0]));
                                self.drag = Some(Drag::Handle { key: (i, k), out, base, moved: false });
                            }
                            None => {
                                if fe.modifiers.shift {
                                    if !self.selected.remove(&(id.clone(), k)) {
                                        self.selected.insert((id, k));
                                    }
                                } else if !self.selected.contains(&(id.clone(), k)) {
                                    self.selected.clear();
                                    self.selected.insert((id, k));
                                }
                                let base = self.base();
                                self.drag = Some(Drag::Keys { from: fe.abs, base, axis: None, moved: false });
                                self.say(cx);
                            }
                        }
                    }
                    _ => {
                        // Ctrl/Cmd+click, or a double click on a curve: a key
                        // there (on the curve when near one, else on the
                        // active channel at the pointer).
                        let near = self.near_curve(fe.abs, if add { 24.0 } else { 8.0 });
                        if add || (double && near.is_some()) {
                            let at = near.or_else(|| {
                                let i = self.hover_channel.or_else(|| self.active_index()).or_else(|| self.sheet_rows().first().copied())?;
                                let t = self.t_of(fe.abs.x);
                                let v = if self.mode == Mode::Graph { self.denorm(i, self.v_of(fe.abs.y)) } else { lane::sample(&self.channels[i].keys, t)? };
                                Some((i, t, v))
                            });
                            if let Some((i, t, v)) = at {
                                self.run(cx, Cmd::InsertAt(i, t, v));
                            }
                            return;
                        }
                        self.modal = None;
                        self.drag = Some(Drag::Box { from: fe.abs, to: fe.abs, add: fe.modifiers.shift });
                    }
                }
                self.draw_bg.redraw(cx);
            }
            Hit::FingerMove(fe) => {
                self.pointer = fe.abs;
                match self.drag.clone() {
                    Some(Drag::Pan { from, view }) => {
                        let d = fe.abs - from;
                        let (t0, t1, v0, v1) = view;
                        let dt = -d.x * (t1 - t0) / self.graph.size.x.max(1.0);
                        let dv = d.y * (v1 - v0) / self.graph.size.y.max(1.0);
                        self.view = Some((t0 + dt, t1 + dt, v0 + dv, v1 + dv));
                    }
                    Some(Drag::Box { from, add, .. }) => self.drag = Some(Drag::Box { from, to: fe.abs, add }),
                    Some(Drag::Scrub) => {
                        let t = self.snap_t(self.t_of(fe.abs.x).clamp(0.0, self.duration), fe.modifiers.control);
                        cx.widget_action(uid, CurveAction::Seek(t));
                    }
                    Some(_) => self.drag_step(cx, fe.abs, fe.modifiers.alt, fe.modifiers.control, false),
                    None => {}
                }
                self.draw_bg.redraw(cx);
            }
            Hit::FingerUp(fe) => {
                match self.drag.clone() {
                    Some(Drag::Box { from, to, add }) => {
                        if !add {
                            self.selected.clear();
                        }
                        let r = Rect { pos: dvec2(from.x.min(to.x), from.y.min(to.y)), size: dvec2((from.x - to.x).abs(), (from.y - to.y).abs()) };
                        for i in (0..self.channels.len()).filter(|&i| self.visible(i)).collect::<Vec<_>>() {
                            let (track, _) = self.channels[i].track();
                            for (k, key) in track.keys.iter().enumerate() {
                                if r.contains(self.key_pos(i, key.t.value, key.v[0])) {
                                    self.selected.insert((self.channels[i].id.clone(), k));
                                }
                            }
                        }
                        self.drag = None;
                        self.modal = None;
                        self.say(cx);
                    }
                    Some(Drag::Keys { .. }) | Some(Drag::Handle { .. }) => {
                        self.drag_step(cx, fe.abs, fe.modifiers.alt, fe.modifiers.control, true);
                        self.say(cx);
                    }
                    _ => self.drag = None,
                }
                self.draw_bg.redraw(cx);
            }
            _ => {}
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, _scope: &mut Scope, walk: Walk) -> DrawStep {
        let p = makepad_look::palette::get(cx);
        let rect = cx.walk_turtle(walk);
        self.draw_bg.color = color(p.well, 1.0);
        self.draw_bg.draw_abs(cx, rect);
        self.spots.clear();
        // ---- the header: mode, menus, snapping, normalise.
        self.draw_shape.fill_round(cx, Rect { pos: rect.pos, size: dvec2(rect.size.x, HEADER) }, color(p.panel, 1.0), 0.0);
        self.draw_shape.fill_round(cx, Rect { pos: dvec2(rect.pos.x, rect.pos.y + HEADER - 1.0), size: dvec2(rect.size.x, 1.0) }, color(p.separator, 1.0), 0.0);
        let mut x = rect.pos.x + LIST + 8.0;
        let y = rect.pos.y + 5.0;
        let segs = [("Graph", Mode::Graph), ("Dopesheet", Mode::Dopesheet)];
        let widths: Vec<f64> = segs.iter().map(|(l, _)| makepad_widgets::badge::advance(&self.draw_small, cx, l) + 16.0).collect();
        self.draw_shape.fill_round(cx, Rect { pos: dvec2(x, y), size: dvec2(widths.iter().sum(), 20.0) }, color(p.well, 1.0), R_CONTROL);
        for ((label, m), w) in segs.into_iter().zip(widths) {
            let r = Rect { pos: dvec2(x, y), size: dvec2(w, 20.0) };
            let on = self.mode == m;
            let hot = self.hot == Some(Spot::ModeTab(m));
            let look = p.pick(on, hot, p.well);
            if on || hot {
                self.draw_shape.edged_round(cx, Rect { pos: r.pos + dvec2(1.0, 1.0), size: r.size - dvec2(2.0, 2.0) }, color(look.fill, 1.0), color(if on { look.edge } else { look.fill }, 1.0), 1.0, R_CONTROL);
            }
            self.draw_small.color = color(look.ink, 1.0);
            self.draw_small.draw_abs(cx, dvec2(r.pos.x + 8.0, r.pos.y + 4.5), label);
            self.spots.push((r, Spot::ModeTab(m)));
            x += w;
        }
        x += 12.0;
        for (j, m) in MENUS.iter().enumerate() {
            let w = makepad_widgets::badge::advance(&self.draw_text, cx, m) + 14.0;
            let r = Rect { pos: dvec2(x, y), size: dvec2(w, 20.0) };
            let open = self.menu.as_ref().is_some_and(|mm| mm.title == *m);
            if open || self.hot == Some(Spot::MenuOpen(j)) {
                self.draw_shape.fill_round(cx, r, color(p.control_hover, 1.0), R_CONTROL);
            }
            self.draw_text.color = color(p.text, 1.0);
            self.draw_text.draw_abs(cx, dvec2(r.pos.x + 7.0, r.pos.y + 4.0), m);
            self.spots.push((r, Spot::MenuOpen(j)));
            x += w + 2.0;
        }
        let snap_label = format!("Snap: {}", match self.snap { Snap::Off => "off", Snap::Frames => "frames", Snap::Beats => "beats", Snap::Words => "words" });
        let sw = makepad_widgets::badge::advance(&self.draw_small, cx, &snap_label) + 26.0;
        let nr = Rect { pos: dvec2(rect.pos.x + rect.size.x - 30.0, y), size: dvec2(22.0, 20.0) };
        let sr = Rect { pos: dvec2(nr.pos.x - sw - 6.0, y), size: dvec2(sw, 20.0) };
        self.draw_shape.fill_round(cx, sr, color(if self.hot == Some(Spot::SnapMenu) { p.control_hover } else { p.well }, 1.0), R_CONTROL);
        self.draw_small.color = color(p.text, 1.0);
        self.draw_small.draw_abs(cx, dvec2(sr.pos.x + 8.0, sr.pos.y + 4.5), &snap_label);
        self.draw_shape.chevron(cx, Rect { pos: dvec2(sr.pos.x + sr.size.x - 15.0, sr.pos.y + 5.0), size: dvec2(10.0, 10.0) }, color(p.muted, 1.0), true);
        self.spots.push((sr, Spot::SnapMenu));
        let look = p.pick(self.normalise, self.hot == Some(Spot::Normalise), p.well);
        self.draw_shape.edged_round(cx, nr, color(look.fill, 1.0), color(if self.normalise { look.edge } else { look.fill }, 1.0), 1.0, R_CONTROL);
        let c = nr.pos + nr.size * 0.5;
        let ink = color(if self.normalise { p.text_hi } else { p.muted }, 1.0);
        self.draw_curve.line(cx, c - dvec2(5.0, 0.0), c + dvec2(5.0, 0.0), 1.2, ink);
        for s in [-1.0, 1.0] {
            self.draw_curve.line(cx, c + dvec2(0.0, s * 7.0), c + dvec2(0.0, s * 2.5), 1.2, ink);
            self.draw_curve.line(cx, c + dvec2(-2.5, s * 5.0), c + dvec2(0.0, s * 2.5), 1.2, ink);
            self.draw_curve.line(cx, c + dvec2(2.5, s * 5.0), c + dvec2(0.0, s * 2.5), 1.2, ink);
        }
        self.spots.push((nr, Spot::Normalise));
        let body = Rect { pos: dvec2(rect.pos.x, rect.pos.y + HEADER), size: dvec2(rect.size.x, (rect.size.y - HEADER).max(0.0)) };
        // ---- the channel list.
        let list = Rect { pos: body.pos, size: dvec2(LIST, body.size.y) };
        self.draw_shape.fill_round(cx, list, color(p.panel, 1.0), 0.0);
        self.draw_shape.fill_round(cx, Rect { pos: dvec2(list.pos.x + LIST - 1.0, list.pos.y), size: dvec2(1.0, list.size.y) }, color(p.separator, 1.0), 0.0);
        cx.begin_turtle(Walk::abs_rect(list), Layout::default().with_clip(true, true));
        let mut ly = list.pos.y + RULER + 2.0;
        self.row_y.clear();
        self.group_y.clear();
        if let Some(node) = &self.node_filter {
            self.draw_small.color = color(p.accent, 1.0);
            let label = format!("showing {}", node.trim_start_matches("Motion."));
            self.draw_small.draw_abs(cx, dvec2(list.pos.x + 10.0, ly + 3.0), &label);
            ly += ROW;
        }
        let mut groups: Vec<String> = Vec::new();
        for c in &self.channels {
            if !groups.contains(&c.group) {
                groups.push(c.group.clone());
            }
        }
        for g in &groups {
            let members: Vec<usize> = (0..self.channels.len()).filter(|&i| &self.channels[i].group == g && self.listed(i)).collect();
            if members.is_empty() {
                continue;
            }
            let open = !self.folded.contains(g);
            let r = Rect { pos: dvec2(list.pos.x + 4.0, ly), size: dvec2(LIST - 10.0, ROW - 2.0) };
            self.draw_shape.chevron(cx, Rect { pos: dvec2(r.pos.x + 3.0, r.pos.y + 5.0), size: dvec2(10.0, 10.0) }, color(p.muted, 1.0), open);
            self.draw_bold.color = color(p.secondary, 1.0);
            let (gl, _) = fit_text(&self.draw_bold, cx, g, LIST - 40.0);
            self.draw_bold.draw_abs(cx, dvec2(r.pos.x + 18.0, r.pos.y + 4.0), &gl);
            self.spots.push((r, Spot::Group(members[0])));
            self.group_y.push((ly, members.iter().copied().filter(|&i| self.visible(i)).collect()));
            ly += ROW;
            if !open {
                continue;
            }
            for &i in &members {
                let ch = &self.channels[i];
                let r = Rect { pos: dvec2(list.pos.x + 4.0, ly), size: dvec2(LIST - 10.0, ROW - 2.0) };
                let active = self.active.as_deref() == Some(ch.id.as_str());
                let any_sel = self.selected.iter().any(|(id, _)| id == &ch.id);
                let shown = self.visible(i);
                if active || any_sel {
                    self.draw_shape.fill_round(cx, r, color(p.accent_wash, if active { 1.0 } else { 0.55 }), R_CONTROL);
                } else if self.hover_channel == Some(i) {
                    self.draw_shape.fill_round(cx, r, color(p.control_hover, 1.0), R_CONTROL);
                }
                let mut col = hue(ch.hue);
                col.w = if shown { 1.0 } else { 0.3 };
                self.draw_shape.fill_round(cx, Rect { pos: dvec2(r.pos.x + 18.0, r.pos.y + 6.0), size: dvec2(9.0, 9.0) }, col, 2.5);
                self.draw_text.color = color(if shown { p.text } else { p.dim }, 1.0);
                let (l, _) = fit_text(&self.draw_text, cx, &ch.label, LIST - 110.0);
                self.draw_text.draw_abs(cx, dvec2(r.pos.x + 33.0, r.pos.y + 4.0), &l);
                let icons = [(Spot::Eye(i), 4.0f32, !self.hidden.contains(&ch.id)), (Spot::Lock(i), 5.0, self.locked.contains(&ch.id)), (Spot::Solo(i), 6.0, self.solo.contains(&ch.id))];
                for (j, (spot, shape, on)) in icons.into_iter().enumerate() {
                    let ir = Rect { pos: dvec2(r.pos.x + r.size.x - 60.0 + j as f64 * 20.0, r.pos.y + 2.0), size: dvec2(16.0, 14.0) };
                    let hot = self.hot == Some(spot);
                    if hot {
                        self.draw_shape.fill_round(cx, ir, color(p.control_hover, 1.0), 3.0);
                    }
                    let ink = color(if on { if shape == 4.0 { p.text_soft } else { p.accent } } else { p.dim }, if on || hot { 1.0 } else { 0.5 });
                    if shape == 6.0 {
                        self.draw_small.color = ink;
                        self.draw_small.draw_abs(cx, dvec2(ir.pos.x + 4.5, ir.pos.y + 1.5), "S");
                    } else {
                        self.draw_curve.icon(cx, Rect { pos: ir.pos + dvec2(1.5, 1.0), size: dvec2(13.0, 12.0) }, shape, ink);
                    }
                    self.spots.push((ir, spot));
                }
                self.spots.push((r, Spot::Channel(i)));
                if shown {
                    self.row_y.insert(i, ly);
                }
                ly += ROW;
                // A channel that does not move yet: how to key it.
                if ch.keys.keys.len() < 2 {
                    self.draw_small.color = color(p.dim, 1.0);
                    self.draw_small.draw_abs(cx, dvec2(r.pos.x + 33.0, ly), "Ctrl+click to add a key · I at the playhead");
                    ly += ROW - 6.0;
                }
            }
        }
        if self.channels.is_empty() {
            self.draw_text.color = color(p.muted, 1.0);
            self.draw_text.draw_abs(cx, dvec2(list.pos.x + 12.0, ly + 24.0), "No animated values yet.");
            self.draw_small.color = color(p.dim, 1.0);
            self.draw_small.draw_abs(cx, dvec2(list.pos.x + 12.0, ly + 42.0), "Hover a value in the panel");
            self.draw_small.draw_abs(cx, dvec2(list.pos.x + 12.0, ly + 56.0), "and press I to key it.");
        }
        cx.end_turtle();
        // ---- the ruler and the graph.
        let right = Rect { pos: dvec2(body.pos.x + LIST, body.pos.y), size: dvec2((body.size.x - LIST).max(0.0), body.size.y) };
        self.ruler = Rect { pos: right.pos, size: dvec2(right.size.x, RULER) };
        self.graph = Rect { pos: dvec2(right.pos.x + 46.0, right.pos.y + RULER), size: dvec2((right.size.x - 54.0).max(10.0), (right.size.y - RULER - 4.0).max(10.0)) };
        self.spots.push((self.ruler, Spot::Ruler));
        cx.begin_turtle(Walk::abs_rect(right), Layout::default().with_clip(true, true));
        self.draw_shape.fill_round(cx, self.ruler, color(p.panel, 0.6), 0.0);
        let (t0, t1, v0, v1) = self.view();
        let step_t = nice_step((t1 - t0) / (self.graph.size.x / 90.0).max(1.0));
        let mut t = (t0 / step_t).floor() * step_t;
        while t <= t1 {
            let gx = self.x_of(t);
            if gx >= self.graph.pos.x - 1.0 {
                self.draw_shape.fill_round(cx, Rect { pos: dvec2(gx, self.graph.pos.y), size: dvec2(1.0, self.graph.size.y) }, color(p.line_faint, 0.5), 0.0);
                self.draw_shape.fill_round(cx, Rect { pos: dvec2(gx, self.ruler.pos.y + RULER - 5.0), size: dvec2(1.0, 5.0) }, color(p.dim, 1.0), 0.0);
                self.draw_small.color = color(p.muted, 1.0);
                self.draw_small.draw_abs(cx, dvec2(gx + 3.0, self.ruler.pos.y + 4.0), &format!("{}s", trim(t)));
            }
            t += step_t;
        }
        for b in makepad_value_pickers::panel::host::beats(t0.max(0.0), t1) {
            let bx = self.x_of(b);
            self.draw_shape.fill_round(cx, Rect { pos: dvec2(bx, self.ruler.pos.y + RULER - 3.0), size: dvec2(1.0, 3.0) }, color(p.accent, 0.5), 0.0);
        }
        // Outside the scene's length: a veil.
        for (a, b) in [(t0, 0.0), (self.duration, t1)] {
            if b > a {
                let (xa, xb) = (self.x_of(a).max(self.graph.pos.x), self.x_of(b));
                self.draw_shape.fill_round(cx, Rect { pos: dvec2(xa, self.graph.pos.y), size: dvec2((xb - xa).max(0.0), self.graph.size.y) }, color(0, 0.18), 0.0);
            }
        }
        if self.mode == Mode::Graph {
            let step_v = nice_step((v1 - v0) / (self.graph.size.y / 36.0).max(1.0));
            let mut v = (v0 / step_v).floor() * step_v;
            while v <= v1 {
                let gy = self.y_of(v);
                self.draw_shape.fill_round(cx, Rect { pos: dvec2(self.graph.pos.x, gy), size: dvec2(self.graph.size.x, 1.0) }, color(if v.abs() < step_v * 0.5 { p.line } else { p.line_faint }, 0.5), 0.0);
                self.draw_small.color = color(p.dim, 1.0);
                self.draw_small.draw_abs(cx, dvec2(right.pos.x + 6.0, gy - 5.0), &trim(v));
                v += step_v;
            }
            let visible: Vec<usize> = (0..self.channels.len()).filter(|&i| self.visible(i)).collect();
            for i in visible {
                let ch = &self.channels[i];
                let (track, _) = ch.track();
                if track.keys.is_empty() {
                    continue;
                }
                let col = hue(ch.hue);
                let lit = self.hover_channel == Some(i) || self.active.as_deref() == Some(ch.id.as_str());
                let dim = Vec4f { w: 0.4, ..col };
                let (k0, kn) = (track.keys[0].t.value, track.keys[track.keys.len() - 1].t.value);
                let steps = (self.graph.size.x / 2.0).max(16.0) as usize;
                let mut prev: Option<DVec2> = None;
                for s in 0..=steps {
                    let tt = t0 + (t1 - t0) * s as f64 / steps as f64;
                    let vv = lane::sample(&ch.keys, tt).unwrap_or(0.0);
                    let pt = dvec2(self.x_of(tt), self.y_of(self.norm(i, vv)));
                    if let Some(a) = prev {
                        let inside = tt >= k0 && tt - (t1 - t0) / steps as f64 <= kn;
                        self.draw_curve.line(cx, a, pt, if inside { if lit { 2.2 } else { 1.6 } } else { 1.0 }, if inside { col } else { dim });
                    }
                    prev = Some(pt);
                }
                let handles = lane::handles(&ch.keys);
                for (k, key) in track.keys.iter().enumerate() {
                    let c = dvec2(self.x_of(key.t.value), self.y_of(self.norm(i, key.v[0])));
                    let sel = self.selected.contains(&(ch.id.clone(), k));
                    if sel {
                        if let Some((hin, hout)) = handles.get(k) {
                            for (out, h) in [(false, hin), (true, hout)] {
                                if h[0].abs() < 1e-9 || !handle_shown(&track, k, out) {
                                    continue;
                                }
                                let hp = dvec2(self.x_of(key.t.value + h[0]), self.y_of(self.norm(i, key.v[0] + h[1])));
                                let hot = self.hover_key == Some(((i, k), Some(out)));
                                self.draw_curve.line(cx, c, hp, 1.0, color(p.text_soft, 0.7));
                                self.draw_curve.mark(cx, hp, if hot { 9.0 } else { 7.0 }, 3.0, if hot { color(p.text_max, 1.0) } else { color(p.well, 1.0) }, color(p.accent, 1.0));
                            }
                        }
                    }
                    let hot = self.hover_key == Some(((i, k), None));
                    let (fill, edge) = if sel { (color(p.accent, 1.0), color(p.text_max, 1.0)) } else if hot { (color(p.text_max, 1.0), col) } else { (color(p.well, 1.0), col) };
                    self.draw_curve.mark(cx, c, if sel || hot { 10.0 } else { 8.0 }, 1.0, fill, edge);
                }
            }
        } else {
            // A group's row: every key of its channels, quiet.
            for (gy, members) in self.group_y.clone() {
                let mut times: Vec<f64> = members.iter().flat_map(|&i| self.channels[i].track().0.keys.iter().map(|k| k.t.value).collect::<Vec<_>>()).collect();
                times.sort_by(|a, b| a.total_cmp(b));
                times.dedup_by(|a, b| (*a - *b).abs() < 1e-4);
                for t in times {
                    self.draw_curve.mark(cx, dvec2(self.x_of(t), gy + (ROW - 2.0) * 0.5), 8.0, 2.0, color(p.muted, 0.8), color(p.well, 1.0));
                }
            }
            for (row, i) in self.sheet_rows().into_iter().enumerate() {
                let ch = &self.channels[i];
                let ry = self.row_y[&i];
                self.draw_shape.fill_round(cx, Rect { pos: dvec2(self.graph.pos.x, ry), size: dvec2(self.graph.size.x, ROW - 2.0) }, color(if self.hover_channel == Some(i) { p.control_hover } else if row % 2 == 0 { p.row } else { p.row_alt }, 0.7), R_GROUP);
                let (track, _) = ch.track();
                let col = hue(ch.hue);
                // Held stretches: a bar between equal keys.
                for w in track.keys.windows(2) {
                    if (w[0].v[0] - w[1].v[0]).abs() < 1e-9 {
                        let (xa, xb) = (self.x_of(w[0].t.value), self.x_of(w[1].t.value));
                        self.draw_shape.fill_round(cx, Rect { pos: dvec2(xa, ry + (ROW - 2.0) * 0.5 - 1.5), size: dvec2((xb - xa).max(0.0), 3.0) }, Vec4f { w: 0.35, ..col }, 1.5);
                    }
                }
                for (k, key) in track.keys.iter().enumerate() {
                    let sel = self.selected.contains(&(ch.id.clone(), k));
                    let hot = self.hover_key == Some(((i, k), None));
                    let c = dvec2(self.x_of(key.t.value), ry + (ROW - 2.0) * 0.5);
                    self.draw_curve.mark(cx, c, if sel || hot { 12.0 } else { 10.0 }, 2.0, if sel { color(p.accent, 1.0) } else if hot { color(p.text_max, 1.0) } else { col }, color(p.well, 1.0));
                }
            }
        }
        if let Some(Drag::Box { from, to, .. }) = &self.drag {
            let r = Rect { pos: dvec2(from.x.min(to.x), from.y.min(to.y)), size: dvec2((from.x - to.x).abs(), (from.y - to.y).abs()) };
            self.draw_shape.edged_round(cx, r, color(p.accent, 0.08), color(p.accent, 0.8), 1.0, 2.0);
        }
        // The playhead, with its time on the ruler.
        let px = self.x_of(self.playhead);
        self.draw_shape.fill_round(cx, Rect { pos: dvec2(px - 0.75, self.ruler.pos.y), size: dvec2(1.5, right.size.y) }, color(p.accent, 0.9), 0.0);
        let tc = format!("{}s", trim((self.playhead * 100.0).round() / 100.0));
        let tw = makepad_widgets::badge::advance(&self.draw_small, cx, &tc) + 10.0;
        self.draw_shape.fill_round(cx, Rect { pos: dvec2(px - tw * 0.5, self.ruler.pos.y + 2.0), size: dvec2(tw, 16.0) }, color(p.accent, 1.0), R_CONTROL);
        self.draw_small.color = color(p.on_accent, 1.0);
        self.draw_small.draw_abs(cx, dvec2(px - tw * 0.5 + 5.0, self.ruler.pos.y + 4.5), &tc);
        if let Some(m) = &self.modal {
            let what = match m {
                Modal::Grab { .. } => "Grab · click to place · Esc or right click cancels · Alt locks an axis",
                Modal::Scale { .. } => "Scale in time about the playhead · click to place · Esc cancels",
                Modal::Rotate { .. } => "Rotate handles · click to place · Esc cancels",
                Modal::Box => "Box select: drag a box",
            };
            let w = makepad_widgets::badge::advance(&self.draw_small, cx, what) + 16.0;
            let r = Rect { pos: dvec2(self.graph.pos.x + 8.0, self.graph.pos.y + 8.0), size: dvec2(w, 20.0) };
            self.draw_shape.fill_round(cx, r, color(p.menu, 0.95), R_CONTROL);
            self.draw_small.color = color(p.text_hi, 1.0);
            self.draw_small.draw_abs(cx, dvec2(r.pos.x + 8.0, r.pos.y + 5.0), what);
        }
        cx.end_turtle();
        // ---- a tip, then the open menu over everything.
        if let Some((at, tip)) = self.tip.clone() {
            let w = makepad_widgets::badge::advance(&self.draw_small, cx, &tip) + 14.0;
            let r = Rect { pos: dvec2((at.x + 12.0).min(rect.pos.x + rect.size.x - w - 4.0), at.y + 14.0), size: dvec2(w, 20.0) };
            self.draw_shape.edged_round(cx, r, color(p.menu, 1.0), color(p.line, 1.0), 1.0, R_CONTROL);
            self.draw_small.color = color(p.text, 1.0);
            self.draw_small.draw_abs(cx, dvec2(r.pos.x + 7.0, r.pos.y + 5.0), &tip);
        }
        if let Some(m) = self.menu.clone() {
            let (r, items) = self.menu_rects(cx, &m, rect);
            self.draw_shape.fill_round(cx, Rect { pos: r.pos + dvec2(0.0, 3.0), size: r.size }, color(0, 0.3), 7.0);
            self.draw_shape.edged_round(cx, r, color(p.menu, 1.0), color(p.border_hi, 1.0), 1.0, 7.0);
            self.draw_bold.color = color(p.muted, 1.0);
            self.draw_bold.draw_abs(cx, dvec2(r.pos.x + 11.0, r.pos.y + 8.0), &m.title);
            for (j, ((label, keys, cmd), ir)) in m.items.iter().zip(items).enumerate() {
                let heading = cmd.is_none();
                if self.menu_hot == Some(j) {
                    self.draw_shape.fill_round(cx, ir, color(p.accent_wash, if heading { 0.5 } else { 1.0 }), R_CONTROL);
                }
                let draw = if heading { &mut self.draw_small } else { &mut self.draw_text };
                draw.color = color(if heading { p.secondary } else { p.text }, 1.0);
                draw.draw_abs(cx, dvec2(ir.pos.x + 8.0, ir.pos.y + 3.5), label);
                if !keys.is_empty() {
                    let kw = makepad_widgets::badge::advance(&self.draw_small, cx, keys);
                    self.draw_small.color = color(p.dim, 1.0);
                    self.draw_small.draw_abs(cx, dvec2(ir.pos.x + ir.size.x - kw - 8.0, ir.pos.y + 4.5), keys);
                }
                self.spots.insert(0, (ir, Spot::MenuItem(j)));
            }
        }
        DrawStep::done()
    }
}

/// A round step near `raw` (1, 2 or 5 times a power of ten).
fn nice_step(raw: f64) -> f64 {
    let raw = raw.max(1e-9);
    let p = 10f64.powf(raw.log10().floor());
    let m = raw / p;
    p * if m < 1.5 { 1.0 } else if m < 3.5 { 2.0 } else if m < 7.5 { 5.0 } else { 10.0 }
}

fn trim(v: f64) -> String {
    makepad_motion::patch::fmt_num((v * 1000.0).round() / 1000.0)
}

// ----------------------------------------------------------------
// the desk's side

/// The page's state: which page shows, and what the channels were read for.
#[derive(Default)]
pub struct Curves {
    pub shown: bool,
    /// A value just animated: shown on the page (file, a byte of its keys).
    pub reveal: Option<(usize, usize)>,
    /// Tab: the other page.
    pub toggle: bool,
    read_rev: Option<u64>,
    playhead: f64,
    node: Option<Option<String>>,
}

/// Every `keys(…)` in the scene and its modules, as channels.
fn channels(page: &VideoCut) -> Vec<Channel> {
    let Some(session) = page.motion.desk.session.as_ref() else { return Vec::new() };
    let root = makepad_motion::structure::read(&session.source);
    let mut out = Vec::new();
    let files: Vec<(usize, &str, &makepad_motion::schema::code::CodeOutline, String)> = std::iter::once((0usize, session.source.as_str(), &session.code, String::new()))
        .chain(session.modules.iter().enumerate().filter(|(_, m)| m.id != "builtin").map(|(k, m)| (k + 1, m.source.as_str(), &m.code, m.name.clone())))
        .collect();
    // Each keys(…) a path names (`LINE_LOOK.axis.heat`): its label.
    let path_files: Vec<makepad_motion::paths::File> = files.iter().map(|(_, text, code, module)| makepad_motion::paths::File { name: module.as_str(), text, code }).collect();
    let mut named: HashMap<(usize, usize), String> = HashMap::new();
    for e in makepad_motion::paths::list(&path_files).into_iter().filter(|e| e.value.trim_start().starts_with("keys(")) {
        if let Ok(site) = makepad_motion::paths::resolve_exact(&path_files, &e.path) {
            named.insert((files[site.file].0, site.range.start), e.path);
        }
    }
    let mut n = 0u32;
    for (doc, text, code, module) in files.iter().map(|(d, t, c, m)| (*d, *t, *c, m.clone())) {
        let shared = std::rc::Rc::new(text.to_string());
        for site in code.times.iter().filter(|s| !s.tween) {
            let Some(keys) = lane::parse(text, site.range.clone()) else { continue };
            if keys.keys.iter().all(|k| k.v.is_none()) {
                continue;
            }
            let at = keys.call.start;
            // Its name: the field or `let` it is written under.
            let before = text[..at].trim_end();
            let field = before.strip_suffix(':').or_else(|| before.strip_suffix('=')).map(|b| b.trim_end()).map(|b| {
                let start = b.rfind(|c: char| !(c.is_alphanumeric() || c == '_')).map_or(0, |i| i + 1);
                b[start..].to_string()
            }).unwrap_or_else(|| "keys".into());
            // Its owner: the layer it is in, else the top-level `let`.
            let node = if doc == 0 { root.as_ref().and_then(|r| makepad_motion::structure::at_byte(r, at)).filter(|n| n.kind != makepad_motion::structure::NodeKind::Scene).map(|n| (n.path.clone(), n.label.clone())) } else { None };
            let owner_let = code.regions.iter().filter(|r| r.kind == makepad_motion::schema::code::RegionKind::Let && r.depth == 0 && r.range.start <= at && at < r.range.end).map(|r| r.name.clone()).next();
            // A value that is its own `let` goes with the scene's values.
            let own_let = owner_let.as_deref().is_some_and(|l| named.get(&(doc, at)).is_some_and(|p| p == l || p.ends_with(&format!(".{l}"))));
            let group = match (&node, &owner_let) {
                _ if own_let => if module.is_empty() { "scene values".to_string() } else { format!("{module} values") },
                (Some((_, label)), _) => label.clone(),
                (None, Some(l)) if !module.is_empty() => format!("{module}.{l}"),
                (None, Some(l)) => l.clone(),
                _ => if module.is_empty() { "scene".into() } else { module.clone() },
            };
            // A path names it: the path after its owner (`axis.heat`).
            let label = match named.get(&(doc, at)) {
                Some(path) => {
                    let short = owner_let.as_ref().and_then(|l| path.split_once(&format!("{l}."))).map(|(_, rest)| rest.to_string());
                    short.unwrap_or_else(|| path.clone())
                }
                None => field,
            };
            out.push(Channel { id: format!("{doc}:{at}"), doc, label, group, node: node.map(|n| n.0), keys, text: shared.clone(), hue: n });
            n += 1;
        }
    }
    out
}

/// Every pump: the channels read again when the code changed, the
/// playhead and the selection's node given.
pub fn pump(page: &mut VideoCut, cx: &mut Cx, ui: &WidgetRef) {
    let desk_ref = ui.widget(cx, ids!(cut_motion));
    let Some(session) = page.motion.desk.session.as_ref() else { return };
    let (rev, playhead, duration) = (session.revision, session.playhead, session.duration());
    let fps = session.doc.as_ref().map_or(60.0, |d| d.fps);
    if std::mem::take(&mut page.motion.curves.toggle) {
        let shown = page.motion.curves.shown;
        show(page, cx, &desk_ref, !shown);
    }
    if let Some((doc, byte)) = page.motion.curves.reveal.take() {
        reveal(page, cx, &desk_ref, doc, byte);
    }
    if !page.motion.curves.shown {
        return;
    }
    let editor = desk_ref.widget(cx, ids!(cv_editor));
    if page.motion.curves.read_rev != Some(rev) {
        page.motion.curves.read_rev = Some(rev);
        let chans = channels(page);
        if let Some(mut e) = editor.borrow_mut::<CurveEditor>() {
            e.set_channels(cx, chans, playhead, duration, fps);
        }
    }
    if page.motion.curves.playhead != playhead {
        page.motion.curves.playhead = playhead;
        if let Some(mut e) = editor.borrow_mut::<CurveEditor>() {
            e.set_playhead(cx, playhead);
        }
    }
    // The structure's selection filters the channels.
    let node = page.motion.structure.selected.clone().filter(|p| p.starts_with("Motion.layers"));
    if page.motion.curves.node.as_ref() != Some(&node) {
        page.motion.curves.node = Some(node.clone());
        if let Some(mut e) = editor.borrow_mut::<CurveEditor>() {
            e.set_node_filter(cx, node);
        }
    }
}

/// Show the CODE or the CURVES page.
pub fn show(page: &mut VideoCut, cx: &mut Cx, desk_ref: &WidgetRef, curves: bool) {
    page.motion.curves.shown = curves;
    page.motion.curves.read_rev = None;
    desk_ref.widget(cx, ids!(m_curves)).set_visible(cx, curves);
    desk_ref.widget(cx, ids!(m_code_page)).set_visible(cx, !curves);
    if let Some(mut tabs) = desk_ref.widget(cx, ids!(m_tab_layers)).borrow_mut::<super::super::controls::CutSegmented>() {
        tabs.set_active(cx, curves as usize);
    }
    desk_ref.redraw(cx);
}

/// A value just animated (or its lane asked for): the CURVES page shows it.
pub fn reveal(page: &mut VideoCut, cx: &mut Cx, desk_ref: &WidgetRef, doc: usize, byte: usize) {
    show(page, cx, desk_ref, true);
    // The channels read again from the text the reveal names.
    page.motion.curves.read_rev = None;
    if let Some(mut e) = desk_ref.widget(cx, ids!(cv_editor)).borrow_mut::<CurveEditor>() {
        e.reveal(cx, format!("{doc}:{byte}"));
    }
}

pub fn handle_actions(page: &mut VideoCut, cx: &mut Cx, ui: &WidgetRef, actions: &Actions) {
    let desk_ref = ui.widget(cx, ids!(cut_motion));
    let tabs = desk_ref.widget(cx, ids!(m_tab_layers));
    for action in actions.filter_widget_actions_cast::<super::super::controls::SegmentAction>(tabs.widget_uid()) {
        if let super::super::controls::SegmentAction::Selected(index) = action {
            show(page, cx, &desk_ref, index == 1);
        }
    }
    if !page.motion.curves.shown {
        return;
    }
    let editor = desk_ref.widget(cx, ids!(cv_editor));
    if let Some(text) = desk_ref.text_input(cx, ids!(cv_search)).changed(actions) {
        if let Some(mut e) = editor.borrow_mut::<CurveEditor>() {
            e.set_filter(cx, text);
        }
    }
    // The selected key's time and value fields: a drag's steps live, its
    // release (or a typed entry) one undo step.
    let number = |v: &PanelValue| v.number().map(|(n, _)| n);
    let time_field = desk_ref.widget(cx, ids!(cv_t));
    let value_field = desk_ref.widget(cx, ids!(cv_v));
    for (field, is_time) in [(&time_field, true), (&value_field, false)] {
        for action in actions.filter_widget_actions_cast::<PanelControlAction>(field.widget_uid()) {
            if let PanelControlAction::Changed { value, done } = action {
                if let (Some(n), Some(mut e)) = (number(&value), editor.borrow_mut::<CurveEditor>()) {
                    if is_time {
                        e.set_key_field(cx, Some(n), None, done);
                    } else {
                        e.set_key_field(cx, None, Some(n), done);
                    }
                }
            }
        }
    }
    for action in actions.filter_widget_actions_cast::<CurveAction>(editor.widget_uid()) {
        match action {
            CurveAction::Edits { doc, edits } => {
                page.motion.desk.author = Some("You · curves".into());
                super::desk::code_edits(page, cx, doc, edits);
            }
            CurveAction::Seek(t) => super::desk::set_playhead(page, t),
            CurveAction::Hover(node) => super::structure_panel::outline_path(page, cx, &desk_ref, node.as_deref()),
            CurveAction::Status { text, key } => {
                desk_ref.label(cx, ids!(cv_status)).set_text(cx, &text);
                desk_ref.widget(cx, ids!(cv_key)).set_visible(cx, key.is_some());
                if let Some((t, v)) = key {
                    if let Some(mut f) = desk_ref.widget(cx, ids!(cv_t)).borrow_mut::<PanelControl>() {
                        f.set_value(cx, &format!("{}s", trim(t)));
                    }
                    if let Some(mut f) = desk_ref.widget(cx, ids!(cv_v)).borrow_mut::<PanelControl>() {
                        f.set_value(cx, &trim(v));
                    }
                }
            }
            CurveAction::None => {}
        }
    }
}
