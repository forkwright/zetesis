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
    ResearchResult, ResearchStatus, Result, ResultHit, SearchConstraints, SourceKind,
    SourceRetriever, SpendEvent, SpendLedger, Synthesizer, TaskId, TaskNotReadySnafu,
    TaskUnavailableSnafu, TimeoutSnafu, TransientIoSnafu, UnauthorizedSnafu, UnsupportedSnafu,
    evaluate_freshness,
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
        assert!(constraints.permits_url(&Url::parse("https://example.org/").unwrap()));
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
