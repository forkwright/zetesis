//! The [`Crawler`] async trait.
//!
//! Crawlers handle the per-URL full-page extraction step — after a
//! [`super::Provider`] returns a hit with just a snippet, the caller may
//! want the full text. Zetesis does not own the crawling loop itself (the
//! README's non-goals list it); implementations here are thin wrappers
//! around existing extractors (Firecrawl, trafilatura, raw reqwest +
//! readability) that normalize output to [`super::PageContent`].

use async_trait::async_trait;
use url::Url;

use crate::constraints::PageContent;
use crate::error::Result;

/// Per-URL full-page extractor.
///
/// Implementations must be `Send + Sync`.
///
/// # Contract
///
/// - `fetch_page` must return [`PageContent`] with at minimum the
///   `final_url`, `content_type`, and `body` filled in. Implementations
///   that can extract plain text should do so and fill `extracted_text`.
/// - Providers that do not support crawling should not implement this
///   trait at all (rather than implementing it and returning
///   [`crate::Error::Unsupported`] for every call). The trait exists so
///   callers can check "does this backend support full-page extraction"
///   by type, not by runtime error.
#[async_trait]
pub trait Crawler: Send + Sync {
    /// Stable crawler identifier.
    fn name(&self) -> &'static str;

    /// Fetch and normalize a single page.
    ///
    /// # Errors
    ///
    /// Returns [`crate::Error`] if the page cannot be fetched or parsed.
    async fn fetch_page(&self, url: &Url) -> Result<PageContent>;
}
