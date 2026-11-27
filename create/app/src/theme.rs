//! Graphite colours and the controls shared by the workspace views.

use makepad_widgets::*;

script_mod! {
    use mod.prelude.widgets.*
    use mod.widgets.*

    mod.palette = {
        bg: #x16181b        chrome: #x121417    panel: #x1b1e22
        panel2: #x22262b    card: #x202429      sel: #x262b31
        line: #x2f343a      text: #xe3e5e8      muted: #x9aa2ab
        faint: #x6d757e     accent: #x8fb0c6    onaccent: #x10171c
        textsel: #x34608c   textsel_inactive: #x40464e
        good: #x8fb59a      warn: #xcdb06a      danger: #xd98b80
        you: #x8fb0c6       yourai: #xa9c3b5    peer: #xc8a07c
        peerai: #xb5a3c2    onseat: #x14171a    onpeer: #x1c140c
    }
    mod.prelude.workspace = {..mod.prelude.widgets, palette: mod.palette}
    let palette = mod.palette

    mod.widgets.SeatBadge = RoundedView{
        width: 20 height: 20 align: Center
        draw_bg +: {color: palette.you border_radius: 5}
        initial := Label{
            padding: 0 text: "Y"
            draw_text +: {color: palette.onseat text_style: theme.font_bold{font_size: 7.125}}
        }
    }
    mod.widgets.AiBadge = mod.widgets.SeatBadge{
        draw_bg +: {color: palette.yourai border_radius: 3}
        initial +: {text: "AI"}
    }
    mod.widgets.PeerBadge = mod.widgets.SeatBadge{
        draw_bg.color: palette.peer
        initial +: {text: "P" draw_text.color: palette.onpeer}
    }
    mod.widgets.PeerAiBadge = mod.widgets.AiBadge{draw_bg.color: palette.peerai}
    mod.widgets.PresenceDot = RoundedView{
        width: 6 height: 6 draw_bg +: {border_radius: 2 color: palette.faint}
    }
    mod.widgets.PanelLine = SolidView{width: Fill height: 1 draw_bg.color: palette.line}
    mod.widgets.PanelButton = ButtonFlat{
        margin: 0 padding: Inset{left: 10 right: 10 top: 6 bottom: 6}
        draw_text +: {
            color: palette.text color_hover: palette.text color_down: palette.text
            color_focus: palette.text color_disabled: palette.faint
            text_style: theme.font_bold{font_size: 9}
        }
        draw_bg +: {
            color: palette.panel2 color_hover: palette.sel color_down: palette.card
            color_focus: palette.sel color_disabled: palette.panel
            color_2_hover: palette.sel color_2_down: palette.card
            color_2_focus: palette.sel color_2_disabled: palette.panel
            border_radius: 4 border_size: 1
            border_color: palette.line border_color_hover: palette.accent border_color_down: palette.line
            border_color_focus: palette.accent border_color_disabled: palette.line
            border_color_2_hover: palette.accent border_color_2_down: palette.line
            border_color_2_focus: palette.accent border_color_2_disabled: palette.line
        }
    }
    mod.widgets.PanelInput = TextInputFlat{
        width: Fill margin: 0 padding: Inset{left: 8 right: 8 top: 6 bottom: 6}
        draw_text +: {
            color: palette.text color_hover: palette.text color_focus: palette.text color_down: palette.text
            color_empty: palette.faint color_empty_hover: palette.muted color_empty_focus: palette.muted
            text_style: theme.font_regular{font_size: 9}
        }
        draw_bg +: {
            color: palette.card color_hover: palette.card color_focus: palette.card color_down: palette.card
            color_empty: palette.card color_2_empty: palette.card
            color_2_hover: palette.card color_2_focus: palette.card color_2_down: palette.card
            border_color: palette.line border_color_hover: palette.line border_color_focus: palette.accent
            border_color_empty: palette.line border_color_down: palette.accent
            border_color_2_hover: palette.line border_color_2_focus: palette.accent
            border_color_2_empty: palette.line border_color_2_down: palette.accent
            border_size: 1 border_radius: 4
        }
        draw_cursor.color: palette.accent
        // As on a desktop: only the field with key focus shows the accent selection; the others
        // keep theirs in gray.
        draw_selection +: {
            color: palette.textsel_inactive color_hover: palette.textsel_inactive
            color_focus: palette.textsel color_down: palette.textsel
            color_empty: palette.textsel_inactive color_disabled: palette.textsel_inactive
            color_2_hover: palette.textsel_inactive color_2_focus: palette.textsel
            color_2_down: palette.textsel color_2_empty: palette.textsel_inactive
            color_2_disabled: palette.textsel_inactive
        }
    }
    // Town's drop-down menus wear the app's look: the card and its line, the card radius, Town's
    // text and its selection colour for the row under the pointer. The chat's context menu
    // (`MenuLayer` in main.rs) uses the same colours. Every drop-down Town makes takes these.
    mod.widgets.PopupMenuItem = mod.widgets.PopupMenuItem{
        draw_text +: {
            color: palette.text color_hover: palette.text color_active: palette.text
            color_disabled: palette.faint
            text_style: theme.font_body_m
        }
        draw_bg +: {
            color: #0000 color_hover: palette.sel color_active: palette.sel color_disabled: #0000
            border_color: #0000 border_color_hover: #0000 border_color_active: #0000 border_color_disabled: #0000
            border_color_2_hover: #0000 border_color_2_active: #0000 border_color_2_disabled: #0000
            border_radius: 4.0
            mark_color_active: palette.accent mark_color_disabled: palette.faint
        }
    }
    mod.widgets.PopupMenu = mod.widgets.PopupMenu{
        menu_item: mod.widgets.PopupMenuItem{}
        draw_bg +: {color: palette.card border_color: palette.line border_size: 1.0 border_radius: 6.0}
    }
    mod.widgets.DropDown = mod.widgets.DropDown{popup_menu: mod.widgets.PopupMenu{}}
}
