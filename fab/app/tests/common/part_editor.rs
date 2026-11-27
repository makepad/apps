use makepad_test::{Role, Selector, TestApp};

/// Open the picked part's editor when the shell places it in the tab bar.
pub fn open(app: &TestApp) {
    if app.widget_snapshot().nodes().into_iter().any(|w| w.visible() && w.node.role == Role::Tab && w.node.name == "Part") {
        app.locator(Selector::role(Role::Tab).name("Part")).press();
    }
}
