//! The caption's workspace row as the tests drive it: each workspace's tab is a part of the row,
//! a button named after the workspace, selected while that workspace shows.

#![allow(dead_code)]

use makepad_test::{Role, Selector, States, TestApp};

/// The tab of the workspace named `name`.
pub fn tab(name: &str) -> Selector {
    Selector::role(Role::Button).name(name)
}

/// Presses the tab of the workspace named `name`, and waits until that workspace shows (the row
/// shows the new one on the frame after the press is taken).
pub fn press(app: &TestApp, name: &str) {
    app.locator(tab(name)).wait_visible().press();
    wait_shown(app, name);
}

/// Waits until the workspace named `name` shows.
pub fn wait_shown(app: &TestApp, name: &str) {
    app.locator(tab(name).state("selected")).wait_visible();
}

/// Whether the workspace named `name` shows now.
pub fn shown(app: &TestApp, name: &str) -> bool {
    app.locator(tab(name)).snapshot().states.contains(States::SELECTED)
}

/// The workspace named `name` shows now.
pub fn assert_shown(app: &TestApp, name: &str) {
    assert!(shown(app, name), "the {name} workspace shows");
}
