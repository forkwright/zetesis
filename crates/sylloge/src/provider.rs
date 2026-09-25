//! The top-level [`Provider`] async trait.
//!
//! Every free-quality API, paid API, and self-hosted endpoint in the
//! fleet's research stack implements `Provider`. The first cohort
//! (Semantic Scholar, arXiv, Wikipedia) is Phase 03 work; further providers
//! join only through the same conformance fixtures and a current endpoint
//! policy.

use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use crate::constraints::SearchConstraints;
use crate::error::Result;
use crate::freshness::PublicationTimeCapability;
use crate::{ProviderTier, QueryShape, ResearchResult};

/// `Send`-bounded boxed future returned by every async method on the
/// [`Provider`], [`crate::DeepResearch`], and [`crate::Connector`] traits.
///
/// WHY: native `async fn` in traits is not dyn-compatible, and the router
/// stores backends as `Arc<dyn Provider>` / `Box<dyn DeepResearch>`.
/// Hand-rolling each async method as `fn name(..) -> BoxFut<'_, T>` keeps
/// the traits object-safe with `Send` futures — the same surface the
/// banned `async-trait` crate generated, without the dependency.
/// Implementations wrap their bodies in `Box::pin(async move { .. })`.
pub type BoxFut<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Single-shot search provider.
///
/// Implementations must be `Send + Sync` and (typically) cheap to clone:
/// the router stores providers behind `Arc<dyn Provider>` and may call
/// `search` concurrently from multiple tasks.
///
/// # Contract
///
/// - [`Provider::name`] returns a stable, lowercase, unique identifier.
///   The [`crate::CostTracking`] layer keys by this name. Two
///   providers returning the same name collapse in the ledger.
/// - [`Provider::tier`] returns the static tier classification. The
///   [`crate::Router`] refuses every paid tier until durable budget
///   enforcement exists.
/// - [`Provider::query_shapes`] declares which [`QueryShape`]s the provider
///   serves. The [`crate::Router`] attempts a provider only for a shape it
///   declares.
/// - [`Provider::search`] is the async call itself. Every return path must
///   produce either a populated [`ResearchResult`] or a structured
///   [`crate::Error`]. Panicking counts as a corruption bug.
///
/// # Cancellation
///
/// `search()` must be cancellation-safe: dropping the returned future
/// mid-`.await` must not leak partial results or budget the ledger hasn't
/// seen. Providers that issue multiple upstream HTTP calls should use a
/// scoped `JoinSet` so dropping the outer future aborts the in-flight
/// calls.
pub trait Provider: Send + Sync {
    /// Stable provider identifier.
    fn name(&self) -> &'static str;

    /// Tier this provider belongs to.
    fn tier(&self) -> ProviderTier;

    /// Query shapes this provider serves.
    ///
    /// Defaults to none, which keeps a provider that does not declare its
    /// shapes out of every route instead of letting it answer queries it
    /// was never built for.
    fn query_shapes(&self) -> &[QueryShape] {
        &[]
    }

    /// Declares whether this provider can supply
    /// [`crate::PublicationTime::Known`] values on the citations it
    /// returns, and at what best-case precision.
    ///
    /// Defaults to [`PublicationTimeCapability::Unsupported`]. Providers
    /// that DO supply a publication or last-updated time (an API field, an
    /// HTTP `Last-Modified` header) must override this so callers can
    /// decide whether [`crate::FreshnessPolicy::Strict`] is usable against
    /// this provider without rejecting every hit it returns.
    fn publication_time_capability(&self) -> PublicationTimeCapability {
        PublicationTimeCapability::Unsupported
    }

    /// Minimum spacing between this provider's requests, which the
    /// [`crate::Router`] enforces per provider. Defaults to zero: no
    /// spacing beyond `Retry-After`.
    fn min_request_interval(&self) -> Duration {
        Duration::ZERO
    }

    /// Execute a search.
    ///
    /// # Errors
    ///
    /// The returned future resolves to [`crate::Error`] if the provider
    /// rejects the query, fails to reach its upstream, or surfaces a
    /// transport-level failure. The caller uses
    /// [`crate::Error::is_transient`] to decide whether to retry.
    fn search<'a>(
        &'a self,
        query: &'a str,
        constraints: &'a SearchConstraints,
    ) -> BoxFut<'a, Result<ResearchResult>>;

    /// Execute a search and report the evidence behind the answer, whether
    /// it succeeded or failed. The [`crate::Router`] calls this and records
    /// the evidence on the attempt receipt.
    ///
    /// Defaults to [`Provider::search`] with no evidence; a provider that
    /// fetches through a [`crate::StaticAcquirer`] reports each envelope's
    /// fingerprint.
    fn search_with_evidence<'a>(
        &'a self,
        query: &'a str,
        constraints: &'a SearchConstraints,
    ) -> BoxFut<'a, ProviderAnswer> {
        Box::pin(async move { ProviderAnswer::from(self.search(query, constraints).await) })
    }
}

/// One provider call's answer and the evidence it gathered.
#[derive(Debug)]
#[non_exhaustive]
pub struct ProviderAnswer {
    /// The search result, or the error the call ended in.
    pub result: Result<ResearchResult>,
    /// Fingerprints of the evidence envelopes the call produced, in order.
    /// Evidence identity only: the envelopes and bodies are not kept.
    pub evidence_fingerprints: Vec<String>,
}

impl From<Result<ResearchResult>> for ProviderAnswer {
    /// An answer that gathered no evidence.
    fn from(result: Result<ResearchResult>) -> Self {
        Self {
            result,
            evidence_fingerprints: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ProviderTier;

    struct MinimalStub;

    impl Provider for MinimalStub {
        fn name(&self) -> &'static str {
            "minimal_stub"
        }

        fn tier(&self) -> ProviderTier {
            ProviderTier::Tier0Free
        }

        fn search<'a>(
            &'a self,
            _query: &'a str,
            _constraints: &'a SearchConstraints,
        ) -> BoxFut<'a, Result<ResearchResult>> {
            Box::pin(async move { unreachable!("not exercised by this test") })
        }
    }

    #[test]
    fn a_provider_paces_nothing_and_reports_no_evidence_by_default() {
        assert_eq!(
            MinimalStub.min_request_interval(),
            Duration::ZERO,
            "no spacing unless the provider declares one"
        );
        let answer = ProviderAnswer::from(Ok(ResearchResult::empty(
            "q",
            QueryShape::QuickFactual,
            "k",
        )));
        assert!(
            answer.evidence_fingerprints.is_empty(),
            "an answer built from a plain result carries no evidence"
        );
    }

    #[test]
    fn query_shapes_default_to_none() {
        // WHY: a provider that never declared its shapes must not be routed
        // any query by default.
        assert!(
            MinimalStub.query_shapes().is_empty(),
            "an undeclared provider serves no shape"
        );
    }

    #[test]
    fn publication_time_capability_defaults_to_unsupported() {
        // WHY(zetesis#50): a provider that does not override the default
        // must be assumed incapable of publication-time reporting, not
        // silently treated as capable.
        assert_eq!(
            MinimalStub.publication_time_capability(),
            PublicationTimeCapability::Unsupported
        );
    }
}
