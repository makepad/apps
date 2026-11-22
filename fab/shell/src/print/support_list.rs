//! The committed slice's ranked overhangs and support removal results. A press shows a line in
//! the view; a cluster's buttons (or the arrow keys) step through its patches, "2 / 297".

use super::{PrintAction, PrintState};
use super::colour_map::ellipsis;
use super::stepper::{self, RowPart};
use super::trays::DrawFabChip;
use crate::api::{AppState, MechLight, ShellAction};
use fab_printability::{OverhangLine, Unheld};
use makepad_widgets_core::*;
use makepad_widgets_core::tip::{TipAction, TipPlace, TipRequest};

script_mod! {
    use mod.prelude.fab.*
    use mod.widgets.*

    mod.widgets.FabSupportListBase = #(FabSupportList::register_widget(vm))
    mod.widgets.FabSupportList = set_type_default() do mod.widgets.FabSupportListBase{
        width: Fill
        height: Fit
        draw_chip: mod.draw.DrawFabChip{ radius: 3.0 }
        draw_hit: mod.draw.DrawFabChip{ radius: 0.0 }
        draw_text: mod.draw.DrawText{
            color: fab.color_text
            text_style: theme.font_regular{ font_size: fab.font_size_small }
        }
        draw_dim: mod.draw.DrawText{
            color: fab.color_text_dim
            text_style: theme.font_regular{ font_size: fab.font_size_small }
        }
        draw_head: mod.draw.DrawText{
            color: fab.color_text_dim
            text_style: theme.font_bold{ font_size: fab.font_size_small }
        }
        draw_prev: mod.draw.DrawSvg{ svg: crate_resource("self://resources/icons/chevron_left.svg") color: fab.color_text_dim }
        draw_next: mod.draw.DrawSvg{ svg: crate_resource("self://resources/icons/chevron_right.svg") color: fab.color_text_dim }
    }

}

#[derive(Script, ScriptHook, Widget)]
pub struct FabSupportList {
    #[uid]
    uid: WidgetUid,
    #[walk]
    walk: Walk,
    #[redraw]
    #[live]
    draw_chip: DrawFabChip,
    #[live]
    draw_hit: DrawFabChip,
    #[live]
    draw_text: DrawText,
    #[live]
    draw_dim: DrawText,
    #[live]
    draw_head: DrawText,
    #[live]
    draw_prev: DrawSvg,
    #[live]
    draw_next: DrawSvg,
    #[rust]
    area: Area,
    /// Each drawn row.
    #[rust]
    lines: Vec<ListLine>,
    #[rust]
    hover: Option<(usize, RowPart)>,
    #[rust]
    text: String,
}

/// A row as drawn: its hit box, its step buttons (a cluster's), what a press does, its tip and
/// its name.
struct ListLine {
    rect: Rect,
    steps: Option<(Rect, Rect)>,
    action: RowAction,
    tip: String,
    name: String,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum RowAction {
    /// Frame the list's line (by its place in the list's lines).
    Frame(usize),
    Check,
}

/// The overhang line shown last, known by its place and box (a new list's line there is
/// another), and which of a cluster's patches it shows (none: all of it).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Picked {
    pub line: usize,
    pub min: [f64; 3],
    pub max: [f64; 3],
    pub member: Option<usize>,
}

impl Picked {
    fn of<'a>(&self, state: &'a PrintState) -> Option<&'a OverhangLine> {
        state.overhang_list.as_ref()?.lines.get(self.line).filter(|l| l.min == self.min && l.max == self.max)
    }
}

/// What a press on `part` of list line `line` shows, `picked` pressed last: the row shows what
/// it shows (the whole line, until one of a cluster's patches is picked), the buttons step
/// through the cluster's patches, wrapping round.
fn overhang_pressed(state: &PrintState, picked: Option<Picked>, line: usize, part: RowPart) -> Option<Picked> {
    let l = state.overhang_list.as_ref()?.lines.get(line)?;
    let member = picked.filter(|p| p.line == line && p.of(state).is_some()).and_then(|p| p.member);
    let member = match part {
        RowPart::Row => member,
        RowPart::Prev => stepper::step(l.members.len(), member, false),
        RowPart::Next => stepper::step(l.members.len(), member, true),
    };
    Some(Picked { line, min: l.min, max: l.max, member })
}

