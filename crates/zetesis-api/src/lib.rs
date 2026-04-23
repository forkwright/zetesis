//! Trait surface for the zetesis sovereign research substrate.
//!
//! This crate defines the three top-level async traits every provider
//! implementation must satisfy, the search-time constraints passed into
//! those traits, and the error taxonomy providers report failures through.
//!
//! # Traits
//!
//! - [`Provider`] — synchronous single-shot search. Returns a
//!   [`zetesis_types::ResearchResult`] in one round trip.
//! - [`DeepResearch`] — multi-step research with async task lifecycle
//!   (submit → poll → fetch).
//! - [`Crawler`] — per-URL full-page content retrieval for when a hit
//!   needs the body extracted.
//!
//! All three traits are `async_trait`-based so they can be stored as
//! `Box<dyn Trait>` in the future `zetesis-router`. See the lib's
//! `WHY async-trait` comment in `Cargo.toml` for rationale.
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

mod constraints;
mod crawler;
mod deep;
mod error;
mod provider;

pub use constraints::{DeepDepth, PageContent, ResearchStatus, SearchConstraints, TaskId};
pub use crawler::Crawler;
pub use deep::DeepResearch;
pub use error::{
    BudgetExceededSnafu, Error, ErrorClass, FatalCorruptionSnafu, InvalidQuerySnafu,
    PermanentIoSnafu, ProviderFailureSnafu, QuotaExhaustedSnafu, RateLimitedSnafu, Result,
    TimeoutSnafu, TransientIoSnafu, UnauthorizedSnafu, UnsupportedSnafu,
};
pub use provider::Provider;

// Re-export the shared types under this crate's namespace so consumers can
// depend on one crate for "everything zetesis" and only pull in
// zetesis-types directly when they want the smaller surface.
pub use zetesis_types::{
    BudgetConstraint, Citation, CostTracking, ProvenanceEntry, ProviderSpend, ProviderTier,
    QueryShape, ResearchResult, ResultHit, SourceKind,
};
