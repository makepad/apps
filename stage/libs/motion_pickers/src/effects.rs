//! An effects stack attached to a node: `PanelEffects{path: "Motion.post"}`
//! shows the list at `path` (the scene's post, a layer's `effects`, a
//! Group's `post`) as a stack, one row per effect in the order the picture
//! applies them: a grip to drag it to another place, a bypass switch, its
//! name, its key parameter as a compact slider, × to take it out; "+ Add"
//! offers what the list can take ([`structure::catalogue`]). With
//! `details: true` a row's chevron unfolds its other parameters; a host that
//! shows the selected entry's parameters itself (the structure column)
//! turns them off and follows [`EffectsAction::Select`].
//!
//! Built from the code as written, never generated, and edited the one way
//! every stack is ([`structure::stack_edit`]): bypass writes `on: false`
//! (and takes it away), a move rewrites the entries' lines, an add writes
//! the entry with its defaults (and the list, when the node has none yet).
//! Each change is a write through the panel's host, so cmd-Z walks back
//! through it; a parameter drag is held live like any control's.

use makepad_edit_values::number::fmt_num;
use makepad_value_pickers::panel::control::{fit_text, DrawPanelShape, LABEL, R_CARD, R_CONTROL};
use makepad_value_pickers::panel::host::{self, PanelValue};
use makepad_motion::structure::{self, Form, Node, NodeKind, Stack, StackOp};
use makepad_widgets::badge::advance;
use makepad_widgets::*;