/// The patch (or patches) a pick shows.
pub fn picked_line<'a>(state: &'a PrintState, picked: &Picked) -> Option<&'a OverhangLine> {
    let line = picked.of(state)?;
    match picked.member {
        Some(m) => line.members.get(m),
        None => Some(line),
    }
}

struct Row {
    name: String,
    right: String,
    tip: String,
    colour: [f32; 4],
    dim: bool,
    action: RowAction,
    /// A cluster's patch count: its row steps through them.
    members: usize,
    /// A cluster's name as drawn: its part alone, its count beside its buttons.
    short: Option<String>,
}

enum Entry {
    Head(String),
    Row(Row),
}

fn light_colour(light: MechLight) -> [f32; 4] {
    match light {
        MechLight::Waiting => [0.72, 0.70, 0.62, 1.0],
        MechLight::Green => [0.36, 0.72, 0.42, 1.0],
        MechLight::Amber => [0.93, 0.68, 0.22, 1.0],
        MechLight::Red => [0.88, 0.26, 0.22, 1.0],
    }
}

fn line_name(line: &OverhangLine) -> String {
    let part = &line.part;
    let mut name = match (line.unheld, line.id) {
        // What the part's own settings keep the trees out from under, listed apart.
        (Some(Unheld::Footprint), _) => return format!("{part} · inside the footprint, unsupported by your setting"),
        (Some(Unheld::NoTrees), _) => return format!("{part} · inside a no-trees box, unsupported by your setting"),
        (_, Some(id)) => format!("{part} p{id}"),
        (_, None) => format!("{part} · {} patches", line.patches),
    };
    if let Some(reason) = line.unheld {
        name.push_str(match reason {
            Unheld::GivenUp => " · given up",
            Unheld::Locked => " · locked in",
            Unheld::Blocked => " · blocked",
            Unheld::Footprint | Unheld::NoTrees => "",
        });
    }
    name
}

fn line_right(line: &OverhangLine) -> String {
    match line.held().filter(|_| !line.chosen()) {
        Some(held) => format!("{:.0}%", held * 100.0),
        None => format!("{:.0} mm²", line.area_mm2),
    }
}

/// Grey: no plan, or what the person's own settings keep the trees out from under.
const UNASKED: [f32; 4] = [0.55, 0.57, 0.6, 1.0];

fn line_colour(line: &OverhangLine) -> [f32; 4] {
    match line.held().filter(|_| !line.chosen()) {
        Some(held) if held >= 0.9 => light_colour(MechLight::Green),
        Some(held) if held >= 0.5 => light_colour(MechLight::Amber),
        Some(_) => light_colour(MechLight::Red),
        None => UNASKED,
    }
}

