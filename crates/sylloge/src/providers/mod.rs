//! First Tier-0 provider cohort: Semantic Scholar, arXiv, and Wikipedia.
//!
//! The public contract (request builders, the response mapping, and the
//! hit mapping) is documented at the crate root; this module holds the
//! pieces the three providers share.
//!
//! # Fetching
//!
//! Each provider's `Provider` implementation builds its documented
//! anonymous request, fetches it through a shared
//! [`crate::StaticAcquirer`] as a data fetch (its own `Accept` media type,
//! the provider's `User-Agent` where its policy requires one, no
//! credential), and hands the recorded status, `Retry-After`, and decoded
//! body to its parser. The acquirer validates every hop, bounds the body,
//! and accepts only the provider's media type; a data body is kept as
//! bytes and no text is extracted from it. The envelope's fingerprint
//! travels with the answer as the evidence behind it; the envelope and
//! body are not kept.
//!
//! Each provider instance owns a [`Gate`] that its clones share: the
//! documented connection limit and request interval, and any
//! `Retry-After` the provider is sent, apply to every caller of the
//! instance. The request is built before the gate is entered, so a query
//! the builder refuses spends neither a slot nor a request.

mod arxiv;
mod semantic_scholar;
mod wikipedia;

use std::fmt::Display;
use std::time::Duration;

use jiff::Timestamp;
use jiff::civil::Date;
use serde_json::Value;
use serde_json::error::Category;
use url::Url;

use crate::acquisition::{
    AcquisitionFailure, ConnectOutcome, RequestProfile, StaticAcquirer, saturating_millis,
};
use crate::citation::{Citation, SourceKind};
use crate::constraints::SearchConstraints;
use crate::cost::{CostTracking, ProviderSpend};
use crate::error::{
    Error, ErrorClass, InvalidConstraintSnafu, InvalidQuerySnafu, PermanentIoSnafu,
    ProviderFailureSnafu, RateLimitedSnafu, Result, TimeoutSnafu, TransientIoSnafu,
    UnauthorizedSnafu,
};
use crate::evidence::envelope::EvidenceEnvelope;
use crate::freshness::PublicationTime;
use crate::pacing::Gate;
use crate::provider::ProviderAnswer;
use crate::query::QueryShape;
use crate::result::{ResearchResult, ResultHit};

pub use arxiv::Arxiv;
pub use semantic_scholar::SemanticScholar;
pub use wikipedia::Wikipedia;

/// Metadata key: DOI, lowercased, without a resolver prefix.
pub(crate) const META_DOI: &str = "doi";
/// Metadata key: arXiv identifier without its version suffix.
pub(crate) const META_ARXIV_ID: &str = "arxiv_id";
/// Metadata key: the DOI arXiv registers for its own record
/// (`10.48550/arxiv.<id>`), lowercased. It names the same work as
/// `arxiv_id`, so it is never a `doi` identity.
pub(crate) const META_ARXIV_DOI: &str = "arxiv_doi";
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

/// DOI prefix arXiv registers for its own preprints, lowercased.
const ARXIV_DOI_PREFIX: &str = "10.48550/arxiv.";

/// Media type the JSON endpoints answer with.
pub(crate) const MEDIA_JSON: &str = "application/json";

const STATUS_OK: u16 = 200;
const STATUS_BAD_REQUEST: u16 = 400;
const STATUS_UNAUTHORIZED: u16 = 401;
const STATUS_FORBIDDEN: u16 = 403;
const STATUS_URI_TOO_LONG: u16 = 414;
const STATUS_UNPROCESSABLE: u16 = 422;
const STATUS_TOO_MANY_REQUESTS: u16 = 429;
const STATUS_SERVICE_UNAVAILABLE: u16 = 503;
const CLIENT_ERRORS: std::ops::RangeInclusive<u16> = 400..=499;

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
    /// Language scope of the endpoint as used here, and whether the
    /// request builder applies [`SearchConstraints::language`].
    pub language_scope: &'static str,
    /// The endpoint's query syntax, and the rule the request builder
    /// applies so that query text (which may come from page content) is
    /// searched as plain terms and cannot reach that syntax.
    pub query_syntax: &'static str,
}

