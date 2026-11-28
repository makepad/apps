//! Computed contrast guard and census over every registered style sheet.
//! No app launch, no GPU, no window; resolves all 51 catalogue sheets + 3 bases via
//! `ThemeResolver` and all 5 host dark sheets via `toned()` in a headless `Cx`.

use fab::theme::{toned, Palette, HOST_FAMILIES};
use makepad_widgets_core::desktop_style::{self, StyleSheet};
use makepad_widgets_core::theme_tokens::{
    over, reads_on, random_weights, sheet_base, Appearance, BlendCache, BlendTheme, ThemeResolver,
    LEGIBLE, READABLE,
};
use makepad_widgets_core::{id, BaseTheme, Cx, CxOsApi, LiveId, ScriptVm, ScriptVmCx};
use std::collections::BTreeSet;
use std::sync::OnceLock;

fn register_all_sheets() {
    // Mirrors `apps/apps/fab/app/src/main.rs`: update here if Fab registers another sheet pack.
    makepad_widgets_sheets::register();
}

fn resolved_cache() -> &'static BlendCache {
    static CACHE: OnceLock<BlendCache> = OnceLock::new();
    CACHE.get_or_init(|| {
        register_all_sheets();
        let mut resolver = ThemeResolver::new(BlendTheme::all());
        while resolver.step() {}
        assert!(resolver.errors().is_empty(), "ThemeResolver errors: {:?}", resolver.errors());
        resolver.into_cache()
    })
}

fn resolve_sheet_in_vm(sheet: StyleSheet) -> Palette {
    let mut cx = Cx::new(Box::new(|_, _| {}));
    cx.with_vm(|vm: &mut ScriptVm| {
        makepad_widgets_core::theme_mod(vm);
        desktop_style::install(vm, sheet.clone());
        vm.capture_errors();
        desktop_style::apply_theme(vm);
        let errors = vm.take_errors();
        assert!(errors.is_empty(), "{}: apply_theme errors: {errors:?}", sheet.name);
        let palette = Palette::read(vm);
        desktop_style::uninstall(vm);
        palette
    })
}

fn hex(rgba: u32) -> String {
    if (rgba & 0xff) == 0xff {
        format!("#{:06x}", rgba >> 8)
    } else {
        format!("#{:08x}", rgba)
    }
}

