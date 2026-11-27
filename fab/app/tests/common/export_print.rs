use makepad_test::{KeyCode, TestApp};

/// Export the sliced plate from the command palette, including with no printer linked.
pub fn request(app: &TestApp) {
    app.press_key(KeyCode::F3);
    app.type_text("Export the Print");
    app.press_key(KeyCode::ReturnKey);
}
