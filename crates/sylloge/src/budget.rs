//! Pre-call budget constraints and the persisted spend ledger.
//!
//! A [`BudgetConstraint`] is supplied by the caller on every research
//! request. The budget layer checks the agent's persisted [`SpendLedger`]
//! against the constraints and short-circuits the router if any ceiling
//! would be exceeded. The caller's intent is captured once at call time;
//! the router does not silently raise limits.
//!
//! [`SpendLedger`] carries timestamped paid-spend events so the per-day cap
//! is a true rolling 24-hour window, while the per-agent (lifetime) cap
//! reads an unpruned running total. [`crate::CostTracking`] remains the
//! per-call cost report; the router folds each call's paid total into the
//! ledger via [`SpendLedger::record_cost`].
//!
//! [`BudgetConstraint::permits`] and [`SpendLedger::record`] are
//! deliberately separate, non-atomic operations -- a caller that checks
//! then records against a ledger it does not hold exclusively (e.g. two
//! concurrent calls sharing one `Arc<Mutex<SpendLedger>>` but each doing
//! `lock(); permits(); unlock(); ...; lock(); record(); unlock();` instead
//! of one held lock) can let both calls see room and both record, exceeding
//! the cap. [`BudgetConstraint::try_reserve`] closes that gap for callers
//! who hold `&mut SpendLedger` across the whole decision: it checks every
//! configured scope and only then records, as one operation, so there is no
//! window between "checked" and "recorded" for a caller using it correctly.

use jiff::{SignedDuration, Timestamp};
use serde::{Deserialize, Serialize};

use crate::cost::CostTracking;
use crate::error::{BudgetExceededSnafu, Result};

/// Length of the rolling per-day budget window.
pub const DAY_WINDOW: SignedDuration = SignedDuration::from_hours(24);

/// Single timestamped paid-spend entry inside a [`SpendLedger`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct SpendEvent {
    /// When the spend was recorded.
    pub at: Timestamp,
    /// Paid spend in USD micro-cents.
    pub paid_micro_cents: u64,
}

/// Persisted per-agent paid-spend ledger with rolling-window accounting.
///
/// Two views over the same recordings:
/// - lifetime total — never pruned; feeds
///   [`BudgetConstraint::per_agent_cap_micro_cents`].
/// - timestamped events — feed the rolling 24-hour
///   [`BudgetConstraint::per_day_cap_micro_cents`] window and may be pruned
///   once they leave it ([`SpendLedger::prune_expired`]).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpendLedger {
    lifetime_paid_micro_cents: u64,
    events: Vec<SpendEvent>,
}

impl SpendLedger {
    /// Empty ledger.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a paid spend of `paid_micro_cents` at `at`. Zero-spend
    /// records are ignored — free-tier usage is tracked in
    /// [`CostTracking::free_tier_units`], not here.
    pub fn record(&mut self, at: Timestamp, paid_micro_cents: u64) {
        if paid_micro_cents == 0 {
            return;
        }
        self.lifetime_paid_micro_cents = self
            .lifetime_paid_micro_cents
            .saturating_add(paid_micro_cents);
        self.events.push(SpendEvent {
            at,
            paid_micro_cents,
        });
    }

    /// Fold a per-call [`CostTracking`] report into the ledger at `at`.
    pub fn record_cost(&mut self, at: Timestamp, cost: &CostTracking) {
        self.record(at, cost.total_paid_micro_cents());
    }

    /// Lifetime paid spend in micro-cents. Unaffected by pruning.
    #[must_use]
    pub const fn lifetime_paid_micro_cents(&self) -> u64 {
        self.lifetime_paid_micro_cents
    }

    /// Paid spend recorded strictly after `cutoff`, in micro-cents.
    #[must_use]
    pub fn paid_since_micro_cents(&self, cutoff: Timestamp) -> u64 {
        self.events
            .iter()
            .filter(|e| e.at > cutoff)
            .map(|e| e.paid_micro_cents)
            .fold(0_u64, u64::saturating_add)
    }

