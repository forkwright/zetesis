//! English Wikipedia page search through the per-wiki `MediaWiki` REST API.

use std::sync::Arc;
use std::time::Duration;

use jiff::Timestamp;
use serde::Deserialize;
use serde_json::Value;
use url::Url;

use super::{
    Endpoint, EndpointPolicy, HitParts, MEDIA_JSON, META_PAGEID, ParsedResponse, ProviderRequest,
    RateLimit, accept_json, bounded_detail, check_status, collapse_whitespace, collect_records,
    endpoint_url, malformed, required, result_limit, search_endpoint, validated_query,
};
use crate::acquisition::StaticAcquirer;
use crate::citation::SourceKind;
use crate::constraints::SearchConstraints;
use crate::error::{InvalidConstraintSnafu, Result};
use crate::evidence::html_text::decode_references;
use crate::freshness::PublicationTime;
use crate::provider::{BoxFut, Provider, ProviderAnswer};
use crate::query::QueryShape;
use crate::result::{ResearchResult, ResultHit};
use crate::tier::ProviderTier;

const PROVIDER: &str = "wikipedia";

/// Documented `limit` range is 1 to 100.
const MAX_LIMIT: usize = 100;

/// Documented ceiling for unauthenticated clients sending a compliant
/// User-Agent: 200 requests per minute.
const REQUESTS_PER_MINUTE: u32 = 200;
const SECONDS_PER_MINUTE: u64 = 60;

/// Documented ceiling on concurrent requests.
const MAX_CONCURRENT: u32 = 3;

/// Article URL base; a hit's URL is this plus the page `key`.
const ARTICLE_BASE: &str = "https://en.wikipedia.org/wiki/";

/// The only markup the parser removes from an excerpt: the highlight the
/// search wraps around matched terms.
const SEARCHMATCH_OPEN: &str = "<span class=\"searchmatch\">";
const SPAN_OPEN: &str = "<span";
const SPAN_CLOSE: &str = "</span>";

/// English Wikipedia page search (`GET /w/rest.php/v1/search/page`).
///
/// Wikimedia's User-Agent policy requires an informative User-Agent with
/// contact information and answers generic ones with HTTP 403, so the
/// caller supplies it: zetesis never invents contact details.
#[derive(Debug, Clone)]
pub struct Wikipedia {
    acquirer: Arc<StaticAcquirer>,
    user_agent: String,
}

impl Wikipedia {
    /// Endpoint policy as documented on 2026-09-25.
    pub const POLICY: EndpointPolicy = EndpointPolicy {
        provider: PROVIDER,
        revision: "wikipedia/2026-09-25",
        endpoint: "https://en.wikipedia.org/w/rest.php/v1/search/page",
        sources: &[
            "https://www.mediawiki.org/wiki/API:REST_API/Reference",
            "https://www.mediawiki.org/wiki/Wikimedia_APIs/Rate_limits",
            "https://foundation.wikimedia.org/wiki/Policy:Wikimedia_Foundation_User-Agent_Policy",
            "https://foundation.wikimedia.org/wiki/Policy:Terms_of_Use",
        ],
        retrieved: jiff::civil::date(2026, 9, 25),
        authentication: "none; an informative User-Agent with contact information is required \
            and generic User-Agents are refused with HTTP 403",
        rate_limit: Some(RateLimit {
            requests: REQUESTS_PER_MINUTE,
            window: Duration::from_secs(SECONDS_PER_MINUTE),
        }),
        max_concurrent: Some(MAX_CONCURRENT),
        documented_limits: "unauthenticated clients with a compliant User-Agent: 200 requests \
            per minute (unidentified clients: 10 per minute), at most 3 concurrent requests; \
            the limits are marked experimental; 429 and 503 usually carry Retry-After, and \
            without one clients wait at least 5 seconds or back off exponentially; limit is 1 \
            to 100 with no offset",
        terms: "https://foundation.wikimedia.org/wiki/Policy:Terms_of_Use",
        license: "CC-BY-SA-4.0",
        query_shapes: &[QueryShape::QuickFactual, QueryShape::GeneralResearch],
        language_scope: "English Wikipedia only; `SearchConstraints::language` is ignored, \
            not mapped to another language edition",
    };

    /// Search through `acquirer`, sending the caller's User-Agent (in place
    /// of the acquirer's) in the form the Wikimedia policy asks for:
    /// `<client>/<version> (<contact information>) <library>/<version>`.
    /// Requests start at least 300 ms apart, the documented rate
    /// ([`EndpointPolicy::rate_limit`]).
    ///
    /// # Errors
    ///
    /// [`crate::Error::InvalidConstraint`] (field `user_agent`) when the
    /// value is blank, has surrounding whitespace, or holds a character
    /// that cannot appear in a header value (anything outside printable
    /// ASCII). Whether it names real contact information is the caller's
    /// responsibility.
    pub fn new(acquirer: Arc<StaticAcquirer>, user_agent: impl Into<String>) -> Result<Self> {
        let user_agent = user_agent.into();
        check_user_agent(&user_agent)?;
        Ok(Self {
            acquirer,
            user_agent,
        })
    }

