//! Router behaviour with stub providers: routes by declared shape, attempt
//! receipts, paid-tier refusal, failure isolation, screening, merging, and
//! the cache key. No network: the stubs answer with fixed hits or errors,
//! some parsed from the provider fixtures.

#![expect(clippy::unwrap_used, reason = "test assertions must fail loudly")]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use jiff::Timestamp;
use serde_json::{Value, json};
use sylloge::{
    Arxiv, AttemptOutcome, BoxFut, BudgetConstraint, Citation, CostTracking, Error, ErrorClass,
    EvidenceState, FatalCorruptionSnafu, FreshnessBasis, FreshnessPolicy, Provider,
    ProviderAttempt, ProviderTier, QueryShape, RateLimitedSnafu, RefusalReason, ResearchResult,
    Result, ResultHit, Router, SearchConstraints, SemanticScholar, SourceKind, UnauthorizedSnafu,
    Wikipedia,
};
use url::Url;

const S2_DUPLICATES: &[u8] =
    include_bytes!("fixtures/providers/semantic_scholar/duplicate_candidates.json");
const ARXIV_DUPLICATES: &[u8] = include_bytes!("fixtures/providers/arxiv/duplicate_candidates.xml");
const S2_DOCUMENTED: &[u8] =
    include_bytes!("fixtures/providers/semantic_scholar/search_documented_shape.json");
const ARXIV_DOCUMENTED: &[u8] =
    include_bytes!("fixtures/providers/arxiv/search_documented_shape.xml");
const WIKIPEDIA_RECORDED: &[u8] =
    include_bytes!("fixtures/providers/wikipedia/search_recorded_2026-09-25.json");

const ALL_SHAPES: [QueryShape; 10] = [
    QueryShape::QuickFactual,
    QueryShape::SemanticDiscovery,
    QueryShape::AcademicLiterature,
    QueryShape::Patent,
    QueryShape::Finance,
    QueryShape::Legal,
    QueryShape::FreshnessSensitive,
    QueryShape::GeneralResearch,
    QueryShape::CodeAndPackages,
    QueryShape::DatasetDiscovery,
];

fn now() -> Timestamp {
    "2026-09-25T18:00:00Z".parse().unwrap()
}

enum Reply {
    Hits(Vec<ResultHit>),
    /// Hits plus a count of records the provider dropped as malformed.
    Parsed(Vec<ResultHit>, usize),
    Fail(fn() -> Error),
}

struct Stub {
    name: &'static str,
    tier: ProviderTier,
    shapes: &'static [QueryShape],
    reply: Reply,
    calls: AtomicUsize,
}

impl Stub {
    fn new(name: &'static str, shapes: &'static [QueryShape], reply: Reply) -> Arc<Self> {
        Self::tiered(name, ProviderTier::Tier0Free, shapes, reply)
    }

    fn tiered(
        name: &'static str,
        tier: ProviderTier,
        shapes: &'static [QueryShape],
        reply: Reply,
    ) -> Arc<Self> {
        Arc::new(Self {
            name,
            tier,
            shapes,
            reply,
            calls: AtomicUsize::new(0),
        })
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

impl Provider for Stub {
    fn name(&self) -> &'static str {
        self.name
    }

    fn tier(&self) -> ProviderTier {
        self.tier
    }

    fn query_shapes(&self) -> &[QueryShape] {
        self.shapes
    }

    fn search<'a>(
        &'a self,
        query: &'a str,
        _constraints: &'a SearchConstraints,
    ) -> BoxFut<'a, Result<ResearchResult>> {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let (hits, malformed_records) = match &self.reply {
                Reply::Hits(hits) => (hits.clone(), 0),
                Reply::Parsed(hits, malformed) => (hits.clone(), *malformed),
                Reply::Fail(make) => return Err(make()),
            };
            let mut result = ResearchResult::new(
                query,
                QueryShape::GeneralResearch,
                hits,
                Vec::new(),
                CostTracking::default(),
                "stub",
            );
            result.malformed_records = malformed_records;
            Ok(result)
        })
    }
}

fn empty() -> Reply {
    Reply::Hits(Vec::new())
}

fn s2_hits(body: &[u8]) -> Reply {
    let parsed = SemanticScholar::parse(200, &[], body, now()).unwrap();
    Reply::Parsed(parsed.hits, parsed.malformed_records)
}

fn arxiv_hits(body: &[u8]) -> Reply {
    let parsed = Arxiv::parse(200, &[], body, now()).unwrap();
    Reply::Parsed(parsed.hits, parsed.malformed_records)
}

