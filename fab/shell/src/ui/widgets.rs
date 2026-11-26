//! The Fab control kit: every reusable styled widget the panels
//! compose. Fab density (20 px rows, 4–6 px paddings, 1 px borders,
//! 4 px corners), every control with hover / press / focus / disabled states
//! and a tooltip that carries its shortcut.
//!
//! The rule here is *restyle, never re-implement*: momentary buttons are
//! `Button`, toggles and segmented controls are `RadioButton` (it already
//! owns the `active` animator state), checkboxes are `CheckBox`, tooltips are
//! `Tip` over the shell's single `TipLayer`. The controls that add shell-level
//! behaviour are the overlay menus (`popover.rs`, `menu.rs`).

use makepad_widgets_core::*;
use makepad_widgets_core::widget_tree::CxWidgetExt;

script_mod! {
    use mod.prelude.fab.*
    use mod.widgets.*

    // =====================================================================
    // Type
    // =====================================================================

    // The stock `Label` carries `padding: theme.mspace_1` (3 px all round),
    // which makes its box ~25 px tall at our 8.5 pt type. In a 20 px Fab
    // row that overflows, `align.y` clamps to 0, and every label lands ~2.5 px
    // below the row's centreline while the buttons beside it (whose own text is
    // `ink_centered`) sit on it. Zero padding, and the ink centres.
    mod.widgets.FabLabel = Label{
        width: Fit
        height: Fit
        padding: Inset{left: 0 right: 0 top: 0 bottom: 0}
        draw_text +: {
            ink_centered: true
            color: fab.color_text
            text_style: theme.font_regular{
                font_size: fab.font_size_ui
            }
        }
    }

    mod.widgets.FabLabelDim = mod.widgets.FabLabel{
        draw_text +: {
            color: fab.color_text_dim
        }
    }

    mod.widgets.FabLabelMuted = mod.widgets.FabLabel{
        draw_text +: {
            color: fab.color_text_muted
        }
    }

    mod.widgets.FabLabelSmall = mod.widgets.FabLabel{
        draw_text +: {
            color: fab.color_text_dim
            text_style: theme.font_regular{
                font_size: fab.font_size_small
            }
        }
    }

    mod.widgets.FabHeaderLabel = Label{
        width: Fit
        height: Fit
        padding: Inset{left: 0 right: 0 top: 0 bottom: 0}
        draw_text +: {
            ink_centered: true
            color: fab.color_text_header
            text_style: theme.font_bold{
                font_size: fab.font_size_header
            }
        }
    }

    mod.widgets.FabTitleLabel = Label{
        width: Fit
        height: Fit
        padding: Inset{left: 0 right: 0 top: 0 bottom: 0}
        draw_text +: {
            ink_centered: true
            color: fab.color_text_active
            text_style: theme.font_bold{
                font_size: fab.font_size_title
            }
        }
    }

    // =====================================================================
    // Tooltip wrapper — one `TipLayer{}` lives in the shell overlay.
    // =====================================================================

    mod.widgets.FabTip = Tip{
        width: Fit
        height: Fit
    }

    // =====================================================================
    // Buttons (momentary) — `Button` owns hover / down / focus / disabled.
    // =====================================================================

    mod.widgets.FabButton = ButtonFlat{
        height: theme.prop_row_height
        width: Fit
        // Text-only: y-align is safe. DrawVector icons must not sit here.
        align: Align{x: 0.5 y: 0.5}
        margin: Inset{top: 0 bottom: 0 left: 0 right: 0}
        padding: Inset{left: 9 right: 9 top: 0 bottom: 0}
        label_walk: Walk{ width: Fit height: Fit }
        // The property kit's field face: no frame at rest, only the keyboard focus ring.
        draw_bg +: {
            color: theme.prop_field
            color_hover: theme.prop_field_hover
            color_down: theme.prop_field_active
            color_focus: theme.prop_field
            color_disabled: fab.color_editor_alt
            border_radius: fab.radius
            border_size: 1.0
            border_color: vec4(0.0, 0.0, 0.0, 0.0)
            border_color_hover: vec4(0.0, 0.0, 0.0, 0.0)
            border_color_down: vec4(0.0, 0.0, 0.0, 0.0)
            border_color_focus: fab.color_focus_ring
            border_color_disabled: vec4(0.0, 0.0, 0.0, 0.0)
            border_color_2: uniform(vec4(-1.0, -1.0, -1.0, -1.0))
        }
        draw_text +: {
            ink_centered: true
            color: fab.color_text
            color_hover: fab.color_text_active
            color_down: fab.color_text_active
            color_focus: fab.color_text
            color_disabled: fab.color_disabled_ink
            text_style: theme.font_regular{
                font_size: fab.font_size_ui
            }
        }
        animator: Animator{
            disabled: {
                default: @off
                off: AnimatorState{
                    from: {all: Forward {duration: 0.0}}
                    apply: {
                        draw_bg: {disabled: 0.0}
                        draw_text: {disabled: 0.0}
                    }
                }
                on: AnimatorState{
                    from: {all: Forward {duration: fab.anim_fast}}
                    apply: {
                        draw_bg: {disabled: 1.0}
                        draw_text: {disabled: 1.0}
                    }
                }
            }
            hover: {
                default: @off
                off: AnimatorState{
                    from: {all: Forward {duration: fab.anim_fast}}
                    apply: {
                        draw_bg: {down: snap(0.0), hover: 0.0}
                        draw_text: {down: snap(0.0), hover: 0.0}
                    }
                }
                on: AnimatorState{
                    from: {all: Snap}
                    apply: {
                        draw_bg: {down: snap(0.0), hover: 1.0}
                        draw_text: {down: snap(0.0), hover: 1.0}
                    }
                }
                down: AnimatorState{
                    from: {all: Snap}
                    apply: {
                        draw_bg: {down: snap(1.0), hover: 1.0}
                        draw_text: {down: snap(1.0), hover: 1.0}
                    }
                }
            }
            focus: {
                default: @off
                off: AnimatorState{
                    from: {all: Forward {duration: fab.anim_fast}}
                    apply: {
                        draw_bg: {focus: 0.0}
                        draw_text: {focus: 0.0}
                    }
                }
                on: AnimatorState{
                    from: {all: Forward {duration: 0.0}}
                    apply: {
                        draw_bg: {focus: 1.0}
                        draw_text: {focus: 1.0}
                    }
                }
            }
        }
    }

    mod.widgets.FabButtonAccent = mod.widgets.FabButton{
        draw_bg +: {
            color: fab.color_accent
            color_hover: fab.color_accent_hover
            color_down: fab.color_accent_dim
            color_focus: fab.color_accent
        }
        // its words on the accent's bright face: black, under the pointer and pressed too
        draw_text +: {
            color: fab.color_text_on_accent
            color_hover: fab.color_text_on_accent
            color_down: fab.color_text_on_accent
            color_focus: fab.color_text_on_accent
        }
    }

    // Flat momentary text button: no chrome until hovered. Text-only:
    // y-align is safe. (Popup-opening header entries are `FabMenuButton` in
    // `dropdown.rs` — they carry the shared open-state machine.)
    mod.widgets.FabFlatButton = ButtonFlatter{
        height: theme.prop_row_height
        width: Fit
        align: Align{x: 0.5 y: 0.5}
        margin: Inset{top: 0 bottom: 0 left: 0 right: 0}
        padding: Inset{left: 7 right: 7 top: 0 bottom: 0}
        label_walk: Walk{ width: Fit height: Fit }
        draw_bg +: {
            color: vec4(0.0, 0.0, 0.0, 0.0)
            color_hover: fab.color_button_hover
            color_down: fab.color_button_down
            color_focus: vec4(0.0, 0.0, 0.0, 0.0)
            border_radius: fab.radius
            border_size: 0.0
        }
        draw_text +: {
            ink_centered: true
            color: fab.color_text
            color_hover: fab.color_text_active
            color_down: fab.color_text_active
            color_focus: fab.color_text
            color_disabled: fab.color_disabled_ink
            text_style: theme.font_regular{
                font_size: fab.font_size_ui
            }
        }
        animator: Animator{
            disabled: {
                default: @off
                off: AnimatorState{
                    from: {all: Forward {duration: 0.0}}
                    apply: {
                        draw_bg: {disabled: 0.0}
                        draw_text: {disabled: 0.0}
                    }
                }
                on: AnimatorState{
                    from: {all: Forward {duration: fab.anim_fast}}
                    apply: {
                        draw_bg: {disabled: 1.0}
                        draw_text: {disabled: 1.0}
                    }
                }
            }
            hover: {
                default: @off
                off: AnimatorState{
                    from: {all: Forward {duration: fab.anim_fast}}
                    apply: {
                        draw_bg: {down: snap(0.0), hover: 0.0}
                        draw_text: {down: snap(0.0), hover: 0.0}
                    }
                }
                on: AnimatorState{
                    from: {all: Snap}
                    apply: {
                        draw_bg: {down: snap(0.0), hover: 1.0}
                        draw_text: {down: snap(0.0), hover: 1.0}
                    }
                }
                down: AnimatorState{
                    from: {all: Snap}
                    apply: {
                        draw_bg: {down: snap(1.0), hover: 1.0}
                        draw_text: {down: snap(1.0), hover: 1.0}
                    }
                }
            }
            focus: {
                default: @off
                off: AnimatorState{
                    from: {all: Forward {duration: fab.anim_fast}}
                    apply: {
                        draw_bg: {focus: 0.0}
                        draw_text: {focus: 0.0}
                    }
                }
                on: AnimatorState{
                    from: {all: Forward {duration: 0.0}}
                    apply: {
                        draw_bg: {focus: 1.0}
                        draw_text: {focus: 1.0}
                    }
                }
            }
        }
    }

    // Icon-only momentary button (16 px glyph in a 22 px well).
    // The glyph is centred by align at any button size.
    mod.widgets.FabIconButton = ButtonFlatterIcon{
        width: 22
        height: 22
        align: Align{x: 0.5 y: 0.5}
        spacing: 0.0
        margin: Inset{top: 0 bottom: 0 left: 0 right: 0}
        padding: Inset{left: 0 right: 0 top: 0 bottom: 0}
        text: ""
        label_walk: Walk{ width: 0 height: 0 }
        icon_walk: Walk{ width: fab.icon_size height: fab.icon_size }
        draw_bg +: {
            color: vec4(0.0, 0.0, 0.0, 0.0)
            // off: no face of its own, its icon dimmed (`draw_icon`'s color_disabled)
            color_disabled: vec4(0.0, 0.0, 0.0, 0.0)
            color_hover: fab.color_button_hover
            color_down: fab.color_button_down
            color_focus: vec4(0.0, 0.0, 0.0, 0.0)
            border_radius: fab.radius
            border_size: 0.0
        }
        draw_icon +: {
            hover: instance(0.0)
            down: instance(0.0)
            disabled: instance(0.0)
            color: fab.color_text
            color_hover: fab.color_text_active
            color_down: fab.color_text_active
            color_disabled: fab.color_disabled_ink
            get_color: fn() {
                return self.color
                    .mix(self.color_hover, self.hover)
                    .mix(self.color_down, self.down)
                    .mix(self.color_disabled, self.disabled)
            }
        }
        // The stock Button animator only drives draw_bg/draw_text; the icon
        // needs the same states or it would sit at one flat tint.
        animator: Animator{
            disabled: {
                default: @off
                off: AnimatorState{
                    from: {all: Forward {duration: 0.0}}
                    apply: {
                        draw_bg: {disabled: 0.0}
                        draw_text: {disabled: 0.0}
                        draw_icon: {disabled: 0.0}
                    }
                }
                on: AnimatorState{
                    from: {all: Forward {duration: fab.anim_fast}}
                    apply: {
                        draw_bg: {disabled: 1.0}
                        draw_text: {disabled: 1.0}
                        draw_icon: {disabled: 1.0}
                    }
                }
            }
            hover: {
                default: @off
                off: AnimatorState{
                    from: {all: Forward {duration: fab.anim_fast}}
                    apply: {
                        draw_bg: {down: snap(0.0), hover: 0.0}
                        draw_text: {down: snap(0.0), hover: 0.0}
                        draw_icon: {down: snap(0.0), hover: 0.0}
                    }
                }
                on: AnimatorState{
                    from: {all: Snap}
                    apply: {
                        draw_bg: {down: snap(0.0), hover: 1.0}
                        draw_text: {down: snap(0.0), hover: 1.0}
                        draw_icon: {down: snap(0.0), hover: 1.0}
                    }
                }
                down: AnimatorState{
                    from: {all: Snap}
                    apply: {
                        draw_bg: {down: snap(1.0), hover: 1.0}
                        draw_text: {down: snap(1.0), hover: 1.0}
                        draw_icon: {down: snap(1.0), hover: 1.0}
                    }
                }
            }
            focus: {
                default: @off
                off: AnimatorState{
                    from: {all: Forward {duration: fab.anim_fast}}
                    apply: {
                        draw_bg: {focus: 0.0}
                        draw_text: {focus: 0.0}
                    }
                }
                on: AnimatorState{
                    from: {all: Forward {duration: 0.0}}
                    apply: {
                        draw_bg: {focus: 1.0}
                        draw_text: {focus: 1.0}
                    }
                }
            }
        }
    }


    // A light icon button for a panel's action strip: no frame, the glyph at the icon size
    // on its own grid in the quiet ink, a field-coloured face only under the pointer.
    mod.widgets.FabActionButton = mod.widgets.FabIconButton{
        width: 26
        height: 22
        icon_walk: Walk{ width: fab.icon_size height: fab.icon_size }
        draw_bg +: {
            color_hover: theme.prop_field_hover
            color_down: theme.prop_field_active
            border_radius: theme.prop_radius
        }
        draw_icon +: {
            color: theme.prop_ink_dim
            color_hover: theme.color_text
            color_down: theme.color_text
        }
    }

    // =====================================================================
    // Toggles / segmented — `RadioButton` owns the `active` state.
    // =====================================================================

    // Text pill (workspaces, N-panel tabs). Text-only: y-align is safe.
    mod.widgets.FabSegmentTab = RadioButtonTab{
        height: theme.prop_row_height
        width: Fit
        align: Align{x: 0.5 y: 0.5}
        margin: Inset{top: 0 bottom: 0 left: 0 right: 0}
        padding: Inset{left: 9 right: 9 top: 0 bottom: 0}
        // RadioButtonTabFlat indents the label 12 px for its (absent) check
        // mark — that is what pushed every pill's ink off-centre.
        label_walk: Walk{ width: Fit height: Fit margin: Inset{left: 0 right: 0 top: 0 bottom: 0} }
        label_align: Align{x: 0.5 y: 0.5}
        icon_walk: Walk{ width: 0 height: 0 }
        // The property kit's choice look: field faces framed only by the keyboard focus ring, the
        // answer a lighter face in the full ink, the others in the quiet ink.
        draw_bg +: {
            color: theme.prop_field
            color_hover: theme.prop_field_hover
            color_down: theme.prop_field_active
            color_active: theme.prop_choice_on
            color_focus: theme.prop_field
            color_disabled: fab.color_editor_alt
            border_radius: fab.radius
            border_size: 1.0
            border_color: vec4(0.0, 0.0, 0.0, 0.0)
            border_color_hover: vec4(0.0, 0.0, 0.0, 0.0)
            border_color_down: vec4(0.0, 0.0, 0.0, 0.0)
            border_color_active: vec4(0.0, 0.0, 0.0, 0.0)
            border_color_focus: fab.color_focus_ring
            border_color_disabled: vec4(0.0, 0.0, 0.0, 0.0)
            border_color_2: uniform(vec4(-1.0, -1.0, -1.0, -1.0))
        }
        draw_text +: {
            ink_centered: true
            color: fab.color_text_dim
            color_hover: fab.color_text_active
            color_down: fab.color_text_active
            color_active: fab.color_text
            color_focus: fab.color_text_dim
            color_disabled: fab.color_disabled_ink
            text_style: theme.font_regular{
                font_size: fab.font_size_ui
            }
        }
        // a pill that shows an icon (a row too narrow for its words): the icon in the text's inks
        draw_icon +: {
            hover: instance(0.0)
            down: instance(0.0)
            active: instance(0.0)
            disabled: instance(0.0)
            color: fab.color_text_dim
            color_hover: fab.color_text_active
            color_active: fab.color_text
            color_disabled: fab.color_disabled_ink
            // premultiplied with the glyph's coverage, as DrawSvg's own colour is (the inks carry
            // alpha)
            get_color: fn() {
                let base = self.eval_gradient()
                let ink = self.color
                    .mix(self.color_hover, self.hover)
                    .mix(self.color_active, self.active)
                    .mix(self.color_disabled, self.disabled)
                return vec4(ink.rgb * ink.a * base.a, ink.a * base.a) * self.opacity
            }
        }
        animator: Animator{
            disabled: {
                default: @off
                off: AnimatorState{
                    from: {all: Forward {duration: 0.0}}
                    apply: {
                        draw_bg: {disabled: 0.0}
                        draw_text: {disabled: 0.0}
                        draw_icon: {disabled: 0.0}
                    }
                }
                on: AnimatorState{
                    from: {all: Forward {duration: fab.anim_fast}}
                    apply: {
                        draw_bg: {disabled: 1.0}
                        draw_text: {disabled: 1.0}
                        draw_icon: {disabled: 1.0}
                    }
                }
            }
            hover: {
                default: @off
                off: AnimatorState{
                    from: {all: Forward {duration: fab.anim_fast}}
                    apply: {
                        draw_bg: {down: snap(0.0), hover: 0.0}
                        draw_text: {down: snap(0.0), hover: 0.0}
                        draw_icon: {down: snap(0.0), hover: 0.0}
                    }
                }
                on: AnimatorState{
                    from: {all: Snap}
                    apply: {
                        draw_bg: {down: snap(0.0), hover: 1.0}
                        draw_text: {down: snap(0.0), hover: 1.0}
                        draw_icon: {down: snap(0.0), hover: 1.0}
                    }
                }
                down: AnimatorState{
                    from: {all: Snap}
                    apply: {
                        draw_bg: {down: snap(1.0), hover: 1.0}
                        draw_text: {down: snap(1.0), hover: 1.0}
                        draw_icon: {down: snap(1.0), hover: 1.0}
                    }
                }
            }
            active: {
                default: @off
                off: AnimatorState{
                    from: {all: Forward {duration: fab.anim_fast}}
                    apply: {
                        draw_bg: {active: 0.0}
                        draw_text: {active: 0.0}
                        draw_icon: {active: 0.0}
                    }
                }
                on: AnimatorState{
                    from: {all: Forward {duration: 0.0}}
                    apply: {
                        draw_bg: {active: 1.0}
                        draw_text: {active: 1.0}
                        draw_icon: {active: 1.0}
                    }
                }
            }
            focus: {
                default: @off
                off: AnimatorState{
                    from: {all: Forward {duration: fab.anim_fast}}
                    apply: {
                        draw_bg: {focus: 0.0}
                        draw_text: {focus: 0.0}
                    }
                }
                on: AnimatorState{
                    from: {all: Forward {duration: 0.0}}
                    apply: {
                        draw_bg: {focus: 1.0}
                        draw_text: {focus: 1.0}
                    }
                }
            }
        }
    }

    // Icon toggle (shading modes, x-ray, overlays, lock, tools).
    // 16 px glyph in a 24×20 well: centre with padding, never turtle-align
    // (DrawSvg is DrawVector).
    // A toolbar's tab (the Printers editor's Printers / Jobs): no face, its name half-lit at rest and lit under the pointer, the chosen
    // one in the full ink over an accent underline.
    mod.widgets.FabUnderlineTab = mod.widgets.FabSegmentTab{
        height: 24
        padding: Inset{left: 10 right: 10 top: 0 bottom: 0}
        draw_bg +: {
            pixel: fn() {
                let sdf = Sdf2d.viewport(self.pos * self.rect_size)
                sdf.box(6.0, self.rect_size.y - 2.0, self.rect_size.x - 12.0, 2.0, 0.5)
                sdf.fill(vec4(fab.color_accent.xyz, fab.color_accent.w * self.active))
                return sdf.result
            }
        }
        draw_text +: {
            color: mix(fab.color_text_dim, fab.color_text, 0.5)
            color_hover: fab.color_text
            color_down: fab.color_text
            color_active: fab.color_text_active
            // (the one just clicked keeps the keyboard: it reads as chosen, not as rest)
            color_focus: fab.color_text_active
        }
    }
    mod.widgets.FabIconToggle = RadioButtonTab{
        width: 24
        height: theme.prop_row_height
        align: Align{x: 0.0 y: 0.0}
        margin: Inset{top: 0 bottom: 0 left: 0 right: 0}
        padding: Inset{left: 4 right: 4 top: 2 bottom: 2}
        text: ""
        label_walk: Walk{ width: 0 height: 0 margin: Inset{left: 0 right: 0 top: 0 bottom: 0} }
        icon_walk: Walk{ width: fab.icon_size height: fab.icon_size }
        draw_bg +: {
            color: vec4(0.0, 0.0, 0.0, 0.0)
            color_hover: fab.color_button_hover
            color_down: fab.color_button_down
            color_active: fab.color_button_active
            color_focus: vec4(0.0, 0.0, 0.0, 0.0)
            color_disabled: vec4(0.0, 0.0, 0.0, 0.0)
            border_radius: fab.radius
            border_size: 1.0
            border_color: vec4(0.0, 0.0, 0.0, 0.0)
            border_color_hover: vec4(0.0, 0.0, 0.0, 0.0)
            border_color_down: vec4(0.0, 0.0, 0.0, 0.0)
            border_color_active: vec4(0.0, 0.0, 0.0, 0.0)
            border_color_focus: fab.color_focus_ring
            border_color_disabled: vec4(0.0, 0.0, 0.0, 0.0)
            border_color_2: uniform(vec4(-1.0, -1.0, -1.0, -1.0))
        }
        draw_icon +: {
            hover: instance(0.0)
            down: instance(0.0)
            active: instance(0.0)
            disabled: instance(0.0)
            color: fab.color_text
            color_hover: fab.color_text_active
            color_down: fab.color_text_active
            // on the accent's bright face, while on: black (the sheet's ink on the accent)
            color_active: fab.color_text_on_accent
            color_disabled: fab.color_disabled_ink
            get_color: fn() {
                return self.color
                    .mix(self.color_active, self.active)
                    .mix(self.color_hover, self.hover * (1.0 - self.active))
                    .mix(self.color_disabled, self.disabled)
            }
        }
        animator: Animator{
            disabled: {
                default: @off
                off: AnimatorState{
                    from: {all: Forward {duration: 0.0}}
                    apply: {
                        draw_bg: {disabled: 0.0}
                        draw_text: {disabled: 0.0}
                        draw_icon: {disabled: 0.0}
                    }
                }
                on: AnimatorState{
                    from: {all: Forward {duration: fab.anim_fast}}
                    apply: {
                        draw_bg: {disabled: 1.0}
                        draw_text: {disabled: 1.0}
                        draw_icon: {disabled: 1.0}
                    }
                }
            }
            hover: {
                default: @off
                off: AnimatorState{
                    from: {all: Forward {duration: fab.anim_fast}}
                    apply: {
                        draw_bg: {down: snap(0.0), hover: 0.0}
                        draw_text: {down: snap(0.0), hover: 0.0}
                        draw_icon: {down: snap(0.0), hover: 0.0}
                    }
                }
                on: AnimatorState{
                    from: {all: Snap}
                    apply: {
                        draw_bg: {down: snap(0.0), hover: 1.0}
                        draw_text: {down: snap(0.0), hover: 1.0}
                        draw_icon: {down: snap(0.0), hover: 1.0}
                    }
                }
                down: AnimatorState{
                    from: {all: Snap}
                    apply: {
                        draw_bg: {down: snap(1.0), hover: 1.0}
                        draw_text: {down: snap(1.0), hover: 1.0}
                        draw_icon: {down: snap(1.0), hover: 1.0}
                    }
                }
            }
            active: {
                default: @off
                off: AnimatorState{
                    from: {all: Forward {duration: fab.anim_fast}}
                    apply: {
                        draw_bg: {active: 0.0}
                        draw_text: {active: 0.0}
                        draw_icon: {active: 0.0}
                    }
                }
                on: AnimatorState{
                    from: {all: Forward {duration: 0.0}}
                    apply: {
                        draw_bg: {active: 1.0}
                        draw_text: {active: 1.0}
                        draw_icon: {active: 1.0}
                    }
                }
            }
            focus: {
                default: @off
                off: AnimatorState{
                    from: {all: Forward {duration: fab.anim_fast}}
                    apply: {
                        draw_bg: {focus: 0.0}
                        draw_text: {focus: 0.0}
                    }
                }
                on: AnimatorState{
                    from: {all: Forward {duration: 0.0}}
                    apply: {
                        draw_bg: {focus: 1.0}
                        draw_text: {focus: 1.0}
                    }
                }
            }
        }
    }

    // The Electronics recorder's two icon controls in its 22 px row, the glyph centred by padding:
    // auto re-record (an edit renders the take again from where it is heard) and the power cycle
    // (⏻: the board restarted from power-on; a momentary button).
    mod.widgets.FabRerecordToggle = mod.widgets.FabIconToggle{
        independent: true width: 22 height: 22 padding: 3
        draw_icon +: { svg: crate_resource("self://resources/icons/refresh.svg") }
    }
    mod.widgets.FabPowerCycleButton = mod.widgets.FabIconButton{
        draw_icon +: { svg: crate_resource("self://resources/icons/power_cycle.svg") }
    }

    // An icon that stands for a boolean of its own (a chip over the view)
    // rather than one choice among several: it must switch OFF on a second
    // click, which a radio in a group deliberately does not do.
    mod.widgets.FabIconCheck = mod.widgets.FabIconToggle{
        independent: true
    }

    // =====================================================================
    // Tool strips — the card that floats over a view, its tool buttons and
    // its divider. The Design plate's strip and the electronics canvases'
    // strips are one widget, each with its own tool list.
    // =====================================================================

    // A floating card's ground (the tool strips, the plate view's tool rows, menus and
    // popovers): its fill and outline, and a soft drop shadow round it. The quad grows past the
    // card by the shadow's reach, so the shadow falls outside its rect; presses still take the
    // card's rect alone. `fade` dims all of it (a closing menu).
    mod.draw.FabFloatBg = mod.draw.DrawQuad{
        color: uniform(fab.color_float)
        border_color: uniform(fab.color_float_border)
        radius: uniform(fab.radius_lg)
        shadow_color: uniform(fab.color_float_shadow)
        shadow_radius: uniform(10.0)
        shadow_offset: uniform(vec2(0.0, 3.0))
        fade: instance(1.0)
        vertex: fn() {
            let lo = min(self.shadow_offset, vec2(0.0, 0.0)) - vec2(self.shadow_radius, self.shadow_radius)
            let hi = max(self.shadow_offset, vec2(0.0, 0.0)) + vec2(self.shadow_radius, self.shadow_radius)
            let pos = self.rect_pos + lo
            let size = self.rect_size + hi - lo
            // the card's own clip (its rect, when it clips its content) grown by the shadow's
            // reach, so the shadow shows round it; the view's clip still holds
            let clipped = clamp(
                clamp(self.geom.pos * size + pos, self.draw_clip.xy + lo, self.draw_clip.zw + hi)
                + self.draw_list.view_shift,
                self.draw_list.view_clip.xy,
                self.draw_list.view_clip.zw
            )
            self.pos = (clipped - pos) / size
            self.world = self.draw_list.view_transform * vec4(clipped.x, clipped.y, self.draw_depth + self.draw_call.zbias, 1.0)
            return self.draw_pass.camera_projection * (self.draw_pass.camera_view * self.world)
        }
        pixel: fn() {
            let lo = min(self.shadow_offset, vec2(0.0, 0.0)) - vec2(self.shadow_radius, self.shadow_radius)
            let hi = max(self.shadow_offset, vec2(0.0, 0.0)) + vec2(self.shadow_radius, self.shadow_radius)
            // this pixel in the card's own points (0,0 its top left)
            let p = self.pos * (self.rect_size + hi - lo) + lo
            let shade = GaussShadow.rounded_box_shadow(self.shadow_offset, self.rect_size + self.shadow_offset, p, self.shadow_radius * 0.5, self.radius)
            let sdf = Sdf2d.viewport(p)
            sdf.clear(self.shadow_color * shade)
            sdf.box(0.5, 0.5, self.rect_size.x - 1.0, self.rect_size.y - 1.0, self.radius)
            sdf.fill_keep(self.color)
            sdf.stroke(self.border_color, 1.0)
            return sdf.result * self.fade
        }
    }

    mod.widgets.FabFloat = View{
        width: Fit
        height: Fit
        cursor: MouseCursor.Default
        show_bg: true
        draw_bg: mod.draw.FabFloatBg{}
    }
    mod.widgets.FabTool = mod.widgets.FabIconToggle{
        width: 32
        height: 32
        padding: Inset{left: 8 right: 8 top: 8 bottom: 8}
    }
    mod.widgets.FabToolSep = View{ width: 20 height: 1 margin: Inset{left: 6 top: 4 bottom: 4} show_bg: true draw_bg +: { color: fab.color_border_light } }

    // =====================================================================
    // Checkbox — a text field's well: a #00000020 fill, its outline #00000040 at rest, #ffffff20
    // under the pointer and with focus; the tick in the text's ink, lit under the pointer.
    // The property kit's PropCheckBox (the slicer's rows, Global, Advanced) draws the same.
    // =====================================================================

    mod.widgets.FabCheckBox = CheckBox{
        // As wide as its label, up to the room left on its line; a longer label wraps there
        // and the box grows a row, so a narrow panel never clips it.
        height: Fit{min: theme.prop_row_height}
        width: Fit{max: FitBound.Rel{base: Base.Line factor: 1.0}}
        flow: Right{wrap: true}
        align: Align{x: 0.0 y: 0.5}
        padding: Inset{left: 0 right: 0 top: 0 bottom: 0}
        margin: Inset{top: 0 bottom: 0 left: 0 right: 0}
        label_walk: Walk{ width: Fit height: Fit margin: Inset{left: 19 right: 0 top: 0 bottom: 0} }
        draw_bg +: {
            size: uniform(13.0)
            border_radius: uniform(1.5)
            mark_at_end: uniform(0.0)
            color: uniform(theme.prop_field)
            border_color: uniform(#x00000000)
            border_color_hover: uniform(#xffffff20)
            border_color_focus: uniform(#xffffff20)
            mark_color_active: uniform(fab.color_text)
            mark_color_hover: uniform(fab.color_text_hover)
            mark_color_disabled: uniform(fab.color_text_dim)
            pixel: fn() {
                let dpi = self.draw_pass.dpi_factor
                let px = 1.0 / dpi
                let origin = self.rect_pos + vec2((self.rect_size.x - self.size) * self.mark_at_end, (self.rect_size.y - self.size) * 0.5 + self.mark_box_offset_y)
                let lo = floor(origin * dpi + 0.5) / dpi - self.rect_pos
                let hi = floor((origin + vec2(self.size)) * dpi + 0.5) / dpi - self.rect_pos
                let sdf = Sdf2d.viewport(self.pos * self.rect_size)
                // The well and its 1 pt outline, as a text field's, covered per device pixel: the
                // SDF's own soft edge would leave a 1 pt line at half strength.
                let q = self.pos * self.rect_size
                let r = self.border_radius
                let e = max(lo - q, q - hi) + vec2(r, r)
                let d = length(max(e, vec2(0.0, 0.0))) + min(max(e.x, e.y), 0.0) - r
                let outer = clamp(0.5 - d * dpi, 0.0, 1.0)
                let ring = outer - clamp(0.5 - (d + 1.0) * dpi, 0.0, 1.0)
                let f = self.color * (1.0 - self.disabled * 0.5)
                let line = self.border_color.mix(self.border_color_hover, self.hover).mix(self.border_color_focus, max(self.focus, self.down)).mix(self.border_color, self.disabled)
                let well = vec4(f.rgb * f.a, f.a) * outer
                sdf.result = vec4(line.rgb * line.a, line.a) * ring + well * (1.0 - line.a * ring)
                let c = (lo + hi) * 0.5
                let ink = self.mark_color_active.mix(self.mark_color_hover, self.hover).mix(self.mark_color_disabled, self.disabled)
                // The tick (or a mixed dash) is 1.5 pt wide, in the text's ink.
                if self.mixed > 0.5 {
                    sdf.move_to(lo.x + 3.0, c.y)
                    sdf.line_to(hi.x - 3.0, c.y)
                    sdf.stroke(ink, 0.75)
                } else if self.active > 0.5 {
                    sdf.move_to(c.x - 3.0, c.y)
                    sdf.line_to(c.x - 1.0, c.y + 2.0)
                    sdf.line_to(c.x + 3.0, c.y - 2.0)
                    sdf.stroke(ink, 0.75)
                }
                return sdf.result
            }
        }
        draw_text +: {
            ink_centered: true
            color: fab.color_text
            color_hover: fab.color_text_hover
            color_active: fab.color_text
            color_disabled: fab.color_text_dim
            text_style: theme.font_regular{
                font_size: fab.font_size_ui
            }
        }
    }

    mod.widgets.PropCheckBox = mod.widgets.PropCheckBox{
        label_walk: Walk{width: Fill height: Fit margin: Inset{left: 19}}
        draw_bg +: {
            size: uniform(13.0)
            border_radius: uniform(1.5)
            mark_at_end: uniform(0.0)
            color: uniform(theme.prop_field)
            border_color: uniform(#x00000000)
            border_color_hover: uniform(#xffffff20)
            border_color_focus: uniform(#xffffff20)
            mark_color_active: uniform(fab.color_text)
            mark_color_hover: uniform(fab.color_text_hover)
            mark_color_disabled: uniform(fab.color_text_dim)
            pixel: fn() {
                let dpi = self.draw_pass.dpi_factor
                let px = 1.0 / dpi
                let origin = self.rect_pos + vec2((self.rect_size.x - self.size) * self.mark_at_end, (self.rect_size.y - self.size) * 0.5 + self.mark_box_offset_y)
                let lo = floor(origin * dpi + 0.5) / dpi - self.rect_pos
                let hi = floor((origin + vec2(self.size)) * dpi + 0.5) / dpi - self.rect_pos
                let sdf = Sdf2d.viewport(self.pos * self.rect_size)
                // The well and its 1 pt outline, as a text field's, covered per device pixel: the
                // SDF's own soft edge would leave a 1 pt line at half strength.
                let q = self.pos * self.rect_size
                let r = self.border_radius
                let e = max(lo - q, q - hi) + vec2(r, r)
                let d = length(max(e, vec2(0.0, 0.0))) + min(max(e.x, e.y), 0.0) - r
                let outer = clamp(0.5 - d * dpi, 0.0, 1.0)
                let ring = outer - clamp(0.5 - (d + 1.0) * dpi, 0.0, 1.0)
                let f = self.color * (1.0 - self.disabled * 0.5)
                let line = self.border_color.mix(self.border_color_hover, self.hover).mix(self.border_color_focus, max(self.focus, self.down)).mix(self.border_color, self.disabled)
                let well = vec4(f.rgb * f.a, f.a) * outer
                sdf.result = vec4(line.rgb * line.a, line.a) * ring + well * (1.0 - line.a * ring)
                let c = (lo + hi) * 0.5
                let ink = self.mark_color_active.mix(self.mark_color_hover, self.hover).mix(self.mark_color_disabled, self.disabled)
                // The tick (or a mixed dash) is 1.5 pt wide, in the text's ink.
                if self.mixed > 0.5 {
                    sdf.move_to(lo.x + 3.0, c.y)
                    sdf.line_to(hi.x - 3.0, c.y)
                    sdf.stroke(ink, 0.75)
                } else if self.active > 0.5 {
                    sdf.move_to(c.x - 3.0, c.y)
                    sdf.line_to(c.x - 1.0, c.y + 2.0)
                    sdf.line_to(c.x + 3.0, c.y - 2.0)
                    sdf.stroke(ink, 0.75)
                }
                return sdf.result
            }
        }
    }

    // =====================================================================
    // Text fields
    // =====================================================================

    // The row sets the height, as for the property kit's fields: a desktop sheet gives a
    // TextInput a field's height of its own, which stood the field taller than its row (a button
    // beside it, a tree row it renames in) and put the text below the row's middle.
    // A text field: a #00000020 fill; its outline #00000040 at rest, #ffffff20 under the
    // pointer and with focus.
    mod.widgets.FabInput = TextInput{
        height: theme.prop_row_height
        min_height: 0
        width: Fill
        padding: Inset{left: 6 right: 6 top: 0 bottom: 0}
        margin: Inset{top: 0 bottom: 0 left: 0 right: 0}
        empty_text: ""
        draw_bg +: {
            color: theme.prop_field
            color_hover: uniform(theme.prop_field_hover)
            color_focus: uniform(theme.prop_field_hover)
            color_down: uniform(theme.prop_field_hover)
            color_empty: uniform(theme.prop_field)
            color_2: uniform(vec4(-1.0, -1.0, -1.0, -1.0))
            border_radius: theme.prop_radius
            border_color: uniform(#x00000000)
            border_color_hover: uniform(#xffffff20)
            border_color_focus: uniform(#xffffff20)
            border_color_down: uniform(#xffffff20)
            border_color_empty: uniform(#x00000000)
        }
        draw_text +: {
            ink_centered: true
            color: fab.color_text
            text_style: theme.font_regular{
                font_size: fab.font_size_ui
            }
        }
    }

    // Search field with the magnifier inside the well. The well's outline is a FabButton's face
    // (kept a ring's width back from its rect for the focus ring), so a search beside a button
    // shares its top and bottom. The input is the library's WellInput: the
    // chrome-less input a well holds, which no desktop sheet gives a field height of its own (a
    // TextInput takes the sheet's, and stood taller than the well with its line centred low).
    // A section's row as the Part editor's are: its name (`name`) right-aligned in the label column
    // every editor's rows share (`param_panel::LABEL_W`), in the body ink, then its fields (the
    // row's other children). The Job and Slice editors' rows.
    mod.widgets.FabFieldRow = View{
        width: Fill
        height: theme.prop_row_height
        flow: Right
        align: Align{x: 0.0 y: 0.5}
        spacing: theme.prop_label_gap
        // (a property row's label: fit to its words, centred by their ink)
        name := Label{
            width: Fill{weight: 0 basis: "42%" max: #(crate::ui::param_panel::LABEL_W)}
            height: Fit padding: Inset{} align: Align{x: 1.0 y: 0.5}
            text: "" max_lines: 1 text_overflow: TextOverflow.Ellipsis
            draw_text +: {
                ink_centered: true
                color: fab.color_text
                text_style: theme.font_regular{font_size: theme.prop_font_size line_spacing: 1.0}
            }
        }
    }

    // A toolbar's vertical rule, between groups of its tools (a light line: the theme's separator
    // inks sit on the panel's own value). A View's quad paints nothing of itself, so the rule says
    // how.
    mod.widgets.FabVRule = View{width: 1 height: 20 show_bg: true draw_bg +: {
        color: uniform(#xffffff1a)
        pixel: fn() { return vec4(self.color.xyz * self.color.w, self.color.w) }
    }}

    // A panel editor's toolbar: the row across its top whose left end is its area's editor
    // selector (the notch layer draws that chip there: `area_notch::TOOLBAR_SLOT`), then the
    // editor's own tools (its toggles and actions), its search always last. Every panel editor
    // starts with one, so the tab bar above holds only the area's tabs.
    // A row of tools that scrolls sideways once it is wider than its room: a drag (from a button
    // too) or the mouse wheel moves it, with no bar drawn. `mask` covers its side padding with
    // the editor ground, so tools scrolled under the area's selector or close button go under.
    mod.widgets.FabToolScrollBase = #(FabToolScroll::register_widget(vm))
    mod.widgets.FabToolScroll = set_type_default() do mod.widgets.FabToolScrollBase{
        width: Fill height: Fit flow: Right
        scroll_bars: mod.widgets.ScrollBars{
            show_scroll_x: true show_scroll_y: false
            scroll_bar_x.drag_scrolling: true
            scroll_bar_x.use_vertical_finger_scroll: true
            scroll_bar_x.show_handle: false
        }
        draw_mask +: {
            color: uniform(fab.color_toolbar)
            pixel: fn() { return vec4(self.color.xyz * self.color.w, self.color.w) }
        }
    }

    mod.widgets.FabEditorBar = mod.widgets.FabToolScroll{
        mask: true
        width: Fill height: #(crate::ui::area_notch::TOOLBAR_H)
        flow: Right align: Align{y: 0.5} spacing: 6
        // its ground (a shade above the panel's) and a subtle line along its foot, a whole point
        // on device pixels
        show_bg: true
        draw_bg +: {
            color: uniform(fab.color_toolbar)
            line: uniform(0.15)
            pixel: fn() {
                let foot = step(self.rect_size.y - 1.0, self.pos.y * self.rect_size.y) * self.line
                let g = self.color
                return vec4(g.xyz * g.w * (1.0 - foot), g.w + foot * (1.0 - g.w))
            }
        }
        // (the right end is the tab's close button's, drawn by the notch layer)
        padding: Inset{left: #(crate::ui::area_notch::TOOLBAR_SLOT) right: #(crate::ui::area_notch::TOOLBAR_END)}
        // a rule after the selector: what follows is this editor's own
        rule := mod.widgets.FabVRule{}
    }

    // A search field (a toolbar's): at rest only its magnifier, a button at the toolbar's right
    // end; pressed, it opens leftward into the field (`FabSearch`). The input is the box (a
    // #00000020 fill; its outline #00000040 at rest, #ffffff20 under the pointer and with
    // focus), the magnifier over its left end; a toolbar's field's height (28: room above and
    // below the words), wherever it is.
    mod.widgets.FabSearchBase = #(FabSearch::register_widget(vm))
    mod.widgets.FabSearch = set_type_default() do mod.widgets.FabSearchBase{
        width: Fill
        height: 28
        flow: Overlay
        row := View{
            width: Fill
            height: Fill
            flow: Overlay
            align: Align{x: 0.0 y: 0.5}
            input := TextInput{
                width: Fill
                height: Fill
                min_height: 0
                margin: 0.0
                // the words start past the magnifier
                padding: Inset{left: 10 + 16 right: 6 top: 0 bottom: 0}
                empty_text: "Search"
                draw_bg +: {
                    border_radius: theme.prop_radius
                    border_size: 1.0
                    color: theme.prop_field
                    color_hover: uniform(theme.prop_field_hover)
                    color_focus: uniform(theme.prop_field_hover)
                    color_down: uniform(theme.prop_field_hover)
                    color_empty: uniform(theme.prop_field)
                    color_2: uniform(vec4(-1.0, -1.0, -1.0, -1.0))
                    border_color: uniform(#x00000000)
                    border_color_hover: uniform(#xffffff20)
                    border_color_focus: uniform(#xffffff20)
                    border_color_down: uniform(#xffffff20)
                    border_color_empty: uniform(#x00000000)
                    border_color_2: uniform(vec4(-1.0, -1.0, -1.0, -1.0))
                }
                draw_text +: {
                    ink_centered: true
                    color: fab.color_text
                    color_empty: fab.color_text_muted
                    color_empty_hover: fab.color_text_dim
                    color_empty_focus: fab.color_text_dim
                    text_style: theme.font_regular{
                        font_size: fab.font_size_ui
                    }
                }
            }
            // the magnifier, as big as the toolbars' icons, centred in the search at rest (28)
            glyph_slot := View{
                width: Fit
                height: Fill
                padding: Inset{left: 6}
                align: Align{x: 0.0 y: 0.5}
                glyph := mod.widgets.FabIconMuted{
                    width: 16
                    height: 16
                    icon_walk: Walk{ width: 16 height: 16 }
                    draw_icon +: {
                        svg: crate_resource("self://resources/icons/search.svg")
                    }
                }
            }
        }
    }

    // =====================================================================
    // Chrome: area headers, panels, rows, rules
    // =====================================================================

    mod.widgets.FabHr = View{
        width: Fill
        height: 1
        show_bg: true
        draw_bg +: {
            color: fab.color_border
        }
    }

    // Panel header: disclosure triangle + title + pin, hover-lit, clickable.
    mod.widgets.FabPanelHeader = View{
        width: Fill
        height: 22
        flow: Right
        // Chevron and title share the row's centre line. Aligned to the top
        // they each sat at their own height and the label rode above the
        // arrow.
        align: Align{x: 0.0 y: 0.5}
        padding: Inset{left: 4 right: 4 top: 0 bottom: 0}
        spacing: 3
        cursor: MouseCursor.Hand
        show_bg: true
        draw_bg +: {
            hover: instance(0.0)
            down: instance(0.0)
            pixel: fn() {
                let sdf = Sdf2d.viewport(self.pos * self.rect_size)
                sdf.box(0.5, 0.5, self.rect_size.x - 1.0, self.rect_size.y - 1.0, fab.radius)
                sdf.fill(fab.color_panel.mix(fab.color_button_hover, self.hover * 0.5).mix(fab.color_button_down, self.down))
                return sdf.result
            }
        }
        animator: Animator{
            hover: {
                default: @off
                off: AnimatorState{
                    from: {all: Forward {duration: fab.anim_fast}}
                    apply: { draw_bg: {hover: 0.0} }
                }
                on: AnimatorState{
                    from: {all: Snap}
                    apply: { draw_bg: {hover: 1.0} }
                }
            }
            down: {
                default: @off
                off: AnimatorState{
                    from: {all: Forward {duration: fab.anim_fast}}
                    apply: { draw_bg: {down: 0.0} }
                }
                on: AnimatorState{
                    from: {all: Snap}
                    apply: { draw_bg: {down: 1.0} }
                }
            }
        }
        // One chevron, rotated — a second SVG for "closed" would be two files
        // that must agree; `IconRotated` keeps it one glyph and one truth.
        // The row centres its children, so the glyph needs no hand-set pad:
        // a fixed 12x12 slot centred on the same line as the title's ink.
        tri_slot := View{
            width: fab.icon_size_sm
            height: fab.icon_size_sm
            padding: Inset{top: 0 bottom: 0 left: 0 right: 0}
            tri := IconRotated{
                width: fab.icon_size_sm
                height: fab.icon_size_sm
                align: Align{x: 0.5 y: 0.5}
                icon_walk: Walk{ width: fab.icon_size_sm height: fab.icon_size_sm }
                draw_icon +: {
                    color: fab.color_text_dim
                    svg: crate_resource("self://resources/icons/chevron_down.svg")
                    rotation_angle: uniform(0.0)
                }
            }
        }
        title := mod.widgets.FabHeaderLabel{
            height: Fit
            text: "Panel"
        }
        Filler{}
    }

    // A collapsible properties panel: header + animated body.
    //
    // `FoldHeader` flattens its `header` / `body` when children are visited —
    // `self.header.children(visit)`, not `visit(header)` — so a panel's own
    // header would have no path from outside. The header therefore sits in a
    // one-child wrapper: from anywhere above, the header is `<panel>.hdr` and
    // the body's rows are `<panel>.<row>` directly.
    mod.widgets.FabPanel = FoldHeader{
        width: Fill
        height: Fit
        body_walk: Walk{ width: Fill height: Fit }
        header: View{
            width: Fill
            height: Fit
            flow: Down
            hdr := mod.widgets.FabPanelHeader{}
        }
        body: View{
            width: Fill
            height: Fit
            flow: Down
            padding: Inset{left: 0 right: 0 top: 2 bottom: 6}
            spacing: 1
        }
    }

    // The input rows' names ("Position", "Layer height", a feature's "Width") in the body ink: a
    // parameter's name is what the row is, not a caption to it. Defined again here, after the
    // shared sheet and before every panel, so all of the app's rows take it.
    mod.widgets.PropRow = mod.widgets.PropRow{ label +: { draw_text +: { color: fab.color_text } } }
    mod.widgets.PropNumberRow = mod.widgets.PropNumberRow{ label +: { draw_text +: { color: fab.color_text } } }
    mod.widgets.PropDropDownRow = mod.widgets.PropDropDownRow{ label +: { draw_text +: { color: fab.color_text } } }
    mod.widgets.PropSegmentedRow = mod.widgets.PropSegmentedRow{ label +: { draw_text +: { color: fab.color_text } } }
    // A section has no plate: its header alone carries a shade under the panel
    // (`fab.color_section`), square, no line, and answers the pointer with its title, not a lit
    // box (lit by `section_headers`). The bar is inset as the rows are (`prop_pad` each side), so
    // it spans the labels to the fields' end, its chevron a gap in from its start (a nested
    // section's bar starts further in: `param_panel`'s Group). Its title is bold, so the sections
    // read apart from their rows; its rows have room above and below; twice the theme's gap below
    // each.
    // A number field's (a slider's) fill: a faint white over its well (#ffffff10), brighter under the
    // pointer and brighter still while pressed or dragged. Fab's own fields and the property
    // kit's number rows (`PropNumberRow`'s field, made before Fab) alike.
    mod.widgets.ValueInput = mod.widgets.ValueInput{
        // the room at each end for the hover chevrons (5 pt in, 4 pt wide): an axis label or a
        // value starts just past them, not the library's wider 14 pt in
        arrow_zone: 10.0
        // the name inside the field (a number's, an axis's): at its left in the first voice, the
        // value at its right in the second; a value that is not the field's own (mixed,
        // inherited, read-only) quieter still
        draw_label +: {color: fab.color_text text_style: theme.font_regular{font_size: theme.prop_font_size line_spacing: 1.0}}
        labelled_value_color: fab.color_text_dim
        muted_color: fab.color_text_muted
        draw_bg +: {
            fill_color: uniform(#xffffff10)
            fill_color_hover: uniform(#xffffff28)
            fill_color_drag: uniform(#xffffff40)
            outline_color: uniform(#xffffff20)
            pixel: fn() {
                let sdf = Sdf2d.viewport_px(self.pos * self.rect_size, self.draw_pass.dpi_factor)
                let w = self.rect_size.x
                let h = self.rect_size.y
                let hairline = 1.0 / self.draw_pass.dpi_factor
                let inset = hairline * 0.5
                let top = self.radius * self.round_top
                let bottom = self.radius * self.round_bottom
                let base = self.color.mix(self.color_hover, self.hover).mix(self.color_drag, self.drag)
                sdf.box_y(inset, inset, w - hairline, h - hairline, top, bottom)
                let reveal = mix(1.0, max(self.hover, max(self.drag, self.focus)), self.flat)
                sdf.fill_keep(vec4(base.rgb, base.a * reveal))
                if self.fill >= 0.0 {
                    // The fill's edge on a device pixel: a crisp line at any scale, never a soft column.
                    let fill_x = floor((1.0 + (w - 2.0) * self.fill) * self.draw_pass.dpi_factor + 0.5) / self.draw_pass.dpi_factor
                    let ink = self.fill_color.mix(self.fill_color_hover, self.hover).mix(self.fill_color_drag, self.drag)
                    let wash = vec4(ink.xyz, ink.w * step(self.pos.x * w, fill_x))
                    sdf.fill_keep(wash)
                }
                sdf.fill_keep(vec4(self.color_marked.rgb, self.color_marked.a * self.marked * 0.18))
                // the outline every field shows under the pointer and while focused (a text
                // field's too: `FabInput`); SDF strokes take a half-width
                if max(self.hover, self.focus) > 0.5 {
                    sdf.stroke_keep(self.outline_color, hairline * 0.5)
                }
                if self.invalid > 0.0 {
                    sdf.stroke(vec4(self.color_invalid.rgb, self.color_invalid.a * self.invalid), 1.5)
                }
                return sdf.result
            }
        }
    }
    mod.widgets.PropNumberRow = mod.widgets.PropNumberRow{
        value +: {
            num := mod.widgets.ValueInput{width: Fill height: Fill min_height: 0}
        }
    }

    mod.widgets.PropSection = mod.widgets.PropSection{
        margin: Inset{bottom: theme.prop_section_gap * 2.0}
        // a grip at the header's right end: a drag on it moves the section among its editor's
        // others (`section_reorder`); a section whose place is fixed turns it off
        grip: true
        draw_bg +: {color: #x00000000}
        // room above and below the rows, so they stand clear of the bars
        body +: {padding: Inset{top: 6 bottom: 8 left: theme.prop_pad right: theme.prop_pad}}
        header +: {hdr +: {
            margin: Inset{left: theme.prop_pad right: theme.prop_pad}
            padding: Inset{left: theme.prop_label_gap right: theme.prop_pad}
            draw_bg +: {color: fab.color_section color_hover: fab.color_section radius: 0.0}
            title +: {draw_text +: {text_style: theme.font_bold{font_size: theme.prop_font_size}}}
            grip +: {cursor: MouseCursor.Move}
        }}
    }
    mod.widgets.PropCheckRow = mod.widgets.PropCheckRow{ label +: { draw_text +: { color: fab.color_text } } }

    // Fab's number rows: their fields carry units, so a field keeps room for `-360.0 mm`
    // between its arrow zones before its row puts it under the label.
    mod.widgets.FabNumberRow = mod.widgets.PropNumberRow{value_min: 84}

    // Label-left / value-right row.
    mod.widgets.FabPropRow = View{
        width: Fill
        height: theme.prop_row_height
        flow: Right
        align: Align{x: 0.0 y: 0.5}
        padding: Inset{}
        spacing: 0
        // One line each, always: a key or value that does not fit elides —
        // it never wraps into the next row's space.
        name := mod.widgets.FabLabelDim{
            width: theme.prop_label_width
            height: Fill
            align: Align{x: 1.0 y: 0.5}
            text: "Name"
            max_lines: 1
            text_overflow: TextOverflow.Ellipsis
        }
        gap := View{width: theme.prop_label_gap height: Fill}
        value := mod.widgets.FabLabel{
            width: Fill
            text: "—"
            max_lines: 1
            text_overflow: TextOverflow.Ellipsis
        }
        trail := View{width: theme.prop_trail_width height: Fill}
    }

    // A key-cap chip (tooltips, keymap help, palette rows).
    mod.widgets.FabKeyCap = View{
        width: Fit
        height: 16
        flow: Right
        align: Align{x: 0.5 y: 0.5}
        padding: Inset{left: 5 right: 5 top: 0 bottom: 0}
        show_bg: true
        draw_bg +: {
            pixel: fn() {
                let sdf = Sdf2d.viewport(self.pos * self.rect_size)
                sdf.box(0.5, 0.5, self.rect_size.x - 1.0, self.rect_size.y - 1.0, fab.radius)
                sdf.fill_keep(fab.color_editor_alt)
                sdf.stroke(fab.color_border_light, 1.0)
                return sdf.result
            }
        }
        cap := mod.widgets.FabLabelSmall{
            text: ""
            draw_text +: {
                color: fab.color_text_dim
                text_style: theme.font_code{
                    font_size: fab.font_size_small
                }
            }
        }
    }

    // Colour swatch (a filament's colour).
    mod.widgets.FabSwatch = View{
        width: fab.swatch_width
        height: 16
        cursor: MouseCursor.Hand
        show_bg: true
        draw_bg +: {
            hover: instance(0.0)
            swatch: instance(vec4(0.8, 0.8, 0.8, 1.0))
            pixel: fn() {
                let sdf = Sdf2d.viewport(self.pos * self.rect_size)
                sdf.box(0.5, 0.5, self.rect_size.x - 1.0, self.rect_size.y - 1.0, fab.radius)
                sdf.fill_keep(vec4(self.swatch.xyz, 1.0))
                sdf.stroke(fab.color_border.mix(fab.color_focus_ring, self.hover), 1.0)
                return sdf.result
            }
        }
        animator: Animator{
            hover: {
                default: @off
                off: AnimatorState{
                    from: {all: Forward {duration: fab.anim_fast}}
                    apply: { draw_bg: {hover: 0.0} }
                }
                on: AnimatorState{
                    from: {all: Snap}
                    apply: { draw_bg: {hover: 1.0} }
                }
            }
        }
    }

    // Progress bar (load / bake / render).
    mod.widgets.FabProgress = View{
        width: 120
        height: 6
        show_bg: true
        draw_bg +: {
            progress: instance(0.0)
            pixel: fn() {
                let sdf = Sdf2d.viewport(self.pos * self.rect_size)
                sdf.box(0.5, 0.5, self.rect_size.x - 1.0, self.rect_size.y - 1.0, 1.5)
                sdf.fill_keep(fab.color_input)
                sdf.stroke(fab.color_border, 1.0)
                sdf.box(1.0, 1.0, max(2.0, (self.rect_size.x - 2.0) * self.progress), self.rect_size.y - 2.0, 1.0)
                sdf.fill(fab.color_accent)
                return sdf.result
            }
        }
    }

    // The scroll body every editor uses.
    // A panel's scrolling body. Scrolled down, a shadow comes up along its top edge (under the
    // editor's toolbar), so the toolbar reads as above the content passing under it; at the top
    // there is none. It ramps in over the first stretch scrolled (`fade_ramp`) rather than
    // switching on. The same shade as a list's (`draw_scroll_shade`).
    mod.widgets.FabScroll = ScrollShadowView{
        width: Fill
        height: Fill
        flow: Down
        // a baseline gap under the editor's toolbar, so its first row never touches it (a body
        // that sets its own padding keeps at least this at its top)
        padding: Inset{top: #(PANEL_TOP)}
        fade_size: #(SCROLL_SHADE_H)
        fade_ramp: #(SCROLL_SHADE_RAMP)
        fade_top: true
        fade_bottom: false
        fade_left: false
        fade_right: false
        bars: mod.widgets.ScrollBars{
            show_scroll_x: false
            show_scroll_y: true
            scroll_bar_y.drag_scrolling: true
            // a mouse drag that starts on a control (a section's header: its fold drag) is that
            // control's, never a scroll; the wheel, the bar and a touch still scroll
            scroll_bar_y.mouse_drags_from_widgets: false
        }
        draw_fade +: {color: uniform(fab.color_scroll_shade) falloff: uniform(1.6)}
    }

    // A list's shadow along its top edge, scrolled down (`draw_scroll_shade`): FabScroll's look.
    mod.draw.FabScrollShade = mod.draw.DrawQuad{
        weight: instance(0.0)
        pixel: fn() {
            let a = pow(clamp(1.0 - self.pos.y, 0.0, 1.0), 1.6) * self.weight * fab.color_scroll_shade.w
            return vec4(fab.color_scroll_shade.xyz * a, a)
        }
    }
}

/// A progress bar's fill, for a `draw_walk` to set before the bar draws. Not by an apply: that
/// asks for a redraw, and one asked while drawing draws the next frame too, and so on for ever.
pub fn set_progress(cx: &Cx, bar: &ViewRef, fraction: f32) {
    if let Some(mut bar) = bar.borrow_mut() {
        bar.draw_bg.set_dyn_instance(cx, live_id!(progress), &[fraction]);
    }
}

/// Point a panel's chevron down (open) or right (closed).
pub fn set_panel_chevron(view: &View, cx: &mut Cx, panel: &[LiveId], open: bool) {
    let mut path = panel.to_vec();
    path.push(live_id!(hdr));
    path.push(live_id!(tri_slot));
    path.push(live_id!(tri));
    let mut tri = view.widget(cx, &path);
    if tri.is_empty() {
        return;
    }
    let a: f32 = if open {
        0.0
    } else {
        -std::f32::consts::FRAC_PI_2
    };
    script_apply_eval!(cx, tri, {
        draw_icon +: { rotation_angle: #(a) }
    });
}

/// Reconcile generated rows under their section, keeping the semantic tree in step.
pub(crate) fn section_rows(cx: &mut Cx, section: &WidgetRef, rows: Vec<(LiveId, WidgetRef)>) {
    let body = section.widget(cx, ids!(content));
    let uid = body.widget_uid();
    if let Some(mut view) = body.borrow_mut::<View>() {
        if view.children.len() == rows.len()
            && view.children.iter().zip(&rows).all(|((a, x), (b, y))| a == b && x.widget_uid() == y.widget_uid()) {
            return;
        }
        view.children = rows.into_iter().collect();
        for (id, child) in &view.children {
            cx.widget_tree_insert_child(uid, *id, child.clone());
        }
        view.redraw(cx);
    }
    cx.widget_tree_mark_dirty(uid);
}

/// The section titles lit now (their widgets' uids), and a fold drag under way: one shared state,
/// so each panel lights and folds only its own headers, a drag runs on across panels (Transform
/// into the parameter groups), and nothing needs a field of its own.
#[derive(Default)]
struct Headers {
    lit: Vec<WidgetUid>,
    drag: Option<FoldDrag>,
    /// A section held by its grip, moving with the pointer, or settling into its place.
    moving: Option<SectionMove>,
}

/// How wide the press zone at a header's right end is that holds the section (its grip and the
/// room around it): a grip alone is too small a target.
const GRIP_ZONE: f64 = 28.0;

/// A section moving: held by its grip it follows the pointer, drawn last so it lies over the
/// others, while they slide (animated) to open the place it would land in; released, it settles
/// into that place and the editor keeps the order (`SectionOrder`). The stack's layout never
/// changes while it moves: each section's offset is its slot's top margin, given back by its
/// bottom margin, so nothing after it moves with it.
struct SectionMove {
    key: LiveId,
    id: LiveId,
    /// The pointer's height below the held section's top at the press, and the pointer now.
    grab: f64,
    pointer: f64,
    /// The room the held section takes (its top to the next one's).
    room: f64,
    /// The others in order: where each sits with the held one out of the stack (its top), and
    /// its room.
    others: Vec<(LiveId, f64, f64)>,
    /// Where the stack lays the held section out while it is held (last).
    natural: f64,
    /// How high and how low its top may go: within its stack (a subsection never leaves the
    /// section it is in), from the first one's top to the last place's.
    lo: f64,
    hi: f64,
    /// Where it would land: how many of the others go before it.
    slot: usize,
    /// The others' offsets: now, and where each is going.
    offsets: std::collections::HashMap<LiveId, (f64, f64)>,
    /// Released: the held section's own offset as it settles (to 0); the move ends at rest.
    settle: Option<f64>,
    frame: NextFrame,
    last: Option<f64>,
    /// What to redraw as it moves (the editor).
    redraw: Area,
}

impl SectionMove {
    /// The held section's top as the pointer holds it, kept within its stack.
    fn held_top(&self) -> f64 {
        (self.pointer - self.grab).clamp(self.lo, self.hi.max(self.lo))
    }
}

/// Each editor's order of its sections, as the person dragged them (by the editor's key; the
/// section ids in order). An editor shows its sections in this order, any it does not name after
/// in their own order. For this run of the app.
#[derive(Default)]
struct SectionOrder(std::collections::HashMap<LiveId, Vec<LiveId>>);

/// Where `id` stands in editor `key`'s order (`usize::MAX` when it was never moved), for an
/// editor that lays its sections out itself to sort them by.
pub(crate) fn section_rank(cx: &mut Cx, key: LiveId, id: LiveId) -> usize {
    cx.global::<SectionOrder>().0.get(&key).and_then(|order| order.iter().position(|o| *o == id)).unwrap_or(usize::MAX)
}

/// The section of editor `key` held by its grip now (not one settling), to draw last.
pub(crate) fn held_section(cx: &mut Cx, key: LiveId) -> Option<LiveId> {
    cx.global::<Headers>().moving.as_ref().filter(|m| m.key == key && m.settle.is_none()).map(|m| m.id)
}

/// The offset section `id` of editor `key` is drawn at this frame (its slot's top margin; its
/// bottom margin gives it back): 0 unless a section is moving.
pub(crate) fn section_offset(cx: &mut Cx, key: LiveId, id: LiveId) -> f64 {
    let Some(m) = cx.global::<Headers>().moving.as_ref().filter(|m| m.key == key) else { return 0.0 };
    if m.id == id {
        return m.settle.unwrap_or(m.held_top() - m.natural);
    }
    m.offsets.get(&id).map_or(0.0, |(now, _)| *now)
}

/// Lay out `container`'s sections (its children named in editor `key`'s order) in that order, the
/// held one last, each at its offset (`section_offset`, as its slot's margins: the slots are
/// plain boxes round the sections). Call before the editor draws. O(children).
pub(crate) fn apply_section_order(cx: &mut Cx, key: LiveId, container: &WidgetRef) {
    let order = cx.global::<SectionOrder>().0.get(&key).cloned();
    let held = held_section(cx, key);
    let ids: Vec<LiveId> = with_children(container, |children| {
        if let Some(order) = &order {
            let slots: Vec<usize> = (0..children.len()).filter(|i| order.contains(&children[*i].0)).collect();
            let mut placed: Vec<(LiveId, WidgetRef)> = slots.iter().map(|i| children[*i].clone()).collect();
            placed.sort_by_key(|(id, _)| order.iter().position(|o| o == id).unwrap_or(usize::MAX));
            for (i, child) in slots.iter().zip(placed) {
                children[*i] = child;
            }
        }
        // the held section last: drawn over the others
        if let Some(held) = held {
            if let Some(i) = children.iter().position(|(id, _)| *id == held) {
                let child = children.remove(i);
                children.push(child);
            }
        }
        children.iter().map(|(id, _)| *id).collect()
    })
    .unwrap_or_default();
    for id in ids {
        let offset = section_offset(cx, key, id);
        let slot = container.widget(cx, &[id]);
        if let Some(mut view) = slot.borrow_mut::<View>() {
            if view.walk.margin.top != offset {
                view.walk.margin.top = offset;
                view.walk.margin.bottom = -offset;
            }
        };
    }
}

/// A box's children, a plain one's or a scrolling one's (`FabScroll`).
fn with_children<R>(w: &WidgetRef, f: impl FnOnce(&mut SmallVec<[(LiveId, WidgetRef); 2]>) -> R) -> Option<R> {
    if let Some(mut view) = w.borrow_mut::<View>() {
        return Some(f(&mut view.children));
    }
    if let Some(mut view) = w.borrow_mut::<ScrollShadowView>() {
        return Some(f(&mut view.children));
    }
    None
}

/// The grips of editor `key`'s `sections` (each its id, the section, and the slot it is laid out
/// in: a plain box round it, or the section itself where the editor draws them): a press at a
/// header's right end (`GRIP_ZONE`) holds its section, which follows the pointer while the others
/// slide aside (`SectionMove`). Call from the editor's `handle_event` with every event (its
/// animation runs on its frames), before `section_headers` and before the sections get it: the
/// press is the grip's, so its header neither folds nor starts a fold drag. `redraw` is the
/// editor's area. O(sections) per event.
pub(crate) fn section_reorder(cx: &mut Cx, event: &Event, key: LiveId, sections: &[(LiveId, WidgetRef, WidgetRef)], redraw: Area) {
    match event {
        Event::MouseDown(e) if e.button.is_primary() && cx.global::<Headers>().moving.is_none() => {
            // the sections shown, top to bottom, and which one's grip the press is on
            let mut shown: Vec<(LiveId, Rect, Option<Area>)> = Vec::new();
            for (id, section, slot) in sections {
                if !section.visible() { continue; }
                let hdr = section.widget(cx, ids!(hdr)).area();
                let rect = slot.area().rect(cx);
                if !hdr.is_valid(cx) || rect.size.y <= 0.0 { continue; }
                let h = hdr.rect(cx);
                let zone = Rect { pos: dvec2(h.pos.x + h.size.x - GRIP_ZONE, h.pos.y), size: dvec2(GRIP_ZONE, h.size.y) };
                let on = section.widget(cx, ids!(hdr.grip)).visible() && zone.contains(e.abs);
                shown.push((*id, rect, on.then_some(hdr)));
            }
            shown.sort_by(|a, b| a.1.pos.y.total_cmp(&b.1.pos.y));
            let Some(k) = shown.iter().position(|s| s.2.is_some()) else { return };
            if let Some(hdr) = shown[k].2 {
                e.handled.set(hdr);
            }
            // each one's room: its top to the next one's (the last, its height and the gap)
            let gap = if shown.len() >= 2 { (shown[1].1.pos.y - shown[0].1.pos.y - shown[0].1.size.y).max(0.0) } else { 0.0 };
            let rooms: Vec<f64> = (0..shown.len())
                .map(|i| if i + 1 < shown.len() { shown[i + 1].1.pos.y - shown[i].1.pos.y } else { shown[i].1.size.y + gap })
                .collect();
            let room = rooms[k];
            let top = shown[k].1.pos.y;
            let bottom = shown.last().map_or(top + room, |s| s.1.pos.y) + rooms[shown.len() - 1];
            let mut others = Vec::new();
            let mut offsets = std::collections::HashMap::new();
            for (i, (id, rect, _)) in shown.iter().enumerate() {
                if i == k { continue; }
                // out of the stack, the held one leaves a gap the ones after it close (their
                // offset holds them where they were until it moves)
                let after = i > k;
                others.push((*id, rect.pos.y - if after { room } else { 0.0 }, rooms[i]));
                let at = if after { room } else { 0.0 };
                offsets.insert(*id, (at, at));
            }
            cx.global::<Headers>().moving = Some(SectionMove {
                key, id: shown[k].0, grab: e.abs.y - top, pointer: e.abs.y, room, others,
                natural: bottom - room, lo: shown[0].1.pos.y, hi: bottom - room, slot: k, offsets, settle: None,
                frame: cx.new_next_frame(), last: None, redraw,
            });
            cx.redraw_area(redraw);
        }
        Event::MouseMove(e) => {
            let Some(m) = cx.global::<Headers>().moving.as_mut().filter(|m| m.key == key && m.settle.is_none()) else { return };
            m.pointer = e.abs.y;
            // it lands where its middle is among the others' middles
            let middle = m.held_top() + m.room * 0.5;
            let slot = m.others.iter().filter(|(_, top, room)| top + room * 0.5 < middle).count();
            if slot != m.slot {
                m.slot = slot;
                for (j, (id, _, _)) in m.others.iter().enumerate() {
                    if let Some(o) = m.offsets.get_mut(id) {
                        o.1 = if j >= slot { m.room } else { 0.0 };
                    }
                }
            }
            let (redraw, frame) = (m.redraw, m.frame);
            let _ = frame;
            cx.set_cursor(MouseCursor::Move);
            let next = cx.new_next_frame();
            if let Some(m) = cx.global::<Headers>().moving.as_mut() { m.frame = next; }
            cx.redraw_area(redraw);
        }
        Event::MouseUp(_) => {
            let Some(m) = cx.global::<Headers>().moving.as_mut().filter(|m| m.key == key && m.settle.is_none()) else { return };
            // the order kept: the others, the held one at its place
            let mut order: Vec<LiveId> = m.others.iter().map(|(id, _, _)| *id).collect();
            order.insert(m.slot.min(order.len()), m.id);
            // laid out in its place now: it settles there from where it was dropped, and the
            // ones after it, laid out a room lower, settle from where they were drawn
            let place = m.others.get(m.slot).map_or(m.natural, |(_, top, _)| *top);
            m.settle = Some(m.held_top() - place);
            for (j, (id, _, _)) in m.others.iter().enumerate() {
                if let Some(o) = m.offsets.get_mut(id) {
                    if j >= m.slot { o.0 -= m.room; }
                    o.1 = 0.0;
                }
            }
            let redraw = m.redraw;
            let next = cx.new_next_frame();
            if let Some(m) = cx.global::<Headers>().moving.as_mut() { m.frame = next; }
            cx.global::<SectionOrder>().0.insert(key, order);
            cx.redraw_area(redraw);
        }
        Event::NextFrame(ne) => {
            let Some(m) = cx.global::<Headers>().moving.as_mut().filter(|m| m.key == key && ne.set.contains(&m.frame)) else { return };
            let dt = m.last.map_or(1.0 / 60.0, |t| (ne.time - t).clamp(0.0, 0.05));
            m.last = Some(ne.time);
            // eased toward where each is going (about 80 ms to all but there)
            let k = 1.0 - (-dt / 0.035).exp();
            let mut moving = false;
            for (now, to) in m.offsets.values_mut() {
                *now += (*to - *now) * k;
                if (*to - *now).abs() < 0.5 { *now = *to; } else { moving = true; }
            }
            if let Some(s) = &mut m.settle {
                *s -= *s * k;
                if s.abs() < 0.5 { *s = 0.0; } else { moving = true; }
            }
            let redraw = m.redraw;
            let done = m.settle.is_some() && !moving;
            if done {
                cx.global::<Headers>().moving = None;
            } else if moving || m.settle.is_none() {
                let next = cx.new_next_frame();
                if let Some(m) = cx.global::<Headers>().moving.as_mut() { m.frame = next; }
            }
            cx.redraw_area(redraw);
        }
        _ => {}
    }
}

/// A press on a section's header, dragged on: every header the pointer then reaches takes the
/// state the pressed one went to (`open`), once per header per drag, and so does every header
/// between it and the one set last (`last`), in their order on screen. Folding moves what is below
/// up (and a scrolled panel back down) between two moves, so a header can pass the pointer without
/// ever being under it; the order catches it. Only the state is kept, not the pressed section: it
/// can hold the panel handling the move (Parameters holds its groups), and is busy then.
struct FoldDrag {
    open: bool,
    done: Vec<WidgetUid>,
    last: WidgetUid,
}

/// The section headers' pointer behaviour beyond a click, for the PropSections in `sections`
/// (call from the panel's `handle_event` with every event, before the sections get it: a press
/// reads the state the section will leave):
/// - the header under the pointer lights its title in the hover ink (no lit ground);
/// - a press on a header dragged over others folds or unfolds them all with that one gesture.
/// O(sections log sections) per mouse event; other events return at once.
pub(crate) fn section_headers<'a>(cx: &mut Cx, event: &Event, sections: impl IntoIterator<Item = &'a WidgetRef>) {
    let at = match event {
        Event::MouseMove(e) => Some(e.abs),
        Event::MouseDown(e) if e.button.is_primary() => Some(e.abs),
        Event::MouseUp(_) => {
            cx.global::<Headers>().drag = None;
            return;
        }
        Event::MouseLeave(_) | Event::ClearHover => None,
        _ => return,
    };
    // the headers shown, in their order down the panel
    let all: Vec<WidgetRef> = sections.into_iter().cloned().collect();
    let mut heads: Vec<(WidgetRef, Rect)> = all
        .iter()
        .filter_map(|section| {
            let area = section.widget(cx, ids!(hdr)).area();
            area.is_valid(cx).then(|| (section.clone(), area.rect(cx)))
        })
        .collect();
    heads.sort_by(|a, b| a.1.pos.y.total_cmp(&b.1.pos.y));
    let under = at.and_then(|p| heads.iter().position(|(_, r)| r.contains(p)));
    match (event, under) {
        // a press starts a drag from this header: the section toggles itself on the press, after
        // this, so the drag's state is the one it is leaving, turned
        (Event::MouseDown(e), Some(h)) if e.handled.get().is_empty() => {
            let (section, _) = &heads[h];
            let open = !section.as_prop_section().is_open(cx);
            let uid = section.widget_uid();
            cx.global::<Headers>().drag = Some(FoldDrag { open, done: vec![uid], last: uid });
        }
        // dragged onto a header: it and every header between it and the last one set take the
        // drag's state (the last one may be another panel's: then this one alone)
        (Event::MouseMove(_), Some(h)) => {
            if let Some(drag) = &cx.global::<Headers>().drag {
                let (open, last) = (drag.open, drag.last);
                let from = heads.iter().position(|(s, _)| s.widget_uid() == last).unwrap_or(h);
                for i in from.min(h)..=from.max(h) {
                    let (section, _) = &heads[i];
                    let uid = section.widget_uid();
                    let Some(drag) = &mut cx.global::<Headers>().drag else { break };
                    if drag.done.contains(&uid) { continue; }
                    drag.done.push(uid);
                    if section.as_prop_section().is_open(cx) != open {
                        section.as_prop_section().set_is_open(cx, open, Animate::Yes);
                        cx.widget_action(uid, if open { FoldHeaderAction::Opening } else { FoldHeaderAction::Closing });
                    }
                }
                if let Some(drag) = &mut cx.global::<Headers>().drag { drag.last = heads[h].0.widget_uid(); }
            }
        }
        _ => {}
    }
    // the title under the pointer lit, the others put back (a hidden one too)
    let hot = under.map(|h| heads[h].0.widget_uid());
    for section in &all {
        let want = hot == Some(section.widget_uid());
        let mut title = section.widget(cx, ids!(hdr.title));
        let uid = title.widget_uid();
        let lit = cx.global::<Headers>().lit.contains(&uid);
        if want == lit { continue; }
        let lit = &mut cx.global::<Headers>().lit;
        if want { lit.push(uid); } else { lit.retain(|u| *u != uid); }
        script_apply_eval!(cx, title, {draw_text +: {color: if #(want) {mod.theme.color_text_hover} else {mod.theme.color_text}}});
        title.redraw(cx);
    }
}

/// The gap at the top of an editor's body, under its toolbar ([`FabScroll`]'s and every body's).
pub const PANEL_TOP: f64 = 8.0;

/// The shadow under an editor's toolbar while its content is scrolled down: how deep it is, and
/// how far the content must be scrolled for it to reach full strength.
pub(crate) const SCROLL_SHADE_H: f64 = 10.0;
pub(crate) const SCROLL_SHADE_RAMP: f64 = 24.0;

/// A list's shadow along the top of `rect` (a `PortalList`'s), `scrolled` points down it (its
/// first row's index and offset: past the first row is all the way): the shade `FabScroll` draws
/// for a scrolled body, none at the top. Call after the list draws, so it lies over the rows.
pub(crate) fn draw_scroll_shade(cx: &mut Cx2d, draw: &mut DrawQuad, rect: Rect, first_id: usize, first_scroll: f64) {
    let scrolled = if first_id > 0 { SCROLL_SHADE_RAMP } else { (-first_scroll).max(0.0) };
    let weight = (scrolled / SCROLL_SHADE_RAMP).clamp(0.0, 1.0) as f32;
    if weight <= 0.0 || rect.size.x <= 0.0 {
        return;
    }
    draw.set_dyn_instance(cx, live_id!(weight), &[weight]);
    draw.draw_abs(cx, Rect { pos: rect.pos, size: dvec2(rect.size.x, SCROLL_SHADE_H) });
}

/// What each editor's filter hid (by the editor's key), to show again when it no longer matches
/// out: only what the filter hid comes back, never what the editor hid itself.
#[derive(Default)]
struct Filtered(std::collections::HashMap<LiveId, Vec<WidgetRef>>);

/// Filter editor `key`'s `sections` by `query` (its toolbar's search): a section whose title
/// holds every word shows whole; else each row (a box with a `label` or `name` label) shows only
/// when its name holds every word, a parameter panel filters its own drawn rows
/// ([`crate::ui::param_panel::FabParamPanel::set_filter`]), and a section (or a group in it) with
/// no row left hides. An
/// empty query shows all it hid. Call in the editor's `draw_walk` after it set its own
/// visibilities. Visibility is touched only where it changes. O(widgets in the sections).
pub(crate) fn filter_sections(cx: &mut Cx, key: LiveId, query: &str, sections: &[WidgetRef]) {
    let words: Vec<String> = query.split_whitespace().map(|w| w.to_lowercase()).collect();
    let before = cx.global::<Filtered>().0.remove(&key).unwrap_or_default();
    let was = |w: &WidgetRef| before.iter().any(|b| b.widget_uid() == w.widget_uid());
    let mut hide: Vec<WidgetRef> = Vec::new();
    // (also with no words: a panel that filters its own rows hears the search was cleared)
    for section in sections {
        filter_tree(cx, section, &words, false, &was, &mut hide);
    }
    let now = |w: &WidgetRef| hide.iter().any(|h| h.widget_uid() == w.widget_uid());
    for w in &before {
        if !now(w) && !w.visible() {
            w.set_visible(cx, true);
        }
    }
    for w in &hide {
        if w.visible() {
            w.set_visible(cx, false);
        }
    }
    cx.global::<Filtered>().0.insert(key, hide);
}

/// Whether anything under `w` matches `words` (every one), marking what to hide in `hide`: a
/// section (a PropSection) by its title, a row by its name; `whole` shows a matched section's
/// rows. Only what shows (or the filter itself hid, `was`) is marked.
fn filter_tree(cx: &mut Cx, w: &WidgetRef, words: &[String], whole: bool, was: &dyn Fn(&WidgetRef) -> bool, hide: &mut Vec<WidgetRef>) -> bool {
    let shown = |w: &WidgetRef| w.visible() || was(w);
    let holds = |text: &str| { let t = text.to_lowercase(); words.iter().all(|w| t.contains(w.as_str())) };
    // a parameter panel draws its rows itself: it filters them, and says whether any matched
    if let Some(mut panel) = w.borrow_mut::<crate::ui::param_panel::FabParamPanel>() {
        return panel.set_filter(cx, if whole { &[] } else { words }) || whole;
    }
    let mut children: Vec<(LiveId, WidgetRef)> = Vec::new();
    w.children(&mut |id, c| children.push((id, c)));
    if w.borrow::<PropSection>().is_some() {
        if !shown(w) { return false; }
        let title = w.widget(cx, ids!(hdr.title)).text();
        let whole = whole || holds(&title);
        let mut any = false;
        for (_, c) in &children {
            any |= filter_tree(cx, c, words, whole, was, hide);
        }
        if !(any || whole) { hide.push(w.clone()); }
        return any || whole;
    }
    // a row: a box whose name is its `label` or `name` label
    let name = children.iter().find(|(id, c)| (*id == live_id!(label) || *id == live_id!(name)) && c.borrow::<Label>().is_some()).map(|(_, c)| c.text());
    if let Some(name) = name {
        if !shown(w) { return false; }
        let keep = whole || holds(&name);
        if !keep { hide.push(w.clone()); }
        return keep;
    }
    let mut any = false;
    for (_, c) in &children {
        any |= filter_tree(cx, c, words, whole, was, hide);
    }
    any
}

/// How long a search takes to open or close, in seconds.
const SEARCH_OPEN_S: f64 = 0.16;

/// A toolbar's search that rests as its magnifier alone and opens into the field when pressed:
/// it grows leftward from the toolbar's right end (eased), the field taking the keyboard as it
/// opens; it closes again on a press outside it or when the field lets go of the keyboard,
/// either empty, or on Escape (which clears it). A search with words in it stays open. Its
/// field is `row.input`, as before.
/// A row of tools that scrolls sideways when it is clipped ([`FabToolScroll`] in the script):
/// the app turns a mouse drag's scrolling off everywhere (a drag selects, folds or moves), and
/// this row turns it back on for the presses that start in it, so a drag that starts on a
/// button scrolls the row (a click still clicks). The wheel scrolls it as well.
#[derive(Script, ScriptHook, Widget)]
pub struct FabToolScroll {
    #[deref]
    view: View,
    /// Its side padding covered with the editor ground, drawn over what scrolled under it.
    #[live]
    mask: bool,
    #[live]
    draw_mask: DrawQuad,
}

impl Widget for FabToolScroll {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        let ours = matches!(event, Event::MouseDown(e) if self.view.area().clipped_rect(cx).contains(e.abs));
        if ours {
            cx.set_mouse_drag_scrolling(true);
        }
        self.view.handle_event(cx, event, scope);
        if ours {
            cx.set_mouse_drag_scrolling(false);
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        while self.view.draw_walk(cx, scope, walk).is_step() {}
        if self.mask {
            let rect = self.view.area().rect(cx);
            let pad = self.view.layout.padding;
            // (above the foot line, which stays)
            let h = (rect.size.y - 1.0).max(0.0);
            self.draw_mask.new_draw_call(cx);
            for (x, w) in [(rect.pos.x, pad.left), (rect.pos.x + rect.size.x - pad.right, pad.right)] {
                if w > 0.0 {
                    self.draw_mask.draw_abs(cx, Rect { pos: dvec2(x, rect.pos.y), size: dvec2(w, h) });
                }
            }
        }
        DrawStep::done()
    }
}

#[derive(Script, ScriptHook, Widget)]
pub struct FabSearch {
    #[deref]
    view: View,
    /// How open it is (0 the magnifier alone, 1 the whole field), and where it is going.
    #[rust]
    open: f64,
    #[rust]
    to: f64,
    #[rust]
    frame: NextFrame,
    #[rust]
    last: Option<f64>,
}

impl FabSearch {
    fn input(&self, cx: &Cx) -> TextInputRef {
        self.view.text_input(cx, ids!(row.input))
    }

    fn go(&mut self, cx: &mut Cx, to: f64) {
        if self.to != to {
            self.to = to;
            self.last = None;
            self.frame = cx.new_next_frame();
        }
    }
}

impl Widget for FabSearch {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        if let Event::NextFrame(ne) = event {
            if ne.set.contains(&self.frame) {
                let dt = self.last.map_or(1.0 / 60.0, |t| (ne.time - t).clamp(0.0, 0.05));
                self.last = Some(ne.time);
                let step = dt / SEARCH_OPEN_S;
                self.open = if self.to > self.open { (self.open + step).min(self.to) } else { (self.open - step).max(self.to) };
                if self.open != self.to {
                    self.frame = cx.new_next_frame();
                }
                self.view.redraw(cx);
            }
        }
        // open, a press anywhere outside it lets its field go of the keyboard (unless what was
        // pressed took it already) and, the field empty, closes it; with words in it, it stays
        // open, so a filter in force always shows
        if let Event::MouseDown(e) = event {
            let area = self.view.area();
            if self.to > 0.5 && area.is_valid(cx) && !area.rect(cx).contains(e.abs) {
                let input = self.input(cx);
                if cx.has_key_focus(input.area()) {
                    cx.set_key_focus(Area::Empty);
                }
                if input.text().trim().is_empty() {
                    self.go(cx, 0.0);
                }
            }
        }
        // at rest, a press anywhere on it opens it and gives its field the keyboard
        if self.to < 0.5 {
            match event.hits(cx, self.view.area()) {
                Hit::FingerHoverIn(_) | Hit::FingerHoverOver(_) => cx.set_cursor(MouseCursor::Hand),
                Hit::FingerDown(fe) if fe.is_primary_hit() => {
                    self.go(cx, 1.0);
                    self.input(cx).set_key_focus(cx);
                    return;
                }
                _ => {}
            }
        }
        self.view.handle_event(cx, event, scope);
        if let Event::Actions(actions) = event {
            let input = self.input(cx);
            if input.escaped(actions) {
                input.set_text(cx, "");
                cx.widget_action(input.widget_uid(), TextInputAction::Changed(String::new()));
                self.go(cx, 0.0);
            } else if input.key_focus_lost(actions) && input.text().trim().is_empty() {
                self.go(cx, 0.0);
            }
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        // a search with words in it (set from outside, or kept) shows open
        if self.to < 0.5 && !self.input(cx).text().is_empty() {
            self.to = 1.0;
            self.open = 1.0;
        }
        let full = cx.peek_walk_turtle(walk).size.x.max(0.0);
        // at rest it is square: its height wide
        let rest = SEARCH_REST.min(full);
        let eased = 1.0 - (1.0 - self.open).powi(3);
        let width = rest + (full - rest) * eased;
        let input = self.input(cx);
        if input.visible() != (self.open > 0.0) {
            input.set_visible(cx, self.open > 0.0);
        }
        let mut walk = walk;
        walk.margin.left += full - width;
        walk.width = Size::Fixed(width);
        self.view.draw_walk(cx, scope, walk)
    }
}

/// A search at rest: its magnifier's square, the toolbar field's height.
const SEARCH_REST: f64 = 28.0;