    /// Build the search request for `query`, asking for at most
    /// `constraints.max_results` pages (capped at the endpoint's 100).
    /// Requests go to English Wikipedia; `constraints.language` is ignored
    /// rather than mapped to another language edition.
    ///
    /// # Errors
    ///
    /// [`crate::Error::InvalidQuery`] when the query has no searchable
    /// text; [`crate::Error::InvalidConstraint`] when `max_results` is 0.
    pub fn request(&self, query: &str, constraints: &SearchConstraints) -> Result<ProviderRequest> {
        let query = validated_query(PROVIDER, query)?;
        let limit = result_limit(constraints, MAX_LIMIT)?;
        let mut url = endpoint_url(&Self::POLICY)?;
        url.query_pairs_mut()
            .append_pair("q", &query)
            .append_pair("limit", &limit.to_string());
        Ok(ProviderRequest {
            url,
            headers: vec![accept_json(), ("user-agent", self.user_agent.clone())],
        })
    }

    /// Parse a search response into hits in search order.
    ///
    /// Unknown fields are ignored and optional fields may be absent or
    /// null. A page without `id`, `key`, or `title` is dropped and counted
    /// in [`ParsedResponse::malformed_records`]. The endpoint reports no
    /// timestamps, so every citation's publication time is `Unknown`. The
    /// snippet is the excerpt with its search-highlight markup removed,
    /// then its character references decoded, then its whitespace
    /// collapsed; a decoded `<` is text, never markup.
    ///
    /// # Errors
    ///
    /// See the [response mapping](crate#provider-response-mapping): a body
    /// that is not the documented JSON shape, including a known field whose
    /// type changed, is a [`crate::Error::ProviderFailure`].
    pub fn parse(
        status: u16,
        headers: &[(&str, &str)],
        body: &[u8],
        accessed_at: Timestamp,
    ) -> Result<ParsedResponse> {
        check_status(PROVIDER, status, headers, accessed_at)?;
        let response: SearchResponse = serde_json::from_slice(body)
            .map_err(|e| malformed(PROVIDER, format!("JSON: {}", bounded_detail(e))))?;
        collect_records(response.pages, |page, rank| {
            page_hit(page, rank, accessed_at)
        })
    }
}

impl Provider for Wikipedia {
    fn name(&self) -> &'static str {
        PROVIDER
    }

    fn tier(&self) -> ProviderTier {
        ProviderTier::Tier0Free
    }

    fn query_shapes(&self) -> &[QueryShape] {
        Self::POLICY.query_shapes
    }

    fn min_request_interval(&self) -> Duration {
        Self::POLICY.min_interval().unwrap_or(Duration::ZERO)
    }

    fn search<'a>(
        &'a self,
        query: &'a str,
        constraints: &'a SearchConstraints,
    ) -> BoxFut<'a, Result<ResearchResult>> {
        Box::pin(async move { self.search_with_evidence(query, constraints).await.result })
    }

    fn search_with_evidence<'a>(
        &'a self,
        query: &'a str,
        constraints: &'a SearchConstraints,
    ) -> BoxFut<'a, ProviderAnswer> {
        let endpoint = Endpoint {
            provider: PROVIDER,
            media: MEDIA_JSON,
            parse: Self::parse,
        };
        Box::pin(search_endpoint(
            &self.acquirer,
            endpoint,
            self.request(query, constraints),
            query,
            constraints,
        ))
    }
}

/// Refuse a User-Agent that is blank, padded, or not a header value.
fn check_user_agent(user_agent: &str) -> Result<()> {
    let reason = if user_agent.trim().is_empty() {
        Some("is blank")
    } else if user_agent.trim() != user_agent {
        Some("has leading or trailing whitespace")
    } else if !user_agent.chars().all(|c| c == ' ' || c.is_ascii_graphic()) {
        Some("holds a character outside printable ASCII")
    } else {
        None
    };
    match reason {
        Some(reason) => Err(InvalidConstraintSnafu {
            field: "user_agent",
            reason,
        }
        .build()),
        None => Ok(()),
    }
}

#[derive(Deserialize)]
struct SearchResponse {
    pages: Vec<Page>,
}

#[derive(Deserialize)]
struct Page {
    id: Option<u64>,
    key: Option<String>,
    title: Option<String>,
    excerpt: Option<String>,
    description: Option<String>,
    matched_title: Option<String>,
}

