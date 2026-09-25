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
//! future router stores them as `Box<dyn Trait>` / `Arc<dyn Trait>` —
//! with `Send`-bounded futures and no `async-trait` dependency.
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
mod provider;
mod query;
mod result;
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
pub use provider::{BoxFut, Provider};
pub use query::QueryShape;
pub use result::{ProvenanceEntry, ResearchResult, ResultHit};
pub use tier::ProviderTier;
