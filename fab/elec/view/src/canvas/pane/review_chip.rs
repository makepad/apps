//! The in-place comparison control shared by the Board and Schematic panes.
use crate::{canvas::tools::{STRIP_INSET, STRIP_TOP}, snapshot::ProposalView};
use makepad_widgets_core::*;

script_mod! {
    use mod.prelude.widgets_internal.*
    use mod.widgets.*
    mod.widgets.ProposalChip = RoundedView{
        width: Fit height: Fit flow: Right align: Align{y: 0.5} spacing: 8 padding: Inset{left: 8 right: 4 top: 3 bottom: 3}
        draw_bg +: {color: theme.color_surface_container border_color: theme.color_focus border_size: 1.0 border_radius: 4.0}
        preview_caption := Label{width: Fill max_lines: 1 text_overflow: Ellipsis draw_text +: {color: theme.color_focus}}
        preview_back_slot := View{height: Fit
            preview_back := Button{text: "Back to design" width: Fill height: Fit flow: Right{wrap: true}
                padding: Inset{left: 3 right: 3 top: 4 bottom: 4}
                label_walk: Walk{width: Fill height: Fit} label_align: Align{x: 0.5}
            }
        }
    }
}

pub(crate) fn caption(view: &ProposalView, removed: bool, progress: Option<&str>) -> String {
    let mut caption = format!("Previewing {}", view.label);
    if removed { caption.push_str(" · This proposal removes this board"); }
    let owed: usize = view.boards.iter().map(|b| b.pending_zones.len()).sum();
    if owed != 0 { caption.push_str(&format!(" · {owed} zone{} not refilled yet", if owed == 1 { "" } else { "s" })); }
    if let Some(progress) = progress { caption.push_str(" · "); caption.push_str(progress); }
    caption
}

pub(crate) fn back(cx: &mut Cx, event: &Event, scope: &mut Scope, chip: &WidgetRef) -> bool {
    let actions = cx.capture_actions(|cx| chip.handle_event(cx, event, scope));
    let button = chip.button(cx, ids!(preview_back_slot.preview_back));
    button.clicked(&actions) || matches!(event, Event::Actions(actions) if button.clicked(actions))
}

pub(crate) fn draw(cx: &mut Cx2d, scope: &mut Scope, uid: WidgetUid, chip: &WidgetRef, rect: Rect) {
    cx.widget_tree_insert_child(uid, live_id!(review_chip), chip.clone());
    let mut walk = chip.walk(cx).with_abs_pos(rect.pos + dvec2(STRIP_INSET, STRIP_TOP));
    let room = (rect.size.x - 2.0 * STRIP_INSET).max(0.0);
    let fit = makepad_widgets_core::width_override::WidthOverride {
        width: Some(Size::fit()), visible: Some(true), ..Default::default()
    };
    let caption = chip.child(id!(preview_caption));
    let slot = chip.child(id!(preview_back_slot));
    let back = slot.child(id!(preview_back)).measure_width(cx, Some(&fit)).unwrap_or(room).ceil();
    let layout = chip.borrow::<View>().unwrap().layout;
    let inside = (room - layout.padding.width()).max(0.);
    let show_caption = inside > back + layout.spacing;
    if caption.visible() != show_caption { caption.set_visible(cx, show_caption); }
    slot.borrow_mut::<View>().unwrap().walk.width = Size::Fixed(back.min(inside));
    // At the sash's narrowest, even Back needs more than one line. Give it the
    // entire inner width there; the pane's semantic value still has the full caption.
    let words = if show_caption { caption.measure_width(cx, Some(&fit)).unwrap_or(inside) + layout.spacing } else { 0. };
    walk.width = Size::Fixed((words + back + layout.padding.width()).ceil().min(room));
    chip.draw_walk_all(cx, scope, walk);
}
