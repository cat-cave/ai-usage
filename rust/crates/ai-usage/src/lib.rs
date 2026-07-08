//! `ai-usage` — library for tracking AI coding-provider capacity and recommending
//! a provider for a task. See `SPEC.md`.
//!
//! The library is side-effect free: no `println!`/`eprintln!`. The CLI crate owns
//! all rendering.

#![forbid(unsafe_code)]

pub mod config;
pub mod error;
pub mod model;
pub mod provider;
pub mod providers;
pub mod recommend;
pub mod redact;
pub mod report;
pub mod source;
#[cfg(test)]
mod test_support;
pub mod view;
pub mod walk;

pub use model::{ProviderId, ProviderReport, ProviderStatus};
pub use provider::{Provider, Registry};
pub use recommend::{Recommendation, TaskKind};
pub use report::{AggregateReport, SCHEMA_VERSION};
pub use source::{Freshness, OptionalSourced, Source, Sourced, UnavailableReason};
