//! The Steps editor (design HSTRIP): the selected part's build as one row of named steps, in
//! build order, under the plate view. A click on a step shows the part as it was at that step
//! (the app builds that off the UI thread); the strip itself only reads [`StepsState`] and sends
//! [`StepsAction`]s. The marker is view state: it is never saved, never undone, and slicing and
//! printing always use the whole part.
//!
//! The row stays one line and never drops a step. As it narrows it first keeps every chip's
//! words, then keeps icon and value (the marker's chip keeps its words), then icons alone
//! scrolled sideways, and under [`STEPPER_BELOW`] points a stepper (`◀ 3/6 Round 1 mm ▶`).
//! Every form is priced from the full words each draw, so widening restores them.

use crate::api::*;
use makepad_widgets_core::tip::TipAction;
use makepad_widgets_core::*;

use crate::print::trays::DrawFabChip;

/// One step of the shown part, as the strip draws and speaks it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct StepChip {
    /// The step's number in build order, from one.
    pub n: usize,
    /// The solid's script id (its let name).
    pub id: String,
    /// The person's word for the builder (Round, Cut, ...).
    pub verb: String,
    /// The headline value in display units (`1 mm`).
    pub value: Option<String>,
    /// The one-based source line.
    pub line: u32,
    /// The other operands of a boolean, one per branch (`+gussets ×2`).
    pub badges: Vec<String>,
    /// The sum of the badges' counts: what a folded chip shows as `×N`.
    pub folded: usize,
    /// The script bypasses this solid as an operand.
    pub off: bool,
    /// The script function that made this solid.
    pub made_in: Option<String>,
    /// Other objects built on this solid.
    pub shared_with: Vec<String>,
    /// Its written selection took something else than before (the selector report).
    pub drift: Option<String>,
    /// Its draft statement lost its selector.
    pub lost: Option<String>,
    /// Why the last attempted step edit could not build this step.
    pub broken: Option<String>,
}

/// What the strip shows. The app fills it on each commit and selection change; chips are
/// addressed by their index in `chips` (step number minus one).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct StepsState {
    /// The plate part whose steps these are (`instance/solid`).
    pub part: Option<String>,
    /// The part's name as the person reads it.
    pub name: String,
    pub chips: Vec<StepChip>,
    /// The chip the person marked; none: at the end, the whole part.
    pub marker: Option<usize>,
    /// The marked step whose parameter rows are being edited.
    pub editing: Option<usize>,
    /// The chip whose preview is being built.
    pub busy: Option<usize>,
    /// The chip whose preview the view shows.
    pub shown: Option<usize>,
    /// The installed committed step scene's revision. Cleared for requests and drafts.
    pub installed: Option<u64>,
    /// Why a marked step does not show in the view (it shows in Design's model view only).
    pub preview_off: Option<&'static str>,
}

/// What the strip asks of the app.
#[derive(Clone, Debug)]
pub enum StepsAction {
    /// Preview chip `i`; none goes back to the end (the whole part).
    Show(Option<usize>),
    /// Edit this chip's values, or finish editing the marked step.
    Edit(Option<usize>),
    /// End the part at chip `i`.
    RollBack(usize),
    /// The pointer is over badge `.1` of chip `.0`: its first operand lights up in the view.
    HoverBadge(Option<(usize, usize)>),
}

/// The words the strip says when a marked step cannot show in the view.
pub const PREVIEW_OFF: &str = "Preview shows in Design's model view";

/// Under this width (points) the strip is a stepper.
pub const STEPPER_BELOW: f64 = 140.0;

/// How much of each chip shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tier {
    /// Every chip with its words; badges as pills.
    Words,
    /// Icon and value; the marker's chip keeps its words; badges fold into a count.
    Value,
    /// Icons alone, scrolled sideways.
    Icons,
    /// One step at a time: `◀ 3/6 Round 1 mm ▶`.
    Stepper,
}

/// The fullest form that fits `width`: `words` and `value` are the widths those forms need.
pub fn tier(width: f64, words: f64, value: f64) -> Tier {
    if width < STEPPER_BELOW {
        Tier::Stepper
    } else if words <= width {
        Tier::Words
    } else if value <= width {
        Tier::Value
    } else {
        Tier::Icons
    }
}

/// Where a click on chip `i` of `len` goes: the last chip is the whole part.
pub fn target(i: usize, len: usize) -> Option<usize> {
    (i + 1 < len).then_some(i)
}

/// The chip the marker is on.
fn position(marker: Option<usize>, len: usize) -> usize {
    marker.unwrap_or(len.saturating_sub(1)).min(len.saturating_sub(1))
}

/// Where a step back (`-1`) or forward (`1`) from the marker goes, never past either end.
pub fn step(marker: Option<usize>, len: usize, delta: isize) -> Option<usize> {
    target(position(marker, len).saturating_add_signed(delta).min(len.saturating_sub(1)), len)
}

/// What the status says.
pub fn status(s: &StepsState) -> String {
    if let (Some(why), Some(_)) = (s.preview_off, s.marker) {
        return why.to_string();
    }
    match s.shown {
        Some(i) => format!("Showing step {} of {}", i + 1, s.chips.len()),
        None if s.chips.len() == 1 => "Showing its one step".to_string(),
        None => format!("Showing all {} steps", s.chips.len()),
    }
}

