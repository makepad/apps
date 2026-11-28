//! The Parts list's tab. Design's default layout holds no Parts list (a person adds it), so a
//! test that works in the list shows it first, as the View menu's reveal does: it joins the
//! Plates stack.

use makepad_test::{Role, Selector, TestApp};

/// The Parts tab, once shown.
#[allow(dead_code)]
pub fn tab() -> Selector {
    Selector::role(Role::Tab).name("Parts")
}

/// Show the Parts list where no area holds it, and bring its tab forward.
#[allow(dead_code)]
pub fn open(app: &TestApp) {
    reveal(app);
    app.locator(tab()).wait_visible().click();
}

/// Show the Parts list where no area holds it, leaving which tab is in front as it is when one
/// already does.
#[allow(dead_code)]
pub fn reveal(app: &TestApp) {
    let shown = app.widget_snapshot().nodes().into_iter().any(|w| w.visible() && w.node.role == Role::Tab && w.node.name == "Parts");
    if shown {
        return;
    }
    let client = makepad_remote::Client::connect(&app.remote_endpoint()).expect("the app's remote endpoint");
    let argv = vec!["call".to_string(), "fab.workspace_edit".to_string(), r#"{"edits":"show(Parts)"}"#.to_string()];
    let mut result = None;
    makepad_remote::app::run(&client, &argv, &mut |line| {
        if line.contains("\"Result\"") {
            result = Some(line.to_string());
        }
        true
    })
    .expect("fab.workspace_edit runs");
    let result = result.expect("fab.workspace_edit answers");
    assert!(!result.contains("error"), "show(Parts) is refused: {result}");
}
