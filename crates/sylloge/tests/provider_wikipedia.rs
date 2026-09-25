//! Wikipedia request builder and response parser, against the recorded
//! sample and synthetic fixtures in `fixtures/providers/wikipedia/`
//! (provenance and licence in that directory's README).

#![expect(clippy::unwrap_used, reason = "test assertions must fail loudly")]

use jiff::Timestamp;
use serde_json::json;
use sylloge::{
    BudgetConstraint, Error, PublicationTime, ResultHit, SearchConstraints, SourceKind, Wikipedia,
};

/// Recorded 2026-09-25T18:00:02Z from
/// `GET https://en.wikipedia.org/w/rest.php/v1/search/page?q=transformer%20attention&limit=2`.
const RECORDED: &[u8] =
    include_bytes!("fixtures/providers/wikipedia/search_recorded_2026-09-25.json");
const EMPTY: &[u8] = include_bytes!("fixtures/providers/wikipedia/search_empty.json");
const PARTIAL: &[u8] = include_bytes!("fixtures/providers/wikipedia/search_partial.json");
const SCHEMA_CHANGED: &[u8] =
    include_bytes!("fixtures/providers/wikipedia/search_schema_changed.json");
const MALFORMED: &[u8] = include_bytes!("fixtures/providers/wikipedia/search_malformed.json");
const MALFORMED_RECORDS: &[u8] =
    include_bytes!("fixtures/providers/wikipedia/search_malformed_records.json");
const TYPE_CHANGED: &[u8] = include_bytes!("fixtures/providers/wikipedia/search_type_changed.json");

const USER_AGENT: &str = "zetesis-fixture/0.0 (https://example.org/contact) sylloge/0.0";

fn accessed() -> Timestamp {
    "2026-09-25T18:00:02Z".parse().unwrap()
}

fn parse_ok(body: &[u8]) -> Vec<ResultHit> {
    let parsed = Wikipedia::parse(200, &[], body, accessed()).unwrap();
    assert_eq!(parsed.malformed_records, 0, "no page is dropped");
    parsed.hits
}

#[test]
fn request_sends_the_callers_user_agent_to_the_per_wiki_endpoint() {
    let wikipedia = Wikipedia::new(USER_AGENT).unwrap();
    let request = wikipedia
        .request(
            "transformer attention",
            &SearchConstraints::new(2, BudgetConstraint::free_only()),
        )
        .unwrap();
    assert_eq!(
        request.url.as_str(),
        "https://en.wikipedia.org/w/rest.php/v1/search/page?q=transformer+attention&limit=2",
        "the per-wiki search endpoint, as recorded"
    );
    assert_eq!(
        request.headers,
        [
            ("accept", "application/json".to_owned()),
            ("user-agent", USER_AGENT.to_owned()),
        ],
        "the User-Agent is exactly the caller's"
    );
}

#[test]
fn request_caps_the_limit_at_the_documented_maximum() {
    let request = Wikipedia::new(USER_AGENT)
        .unwrap()
        .request(
            "q",
            &SearchConstraints::new(500, BudgetConstraint::free_only()),
        )
        .unwrap();
    assert!(
        request.url.as_str().ends_with("&limit=100"),
        "limit is at most 100: {}",
        request.url
    );
}

#[test]
fn recorded_sample_maps_to_cited_wiki_hits() {
    let hits = parse_ok(RECORDED);
    assert_eq!(hits.len(), 2, "one hit per page");

    let first = hits.first().unwrap();
    assert_eq!(first.title, "Transformer (deep learning)");
    assert_eq!(
        first.url.as_str(),
        "https://en.wikipedia.org/wiki/Transformer_(deep_learning)",
        "the article URL from the page key"
    );
    assert_eq!(
        first.snippet,
        "In deep learning, the transformer is a family of artificial neural network \
         architectures based on the multi-head attention mechanism, in which input data",
        "highlight markup stripped, text kept, mid-sentence cut kept"
    );
    let citation = first.citations.first().unwrap();
    assert_eq!(citation.source_kind, SourceKind::Wiki);
    assert_eq!(
        citation.published_at,
        PublicationTime::Unknown,
        "search results carry no timestamps"
    );
    assert_eq!(
        citation.accessed_at,
        accessed(),
        "access time is the caller's"
    );
    assert_eq!(
        serde_json::to_value(&first.metadata).unwrap(),
        json!({
            "pageid": 61_603_971,
            "attribution_url": "https://en.wikipedia.org/wiki/Transformer_(deep_learning)",
            "description": "Algorithm for modelling sequential data",
            "license": "CC-BY-SA-4.0",
            "provider_policy_revision": "wikipedia/2026-09-25",
        }),
        "page identity, licence, and attribution URL"
    );

    let second = hits.get(1).unwrap();
    assert_eq!(second.title, "Attention Is All You Need");
    assert_eq!(second.metadata.get("pageid"), Some(&json!(75_477_752)));
    assert_eq!(
        second.snippet,
        "architecture known as the transformer, based on the attention mechanism proposed in \
         2014 by Bahdanau et al. The transformer approach it describes has",
        "every highlight in the excerpt is stripped"
    );
    assert!(
        (second.score - 0.5).abs() < f32::EPSILON,
        "rank 2 scores 0.5"
    );
}

#[test]
fn empty_page_list_is_an_empty_answer() {
    assert!(parse_ok(EMPTY).is_empty(), "no pages, no hits, no filler");
}

