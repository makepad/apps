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
            color_disabled: fab.color_text_muted
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
        draw_text +: {
            color: fab.color_text_on_accent
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
            color_disabled: fab.color_text_muted
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
            color_disabled: fab.color_text_muted
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
            color_disabled: fab.color_text_muted
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
            active: {
                default: @off
                off: AnimatorState{
                    from: {all: Forward {duration: fab.anim_fast}}
                    apply: {
                        draw_bg: {active: 0.0}
                        draw_text: {active: 0.0}
                    }
                }
                on: AnimatorState{
                    from: {all: Forward {duration: 0.0}}
                    apply: {
                        draw_bg: {active: 1.0}
                        draw_text: {active: 1.0}
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
            color_active: fab.color_text_on_accent
            color_disabled: fab.color_text_muted
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

    mod.widgets.FabFloat = View{
        width: Fit
        height: Fit
        cursor: MouseCursor.Default
        show_bg: true
        draw_bg +: {
            pixel: fn() {
                let sdf = Sdf2d.viewport(self.pos * self.rect_size)
                sdf.box(0.5, 0.5, self.rect_size.x - 1.0, self.rect_size.y - 1.0, fab.radius_lg)
                sdf.fill_keep(vec4(fab.color_float.xyz, 0.94))
                sdf.stroke(fab.color_float_border, 1.0)
                return sdf.result
            }
        }
    }
    mod.widgets.FabTool = mod.widgets.FabIconToggle{
        width: 32
        height: 32
        padding: Inset{left: 8 right: 8 top: 8 bottom: 8}
    }
    mod.widgets.FabToolSep = View{ width: 20 height: 1 margin: Inset{left: 6 top: 4 bottom: 4} show_bg: true draw_bg +: { color: fab.color_border_light } }

    // =====================================================================
    // Checkbox — Fab's inset well with a white tick.
    // =====================================================================

    mod.widgets.FabCheckBox = CheckBox{
        height: theme.prop_row_height
        width: Fit
        align: Align{x: 0.0 y: 0.5}
        padding: Inset{left: 0 right: 0 top: 0 bottom: 0}
        margin: Inset{top: 0 bottom: 0 left: 0 right: 0}
        label_walk: Walk{ width: Fit height: Fit margin: Inset{left: 18 right: 0 top: 0 bottom: 0} }
        draw_bg +: {
            size: uniform(13.0)
            border_size: uniform(1.0)
            border_radius: uniform(fab.radius)
            color: fab.color_toggle_off
            color_hover: fab.color_input_hover
            color_down: fab.color_input_active
            color_active: fab.color_toggle_on
            color_focus: fab.color_toggle_off
            color_disabled: fab.color_editor_alt
            border_color: fab.color_border
            border_color_hover: fab.color_border_light
            border_color_down: fab.color_border
            border_color_active: fab.color_border
            border_color_focus: fab.color_focus_ring
            border_color_disabled: fab.color_border
            mark_color: vec4(0.0, 0.0, 0.0, 0.0)
            mark_color_hover: vec4(0.0, 0.0, 0.0, 0.0)
            mark_color_down: vec4(0.0, 0.0, 0.0, 0.0)
            mark_color_active: fab.color_toggle_mark
            mark_color_active_hover: fab.color_toggle_mark
            mark_color_focus: vec4(0.0, 0.0, 0.0, 0.0)
            mark_color_disabled: fab.color_text_muted
        }
        draw_text +: {
            ink_centered: true
            color: fab.color_text
            color_hover: fab.color_text_active
            color_active: fab.color_text
            color_disabled: fab.color_text_muted
            text_style: theme.font_regular{
                font_size: fab.font_size_ui
            }
        }
    }

    // =====================================================================
    // Text fields
    // =====================================================================

    // The row sets the height, as for the property kit's fields: a desktop sheet gives a
    // TextInput a field's height of its own, which stood the field taller than its row (a button
    // beside it, a tree row it renames in) and put the text below the row's middle.
    mod.widgets.FabInput = TextInput{
        height: theme.prop_row_height
        min_height: 0
        width: Fill
        padding: Inset{left: 6 right: 6 top: 0 bottom: 0}
        margin: Inset{top: 0 bottom: 0 left: 0 right: 0}
        empty_text: ""
        draw_bg +: {
            color: fab.color_input
            border_radius: fab.radius
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
    mod.widgets.FabSearch = View{
        width: Fill
        height: theme.prop_row_height
        flow: Overlay
        bg := View{
            width: Fill
            height: Fill
            show_bg: true
            draw_bg +: {
                pixel: fn() {
                    let sdf = Sdf2d.viewport(self.pos * self.rect_size)
                    // a FabButton's face: its box, kept its border_size back from the rect
                    sdf.box(1.0, 1.0, self.rect_size.x - 2.0, self.rect_size.y - 2.0, fab.radius)
                    sdf.fill(fab.color_border)
                    // the well, inside a one-point border
                    sdf.box(2.0, 2.0, self.rect_size.x - 4.0, self.rect_size.y - 4.0, max(fab.radius - 1.0, 0.0))
                    sdf.fill(fab.color_input)
                    return sdf.result
                }
            }
        }
        row := View{
            width: Fill
            height: Fill
            flow: Right
            align: Align{x: 0.0 y: 0.5}
            padding: Inset{left: 6 right: 4 top: 0 bottom: 0}
            spacing: 4
            glyph_slot := View{
                width: fab.icon_size_sm
                height: Fill
                align: Align{x: 0.5 y: 0.5}
                glyph := mod.widgets.FabIconMuted{
                    width: fab.icon_size_sm
                    height: fab.icon_size_sm
                    icon_walk: Walk{ width: fab.icon_size_sm height: fab.icon_size_sm }
                    draw_icon +: {
                        svg: crate_resource("self://resources/icons/search.svg")
                    }
                }
            }
            input := mod.widgets.WellInput{
                width: Fill
                height: Fill
                empty_text: "Search"
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
    mod.widgets.FabScroll = ScrollYView{
        width: Fill
        height: Fill
        flow: Down
        scroll_bars +: {
            show_scroll_x: false
            show_scroll_y: true
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
