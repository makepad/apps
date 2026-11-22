//! The Paint editor: the brush's colour (the print colours) and the settings of the paint mode in
//! use, as rows. The plate's rail keeps the paint tools; while this editor shows, the rail's palette
//! and dials stay hidden.
//! Row visibility costs O(1) per draw. The palette draws its swatches as it does on the strip.

use crate::api::*;
use crate::ui::keymap::{self, ShortcutId as Key};
use makepad_widgets_core::*;

script_mod! {
    use mod.prelude.widgets_internal.*
    use mod.prelude.fab.*
    use mod.widgets.*

    let SettingRow = View{width: Fill height: Fit flow: Right align: Align{y: 0.5}}

    mod.widgets.FabPaintPanelBase = #(FabPaintPanel::register_widget(vm))
    mod.widgets.FabPaintPanel = set_type_default() do mod.widgets.FabPaintPanelBase{
        width: Fill height: Fill flow: Down
        show_bg: true draw_bg +: { color: fab.color_editor }
        body := mod.widgets.FabScroll{
            padding: Inset{left: theme.prop_pad right: theme.prop_pad top: 6 bottom: 6}
            mod.widgets.FabLabel{
                width: Fill text: "Colour"
                draw_text +: {text_style: theme.font_bold{font_size: fab.font_size_ui}}
            }
            palette := mod.widgets.FabPaintPalette{chip: 22.0 centred: false margin: Inset{top: 4 bottom: 6}}
            diameter_row := SettingRow{
                mod.widgets.FabLabelSmall{width: Fill text: "Brush diameter"}
                FabTip{ place: TipPlace.Left text: #(format!("Brush diameter in mm ({} in the view, {} and {}): drag up or down, or scroll", keymap::shortcut(Key::PaintSizeScroll), keymap::shortcut(Key::PaintSmaller), keymap::shortcut(Key::PaintLarger)))
                    diameter := mod.widgets.FabBrushDial{width: 38 height: 44}
                }
            }
            angle_row := SettingRow{
                mod.widgets.FabLabelSmall{width: Fill text: "Smooth fill angle"}
                FabTip{ place: TipPlace.Left text: "Smooth fill angle: the sharpest edge it spreads over. Drag up or down, or scroll"
                    angle := mod.widgets.FabBrushDial{width: 38 height: 44 angle: true}
                }
            }
            kind_row := SettingRow{
                mod.widgets.FabLabelSmall{width: Fill text: "Pattern"}
                FabTip{ place: TipPlace.Left text: "Pattern: cells, noise, marble, bands or a ramp up the part. Click for the next"
                    pattern_kind := mod.widgets.FabPatternDial{width: 38 height: 44 which: 0}
                }
            }
            scale_row := SettingRow{
                mod.widgets.FabLabelSmall{width: Fill text: "Pattern size"}
                FabTip{ place: TipPlace.Left text: "Pattern size in mm: a cell, a swirl, a band. Drag up or down, or scroll"
                    pattern_scale := mod.widgets.FabPatternDial{width: 38 height: 44 which: 1}
                }
            }
            seed_row := SettingRow{
                mod.widgets.FabLabelSmall{width: Fill text: "Pattern seed"}
                FabTip{ place: TipPlace.Left text: "Pattern seed: another number, another pattern of the kind. Drag up or down, or scroll"
                    pattern_seed := mod.widgets.FabPatternDial{width: 38 height: 44 which: 2}
                }
            }
            sharp_row := SettingRow{
                mod.widgets.FabLabelSmall{width: Fill text: "Sharpness"}
                FabTip{ place: TipPlace.Left text: "Sharpness: 0 blends the colours (printed dithered), 1 turns one into the other at once. Drag up or down, or scroll"
                    pattern_sharp := mod.widgets.FabPatternDial{width: 38 height: 44 which: 3}
                }
            }
            balance_row := SettingRow{
                mod.widgets.FabLabelSmall{width: Fill text: "Balance"}
                FabTip{ place: TipPlace.Left text: "Balance: more of the paint colour (up) or of the part's own (down). Drag up or down, or scroll"
                    pattern_balance := mod.widgets.FabPatternDial{width: 38 height: 44 which: 4}
                }
            }
        }
    }
}

/// The Paint editor. Per draw: update visibility for seven setting rows in O(1).
#[derive(Script, ScriptHook, Widget)]
pub struct FabPaintPanel {
    #[deref]
    view: View,
}

impl Widget for FabPaintPanel {
    fn semantics(&self, _cx: &Cx, node: &mut a11y_tree::Node) {
        node.role = a11y_tree::Role::Group;
        makepad_platform::a11y::set_string(&mut node.name, "Paint");
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope);
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        if let Some(state) = scope.data.get::<AppState>() {
            let mode = state.ui.paint_mode;
            let pattern = mode == PaintMode::Pattern;
            let scaled = pattern && state.ui.paint_pattern.kind.scaled();
            for (row, visible) in [
                (ids!(body.diameter_row), mode == PaintMode::Brush),
                (ids!(body.angle_row), mode == PaintMode::Flood),
                (ids!(body.kind_row), pattern),
                (ids!(body.scale_row), scaled),
                (ids!(body.seed_row), scaled),
                (ids!(body.sharp_row), pattern),
                (ids!(body.balance_row), pattern),
            ] {
                self.view.widget(cx, row).set_visible(cx, visible);
            }
        }
        self.view.draw_walk(cx, scope, walk)
    }
}
