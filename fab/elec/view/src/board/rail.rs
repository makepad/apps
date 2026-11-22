//! The Board pane's layer picker, in the stack order supplied by the caller.

use fab_elec_core::LayerId;
use makepad_widgets_core::*;

// Registers `mod.widgets.BoardLayerRail` and its row template.
script_mod! {
    use mod.prelude.widgets_internal.*
    use mod.widgets.*

    mod.widgets.BoardLayerRailBase = #(BoardLayerRail::register_widget(vm))
    mod.widgets.BoardLayerRail = set_type_default() do mod.widgets.BoardLayerRailBase{
        width: Fit{max: FitBound.Rel{base: Base.Full factor: 1.0}}
        height: Fit
        flow: Down
        padding: 6
        show_bg: false

        layers := FlatList{
            width: Fill
            height: Fit
            flow: Down
            spacing: 1
            scroll_bars +: {show_scroll_x: false show_scroll_y: false}

            Row := SolidView{
                width: Fill
                height: 22
                flow: Right
                spacing: 4
                align: Align{y: 0.5}
                cursor: MouseCursor.Hand
                capture_overload: true
                grab_key_focus: false
                draw_bg +: {
                    hover: instance(0.0)
                    get_color: fn() {
                        return mix(#0000, #x1f232b, self.hover)
                    }
                }
                animator: Animator{
                    hover: {
                        default: @off
                        off: AnimatorState{
                            from: {all: Snap}
                            apply: {draw_bg: {hover: 0.0}}
                        }
                        on: AnimatorState{
                            from: {all: Snap}
                            apply: {draw_bg: {hover: 1.0}}
                        }
                    }
                }

                active_bar := SolidView{
                    width: 2
                    height: Fill
                    draw_bg +: {color: #x6aa7ff00}
                }
                swatch := RoundedView{
                    width: 10
                    height: 10
                    draw_bg +: {color: #0000 border_radius: 2 material: 0.0}
                }
                name := Label{
                    padding: 0
                    text: ""
                    max_lines: 1
                    draw_text +: {
                        color: #xc8cedc
                        text_style +: {font_size: 9}
                    }
                }
                count_box := View{
                    width: Fill
                    height: Fill
                    align: Align{x: 1.0 y: 0.5}
                    count := Label{
                        padding: 0
                        text: ""
                        max_lines: 1
                        draw_text +: {
                            color: #x6b7280
                            text_style +: {font_size: 8}
                        }
                    }
                }
                eye := Button{
                    width: 18
                    height: 22
                    padding: 0
                    margin: 0
                    spacing: 0
                    text: "\u{25cf}"
                    draw_bg +: {
                        pixel: fn() {return vec4(0.0, 0.0, 0.0, 0.0)}
                    }
                    draw_text +: {
                        color: #x8c95aa
                        text_style +: {font_size: 9}
                        get_color: fn() {return self.color}
                    }
                }
            }
        }
    }
}

/// One row of the rail.
#[derive(Clone, Debug, PartialEq)]
pub struct RailLayer {
    /// Stable layer identity, used as the list item's key.
    pub layer: LayerId,
    /// Shown as written (for example, `F.Cu` from [`LayerId::name`]).
    pub name: String,
    /// Straight RGBA in 0..1, the layer's ink on the board.
    pub ink: [f32; 4],
    /// Whether the layer is currently shown on the board.
    pub visible: bool,
    /// Whether new tracks go on this layer.
    pub active: bool,
    /// Items drawn on it; shown dim and right aligned, with zero left blank.
    pub count: usize,
}

/// An interaction with a layer row; the caller owns the resulting board state.
#[derive(Clone, Debug, Default)]
pub enum RailAction {
    /// No layer interaction.
    #[default]
    None,
    /// A row was clicked: make it the active layer.
    Activate(LayerId),
    /// The eye was clicked: show or hide it.
    Toggle(LayerId),
    /// Alt-clicked the eye: solo it, or restore all layers if already solo.
    /// The caller decides which change to make.
    Solo(LayerId),
}

/// The Board pane's layer rail, registered as `mod.widgets.BoardLayerRail`.
#[derive(Script, ScriptHook, Widget)]
pub struct BoardLayerRail {
    #[deref]
    view: View,
    #[rust]
    layers: Vec<RailLayer>,
}

fn row_id(layer: LayerId) -> LiveId {
    LiveId::from_num(0, layer.0 as u64)
}

impl BoardLayerRail {
    /// Replaces the rows in topmost-first stack order; unchanged rows cost no redraw.
    pub fn set_layers(&mut self, cx: &mut Cx, layers: &[RailLayer]) {
        if self.layers == layers {
            return;
        }
        self.layers.clear();
        self.layers.extend_from_slice(layers);
        self.view.redraw(cx);
    }

    /// Returns this rail's actions from the frame, for the parent's action handler.
    pub fn rail_actions(&self, actions: &Actions) -> Vec<RailAction> {
        actions
            .filter_widget_actions_cast::<RailAction>(self.widget_uid())
            .filter(|action| !matches!(action, RailAction::None))
            .collect()
    }
}

impl Widget for BoardLayerRail {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        let actions = cx.capture_actions(|cx| self.view.handle_event(cx, event, scope));
        let list = self.view.flat_list(cx, ids!(layers));
        let uid = self.widget_uid();
        let mut seen = [false; 256];
        for (id, item) in list.items_with_actions(&actions) {
            let Some(layer) = self.layers.iter().find(|layer| row_id(layer.layer) == id) else {
                continue;
            };
            // FlatList reports an item for each of its child actions.
            let index = layer.layer.0 as usize;
            if seen[index] {
                continue;
            }
            seen[index] = true;

            let eye = item.button(cx, ids!(eye));
            if let Some(modifiers) = eye.clicked_modifiers(&actions) {
                let action = if modifiers.alt {
                    RailAction::Solo(layer.layer)
                } else {
                    RailAction::Toggle(layer.layer)
                };
                cx.widget_action(uid, action);
            } else if let Some(up) = item.as_view().finger_up(&actions) {
                let eye_rect = eye.area().rect(cx);
                // Overloaded capture keeps the row hovered over its eye, but
                // a gesture starting or ending on the eye belongs to the button.
                if up.is_primary_hit()
                    && up.is_over
                    && !up.cancelled
                    && up.was_tap()
                    && !eye_rect.contains(up.abs_start)
                    && !eye_rect.contains(up.abs)
                {
                    cx.widget_action(uid, RailAction::Activate(layer.layer));
                }
            }
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        while let Some(step) = self.view.draw_walk(cx, scope, walk).step() {
            let Some(mut list) = step.borrow_mut::<FlatList>() else {
                continue;
            };
            for layer in &self.layers {
                let Some(row) = list.item(cx, row_id(layer.layer), id!(Row)) else {
                    continue;
                };
                let bar = row.view(cx, ids!(active_bar));
                if let Some(mut bar) = bar.borrow_mut() {
                    bar.draw_bg.set_dyn_instance(
                        cx,
                        id!(color),
                        &[
                            106.0 / 255.0,
                            167.0 / 255.0,
                            1.0,
                            if layer.active { 1.0 } else { 0.0 },
                        ],
                    );
                }
                let mut ink = layer.ink;
                if !layer.visible {
                    ink[3] = 0.35;
                }
                let swatch = row.view(cx, ids!(swatch));
                if let Some(mut swatch) = swatch.borrow_mut() {
                    swatch.draw_bg.set_dyn_instance(cx, id!(color), &ink);
                }
                let name = row.label(cx, ids!(name));
                name.set_text(cx, &layer.name);
                name.set_text_color(
                    cx,
                    if layer.active {
                        vec4(1.0, 1.0, 1.0, 1.0)
                    } else if layer.visible {
                        vec4(200.0 / 255.0, 206.0 / 255.0, 220.0 / 255.0, 1.0)
                    } else {
                        vec4(107.0 / 255.0, 114.0 / 255.0, 128.0 / 255.0, 1.0)
                    },
                );
                let count = if layer.count == 0 {
                    String::new()
                } else {
                    layer.count.to_string()
                };
                row.label(cx, ids!(count_box.count)).set_text(cx, &count);
                row.button(cx, ids!(eye))
                    .set_text(cx, if layer.visible { "\u{25cf}" } else { "\u{25cb}" });
                row.draw_all(cx, scope);
            }
            list.items.retain_visible();
        }
        DrawStep::done()
    }
}