impl EndpointPolicy {
    /// Minimum spacing between requests that keeps within the documented
    /// per-client rate (`window / requests`); `None` when the provider
    /// documents no per-client rate, which leaves the interval to the
    /// caller.
    pub(crate) fn min_interval(&self) -> Option<Duration> {
        let limit = self.rate_limit?;
        limit.window.checked_div(limit.requests)
    }
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

/// A provider response a parser accepted.
///
/// A record that lacks a required field, or whose identity or URL cannot
/// be used, is dropped and counted in `malformed_records`; the remaining
/// hits keep the rank the provider gave them. A defect in the response as
/// a whole (a body that does not parse, or a known field whose type
/// changed) fails the parse instead.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct ParsedResponse {
    /// Usable hits, in provider rank order.
    pub hits: Vec<ResultHit>,
    /// Records dropped because they could not become a cited hit.
    pub malformed_records: usize,
}

/// A provider's structured parser: status, headers, body, access time.
pub(crate) type Parse = fn(u16, &[(&str, &str)], &[u8], Timestamp) -> Result<ParsedResponse>;

/// What a provider's fetch needs besides the request.
#[derive(Clone, Copy)]
pub(crate) struct Endpoint<'p> {
    /// Provider identifier, for errors and the cost line.
    pub(crate) provider: &'static str,
    /// The data media type requested and accepted.
    pub(crate) media: &'static str,
    /// The endpoint's documented per-request maximum, which caps the
    /// requested limit as the request builder capped it.
    pub(crate) max_results: usize,
    /// The provider's parser.
    pub(crate) parse: Parse,
    /// The provider instance's pacing and connection limit.
    pub(crate) gate: &'p Gate,
}

/// Build, fetch, and parse one provider search.
///
/// The request is built before anything is claimed, so a query the
/// builder refuses spends no pacing slot and sends nothing. The call then
/// waits at the instance's [`Gate`], fetches through the acquirer, and
/// hands the recorded response to the parser; a `Retry-After` in the
/// answer holds the gate for every caller of the instance. At most the
/// requested number of hits is kept, in the provider's rank order.
pub(crate) async fn search_endpoint(
    acquirer: &StaticAcquirer,
    endpoint: Endpoint<'_>,
    request: Result<ProviderRequest>,
    query: &str,
    constraints: &SearchConstraints,
) -> ProviderAnswer {
    let prepared = request.and_then(|request| {
        let profile = RequestProfile::data(endpoint.media, header(&request.headers, "user-agent"))?;
        let limit = result_limit(constraints, endpoint.max_results)?;
        Ok((request, profile, limit))
    });
    let (request, profile, limit) = match prepared {
        Ok(prepared) => prepared,
        Err(error) => return ProviderAnswer::new(Err(error), Vec::new(), 0),
    };
    let pass = match endpoint.gate.enter().await {
        Ok(pass) => pass,
        Err(slot) => {
            let error = RateLimitedSnafu {
                provider: endpoint.provider,
                retry_after_ms: Some(saturating_millis(slot.opens_in)),
            }
            .build();
            return ProviderAnswer::new(Err(error), Vec::new(), 0);
        }
    };
    // WHY: the caller's allow list says which hits a search may return, and
    // the router screens hit URLs with it; the provider's own API host is
    // not a hit, so screening it would refuse every search whose allow list
    // names only result domains. The deny list is different: a caller that
    // denies a domain never has zetesis contact it, as a hit or as an API
    // host, so it applies here with the rest of the network-target policy.
    let mut endpoint_constraints = constraints.clone();
    endpoint_constraints.domain_allowlist = None;
    let fetched = acquirer
        .acquire_with(&request.url, &endpoint_constraints, None, &profile)
        .await;
    drop(pass);
    let acquisition = match fetched {
        Ok(acquisition) => acquisition,
        Err(error) => return ProviderAnswer::new(Err(error), Vec::new(), 0),
    };
    let envelope = acquisition.envelope();
    let sent = requests_sent(envelope);
    let result = parse_envelope(endpoint, envelope, acquisition.body()).map(|parsed| {
        let mut hits = parsed.hits;
        hits.truncate(limit);
        let mut result = ResearchResult::new(
            query,
            QueryShape::default(),
            hits,
            Vec::new(),
            CostTracking::from_line_items([ProviderSpend::new(
                endpoint.provider,
                0,
                u64::from(sent),
                sent,
            )]),
            "",
        );
        result.malformed_records = parsed.malformed_records;
        result
    });
    if let Err(Error::RateLimited {
        retry_after_ms: Some(delay),
        ..
    }) = &result
    {
        endpoint.gate.hold_for(Duration::from_millis(*delay)).await;
    }
    ProviderAnswer::new(result, vec![envelope.fingerprint().to_owned()], sent)
}

