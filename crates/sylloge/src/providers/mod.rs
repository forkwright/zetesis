//! First Tier-0 provider cohort: Semantic Scholar, arXiv, and Wikipedia.
//!
//! The public contract (request builders, the response mapping, and the
//! hit mapping) is documented at the crate root; this module holds the
//! pieces the three providers share.
//!
//! # Transport seam
//!
//! The fetch between a provider's request builder and its parser belongs to
//! the static acquisition transport, which validates the target on every
//! hop and bounds the body. Each provider's `impl Provider` lands with that
//! wiring: build the request, acquire it, parse the response, and return
//! the provider's [`EndpointPolicy::query_shapes`] from
//! `Provider::query_shapes`. Pacing belongs there too: every
//! [`EndpointPolicy`] records the documented per-client rate limit and
//! concurrency the wiring must hold to.

mod arxiv;
mod semantic_scholar;
mod wikipedia;

use std::fmt::Display;
use std::time::Duration;

use jiff::Timestamp;
use jiff::civil::Date;
use serde_json::Value;
use url::Url;

use crate::citation::{Citation, SourceKind};
use crate::constraints::SearchConstraints;
use crate::error::{
    Error, InvalidConstraintSnafu, InvalidQuerySnafu, PermanentIoSnafu, ProviderFailureSnafu,
    RateLimitedSnafu, Result, UnauthorizedSnafu,
};
use crate::freshness::PublicationTime;
use crate::query::QueryShape;
use crate::result::ResultHit;

pub use arxiv::Arxiv;
pub use semantic_scholar::SemanticScholar;
pub use wikipedia::Wikipedia;

/// Metadata key: DOI, lowercased, without a resolver prefix.
pub(crate) const META_DOI: &str = "doi";
/// Metadata key: arXiv identifier without its version suffix.
pub(crate) const META_ARXIV_ID: &str = "arxiv_id";
/// Metadata key: arXiv version number of the retrieved record.
pub(crate) const META_ARXIV_VERSION: &str = "arxiv_version";
/// Metadata key: Semantic Scholar `paperId`.
pub(crate) const META_S2_PAPER_ID: &str = "s2_paper_id";
/// Metadata key: Semantic Scholar `corpusId`.
pub(crate) const META_CORPUS_ID: &str = "corpus_id";
/// Metadata key: Wikipedia page id.
pub(crate) const META_PAGEID: &str = "pageid";
/// Metadata key: author names in order.
pub(crate) const META_AUTHORS: &str = "authors";
/// Metadata key: publication year the provider declared.
pub(crate) const META_YEAR: &str = "year";
/// Metadata key: publication venue.
pub(crate) const META_VENUE: &str = "venue";
/// Metadata key: licence or attribution terms for the hit's metadata.
pub(crate) const META_LICENSE: &str = "license";
/// Metadata key: [`EndpointPolicy::revision`] the request was built under.
pub(crate) const META_POLICY_REVISION: &str = "provider_policy_revision";

/// Media type the JSON endpoints answer with.
const MEDIA_JSON: &str = "application/json";

const STATUS_OK: u16 = 200;
const STATUS_BAD_REQUEST: u16 = 400;
const STATUS_UNAUTHORIZED: u16 = 401;
const STATUS_FORBIDDEN: u16 = 403;
const STATUS_URI_TOO_LONG: u16 = 414;
const STATUS_UNPROCESSABLE: u16 = 422;
const STATUS_TOO_MANY_REQUESTS: u16 = 429;
const CLIENT_ERRORS: std::ops::RangeInclusive<u16> = 400..=499;

/// Longest parser-error detail carried into an error message, in
/// characters.
const MAX_DETAIL_CHARS: usize = 200;

const MILLIS_PER_SECOND: u64 = 1_000;

/// Documented upstream policy for one provider endpoint.
///
/// Every value comes from the provider's official documentation as read on
/// [`EndpointPolicy::retrieved`]. [`EndpointPolicy::revision`] names that
/// reading and is stamped on every hit the provider's parser produces
/// (`provider_policy_revision`), so each hit traces to the policy it was
/// requested under. A change to any documented value is a new revision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct EndpointPolicy {
    /// Provider identifier; the provider's `Provider::name()`.
    pub provider: &'static str,
    /// `<provider>/<retrieved>` revision stamp.
    pub revision: &'static str,
    /// Search endpoint every request targets.
    pub endpoint: &'static str,
    /// Official pages the recorded values were read from.
    pub sources: &'static [&'static str],
    /// Date the sources were read.
    pub retrieved: Date,
    /// Documented authentication requirement, and how requests meet it.
    pub authentication: &'static str,
    /// Documented per-client request ceiling, when the provider documents
    /// one.
    pub rate_limit: Option<RateLimit>,
    /// Documented ceiling on concurrent requests per client, when the
    /// provider documents one.
    pub max_concurrent: Option<u32>,
    /// Documented limits and conditions, in the provider's terms.
    pub documented_limits: &'static str,
    /// Terms of use governing the endpoint.
    pub terms: &'static str,
    /// Licence or attribution terms stamped in hit metadata (`license`).
    pub license: &'static str,
    /// Query shapes this endpoint serves.
    pub query_shapes: &'static [QueryShape],
}

