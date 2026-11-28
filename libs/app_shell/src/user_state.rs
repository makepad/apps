//! Where Stage keeps the person's own work between launches: EDIT drafts,
//! storyboards, the SCORE score draft, the pane layout and the saved values
//! of the VJ and FLOW graphs.
//!
//! A hidden instance (`MAKEPAD_HIDE_WINDOWS`, which tests and agents use)
//! never writes into the person's folders: unless it names a folder itself
//! (`STAGE_EDIT_DRAFTS`, `STAGE_STORYBOARDS`, `STAGE_SCORES`, …) it gets a
//! throwaway folder of its own under the temp dir, one per process.

use std::path::{Path, PathBuf};

/// `name`'s folder under `stage-product`, or the folder `env` names.
pub fn dir(env: Option<&str>, name: &str) -> PathBuf {
    let explicit = env.and_then(|env| std::env::var_os(env)).filter(|dir| !dir.is_empty()).map(PathBuf::from);
    resolve(explicit, hidden(), &makepad_widgets::makepad_platform::home::app_state_home(), &std::env::temp_dir(), std::process::id(), name)
}

/// A hidden (test or agent) instance.
pub fn hidden() -> bool {
    std::env::var_os("MAKEPAD_HIDE_WINDOWS").is_some_and(|value| !value.is_empty() && value != "0")
}

pub fn resolve(explicit: Option<PathBuf>, hidden: bool, home: &Path, temp: &Path, pid: u32, name: &str) -> PathBuf {
    match explicit {
        Some(dir) => dir,
        None if hidden => temp.join(format!("stage-hidden-{pid}")).join(name),
        None => home.join("stage-product").join(name),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_hidden_instance_never_writes_into_the_persons_folders() {
        let (home, temp) = (Path::new("/home/user/.makepad"), Path::new("/tmp"));
        assert_eq!(resolve(None, false, home, temp, 7, "edit-drafts"), PathBuf::from("/home/user/.makepad/stage-product/edit-drafts"), "visible drafts belong under the supplied Makepad home");
        let test_state = temp.join("test-home").join(".makepad");
        assert_eq!(resolve(None, false, &test_state, temp, 7, "edit-drafts"), test_state.join("stage-product/edit-drafts"), "visible test drafts belong under the test instance's app state home");
        let hidden = resolve(None, true, home, temp, 7, "edit-drafts");
        assert_eq!(hidden, PathBuf::from("/tmp/stage-hidden-7/edit-drafts"), "hidden drafts belong in the process's temporary folder");
        assert_eq!(resolve(None, true, &test_state, temp, 7, "edit-drafts"), hidden, "the hidden folder takes precedence over a test home");
        assert!(!hidden.starts_with(home), "hidden drafts must stay outside the person's home");
        // Two hidden instances do not share.
        assert_ne!(resolve(None, true, home, temp, 8, "scores"), resolve(None, true, home, temp, 7, "scores"), "each hidden process needs its own scores folder");
        // A folder named explicitly wins, hidden or not.
        assert_eq!(resolve(Some(PathBuf::from("/s/d")), true, home, temp, 7, "edit-drafts"), PathBuf::from("/s/d"), "an explicit drafts folder takes precedence for a hidden process");
        for name in ["edit-drafts", "storyboards", "scores", "values", "studio-values"] {
            assert!(!resolve(None, true, home, temp, 1, name).starts_with(home), "hidden {name} must stay outside the person's home");
        }
    }
}
