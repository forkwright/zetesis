//! Semantic Scholar request builder and response parser, against the
//! recorded and documented-shape fixtures in
//! `fixtures/providers/semantic_scholar/` (provenance in that directory's
//! README).

#![expect(clippy::unwrap_used, reason = "test assertions must fail loudly")]

use jiff::Timestamp;
use serde_json::json;
use sylloge::{
    BudgetConstraint, Error, PublicationPrecision, PublicationProvenance, PublicationTime,
    ResultHit, SearchConstraints, SemanticScholar, SourceKind,
};

const DOCUMENTED: &[u8] =
    include_bytes!("fixtures/providers/semantic_scholar/search_documented_shape.json");
const EMPTY: &[u8] = include_bytes!("fixtures/providers/semantic_scholar/search_empty.json");
const PARTIAL: &[u8] = include_bytes!("fixtures/providers/semantic_scholar/search_partial.json");
const SCHEMA_CHANGED: &[u8] =
    include_bytes!("fixtures/providers/semantic_scholar/search_schema_changed.json");
const TYPE_CHANGED: &[u8] =
    include_bytes!("fixtures/providers/semantic_scholar/search_type_changed.json");
const HOSTILE_TYPE_CHANGED: &[u8] =
    include_bytes!("fixtures/providers/semantic_scholar/search_hostile_type_changed.json");
const HOSTILE_MALFORMED: &[u8] =
    include_bytes!("fixtures/providers/semantic_scholar/search_hostile_malformed.json");
const MALFORMED: &[u8] =
    include_bytes!("fixtures/providers/semantic_scholar/search_malformed.json");
const MALFORMED_RECORDS: &[u8] =
    include_bytes!("fixtures/providers/semantic_scholar/search_malformed_records.json");
const BAD_REQUEST: &[u8] =
    include_bytes!("fixtures/providers/semantic_scholar/bad_request_documented.json");
const RATE_LIMITED: &[u8] =
    include_bytes!("fixtures/providers/semantic_scholar/rate_limited_recorded_2026-09-25.json");
const FORBIDDEN: &[u8] = include_bytes!("fixtures/providers/semantic_scholar/forbidden.json");
const SERVER_ERROR: &[u8] = include_bytes!("fixtures/providers/semantic_scholar/server_error.json");

fn accessed() -> Timestamp {
    "2026-09-25T18:00:00Z".parse().unwrap()
}

fn parse_ok(body: &[u8]) -> Vec<ResultHit> {
    let parsed = SemanticScholar::parse(200, &[], body, accessed()).unwrap();
    assert_eq!(parsed.malformed_records, 0, "no record is dropped");
    parsed.hits
}

fn query_pairs(url: &url::Url) -> Vec<(String, String)> {
    url.query_pairs()
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect()
}

#[test]
fn request_targets_relevance_search_with_fields_and_a_capped_limit() {
    let request = SemanticScholar::request(
        "  transformer   attention ",
        &SearchConstraints::new(250, BudgetConstraint::free_only()),
    )
    .unwrap();
    assert_eq!(
        request.url.as_str().split('?').next(),
        Some("https://api.semanticscholar.org/graph/v1/paper/search"),
        "relevance search, not the bulk listing endpoint"
    );
    assert_eq!(
        query_pairs(&request.url),
        [
            ("query".to_owned(), "transformer attention".to_owned()),
            ("limit".to_owned(), "100".to_owned()),
            (
                "fields".to_owned(),
                "paperId,corpusId,externalIds,url,title,abstract,venue,year,publicationDate,\
                 publicationTypes,authors"
                    .to_owned()
            ),
        ],
        "whitespace collapsed, limit capped at the documented 100"
    );
    assert_eq!(
        request.headers,
        [("accept", "application/json".to_owned())],
        "JSON accepted; no API key is sent"
    );
}

#[test]
fn request_replaces_hyphens_as_the_endpoint_documents() {
    let request =
        SemanticScholar::request("self-supervised learning", &SearchConstraints::default())
            .unwrap();
    assert_eq!(
        query_pairs(&request.url).first().map(|(_, v)| v.as_str()),
        Some("self supervised learning"),
        "hyphenated terms match nothing, so hyphens become spaces"
    );
}

#[test]
fn request_refuses_a_blank_query_and_a_zero_result_budget() {
    let err = SemanticScholar::request(" - ", &SearchConstraints::default()).unwrap_err();
    assert!(
        matches!(err, Error::InvalidQuery { .. }),
        "a query of hyphens has no searchable text: {err:?}"
    );
    let err = SemanticScholar::request(
        "q",
        &SearchConstraints::new(0, BudgetConstraint::free_only()),
    )
    .unwrap_err();
    assert!(
        matches!(err, Error::InvalidConstraint { ref field, .. } if field == "max_results"),
        "max_results 0 is refused: {err:?}"
    );
}

