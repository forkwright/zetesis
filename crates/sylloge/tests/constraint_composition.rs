//! Constraint-composition integration tests.
//!
//! `SearchConstraints` is the most-used public surface in sylloge —
//! every router call composes one. This file exercises composition paths
//! that the unit tests don't cover: budget + allowlist + denylist
//! combinations, default semantics under builder chains, and the
//! round-trip between caller-composed constraints and their serialized
//! form.

#![expect(clippy::unwrap_used, reason = "test assertions must fail loudly")]

use std::time::Duration;

use jiff::Timestamp;
use url::Url;

use sylloge::{
    BudgetConstraint, Citation, FreshnessBasis, FreshnessPolicy, PublicationPrecision,
    PublicationProvenance, PublicationTime, SearchConstraints, SourceKind, SpendLedger,
};

fn now() -> Timestamp {
    "2026-07-01T00:00:00Z".parse().unwrap()
}

#[test]
fn default_constraints_permit_arbitrary_url() {
    let c = SearchConstraints::default();
    assert!(c.permits_url(&Url::parse("https://anywhere.example/x").unwrap()));
}

#[test]
fn compose_all_builders() {
    let c = SearchConstraints::new(20, BudgetConstraint::phase_zero_default())
        .with_freshness(Duration::from_secs(86_400))
        .with_language("en-US".parse().unwrap())
        .with_allowlist(vec![".edu".to_owned(), ".gov".to_owned()])
        .with_denylist(vec!["spam.example".to_owned()]);

    assert_eq!(c.max_results, 20);
    assert_eq!(c.freshness_window, Some(Duration::from_secs(86_400)));
    assert_eq!(c.language.as_ref().unwrap().as_str(), "en-US");
    assert!(c.permits_url(&Url::parse("https://mit.edu/x").unwrap()));
    assert!(c.permits_url(&Url::parse("https://nasa.gov/x").unwrap()));
    assert!(!c.permits_url(&Url::parse("https://example.com/x").unwrap()));
    assert!(!c.permits_url(&Url::parse("https://spam.example/x").unwrap()));
}

#[test]
fn budget_composition_free_only_rejects_paid_tier() {
    let c = SearchConstraints::new(10, BudgetConstraint::free_only());
    // Free-only budget: any paid spend fails.
    assert!(!c.budget.permits(1, &SpendLedger::new(), now()));
    assert!(c.budget.permits(0, &SpendLedger::new(), now()));
}

#[test]
fn budget_composition_phase_zero_blocks_expensive_call() {
    let c = SearchConstraints::new(10, BudgetConstraint::phase_zero_default());
    // $0.05 = 500_000 micro-cents per query cap.
    assert!(c.budget.permits(500_000, &SpendLedger::new(), now()));
    assert!(!c.budget.permits(500_001, &SpendLedger::new(), now()));
}

#[test]
fn budget_composition_ledger_drives_cumulative_caps() {
    let c = SearchConstraints::new(10, BudgetConstraint::phase_zero_default());
    let mut full = SpendLedger::new();
    full.record(now(), c.budget.per_day_cap_micro_cents);
    assert!(!c.budget.permits(1, &full, now()));
    // The same ledger permits again once the 24h window rolls past the
    // recorded spend.
    let next_day: Timestamp = "2026-07-02T01:00:00Z".parse().unwrap();
    assert!(c.budget.permits(1, &full, next_day));
}

#[test]
fn serialize_and_deserialize_composed_constraints_round_trip() {
    // WHY: the router persists a constraint digest into cache keys; if
    // serde round-trip isn't stable across builder composition, two
    // semantically-identical constraints could key differently.
    let c = SearchConstraints::new(10, BudgetConstraint::phase_zero_default())
        .with_freshness(Duration::from_secs(3_600))
        .with_allowlist(vec![".edu".to_owned()]);
    let json = serde_json::to_string(&c).unwrap();
    let back: SearchConstraints = serde_json::from_str(&json).unwrap();
    assert_eq!(back, c);
    // And round-trip again to catch non-idempotent serialization.
    let json2 = serde_json::to_string(&back).unwrap();
    assert_eq!(json, json2);
}

#[test]
fn empty_allowlist_rejects_everything() {
    let c = SearchConstraints::new(10, BudgetConstraint::default()).with_allowlist(Vec::new());
    assert!(!c.permits_url(&Url::parse("https://mit.edu/x").unwrap()));
    assert!(!c.permits_url(&Url::parse("https://example.com/x").unwrap()));
}

#[test]
fn empty_denylist_permits_everything() {
    let c = SearchConstraints::new(10, BudgetConstraint::default()).with_denylist(Vec::new());
    assert!(c.permits_url(&Url::parse("https://mit.edu/x").unwrap()));
}

#[test]
fn language_tag_preserves_subtags() {
    let c = SearchConstraints::new(10, BudgetConstraint::default())
        .with_language("zh-Hant-TW".parse().unwrap());
    let json = serde_json::to_string(&c).unwrap();
    assert!(json.contains("zh-Hant-TW"));
    let back: SearchConstraints = serde_json::from_str(&json).unwrap();
    assert_eq!(back.language.as_ref().unwrap().as_str(), "zh-Hant-TW");
}

#[test]
fn evaluate_freshness_no_window_always_accepts() {
    let c = SearchConstraints::default();
    let citation = Citation::new(
        Url::parse("https://example.org/").unwrap(),
        "2020-01-01T00:00:00Z".parse().unwrap(),
        SourceKind::Web,
        1.0,
        None,
    );
    let decision = c.evaluate_freshness(&citation, "2026-08-01T00:00:00Z".parse().unwrap());
    assert!(decision.accepted);
    assert_eq!(decision.basis, FreshnessBasis::NoWindowConfigured);
}

#[test]
fn evaluate_freshness_rejects_old_publication_despite_recent_retrieval() {
    // WHY(zetesis#50): a citation freshly retrieved but describing
    // year-old content must not pass a freshness window on the strength
    // of accessed_at alone.
    let at_now: Timestamp = "2026-08-01T00:00:00Z".parse().unwrap();
    let c = SearchConstraints::new(10, BudgetConstraint::default())
        .with_freshness(Duration::from_secs(86_400));
    let citation = Citation::new(
        Url::parse("https://example.org/").unwrap(),
        at_now, // retrieved right now
        SourceKind::Web,
        1.0,
        None,
    )
    .with_published_at(PublicationTime::Known {
        at: "2025-01-01T00:00:00Z".parse().unwrap(),
        precision: PublicationPrecision::Exact,
        provenance: PublicationProvenance::ProviderDeclared,
    });
    let decision = c.evaluate_freshness(&citation, at_now);
    assert!(!decision.accepted);
    assert_eq!(decision.basis, FreshnessBasis::PublicationTime);
}

#[test]
fn evaluate_freshness_unknown_time_respects_configured_policy() {
    let at_now: Timestamp = "2026-08-01T00:00:00Z".parse().unwrap();
    let citation = Citation::new(
        Url::parse("https://example.org/").unwrap(),
        at_now,
        SourceKind::Web,
        1.0,
        None,
    );

    let strict = SearchConstraints::new(10, BudgetConstraint::default())
        .with_freshness(Duration::from_secs(86_400))
        .with_freshness_policy(FreshnessPolicy::Strict);
    assert!(!strict.evaluate_freshness(&citation, at_now).accepted);

    let permissive = SearchConstraints::new(10, BudgetConstraint::default())
        .with_freshness(Duration::from_secs(86_400))
        .with_freshness_policy(FreshnessPolicy::Permissive);
    assert!(permissive.evaluate_freshness(&citation, at_now).accepted);
}
