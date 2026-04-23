//! Integration tests that exercise invariants *between* the types in
//! this crate. Unit tests inside each module cover per-type invariants;
//! this file covers the interactions.

#![allow(clippy::unwrap_used)]
#![allow(clippy::expect_used)]

use std::collections::BTreeMap;

use jiff::Timestamp;
use url::Url;

use zetesis_types::{
    BudgetConstraint, Citation, CostTracking, ProvenanceEntry, ProviderSpend, ProviderTier,
    QueryShape, ResearchResult, ResultHit, SourceKind,
};

fn ts() -> Timestamp {
    "2026-04-22T10:00:00Z".parse().unwrap()
}

fn authoritative_citation() -> Citation {
    Citation::new(
        Url::parse("https://pubmed.ncbi.nlm.nih.gov/1/").unwrap(),
        ts(),
        SourceKind::Journal,
        0.95,
        Some("application/pdf".to_owned()),
    )
}

fn web_citation() -> Citation {
    Citation::new(
        Url::parse("https://random.example/page").unwrap(),
        ts(),
        SourceKind::Web,
        0.7,
        Some("text/html".to_owned()),
    )
}

#[test]
fn research_result_with_mixed_provenance_reports_provider_count_correctly() {
    let hits = vec![ResultHit::new(
        "t",
        "s",
        Url::parse("https://example.org/x").unwrap(),
        vec![authoritative_citation()],
        0.9,
    )];
    let provenance = vec![
        ProvenanceEntry::new("semantic_scholar", authoritative_citation()),
        ProvenanceEntry::new("semantic_scholar", authoritative_citation()),
        ProvenanceEntry::new("crossref", web_citation()),
    ];
    let r = ResearchResult::new(
        "q",
        QueryShape::AcademicLiterature,
        hits,
        provenance,
        CostTracking::default(),
        "k",
    );
    assert_eq!(r.provider_count(), 2);
}

#[test]
fn hit_with_multiple_citations_one_strong_is_strong() {
    let hit = ResultHit::new(
        "t",
        "s",
        Url::parse("https://example.org/x").unwrap(),
        vec![web_citation(), authoritative_citation()],
        0.8,
    );
    assert!(hit.has_strong_citation());
}

#[test]
fn hit_with_only_weak_citations_is_not_strong() {
    let hit = ResultHit::new(
        "t",
        "s",
        Url::parse("https://example.org/x").unwrap(),
        vec![web_citation(), web_citation()],
        0.9,
    );
    assert!(!hit.has_strong_citation());
}

#[test]
fn cost_tracking_in_research_result_round_trips() {
    let r = ResearchResult::new(
        "attention is all you need",
        QueryShape::AcademicLiterature,
        Vec::new(),
        Vec::new(),
        CostTracking::from_line_items([
            ProviderSpend::new("semantic_scholar", 0, 1, 1),
            ProviderSpend::new("crossref", 0, 1, 1),
        ]),
        "k",
    );
    let json = serde_json::to_string(&r).unwrap();
    let back: ResearchResult = serde_json::from_str(&json).unwrap();
    assert_eq!(r, back);
    assert_eq!(back.cost_spent.by_provider.len(), 2);
    assert!(!back.cost_spent.any_paid());
}

#[test]
fn budget_composes_with_cost_tracking_from_research_result() {
    // Simulate: a Tier-1 call made against a Tier-1 budget, then the
    // cost_spent from the response is used to check the next call.
    let b = BudgetConstraint::phase_zero_default();
    let first_call = CostTracking::from_line_items([ProviderSpend::new("brave", 100_000, 0, 1)]);
    // A second call of 100_000 micro-cents should still be permitted; a
    // giant cumulative call would not.
    assert!(b.permits(100_000, &first_call));
    assert!(!b.permits(b.per_day_cap_micro_cents, &first_call));
}