/// A documented per-client request ceiling: at most `requests` requests in
/// any `window`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct RateLimit {
    /// Requests permitted per window.
    pub requests: u32,
    /// Length of the window.
    pub window: Duration,
}

/// A `GET` request built by a provider's request builder, ready for the
/// transport. It never carries credentials.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ProviderRequest {
    /// Absolute request URL, query string included.
    pub url: Url,
    /// Request headers as `(lowercase name, value)` pairs.
    pub headers: Vec<(&'static str, String)>,
}

/// Values a parser gathered for one hit, before it becomes a [`ResultHit`].
pub(crate) struct HitParts {
    pub(crate) title: String,
    pub(crate) snippet: String,
    pub(crate) url: Url,
    pub(crate) published_at: PublicationTime,
    pub(crate) source_kind: SourceKind,
    /// Zero-based position in the provider's answer.
    pub(crate) rank: usize,
    pub(crate) accessed_at: Timestamp,
}

impl HitParts {
    /// Build the cited hit, stamping the policy revision and licence.
    pub(crate) fn into_hit(
        self,
        policy: &EndpointPolicy,
        metadata: Vec<(&'static str, Value)>,
    ) -> Result<ResultHit> {
        let confidence = rank_confidence(self.rank);
        let citation = Citation::new(
            self.url.clone(),
            self.accessed_at,
            self.source_kind,
            confidence,
            None,
        )
        .with_published_at(self.published_at);
        let mut hit = ResultHit::new(
            self.title,
            self.snippet,
            self.url,
            vec![citation],
            confidence,
        )?
        .with_metadata(META_LICENSE, Value::from(policy.license))
        .with_metadata(META_POLICY_REVISION, Value::from(policy.revision));
        for (key, value) in metadata {
            hit = hit.with_metadata(key, value);
        }
        Ok(hit)
    }
}

/// Reciprocal-rank confidence for the hit at zero-based `rank`.
fn rank_confidence(rank: usize) -> f32 {
    let position = rank.saturating_add(1);
    // WHY: every provider in the cohort caps a page far below `u16::MAX`,
    // so the saturating fallback only guards a misbehaving caller.
    let position = u16::try_from(position).unwrap_or(u16::MAX);
    1.0 / f32::from(position)
}

/// Trim `query` and collapse its whitespace, refusing a query with no text.
pub(crate) fn validated_query(provider: &str, query: &str) -> Result<String> {
    let query = collapse_whitespace(query);
    snafu::ensure!(
        !query.is_empty(),
        InvalidQuerySnafu {
            reason: format!("{provider}: query has no searchable text"),
        }
    );
    Ok(query)
}

/// The caller's `max_results`, capped at the endpoint's per-request
/// maximum. Zero is refused: no endpoint in the cohort accepts it and the
/// caller asked for nothing.
pub(crate) fn result_limit(constraints: &SearchConstraints, endpoint_max: usize) -> Result<usize> {
    snafu::ensure!(
        constraints.max_results > 0,
        InvalidConstraintSnafu {
            field: "max_results",
            reason: "must be at least 1",
        }
    );
    Ok(constraints.max_results.min(endpoint_max))
}

/// Parse the policy's endpoint constant.
pub(crate) fn endpoint_url(policy: &EndpointPolicy) -> Result<Url> {
    Url::parse(policy.endpoint).map_err(|e| {
        ProviderFailureSnafu {
            provider: policy.provider,
            message: format!("endpoint constant does not parse: {e}"),
        }
        .build()
    })
}

/// The `Accept` header for a JSON endpoint.
pub(crate) fn accept_json() -> (&'static str, String) {
    ("accept", MEDIA_JSON.to_owned())
}

/// Map a non-200 status to its error; `Ok(())` for 200.
pub(crate) fn check_status(
    provider: &str,
    status: u16,
    headers: &[(&str, &str)],
    accessed_at: Timestamp,
) -> Result<()> {
    let error = match status {
        STATUS_OK => return Ok(()),
        STATUS_TOO_MANY_REQUESTS => RateLimitedSnafu {
            provider,
            retry_after_ms: retry_after_ms(headers, accessed_at),
        }
        .build(),
        STATUS_UNAUTHORIZED | STATUS_FORBIDDEN => UnauthorizedSnafu {
            provider,
            message: format!("HTTP {status}"),
        }
        .build(),
        STATUS_BAD_REQUEST | STATUS_URI_TOO_LONG | STATUS_UNPROCESSABLE => InvalidQuerySnafu {
            reason: format!("{provider} rejected the request with HTTP {status}"),
        }
        .build(),
        s if CLIENT_ERRORS.contains(&s) => PermanentIoSnafu {
            message: format!("{provider} answered HTTP {status}"),
        }
        .build(),
        _ => ProviderFailureSnafu {
            provider,
            message: format!("HTTP {status}"),
        }
        .build(),
    };
    Err(error)
}

/// A `ProviderFailure` for a 200 response whose body cannot be used.
pub(crate) fn malformed(provider: &str, detail: impl Display) -> Error {
    ProviderFailureSnafu {
        provider,
        message: format!("malformed response: {detail}"),
    }
    .build()
}

/// A bounded rendering of a parser error, for [`malformed`].
pub(crate) fn bounded_detail(detail: impl Display) -> String {
    let text = detail.to_string();
    match text.char_indices().nth(MAX_DETAIL_CHARS) {
        Some((cut, _)) => format!("{}...", text.get(..cut).unwrap_or_default()),
        None => text,
    }
}

/// A required per-result string that is present and not blank.
pub(crate) fn required(
    provider: &str,
    value: Option<String>,
    position: usize,
    field: &str,
) -> Result<String> {
    value
        .map(|v| collapse_whitespace(&v))
        .filter(|v| !v.is_empty())
        .ok_or_else(|| malformed(provider, format!("result {position} has no `{field}`")))
}

/// Case-insensitive header lookup.
fn header<'h>(headers: &[(&str, &'h str)], name: &str) -> Option<&'h str> {
    headers
        .iter()
        .find(|(n, _)| n.eq_ignore_ascii_case(name))
        .map(|(_, v)| *v)
}

