//! The Fab theme: one token table every module's `script_mod!` reaches
//! through `use mod.prelude.fab.*` as `fab.<token>`. Desktop appearance, Fab
//! density. Nothing in the app hardcodes a color, size or duration that has a
//! token here.
//!
//! Registered before every other module (`fab::script_mod`), so
//! `mod.prelude.fab` (= the widgets prelude + `fab:` alias) is always in
//! scope for them.
//!
//! Surfaces and control geometry follow the shared desktop style; viewport
//! content and semantic axis/status colors remain local to Fab.
//!
//! The surfaces are the theme's opaque surface ladder (`color_surface_*`),
//! which every sheet re-derives from its own grounds: the shell ground the
//! docked panels and lists show on the container rung, the tab bars,
//! caption and editor bars one rung up. The words are measured, not
//! trusted: the theme's body, second and disabled inks are each moved toward
//! the plain end just far enough to read on every ground Fab lays text on
//! ([`Inks`]). A sheet chose its inks for its own flat page, and the plain
//! base theme's second and disabled inks are white at a few percent, made for
//! layering, which is what turned the feature kinds, counts and inactive tabs
//! into grey on grey.

use makepad_widgets_core::desktop_style::{self, DesktopStyle, StyleSheet};
use makepad_widgets_core::theme_tokens::{mix_rgba, over, reads_on, READABLE};
use makepad_widgets_core::*;

/// Body text, the second voice (kinds, counts, captions, inactive tabs) and
/// the quietest one (hints, shortcuts, chevrons, disabled controls), as
/// `0xRRGGBBAA`: the theme's own inks where they already read, else moved
/// toward white or black until they do. Body and second inks are held to
/// `READABLE` (WCAG AA, 4.5:1) on each ground below, the quietest to `QUIET`
/// (3:1), so it reads as a step back; they stay apart because each starts
/// from its own theme ink.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Inks {
    pub text: u32,
    pub dim: u32,
    pub muted: u32,
}

impl Inks {
    /// The grounds Fab draws words on: the panel and list, the tab bars and
    /// caption, the input wells and the selected row.
    pub const GROUNDS: [&'static str; 4] = [
        "color_surface_container",
        "color_surface_container_high",
        "color_inset",
        "color_bg_highlight",
    ];

    /// Read off the theme as it stands now (after the sheet, if any, went on).
    pub fn read(vm: &mut ScriptVm) -> Self {
        let theme = vm.module(id!(theme));
        let read = |key: &str| vm.value(theme, LiveId::from_str(key)).as_color();
        let panel = read("color_surface_container").unwrap_or(0x28282aff) | 0xff;
        let grounds: Vec<u32> = Self::GROUNDS.iter().map(|key| read(key).map_or(panel, |c| over(panel, c))).collect();
        let text = read("color_text").unwrap_or(0xffffffff);
        let dim = read("color_on_surface_variant").unwrap_or(text);
        let muted = read("color_text_disabled").unwrap_or(dim);
        let fitted = Self::fit(&grounds, text, dim, muted);
        // Fab's own tones (`toned`) are chosen values: kept as they are, not lifted to the contrast
        // floors as a sheet's own inks would be.
        match (Some(text) == hex_rgba(TEXT_REST), hex_rgba(TEXT_DIM)) {
            (true, Some(dim)) => Self { text, dim, muted: dim },
            _ => fitted,
        }
    }

    /// The inks for these (opaque) grounds.
    pub fn fit(grounds: &[u32], text: u32, dim: u32, muted: u32) -> Self {
        Self {
            text: ink_reading(grounds, text, READABLE),
            dim: ink_reading(grounds, dim, READABLE),
            muted: ink_reading(grounds, muted, QUIET),
        }
    }
}

/// The quietest ink's floor: 3:1 (WCAG's for large text and for the parts of controls), under
/// the body and second inks' 4.5:1, so the words that only qualify another (a feature's kind, a
/// part's dimensions, a field's placeholder) stand a step back from the second voice instead of
/// being lifted to it.
const QUIET: f64 = 3.0;

