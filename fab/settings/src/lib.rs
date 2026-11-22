//! Fab's settings.
//!
//! - `options`: every profile key with type, axis and default.
//! - `config`: a flat key → value configuration in profile text form.
//! - `profiles`: the user's installed vendor presets: resolution and the
//!   collapsed per-extruder job configuration.
//! - `import`: profiles the user exported (preset files, bundles, projects,
//!   G-code) copied into Fab's own profile folder, with a per-key report.
//! - `template`: the custom G-code template language (both dialects).
//! - `machine`: typed per-extruder / per-filament views the slicer reads.
//! - `family`: the printer families: firmware dialect, job package, link.
//! - `flush`, `change`: Fab's purge volumes per colour pair, and what each change costs.
//! - `home`: the home folder the person's state lives under (a test's own in a test).

pub mod profiles;
pub mod change;
pub mod config;
pub mod family;
pub mod flush;
pub mod home;
pub mod import;
mod import_ini;
pub mod machine;
pub mod options;
mod regex_lite;
pub mod spellings;
pub mod trees;
pub mod template;

pub use config::{Config, Value};
