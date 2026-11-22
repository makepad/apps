//! What a call returns: JSON, pictures, or an error that says how to fix
//! the call.

use makepad_strict_json::{self as json, Value};

/// A picture a tool returns (frames, strips, previews), for vision models.
#[derive(Clone, Debug, PartialEq)]
pub struct Image {
    pub png: Vec<u8>,
    /// What it shows, e.g. `strip of the cut, 12 frames 0–15 s`.
    pub label: String,
}

/// A document a call changed, whole before and after (the Generate
/// panel's revisions and diff view). Keys: `cut`, `motion:<asset id>`,
/// `effect:<asset id>`.
#[derive(Clone, Debug, PartialEq)]
pub struct DocChange {
    pub key: String,
    pub label: String,
    pub before: String,
    pub after: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ToolOutput {
    pub json: Value,
    pub images: Vec<Image>,
    /// In process only (the MCP face carries the diff in `json`).
    pub changes: Vec<DocChange>,
}

impl ToolOutput {
    pub fn json(json: Value) -> Self {
        Self { json, images: Vec::new(), changes: Vec::new() }
    }

    pub fn with_image(mut self, image: Image) -> Self {
        self.images.push(image);
        self
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorKind {
    /// No tool by that name.
    UnknownTool,
    /// The arguments do not match the schema.
    InvalidArgs,
    /// The tool exists but this Stage has no implementation yet.
    NotAvailable,
    /// Nothing is open that the tool works on, or a name does not exist.
    NotFound,
    /// The call is valid but the result would be wrong (a source that
    /// does not load, a non-seekable effect, a stale base revision).
    Refused,
    /// It ran and failed.
    Failed,
}

impl ErrorKind {
    pub fn id(self) -> &'static str {
        match self {
            ErrorKind::UnknownTool => "unknown_tool",
            ErrorKind::InvalidArgs => "invalid_args",
            ErrorKind::NotAvailable => "not_available",
            ErrorKind::NotFound => "not_found",
            ErrorKind::Refused => "refused",
            ErrorKind::Failed => "failed",
        }
    }

    pub fn from_id(id: &str) -> ErrorKind {
        match id {
            "unknown_tool" => ErrorKind::UnknownTool,
            "invalid_args" => ErrorKind::InvalidArgs,
            "not_available" => ErrorKind::NotAvailable,
            "not_found" => ErrorKind::NotFound,
            "refused" => ErrorKind::Refused,
            _ => ErrorKind::Failed,
        }
    }
}

/// An error the model reads: what is wrong and how to fix it.
#[derive(Clone, Debug, PartialEq)]
pub struct ToolError {
    pub kind: ErrorKind,
    pub message: String,
}

impl ToolError {
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self { kind, message: message.into() }
    }

    pub fn refused(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Refused, message)
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::NotFound, message)
    }

    pub fn failed(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Failed, message)
    }

    pub fn invalid(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::InvalidArgs, message)
    }

    pub fn to_json(&self) -> Value {
        json::obj(vec![("error", json::s(self.kind.id())), ("message", json::s(self.message.clone()))])
    }
}

impl std::fmt::Display for ToolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.kind.id(), self.message)
    }
}

pub type CallResult = Result<ToolOutput, ToolError>;
