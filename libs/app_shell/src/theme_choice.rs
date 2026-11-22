//! A product's UI theme (Stage's, Story's): the house theme (the house
//! palette, no sheet) or one of the themes in the catalogue, the open and
//! commercial ones alike, and the picker that chooses it ([`ThemePicker`]:
//! the caption's theme button opens it in `mod.widgets.ThemePickerPanel`).
//!
//! The choice goes on through the library's one path -- `desktop_style`'s
//! install and a theme reload -- so the Tweaker, the window manager and this
//! picker all move the same thing, and `makepad_look::palette` recolours Stage's
//! own roles from whatever is in force. A sheet with a dark variant is one
//! entry in the list, with its light or dark appearance picked beside it.
//!
//! The choice is kept between launches in `theme.txt` under Stage's user
//! state (a hidden instance's throwaway folder, see `user_state`): one line,
//! the sheet's id, or `house`.

use makepad_widgets::desktop_style::{self, SheetEntry, StyleSheet};
use makepad_widgets::*;
use std::path::PathBuf;

/// What the list calls the products' own theme.
pub const HOUSE_LABEL: &str = "House theme";
const HOUSE_ID: &str = "house";

/// The entries the picker lists after the house theme: every sheet that is
/// not another's dark variant, which the appearance beside it reaches.
pub fn picks() -> Vec<&'static SheetEntry> {
    desktop_style::picks()
}

/// The picker's labels: the house theme, then every pick.
pub fn labels() -> Vec<String> {
    std::iter::once(HOUSE_LABEL.to_string()).chain(picks().into_iter().map(|entry| entry.label.to_string())).collect()
}

/// Rows in the picker: the house theme and every pick.
pub fn rows() -> usize {
    picks().len() + 1
}

/// The picker's button for a row.
pub fn row_id(row: usize) -> LiveId {
    LiveId::from_str(&format!("theme_row_{row}"))
}

/// `mod.widgets.StageThemeList`: one button per row, wrapped, so every
/// theme is in sight at once and one click wears it. Built from the
/// catalogue, which is complete by the time the app's modules run.
pub fn script_mod(vm: &mut ScriptVm) {
    let mut code = String::from("use mod.prelude.widgets.*\nuse mod.widgets.*\nmod.widgets.StageThemeList = View{width: Fill height: Fit flow: Flow.Right{wrap: true} spacing: 6\n");
    for (row, label) in labels().iter().enumerate() {
        code.push_str(&format!("    theme_row_{row} := StageButton{{height: 24 margin: Inset{{bottom: 6}} text: {label:?}}}\n"));
    }
    code.push_str("}\ntrue\n");
    vm.eval(ScriptMod::new(&(env!("CARGO_MANIFEST_DIR")), "stage_theme_list", "stage_theme_list.splash", 0, 0, &(code), &[]));
    picker::script_mod(vm);
}

mod picker {
    use makepad_widgets::*;

    script_mod! {
        use mod.prelude.widgets.*
        use mod.widgets.*
        let stage = mod.stage
        // The theme picker: every theme in sight, one click wears it; the
        // appearance beside the title for a theme that has both.
        mod.widgets.ThemePickerPanel = Modal{can_dismiss: true
            content +: {width: 460 height: Fit flow: Down spacing: 12 padding: 18 show_bg: true
                draw_bg +: {
                    color: stage.panel
                    pixel: fn(){
                        let sdf = Sdf2d.viewport(self.pos * self.rect_size)
                        sdf.box(0., 0., self.rect_size.x, self.rect_size.y, 8.)
                        sdf.fill(vec4(self.color.xyz, 1.))
                        return sdf.result
                    }
                }
                View{width: Fill height: Fit flow: Right spacing: 8 align: Align{y: 0.5}
                    Label{width: Fill text: "Theme" draw_text +: {color: stage.text_bright text_style: theme.font_bold{font_size: 14}}}
                    Tip{text: "Light or dark, for a theme that has both"
                        theme_picker_mode := DropDown{width: 84 labels: ["Light" "Dark"]}}
                    theme_picker_close := StageButton{width: 56 height: 24 text: "Close"}
                }
                Label{width: Fill height: Fit text: "Applies at once; kept for the next launch." draw_text +: {color: stage.label text_style: theme.font_regular{font_size: 11}}}
                theme_picker_list := mod.widgets.StageThemeList{}
            }
        }
    }
}

/// The theme picker in a product window: the window's template has a
/// `theme_picker := mod.widgets.ThemePickerPanel{}` and a button that opens
/// it ([`ThemePicker::open`]). Main-loop cost: one palette generation compare
/// per [`ThemePicker::sync`] while the panel is open; nothing while closed.
#[derive(Default)]
pub struct ThemePicker {
    /// The palette generation the rows were last painted for: a style
    /// reload puts the markup's labels back.
    painted: Option<u64>,
}

