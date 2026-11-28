//! Virtual row templates and bindings. Only the mounted PortalList items own widgets.
use super::{DropIndicator, TreeAction, TreeLine};
use makepad_widgets::*;

pub(in crate::tweak) fn tree_list_value(vm: &mut ScriptVm, row: ScriptValue) -> ScriptValue {
    script_eval!(vm, {
        use mod.prelude.workspace.*
        use mod.widgets.*
        let fab = mod.fab
        let TweakRow = #(row)
        let TreeButton = ButtonFlat{
            min_height: 0 height: 22 padding: 0 margin: 0 spacing: 0
            draw_bg +: { pixel: fn() { return vec4(0.0) } }
            draw_text +: {
                color: fab.color_text
                dim: instance(0.0)
                selected: instance(0.0)
                text_style +: {font_size: 8.5}
                get_color: fn() {
                    return self.color.mix(fab.color_text_dim, self.dim).mix(fab.color_text_active, self.selected)
                }
            }
        }
        let Dot = RoundedView{visible: false width: 6 height: 6 draw_bg +: {border_radius: 3.0}}
        let Indent = View{width: 0 height: 1}
        PortalList{width: Fill height: Fill reuse_items: true
            TreeRow := TweakRow{width: Fill height: 22
                tree_face := View{width: Fill height: Fill flow: Right align: Align{y: 0.5}
                    show_bg: true
                    draw_bg +: {
                        even: instance(0.0)
                        selected: instance(0.0)
                        drop_mode: instance(0.0)
                        drop_line: uniform(theme.color_primary)
                        drop_veil: uniform(theme.color_drag_target_preview)
                        pixel: fn() {
                            let base = fab.color_panel_sub.mix(fab.color_area, self.even).mix(fab.color_selection_bg, self.selected)
                            let p = self.pos * self.rect_size
                            let edge = self.rect_size - p
                            if self.drop_mode == 1.0 && p.y < 2.0 { return self.drop_line }
                            if self.drop_mode == 2.0 && edge.y <= 2.0 { return self.drop_line }
                            if self.drop_mode == 3.0 {
                                if min(min(p.x, edge.x), min(p.y, edge.y)) < 1.5 { return self.drop_line }
                                return vec4(mix(base.rgb, self.drop_veil.rgb, min(self.drop_veil.a, 0.25)), base.a)
                            }
                            return base
                        }
                    }
                    indent := Indent{}
                    twisty := TreeButton{width: 16 text: "▸"}
                    twisty_gap := View{width: 16 height: 1}
                    select_btn := TreeButton{width: Fill align: Align{x: 0 y: 0.5}}
                    seat_dots := View{width: Fit height: Fill spacing: 2 align: Align{y: 0.5} padding: Inset{right: 4}
                        dot_a := Dot{}
                        dot_b := Dot{}
                        dot_c := Dot{}
                        dot_d := Dot{}
                        dot_overflow := Label{visible: false width: Fit draw_text +: {color: fab.color_text_dim text_style +: {font_size: 7.5}}}
                    }
                }
            }
            TreePagerRow := TweakRow{width: Fill height: 22 flow: Right align: Align{y: 0.5}
                indent := Indent{}
                tree_page := TreeButton{width: Fill align: Align{x: 0 y: 0.5} padding: Inset{left: 16}
                    draw_text +: {color: fab.color_text_dim}}
            }
            TreeInfoRow := TweakRow{width: Fill height: Fit{min: 22} flow: Right align: Align{y: 0.5}
                indent := Indent{}
                tree_info := BodyNote{width: Fill flow: Right{wrap: true} padding: Inset{left: 16}}
            }
        }
    })
}

impl TreeLine {
    pub(in crate::tweak) fn template(&self) -> LiveId {
        match self { Self::Node { .. } => id!(TreeRow), Self::Pager { .. } => id!(TreePagerRow), Self::Info { .. } => id!(TreeInfoRow) }
    }

    pub(in crate::tweak) fn draw_tree(&self, cx: &mut Cx, row: &View, draggable: bool) {
        let depth = match self {
            Self::Node { depth, label, dim, has_children, expanded, selected, even, seats, drop, .. } => {
                if let Some(mut face) = row.view(cx, ids!(tree_face)).borrow_mut() {
                    face.cursor = draggable.then_some(MouseCursor::Hand);
                    let mode = match drop { None => 0., Some(DropIndicator::Before) => 1.,
                        Some(DropIndicator::After) => 2., Some(DropIndicator::Inside) => 3. };
                    face.draw_bg.set_dyn_instance(cx, id!(drop_mode), &[mode]);
                    face.draw_bg.set_dyn_instance(cx, id!(even), &[u8::from(*even) as f32]);
                    face.draw_bg.set_dyn_instance(cx, id!(selected), &[u8::from(*selected) as f32]);
                }
                let twisty = row.button(cx, ids!(twisty));
                twisty.set_visible(cx, *has_children);
                twisty.set_text(cx, if *expanded { "▾" } else { "▸" });
                row.widget(cx, ids!(twisty_gap)).set_visible(cx, !has_children);
                let select = row.button(cx, ids!(select_btn));
                select.set_text(cx, label);
                if let Some(mut button) = select.borrow_mut() {
                    button.draw_text.set_dyn_instance(cx, id!(dim), &[u8::from(*dim) as f32]);
                    button.draw_text.set_dyn_instance(cx, id!(selected), &[u8::from(*selected) as f32]);
                }
                for (i, id) in [ids!(dot_a), ids!(dot_b), ids!(dot_c), ids!(dot_d)].into_iter().enumerate() {
                    let dot = row.view(cx, id);
                    dot.set_visible(cx, i < seats.len());
                    let Some(seat) = seats.get(i) else { continue };
                    if let Some(mut dot) = dot.borrow_mut() {
                        let c = seat.colour;
                        dot.draw_bg.set_dyn_instance(cx, id!(color), &[c.x, c.y, c.z, c.w]);
                    };
                }
                let overflow = row.label(cx, ids!(dot_overflow));
                overflow.set_visible(cx, seats.len() > 4);
                if seats.len() > 4 { overflow.set_text(cx, &format!("+{}", seats.len() - 4)); }
                *depth
            }
            Self::Pager { depth, label, .. } => { row.button(cx, ids!(tree_page)).set_text(cx, label); *depth }
            Self::Info { depth, text } => { row.label(cx, ids!(tree_info)).set_text(cx, text); *depth }
        };
        if let Some(mut indent) = row.view(cx, ids!(indent)).borrow_mut() {
            indent.walk.width = Size::Fixed(12.0 * f64::from(depth));
        }
    }

    pub(in crate::tweak) fn handle_tree_actions(&self, cx: &Cx, item: &WidgetRef, actions: &Actions, filtering: bool) -> Option<TreeAction> {
        match self {
            Self::Node { path, has_children: true, expanded, .. } if !filtering && item.button(cx, ids!(twisty)).clicked(actions) => {
                Some(if *expanded { TreeAction::Collapse(path.to_string()) } else { TreeAction::Expand { path: path.to_string(), offset: 0 } })
            }
            Self::Node { path, .. } if item.button(cx, ids!(select_btn)).clicked(actions) => Some(TreeAction::Select(path.to_string())),
            Self::Pager { parent, offset, is_search, .. } if item.button(cx, ids!(tree_page)).clicked(actions) => {
                Some(if *is_search { TreeAction::LoadMoreMatches(*offset) } else { TreeAction::Expand { path: parent.to_string(), offset: *offset } })
            }
            _ => None,
        }
    }
}