#[test]
fn partial_pages_keep_only_what_is_present() {
    let hits = parse_ok(PARTIAL);
    assert_eq!(hits.len(), 2, "both pages parse");
    for hit in &hits {
        assert_eq!(hit.snippet, "", "no excerpt: {}", hit.title);
        assert!(
            !hit.metadata.contains_key("description")
                && !hit.metadata.contains_key("matched_title"),
            "absent or null fields are not recorded: {}",
            hit.title
        );
    }
}

#[test]
fn schema_change_with_added_and_removed_fields_still_parses() {
    let hits = parse_ok(SCHEMA_CHANGED);
    let hit = hits.first().unwrap();
    assert_eq!(
        hit.snippet, "a changed schema with a new field and no description",
        "excerpt still stripped"
    );
    assert_eq!(
        hit.metadata.get("matched_title"),
        Some(&json!("Schema change")),
        "a redirect match is kept"
    );
    assert!(
        !hit.metadata.contains_key("description"),
        "description is gone"
    );
}

#[test]
fn malformed_body_is_a_provider_failure() {
    let err = Wikipedia::parse(200, &[], MALFORMED, accessed()).unwrap_err();
    assert!(
        matches!(err, Error::ProviderFailure { .. }) && err.is_transient(),
        "cut-off body: {err:?}"
    );
    assert!(
        err.to_string().contains("malformed response: JSON"),
        "the message names the defect: {err}"
    );
}

#[test]
fn pages_without_a_key_or_id_are_dropped_and_counted() {
    let parsed = Wikipedia::parse(200, &[], MALFORMED_RECORDS, accessed()).unwrap();
    assert_eq!(
        parsed.malformed_records, 2,
        "without a key there is no article URL; without an id no page identity"
    );
    let titles: Vec<&str> = parsed.hits.iter().map(|h| h.title.as_str()).collect();
    assert_eq!(
        titles,
        ["Complete page", "Another complete page"],
        "the complete pages survive"
    );
    let scores: Vec<f32> = parsed.hits.iter().map(|h| h.score).collect();
    assert_eq!(
        scores,
        [1.0, 0.25],
        "survivors keep the rank the search gave them"
    );
}

#[test]
fn type_change_in_a_known_field_is_a_malformed_response() {
    let err = Wikipedia::parse(200, &[], TYPE_CHANGED, accessed()).unwrap_err();
    assert!(
        matches!(err, Error::ProviderFailure { .. }),
        "schema drift fails the whole response: {err:?}"
    );
    assert!(
        err.to_string().contains("malformed response: JSON"),
        "the message names the defect: {err}"
    );
}

#[test]
fn rate_limit_reads_retry_after_as_seconds_or_a_date() {
    let err = Wikipedia::parse(429, &[], b"", accessed()).unwrap_err();
    assert!(
        matches!(
            err,
            Error::RateLimited {
                retry_after_ms: None,
                ..
            }
        ),
        "no Retry-After: {err:?}"
    );
    let err = Wikipedia::parse(
        429,
        &[("Retry-After", "Fri, 25 Sep 2026 18:00:12 GMT")],
        b"",
        accessed(),
    )
    .unwrap_err();
    assert!(
        matches!(
            err,
            Error::RateLimited {
                retry_after_ms: Some(10_000),
                ..
            }
        ),
        "an HTTP-date ten seconds after access: {err:?}"
    );
}

#[test]
fn generic_user_agent_denial_is_unauthorized() {
    let err = Wikipedia::parse(403, &[], b"", accessed()).unwrap_err();
    assert!(
        matches!(err, Error::Unauthorized { .. }) && err.is_permanent(),
        "403 for a non-compliant User-Agent: {err:?}"
    );
}

#[test]
fn server_error_is_a_transient_provider_failure() {
    let err = Wikipedia::parse(503, &[], b"", accessed()).unwrap_err();
    assert!(
        matches!(err, Error::ProviderFailure { .. }) && err.is_transient(),
        "503 without Retry-After: {err:?}"
    );
}

#[test]
fn service_unavailable_with_retry_after_is_rate_limited() {
    // NOTE: Wikimedia documents 429 and 503 with Retry-After as its
    // rate-limit answers.
    let err = Wikipedia::parse(503, &[("retry-after", "5")], b"", accessed()).unwrap_err();
    assert!(
        matches!(
            err,
            Error::RateLimited {
                retry_after_ms: Some(5_000),
                ..
            }
        ),
        "503 with Retry-After asks the caller to hold off five seconds: {err:?}"
    );
    let err = Wikipedia::parse(
        503,
        &[("Retry-After", "Friday, 25-Sep-26 18:00:32 GMT")],
        b"",
        accessed(),
    )
    .unwrap_err();
    assert!(
        matches!(
            err,
            Error::RateLimited {
                retry_after_ms: Some(30_000),
                ..
            }
        ),
        "an rfc850-date Retry-After is read too: {err:?}"
    );
}

#[test]
fn status_decides_even_when_the_body_would_parse() {
    let err = Wikipedia::parse(429, &[], RECORDED, accessed()).unwrap_err();
    assert!(
        matches!(err, Error::RateLimited { .. }),
        "a result-shaped body under 429 is still a rate limit: {err:?}"
    );
}

#[test]
fn policy_record_carries_the_documented_pacing() {
    let policy = Wikipedia::POLICY;
    let limit = policy.rate_limit.unwrap();
    assert_eq!(limit.requests, 200, "200 requests");
    assert_eq!(
        limit.window,
        std::time::Duration::from_secs(60),
        "per minute"
    );
    assert_eq!(policy.max_concurrent, Some(3), "at most three concurrent");
    assert_eq!(policy.license, "CC-BY-SA-4.0");
}
