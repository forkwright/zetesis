//! Pre-call budget constraints.
//!
//! A [`BudgetConstraint`] is supplied by the caller on every research
//! request. `zetesis-budget` (future phase) checks the current ledger
//! against the constraints and short-circuits the router if any ceiling
//! would be exceeded. The caller's intent is captured once at call time;
//! the router does not silently raise limits.

use serde::{Deserialize, Serialize};

use crate::cost::CostTracking;

/// Per-call budget ceiling.
///
/// All caps are in USD micro-cents (see [`crate::ProviderSpend`] for the
/// unit rationale). A value of `0` for a cap means "cap disabled", not
/// "cap of zero cents" — the latter would reject every paid call, which
/// is not the common intent. Callers who want a true zero-spend budget
/// should set `allow_paid_tier = false` instead.
///
/// The cap hierarchy:
/// - [`BudgetConstraint::per_query_cap_micro_cents`] — single-call ceiling.
///   Violations reject the call before it reaches any paid provider.
/// - [`BudgetConstraint::per_day_cap_micro_cents`] — rolling-24-hour
///   ceiling for the calling agent. Enforced against `zetesis-budget`'s
///   persisted ledger.
/// - [`BudgetConstraint::per_agent_cap_micro_cents`] — lifetime cap for
///   the calling agent (typically set per-deployment, not per-call).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct BudgetConstraint {
    /// Max paid spend this single call may incur, in micro-cents.
    /// `0` = no per-query cap (subject to the per-day / per-agent caps).
    pub per_query_cap_micro_cents: u64,

    /// Max paid spend the calling agent may incur in a rolling 24-hour
    /// window, in micro-cents. `0` = no per-day cap.
    pub per_day_cap_micro_cents: u64,

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
            per_agent_cap_micro_cents: 0,
            allow_paid_tier: false,
        }
    }

    /// Default-ish permissive budget: paid tier allowed with a $0.05 per-query
    /// cap, $5/day soft cap, $20 lifetime cap. Matches the Phase 0 initial
    /// proposal in `projects/zetesis/phases/00-spec/PLAN.md` (REQ-00-04).
    #[must_use]
    pub const fn phase_zero_default() -> Self {
        Self {
            // $0.05 = 500_000 micro-cents
            per_query_cap_micro_cents: 500_000,
            // $5.00 = 50_000_000 micro-cents
            per_day_cap_micro_cents: 50_000_000,
            // $20.00 = 200_000_000 micro-cents
            per_agent_cap_micro_cents: 200_000_000,
            allow_paid_tier: true,
        }
    }

    /// Whether this budget would permit a paid call of `spend_micro_cents`
    /// against a ledger currently showing `ledger` of recorded spend.
    ///
    /// Only paid spend counts toward the caps; free-tier units are
    /// tracked separately for rate-limit enforcement. A cap of `0` is
    /// treated as "disabled".
    #[must_use]
    pub fn permits(&self, spend_micro_cents: u64, ledger: &CostTracking) -> bool {
        if !self.allow_paid_tier && spend_micro_cents > 0 {
            return false;
        }
        let total = ledger.total_paid_micro_cents();
        if self.per_query_cap_micro_cents > 0 && spend_micro_cents > self.per_query_cap_micro_cents
        {
            return false;
        }
        if self.per_day_cap_micro_cents > 0
            && total.saturating_add(spend_micro_cents) > self.per_day_cap_micro_cents
        {
            return false;
        }
        if self.per_agent_cap_micro_cents > 0
            && total.saturating_add(spend_micro_cents) > self.per_agent_cap_micro_cents
        {
            return false;
        }
        true
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

    #[test]
    fn default_is_free_only() {
        let b = BudgetConstraint::default();
        assert!(!b.allow_paid_tier);
        assert_eq!(b.per_query_cap_micro_cents, 0);
    }

    #[test]
    fn free_only_rejects_any_paid_spend() {
        let b = BudgetConstraint::free_only();
        assert!(b.permits(0, &CostTracking::default()));
        assert!(!b.permits(1, &CostTracking::default()));
    }

    #[test]
    fn phase_zero_default_permits_small_spend() {
        let b = BudgetConstraint::phase_zero_default();
        assert!(b.permits(100_000, &CostTracking::default()));
    }

    #[test]
    fn per_query_cap_blocks_large_spend() {
        let b = BudgetConstraint::phase_zero_default();
        // $1.00 single call exceeds the $0.05 per-query cap.
        assert!(!b.permits(10_000_000, &CostTracking::default()));
    }

    #[test]
    fn per_day_cap_blocks_when_ledger_full() {
        let b = BudgetConstraint::phase_zero_default();
        let ledger = CostTracking::from_line_items([ProviderSpend::new(
            "brave",
            b.per_day_cap_micro_cents,
            0,
            1,
        )]);
        assert!(!b.permits(1, &ledger));
    }

    #[test]
    fn disabled_cap_is_permissive() {
        let b = BudgetConstraint {
            per_query_cap_micro_cents: 0,
            per_day_cap_micro_cents: 0,
            per_agent_cap_micro_cents: 0,
            allow_paid_tier: true,
        };
        let big_ledger =
            CostTracking::from_line_items([ProviderSpend::new("exa", u64::MAX / 2, 0, 1)]);
        assert!(b.permits(1_000_000, &big_ledger));
    }

    #[test]
    fn allow_paid_tier_false_blocks_any_nonzero() {
        let b = BudgetConstraint {
            per_query_cap_micro_cents: 1_000_000,
            per_day_cap_micro_cents: 0,
            per_agent_cap_micro_cents: 0,
            allow_paid_tier: false,
        };
        assert!(!b.permits(1, &CostTracking::default()));
        assert!(b.permits(0, &CostTracking::default()));
    }

    #[test]
    fn budget_serde_round_trip() {
        let b = BudgetConstraint::phase_zero_default();
        let json = serde_json::to_string(&b).unwrap();
        let back: BudgetConstraint = serde_json::from_str(&json).unwrap();
        assert_eq!(back, b);
    }

    #[test]
    fn per_agent_cap_stops_lifetime_growth() {
        let b = BudgetConstraint {
            per_query_cap_micro_cents: 0,
            per_day_cap_micro_cents: 0,
            per_agent_cap_micro_cents: 1_000,
            allow_paid_tier: true,
        };
        let ledger = CostTracking::from_line_items([ProviderSpend::new("x", 999, 0, 1)]);
        assert!(b.permits(1, &ledger));
        assert!(!b.permits(2, &ledger));
    }
}