#[test]
fn documented_shape_keeps_identities_provenance_and_kinds() {
    let hits = parse_ok(DOCUMENTED);
    assert_eq!(hits.len(), 3, "one hit per paper");

    let journal = hits.first().unwrap();
    assert_eq!(
        journal.title,
        "Routing Queries Across Free Scholarly Indexes"
    );
    assert_eq!(
        journal.url.as_str(),
        "https://www.semanticscholar.org/paper/0f3c9a1b2d4e5f60718293a4b5c6d7e8f9012345",
        "the paper's website URL"
    );
    assert_eq!(
        journal.snippet, "A synthetic journal record used only as a parser fixture.",
        "the abstract is the snippet"
    );
    let citation = journal.citations.first().unwrap();
    assert_eq!(
        citation.source_url, journal.url,
        "the citation names the hit URL"
    );
    assert_eq!(
        citation.accessed_at,
        accessed(),
        "access time is the caller's"
    );
    assert_eq!(
        citation.source_kind,
        SourceKind::Journal,
        "typed JournalArticle"
    );
    assert_eq!(
        citation.content_type, None,
        "metadata, not the source payload"
    );
    assert_eq!(
        citation.published_at,
        PublicationTime::Known {
            at: "2019-07-15T00:00:00Z".parse().unwrap(),
            precision: PublicationPrecision::DateOnly,
            provenance: PublicationProvenance::ProviderDeclared,
        },
        "publicationDate is a date-only, provider-declared time"
    );
    assert_eq!(
        serde_json::to_value(&journal.metadata).unwrap(),
        json!({
            "s2_paper_id": "0f3c9a1b2d4e5f60718293a4b5c6d7e8f9012345",
            "corpus_id": 900_000_001,
            "doi": "10.5555/synthetic.journal.2019.07",
            "year": 2019,
            "venue": "Journal of Synthetic Fixtures",
            "authors": ["Ada Example", "Brook Sample"],
            "publication_types": ["JournalArticle", "Review"],
            "publication_date": "2019-07-15",
            "license": "attribution: Semantic Scholar",
            "provider_policy_revision": "semantic_scholar/2026-09-25",
        }),
        "every identity and provenance field, DOI lowercased"
    );
    assert!(
        (journal.score - 1.0).abs() < f32::EPSILON,
        "rank 1 scores 1.0"
    );
}

#[test]
fn documented_shape_keeps_an_undated_preprint_undated() {
    let hits = parse_ok(DOCUMENTED);
    let preprint = hits.get(1).unwrap();
    let citation = preprint.citations.first().unwrap();
    assert_eq!(
        citation.source_kind,
        SourceKind::Preprint,
        "arXiv is its only identity; arXiv's own DOI does not change that"
    );
    assert_eq!(
        citation.published_at,
        PublicationTime::Unknown,
        "a null publicationDate with a year is still undated"
    );
    assert_eq!(
        preprint.metadata.get("year"),
        Some(&json!(2021)),
        "year kept"
    );
    assert_eq!(
        preprint.metadata.get("arxiv_id"),
        Some(&json!("2101.00001")),
        "arXiv identity kept"
    );
    assert_eq!(
        preprint.metadata.get("arxiv_doi"),
        Some(&json!("10.48550/arxiv.2101.00001")),
        "arXiv's DataCite DOI is kept, lowercased, as the arXiv DOI"
    );
    assert!(
        !preprint.metadata.contains_key("doi"),
        "arXiv's DataCite DOI is not a publisher DOI identity"
    );
    assert_eq!(preprint.snippet, "", "a null abstract is an empty snippet");
    assert!(
        (preprint.score - 0.5).abs() < f32::EPSILON,
        "rank 2 scores 0.5"
    );

    let conference = hits.get(2).unwrap();
    assert_eq!(
        conference.citations.first().unwrap().source_kind,
        SourceKind::Web,
        "neither a journal article nor an arXiv-only record"
    );
    assert_eq!(
        conference.metadata.get("authors"),
        Some(&json!([])),
        "an empty author list is kept as declared"
    );
}

#[test]
fn empty_result_list_is_an_empty_answer() {
    assert!(parse_ok(EMPTY).is_empty(), "no papers, no hits, no filler");
}

