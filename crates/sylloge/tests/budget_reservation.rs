//! `BudgetConstraint::try_reserve` integration coverage.
//!
//! `budget.rs`'s own unit tests cover `permits`/`SpendLedger` in
//! isolation; this file exercises the atomic check-and-record contract
//! `try_reserve` adds on top -- the concurrent-double-spend closure, the
//! fleet scope, denial without mutation, and the typed denial data
//! (remaining allowance, reset time, retry classification).

#![expect(clippy::unwrap_used, reason = "test assertions must fail loudly")]

use jiff::{SignedDuration, Timestamp};
use sylloge::{BudgetConstraint, BudgetScope, DAY_WINDOW, Error, SpendLedger};

fn t0() -> Timestamp {
    "2026-07-01T00:00:00Z".parse().unwrap()
}

fn ts(s: &str) -> Timestamp {
    s.parse().unwrap()
}

fn day_window() -> SignedDuration {
    DAY_WINDOW
}

#[test]
fn builders_construct_a_custom_budget_without_struct_literal() {
    // WHY(zetesis#47): BudgetConstraint is #[non_exhaustive], so a
    // downstream crate could not previously construct a custom budget at
    // all -- free_only()/phase_zero_default() were the only reachable
    // values. The with_* builders close that gap.
    let b = BudgetConstraint::free_only()
        .with_per_query_cap(1_000)
        .with_per_day_cap(2_000)
        .with_per_fleet_day_cap(3_000)
        .with_per_agent_cap(4_000)
        .with_paid_tier_allowed(true);
    assert_eq!(b.per_query_cap_micro_cents, 1_000);
    assert_eq!(b.per_day_cap_micro_cents, 2_000);
    assert_eq!(b.per_fleet_day_cap_micro_cents, 3_000);
    assert_eq!(b.per_agent_cap_micro_cents, 4_000);
    assert!(b.allow_paid_tier);
}

#[test]
fn day_window_reset_at_is_none_with_no_events_in_window() {
    let ledger = SpendLedger::new();
    assert!(ledger.day_window_reset_at(t0()).is_none());
}

#[test]
fn day_window_reset_at_is_earliest_event_plus_window() {
    let mut ledger = SpendLedger::new();
    ledger.record(t0(), 10_000);
    ledger.record(ts("2026-07-01T10:00:00Z"), 10_000);
    // The window next has room when the EARLIEST in-window event ages
    // out, not the latest.
    assert_eq!(
        ledger.day_window_reset_at(ts("2026-07-01T12:00:00Z")),
        Some(t0().checked_add(day_window()).unwrap())
    );
}

#[test]
fn try_reserve_concurrent_75_plus_75_never_exceeds_100_cap() {
    // WHY(zetesis#47): the demonstrated defect -- two permits() calls
    // against the same unmodified ledger both see room, and two
    // independent record() calls both land, for 150 against a 100 cap.
    // try_reserve makes the check-then-record a single operation, so the
    // second call observes the first call's recorded spend.
    let b = BudgetConstraint::free_only()
        .with_per_day_cap(100)
        .with_paid_tier_allowed(true);
    let mut consumer = SpendLedger::new();
    let mut fleet = SpendLedger::new();

    assert!(b.try_reserve(75, &mut consumer, &mut fleet, t0()).is_ok());
    let second = b.try_reserve(75, &mut consumer, &mut fleet, t0());
    assert!(
        second.is_err(),
        "second reservation must be denied: {second:?}"
    );
    assert_eq!(consumer.paid_in_day_window_micro_cents(t0()), 75);
}

#[test]
fn try_reserve_denies_without_mutating_either_ledger() {
    let b = BudgetConstraint::free_only()
        .with_per_query_cap(50)
        .with_paid_tier_allowed(true);
    let mut consumer = SpendLedger::new();
    let mut fleet = SpendLedger::new();

    assert!(b.try_reserve(51, &mut consumer, &mut fleet, t0()).is_err());
    assert_eq!(consumer.lifetime_paid_micro_cents(), 0);
    assert_eq!(fleet.lifetime_paid_micro_cents(), 0);
}

#[test]
fn try_reserve_enforces_fleet_scope_independent_of_consumer_scope() {
    let b = BudgetConstraint::free_only()
        .with_per_fleet_day_cap(100)
        .with_paid_tier_allowed(true);
    let mut consumer_a = SpendLedger::new();
    let mut consumer_b = SpendLedger::new();
    let mut fleet = SpendLedger::new();

    // Two different consumers share one fleet ledger; neither
    // consumer-scoped cap is set, so only the fleet total matters.
    assert!(b.try_reserve(60, &mut consumer_a, &mut fleet, t0()).is_ok());
    let denied = b.try_reserve(60, &mut consumer_b, &mut fleet, t0());
    let err = denied.unwrap_err();
    assert!(err.to_string().contains("PerFleetDay"));
}

#[test]
fn try_reserve_denial_carries_remaining_and_reset_time() {
    let b = BudgetConstraint::free_only()
        .with_per_day_cap(100)
        .with_paid_tier_allowed(true);
    let mut consumer = SpendLedger::new();
    let mut fleet = SpendLedger::new();
    consumer.record(t0(), 80);

    let err = b
        .try_reserve(30, &mut consumer, &mut fleet, t0())
        .unwrap_err();
    match err {
        Error::BudgetExceeded {
            scope,
            remaining_micro_cents,
            resets_at,
            ..
        } => {
            assert_eq!(scope, BudgetScope::PerConsumerDay);
            assert_eq!(remaining_micro_cents, 20);
            assert_eq!(resets_at, Some(t0().checked_add(day_window()).unwrap()));
        }
        other => panic!("expected BudgetExceeded, got {other:?}"),
    }
}

#[test]
fn try_reserve_query_and_lifetime_denials_have_no_reset_time() {
    let query_capped = BudgetConstraint::free_only()
        .with_per_query_cap(10)
        .with_paid_tier_allowed(true);
    let mut consumer = SpendLedger::new();
    let mut fleet = SpendLedger::new();
    let err = query_capped
        .try_reserve(11, &mut consumer, &mut fleet, t0())
        .unwrap_err();
    assert!(!err.is_transient());

    let lifetime_capped = BudgetConstraint::free_only()
        .with_per_agent_cap(10)
        .with_paid_tier_allowed(true);
    consumer.record(t0(), 10);
    let err = lifetime_capped
        .try_reserve(1, &mut consumer, &mut fleet, t0())
        .unwrap_err();
    assert!(!err.is_transient());
}
