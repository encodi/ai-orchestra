//! Smart orchestration (Milestone 6): choosing how much AI work a consultation needs, keeping it
//! inside explicit budgets, deciding when to escalate or stop, checking that the result is honestly
//! classified, and measuring all of it.
//!
//! Everything here is deterministic and explainable: no model call is ever made to decide how to
//! orchestrate, and nothing is learned or predicted. The pieces:
//!
//! * `strategy` and `engine`: the strategy engine turns the question, the routing result, the
//!   user's explicit settings, the budgets and the provider history into an execution plan
//!   (strategy, agents, rounds, flow) with a human-readable explanation.
//! * `budget`: the budget manager enforces the limits while the consultation runs (calls,
//!   wall-clock deadline, parallelism, response size).
//! * `adaptive`: the deliberation controller decides, from structured results only, whether to
//!   continue, add an agent or finish.
//! * `quality`: the quality gate checks structural properties of the result.
//! * `profiles`: bounded local statistics about how each provider behaved.
//! * `reuse`: identical calls inside one consultation are answered once.
//! * `trace`, `metrics` and `run`: the decision trace and the measured metrics stored with the
//!   consultation.

pub mod adaptive;
pub mod budget;
pub mod engine;
pub mod metrics;
pub mod profiles;
pub mod quality;
pub mod reuse;
pub mod run;
pub mod strategy;
pub mod trace;
