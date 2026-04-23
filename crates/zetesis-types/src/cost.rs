//! Per-call cost tracking.
//!
//! After a provider resolves a query, it reports a [`CostTracking`] record
//! alongside the result. This captures the paid spend (if any) plus the
//! free-tier quota units consumed (so the budget layer can throttle before
//! hitting provider-side rate limits).
//!
//! Spend is tracked in USD cents as `u64` rather than floating point to
//! avoid rounding drift across aggregation. A `u64` of cents covers every
//! realistic fleet budget.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Per-provider line item inside [`CostTracking`].
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[non_exhaustive]
pub struct ProviderSpend {
    /// Provider identifier matching `Provider::name()`. Stable, lowercase,
    /// kebab-or-underscore-separated (e.g. `semantic_scholar`).
    pub provider_id: String,

    /// Paid spend in USD micro-cents (one-hundred-thousandths of a cent, so
    /// 1 USD = 10_000_000 micro-cents). Micro-cents give per-request
    /// resolution for providers that bill below one cent per query.
    pub paid_micro_cents: u64,

    /// Free-tier quota units consumed (e.g. "1 request against the
    /// 100-per-5-min Semantic Scholar quota"). The meaning of a unit is
    /// provider-specific; the budget layer compares against the provider's
    /// declared quota size.
    pub free_tier_units: u64,

    /// Number of requests that made up this line item (useful for
    /// aggregation when a single query fans out to several provider calls).
    pub request_count: u32,
}

impl ProviderSpend {
    /// Construct a line item.
    #[must_use]
    pub fn new(
        provider_id: impl Into<String>,
        paid_micro_cents: u64,
        free_tier_units: u64,
        request_count: u32,
    ) -> Self {
        Self {
            provider_id: provider_id.into(),
            paid_micro_cents,
            free_tier_units,
            request_count,
        }
    }

    /// Whether this line item contributed paid spend.
    #[must_use]
    pub const fn is_paid(&self) -> bool {
        self.paid_micro_cents > 0
    }
}

/// Aggregated cost ledger for a single research call.
///
/// One per-provider entry covers each provider the router attempted
/// (including failed attempts — a failed Tier 0 call that fell through to
/// Tier 1 appears as two entries). The keys in `by_provider` are sorted
/// (BTreeMap) so serialization is deterministic across ledger writes.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[non_exhaustive]
pub struct CostTracking {
    /// Per-provider breakdown keyed by `ProviderSpend::provider_id`.
    pub by_provider: BTreeMap<String, ProviderSpend>,
}

impl CostTracking {
    /// Build from an iterator of line items. Entries with the same
    /// `provider_id` are summed.
    #[must_use]
    pub fn from_line_items<I: IntoIterator<Item = ProviderSpend>>(items: I) -> Self {
        let mut by_provider: BTreeMap<String, ProviderSpend> = BTreeMap::new();
        for item in items {
            by_provider
                .entry(item.provider_id.clone())
                .and_modify(|existing| {
                    existing.paid_micro_cents = existing
                        .paid_micro_cents
                        .saturating_add(item.paid_micro_cents);
                    existing.free_tier_units = existing
                        .free_tier_units
                        .saturating_add(item.free_tier_units);
                    existing.request_count =
                        existing.request_count.saturating_add(item.request_count);
                })
                .or_insert(item);
        }
        Self { by_provider }
    }

    /// Record (or accumulate) a line item in-place.
    pub fn add(&mut self, item: ProviderSpend) {
        self.by_provider
            .entry(item.provider_id.clone())
            .and_modify(|existing| {
                existing.paid_micro_cents = existing
                    .paid_micro_cents
                    .saturating_add(item.paid_micro_cents);
                existing.free_tier_units = existing
                    .free_tier_units
                    .saturating_add(item.free_tier_units);
                existing.request_count = existing.request_count.saturating_add(item.request_count);
            })
            .or_insert(item);
    }

