//! Publication-time typing and centralized freshness enforcement.
//!
//! Retrieval freshness (when zetesis fetched a document,
//! [`crate::Citation::accessed_at`]) and content freshness (when the
//! document was actually published or last updated) are different facts.
//! Collapsing them lets stale content that merely happens to have been
//! retrieved a moment ago pass as recent. This module keeps the two facts
//! separate: [`PublicationTime`] carries the content-side fact, with an
//! explicit [`PublicationTime::Unknown`] state for providers that don't
//! report one, and [`evaluate_freshness`] is the one place a
//! [`crate::SearchConstraints::freshness_window`] is actually enforced,
//! rather than left to each provider's own convention.

use jiff::{SignedDuration, Timestamp};
use serde::{Deserialize, Serialize};

/// How precisely a [`PublicationTime::Known`] timestamp is known.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
#[serde(rename_all = "snake_case")]
pub enum PublicationPrecision {
    /// The provider reported a full date-and-time.
    Exact,
    /// The provider reported only a calendar date; `at` is midnight UTC of
    /// that date.
    DateOnly,
}

/// Where a [`PublicationTime::Known`] value came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
#[serde(rename_all = "snake_case")]
pub enum PublicationProvenance {
    /// The provider's own structured metadata declared this as the
    /// publication or last-updated time (an API field, an HTTP
    /// `Last-Modified` header).
    ProviderDeclared,
    /// Zetesis inferred this from page content (a parsed dateline or
    /// `<time>` element) rather than structured provider metadata. Lower
    /// trust than [`PublicationProvenance::ProviderDeclared`].
    Inferred,
}

/// The publication or last-updated time of the material a
/// [`crate::Citation`] points at -- distinct from
/// [`crate::Citation::accessed_at`], which is only ever the retrieval
/// time.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum PublicationTime {
    /// The provider (or an inference step) supplied a publication/update
    /// time.
    Known {
        /// The publication or last-updated instant.
        at: Timestamp,
        /// How precisely `at` is known.
        precision: PublicationPrecision,
        /// Where this value came from.
        provenance: PublicationProvenance,
    },
    /// No publication/update time is available for this citation.
    Unknown,
}

impl Default for PublicationTime {
    /// Absence of information is the honest default -- never silently
    /// widen [`crate::Citation::accessed_at`] into a publication-time
    /// claim.
    fn default() -> Self {
        Self::Unknown
    }
}

/// Whether a citation with [`PublicationTime::Unknown`] is rejected or
/// allowed to fall back to [`crate::Citation::accessed_at`] when a
/// [`crate::SearchConstraints::freshness_window`] is configured.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
#[serde(rename_all = "snake_case")]
pub enum FreshnessPolicy {
    /// A citation with [`PublicationTime::Unknown`] is rejected outright
    /// -- retrieval time never substitutes for an unproven content-freshness
    /// claim. The safe default.
    Strict,
    /// A citation with [`PublicationTime::Unknown`] falls back to
    /// [`crate::Citation::accessed_at`]. Explicit opt-in: this can accept
    /// stale content that merely happens to have been retrieved recently.
    Permissive,
}

impl Default for FreshnessPolicy {
    fn default() -> Self {
        Self::Strict
    }
}

/// Which timestamp actually drove a [`FreshnessDecision`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
#[serde(rename_all = "snake_case")]
pub enum FreshnessBasis {
    /// No freshness window was configured; the citation passes without
    /// consulting a timestamp.
    NoWindowConfigured,
    /// The citation's typed [`PublicationTime::Known`] value drove the
    /// decision.
    PublicationTime,
    /// The citation had [`PublicationTime::Unknown`] and
    /// [`FreshnessPolicy::Permissive`] fell back to
    /// [`crate::Citation::accessed_at`] (retrieval time, not content
    /// freshness).
    AccessedAtFallback,
    /// The citation had [`PublicationTime::Unknown`] and
    /// [`FreshnessPolicy::Strict`] rejected it rather than fall back to
    /// retrieval time.
    UnknownRejected,
}