script_mod! {
    use mod.prelude.widgets_internal.*
    use mod.widgets.*

    mod.widgets.PanelEffectsBase = #(PanelEffects::register_widget(vm))
    /** An effects stack attached to a node: the list at `path` (the scene's
     * `post`, a layer's `effects`), each effect a row with a bypass, its key
     * parameter (and, with `details`, the rest folded); drag to reorder,
     * add, remove. */
    mod.widgets.PanelEffects = set_type_default() do mod.widgets.PanelEffectsBase{
        width: Fill height: Fit
        path: "Motion.post"
        title: "Post"
        details: true
        draw_bg +: {color: #x00000000}
        draw_title +: {text_style: theme.font_bold{font_size: 8.5}}
        draw_text +: {text_style: theme.font_regular{font_size: 8.5}}
        draw_small +: {text_style: theme.font_regular{font_size: 7.5}}
    }
}

/// What a stack says to its host.
#[derive(Clone, Debug, Default)]
pub enum EffectsAction {
    #[default]
    None,
    /// An entry was clicked (or added): its path (`Motion.post[1]`).
    Select(String),
    /// The pointer is on an entry (or left the stack): a host outlines what
    /// it does.
    Hover(Option<String>),
}

fn color(value: u32, alpha: f32) -> Vec4f {
    let mut c = Vec4f::from_u32(value);
    c.w *= alpha;
    c
}

const HEAD: f64 = 24.0;
const ROW: f64 = 28.0;
const PARAM: f64 = 24.0;
const PAD: f64 = 8.0;

#[derive(Clone, Debug, PartialEq)]
enum Spot {
    Head,
    Add,
    Kit(usize),
    Toggle(usize),
    Open(usize),
    Select(usize),
    Remove(usize),
    Grip(usize),
    /// A parameter's track.
    Track(Num),
}

/// A numeric parameter as its track writes it: where (the field, or the
/// `let` a name there holds), its range and step, and whether it is whole.
#[derive(Clone, Debug, PartialEq)]
struct Num {
    target: String,
    lo: f64,
    hi: f64,
    step: f64,
    whole: bool,
}

impl Num {
    /// `v` as code: on the step, a whole number written whole, any other
    /// with a decimal point (a float written `1` stays a float: `1.0`).
    fn code(&self, v: f64) -> String {
        let v = (v / self.step).round() * self.step;
        if self.whole {
            return format!("{}", v.round() as i64);
        }
        let decimals = ((-self.step.log10()).ceil().max(1.0)) as usize;
        let t = fmt_num(v, decimals);
        if t.contains('.') { t } else { format!("{t}.0") }
    }
}

#[derive(Script, ScriptHook, Widget)]
pub struct PanelEffects {
    #[uid]
    uid: WidgetUid,
    #[walk]
    walk: Walk,
    /// Under the whole card (what the pointer hits).
    #[redraw]
    #[live]
    draw_bg: DrawColor,
    #[live]
    draw_shape: DrawPanelShape,
    #[live]
    draw_title: DrawText,
    #[live]
    draw_text: DrawText,
    #[live]
    draw_small: DrawText,
    /// The list's path (`Motion.post`, `Motion.layers[2].effects`).
    #[live]
    pub path: String,
    #[live]
    pub title: String,
    /// Whether a row unfolds its parameters (off where the host shows the
    /// selected entry's parameters itself).
    #[live]
    pub details: bool,
    #[live(true)]
    #[visible]
    visible: bool,
    #[rust]
    folded: bool,
    /// The entry whose parameters are unfolded.
    #[rust]
    open: Option<usize>,
    /// The entry the host shows (marked).
    #[rust]
    selected: Option<String>,
    #[rust]
    adding: bool,
    #[rust]
    spots: Vec<(Rect, Spot)>,
    #[rust]
    hot: Option<Spot>,
    /// The rows as drawn (for a drag's drop place).
    #[rust]
    rows: Vec<Rect>,
    /// A row dragged: which, and where the pointer is.
    #[rust]
    dragging: Option<(usize, f64)>,
    /// A parameter dragged, and its track.
    #[rust]
    sliding: Option<(Num, Rect)>,
    #[rust]
    note: String,
    /// The code's change it last drew (it draws again on the next).
    #[rust]
    seen: u64,
}

/// The scene's text as the host reads it (`Motion{…}`), its structure,
/// and the stack at `path` with the node it is attached to.
struct Read {
    text: String,
    owner: Node,
    stack: Stack,
}

impl PanelEffects {
    fn read(&self) -> Option<Read> {
        let text = host::read("Motion")?.code();
        let root = structure::read(&text)?;
        let (owner, stack) = structure::flatten(&root).into_iter().find_map(|n| n.stacks.iter().find(|s| s.path == self.path).map(|s| (n.clone(), s.clone())))?;
        Some(Read { text, owner, stack })
    }

    /// `op` on the stack, written through the host: the list when it is
    /// written before and after, else the node it is attached to.
    fn edit(&mut self, op: StackOp) {
        let Some(r) = self.read() else { return };
        match structure::stack_edit(&r.text, &r.owner, &r.stack, &op) {
            Ok(splice) => {
                let next = splice.apply(&r.text);
                let Some(root) = structure::read(&next) else { return };
                let after = structure::flatten(&root).into_iter().find_map(|n| n.stacks.iter().find(|s| s.path == self.path).cloned());
                // Written as code outright, never held as a gesture's live
                // step (a drag starting right after it would take it in).
                match (r.stack.range.is_some(), after.and_then(|s| s.range)) {
                    (true, Some(range)) => host::swap(&self.path, &next[range]),
                    _ => {
                        let Some(owner) = structure::find(&root, &r.owner.path) else { return };
                        host::swap(&r.owner.path, &next[owner.range.clone()]);
                    }
                }
                self.note.clear();
            }
            Err(why) => self.note = why,
        }
    }

    fn slide(&mut self, cx: &mut Cx, x: f64) {
        let Some((num, track)) = self.sliding.clone() else { return };
        let t = ((x - track.pos.x) / track.size.x.max(1.0)).clamp(0.0, 1.0);
        // Written as the parameter is (its kit says), not as the literal
        // happens to be: live while the hand is down.
        host::set(&num.target, PanelValue::Code(num.code(num.lo + (num.hi - num.lo) * t)));
        self.draw_bg.redraw(cx);
    }

    fn click(&mut self, cx: &mut Cx, spot: Spot) {
        let field = if self.path.ends_with(".effects") { "effects" } else { "post" };
        match spot {
            Spot::Head => self.folded = !self.folded,
            Spot::Add => {
                self.adding = !self.adding;
                // The catalogue shows in the open stack.
                if self.adding {
                    self.folded = false;
                }
            }
            Spot::Kit(k) => {
                if let Some((name, _, _)) = structure::catalogue(field).get(k) {
                    let n = self.read().map_or(0, |r| r.stack.items.len());
                    self.edit(StackOp::Add(structure::new_entry(field, name)));
                    cx.widget_action(self.widget_uid(), EffectsAction::Select(format!("{}[{n}]", self.path)));
                }
                self.adding = false;
            }
            Spot::Toggle(i) => {
                if let Some(r) = self.read() {
                    let on = r.stack.items.get(i).is_some_and(|it| it.on);
                    self.edit(StackOp::Bypass(i, !on));
                }
            }
            Spot::Open(i) => self.open = if self.open == Some(i) { None } else { Some(i) },
            Spot::Select(i) => {
                let path = format!("{}[{i}]", self.path);
                self.selected = Some(path.clone());
                cx.widget_action(self.widget_uid(), EffectsAction::Select(path));
            }
            Spot::Remove(i) => {
                self.edit(StackOp::Remove(i));
                self.open = None;
            }
            Spot::Grip(_) | Spot::Track(..) => {}
        }
        self.draw_bg.redraw(cx);
    }

    /// Where a row dragged to `y` lands.
    fn drop_place(&self, y: f64) -> usize {
        self.rows.iter().filter(|r| y > r.pos.y + r.size.y * 0.5).count()
    }

    /// The entry the host shows, marked in the stack.
    pub fn set_selected(&mut self, cx: &mut Cx, path: Option<String>) {
        if self.selected != path {
            self.selected = path;
            self.draw_bg.redraw(cx);
        }
    }
}

/// A parameter written as a number, or as a name of one (`amount:
/// GRAIN_AMOUNT`): its value now and where a drag writes it (the field, or
/// the name's `let`), with its range and step: the kit's own for a post
/// kit's parameter, else around the value (a float).
fn number_of(node: &Node, p: &structure::Param) -> Option<(f64, Num)> {
    let w = p.written.as_deref()?.trim();
    let (v, target) = match p.form {
        Form::Literal => (host::read(&p.path).and_then(|v| v.code().trim().parse::<f64>().ok()).or_else(|| w.parse().ok())?, p.path.clone()),
        Form::Name => (host::read(w).and_then(|v| v.code().trim().parse::<f64>().ok())?, w.to_string()),
        _ => return None,
    };
    let num = match makepad_render_graph::kits::param(&node.ty, &p.name) {
        Some(k) => Num { target, lo: k.min.min(v), hi: k.max.max(v), step: k.step, whole: k.whole },
        None => {
            let reach = v.abs().max(p.default.as_deref().and_then(|d| d.parse::<f64>().ok()).unwrap_or(0.0).abs());
            let hi = if reach <= 1.0 { 1.0 } else { (reach * 2.0).ceil() };
            let lo = if v < 0.0 { -hi } else { 0.0 };
            let raw = (hi - lo) / 200.0;
            let step = 10f64.powf(raw.log10().floor());
            Num { target, lo, hi, step, whole: false }
        }
    };
    Some((v, num))
}

/// An entry's key parameter: the first its kit names that is a number (or
/// a name of one) the row can move, else the first such written.
fn key_param(node: &Node, params: &[structure::Param]) -> Option<(structure::Param, f64, Num)> {
    let order = makepad_render_graph::kits::kit(&node.ty).map_or(Vec::new(), |k| k.params.to_vec());
    let mut ordered: Vec<&structure::Param> = order.iter().filter_map(|n| params.iter().find(|p| p.name == *n)).collect();
    ordered.extend(params.iter().filter(|p| !order.contains(&p.name.as_str())));
    // A mode or a count is no key: the first number that is not whole.
    ordered.into_iter().filter_map(|p| number_of(node, p).map(|(v, n)| (p.clone(), v, n))).find(|(_, _, n)| !n.whole)
}

impl Widget for PanelEffects {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, _scope: &mut Scope) {
        if !self.visible {
            return;
        }
        // The code changed (a write, an undo, a reload): drawn again.
        if host::revision() != self.seen {
            self.seen = host::revision();
            self.draw_bg.redraw(cx);
        }
        let at = |s: &Self, abs: DVec2| s.spots.iter().rev().find(|(r, _)| r.contains(abs)).map(|(_, x)| x.clone());
        match event.hits(cx, self.draw_bg.area()) {
            Hit::FingerHoverIn(fe) | Hit::FingerHoverOver(fe) => {
                let hot = at(self, fe.abs);
                cx.set_cursor(match &hot {
                    Some(Spot::Grip(_)) => MouseCursor::Grab,
                    Some(Spot::Track(..)) => MouseCursor::EwResize,
                    Some(_) => MouseCursor::Hand,
                    None => MouseCursor::Default,
                });
                if hot != self.hot {
                    let entry = |h: &Option<Spot>| match h {
                        Some(Spot::Grip(i) | Spot::Toggle(i) | Spot::Select(i) | Spot::Open(i) | Spot::Remove(i)) => Some(*i),
                        _ => None,
                    };
                    if entry(&hot) != entry(&self.hot) {
                        let path = entry(&hot).map(|i| format!("{}[{i}]", self.path));
                        cx.widget_action(self.widget_uid(), EffectsAction::Hover(path));
                    }
                    self.hot = hot;
                    self.draw_bg.redraw(cx);
                }
            }
            Hit::FingerHoverOut(_) => {
                self.hot = None;
                cx.widget_action(self.widget_uid(), EffectsAction::Hover(None));
                self.draw_bg.redraw(cx);
            }
            // The track's path and range were taken when it was drawn: a
            // press right after a write (the code read again) still lands.
            Hit::FingerDown(fe) if fe.is_primary_hit() => match self.spots.iter().rev().find(|(r, _)| r.contains(fe.abs)).cloned() {
                Some((_, Spot::Grip(i))) => self.dragging = Some((i, fe.abs.y)),
                Some((rect, Spot::Track(num))) => {
                    self.sliding = Some((num, rect));
                    self.slide(cx, fe.abs.x);
                }
                _ => {}
            },
            Hit::FingerMove(fe) => {
                if let Some((i, _)) = self.dragging {
                    self.dragging = Some((i, fe.abs.y));
                    self.draw_bg.redraw(cx);
                }
                if self.sliding.is_some() {
                    self.slide(cx, fe.abs.x);
                }
            }
            Hit::FingerUp(fe) => {
                if let Some((from, y)) = self.dragging.take() {
                    let to = self.drop_place(y);
                    let to = if to > from { to - 1 } else { to };
                    if to != from {
                        self.edit(StackOp::Move(from, to));
                        self.open = None;
                    }
                    self.draw_bg.redraw(cx);
                    return;
                }
                if self.sliding.take().is_some() {
                    return;
                }
                if fe.is_over && fe.is_primary_hit() {
                    if let Some(spot) = at(self, fe.abs) {
                        self.click(cx, spot);
                    }
                }
            }
            _ => {}
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, _scope: &mut Scope, walk: Walk) -> DrawStep {
        if !self.visible {
            return DrawStep::done();
        }
        // Neither the list nor the node it would go on (a score, an edit):
        // nothing to show.
        let Some(read) = self.read() else {
            cx.walk_turtle(Walk { height: Size::Fixed(0.0), ..walk });
            return DrawStep::done();
        };
        let p = makepad_look::palette::get(cx);
        let field = if self.path.ends_with(".effects") { "effects" } else { "post" };
        let items = read.stack.items.clone();
        let params: Vec<Vec<structure::Param>> = items.iter().map(|n| structure::params(&read.text, n)).collect();
        let catalogue = structure::catalogue(field);
        let width = cx.turtle().inner_width();
        let inner = (width - 2.0 * PAD).max(80.0);
        // Laid out first: the catalogue's chips when adding, each row and
        // the parameters of the one unfolded.
        let mut chips: Vec<(f64, f64, f64)> = Vec::new();
        let mut chips_h = 0.0;
        if self.adding && !self.folded {
            let (mut x, mut y) = (0.0, 0.0);
            for (name, _, _) in &catalogue {
                let w = advance(&self.draw_small, cx, name) + 16.0;
                if x + w > inner && x > 0.0 {
                    x = 0.0;
                    y += 24.0;
                }
                chips.push((x, y, w));
                x += w + 4.0;
            }
            chips_h = y + 28.0;
        }
        let open = self.open.filter(|i| self.details && *i < items.len());
        let open_params: Vec<&structure::Param> = open.map(|i| params[i].iter().filter(|p| p.written.is_some()).collect()).unwrap_or_default();
        // An empty stack is one tight row (its name and "+") until it is added to.
        let shut = self.folded || (items.is_empty() && !self.adding);
        let body = if shut {
            0.0
        } else {
            let rows = items.len() as f64 * ROW;
            let unfolded = if open.is_some() { open_params.len().max(1) as f64 * PARAM + 6.0 } else { 0.0 };
            let note = if self.note.is_empty() { 0.0 } else { 18.0 };
            chips_h + rows + unfolded + note + 6.0
        };
        let rect = cx.walk_turtle(Walk { height: Size::Fixed(HEAD + body), ..walk });
        self.draw_bg.color = color(0, 0.0);
        self.draw_bg.draw_abs(cx, rect);
        // Where a dragged row would land (by the rows drawn last).
        let drop_at = self.dragging.map(|(from, py)| (from, self.drop_place(py)));
        self.spots.clear();
        self.rows.clear();
        self.draw_shape.edged_round(cx, rect, color(p.row, 1.0), color(p.separator, 1.0), 1.0, R_CARD);
        // The heading: a chevron, the title and how many effects are on, and
        // "+ Add".
        let cy = rect.pos.y + HEAD * 0.5;
        if !items.is_empty() {
            self.draw_shape.chevron(cx, Rect { pos: dvec2(rect.pos.x + 8.0, cy - 6.0), size: dvec2(12.0, 12.0) }, color(p.muted, 1.0), !self.folded);
        }
        self.draw_title.color = color(p.text_hi, 1.0);
        let title = if self.title.is_empty() { "Effects".to_string() } else { self.title.clone() };
        self.draw_title.draw_abs(cx, dvec2(rect.pos.x + 26.0, cy - 6.0), &title);
        let tw = advance(&self.draw_title, cx, &title);
        let on = items.iter().filter(|i| i.on).count();
        let said = match (items.len(), on) {
            (0, _) => String::new(),
            (n, k) if n == k => format!("{n} effects"),
            (n, k) => format!("{k} of {n} on"),
        };
        self.draw_small.color = color(p.muted, 1.0);
        self.draw_small.draw_abs(cx, dvec2(rect.pos.x + 34.0 + tw, cy - 5.0), &said);
        self.spots.push((Rect { pos: rect.pos, size: dvec2(rect.size.x, HEAD) }, Spot::Head));
        let add_w = 22.0;
        let add = Rect { pos: dvec2(rect.pos.x + rect.size.x - 8.0 - add_w, cy - 10.0), size: dvec2(add_w, 20.0) };
        let look = p.pick(self.adding, self.hot == Some(Spot::Add), p.raised);
        self.draw_shape.edged_round(cx, add, color(look.fill, 1.0), color(look.edge, 1.0), 1.0, R_CONTROL);
        self.draw_small.color = color(look.ink, 1.0);
        self.draw_small.draw_abs(cx, dvec2(add.pos.x + 8.0, add.pos.y + 5.0), "+");
        self.spots.push((add, Spot::Add));
        if shut {
            return DrawStep::done();
        }
        let x0 = rect.pos.x + PAD;
        let mut y = rect.pos.y + HEAD;
        // The catalogue, as chips.
        for (k, (x, cyy, w)) in chips.iter().enumerate() {
            let r = Rect { pos: dvec2(x0 + x, y + cyy), size: dvec2(*w, 20.0) };
            let hot = self.hot == Some(Spot::Kit(k));
            self.draw_shape.edged_round(cx, r, color(if hot { p.control_hover } else { p.raised }, 1.0), color(p.line_faint, 1.0), 1.0, R_CONTROL);
            self.draw_small.color = color(p.text, 1.0);
            self.draw_small.draw_abs(cx, dvec2(r.pos.x + 8.0, r.pos.y + 5.0), &catalogue[k].0);
            self.spots.push((r, Spot::Kit(k)));
        }
        y += chips_h;
        for (i, item) in items.iter().enumerate() {
            let row = Rect { pos: dvec2(x0, y), size: dvec2(inner, ROW) };
            self.rows.push(row);
            let ry = row.pos.y + ROW * 0.5;
            let lit = if item.on { 1.0 } else { 0.45 };
            let path = format!("{}[{i}]", self.path);
            let selected = self.selected.as_deref() == Some(path.as_str());
            let dragged = drop_at.is_some_and(|(from, _)| from == i);
            let hot_row = self.hot.as_ref().is_some_and(|h| matches!(h, Spot::Grip(k) | Spot::Open(k) | Spot::Toggle(k) | Spot::Remove(k) | Spot::Select(k) if *k == i));
            if dragged || selected || open == Some(i) || hot_row {
                let wash = if dragged || selected { p.accent_wash } else { p.raised };
                self.draw_shape.fill_round(cx, Rect { pos: row.pos + dvec2(-2.0, 1.0), size: row.size + dvec2(4.0, -2.0) }, color(wash, 1.0), R_CONTROL);
            }
            // Where a dragged row would land.
            if let Some((from, to)) = drop_at {
                if to == i && to != from && to != from + 1 {
                    self.draw_shape.fill_round(cx, Rect { pos: dvec2(row.pos.x, row.pos.y - 1.0), size: dvec2(row.size.x, 2.0) }, color(p.accent, 1.0), 1.0);
                }
            }
            let movable = !read.stack.generated;
            if movable {
                for (dx, dy) in [(0.0, -4.0), (0.0, 0.0), (0.0, 4.0), (4.0, -4.0), (4.0, 0.0), (4.0, 4.0)] {
                    self.draw_shape.fill_round(cx, Rect { pos: dvec2(row.pos.x + 4.0 + dx, ry - 1.0 + dy), size: dvec2(2.0, 2.0) }, color(p.dim, 1.0), 1.0);
                }
                self.spots.push((Rect { pos: row.pos, size: dvec2(14.0, ROW) }, Spot::Grip(i)));
            }
            // The bypass switch (`on: false`); code (a generator) has none.
            let sw = Rect { pos: dvec2(row.pos.x + 18.0, ry - 6.0), size: dvec2(22.0, 12.0) };
            if !matches!(item.kind, NodeKind::Generator | NodeKind::Code) {
                self.draw_shape.fill_round(cx, sw, color(if item.on { p.accent } else { p.track }, 1.0), 6.0);
                let knob = if item.on { sw.pos.x + sw.size.x - 11.0 } else { sw.pos.x + 1.0 };
                self.draw_shape.fill_round(cx, Rect { pos: dvec2(knob, sw.pos.y + 1.0), size: dvec2(10.0, 10.0) }, color(p.text_bright, 1.0), 5.0);
                self.spots.push((Rect { pos: sw.pos - dvec2(3.0, 6.0), size: sw.size + dvec2(6.0, 12.0) }, Spot::Toggle(i)));
            }
            // Its name (a click selects it), then its key parameter.
            let right = row.pos.x + row.size.x;
            let name_x = sw.pos.x + sw.size.x + 10.0;
            // Narrow (the structure column): a shorter name column and the
            // key parameter's track without its name.
            let wide = inner >= 380.0;
            let name_w = if wide { 90.0 } else { (inner * 0.3).clamp(56.0, 90.0) };
            self.draw_text.color = color(p.text_hi, lit);
            let (name, _) = fit_text(&self.draw_text, cx, &item.label, name_w);
            self.draw_text.draw_abs(cx, dvec2(name_x, ry - 6.0), &name);
            self.spots.push((Rect { pos: dvec2(name_x - 4.0, row.pos.y), size: dvec2(name_w + 6.0, ROW) }, Spot::Select(i)));
            let track_x = name_x + name_w + 6.0;
            // The value after the track, then (with details) the chevron,
            // then ×.
            let track_end = right - if self.details { 40.0 } else { 22.0 } - 40.0;
            let written = params[i].iter().filter(|p| p.written.is_some()).count();
            match (item.on, key_param(item, &params[i])) {
                (true, Some((param, v, num))) => {
                    self.draw_param(cx, &param.name, v, num, track_x, track_end, ry, lit, wide);
                }
                (on, _) => {
                    let said = if !on {
                        "bypassed"
                    } else if item.kind == NodeKind::Generator {
                        "generated: its options are its parameters"
                    } else if item.ty == "Pass" {
                        "a shader of its own"
                    } else if params[i].iter().any(|p| p.form == Form::Expression) {
                        "animated in code"
                    } else {
                        "defaults"
                    };
                    self.draw_small.color = color(p.dim, 1.0);
                    let (said, _) = fit_text(&self.draw_small, cx, said, (right - 24.0 - track_x).max(10.0));
                    self.draw_small.draw_abs(cx, dvec2(track_x, ry - 5.0), &said);
                }
            }
            // Details and remove.
            let unfolded = open == Some(i);
            if self.details && written > 0 {
                let chev = Rect { pos: dvec2(right - 40.0, ry - 8.0), size: dvec2(16.0, 16.0) };
                self.draw_shape.chevron(cx, Rect { pos: chev.pos + dvec2(2.0, 2.0), size: dvec2(12.0, 12.0) }, color(if unfolded { p.text_hi } else { p.muted }, 1.0), unfolded);
                self.spots.push((Rect { pos: chev.pos - dvec2(4.0, 4.0), size: chev.size + dvec2(8.0, 8.0) }, Spot::Open(i)));
            }
            if movable {
                let x_hot = self.hot == Some(Spot::Remove(i));
                let cross = Rect { pos: dvec2(right - 18.0, ry - 8.0), size: dvec2(16.0, 16.0) };
                if x_hot {
                    self.draw_shape.fill_round(cx, cross, color(p.remove_wash, 1.0), 8.0);
                }
                self.draw_small.color = color(if x_hot { p.text_hi } else { p.dim }, 1.0);
                let xw = advance(&self.draw_small, cx, "×");
                self.draw_small.draw_abs(cx, dvec2(cross.pos.x + (16.0 - xw) * 0.5, ry - 5.0), "×");
                self.spots.push((cross, Spot::Remove(i)));
            }
            y += ROW;
            // Unfolded: every parameter written, a slider for each number.
            if unfolded {
                for param in params[i].iter().filter(|p| p.written.is_some()) {
                    let py = y + PARAM * 0.5;
                    self.draw_small.color = color(p.label, lit);
                    let (label, _) = fit_text(&self.draw_small, cx, &param.name, LABEL - 30.0);
                    self.draw_small.draw_abs(cx, dvec2(x0 + 30.0, py - 5.0), &label);
                    match number_of(item, param) {
                        Some((v, num)) => {
                            self.draw_param(cx, &param.name, v, num, x0 + LABEL + 8.0, right - 52.0, py, lit, false);
                        }
                        None => {
                            let value = param.written.clone().unwrap_or_default();
                            let shown = if value.trim_start().starts_with("fn") { "fn(t) … (animated in code)".to_string() } else { value.split_whitespace().collect::<Vec<_>>().join(" ") };
                            let tx = x0 + LABEL + 8.0;
                            let (shown, _) = fit_text(&self.draw_small, cx, &shown, (right - 8.0 - tx).max(10.0));
                            self.draw_small.color = color(p.dim_hi, lit);
                            self.draw_small.draw_abs(cx, dvec2(tx, py - 5.0), &shown);
                        }
                    }
                    y += PARAM;
                }
                y += 6.0;
            }
        }
        if !self.note.is_empty() {
            self.draw_small.color = color(p.warm, 1.0);
            self.draw_small.draw_abs(cx, dvec2(x0 + 4.0, y + 4.0), &self.note);
        }
        DrawStep::done()
    }
}

impl PanelEffects {
    /// A parameter as a slim track with a round thumb and its value; its
    /// name before it (`inline`: the key parameter in an entry's row).
    #[allow(clippy::too_many_arguments)]
    fn draw_param(&mut self, cx: &mut Cx2d, name: &str, v: f64, num: Num, x: f64, end: f64, cy: f64, lit: f32, inline: bool) {
        let (lo, hi) = (num.lo, num.hi);
        let p = makepad_look::palette::get(cx);
        let mut x = x;
        if inline {
            self.draw_small.color = color(p.muted, lit);
            let (label, _) = fit_text(&self.draw_small, cx, name, 64.0);
            self.draw_small.draw_abs(cx, dvec2(x, cy - 5.0), &label);
            x += 70.0;
        }
        let w = (end - x).max(24.0);
        let t = if hi > lo { ((v - lo) / (hi - lo)).clamp(0.0, 1.0) } else { 0.0 };
        let track = Rect { pos: dvec2(x, cy - 1.5), size: dvec2(w, 3.0) };
        self.draw_shape.fill_round(cx, track, color(p.track, 1.0), 1.5);
        self.draw_shape.fill_round(cx, Rect { pos: track.pos, size: dvec2(w * t, 3.0) }, color(p.accent, 0.9 * lit), 1.5);
        self.draw_shape.edged_round(cx, Rect { pos: dvec2(x + w * t - 5.0, cy - 5.0), size: dvec2(10.0, 10.0) }, color(p.text_bright, lit), color(p.row, 1.0), 1.0, 5.0);
        self.draw_small.color = color(p.text_bright, lit);
        self.draw_small.draw_abs(cx, dvec2(x + w + 8.0, cy - 5.0), &num.code(v));
        self.spots.push((Rect { pos: dvec2(x - 4.0, cy - 9.0), size: dvec2(w + 8.0, 18.0) }, Spot::Track(num)));
    }
}

impl PanelEffectsRef {
    /// The entry the stack asked its host to show, in this batch of actions.
    pub fn selected(&self, actions: &Actions) -> Option<String> {
        actions.filter_widget_actions_cast::<EffectsAction>(self.widget_uid()).find_map(|a| match a {
            EffectsAction::Select(path) => Some(path),
            _ => None,
        })
    }

    /// The entry the pointer went onto (`Some(None)`: it left), in this
    /// batch of actions.
    pub fn hovered(&self, actions: &Actions) -> Option<Option<String>> {
        actions.filter_widget_actions_cast::<EffectsAction>(self.widget_uid()).filter_map(|a| match a {
            EffectsAction::Hover(path) => Some(path),
            _ => None,
        }).last()
    }

    pub fn set_selected(&self, cx: &mut Cx, path: Option<String>) {
        if let Some(mut inner) = self.borrow_mut() {
            inner.set_selected(cx, path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Num;

    #[test]
    fn a_float_written_whole_stays_a_float() {
        let n = Num { target: String::new(), lo: 0.0, hi: 2.0, step: 0.01, whole: false };
        assert_eq!(n.code(1.0), "1.0");
        assert_eq!(n.code(0.736), "0.74");
        let mode = Num { target: String::new(), lo: 0.0, hi: 1.0, step: 1.0, whole: true };
        assert_eq!(mode.code(0.7), "1");
        // The kit's own range and step, not the literal's form.
        let k = makepad_render_graph::kits::param("Bloom", "strength").unwrap();
        assert!(!k.whole && k.max >= 2.0);
    }
}
