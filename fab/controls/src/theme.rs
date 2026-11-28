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

use makepad_widgets_core::desktop_style::{self, DesktopStyle, FaceMode, StyleSheet};
use makepad_widgets_core::theme_tokens::{mix_rgb, mix_rgba, over, reads_on, READABLE};
use makepad_widgets_core::*;

/// Every host family [`wear_host_sheet`] may load standalone.
pub const HOST_FAMILIES: [DesktopStyle; 5] = [
    DesktopStyle::Macos,
    DesktopStyle::Windows,
    DesktopStyle::Ios,
    DesktopStyle::Android,
    DesktopStyle::Omarchy,
];

/// Which of Fab's lifted or derived chrome grounds were pulled back by the contrast bound
/// on this theme. Pure metadata on [`Grounds`]; [`script_mod`] logs once per theme resolve
/// when [`Self::any`] is true.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LiftClamps {
    pub panel: bool,
    pub toolbar: bool,
    pub header: bool,
    pub selection: bool,
    pub section: bool,
    pub field: bool,
}

impl LiftClamps {
    pub const fn any(&self) -> bool {
        self.panel || self.toolbar || self.header || self.selection || self.section || self.field
    }
}

/// Every opaque ground Fab lays chrome words or icons on, resolved from a theme.
/// Computed once here and shared by [`script_mod`], [`toned`] and the contrast guards.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Grounds {
    /// `theme.color_surface_container` (opaque).
    pub container: u32,
    /// `brighter(container, PANEL_LIFT)` bounded for `READABLE` (`color_area`, `color_editor`, `color_panel`).
    pub panel: u32,
    /// `brighter(container, TOOLBAR_LIFT)` bounded for `READABLE` (`color_toolbar`).
    pub toolbar: u32,
    /// `theme.color_surface_container_high` over `container` (`color_topbar`, `color_statusbar`, `color_popover`, `color_float`).
    pub high: u32,
    /// `brighter(high, TAB_LIFT)` bounded for `READABLE` (`color_header`).
    pub header: u32,
    /// `mix_rgb(panel, high, 0.5)` (`color_editor_alt`).
    pub editor_alt: u32,
    /// `mix_rgb(panel, 0xffffffff, SELECTION_MIX)` bounded for `READABLE` (`color_selection_bg`).
    pub selection: u32,
    /// Translucent section-header wash (`color_section`, nominal `#ffffff10`, alpha bounded for `READABLE`).
    pub section_wash: u32,
    /// `over(panel, section_wash)` (`color_section` composited over `panel`).
    pub section: u32,
    /// `theme.color_inset` over `panel` (`color_input`, `color_panel_sub`, `color_num`).
    pub inset: u32,
    /// Property-field fill bound to `fab.color_field` (`theme.prop_field`, pulled back toward
    /// `panel` or lower alpha only if `over(panel|toolbar, prop_field)` falls under `READABLE`).
    pub field: u32,
    /// `field` over `panel` (`FabInput`, `ValueInput`, `FabButton`, engine pill).
    pub prop_field_panel: u32,
    /// `field` over `toolbar` (`FabSearch`, toolbar `FabButton`).
    pub prop_field_toolbar: u32,
    /// Hovered/focused property-field fill bound to `fab.color_input_field_hover`
    /// (`theme.prop_field_hover`, contrast-bounded on `[panel, toolbar]` like `field`).
    pub field_hover: u32,
    /// `field_hover` over `panel`.
    pub prop_field_hover_panel: u32,
    /// `field_hover` over `toolbar`.
    pub prop_field_hover_toolbar: u32,
    /// `theme.color_bg_highlight` over `container` (hover/open highlight).
    pub highlight: u32,
    /// `theme.color_ctrl_selected` over `container` (active core `FabIconToggle`).
    pub ctrl_selected: u32,
    /// `theme.color_focus` over `container` (`FabButtonAccent`).
    pub accent: u32,
    /// `theme.prop_choice_on` over `panel` (active core `FabSegmentTab`, `TimeStrip`).
    pub choice_on_raw: u32,
    /// Raw `theme.color_outset_hover` (uncomposited, preserves alpha).
    pub button_hover_raw: u32,
    /// Raw `theme.color_outset_down` (uncomposited, preserves alpha).
    pub button_down_raw: u32,
    /// Pressed property-field face on a toned host.
    pub field_active_raw: u32,
    /// Which lifts or field fills were pulled back by the contrast bound.
    pub lift_clamps: LiftClamps,
}

impl Grounds {
    /// Resolve Fab's grounds from any theme token reader (VM, `ThemeValues`, `ThemeBlend`, or sheet).
    pub fn from_theme(read_color: impl Fn(&str) -> Option<u32>) -> Self {
        let container = read_color("color_surface_container").unwrap_or(0x28282aff) | 0xff;
        let end = plain_end(container);
        let (panel, _, panel_clamped) = bounded_brighter(container, PANEL_LIFT, end, READABLE);
        let (toolbar, _, toolbar_clamped) = bounded_brighter(container, TOOLBAR_LIFT, end, READABLE);
        let high = read_color("color_surface_container_high").map_or(container, |c| over(container, c));
        let (header, _, header_clamped) = bounded_brighter(high, TAB_LIFT, end, READABLE);
        let editor_alt = mix_rgb(panel, high, 0.5);
        let (selection, _, selection_clamped) =
            bounded_mix(panel, 0xffffffff, SELECTION_MIX, end, READABLE);
        let (section_wash, section_clamped) =
            bounded_wash(&[panel], 0xffffff00 | SECTION_WASH_ALPHA as u32, end, READABLE);
        let section = over(panel, section_wash);
        let inset = read_color("color_inset").map_or(panel, |c| over(panel, c));
        let (field, field_clamped) = match read_color("prop_field") {
            Some(raw) if (raw & 0xff) == 0xff => {
                let (c, _, clamped) = bounded_mix(panel, raw, 1.0, end, READABLE);
                (c, clamped)
            }
            Some(raw) => bounded_wash(&[panel, toolbar], raw, end, READABLE),
            None => (panel, false),
        };
        let prop_field_panel = over(panel, field);
        let prop_field_toolbar = over(toolbar, field);
        let (field_hover, _) = match read_color("prop_field_hover") {
            Some(raw) if (raw & 0xff) == 0xff => {
                let (c, _, clamped) = bounded_mix(panel, raw, 1.0, end, READABLE);
                (c, clamped)
            }
            Some(raw) => bounded_wash(&[panel, toolbar], raw, end, READABLE),
            None => (field, false),
        };
        let prop_field_hover_panel = over(panel, field_hover);
        let prop_field_hover_toolbar = over(toolbar, field_hover);
        let highlight = read_color("color_bg_highlight").map_or(container, |c| over(container, c));
        let ctrl_selected = read_color("color_ctrl_selected").map_or(container, |c| over(container, c));
        let accent = read_color("color_focus").map_or(ctrl_selected, |c| over(container, c));
        let choice_on_raw = read_color("prop_choice_on").map_or(panel, |c| over(panel, c));
        let button_hover_raw = read_color("color_outset_hover").unwrap_or(field_hover);
        let button_down_raw = read_color("color_outset_down").unwrap_or(field);
        let field_active_raw = read_color("prop_field_active").unwrap_or(field);
        Self {
            container,
            panel,
            toolbar,
            high,
            header,
            editor_alt,
            selection,
            section_wash,
            section,
            inset,
            field,
            prop_field_panel,
            prop_field_toolbar,
            field_hover,
            prop_field_hover_panel,
            prop_field_hover_toolbar,
            ctrl_selected,
            accent,
            choice_on_raw,
            button_hover_raw,
            button_down_raw,
            field_active_raw,
            highlight,
            lift_clamps: LiftClamps {
                panel: panel_clamped,
                toolbar: toolbar_clamped,
                header: header_clamped,
                selection: selection_clamped,
                section: section_clamped,
                field: field_clamped,
            },
        }
    }

