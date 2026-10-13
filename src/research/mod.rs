//! Research and fact checking (Milestone 5): safe retrieval of real web pages, source records,
//! claims, evidence verification and citations.
//!
//! The principle of the whole module: **evidence is what was retrieved**. A model's statement,
//! a URL a model wrote, or the fact that several models agree is never evidence. Pages are fetched
//! by this program ([`fetch`]), claims are checked against passages we extracted ourselves, and
//! the sources and citations in the final answer are generated from the recorded sources.

pub mod capabilities;
pub mod citations;
pub mod claims;
pub mod discovery;
pub mod engine;
pub mod fetch;
pub mod html;
pub mod net;
pub mod plan;
pub mod quality;
pub mod robots;
pub mod source;
#[cfg(test)]
mod testserver;
pub mod verify;