/// The name a chip answers to.
pub fn chip_name(c: &StepChip, of: usize) -> String {
    let mut name = format!("Step {} of {of}: {}", c.n, c.verb);
    if let Some(v) = &c.value {
        name.push(' ');
        name.push_str(v);
    }
    name.push_str(&format!(", {}, line {}", c.id, c.line));
    if c.off {
        name.push_str(", bypassed");
    }
    if let Some(f) = &c.made_in {
        name.push_str(&format!(", made by {f}()"));
    }
    if !c.shared_with.is_empty() {
        name.push_str(&format!(", also {}", c.shared_with.join(", ")));
    }
    if let Some(d) = &c.drift {
        name.push_str(&format!(", selector changed: {d}"));
    }
    if let Some(l) = &c.lost {
        name.push_str(&format!(", lost its selector: {l}"));
    }
    for b in &c.badges {
        name.push_str(&format!(", {b}"));
    }
    if let Some(why) = &c.broken {
        name.push_str(&format!(", breaks: {why}"));
    }
    name
}

pub fn fn_tool_reason(name: &str) -> String {
    format!("This step is made inside fn `{name}` and has no statement of its own. Change `{name}`, or build on what it returns.")
}

/// What a chip's tip says: its words and what the chip is marked with.
fn chip_tip(c: &StepChip) -> String {
    let mut tip = format!("{}{}", c.verb, c.value.as_ref().map(|v| format!(" {v}")).unwrap_or_default());
    tip.push_str(&format!(" · {} · line {}", c.id, c.line));
    if let Some(f) = &c.made_in {
        tip.push_str(&format!(" · {}", fn_tool_reason(f)));
    }
    if !c.shared_with.is_empty() {
        tip.push_str(&format!(" · also {}", c.shared_with.join(", ")));
    }
    if let Some(d) = &c.drift {
        tip.push_str(&format!(" · check: {d}"));
    }
    if let Some(l) = &c.lost {
        tip.push_str(&format!(" · {l}: pick it"));
    }
    if c.off {
        tip.push_str(" · bypassed");
    }
    if let Some(why) = &c.broken {
        tip.push_str(&format!(" · breaks: {why}"));
    }
    tip
}

const ROW_H: f64 = 30.0;
const CHIP_H: f64 = 22.0;
const PAD: f64 = 6.0;
const ICON: f64 = 14.0;
const GAP: f64 = 4.0;
const MARK_W: f64 = 2.0;
const ARROW_W: f64 = 18.0;

script_mod! {
    use mod.prelude.fab.*
    use mod.widgets.*

    mod.widgets.FabStepsPanelBase = #(FabStepsPanel::register_widget(vm))
    mod.widgets.FabStepsPanel = set_type_default() do mod.widgets.FabStepsPanelBase{
        width: Fill
        height: 30
        draw_chip: mod.draw.DrawFabChip{ radius: 3.0 }
        draw_icon_cube: mod.draw.DrawSvg{ svg: crate_resource("self://resources/icons/cube.svg") color: fab.color_text_dim }
        draw_icon_cylinder: mod.draw.DrawSvg{ svg: crate_resource("self://resources/icons/add_cylinder.svg") color: fab.color_text_dim }
        draw_icon_sphere: mod.draw.DrawSvg{ svg: crate_resource("self://resources/icons/add_sphere.svg") color: fab.color_text_dim }
        draw_icon_cut: mod.draw.DrawSvg{ svg: crate_resource("self://resources/icons/cut_box.svg") color: fab.color_text_dim }
        draw_icon_join: mod.draw.DrawSvg{ svg: crate_resource("self://resources/icons/plus.svg") color: fab.color_text_dim }
        draw_icon_hole: mod.draw.DrawSvg{ svg: crate_resource("self://resources/icons/cut_cylinder.svg") color: fab.color_text_dim }
        draw_icon_mirror: mod.draw.DrawSvg{ svg: crate_resource("self://resources/icons/swap_sides.svg") color: fab.color_text_dim }
        draw_icon_shell: mod.draw.DrawSvg{ svg: crate_resource("self://resources/icons/closed_box.svg") color: fab.color_text_dim }
        draw_icon_import: mod.draw.DrawSvg{ svg: crate_resource("self://resources/icons/import.svg") color: fab.color_text_dim }
        draw_icon_pull: mod.draw.DrawSvg{ svg: crate_resource("self://resources/icons/move.svg") color: fab.color_text_dim }
        draw_icon_blend: mod.draw.DrawSvg{ svg: crate_resource("self://resources/icons/drop.svg") color: fab.color_text_dim }
        draw_icon_round: mod.draw.DrawSvg{ svg: crate_resource("self://resources/icons/round.svg") color: fab.color_text_dim }
        draw_icon_chamfer: mod.draw.DrawSvg{ svg: crate_resource("self://resources/icons/chamfer.svg") color: fab.color_text_dim }
        draw_icon_extrude: mod.draw.DrawSvg{ svg: crate_resource("self://resources/icons/extrude.svg") color: fab.color_text_dim }
        draw_icon_other: mod.draw.DrawSvg{ svg: crate_resource("self://resources/icons/shape.svg") color: fab.color_text_dim }
        draw_verb: mod.draw.DrawText{
            color: fab.color_text
            text_style: theme.font_regular{ font_size: fab.font_size_ui }
        }
        draw_note: mod.draw.DrawText{
            color: fab.color_text_dim
            text_style: theme.font_regular{ font_size: fab.font_size_small }
        }
        color_selected: fab.color_selection_bg
        color_hover: fab.color_row_hover
        color_error: fab.color_error
        color_check: fab.color_warning
        color_marker: fab.color_accent
    }
}