fn wikipedia_hits() -> Reply {
    let parsed = Wikipedia::parse(200, &[], WIKIPEDIA_RECORDED, now()).unwrap();
    Reply::Parsed(parsed.hits, parsed.malformed_records)
}

/// The first cohort in its canonical registration order, each stub
/// declaring the shapes its provider's policy records.
fn cohort(wikipedia: Reply, s2: Reply, arxiv: Reply) -> (Router, [Arc<Stub>; 3]) {
    let stubs = [
        Stub::new("wikipedia", Wikipedia::POLICY.query_shapes, wikipedia),
        Stub::new("semantic_scholar", SemanticScholar::POLICY.query_shapes, s2),
        Stub::new("arxiv", Arxiv::POLICY.query_shapes, arxiv),
    ];
    let providers: Vec<Arc<dyn Provider>> = stubs
        .iter()
        .map(|stub| Arc::clone(stub) as Arc<dyn Provider>)
        .collect();
    (Router::new(providers).unwrap(), stubs)
}

fn router_of(stubs: &[Arc<Stub>]) -> Router {
    Router::new(
        stubs
            .iter()
            .map(|stub| Arc::clone(stub) as Arc<dyn Provider>)
            .collect(),
    )
    .unwrap()
}

fn attempts(result: &ResearchResult) -> Vec<(String, ProviderAttempt)> {
    result
        .provenance
        .iter()
        .map(|entry| {
            assert!(
                entry.citation.is_none(),
                "an attempt receipt carries no citation"
            );
            (
                entry.provider_id.to_string(),
                entry.attempt.clone().unwrap(),
            )
        })
        .collect()
}

fn route_of(result: &ResearchResult) -> Vec<String> {
    attempts(result).into_iter().map(|(name, _)| name).collect()
}

fn outcome(result: &ResearchResult, provider: &str) -> AttemptOutcome {
    attempts(result)
        .into_iter()
        .find(|(name, _)| name == provider)
        .map(|(_, attempt)| attempt.outcome)
        .unwrap()
}

fn titles(result: &ResearchResult) -> Vec<&str> {
    result.hits.iter().map(|hit| hit.title.as_str()).collect()
}

fn hit<'r>(result: &'r ResearchResult, title: &str) -> &'r ResultHit {
    result.hits.iter().find(|hit| hit.title == title).unwrap()
}

fn answered(returned: usize) -> AttemptOutcome {
    serde_json::from_value(json!({
        "status": "answered",
        "returned": returned,
        "rejected_by_freshness": 0,
        "rejected_by_domain": 0,
        "rejected_uncited": 0,
        "malformed_records": 0,
    }))
    .unwrap()
}

fn web_hit(title: &str, url: &str) -> ResultHit {
    let url = Url::parse(url).unwrap();
    let citation = Citation::new(url.clone(), now(), SourceKind::Web, 1.0, None);
    ResultHit::new(title, "", url, vec![citation], 1.0).unwrap()
}

async fn search(
    router: &Router,
    shape: QueryShape,
    constraints: &SearchConstraints,
) -> ResearchResult {
    router
        .search("stable identity", shape, constraints, now())
        .await
        .unwrap()
}

#[tokio::test]
async fn cohort_routes_follow_the_documented_table() {
    let expected: [(QueryShape, &[&str]); 4] = [
        (
            QueryShape::AcademicLiterature,
            &["semantic_scholar", "arxiv"],
        ),
        (QueryShape::QuickFactual, &["wikipedia"]),
        (
            QueryShape::GeneralResearch,
            &["wikipedia", "semantic_scholar"],
        ),
        (QueryShape::SemanticDiscovery, &["semantic_scholar"]),
    ];
    for (shape, route) in expected {
        let (router, _) = cohort(empty(), empty(), empty());
        let result = search(&router, shape, &SearchConstraints::default()).await;
        assert_eq!(route_of(&result), route, "route for {shape:?}");
        let ordinals: Vec<u32> = attempts(&result).iter().map(|(_, a)| a.ordinal).collect();
        let expected_ordinals: Vec<u32> = (0..).take(route.len()).collect();
        assert_eq!(
            ordinals, expected_ordinals,
            "ordinals follow the route for {shape:?}"
        );
        assert_eq!(result.shape, shape, "the result echoes the shape");
    }
}