fn line_tip(line: &OverhangLine) -> String {
    let part = &line.part;
    let area = line.area_mm2;
    let patches = if line.patches == 1 { "1 patch".to_string() } else { format!("{} patches", line.patches) };
    match line.unheld {
        Some(Unheld::Footprint) => return format!("{part}: {area:.0} mm² of overhang inside its footprint, layer by layer as Printability counts it, in {patches}. Only outside the part's footprint, in its support settings, keeps the trees out from under the part, and a branch reaches in only where it gets straight back out: nothing holds this, by your setting, and the share held leaves it out. Turn the setting off to hold it. Click to frame it."),
        Some(Unheld::NoTrees) => return format!("{part}: {area:.0} mm² of overhang over a no-trees box, layer by layer as Printability counts it, in {patches}. No trees here keeps the trees out of the box, and a branch reaches in over it only from outside: nothing holds this, by your setting, and the share held leaves it out. Move or shrink the box to hold it. Click to frame it."),
        _ => {}
    }
    let mut tip = match (line.held(), line.left_mm2) {
        (Some(held), Some(left)) => {
            let held = held * 100.0;
            match line.id {
                Some(id) => {
                    let over = if line.over_part > 0 { "part" } else { "bed" };
                    format!("{part} p{id}: {area:.0} mm² over the {over}, {held:.0}% held ({left:.0} mm² left).")
                }
                None => format!("{part}: {} patches, {area:.0} mm², {held:.0}% held ({left:.0} mm² left), {} over the part.", line.patches, line.over_part),
            }
        }
        _ => format!("{part}: {area:.0} mm². The plate plans no supports: nothing holds it."),
    };
    if let Some(reason) = line.unheld {
        tip.push_str(match reason {
            Unheld::GivenUp => " No support can reach it.",
            Unheld::Locked => " A support there would be locked in the part: it could not come out. Allow supports that can't be removed, in the part's support settings, puts them there.",
            Unheld::Blocked => " Block supports paint keeps supports off it.",
            Unheld::Footprint | Unheld::NoTrees => "",
        });
    }
    tip.push_str(" Click to frame it. Enforce supports and Block supports paint add and take away supports where you paint.");
    tip
}

fn line_box(line: &OverhangLine) -> crate::Aabb {
    crate::Aabb {
        min: vec3(line.min[0] as f32 * 0.001 - 0.003, line.min[1] as f32 * 0.001 - 0.003, line.min[2] as f32 * 0.001 - 0.003),
        max: vec3(line.max[0] as f32 * 0.001 + 0.003, line.max[1] as f32 * 0.001 + 0.003, line.max[2] as f32 * 0.001 + 0.003),
    }
}

fn entries(state: &PrintState) -> Vec<Entry> {
    let Some(list) = &state.overhang_list else { return Vec::new() };
    let mut entries = Vec::new();
    if list.patches > 0 {
        // Held of what the supports are asked to hold: the list's area leaves out what the parts'
        // own settings keep the trees out from under, which has lines of its own.
        let head = match list.left_mm2 {
            Some(_) if list.area_mm2 <= 0.0 && list.lines.iter().any(OverhangLine::chosen) => "OVERHANGS · all kept out by your settings".into(),
            Some(left) => {
                let held = if list.area_mm2 > 0.0 { (1.0 - left / list.area_mm2).max(0.0) } else { 1.0 };
                format!("OVERHANGS · {:.0}% held", held * 100.0)
            }
            None => "OVERHANGS · no supports planned".into(),
        };
        entries.push(Entry::Head(head));
        for (i, line) in list.lines.iter().enumerate() {
            entries.push(Entry::Row(Row {
                name: line_name(line), right: line_right(line), tip: line_tip(line), colour: line_colour(line),
                dim: false, action: RowAction::Frame(i), members: line.members.len(),
                short: (line.members.len() > 1).then(|| line.part.clone()),
            }));
        }
    }
    if list.left_mm2.is_some() {
        match &state.support_pieces {
            None => {
                entries.push(Entry::Head("SUPPORT REMOVAL".into()));
                entries.push(Entry::Row(Row {
                    name: "Not checked".into(), right: "Check".into(),
                    tip: "Mechanics, Supports, cuts each support piece free and checks it has a way out and room for a tool. Click to open it.".into(),
                    colour: light_colour(MechLight::Waiting), dim: false, action: RowAction::Check, members: 0, short: None,
                }));
            }
            Some(pieces) => {
                entries.push(Entry::Head(format!("SUPPORT REMOVAL · {} pieces", pieces.len())));
                let mut shown = 0;
                let mut more = 0;
                for light in [MechLight::Red, MechLight::Amber] {
                    for piece in pieces.iter().filter(|p| p.light == light) {
                        if shown == 6 {
                            more += 1;
                            continue;
                        }
                        shown += 1;
                        entries.push(Entry::Row(Row {
                            name: piece.label.clone(), right: String::new(),
                            tip: format!("{}: {}. Click to see it in Mechanics.", piece.label, piece.words),
                            colour: light_colour(light), dim: false, action: RowAction::Check, members: 0, short: None,
                        }));
                    }
                }
                let green = pieces.iter().filter(|p| p.light == MechLight::Green).count();
                if green > 0 {
                    entries.push(Entry::Row(Row {
                        name: format!("{green} come out"), right: String::new(),
                        tip: format!("{green} come out. Click to see them in Mechanics."),
                        colour: light_colour(MechLight::Green), dim: false, action: RowAction::Check, members: 0, short: None,
                    }));
                }
                if more > 0 {
                    entries.push(Entry::Row(Row {
                        name: format!("{more} more"), right: String::new(),
                        tip: format!("{more} more pieces. Click to see them in Mechanics."),
                        colour: [0.0; 4], dim: true, action: RowAction::Check, members: 0, short: None,
                    }));
                }
            }
        }
    }
    entries
}

