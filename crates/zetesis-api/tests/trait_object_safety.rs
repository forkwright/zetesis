//! Trait-object-safety integration test.
//!
//! Zetesis-router needs to store providers, deep-research backends, and
//! crawlers in heterogeneous collections (e.g. `Vec<Arc<dyn Provider>>`).
//! That only works if the traits are object-safe. This test file stands
//! up minimal in-memory implementations of every trait and exercises them
//! through their trait-object form. If async-trait's erasure ever breaks
//! object-safety, this test catches it at compile time.

#![allow(clippy::unwrap_used)]
#![allow(clippy::expect_used)]

use std::sync::Arc;

use async_trait::async_trait;
use jiff::Timestamp;
use url::Url;

use zetesis_api::{
    BudgetConstraint, Citation, CostTracking, Crawler, DeepDepth, DeepResearch, Error, PageContent,
    Provider, ProviderSpend, ProviderTier, QueryShape, ResearchResult, ResearchStatus, Result,
    ResultHit, SearchConstraints, SourceKind, TaskId,
};

struct StubProvider;

#[async_trait]
impl Provider for StubProvider {
    fn name(&self) -> &'static str {
        "stub"
    }

    fn tier(&self) -> ProviderTier {
        ProviderTier::Tier0Free
    }

    async fn search(
        &self,
        query: &str,
        _constraints: &SearchConstraints,
    ) -> Result<ResearchResult> {
        let ts: Timestamp = "2026-04-22T00:00:00Z".parse().unwrap();
        let citation = Citation::new(
            Url::parse("https://example.org/stub").unwrap(),
            ts,
            SourceKind::Wiki,
            1.0,
            Some("text/html".to_owned()),
        );
        let hit = ResultHit::new(
            "stub title",
            "stub snippet",
            Url::parse("https://example.org/stub").unwrap(),
            vec![citation],
            1.0,
        );
        let cost = CostTracking::from_line_items([ProviderSpend::new("stub", 0, 1, 1)]);
        Ok(ResearchResult::new(
            query,
            QueryShape::QuickFactual,
            vec![hit],
            Vec::new(),
            cost,
            "stub-cache-key",
        ))
    }
}

struct StubDeep;

#[async_trait]
impl DeepResearch for StubDeep {
    fn name(&self) -> &'static str {
        "stub-deep"
    }

    async fn submit(&self, _query: &str, depth: DeepDepth) -> Result<TaskId> {
        Ok(TaskId::new(format!("task-{}", depth.as_str())))
    }

    async fn poll(&self, _task: &TaskId) -> Result<ResearchStatus> {
        Ok(ResearchStatus::Ready {
            completed_at: "2026-04-22T00:00:00Z".parse().unwrap(),
        })
    }

    async fn fetch(&self, _task: &TaskId) -> Result<ResearchResult> {
        Ok(ResearchResult::empty(
            "stub",
            QueryShape::GeneralResearch,
            "stub-cache",
        ))
    }
}

struct StubCrawler;

#[async_trait]
impl Crawler for StubCrawler {
    fn name(&self) -> &'static str {
        "stub-crawler"
    }

    async fn fetch_page(&self, url: &Url) -> Result<PageContent> {
        Ok(PageContent::new(
            url.clone(),
            "text/html",
            b"<html></html>".to_vec(),
            "2026-04-22T00:00:00Z".parse().unwrap(),
        )
        .with_extracted_text(""))
    }
}

#[tokio::test]
async fn provider_is_dyn_compatible() {
    let providers: Vec<Arc<dyn Provider>> = vec![Arc::new(StubProvider)];
    let constraints = SearchConstraints::new(5, BudgetConstraint::default());
    let out = providers[0].search("q", &constraints).await.unwrap();
    assert_eq!(out.query, "q");
    assert_eq!(providers[0].name(), "stub");
    assert_eq!(providers[0].tier(), ProviderTier::Tier0Free);
}

#[tokio::test]
async fn deep_research_is_dyn_compatible() {
    let dr: Arc<dyn DeepResearch> = Arc::new(StubDeep);
    let task = dr.submit("q", DeepDepth::Deep).await.unwrap();
    assert_eq!(task.as_str(), "task-deep");
    let status = dr.poll(&task).await.unwrap();
    assert!(status.is_ready());
    let result = dr.fetch(&task).await.unwrap();
    assert_eq!(result.hits.len(), 0);
    assert_eq!(dr.name(), "stub-deep");
}

#[tokio::test]
async fn crawler_is_dyn_compatible() {
    let c: Arc<dyn Crawler> = Arc::new(StubCrawler);
    let page = c
        .fetch_page(&Url::parse("https://example.org/").unwrap())
        .await
        .unwrap();
    assert!(page.is_html());
    assert_eq!(c.name(), "stub-crawler");
}

#[tokio::test]
async fn provider_collection_round_trips_over_dyn() {
    // WHY: zetesis-router stores heterogeneous providers keyed by name.
    // The shape below is the router's exact storage pattern; if any
    // trait object bound ever slips (e.g. a change that adds a Sized
    // method), this test breaks at compile time.
    let providers: Vec<Arc<dyn Provider + Send + Sync>> =
        vec![Arc::new(StubProvider), Arc::new(StubProvider)];
    for p in &providers {
        assert_eq!(p.name(), "stub");
    }
    let constraints = SearchConstraints::default();
    let futures = providers
        .iter()
        .map(|p| p.search("concurrent", &constraints));
    let results = futures::future::join_all(futures).await;
    assert_eq!(results.len(), 2);
    for r in results {
        let out = r.unwrap();
        assert_eq!(out.query, "concurrent");
    }
}

#[test]
fn error_types_are_reachable_through_re_exports() {
    // WHY: downstream crates will `use zetesis_api::Error` rather than
    // reaching into zetesis_api::error::*. Guard the re-export surface.
    fn assert_error_type<E>() {}
    assert_error_type::<Error>();
}