/// One chip's rectangles for hits and a11y.
#[derive(Clone, Default)]
struct Hit1 {
    /// The chip and its badges.
    rect: Rect,
    badges: Vec<Rect>,
}

/// What a pointer press is on.
#[derive(Clone, Copy, PartialEq)]
enum Over {
    Chip(usize),
    Edit(usize),
    Previous,
    Next,
    ScrollBack,
    ScrollForward,
}

#[derive(Script, ScriptHook, Widget)]
pub struct FabStepsPanel {
    #[uid]
    uid: WidgetUid,
    #[walk]
    walk: Walk,
    #[redraw]
    #[live]
    draw_chip: DrawFabChip,
    #[live]
    draw_icon_cube: DrawSvg,
    #[live]
    draw_icon_cylinder: DrawSvg,
    #[live]
    draw_icon_sphere: DrawSvg,
    #[live]
    draw_icon_cut: DrawSvg,
    #[live]
    draw_icon_join: DrawSvg,
    #[live]
    draw_icon_hole: DrawSvg,
    #[live]
    draw_icon_mirror: DrawSvg,
    #[live]
    draw_icon_shell: DrawSvg,
    #[live]
    draw_icon_import: DrawSvg,
    #[live]
    draw_icon_pull: DrawSvg,
    #[live]
    draw_icon_blend: DrawSvg,
    #[live]
    draw_icon_round: DrawSvg,
    #[live]
    draw_icon_chamfer: DrawSvg,
    #[live]
    draw_icon_extrude: DrawSvg,
    #[live]
    draw_icon_other: DrawSvg,
    #[live]
    draw_verb: DrawText,
    #[live]
    draw_note: DrawText,
    #[live]
    color_selected: Vec4f,
    #[live]
    color_hover: Vec4f,
    #[live]
    color_error: Vec4f,
    #[live]
    color_check: Vec4f,
    #[live]
    color_marker: Vec4f,
    #[rust]
    area: Area,
    /// The chips as last drawn (a chip that is not drawn has the whole strip's rectangle).
    #[rust]
    hits: Vec<Hit1>,
    /// The form the last draw chose.
    #[rust]
    form: Option<Tier>,
    /// The stepper's buttons, and the icon form's scroll arrows.
    #[rust]
    buttons: [Rect; 4],
    #[rust]
    pencil: Option<Rect>,
    #[rust]
    rect: Rect,
    /// The state as last drawn, for the a11y tree and the keys.
    #[rust]
    seen: StepsState,
    #[rust]
    hover: Option<usize>,
    #[rust]
    hover_badge: Option<(usize, usize)>,
    /// The chips changed under the pointer (another part, or its steps): the tip and the badge's
    /// light go out on the next frame, as a hover out would take them.
    #[rust]
    hover_gone: bool,
    /// The chip last sent while the pointer is held down (a scrub sends one only when it changes).
    #[rust]
    scrub: Option<usize>,
    #[rust]
    pressed: bool,
    /// The icon form's sideways scroll, points.
    #[rust]
    scroll: f64,
}

impl FabStepsPanel {
    fn icon(&mut self, verb: &str) -> &mut DrawSvg {
        match verb {
            "Box" => &mut self.draw_icon_cube,
            "Cylinder" | "Cone" => &mut self.draw_icon_cylinder,
            "Sphere" | "Torus" => &mut self.draw_icon_sphere,
            "Cut" | "Intersect" => &mut self.draw_icon_cut,
            "Join" => &mut self.draw_icon_join,
            "Hole" => &mut self.draw_icon_hole,
            "Mirror" => &mut self.draw_icon_mirror,
            "Shell" => &mut self.draw_icon_shell,
            "Import" => &mut self.draw_icon_import,
            "Pull" => &mut self.draw_icon_pull,
            "Blend" => &mut self.draw_icon_blend,
            "Round" => &mut self.draw_icon_round,
            "Chamfer" => &mut self.draw_icon_chamfer,
            "Extrude" | "Revolve" | "Sweep" | "Loft" => &mut self.draw_icon_extrude,
            _ => &mut self.draw_icon_other,
        }
    }

    fn width_of(&mut self, cx: &mut Cx2d, small: bool, text: &str) -> f64 {
        let draw = if small { &mut self.draw_note } else { &mut self.draw_verb };
        draw.prepare_single_line_run(cx, text).map_or(0.0, |run| run.width_in_lpxs as f64)
    }

    /// What is under `abs`.
    fn over(&self, abs: DVec2) -> Option<Over> {
        let tier = self.form?;
        if tier == Tier::Stepper {
            if self.buttons[0].contains(abs) { return Some(Over::Previous); }
            if self.buttons[1].contains(abs) { return Some(Over::Next); }
        } else if tier == Tier::Icons {
            if self.buttons[2].contains(abs) {
                return Some(Over::ScrollBack);
            }
            if self.buttons[3].contains(abs) {
                return Some(Over::ScrollForward);
            }
        }
        if self.pencil.is_some_and(|r| r.contains(abs)) {
            return Some(Over::Edit(position(self.seen.marker, self.seen.chips.len())));
        }
        self.hits.iter().position(|h| h.rect != self.rect && h.rect.contains(abs)).map(Over::Chip)
    }

