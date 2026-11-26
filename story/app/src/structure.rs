//! Shared Structure rows and interaction for document and scene inspectors.
use makepad_motion::structure::NodeKind;
use makepad_value_pickers::panel::control::{fit_text, DrawPanelShape, R_CONTROL};
use makepad_widgets::*;

script_mod! {
    use mod.prelude.widgets_internal.*
    use mod.widgets.*
    mod.widgets.StructureTreeBase = #(StructureTree::register_widget(vm))
    /** The scene's structure: its nodes as rows (indented, folding), and
     * the selected node's unwritten parameters as chips under them. */
    mod.widgets.StructureTree = set_type_default() do mod.widgets.StructureTreeBase{
        width: Fill height: Fit
        draw_text +: {text_style: theme.font_regular{font_size: 8.5}}
        draw_small +: {text_style: theme.font_regular{font_size: 7.5}}
        draw_bold +: {text_style: theme.font_bold{font_size: 8}}
    }

}

/// One row of the tree.
#[derive(Clone, Debug, PartialEq)]
pub struct TreeRow {
    pub path: String,
    pub depth: usize,
    pub label: String,
    /// Dim text after the label (its type, `×` a generator, its effects).
    pub detail: String,
    pub kind: NodeKind,
    pub on: bool,
    /// Folding: `None` no children, else open or not.
    pub open: Option<bool>,
}

#[derive(Clone, Debug, Default)]
pub enum StructureAction {
    #[default]
    None,
    Select(String),
    Fold(String),
    Hover(Option<String>),
    /// An unwritten parameter written at its default.
    AddParam { node: String, name: String },
}

#[derive(Clone, Debug)]
pub struct StructureContextAction {
    pub path: String,
    pub abs: DVec2,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Spot {
    Row(usize),
    Fold(usize),
    Param(usize),
    /// The unwritten parameters, folded behind one chip.
    More,
}

#[derive(Script, ScriptHook, Widget)]
pub struct StructureTree {
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
    draw_text: DrawText,
    #[live]
    draw_small: DrawText,
    #[live]
    draw_bold: DrawText,
    #[live]
    context_menu: bool,
    #[rust]
    rows: Vec<TreeRow>,
    /// Parameters the definition has that are not written: (node, name).
    #[rust]
    unwritten: Vec<(String, String)>,
    #[rust]
    more_open: bool,
    #[rust]
    selected: Option<String>,
    #[rust]
    spots: Vec<(Rect, Spot)>,
    #[rust]
    hot: Option<Spot>,
}

pub const ROW_HEIGHT: f64 = 22.0;
const INDENT: f64 = 14.0;

fn color(value: u32, alpha: f32) -> Vec4f {
    let mut c = Vec4f::from_u32(value);
    c.w *= alpha;
    c
}

impl StructureTree {
    pub fn set_model(&mut self, cx: &mut Cx, rows: Vec<TreeRow>, unwritten: Vec<(String, String)>, selected: Option<String>) {
        if self.rows == rows && self.unwritten == unwritten && self.selected == selected {
            return;
        }
        self.rows = rows;
        if self.unwritten != unwritten {
            self.more_open = false;
        }
        self.unwritten = unwritten;
        self.selected = selected;
        self.draw_bg.redraw(cx);
    }

