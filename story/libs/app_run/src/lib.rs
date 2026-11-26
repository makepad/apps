//! App runs: a typed Splash script that launches a
//! Makepad app from the shared registry, drives it on the virtual clock and
//! records it in process at an exact pixel size.
//!
//! - [`AppRunDoc::load`] parses and checks an `AppRun{...}` file.
//! - [`execute`] builds the app, launches it hidden with `--remote
//!   --virtual-clock --window WxH@scale --seed N`, refuses a window whose
//!   pixels are not the output, runs the script (every call steps whole
//!   frames through `/step`) and writes `run.mp4` and `run.json` (build
//!   identity, output, marks, per-call frames, the focus track, failures).
//!   `RunOptions::validate` resolves the targets against the launch dump
//!   and runs at 5 fps instead.
//! - [`publish_run`] publishes the script as an `AppRun` asset and the MP4
//!   with run.json as its render; [`stale`] says whether a render's cache
//!   key still holds (it never re-runs anything).
//! - [`app_dump`] is the AI authoring blob: registry entry, widget paths at
//!   the target size, the API table.
//!
//! Everything here blocks (child processes, HTTP to the app, git): Stage
//! calls it from a worker, never from its UI thread.

pub mod call;
pub mod doc;
pub mod driver;
#[cfg(feature = "host")]
pub mod dump;
#[cfg(feature = "host")]
pub mod exec;
#[cfg(feature = "host")]
pub mod host;
pub mod identity;
pub mod plan;
#[cfg(feature = "host")]
pub mod publish;
#[cfg(feature = "host")]
mod remote;
pub mod runner;
#[cfg(feature = "host")]
pub mod session;
pub mod track;
pub mod values;

pub use doc::{AppRunDoc, CursorMode, Diagnostic, RunOutput};
#[cfg(feature = "host")]
pub use dump::{app_dump, app_dump_doc, API_TABLE};
#[cfg(feature = "host")]
pub use exec::{compare_marks, execute, unpublished_revision, RunOptions, RunResult, VALIDATE_FPS};
pub use identity::{cache_key, BuildIdentity};
#[cfg(feature = "host")]
pub use publish::{is_stale, publish_run, stale, PublishFailure, PublishedRun};
