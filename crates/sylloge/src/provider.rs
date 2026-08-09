//! The top-level [`Provider`] async trait.
//!
//! Every free-quality API, paid API, and self-hosted endpoint in the
//! fleet's research stack implements `Provider`. Phase 1b Kimi-dispatched
//! agents will land six Tier 0 providers (Semantic Scholar, arXiv,
//! `OpenAlex`, Crossref, `PubMed`, Wikipedia) against this trait.

use std::future::Future;
use std::pin::Pin;

use crate::constraints::SearchConstraints;
use crate::error::Result;
use crate::freshness::PublicationTimeCapability;
use crate::{ProviderTier, ResearchResult};

/// `Send`-bounded boxed future returned by every async method on the
/// [`Provider`], [`crate::DeepResearch`], and [`crate::Crawler`] traits.
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
/// - [`Provider::tier`] returns the static tier classification. Used by
///   the router to pick ordering in the fallback chain.
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
