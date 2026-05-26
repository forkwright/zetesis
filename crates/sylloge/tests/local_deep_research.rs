//! Fixture-backed lifecycle tests for the local deep-research backend slot.

#![allow(clippy::unwrap_used)]

use jiff::Timestamp;
use url::Url;

use sylloge::{
    Citation, CostTracking, DeepDepth, DeepResearch, LocalDeepResearch, OfflineFixture,
    ProviderSpend, QueryGenerator, QueryShape, ResearchResult, ResearchStatus, ResultHit,
    SourceKind, SourceRetriever, Synthesizer,
};

fn ts() -> Timestamp {
    "2026-05-25T00:00:00Z".parse().unwrap()
}

fn sample_result(query: &str) -> ResearchResult {
    let url = Url::parse("https://example.org/local-deep-research").unwrap();
    let citation = Citation::new(
        url.clone(),
        ts(),
        SourceKind::Web,
        0.9,
        Some("text/html".to_owned()),
    );
    let hit = ResultHit::new(
        "local synthesis source",
        "fixture-backed evidence",
        url,
        vec![citation.clone()],
        0.9,
    );
    ResearchResult::new(
        query,
        QueryShape::GeneralResearch,
        vec![hit],
        vec![sylloge::ProvenanceEntry::new(
            "local_deep_research",
            citation,
        )],
        CostTracking::from_line_items([ProviderSpend::new("local_deep_research", 0, 1, 1)]),
        "local-deep-research-fixture",
    )
}

#[tokio::test]
async fn submit_creates_pending_task() {
    let backend = LocalDeepResearch::new();

    let task = backend
        .submit("how does local deep research work?", DeepDepth::Standard)
        .await
        .unwrap();

    assert_eq!(backend.name(), "local_deep_research");
    assert_eq!(backend.task_count().unwrap(), 1);
    assert_eq!(task.as_str(), "local-deep-research-1");
    assert_eq!(backend.poll(&task).await.unwrap(), ResearchStatus::Pending);
}

#[tokio::test]
async fn running_status_clamps_progress() {
    let backend = LocalDeepResearch::new();
    let task = backend.submit("q", DeepDepth::Shallow).await.unwrap();

    backend.mark_running(&task, Some(150)).unwrap();

    assert_eq!(
        backend.poll(&task).await.unwrap(),
        ResearchStatus::Running {
            progress_pct: Some(100)
        }
    );
}

#[tokio::test]
async fn fetch_before_ready_is_permanent_error() {
    let backend = LocalDeepResearch::new();
    let task = backend.submit("q", DeepDepth::Standard).await.unwrap();

    let err = backend.fetch(&task).await.unwrap_err();

    assert!(err.is_permanent());
    assert!(err.to_string().contains("is not ready"));
}

#[tokio::test]
async fn completed_task_fetches_original_result_envelope() {
    let backend = LocalDeepResearch::new();
    let task = backend.submit("q", DeepDepth::Deep).await.unwrap();
    let result = sample_result("q");

    backend.complete_task(&task, result.clone(), ts()).unwrap();

    assert!(backend.poll(&task).await.unwrap().is_ready());
    let fetched = backend.fetch(&task).await.unwrap();
    assert_eq!(fetched, result);
    assert_eq!(fetched.provider_count(), 1);
    assert_eq!(fetched.cost_spent.total_requests(), 1);
    assert_eq!(fetched.hits[0].citations.len(), 1);
}

#[tokio::test]
async fn failed_and_cancelled_tasks_are_terminal_and_unfetchable() {
    let backend = LocalDeepResearch::new();
    let failed = backend.submit("failed", DeepDepth::Standard).await.unwrap();
    let cancelled = backend
        .submit("cancelled", DeepDepth::Standard)
        .await
        .unwrap();

    backend.fail_task(&failed, "fixture failure").unwrap();
    backend.cancel_task(&cancelled).unwrap();

    assert!(backend.poll(&failed).await.unwrap().is_terminal());
    assert!(backend.fetch(&failed).await.unwrap_err().is_permanent());
    assert!(backend.poll(&cancelled).await.unwrap().is_terminal());
    assert!(backend.fetch(&cancelled).await.unwrap_err().is_permanent());
}

#[tokio::test]
async fn empty_query_is_rejected() {
    let backend = LocalDeepResearch::new();

    let err = backend
        .submit("   ", DeepDepth::Standard)
        .await
        .unwrap_err();

    assert!(err.is_permanent());
    assert!(err.to_string().contains("must not be empty"));
}

#[tokio::test]
async fn local_backend_remains_dyn_compatible() {
    let backend: Box<dyn DeepResearch> = Box::new(LocalDeepResearch::new());

    let task = backend.submit("q", DeepDepth::Standard).await.unwrap();

    assert_eq!(backend.poll(&task).await.unwrap(), ResearchStatus::Pending);
}

struct CountingQueryGen;

impl QueryGenerator for CountingQueryGen {
    fn generate(&self, original: &str, iteration: usize) -> String {
        format!("{original} [iter {iteration}]")
    }
}