/// `ink`, or the nearest step from it toward the plain end (white on a dark
/// panel, black on a light one) that reads at `need` on every ground.
fn ink_reading(grounds: &[u32], ink: u32, need: f64) -> u32 {
    let panel = grounds[0];
    let end = if reads_on(panel, 0xffffffff) >= reads_on(panel, 0x000000ff) { 0xffffffff } else { 0x000000ff };
    (0..=20)
        .map(|step| mix_rgba(ink, end, step as f64 / 20.0))
        .find(|c| grounds.iter().all(|g| reads_on(*g, *c) >= need))
        .unwrap_or(end)
}

/// Fab's words on a dark sheet: at rest a step under white (`TEXT_REST`), the dimmed ones at
/// `TEXT_DIM`, so plain white is left for what the pointer is on: every text and label ink's
/// hover and press state lights to `TEXT_HOT`. Appended to the host's sheet before Fab wears it, so the library's own widgets
/// (buttons, fields, tabs) take them as Fab's do. A light sheet's dark inks are left as they are.
const TEXT_REST: &str = "#aaaaaa";
/// Every dimmed word (labels, counts, kinds, dimensions, placeholders, inactive tabs).
const TEXT_DIM: &str = "#666666";
const TEXT_HOT: &str = "#ffffff";

/// How much brighter the editors' toolbars (and the shown tab over each) are than the sheet's
/// `color_surface_container`.
const TOOLBAR_LIFT: f64 = 1.15;
/// How much brighter the panels' ground is: a step under their toolbars, which it parts from.
const PANEL_LIFT: f64 = 1.08;

/// `0xRRGGBBAA` with each colour channel times `by` (capped at full), its alpha kept.
fn brighter(c: u32, by: f64) -> u32 {
    let ch = |shift: u32| ((((c >> shift) & 0xff) as f64 * by).round().min(255.0) as u32) << shift;
    ch(24) | ch(16) | ch(8) | (c & 0xff)
}

/// `"#rrggbb"` as `0xRRGGBBAA` (opaque).
fn hex_rgba(hex: &str) -> Option<u32> {
    u32::from_str_radix(hex.strip_prefix('#')?, 16).ok().map(|rgb| (rgb << 8) | 0xff)
}

fn toned(mut sheet: StyleSheet) -> StyleSheet {
    let light_ink = sheet.theme_color("color_text").is_some_and(|c| reads_on(0x000000ff, c) > reads_on(0xffffffff, c));
    if light_ink {
        let mut tones = String::from("\n// Fab: resting text under white, white on hover and press (fab::theme::toned)\n");
        for ink in ["color_text", "color_label_inner", "color_label_outer", "color_label"] {
            for (state, tone) in [("", TEXT_REST), ("_focus", TEXT_REST), ("_active", TEXT_REST), ("_hover", TEXT_HOT), ("_down", TEXT_HOT)] {
                tones.push_str(&format!("mod.theme.{ink}{state} = {tone}\n"));
            }
        }
        tones.push_str(&format!("mod.theme.color_text_on_accent = {TEXT_REST}\n"));
        for ink in ["color_on_surface_variant", "color_text_meta", "color_text_disabled", "color_label_inner_disabled", "color_label_outer_disabled", "color_label_disabled"] {
            tones.push_str(&format!("mod.theme.{ink} = {TEXT_DIM}\n"));
        }
        // The property kit's fields (number fields and sliders, dropdowns, segmented choices) a
        // tad darker than the library's light grey (#3d3d3f on Fab's #2e2e30 panel): resting at
        // #373739, and lighting to that old grey under the pointer.
        tones.push_str("mod.theme.prop_field = #x373739\nmod.theme.prop_field_hover = #x3d3d3f\n");
        sheet.theme.push_str(&tones);
    }
    sheet
}

/// The sheet Fab put on itself standalone, so an appearance change moves it
/// and nothing else (a user's or the window system's sheet stands).
static WORN_BY_FAB: std::sync::Mutex<Option<&'static str>> = std::sync::Mutex::new(None);

fn host_family(cx: &Cx) -> DesktopStyle {
    match cx.os_type() {
        OsType::Macos => DesktopStyle::Macos,
        OsType::Windows => DesktopStyle::Windows,
        OsType::Ios(_) => DesktopStyle::Ios,
        OsType::Android(_) => DesktopStyle::Android,
        _ => DesktopStyle::Omarchy,
    }
}