fn check_palette(sheet_id: &str, appearance: Appearance, p: &Palette, out: &mut Vec<String>) {
    let app = match appearance {
        Appearance::Dark => "dark",
        Appearance::Light => "light",
    };
    let chrome = p.grounds.chrome_surfaces();
    let kind_grounds = [
        ("panel", p.grounds.panel),
        ("toolbar", p.grounds.toolbar),
        ("high", p.grounds.high),
        ("selection", p.grounds.selection),
        ("prop_field(panel)", p.grounds.prop_field_panel),
        ("prop_field(toolbar)", p.grounds.prop_field_toolbar),
    ];
    let tab_grounds = [("header", p.grounds.header), ("toolbar", p.grounds.toolbar)];
    let graphics = p.grounds.graphics_surfaces();
    let well_all = [
        ("inset", p.grounds.inset),
        ("prop_field(panel)", p.grounds.prop_field_panel),
        ("prop_field(toolbar)", p.grounds.prop_field_toolbar),
        ("prop_field_hover(panel)", p.grounds.prop_field_hover_panel),
        ("prop_field_hover(toolbar)", p.grounds.prop_field_hover_toolbar),
    ];
    let well: &[(&str, u32)] = if p.grounds.inset_matches_chrome() {
        &well_all
    } else {
        &well_all[..1]
    };
    let accent = [("accent", p.grounds.accent)];
    let selected = [("ctrl_selected", p.grounds.ctrl_selected)];
    let choice = [("choice_on", p.choice_on)];
    let accent_down = [("accent_down", p.accent_down)];
    let bases = [("panel", p.grounds.panel), ("toolbar", p.grounds.toolbar), ("high", p.grounds.high)];
    let wash85 = (p.button_hover & 0xffff_ff00) | (((p.button_hover & 255) as f64 * 0.85).round() as u32);
    let mut hover = bases.map(|(name, bg)| (name, over(bg, p.button_hover))).to_vec();
    hover.extend([("panel@85%", p.grounds.panel), ("toolbar@85%", p.grounds.toolbar), ("high@85%", p.grounds.high)]
        .map(|(name, bg)| (name, over(bg, wash85))));
    let down = bases.map(|(name, bg)| (name, over(bg, p.button_down)));
    let mut checks: Vec<(&str, u32, &[(&str, u32)], f64)> = vec![
        ("body", p.inks.text, &chrome, READABLE),
        ("dim", p.inks.dim, &chrome, READABLE),
        ("kind", p.inks.dim, &kind_grounds, READABLE),
        ("inactive_tab", p.inactive_tab_ink(), &tab_grounds, READABLE),
        ("add_chevron", p.inks.muted, &graphics, LEGIBLE),
        ("well_body", p.inks.well_text, well, READABLE),
        ("well_dim", p.inks.well_dim, well, READABLE),
        ("well_graphic", p.inks.well_muted, well, LEGIBLE),
        ("on_accent", p.inks.on_accent, &accent, READABLE),
        ("on_ctrl_sel", p.inks.on_ctrl_selected, &selected, READABLE),
        ("seg_active", p.inks.segment_active, &choice, if p.toned { LEGIBLE } else { READABLE }),
        ("btn_hover", p.inks.text, &hover, READABLE),
        ("btn_hover_muted", p.inks.muted, &hover, LEGIBLE),
        ("btn_down", p.inks.text, &down, if p.toned { LEGIBLE } else { READABLE }),
        ("btn_down_muted", p.inks.muted, &down, LEGIBLE),
    ];
    if !p.toned {
        checks.push(("accent_down", p.inks.on_accent, &accent_down, READABLE));
        checks.push(("btn_hover_dim", p.inks.dim, &hover, READABLE));
        checks.push(("btn_down_dim", p.inks.dim, &down, READABLE));
    }
    for (element, ink, grounds, need) in checks {
        let mut bad: Vec<(&str, u32, f64)> = grounds
            .iter()
            .filter_map(|&(gname, bg)| {
                let r = reads_on(bg, ink);
                (r < need).then_some((gname, bg, r))
            })
            .collect();
        if bad.is_empty() {
            continue;
        }
        bad.sort_by(|a, b| a.2.total_cmp(&b.2));
        let (worst_name, worst_bg, worst_r) = bad[0];
        let extra = if bad.len() > 1 {
            let names: Vec<&str> = bad[1..].iter().map(|b| b.0).collect();
            format!(" (+{} more: {})", bad.len() - 1, names.join(", "))
        } else {
            String::new()
        };
        out.push(format!(
            "{sheet_id:<22} {app:<5} {element:<12} ink={} on {worst_name}={} ratio={worst_r:.2}:1 need={need:.1}:1{extra}",
            hex(ink),
            hex(worst_bg),
        ));
    }
}

fn assert_census(label: &str, total_sheets: usize, failures: &[String]) {
    if failures.is_empty() {
        return;
    }
    let failing_sheets: BTreeSet<&str> = failures
        .iter()
        .filter_map(|line| line.split_whitespace().next())
        .collect();
    panic!(
        "{label}: {} of {total_sheets} sheets fell short ({} failing sheet/element rows):\n{}",
        failing_sheets.len(),
        failures.len(),
        failures.join("\n")
    );
}

