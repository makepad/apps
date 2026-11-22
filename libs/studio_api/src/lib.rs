//! Stage's AI control API (CUTS.md §10.4): one typed tool registry that
//! drives the CUT page, Motion documents, effects, the library and FLOW
//! generation, app runs, audio sync and feedback, exposed unchanged through
//! three front ends:
//!
//! 1. **In process**, for the CUT page's Generate loop and for Stage's one
//!    chat: Stage's host (`app/src/product/studio_api_host.rs`) runs a
//!    [`Registry`] of live implementations; `ProductApp::studio_call` /
//!    `studio_result`. The chat sees the catalogue as one AI service per
//!    target ([`services`]).
//! 2. **MCP**: `makepad-remote mcp makepad-stage` adapts the running app's
//!    AI service manifests to stdio JSON-RPC.
//! 3. **CLI**: `makepad-remote app makepad-stage call <service>.<tool> '<json>'`.
//!    For example: `makepad-remote app makepad-stage call stage_page.stage_status '{}'`.
//!
//! Driving a running Stage from Claude Code:
//!
//! 1. Build Stage and `makepad-remote`, then start Stage with `--remote`.
//! 2. Add the adapter to the project's `.mcp.json`:
//!
//! ```json
//! {"mcpServers": {"stage": {"command": "/path/to/makepad/target/release/makepad-remote", "args": ["mcp", "makepad-stage"]}}}
//! ```
//!
//! `makepad-remote app makepad-stage tools` lists the manifests. Their
//! service ids and tool groups are defined by [`services::TARGETS`].
//!
//! The table is [`catalog::TOOLS`]; every call is checked against its
//! schema ([`schema`]) before a handler sees it, and errors say how to fix
//! the call. Mutating tools act on the cut open in the CUT page through
//! the page's own operations (undoable, autosaved) and are logged as diff
//! events ([`diff`]).

pub mod base64;
pub mod beats;
pub mod catalog;
pub mod diff;
pub mod output;
pub mod registry;
pub mod schema;
pub mod services;
pub mod spec;

pub use catalog::{find, TOOLS};
pub use diff::{ChangeEvent, ChangeLog};
pub use output::{CallResult, DocChange, ErrorKind, Image, ToolError, ToolOutput};
pub use registry::{Checked, Registry};
pub use spec::{Family, Layout, ToolSpec};
pub mod document;