/// Standalone, Fab wears its host's desktop sheet in the host's appearance
/// (macOS or macOS dark on a Mac; dark where the host cannot say). Under the
/// window manager the WM hands its sheet down and this does nothing; in
/// Studio neither. Without it a Mac ran Fab under the library's plain base
/// theme: a mid-grey ground and inks that are tints for layering. Call before
/// the widgets register; later calls (theme reloads) leave what is worn.
pub fn wear_host_sheet(vm: &mut ScriptVm) {
    if vm.cx().in_host() || desktop_style::current_name(vm).is_some() {
        return;
    }
    let dark = makepad_platform::host_dark().unwrap_or(true);
    let entry = host_family(vm.cx()).sheet(dark);
    desktop_style::install(vm, toned(StyleSheet::load(entry)));
    *WORN_BY_FAB.lock().unwrap() = Some(entry.id);
}

/// Follow a change of the host's light or dark appearance (call on startup
/// and when a window gets focus): only the sheet Fab put on itself moves.
pub fn follow_host_appearance(cx: &mut Cx) {
    let Some(worn) = *WORN_BY_FAB.lock().unwrap() else { return };
    let Some(dark) = makepad_platform::host_dark() else { return };
    let Some(entry) = desktop_style::find(worn) else { return };
    let want = entry.with_appearance(dark);
    if want.id == worn || cx.with_vm(desktop_style::current_name).as_deref() != Some(worn) {
        return;
    }
    cx.with_vm(|vm| desktop_style::install(vm, toned(StyleSheet::load(want))));
    *WORN_BY_FAB.lock().unwrap() = Some(want.id);
    cx.request_style_reload();
}