    /// Paid spend inside the rolling 24-hour window ending at `now`, in
    /// micro-cents.
    #[must_use]
    pub fn paid_in_day_window_micro_cents(&self, now: Timestamp) -> u64 {
        self.paid_since_micro_cents(day_window_start(now))
    }

    /// When the rolling 24-hour window as of `now` next has room -- the
    /// instant the oldest event currently inside the window rolls out of
    /// it. `None` when no event is currently inside the window (there is
    /// nothing to wait on; the window already has room for anything up to
    /// the configured cap).
    #[must_use]
    pub fn day_window_reset_at(&self, now: Timestamp) -> Option<Timestamp> {
        let cutoff = day_window_start(now);
        self.events
            .iter()
            .filter(|e| e.at > cutoff)
            .map(|e| e.at)
            .min()
            .map(|earliest| earliest.checked_add(DAY_WINDOW).unwrap_or(Timestamp::MAX))
    }

    /// Recorded spend events, in recording order.
    #[must_use]
    pub fn events(&self) -> &[SpendEvent] {
        &self.events
    }

    /// Drop events that have left the 24-hour window as of `now`, bounding
    /// ledger growth. The lifetime total is unaffected: only the per-day
    /// window reads the event list, and an expired event can never re-enter
    /// a later window (windows only move forward).
    pub fn prune_expired(&mut self, now: Timestamp) {
        let cutoff = day_window_start(now);
        self.events.retain(|e| e.at > cutoff);
    }
}

/// Start of the rolling 24-hour window ending at `now`.
fn day_window_start(now: Timestamp) -> Timestamp {
    // WHY: saturate at Timestamp::MIN instead of erroring — a window that
    // reaches past the representable range simply includes every event,
    // which is the conservative (spend-limiting) direction.
    now.checked_sub(DAY_WINDOW).unwrap_or(Timestamp::MIN)
}

/// Which configured ceiling a [`crate::Error::BudgetExceeded`] denial
/// names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
#[serde(rename_all = "snake_case")]
pub enum BudgetScope {
    /// [`BudgetConstraint::per_query_cap_micro_cents`] -- single-call
    /// ceiling.
    PerQuery,
    /// [`BudgetConstraint::per_day_cap_micro_cents`] -- rolling 24-hour
    /// ceiling for the calling consumer/agent.
    PerConsumerDay,
    /// [`BudgetConstraint::per_fleet_day_cap_micro_cents`] -- rolling
    /// 24-hour ceiling shared across every consumer/agent in the fleet.
    PerFleetDay,
    /// [`BudgetConstraint::per_agent_cap_micro_cents`] -- lifetime ceiling
    /// for the calling consumer/agent.
    PerAgentLifetime,
    /// [`BudgetConstraint::allow_paid_tier`] is `false`; no paid spend is
    /// permitted regardless of the numeric caps.
    PaidTierDisabled,
}

