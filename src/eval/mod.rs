//! Milestone 8, quality evaluation: does a multi-agent consultation answer better than one agent?
//!
//! This module answers that question with data and refuses to answer it without. Its pieces:
//!
//! * [`dataset`]: evaluation cases with their own reference evidence (never invented ground truth);
//! * [`citations`]: mechanical citation checks, kept apart from semantic support;
//! * [`rubric`]: deterministic checks per criterion; judgement criteria are left to people;
//! * [`blind`]: reproducible blind pairwise comparison and recorded judgments;
//! * [`stats`]: descriptive statistics and seeded bootstrap intervals, with sample-size limits;
//! * [`run`]: run records, the offline runner, storage;
//! * [`report`]: reports that keep fixture, mock, recorded and live results apart;
//! * [`live`]: the live runner, which spends subscription quota and is never started by default;
//! * [`feedback`]: explicit user feedback, stored locally and nothing more.
//!
//! Nothing here calls a provider except [`live`], and that only when the caller has explicitly
//! authorised it. No result is merged across provenance classes.

pub mod blind;
pub mod citations;
pub mod dataset;
pub mod feedback;
pub mod live;
pub mod report;
pub mod rubric;
pub mod run;
pub mod stats;

use std::path::PathBuf;

use crate::config::Config;

/// Where local evaluation data lives: beside the memory database.
fn data_dir(config: &Config) -> Option<PathBuf> {
    config.resolved_memory_path()?.parent().map(PathBuf::from)
}

/// The directory of evaluation runs.
pub fn runs_dir(config: &Config) -> Option<PathBuf> {
    Some(data_dir(config)?.join("evals"))
}

/// The user feedback file.
pub fn feedback_path(config: &Config) -> Option<PathBuf> {
    Some(data_dir(config)?.join("feedback.json"))
}
