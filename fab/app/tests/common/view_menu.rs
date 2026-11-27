use makepad_test::{Role, Selector, TestApp};

pub fn pick(app: &TestApp, name: &str) {
    app.locator(Selector::role(Role::Button).name("View")).wait_visible().press();
    app.locator(Selector::role(Role::MenuItem).name(name)).wait_visible().press();
}
