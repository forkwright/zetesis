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
//! [`Router`] stores providers as `Arc<dyn Provider>` — with
//! `Send`-bounded futures and no `async-trait` dependency.
//! Implementations wrap method bodies in `Box::pin(async move { .. })`.
//!
//! # Routing and the first provider cohort
//!
//! [`Router`] sends a query to every registered provider that declares its
//! [`QueryShape`], records one receipt per attempt in the result's
//! provenance, refuses paid tiers, and merges the answers by stable
//! identity; its documentation covers routes, receipts, screening,
//! merging, and the cache key.
//!
//! [`SemanticScholar`], [`Arxiv`], and [`Wikipedia`] are the first Tier-0
//! cohort. Each is a pure request builder (`request`: query and
//! [`SearchConstraints`] to a [`ProviderRequest`]) and a structured parser
//! (`parse`: HTTP status, headers, body, and access time to a
//! [`ParsedResponse`] of cited [`ResultHit`]s), with an [`EndpointPolicy`]
//! recording the endpoint's documented terms. Their `Provider`
//! implementations, and the pacing each policy records, land with the HTTP
//! transport. All three serve one language scope and ignore
//! [`SearchConstraints::language`] ([`EndpointPolicy::language_scope`]).
//!
//! ## Provider response mapping
//!
//! | Response | Result |
//! |---|---|
//! | 200 with results | `Ok` with hits in provider rank order |
//! | 200 with an empty result list | `Ok` with no hits |
//! | 200 with a record that lacks a required field, or whose identity or URL is unusable | that record dropped and counted in [`ParsedResponse::malformed_records`]; the rest keep their rank |
//! | 200 whose body does not parse, or whose known field changed type | [`Error::ProviderFailure`] naming the defect |
//! | 400, 414, 422 | [`Error::InvalidQuery`] |
//! | 401, 403 | [`Error::Unauthorized`] |
//! | 429 | [`Error::RateLimited`], with `Retry-After` in milliseconds when present |
//! | 503 with a readable `Retry-After` | [`Error::RateLimited`] with that delay |
//! | any other 4xx | [`Error::PermanentIo`] |
//! | any other 5xx, and any other status | [`Error::ProviderFailure`] |
//!
//! The status decides: a result-shaped body under an error status is still
//! the error. Error messages name the status and the defect and never quote
//! free text from the response body, so upstream text cannot reach a caller
//! through the error channel.
//!
//! ## Provider hit mapping
//!
//! Every hit carries one [`Citation`] whose `accessed_at` is the
//! caller-supplied access time. Its `confidence`, and the hit's `score`, is
//! the reciprocal of the hit's 1-based rank in the provider's answer (1.0,
//! 0.5, 0.33, ...): none of the three endpoints returns a relevance score,
//! so rank is the only relevance signal they give. `content_type` stays
//! `None` because the provider returned metadata about the source, not the
//! source payload.
//!
//! Metadata keys, each present only when the provider supplied the value:
//! `doi` (a publisher DOI, lowercased, no resolver prefix), `arxiv_doi`
//! (the DOI arXiv registers for its own record, which also supplies
//! `arxiv_id`), `arxiv_id` (no version), `arxiv_version`, `s2_paper_id`, `corpus_id`, `pageid`,
//! `authors` (names in order), `year`, `venue`, `license`, and
//! `provider_policy_revision` (the [`EndpointPolicy::revision`] the request
//! was built under).
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
mod net_policy;
mod pacing;
mod provider;
mod providers;
mod query;
mod result;
mod router;
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
    InvalidConstraintSnafu, InvalidQuerySnafu, MissingCitationsSnafu, OversizedPayloadSnafu,
    PermanentIoSnafu, ProviderFailureSnafu, QuotaExhaustedSnafu, RateLimitedSnafu, Result,
    TaskNotReadySnafu, TaskUnavailableSnafu, TimeoutSnafu, TransientIoSnafu, UnauthorizedSnafu,
    UnsafeTargetSnafu, UnsupportedSnafu,
};
pub use fixture::{OfflineFixture, QueryGenerator, SourceRetriever, Synthesizer};
pub use freshness::{
    FreshnessBasis, FreshnessDecision, FreshnessPolicy, PublicationPrecision,
    PublicationProvenance, PublicationTime, PublicationTimeCapability, evaluate_freshness,
};
pub use local_deep_research::LocalDeepResearch;
pub use net_policy::{LocalTargetAuthorization, Resolver, SystemResolver, ValidatedTarget};
pub use provider::{BoxFut, Provider};
pub use providers::{
    Arxiv, EndpointPolicy, ParsedResponse, ProviderRequest, RateLimit, SemanticScholar, Wikipedia,
};
pub use query::QueryShape;
pub use result::{
    AttemptOutcome, EvidenceState, ProvenanceEntry, ProviderAttempt, RefusalReason, ResearchResult,
    ResultHit,
};
pub use router::Router;
pub use tier::ProviderTier;