/// `Retry-After` as milliseconds from `accessed_at`: delta-seconds, or an
/// IMF-fixdate HTTP-date. `None` when absent or unparseable, which tells
/// the caller to back off on its own schedule.
///
/// NOTE: the two obsolete HTTP-date forms (RFC 850 and asctime) are not
/// recognized; they read as absent.
fn retry_after_ms(headers: &[(&str, &str)], accessed_at: Timestamp) -> Option<u64> {
    let value = header(headers, "retry-after")?.trim();
    if let Ok(seconds) = value.parse::<u64>() {
        return Some(seconds.saturating_mul(MILLIS_PER_SECOND));
    }
    let at = jiff::fmt::rfc2822::DateTimeParser::new()
        .parse_timestamp(value)
        .ok()?;
    let wait = accessed_at.duration_until(at);
    Some(u64::try_from(wait.as_millis()).unwrap_or(0))
}

/// Trim and collapse every whitespace run to one space.
pub(crate) fn collapse_whitespace(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Canonical DOI: resolver or `doi:` prefix removed, lowercased. `None`
/// when the value is not a DOI (no `10.` directory prefix and suffix), so
/// an unusable value never becomes a merge identity.
pub(crate) fn normalize_doi(raw: &str) -> Option<String> {
    const PREFIXES: [&str; 5] = [
        "https://doi.org/",
        "http://doi.org/",
        "https://dx.doi.org/",
        "http://dx.doi.org/",
        "doi:",
    ];
    let lower = raw.trim().to_lowercase();
    let bare = PREFIXES
        .iter()
        .find_map(|prefix| lower.strip_prefix(prefix))
        .unwrap_or(&lower);
    let (directory, suffix) = bare.split_once('/')?;
    let well_formed = directory.starts_with("10.")
        && !suffix.is_empty()
        && !bare.chars().any(char::is_whitespace);
    well_formed.then(|| bare.to_owned())
}

/// Canonical arXiv identifier and version: `arXiv:` prefix removed,
/// lowercased, trailing `v<N>` split off. `None` when nothing identifying
/// remains.
pub(crate) fn normalize_arxiv_id(raw: &str) -> Option<(String, Option<u32>)> {
    let lower = raw.trim().to_lowercase();
    let bare = lower.strip_prefix("arxiv:").unwrap_or(&lower);
    if bare.is_empty() || bare.chars().any(char::is_whitespace) {
        return None;
    }
    let split = bare.rfind('v').and_then(|at| {
        let digits = bare.get(at + 1..)?;
        let version = digits.parse::<u32>().ok()?;
        let id = bare.get(..at)?;
        let id_ends_in_digit = id.chars().next_back().is_some_and(|c| c.is_ascii_digit());
        id_ends_in_digit.then(|| (id.to_owned(), Some(version)))
    });
    Some(split.unwrap_or_else(|| (bare.to_owned(), None)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ts(s: &str) -> Timestamp {
        s.parse().unwrap()
    }

    #[test]
    fn rank_confidence_is_reciprocal_rank() {
        assert!((rank_confidence(0) - 1.0).abs() < f32::EPSILON, "rank 1");
        assert!((rank_confidence(1) - 0.5).abs() < f32::EPSILON, "rank 2");
        assert!((rank_confidence(3) - 0.25).abs() < f32::EPSILON, "rank 4");
    }

    #[test]
    fn retry_after_accepts_delta_seconds() {
        let at = ts("2026-09-25T18:00:00Z");
        assert_eq!(
            retry_after_ms(&[("Retry-After", "120")], at),
            Some(120_000),
            "delta-seconds convert to milliseconds"
        );
    }

    #[test]
    fn retry_after_accepts_an_http_date() {
        let at = ts("2026-09-25T18:00:00Z");
        assert_eq!(
            retry_after_ms(&[("retry-after", "Fri, 25 Sep 2026 18:00:30 GMT")], at),
            Some(30_000),
            "an HTTP-date is measured from the access time"
        );
        assert_eq!(
            retry_after_ms(&[("retry-after", "Fri, 25 Sep 2026 17:59:00 GMT")], at),
            Some(0),
            "a date already past means retry now"
        );
    }

    #[test]
    fn retry_after_absent_or_unparseable_is_none() {
        let at = ts("2026-09-25T18:00:00Z");
        assert_eq!(retry_after_ms(&[], at), None, "absent header");
        assert_eq!(
            retry_after_ms(&[("retry-after", "soon")], at),
            None,
            "unparseable header"
        );
    }

    #[test]
    fn normalize_doi_strips_resolver_prefixes_and_lowercases() {
        for raw in [
            "10.18653/V1/2020.ACL-MAIN.447",
            "https://doi.org/10.18653/v1/2020.acl-main.447",
            "http://dx.doi.org/10.18653/v1/2020.acl-main.447",
            " doi:10.18653/v1/2020.acl-main.447 ",
        ] {
            assert_eq!(
                normalize_doi(raw).as_deref(),
                Some("10.18653/v1/2020.acl-main.447"),
                "{raw:?}"
            );
        }
    }

    #[test]
    fn normalize_doi_refuses_values_that_are_not_dois() {
        for raw in ["", "not a doi", "11.1000/x", "10.1000/", "10.1000"] {
            assert_eq!(normalize_doi(raw), None, "{raw:?}");
        }
    }

    #[test]
    fn normalize_arxiv_id_splits_the_version() {
        assert_eq!(
            normalize_arxiv_id("2101.00001v2"),
            Some(("2101.00001".to_owned(), Some(2))),
            "new-style identifier"
        );
        assert_eq!(
            normalize_arxiv_id("hep-ex/0307015v1"),
            Some(("hep-ex/0307015".to_owned(), Some(1))),
            "old-style identifier"
        );
        assert_eq!(
            normalize_arxiv_id("arXiv:1706.03762"),
            Some(("1706.03762".to_owned(), None)),
            "prefix removed, no version"
        );
        assert_eq!(
            normalize_arxiv_id("solv-int/9901001"),
            Some(("solv-int/9901001".to_owned(), None)),
            "a `v` inside the archive name is not a version"
        );
        assert_eq!(normalize_arxiv_id("  "), None, "blank");
    }

    #[test]
    fn status_mapping_follows_the_documented_table() {
        let at = ts("2026-09-25T18:00:00Z");
        assert!(check_status("p", 200, &[], at).is_ok(), "200 is success");
        let cases: [(u16, fn(&Error) -> bool); 7] = [
            (400, |e| matches!(e, Error::InvalidQuery { .. })),
            (401, |e| matches!(e, Error::Unauthorized { .. })),
            (403, |e| matches!(e, Error::Unauthorized { .. })),
            (404, |e| matches!(e, Error::PermanentIo { .. })),
            (429, |e| matches!(e, Error::RateLimited { .. })),
            (500, |e| matches!(e, Error::ProviderFailure { .. })),
            (302, |e| matches!(e, Error::ProviderFailure { .. })),
        ];
        for (status, expected) in cases {
            let err = check_status("p", status, &[], at).unwrap_err();
            assert!(expected(&err), "HTTP {status} mapped to {err:?}");
        }
    }

    #[test]
    fn bounded_detail_truncates_long_text() {
        let long = "x".repeat(MAX_DETAIL_CHARS + 10);
        let detail = bounded_detail(&long);
        assert_eq!(
            detail.chars().count(),
            MAX_DETAIL_CHARS + 3,
            "cut to the bound plus an ellipsis"
        );
        assert_eq!(bounded_detail("short"), "short", "short text is unchanged");
    }
}