    fn draw_pencil(&mut self, cx: &mut Cx2d, rect: Rect) {
        self.draw_note.draw_abs(cx, dvec2(rect.pos.x + 2.0, rect.pos.y + 3.0), "✎");
        self.pencil = Some(rect);
    }

    fn show(&mut self, cx: &mut Cx, to: Option<usize>) {
        cx.action(StepsAction::Show(to));
        self.area.redraw(cx);
    }
}

impl Widget for FabStepsPanel {
    fn semantics(&self, _cx: &Cx, node: &mut a11y_tree::Node) {
        node.role = a11y_tree::Role::List;
        makepad_platform::a11y::set_string(&mut node.name, &format!("Steps of {}", self.seen.name));
        node.set_size = self.seen.chips.len() as u32;
    }

    fn semantics_parts(&self, _cx: &Cx, out: &mut Vec<(u64, Option<u64>, a11y_tree::Node)>) {
        let of = self.seen.chips.len();
        for (n, chip) in self.seen.chips.iter().enumerate() {
            let mut node = a11y_tree::Node {
                role: a11y_tree::Role::ListItem,
                bounds: makepad_platform::a11y::semantic_rect(self.hits.get(n).map_or(self.rect, |h| h.rect)),
                pos_in_set: n as u32 + 1,
                set_size: of as u32,
                actions: a11y_tree::Actions::PRESS | a11y_tree::Actions::SELECT | a11y_tree::Actions::CUSTOM,
                custom_actions: vec!["edit".into()],
                ..Default::default()
            };
            if n + 1 < of {
                node.custom_actions.push("roll back".into());
            }
            if self.seen.editing.or(self.seen.shown).map_or(n + 1 == of, |shown| shown == n) {
                node.states.set(a11y_tree::States::SELECTED, true);
            }
            makepad_platform::a11y::set_string(&mut node.name, &chip_name(chip, of));
            out.push((n as u64 + 1, None, node));
        }
        if of > 0 {
            let mut node = a11y_tree::Node { role: a11y_tree::Role::Status, bounds: makepad_platform::a11y::semantic_rect(self.rect), ..Default::default() };
            makepad_platform::a11y::set_string(&mut node.name, &status(&self.seen));
            out.push((STATUS_KEY, None, node));
            if self.form == Some(Tier::Stepper) {
                for (key, rect, name) in [(PREVIOUS_KEY, self.buttons[0], "Previous step"), (NEXT_KEY, self.buttons[1], "Next step")] {
                    let mut node = a11y_tree::Node { role: a11y_tree::Role::Button, bounds: makepad_platform::a11y::semantic_rect(rect), actions: a11y_tree::Actions::PRESS, ..Default::default() };
                    makepad_platform::a11y::set_string(&mut node.name, name);
                    out.push((key, None, node));
                }
            }
        }
    }

