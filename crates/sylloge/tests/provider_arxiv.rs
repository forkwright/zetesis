//! arXiv request builder and Atom parser, against the documented-shape and
//! synthetic fixtures in `fixtures/providers/arxiv/` (provenance in that
//! directory's README).

#![expect(clippy::unwrap_used, reason = "test assertions must fail loudly")]

use jiff::Timestamp;
use serde_json::json;
use sylloge::{
    Arxiv, BudgetConstraint, Error, PublicationPrecision, PublicationProvenance, PublicationTime,
    ResultHit, SearchConstraints, SourceKind,
};

const DOCUMENTED: &[u8] = include_bytes!("fixtures/providers/arxiv/search_documented_shape.xml");
const EMPTY: &[u8] = include_bytes!("fixtures/providers/arxiv/search_empty.xml");
const PARTIAL: &[u8] = include_bytes!("fixtures/providers/arxiv/search_partial.xml");
const SCHEMA_CHANGED: &[u8] = include_bytes!("fixtures/providers/arxiv/search_schema_changed.xml");
const MALFORMED: &[u8] = include_bytes!("fixtures/providers/arxiv/search_malformed.xml");
const TRUNCATED: &[u8] = include_bytes!("fixtures/providers/arxiv/search_truncated.xml");
const MISSING_TITLE: &[u8] = include_bytes!("fixtures/providers/arxiv/search_missing_title.xml");
const NOT_ATOM: &[u8] = include_bytes!("fixtures/providers/arxiv/not_atom.xml");
const API_ERROR: &[u8] = include_bytes!("fixtures/providers/arxiv/api_error_documented.xml");

fn accessed() -> Timestamp {
    "2026-09-25T18:00:00Z".parse().unwrap()
}

fn parse_ok(body: &[u8]) -> Vec<ResultHit> {
    Arxiv::parse(200, &[], body, accessed()).unwrap()
}

fn search_query(max_results: usize, query: &str) -> Vec<(String, String)> {
    let request = Arxiv::request(
        query,
        &SearchConstraints::new(max_results, BudgetConstraint::free_only()),
    )
    .unwrap();
    assert_eq!(
        request.url.as_str().split('?').next(),
        Some("https://export.arxiv.org/api/query"),
        "the documented query endpoint"
    );
    assert_eq!(
        request.headers,
        [("accept", "application/atom+xml".to_owned())],
        "the feed's media type"
    );
    request
        .url
        .query_pairs()
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect()
}

#[test]
fn request_ands_quoted_all_field_terms_in_relevance_order() {
    assert_eq!(
        search_query(5_000, "transformer  attention"),
        [
            (
                "search_query".to_owned(),
                "all:\"transformer\" AND all:\"attention\"".to_owned()
            ),
            ("start".to_owned(), "0".to_owned()),
            ("max_results".to_owned(), "2000".to_owned()),
            ("sortBy".to_owned(), "relevance".to_owned()),
            ("sortOrder".to_owned(), "descending".to_owned()),
        ],
        "every term searched in all fields, capped at the documented 2000 slice"
    );
}

#[test]
fn request_keeps_query_syntax_out_of_the_search() {
    let pairs = search_query(10, "ti:\"quantum\" OR (au:x)");
    assert_eq!(
        pairs.first().map(|(_, v)| v.as_str()),
        Some("all:\"ti:quantum\" AND all:\"OR\" AND all:\"(au:x)\""),
        "field prefixes, operators, and grouping stay quoted text"
    );
}

#[test]
fn request_refuses_a_query_of_only_quotes() {
    let err = Arxiv::request("\" \\", &SearchConstraints::default()).unwrap_err();
    assert!(
        matches!(err, Error::InvalidQuery { .. }),
        "nothing searchable remains: {err:?}"
    );
}