/// The page as a cited hit; `None` when it lacks `id`, `key`, or `title`.
fn page_hit(page: Page, rank: usize, accessed_at: Timestamp) -> Result<Option<ResultHit>> {
    let key = page.key.filter(|k| !k.trim().is_empty());
    let (Some(pageid), Some(key), Some(title)) = (page.id, key, required(page.title)) else {
        return Ok(None);
    };
    let url = article_url(&key)?;

    let mut metadata = vec![
        (META_PAGEID, Value::from(pageid)),
        ("attribution_url", Value::from(url.as_str())),
    ];
    metadata.extend(
        page.description
            .map(|d| collapse_whitespace(&d))
            .filter(|d| !d.is_empty())
            .map(|d| ("description", Value::from(d))),
    );
    metadata.extend(
        page.matched_title
            .map(|t| collapse_whitespace(&t))
            .filter(|t| !t.is_empty())
            .map(|t| ("matched_title", Value::from(t))),
    );

    HitParts {
        title,
        snippet: page
            .excerpt
            .map(|e| collapse_whitespace(&decode_references(&strip_searchmatch(&e))))
            .unwrap_or_default(),
        url,
        published_at: PublicationTime::Unknown,
        source_kind: SourceKind::Wiki,
        rank,
        accessed_at,
    }
    .into_hit(&Wikipedia::POLICY, metadata)
    .map(Some)
}

/// The article URL for a page `key`, the key percent-encoded as one path
/// segment.
fn article_url(key: &str) -> Result<Url> {
    let mut url = Url::parse(ARTICLE_BASE)
        .map_err(|e| malformed(PROVIDER, format!("article base does not parse: {e}")))?;
    url.path_segments_mut()
        .map_err(|()| malformed(PROVIDER, "article base cannot take a path"))?
        .pop_if_empty()
        .push(key);
    Ok(url)
}

/// Remove the search-highlight spans from an excerpt and keep everything
/// else, text and any other markup, exactly as delivered. Character
/// references are decoded afterwards, so a decoded `&lt;span` is text,
/// never a tag.
///
/// A `</span>` is removed only when it closes a highlight span; other spans
/// keep both tags. An excerpt cut off inside a highlight simply loses the
/// opening tag.
fn strip_searchmatch(excerpt: &str) -> String {
    let mut out = String::with_capacity(excerpt.len());
    // WHY: one entry per open span, `true` for a highlight, so a nested
    // non-highlight span keeps its own closing tag.
    let mut open_spans: Vec<bool> = Vec::new();
    let mut rest = excerpt;
    while let Some(at) = rest.find('<') {
        let (before, tail) = rest.split_at(at);
        out.push_str(before);
        if let Some(after) = tail.strip_prefix(SEARCHMATCH_OPEN) {
            open_spans.push(true);
            rest = after;
        } else if let Some(after) = tail.strip_prefix(SPAN_CLOSE) {
            if open_spans.pop() != Some(true) {
                out.push_str(SPAN_CLOSE);
            }
            rest = after;
        } else {
            if tail.starts_with(SPAN_OPEN) {
                open_spans.push(false);
            }
            out.push('<');
            rest = tail.get(1..).unwrap_or_default();
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_searchmatch_removes_only_the_highlight_markup() {
        assert_eq!(
            strip_searchmatch(
                "the <span class=\"searchmatch\">transformer</span> is a family of \
                 <span class=\"searchmatch\">attention</span> models"
            ),
            "the transformer is a family of attention models",
            "highlight tags removed, text kept"
        );
    }

    #[test]
    fn strip_searchmatch_keeps_other_markup_and_entities() {
        assert_eq!(
            strip_searchmatch(
                "a <span class=\"other\">b <span class=\"searchmatch\">c</span></span> &amp; d"
            ),
            "a <span class=\"other\">b c</span> &amp; d",
            "a non-highlight span keeps both tags; entities stay as delivered"
        );
    }

    #[test]
    fn strip_searchmatch_tolerates_a_truncated_highlight() {
        assert_eq!(
            strip_searchmatch("ends mid <span class=\"searchmatch\">matc"),
            "ends mid matc",
            "an excerpt cut inside a highlight loses only the opening tag"
        );
        assert_eq!(strip_searchmatch("a < b"), "a < b", "a bare `<` is text");
    }

    #[test]
    fn article_url_encodes_the_key_as_one_segment() {
        assert_eq!(
            article_url("Transformer_(deep_learning)").unwrap().as_str(),
            "https://en.wikipedia.org/wiki/Transformer_(deep_learning)",
            "parentheses need no encoding"
        );
        assert_eq!(
            article_url("AC/DC").unwrap().as_str(),
            "https://en.wikipedia.org/wiki/AC%2FDC",
            "a slash stays inside the one segment"
        );
    }

    #[test]
    fn user_agent_must_be_a_usable_header_value() {
        for bad in ["", "   ", " padded/1.0", "line\nbreak/1.0", "caf\u{e9}/1.0"] {
            let err = check_user_agent(bad).unwrap_err();
            assert!(
                matches!(err, crate::Error::InvalidConstraint { .. }),
                "{bad:?} must be refused, got {err:?}"
            );
        }
        assert!(
            check_user_agent("example-client/1.0 (https://example.org/contact) sylloge/0.0")
                .is_ok(),
            "a policy-shaped User-Agent is accepted"
        );
    }
}
