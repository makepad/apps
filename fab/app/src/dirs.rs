use std::path::{Path, PathBuf};

/// Resolved filesystem locations for a Fab instance, built once from a home
/// root (`under`) with environment overrides applied for the running system (`system`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FabDirs {
    pub app_data: PathBuf,
    pub documents: PathBuf,
    pub jobs: PathBuf,
    pub calibration: PathBuf,
    pub head_envelope: PathBuf,
    pub profiles: PathBuf,
}

fn env_path(name: &str) -> Option<PathBuf> {
    std::env::var_os(name).filter(|v| !v.is_empty()).map(PathBuf::from)
}

impl FabDirs {
    pub fn under(home: &Path) -> Self {
        let app_data = home.join(".makepad").join("fab");
        let documents = home.join("Documents").join("Fab");
        Self {
            jobs: app_data.join("jobs"),
            calibration: documents.join("calibration"),
            head_envelope: documents.join("head-envelope.conf"),
            profiles: documents.join("profiles"),
            app_data,
            documents,
        }
    }

    pub fn system() -> Self {
        let mut dirs = Self::under(&makepad_fab_settings::home::user_home());
        if let Some(path) = env_path("FAB_CALIBRATION_DIR") {
            dirs.calibration = path;
        }
        if let Some(path) = env_path("FAB_HEAD_ENVELOPE") {
            dirs.head_envelope = path;
        }
        if let Some(path) = env_path("FAB_PROFILES_DIR") {
            dirs.profiles = path;
        }
        dirs
    }
}

impl Default for FabDirs {
    fn default() -> Self {
        Self::system()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn under_user_home_matches_existing_system_paths() {
        let home = makepad_fab_settings::home::user_home();
        let base = FabDirs::under(&home);
        assert_eq!(base.documents, crate::model_host::documents_dir());
        assert_eq!(base.jobs, fab::print::jobs::jobs_dir());
        if env_path("FAB_CALIBRATION_DIR").is_none() {
            assert_eq!(base.calibration, fab_calibrate::results::default_dir());
        }
        if env_path("FAB_PROFILES_DIR").is_none() {
            assert_eq!(base.profiles, makepad_fab_settings::import::fab_profile_dir());
        }
        let sys = FabDirs::system();
        assert_eq!(sys.app_data, base.app_data);
        assert_eq!(sys.documents, base.documents);
        assert_eq!(sys.jobs, base.jobs);
        assert_eq!(sys.calibration, fab_calibrate::results::default_dir());
        assert_eq!(sys.profiles, makepad_fab_settings::import::fab_profile_dir());
    }
}
