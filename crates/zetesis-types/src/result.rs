//! Research result schema.
//!
//! The normalized envelope every provider returns. `ResearchResult` is the
//! single type downstream consumers (aletheia nous, dioptron, akroasis) see
//! regardless of which Tier-0/1/2/3 provider actually served the query. The
//! provenance trail records which providers were tried in what order so
//! fallback-chain decisions are auditable after the fact.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use url::Url;

use crate::citation::Citation;
use crate::cost::CostTracking;
use crate::query::QueryShape;

/// Single result item from a research call.
///
/// Providers fill the fields they can (Tier 0 academic providers typically
/// fill `title` + `snippet` + `url` + at least one `citation`; a Tier 1
/// web provider might additionally fill `full_text`). Downstream consumers
/// must handle missing optional fields gracefully.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct ResultHit {
    /// Display title (paper title, page title, document title).
    pub title: String,

    /// Short excerpt or abstract. Providers that don't surface one must
    /// leave this empty rather than fabricating one.
    pub snippet: String,

    /// Primary landing URL for the hit.
    pub url: Url,

    /// Full extracted text, if the provider (or crawler) resolved one.
    /// Kept `Option` because Tier 0 academic providers return abstracts,
    /// not bodies.
    pub full_text: Option<String>,

    /// Provenance records — must have at least one entry. Multiple entries
    /// appear when the same hit was corroborated across providers (e.g.
    /// Semantic Scholar + Crossref both returned the same DOI).
    pub citations: Vec<Citation>,

    /// Provider-supplied relevance score, normalized to `0.0..=1.0`
    /// ([`ResultHit::new`] clamps). Higher = more relevant.
    pub score: f32,

    /// Extra provider-specific metadata the normalizer chose to pass through
    /// (DOI, author list, venue, publication year, etc.). Keyed alphabetically
    /// for deterministic serialization.
    pub metadata: BTreeMap<String, Value>,
}

impl ResultHit {
    /// Construct a hit, clamping `score` into `0.0..=1.0` (see
    /// [`Citation::new`] for the rationale; providers surprisingly often
    /// emit slightly-out-of-range scores).
    ///
    /// [`Citation::new`]: crate::Citation::new
    #[must_use]
    pub fn new(
        title: impl Into<String>,
        snippet: impl Into<String>,
        url: Url,
        citations: Vec<Citation>,
        score: f32,
    ) -> Self {
        let score = if score.is_nan() {
            0.0
        } else {
            score.clamp(0.0, 1.0)
        };
        Self {
            title: title.into(),
            snippet: snippet.into(),
            url,
            full_text: None,
            citations,
            score,
            metadata: BTreeMap::new(),
        }
    }

    /// Builder-style: attach full-text body.
    #[must_use]
    pub fn with_full_text(mut self, text: impl Into<String>) -> Self {
        self.full_text = Some(text.into());
        self
    }

    /// Builder-style: attach a metadata key.
    #[must_use]
    pub fn with_metadata(mut self, key: impl Into<String>, value: Value) -> Self {
        self.metadata.insert(key.into(), value);
        self
    }

    /// Whether this hit has at least one citation marked strong by
    /// [`Citation::is_strong`].
    ///
    /// [`Citation::is_strong`]: crate::Citation::is_strong
    #[must_use]
    pub fn has_strong_citation(&self) -> bool {
        self.citations.iter().any(Citation::is_strong)
    }
}

/// Top-level envelope returned by a research call.
///
/// Contains the caller's query (echoed so consumers can round-trip the
/// request without holding state), the ordered list of hits, a chain of
/// (provider_id → citation) entries describing which providers were tried
/// in what order, the call-level cost ledger, and a stable cache key that
/// `zetesis-cache` derives from the query + shape + constraint digest.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct ResearchResult {
    /// The original query string as supplied by the caller.
    pub query: String,

    /// Shape the classifier assigned this query. Matches the shape the
    /// caller supplied, or [`QueryShape::GeneralResearch`] if unset.
    pub shape: QueryShape,

    /// Ordered hits (highest `score` first by convention — providers that
    /// don't surface scores must order by their own relevance ranking).
    pub hits: Vec<ResultHit>,

    /// Ordered chain of `(provider_id, citation)` describing which
    /// providers touched this query in what sequence. Failed attempts
    /// appear in this chain too.
    pub provenance: Vec<ProvenanceEntry>,

    /// Aggregated cost / quota ledger for this call.
    pub cost_spent: CostTracking,

    /// Stable cache key, derived by `zetesis-cache` from the query plus
    /// shape plus a constraint digest. Two calls with identical query,
    /// shape, and constraints produce identical `cache_key`. Format is
    /// provider-layer opaque.
    pub cache_key: String,
}