#[tokio::test]
async fn a_shape_no_provider_serves_is_an_explicit_gap() {
    let served = [
        QueryShape::AcademicLiterature,
        QueryShape::QuickFactual,
        QueryShape::GeneralResearch,
        QueryShape::SemanticDiscovery,
    ];
    for shape in ALL_SHAPES.into_iter().filter(|s| !served.contains(s)) {
        let (router, stubs) = cohort(wikipedia_hits(), s2_hits(S2_DOCUMENTED), empty());
        let err = router
            .search("q", shape, &SearchConstraints::default(), now())
            .await
            .unwrap_err();
        assert!(
            matches!(err, Error::Unsupported { .. }),
            "{shape:?} is unsupported: {err:?}"
        );
        assert!(
            err.to_string().contains(shape.as_str()),
            "the error names the shape: {err}"
        );
        assert!(
            stubs.iter().all(|stub| stub.calls() == 0),
            "no provider answers a shape it does not declare ({shape:?})"
        );
    }
}

#[tokio::test]
async fn paid_provider_is_refused_even_when_the_budget_allows_paid_spend() {
    let free = Stub::new("wikipedia", &[QueryShape::GeneralResearch], empty());
    let paid = Stub::tiered(
        "brave",
        ProviderTier::Tier1Cheap,
        &[QueryShape::GeneralResearch],
        Reply::Hits(vec![web_hit("Paid", "https://paid.example/a")]),
    );
    let router = router_of(&[Arc::clone(&paid), Arc::clone(&free)]);
    let budget = BudgetConstraint::free_only()
        .with_paid_tier_allowed(true)
        .with_per_query_cap(10_000_000);
    let result = search(
        &router,
        QueryShape::GeneralResearch,
        &SearchConstraints::new(5, budget),
    )
    .await;

    assert_eq!(paid.calls(), 0, "the paid provider is never called");
    assert_eq!(
        attempts(&result).first().map(|(_, a)| a.clone()),
        Some(
            serde_json::from_value::<ProviderAttempt>(json!({
                "ordinal": 0,
                "tier": "tier1_cheap",
                "outcome": {"status": "refused", "reason": "paid_routing_unavailable"},
            }))
            .unwrap()
        ),
        "the refusal is a typed receipt in route order"
    );
    assert_eq!(
        outcome(&result, "wikipedia"),
        AttemptOutcome::Empty,
        "the free provider still answers"
    );
    assert!(result.hits.is_empty(), "no paid hit leaks into the answer");
    assert!(
        !result.cost_spent.by_provider.contains_key("brave"),
        "a refused provider costs nothing"
    );
    assert!(!result.cost_spent.any_paid(), "no paid spend is recorded");
}

#[tokio::test]
async fn a_tier0_miss_does_not_enable_paid_use() {
    let free = Stub::new(
        "semantic_scholar",
        &[QueryShape::AcademicLiterature],
        empty(),
    );
    let paid = Stub::tiered(
        "paid_deep",
        ProviderTier::Tier3PaidDeep,
        &[QueryShape::AcademicLiterature],
        Reply::Hits(vec![web_hit("Paid", "https://paid.example/b")]),
    );
    let router = router_of(&[Arc::clone(&free), Arc::clone(&paid)]);
    let result = search(
        &router,
        QueryShape::AcademicLiterature,
        &SearchConstraints::default(),
    )
    .await;
    assert_eq!(free.calls(), 1, "the free provider is tried");
    assert_eq!(paid.calls(), 0, "its miss does not open the paid tier");
    assert_eq!(
        outcome(&result, "paid_deep"),
        serde_json::from_value::<AttemptOutcome>(
            json!({"status": "refused", "reason": "paid_routing_unavailable"})
        )
        .unwrap(),
        "refused, not skipped silently"
    );
    assert_eq!(
        serde_json::to_value(RefusalReason::PaidRoutingUnavailable).unwrap(),
        json!("paid_routing_unavailable"),
        "the reason's wire name"
    );
}

#[tokio::test]
async fn a_transient_failure_does_not_stop_the_route() {
    let (router, stubs) = cohort(
        empty(),
        Reply::Fail(|| {
            RateLimitedSnafu {
                provider: "semantic_scholar",
                retry_after_ms: None::<u64>,
            }
            .build()
        }),
        arxiv_hits(ARXIV_DOCUMENTED),
    );
    let result = search(
        &router,
        QueryShape::AcademicLiterature,
        &SearchConstraints::default(),
    )
    .await;
    assert!(
        stubs.iter().skip(1).all(|stub| stub.calls() == 1),
        "both academic providers are called"
    );
    assert!(
        matches!(
            outcome(&result, "semantic_scholar"),
            AttemptOutcome::Failed { class: ErrorClass::Transient, ref message } if message.contains("rate limited")
        ),
        "the failure is receipted with its class"
    );
    assert_eq!(
        outcome(&result, "arxiv"),
        answered(2),
        "the next provider answers"
    );
    assert_eq!(result.hits.len(), 2, "the answer is arXiv's two hits");
}

