//! Constraint-composition integration tests.
//!
//! `SearchConstraints` is the most-used public surface in zetesis-api —
//! every router call composes one. This file exercises composition paths
//! that the unit tests don't cover: budget + allowlist + denylist
//! combinations, default semantics under builder chains, and the
//! round-trip between caller-composed constraints and their serialized
//! form.

#![allow(clippy::unwrap_used)]
#![allow(clippy::expect_used)]

use std::time::Duration;

use url::Url;

use zetesis_api::{BudgetConstraint, CostTracking, ProviderSpend, SearchConstraints};

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
    assert!(!c.budget.permits(1, &CostTracking::default()));
    assert!(c.budget.permits(0, &CostTracking::default()));
}

#[test]
fn budget_composition_phase_zero_blocks_expensive_call() {
    let c = SearchConstraints::new(10, BudgetConstraint::phase_zero_default());
    // $0.05 = 500_000 micro-cents per query cap.
    assert!(c.budget.permits(500_000, &CostTracking::default()));
    assert!(!c.budget.permits(500_001, &CostTracking::default()));
}

#[test]
fn budget_composition_ledger_drives_cumulative_caps() {
    let c = SearchConstraints::new(10, BudgetConstraint::phase_zero_default());
    let full = CostTracking::from_line_items([ProviderSpend::new(
        "exa",
        c.budget.per_day_cap_micro_cents,
        0,
        1,
    )]);
    assert!(!c.budget.permits(1, &full));
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