    /// Every neutral chrome ground outside the inset wells (`color_input` / `color_panel_sub`),
    /// where Fab lays `color_text` and `color_text_dim`. Starts with `container` so
    /// `plain_end(grounds[0])` matches the sheet's chrome polarity.
    pub fn chrome_surfaces(&self) -> [(&'static str, u32); 10] {
        [
            ("container", self.container),
            ("panel", self.panel),
            ("toolbar", self.toolbar),
            ("high", self.high),
            ("header", self.header),
            ("editor_alt", self.editor_alt),
            ("selection", self.selection),
            ("section", self.section),
            ("prop_field(panel)", self.prop_field_panel),
            ("prop_field(toolbar)", self.prop_field_toolbar),
        ]
    }

    /// Every non-inset chrome ground where Fab draws muted (`3:1`) icons, chevrons, dots, or
    /// disabled marks (`FabIconMuted`, `FabDropdownButton`, `FabSearch`, `FabStatusPill`,
    /// `FabMenuLayer`, `TimeStrip`). Starts with `panel` for polarity.
    pub fn graphics_surfaces(&self) -> [(&'static str, u32); 6] {
        [
            ("panel", self.panel),
            ("toolbar", self.toolbar),
            ("high", self.high),
            ("section", self.section),
            ("prop_field(panel)", self.prop_field_panel),
            ("prop_field(toolbar)", self.prop_field_toolbar),
        ]
    }

    /// Every neutral chrome ground Fab lays resting words on, including `inset` (used by
    /// `toned_inks` on host dark sheets and extremal-vertex selection in `sheet_contrast`).
    pub fn neutral_chrome(&self) -> [(&'static str, u32); 11] {
        [
            ("container", self.container),
            ("panel", self.panel),
            ("toolbar", self.toolbar),
            ("high", self.high),
            ("header", self.header),
            ("editor_alt", self.editor_alt),
            ("selection", self.selection),
            ("section", self.section),
            ("inset", self.inset),
            ("prop_field(panel)", self.prop_field_panel),
            ("prop_field(toolbar)", self.prop_field_toolbar),
        ]
    }

    /// True when `inset` shares polarity (white vs. black plain end) with `container`
    /// (48 of 51 catalogue sheets, all 5 host dark sheets, all 3 base themes, and all
    /// same-polarity blends). False on `safety-yellow`, `mecha-light`, `knitted`, and
    /// mixed-polarity blends whose averaged `inset` crosses the midpoint.
    pub fn inset_matches_chrome(&self) -> bool {
        plain_end(self.inset) == plain_end(self.container)
    }

    /// Fill bound to `fab.color_input_field` for `FabInput` and `FabSearch.row.input`:
    /// `field` (`prop_field`) when its polarity agrees with `inset`, else `inset` so
    /// inverted-inset blends (which run core `TextInputFlat` on `self.color`) stay matched
    /// to `well_*` inks.
    pub fn input_fill(&self) -> u32 {
        if self.inset_matches_chrome() { self.field } else { self.inset }
    }

    /// Hover/focus fill bound to `fab.color_input_field_hover` (`field_hover` on
    /// same-polarity sheets, `inset` on inverted-inset sheets/blends).
    pub fn input_fill_hover(&self) -> u32 {
        if self.inset_matches_chrome() { self.field_hover } else { self.inset }
    }
}

/// Fab's resolved chrome grounds and text inks for one theme.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Palette {
    pub grounds: Grounds,
    pub inks: Inks,
    /// True when `theme.fab_toned` was present (`toned()` host dark sheet).
    pub toned: bool,
    /// Active segment pill face (`theme.prop_choice_on` on `toned`, contrast-bounded on `!toned`).
    pub choice_on: u32,
    /// Hover face for `FabDropdownButton`, `ElecControlsStrip`, `TimeStrip`, core flat buttons.
    pub button_hover: u32,
    /// Pressed face for core flat buttons (`FabFlatButton`, `FabIconButton`).
    pub button_down: u32,
    /// Pressed face for `FabActionButton` (`theme.prop_field_active` on `toned`, `button_down` on `!toned`).
    pub action_down: u32,
    /// Pressed face for core `FabButtonAccent` (`theme.color_bg_highlight` on `toned`, fitted on `!toned`).
    pub accent_down: u32,
}

impl Palette {
    /// Pure resolution from any theme token reader (no VM required).
    /// `toned` is true when [`toned`] wrote Fab's host-sheet ink overrides onto the theme.
    pub fn resolve(read_color: impl Fn(&str) -> Option<u32>, toned: bool) -> Self {
        let grounds = Grounds::from_theme(&read_color);
        let text = read_color("color_text").unwrap_or(0xffffffff);
        let dim = read_color("color_on_surface_variant").unwrap_or(text);
        let muted = read_color("color_text_disabled").unwrap_or(dim);
        let raw_on_accent = read_color("color_text_on_accent").unwrap_or(0xffffffff);
        let choice_on = if toned {
            grounds.choice_on_raw
        } else if plain_end(grounds.choice_on_raw) == plain_end(grounds.panel) {
            bounded_mix(grounds.panel, grounds.choice_on_raw, 1.0, plain_end(grounds.panel), READABLE).0
        } else {
            grounds.choice_on_raw
        };
        let inks = if toned {
            Inks {
                text,
                dim,
                muted: dim,
                well_text: text,
                well_dim: dim,
                well_muted: dim,
                on_accent: raw_on_accent,
                on_ctrl_selected: raw_on_accent,
                segment_active: text,
                action_dim: read_color("prop_ink_dim").unwrap_or(dim),
                input_hot_hover: read_color("color_label_inner_hover").unwrap_or(text),
                input_hot_down: read_color("color_label_inner_down").unwrap_or(text),
                toggle_label_hover: read_color("color_label_outer_hover").unwrap_or(text),
                toggle_label_down: read_color("color_label_outer_down").unwrap_or(text),
            }
        } else {
            Inks::fit(&grounds, choice_on, text, dim, muted, raw_on_accent)
        };
        let (button_hover, button_down, accent_down) = if toned {
            (grounds.button_hover_raw, grounds.button_down_raw, grounds.highlight)
        } else {
            let hover = safe_button_face(grounds.button_hover_raw, &grounds, &inks);
            let down = safe_button_face(grounds.button_down_raw, &grounds, &inks);
            let opp = if plain_end(grounds.accent) == 0xffffffff { 0x000000ff } else { 0xffffffff };
            let acc_down = bounded_mix(grounds.accent, opp, 0.14, inks.on_accent, READABLE).0;
            (hover, down, acc_down)
        };
        let action_down = if toned { grounds.field_active_raw } else { button_down };
        Self { grounds, inks, toned, choice_on, button_hover, button_down, action_down, accent_down }
    }

    /// Read Fab's grounds and inks off `mod.theme` in `vm`.
    pub fn read(vm: &mut ScriptVm) -> Self {
        let theme = vm.module(id!(theme));
        let read = |key: &str| vm.value(theme, LiveId::from_str(key)).as_color();
        let toned = vm.value(theme, id!(fab_toned)).as_number().is_some();
        Self::resolve(read, toned)
    }

    /// Inactive `FabPanelTab` / `FabUnderlineTab` ink (`mix(color_text_dim, color_text, 0.5)`).
    pub fn inactive_tab_ink(&self) -> u32 {
        mix_rgba(self.inks.dim, self.inks.text, 0.5)
    }
}

/// Body text, the second voice (labels, kinds, dimensions, placeholders, counts, captions,
/// inactive tabs), muted graphics (`3:1`: chevrons, icons, dots, disabled rows), and the
/// matching three inks fitted independently to the inset well (`color_input` / `color_panel_sub`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Inks {
    pub text: u32,
    pub dim: u32,
    pub muted: u32,
    pub well_text: u32,
    pub well_dim: u32,
    pub well_muted: u32,
    /// Words/icons on `grounds.accent` (`FabButtonAccent`).
    pub on_accent: u32,
    /// Icons/words on `grounds.ctrl_selected` (core `FabIconToggle` active).
    pub on_ctrl_selected: u32,
    /// Words/icons on `Palette::choice_on` (core `FabSegmentTab` active).
    pub segment_active: u32,
    /// `theme.prop_ink_dim` on `toned`, `dim` on `!toned` (`FabActionButton` resting icon).
    pub action_dim: u32,
    /// `theme.color_label_inner_{hover,down}` on `toned`, `well_text` on `!toned` (`ComboBox` hover/down text).
    pub input_hot_hover: u32,
    pub input_hot_down: u32,
    /// `theme.color_label_outer_{hover,down}` on `toned`, `text` on `!toned` (labeled toggle hover/down text).
    pub toggle_label_hover: u32,
    pub toggle_label_down: u32,
}

impl Inks {
    /// Read off the theme as it stands now (after the sheet, if any, went on).
    pub fn read(vm: &mut ScriptVm) -> Self {
        Palette::read(vm).inks
    }