#[tokio::test]
async fn a_permanent_failure_is_receipted_and_the_route_continues() {
    let (router, _) = cohort(
        empty(),
        Reply::Fail(|| {
            UnauthorizedSnafu {
                provider: "semantic_scholar",
                message: "HTTP 403",
            }
            .build()
        }),
        empty(),
    );
    let result = search(
        &router,
        QueryShape::AcademicLiterature,
        &SearchConstraints::default(),
    )
    .await;
    assert!(
        matches!(
            outcome(&result, "semantic_scholar"),
            AttemptOutcome::Failed {
                class: ErrorClass::Permanent,
                ..
            }
        ),
        "authentication denial is permanent"
    );
    assert_eq!(
        outcome(&result, "arxiv"),
        AttemptOutcome::Empty,
        "arXiv still tried"
    );
    assert!(result.hits.is_empty(), "absent evidence stays absent");
}

#[tokio::test]
async fn a_fatal_error_aborts_the_route() {
    let (router, stubs) = cohort(
        empty(),
        Reply::Fail(|| {
            FatalCorruptionSnafu {
                message: "fixture corruption",
            }
            .build()
        }),
        arxiv_hits(ARXIV_DOCUMENTED),
    );
    let err = router
        .search(
            "q",
            QueryShape::AcademicLiterature,
            &SearchConstraints::default(),
            now(),
        )
        .await
        .unwrap_err();
    assert!(err.is_fatal(), "fatal errors surface as is: {err:?}");
    assert_eq!(
        stubs.get(2).unwrap().calls(),
        0,
        "nothing runs after a fatal error"
    );
}

#[tokio::test]
async fn every_call_costs_one_free_request() {
    let (router, _) = cohort(
        empty(),
        Reply::Fail(|| {
            RateLimitedSnafu {
                provider: "semantic_scholar",
                retry_after_ms: Some(1_000_u64),
            }
            .build()
        }),
        arxiv_hits(ARXIV_DOCUMENTED),
    );
    let result = search(
        &router,
        QueryShape::AcademicLiterature,
        &SearchConstraints::default(),
    )
    .await;
    assert_eq!(
        serde_json::to_value(&result.cost_spent).unwrap(),
        json!({"by_provider": {
            "arxiv": {"provider_id": "arxiv", "paid_micro_cents": 0, "free_tier_units": 1, "request_count": 1},
            "semantic_scholar": {"provider_id": "semantic_scholar", "paid_micro_cents": 0, "free_tier_units": 1, "request_count": 1},
        }}),
        "a failed call still spent a request"
    );
}

#[tokio::test]
async fn the_same_paper_from_semantic_scholar_and_arxiv_merges_by_stable_identity() {
    let (router, _) = cohort(
        empty(),
        s2_hits(S2_DUPLICATES),
        arxiv_hits(ARXIV_DUPLICATES),
    );
    let result = search(
        &router,
        QueryShape::AcademicLiterature,
        &SearchConstraints::default(),
    )
    .await;

    let merged = hit(&result, "Deduplicating Preprints by Stable Identity");
    let sources: Vec<&str> = merged
        .citations
        .iter()
        .map(|citation| citation.source_url.as_str())
        .collect();
    assert_eq!(
        sources,
        [
            "https://www.semanticscholar.org/paper/a3b4c5d6e7f8091a2b3c4d5e6f708192a3b4c5d6",
            "http://arxiv.org/abs/2101.00001v2",
        ],
        "both providers' citations corroborate one hit"
    );
    assert_eq!(
        merged.metadata.get("provider"),
        Some(&json!("semantic_scholar"))
    );
    let absorbed = merged.metadata.get("merged_records").unwrap();
    assert_eq!(
        absorbed.pointer("/0/provider"),
        Some(&json!("arxiv")),
        "the absorbed record's lineage is kept"
    );
    assert_eq!(
        absorbed.pointer("/0/metadata/arxiv_version"),
        Some(&json!(2)),
        "the absorbed record's metadata is kept whole"
    );
    assert!(
        !merged.metadata.contains_key("conflicts_with"),
        "equal DOI and arXiv id do not conflict"
    );
}