/// Per-call budget ceiling.
///
/// All caps are in USD micro-cents (see [`crate::ProviderSpend`] for the
/// unit rationale). A value of `0` for a cap means "cap disabled", not
/// "cap of zero cents" — the latter would reject every paid call, which
/// is not the common intent. Callers who want a true zero-spend budget
/// should set `allow_paid_tier = false` instead.
///
/// The cap hierarchy has four scopes: the three canonical Phase 0 scopes
/// (query, consumer-day, fleet-day) plus a lifetime cap:
/// - [`BudgetConstraint::per_query_cap_micro_cents`] — single-call ceiling.
///   Violations reject the call before it reaches any paid provider.
/// - [`BudgetConstraint::per_day_cap_micro_cents`] — rolling-24-hour
///   ceiling for the calling consumer/agent. Enforced against the
///   timestamped events in the persisted [`SpendLedger`].
/// - [`BudgetConstraint::per_fleet_day_cap_micro_cents`] — rolling-24-hour
///   ceiling shared across the whole fleet. Enforced against a second,
///   fleet-scoped [`SpendLedger`] the caller supplies separately from the
///   consumer/agent ledger (see [`BudgetConstraint::try_reserve`]).
/// - [`BudgetConstraint::per_agent_cap_micro_cents`] — lifetime cap for
///   the calling agent (typically set per-deployment, not per-call).
///
/// Constructing a custom [`BudgetConstraint`] outside this crate: the
/// type is `#[non_exhaustive]`, so use [`BudgetConstraint::free_only`] or
/// [`BudgetConstraint::phase_zero_default`] as a base and the `with_*`
/// builders to adjust individual ceilings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct BudgetConstraint {
    /// Max paid spend this single call may incur, in micro-cents.
    /// `0` = no per-query cap (subject to the per-day / per-agent caps).
    pub per_query_cap_micro_cents: u64,

    /// Max paid spend the calling agent may incur in a rolling 24-hour
    /// window, in micro-cents. `0` = no per-day cap.
    pub per_day_cap_micro_cents: u64,

    /// Max paid spend the whole fleet may incur in a rolling 24-hour
    /// window, in micro-cents, across every consumer/agent. `0` = no
    /// fleet-day cap. Enforced against a fleet-scoped [`SpendLedger`]
    /// distinct from any individual agent's ledger.
    pub per_fleet_day_cap_micro_cents: u64,

    /// Max lifetime paid spend the calling agent may incur, in
    /// micro-cents. `0` = no lifetime cap.
    pub per_agent_cap_micro_cents: u64,

    /// Whether the router is allowed to attempt any paid tier (Tier 1 /
    /// Tier 3). Setting this `false` forces free-only routing even if the
    /// numeric caps would permit spend.
    pub allow_paid_tier: bool,
}

impl BudgetConstraint {
    /// Free-only budget (no paid tier permitted, all numeric caps disabled).
    #[must_use]
    pub const fn free_only() -> Self {
        Self {
            per_query_cap_micro_cents: 0,
            per_day_cap_micro_cents: 0,
            per_fleet_day_cap_micro_cents: 0,
            per_agent_cap_micro_cents: 0,
            allow_paid_tier: false,
        }
    }

    /// Default-ish permissive budget: paid tier allowed with a $0.05 per-query
    /// cap, $5/day soft cap, $20 lifetime cap. Matches the Phase 0 initial
    /// proposal in `projects/zetesis/phases/00-spec/PLAN.md` (REQ-00-04).
    /// No fleet-day cap by default -- set one explicitly via
    /// [`BudgetConstraint::with_per_fleet_day_cap`] once a fleet-wide
    /// ledger is wired up.
    #[must_use]
    pub const fn phase_zero_default() -> Self {
        Self {
            // $0.05 = 500_000 micro-cents
            per_query_cap_micro_cents: 500_000,
            // $5.00 = 50_000_000 micro-cents
            per_day_cap_micro_cents: 50_000_000,
            per_fleet_day_cap_micro_cents: 0,
            // $20.00 = 200_000_000 micro-cents
            per_agent_cap_micro_cents: 200_000_000,
            allow_paid_tier: true,
        }
    }

    /// Builder: set the per-query cap. See
    /// [`BudgetConstraint::per_query_cap_micro_cents`].
    #[must_use]
    pub const fn with_per_query_cap(mut self, cap_micro_cents: u64) -> Self {
        self.per_query_cap_micro_cents = cap_micro_cents;
        self
    }

    /// Builder: set the per-consumer-day cap. See
    /// [`BudgetConstraint::per_day_cap_micro_cents`].
    #[must_use]
    pub const fn with_per_day_cap(mut self, cap_micro_cents: u64) -> Self {
        self.per_day_cap_micro_cents = cap_micro_cents;
        self
    }

    /// Builder: set the per-fleet-day cap. See
    /// [`BudgetConstraint::per_fleet_day_cap_micro_cents`].
    #[must_use]
    pub const fn with_per_fleet_day_cap(mut self, cap_micro_cents: u64) -> Self {
        self.per_fleet_day_cap_micro_cents = cap_micro_cents;
        self
    }

    /// Builder: set the per-agent lifetime cap. See
    /// [`BudgetConstraint::per_agent_cap_micro_cents`].
    #[must_use]
    pub const fn with_per_agent_cap(mut self, cap_micro_cents: u64) -> Self {
        self.per_agent_cap_micro_cents = cap_micro_cents;
        self
    }

