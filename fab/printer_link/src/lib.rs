//! The printer link: telemetry, storage (FTPS) and typed print control.
//! Prints go through [`Command::Print`] with a safety-checked file.
//! Parsing and TLS run on one worker. The UI checks an atomic revision per event;
//! the app maps its print colours onto the reported trays on publications and edits only.

pub mod camera;
pub mod ftps;
pub mod job;
mod link;
mod inspect;
pub mod model;
#[cfg(not(target_arch = "wasm32"))]
pub mod moonraker;
#[cfg(not(target_arch = "wasm32"))]
pub mod moonraker_sim;
#[cfg(not(target_arch = "wasm32"))]
mod moonraker_link;
#[cfg(not(target_arch = "wasm32"))]
pub(crate) mod mqtt;
pub mod safety;
pub mod secrets;
#[cfg(not(target_arch = "wasm32"))]
pub mod sim;
#[cfg(not(target_arch = "wasm32"))]
mod tls;

pub use job::{JobStage, JobStatus, PrintJob, PrintOptions};
pub use link::{Command, CommandState, ConnectionStatus, PrinterLink, Snapshot};

/// Why print actions are hidden on this platform.
pub const UNSUPPORTED_PRINTING: &str = "Printing to this printer isn't supported on Windows yet";

/// Upload and start/pause/stop exist on macOS and Linux; Windows stays read-only
/// (its TLS stack cannot resume the printer's storage sessions).
pub const fn printing_supported() -> bool { cfg!(not(any(windows, target_arch = "wasm32"))) }
pub use model::{NozzleSide, PrinterState};
pub use secrets::first_config_name;