#[test]
fn documented_shape_keeps_identity_version_and_provenance() {
    let hits = parse_ok(DOCUMENTED);
    assert_eq!(hits.len(), 2, "one hit per entry");

    let hit = hits.first().unwrap();
    assert_eq!(
        hit.title, "Deduplicating Preprints by Stable Identity",
        "the title's line break collapses"
    );
    assert_eq!(
        hit.snippet,
        "A synthetic abstract used only as a parser fixture. It spans several lines, as \
         abstracts in the feed do.",
        "the summary is the snippet, whitespace collapsed"
    );
    assert_eq!(
        hit.url.as_str(),
        "http://arxiv.org/abs/2101.00001v2",
        "the abstract page of the retrieved version"
    );
    let citation = hit.citations.first().unwrap();
    assert_eq!(
        citation.source_kind,
        SourceKind::Preprint,
        "arXiv is a preprint server"
    );
    assert_eq!(
        citation.published_at,
        PublicationTime::Known {
            at: "2021-03-02T14:15:00Z".parse().unwrap(),
            precision: PublicationPrecision::Exact,
            provenance: PublicationProvenance::ProviderDeclared,
        },
        "<updated> dates the retrieved version"
    );
    assert_eq!(
        serde_json::to_value(&hit.metadata).unwrap(),
        json!({
            "arxiv_id": "2101.00001",
            "arxiv_version": 2,
            "doi": "10.5555/synthetic.2021.042",
            "authors": ["Casey Placeholder", "Ada Example"],
            "year": 2021,
            "published": "2021-01-04T13:46:39-05:00",
            "journal_ref": "Synthetic J. Fixtures 7 (2021) 1-12",
            "comment": "12 pages, 3 figures",
            "primary_category": "cs.IR",
            "categories": ["cs.IR", "cs.DL"],
            "license": "CC0-1.0",
            "provider_policy_revision": "arxiv/2026-09-25",
        }),
        "identity, first-version date, and licence"
    );
}

#[test]
fn old_style_identifier_takes_its_version_from_the_abstract_link() {
    let hits = parse_ok(DOCUMENTED);
    let hit = hits.get(1).unwrap();
    assert_eq!(
        hit.metadata.get("arxiv_id"),
        Some(&json!("hep-ex/0000001")),
        "old-style identifier with its archive"
    );
    assert_eq!(
        hit.metadata.get("arxiv_version"),
        Some(&json!(1)),
        "the unversioned <id> leaves the version to the alternate link"
    );
    assert_eq!(
        hit.url.as_str(),
        "http://arxiv.org/abs/hep-ex/0000001v1",
        "the versioned abstract page"
    );
    assert!(!hit.metadata.contains_key("doi"), "no DOI was declared");
    assert!((hit.score - 0.5).abs() < f32::EPSILON, "rank 2 scores 0.5");
}

#[test]
fn empty_feed_is_an_empty_answer() {
    assert!(parse_ok(EMPTY).is_empty(), "no entries, no hits, no filler");
}

#[test]
fn partial_entry_keeps_only_what_is_present() {
    let hits = parse_ok(PARTIAL);
    let hit = hits.first().unwrap();
    assert_eq!(
        hit.url.as_str(),
        "http://arxiv.org/abs/2104.00004v1",
        "<id> is the URL"
    );
    assert_eq!(hit.snippet, "", "no summary");
    assert_eq!(
        hit.citations.first().unwrap().published_at,
        PublicationTime::Unknown,
        "no <updated>"
    );
    assert_eq!(
        serde_json::to_value(&hit.metadata).unwrap(),
        json!({
            "arxiv_id": "2104.00004",
            "arxiv_version": 1,
            "license": "CC0-1.0",
            "provider_policy_revision": "arxiv/2026-09-25",
        }),
        "nothing is invented for absent elements"
    );
}

#[test]
fn schema_change_is_read_by_namespace_and_text_is_decoded() {
    let hits = parse_ok(SCHEMA_CHANGED);
    assert_eq!(hits.len(), 1, "unknown elements add no entries");
    let hit = hits.first().unwrap();
    assert_eq!(
        hit.title, "Entities & Unknown Elements: \u{3b1}-Routing",
        "entity and character references resolve; a same-named foreign element is ignored"
    );
    assert_eq!(
        hit.snippet, "A CDATA abstract with <markup> kept as text.",
        "CDATA is text"
    );
    assert_eq!(hit.metadata.get("authors"), Some(&json!(["Gale Mock"])));
    assert_eq!(hit.metadata.get("arxiv_version"), Some(&json!(3)));
    assert!(
        !hit.metadata.contains_key("year") && !hit.metadata.contains_key("published"),
        "no <published>, so no first-version year"
    );
    assert_eq!(
        hit.citations.first().unwrap().published_at,
        PublicationTime::Known {
            at: "2021-05-20T08:00:00Z".parse().unwrap(),
            precision: PublicationPrecision::Exact,
            provenance: PublicationProvenance::ProviderDeclared,
        },
        "<updated> still dates the version"
    );
}