#[tokio::test]
async fn a_title_only_match_with_different_years_stays_separate_and_marked() {
    let (router, _) = cohort(
        empty(),
        s2_hits(S2_DUPLICATES),
        arxiv_hits(ARXIV_DUPLICATES),
    );
    let result = search(
        &router,
        QueryShape::AcademicLiterature,
        &SearchConstraints::default(),
    )
    .await;

    let s2 = hit(&result, "Routing Queries Across Free Scholarly Indexes");
    let arxiv = hit(&result, "Routing Queries Across Free Scholarly Indexes.");
    assert_eq!(
        s2.citations.len(),
        1,
        "2019 and 2018 records with equal titles do not merge"
    );
    assert_eq!(
        s2.metadata.get("conflicts_with"),
        Some(&json!(["http://arxiv.org/abs/1812.00003v1"])),
        "the Semantic Scholar record names its look-alike"
    );
    assert_eq!(
        arxiv.metadata.get("conflicts_with"),
        Some(&json!([
            "https://www.semanticscholar.org/paper/b4c5d6e7f8091a2b3c4d5e6f708192a3b4c5d6e7"
        ])),
        "and the arXiv record names it back"
    );
}

#[tokio::test]
async fn conflicting_identities_keep_both_records() {
    let (router, _) = cohort(
        empty(),
        s2_hits(S2_DUPLICATES),
        arxiv_hits(ARXIV_DUPLICATES),
    );
    let result = search(
        &router,
        QueryShape::AcademicLiterature,
        &SearchConstraints::default(),
    )
    .await;

    let conflicting: Vec<&ResultHit> = result
        .hits
        .iter()
        .filter(|hit| hit.title == "Conflicting Identifiers in Practice")
        .collect();
    assert_eq!(
        conflicting.len(),
        2,
        "same arXiv id but different publisher DOIs: both records are kept"
    );
    for record in conflicting {
        assert_eq!(record.citations.len(), 1, "neither absorbed the other");
        assert!(
            record.metadata.contains_key("conflicts_with"),
            "each names the other: {}",
            record.url
        );
    }
}

#[tokio::test]
async fn a_title_and_year_match_without_conflicting_identity_merges() {
    let (router, _) = cohort(
        empty(),
        s2_hits(S2_DUPLICATES),
        arxiv_hits(ARXIV_DUPLICATES),
    );
    let result = search(
        &router,
        QueryShape::AcademicLiterature,
        &SearchConstraints::default(),
    )
    .await;

    let merged = hit(&result, "Same Title, Same Year");
    assert_eq!(
        merged.citations.len(),
        2,
        "equal normalized titles and years, no shared identity kind, merge"
    );
    assert_eq!(
        titles(&result),
        [
            "Deduplicating Preprints by Stable Identity",
            "Routing Queries Across Free Scholarly Indexes",
            "Routing Queries Across Free Scholarly Indexes.",
            "Conflicting Identifiers in Practice",
            "Conflicting Identifiers in Practice",
            "Same Title, Same Year",
            "An arXiv DOI Meets a Journal DOI",
        ],
        "ten records become seven hits, by score then route order"
    );
}

#[tokio::test]
async fn strict_freshness_drops_undated_hits_and_counts_them() {
    let (router, _) = cohort(wikipedia_hits(), empty(), empty());
    let constraints = SearchConstraints::default().with_freshness(Duration::from_secs(86_400));
    let result = search(&router, QueryShape::QuickFactual, &constraints).await;
    assert!(
        result.hits.is_empty(),
        "undated wiki hits fail a strict window; nothing fills their place"
    );
    assert_eq!(
        outcome(&result, "wikipedia"),
        serde_json::from_value::<AttemptOutcome>(json!({
            "status": "answered",
            "returned": 2,
            "rejected_by_freshness": 2,
            "rejected_by_domain": 0,
            "rejected_uncited": 0,
            "malformed_records": 0,
        }))
        .unwrap(),
        "the drop is counted in the receipt"
    );
}

#[tokio::test]
async fn kept_hits_carry_their_freshness_receipt() {
    let (router, _) = cohort(empty(), s2_hits(S2_DOCUMENTED), empty());
    let seven_years = Duration::from_secs(7 * 365 * 86_400);
    let constraints = SearchConstraints::default()
        .with_freshness(seven_years)
        .with_freshness_policy(FreshnessPolicy::Permissive);
    let result = search(&router, QueryShape::SemanticDiscovery, &constraints).await;

    assert_eq!(
        titles(&result),
        [
            "Deduplicating Preprints by Stable Identity",
            "A Workshop Paper Without a Journal",
        ],
        "the 2019-07-15 paper is outside a seven-year window"
    );
    assert_eq!(
        hit(&result, "Deduplicating Preprints by Stable Identity")
            .freshness
            .map(|d| d.basis),
        Some(FreshnessBasis::AccessedAtFallback),
        "an undated hit passes only by the permissive fallback, and says so"
    );
    assert_eq!(
        hit(&result, "A Workshop Paper Without a Journal")
            .freshness
            .map(|d| d.basis),
        Some(FreshnessBasis::PublicationTime),
        "a dated hit passes on its publication time"
    );
    assert!(
        matches!(
            outcome(&result, "semantic_scholar"),
            AttemptOutcome::Answered {
                returned: 3,
                rejected_by_freshness: 1,
                ..
            }
        ),
        "one of three rejected"
    );
}