    fn at(&self, abs: DVec2) -> Option<Spot> {
        // The smallest spot under the pointer (a switch over its row).
        self.spots.iter().filter(|(r, _)| r.contains(abs)).min_by(|a, b| (a.0.size.x * a.0.size.y).total_cmp(&(b.0.size.x * b.0.size.y))).map(|(_, s)| *s)
    }

}

impl Widget for StructureTree {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, _scope: &mut Scope) {
        let uid = self.widget_uid();
        match event.hits(cx, self.draw_bg.area()) {
            Hit::FingerHoverIn(fe) | Hit::FingerHoverOver(fe) => {
                let hot = self.at(fe.abs);
                cx.set_cursor(match hot {
                    Some(_) => MouseCursor::Hand,
                    None => MouseCursor::Default,
                });
                if hot != self.hot {
                    self.hot = hot;
                    let path = match hot {
                        Some(Spot::Row(i)) | Some(Spot::Fold(i)) => self.rows.get(i).map(|r| r.path.clone()),
                        _ => None,
                    };
                    cx.widget_action(uid, StructureAction::Hover(path));
                    self.draw_bg.redraw(cx);
                }
            }
            Hit::FingerHoverOut(_) => {
                self.hot = None;
                cx.widget_action(uid, StructureAction::Hover(None));
                self.draw_bg.redraw(cx);
            }
            Hit::FingerDown(fe) if self.context_menu && fe.mouse_button().is_some_and(|button| button.is_secondary()) => {
                if let Some(Spot::Row(i) | Spot::Fold(i)) = self.at(fe.abs) {
                    if let Some(row) = self.rows.get(i) {
                        cx.widget_action(uid, StructureContextAction { path: row.path.clone(), abs: fe.abs });
                    }
                }
            }
            Hit::FingerUp(fe) if fe.is_primary_hit() => {
                if !fe.is_over {
                    return;
                }
                match self.at(fe.abs) {
                    Some(Spot::Row(i)) => {
                        if let Some(row) = self.rows.get(i) {
                            cx.widget_action(uid, StructureAction::Select(row.path.clone()));
                        }
                    }
                    Some(Spot::Fold(i)) => {
                        if let Some(row) = self.rows.get(i) {
                            cx.widget_action(uid, StructureAction::Fold(row.path.clone()));
                        }
                    }
                    Some(Spot::More) => {
                        self.more_open = !self.more_open;
                        self.draw_bg.redraw(cx);
                    }
                    Some(Spot::Param(j)) => {
                        if let Some((node, name)) = self.unwritten.get(j) {
                            cx.widget_action(uid, StructureAction::AddParam { node: node.clone(), name: name.clone() });
                        }
                    }
                    None => {}
                }
            }
            _ => {}
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, _scope: &mut Scope, walk: Walk) -> DrawStep {
        let p = makepad_look::palette::get(cx);
        // Measured first: the rows, then each stack (a title, its entries,
        // its catalogue when open), then the unwritten parameters' chips.
        let width = cx.turtle().inner_width().max(120.0);
        let mut height = 4.0 + self.rows.len() as f64 * ROW_HEIGHT + 6.0;
        let chip_rows = |s: &Self, cx: &mut Cx2d, labels: &[String]| -> Vec<(f64, f64, f64)> {
            let (mut x, mut y, mut out) = (0.0, 0.0, Vec::new());
            for l in labels {
                let w = makepad_widgets::badge::advance(&s.draw_small, cx, l) + 14.0;
                if x + w > width - 16.0 && x > 0.0 {
                    x = 0.0;
                    y += 22.0;
                }
                out.push((x, y, w));
                x += w + 4.0;
            }
            out
        };
        let unwritten_labels: Vec<String> = self.unwritten.iter().map(|(_, n)| if n.starts_with('↑') { n.clone() } else { format!("+ {n}") }).collect();
        let unwritten_layout = chip_rows(self, cx, &unwritten_labels);
        if !self.unwritten.is_empty() {
            height += 22.0 + if self.more_open { unwritten_layout.last().map_or(0.0, |l| l.1 + 22.0) } else { 0.0 } + 4.0;
        }
        let rect = cx.walk_turtle(Walk { height: Size::Fixed(height), ..walk });
        self.draw_bg.color = color(0, 0.0);
        self.draw_bg.draw_abs(cx, rect);
        self.spots.clear();
        let x0 = rect.pos.x + 6.0;
        let w = rect.size.x - 12.0;
        let mut y = rect.pos.y + 4.0;
        // The tree.
        for (i, row) in self.rows.iter().enumerate() {
            let r = Rect { pos: dvec2(x0, y), size: dvec2(w, ROW_HEIGHT - 2.0) };
            let selected = self.selected.as_deref() == Some(row.path.as_str());
            let hot = matches!(self.hot, Some(Spot::Row(k)) | Some(Spot::Fold(k)) if k == i);
            if selected {
                self.draw_shape.fill_round(cx, r, color(p.accent_wash, 1.0), R_CONTROL);
            } else if hot {
                self.draw_shape.fill_round(cx, r, color(p.control_hover, 1.0), R_CONTROL);
            }
            let x = r.pos.x + 4.0 + row.depth as f64 * INDENT;
            if let Some(open) = row.open {
                let c = Rect { pos: dvec2(x, r.pos.y + 5.0), size: dvec2(10.0, 10.0) };
                self.draw_shape.chevron(cx, c, color(p.muted, 1.0), open);
                self.spots.push((Rect { pos: dvec2(x - 3.0, r.pos.y), size: dvec2(16.0, r.size.y) }, Spot::Fold(i)));
            }
            let tx = x + 14.0;
            let ink = if !row.on { p.dim } else if row.kind == NodeKind::Code { p.muted } else if selected { p.text_hi } else { p.text };
            let draw = if row.depth == 0 { &mut self.draw_bold } else { &mut self.draw_text };
            draw.color = color(ink, 1.0);
            let detail_w = makepad_widgets::badge::advance(&self.draw_small, cx, &row.detail).min(w * 0.45);
            let draw = if row.depth == 0 { &self.draw_bold } else { &self.draw_text };
            let (label, _) = fit_text(draw, cx, &row.label, (r.pos.x + r.size.x - tx - detail_w - 10.0).max(20.0));
            let draw = if row.depth == 0 { &mut self.draw_bold } else { &mut self.draw_text };
            draw.draw_abs(cx, dvec2(tx, r.pos.y + 4.0), &label);
            if !row.detail.is_empty() {
                let (detail, _) = fit_text(&self.draw_small, cx, &row.detail, detail_w);
                self.draw_small.color = color(p.muted, 1.0);
                self.draw_small.draw_abs(cx, dvec2(r.pos.x + r.size.x - detail_w - 4.0, r.pos.y + 5.0), &detail);
            }
            self.spots.push((r, Spot::Row(i)));
            y += ROW_HEIGHT;
        }
        y += 6.0;
        // Parameters the definition has that are not written.
        if !self.unwritten.is_empty() {
            // One quiet chip; open, a chip per parameter (a click writes its default).
            let promotes = self.unwritten.iter().any(|(_, n)| n.starts_with('↑'));
            let label = format!("{} {} {}", if self.more_open { "−" } else { "+" }, self.unwritten.len(), if promotes { "values to make parameters" } else { "more" });
            let w = makepad_widgets::badge::advance(&self.draw_small, cx, &label) + 14.0;
            let r = Rect { pos: dvec2(x0 + 4.0, y + 1.0), size: dvec2(w, 18.0) };
            let hot = self.hot == Some(Spot::More);
            self.draw_shape.fill_round(cx, r, color(if hot { p.control_hover } else { p.raised }, 1.0), R_CONTROL);
            self.draw_small.color = color(p.muted, 1.0);
            self.draw_small.draw_abs(cx, dvec2(r.pos.x + 7.0, r.pos.y + 3.5), &label);
            self.spots.push((r, Spot::More));
            y += 22.0;
            if !self.more_open {
                return DrawStep::done();
            }
            for (j, (label, (cx_, cy, cw))) in unwritten_labels.iter().zip(&unwritten_layout).enumerate() {
                let r = Rect { pos: dvec2(x0 + 8.0 + cx_, y + cy), size: dvec2(*cw, 18.0) };
                let hot = self.hot == Some(Spot::Param(j));
                self.draw_shape.edged_round(cx, r, color(if hot { p.control_hover } else { 0 }, if hot { 1.0 } else { 0.0 }), color(p.line, 1.0), 1.0, R_CONTROL);
                self.draw_small.color = color(p.text_soft, 1.0);
                self.draw_small.draw_abs(cx, dvec2(r.pos.x + 7.0, r.pos.y + 3.5), label);
                self.spots.push((r, Spot::Param(j)));
            }
        }
        DrawStep::done()
    }
}