#[test]
fn registry_and_resolver_parity() {
    let cache = resolved_cache();
    let catalogue = desktop_style::catalogue();
    assert!(catalogue.len() >= 51, "expected >= 51 registered sheets, got {}", catalogue.len());
    // Every (pick, appearance) pair maps onto the catalogue, and every catalogue entry has a base.
    let mut from_picks = BTreeSet::new();
    for pick in desktop_style::picks() {
        from_picks.insert(pick.with_appearance(false).id);
        from_picks.insert(pick.with_appearance(true).id);
    }
    let from_catalogue: BTreeSet<&str> = catalogue.iter().map(|e| e.id).collect();
    assert_eq!(from_picks, from_catalogue, "picks() x appearances must cover the whole catalogue");
    for entry in &catalogue {
        assert!(sheet_base(entry).is_some(), "{} names no base theme", entry.id);
        assert!(cache.get(BlendTheme::Sheet(entry.id)).is_some(), "{} missing from ThemeResolver", entry.id);
    }
    // `ThemeResolver` matches live `theme_mod` + `apply_theme` + `Palette::read`.
    for id in ["macos", "macos-dark", "omarchy", "kids-toy", "safety-yellow"] {
        let vm_pal = resolve_sheet_in_vm(StyleSheet::named(id).unwrap());
        let values = cache.get(BlendTheme::Sheet(id)).unwrap();
        let pure_pal = Palette::resolve(|k| values.color(k), false);
        assert_eq!(vm_pal, pure_pal, "{id}: ThemeResolver diverged from live VM Palette::read");
    }
}

#[test]
fn host_sheets_meet_contrast() {
    register_all_sheets();
    let mut failures = Vec::new();
    let mut checked = 0usize;
    for family in HOST_FAMILIES {
        let dark_entry = family.sheet(true);
        let sheet = toned(StyleSheet::load(dark_entry));
        let pal = resolve_sheet_in_vm(sheet);
        let app = BlendTheme::Sheet(dark_entry.id).appearance();
        check_palette(&format!("host:{}", dark_entry.id), app, &pal, &mut failures);
        checked += 1;

        // Light host sheets must remain untoned.
        let light_entry = family.sheet(false);
        if light_entry.id != dark_entry.id {
            let raw = StyleSheet::load(light_entry);
            let before = raw.theme.clone();
            assert_eq!(toned(raw).theme, before, "{}: light host sheet was modified by toned()", light_entry.id);
        }
    }
    assert_census("host sheets (toned)", checked, &failures);
}