#[tokio::test]
async fn domain_lists_screen_hit_hosts() {
    let (router, _) = cohort(
        empty(),
        s2_hits(S2_DOCUMENTED),
        arxiv_hits(ARXIV_DOCUMENTED),
    );
    let deny = SearchConstraints::default().with_denylist(vec!["SemanticScholar.org".to_owned()]);
    let result = search(&router, QueryShape::AcademicLiterature, &deny).await;
    assert!(
        result
            .hits
            .iter()
            .all(|hit| hit.url.host_str() == Some("arxiv.org")),
        "denied hosts are dropped, case-insensitively and with subdomains"
    );
    assert!(
        matches!(
            outcome(&result, "semantic_scholar"),
            AttemptOutcome::Answered {
                returned: 3,
                rejected_by_domain: 3,
                ..
            }
        ),
        "the drop is counted"
    );

    let allow = SearchConstraints::default().with_allowlist(vec!["semanticscholar.org".to_owned()]);
    let result = search(&router, QueryShape::AcademicLiterature, &allow).await;
    assert!(
        result
            .hits
            .iter()
            .all(|hit| hit.url.host_str() == Some("www.semanticscholar.org")),
        "only allowed hosts remain"
    );
}

#[tokio::test]
async fn unusable_input_fails_before_any_provider_is_called() {
    let (router, stubs) = cohort(empty(), empty(), empty());
    let cases = [
        ("   ", SearchConstraints::default()),
        (
            "q",
            SearchConstraints::new(0, BudgetConstraint::free_only()),
        ),
        (
            "q",
            SearchConstraints::default().with_denylist(vec!["*.example".to_owned()]),
        ),
    ];
    for (query, constraints) in cases {
        let err = router
            .search(query, QueryShape::GeneralResearch, &constraints, now())
            .await
            .unwrap_err();
        assert!(
            matches!(
                err,
                Error::InvalidQuery { .. } | Error::InvalidConstraint { .. }
            ),
            "{query:?} is refused up front: {err:?}"
        );
    }
    assert!(
        stubs.iter().all(|stub| stub.calls() == 0),
        "no provider saw unusable input"
    );
}

#[tokio::test]
async fn max_results_caps_the_answer_and_never_pads_it() {
    let (router, _) = cohort(
        empty(),
        s2_hits(S2_DOCUMENTED),
        arxiv_hits(ARXIV_DOCUMENTED),
    );
    let capped = search(
        &router,
        QueryShape::AcademicLiterature,
        &SearchConstraints::new(2, BudgetConstraint::free_only()),
    )
    .await;
    assert_eq!(
        titles(&capped),
        [
            "Routing Queries Across Free Scholarly Indexes",
            "Deduplicating Preprints by Stable Identity",
        ],
        "the two rank-1 hits, route order breaking the tie"
    );

    let generous = search(
        &router,
        QueryShape::AcademicLiterature,
        &SearchConstraints::new(50, BudgetConstraint::free_only()),
    )
    .await;
    assert_eq!(
        generous.hits.len(),
        4,
        "five records, the arXiv preprint merged by its DataCite DOI: four hits, no filler"
    );
}

#[tokio::test]
async fn uncited_hits_are_dropped_and_counted() {
    let mut uncited = web_hit("No provenance", "https://example.org/uncited");
    uncited.citations.clear();
    let stub = Stub::new(
        "wikipedia",
        &[QueryShape::QuickFactual],
        Reply::Hits(vec![uncited, web_hit("Cited", "https://example.org/cited")]),
    );
    let router = router_of(&[stub]);
    let result = search(
        &router,
        QueryShape::QuickFactual,
        &SearchConstraints::default(),
    )
    .await;
    assert_eq!(titles(&result), ["Cited"], "no hit without a citation");
    assert!(
        matches!(
            outcome(&result, "wikipedia"),
            AttemptOutcome::Answered {
                returned: 2,
                rejected_uncited: 1,
                ..
            }
        ),
        "the drop is counted"
    );
}