pub fn script_mod(vm: &mut ScriptVm) {
    let inks = Inks::read(vm);
    let text = ScriptValue::from_color(inks.text);
    let dim = ScriptValue::from_color(inks.dim);
    let muted = ScriptValue::from_color(inks.muted);
    // the panels' ground (the lists and editors under their tabs) a step above the sheet's own
    // rung: its colour, `PANEL_LIFT` up
    let (panel, toolbar) = {
        let theme = vm.module(id!(theme));
        // (nil when the sheet has no such rung: the plain rung then, below)
        let rung = vm.value(theme, id!(color_surface_container)).as_color();
        let lift = |by| rung.map_or(ScriptValue::NIL, |c| ScriptValue::from_color(brighter(c, by)));
        (lift(PANEL_LIFT), lift(TOOLBAR_LIFT))
    };
    script_eval!(vm, {
        use mod.prelude.widgets.*

        mod.fab = {
            // ---- surfaces ----
            // The shell ground the docked panels and lists show (the sheet's
            // container rung, `PANEL_LIFT` brighter), then the tab bars,
            // caption and editor bars one rung up (opaque ladder).
            color_area: #(panel) ?? theme.color_surface_container
            color_editor: #(panel) ?? theme.color_surface_container
            color_editor_alt: mix(#(panel) ?? theme.color_surface_container, theme.color_surface_container_high, 0.5)
            color_header: theme.color_surface_container_high
            // the editors' toolbars (under their area's tabs) and the shown tab over each: a
            // shade above the panel ground
            color_toolbar: #(toolbar) ?? theme.color_surface_container
            color_panel: #(panel) ?? theme.color_surface_container
            color_panel_sub: theme.color_inset
            color_popover: theme.color_surface_container_high
            color_popover_border: theme.color_bevel_outset_2
            color_topbar: theme.color_surface_container_high
            color_statusbar: theme.color_surface_container_high
            color_border: theme.color_bevel_inset_2
            color_border_light: theme.color_bevel_outset_1
            color_row_hover: theme.color_outset_hover
            // a foldable section's header: a shade under the panel, never a lift
            color_section: #x00000020
            color_input: theme.color_inset
            color_input_hover: theme.color_inset_hover
            color_input_active: theme.color_inset_active
            color_button: theme.color_outset
            color_button_hover: theme.color_outset_hover
            color_button_down: theme.color_outset_down
            color_button_active: theme.color_ctrl_selected

            // ---- text ----
            // Measured against every ground they are drawn on (`Inks`): text
            // and dim at 4.5:1, muted at 3:1. `dim` is the second voice
            // (labels, counts, captions, inactive tabs); `muted` the quietest
            // (feature kinds, dimensions, placeholders, hints, shortcuts,
            // chevrons, disabled controls).
            color_text: #(text)
            color_text_dim: #(dim)
            color_text_muted: #(muted)
            color_text_active: #(text)
            // what the pointer is on lights to the sheet's hover ink (white over Fab's resting ink)
            color_text_hover: theme.color_text_hover
            color_text_header: #(text)
            color_text_on_accent: theme.color_text_on_accent

            // ---- accents ----
            color_accent: theme.color_focus
            color_accent_hover: theme.color_focus
            color_accent_dim: theme.color_bg_highlight
            color_selection_bg: theme.color_bg_highlight
            color_focus_ring: theme.color_focus
            color_warning: #xe0a020
            color_error: #xe04040
            color_ok: #x5cb85c

            // ---- viewport ----
            color_vp_bg_top: #x3f3f3f
            color_vp_bg_bottom: #x2b2b2b
            color_vp_grid: #x505050
            color_vp_grid_major: #x5a5a5a
            color_vp_axis_x: #xff3352
            color_vp_axis_y: #x8bdc00
            color_vp_axis_z: #x2890ff
            color_vp_select: #xe96a2b
            color_vp_select_dim: #xa5461d
            color_vp_hover: #xffffff
            color_vp_measure: #xffd05a
            color_vp_section: #x39c5ff
            color_vp_cap: #x8a8a8a

            // ---- chrome extras ----
            // Floating chrome over the view (the tool strip, the cards, chips).
            color_float: theme.color_surface_container
            color_float_border: #x333333
            // the shadow along a scrolled panel's top edge, under its toolbar
            color_scroll_shade: #x0000004d
            // Checkboxes / toggles, Fab's inset well and its filled state.
            color_toggle_off: theme.color_inset
            color_toggle_on: theme.color_ctrl_selected
            color_toggle_mark: #xffffff
            // Drag-numeric field: inset well with an accent progress fill.
            color_num: theme.color_inset
            color_num_hover: theme.color_inset_hover
            color_num_fill: theme.color_bg_highlight
            color_num_arrow: #(text)
            // Menus.
            color_menu_row_hover: theme.color_bg_highlight
            color_menu_sep: theme.color_bevel_inset_2

            // ---- density (Fab at 1x) ----
            splitter_size: 3.0
            pad_1: 4.0
            // Corner radii in `Sdf2d.box` / `border_radius` units: every rounded
            // box in the kit draws its corner at TWICE the value it is given
            // (draw/src/shader/sdf.rs `box`: k = 2 * r). The theme's
            // corner_radius / container_corner_radius are in the same units and
            // a desktop sheet may set them large (macOS 6 / 12 -> 12 px and 24 px
            // drawn, iOS more), which made Fab's menus and popovers balloon.
            // Fab follows the sheet down to square, never past a modest cap:
            // controls at most 5 px drawn, menus / popovers / panels 7 px.
            radius: min(theme.corner_radius 2.5)
            radius_lg: min(theme.container_corner_radius 3.5)
            icon_size: theme.prop_icon_size
            icon_size_sm: 12.0
            // 10 px chevron on the icon grid, 4 px from the label — optical
            // match to the label's cap-height midpoint, not a vertical nudge.
            chevron_size: 10.0
            chevron_gap: 4.0
            gizmo_size: 96.0
            swatch_width: 46.0

            // ---- type (points; px = pt * 4/3) ----
            // The body size is the property kit's, so the panels, lists and bars read as one.
            font_size_ui: theme.prop_font_size
            font_size_small: 7.5
            font_size_header: 9.0
            font_size_title: 10.5

            // ---- motion (seconds), eases only, no bounce ----
            anim_fast: 0.10
        }

        mod.prelude.fab = {
            ..mod.prelude.widgets,
            fab: mod.fab
        }
        true
    });
}
