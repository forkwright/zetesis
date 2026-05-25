//! Provider, routing, budget, and research-result surface for zetesis.
//!
//! This crate folds the old `zetesis-api` and `zetesis-types` branch work
//! into the locked `sylloge` boundary. It owns the provider traits, search
//! constraints, error taxonomy, cited result schema, budget/cost ledger
//! primitives, and deep-research lifecycle values.
//!
//! # Traits
//!
//! - [`Provider`] — single-shot search. Returns a
//!   [`crate::ResearchResult`] in one round trip.
//! - [`DeepResearch`] — multi-step research with async task lifecycle
//!   (submit → poll → fetch).
//! - [`Crawler`] — per-URL full-page content retrieval for when a hit
//!   needs the body extracted.
//!
//! All three traits are `async_trait`-based so they can be stored as
//! `Box<dyn Trait>` in the future router. See `Cargo.toml` for the
//! `async-trait` rationale.
//!
//! # Error taxonomy
//!
//! [`Error`] is a flat snafu enum; every variant carries a
//! `#[snafu(implicit)] location: snafu::Location`. The [`Error::is_transient`]
//! classifier lets callers decide whether a failure is worth retrying.
//!
//! # Stability
//!
//! Every pub enum carries `#[non_exhaustive]`. Adding an [`Error`] variant
//! or a new [`SearchConstraints`] field is a minor-version change.

#![deny(missing_docs)]
#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

mod budget;
mod citation;
mod constraints;
mod cost;
mod crawler;
mod deep;
mod error;
mod local_deep_research;
mod provider;
mod query;
mod result;
mod tier;

pub use budget::BudgetConstraint;
pub use citation::{Citation, SourceKind};
pub use constraints::{DeepDepth, PageContent, ResearchStatus, SearchConstraints, TaskId};
pub use cost::{CostTracking, ProviderSpend};
pub use crawler::Crawler;
pub use deep::DeepResearch;
pub use error::{
    BudgetExceededSnafu, Error, ErrorClass, FatalCorruptionSnafu, InvalidQuerySnafu,
    PermanentIoSnafu, ProviderFailureSnafu, QuotaExhaustedSnafu, RateLimitedSnafu, Result,
    TimeoutSnafu, TransientIoSnafu, UnauthorizedSnafu, UnsupportedSnafu,
};
pub use local_deep_research::LocalDeepResearch;
pub use provider::Provider;
pub use query::QueryShape;
pub use result::{ProvenanceEntry, ResearchResult, ResultHit};
pub use tier::ProviderTier;
