//! Fixture-backed lifecycle tests for the local deep-research backend slot.

#![allow(clippy::unwrap_used)]

use jiff::Timestamp;
use url::Url;

use sylloge::{
    Citation, CostTracking, DeepDepth, DeepResearch, LocalDeepResearch, ProviderSpend, QueryShape,
    ResearchResult, ResearchStatus, ResultHit, SourceKind,
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
