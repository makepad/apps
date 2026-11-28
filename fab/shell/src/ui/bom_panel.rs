//! The Electronics bill of materials editor.

use crate::api::AppState;
use crate::ui::editors::Editor;
use makepad_widgets_core::*;

script_mod! {
    use mod.prelude.widgets_internal.*
    use mod.prelude.fab.*
    use mod.widgets.*

    let well_btn_bg = {
        vertex: fn() {
            self.vertex_pos = self.clip_and_transform_vertex(self.rect_pos, self.rect_size)
        }
        pixel: fn() {
            let sdf = Sdf2d.viewport(self.pos * self.rect_size)
            sdf.box(0.0, 0.0, self.rect_size.x, self.rect_size.y, fab.radius)
            let a = (0.10 * self.hover + 0.18 * self.down) * (1.0 - self.disabled)
            sdf.fill(vec4(fab.color_text_well.rgb, a))
            return sdf.result
        }
    }
    mod.widgets.FabBomPanelBase = #(FabBomPanel::register_widget(vm))
    mod.widgets.FabBomPanel = set_type_default() do mod.widgets.FabBomPanelBase{
        width: Fill height: Fill flow: Down
        show_bg: true draw_bg +: {color: fab.color_editor}
        list := mod.widgets.BomPane{
            rows +: {
                Row +: {
                    body +: {
                        fitted +: {
                            cells +: {
                                refs +: { draw_text +: { color: fab.color_text_well } }
                                qty +: { draw_text +: { color: fab.color_text_well } }
                                value +: { draw_text +: { color: fab.color_text_well } }
                                footprint +: { draw_text +: { color: fab.color_text_well } }
                                mpn +: {
                                    draw_bg +: { ..well_btn_bg }
                                    draw_text +: { color: fab.color_text_well color_hover: fab.color_text_well color_down: fab.color_text_well color_focus: fab.color_text_well color_disabled: fab.color_text_well_dim }
                                }
                                supplier +: {
                                    draw_bg +: { ..well_btn_bg }
                                    draw_text +: { color: fab.color_text_well color_hover: fab.color_text_well color_down: fab.color_text_well color_focus: fab.color_text_well color_disabled: fab.color_text_well_dim }
                                }
                                supplier_part +: {
                                    draw_bg +: { ..well_btn_bg }
                                    draw_text +: { color: fab.color_text_well color_hover: fab.color_text_well color_down: fab.color_text_well color_focus: fab.color_text_well color_disabled: fab.color_text_well_dim }
                                }
                                description +: {
                                    draw_bg +: { ..well_btn_bg }
                                    draw_text +: { color: fab.color_text_well color_hover: fab.color_text_well color_down: fab.color_text_well color_focus: fab.color_text_well color_disabled: fab.color_text_well_dim }
                                }
                                dnp +: {
                                    draw_bg +: {
                                        vertex: fn() {
                                            self.vertex_pos = self.clip_and_transform_vertex(self.rect_pos, self.rect_size)
                                        }
                                        border_color: fab.color_text_well
                                        border_color_disabled: fab.color_text_well_muted
                                        mark_color_active: fab.color_text_well
                                        mark_color_active_hover: fab.color_text_well
                                        mark_color_focus: fab.color_text_well
                                    }
                                }
                            }
                            actions +: {
                                add +: {
                                    draw_bg +: { ..well_btn_bg }
                                    draw_text +: { color: fab.color_text_well color_hover: fab.color_text_well color_down: fab.color_text_well color_focus: fab.color_text_well color_disabled: fab.color_text_well_muted }
                                }
                            }
                        }
                        alternate +: {
                            option +: { draw_text +: { color: fab.color_text_well } }
                            decision +: {
                                use_alt +: {
                                    draw_bg +: { ..well_btn_bg }
                                    draw_text +: { color: fab.color_text_well color_hover: fab.color_text_well color_down: fab.color_text_well color_focus: fab.color_text_well color_disabled: fab.color_text_well_muted }
                                }
                                note +: { draw_text +: { color: fab.color_text_well_dim } }
                            }
                        }
                        price +: {
                            change +: {
                                draw_bg +: { ..well_btn_bg }
                                draw_text +: { color: fab.color_text_well color_hover: fab.color_text_well color_down: fab.color_text_well color_focus: fab.color_text_well color_disabled: fab.color_text_well_muted }
                            }
                            quote +: { draw_text +: { color: fab.color_text_well } }
                            old_quote +: { draw_text +: { color: fab.color_text_well_dim } }
                        }
                        detail +: { draw_text +: { color: fab.color_text_well_dim } }
                    }
                }
            }
            cost +: {
                bar +: {
                    heading +: { draw_text +: { color: fab.color_text_well } }
                    boards +: { draw_text +: { color: fab.color_text_well } }
                    qty +: {
                        draw_bg +: {
                            color: fab.color_input_field
                            color_hover: uniform(fab.color_input_field_hover)
                            color_focus: uniform(fab.color_input_field_hover)
                            color_down: uniform(fab.color_input_field_hover)
                            color_empty: uniform(fab.color_input_field)
                        }
                        draw_text +: {
                            color: fab.color_text_input
                            color_hover: fab.color_text_input
                            color_focus: fab.color_text_input
                            color_down: fab.color_text_input
                            color_disabled: fab.color_text_input_muted
                            color_empty: fab.color_text_input_dim
                            color_empty_hover: fab.color_text_input_dim
                            color_empty_focus: fab.color_text_input_dim
                        }
                        draw_cursor +: { color: fab.color_text_input }
                    }
                    compare +: {
                        draw_bg +: { ..well_btn_bg }
                        draw_text +: { color: fab.color_text_well color_hover: fab.color_text_well color_down: fab.color_text_well color_focus: fab.color_text_well color_disabled: fab.color_text_well_muted }
                    }
                    reload +: {
                        draw_bg +: { ..well_btn_bg }
                        draw_text +: { color: fab.color_text_well color_hover: fab.color_text_well color_down: fab.color_text_well color_focus: fab.color_text_well color_disabled: fab.color_text_well_muted }
                    }
                }
                breakdown +: {
                    data +: { draw_text +: { color: fab.color_text_well } }
                }
                figures +: { draw_text +: { color: fab.color_text_well } }
            }
        }
    }
}

#[derive(Script, ScriptHook, Widget)]
pub struct FabBomPanel {
    #[deref] view: View,
}

impl Widget for FabBomPanel {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        if scope.data.get::<AppState>().is_some_and(|state| state.ui.on_screen(Editor::Bom)) {
            self.view.handle_event(cx, event, scope);
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        self.view.draw_walk(cx, scope, walk)
    }
}
