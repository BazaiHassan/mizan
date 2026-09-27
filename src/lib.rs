//! mzn: tunes RTK to each project.
//!
//! Reads Claude Code session logs locally, measures how much command output
//! RTK filters, and generates project-specific RTK filters for the rest.
//! No network access, no LLM calls, no telemetry.

pub mod analyze;
pub mod cmdkey;
pub mod doctor;
pub mod paths;
pub mod report;
pub mod rtk;
pub mod session;
pub mod settings;
pub mod suggest;
pub mod util;