impl ResearchResult {
    /// Minimal constructor; the full-builder form would add per-hit /
    /// per-provenance helpers but this is Phase 1a scope.
    #[must_use]
    pub fn new(
        query: impl Into<String>,
        shape: QueryShape,
        hits: Vec<ResultHit>,
        provenance: Vec<ProvenanceEntry>,
        cost_spent: CostTracking,
        cache_key: impl Into<String>,
    ) -> Self {
        Self {
            query: query.into(),
            shape,
            hits,
            provenance,
            cost_spent,
            cache_key: cache_key.into(),
        }
    }

    /// Empty result for a query, useful for provider-miss bookkeeping.
    #[must_use]
    pub fn empty(
        query: impl Into<String>,
        shape: QueryShape,
        cache_key: impl Into<String>,
    ) -> Self {
        Self {
            query: query.into(),
            shape,
            hits: Vec::new(),
            provenance: Vec::new(),
            cost_spent: CostTracking::default(),
            cache_key: cache_key.into(),
        }
    }

    /// Highest-scoring hit, if any.
    #[must_use]
    pub fn top_hit(&self) -> Option<&ResultHit> {
        self.hits.iter().max_by(|a, b| {
            a.score
                .partial_cmp(&b.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        })
    }

    /// Whether any hit carries a strong citation (see
    /// [`ResultHit::has_strong_citation`]).
    #[must_use]
    pub fn any_strong_citation(&self) -> bool {
        self.hits.iter().any(ResultHit::has_strong_citation)
    }

    /// Number of distinct providers that appeared in the provenance chain.
    #[must_use]
    pub fn provider_count(&self) -> usize {
        let mut ids: Vec<&str> = self
            .provenance
            .iter()
            .map(|p| p.provider_id.as_str())
            .collect();
        ids.sort_unstable();
        ids.dedup();
        ids.len()
    }
}

/// Single entry in the `provenance` chain.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct ProvenanceEntry {
    /// Stable provider identifier (matches `Provider::name()`).
    pub provider_id: String,
    /// Citation the provider surfaced (or a synthetic "miss" citation for
    /// providers that were attempted but returned no hits).
    pub citation: Citation,
}