const ROW_H: f64 = 18.0;

impl FabSupportList {
    fn line_at(&self, abs: DVec2) -> Option<(usize, RowPart)> {
        let n = self.lines.iter().position(|l| l.rect.contains(abs))?;
        Some((n, stepper::part_at(self.lines[n].steps, abs)))
    }

    fn press(&mut self, cx: &mut Cx, scope: &mut Scope, n: usize, part: RowPart) {
        // the arrow keys step on from here
        cx.set_key_focus(self.area);
        let Some(action) = self.lines.get(n).map(|l| l.action) else { return };
        match action {
            RowAction::Frame(line) => {
                let Some(state) = scope.data.get::<AppState>() else { return };
                let Some(picked) = overhang_pressed(&state.print, state.print.picked_overhang, line, part) else { return };
                let Some(shown) = picked_line(&state.print, &picked) else { return };
                cx.action(ShellAction::FrameBox(line_box(shown)));
                cx.action(PrintAction::PickOverhang(picked));
            }
            RowAction::Check => cx.action(PrintAction::OpenSupportCheck),
        }
    }

    fn hover_tip(&self, cx: &mut Cx, now: Option<(usize, RowPart)>) {
        match now {
            Some((n, part)) => {
                let l = &self.lines[n];
                let (rect, tip) = match (part, l.steps) {
                    (RowPart::Prev, Some((prev, _))) => (prev, format!("The previous patch of {}. The arrow keys step too.", l.name)),
                    (RowPart::Next, Some((_, next))) => (next, format!("The next patch of {}. The arrow keys step too.", l.name)),
                    _ => (l.rect, l.tip.clone()),
                };
                let mut request = TipRequest::plain(tip, rect);
                request.place = TipPlace::Right;
                request.wrap_width = 280.0;
                cx.widget_action(self.uid, TipAction::HoverInWith(request));
            }
            None => cx.widget_action(self.uid, TipAction::HoverOut),
        }
    }
}

impl Widget for FabSupportList {
    fn semantics(&self, _cx: &Cx, node: &mut a11y_tree::Node) {
        node.role = a11y_tree::Role::StaticText;
        makepad_platform::a11y::set_string(&mut node.name, "Supports");
        makepad_platform::a11y::set_string(&mut node.value, &self.text);
    }

    fn semantics_parts(&self, _cx: &Cx, out: &mut Vec<(u64, Option<u64>, a11y_tree::Node)>) {
        for (n, l) in self.lines.iter().enumerate() {
            let key = n as u64 * 3;
            let mut parts = vec![(key + 1, l.rect, l.name.clone())];
            if let Some((prev, next)) = l.steps {
                parts.push((key + 2, prev, format!("Previous patch of {}", l.name)));
                parts.push((key + 3, next, format!("Next patch of {}", l.name)));
            }
            for (key, rect, label) in parts {
                let mut node = a11y_tree::Node {
                    role: a11y_tree::Role::Button,
                    bounds: makepad_platform::a11y::semantic_rect(rect),
                    actions: a11y_tree::Actions::PRESS,
                    ..Default::default()
                };
                makepad_platform::a11y::set_string(&mut node.name, &label);
                out.push((key, None, node));
            }
        }
    }

