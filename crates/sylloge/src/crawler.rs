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
///   responses. Implementations MUST call
///   [`SearchConstraints::check_url`] on the request URL AND on every
///   redirect target (including the final URL) BEFORE following it --
///   this is not optional per-implementation convention, it is the
///   fail-closed network-target policy (scheme allowlist, no userinfo,
///   no loopback/private/link-local/unspecified/multicast/reserved
///   resolved address; see zetesis#48) that a permitted request URL does
///   NOT extend to a redirect target on its own.
/// - Implementations MUST propagate a failing check's `Err` (typically
///   [`crate::Error::UnsafeTarget`], occasionally
///   [`crate::Error::TransientIo`] if resolution itself fails) rather
///   than following the URL anyway.
/// - Implementations MUST connect to one of the returned
///   [`crate::ValidatedTarget::addrs`] directly rather than letting the
///   underlying HTTP client re-resolve the hostname. Re-resolving after
///   the check passed reopens the exact gap the check closed: the DNS
///   answer can differ between check-time and connect-time (rebinding).
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

    /// Fetch and normalize a single page, subject to the caller's
    /// network-target policy (see the trait-level contract above).
    ///
    /// # Errors
    ///
    /// The returned future resolves to [`crate::Error::UnsafeTarget`] or
    /// [`crate::Error::TransientIo`] if `url` (or any redirect target)
    /// fails [`SearchConstraints::check_url`], and to another
    /// [`crate::Error`] if the page cannot be fetched or parsed.
    fn fetch_page<'a>(
        &'a self,
        url: &'a Url,
        constraints: &'a SearchConstraints,
    ) -> BoxFut<'a, Result<PageContent>>;
}