    /// Builder: set whether any paid tier may be attempted. See
    /// [`BudgetConstraint::allow_paid_tier`].
    #[must_use]
    pub const fn with_paid_tier_allowed(mut self, allowed: bool) -> Self {
        self.allow_paid_tier = allowed;
        self
    }

    /// Whether this budget would permit a paid call of `spend_micro_cents`
    /// at time `now`, given the calling agent's persisted `ledger`.
    ///
    /// Only paid spend counts toward the caps; free-tier units are
    /// tracked separately for rate-limit enforcement. A cap of `0` is
    /// treated as "disabled". The per-day cap is evaluated against the
    /// rolling 24-hour window ending at `now`; the per-agent cap against
    /// the ledger's lifetime total. Does not check
    /// [`BudgetConstraint::per_fleet_day_cap_micro_cents`] -- that scope
    /// needs a second, fleet-wide ledger; use
    /// [`BudgetConstraint::try_reserve`] to check every configured scope
    /// including fleet.
    ///
    /// NOT atomic with any later [`SpendLedger::record`] against the same
    /// ledger -- see the module docs. Prefer
    /// [`BudgetConstraint::try_reserve`] wherever the caller can hold
    /// `&mut SpendLedger` across the decision.
    #[must_use]
    pub fn permits(&self, spend_micro_cents: u64, ledger: &SpendLedger, now: Timestamp) -> bool {
        if !self.allow_paid_tier && spend_micro_cents > 0 {
            return false;
        }
        if self.per_query_cap_micro_cents > 0 && spend_micro_cents > self.per_query_cap_micro_cents
        {
            return false;
        }
        if self.per_day_cap_micro_cents > 0
            && ledger
                .paid_in_day_window_micro_cents(now)
                .saturating_add(spend_micro_cents)
                > self.per_day_cap_micro_cents
        {
            return false;
        }
        if self.per_agent_cap_micro_cents > 0
            && ledger
                .lifetime_paid_micro_cents()
                .saturating_add(spend_micro_cents)
                > self.per_agent_cap_micro_cents
        {
            return false;
        }
        true
    }