#[tokio::test]
async fn an_arxiv_datacite_doi_merges_with_the_record_carrying_the_journal_doi() {
    let (router, _) = cohort(
        empty(),
        s2_hits(S2_DUPLICATES),
        arxiv_hits(ARXIV_DUPLICATES),
    );
    let result = search(
        &router,
        QueryShape::AcademicLiterature,
        &SearchConstraints::default(),
    )
    .await;

    let merged = hit(&result, "An arXiv DOI Meets a Journal DOI");
    assert_eq!(
        merged.citations.len(),
        2,
        "the DataCite DOI 10.48550/arXiv.2107.00005 is arXiv 2107.00005"
    );
    assert_eq!(
        merged.metadata.get("arxiv_id"),
        Some(&json!("2107.00005")),
        "the arXiv identity, version removed"
    );
    assert_eq!(
        merged.metadata.get("arxiv_doi"),
        Some(&json!("10.48550/arxiv.2107.00005")),
        "the DataCite DOI stays on record"
    );
    assert_eq!(
        merged.metadata.get("doi"),
        Some(&json!("10.5555/synthetic.2021.555")),
        "the journal DOI from the absorbed arXiv record is carried as its own identity"
    );
    assert!(
        !merged.metadata.contains_key("conflicts_with"),
        "an arXiv DOI and a journal DOI do not conflict"
    );
    assert!(
        result
            .hits
            .iter()
            .all(|h| h.title != "An arXiv DOI meets a journal DOI"),
        "the arXiv record does not survive as a separate hit"
    );
}

#[tokio::test]
async fn malformed_records_are_counted_in_the_receipt_and_the_result() {
    const MALFORMED_S2: &[u8] =
        include_bytes!("fixtures/providers/semantic_scholar/search_malformed_records.json");
    const MALFORMED_ARXIV: &[u8] =
        include_bytes!("fixtures/providers/arxiv/search_malformed_records.xml");
    let (router, _) = cohort(empty(), s2_hits(MALFORMED_S2), arxiv_hits(MALFORMED_ARXIV));
    let result = search(
        &router,
        QueryShape::AcademicLiterature,
        &SearchConstraints::default(),
    )
    .await;
    for provider in ["semantic_scholar", "arxiv"] {
        assert!(
            matches!(
                outcome(&result, provider),
                AttemptOutcome::Answered {
                    returned: 2,
                    malformed_records: 2,
                    ..
                }
            ),
            "{provider}: two hits kept, two records dropped"
        );
    }
    assert_eq!(
        result.malformed_records, 4,
        "the routed result sums the drops"
    );
    assert_eq!(result.hits.len(), 4, "the surviving records are answered");
}

#[tokio::test]
async fn a_provider_whose_records_were_all_malformed_answered_without_evidence() {
    let stub = Stub::new(
        "wikipedia",
        &[QueryShape::QuickFactual],
        Reply::Parsed(Vec::new(), 3),
    );
    let router = router_of(&[stub]);
    let result = search(
        &router,
        QueryShape::QuickFactual,
        &SearchConstraints::default(),
    )
    .await;
    assert!(
        matches!(
            outcome(&result, "wikipedia"),
            AttemptOutcome::Answered {
                returned: 0,
                malformed_records: 3,
                ..
            }
        ),
        "dropped records are not an empty answer"
    );
    assert_eq!(result.evidence_state(), EvidenceState::NoEvidence);
}

#[tokio::test]
async fn evidence_state_is_answered_when_a_hit_survives() {
    let (router, _) = cohort(
        empty(),
        Reply::Fail(|| {
            RateLimitedSnafu {
                provider: "semantic_scholar",
                retry_after_ms: None::<u64>,
            }
            .build()
        }),
        arxiv_hits(ARXIV_DOCUMENTED),
    );
    let result = search(
        &router,
        QueryShape::AcademicLiterature,
        &SearchConstraints::default(),
    )
    .await;
    assert_eq!(
        result.evidence_state(),
        EvidenceState::Answered,
        "one provider's hits are evidence even when another failed"
    );
}

#[tokio::test]
async fn evidence_state_is_no_evidence_when_a_provider_answered_and_nothing_survived() {
    let (router, _) = cohort(empty(), empty(), empty());
    let empty_answer = search(
        &router,
        QueryShape::AcademicLiterature,
        &SearchConstraints::default(),
    )
    .await;
    assert_eq!(
        empty_answer.evidence_state(),
        EvidenceState::NoEvidence,
        "providers answered with no hits"
    );

    let (router, _) = cohort(wikipedia_hits(), empty(), empty());
    let strict = SearchConstraints::default().with_freshness(Duration::from_secs(86_400));
    let screened_out = search(&router, QueryShape::QuickFactual, &strict).await;
    assert_eq!(
        screened_out.evidence_state(),
        EvidenceState::NoEvidence,
        "a provider answered and the caller's screens dropped every hit"
    );
}

