#![doc = "Facade crate for the zetesis sovereign research substrate."]
#![deny(missing_docs)]
#![forbid(unsafe_code)]

pub use elenkhos as steelman;
pub use sylloge::{
    BoxFut, BudgetConstraint, BudgetExceededSnafu, BudgetScope, Citation, CostTracking, Crawler,
    DAY_WINDOW, DeepDepth, DeepResearch, DomainDeniedSnafu, Error, ErrorClass,
    FatalCorruptionSnafu, FreshnessBasis, FreshnessDecision, FreshnessPolicy, InvalidQuerySnafu,
    LocalDeepResearch, MissingCitationsSnafu, OfflineFixture, OversizedPayloadSnafu, PageContent,
    PermanentIoSnafu, ProvenanceEntry, Provider, ProviderFailureSnafu, ProviderId, ProviderSpend,
    ProviderTier, PublicationPrecision, PublicationProvenance, PublicationTime,
    PublicationTimeCapability, QueryGenerator, QueryShape, QuotaExhaustedSnafu, RateLimitedSnafu,
    ResearchResult, ResearchStatus, Resolver, Result, ResultHit, SearchConstraints, SourceKind,
    SourceRetriever, SpendEvent, SpendLedger, Synthesizer, SystemResolver, TaskId,
    TaskNotReadySnafu, TaskUnavailableSnafu, TimeoutSnafu, TransientIoSnafu, UnauthorizedSnafu,
    UnsafeTargetSnafu, UnsupportedSnafu, ValidatedTarget, evaluate_freshness,
};
pub use synopsis as briefing;

#[cfg(test)]
mod tests {
    #![expect(clippy::unwrap_used, reason = "test assertions must fail loudly")]

    use url::Url;

    use super::*;

    #[test]
    fn facade_re_exports_compose_end_to_end() {
        // WHY: the facade is the surface downstream consumers import; a
        // dropped re-export is a silent breaking change this test catches.
        let budget = BudgetConstraint::free_only();
        let ledger = SpendLedger::new();
        let now: jiff::Timestamp = "2026-07-01T00:00:00Z".parse().unwrap();
        assert!(budget.permits(0, &ledger, now));
        assert!(!budget.permits(1, &ledger, now));

        let constraints = SearchConstraints::new(5, budget);
        // WHY: an IP-literal target needs no DNS resolution, keeping this
        // facade smoke test independent of live network access.
        assert!(
            constraints
                .check_url(&Url::parse("http://8.8.8.8/").unwrap())
                .is_ok()
        );
    }

    #[test]
    fn network_target_policy_types_are_reachable_through_facade() {
        // WHY: `check_url`'s Resolver/SystemResolver/ValidatedTarget/
        // UnsafeTargetSnafu surface (zetesis#48) is easy to add to
        // sylloge and forget to add here -- exercise each through the
        // facade path rather than only asserting the `pub use` compiles.
        // WHY: an IP-literal target needs no DNS resolution, keeping this
        // reachability check independent of live network access.
        let target = SearchConstraints::default()
            .check_url_with(&Url::parse("http://8.8.8.8/").unwrap(), &SystemResolver)
            .unwrap();
        let _: ValidatedTarget = target;

        let e: Error = UnsafeTargetSnafu {
            url: "http://127.0.0.1/".to_owned(),
            reason: "re-export check".to_owned(),
        }
        .build();
        assert!(e.is_permanent());
        assert!(e.to_string().contains("re-export check"));
    }

    #[test]
    fn crate_boundary_aliases_are_reachable() {
        assert_eq!(
            format!("{:?}", steelman::Elenkhos),
            "Elenkhos",
            "steelman alias must expose the elenkhos boundary marker"
        );
        assert_eq!(
            format!("{:?}", briefing::Synopsis),
            "Synopsis",
            "briefing alias must expose the synopsis boundary marker"
        );
    }
}