    /// Atomically check every configured scope -- query, consumer-day,
    /// fleet-day, agent-lifetime -- against `spend_micro_cents` and, only
    /// if every scope permits it, record the spend into both
    /// `consumer_ledger` and `fleet_ledger` as a single operation.
    ///
    /// Scopes are checked in the order listed above; the first violated
    /// scope is returned and NEITHER ledger is mutated on denial -- a
    /// caller retrying after a denial starts from the same ledger state,
    /// not a partially-charged one.
    ///
    /// WHY(zetesis#47): [`BudgetConstraint::permits`] followed by a
    /// separate [`SpendLedger::record`] leaves a window in which two
    /// concurrent calls both see room and both record, exceeding the cap
    /// (demonstrated by the
    /// `try_reserve_concurrent_75_plus_75_never_exceeds_100_cap`
    /// integration test). This method closes that window for a caller
    /// that holds `&mut SpendLedger` for both ledgers across the whole
    /// call -- typically behind one lock acquisition covering both.
    ///
    /// This is a single-phase reservation: the recorded amount is
    /// `spend_micro_cents` itself, treated as final. It does not implement
    /// a two-phase reserve-then-reconcile-to-actual-cost protocol -- a
    /// recorded spend has no identity a later call could target to adjust
    /// it -- and it does not persist across a process restart: both
    /// ledgers are caller-owned, in-memory value types.
    ///
    /// # Errors
    ///
    /// Returns [`crate::Error::BudgetExceeded`] naming the first violated
    /// [`BudgetScope`], with the remaining allowance and (for
    /// rolling-window scopes) the reset time at which the scope will next
    /// have room.
    pub fn try_reserve(
        &self,
        spend_micro_cents: u64,
        consumer_ledger: &mut SpendLedger,
        fleet_ledger: &mut SpendLedger,
        now: Timestamp,
    ) -> Result<()> {
        if !self.allow_paid_tier && spend_micro_cents > 0 {
            return Err(BudgetExceededSnafu {
                scope: BudgetScope::PaidTierDisabled,
                attempted_micro_cents: spend_micro_cents,
                cap_micro_cents: 0_u64,
                remaining_micro_cents: 0_u64,
                resets_at: None,
            }
            .build());
        }
        if self.per_query_cap_micro_cents > 0 && spend_micro_cents > self.per_query_cap_micro_cents
        {
            return Err(BudgetExceededSnafu {
                scope: BudgetScope::PerQuery,
                attempted_micro_cents: spend_micro_cents,
                cap_micro_cents: self.per_query_cap_micro_cents,
                remaining_micro_cents: self.per_query_cap_micro_cents,
                resets_at: None,
            }
            .build());
        }
        if self.per_day_cap_micro_cents > 0 {
            let used = consumer_ledger.paid_in_day_window_micro_cents(now);
            if used.saturating_add(spend_micro_cents) > self.per_day_cap_micro_cents {
                return Err(BudgetExceededSnafu {
                    scope: BudgetScope::PerConsumerDay,
                    attempted_micro_cents: spend_micro_cents,
                    cap_micro_cents: self.per_day_cap_micro_cents,
                    remaining_micro_cents: self.per_day_cap_micro_cents.saturating_sub(used),
                    resets_at: consumer_ledger.day_window_reset_at(now),
                }
                .build());
            }
        }
        if self.per_fleet_day_cap_micro_cents > 0 {
            let used = fleet_ledger.paid_in_day_window_micro_cents(now);
            if used.saturating_add(spend_micro_cents) > self.per_fleet_day_cap_micro_cents {
                return Err(BudgetExceededSnafu {
                    scope: BudgetScope::PerFleetDay,
                    attempted_micro_cents: spend_micro_cents,
                    cap_micro_cents: self.per_fleet_day_cap_micro_cents,
                    remaining_micro_cents: self.per_fleet_day_cap_micro_cents.saturating_sub(used),
                    resets_at: fleet_ledger.day_window_reset_at(now),
                }
                .build());
            }
        }
        if self.per_agent_cap_micro_cents > 0 {
            let used = consumer_ledger.lifetime_paid_micro_cents();
            if used.saturating_add(spend_micro_cents) > self.per_agent_cap_micro_cents {
                return Err(BudgetExceededSnafu {
                    scope: BudgetScope::PerAgentLifetime,
                    attempted_micro_cents: spend_micro_cents,
                    cap_micro_cents: self.per_agent_cap_micro_cents,
                    remaining_micro_cents: self.per_agent_cap_micro_cents.saturating_sub(used),
                    resets_at: None,
                }
                .build());
            }
        }
        consumer_ledger.record(now, spend_micro_cents);
        fleet_ledger.record(now, spend_micro_cents);
        Ok(())
    }
}

