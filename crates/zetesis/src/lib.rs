#![doc = "Facade crate for the zetesis sovereign research substrate."]
#![deny(missing_docs)]
#![forbid(unsafe_code)]

pub use elenkhos as steelman;
pub use sylloge::{
    Acquisition, AcquisitionFailure, AcquisitionLimits, Arxiv, AttemptOutcome, BodyRecord, BoxFut,
    BudgetConstraint, BudgetExceededSnafu, BudgetScope, Charset, CharsetSource, Citation,
    ConnectAttempt, ConnectDeniedSnafu, ConnectError, ConnectIoSnafu, ConnectOutcome,
    ConnectTimedOutSnafu, ConnectedStream, Connector, ContentCoding, CostTracking, DAY_WINDOW,
    DeepDepth, DeepResearch, DirectConnector, DomainDeniedSnafu, DowngradePolicy,
    EVIDENCE_SCHEMA_ID, EVIDENCE_SCHEMA_VERSION, EndpointPolicy, Error, ErrorClass,
    EvidenceEnvelope, EvidenceState, ExtractionRecord, ExtractorId, FatalCorruptionSnafu,
    FreshnessBasis, FreshnessDecision, FreshnessPolicy, HopRecord, InvalidConstraintSnafu,
    InvalidQuerySnafu, LocalDeepResearch, LocalTargetAuthorization, Media, MissingCitationsSnafu,
    OfflineFixture, Outcome, OversizedPayloadSnafu, ParsedResponse, PartialReason,
    PermanentIoSnafu, Producer, ProvenanceEntry, Provider, ProviderAttempt, ProviderFailureSnafu,
    ProviderId, ProviderRequest, ProviderSpend, ProviderTier, PublicationPrecision,
    PublicationProvenance, PublicationTime, PublicationTimeCapability, QueryGenerator, QueryShape,
    QuotaExhaustedSnafu, RateLimit, RateLimitedSnafu, RefusalReason, ReplayOutcome, ResearchResult,
    ResearchStatus, Resolver, ResponseRecord, Result, ResultHit, Router, SchemePolicy,
    SearchConstraints, Segment, SemanticScholar, SourceKind, SourceRetriever, SpendEvent,
    SpendLedger, StaticAcquirer, StaticAcquirerBuilder, Synthesizer, SystemResolver, TaskId,
    TaskNotReadySnafu, TaskUnavailableSnafu, TimeoutSnafu, TlsRecord, TransientIoSnafu,
    TrustAnchors, UnauthorizedSnafu, UnsafeTargetSnafu, UnsupportedSnafu, ValidatedTarget,
    Wikipedia, evaluate_freshness, replay,
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

    /// The evidence types, reached through the facade, for an acquisition
    /// refused before any socket.
    fn assert_refused_envelope(acquisition: &Acquisition) -> &EvidenceEnvelope {
        let envelope: &EvidenceEnvelope = acquisition.envelope();
        assert_eq!(
            envelope.schema_version(),
            EVIDENCE_SCHEMA_VERSION,
            "the envelope carries the producer's schema version"
        );
        assert!(
            envelope.fingerprint().starts_with("sha256:"),
            "the envelope is fingerprinted"
        );
        let hops: &[HopRecord] = envelope.hops();
        let attempts: &[ConnectAttempt] = hops[0].connect_attempts();
        assert!(attempts.is_empty(), "a refused target is never dialed");
        let tls: Option<&TlsRecord> = hops[0].tls();
        assert!(tls.is_none(), "no handshake happened");
        let response: Option<&ResponseRecord> = envelope.response();
        assert!(response.is_none(), "a refused transfer has no response");
        let body: Option<&BodyRecord> = envelope.body();
        assert!(body.is_none(), "a refused transfer has no body record");
        assert!(acquisition.body().is_empty(), "and no body bytes");
        assert_eq!(
            replay(envelope, acquisition.body()),
            ReplayOutcome::NothingToReplay,
            "a failed acquisition has no transformation to replay"
        );
        envelope
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
        let acquisition: Acquisition = acquirer
            .acquire(
                &Url::parse("http://127.0.0.1/").unwrap(),
                &SearchConstraints::default(),
                None,
            )
            .await
            .unwrap();
        let envelope = assert_refused_envelope(&acquisition);
        let Outcome::Failed { failure } = envelope.outcome() else {
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
    fn provider_cohort_and_router_are_reachable_through_facade() {
        // WHY: the router, the first provider cohort, and the attempt
        // receipt types are consumer-facing; exercise each through the
        // facade path rather than only asserting the `pub use` compiles.
        let constraints = SearchConstraints::new(3, BudgetConstraint::free_only());
        let requests: [ProviderRequest; 3] = [
            SemanticScholar::request("q", &constraints).unwrap(),
            Arxiv::request("q", &constraints).unwrap(),
            Wikipedia::new("facade-check/0.0 (https://example.org/contact)")
                .unwrap()
                .request("q", &constraints)
                .unwrap(),
        ];
        assert!(
            requests.iter().all(|r| r.url.scheme() == "https"),
            "every cohort endpoint is https"
        );
        let policies: [EndpointPolicy; 3] =
            [SemanticScholar::POLICY, Arxiv::POLICY, Wikipedia::POLICY];
        let paced: Vec<RateLimit> = policies.iter().filter_map(|p| p.rate_limit).collect();
        assert_eq!(
            paced.len(),
            2,
            "arXiv and Wikipedia document per-client limits"
        );

        let parsed: ParsedResponse = Wikipedia::parse(
            200,
            &[],
            br#"{"pages":[]}"#,
            "2026-09-25T00:00:00Z".parse().unwrap(),
        )
        .unwrap();
        assert!(
            parsed.hits.is_empty() && parsed.malformed_records == 0,
            "an empty page list parses through the facade"
        );
        assert_eq!(
            ResearchResult::empty("q", QueryShape::QuickFactual, "k").evidence_state(),
            EvidenceState::Unanswered,
            "evidence state is reachable through the facade"
        );

        let router = Router::new(Vec::new(), std::time::Duration::from_secs(30)).unwrap();
        assert!(
            format!("{router:?}").contains("Router"),
            "the router is reachable and debuggable"
        );
        let attempt: ProviderAttempt = serde_json::from_value(serde_json::json!({
            "ordinal": 0,
            "tier": "tier1_cheap",
            "outcome": {"status": "refused", "reason": "paid_routing_unavailable"},
        }))
        .unwrap();
        assert_eq!(
            attempt.outcome,
            AttemptOutcome::Refused {
                reason: RefusalReason::PaidRoutingUnavailable
            },
            "receipt types decode through the facade"
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
