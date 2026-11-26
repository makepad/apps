//! A Widget layer's `ui` moves from the document's VM to Makepad's by its
//! tokens (no text), and builds the widget tree there from the props.
#![cfg(feature = "scene3d")]
use makepad_motion::model::LayerKind;
use makepad_motion::*;
use makepad_widgets::*;

#[test]
fn a_widget_ui_is_the_documents_fn_in_makepads_vm() {
    let doc = MotionDoc::load(r#"use mod.motion.*; let TITLE_SIZE = 40
Motion{duration: 1.0 layers: [Widget{size: vec2(400, 100) props: {title: "Hello"} ui: fn(props) { View{width: Fill height: Fill Label{text: props.title draw_text.text_style.font_size: 40}} }}]}"#).unwrap_or_else(|d| panic!("{d:?}"));
    let layer = doc.all_layers().into_iter().find(|l| matches!(l.kind, LayerKind::Widget { .. })).unwrap();
    let LayerKind::Widget { ui: Some(ui), .. } = &layer.kind else { panic!("no ui") };
    assert!(ui.text().starts_with("fn(props) { View{"), "{}", ui.text());
    let mut cx = Cx::new(Box::new(|_, _| {}));
    cx.with_vm(|vm| {
        makepad_widgets::script_mod(vm);
        makepad_motion_draw::script_mod(vm);
        let f = makepad_motion_draw::widget_ui(vm, ui).unwrap_or_else(|e| panic!("{e}"));
        let props = vm.bx.heap_mut().new_object();
        let title = vm.new_string_with(|_, s| s.push_str("Hello"));
        vm.bx.heap_mut().set_value_def(props, LiveId::from_str("title").into(), title);
        vm.capture_errors();
        let tree = vm.call(f, &[props.into()]);
        let errors = vm.take_errors();
        assert!(errors.is_empty(), "{errors:?}");
        let root = WidgetRef::script_from_value(vm, tree);
        assert!(!root.is_empty(), "no widget tree");
    });
}