#[test]
fn sheetless_install_clears_fab_toned() {
    fn read_vm_palette(vm: &mut ScriptVm, toned: bool) -> Palette {
        let theme = vm.module(id!(theme));
        Palette::resolve(|key| vm.value(theme, LiveId::from_str(key)).as_color(), toned)
    }

    register_all_sheets();
    let blend = resolved_cache().blend(&[(BlendTheme::Sheet("macos-dark"), 50.0), (BlendTheme::Sheet("omarchy"), 50.0)]).unwrap();
    let mut cx = Cx::new(Box::new(|_, _| {}));
    cx.with_vm(|vm| {
        // wear_host_sheet tracks its fallback sheet per heap (desktop_style), so this Cx is its own.
        let run_fab = |vm: &mut ScriptVm| {
            fab::theme::wear_host_sheet(vm);
            makepad_widgets_core::theme_mod(vm);
            desktop_style::apply_theme(vm);
        };
        vm.capture_errors();
        run_fab(vm);
        let errors = vm.take_errors();
        assert!(errors.is_empty(), "host startup errors: {errors:?}");
        let theme = vm.module(id!(theme));
        assert!(vm.value(theme, id!(fab_toned)).as_number().is_some(), "host startup must set fab_toned");
        assert_eq!(Palette::read(vm), read_vm_palette(vm, true), "host startup must read the toned palette");
        let worn = desktop_style::current(vm).expect("host sheet worn");

        for (case, preset) in [("mix", None), ("light preset", Some(BaseTheme::Light)), ("dark preset", Some(BaseTheme::Dark))] {
            if let Some(base) = preset {
                makepad_widgets_core::set_base_theme(vm.cx_mut(), BaseTheme::Dark);
                desktop_style::install(vm, worn.clone());
                vm.capture_errors();
                vm.with_reload(|vm| {
                    makepad_widgets_core::theme_mod(vm);
                    desktop_style::apply_theme(vm);
                });
                let errors = vm.take_errors();
                assert!(errors.is_empty(), "{case}: host reset errors: {errors:?}");
                let theme = vm.module(id!(theme));
                assert!(vm.value(theme, id!(fab_toned)).as_number().is_some(), "{case}: host reset must set fab_toned");

                makepad_widgets_core::set_base_theme(vm.cx_mut(), base);
                desktop_style::uninstall(vm);
            } else {
                // The theme lab's mix and the theme builder both uninstall the sheet and set a mix.
                desktop_style::uninstall(vm);
                makepad_widgets_core::set_theme_mix(vm.cx_mut(), Some(blend.script("fab_census_mix")));
            }
            vm.capture_errors();
            vm.with_reload(run_fab);
            let errors = vm.take_errors();
            assert!(errors.is_empty(), "{case}: reload errors: {errors:?}");
            let theme = vm.module(id!(theme));
            let marker = vm.value(theme, id!(fab_toned));
            let sheet = desktop_style::current_name(vm);
            assert!(marker.is_nil() || marker.is_err(), "{case}: expected fab_toned to be absent after reload, got {marker:?} with sheet {sheet:?}");
            let plain = read_vm_palette(vm, false);
            assert_ne!(read_vm_palette(vm, true), plain, "{case}: a leaked marker must change the palette");
            let palette = Palette::read(vm);
            assert_eq!(palette, plain, "{case}: expected the untoned palette");
            if preset.is_none() {
                assert_eq!(palette, Palette::resolve(|key| blend.color(key), false), "{case}: expected the installed mix");
            }
            assert!(sheet.is_none(), "{case}: host sheet was worn again: {sheet:?}");
        }
    });
}

#[test]
fn picked_sheets_meet_contrast() {
    let cache = resolved_cache();
    let catalogue = desktop_style::catalogue();
    let mut failures = Vec::new();
    for entry in &catalogue {
        let theme = BlendTheme::Sheet(entry.id);
        let values = cache.get(theme).unwrap_or_else(|| panic!("missing {}", entry.id));
        let pal = Palette::resolve(|k| values.color(k), false);
        check_palette(entry.id, theme.appearance(), &pal, &mut failures);
    }
    assert_census("picked sheets (Inks::fit)", catalogue.len(), &failures);
}