/// The outcome of applying a [`crate::SearchConstraints`] freshness window
/// and policy to one [`crate::Citation`] -- the "applied policy" receipt
/// callers can attach to a [`crate::ResultHit`] via
/// [`crate::ResultHit::with_freshness`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct FreshnessDecision {
    /// Whether the citation passes the configured freshness window.
    pub accepted: bool,
    /// The policy that was applied.
    pub policy: FreshnessPolicy,
    /// Which timestamp drove the decision.
    pub basis: FreshnessBasis,
}

/// Declares whether a [`crate::Provider`] can supply
/// [`PublicationTime::Known`] values, and at what best-case precision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum PublicationTimeCapability {
    /// This provider never supplies a publication/update time; every
    /// citation from it carries [`PublicationTime::Unknown`].
    Unsupported,
    /// This provider supplies a publication/update time for some or all
    /// hits, at best-case `precision`.
    Supported {
        /// Best-case precision this provider can report.
        precision: PublicationPrecision,
    },
}

/// Evaluate `published_at`/`accessed_at` against `window` and `policy` as
/// of `now`. Pure function; the one place freshness is actually enforced,
/// rather than an interpretation each provider is trusted to apply itself.
///
/// WHY(zetesis#50): a citation whose only timestamp is `accessed_at`
/// (retrieval time) can be arbitrarily old content that was merely fetched
/// a moment ago. This function refuses to let `accessed_at` stand in for
/// [`PublicationTime::Known`] and, under [`FreshnessPolicy::Strict`],
/// refuses to let it stand in for [`PublicationTime::Unknown`] either.
#[must_use]
pub fn evaluate_freshness(
    published_at: &PublicationTime,
    accessed_at: Timestamp,
    window: std::time::Duration,
    policy: FreshnessPolicy,
    now: Timestamp,
) -> FreshnessDecision {
    // WHY: saturate at Timestamp::MIN instead of erroring -- a window that
    // reaches past the representable range simply includes every event,
    // which is the conservative (freshness-limiting) direction. Mirrors
    // `budget::day_window_start`.
    let window = SignedDuration::try_from(window).unwrap_or(SignedDuration::MAX);
    let cutoff = now.checked_sub(window).unwrap_or(Timestamp::MIN);

    match published_at {
        PublicationTime::Known { at, .. } => FreshnessDecision {
            accepted: *at > cutoff,
            policy,
            basis: FreshnessBasis::PublicationTime,
        },
        PublicationTime::Unknown => match policy {
            FreshnessPolicy::Permissive => FreshnessDecision {
                accepted: accessed_at > cutoff,
                policy,
                basis: FreshnessBasis::AccessedAtFallback,
            },
            FreshnessPolicy::Strict => FreshnessDecision {
                accepted: false,
                policy,
                basis: FreshnessBasis::UnknownRejected,
            },
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ts(s: &str) -> Timestamp {
        s.parse().unwrap()
    }

    fn now() -> Timestamp {
        ts("2026-08-01T00:00:00Z")
    }

    fn day() -> std::time::Duration {
        std::time::Duration::from_secs(86_400)
    }

    fn known(at: Timestamp) -> PublicationTime {
        PublicationTime::Known {
            at,
            precision: PublicationPrecision::Exact,
            provenance: PublicationProvenance::ProviderDeclared,
        }
    }

    #[test]
    fn publication_time_default_is_unknown() {
        assert_eq!(PublicationTime::default(), PublicationTime::Unknown);
    }

    #[test]
    fn freshness_policy_defaults_to_strict() {
        assert_eq!(FreshnessPolicy::default(), FreshnessPolicy::Strict);
    }

    #[test]
    fn old_publication_new_retrieval_is_rejected() {
        // The core zetesis#50 regression: a document published a year ago
        // but retrieved moments ago must NOT pass a freshness window on
        // the strength of the retrieval timestamp alone.
        let published = known(ts("2025-01-01T00:00:00Z"));
        let accessed_now = now();
        let decision = evaluate_freshness(
            &published,
            accessed_now,
            day(),
            FreshnessPolicy::Permissive,
            now(),
        );
        assert!(!decision.accepted);
        assert_eq!(decision.basis, FreshnessBasis::PublicationTime);
    }

    #[test]
    fn recent_publication_is_accepted() {
        let published = known(ts("2026-07-31T12:00:00Z"));
        let decision = evaluate_freshness(
            &published,
            ts("2020-01-01T00:00:00Z"), // stale accessed_at must not matter
            day(),
            FreshnessPolicy::Strict,
            now(),
        );
        assert!(decision.accepted);
        assert_eq!(decision.basis, FreshnessBasis::PublicationTime);
    }

    #[test]
    fn unknown_time_strict_rejects_without_consulting_accessed_at() {
        let decision = evaluate_freshness(
            &PublicationTime::Unknown,
            now(),
            day(),
            FreshnessPolicy::Strict,
            now(),
        );
        assert!(!decision.accepted);
        assert_eq!(decision.basis, FreshnessBasis::UnknownRejected);
    }

    #[test]
    fn unknown_time_permissive_falls_back_to_accessed_at() {
        let recent = evaluate_freshness(
            &PublicationTime::Unknown,
            now(),
            day(),
            FreshnessPolicy::Permissive,
            now(),
        );
        assert!(recent.accepted);
        assert_eq!(recent.basis, FreshnessBasis::AccessedAtFallback);

        let stale = evaluate_freshness(
            &PublicationTime::Unknown,
            ts("2020-01-01T00:00:00Z"),
            day(),
            FreshnessPolicy::Permissive,
            now(),
        );
        assert!(!stale.accepted);
        assert_eq!(stale.basis, FreshnessBasis::AccessedAtFallback);
    }

    #[test]
    fn date_only_precision_still_compares_correctly() {
        // A date-only publication time is midnight UTC of that date; a
        // two-day window must accept yesterday's date-only timestamp and
        // reject the day before.
        let two_days = std::time::Duration::from_secs(2 * 86_400);
        let yesterday = PublicationTime::Known {
            at: ts("2026-07-31T00:00:00Z"),
            precision: PublicationPrecision::DateOnly,
            provenance: PublicationProvenance::Inferred,
        };
        assert!(
            evaluate_freshness(&yesterday, now(), two_days, FreshnessPolicy::Strict, now())
                .accepted
        );

        let two_days_ago = PublicationTime::Known {
            at: ts("2026-07-30T00:00:00Z"),
            precision: PublicationPrecision::DateOnly,
            provenance: PublicationProvenance::Inferred,
        };
        assert!(
            !evaluate_freshness(
                &two_days_ago,
                now(),
                two_days,
                FreshnessPolicy::Strict,
                now()
            )
            .accepted
        );
    }

    #[test]
    fn cutoff_boundary_is_exclusive() {
        // A publication exactly `window` old is NOT strictly newer than
        // the cutoff and must be rejected -- mirrors the rolling-window
        // convention in `budget::SpendLedger::paid_since_micro_cents`.
        let cutoff_exact = known(ts("2026-07-31T00:00:00Z"));
        let decision =
            evaluate_freshness(&cutoff_exact, now(), day(), FreshnessPolicy::Strict, now());
        assert!(!decision.accepted);
    }

    #[test]
    fn publication_time_serde_round_trip() {
        let known = known(ts("2026-04-22T00:00:00Z"));
        let json = serde_json::to_string(&known).unwrap();
        let back: PublicationTime = serde_json::from_str(&json).unwrap();
        assert_eq!(back, known);

        let json = serde_json::to_string(&PublicationTime::Unknown).unwrap();
        let back: PublicationTime = serde_json::from_str(&json).unwrap();
        assert_eq!(back, PublicationTime::Unknown);
    }

    #[test]
    fn freshness_decision_serde_round_trip() {
        let decision = FreshnessDecision {
            accepted: true,
            policy: FreshnessPolicy::Permissive,
            basis: FreshnessBasis::AccessedAtFallback,
        };
        let json = serde_json::to_string(&decision).unwrap();
        let back: FreshnessDecision = serde_json::from_str(&json).unwrap();
        assert_eq!(back, decision);
    }

    #[test]
    fn publication_time_capability_serde_round_trip() {
        let supported = PublicationTimeCapability::Supported {
            precision: PublicationPrecision::DateOnly,
        };
        let json = serde_json::to_string(&supported).unwrap();
        let back: PublicationTimeCapability = serde_json::from_str(&json).unwrap();
        assert_eq!(back, supported);

        let json = serde_json::to_string(&PublicationTimeCapability::Unsupported).unwrap();
        let back: PublicationTimeCapability = serde_json::from_str(&json).unwrap();
        assert_eq!(back, PublicationTimeCapability::Unsupported);
    }
}
