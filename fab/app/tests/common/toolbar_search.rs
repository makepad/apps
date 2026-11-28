use makepad_test::{NodeMatch, Role, Selector, TestApp};

/// Bring the Slicer toolbar's last tool into view, including its end padding.
pub fn slicer_end(app: &TestApp) -> NodeMatch {
    let advanced = app.locator(Selector::role(Role::CheckBox).name("Advanced"));
    advanced.wait_visible().wheel(1000.0, 0.0);
    app.locator(Selector::path("bar.search").name("Search")).wait_visible().snapshot()
}
