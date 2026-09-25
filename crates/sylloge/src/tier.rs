//! Provider tier classification.
//!
//! Every zetesis provider declares which tier it belongs to. Tiers order
//! preference: free Tier 0 first. A paid tier is eligible only when the
//! caller's budget explicitly enables paid spend and a reservation for the
//! attempt succeeds; a Tier 0 miss never enables paid use on its own, and
//! there is no automatic paid fallback. Paid routing stays disabled until
//! durable budget enforcement lands (zetesis#47).

use serde::{Deserialize, Serialize};

/// Tier a provider is classified into for free-first routing.
///
/// `#[non_exhaustive]` — the tier set is expected to grow (Tier 4 for
/// domain-specialist providers has been discussed but not yet locked).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[non_exhaustive]
#[serde(rename_all = "snake_case")]
pub enum ProviderTier {
    /// Free / quality APIs with generous rate limits and no marginal cost
    /// per query (Semantic Scholar, arXiv, `OpenAlex`, Crossref, `PubMed`,
    /// Wikipedia). Always attempted first; most research queries resolve
    /// here.
    Tier0Free,

    /// Paid per-query APIs (for example Brave, Exa, Tavily). Eligible only
    /// under an explicitly paid-enabled, reserved budget; never an automatic
    /// fallback for a Tier 0 miss.
    Tier1Cheap,

    /// Self-hosted orchestration: the local deep-research loop against a
    /// consumer-granted model (Logismos owns inference execution). No
    /// provider spend; bounded by the granted compute and latency.
    Tier2SelfHosted,

    /// Paid deep-research APIs (You.com, Valyu, Perplexity `DeepResearch`).
    /// Reserved for budget-authorized critical queries.
    Tier3PaidDeep,
}

impl ProviderTier {
    /// Stable lowercase identifier suitable for cache keys and telemetry.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Tier0Free => "tier0_free",
            Self::Tier1Cheap => "tier1_cheap",
            Self::Tier2SelfHosted => "tier2_self_hosted",
            Self::Tier3PaidDeep => "tier3_paid_deep",
        }
    }

    /// Whether this tier incurs paid-provider spend (as opposed to free
    /// quotas or self-hosted electricity-only cost).
    #[must_use]
    pub const fn is_paid(self) -> bool {
        matches!(self, Self::Tier1Cheap | Self::Tier3PaidDeep)
    }

    /// Ascending free-first preference order.
    ///
    /// Lower number = tried first. Derived `PartialOrd` on the enum already
    /// produces this order, but the explicit accessor documents the
    /// contract (and reserves room for a future override if tier-priority
    /// ever needs to diverge from variant declaration order).
    #[must_use]
    pub const fn fallback_priority(self) -> u8 {
        match self {
            Self::Tier0Free => 0,
            Self::Tier1Cheap => 1,
            Self::Tier2SelfHosted => 2,
            Self::Tier3PaidDeep => 3,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tier_round_trip() {
        let tiers = [
            ProviderTier::Tier0Free,
            ProviderTier::Tier1Cheap,
            ProviderTier::Tier2SelfHosted,
            ProviderTier::Tier3PaidDeep,
        ];
        for tier in tiers {
            let json = serde_json::to_string(&tier).unwrap();
            let back: ProviderTier = serde_json::from_str(&json).unwrap();
            assert_eq!(back, tier);
            assert_eq!(json.trim_matches('"'), tier.as_str());
        }
    }

    #[test]
    fn ordering_is_free_first() {
        assert!(ProviderTier::Tier0Free < ProviderTier::Tier1Cheap);
        assert!(ProviderTier::Tier1Cheap < ProviderTier::Tier2SelfHosted);
        assert!(ProviderTier::Tier2SelfHosted < ProviderTier::Tier3PaidDeep);
    }

    #[test]
    fn fallback_priority_matches_ordering() {
        let mut tiers = [
            ProviderTier::Tier3PaidDeep,
            ProviderTier::Tier0Free,
            ProviderTier::Tier2SelfHosted,
            ProviderTier::Tier1Cheap,
        ];
        tiers.sort_by_key(|t| t.fallback_priority());
        assert_eq!(
            tiers,
            [
                ProviderTier::Tier0Free,
                ProviderTier::Tier1Cheap,
                ProviderTier::Tier2SelfHosted,
                ProviderTier::Tier3PaidDeep,
            ]
        );
    }

    #[test]
    fn is_paid_classification() {
        assert!(!ProviderTier::Tier0Free.is_paid());
        assert!(ProviderTier::Tier1Cheap.is_paid());
        assert!(!ProviderTier::Tier2SelfHosted.is_paid());
        assert!(ProviderTier::Tier3PaidDeep.is_paid());
    }
}
