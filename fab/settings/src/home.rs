//! The home folder Fab keeps the person's state under: preferences, the
//! autosave, linked printers, calibration, imported profiles, and where the
//! printer vendor's app keeps its installed profiles.
//!
//! A test instance gets its own (`MAKEPAD_TEST_HOME`, an empty folder the
//! test harness owns), so a test starts from Fab's defaults and never reads
//! or changes the person's settings.

use std::path::{Path, PathBuf};

fn test_home() -> Option<PathBuf> {
    std::env::var_os("MAKEPAD_TEST_HOME").filter(|h| !h.is_empty()).map(PathBuf::from)
}

/// The person's home folder (a test's own home in a test).
pub fn user_home() -> PathBuf {
    test_home()
        .or_else(|| std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).map(PathBuf::from))
        .unwrap_or_else(std::env::temp_dir)
}

/// Where the vendor's desktop app keeps its data below `home` on this OS.
pub fn vendor_data_dir_in(home: &Path, app_dir: &str) -> PathBuf {
    if cfg!(target_os = "macos") {
        home.join("Library").join("Application Support").join(app_dir)
    } else if cfg!(windows) {
        home.join("AppData").join("Roaming").join(app_dir)
    } else {
        home.join(".config").join(app_dir)
    }
}

/// The vendor app's data folder: below the test's home in a test, else
/// where this OS puts it (`APPDATA`, `XDG_CONFIG_HOME` when set).
pub fn vendor_data_dir(app_dir: &str) -> PathBuf {
    if let Some(home) = test_home() {
        return vendor_data_dir_in(&home, app_dir);
    }
    if cfg!(windows) {
        if let Some(a) = std::env::var_os("APPDATA").filter(|a| !a.is_empty()) {
            return PathBuf::from(a).join(app_dir);
        }
    } else if !cfg!(target_os = "macos") {
        if let Some(c) = std::env::var_os("XDG_CONFIG_HOME").filter(|c| !c.is_empty()) {
            return PathBuf::from(c).join(app_dir);
        }
    }
    vendor_data_dir_in(&user_home(), app_dir)
}
