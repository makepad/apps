//! Errors of the readers, writers and the package.

use std::fmt;

#[derive(Clone, PartialEq, Debug)]
pub enum CamError {
    /// A file is not what its format says; `at` is the 1-based line (or block) number.
    Parse { at: usize, msg: String },
    /// A valid construct this reader does not read.
    Unsupported { at: usize, msg: String },
    /// A profile or model that cannot be written.
    Profile(String),
    /// Geometry the format cannot carry faithfully (named with its place): the export fails
    /// instead of writing something that differs from the model.
    Geometry(String),
}

impl CamError {
    pub fn parse(at: usize, msg: impl Into<String>) -> CamError {
        CamError::Parse { at, msg: msg.into() }
    }
    pub fn unsupported(at: usize, msg: impl Into<String>) -> CamError {
        CamError::Unsupported { at, msg: msg.into() }
    }
}

impl fmt::Display for CamError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CamError::Parse { at, msg } => write!(f, "malformed file at {at}: {msg}"),
            CamError::Unsupported { at, msg } => write!(f, "unsupported at {at}: {msg}"),
            CamError::Profile(m) => write!(f, "profile: {m}"),
            CamError::Geometry(m) => write!(f, "geometry: {m}"),
        }
    }
}

impl std::error::Error for CamError {}
