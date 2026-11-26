//! Electronics host core: the shared UI driver, its engine strand and the apply/publish passes.
//!
//! The strand owns committed state, previews and history. It publishes shared snapshots and
//! returns pure fill jobs for a pool; completed jobs and retired snapshots return through the
//! mailbox. [`Driver`] schedules that loop; the project-wide choice of which host to undo belongs
//! to the caller.

mod answer;
mod board_actions;
pub mod board_settings;
mod legal_plan;
mod docs;
mod driver;
pub mod doc_actions;
pub mod edit_actions;
pub mod model_actions;
pub mod sheet_actions;
pub mod bench_actions;
pub mod req_actions;
pub mod bom_actions;
mod inbox;
mod engine;
mod eval;
mod plan;
mod refusal;

pub use answer::{gesture_sample, job_sample};
pub use docs::Docs;
pub use driver::{place_action, route_action, Driver, Drained, HostTx};
pub use engine::{CheckJob, Engine, FillJob, JournalEvent, JournalSink, JournalStep, Step, Written};
pub use eval::{EvalJob, EvalDone, EvalOut, MISTAKE};
pub use inbox::{circuits_named, CircuitChange, Cmd, DocSet, Inbox, Mailbox, Outcome, Sample, TaskResult, Ticket, CASE_KEEPOUT};
pub use plan::{route_plan, Job, Plan, Resolver, RouteStart};
