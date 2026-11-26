//! The generic UI checks (`makepad_widgets::ui_checks`) over Story's CUT
//! page, drawn headlessly with the real fonts at a desktop size and
//! a half-width split. The pages are drawn as they stand with no project
//! loaded: this holds their static layout to the checks.
use makepad_widgets::test_cx::checkout_test_cx;
use makepad_widgets::page_flip::PageFlip;
use makepad_widgets::ui_checks::{draw_and_check, UiCheckOptions, UiIssue};
use makepad_widgets::*;
use story::StoryApp;

fn register(vm: &mut ScriptVm) {
    let _ = <StoryApp as AppMain>::script_mod(vm);
}

fn page_ui(cx: &mut Cx) -> WidgetRef {
    cx.with_vm(|vm| {
        let value = script_eval!(vm, {
            use mod.prelude.widgets.*
            use mod.widgets.*
            mod.story.Ui{}
        });
        let errors = vm.take_errors();
        assert!(errors.is_empty(), "the Story UI does not evaluate: {errors:?}");
        WidgetRef::script_from_value(vm, value)
    })
}

/// The first widget named `name` under `from`, depth first.
fn find(from: &WidgetRef, name: LiveId) -> WidgetRef {
    let mut kids = Vec::new();
    from.children(&mut |id, child| kids.push((id, child)));
    for (id, child) in kids {
        if id == name {
            return child;
        }
        let found = find(&child, name);
        if !found.is_empty() {
            return found;
        }
    }
    WidgetRef::empty()
}

fn check_page(page: LiveId, name: &str, size: Vec2d) -> Vec<UiIssue> {
    let mut cx = checkout_test_cx(register);
    let ui = page_ui(&mut cx);
    // The product keeps its pages in a lazy page flip: ask it for this one.
    let flip = find(&ui, id!(product_pages));
    let root = flip
        .borrow_mut::<PageFlip>()
        .expect("the product's page flip")
        .page(&mut cx, page)
        .unwrap_or_else(|| panic!("{name}: no such page"));
    let mut opts = UiCheckOptions::for_pane(size);
    // A desktop tool, not a game HUD: 6 px caps are the floor, not 13 px.
    opts.tiny = 0.005;
    draw_and_check(&mut cx, &root, size, &opts)
}

/// What the checks find on the pages today, as the pages stand (no project
/// loaded): (page, pane width, check, a widget name on the path). A name
/// stands for that widget and everything under it, so unnamed children are
/// covered by their nearest named ancestor. These are layout faults of the
/// pages, listed so a new one fails the test; fixing one means deleting its
/// line (a line that no longer matches fails too).
const KNOWN: &[(&str, u32, &str, &str)] = &[
    // The info pane's action row starts 4 px above the bottom edge and runs 27 px past it.
    ("CUT", 1920, "offscreen", "cut_info_actions"),
    ("CUT", 960, "offscreen", "cut_info_actions"),
    // The timeline toolbar is wider than the half-width pane: zoom and two buttons run off its right edge.
    ("CUT", 960, "offscreen", "cut_tl_bar"),
    // Text wider than the field or button that holds it.
    ("CUT", 960, "clipped", "lib_search"),
    ("CUT", 960, "clipped", "cut_voice_add"),
    // The clock readout is pushed out of its clip entirely.
    ("CUT", 960, "clipped", "cut_time"),
];

#[test]
fn story_pages_hold_to_the_ui_checks() {
    let sizes = [dvec2(1920.0, 1080.0), dvec2(960.0, 1080.0)];
    let mut found: Vec<(&str, u32, &str, String)> = Vec::new();
    for (id, name) in [(id!(cut_page), "CUT")] {
        for size in sizes {
            for issue in check_page(id, name, size) {
                eprintln!("{name} {}x{}: {issue}", size.x, size.y);
                found.push((name, size.x as u32, issue.kind.name(), issue.path));
            }
        }
    }
    let is_known = |f: &(&str, u32, &str, String), k: &(&str, u32, &str, &str)| {
        f.0 == k.0 && f.1 == k.1 && f.2 == k.2 && f.3.split('.').any(|segment| segment == k.3)
    };
    let new: Vec<_> = found.iter().filter(|f| !KNOWN.iter().any(|k| is_known(f, k))).collect();
    let fixed: Vec<_> = KNOWN.iter().filter(|k| !found.iter().any(|f| is_known(f, k))).collect();
    assert!(new.is_empty(), "new findings: {new:?}");
    assert!(fixed.is_empty(), "known findings that no longer occur (delete them): {fixed:?}");
}