    /// Total paid spend in micro-cents across every provider.
    #[must_use]
    pub fn total_paid_micro_cents(&self) -> u64 {
        self.by_provider
            .values()
            .map(|s| s.paid_micro_cents)
            .fold(0_u64, u64::saturating_add)
    }

    /// Total paid spend in whole-USD as `f64`. Reserved for display / log
    /// output; accounting should operate on the integer micro-cents field.
    #[must_use]
    pub fn total_paid_usd(&self) -> f64 {
        // 1 USD = 10_000_000 micro-cents.
        (self.total_paid_micro_cents() as f64) / 10_000_000.0
    }

    /// Total request count across every provider.
    #[must_use]
    pub fn total_requests(&self) -> u64 {
        self.by_provider
            .values()
            .map(|s| u64::from(s.request_count))
            .fold(0_u64, u64::saturating_add)
    }

    /// Whether any provider recorded paid spend.
    #[must_use]
    pub fn any_paid(&self) -> bool {
        self.by_provider.values().any(ProviderSpend::is_paid)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_line_items_sums_duplicates() {
        let a = ProviderSpend::new("brave", 500, 1, 1);
        let b = ProviderSpend::new("brave", 300, 1, 2);
        let c = ProviderSpend::new("exa", 1_000, 0, 1);
        let ct = CostTracking::from_line_items([a, b, c]);

        let brave = ct.by_provider.get("brave").unwrap();
        assert_eq!(brave.paid_micro_cents, 800);
        assert_eq!(brave.free_tier_units, 2);
        assert_eq!(brave.request_count, 3);

        let exa = ct.by_provider.get("exa").unwrap();
        assert_eq!(exa.paid_micro_cents, 1_000);
    }

    #[test]
    fn add_accumulates() {
        let mut ct = CostTracking::default();
        ct.add(ProviderSpend::new("semantic_scholar", 0, 1, 1));
        ct.add(ProviderSpend::new("semantic_scholar", 0, 1, 1));
        let row = ct.by_provider.get("semantic_scholar").unwrap();
        assert_eq!(row.free_tier_units, 2);
        assert_eq!(row.request_count, 2);
    }

    #[test]
    fn totals_match_sum() {
        let ct = CostTracking::from_line_items([
            ProviderSpend::new("a", 250, 0, 1),
            ProviderSpend::new("b", 750, 0, 2),
            ProviderSpend::new("c", 0, 5, 3),
        ]);
        assert_eq!(ct.total_paid_micro_cents(), 1_000);
        assert_eq!(ct.total_requests(), 6);
        assert!(ct.any_paid());
    }

    #[test]
    fn any_paid_false_for_free_only() {
        let ct = CostTracking::from_line_items([
            ProviderSpend::new("semantic_scholar", 0, 1, 1),
            ProviderSpend::new("arxiv", 0, 1, 1),
        ]);
        assert!(!ct.any_paid());
        assert_eq!(ct.total_paid_micro_cents(), 0);
    }

    #[test]
    fn saturating_add_caps_at_u64_max() {
        let mut ct = CostTracking::default();
        ct.add(ProviderSpend::new("wild", u64::MAX - 1, 0, 0));
        ct.add(ProviderSpend::new("wild", 10, 0, 0));
        assert_eq!(ct.by_provider["wild"].paid_micro_cents, u64::MAX);
    }

    #[test]
    fn cost_tracking_serde_json_round_trip() {
        let ct = CostTracking::from_line_items([ProviderSpend::new("brave", 500, 0, 1)]);
        let json = serde_json::to_string(&ct).unwrap();
        let back: CostTracking = serde_json::from_str(&json).unwrap();
        assert_eq!(back, ct);
    }

    #[test]
    fn provider_spend_is_paid_true_for_nonzero() {
        assert!(ProviderSpend::new("x", 1, 0, 0).is_paid());
        assert!(!ProviderSpend::new("x", 0, 100, 0).is_paid());
    }

    #[test]
    fn default_is_empty() {
        let ct = CostTracking::default();
        assert!(ct.by_provider.is_empty());
        assert_eq!(ct.total_paid_micro_cents(), 0);
        assert!(!ct.any_paid());
    }
}
