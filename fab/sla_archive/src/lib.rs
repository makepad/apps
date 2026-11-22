//! The archive a masked resin printer reads from a USB stick.
//!
//! - [`format`]: the format's required bytes (entry names, magic, markers, JSON keys) and the
//!   maker's own values a file copies, in one place;
//! - [`read`]: an independent reader. It checks a file the way the printer reads it (every rule
//!   and checksum of the layout) and decodes its layers back to pixels. It shares no code with
//!   Fab's writer, so it is the writer's test oracle, and Print reads every file it saves back
//!   through it.

pub mod format;
pub mod read;
