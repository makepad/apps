//! Graphite colours and the controls shared by the workspace views.

use makepad_widgets::*;

pub use crate::view::SEAT_SLOTS;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct SeatPalette {
    pub seats: [Vec4f; SEAT_SLOTS],
    pub peer_ai_default: Vec4f,
    pub ink_dark: Vec4f,
    pub ink_light: Vec4f,
}

impl SeatPalette {
    pub fn from_cx(cx: &mut Cx) -> Self {
        cx.with_vm(|vm| {
            let palette = vm.module(id!(palette));
            let mut color = |id: LiveId| {
                let value = vm.value(palette, id);
                Vec4f::script_from_value(vm, value)
            };
            Self {
                seats: [id!(seat_0), id!(seat_1), id!(seat_2), id!(seat_3),
                    id!(seat_4), id!(seat_5), id!(seat_6), id!(seat_7)].map(&mut color),
                peer_ai_default: color(id!(peerai)),
                ink_dark: color(id!(seat_ink_dark)),
                ink_light: color(id!(seat_ink_light)),
            }
        })
    }
}

fn luminance(color: Vec4f) -> f32 {
    let linear = |x: f32| if x <= 0.04045 { x / 12.92 } else { ((x + 0.055) / 1.055).powf(2.4) };
    0.2126 * linear(color.x) + 0.7152 * linear(color.y) + 0.0722 * linear(color.z)
}

fn contrast(a: f32, b: f32) -> f32 {
    (a.max(b) + 0.05) / (a.min(b) + 0.05)
}

pub fn wcag_contrast(a: Vec4f, b: Vec4f) -> f32 {
    contrast(luminance(a), luminance(b))
}

pub fn contrast_ink(bg: Vec4f, dark: Vec4f, light: Vec4f) -> Vec4f {
    let bg = luminance(bg);
    if contrast(bg, luminance(dark)) >= contrast(bg, luminance(light)) { dark } else { light }
}

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
        seat_0: mod.themes.dark.color_map_7    seat_1: mod.themes.dark.color_map_2
        seat_2: mod.themes.dark.color_map_4    seat_3: mod.themes.dark.color_map_3
        seat_4: mod.themes.dark.color_map_8    seat_5: mod.themes.dark.color_map_6
        seat_6: mod.themes.dark.color_map_10   seat_7: mod.themes.dark.color_map_9
        seat_ink_dark: #x14171a                seat_ink_light: theme.color_w
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

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::lists::{badge_colors, seat_icon};
    use crate::view::{Seat, SeatTone};

    pub(crate) fn check_seat_contrast() {
        let mut pool = test_cx::checkout_test_cx(|vm| { makepad_widgets::script_mod(vm); crate::script_mod(vm); });
        let cx = &mut *pool;
        let palette = SeatPalette::from_cx(cx);
        cx.with_vm(|vm| {
            let themes = vm.module(id!(themes));
            for (theme, ink) in [(id!(dark), palette.ink_light), (id!(light), palette.ink_dark)] {
                let theme = vm.value(themes, theme).as_object().unwrap();
                for (slot, role) in [id!(color_map_7), id!(color_map_2), id!(color_map_4), id!(color_map_3),
                    id!(color_map_8), id!(color_map_6), id!(color_map_10), id!(color_map_9)].into_iter().enumerate() {
                    let value = vm.value(theme, role);
                    assert!(value.as_color().is_some() || value.as_pod().is_some(), "missing seat colour {role}");
                    let fill = Vec4f::script_from_value(vm, value);
                    if ink == palette.ink_light { assert_eq!(palette.seats[slot], fill, "Graphite slot {slot}"); }
                    let chosen = contrast_ink(fill, palette.ink_dark, palette.ink_light);
                    assert_eq!(chosen, ink, "ink for {role}");
                    assert!(wcag_contrast(fill, chosen) >= 4.5, "contrast for {role}");
                }
            }
        });
    }

    #[test]
    fn seat_icons_reapply_a_local_fill_to_the_peer_and_their_ai() {
        let mut pool = test_cx::checkout_test_cx(|vm| { makepad_widgets::script_mod(vm); crate::script_mod(vm); });
        let cx = &mut *pool;
        let palette = SeatPalette::from_cx(cx);
        let mut icons = cx.with_vm(|vm| {
            let value = script_eval!(vm, { mod.widgets.SeatIcons{} });
            WidgetRef::script_from_value(vm, value)
        });
        let tone = Some(SeatTone(0));
        seat_icon(cx, &icons, Seat::Peer, "Ana", tone);
        assert_eq!(badge_colors(cx, &icons.widget(cx, ids!(peer))), (palette.seats[0], palette.ink_light));
        let light = cx.with_vm(|vm| {
            let value = vm.with_reload(|vm| script_eval!(vm, { mod.widgets.SeatIcons{seat_0: mod.themes.light.color_map_7} }));
            icons.script_apply(vm, &Apply::ScriptReapply, &mut Scope::empty(), value);
            let fill = script_eval!(vm, { mod.themes.light.color_map_7 });
            Vec4f::script_from_value(vm, fill)
        });
        for (seat, path) in [(Seat::Peer, ids!(peer)), (Seat::PeerAi, ids!(peer_ai))] {
            seat_icon(cx, &icons, seat, "Ana", tone);
            assert_eq!(badge_colors(cx, &icons.widget(cx, path)), (light, palette.ink_dark));
        }
        assert_eq!(icons.label(cx, ids!(peer_ai.initial)).text(), "AI");
        assert_eq!(SeatPalette::from_cx(cx), palette, "the local override leaves the Town palette intact");
    }
}
