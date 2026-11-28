use makepad_test::{KeyCode, KeyModifiers, TestApp};

/// Export the sliced plate from the command palette (Cmd/Ctrl+Shift+P), including with no
/// printer linked.
pub fn request(app: &TestApp) {
    app.press_key_with_modifiers(KeyCode::KeyP, if cfg!(target_os = "macos") { KeyModifiers { shift: true, logo: true, ..Default::default() } } else { KeyModifiers { shift: true, control: true, ..Default::default() } });
    app.type_text("Export the Print");
    app.press_key(KeyCode::ReturnKey);
}
