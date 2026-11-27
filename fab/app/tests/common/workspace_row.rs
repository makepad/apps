//! The caption's workspace control as the tests drive it: the caption is named after the
//! workspace that shows ("Current workspace: <name>"), and All workspaces opens a menu of every
//! workspace, whichever of their tabs fit the row.

#![allow(dead_code)]

use makepad_test::{Role, Selector, TestApp};

/// The tab of the workspace named `name`.
pub fn tab(name: &str) -> Selector {
    Selector::role(Role::Button).name(name)
}

/// The caption while the workspace named `name` shows.
pub fn current(name: &str) -> Selector {
    Selector::role(Role::Group).name(format!("Current workspace: {name}"))
}

/// Picks the workspace named `name` from All workspaces, and waits until it shows (the caption
/// names the new one on the frame after the pick is taken).
pub fn press(app: &TestApp, name: &str) {
    app.locator(Selector::role(Role::Button).name("All workspaces")).wait_visible().press();
    app.locator(Selector::role(Role::MenuItem).name(name)).wait_visible().press();
    wait_shown(app, name);
}

/// Waits until the workspace named `name` shows.
pub fn wait_shown(app: &TestApp, name: &str) {
    app.locator(current(name)).wait_visible();
}

/// Whether the workspace named `name` shows now.
pub fn shown(app: &TestApp, name: &str) -> bool {
    app.locator(current(name)).try_snapshot().is_ok()
}

/// The workspace named `name` shows now.
pub fn assert_shown(app: &TestApp, name: &str) {
    assert!(shown(app, name), "the {name} workspace shows");
}