impl ProvenanceEntry {
    /// Construct a provenance entry.
    #[must_use]
    pub fn new(provider_id: impl Into<String>, citation: Citation) -> Self {
        Self {
            provider_id: provider_id.into(),
            citation,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::citation::SourceKind;
    use crate::cost::ProviderSpend;
    use jiff::Timestamp;

    fn url(s: &str) -> Url {
        Url::parse(s).unwrap()
    }

    fn ts() -> Timestamp {
        "2026-04-22T10:00:00Z".parse().unwrap()
    }

    fn sample_citation() -> Citation {
        Citation::new(
            url("https://example.org/a"),
            ts(),
            SourceKind::Journal,
            0.95,
            Some("application/pdf".to_owned()),
        )
    }

    #[test]
    fn new_clamps_hit_score() {
        let hit = ResultHit::new(
            "t",
            "s",
            url("https://example.org/"),
            vec![sample_citation()],
            1.5,
        );
        assert!((hit.score - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn new_clamps_nan_score() {
        let hit = ResultHit::new(
            "t",
            "s",
            url("https://example.org/"),
            vec![sample_citation()],
            f32::NAN,
        );
        assert!(hit.score.abs() < f32::EPSILON);
    }

    #[test]
    fn with_full_text_builder() {
        let hit = ResultHit::new(
            "t",
            "s",
            url("https://example.org/"),
            vec![sample_citation()],
            0.8,
        )
        .with_full_text("body");
        assert_eq!(hit.full_text.as_deref(), Some("body"));
    }

    #[test]
    fn with_metadata_preserves_ordering() {
        let hit = ResultHit::new(
            "t",
            "s",
            url("https://example.org/"),
            vec![sample_citation()],
            0.8,
        )
        .with_metadata("doi", Value::String("10.1/abc".to_owned()))
        .with_metadata("year", Value::Number(2026.into()));

        // BTreeMap orders keys alphabetically, so serialization is stable.
        let json = serde_json::to_value(&hit.metadata).unwrap();
        let keys: Vec<&str> = json
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(keys, ["doi", "year"]);
    }

    #[test]
    fn has_strong_citation_true_for_authoritative_high() {
        let hit = ResultHit::new(
            "t",
            "s",
            url("https://example.org/"),
            vec![sample_citation()],
            0.9,
        );
        assert!(hit.has_strong_citation());
    }

    #[test]
    fn has_strong_citation_false_for_web_only() {
        let hit = ResultHit::new(
            "t",
            "s",
            url("https://example.org/"),
            vec![Citation::new(
                url("https://example.org/p"),
                ts(),
                SourceKind::Web,
                1.0,
                None,
            )],
            0.9,
        );
        assert!(!hit.has_strong_citation());
    }

    fn sample_result() -> ResearchResult {
        let hit_a = ResultHit::new(
            "Paper A",
            "abstract",
            url("https://example.org/a"),
            vec![sample_citation()],
            0.9,
        );
        let hit_b = ResultHit::new(
            "Paper B",
            "abstract",
            url("https://example.org/b"),
            vec![sample_citation()],
            0.7,
        );
        let provenance = vec![
            ProvenanceEntry::new("semantic_scholar", sample_citation()),
            ProvenanceEntry::new("crossref", sample_citation()),
        ];
        let cost = CostTracking::from_line_items([
            ProviderSpend::new("semantic_scholar", 0, 1, 1),
            ProviderSpend::new("crossref", 0, 1, 1),
        ]);
        ResearchResult::new(
            "attention is all you need",
            QueryShape::AcademicLiterature,
            vec![hit_a, hit_b],
            provenance,
            cost,
            "cache-key-hash".to_owned(),
        )
    }

    #[test]
    fn top_hit_returns_highest_score() {
        let r = sample_result();
        let top = r.top_hit().unwrap();
        assert_eq!(top.title, "Paper A");
    }

    #[test]
    fn top_hit_none_for_empty() {
        let r = ResearchResult::empty("x", QueryShape::QuickFactual, "k");
        assert!(r.top_hit().is_none());
    }

    #[test]
    fn provider_count_dedups() {
        let r = sample_result();
        assert_eq!(r.provider_count(), 2);
    }

    #[test]
    fn any_strong_citation_checks_any_hit() {
        let r = sample_result();
        assert!(r.any_strong_citation());
    }

    #[test]
    fn empty_has_no_strong_citation() {
        let r = ResearchResult::empty("x", QueryShape::QuickFactual, "k");
        assert!(!r.any_strong_citation());
    }

    #[test]
    fn research_result_serde_round_trip_json() {
        let r = sample_result();
        let json = serde_json::to_string(&r).unwrap();
        let back: ResearchResult = serde_json::from_str(&json).unwrap();
        assert_eq!(back, r);
    }

    #[test]
    fn research_result_ciborium_round_trip() {
        // WHY: ciborium is the mandated binary codec (STANDARDS.md §
        // binary serialization). Every persisted type must round-trip
        // through it without loss.
        let r = sample_result();
        let mut buf = Vec::new();
        ciborium::into_writer(&r, &mut buf).unwrap();
        let back: ResearchResult = ciborium::from_reader(buf.as_slice()).unwrap();
        assert_eq!(back, r);
    }

    #[test]
    fn hit_ciborium_round_trip() {
        let hit = ResultHit::new(
            "title",
            "snippet",
            url("https://example.org/"),
            vec![sample_citation()],
            0.5,
        )
        .with_full_text("body")
        .with_metadata("k", Value::String("v".to_owned()));
        let mut buf = Vec::new();
        ciborium::into_writer(&hit, &mut buf).unwrap();
        let back: ResultHit = ciborium::from_reader(buf.as_slice()).unwrap();
        assert_eq!(back, hit);
    }

    #[test]
    fn provenance_entry_round_trip() {
        let p = ProvenanceEntry::new("openalex", sample_citation());
        let json = serde_json::to_string(&p).unwrap();
        let back: ProvenanceEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(back, p);
    }
}