#[test]
fn partial_records_keep_only_what_is_present() {
    let hits = parse_ok(PARTIAL);
    assert_eq!(hits.len(), 2, "both papers parse");

    let bare = hits.first().unwrap();
    assert_eq!(
        bare.url.as_str(),
        "https://api.semanticscholar.org/graph/v1/paper/4d5e6f708192a3b4c5d6e7f8091a2b3c4d5e6f70",
        "without a website url the hit names the paper resource"
    );
    assert_eq!(
        serde_json::to_value(&bare.metadata).unwrap(),
        json!({
            "s2_paper_id": "4d5e6f708192a3b4c5d6e7f8091a2b3c4d5e6f70",
            "license": "attribution: Semantic Scholar",
            "provider_policy_revision": "semantic_scholar/2026-09-25",
        }),
        "nothing is invented for absent fields"
    );
    assert_eq!(
        bare.citations.first().unwrap().published_at,
        PublicationTime::Unknown,
        "no date field"
    );

    let nulls = hits.get(1).unwrap();
    assert!(
        !nulls.metadata.contains_key("venue"),
        "a blank venue is not recorded"
    );
    assert!(
        !nulls.metadata.contains_key("authors"),
        "null authors are not an empty list"
    );
}

#[test]
fn schema_change_with_added_and_removed_fields_still_parses() {
    let hits = parse_ok(SCHEMA_CHANGED);
    let hit = hits.first().unwrap();
    assert_eq!(hit.title, "Fields Added and Removed");
    assert_eq!(
        hit.metadata.get("arxiv_id"),
        Some(&json!("2103.00006")),
        "a versioned ArXiv external id loses its version"
    );
    assert!(!hit.metadata.contains_key("year"), "year is gone");
    assert_eq!(
        hit.citations.first().unwrap().source_kind,
        SourceKind::Preprint,
        "no publicationTypes, an arXiv id, and no DOI"
    );
    assert_eq!(
        hit.metadata.get("authors"),
        Some(&json!(["Dana Fixture"])),
        "unknown author fields are ignored"
    );
}

#[test]
fn type_change_in_a_known_field_is_a_malformed_response() {
    let err = SemanticScholar::parse(200, &[], TYPE_CHANGED, accessed()).unwrap_err();
    assert!(
        matches!(err, Error::ProviderFailure { .. }),
        "a known field of the wrong type is not guessed at: {err:?}"
    );
    assert!(
        err.to_string().contains("malformed response: JSON"),
        "the message names the defect: {err}"
    );
    assert!(
        !err.to_string().contains("2021"),
        "the offending value is not quoted: {err}"
    );
}

#[test]
fn hostile_text_in_a_type_changed_or_malformed_body_never_reaches_the_error() {
    for (body, case) in [
        (HOSTILE_TYPE_CHANGED, "type-changed"),
        (HOSTILE_MALFORMED, "malformed"),
    ] {
        let err = SemanticScholar::parse(200, &[], body, accessed()).unwrap_err();
        let message = err.to_string();
        assert!(
            matches!(err, Error::ProviderFailure { .. })
                && message.contains("malformed response: JSON"),
            "{case}: still a named JSON defect: {message}"
        );
        assert!(
            !message.contains("IGNORE") && !message.contains("INSTRUCTIONS"),
            "{case}: no body text in the error: {message}"
        );
    }
}

#[test]
fn malformed_body_is_a_provider_failure_naming_the_position() {
    let err = SemanticScholar::parse(200, &[], MALFORMED, accessed()).unwrap_err();
    assert!(
        matches!(err, Error::ProviderFailure { .. }),
        "cut-off body: {err:?}"
    );
    assert!(
        err.to_string().contains("line 1 column"),
        "the parser position is reported: {err}"
    );
    assert!(err.is_transient(), "a bad body may clear on retry");
}

#[test]
fn records_without_a_title_or_with_an_unusable_url_are_dropped_and_counted() {
    let parsed = SemanticScholar::parse(200, &[], MALFORMED_RECORDS, accessed()).unwrap();
    assert_eq!(
        parsed.malformed_records, 2,
        "the untitled paper and the paper with a bad url are dropped"
    );
    let titles: Vec<&str> = parsed.hits.iter().map(|h| h.title.as_str()).collect();
    assert_eq!(
        titles,
        ["A Complete Record", "Another Complete Record"],
        "the complete papers survive"
    );
    let scores: Vec<f32> = parsed.hits.iter().map(|h| h.score).collect();
    assert_eq!(
        scores,
        [1.0, 0.25],
        "survivors keep the rank the provider gave them"
    );
}