    fn handle_action(&mut self, cx: &mut Cx, request: &a11y_tree::ActionRequest, scope: &mut Scope) -> a11y_tree::ActionResult {
        use a11y_tree::{ActionResult, Actions};
        if request.action != Actions::PRESS {
            return ActionResult::Unhandled;
        }
        let Some(key) = cx.a11y.action_part_key(request.node) else { return ActionResult::Unhandled };
        let Some(key) = key.checked_sub(1) else { return ActionResult::Unsupported };
        let (n, part) = (key as usize / 3, [RowPart::Row, RowPart::Prev, RowPart::Next][key as usize % 3]);
        if n >= self.lines.len() {
            return ActionResult::Unsupported;
        }
        self.press(cx, scope, n, part);
        ActionResult::Handled
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        match event.hits(cx, self.area) {
            Hit::FingerDown(_) => cx.set_key_focus(self.area),
            Hit::FingerUp(fe) if !fe.cancelled => {
                if let Some((n, part)) = self.line_at(fe.abs) {
                    self.press(cx, scope, n, part);
                }
            }
            Hit::KeyDown(ke) => {
                // the arrow keys step the cluster pressed last
                let picked = scope.data.get::<AppState>().and_then(|s| s.print.picked_overhang);
                let line = picked.and_then(|p| self.lines.iter().position(|l| l.action == RowAction::Frame(p.line) && l.steps.is_some()));
                if let (Some(forward), Some(n)) = (stepper::key_step(ke.key_code), line) {
                    self.press(cx, scope, n, if forward { RowPart::Next } else { RowPart::Prev });
                }
            }
            Hit::FingerHoverIn(fh) | Hit::FingerHoverOver(fh) => {
                let now = self.line_at(fh.abs);
                if now != self.hover {
                    self.hover = now;
                    self.hover_tip(cx, now);
                    self.area.redraw(cx);
                }
                cx.set_cursor(if now.is_some() { MouseCursor::Hand } else { MouseCursor::Default });
            }
            Hit::FingerHoverOut(_) => {
                cx.widget_action(self.uid, TipAction::HoverOut);
                self.hover = None;
                self.area.redraw(cx);
            }
            _ => {}
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let Some(state) = scope.data.get::<AppState>() else { return DrawStep::done() };
        let entries = entries(&state.print);
        let rect = cx.walk_turtle(Walk { height: Size::Fixed(entries.len() as f64 * ROW_H), ..walk });
        self.lines.clear();
        self.text.clear();
        // a pick of a line the list no longer has shows on no row
        let picked = state.print.picked_overhang.filter(|p| p.of(&state.print).is_some());
        if entries.is_empty() {
            self.area = Area::Empty;
            return DrawStep::done();
        }
        self.draw_hit.fill = vec4(0.0, 0.0, 0.0, 0.0);
        self.draw_hit.ring = vec4(0.0, 0.0, 0.0, 0.0);
        self.draw_hit.draw_abs(cx, rect);
        self.area = self.draw_hit.area();
        let right = rect.pos.x + rect.size.x;
        let mut y = rect.pos.y;
        for entry in entries {
            match entry {
                Entry::Head(head) => {
                    self.draw_head.draw_abs(cx, dvec2(rect.pos.x, y + 3.0), &head);
                    self.text.push_str(&head);
                    self.text.push('\n');
                }
                Entry::Row(row) => {
                    let n = self.lines.len();
                    let line = Rect { pos: dvec2(rect.pos.x, y), size: dvec2(rect.size.x, ROW_H) };
                    let picked = picked.filter(|p| row.action == RowAction::Frame(p.line));
                    let hover = self.hover.filter(|(h, _)| *h == n).map(|(_, part)| part);
                    if picked.is_some() || hover == Some(RowPart::Row) {
                        self.draw_chip.fill = vec4(1.0, 1.0, 1.0, if picked.is_some() { 0.12 } else { 0.07 });
                        self.draw_chip.ring = vec4(0.0, 0.0, 0.0, 0.0);
                        self.draw_chip.draw_abs(cx, line);
                    }
                    if !row.dim {
                        let c = row.colour;
                        self.draw_chip.spool = 0.0;
                        self.draw_chip.fill = vec4(c[0], c[1], c[2], c[3]);
                        self.draw_chip.ring = vec4(0.0, 0.0, 0.0, 0.4);
                        self.draw_chip.draw_abs(cx, Rect { pos: dvec2(rect.pos.x + 2.0, y + 4.0), size: dvec2(10.0, 10.0) });
                    }
                    // A cluster steps through its patches with the buttons at its right.
                    let at = picked.and_then(|p| p.member);
                    let mut end = right;
                    let steps = (row.members > 1).then(|| {
                        let count = stepper::count_text(row.members, at);
                        let cw = self.draw_dim.prepare_single_line_run(cx, &count).map(|r| r.width_in_lpxs as f64).unwrap_or(10.0);
                        let (prev, next) = stepper::buttons(line, cw);
                        for (button, part) in [(prev, RowPart::Prev), (next, RowPart::Next)] {
                            if hover == Some(part) {
                                self.draw_chip.fill = vec4(1.0, 1.0, 1.0, 0.14);
                                self.draw_chip.ring = vec4(0.0, 0.0, 0.0, 0.0);
                                self.draw_chip.draw_abs(cx, button);
                            }
                        }
                        self.draw_prev.draw_abs(cx, stepper::icon(prev));
                        self.draw_dim.draw_abs(cx, dvec2(stepper::count_x(prev), y + 3.0), &count);
                        self.draw_next.draw_abs(cx, stepper::icon(next));
                        end = prev.pos.x - 4.0;
                        (prev, next)
                    });
                    let tw = self.draw_dim.prepare_single_line_run(cx, &row.right).map(|r| r.width_in_lpxs as f64).unwrap_or(0.0);
                    self.draw_dim.draw_abs(cx, dvec2(end - tw - 2.0, y + 3.0), &row.right);
                    let name_x = rect.pos.x + 18.0;
                    let text = if row.dim { &self.draw_dim } else { &self.draw_text };
                    let name = ellipsis(cx, text, row.short.as_deref().unwrap_or(&row.name), (end - tw - 8.0 - name_x).max(0.0));
                    let text = if row.dim { &mut self.draw_dim } else { &mut self.draw_text };
                    text.draw_abs(cx, dvec2(name_x, y + 3.0), &name);
                    let shows = match (at, steps) {
                        (Some(a), Some(_)) => format!(", patch {} of {}", a + 1, row.members),
                        _ => String::new(),
                    };
                    self.text.push_str(&format!("{} {}{shows}\n", row.name, row.right));
                    // the tip says what the row shows: the picked patch of a cluster
                    let tip = match picked.and_then(|p| picked_line(&state.print, &p)).filter(|_| at.is_some()) {
                        Some(member) => line_tip(member),
                        None => row.tip,
                    };
                    self.lines.push(ListLine { rect: line, steps, action: row.action, tip, name: row.name });
                }
            }
            y += ROW_H;
        }
        DrawStep::done()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn patch(id: usize, z: f64) -> OverhangLine {
        OverhangLine {
            part: "Sponge".into(), id: Some(id), patches: 1, area_mm2: 10.0, left_mm2: None, over_part: 0, unheld: None,
            min: [id as f64, 0.0, z], max: [id as f64 + 2.0, 2.0, z + 0.4], members: Vec::new(),
        }
    }

    /// A cluster's row steps through each of its patches and wraps round, each shown at the layer
    /// it starts on; its row shows what it shows, another line starts from none.
    #[test]
    fn a_cluster_steps_through_its_patches_and_wraps() {
        let members: Vec<_> = (1..=3).map(|i| patch(i, 0.2 * i as f64)).collect();
        let cluster = OverhangLine { id: None, patches: 3, min: [1.0, 0.0, 0.2], max: [5.0, 2.0, 1.0], members, ..patch(0, 0.2) };
        let mut state = PrintState::default();
        state.set_preview(crate::print::follow::tests::preview_of(8));
        state.overhang_list = Some(fab_printability::OverhangList { patches: 4, area_mm2: 40.0, left_mm2: None, lines: vec![cluster, patch(4, 1.0)] });
        let shows = |p: Option<Picked>| p.and_then(|p| p.member);
        let first = overhang_pressed(&state, None, 0, RowPart::Row);
        assert_eq!(shows(first), None, "the row alone frames the whole cluster");
        let mut at = first;
        let mut seen = Vec::new();
        for _ in 0..3 {
            at = overhang_pressed(&state, at, 0, RowPart::Next);
            assert!(crate::print::apply(&mut state, &PrintAction::PickOverhang(at.unwrap())));
            assert_eq!(state.picked_overhang, at);
            let patch = picked_line(&state, &at.unwrap()).unwrap();
            assert_eq!(state.play.layer_hi, patch.id.unwrap() as u32 - 1, "the layers shown up to the one {patch:?} starts on");
            assert!(state.issue_spot.is_some());
            seen.push(patch.id.unwrap());
        }
        assert_eq!(seen, [1, 2, 3]);
        assert_eq!(shows(overhang_pressed(&state, at, 0, RowPart::Next)), Some(0), "past the last it wraps to the first");
        assert_eq!(shows(overhang_pressed(&state, at, 0, RowPart::Row)), Some(2), "the row shows the one it shows");
        assert_eq!(shows(overhang_pressed(&state, first, 0, RowPart::Prev)), Some(2), "back from none is the last");
        assert_eq!(shows(overhang_pressed(&state, at, 1, RowPart::Row)), None, "another line starts from none");
        // the whole cluster again: framed in view, the layers stay; it is what shows now, not an
        // issue picked before it
        state.picked_issue = Some(0);
        assert!(crate::print::apply(&mut state, &PrintAction::PickOverhang(first.unwrap())));
        assert_eq!(state.play.layer_hi, 2);
        assert_eq!((state.picked_overhang, state.picked_issue), (first, None));
        // a new list with another line in its place is not the one picked
        state.overhang_list.as_mut().unwrap().lines[0].min[0] = 9.0;
        assert_eq!(shows(overhang_pressed(&state, at, 0, RowPart::Next)), Some(0));
        assert!(!crate::print::apply(&mut state, &PrintAction::PickOverhang(at.unwrap())), "nothing to show");
    }

    /// What the part's own settings keep the trees out from under is a grey line of its own, by
    /// its area, after the ranked ones, and the header's share held leaves it out.
    #[test]
    fn kept_out_overhang_is_listed_apart_and_not_held_against() {
        let held = OverhangLine {
            part: "Lattice cube".into(), id: Some(1), patches: 1, area_mm2: 80.0, left_mm2: Some(8.0), over_part: 0, unheld: None,
            min: [0.0; 3], max: [1.0; 3], members: Vec::new(),
        };
        let footprint = OverhangLine { id: None, patches: 9, area_mm2: 1000.0, left_mm2: Some(1000.0), unheld: Some(Unheld::Footprint), ..held.clone() };
        let no_trees = OverhangLine { area_mm2: 12.0, left_mm2: Some(12.0), unheld: Some(Unheld::NoTrees), ..footprint.clone() };
        let mut state = PrintState::default();
        state.overhang_list = Some(fab_printability::OverhangList { patches: 11, area_mm2: 80.0, left_mm2: Some(8.0), lines: vec![held, footprint.clone(), no_trees] });
        let shown = entries(&state);
        let Entry::Head(head) = &shown[0] else { panic!("the header first") };
        assert_eq!(head, "OVERHANGS · 90% held", "held of what the supports are asked to hold");
        let rows: Vec<&Row> = shown.iter().filter_map(|e| if let Entry::Row(r) = e { Some(r) } else { None }).collect();
        assert_eq!((rows[1].name.as_str(), rows[1].right.as_str(), rows[1].colour), ("Lattice cube · inside the footprint, unsupported by your setting", "1000 mm²", UNASKED));
        assert_eq!((rows[2].name.as_str(), rows[2].right.as_str(), rows[2].colour), ("Lattice cube · inside a no-trees box, unsupported by your setting", "12 mm²", UNASKED));
        assert!(rows[1].tip.contains("nothing holds this, by your setting") && rows[2].tip.contains("No trees here"), "{} / {}", rows[1].tip, rows[2].tip);
        // All of it the part's own choice: no share held to tell.
        state.overhang_list = Some(fab_printability::OverhangList { patches: 9, area_mm2: 0.0, left_mm2: Some(0.0), lines: vec![footprint] });
        let Entry::Head(head) = &entries(&state)[0] else { panic!("the header first") };
        assert_eq!(head, "OVERHANGS · all kept out by your settings");
    }

    #[test]
    fn support_list_text_and_colours() {
        let mut line = OverhangLine {
            part: "Bracket".into(), id: Some(3), patches: 1, area_mm2: 100.0, left_mm2: Some(10.0),
            over_part: 0, unheld: None, min: [10.0, 20.0, 30.0], max: [40.0, 50.0, 60.0], members: Vec::new(),
        };
        let instruction = " Click to frame it. Enforce supports and Block supports paint add and take away supports where you paint.";
        assert_eq!(line_name(&line), "Bracket p3", "single patch name");
        assert_eq!(line_right(&line), "90%", "held share");
        assert_eq!(line_colour(&line), [0.36, 0.72, 0.42, 1.0], "green threshold");
        assert_eq!(line_tip(&line), format!("Bracket p3: 100 mm² over the bed, 90% held (10 mm² left).{instruction}"));
        line.over_part = 1;
        assert_eq!(line_tip(&line), format!("Bracket p3: 100 mm² over the part, 90% held (10 mm² left).{instruction}"));
        let bounds = line_box(&line);
        for (actual, expected) in [(bounds.min.x, 0.007), (bounds.min.y, 0.017), (bounds.min.z, 0.027), (bounds.max.x, 0.043), (bounds.max.y, 0.053), (bounds.max.z, 0.063)] {
            assert!((actual - expected).abs() < 1e-6, "frame uses metres and a 3 mm margin");
        }
        line.id = None;
        line.patches = 4;
        line.left_mm2 = Some(50.0);
        assert_eq!(line_name(&line), "Bracket · 4 patches", "cluster name");
        assert_eq!(line_right(&line), "50%");
        assert_eq!(line_colour(&line), [0.93, 0.68, 0.22, 1.0], "amber threshold");
        assert_eq!(line_tip(&line), format!("Bracket: 4 patches, 100 mm², 50% held (50 mm² left), 1 over the part.{instruction}"));
        line.left_mm2 = None;
        assert_eq!(line_right(&line), "100 mm²", "no plan shows area");
        assert_eq!(line_colour(&line), UNASKED, "no plan is grey");
        assert_eq!(line_tip(&line), format!("Bracket: 100 mm². The plate plans no supports: nothing holds it.{instruction}"));
        line.id = Some(3);
        line.patches = 1;
        line.left_mm2 = Some(100.0);
        assert_eq!(line_colour(&line), [0.88, 0.26, 0.22, 1.0], "unheld is red");
        for (reason, name, tip) in [
            (Unheld::GivenUp, "given up", "No support can reach it."),
            (Unheld::Locked, "locked in", "A support there would be locked in the part: it could not come out. Allow supports that can't be removed, in the part's support settings, puts them there."),
            (Unheld::Blocked, "blocked", "Block supports paint keeps supports off it."),
        ] {
            line.unheld = Some(reason);
            assert_eq!(line_name(&line), format!("Bracket p3 · {name}"));
            assert_eq!(line_tip(&line), format!("Bracket p3: 100 mm² over the part, 0% held (100 mm² left). {tip}{instruction}"));
        }
    }
}