struct CountingRetriever;

impl SourceRetriever for CountingRetriever {
    fn retrieve(&self, query: &str, _max_results: usize) -> Vec<ResultHit> {
        let url = Url::parse(&format!("https://example.org/?q={query}")).unwrap();
        let citation = Citation::new(
            url.clone(),
            ts(),
            SourceKind::Web,
            0.85,
            Some("text/html".to_owned()),
        );
        vec![ResultHit::new(
            query,
            "fixture snippet",
            url,
            vec![citation],
            0.85,
        )]
    }
}

struct ReflectingSynthesizer {
    reflect_until: usize,
}

impl Synthesizer for ReflectingSynthesizer {
    fn summarize(&self, query: &str, sources: &[ResultHit]) -> String {
        format!("summary of {query} ({} sources)", sources.len())
    }

    fn reflect(&self, _summary: &str, iteration: usize) -> bool {
        iteration < self.reflect_until
    }

    fn finalize(&self, original: &str, all_hits: &[ResultHit], summaries: &[String]) -> String {
        format!(
            "final answer for {original} with {} hits and {} summaries",
            all_hits.len(),
            summaries.len()
        )
    }
}

#[tokio::test]
async fn reflection_repeats_web_research_until_depth_exhausted() {
    let backend = LocalDeepResearch::new();
    let task = backend
        .submit("how does rust work?", DeepDepth::Standard)
        .await
        .unwrap();

    let fixture = OfflineFixture::new(
        CountingQueryGen,
        CountingRetriever,
        ReflectingSynthesizer { reflect_until: 10 },
    );

    let result = backend
        .execute_offline(&task, &fixture, QueryShape::GeneralResearch)
        .unwrap();

    assert_eq!(
        result.hits.len(),
        3,
        "Standard depth must yield 2 source hits plus synthesis"
    );
    assert_eq!(
        result.provenance.len(),
        2,
        "each iteration must add a provenance entry"
    );

    assert_eq!(
        result.cost_spent.total_requests(),
        9,
        "cost ledger must account for every loop step"
    );

    let fetched = backend.fetch(&task).await.unwrap();
    assert_eq!(fetched, result);
    assert!(backend.poll(&task).await.unwrap().is_ready());
}

#[tokio::test]
async fn offline_result_has_citations_provenance_cost_and_stable_cache_key() {
    let backend = LocalDeepResearch::new();
    let task = backend
        .submit("cache stability test", DeepDepth::Shallow)
        .await
        .unwrap();

    let fixture = OfflineFixture::new(
        CountingQueryGen,
        CountingRetriever,
        ReflectingSynthesizer { reflect_until: 0 },
    );

    let result = backend
        .execute_offline(&task, &fixture, QueryShape::QuickFactual)
        .unwrap();

    assert!(!result.hits.is_empty(), "result must contain hits");
    assert_eq!(result.hits[0].title, "local deep research synthesis");
    assert!(
        result.hits[0].snippet.contains("final answer"),
        "synthesis hit must expose the finalized summary"
    );
    for hit in &result.hits {
        assert!(
            !hit.citations.is_empty(),
            "every hit must carry at least one citation"
        );
    }

    assert!(
        !result.provenance.is_empty(),
        "provenance chain must not be empty"
    );
    assert_eq!(result.provenance[0].provider_id, "offline_fixture");

    assert!(
        result.cost_spent.total_requests() > 0,
        "cost ledger must record loop steps"
    );
    assert!(
        result
            .cost_spent
            .by_provider
            .contains_key("offline_fixture"),
        "cost must be tracked under the offline fixture provider"
    );

    let task2 = backend
        .submit("cache stability test", DeepDepth::Shallow)
        .await
        .unwrap();
    let result2 = backend
        .execute_offline(&task2, &fixture, QueryShape::QuickFactual)
        .unwrap();
    assert_eq!(
        result.cache_key, result2.cache_key,
        "cache key must be stable for identical inputs"
    );

    let task3 = backend
        .submit("cache stability test", DeepDepth::Deep)
        .await
        .unwrap();
    let result3 = backend
        .execute_offline(&task3, &fixture, QueryShape::QuickFactual)
        .unwrap();
    assert_ne!(
        result.cache_key, result3.cache_key,
        "cache key must differ when depth changes"
    );
}

#[tokio::test]
async fn shallow_depth_never_repeats() {
    let backend = LocalDeepResearch::new();
    let task = backend
        .submit("shallow query", DeepDepth::Shallow)
        .await
        .unwrap();

    let fixture = OfflineFixture::new(
        CountingQueryGen,
        CountingRetriever,
        ReflectingSynthesizer { reflect_until: 10 },
    );

    let result = backend
        .execute_offline(&task, &fixture, QueryShape::GeneralResearch)
        .unwrap();

    assert_eq!(
        result.hits.len(),
        2,
        "Shallow must yield 1 source hit plus synthesis"
    );
    assert_eq!(result.provenance.len(), 1);
    assert_eq!(result.cost_spent.total_requests(), 5);
}
