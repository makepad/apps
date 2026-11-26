//! What Stage itself wrote, and the one check that lets a tool show or open
//! it: `reveal_file` and `open_file` take only a render's file, the folder
//! of the last web export (and the files inside it) and that export's local
//! address. Any other path or address is refused, so a model cannot make
//! Stage open an arbitrary file.

use std::path::{Component, Path, PathBuf};

/// The places Stage wrote and served this session.
#[derive(Default, Clone, Debug, PartialEq)]
pub(crate) struct Written {
    /// Files (a render's output).
    pub files: Vec<PathBuf>,
    /// Folders, with everything inside them (a web export).
    pub dirs: Vec<PathBuf>,
    /// The addresses Stage serves (the web export's local one).
    pub urls: Vec<String>,
}

/// What a checked argument is.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Place {
    Path(PathBuf),
    Url(String),
}

impl Written {
    /// `arg` as a place Stage wrote, or why it is refused.
    pub fn check(&self, arg: &str) -> Result<Place, String> {
        let arg = arg.trim();
        if arg.is_empty() {
            return Err("`path` is empty".into());
        }
        if self.urls.iter().any(|url| url == arg) {
            return Ok(Place::Url(arg.to_string()));
        }
        let path = Path::new(arg);
        if !path.is_absolute() || path.components().any(|part| matches!(part, Component::ParentDir | Component::CurDir)) {
            return Err(format!("`{arg}` is not a file Stage wrote: it is not an absolute path without `..`"));
        }
        let wrote = self.files.iter().any(|file| file == path) || self.dirs.iter().any(|dir| path.starts_with(dir));
        if wrote {
            Ok(Place::Path(path.to_path_buf()))
        } else {
            Err(format!("`{arg}` is not a file Stage wrote (only a render's file and the last web export are shown or opened)"))
        }
    }
}

impl super::VideoCut {
    /// What this session's renders and web exports wrote.
    pub(crate) fn written(&self) -> Written {
        let mut out = Written::default();
        let ui = &self.render_ui;
        out.files.extend(ui.exported.iter().cloned());
        if let Some(files) = &ui.last_file {
            out.files.extend(files.split(", ").map(PathBuf::from).filter(|file| !out.files.contains(file)).collect::<Vec<_>>());
        }
        let (dir, url) = self.web_export.written();
        out.dirs.extend(dir);
        out.urls.extend(url);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn written() -> Written {
        Written {
            files: vec![PathBuf::from("/m/Film.mp4")],
            dirs: vec![PathBuf::from("/m/Film web")],
            urls: vec!["http://127.0.0.1:9000/".into()],
        }
    }

    #[test]
    fn only_what_stage_wrote_is_shown_or_opened() {
        let w = written();
        assert_eq!(w.check("/m/Film.mp4"), Ok(Place::Path("/m/Film.mp4".into())));
        assert_eq!(w.check(" /m/Film web "), Ok(Place::Path("/m/Film web".into())));
        assert_eq!(w.check("/m/Film web/index.html"), Ok(Place::Path("/m/Film web/index.html".into())));
        assert_eq!(w.check("http://127.0.0.1:9000/"), Ok(Place::Url("http://127.0.0.1:9000/".into())));
        for bad in ["", "  ", "/etc/passwd", "/m/Film.mp4.sh", "/m/Film webby/x", "/m", "/m/Film web/../secret", "Film.mp4", "~/Film.mp4", "http://127.0.0.1:9001/", "file:///etc/passwd"] {
            assert!(w.check(bad).is_err(), "{bad:?} must be refused");
        }
        assert!(Written::default().check("/m/Film.mp4").is_err(), "nothing written, nothing shown");
    }
}