/// Requests an acquisition put on the wire: one per hop that got past its
/// connection and, for `https`, its TLS handshake.
fn requests_sent(envelope: &EvidenceEnvelope) -> u32 {
    let sent = envelope
        .hops()
        .iter()
        .filter(|hop| {
            let connected = hop
                .connect_attempts()
                .iter()
                .any(|attempt| attempt.result() == ConnectOutcome::Connected);
            connected && (hop.url().scheme() != "https" || hop.tls().is_some())
        })
        .count();
    u32::try_from(sent).unwrap_or(u32::MAX)
}

/// Hand a recorded response to the parser, or map the acquisition failure.
///
/// Whenever a response head was recorded and it is not a success, the
/// status decides, even if the acquirer then refused the body (an error
/// page in another media type): the parser maps the status. A failure after
/// a successful head, or before any head, is the acquisition's own.
fn parse_envelope(
    endpoint: Endpoint<'_>,
    envelope: &EvidenceEnvelope,
    body: &[u8],
) -> Result<ParsedResponse> {
    let failure = envelope.failure();
    match envelope.response() {
        Some(response) if response.status() != STATUS_OK || failure.is_none() => {
            let headers: Vec<(&str, &str)> = response
                .retry_after()
                .map(|value| ("retry-after", value))
                .into_iter()
                .collect();
            (endpoint.parse)(response.status(), &headers, body, envelope.completed_at())
        }
        _ => Err(acquisition_error(endpoint.provider, failure)),
    }
}

