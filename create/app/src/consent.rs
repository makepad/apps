//! Input provenance for the controls that open call and sharing resources.
use makepad_town_core::home::{Home, HomeSource};
use makepad_widgets::{log, Cx};
use std::path::Path;

#[derive(Debug, Eq, PartialEq)]
pub enum Consent {
    Local,
    TestDriver(String),
    Refused(String),
}

pub fn decide(injected_by: Option<String>, test_home: bool) -> Consent {
    match injected_by {
        None => Consent::Local,
        Some(by) if test_home => Consent::TestDriver(by),
        Some(by) => Consent::Refused(by),
    }
}

pub fn granted(cx: &Cx, test_home: bool) -> bool {
    match decide(cx.injected_by(), test_home) {
        Consent::Local => true,
        Consent::TestDriver(by) => {
            log!("consent by test driver ({})", by.replace(['\r', '\n'], " "));
            true
        }
        Consent::Refused(by) => {
            log!("refused a press injected by {}: only this computer's own input opens a call or share", by.replace(['\r', '\n'], " "));
            false
        }
    }
}

/// Provenance of the directory opened by the model, cached for its lifetime.
#[derive(Default)]
pub struct TestHome(bool);

impl TestHome {
    pub fn opened(dir: &Path) -> Self {
        Self(matches!(Home::default_dir_source(), Some((path, HomeSource::TestHome)) if path == dir))
    }

    pub fn verified(&self) -> bool {
        self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_an_opened_test_home_grants_the_named_driver_consent() {
        for test_home in [false, true] {
            assert_eq!(decide(None, test_home), Consent::Local);
        }
        assert_eq!(decide(Some("driver".into()), false), Consent::Refused("driver".into()));
        assert_eq!(decide(Some("driver".into()), true), Consent::TestDriver("driver".into()));

        let root = std::env::temp_dir().join("consent-home");
        let opened = root.join(".makepad").join("create");
        let overrides = ["MAKEPAD_CREATE_HOME", "MAKEPAD_TEST_HOME"].map(|key| (key, std::env::var_os(key)));
        std::env::remove_var("MAKEPAD_CREATE_HOME");
        std::env::set_var("MAKEPAD_TEST_HOME", &root);
        let test_only = TestHome::opened(&opened).verified();
        let wrong_dir = TestHome::opened(&root).verified();
        std::env::set_var("MAKEPAD_CREATE_HOME", &opened);
        let both = TestHome::opened(&opened).verified();
        std::env::remove_var("MAKEPAD_TEST_HOME");
        let explicit_only = TestHome::opened(&opened).verified();
        std::env::remove_var("MAKEPAD_CREATE_HOME");
        let user = Home::default_dir().is_some_and(|dir| TestHome::opened(&dir).verified());
        for (key, old) in overrides {
            if let Some(value) = old { std::env::set_var(key, value); } else { std::env::remove_var(key); }
        }
        assert!(test_only, "the resolved subdirectory of the test home grants");
        assert!(!wrong_dir && !both && !explicit_only && !user, "only the opened test directory grants");
    }
}