#[test]
fn blended_sheets_meet_contrast() {
    let cache = resolved_cache();
    let mut failures = Vec::new();
    let mut checked = 0usize;
    for appearance in Appearance::ALL {
        let group = BlendTheme::group(appearance);
        // Select <= 6 extremal vertices without an O(N^2) sweep:
        // 2 worst-luminance neutral grounds + 3 channel extrema on `panel` + base theme.
        let mut vertices: Vec<BlendTheme> = Vec::new();
        let mut push_uniq = |t: BlendTheme| {
            if !vertices.contains(&t) {
                vertices.push(t);
            }
        };
        let score_ground = |t: BlendTheme| -> f64 {
            let v = cache.get(t).unwrap();
            let g = Palette::resolve(|k| v.color(k), false).grounds;
            let ref_ink = if appearance == Appearance::Dark { 0xffffffff } else { 0x000000ff };
            g.neutral_chrome().into_iter().map(|(_, bg)| reads_on(bg, ref_ink)).fold(f64::MAX, f64::min)
        };
        let mut by_margin = group.clone();
        by_margin.sort_by(|a, b| score_ground(*a).total_cmp(&score_ground(*b)));
        for &t in by_margin.iter().take(2) {
            push_uniq(t);
        }
        for shift in [24u32, 16, 8] {
            let ext = group.iter().copied().min_by_key(|&t| {
                let p = Palette::resolve(|k| cache.get(t).unwrap().color(k), false).grounds.panel;
                let ch = (p >> shift) & 0xff;
                if appearance == Appearance::Dark { 255 - ch } else { ch }
            });
            if let Some(t) = ext {
                push_uniq(t);
            }
        }
        // 1. Extremal-vertex 50/50 pairs (<= 10..15 pairs).
        for (i, &a) in vertices.iter().enumerate() {
            for &b in &vertices[i + 1..] {
                let blend = cache.blend(&[(a, 50.0), (b, 50.0)]).unwrap();
                let pal = Palette::resolve(|k| blend.color(k), false);
                check_palette(&format!("mix:{}+{}", a.name(), b.name()), appearance, &pal, &mut failures);
                checked += 1;
            }
        }
        // 2. Group equal-weight centroid.
        let centroid: Vec<(BlendTheme, f64)> = group.iter().copied().map(|t| (t, 1.0)).collect();
        let blend = cache.blend(&centroid).unwrap();
        let pal = Palette::resolve(|k| blend.color(k), false);
        check_palette(&format!("mix:centroid-{}", appearance.label().to_lowercase()), appearance, &pal, &mut failures);
        checked += 1;
        // 3. 16 deterministic multi-theme mixes.
        for seed in 0..16u64 {
            let w = random_weights(seed, group.len());
            let mix: Vec<(BlendTheme, f64)> = group.iter().copied().zip(w).collect();
            let blend = cache.blend(&mix).unwrap();
            let pal = Palette::resolve(|k| blend.color(k), false);
            check_palette(&format!("mix:{}-seed{seed}", appearance.label().to_lowercase()), appearance, &pal, &mut failures);
            checked += 1;
        }
    }
    assert_census("blended sheets", checked, &failures);
}

#[test]
fn input_tokens_follow_divergent_text_input_sheets() {
    register_all_sheets();
    // Representative painted `TextInputFlat` well ground per sheet (from each sheet's `widgets.splash`
    // `*_field` / `*_well` shader, or `prop_field(toolbar)` for core `self.color` sheets).
    let cases = [
        ("safety-yellow", 0x272729ff_u32), // sy_field lower gradient
        ("mecha-light",   0x2f3b4eff_u32), // mk_field lower gradient
        ("knitted",       0xe3d7bdff_u32), // mk_field cream patch
        ("concrete",      0x454442ff_u32), // cc_field charcoal pocket
        ("handheld",      0x2f4836ff_u32), // pa_well dark LCD
        ("pop-art",       0x0e1b2eff_u32), // pa_field pa_col(7.0) navy
        ("macos",         0xc8c8c8ff_u32), // core TextInputFlat prop_field
        ("macos-dark",    0x454547ff_u32), // core TextInputFlat prop_field
    ];
    let mut cx = Cx::new(Box::new(|_, _| {}));
    cx.init_cx_os();
    cx.with_vm(|vm: &mut ScriptVm| {
        makepad_widgets_core::script_mod(vm);
        fab::theme::script_mod(vm);
    });
    for (id, painted_bg) in cases {
        cx.with_vm(|vm: &mut ScriptVm| {
            desktop_style::install(vm, StyleSheet::named(id).unwrap());
            vm.capture_errors();
            vm.with_reload(|vm| {
                makepad_widgets_core::script_mod(vm);
                fab::theme::script_mod(vm);
            });
            let errors = vm.take_errors();
            assert!(errors.is_empty(), "{id}: script_mod errors: {errors:?}");
            let fab = vm.module(id!(fab));
            let text = vm.value(fab, id!(color_text_input)).as_color().expect("color_text_input");
            let dim = vm.value(fab, id!(color_text_input_dim)).as_color().expect("color_text_input_dim");
            let muted = vm.value(fab, id!(color_text_input_muted)).as_color().expect("color_text_input_muted");
            assert!(reads_on(painted_bg, text) >= READABLE, "{id}: input text {} on {} = {:.2}:1", hex(text), hex(painted_bg), reads_on(painted_bg, text));
            assert!(reads_on(painted_bg, dim) >= READABLE, "{id}: input dim {} on {} = {:.2}:1", hex(dim), hex(painted_bg), reads_on(painted_bg, dim));
            assert!(reads_on(painted_bg, muted) >= LEGIBLE, "{id}: input muted {} on {} = {:.2}:1", hex(muted), hex(painted_bg), reads_on(painted_bg, muted));
            desktop_style::uninstall(vm);
        });
    }
}

