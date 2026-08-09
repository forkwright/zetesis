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
//! All three traits hand-roll their async methods as [`BoxFut`] returns
//! (`Pin<Box<dyn Future + Send>>`) so they stay dyn-compatible — the
//! future router stores them as `Box<dyn Trait>` / `Arc<dyn Trait>` —
//! with `Send`-bounded futures and no `async-trait` dependency.
//! Implementations wrap method bodies in `Box::pin(async move { .. })`.
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
mod fixture;
mod freshness;
mod local_deep_research;
mod provider;
mod query;
mod result;
mod serde_util;
mod tier;

pub use budget::{BudgetConstraint, BudgetScope, DAY_WINDOW, SpendEvent, SpendLedger};
pub use citation::{Citation, SourceKind};
pub use constraints::{DeepDepth, PageContent, ResearchStatus, SearchConstraints, TaskId};
pub use cost::{CostTracking, ProviderId, ProviderSpend};
pub use crawler::Crawler;
pub use deep::DeepResearch;
pub use error::{
    BudgetExceededSnafu, DomainDeniedSnafu, Error, ErrorClass, FatalCorruptionSnafu,
    InvalidQuerySnafu, MissingCitationsSnafu, OversizedPayloadSnafu, PermanentIoSnafu,
    ProviderFailureSnafu, QuotaExhaustedSnafu, RateLimitedSnafu, Result, TaskNotReadySnafu,
    TaskUnavailableSnafu, TimeoutSnafu, TransientIoSnafu, UnauthorizedSnafu, UnsupportedSnafu,
};
pub use fixture::{OfflineFixture, QueryGenerator, SourceRetriever, Synthesizer};
pub use freshness::{
    FreshnessBasis, FreshnessDecision, FreshnessPolicy, PublicationPrecision,
    PublicationProvenance, PublicationTime, PublicationTimeCapability, evaluate_freshness,
};
pub use local_deep_research::LocalDeepResearch;
pub use provider::{BoxFut, Provider};
pub use query::QueryShape;
pub use result::{ProvenanceEntry, ResearchResult, ResultHit};
pub use tier::ProviderTier;
