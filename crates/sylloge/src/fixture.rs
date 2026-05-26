//! Offline fixtures for deterministic deep-research loop tests.
//!
//! The fixture surface models the five local-deep-researcher nodes without
//! network calls: `generate_query`, `web_research`, `summarize_sources`,
//! `reflect_on_summary`, and `finalize_summary`.

use std::collections::BTreeSet;

use crate::{
    CostTracking, DeepDepth, ProvenanceEntry, ProviderSpend, QueryShape, ResearchResult, ResultHit,
};

const FIXTURE_PROVIDER: &str = "offline_fixture";
const MAX_RESULTS_PER_ITERATION: usize = 10;

/// Deterministic seam for generating search queries.
pub trait QueryGenerator: Send + Sync {
    /// Generate the search query for a zero-based loop iteration.
    fn generate(&self, original: &str, iteration: usize) -> String;
}

/// Deterministic seam for retrieving source hits.
pub trait SourceRetriever: Send + Sync {
    /// Retrieve up to `max_results` source hits for `query`.
    fn retrieve(&self, query: &str, max_results: usize) -> Vec<ResultHit>;
}

/// Deterministic seam for summarization, reflection, and finalization.
pub trait Synthesizer: Send + Sync {
    /// Summarize the hits retrieved for a generated query.
    fn summarize(&self, query: &str, sources: &[ResultHit]) -> String;

    /// Return `true` when another research iteration should run.
    fn reflect(&self, summary: &str, iteration: usize) -> bool;

    /// Produce the final synthesized answer text.
    fn finalize(&self, original: &str, all_hits: &[ResultHit], summaries: &[String]) -> String;
}

/// Bundled offline seams that drive the deterministic five-node loop.
#[derive(Debug)]
pub struct OfflineFixture<Q, R, S> {
    /// Query-generation seam.
    pub query_generator: Q,
    /// Source-retrieval seam.
    pub source_retriever: R,
    /// Synthesis and reflection seam.
    pub synthesizer: S,
}

impl<Q, R, S> OfflineFixture<Q, R, S>
where
    Q: QueryGenerator,
    R: SourceRetriever,
    S: Synthesizer,
{
    /// Create an offline fixture from its three seams.
    #[must_use]
    pub const fn new(query_generator: Q, source_retriever: R, synthesizer: S) -> Self {
        Self {
            query_generator,
            source_retriever,
            synthesizer,
        }
    }

    /// Run generate, retrieve, summarize, reflect, and finalize.
    ///
    /// Depth caps the maximum number of retrieval iterations. Reflection can
    /// stop earlier, but it cannot exceed the cap.
    #[must_use]
    pub fn run(&self, original: &str, shape: QueryShape, depth: DeepDepth) -> ResearchResult {
        let max_iterations = max_iterations(depth);
        let mut source_hits = Vec::new();
        let mut summaries = Vec::new();
        let mut provenance = Vec::new();
        let mut cost_spent = CostTracking::default();

        for iteration in 0..max_iterations {
            let generated_query = self.query_generator.generate(original, iteration);
            record_step(&mut cost_spent);

            let hits = self
                .source_retriever
                .retrieve(&generated_query, MAX_RESULTS_PER_ITERATION);
            record_step(&mut cost_spent);

            let summary = self.synthesizer.summarize(&generated_query, &hits);
            record_step(&mut cost_spent);

            append_unique_hits(&mut source_hits, hits);
            append_provenance(&mut provenance, &source_hits);
            summaries.push(summary.clone());

            let needs_more = self.synthesizer.reflect(&summary, iteration);
            record_step(&mut cost_spent);
            if !needs_more {
                break;
            }
        }

        let final_summary = self
            .synthesizer
            .finalize(original, &source_hits, &summaries);
        record_step(&mut cost_spent);

        let hits = with_synthesis_hit(final_summary, source_hits);
        let cache_key = format!(
            "offline|{}|{}|{}|{}",
            original,
            shape.as_str(),
            depth.as_str(),
            hits.len()
        );

        ResearchResult::new(original, shape, hits, provenance, cost_spent, cache_key)
    }
}

fn max_iterations(depth: DeepDepth) -> usize {
    match depth {
        DeepDepth::Shallow => 1,
        DeepDepth::Standard => 2,
        DeepDepth::Deep => 3,
        DeepDepth::Exhaustive => 4,
    }
}

fn record_step(cost_spent: &mut CostTracking) {
    cost_spent.add(ProviderSpend::new(FIXTURE_PROVIDER, 0, 1, 1));
}

fn append_unique_hits(all_hits: &mut Vec<ResultHit>, hits: Vec<ResultHit>) {
    for hit in hits {
        if !all_hits.iter().any(|existing| existing.url == hit.url) {
            all_hits.push(hit);
        }
    }
}

fn append_provenance(provenance: &mut Vec<ProvenanceEntry>, hits: &[ResultHit]) {
    let mut seen: BTreeSet<String> = provenance
        .iter()
        .map(|entry| entry.citation.source_url.as_str().to_owned())
        .collect();

    for citation in hits.iter().flat_map(|hit| hit.citations.iter()) {
        if seen.insert(citation.source_url.as_str().to_owned()) {
            provenance.push(ProvenanceEntry::new(FIXTURE_PROVIDER, citation.clone()));
        }
    }
}

fn with_synthesis_hit(summary: String, mut source_hits: Vec<ResultHit>) -> Vec<ResultHit> {
    let citations = source_hits
        .iter()
        .flat_map(|hit| hit.citations.iter().cloned())
        .collect::<Vec<_>>();

    if let Some(first_citation) = citations.first() {
        let synthesis = ResultHit::new(
            "local deep research synthesis",
            summary,
            first_citation.source_url.clone(),
            citations,
            1.0,
        );
        source_hits.insert(0, synthesis);
    }

    source_hits
}