#[test]
fn malformed_xml_is_a_provider_failure() {
    let err = Arxiv::parse(200, &[], MALFORMED, accessed()).unwrap_err();
    assert!(
        matches!(err, Error::ProviderFailure { .. }),
        "mismatched tags: {err:?}"
    );
    assert!(
        err.to_string().contains("malformed response: XML at byte"),
        "the message names the parser position: {err}"
    );
}

#[test]
fn truncated_feed_is_a_provider_failure() {
    let err = Arxiv::parse(200, &[], TRUNCATED, accessed()).unwrap_err();
    assert!(
        err.to_string()
            .contains("document ended inside an open element"),
        "a cut-off feed is not an empty one: {err}"
    );
}

#[test]
fn empty_body_is_not_an_empty_feed() {
    let err = Arxiv::parse(200, &[], b"", accessed()).unwrap_err();
    assert!(
        err.to_string().contains("document has no root element"),
        "no feed at all: {err}"
    );
}

#[test]
fn a_document_that_is_not_a_feed_is_refused() {
    let err = Arxiv::parse(200, &[], NOT_ATOM, accessed()).unwrap_err();
    assert!(
        err.to_string()
            .contains("root element is not an Atom <feed>"),
        "HTML is not a feed: {err}"
    );
}

#[test]
fn entry_without_a_title_fails_the_response_by_position() {
    let err = Arxiv::parse(200, &[], MISSING_TITLE, accessed()).unwrap_err();
    assert!(
        err.to_string().contains("result 2 has no `title`"),
        "the message names the entry and field: {err}"
    );
}

#[test]
fn documented_error_feed_is_an_invalid_query_carrying_the_error_code() {
    let err = Arxiv::parse(200, &[], API_ERROR, accessed()).unwrap_err();
    assert!(
        matches!(err, Error::InvalidQuery { .. }),
        "an error entry is not a hit titled `Error`: {err:?}"
    );
    let message = err.to_string();
    assert!(
        message.contains("`incorrect_id_format_for_1234.12345`"),
        "the error code is carried: {message}"
    );
    assert!(
        !message.contains("incorrect id format for"),
        "the summary's free text is not quoted: {message}"
    );
}

#[test]
fn rate_limit_status_maps_with_and_without_retry_after() {
    // NOTE: arXiv documents no rate-limit status; a 429 maps like any
    // other provider's.
    let err = Arxiv::parse(429, &[], b"", accessed()).unwrap_err();
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
    let err = Arxiv::parse(429, &[("retry-after", "3")], b"", accessed()).unwrap_err();
    assert!(
        matches!(
            err,
            Error::RateLimited {
                retry_after_ms: Some(3_000),
                ..
            }
        ),
        "Retry-After in milliseconds: {err:?}"
    );
}

#[test]
fn authentication_denial_and_server_errors_map_by_status() {
    let err = Arxiv::parse(403, &[], b"", accessed()).unwrap_err();
    assert!(matches!(err, Error::Unauthorized { .. }), "403: {err:?}");
    let err = Arxiv::parse(503, &[], NOT_ATOM, accessed()).unwrap_err();
    assert!(
        matches!(err, Error::ProviderFailure { .. }) && err.is_transient(),
        "503: {err:?}"
    );
    let err = Arxiv::parse(406, &[], b"", accessed()).unwrap_err();
    assert!(
        matches!(err, Error::PermanentIo { .. }),
        "the 406 seen live is a permanent client error: {err:?}"
    );
}

#[test]
fn status_decides_even_when_the_body_would_parse() {
    let err = Arxiv::parse(429, &[], DOCUMENTED, accessed()).unwrap_err();
    assert!(
        matches!(err, Error::RateLimited { .. }),
        "a result feed under 429 is still a rate limit: {err:?}"
    );
}

#[test]
fn policy_record_carries_the_documented_pacing() {
    let policy = Arxiv::POLICY;
    let limit = policy.rate_limit.unwrap();
    assert_eq!(limit.requests, 1, "one request");
    assert_eq!(
        limit.window,
        std::time::Duration::from_secs(3),
        "every three seconds"
    );
    assert_eq!(policy.max_concurrent, Some(1), "one connection at a time");
    assert_eq!(
        policy.revision,
        format!("{}/{}", policy.provider, policy.retrieved),
        "the revision names the provider and retrieval date"
    );
}