/// An acquisition failure as a provider error of the same class. Only the
/// failure kind is named: its detail can carry text the origin sent.
fn acquisition_error(provider: &str, failure: Option<&AcquisitionFailure>) -> Error {
    let Some(failure) = failure else {
        return ProviderFailureSnafu {
            provider,
            message: "the acquisition recorded no response",
        }
        .build();
    };
    let message = format!("{provider}: acquisition failed: {}", failure.kind());
    match failure {
        AcquisitionFailure::DeadlineExceeded { deadline_ms } => TimeoutSnafu {
            provider,
            timeout_ms: *deadline_ms,
        }
        .build(),
        _ if failure.class() == ErrorClass::Transient => TransientIoSnafu { message }.build(),
        _ => PermanentIoSnafu { message }.build(),
    }
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
    /// `None`, a malformed record, when the URL is not `http` or `https`
    /// with a host: a hit URL is something a caller may fetch or show as a
    /// link, so a `javascript:`, `file:`, or host-less URL never becomes
    /// one.
    pub(crate) fn into_hit(
        self,
        policy: &EndpointPolicy,
        metadata: Vec<(&'static str, Value)>,
    ) -> Result<Option<ResultHit>> {
        if !matches!(self.url.scheme(), "http" | "https") || self.url.host().is_none() {
            return Ok(None);
        }
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
        Ok(Some(hit))
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
    if status == STATUS_OK {
        return Ok(());
    }
    let retry_after = retry_after_ms(headers, accessed_at);
    let rate_limited = || {
        RateLimitedSnafu {
            provider,
            retry_after_ms: retry_after,
        }
        .build()
    };
    let error = match status {
        STATUS_TOO_MANY_REQUESTS => rate_limited(),
        // WHY: a 503 that carries `Retry-After` names how long the service
        // expects to be unavailable (RFC 9110 section 10.2.3), and Wikimedia
        // documents 503 with `Retry-After` as a rate-limit answer; either way
        // the caller must hold off that long before asking again.
        STATUS_SERVICE_UNAVAILABLE if retry_after.is_some() => rate_limited(),
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

/// A `ProviderFailure` for a JSON body that does not parse, named by the
/// kind of defect and its position only. `serde_json`'s own message quotes
/// the offending value, which is text the origin sent, so it never reaches
/// an error.
pub(crate) fn json_defect(provider: &str, error: &serde_json::Error) -> Error {
    let kind = match error.classify() {
        Category::Io => "read",
        Category::Syntax => "syntax",
        Category::Data => "data",
        Category::Eof => "end-of-input",
    };
    malformed(
        provider,
        format!(
            "JSON {kind} error at line {} column {}",
            error.line(),
            error.column()
        ),
    )
}

/// A required per-record string, whitespace collapsed; `None` when absent
/// or blank, which drops the record.
pub(crate) fn required(value: Option<String>) -> Option<String> {
    value
        .map(|v| collapse_whitespace(&v))
        .filter(|v| !v.is_empty())
}

/// Map each record at its zero-based provider rank; a record the mapper
/// returns `None` for is dropped and counted.
pub(crate) fn collect_records<R>(
    records: Vec<R>,
    mut map: impl FnMut(R, usize) -> Result<Option<ResultHit>>,
) -> Result<ParsedResponse> {
    let mut hits = Vec::with_capacity(records.len());
    let mut malformed_records = 0;
    for (rank, record) in records.into_iter().enumerate() {
        match map(record, rank)? {
            Some(hit) => hits.push(hit),
            None => malformed_records += 1,
        }
    }
    Ok(ParsedResponse {
        hits,
        malformed_records,
    })
}

/// Case-insensitive lookup of header `name` among `(name, value)` pairs.
pub(crate) fn header<'h, N, V>(headers: &'h [(N, V)], name: &str) -> Option<&'h str>
where
    N: AsRef<str>,
    V: AsRef<str>,
{
    headers
        .iter()
        .find(|(n, _)| n.as_ref().eq_ignore_ascii_case(name))
        .map(|(_, v)| v.as_ref())
}

/// `Retry-After` as milliseconds from `accessed_at`, in any form
/// [`crate::pacing::retry_after`] reads. `None` when absent or unparseable,
/// which tells the caller to back off on its own schedule.
fn retry_after_ms(headers: &[(&str, &str)], accessed_at: Timestamp) -> Option<u64> {
    let delay = crate::pacing::retry_after(header(headers, "retry-after")?, accessed_at)?;
    Some(saturating_millis(delay))
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

/// What a DOI identifies, once normalized.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DoiIdentity {
    /// A DOI registered by a publisher or repository other than arXiv.
    Publisher(String),
    /// The DOI arXiv registers for its own record, and the arXiv
    /// identifier it names (version removed).
    Arxiv {
        /// The normalized DOI.
        doi: String,
        /// The arXiv identifier without its version.
        id: String,
    },
}

/// Normalize a DOI and tell an arXiv-registered DOI (`10.48550/arXiv.<id>`)
/// from any other. `None` when the value is not a DOI.
pub(crate) fn classify_doi(raw: &str) -> Option<DoiIdentity> {
    let doi = normalize_doi(raw)?;
    match doi
        .strip_prefix(ARXIV_DOI_PREFIX)
        .and_then(normalize_arxiv_id)
    {
        Some((id, _version)) => Some(DoiIdentity::Arxiv { doi, id }),
        None => Some(DoiIdentity::Publisher(doi)),
    }
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
    fn min_interval_follows_each_documented_rate() {
        assert_eq!(
            Arxiv::POLICY.min_interval(),
            Some(Duration::from_secs(3)),
            "arXiv: one request every three seconds"
        );
        assert_eq!(
            Wikipedia::POLICY.min_interval(),
            Some(Duration::from_millis(300)),
            "Wikipedia: 200 requests per minute"
        );
        assert_eq!(
            SemanticScholar::POLICY.min_interval(),
            None,
            "Semantic Scholar documents no per-client rate for unauthenticated use"
        );
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
    fn retry_after_accepts_the_obsolete_date_forms() {
        let at = ts("2026-09-25T18:00:00Z");
        for value in ["Friday, 25-Sep-26 18:00:10 GMT", "Fri Sep 25 18:00:10 2026"] {
            assert_eq!(
                retry_after_ms(&[("retry-after", value)], at),
                Some(10_000),
                "{value:?}"
            );
        }
    }

    #[test]
    fn service_unavailable_with_retry_after_is_rate_limited() {
        let at = ts("2026-09-25T18:00:00Z");
        let err = check_status("p", 503, &[("Retry-After", "7")], at).unwrap_err();
        assert!(
            matches!(
                err,
                Error::RateLimited {
                    retry_after_ms: Some(7_000),
                    ..
                }
            ),
            "503 with Retry-After asks the caller to hold off: {err:?}"
        );
        let err = check_status("p", 503, &[("Retry-After", "later")], at).unwrap_err();
        assert!(
            matches!(err, Error::ProviderFailure { .. }),
            "503 with an unreadable Retry-After is a plain failure: {err:?}"
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
    fn arxiv_datacite_doi_is_an_arxiv_identity() {
        assert_eq!(
            classify_doi("https://doi.org/10.48550/arXiv.2101.00001v2"),
            Some(DoiIdentity::Arxiv {
                doi: "10.48550/arxiv.2101.00001v2".to_owned(),
                id: "2101.00001".to_owned(),
            }),
            "arXiv's DOI names the arXiv identifier, version removed"
        );
        assert_eq!(
            classify_doi("10.5555/Synthetic.2021.042"),
            Some(DoiIdentity::Publisher(
                "10.5555/synthetic.2021.042".to_owned()
            )),
            "any other DOI is a publisher identity"
        );
        assert_eq!(
            classify_doi("10.48550/other.1"),
            Some(DoiIdentity::Publisher("10.48550/other.1".to_owned())),
            "only the arXiv namespace under the prefix is an arXiv identity"
        );
        assert_eq!(classify_doi("not a doi"), None, "not a DOI");
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
    fn json_defect_names_the_kind_and_position_only() {
        let err = serde_json::from_slice::<u64>(br#""secret value""#).unwrap_err();
        assert_eq!(
            json_defect("p", &err).to_string(),
            "provider 'p' failed: malformed response: JSON data error at line 1 column 14",
            "the kind and position, never the value"
        );
        let err = serde_json::from_slice::<Vec<u64>>(b"[1,").unwrap_err();
        assert_eq!(
            json_defect("p", &err).to_string(),
            "provider 'p' failed: malformed response: JSON end-of-input error at line 1 column 3",
            "a cut-off body"
        );
    }

    #[test]
    fn a_hit_url_must_be_http_with_a_host() {
        let parts = |url: &str| HitParts {
            title: "t".to_owned(),
            snippet: String::new(),
            url: Url::parse(url).unwrap(),
            published_at: PublicationTime::Unknown,
            source_kind: SourceKind::Web,
            rank: 0,
            accessed_at: ts("2026-09-25T18:00:00Z"),
        };
        for url in [
            "javascript:alert(1)",
            "file:///etc/passwd",
            "data:text/plain,hello",
            "mailto:alice@example.org",
        ] {
            assert_eq!(
                parts(url).into_hit(&Arxiv::POLICY, Vec::new()).unwrap(),
                None,
                "{url:?} never becomes a hit"
            );
        }
        for url in ["http://example.org/a", "https://example.org/b"] {
            assert!(
                parts(url)
                    .into_hit(&Arxiv::POLICY, Vec::new())
                    .unwrap()
                    .is_some(),
                "{url:?} does"
            );
        }
    }
}
