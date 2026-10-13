//! AI Orchestra: local-first orchestration of the official Claude Code,
//! Codex and Antigravity CLIs through managed subprocesses.
//!
//! No model APIs, API keys or credentials are touched; every request goes
//! through a CLI the user has already signed in to.

pub mod config;
pub mod consult;
pub mod debate;
pub mod doctor;
pub mod error;
pub mod eval;
pub mod history;
pub mod mcp;
pub mod mcp_memory;
pub mod memory;
pub mod orchestrator;
pub mod process;
pub mod progress;
pub mod provider;
pub mod providers;
pub mod research;
pub mod smart;
pub mod store;
pub mod style;
pub mod summary;
pub mod tasks;
pub mod types;
pub mod ux;
pub mod web;

pub use config::Config;
pub use error::{OrchestraError, Result};
pub use orchestrator::Orchestrator;
pub use provider::{AgentProvider, ProviderRegistry};
pub use types::{AccessMode, AgentRequest, AgentResponse, ProviderId};