/// For an opaque sheet face, ButtonFlat.host_over shifts each RGB channel by
/// the difference between the app's requested colour and the sheet's default.
fn host_over_opaque(face: u32, host: u32, default: u32) -> u32 {
    let channel = |c: u32, shift: u32| ((c >> shift) & 255) as i32;
    [24, 16, 8].into_iter().fold(255, |out, shift| {
        let value = channel(face, shift) + channel(host, shift) - channel(default, shift);
        out | ((value.clamp(0, 255) as u32) << shift)
    })
}

fn register_fab_theme(vm: &mut ScriptVm) {
    makepad_widgets_core::script_mod(vm);
    fab::theme::script_mod(vm);
}

#[test]
fn fab_button_and_toggle_tokens_clear_sheet_faces() {
    use desktop_style::{widget_face, FaceMode};

    register_all_sheets();
    let sheets = desktop_style::catalogue();
    let mut cx = Cx::new(Box::new(|_, _| {}));
    cx.init_cx_os();
    cx.with_vm(register_fab_theme);
    let mut failures = Vec::new();
    let mut focus_tinted = 0usize;
    for entry in &sheets {
        cx.with_vm(|vm| {
            desktop_style::install(vm, StyleSheet::load(entry));
            vm.capture_errors();
            vm.with_reload(register_fab_theme);
            let errors = vm.take_errors();
            assert!(errors.is_empty(), "{}: {errors:?}", entry.id);
            let p = Palette::read(vm);
            let fab = vm.module(id!(fab));
            let tab = widget_face(vm, id!(RadioButtonTabFlat));
            let button = widget_face(vm, id!(ButtonFlat));
            let flat = widget_face(vm, id!(ButtonFlatter));
            let widgets = vm.module(id!(widgets));
            let bf = vm.value(widgets, id!(ButtonFlat)).as_object().unwrap();
            let bg = vm.value(bf, id!(draw_bg)).as_object().unwrap();
            let val = vm.value(bg, id!(color_default));
            let default = val.as_color().or_else(|| val.as_object().and_then(|o| vm.heap().proto(o).as_color()));
            let token = |key: &str| vm.value(fab, LiveId::from_str(key)).as_color()
                .unwrap_or_else(|| panic!("{}: missing fab.{key}", entry.id));
            let btn_bg = token("color_btn_bg");
            let accent = token("color_accent");
            let own_button = button.filter(|f| f.mode == FaceMode::HostOver);
            let own_flat = flat.filter(|f| f.mode == FaceMode::HostOver);
            let own_tab = tab.filter(|f| f.mode == FaceMode::OwnFace);
            let press_selects = tab.is_some_and(|f| f.mode == FaceMode::SelfColor && f.down.is_some());
            let mut check = |key: &str, surface: &str, ground: u32, need: f64| {
                let ink = token(key);
                let ratio = reads_on(ground, ink);
                if ratio < need {
                    failures.push(format!("{} {key} {} on {surface}={} = {ratio:.2}:1 (need {need:.1})",
                        entry.id, hex(ink), hex(ground)));
                }
            };
            check("color_text_on_accent", "accent", p.grounds.accent, READABLE);
            if let Some(face) = own_button {
                let default = default.expect("ButtonFlat.draw_bg.color_default");
                assert_eq!(btn_bg, default, "{}: FabButton must preserve the sheet's host_over baseline", entry.id);
                let rest = face.rest.expect("ButtonFlat face.rest");
                if rest & 255 == 255 {
                    check("color_text_on_accent", "accent on ButtonFlat", host_over_opaque(rest, accent, default), READABLE);
                }
            }
            let mut needs_focus_tint = false;
            for (ground, base) in [("panel", p.grounds.panel), ("toolbar", p.grounds.toolbar), ("high", p.grounds.high)] {
                let tab_rest = own_tab.map_or(0, |f| f.rest.unwrap());
                let tab_hover = own_tab.map_or(0, |f| f.hover.unwrap());
                let tab_down = own_tab.map_or(if press_selects { p.grounds.ctrl_selected } else { 0 }, |f| f.down.unwrap());
                let tab_active = own_tab.map_or(p.grounds.ctrl_selected, |f| f.active.unwrap());
                for (key, face) in [("color_toggle_ink", tab_rest), ("color_toggle_ink_hover", tab_hover),
                                    ("color_toggle_ink_down", tab_down), ("color_toggle_ink_active", tab_active)] {
                    check(key, ground, over(base, face), LEGIBLE);
                }
                if own_tab.is_some() || ground != "high" {
                    let seg_rest = own_tab.map_or(p.grounds.field, |f| f.rest.unwrap());
                    let seg_hover = own_tab.map_or(p.grounds.field, |f| f.hover.unwrap());
                    let seg_down = own_tab.map_or(if press_selects { p.choice_on } else { p.grounds.field }, |f| f.down.unwrap());
                    let seg_active = own_tab.map_or(p.choice_on, |f| f.active.unwrap());
                    for (key, face) in [("color_segment_ink", seg_rest), ("color_segment_ink_hover", seg_hover),
                                        ("color_segment_ink_down", seg_down), ("color_segment_ink_active", seg_active)] {
                        check(key, ground, over(base, face), READABLE);
                    }
                }
                let rest = own_button.map_or(btn_bg, |f| f.rest.unwrap());
                let hover = own_button.map_or(p.grounds.field_hover, |f| f.hover.unwrap());
                let down = own_button.map_or(p.grounds.field_active_raw, |f| f.down.unwrap());
                let focus = own_button.and_then(|f| f.focus).unwrap_or(rest);
                for (key, face) in [("color_btn_ink", rest), ("color_btn_ink_hover", hover),
                                    ("color_btn_ink_down", down), ("color_btn_ink_focus", focus)] {
                    check(key, ground, over(base, face), READABLE);
                }
                let rest = own_flat.map_or(0, |f| f.rest.unwrap());
                let hover = own_flat.map_or(p.button_hover, |f| f.hover.unwrap());
                let down = own_flat.map_or(p.button_down, |f| f.down.unwrap());
                let focus = own_flat.and_then(|f| f.focus).unwrap_or(rest);
                for (key, face) in [("color_flat_ink", rest), ("color_flat_ink_dim", rest),
                                    ("color_action_ink", rest), ("color_flat_ink_hover", hover),
                                    ("color_flat_ink_down", down), ("color_flat_ink_focus", focus)] {
                    check(key, ground, over(base, face), READABLE);
                }
                let focus_bg = over(base, focus);
                if ["color_flat_ink", "color_flat_ink_dim", "color_action_ink"]
                    .into_iter()
                    .any(|key| reads_on(focus_bg, token(key)) < READABLE)
                {
                    needs_focus_tint = true;
                }
            }
            let focus_tint = vm.value(fab, id!(flat_focus_tint)).as_f64()
                .unwrap_or_else(|| panic!("{}: missing fab.flat_focus_tint", entry.id));
            let expected_tint = if needs_focus_tint { 1.0 } else { 0.0 };
            if focus_tint != expected_tint {
                failures.push(format!("{} flat_focus_tint={focus_tint} (expected {expected_tint})", entry.id));
            }
            if focus_tint == 1.0 {
                focus_tinted += 1;
            }
        });
    }
    assert_census("Fab button and toggle tokens", sheets.len(), &failures);
    assert!(focus_tinted > 0, "expected at least one sheet to exercise flat_focus_tint == 1.0");
}

