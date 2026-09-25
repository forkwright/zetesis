#![doc = "Facade crate for the zetesis sovereign research substrate."]
#![deny(missing_docs)]
#![forbid(unsafe_code)]

pub use elenkhos as steelman;
pub use sylloge::{
    AcquisitionFailure, AcquisitionLimits, BoxFut, BudgetConstraint, BudgetExceededSnafu,
    BudgetScope, Citation, ConnectAttempt, ConnectDeniedSnafu, ConnectError, ConnectIoSnafu,
    ConnectOutcome, ConnectTimedOutSnafu, ConnectedStream, Connector, CostTracking, DAY_WINDOW,
    DeepDepth, DeepResearch, DirectConnector, DomainDeniedSnafu, DowngradePolicy, Error,
    ErrorClass, FatalCorruptionSnafu, FreshnessBasis, FreshnessDecision, FreshnessPolicy,
    HopRecord, InvalidConstraintSnafu, InvalidQuerySnafu, LocalDeepResearch,
    LocalTargetAuthorization, MissingCitationsSnafu, OfflineFixture, OversizedPayloadSnafu,
    PermanentIoSnafu, ProvenanceEntry, Provider, ProviderFailureSnafu, ProviderId, ProviderSpend,
    ProviderTier, PublicationPrecision, PublicationProvenance, PublicationTime,
    PublicationTimeCapability, QueryGenerator, QueryShape, QuotaExhaustedSnafu, RateLimitedSnafu,
    ResearchResult, ResearchStatus, Resolver, ResponseRecord, Result, ResultHit, SchemePolicy,
    SearchConstraints, SourceKind, SourceRetriever, SpendEvent, SpendLedger, StaticAcquirer,
    StaticAcquirerBuilder, Synthesizer, SystemResolver, TaskId, TaskNotReadySnafu,
    TaskUnavailableSnafu, TimeoutSnafu, TlsRecord, Transfer, TransferOutcome, TransientIoSnafu,
    TrustAnchors, UnauthorizedSnafu, UnsafeTargetSnafu, UnsupportedSnafu, ValidatedTarget,
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

    #[tokio::test]
    async fn static_acquisition_types_are_reachable_through_facade() {
        // WHY: the acquisition surface is new in sylloge; a dropped facade
        // re-export would break consumers silently. Each type is exercised
        // through the facade path, not only named in a `pub use`.
        use std::sync::Arc;
        use std::time::Duration;

        let limits = AcquisitionLimits::new(
            SchemePolicy::HttpAndHttps,
            3,
            Duration::from_secs(1),
            Duration::from_secs(5),
            1024,
        )
        .unwrap()
        .with_downgrade(DowngradePolicy::Refuse);
        let anchor = rcgen::generate_simple_self_signed(vec!["facade.example".to_owned()]).unwrap();
        let trust = TrustAnchors::from_der([anchor.cert.der().as_ref()]).unwrap();
        let connector: Arc<dyn Connector> = Arc::new(DirectConnector);
        let resolver: Arc<dyn Resolver + Send + Sync> = Arc::new(SystemResolver);
        let builder: StaticAcquirerBuilder = StaticAcquirer::builder(limits, trust);
        let acquirer = builder
            .connector(connector)
            .resolver(resolver)
            .user_agent("zetesis-facade-test")
            .build()
            .unwrap();

        // WHY: a loopback IP literal is refused by policy before any
        // resolution or socket, so this needs no network access.
        let transfer: Transfer = acquirer
            .acquire(
                &Url::parse("http://127.0.0.1/").unwrap(),
                &SearchConstraints::default(),
                None,
            )
            .await
            .unwrap();
        let hops: &[HopRecord] = transfer.hops();
        let attempts: &[ConnectAttempt] = hops[0].connect_attempts();
        assert!(attempts.is_empty(), "a refused target is never dialed");
        let tls: Option<&TlsRecord> = hops[0].tls();
        assert!(tls.is_none(), "no handshake happened");
        let response: Option<&ResponseRecord> = transfer.response();
        assert!(response.is_none(), "a refused transfer has no response");
        let TransferOutcome::Failed { failure } = transfer.outcome() else {
            panic!("loopback without authority must fail");
        };
        let failure: &AcquisitionFailure = failure;
        assert_eq!(failure.kind(), "unsafe_target", "policy refusal kind");
        assert_eq!(
            failure.class(),
            ErrorClass::Permanent,
            "policy never clears"
        );

        let denied: ConnectError = ConnectDeniedSnafu { reason: "facade" }.build();
        let timed_out: ConnectError = ConnectTimedOutSnafu.build();
        let io: ConnectError = snafu::IntoError::into_error(
            ConnectIoSnafu,
            std::io::Error::from(std::io::ErrorKind::ConnectionRefused),
        );
        for err in [denied, timed_out, io] {
            assert!(!err.to_string().is_empty(), "connect errors render");
        }
        let outcome = ConnectOutcome::Denied;
        assert_ne!(outcome, ConnectOutcome::Connected, "outcomes are distinct");
        let no_stream: Option<Box<dyn ConnectedStream>> = None;
        assert!(
            no_stream.is_none(),
            "ConnectedStream is nameable as a trait object"
        );
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