#[tokio::test]
async fn evidence_state_is_unanswered_when_every_provider_failed_or_was_refused() {
    let failed = Stub::new(
        "semantic_scholar",
        &[QueryShape::AcademicLiterature],
        Reply::Fail(|| {
            UnauthorizedSnafu {
                provider: "semantic_scholar",
                message: "HTTP 403",
            }
            .build()
        }),
    );
    let paid = Stub::tiered(
        "paid_deep",
        ProviderTier::Tier3PaidDeep,
        &[QueryShape::AcademicLiterature],
        Reply::Hits(vec![web_hit("Paid", "https://paid.example/c")]),
    );
    let router = router_of(&[failed, paid]);
    let result = search(
        &router,
        QueryShape::AcademicLiterature,
        &SearchConstraints::default(),
    )
    .await;
    assert!(result.hits.is_empty(), "no hits either way");
    assert_eq!(
        result.evidence_state(),
        EvidenceState::Unanswered,
        "total failure is not reported as absence of evidence"
    );
}

#[tokio::test]
async fn cache_key_is_stable_and_sensitive_to_every_input() {
    let (router, _) = cohort(empty(), empty(), empty());
    let key = |query: &'static str, shape: QueryShape, constraints: SearchConstraints| {
        let router = &router;
        async move {
            router
                .search(query, shape, &constraints, now())
                .await
                .unwrap()
                .cache_key
        }
    };
    let base = key(
        "attention routing",
        QueryShape::GeneralResearch,
        SearchConstraints::default(),
    )
    .await;
    assert!(base.starts_with("sha256:"), "algorithm-prefixed: {base}");
    assert_eq!(
        key(
            "  attention \n routing ",
            QueryShape::GeneralResearch,
            SearchConstraints::default()
        )
        .await,
        base,
        "whitespace does not change the key"
    );
    let spelled = |entries: &[&str]| {
        SearchConstraints::default().with_denylist(entries.iter().map(|&e| e.to_owned()).collect())
    };
    assert_eq!(
        key(
            "q",
            QueryShape::GeneralResearch,
            spelled(&["b.example", "A.Example."])
        )
        .await,
        key(
            "q",
            QueryShape::GeneralResearch,
            spelled(&[".a.example", "b.example", "a.example"])
        )
        .await,
        "domain entries that canonicalize alike, in any order, give one key"
    );
    for (label, other) in [
        (
            "case",
            key(
                "Attention Routing",
                QueryShape::GeneralResearch,
                SearchConstraints::default(),
            )
            .await,
        ),
        (
            "shape",
            key(
                "attention routing",
                QueryShape::QuickFactual,
                SearchConstraints::default(),
            )
            .await,
        ),
        (
            "constraints",
            key(
                "attention routing",
                QueryShape::GeneralResearch,
                SearchConstraints::new(3, BudgetConstraint::free_only()),
            )
            .await,
        ),
    ] {
        assert_ne!(other, base, "{label} changes the key");
    }
}

#[test]
fn duplicate_provider_names_are_refused() {
    let a: Arc<dyn Provider> = Stub::new("arxiv", &[], empty());
    let b: Arc<dyn Provider> = Stub::new("arxiv", &[], empty());
    let err = Router::new(vec![a, b]).unwrap_err();
    assert!(
        matches!(err, Error::InvalidConstraint { ref field, .. } if field == "providers"),
        "receipts keyed by name would be ambiguous: {err:?}"
    );
}

#[test]
fn router_search_future_is_send() {
    // WHY: consumers run searches on multi-threaded executors
    // (`tokio::spawn`), which needs a `Send` future.
    fn assert_send<T: Send>(_: &T) {}
    let (router, _) = cohort(empty(), empty(), empty());
    let constraints = SearchConstraints::default();
    let future = router.search("q", QueryShape::QuickFactual, &constraints, now());
    assert_send(&future);
}

#[test]
fn provenance_receipts_round_trip_through_json() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    let (router, _) = cohort(
        empty(),
        s2_hits(S2_DUPLICATES),
        arxiv_hits(ARXIV_DUPLICATES),
    );
    let result = runtime.block_on(search(
        &router,
        QueryShape::AcademicLiterature,
        &SearchConstraints::default(),
    ));
    let json: Value = serde_json::to_value(&result).unwrap();
    let back: ResearchResult = serde_json::from_value(json).unwrap();
    assert_eq!(
        back, result,
        "a routed result, receipts included, round-trips"
    );
}