impl ThemePicker {
    pub fn open(&mut self, cx: &mut Cx, ui: &WidgetRef) {
        ui.modal(cx, ids!(theme_picker)).open(cx);
        self.painted = None;
        self.sync(cx, ui);
    }

    /// The lit row and appearance, from the sheet in force (while open).
    pub fn sync(&mut self, cx: &mut Cx, ui: &WidgetRef) {
        if !ui.modal(cx, ids!(theme_picker)).is_open() {
            return;
        }
        let generation = makepad_look::palette::generation(cx);
        if self.painted == Some(generation) {
            return;
        }
        self.painted = Some(generation);
        let current = current(cx);
        let (row, dark) = position(current.as_deref());
        // The one worn is ticked and in the accent: words, since some
        // sheets draw every button face in their own grey whatever is painted.
        let palette = makepad_look::palette::get(cx);
        for (index, label) in labels().iter().enumerate() {
            let ink = Vec4f::from_u32(if index == row { makepad_look::palette::on_page(cx, palette.accent) } else { palette.label });
            let mut widget = ui.widget(cx, &[row_id(index)]);
            widget.set_text(cx, &if index == row { format!("✓ {label}") } else { label.clone() });
            script_apply_eval!(cx, widget, {draw_text +: {color: #(ink) color_focus: #(ink)}});
        }
        ui.drop_down(cx, ids!(theme_picker_mode)).set_selected_item(cx, dark as usize);
        ui.widget(cx, ids!(theme_picker_mode)).set_disabled(cx, !has_dark(row));
    }

    /// The panel's clicks: a row or an appearance wears that theme. Says
    /// what it put on (its label), for the product's status line.
    pub fn handle_actions(&mut self, cx: &mut Cx, ui: &WidgetRef, actions: &Actions) -> Option<&'static str> {
        if !ui.modal(cx, ids!(theme_picker)).is_open() {
            return None;
        }
        if ui.button(cx, ids!(theme_picker_close)).clicked(actions) {
            ui.modal(cx, ids!(theme_picker)).close(cx);
            return None;
        }
        let row = (0..rows()).find(|index| ui.button(cx, &[row_id(*index)]).clicked(actions));
        let dark = ui.drop_down(cx, ids!(theme_picker_mode)).selected(actions);
        if row.is_none() && dark.is_none() {
            return None;
        }
        let row = row.unwrap_or_else(|| position(current(cx).as_deref()).0);
        let dark = dark.unwrap_or_else(|| ui.drop_down(cx, ids!(theme_picker_mode)).selected_item()) == 1;
        let entry = entry_at(row, dark);
        choose(cx, entry);
        self.painted = None;
        Some(entry.map_or(HOUSE_LABEL, |entry| entry.label))
    }
}

/// Where a sheet id sits in the picker: its row (0 the house theme) and
/// whether it is the dark appearance. An id the catalogue does not know
/// reads as the house theme.
pub fn position(id: Option<&str>) -> (usize, bool) {
    let Some(entry) = id.and_then(desktop_style::find) else { return (0, false) };
    let light = entry.light();
    let row = picks().iter().position(|pick| std::ptr::eq(*pick, light)).map_or(0, |index| index + 1);
    (row, row > 0 && entry.is_dark_variant())
}

/// The sheet a picker row and appearance name: `None` for the house theme.
pub fn entry_at(row: usize, dark: bool) -> Option<&'static SheetEntry> {
    let pick = *picks().get(row.checked_sub(1)?)?;
    Some(pick.with_appearance(dark))
}

/// Whether the row has a dark appearance to offer.
pub fn has_dark(row: usize) -> bool {
    row.checked_sub(1).and_then(|index| picks().get(index).copied()).is_some_and(|pick| pick.dark.is_some())
}

fn file() -> PathBuf {
    crate::user_state::dir(None, "theme.txt")
}

/// The saved choice: `Some(None)` the house theme, `Some(Some(id))` a sheet,
/// `None` nothing saved (or a sheet this build no longer has).
pub fn parse(text: &str) -> Option<Option<&'static SheetEntry>> {
    match text.trim() {
        HOUSE_ID => Some(None),
        id => desktop_style::find(id).map(Some),
    }
}

pub fn saved() -> Option<Option<&'static SheetEntry>> {
    parse(&std::fs::read_to_string(file()).ok()?)
}

fn save(entry: Option<&'static SheetEntry>) {
    let path = file();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Err(error) = std::fs::write(&path, format!("{}\n", entry.map_or(HOUSE_ID, |entry| entry.id))) {
        error!("stage: could not keep the theme in {}: {error}", path.display());
    }
}