#[test]
fn recorded_rate_limit_without_retry_after_leaves_backoff_to_the_caller() {
    let err = SemanticScholar::parse(429, &[], RATE_LIMITED, accessed()).unwrap_err();
    assert!(
        matches!(
            err,
            Error::RateLimited { ref provider, retry_after_ms: None, .. } if provider == "semantic_scholar"
        ),
        "429 without Retry-After: {err:?}"
    );
    assert!(
        !err.to_string().contains("apply for a key"),
        "upstream text is not quoted: {err}"
    );
}

#[test]
fn rate_limit_with_retry_after_reports_milliseconds() {
    let err =
        SemanticScholar::parse(429, &[("Retry-After", "5")], RATE_LIMITED, accessed()).unwrap_err();
    assert!(
        matches!(
            err,
            Error::RateLimited {
                retry_after_ms: Some(5_000),
                ..
            }
        ),
        "Retry-After seconds become milliseconds: {err:?}"
    );
}

#[test]
fn authentication_denial_is_unauthorized() {
    let err = SemanticScholar::parse(403, &[], FORBIDDEN, accessed()).unwrap_err();
    assert!(
        matches!(err, Error::Unauthorized { .. }),
        "403 is an authentication denial: {err:?}"
    );
    assert!(err.is_permanent(), "credentials must change before a retry");
}

#[test]
fn documented_bad_request_is_an_invalid_query() {
    let err = SemanticScholar::parse(400, &[], BAD_REQUEST, accessed()).unwrap_err();
    assert!(
        matches!(err, Error::InvalidQuery { .. }),
        "400 rejects the request itself: {err:?}"
    );
    assert!(
        !err.to_string().contains("creditCardNumber"),
        "upstream text is not quoted: {err}"
    );
}

#[test]
fn request_encodes_reserved_characters_in_the_query() {
    let request = SemanticScholar::request(
        "a&b=c #d +e",
        &SearchConstraints::new(3, BudgetConstraint::free_only()),
    )
    .unwrap();
    assert_eq!(
        request.url.as_str(),
        "https://api.semanticscholar.org/graph/v1/paper/search?query=a%26b%3Dc+%23d+%2Be&limit=3\
         &fields=paperId%2CcorpusId%2CexternalIds%2Curl%2Ctitle%2Cabstract%2Cvenue%2Cyear\
         %2CpublicationDate%2CpublicationTypes%2Cauthors",
        "`&`, `=`, `#`, and `+` stay inside the one query parameter"
    );
}

#[test]
fn a_paper_url_that_is_not_http_with_a_host_drops_the_record() {
    let body = br#"{"data":[
        {"paperId":"p1","title":"Script","url":"javascript:alert(1)"},
        {"paperId":"p2","title":"Local file","url":"file:///etc/passwd"},
        {"paperId":"p3","title":"No host","url":"data:text/html,hello"},
        {"paperId":"p4","title":"Kept","url":"https://www.semanticscholar.org/paper/p4"}
    ]}"#;
    let parsed = SemanticScholar::parse(200, &[], body, accessed()).unwrap();
    assert_eq!(
        parsed
            .hits
            .iter()
            .map(|hit| hit.url.as_str())
            .collect::<Vec<_>>(),
        ["https://www.semanticscholar.org/paper/p4"],
        "only an http(s) URL with a host becomes a hit"
    );
    assert_eq!(
        parsed.malformed_records, 3,
        "the other three are malformed records"
    );
}

#[test]
fn server_error_is_a_transient_provider_failure() {
    let err = SemanticScholar::parse(500, &[], SERVER_ERROR, accessed()).unwrap_err();
    assert!(
        matches!(err, Error::ProviderFailure { .. }) && err.is_transient(),
        "5xx may clear on retry: {err:?}"
    );
}

#[test]
fn status_decides_even_when_the_body_would_parse() {
    let err = SemanticScholar::parse(429, &[], DOCUMENTED, accessed()).unwrap_err();
    assert!(
        matches!(err, Error::RateLimited { .. }),
        "a result-shaped body under 429 is still a rate limit: {err:?}"
    );
}

#[test]
fn policy_record_matches_the_documented_endpoint() {
    let policy = SemanticScholar::POLICY;
    assert_eq!(policy.provider, "semantic_scholar");
    assert_eq!(
        policy.revision, "semantic_scholar/2026-09-25",
        "the revision names the provider and retrieval date"
    );
    assert_eq!(
        policy.retrieved,
        jiff::civil::date(2026, 9, 25),
        "the date the sources were read"
    );
    assert_eq!(policy.rate_limit, None, "no per-client limit is documented");
    assert_eq!(policy.license, "attribution: Semantic Scholar");
}
