//! The editor panel's gesture rules over the Motion document kind.

use makepad_editor_panel::{install_doc, press, queue, release, set_files, Edit, File, Runtime};
use makepad_motion::schema::code::outline;
use makepad_stage_motion_pickers::motion_doc;
use makepad_strict_json::Value as Json;

/// A long drag (press, a hundred steps, release) is one write: every
/// step is held (`done: false`, nothing written), and the release is
/// the only splice that writes, one undo step however many steps.
#[test]
fn a_long_drag_is_one_write() {
    install_doc(motion_doc());
    let text = "use mod.motion.*; let LOOK = {width: 1.5 color: #ff4d12}\nMotion{}\n".to_string();
    let code = outline(&text);
    set_files(vec![(String::new(), text.clone(), code.clone())]);
    let files = [File { name: "", text: &text, code: &code }];
    let mut runtime = Runtime::default();
    press();
    let mut held = 0;
    for k in 0..100 {
        queue(Edit::Set { path: "LOOK.width".into(), value: Json::F64(1.5 + k as f64 * 0.01) });
        queue(Edit::SetCode { path: "LOOK.color".into(), code: format!("#{:02x}4d12", 0x80 + k) });
        for splice in runtime.take(&files, &|_, _| None) {
            assert!(!splice.done, "a step of a held gesture is not written");
            held += 1;
        }
    }
    release();
    let written = runtime.take(&files, &|_, _| None);
    assert!(held > 0);
    assert_eq!(written.len(), 2, "one splice per value moved: {written:?}");
    assert!(written.iter().all(|s| s.done && s.gesture == written[0].gesture), "one gesture, written at the release");
    assert_eq!(written.iter().find(|s| s.path == "LOOK.width").map(|s| s.splice.text.as_str()), Some("2.49"));
    assert!(runtime.take(&files, &|_, _| None).is_empty(), "nothing after the release");
}
