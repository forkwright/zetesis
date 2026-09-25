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
//! - [`Connector`] — the connection-binding seam [`StaticAcquirer`] opens
//!   each validated address through; consumer egress adapters implement it.
//!
//! All three traits hand-roll their async methods as [`BoxFut`] returns
//! (`Pin<Box<dyn Future + Send>>`) so they stay dyn-compatible — the
//! [`Router`] stores providers as `Arc<dyn Provider>` — with
//! `Send`-bounded futures and no `async-trait` dependency.
//! Implementations wrap method bodies in `Box::pin(async move { .. })`.
//!
//! # Static acquisition
//!
//! [`StaticAcquirer`] is the one concrete anonymous `GET` fetcher. It owns
//! every hop of a transfer, validates each hop's target before any socket,
//! connects only to the validated addresses, decodes the bounded body,
//! extracts its static text, and returns an [`Acquisition`]: the versioned
//! [`EvidenceEnvelope`] to store verbatim and the decoded body bytes. See
//! [`StaticAcquirer`] for the policy and [`EvidenceEnvelope`] for the
//! schema, fingerprint, and [`replay`].
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
//! cohort. Each has a pure request builder (`request`: query and
//! [`SearchConstraints`] to a [`ProviderRequest`]) and a structured parser
//! (`parse`: HTTP status, headers, body, and access time to a
//! [`ParsedResponse`] of cited [`ResultHit`]s), with an [`EndpointPolicy`]
//! recording the endpoint's documented terms. Each implements [`Provider`]
//! over a shared [`StaticAcquirer`]: it sends only its documented anonymous
//! request (its own `Accept` media type, a `User-Agent`, no credential),
//! and the acquirer accepts only that media type, keeps the body as bytes
//! without extracting text, and bounds the transfer as it does any other.
//! [`Provider::search_with_evidence`] returns the envelope fingerprint
//! beside the result, and the [`Router`] copies it onto the attempt's
//! receipt ([`ProviderAttempt::evidence_fingerprints`]); the envelope and
//! body are not kept. [`Provider::min_request_interval`] spaces a
//! provider's attempts: arXiv's comes from its policy (3 s), Wikipedia's
//! from its policy (300 ms), and Semantic Scholar's from the caller,
//! because its shared anonymous pool documents no per-client rate. All
//! three serve one language scope and ignore [`SearchConstraints::language`]
//! ([`EndpointPolicy::language_scope`]).
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
//! the error, and an error status is mapped even when the acquirer refused
//! its body (an error page in another media type). Error messages name the
//! status and the defect and never quote free text from the response body,
//! so upstream text cannot reach a caller through the error channel.
//!
//! When the acquisition itself fails, no status decides and the failure
//! keeps its class ([`AcquisitionFailure::class`]):
//!
//! | Acquisition failure | Result |
//! |---|---|
//! | the acquisition deadline passed | [`Error::Timeout`] with that deadline |
//! | resolution, connect, connect timeout, or an interrupted body | [`Error::TransientIo`] naming the failure kind |
//! | a policy refusal (unsafe target, scheme, port, egress, redirect) or a limit, TLS, protocol, encoding, or media-type failure (another media type on a 200) | [`Error::PermanentIo`] naming the failure kind |
//!
//! ## Provider hit mapping
//!
//! Every hit carries one [`Citation`] whose `accessed_at` is the
//! caller-supplied access time. Its `confidence`, and the hit's `score`, is
//! the reciprocal of the hit's 1-based rank in the provider's answer (1.0,
//! 0.5, 0.33, ...): none of the three endpoints returns a relevance score,
//! so rank is the only relevance signal they give. `content_type` stays
//! `None` because the provider returned metadata about the source, not the
//! source payload. A Wikipedia excerpt loses its search-highlight markup
//! and then has its character references decoded, with the same decoder
//! the static extractor uses.
//!
//! Metadata keys, each present only when the provider supplied the value:
//! `doi` (a publisher DOI, lowercased, no resolver prefix), `arxiv_doi`
//! (the DOI arXiv registers for its own record, which also supplies
//! `arxiv_id`), `arxiv_id` (no version), `arxiv_version`, `s2_paper_id`,
//! `corpus_id`, `pageid`, `authors` (names in order), `year`, `venue`,
//! `license`, and
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

mod acquisition;
mod budget;
mod citation;
mod constraints;
mod cost;
mod deep;
mod digest;
mod error;
mod evidence;
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

pub use acquisition::{
    AcquisitionFailure, AcquisitionLimits, ConnectAttempt, ConnectDeniedSnafu, ConnectError,
    ConnectIoSnafu, ConnectOutcome, ConnectTimedOutSnafu, ConnectedStream, Connector,
    DirectConnector, DowngradePolicy, HopRecord, ResponseRecord, SchemePolicy, StaticAcquirer,
    StaticAcquirerBuilder, TlsRecord, TrustAnchors,
};
pub use budget::{BudgetConstraint, BudgetScope, DAY_WINDOW, SpendEvent, SpendLedger};
pub use citation::{Citation, SourceKind};
pub use constraints::{DeepDepth, ResearchStatus, SearchConstraints, TaskId};
pub use cost::{CostTracking, ProviderId, ProviderSpend};
pub use deep::DeepResearch;
pub use error::{
    BudgetExceededSnafu, DomainDeniedSnafu, Error, ErrorClass, FatalCorruptionSnafu,
    InvalidConstraintSnafu, InvalidQuerySnafu, MissingCitationsSnafu, OversizedPayloadSnafu,
    PermanentIoSnafu, ProviderFailureSnafu, QuotaExhaustedSnafu, RateLimitedSnafu, Result,
    TaskNotReadySnafu, TaskUnavailableSnafu, TimeoutSnafu, TransientIoSnafu, UnauthorizedSnafu,
    UnsafeTargetSnafu, UnsupportedSnafu,
};
pub use evidence::decode::ContentCoding;
pub use evidence::envelope::{
    Acquisition, BodyRecord, EVIDENCE_SCHEMA_ID, EVIDENCE_SCHEMA_VERSION, EvidenceEnvelope,
    ExtractionRecord, ExtractorId, Outcome, PartialReason, Producer, ReplayOutcome, replay,
};
pub use evidence::html_text::Segment;
pub use evidence::media::{Charset, CharsetSource, Media};
pub use fixture::{OfflineFixture, QueryGenerator, SourceRetriever, Synthesizer};
pub use freshness::{
    FreshnessBasis, FreshnessDecision, FreshnessPolicy, PublicationPrecision,
    PublicationProvenance, PublicationTime, PublicationTimeCapability, evaluate_freshness,
};
pub use local_deep_research::LocalDeepResearch;
pub use net_policy::{LocalTargetAuthorization, Resolver, SystemResolver, ValidatedTarget};
pub use provider::{BoxFut, Provider, ProviderAnswer};
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
