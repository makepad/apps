//! The plate's render mode as the tests drive it: the view bar's Render mode dropdown opens
//! Normal, Sliced and, on Design's plate, Force, and its value is the mode that shows.

#![allow(dead_code)]

use makepad_test::{Role, Selector, TestApp};

/// The Render mode dropdown.
pub fn control() -> Selector {
    Selector::role(Role::Button).name("Render mode")
}

/// Picks `mode` ("Normal", "Sliced" or "Force") from Render mode, and waits until it shows.
pub fn pick(app: &TestApp, mode: &str) {
    app.locator(control()).wait_visible().press();
    app.locator(Selector::role(Role::MenuItem).name(mode)).wait_visible().press();
    wait_shown(app, mode);
}

/// Waits until the render mode is `mode`.
pub fn wait_shown(app: &TestApp, mode: &str) {
    app.locator(control().value(mode)).wait_visible();
}

/// Waits until the render mode is anything but `mode`.
pub fn wait_not_shown(app: &TestApp, mode: &str) {
    app.wait_until(|| shown(app, mode).then(|| format!("waiting for the render mode to leave {mode}")));
}

/// Whether the render mode is `mode` now.
pub fn shown(app: &TestApp, mode: &str) -> bool {
    app.locator(control().value(mode)).try_snapshot().is_ok()
}
