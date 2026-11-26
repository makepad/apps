//! Stage's Generate agent: a model makes a video as
//! ordinary Splash in the cut open on the CUT page, and checks its own
//! work by looking at it.
//!
//! - [`agent`] — the loop (plan → write → inspect → look → critique → fix →
//!   finish), passive and host-driven, with its transcript, storyboard and
//!   per-document diffs (hand edits included).
//! - [`brain`] — the models: Claude Code, the Claude API ([`claude_api`]),
//!   the AI Hub's LAN chat nodes.
//! - [`tools`] — the agent's own tools and the typing of call arguments by
//!   the host's JSON schemas; [`calls`] — the text protocol for backends
//!   without native tools.
//! - [`guide`] — the system text: the authoring guide (the Claude Code
//!   skill `stage-motion`), the process, the brief.
//! - [`team`] — a director with parallel scene authors.
//! - [`diff`] — line diffs of documents.

pub mod agent;
pub mod brain;
pub mod calls;
pub mod claude_api;
pub mod diff;
pub mod guide;
pub mod team;
pub mod tools;

pub use agent::{Action, Agent, Author, Brief, CallState, Doc, DocChange, Entry, InspectFacts, Kind, Limits, Outcome, Phase, Revision, State};
pub use brain::{Ask, Backend, Brain, BrainEvent, BrainImage, CallResult, DEFAULT_CLAUDE_MODEL};
pub use team::{Authors, Crew, Team, TeamAction, DIRECTOR};
pub use tools::{Critique, Scene, Storyboard, ToolDef};
