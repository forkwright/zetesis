//! The [`Crawler`] async trait.
//!
//! Crawlers handle the per-URL full-page extraction step — after a
//! [`super::Provider`] returns a hit with just a snippet, the caller may
//! want the full text. Zetesis does not own the crawling loop itself (the
//! README's non-goals list it); implementations here are thin wrappers
//! around existing extractors (Firecrawl, trafilatura, raw reqwest +
//! readability) that normalize output to [`super::PageContent`].

use url::Url;

use crate::constraints::{PageContent, SearchConstraints};
use crate::error::Result;
use crate::provider::BoxFut;

/// Per-URL full-page extractor.
///
/// Implementations must be `Send + Sync`.
///
/// # Contract
///
/// - `fetch_page` receives URLs that originate from untrusted provider
///   responses. Implementations MUST enforce the caller's domain rules:
///   reject the request URL with [`crate::Error::DomainDenied`] when
///   [`SearchConstraints::permits_url`] returns `false`, and apply the
///   same check to every redirect target (including the final URL) so a
///   permitted URL cannot bounce into a denied domain.
/// - `fetch_page` must return [`PageContent`] with at minimum the
///   `final_url`, `content_type`, and `body` filled in. Implementations
///   that can extract plain text should do so and fill `extracted_text`.
/// - Providers that do not support crawling should not implement this
///   trait at all (rather than implementing it and returning
///   [`crate::Error::Unsupported`] for every call). The trait exists so
///   callers can check "does this backend support full-page extraction"
///   by type, not by runtime error.
pub trait Crawler: Send + Sync {
    /// Stable crawler identifier.
    fn name(&self) -> &'static str;

    /// Fetch and normalize a single page, subject to the caller's domain
    /// constraints.
    ///
    /// # Errors
    ///
    /// The returned future resolves to [`crate::Error::DomainDenied`] if
    /// `url` (or any redirect target) fails
    /// [`SearchConstraints::permits_url`], and to another [`crate::Error`]
    /// if the page cannot be fetched or parsed.
    fn fetch_page<'a>(
        &'a self,
        url: &'a Url,
        constraints: &'a SearchConstraints,
    ) -> BoxFut<'a, Result<PageContent>>;
}
