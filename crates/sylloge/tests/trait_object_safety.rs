//! Trait-object-safety integration test.
//!
//! Zetesis-router needs to store providers, deep-research backends, and
//! crawlers in heterogeneous collections (e.g. `Vec<Arc<dyn Provider>>`).
//! That only works if the traits are object-safe. This test file stands
//! up minimal in-memory implementations of every trait and exercises them
//! through their trait-object form. If the hand-rolled `BoxFut` erasure
//! ever breaks object-safety, this test catches it at compile time.

#![expect(clippy::unwrap_used, reason = "test assertions must fail loudly")]

use std::sync::Arc;

use jiff::Timestamp;
use url::Url;

use sylloge::{
    BoxFut, BudgetConstraint, Citation, CostTracking, Crawler, DeepDepth, DeepResearch, Error,
    PageContent, Provider, ProviderSpend, ProviderTier, QueryShape, ResearchResult, ResearchStatus,
    Result, ResultHit, SearchConstraints, SourceKind, TaskId,
};

struct StubProvider;

impl Provider for StubProvider {
    fn name(&self) -> &'static str {
        "stub"
    }

    fn tier(&self) -> ProviderTier {
        ProviderTier::Tier0Free
    }

    fn search<'a>(
        &'a self,
        query: &'a str,
        _constraints: &'a SearchConstraints,
    ) -> BoxFut<'a, Result<ResearchResult>> {
        Box::pin(async move {
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
            )
            .unwrap();
            let cost = CostTracking::from_line_items([ProviderSpend::new("stub", 0, 1, 1)]);
            Ok(ResearchResult::new(
                query,
                QueryShape::QuickFactual,
                vec![hit],
                Vec::new(),
                cost,
                "stub-cache-key",
            ))
        })
    }
}

struct StubDeep;

impl DeepResearch for StubDeep {
    fn name(&self) -> &'static str {
        "stub-deep"
    }

    fn submit<'a>(&'a self, _query: &'a str, depth: DeepDepth) -> BoxFut<'a, Result<TaskId>> {
        Box::pin(async move { Ok(TaskId::new(format!("task-{}", depth.as_str()))) })
    }

    fn poll<'a>(&'a self, _task: &'a TaskId) -> BoxFut<'a, Result<ResearchStatus>> {
        Box::pin(async move {
            Ok(ResearchStatus::Ready {
                completed_at: "2026-04-22T00:00:00Z".parse().unwrap(),
            })
        })
    }

    fn fetch<'a>(&'a self, _task: &'a TaskId) -> BoxFut<'a, Result<ResearchResult>> {
        Box::pin(async move {
            Ok(ResearchResult::empty(
                "stub",
                QueryShape::GeneralResearch,
                "stub-cache",
            ))
        })
    }

    fn cancel<'a>(&'a self, _task: &'a TaskId) -> BoxFut<'a, Result<()>> {
        // WHY: StubDeep is stateless (no task store), so there is nothing
        // to transition -- this only proves `cancel` is reachable through
        // `Arc<dyn DeepResearch>` (see deep_research_is_dyn_compatible).
        // LocalDeepResearch::cancel exercises the real per-state contract.
        Box::pin(async move { Ok(()) })
    }
}

/// Minimal `Crawler` implementing the trait's redirect contract: checks
/// the request URL, and -- if `redirect_to` is set, simulating the
/// `Location` header a fetch returned -- checks the redirect target too
/// before "following" it.
struct StubCrawler {
    redirect_to: Option<Url>,
}

impl Crawler for StubCrawler {
    fn name(&self) -> &'static str {
        "stub-crawler"
    }

    fn fetch_page<'a>(
        &'a self,
        url: &'a Url,
        constraints: &'a SearchConstraints,
    ) -> BoxFut<'a, Result<PageContent>> {
        Box::pin(async move {
            // The trait contract: check the network-target policy on the
            // request URL, then on every redirect target before following
            // it -- a permitted request URL does not extend to wherever it
            // redirects.
            constraints.check_url(url)?;
            let final_url = match &self.redirect_to {
                Some(redirect) => {
                    constraints.check_url(redirect)?;
                    redirect.clone()
                }
                None => url.clone(),
            };
            PageContent::new(
                final_url,
                "text/html",
                b"<html></html>".to_vec(),
                "2026-04-22T00:00:00Z".parse().unwrap(),
            )?
            .with_extracted_text("")
        })
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
    dr.cancel(&task).await.unwrap();
}

#[tokio::test]
async fn crawler_is_dyn_compatible() {
    // WHY: an IP-literal target needs no DNS resolution, keeping this
    // trait-object-shape test independent of live network access.
    let c: Arc<dyn Crawler> = Arc::new(StubCrawler { redirect_to: None });
    let constraints = SearchConstraints::default();
    let page = c
        .fetch_page(&Url::parse("http://8.8.8.8/").unwrap(), &constraints)
        .await
        .unwrap();
    assert!(page.is_html());
    assert_eq!(c.name(), "stub-crawler");
}

#[tokio::test]
async fn crawler_rejects_denied_domain() {
    // WHY: fetch_page receives untrusted provider URLs; the constraints
    // parameter carries the domain-filter policy and a denied host must
    // be rejected with the permanent UnsafeTarget error before any fetch.
    // The denylist entry matches on the host string, which works the same
    // for an IP literal as a domain name -- using one here keeps the test
    // independent of live DNS.
    let c: Arc<dyn Crawler> = Arc::new(StubCrawler { redirect_to: None });
    let constraints = SearchConstraints::default().with_denylist(vec!["8.8.8.8".to_owned()]);
    let err = c
        .fetch_page(&Url::parse("http://8.8.8.8/payload").unwrap(), &constraints)
        .await
        .unwrap_err();
    assert!(err.is_permanent());
    assert!(err.to_string().contains("8.8.8.8"));
}

#[tokio::test]
async fn crawler_rejects_redirect_to_unsafe_target() {
    // WHY: the redirect fixture zetesis#48 requires -- the initial URL
    // alone passing the network-target policy is insufficient. A
    // provider-controlled response that redirects a permitted URL to the
    // cloud metadata endpoint must still fail, proving the Crawler
    // contract's "check every redirect target" requirement is exercised,
    // not just documented.
    let c: Arc<dyn Crawler> = Arc::new(StubCrawler {
        redirect_to: Some(Url::parse("http://169.254.169.254/latest/meta-data/").unwrap()),
    });
    let constraints = SearchConstraints::default();
    let err = c
        .fetch_page(&Url::parse("http://8.8.8.8/").unwrap(), &constraints)
        .await
        .unwrap_err();
    assert!(err.is_permanent());
    assert!(err.to_string().contains("169.254.169.254"));
}

#[tokio::test]
async fn provider_collection_round_trips_over_dyn() {
    // WHY: the router stores heterogeneous providers keyed by name.
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
    // WHY: downstream crates will `use sylloge::Error` and the re-exported
    // snafu selectors rather than reaching into sylloge::error::*. Guard
    // the re-export surface by building and classifying through it.
    let e: Error = sylloge::UnsupportedSnafu {
        reason: "re-export check".to_owned(),
    }
    .build();
    assert!(e.is_permanent());
    assert!(e.to_string().contains("re-export check"));
}
