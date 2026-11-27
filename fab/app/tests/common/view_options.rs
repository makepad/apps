use makepad_test::{Role, Selector, TestApp};

/// Toggle the plate view's options flyout. Its tiles stay open after a pick.
pub fn toggle(app: &TestApp) {
    app.locator(Selector::role(Role::Button).name("View options")).wait_visible().press();
}

pub fn tile(name: &str) -> Selector {
    Selector::role(Role::ToggleButton).name(name)
}
