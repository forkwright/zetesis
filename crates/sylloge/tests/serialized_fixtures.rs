//! Deterministic serialized fixtures for the persisted and exchanged types.
//!
//! Each golden file under `fixtures/serialized/` was written by hand from
//! the documented schema, not captured from this crate's own output, so it
//! is an independent expected value. Every fixture is checked in both
//! directions: the value built through the public API must serialize to
//! exactly the golden bytes (pretty JSON, two-space indent, trailing
//! newline), and the golden bytes must decode back to that same value. A
//! field rename, reorder, re-tag, or representation change fails here
//! before it can silently change what consumers persist or cache-key.
//!
//! `include_str!` makes a missing fixture a compile error, never a skip.

#![expect(clippy::unwrap_used, reason = "test assertions must fail loudly")]

use std::fmt::Debug;
use std::time::Duration;

use jiff::Timestamp;
use serde::Serialize;
use serde::de::DeserializeOwned;
use url::Url;

use sylloge::{
    BudgetConstraint, Citation, CostTracking, FreshnessPolicy, ProvenanceEntry, ProviderSpend,
    PublicationPrecision, PublicationProvenance, PublicationTime, QueryShape, ResearchResult,
    ResearchStatus, ResultHit, SearchConstraints, SourceKind, SpendLedger,
};

fn ts(s: &str) -> Timestamp {
    s.parse().unwrap()
}

/// Assert `value` serializes to exactly `golden` and `golden` decodes to
/// `value`.
fn assert_golden<T>(name: &str, value: &T, golden: &str)
where
    T: Serialize + DeserializeOwned + PartialEq + Debug,
{
    let encoded = serde_json::to_string_pretty(value).unwrap() + "\n";
    assert_eq!(
        encoded, golden,
        "{name}: serialized form drifted from the golden fixture"
    );
    let decoded: T = serde_json::from_str(golden).unwrap();
    assert_eq!(
        &decoded, value,
        "{name}: golden fixture no longer decodes to the same value"
    );
}

fn paper_url() -> Url {
    Url::parse("https://example.org/papers/1706.03762").unwrap()
}

#[test]
fn search_constraints_match_golden_fixture() {
    let budget = BudgetConstraint::free_only()
        .with_per_query_cap(500_000)
        .with_per_day_cap(50_000_000)
        .with_per_agent_cap(200_000_000)
        .with_paid_tier_allowed(true);
    let constraints = SearchConstraints::new(25, budget)
        .with_freshness(Duration::from_secs(86_400))
        .with_freshness_policy(FreshnessPolicy::Permissive)
        .with_language("en-US".parse().unwrap())
        .with_allowlist(vec![".edu".to_owned()])
        .with_denylist(vec!["spam.example".to_owned()]);
    assert_golden(
        "search_constraints",
        &constraints,
        include_str!("fixtures/serialized/search_constraints.json"),
    );
}

#[test]
fn default_search_constraints_match_golden_fixture() {
    assert_golden(
        "search_constraints_default",
        &SearchConstraints::default(),
        include_str!("fixtures/serialized/search_constraints_default.json"),
    );
}

#[test]
fn spend_ledger_matches_golden_fixture() {
    let mut ledger = SpendLedger::new();
    ledger.record(ts("2026-06-01T00:00:00Z"), 50);
    ledger.record(ts("2026-07-01T00:00:00Z"), 100);
    // The June event leaves the rolling window; lifetime keeps it.
    ledger.prune_expired(ts("2026-07-01T12:00:00Z"));
    assert_golden(
        "spend_ledger",
        &ledger,
        include_str!("fixtures/serialized/spend_ledger.json"),
    );
}

#[test]
fn spend_ledger_matches_golden_cbor_fixture() {
    // WHY: ciborium is the fleet's binary codec; the golden hex was encoded
    // by an independent RFC 8949 encoder, so this pins the binary form, not
    // just a round trip through the same library.
    let golden_hex = include_str!("fixtures/serialized/spend_ledger.cbor.hex").trim();
    let golden: Vec<u8> = (0..golden_hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(golden_hex.get(i..i + 2).unwrap(), 16).unwrap())
        .collect();

    let ledger: SpendLedger =
        serde_json::from_str(include_str!("fixtures/serialized/spend_ledger.json")).unwrap();
    let mut encoded = Vec::new();
    ciborium::into_writer(&ledger, &mut encoded).unwrap();
    assert_eq!(
        encoded, golden,
        "CBOR encoding drifted from the golden fixture"
    );
    let decoded: SpendLedger = ciborium::from_reader(golden.as_slice()).unwrap();
    assert_eq!(
        decoded, ledger,
        "golden CBOR must decode to the same ledger"
    );
}

#[test]
fn research_status_variants_match_golden_fixture() {
    let statuses = vec![
        ResearchStatus::Pending,
        ResearchStatus::running(Some(40)),
        ResearchStatus::Ready {
            completed_at: ts("2026-04-22T10:00:00Z"),
        },
        ResearchStatus::Failed {
            message: "upstream unavailable".to_owned(),
        },
        ResearchStatus::Cancelled,
    ];
    assert_golden(
        "research_status",
        &statuses,
        include_str!("fixtures/serialized/research_status.json"),
    );
}

#[test]
fn research_result_matches_golden_fixture() {
    let accessed = ts("2026-04-22T10:00:00Z");
    let citation = Citation::new(
        paper_url(),
        accessed,
        SourceKind::Preprint,
        0.95,
        Some("application/pdf".to_owned()),
    )
    .with_published_at(PublicationTime::Known {
        at: ts("2017-06-12T00:00:00Z"),
        precision: PublicationPrecision::DateOnly,
        provenance: PublicationProvenance::ProviderDeclared,
    });
    // No freshness window configured: the public evaluator's receipt.
    let freshness = SearchConstraints::default().evaluate_freshness(&citation, accessed);
    let hit = ResultHit::new(
        "Attention Is All You Need",
        "The dominant sequence transduction models",
        paper_url(),
        vec![citation],
        0.9,
    )
    .unwrap()
    .with_metadata("year", serde_json::json!(2017))
    .with_metadata("arxiv_id", serde_json::json!("1706.03762"))
    .with_freshness(freshness);
    let provenance = vec![ProvenanceEntry::new(
        "arxiv",
        Citation::new(paper_url(), accessed, SourceKind::Preprint, 1.0, None),
    )];
    let result = ResearchResult::new(
        "attention is all you need",
        QueryShape::AcademicLiterature,
        vec![hit],
        provenance,
        CostTracking::from_line_items([ProviderSpend::new("arxiv", 0, 1, 1)]),
        "fixture-cache-key",
    );
    assert_golden(
        "research_result",
        &result,
        include_str!("fixtures/serialized/research_result.json"),
    );
}