    fn handle_action(&mut self, cx: &mut Cx, request: &a11y_tree::ActionRequest, _scope: &mut Scope) -> a11y_tree::ActionResult {
        use a11y_tree::{ActionResult, Actions};
        let Some(key) = cx.a11y.action_part_key(request.node) else { return ActionResult::Unhandled };
        let len = self.seen.chips.len();
        if request.action == Actions::CUSTOM && (1..=len as u64).contains(&key) {
            let i = key as usize - 1;
            let a11y_tree::ActionArg::Custom(name) = &request.arg else { return ActionResult::Unhandled };
            let action = match name.as_str() {
                "edit" => StepsAction::Edit(Some(i)),
                "roll back" if i + 1 < len => StepsAction::RollBack(i),
                _ => return ActionResult::Unhandled,
            };
            cx.set_key_focus(self.area);
            cx.action(action);
            return ActionResult::Handled;
        }
        if request.action != Actions::PRESS && request.action != Actions::SELECT {
            return ActionResult::Unhandled;
        }
        let to = match key {
            PREVIOUS_KEY => step(self.seen.marker, len, -1),
            NEXT_KEY => step(self.seen.marker, len, 1),
            k if (1..=len as u64).contains(&k) => target(k as usize - 1, len),
            _ => return ActionResult::Unsupported,
        };
        cx.set_key_focus(self.area);
        self.show(cx, to);
        ActionResult::Handled
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event, _scope: &mut Scope) {
        if let Event::NextFrame(_) = event {
            if self.seen.busy.is_some() {
                self.area.redraw(cx);
            }
            if std::mem::take(&mut self.hover_gone) {
                cx.widget_action(self.uid, TipAction::HoverOut);
                cx.action(StepsAction::HoverBadge(None));
            }
        }
        let len = self.seen.chips.len();
        match event.hits(cx, self.area) {
            Hit::FingerHoverIn(fe) | Hit::FingerHoverOver(fe) => {
                let over = self.over(fe.abs);
                let chip = match over { Some(Over::Chip(i) | Over::Edit(i)) => Some(i), _ => None };
                if over.is_some() { cx.set_cursor(MouseCursor::Hand); }
                let badge = chip.and_then(|i| self.hits[i].badges.iter().position(|b| b.contains(fe.abs)).map(|b| (i, b)));
                if chip != self.hover {
                    self.hover = chip;
                    self.area.redraw(cx);
                    let tip = match (chip, self.seen.chips.get(chip.unwrap_or(usize::MAX))) {
                        (Some(i), Some(c)) => TipAction::HoverIn(chip_tip(c), self.hits[i].rect),
                        _ => TipAction::HoverOut,
                    };
                    cx.widget_action(self.uid, tip);
                }
                if badge != self.hover_badge {
                    self.hover_badge = badge;
                    cx.action(StepsAction::HoverBadge(badge));
                }
            }
            Hit::FingerHoverOut(_) => {
                if self.hover.take().is_some() {
                    self.area.redraw(cx);
                    cx.widget_action(self.uid, TipAction::HoverOut);
                }
                if self.hover_badge.take().is_some() {
                    cx.action(StepsAction::HoverBadge(None));
                }
            }
            Hit::FingerDown(fe) if fe.device.is_primary_hit() => {
                cx.set_key_focus(self.area);
                match self.over(fe.abs) {
                    Some(Over::Edit(i)) => cx.action(StepsAction::Edit(Some(i))),
                    Some(Over::Chip(i)) if fe.tap_count == 2 => cx.action(StepsAction::Edit(Some(i))),
                    Some(Over::Chip(i)) => {
                        self.pressed = true;
                        self.scrub = Some(i);
                        self.show(cx, target(i, len));
                    }
                    Some(Over::Previous) => self.show(cx, step(self.seen.marker, len, -1)),
                    Some(Over::Next) => self.show(cx, step(self.seen.marker, len, 1)),
                    Some(Over::ScrollBack) => {
                        self.scroll = (self.scroll - 80.0).max(0.0);
                        self.area.redraw(cx);
                    }
                    Some(Over::ScrollForward) => {
                        self.scroll += 80.0;
                        self.area.redraw(cx);
                    }
                    None => {}
                }
            }
            Hit::FingerMove(fe) if self.pressed => {
                if let Some(Over::Chip(i)) = self.over(fe.abs) {
                    if self.scrub != Some(i) {
                        self.scrub = Some(i);
                        self.show(cx, target(i, len));
                    }
                }
            }
            Hit::FingerUp(_) => {
                self.pressed = false;
                self.scrub = None;
            }
            Hit::KeyDown(k) if len > 0 => {
                let to = match k.key_code {
                    KeyCode::ReturnKey => {
                        cx.action(StepsAction::Edit(Some(position(self.seen.marker, len))));
                        None
                    }
                    KeyCode::Escape if self.seen.editing.is_some() => {
                        cx.action(StepsAction::Edit(None));
                        None
                    }
                    KeyCode::ArrowLeft => Some(step(self.seen.marker, len, -1)),
                    KeyCode::ArrowRight => Some(step(self.seen.marker, len, 1)),
                    KeyCode::Home => Some(target(0, len)),
                    KeyCode::End | KeyCode::Escape => Some(None),
                    _ => None,
                };
                if let Some(to) = to {
                    self.show(cx, to);
                }
            }
            _ => {}
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let mut walk = walk;
        walk.height = Size::Fixed(ROW_H);
        let rect = cx.walk_turtle(walk);
        cx.add_rect_area(&mut self.area, rect);
        self.rect = rect;
        self.hits.clear();
        self.pencil = None;
        let Some(state) = scope.data.get::<AppState>() else { return DrawStep::done() };
        let s = state.steps.clone();
        if (s.part != self.seen.part || s.chips != self.seen.chips) && (self.hover.take().is_some() | self.hover_badge.take().is_some()) {
            self.hover_gone = true;
            cx.new_next_frame();
        }
        self.seen = s.clone();
        let (muted, ink) = (self.draw_note.color, self.draw_verb.color);
        let mid = rect.pos.y + ROW_H * 0.5;
        if s.part.is_none() || s.chips.is_empty() {
            self.form = None;
            let words = if s.part.is_none() { "Select a part to see how it was made" } else { "This part has no steps in its script" };
            self.draw_note.color = muted;
            self.draw_note.draw_abs(cx, dvec2(rect.pos.x + 10.0, mid - 7.0), words);
            return DrawStep::done();
        }
        let len = s.chips.len();
        let at = position(s.marker, len);

        // Price every form from the full words, so widening restores them.
        let lead_w = self.width_of(cx, true, &s.name).min(120.0);
        let lead = lead_w + 12.0;
        struct Cost { words: f64, value: f64, pills: Vec<f64>, count: f64 }
        let mut costs = Vec::with_capacity(len);
        for (i, c) in s.chips.iter().enumerate() {
            let verb = self.width_of(cx, false, &c.verb);
            let value = c.value.as_deref().map_or(0.0, |v| self.width_of(cx, false, v));
            let id = self.width_of(cx, true, &c.id);
            let pills: Vec<f64> = c.badges.iter().map(|b| self.width_of(cx, true, b) + 10.0).collect();
            let count = if c.folded > 0 { self.width_of(cx, true, &format!("×{}", c.folded)) } else { 0.0 };
            let words = PAD + ICON + GAP + verb + if c.value.is_some() { GAP + value } else { 0.0 } + GAP * 1.5 + id + PAD;
            let words = words + if i == at { ICON + GAP } else { 0.0 };
            costs.push(Cost { words, value, pills, count });
        }
        let pill_total = |c: &Cost| c.pills.iter().map(|p| GAP + p).sum::<f64>();
        let words_total = lead + costs.iter().map(|c| c.words + pill_total(c) + GAP).sum::<f64>() + MARK_W;
        let short = |i: usize, c: &Cost| -> f64 {
            if i == at {
                c.words + if c.count > 0.0 { GAP + c.count } else { 0.0 }
            } else {
                PAD + ICON + if s.chips[i].value.is_some() { GAP + c.value } else { 0.0 } + if c.count > 0.0 { GAP + c.count } else { 0.0 } + PAD
            }
        };
        let value_total = lead + costs.iter().enumerate().map(|(i, c)| short(i, c) + GAP).sum::<f64>() + MARK_W;
        let form = tier(rect.size.x, words_total, value_total);
        self.form = Some(form);
        self.buttons = [Rect::default(); 4];
        let dim = |on: bool| if on { 1.0 } else { 0.45 };

        let h = CHIP_H;
        let top = mid - h * 0.5;
        let busy_alpha = (0.5 + 0.5 * (Cx::monotonic_now() * 5.0).sin()) as f32;
        let right = rect.pos.x + rect.size.x;

        if form == Tier::Stepper {
            let c = &s.chips[at];
            let prev = Rect { pos: dvec2(rect.pos.x + 2.0, top), size: dvec2(ARROW_W, h) };
            let next = Rect { pos: dvec2(right - 2.0 - ARROW_W, top), size: dvec2(ARROW_W, h) };
            self.buttons[0] = prev;
            self.buttons[1] = next;
            for (r, glyph, on) in [(prev, "◀", at > 0), (next, "▶", at + 1 < len)] {
                self.draw_note.color = if on { ink } else { vec4(muted.x, muted.y, muted.z, muted.w * 0.4) };
                self.draw_note.draw_abs(cx, dvec2(r.pos.x + 3.0, mid - 7.0), glyph);
            }
            let text = format!("{}/{} {}{}", c.n, len, c.verb, c.value.as_ref().map(|v| format!(" {v}")).unwrap_or_default());
            self.draw_verb.color = ink;
            let body = Rect { pos: dvec2(prev.pos.x + ARROW_W, top), size: dvec2((next.pos.x - prev.pos.x - ARROW_W).max(0.0), h) };
            if s.editing == Some(at) || c.broken.is_some() {
                self.draw_chip.fill = self.color_selected;
                self.draw_chip.ring = if c.broken.is_some() { self.color_error } else { self.color_marker };
                self.draw_chip.ring_width = 1.0;
                self.draw_chip.radius = 3.0;
                self.draw_chip.draw_abs(cx, body);
            }
            let room = (body.size.x - 4.0 - ICON - GAP).max(0.0);
            cx.begin_turtle(Walk::abs_rect(Rect { pos: dvec2(body.pos.x + 4.0, rect.pos.y), size: dvec2(room, ROW_H) }), Layout::default().with_clip(true, true));
            self.draw_verb.draw_abs(cx, dvec2(prev.pos.x + ARROW_W + 4.0, mid - 8.0), &text);
            cx.end_turtle();
            self.draw_note.color = ink;
            self.draw_pencil(cx, Rect { pos: dvec2(next.pos.x - ICON - GAP, top), size: dvec2(ICON + GAP, h) });
            if s.busy == Some(at) {
                self.draw_chip.fill = vec4(self.color_marker.x, self.color_marker.y, self.color_marker.z, busy_alpha);
                self.draw_chip.ring = vec4(0.0, 0.0, 0.0, 0.0);
                self.draw_chip.radius = 2.0;
                self.draw_chip.draw_abs(cx, Rect { pos: dvec2(next.pos.x - 5.0, top + 2.0), size: dvec2(4.0, 4.0) });
            }
            self.hits = vec![Hit1 { rect, badges: Vec::new() }; len];
            self.hits[at].rect = body;
            self.draw_note.color = muted;
            if s.busy.is_some() {
                cx.new_next_frame();
            }
            return DrawStep::done();
        }

        // Where each item sits: the lead, then the chips, the marker's bar after chip `at`.
        let mut x = rect.pos.x + 6.0;
        self.draw_note.color = muted;
        if form != Tier::Icons {
            self.draw_note.draw_abs(cx, dvec2(x, mid - 7.0), &s.name);
            x += lead;
        }
        let mut item_w = Vec::with_capacity(len);
        for (i, c) in costs.iter().enumerate() {
            item_w.push(match form {
                Tier::Words => c.words + pill_total(c),
                Tier::Value => short(i, c),
                _ => PAD + ICON + PAD + if i == at { ICON + GAP } else { 0.0 },
            });
        }
        // The icon form scrolls to keep the marker's chip in view, between its arrows.
        let (view_l, view_r) = if form == Tier::Icons { (x + ARROW_W, right - ARROW_W - 4.0) } else { (x, right) };
        if form == Tier::Icons {
            let before: f64 = item_w[..at].iter().map(|w| w + GAP).sum();
            let (lo, hi) = (before, before + item_w[at] + MARK_W);
            let room = (view_r - view_l).max(0.0);
            if lo < self.scroll { self.scroll = lo; }
            if hi > self.scroll + room { self.scroll = hi - room; }
            let total: f64 = item_w.iter().map(|w| w + GAP).sum::<f64>() + MARK_W;
            self.scroll = self.scroll.min((total - room).max(0.0)).max(0.0);
            let back = Rect { pos: dvec2(x, top), size: dvec2(ARROW_W, h) };
            let forward = Rect { pos: dvec2(right - ARROW_W - 2.0, top), size: dvec2(ARROW_W, h) };
            self.buttons[2] = back;
            self.buttons[3] = forward;
            for (r, glyph, on) in [(back, "◀", self.scroll > 0.5), (forward, "▶", self.scroll + room < total - 0.5)] {
                self.draw_note.color = if on { ink } else { vec4(muted.x, muted.y, muted.z, muted.w * 0.4) };
                self.draw_note.draw_abs(cx, dvec2(r.pos.x + 3.0, mid - 7.0), glyph);
            }
            x = view_l - self.scroll;
            cx.begin_turtle(Walk::abs_rect(Rect { pos: dvec2(view_l, rect.pos.y), size: dvec2((view_r - view_l).max(0.0), ROW_H) }), Layout::default().with_clip(true, true));
        }
        for (i, c) in s.chips.iter().enumerate() {
            let w = item_w[i];
            let body_w = match form { Tier::Words => costs[i].words, _ => w };
            let body = Rect { pos: dvec2(x, top), size: dvec2(body_w, h) };
            let alpha = dim(i <= at);
            let on = s.editing.or(s.shown).map_or(i + 1 == len, |shown| i == shown);
            self.draw_chip.fill = if on { self.color_selected } else if self.hover == Some(i) { self.color_hover } else { vec4(0.0, 0.0, 0.0, 0.0) };
            self.draw_chip.ring = if c.broken.is_some() || c.lost.is_some() { self.color_error } else if s.editing == Some(i) { self.color_marker } else if c.drift.is_some() { self.color_check } else { vec4(0.0, 0.0, 0.0, 0.0) };
            self.draw_chip.ring_width = 1.5;
            self.draw_chip.radius = 3.0;
            self.draw_chip.draw_abs(cx, body);
            let icon = Rect { pos: dvec2(body.pos.x + PAD, mid - ICON * 0.5), size: dvec2(ICON, ICON) };
            let mut ic = ink;
            ic.w *= alpha as f32;
            let draw_icon = self.icon(&c.verb);
            draw_icon.color = ic;
            draw_icon.draw_abs(cx, icon);
            let mut tx = icon.pos.x + ICON + GAP;
            let full = form == Tier::Words || (form == Tier::Value && i == at);
            let mut text_color = ink;
            text_color.w *= alpha as f32;
            let mut dim_color = muted;
            dim_color.w *= alpha as f32;
            if full {
                self.draw_verb.color = text_color;
                self.draw_verb.draw_abs(cx, dvec2(tx, mid - 8.0), &c.verb);
                tx += self.width_of(cx, false, &c.verb);
            }
            if let (true, Some(v)) = (full || form == Tier::Value, &c.value) {
                if full { tx += GAP; }
                self.draw_verb.color = text_color;
                self.draw_verb.draw_abs(cx, dvec2(tx, mid - 8.0), v);
                tx += self.width_of(cx, false, v);
            }
            if full {
                self.draw_note.color = dim_color;
                self.draw_note.draw_abs(cx, dvec2(tx + GAP * 1.5, mid - 7.0), &c.id);
            }
            if form != Tier::Words && form != Tier::Icons && c.folded > 0 {
                self.draw_note.color = dim_color;
                self.draw_note.draw_abs(cx, dvec2(tx + GAP, mid - 7.0), &format!("×{}", c.folded));
            }
            if c.off && form != Tier::Icons {
                self.draw_chip.fill = text_color;
                self.draw_chip.ring = vec4(0.0, 0.0, 0.0, 0.0);
                self.draw_chip.draw_abs(cx, Rect { pos: dvec2(body.pos.x + PAD, mid), size: dvec2((body.size.x - 2.0 * PAD).max(0.0), 1.0) });
            }
            if i == at {
                self.draw_note.color = ink;
                self.draw_pencil(cx, Rect { pos: dvec2(body.pos.x + body.size.x - ICON - GAP, top), size: dvec2(ICON + GAP, h) });
            }
            if s.busy == Some(i) {
                self.draw_chip.fill = vec4(self.color_marker.x, self.color_marker.y, self.color_marker.z, busy_alpha);
                self.draw_chip.ring = vec4(0.0, 0.0, 0.0, 0.0);
                self.draw_chip.radius = 2.0;
                self.draw_chip.draw_abs(cx, Rect { pos: dvec2(body.pos.x + body.size.x - 7.0, top + 2.0), size: dvec2(4.0, 4.0) });
            }
            let mut item = body;
            let mut badges = Vec::new();
            if form == Tier::Words {
                let mut px = body.pos.x + body.size.x + GAP;
                for (b, pw) in c.badges.iter().zip(&costs[i].pills) {
                    let pill = Rect { pos: dvec2(px, mid - 8.0), size: dvec2(*pw, 16.0) };
                    self.draw_chip.fill = if self.hover_badge == Some((i, badges.len())) { self.color_hover } else { vec4(muted.x, muted.y, muted.z, 0.18 * alpha as f32) };
                    self.draw_chip.radius = 8.0;
                    self.draw_chip.draw_abs(cx, pill);
                    self.draw_note.color = dim_color;
                    self.draw_note.draw_abs(cx, dvec2(px + 5.0, mid - 6.5), b);
                    badges.push(pill);
                    px += pw + GAP;
                }
                item.size.x = (px - GAP - body.pos.x).max(body.size.x);
            }
            self.hits.push(Hit1 { rect: item, badges });
            x += w + GAP;
            if i == at {
                self.draw_chip.fill = self.color_marker;
                self.draw_chip.ring = vec4(0.0, 0.0, 0.0, 0.0);
                self.draw_chip.radius = 1.0;
                self.draw_chip.draw_abs(cx, Rect { pos: dvec2(x - GAP * 0.5 - MARK_W * 0.5, top - 2.0), size: dvec2(MARK_W, h + 4.0) });
                x += MARK_W;
            }
        }
        if form == Tier::Icons {
            cx.end_turtle();
        }
        if let (Some(why), Some(_)) = (s.preview_off, s.marker) {
            let w = self.width_of(cx, true, why);
            if x + w + 12.0 <= right {
                self.draw_note.color = muted;
                self.draw_note.draw_abs(cx, dvec2(right - w - 8.0, mid - 7.0), why);
            }
        }
        self.draw_verb.color = ink;
        self.draw_note.color = muted;
        if s.busy.is_some() {
            cx.new_next_frame();
        }
        DrawStep::done()
    }
}

const STATUS_KEY: u64 = 1000;
const PREVIOUS_KEY: u64 = 2001;
const NEXT_KEY: u64 = 2002;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_strip_gives_up_words_before_steps() {
        // 600 points of words, 300 of icon and value
        let at = |width: f64| tier(width, 600.0, 300.0);
        assert_eq!(at(700.0), Tier::Words, "room for every word keeps them");
        assert_eq!(at(450.0), Tier::Value, "short of the words, the chips keep icon and value");
        assert_eq!(at(300.0), Tier::Value, "exactly the value form's width still fits it");
        assert_eq!(at(200.0), Tier::Icons, "short of the value form, icons scroll");
        assert_eq!(at(139.0), Tier::Stepper, "under the stepper's width the strip is a stepper");
        assert_eq!(at(140.0), Tier::Icons, "the stepper's width itself is still a row");
    }

