use super::*;
use makepad_widgets_core::design_data::theme::{apply_install_spec, parse_install_spec};

fn fab_script_mod(vm: &mut ScriptVm) {
    wear_host_sheet(vm);
    makepad_widgets_core::script_mod(vm);
    super::script_mod(vm);
}

fn install_theme(cx: &mut Cx, json: &str) {
    apply_install_spec(cx, &parse_install_spec(json).unwrap()).unwrap();
}

fn reload_fab(vm: &mut ScriptVm) {
    vm.capture_errors();
    vm.with_reload(fab_script_mod);
    let errors = vm.take_errors();
    assert!(errors.is_empty(), "Fab theme reload: {errors:?}");
}

#[test]
fn sheetless_install_survives_fab_reload() {
    let mut cx = Cx::new(Box::new(|_, _| {}));
    cx.init_cx_os();
    cx.with_vm(fab_script_mod);
    assert!(cx.with_vm(desktop_style::current_name).is_some(), "standalone startup wears a host sheet");

    install_theme(&mut cx, r##"{"base":"dark","sheet":null,"globals":[["space_factor",12.0]],"overrides":[["color_bg_app","#223344"]]}"##);
    cx.with_vm(|vm| {
        reload_fab(vm);
        let theme = vm.module(id!(theme));
        let spacing = vm.value(theme, id!(space_factor)).as_f64();
        assert_eq!((desktop_style::current_name(vm), spacing), (None, Some(12.0)), "sheet and spacing after reload");
        assert_eq!(vm.value(theme, id!(color_bg_app)).as_color(), Some(0x223344ff), "builder override");
    });
    assert!(theme_mix(&mut cx).is_some(), "the installed mix survives reload");

    install_theme(&mut cx, r#"{"base":"light","sheet":null}"#);
    cx.with_vm(|vm| {
        reload_fab(vm);
        assert_eq!(desktop_style::current_name(vm), None, "the plain preset stays sheetless");
        let light = script_eval!(vm, {mod.themes.light.color_bg_app}).as_color().expect("light background");
        let theme = vm.module(id!(theme));
        assert_eq!(vm.value(theme, id!(color_bg_app)).as_color(), Some(light), "plain light background");
    });
    assert_eq!(base_theme(&mut cx), BaseTheme::Light);
}

#[test]
fn follow_host_appearance_stops_after_theme_install_or_edit() {
    let mut cx = Cx::new(Box::new(|_, _| {}));
    cx.with_vm(|vm| wear_host_sheet_with(vm, DesktopStyle::Macos, true));
    assert_eq!(cx.with_vm(desktop_style::current_name).as_deref(), Some("macos-dark"));
    for (dark, name) in [(false, "macos"), (true, "macos-dark")] {
        assert!(follow_host_appearance_dark(&mut cx, dark), "the standalone sheet follows {name}");
        assert!(std::mem::take(&mut cx.pending_style_reload), "changing appearance requests a reload");
        assert_eq!(cx.with_vm(desktop_style::current_name).as_deref(), Some(name));
    }

    let stays = |cx: &mut Cx, expected: Option<&str>| {
        cx.pending_style_reload = false;
        assert!(!follow_host_appearance_dark(cx, false), "a host flip replaced the member's theme");
        assert!(!cx.pending_style_reload, "a host flip scheduled a reload");
        assert_eq!(cx.with_vm(desktop_style::current_name).as_deref(), expected);
    };
    set_theme_edit(&mut cx, "color_bg_app", Some("#x223344FF".into()));
    stays(&mut cx, Some("macos-dark"));
    set_theme_edit(&mut cx, "color_bg_app", None);

    install_theme(&mut cx, r#"{"base":"dark","sheet":"macos-dark"}"#);
    stays(&mut cx, Some("macos-dark"));
    install_theme(&mut cx, r#"{"base":"dark","sheet":null}"#);
    stays(&mut cx, None);
}