    /// Fit Fab's chrome and inset-well inks against `grounds`.
    pub fn fit(grounds: &Grounds, choice_on: u32, text: u32, dim: u32, muted: u32, raw_on_accent: u32) -> Self {
        let chrome: [u32; 10] = grounds.chrome_surfaces().map(|(_, bg)| bg);
        let graphics: [u32; 6] = grounds.graphics_surfaces().map(|(_, bg)| bg);
        let same_pol = grounds.inset_matches_chrome();
        let well_all = [
            grounds.inset,
            grounds.prop_field_panel,
            grounds.prop_field_toolbar,
            grounds.prop_field_hover_panel,
            grounds.prop_field_hover_toolbar,
        ];
        let well: &[u32] = if same_pol { &well_all } else { &well_all[..1] };
        let (need_text, need_dim, need_muted) = if same_pol {
            (READABLE, READABLE, QUIET)
        } else {
            (WELL_BODY_INVERTED, WELL_DIM_INVERTED, WELL_MUTED_INVERTED)
        };
        let body = ink_reading(&chrome, text, READABLE);
        let quiet = ink_reading(&chrome, dim, READABLE);
        let well_text = ink_reading(well, text, need_text);
        Self {
            text: body,
            dim: quiet,
            muted: ink_reading(&graphics, muted, QUIET),
            well_text,
            on_accent: ink_reading(&[grounds.accent], raw_on_accent, READABLE),
            on_ctrl_selected: ink_reading(&[grounds.ctrl_selected], raw_on_accent, READABLE),
            segment_active: ink_reading(&[choice_on], text, READABLE),
            action_dim: quiet,
            input_hot_hover: well_text,
            input_hot_down: well_text,
            toggle_label_hover: body,
            toggle_label_down: body,
            well_dim: ink_reading(well, dim, need_dim),
            well_muted: ink_reading(well, muted, need_muted),
        }
    }
}

fn button_face_ok(raw: u32, grounds: &Grounds, inks: &Inks) -> bool {
    let a = (raw & 0xff) as f64;
    let wash85 = (raw & 0xffff_ff00) | ((a * 0.85).round() as u32);
    [grounds.panel, grounds.toolbar, grounds.high].into_iter().all(|base| {
        [over(base, raw), over(base, wash85)].into_iter().all(|bg| {
            reads_on(bg, inks.text) >= READABLE
                && reads_on(bg, inks.dim) >= READABLE
                && reads_on(bg, inks.muted) >= QUIET
        })
    })
}

fn safe_button_face(raw: u32, grounds: &Grounds, inks: &Inks) -> u32 {
    for cand in [raw, grounds.field_hover, grounds.field, grounds.panel] {
        if button_face_ok(cand, grounds, inks) {
            return cand;
        }
    }
    grounds.panel
}

/// The quietest graphic ink's floor: 3:1 (WCAG non-text UI components: chevrons, search
/// magnifier, status dots, empty-well glyphs, disabled menu items). Words use `READABLE` (4.5:1).
const QUIET: f64 = 3.0;

/// Inverted-well targets (`safety-yellow`, `mecha-light`, `knitted`, inverted blends):
/// `theme.color_text` has the wrong polarity, so bisecting to bare `4.5:1` collapses body and
/// dim onto one mid-grey that dips under `4.5:1` on gradient sheet wells (`sy_field` `#272729`,
/// `mk_field` `#2f3b4e`). `ink_reading` caps cleanly at `plain_end(inset)` on mid-grey blends.
const WELL_BODY_INVERTED: f64 = 10.0;
const WELL_DIM_INVERTED: f64 = 5.5;
const WELL_MUTED_INVERTED: f64 = 3.5;

/// Plain high-contrast endpoint (white or black) for an opaque ground.
fn plain_end(ground: u32) -> u32 {
    if reads_on(ground, 0xffffffff) >= reads_on(ground, 0x000000ff) {
        0xffffffff
    } else {
        0x000000ff
    }
}

/// `ink`, or the smallest mix from `ink` toward `plain_end(grounds[0])` that reaches `need`
/// on every ground in `grounds`. Returns `ink` unchanged when it already reads everywhere,
/// and falls back to `plain_end` if even the plain end cannot clear `need`.
fn ink_reading(grounds: &[u32], ink: u32, need: f64) -> u32 {
    debug_assert!(!grounds.is_empty());
    let ok = |c: u32| grounds.iter().all(|&g| reads_on(g, c) >= need);
    if ok(ink) {
        return ink;
    }
    let end = plain_end(grounds[0]);
    if !ok(end) {
        return end;
    }
    let (mut lo, mut hi) = (0.0_f64, 1.0_f64);
    // 16 bisections (1/65536) resolve well past 8-bit channel rounding, matching `property_dim_ink`.
    for _ in 0..16 {
        let mid = 0.5 * (lo + hi);
        if ok(mix_rgba(ink, end, mid)) {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    mix_rgba(ink, end, hi)
}

/// Nominal white-mix fraction for `color_selection_bg` over `panel`.
const SELECTION_MIX: f64 = 0.063;
/// Nominal alpha of the white section-header wash (`#ffffff10`).
const SECTION_WASH_ALPHA: u8 = 0x10;

/// Largest factor in `1.0..=want` where `brighter(base, factor)` still lets `end` read at `need`.
/// Returns `(color, actual_factor, clamped)`.
fn bounded_brighter(base: u32, want: f64, end: u32, need: f64) -> (u32, f64, bool) {
    let full = brighter(base, want);
    if reads_on(full, end) >= need {
        return (full, want, false);
    }
    if reads_on(base, end) < need {
        return (base, 1.0, true);
    }
    let (mut lo, mut hi) = (1.0_f64, want);
    for _ in 0..16 {
        let mid = 0.5 * (lo + hi);
        if reads_on(brighter(base, mid), end) >= need {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    (brighter(base, lo), lo, true)
}

/// Largest `t` in `0.0..=want` where `mix_rgb(base, target, t)` still lets `end` read at `need`.
/// Returns `(color, actual_t, clamped)`.
fn bounded_mix(base: u32, target: u32, want: f64, end: u32, need: f64) -> (u32, f64, bool) {
    let full = mix_rgb(base, target, want);
    if reads_on(full, end) >= need {
        return (full, want, false);
    }
    if reads_on(base, end) < need {
        return (base, 0.0, true);
    }
    let (mut lo, mut hi) = (0.0_f64, want);
    for _ in 0..16 {
        let mid = 0.5 * (lo + hi);
        if reads_on(mix_rgb(base, target, mid), end) >= need {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    (mix_rgb(base, target, lo), lo, true)
}

/// Largest alpha in `0..=want_alpha` where `over(base, rgb | alpha)` still lets `end` read
/// at `need` on every base in `bases`. Returns `(wash_rgba, clamped)`.
fn bounded_wash(bases: &[u32], wash_rgba: u32, end: u32, need: f64) -> (u32, bool) {
    let ok = |rgba: u32| bases.iter().all(|&b| reads_on(over(b, rgba), end) >= need);
    if ok(wash_rgba) {
        return (wash_rgba, false);
    }
    let rgb = wash_rgba & 0xffff_ff00;
    let want_a = (wash_rgba & 0xff) as u8;
    if !ok(rgb) {
        return (rgb, true);
    }
    let (mut lo, mut hi) = (0u8, want_a);
    while lo < hi {
        let mid = lo + (hi - lo).div_ceil(2);
        if ok(rgb | mid as u32) {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    (rgb | lo as u32, true)
}

/// Darkest neutral grey for resting body text on a toned dark sheet (`#aaaaaa`, L* = 69.6);
/// lifted on brighter dark sheets so dim stays ~10 L* below body (`toned_inks`).
const TEXT_REST_FLOOR: u8 = 0xaa;
/// Darkest neutral grey considered for dimmed words (`#666666`); `toned_inks` lifts this per sheet.
const TEXT_DIM_FLOOR: u8 = 0x66;
/// Target CIE L* step from dimmed text up to resting body text on a toned dark sheet.
const TEXT_BODY_STEP_LSTAR: f64 = 10.0;
const TEXT_HOT: &str = "#ffffff";
/// Words and marks on the accent's bright face (the Print button, an active toggle).
const TEXT_ON_ACCENT: &str = "#x000000";

/// How much brighter the editors' toolbars (and the shown tab over each) are than the sheet's
/// `color_surface_container`.
const TOOLBAR_LIFT: f64 = 1.15 * 1.10;
/// How much brighter the tab bars are than the sheet's `color_surface_container_high`.
const TAB_LIFT: f64 = 1.10;
/// How much brighter the panels' ground is: a step under their toolbars, which it parts from.
const PANEL_LIFT: f64 = 1.08 * 0.95;

/// `0xRRGGBBAA` with each colour channel times `by` (capped at full), its alpha kept.
fn brighter(c: u32, by: f64) -> u32 {
    let ch = |shift: u32| ((((c >> shift) & 0xff) as f64 * by).round().min(255.0) as u32) << shift;
    ch(24) | ch(16) | ch(8) | (c & 0xff)
}

const fn grey_rgba(v: u8) -> u32 {
    let v = v as u32;
    (v << 24) | (v << 16) | (v << 8) | 0xff
}

fn grey_luminance(g: u8) -> f64 {
    let v = g as f64 / 255.0;
    if v <= 0.03928 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
}

fn grey_lstar(g: u8) -> f64 {
    let y = grey_luminance(g);
    if y > (6.0_f64 / 29.0).powi(3) {
        116.0 * y.cbrt() - 16.0
    } else {
        (29.0_f64 / 3.0).powi(3) * y
    }
}

fn toned_grounds(sheet: &StyleSheet) -> Grounds {
    let container = sheet
        .theme_color("color_surface_container")
        .or_else(|| sheet.theme_color("color_fg_app"))
        .unwrap_or(0x28282aff)
        | 0xff;
    // Matches `DERIVED_ROLES` for dark sheets (`theme_tokens.rs`): `mix(color_fg_app, #F, 0.08)`.
    let high = sheet
        .theme_color("color_surface_container_high")
        .map(|c| c | 0xff)
        .unwrap_or_else(|| mix_rgb(container, 0xffffffff, 0.08));
    let inset = sheet.theme_color("color_inset");
    let highlight = sheet.theme_color("color_bg_highlight");
    Grounds::from_theme(|key| match key {
        "color_surface_container" => Some(container),
        "color_surface_container_high" => Some(high),
        "color_inset" => inset,
        // Matches what `toned()` assigns to `mod.theme.prop_field`.
        "prop_field" => Some(0x0000002b),
        "color_bg_highlight" => highlight,
        _ => None,
    })
}

/// Smallest neutral grey `dim >= TEXT_DIM_FLOOR` reaching `READABLE` (4.5:1) on every neutral
/// chrome ground of `sheet`, and neutral grey `body >= TEXT_REST_FLOOR` closest in CIE `L*` to
/// `L*(dim) + TEXT_BODY_STEP_LSTAR`.
fn toned_inks(sheet: &StyleSheet) -> (u32, u32) {
    let grounds = toned_grounds(sheet).neutral_chrome();
    let dim_v = (TEXT_DIM_FLOOR..=0xff)
        .find(|&g| {
            let c = grey_rgba(g);
            grounds.iter().all(|&(_, bg)| reads_on(bg, c) >= READABLE)
        })
        .unwrap_or(0xff);
    let target_lstar = grey_lstar(dim_v) + TEXT_BODY_STEP_LSTAR;
    let body_v = (TEXT_REST_FLOOR.max(dim_v)..=0xff)
        .min_by(|&a, &b| {
            let da = (grey_lstar(a) - target_lstar).abs();
            let db = (grey_lstar(b) - target_lstar).abs();
            da.total_cmp(&db)
        })
        .unwrap_or(0xff);
    (grey_rgba(body_v), grey_rgba(dim_v))
}

pub fn toned(mut sheet: StyleSheet) -> StyleSheet {
    let light_ink = sheet.theme_color("color_text").is_some_and(|c| reads_on(0x000000ff, c) > reads_on(0xffffffff, c));
    if light_ink {
        let (body, dim) = toned_inks(&sheet);
        // `#x` avoids Splash reading a hex run like `8e...` / `9e...` as a scientific-notation float.
        let body_hex = format!("#x{:06x}", body >> 8);
        let dim_hex = format!("#x{:06x}", dim >> 8);
        let mut tones = String::from("\n// Fab: resting text under white, white on hover and press (fab::theme::toned)\n");
        tones.push_str("mod.theme.fab_toned = 1.0\n");
        for ink in ["color_text", "color_label_inner", "color_label_outer", "color_label"] {
            for (state, tone) in [("", body_hex.as_str()), ("_focus", body_hex.as_str()), ("_active", body_hex.as_str()), ("_hover", TEXT_HOT), ("_down", TEXT_HOT)] {
                tones.push_str(&format!("mod.theme.{ink}{state} = {tone}\n"));
            }
        }
        // on the accent's bright face (a primary button, an active toggle): black
        tones.push_str(&format!("mod.theme.color_text_on_accent = {TEXT_ON_ACCENT}\n"));
        for ink in ["color_on_surface_variant", "color_text_meta", "color_text_disabled", "color_label_inner_disabled", "color_label_outer_disabled", "color_label_disabled"] {
            tones.push_str(&format!("mod.theme.{ink} = {dim_hex}\n"));
        }
        // The property kit's fields (number fields and sliders, dropdowns, segmented choices): a
        // shade under whatever ground they sit on (#0000002b, the section headers' were), a
        // little less under the pointer.
        tones.push_str("mod.theme.prop_field = #x0000002b\nmod.theme.prop_field_hover = #x0000001c\n");
        sheet.theme.push_str(&tones);
    }
    sheet
}

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
    if vm.cx().in_host() {
        return;
    }
    let dark = makepad_platform::host_dark().unwrap_or(true);
    let family = host_family(vm.cx());
    wear_host_sheet_with(vm, family, dark);
}

fn wear_host_sheet_with(vm: &mut ScriptVm, family: DesktopStyle, dark: bool) {
    desktop_style::install_default(vm, |_| toned(StyleSheet::load(family.sheet(dark))));
}

fn follow_host_appearance_dark(cx: &mut Cx, dark: bool) -> bool {
    if !theme_edits(cx).is_empty() || theme_pins(cx).is_some() || theme_mix(cx).is_some() {
        return false;
    }
    let changed = cx.with_vm(|vm| {
        let Some(worn) = desktop_style::default_name(vm) else { return false };
        let Some(entry) = desktop_style::find(&worn) else { return false };
        let want = entry.with_appearance(dark);
        if want.id == worn {
            return false;
        }
        desktop_style::replace_default(vm, toned(StyleSheet::load(want)))
    });
    if changed {
        cx.request_style_reload();
    }
    changed
}

/// Follow a change of the host's light or dark appearance (call on startup
/// and when a window gets focus): only the sheet Fab put on itself moves.
pub fn follow_host_appearance(cx: &mut Cx) {
    let Some(dark) = makepad_platform::host_dark() else { return };
    follow_host_appearance_dark(cx, dark);
}

#[cfg(test)]
mod host_tests;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct WidgetInks {
    rest: u32,
    hover: u32,
    down: u32,
    focus: u32,
    active: u32,
}

fn widget_sheet_inks(vm: &mut ScriptVm, widget: LiveId) -> Option<WidgetInks> {
    desktop_style::current_name(vm)?;
    let widgets = vm.module(id!(widgets));
    let w = vm.value(widgets, widget).as_object()?;
    let dt = vm.value(w, id!(draw_text)).as_object()?;
    let rest = shader_color(vm, vm.value(dt, id!(color)))?;
    let hover = shader_color(vm, vm.value(dt, id!(color_hover))).unwrap_or(rest);
    let down = shader_color(vm, vm.value(dt, id!(color_down))).unwrap_or(rest);
    let focus = shader_color(vm, vm.value(dt, id!(color_focus))).unwrap_or(rest);
    let active = shader_color(vm, vm.value(dt, id!(color_active))).unwrap_or(rest);
    Some(WidgetInks { rest, hover, down, focus, active })
}

fn widget_color_default(vm: &mut ScriptVm, widget: LiveId) -> Option<u32> {
    desktop_style::current_name(vm)?;
    let widgets = vm.module(id!(widgets));
    let w = vm.value(widgets, widget).as_object()?;
    let bg = vm.value(w, id!(draw_bg)).as_object()?;
    shader_color(vm, vm.value(bg, id!(color_default)))
}

fn shader_color(vm: &ScriptVm, value: ScriptValue) -> Option<u32> {
    if let Some(c) = value.as_color() {
        return Some(c);
    }
    let obj = value.as_object()?;
    vm.heap().as_shader_io(obj)?;
    vm.heap().proto(obj).as_color()
}

/// When a style sheet is worn, read its `TextInputFlat.draw_text.{color, color_empty}` after
/// `desktop_style::apply_widgets` so sheets whose `TextInputFlat.draw_bg.pixel` diverges in
/// polarity from `theme.color_inset` (`concrete`, `handheld`, `pop-art`) keep their own well ink.
fn text_input_sheet_inks(vm: &mut ScriptVm) -> Option<(u32, Option<u32>)> {
    desktop_style::current_name(vm)?;
    let widgets = vm.module(id!(widgets));
    let ti = vm.value(widgets, id!(TextInputFlat)).as_object()?;
    let dt = vm.value(ti, id!(draw_text)).as_object()?;
    let text = shader_color(vm, vm.value(dt, id!(color)))?;
    let empty = shader_color(vm, vm.value(dt, id!(color_empty)));
    Some((text, empty))
}

/// Choose one `ButtonFlatter` ink on a `@host_over` sheet by measured contrast (`>= READABLE`)
/// over `[panel, toolbar, high]` composited under `face`: prefer Fab's `fab_ink`, then the
/// sheet's `ButtonFlatter` ink, then Fab's `body_ink` (when `fab_ink != body_ink`), else fit
/// with `ink_reading`.
fn pick_flat_ink(
    bases: [u32; 3],
    face: u32,
    sheet_ink: Option<u32>,
    fab_ink: u32,
    body_ink: u32,
) -> u32 {
    let grounds = bases.map(|base| over(base, face));
    let ok = |ink: u32| grounds.iter().all(|&bg| reads_on(bg, ink) >= READABLE);
    if ok(fab_ink) {
        return fab_ink;
    }
    if let Some(c) = sheet_ink.filter(|&c| ok(c)) {
        return c;
    }
    if fab_ink != body_ink && ok(body_ink) {
        return body_ink;
    }
    ink_reading(&grounds, sheet_ink.unwrap_or(fab_ink), READABLE)
}

pub fn script_mod(vm: &mut ScriptVm) {
    let palette = Palette::read(vm);
    if palette.grounds.lift_clamps.any() {
        let sheet = desktop_style::current_name(vm).unwrap_or_else(|| "base".to_string());
        let c = palette.grounds.lift_clamps;
        let mut clamped = Vec::new();
        if c.panel { clamped.push(format!("panel=#{:06x}", palette.grounds.panel >> 8)); }
        if c.toolbar { clamped.push(format!("toolbar=#{:06x}", palette.grounds.toolbar >> 8)); }
        if c.header { clamped.push(format!("header=#{:06x}", palette.grounds.header >> 8)); }
        if c.selection { clamped.push(format!("selection=#{:06x}", palette.grounds.selection >> 8)); }
        if c.section { clamped.push(format!("section=#{:08x}", palette.grounds.section_wash)); }
        if c.field { clamped.push(format!("field=#{:08x}", palette.grounds.field)); }
        log!(
            "fab theme ({sheet}): contrast bound clamped surface lift(s) to keep 4.5:1 body text: {}",
            clamped.join(", ")
        );
    }
    let text = ScriptValue::from_color(palette.inks.text);
    let dim = ScriptValue::from_color(palette.inks.dim);
    let muted = ScriptValue::from_color(palette.inks.muted);
    let panel = ScriptValue::from_color(palette.grounds.panel);
    let toolbar = ScriptValue::from_color(palette.grounds.toolbar);
    let header = ScriptValue::from_color(palette.grounds.header);
    let editor_alt = ScriptValue::from_color(palette.grounds.editor_alt);
    let selection = ScriptValue::from_color(palette.grounds.selection);
    let well_text = ScriptValue::from_color(palette.inks.well_text);
    let well_dim = ScriptValue::from_color(palette.inks.well_dim);
    let well_muted = ScriptValue::from_color(palette.inks.well_muted);
    let section_wash = ScriptValue::from_color(palette.grounds.section_wash);
    let field = ScriptValue::from_color(palette.grounds.field);
    let input_field = ScriptValue::from_color(palette.grounds.input_fill());
    let input_field_hover = ScriptValue::from_color(palette.grounds.input_fill_hover());
    let is_light = |c: u32| reads_on(0x000000ff, c) > reads_on(0xffffffff, c);
    let (input_text_c, input_dim_c, input_muted_c) = match text_input_sheet_inks(vm) {
        Some((ti_text, ti_empty)) if is_light(ti_text) != is_light(palette.inks.well_text) => {
            // `concrete` (`#aeada7` = 4.32:1 on `#454442`) nudges 25% toward `ti_text` -> 5.02:1.
            let dim = ti_empty.map(|e| mix_rgba(e, ti_text, 0.25)).unwrap_or(ti_text);
            let muted = ti_empty.unwrap_or(ti_text);
            (ti_text, dim, muted)
        }
        _ => (palette.inks.well_text, palette.inks.well_dim, palette.inks.well_muted),
    };
    let rbt_face = desktop_style::widget_face(vm, id!(RadioButtonTabFlat));
    let bf_face = desktop_style::widget_face(vm, id!(ButtonFlat));
    let bfr_face = desktop_style::widget_face(vm, id!(ButtonFlatter));
    let rbt_own = rbt_face.is_some_and(|f| f.mode == FaceMode::OwnFace);
    let bf_own = bf_face.is_some_and(|f| f.mode == FaceMode::HostOver);
    let bfr_own = bfr_face.is_some_and(|f| f.mode == FaceMode::HostOver);
    let rbt_inks = widget_sheet_inks(vm, id!(RadioButtonTabFlat));
    let bf_inks = widget_sheet_inks(vm, id!(ButtonFlat));
    let bfr_inks = widget_sheet_inks(vm, id!(ButtonFlatter));
    let text_is_light = is_light(palette.inks.text);
    let rbt_down_flips = rbt_inks.is_some_and(|i| is_light(i.down) != text_is_light);

    let (toggle_ink, toggle_hover, toggle_down, toggle_active) = match (rbt_own, rbt_inks) {
        (true, Some(i)) => (i.rest, i.hover, i.down, i.active),
        _ => (
            palette.inks.text,
            palette.inks.text,
            if rbt_down_flips { palette.inks.on_ctrl_selected } else { palette.inks.text },
            palette.inks.on_ctrl_selected,
        ),
    };
    let (seg_ink, seg_hover, seg_down, seg_active) = match (rbt_own, rbt_inks) {
        (true, Some(i)) => (i.rest, i.hover, i.down, i.active),
        _ => (
            palette.inks.dim,
            palette.inks.text,
            if rbt_down_flips { palette.inks.segment_active } else { palette.inks.text },
            palette.inks.segment_active,
        ),
    };
    let (btn_bg, btn_ink, btn_hover, btn_down, btn_focus) = match (bf_own, widget_color_default(vm, id!(ButtonFlat)), bf_inks) {
        (true, Some(bg), Some(i)) => (bg, i.rest, i.hover, i.down, i.focus),
        _ => (palette.grounds.field, palette.inks.text, palette.inks.text, palette.inks.text, palette.inks.text),
    };
    let flat_bases = [palette.grounds.panel, palette.grounds.toolbar, palette.grounds.high];
    let pick_flat = |face: Option<u32>, sheet_c: Option<u32>, fab_c: u32| match face {
        Some(f) if bfr_own => pick_flat_ink(flat_bases, f, sheet_c, fab_c, palette.inks.text),
        _ => fab_c,
    };
    let bfr_rest = bfr_face.and_then(|f| f.rest);
    let bfr_hover = bfr_face.and_then(|f| f.hover);
    let bfr_down = bfr_face.and_then(|f| f.down);
    let bfr_focus = bfr_face.and_then(|f| f.focus.or(f.rest));
    let flat_ink = pick_flat(bfr_rest, bfr_inks.map(|i| i.rest), palette.inks.text);
    let flat_ink_dim = pick_flat(bfr_rest, bfr_inks.map(|i| i.rest), palette.inks.dim);
    let flat_ink_hover = pick_flat(bfr_hover, bfr_inks.map(|i| i.hover), palette.inks.text);
    let flat_ink_down = pick_flat(bfr_down, bfr_inks.map(|i| i.down), palette.inks.text);
    let flat_ink_focus = pick_flat(bfr_focus, bfr_inks.map(|i| i.focus), palette.inks.text);
    let toned = palette.toned;
    let action_ink = if toned { palette.inks.action_dim } else { flat_ink_dim };
    let action_down = palette.action_down;
    // Only tint `FabIconButton.draw_icon` on focus when `ButtonFlatter`'s focus face pushes
    // the resting ink below `READABLE` (in the 51-sheet catalogue, `kids-toy` only).
    // `0.0` on all 5 toned hosts and on every sheet whose resting ink holds on focus, so
    // subclasses (`FabActionButton`, `ToolIcon`, `FoldIcon`, `close`) never shift on focus.
    let flat_focus_tint: f64 = if !toned
        && bfr_own
        && bfr_focus.is_some_and(|face| {
            flat_bases.iter().any(|&base| {
                let bg = over(base, face);
                reads_on(bg, flat_ink) < READABLE || reads_on(bg, flat_ink_dim) < READABLE
            })
        })
    {
        1.0
    } else {
        0.0
    };
    let input_text_hover = if toned { palette.inks.input_hot_hover } else { input_text_c };
    let input_text_down = if toned { palette.inks.input_hot_down } else { input_text_c };
    let toggle_label_hover = if toned { palette.inks.toggle_label_hover } else { toggle_hover };
    let toggle_label_down = if toned { palette.inks.toggle_label_down } else { toggle_down };
    let mute_ink_down = if toned { palette.inks.text } else { toggle_down };
    let mute_ink_active = if toned { 0xffffffff } else { toggle_active };
    let cam_close_hover = if toned { palette.button_hover } else { 0x000000c0 };
    let cam_close_down = if toned { palette.button_down } else { 0x000000d8 };
    let cam_close_focus = if toned { 0x00000000 } else { 0x000000a0 };
    let cam_close_ink_hot = if toned { palette.inks.text } else { 0xffffffff };
    let flat_icon_or_raw = if toned {
        script_eval!(vm, { use mod.pod.*; vec4(-1.0, -1.0, -1.0, -1.0) })
    } else {
        ScriptValue::from_color(flat_ink)
    };
    let input_text = ScriptValue::from_color(input_text_c);
    let input_dim = ScriptValue::from_color(input_dim_c);
    let input_muted = ScriptValue::from_color(input_muted_c);
    script_eval!(vm, {
        use mod.prelude.widgets.*

        mod.fab = {
            // ---- surfaces ----
            // The shell ground the docked panels and lists show (the sheet's
            // container rung, `PANEL_LIFT` brighter), then the tab bars,
            // caption and editor bars one rung up (opaque ladder).
            color_area: #(panel)
            color_editor: #(panel)
            color_editor_alt: #(editor_alt)
            // the tab bars (`TAB_LIFT` over the sheet's high rung)
            color_header: #(header)
            // the editors' toolbars (under their area's tabs) and the shown tab over each: a
            // shade above the panel ground
            color_toolbar: #(toolbar)
            color_panel: #(panel)
            color_panel_sub: theme.color_inset
            color_popover: theme.color_surface_container_high
            color_popover_border: theme.color_bevel_outset_2
            color_topbar: theme.color_surface_container_high
            color_statusbar: theme.color_surface_container_high
            color_border: theme.color_bevel_inset_2
            color_border_light: theme.color_bevel_outset_1
            color_row_hover: theme.color_outset_hover
            // a foldable section's header: a faint white over the panel (#ffffff10, the fields'
            // were)
            color_section: #(section_wash)
            color_input: theme.color_inset
            color_input_hover: theme.color_inset_hover
            color_input_active: theme.color_inset_active
            color_field: #(field)
            color_input_field: #(input_field)
            color_input_field_hover: #(input_field_hover)
            color_button: theme.color_outset
            color_button_hover: #(ScriptValue::from_color(palette.button_hover))
            color_button_down: #(ScriptValue::from_color(palette.button_down))
            color_button_active: theme.color_ctrl_selected

            color_field_hover: #(ScriptValue::from_color(palette.grounds.field_hover))
            color_choice_on: #(ScriptValue::from_color(palette.choice_on))
            color_accent_down: #(ScriptValue::from_color(palette.accent_down))
            color_btn_bg: #(ScriptValue::from_color(btn_bg))
            color_toggle_ink: #(ScriptValue::from_color(toggle_ink))
            color_toggle_ink_hover: #(ScriptValue::from_color(toggle_hover))
            color_toggle_ink_down: #(ScriptValue::from_color(toggle_down))
            color_toggle_ink_active: #(ScriptValue::from_color(toggle_active))
            color_segment_ink: #(ScriptValue::from_color(seg_ink))
            color_segment_ink_hover: #(ScriptValue::from_color(seg_hover))
            color_segment_ink_down: #(ScriptValue::from_color(seg_down))
            color_segment_ink_active: #(ScriptValue::from_color(seg_active))
            color_btn_ink: #(ScriptValue::from_color(btn_ink))
            color_btn_ink_hover: #(ScriptValue::from_color(btn_hover))
            color_btn_ink_down: #(ScriptValue::from_color(btn_down))
            color_btn_ink_focus: #(ScriptValue::from_color(btn_focus))
            color_flat_ink: #(ScriptValue::from_color(flat_ink))
            color_flat_ink_dim: #(ScriptValue::from_color(flat_ink_dim))
            color_flat_ink_hover: #(ScriptValue::from_color(flat_ink_hover))
            color_flat_ink_down: #(ScriptValue::from_color(flat_ink_down))
            color_flat_ink_focus: #(ScriptValue::from_color(flat_ink_focus))
            color_action_ink: #(ScriptValue::from_color(action_ink))
            color_action_down: #(ScriptValue::from_color(action_down))
            flat_focus_tint: #(flat_focus_tint)
            color_text_input_hover: #(ScriptValue::from_color(input_text_hover))
            color_text_input_down: #(ScriptValue::from_color(input_text_down))
            color_toggle_label_hover: #(ScriptValue::from_color(toggle_label_hover))
            color_toggle_label_down: #(ScriptValue::from_color(toggle_label_down))
            color_mute_ink_down: #(ScriptValue::from_color(mute_ink_down))
            color_mute_ink_active: #(ScriptValue::from_color(mute_ink_active))
            color_flat_icon_or_raw: #(flat_icon_or_raw)
            color_cam_close_hover: #(ScriptValue::from_color(cam_close_hover))
            color_cam_close_down: #(ScriptValue::from_color(cam_close_down))
            color_cam_close_focus: #(ScriptValue::from_color(cam_close_focus))
            color_cam_close_ink_hot: #(ScriptValue::from_color(cam_close_ink_hot))

            // ---- text ----
            // Measured against every ground they are drawn on (`Inks`): text and dim at 4.5:1 on
            // `chrome_surfaces()`, muted at 3:1 on `graphics_surfaces()` for icons/chevrons/disabled
            // marks only, and the three `well_*` inks fitted independently to `inset`.
            color_text: #(text)
            color_text_dim: #(dim)
            color_text_muted: #(muted)
            color_text_well: #(well_text)
            color_text_well_dim: #(well_dim)
            color_text_well_muted: #(well_muted)
            color_text_input: #(input_text)
            color_text_input_dim: #(input_dim)
            color_text_input_muted: #(input_muted)
            // a disabled button's text and icon: a dark wash over its ground, not a dimmed ink
            color_disabled_ink: #x00000066
            color_text_active: #(text)
            // what the pointer is on lights to the sheet's hover ink (white over Fab's resting ink)
            color_text_hover: theme.color_text_hover
            color_text_header: #(text)
            color_text_on_accent: #(ScriptValue::from_color(palette.inks.on_accent))

            // ---- accents ----
            color_accent: theme.color_focus
            color_accent_hover: theme.color_focus
            color_accent_dim: theme.color_bg_highlight
            // a picked list row: the section headers' lighter grey (`color_section` over the panel
            // ground), opaque, so a row that mixes into it from its ground reads as that grey
            color_selection_bg: #(selection)
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
            // (one look for everything that floats: a toolbar over the view is a flyout's fill
            // and outline, as the menus and popovers)
            color_float: theme.color_surface_container_high
            color_float_border: theme.color_bevel_outset_2
            // the soft drop shadow under every floating card (toolbars, flyouts, menus)
            color_float_shadow: #x00000070
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
            color_num_arrow: #(well_text)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ink_reading_binary_search_is_minimal_and_exact() {
        assert_eq!(ink_reading(&[0x202020ff], 0xaaaaaaff, READABLE), 0xaaaaaaff, "keep a readable ink");

        let ground = 0x454547ff;
        let first = (0..=u8::MAX).find(|&g| reads_on(ground, grey_rgba(g)) >= READABLE).unwrap();
        let fitted = ink_reading(&[ground], 0x505050ff, READABLE);
        assert!(reads_on(ground, fitted) >= READABLE, "the fitted ink must clear the threshold");
        assert!(fitted == grey_rgba(first) || fitted == grey_rgba(first + 1),
            "expected a grey within one level of {first}, got #{:06x}", fitted >> 8);

        let incompatible = [0x000000ff, 0xffffffff];
        assert_eq!(ink_reading(&incompatible, 0x333333ff, READABLE), plain_end(incompatible[0]),
            "fall back to the first ground's endpoint when it cannot read on both");
    }

    #[test]
    fn contrast_bound_clamps_kids_toy_and_spares_field_radio() {
        let grounds = |container, high, field| Grounds::from_theme(|key| match key {
            "color_surface_container" => Some(container),
            "color_surface_container_high" => Some(high),
            "prop_field" => Some(field),
            _ => None,
        });
        let kids = grounds(0x0f7a78ff, 0x386fbfff, 0x3883a3ff);
        assert_eq!(kids.lift_clamps, LiftClamps {
            panel: false, toolbar: true, header: true, selection: true, section: true, field: true,
        }, "only lifts that put white below the threshold should clamp");
        for (name, bg) in kids.chrome_surfaces() {
            assert!(reads_on(bg, 0xffffffff) >= READABLE, "kids-toy: white on {name} #{:06x}", bg >> 8);
        }

        let radio = grounds(0x454838ff, 0x494c3cff, 0x575947ff);
        assert!(!radio.lift_clamps.any(), "field-radio has room for all its lifts");
        assert_eq!(radio.toolbar, brighter(0x454838ff, TOOLBAR_LIFT), "keep field-radio's full toolbar lift");
    }

    #[test]
    fn inverted_well_inks_flip_polarity_and_preserve_normal_sheets() {
        let palette = |container, inset, text, dim, muted| Palette::resolve(|key| match key {
            "color_surface_container" => Some(container),
            "color_inset" => Some(inset),
            "color_text" => Some(text),
            "color_on_surface_variant" => Some(dim),
            "color_text_disabled" => Some(muted),
            _ => None,
        }, false);
        let yellow = palette(0xd4a234ff, 0x252527ff, 0x1c1a14ff, 0x2e2616ff, 0x7a6630ff);
        assert_eq!(yellow.inks.text, 0x1c1a14ff, "safety-yellow keeps its dark body text on yellow");
        assert_ne!(yellow.inks.well_text, yellow.inks.well_dim, "safety-yellow's inverted well keeps body and dim apart");
        assert!(reads_on(0x272729ff, yellow.inks.well_text) >= 9.0, "safety-yellow well body stays crisp on the sy_field gradient");
        assert!(reads_on(0x272729ff, yellow.inks.well_dim) >= 5.0, "safety-yellow well dim keeps a margin over 4.5 on the sy_field gradient");
        assert!(reads_on(0x272729ff, yellow.inks.well_muted) >= 3.0, "safety-yellow well graphics clear 3:1 on the sy_field gradient");
        assert_eq!(yellow.grounds.input_fill(), yellow.grounds.inset, "an inverted well's input field paints the inset");
        for (ink, need) in [(yellow.inks.well_text, READABLE), (yellow.inks.well_dim, READABLE), (yellow.inks.well_muted, QUIET)] {
            assert!(reads_on(yellow.grounds.inset, ink) >= need, "safety-yellow's well ink #{:06x} must clear {need}", ink >> 8);
        }

        for (name, inset) in [("knitted", 0xe3d7bdff), ("knitted+kids-toy", 0x7c879eff)] {
            let p = palette(0x2f5c3eff, inset, 0xf1e9d6ff, 0xf1e9d6ff, 0xf1e9d6ff);
            assert!(reads_on(inset, p.inks.well_text) >= READABLE, "{name}: the well text must read");
            assert!(reads_on(0xffffffff, p.inks.well_text) > reads_on(0xffffffff, inset), "{name}: the well text must be darker than the well");
        }
        let normal = palette(0x28282aff, 0x353537ff, 0xf2f2f4ff, 0x98989eff, 0x98989eff);
        assert_eq!(normal.inks.well_text, 0xf2f2f4ff, "a normal dark well keeps its readable body ink");

        let macos = Palette::resolve(|key| match key {
            "color_surface_container" => Some(0xecececff),
            "color_surface_container_high" => Some(0xc9c9c9ff),
            "color_inset" => Some(0xffffffff),
            "prop_field" => Some(0xc8c8c8ff),
            "color_text" => Some(0x242426ff),
            "color_on_surface_variant" => Some(0x00000099),
            "color_text_disabled" => Some(0x77777bff),
            _ => None,
        }, false);
        assert_eq!(macos.grounds.input_fill(), macos.grounds.field, "a same-polarity sheet's input field paints the property field");
        for bg in [macos.grounds.inset, macos.grounds.prop_field_panel] {
            for (ink, need) in [(macos.inks.well_text, READABLE), (macos.inks.well_dim, READABLE), (macos.inks.well_muted, QUIET)] {
                assert!(reads_on(bg, ink) >= need, "macos: well ink #{:06x} on #{:06x} must clear {need}", ink >> 8, bg >> 8);
            }
        }
    }

    #[test]
    fn accent_and_button_hover_fit_on_omarchy_and_safety_yellow() {
        makepad_widgets_sheets::register();
        let mut cx = Cx::new(Box::new(|_, _| {}));
        cx.init_cx_os();
        cx.with_vm(|vm| {
            makepad_widgets_core::script_mod(vm);
            for name in ["omarchy", "safety-yellow"] {
                desktop_style::install(vm, StyleSheet::named(name).unwrap());
                vm.with_reload(makepad_widgets_core::script_mod);
                let p = Palette::read(vm);
                if name == "omarchy" {
                    assert!(reads_on(p.grounds.accent, p.inks.on_accent) >= READABLE, "Omarchy accent");
                    assert!(reads_on(p.grounds.ctrl_selected, p.inks.on_ctrl_selected) >= READABLE, "Omarchy selected control");
                    assert!(reads_on(p.accent_down, p.inks.on_accent) >= READABLE, "Omarchy pressed accent");
                } else {
                    assert_ne!(p.button_hover, p.grounds.button_hover_raw, "safety-yellow needs a lighter hover");
                    assert!(button_face_ok(p.button_hover, &p.grounds, &p.inks), "safety-yellow hover carries chrome words and icons");
                }
            }
        });
    }

    #[test]
    fn widget_face_classifies_custom_terminal_and_desktop() {
        makepad_widgets_sheets::register();
        let mut cx = Cx::new(Box::new(|_, _| {}));
        cx.init_cx_os();
        cx.with_vm(|vm| {
            makepad_widgets_core::script_mod(vm);
            for name in ["safety-yellow", "futuristic-plastic", "kids-toy", "lacquer-red", "terminal", "macos", "macos-dark", "omarchy"] {
                desktop_style::install(vm, StyleSheet::named(name).unwrap());
                vm.capture_errors();
                vm.with_reload(makepad_widgets_core::script_mod);
                let errors = vm.take_errors();
                assert!(errors.is_empty(), "{name}: {errors:?}");
                for widget in [id!(RadioButtonTabFlat), id!(ButtonFlat), id!(ButtonFlatter)] {
                    let expected = match name {
                        "terminal" => Some(FaceMode::SelfColor),
                        "macos" | "macos-dark" | "omarchy" => None,
                        _ if widget == id!(RadioButtonTabFlat) => Some(FaceMode::OwnFace),
                        _ => Some(FaceMode::HostOver),
                    };
                    assert_eq!(desktop_style::widget_face(vm, widget).map(|f| f.mode), expected, "{name}: {widget}");
                }
            }
        });
    }

    #[test]
    fn toned_host_sheets_hit_minimal_dim_and_ten_lstar_body_step() {
        let expected = [
            ("omarchy",      0xaaaaaaff_u32, 0x909090ff_u32, 0x9d9d9dff_u32),
            ("windows-dark", 0xbbbbbbff_u32, 0xa0a0a0ff_u32, 0xaeaeaeff_u32),
            ("macos-dark",   0xc5c5c5ff_u32, 0xaaaaaaff_u32, 0xb8b8b8ff_u32),
            ("ios-dark",     0xaaaaaaff_u32, 0x848484ff_u32, 0x979797ff_u32),
            ("android-dark", 0xaaaaaaff_u32, 0x909090ff_u32, 0x9d9d9dff_u32),
        ];
        for (id, want_body, want_dim, want_tab) in expected {
            let base = StyleSheet::named(id).unwrap_or_else(|| panic!("missing sheet {id}"));
            let grounds = toned_grounds(&base).neutral_chrome();
            let out = toned(base);
            let body = out.theme_color("color_text").expect("toned sets color_text");
            let dim = out.theme_color("color_on_surface_variant").expect("toned sets dim");
            assert_eq!(body, want_body, "{id}: body expected #{:06x}, got #{:06x}", want_body >> 8, body >> 8);
            assert_eq!(dim, want_dim, "{id}: dim expected #{:06x}, got #{:06x}", want_dim >> 8, dim >> 8);
            assert_eq!(out.theme_color("color_text_disabled"), Some(dim), "{id}: disabled ink");
            assert_eq!(out.theme_color("color_text_meta"), Some(dim), "{id}: meta ink");
            let (bg_g, dm_g) = ((body >> 24) as u8, (dim >> 24) as u8);
            assert_eq!(body, grey_rgba(bg_g), "{id}: body must stay neutral grey");
            assert_eq!(dim, grey_rgba(dm_g), "{id}: dim must stay neutral grey");
            assert!(dm_g >= TEXT_DIM_FLOOR && bg_g >= TEXT_REST_FLOOR);
            assert!(grey_lstar(bg_g) - grey_lstar(dm_g) >= 9.7, "{id}: body/dim gap collapsed");
            // Minimal dim: one step darker fails READABLE on at least one neutral ground.
            for &(_, bg) in &grounds {
                assert!(reads_on(bg, dim) >= READABLE, "{id}: dim under {READABLE} on #{:06x}", bg >> 8);
                assert!(reads_on(bg, body) >= READABLE, "{id}: body under {READABLE} on #{:06x}", bg >> 8);
            }
            if dm_g > TEXT_DIM_FLOOR {
                let dimmer = grey_rgba(dm_g - 1);
                assert!(
                    grounds.iter().any(|&(_, bg)| reads_on(bg, dimmer) < READABLE),
                    "{id}: dim #{:06x} is not minimal",
                    dim >> 8
                );
            }
            let tab = mix_rgb(dim, body, 0.5);
            assert_eq!(tab, want_tab, "{id}: inactive tab");
            let header = grounds.iter().find(|&&(n, _)| n == "header").unwrap().1;
            assert!(reads_on(header, tab) >= READABLE, "{id}: inactive tab under {READABLE}");
        }
        for id in ["macos", "windows", "ios", "android"] {
            let base = StyleSheet::named(id).unwrap();
            let before = base.theme.clone();
            assert_eq!(toned(base).theme, before, "{id}: light sheet must stay untoned");
        }
    }

    #[test]
    fn pick_flat_ink_measures_contrast_on_painted_faces() {
        // Concrete resting pocket (#898884): body (#161615 = 5.11:1) stays; dim (#2a2927 = 4.10:1)
        // falls back to the sheet's stamped ink (#161615).
        let concrete_bases = [0xc7c6c1ff, 0xf6f5efff, 0xd1d0cbff];
        assert_eq!(pick_flat_ink(concrete_bases, 0x898884ff, Some(0x161615ff), 0x161615ff, 0x161615ff), 0x161615ff);
        assert_eq!(pick_flat_ink(concrete_bases, 0x898884ff, Some(0x161615ff), 0x2a2927ff, 0x161615ff), 0x161615ff);

        // Knitted pressed yarn (#6f727b): Fab cream (#f3ecdd = 4.09:1) is same polarity as white
        // yet under 4.5:1, so the sheet's white (#ffffff = 4.81:1) wins.
        let knitted_bases = [0x305f40ff, 0x3b744eff, 0x336544ff];
        assert_eq!(pick_flat_ink(knitted_bases, 0x6f727bff, Some(0xffffffff), 0xf3ecddff, 0xf3ecddff), 0xffffffff);

        // Kids-toy hover/focus (#e0dfd9): light body fails -> sheet dark ink (#1c2438) wins.
        let kids_bases = [0x0f7d7bff, 0x108482ff, 0x386fbfff];
        assert_eq!(pick_flat_ink(kids_bases, 0xe0dfd9ff, Some(0x1c2438ff), 0xf6f8ffff, 0xf6f8ffff), 0x1c2438ff);

        // When both dim and sheet_ink fall short on a ground, body_ink is chosen before fitting.
        assert_eq!(pick_flat_ink(concrete_bases, 0x898884ff, Some(0x2a2927ff), 0x2a2927ff, 0x161615ff), 0x161615ff);

        // When Fab ink, sheet ink and body ink all fall short, ink_reading fits past 4.5:1.
        let mid = [0x767676ff, 0x767676ff, 0x767676ff];
        let fitted = pick_flat_ink(mid, 0x767676ff, Some(0xddddddff), 0xddddddff, 0xddddddff);
        assert!(reads_on(0x767676ff, fitted) >= READABLE, "fitted #{:06x} must clear 4.5:1", fitted >> 8);
    }
}
