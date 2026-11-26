//! The evaluation harness for AI-generated videos.
//!
//! A fixed benchmark of briefs ([`bench`]) is run through a headless
//! generator ([`contract`]), each result is measured ([`metrics`], through
//! `makepad-edit-tools` in [`tools`]), pictured and judged against the
//! written rubric ([`judge`]), and the run is written as a table and a
//! gallery ([`report`]) so guide/prompt versions and models can be compared.

pub mod bench;
pub mod contract;
pub mod eval;
pub mod judge;
pub mod metrics;
pub mod report;
pub mod tools;