#[test]
fn provider_tier_in_fallback_order_matches_principles() {
    let mut tiers = [
        ProviderTier::Tier3PaidDeep,
        ProviderTier::Tier0Free,
        ProviderTier::Tier2SelfHosted,
        ProviderTier::Tier1Cheap,
    ];
    tiers.sort();
    assert_eq!(tiers[0], ProviderTier::Tier0Free);
    assert_eq!(tiers[3], ProviderTier::Tier3PaidDeep);
}

#[test]
fn query_shape_cache_tolerance_informs_freshness_routing() {
    assert!(!QueryShape::FreshnessSensitive.tolerates_stale_cache());
    assert!(QueryShape::AcademicLiterature.tolerates_stale_cache());
    // Shapes that tolerate stale cache should be the common case — weed
    // out accidents by checking the finance / freshness pair are the
    // only exceptions.
    let intolerant = [
        QueryShape::QuickFactual,
        QueryShape::SemanticDiscovery,
        QueryShape::AcademicLiterature,
        QueryShape::Patent,
        QueryShape::Finance,
        QueryShape::Legal,
        QueryShape::FreshnessSensitive,
        QueryShape::GeneralResearch,
        QueryShape::CodeAndPackages,
        QueryShape::DatasetDiscovery,
    ]
    .iter()
    .filter(|s| !s.tolerates_stale_cache())
    .count();
    assert_eq!(
        intolerant, 2,
        "only Finance + FreshnessSensitive should be cache-intolerant"
    );
}

#[test]
fn metadata_bmap_preserves_insertion_independent_ordering() {
    // WHY: BTreeMap ordering is deterministic by key, independent of
    // insertion order. Cache-key stability depends on this — two
    // semantically-identical hits with differently-ordered metadata
    // writes must serialize identically.
    let hit_a = ResultHit::new(
        "t",
        "s",
        Url::parse("https://example.org/x").unwrap(),
        vec![authoritative_citation()],
        0.9,
    )
    .with_metadata("z", serde_json::Value::String("last".to_owned()))
    .with_metadata("a", serde_json::Value::String("first".to_owned()));

    let hit_b = ResultHit::new(
        "t",
        "s",
        Url::parse("https://example.org/x").unwrap(),
        vec![authoritative_citation()],
        0.9,
    )
    .with_metadata("a", serde_json::Value::String("first".to_owned()))
    .with_metadata("z", serde_json::Value::String("last".to_owned()));

    let ja = serde_json::to_string(&hit_a).unwrap();
    let jb = serde_json::to_string(&hit_b).unwrap();
    assert_eq!(ja, jb);
}

#[test]
fn empty_result_still_carries_query_and_shape() {
    let r = ResearchResult::empty("q", QueryShape::Finance, "k");
    assert_eq!(r.query, "q");
    assert_eq!(r.shape, QueryShape::Finance);
    assert_eq!(r.cache_key, "k");
    assert!(r.hits.is_empty());
    assert!(r.provenance.is_empty());
    assert_eq!(r.cost_spent, CostTracking::default());
}

#[test]
fn btreemap_metadata_survives_ciborium() {
    let mut meta = BTreeMap::new();
    meta.insert(
        "doi".to_owned(),
        serde_json::Value::String("10.1/x".to_owned()),
    );
    meta.insert("year".to_owned(), serde_json::Value::Number(2026.into()));
    let mut hit = ResultHit::new(
        "title",
        "snippet",
        Url::parse("https://example.org/x").unwrap(),
        vec![authoritative_citation()],
        0.9,
    );
    hit.metadata = meta;
    let mut buf = Vec::new();
    ciborium::into_writer(&hit, &mut buf).unwrap();
    let back: ResultHit = ciborium::from_reader(buf.as_slice()).unwrap();
    assert_eq!(back.metadata.len(), 2);
    assert_eq!(
        back.metadata.get("doi"),
        Some(&serde_json::Value::String("10.1/x".to_owned()))
    );
}