impl Default for BudgetConstraint {
    /// Default is [`BudgetConstraint::free_only`] — safest default.
    /// Opt-in to paid spend explicitly via [`BudgetConstraint::phase_zero_default`]
    /// or by constructing the struct directly.
    fn default() -> Self {
        Self::free_only()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cost::ProviderSpend;

    fn ts(s: &str) -> Timestamp {
        s.parse().unwrap()
    }

    fn t0() -> Timestamp {
        ts("2026-07-01T00:00:00Z")
    }

    #[test]
    fn default_is_free_only() {
        let b = BudgetConstraint::default();
        assert!(!b.allow_paid_tier);
        assert_eq!(b.per_query_cap_micro_cents, 0);
    }

    #[test]
    fn free_only_rejects_any_paid_spend() {
        let b = BudgetConstraint::free_only();
        assert!(b.permits(0, &SpendLedger::new(), t0()));
        assert!(!b.permits(1, &SpendLedger::new(), t0()));
    }

    #[test]
    fn phase_zero_default_permits_small_spend() {
        let b = BudgetConstraint::phase_zero_default();
        assert!(b.permits(100_000, &SpendLedger::new(), t0()));
    }

    #[test]
    fn per_query_cap_blocks_large_spend() {
        let b = BudgetConstraint::phase_zero_default();
        // $1.00 single call exceeds the $0.05 per-query cap.
        assert!(!b.permits(10_000_000, &SpendLedger::new(), t0()));
    }

    #[test]
    fn per_day_cap_blocks_within_window() {
        let b = BudgetConstraint::phase_zero_default();
        let mut ledger = SpendLedger::new();
        ledger.record(t0(), b.per_day_cap_micro_cents);
        assert!(!b.permits(1, &ledger, t0()));
        // Still inside the 24h window 23 hours later.
        assert!(!b.permits(1, &ledger, ts("2026-07-01T23:00:00Z")));
    }

    #[test]
    fn per_day_cap_resets_after_window_rolls_over() {
        // The issue-#30 contract: spend up to the day cap, advance past
        // 24h, and the full day cap is available again while the lifetime
        // cap keeps counting.
        let b = BudgetConstraint {
            per_query_cap_micro_cents: 0,
            per_day_cap_micro_cents: 50_000_000,
            per_fleet_day_cap_micro_cents: 0,
            per_agent_cap_micro_cents: 200_000_000,
            allow_paid_tier: true,
        };
        let mut ledger = SpendLedger::new();
        ledger.record(t0(), 50_000_000);

        assert!(!b.permits(1, &ledger, t0()));

        let next_day = ts("2026-07-02T01:00:00Z");
        assert!(b.permits(1, &ledger, next_day));
        assert!(b.permits(50_000_000, &ledger, next_day));
        assert!(!b.permits(50_000_001, &ledger, next_day));
    }

    #[test]
    fn day_window_is_rolling_not_calendar() {
        let b = BudgetConstraint {
            per_query_cap_micro_cents: 0,
            per_day_cap_micro_cents: 50_000_000,
            per_fleet_day_cap_micro_cents: 0,
            per_agent_cap_micro_cents: 0,
            allow_paid_tier: true,
        };
        let mut ledger = SpendLedger::new();
        ledger.record(t0(), 30_000_000);
        ledger.record(ts("2026-07-01T20:00:00Z"), 20_000_000);

        // At T0+23h both events are in the window: 50M spent, cap reached.
        assert!(!b.permits(1, &ledger, ts("2026-07-01T23:00:00Z")));
        // At T0+25h the first event has rolled out: only 20M in window.
        assert!(b.permits(30_000_000, &ledger, ts("2026-07-02T01:00:00Z")));
        assert!(!b.permits(30_000_001, &ledger, ts("2026-07-02T01:00:00Z")));
    }

    #[test]
    fn lifetime_cap_enforced_across_windows() {
        let b = BudgetConstraint {
            per_query_cap_micro_cents: 0,
            per_day_cap_micro_cents: 50_000_000,
            per_fleet_day_cap_micro_cents: 0,
            per_agent_cap_micro_cents: 200_000_000,
            allow_paid_tier: true,
        };
        let mut ledger = SpendLedger::new();
        // Four separate days of $5 spend reach the $20 lifetime cap.
        ledger.record(ts("2026-07-01T00:00:00Z"), 50_000_000);
        ledger.record(ts("2026-07-03T00:00:00Z"), 50_000_000);
        ledger.record(ts("2026-07-05T00:00:00Z"), 50_000_000);
        ledger.record(ts("2026-07-07T00:00:00Z"), 50_000_000);

        // A week later the day window is empty, but lifetime is exhausted.
        let later = ts("2026-07-14T00:00:00Z");
        assert_eq!(ledger.paid_in_day_window_micro_cents(later), 0);
        assert!(!b.permits(1, &ledger, later));
    }

    #[test]
    fn prune_expired_preserves_lifetime_and_window_accounting() {
        let mut ledger = SpendLedger::new();
        ledger.record(t0(), 10_000_000);
        ledger.record(ts("2026-07-02T12:00:00Z"), 5_000_000);

        let now = ts("2026-07-03T00:00:00Z");
        ledger.prune_expired(now);

        assert_eq!(ledger.events().len(), 1);
        assert_eq!(ledger.lifetime_paid_micro_cents(), 15_000_000);
        assert_eq!(ledger.paid_in_day_window_micro_cents(now), 5_000_000);
    }

    #[test]
    fn record_ignores_zero_spend() {
        let mut ledger = SpendLedger::new();
        ledger.record(t0(), 0);
        assert!(ledger.events().is_empty());
        assert_eq!(ledger.lifetime_paid_micro_cents(), 0);
    }

    #[test]
    fn record_cost_folds_call_report() {
        let mut ledger = SpendLedger::new();
        let cost = CostTracking::from_line_items([
            ProviderSpend::new("brave", 300, 0, 1),
            ProviderSpend::new("exa", 700, 0, 1),
        ]);
        ledger.record_cost(t0(), &cost);
        assert_eq!(ledger.lifetime_paid_micro_cents(), 1_000);
        assert_eq!(ledger.paid_in_day_window_micro_cents(t0()), 1_000);
    }

    #[test]
    fn ledger_saturates_instead_of_overflowing() {
        let b = BudgetConstraint {
            per_query_cap_micro_cents: 0,
            per_day_cap_micro_cents: 0,
            per_fleet_day_cap_micro_cents: 0,
            per_agent_cap_micro_cents: 1_000,
            allow_paid_tier: true,
        };
        let mut ledger = SpendLedger::new();
        ledger.record(t0(), u64::MAX - 1);
        ledger.record(t0(), u64::MAX - 1);
        assert_eq!(ledger.lifetime_paid_micro_cents(), u64::MAX);
        assert!(!b.permits(1, &ledger, t0()));
    }

    #[test]
    fn disabled_cap_is_permissive() {
        let b = BudgetConstraint {
            per_query_cap_micro_cents: 0,
            per_day_cap_micro_cents: 0,
            per_fleet_day_cap_micro_cents: 0,
            per_agent_cap_micro_cents: 0,
            allow_paid_tier: true,
        };
        let mut big_ledger = SpendLedger::new();
        big_ledger.record(t0(), u64::MAX / 2);
        assert!(b.permits(1_000_000, &big_ledger, t0()));
    }

    #[test]
    fn allow_paid_tier_false_blocks_any_nonzero() {
        let b = BudgetConstraint {
            per_query_cap_micro_cents: 1_000_000,
            per_day_cap_micro_cents: 0,
            per_fleet_day_cap_micro_cents: 0,
            per_agent_cap_micro_cents: 0,
            allow_paid_tier: false,
        };
        assert!(!b.permits(1, &SpendLedger::new(), t0()));
        assert!(b.permits(0, &SpendLedger::new(), t0()));
    }

    #[test]
    fn budget_serde_round_trip() {
        let b = BudgetConstraint::phase_zero_default();
        let json = serde_json::to_string(&b).unwrap();
        let back: BudgetConstraint = serde_json::from_str(&json).unwrap();
        assert_eq!(back, b);
    }

    #[test]
    fn spend_ledger_serde_round_trip() {
        let mut ledger = SpendLedger::new();
        ledger.record(t0(), 1_234);
        ledger.record(ts("2026-07-01T12:00:00Z"), 5_678);
        let json = serde_json::to_string(&ledger).unwrap();
        let back: SpendLedger = serde_json::from_str(&json).unwrap();
        assert_eq!(back, ledger);
        assert_eq!(back.lifetime_paid_micro_cents(), 6_912);
    }

    #[test]
    fn per_agent_cap_stops_lifetime_growth() {
        let b = BudgetConstraint {
            per_query_cap_micro_cents: 0,
            per_day_cap_micro_cents: 0,
            per_fleet_day_cap_micro_cents: 0,
            per_agent_cap_micro_cents: 1_000,
            allow_paid_tier: true,
        };
        let mut ledger = SpendLedger::new();
        ledger.record(t0(), 999);
        assert!(b.permits(1, &ledger, t0()));
        assert!(!b.permits(2, &ledger, t0()));
    }
}
