//! Milestone 8, usability: the interactive chat, its settings, how results are shown.
//!
//! This layer only reads and drives the existing engines (`Orchestrator::consult`, memory,
//! history). It adds no orchestration logic of its own.

pub mod chat;
pub mod prefs;
pub mod present;
pub mod settings;
