//! Fixture-backed lifecycle tests for the local deep-research backend slot.

#![expect(clippy::unwrap_used, reason = "test assertions must fail loudly")]

use std::sync::Arc;

use jiff::Timestamp;
use url::Url;

use sylloge::{
    Citation, CostTracking, DeepDepth, DeepResearch, LocalDeepResearch, OfflineFixture,
    ProviderSpend, QueryGenerator, QueryShape, ResearchResult, ResearchStatus, ResultHit,
    SourceKind, SourceRetriever, Synthesizer, TaskId,
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
    )
    .unwrap();
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
async fn fetch_before_ready_is_transient_not_ready() {
    // WHY: "still running, poll again" is a retryable condition; callers
    // following the crate's retry contract must see is_transient() ==
    // true, not a permanent error that aborts polling (issue #33).
    let backend = LocalDeepResearch::new();
    let task = backend.submit("q", DeepDepth::Standard).await.unwrap();

    let err = backend.fetch(&task).await.unwrap_err();

    assert!(err.is_transient());
    assert!(!err.is_permanent());
    assert!(err.to_string().contains("is not ready"));

    backend.mark_running(&task, Some(10)).unwrap();
    let err = backend.fetch(&task).await.unwrap_err();
    assert!(err.is_transient());
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
async fn cancel_task_rejects_an_already_ready_task_without_discarding_its_result() {
    // WHY(zetesis#49): cancel_task previously overwrote ANY task's status
    // unconditionally, including a completed one -- cancelling a Ready
    // task silently wiped its result. Cancellation of a finished task must
    // be rejected instead.
    let backend = LocalDeepResearch::new();
    let task = backend.submit("q", DeepDepth::Deep).await.unwrap();
    let result = sample_result("q");
    backend.complete_task(&task, result.clone(), ts()).unwrap();

    let err = backend.cancel_task(&task).unwrap_err();
    assert!(err.is_permanent());

    // The result must still be intact and fetchable.
    assert!(backend.poll(&task).await.unwrap().is_ready());
    assert_eq!(backend.fetch(&task).await.unwrap(), result);
}

#[tokio::test]
async fn cancel_task_rejects_an_already_failed_task() {
    let backend = LocalDeepResearch::new();
    let task = backend.submit("q", DeepDepth::Standard).await.unwrap();
    backend.fail_task(&task, "fixture failure").unwrap();

    let err = backend.cancel_task(&task).unwrap_err();
    assert!(err.is_permanent());
    // The original failure reason must still be the one reported.
    let fetch_err = backend.fetch(&task).await.unwrap_err();
    assert!(fetch_err.to_string().contains("fixture failure"));
}

#[tokio::test]
async fn cancel_task_is_idempotent_for_an_already_cancelled_task() {
    let backend = LocalDeepResearch::new();
    let task = backend.submit("q", DeepDepth::Standard).await.unwrap();

    backend.cancel_task(&task).unwrap();
    // WHY: a second cancel of the same task must succeed, not error --
    // that is what "idempotent" means for the trait-level contract.
    backend.cancel_task(&task).unwrap();

    assert!(backend.poll(&task).await.unwrap().is_terminal());
}

#[tokio::test]
async fn cancel_reachable_through_dyn_deep_research_delegates_to_cancel_task() {
    let backend: Arc<dyn DeepResearch> = Arc::new(LocalDeepResearch::new());
    let task = backend.submit("q", DeepDepth::Standard).await.unwrap();
    backend.cancel(&task).await.unwrap();
    assert!(backend.poll(&task).await.unwrap().is_terminal());
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
    let failed_err = backend.fetch(&failed).await.unwrap_err();
    assert!(failed_err.is_permanent());
    assert!(failed_err.to_string().contains("fixture failure"));

    assert!(backend.poll(&cancelled).await.unwrap().is_terminal());
    let cancelled_err = backend.fetch(&cancelled).await.unwrap_err();
    assert!(cancelled_err.is_permanent());
    assert!(cancelled_err.to_string().contains("cancelled"));
}

#[tokio::test]
async fn unknown_task_is_permanent_unavailable() {
    let backend = LocalDeepResearch::new();
    let err = backend
        .fetch(&TaskId::new("no-such-task"))
        .await
        .unwrap_err();
    assert!(err.is_permanent());
    assert!(err.to_string().contains("unknown task"));
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

#[tokio::test]
async fn full_store_evicts_terminal_tasks_then_applies_backpressure() {
    // WHY: the task map is bounded (issue #31, resilience); terminal
    // tasks are evicted first and a store full of in-flight tasks
    // rejects with a transient error so callers back off.
    let backend = LocalDeepResearch::new();
    let mut first_task = None;
    for i in 0..LocalDeepResearch::MAX_TASKS {
        let task = backend
            .submit(&format!("query {i}"), DeepDepth::Shallow)
            .await
            .unwrap();
        first_task.get_or_insert(task);
    }
    assert_eq!(backend.task_count().unwrap(), LocalDeepResearch::MAX_TASKS);

    // Every task is pending: the next submit must be rejected transiently.
    let err = backend
        .submit("overflow", DeepDepth::Shallow)
        .await
        .unwrap_err();
    assert!(err.is_transient());

    // Completing one task frees a slot via automatic terminal eviction.
    let done = first_task.unwrap();
    backend
        .complete_task(&done, sample_result("query 0"), ts())
        .unwrap();
    let task = backend
        .submit("after-evict", DeepDepth::Shallow)
        .await
        .unwrap();
    assert_eq!(backend.poll(&task).await.unwrap(), ResearchStatus::Pending);
    assert_eq!(backend.task_count().unwrap(), LocalDeepResearch::MAX_TASKS);
}

#[tokio::test]
async fn evict_terminal_removes_only_terminal_tasks() {
    let backend = LocalDeepResearch::new();
    let pending = backend.submit("pending", DeepDepth::Shallow).await.unwrap();
    let done = backend.submit("done", DeepDepth::Shallow).await.unwrap();
    let dead = backend.submit("dead", DeepDepth::Shallow).await.unwrap();
    backend
        .complete_task(&done, sample_result("done"), ts())
        .unwrap();
    backend.fail_task(&dead, "gone").unwrap();

    assert_eq!(backend.evict_terminal().unwrap(), 2);
    assert_eq!(backend.task_count().unwrap(), 1);
    assert_eq!(
        backend.poll(&pending).await.unwrap(),
        ResearchStatus::Pending
    );
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
        vec![ResultHit::new(query, "fixture snippet", url, vec![citation], 0.85).unwrap()]
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
async fn synthesis_hit_has_own_identity_and_all_citations() {
    // WHY: the synthesis hit is derived material; borrowing the first
    // source's URL misattributed the synthesis to that source (issue #31).
    let backend = LocalDeepResearch::new();
    let task = backend
        .submit("attribution", DeepDepth::Shallow)
        .await
        .unwrap();
    let fixture = OfflineFixture::new(
        CountingQueryGen,
        CountingRetriever,
        ReflectingSynthesizer { reflect_until: 0 },
    );

    let result = backend
        .execute_offline(&task, &fixture, QueryShape::GeneralResearch)
        .unwrap();

    let synthesis = &result.hits[0];
    assert_eq!(synthesis.title, "local deep research synthesis");
    assert_eq!(synthesis.url.scheme(), "urn");
    for source in &result.hits[1..] {
        assert_ne!(
            synthesis.url, source.url,
            "synthesis must not claim a source's URL"
        );
    }
    assert!(
        !synthesis.citations.is_empty(),
        "synthesis must cite its sources"
    );
}

#[tokio::test]
async fn execute_offline_requires_pending_task() {
    let backend = LocalDeepResearch::new();
    let task = backend
        .submit("conflict", DeepDepth::Shallow)
        .await
        .unwrap();
    backend.cancel_task(&task).unwrap();

    let fixture = OfflineFixture::new(
        CountingQueryGen,
        CountingRetriever,
        ReflectingSynthesizer { reflect_until: 0 },
    );

    let err = backend
        .execute_offline(&task, &fixture, QueryShape::GeneralResearch)
        .unwrap_err();
    assert!(err.is_permanent());
    assert!(err.to_string().contains("cancelled"));
    // The terminal state must be preserved, not overwritten.
    assert_eq!(
        backend.poll(&task).await.unwrap(),
        ResearchStatus::Cancelled
    );
}

struct CancellingRetriever {
    backend: Arc<LocalDeepResearch>,
    task: TaskId,
}

impl SourceRetriever for CancellingRetriever {
    fn retrieve(&self, query: &str, _max_results: usize) -> Vec<ResultHit> {
        // Simulates a concurrent cancel landing while the fixture loop is
        // executing (the loop runs outside the task-store lock).
        self.backend.cancel_task(&self.task).unwrap();
        let url = Url::parse(&format!("https://example.org/?q={query}")).unwrap();
        let citation = Citation::new(url.clone(), ts(), SourceKind::Web, 0.5, None);
        vec![ResultHit::new(query, "s", url, vec![citation], 0.5).unwrap()]
    }
}

#[tokio::test]
async fn execute_offline_discards_result_when_cancelled_mid_run() {
    // WHY: execute_offline is a multi-step mutation; a cancel that lands
    // between claim and commit must win — the stale fixture result must
    // not resurrect the cancelled task (issue #31, race corruption).
    let backend = Arc::new(LocalDeepResearch::new());
    let task = backend.submit("racy", DeepDepth::Shallow).await.unwrap();

    let fixture = OfflineFixture::new(
        CountingQueryGen,
        CancellingRetriever {
            backend: Arc::clone(&backend),
            task: task.clone(),
        },
        ReflectingSynthesizer { reflect_until: 0 },
    );

    let err = backend
        .execute_offline(&task, &fixture, QueryShape::GeneralResearch)
        .unwrap_err();
    assert!(err.is_permanent());
    assert!(err.to_string().contains("result discarded"));
    assert_eq!(
        backend.poll(&task).await.unwrap(),
        ResearchStatus::Cancelled
    );
    assert!(backend.fetch(&task).await.unwrap_err().is_permanent());
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
