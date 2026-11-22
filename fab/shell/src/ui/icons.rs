//! Icons are **our own SVG files** in `fab/shell/resources/icons/`, named
//! literally where they are used (`crate_resource("self://resources/icons/…")`)
//! and rendered through the stock `Icon` widget (`DrawSvg`, tessellated onto
//! one quad, tinted by `draw_icon.color`). Never a bitmap.
//!
//! Style rules for the set:
//! * `viewBox 0 0 16 16`, drawn on the 16 px grid, rendered at 16 px (rows,
//!   tabs) or 20 px (toolbar); optical center, 1 px inset all round.
//! * strokes `1.25` (line glyphs) / `1.5` (emphasis), round caps + joins,
//!   `fill="#000"` for solid glyphs — the SVG color is irrelevant, the widget
//!   tints with `fab.color_text` / `color_text_active` / `color_accent`.
//! * corner radius `1` on rects, no gradients, no text, ≤ 3 sub-shapes.
//!
//! `DrawSvg` derefs to `DrawVector`: never place an icon under a parent that
//! aligns to the far edge (`align: Align{x: 1.0}` / `{y: 1.0}`) — the deferred
//! turtle shift displaces vector geometry. Use `Filler{}` to push instead.

use makepad_widgets_core::*;

script_mod! {
    use mod.prelude.fab.*
    use mod.widgets.*

    // DrawVector (DrawSvg) must not sit under an aligning parent — deferred
    // turtle alignment is why chevrons used to ride high. The icon fills its
    // own box; the *parent row* centres that box with padding, never align.
    mod.widgets.FabIcon = Icon{
        width: fab.icon_size
        height: fab.icon_size
        align: Align{x: 0.0 y: 0.0}
        icon_walk: Walk{
            width: fab.icon_size
            height: fab.icon_size
        }
        draw_icon +: {
            color: fab.color_text
        }
    }

    mod.widgets.FabIconMuted = mod.widgets.FabIcon{
        draw_icon +: {
            color: fab.color_text_muted
        }
    }

    mod.widgets.FabIconSmall = mod.widgets.FabIcon{
        width: fab.icon_size_sm
        height: fab.icon_size_sm
        icon_walk: Walk{
            width: fab.icon_size_sm
            height: fab.icon_size_sm
        }
        draw_icon +: {
            color: fab.color_text_dim
        }
    }
}
