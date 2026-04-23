//! The top-level [`Provider`] async trait.
//!
//! Every free-quality API, paid API, and self-hosted endpoint in the
//! fleet's research stack implements `Provider`. Phase 1b Kimi-dispatched
//! agents will land six Tier 0 providers (Semantic Scholar, arXiv,
//! OpenAlex, Crossref, PubMed, Wikipedia) against this trait.

use async_trait::async_trait;

use zetesis_types::{ProviderTier, ResearchResult};

use crate::constraints::SearchConstraints;
use crate::error::Result;

/// Single-shot search provider.
///
/// Implementations must be `Send + Sync` and (typically) cheap to clone:
/// the router stores providers behind `Arc<dyn Provider>` and may call
/// `search` concurrently from multiple tasks.
///
/// # Contract
///
/// - [`Provider::name`] returns a stable, lowercase, unique identifier.
///   The [`zetesis_types::CostTracking`] layer keys by this name. Two
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
#[async_trait]
pub trait Provider: Send + Sync {
    /// Stable provider identifier.
    fn name(&self) -> &'static str;

    /// Tier this provider belongs to.
    fn tier(&self) -> ProviderTier;

    /// Execute a search.
    ///
    /// # Errors
    ///
    /// Returns [`crate::Error`] if the provider rejects the query, fails
    /// to reach its upstream, or surfaces a transport-level failure. The
    /// caller uses [`crate::Error::is_transient`] to decide whether to
    /// retry.
    async fn search(&self, query: &str, constraints: &SearchConstraints) -> Result<ResearchResult>;
}