#[test]
fn host_dark_button_and_toggle_tokens_match_slice_3() {
    use makepad_widgets_core::{ScriptNew, Vec4f};

    let mut cx = Cx::new(Box::new(|_, _| {}));
    cx.init_cx_os();
    cx.with_vm(register_fab_theme);
    for family in HOST_FAMILIES {
        cx.with_vm(|vm| {
            let entry = family.sheet(true);
            desktop_style::install(vm, toned(StyleSheet::load(entry)));
            vm.capture_errors();
            vm.with_reload(register_fab_theme);
            let errors = vm.take_errors();
            assert!(errors.is_empty(), "{}: {errors:?}", entry.id);
            let theme = vm.module(id!(theme));
            let fab = vm.module(id!(fab));
            let theme_color = |key| vm.value(theme, key).as_color().expect("theme colour");
            let body = theme_color(id!(color_text));
            let dim = theme_color(id!(color_on_surface_variant));
            for (expected, keys) in [
                (body, &["color_toggle_ink", "color_toggle_ink_hover", "color_toggle_ink_down",
                         "color_segment_ink_hover", "color_segment_ink_down", "color_segment_ink_active",
                         "color_btn_ink", "color_btn_ink_hover", "color_btn_ink_down", "color_btn_ink_focus",
                         "color_flat_ink", "color_flat_ink_hover", "color_flat_ink_down", "color_flat_ink_focus",
                         "color_mute_ink_down", "color_cam_close_ink_hot"][..]),
                (dim, &["color_segment_ink", "color_flat_ink_dim"]),
                (theme_color(id!(color_text_on_accent)), &["color_toggle_ink_active", "color_text_on_accent"]),
                (theme_color(id!(prop_field)), &["color_btn_bg"]),
                (theme_color(id!(prop_field_hover)), &["color_field_hover"]),
                (theme_color(id!(prop_field_active)), &["color_action_down"]),
                (theme_color(id!(prop_ink_dim)), &["color_action_ink"]),
                (theme_color(id!(prop_choice_on)), &["color_choice_on"]),
                (theme_color(id!(color_outset_hover)), &["color_button_hover", "color_cam_close_hover"]),
                (theme_color(id!(color_outset_down)), &["color_button_down", "color_cam_close_down"]),
                (theme_color(id!(color_bg_highlight)), &["color_accent_down"]),
                (theme_color(id!(color_label_inner_hover)), &["color_text_input_hover"]),
                (theme_color(id!(color_label_inner_down)), &["color_text_input_down"]),
                (theme_color(id!(color_label_outer_hover)), &["color_toggle_label_hover"]),
                (theme_color(id!(color_label_outer_down)), &["color_toggle_label_down"]),
                (0xffffffff, &["color_mute_ink_active"]),
                (0x00000000, &["color_cam_close_focus"]),
            ] {
                for key in keys {
                    assert_eq!(vm.value(fab, LiveId::from_str(key)).as_color(), Some(expected), "{}: fab.{key}", entry.id);
                }
            }
            assert_eq!(vm.value(fab, id!(flat_focus_tint)).as_f64(), Some(0.0), "{}: flat_focus_tint", entry.id);
            let svg = vm.value(fab, id!(color_flat_icon_or_raw));
            assert_eq!(Vec4f::script_from_value(vm, svg), Vec4f::all(-1.0), "{}: color_flat_icon_or_raw", entry.id);
        });
    }
}