/// Put the saved choice on before the first theme load: the house theme or
/// a sheet, picked, so it stands over the window manager's look (handed down
/// as `MAKEPAD_WIDGET_STYLE` and sent again as it changes). With nothing
/// saved Stage wears the system's look. Only the first call does anything:
/// later module runs are theme reloads, and whatever is installed by then
/// (this picker's, the Tweaker's, the WM's) stands.
pub fn install_saved(vm: &mut ScriptVm) {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| match saved() {
        Some(Some(entry)) => desktop_style::install(vm, StyleSheet::load(entry)),
        Some(None) => {
            desktop_style::uninstall(vm);
            set_app_theme_tokens(vm.cx_mut(), Some(house_tokens()));
        }
        None => {}
    });
}

/// The house theme in the library's tokens: the menus, popups, drop down lists and
/// tooltips Stage opens (the AI chat's provider menu, DECK's talk switcher) and the
/// library's control faces wear the house look, not the plain base theme. The same
/// roles the palette maps to library tokens (`Kind::Token`), plus the menu surface, its
/// edges and the accent.
pub fn house_tokens() -> String {
    use makepad_look::palette::Palette;
    let h = Palette::HOUSE;
    let c = |v: u32| format!("#{v:08x}");
    [
        ("color_fg_app", h.menu),
        ("color_bevel", h.edge_lo),
        ("color_bevel_outset_1", h.edge),
        ("color_bevel_outset_2", h.edge_lo),
        ("color_inset", h.well),
        ("color_outset", h.control),
        ("color_outset_hover", h.control_hover),
        ("color_outset_2_hover", h.control_hover),
        ("color_outset_down", h.control_down),
        ("color_outset_active", h.accent_down),
        ("color_outset_2_active", h.accent_down),
        ("color_mark_active", h.accent),
        ("color_label_inner", h.label),
        ("color_label_inner_hover", h.text_hi),
        ("color_label_inner_active", h.text_max),
        // A row that cannot be picked is just dimmer words on the menu, no face.
        ("color_outset_disabled", h.menu),
        ("color_outset_2_disabled", h.menu),
        ("color_label_inner_disabled", h.dim),
    ]
    .iter()
    .map(|(token, value)| format!("mod.theme.{token} = {}\n", c(*value)))
    .collect()
}

/// The sheet in force, by id.
pub fn current(cx: &mut Cx) -> Option<String> {
    cx.with_vm(desktop_style::current_name)
}

/// Wear `entry` (`None`: the house theme) from now on and keep it. A pick is
/// somebody saying which theme the app wears, so the base goes back to the
/// dark one the house theme is drawn on, and a standing Tweaker mix or token
/// edit comes off with it (`set_base_theme`).
pub fn choose(cx: &mut Cx, entry: Option<&'static SheetEntry>) {
    set_base_theme(cx, BaseTheme::Dark);
    cx.with_vm(|vm| match entry {
        Some(entry) => desktop_style::install(vm, StyleSheet::load(entry)),
        None => desktop_style::uninstall(vm),
    });
    // A sheet brings its own tokens; the house theme lays its own over the base.
    set_app_theme_tokens(cx, entry.is_none().then(house_tokens));
    cx.request_style_reload();
    save(entry);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_picker_lists_the_house_theme_then_every_light_sheet() {
        makepad_widgets_sheets::register();
        let labels = labels();
        assert_eq!(labels[0], HOUSE_LABEL);
        assert!(labels.iter().any(|label| label == "Neumorphic"));
        assert!(labels.iter().any(|label| label == "Steampunk brass"));
        assert!(labels.iter().any(|label| label == "macOS"));
        assert!(!labels.iter().any(|label| label == "Neumorphic dark"), "a dark variant is reached by the appearance");
        assert_eq!(labels.len(), picks().len() + 1);
    }

    #[test]
    fn a_sheet_and_its_dark_variant_share_a_row() {
        makepad_widgets_sheets::register();
        assert_eq!(position(None), (0, false));
        assert_eq!(position(Some("no-such-sheet")), (0, false));
        let (row, dark) = position(Some("neumorphic"));
        assert!(row > 0 && !dark);
        assert_eq!(position(Some("neumorphic-dark")), (row, true));
        assert!(has_dark(row));
        assert_eq!(entry_at(row, false).unwrap().id, "neumorphic");
        assert_eq!(entry_at(row, true).unwrap().id, "neumorphic-dark");
        // A sheet without a dark variant stays itself either way.
        let (brass, _) = position(Some("steampunk-brass"));
        assert!(!has_dark(brass));
        assert_eq!(entry_at(brass, true).unwrap().id, "steampunk-brass");
        assert!(entry_at(0, true).is_none());
        assert!(!has_dark(0));
    }

    #[test]
    fn the_saved_choice_reads_back() {
        makepad_widgets_sheets::register();
        assert_eq!(parse("house\n").map(|entry| entry.map(|entry| entry.id)), Some(None));
        assert_eq!(parse("terminal\n").map(|entry| entry.map(|entry| entry.id)), Some(Some("terminal")));
        assert_eq!(parse("macos-dark").map(|entry| entry.map(|entry| entry.id)), Some(Some("macos-dark")));
        assert!(parse("gone").is_none());
        assert!(parse("").is_none());
    }
}