    #[test]
    fn a_step_goes_back_or_forward_to_the_whole_part_at_the_end() {
        assert_eq!(target(1, 4), Some(1), "a middle chip is previewed");
        assert_eq!(target(3, 4), None, "the last chip is the whole part");
        assert_eq!(step(None, 4, -1), Some(2), "back from the end is the step before the last");
        assert_eq!(step(Some(1), 4, 1), Some(2), "forward moves one chip");
        assert_eq!(step(Some(2), 4, 1), None, "forward onto the last chip is the whole part");
        assert_eq!(step(Some(0), 4, -1), Some(0), "back stops at the first step");
        assert_eq!(step(None, 4, 1), None, "forward stays at the end");
        assert_eq!(step(None, 1, -1), None, "one step is always the whole part");
    }

    #[test]
    fn a_chip_is_named_with_what_marks_it() {
        let chip = StepChip {
            n: 3, id: "rounded".into(), verb: "Round".into(), value: Some("1 mm".into()), line: 31, badges: vec!["+gussets ×2".into()], folded: 2,
            off: true, made_in: Some("leg".into()), shared_with: vec!["lid".into()], drift: Some("2 edges".into()), lost: Some("a face".into()), broken: Some("edge vanished".into()),
        };
        assert_eq!(
            chip_name(&chip, 6),
            "Step 3 of 6: Round 1 mm, rounded, line 31, bypassed, made by leg(), also lid, selector changed: 2 edges, lost its selector: a face, +gussets ×2, breaks: edge vanished",
            "the name says step, words, let name, line, then each mark in order"
        );
        assert!(chip_tip(&chip).ends_with("breaks: edge vanished"));
        let plain = StepChip { n: 1, id: "base".into(), verb: "Box".into(), line: 2, ..Default::default() };
        assert_eq!(chip_name(&plain, 6), "Step 1 of 6: Box, base, line 2", "a plain chip has no marks");
    }

    #[test]
    fn the_status_says_what_the_view_shows() {
        let chips = vec![StepChip::default(); 6];
        let mut s = StepsState { part: Some("p/p".into()), chips, ..Default::default() };
        assert_eq!(status(&s), "Showing all 6 steps", "at rest the whole part shows");
        s.marker = Some(2);
        s.shown = Some(2);
        assert_eq!(status(&s), "Showing step 3 of 6", "a shown step is numbered from one");
        s.shown = None;
        s.preview_off = Some(PREVIEW_OFF);
        assert_eq!(status(&s), PREVIEW_OFF, "a marked step that cannot show says where it does");
        let one = StepsState { part: Some("p/p".into()), chips: vec![StepChip::default()], ..Default::default() };
        assert_eq!(status(&one), "Showing its one step", "a part of one step is not counted as many");
    }
}
