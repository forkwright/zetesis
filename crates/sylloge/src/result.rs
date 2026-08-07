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
use crate::cost::{CostTracking, ProviderId};
use crate::error::{MissingCitationsSnafu, OversizedPayloadSnafu, Result};
use crate::freshness::FreshnessDecision;
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
    /// not bodies. Capped at [`ResultHit::MAX_FULL_TEXT_BYTES`] by
    /// [`ResultHit::with_full_text`].
    pub full_text: Option<String>,

    /// Provenance records — must have at least one entry (enforced by
    /// [`ResultHit::new`] and at the deserialization boundary). Multiple
    /// entries appear when the same hit was corroborated across providers
    /// (e.g. Semantic Scholar + Crossref both returned the same DOI).
    #[serde(deserialize_with = "non_empty_citations")]
    pub citations: Vec<Citation>,

    /// Provider-supplied relevance score, normalized to `0.0..=1.0`
    /// ([`ResultHit::new`] and the deserialization boundary clamp).
    /// Higher = more relevant.
    #[serde(deserialize_with = "crate::serde_util::clamp_unit_f32")]
    pub score: f32,

    /// Extra provider-specific metadata the normalizer chose to pass through
    /// (DOI, author list, venue, publication year, etc.). Keyed alphabetically
    /// for deterministic serialization.
    pub metadata: BTreeMap<String, Value>,

    /// The freshness-enforcement receipt, if a
    /// [`crate::SearchConstraints::freshness_window`] was evaluated against
    /// this hit's primary citation. `None` when no freshness window was
    /// configured or evaluation has not run. Set via
    /// [`ResultHit::with_freshness`]. `#[serde(default)]` so payloads
    /// persisted before this field existed decode as `None`.
    #[serde(default)]
    pub freshness: Option<FreshnessDecision>,
}

impl ResultHit {
    /// Maximum accepted `full_text` size in bytes. Untrusted provider
    /// payloads beyond this cap are rejected rather than buffered (see
    /// [`crate::PageContent::MAX_BODY_BYTES`] for the crawler-side cap).
    pub const MAX_FULL_TEXT_BYTES: usize = 4 * 1024 * 1024;

    /// Construct a hit, clamping `score` into `0.0..=1.0` (see
    /// [`Citation::new`] for the rationale; providers surprisingly often
    /// emit slightly-out-of-range scores).
    ///
    /// # Errors
    ///
    /// Returns [`crate::Error::MissingCitations`] when `citations` is
    /// empty — the "no synthesis without source provenance" invariant is
    /// enforced at the one place callers construct a hit.
    ///
    /// [`Citation::new`]: crate::Citation::new
    pub fn new(
        title: impl Into<String>,
        snippet: impl Into<String>,
        url: Url,
        citations: Vec<Citation>,
        score: f32,
    ) -> Result<Self> {
        let title = title.into();
        snafu::ensure!(
            !citations.is_empty(),
            MissingCitationsSnafu {
                title: title.clone()
            }
        );
        let score = if score.is_nan() {
            0.0
        } else {
            score.clamp(0.0, 1.0)
        };
        Ok(Self {
            title,
            snippet: snippet.into(),
            url,
            full_text: None,
            citations,
            score,
            metadata: BTreeMap::new(),
            freshness: None,
        })
    }

    /// Builder-style: attach full-text body.
    ///
    /// # Errors
    ///
    /// Returns [`crate::Error::OversizedPayload`] when `text` exceeds
    /// [`ResultHit::MAX_FULL_TEXT_BYTES`].
    pub fn with_full_text(mut self, text: impl Into<String>) -> Result<Self> {
        let text = text.into();
        snafu::ensure!(
            text.len() <= Self::MAX_FULL_TEXT_BYTES,
            OversizedPayloadSnafu {
                what: "hit full_text",
                len: text.len(),
                max: Self::MAX_FULL_TEXT_BYTES,
            }
        );
        self.full_text = Some(text);
        Ok(self)
    }

    /// Builder-style: attach a metadata key.
    #[must_use]
    pub fn with_metadata(mut self, key: impl Into<String>, value: Value) -> Self {
        self.metadata.insert(key.into(), value);
        self
    }

    /// Builder-style: attach a freshness-enforcement receipt (see
    /// [`ResultHit::freshness`]).
    #[must_use]
    pub fn with_freshness(mut self, decision: FreshnessDecision) -> Self {
        self.freshness = Some(decision);
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

/// Reject an empty citations vector at the deserialization boundary.
///
/// WHY: `citations` is a pub field, so serde is a second construction
/// path; without this check a persisted or provider-supplied uncited hit
/// would silently bypass the [`ResultHit::new`] invariant.
fn non_empty_citations<'de, D>(deserializer: D) -> std::result::Result<Vec<Citation>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let citations = Vec::<Citation>::deserialize(deserializer)?;
    if citations.is_empty() {
        return Err(serde::de::Error::invalid_length(
            0,
            &"at least one citation per hit",
        ));
    }
    Ok(citations)
}

/// Total order for relevance scores: NaN ranks below every comparable
/// value, so a NaN-scored hit can never displace a legitimately-scored one.
fn cmp_score(a: f32, b: f32) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    match (a.is_nan(), b.is_nan()) {
        (true, true) => Ordering::Equal,
        (true, false) => Ordering::Less,
        (false, true) => Ordering::Greater,
        // INVARIANT: partial_cmp on two non-NaN f32 values always succeeds.
        (false, false) => a.partial_cmp(&b).unwrap_or(Ordering::Equal),
    }
}

/// Top-level envelope returned by a research call.
///
/// Contains the caller's query (echoed so consumers can round-trip the
/// request without holding state), the ordered list of hits, a chain of
/// (`provider_id` → citation) entries describing which providers were tried
/// in what order, the call-level cost ledger, and a stable cache key that
/// the future cache layer derives from the query + shape + constraint digest.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
// kanon:ignore RUST/no-debug-derive-on-public-types -- cache_key is a derived cache lookup key, not a credential; nothing to redact
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

    /// Stable cache key, derived by the future cache layer from the query plus
    /// shape plus a constraint digest. Two calls with identical query,
    /// shape, and constraints produce identical `cache_key`. Format is
    /// provider-layer opaque.
    pub cache_key: String, // kanon:ignore RUST/plain-string-secret -- derived cache lookup key, not a credential
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
    ///
    /// NaN scores (reachable only via direct field mutation — construction
    /// and deserialization both clamp) rank below every comparable score,
    /// so a NaN-scored hit is returned only when every hit is NaN-scored.
    #[must_use]
    pub fn top_hit(&self) -> Option<&ResultHit> {
        self.hits.iter().max_by(|a, b| cmp_score(a.score, b.score))
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
    pub provider_id: ProviderId,
    /// Citation the provider surfaced (or a synthetic "miss" citation for
    /// providers that were attempted but returned no hits).
    pub citation: Citation,
}

impl ProvenanceEntry {
    /// Construct a provenance entry.
    #[must_use]
    pub fn new(provider_id: impl Into<ProviderId>, citation: Citation) -> Self {
        Self {
            provider_id: provider_id.into(),
            citation,
        }
    }
}

#[cfg(test)]
mod tests {
    use jiff::Timestamp;

    use super::*;
    use crate::citation::SourceKind;
    use crate::cost::ProviderSpend;

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

    fn hit(title: &str, score: f32) -> ResultHit {
        ResultHit::new(
            title,
            "s",
            url("https://example.org/"),
            vec![sample_citation()],
            score,
        )
        .unwrap()
    }

    #[test]
    fn new_clamps_hit_score() {
        let hit = hit("t", 1.5);
        assert!((hit.score - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn new_clamps_nan_score() {
        let hit = hit("t", f32::NAN);
        assert!(hit.score.abs() < f32::EPSILON);
    }

    #[test]
    fn new_rejects_empty_citations() {
        // WHY: the "no synthesis without citation" invariant must be
        // unforgeable through the safe constructor.
        let err =
            ResultHit::new("t", "s", url("https://example.org/"), Vec::new(), 0.9).unwrap_err();
        assert!(err.is_permanent());
        assert!(err.to_string().contains("no citations"));
    }

    #[test]
    fn deserialization_rejects_empty_citations() {
        let hit = hit("t", 0.5);
        let mut value = serde_json::to_value(&hit).unwrap();
        value["citations"] = serde_json::Value::Array(Vec::new());
        let result: std::result::Result<ResultHit, _> = serde_json::from_value(value);
        assert!(result.is_err());
    }

    #[test]
    fn deserialization_clamps_out_of_range_score() {
        let hit = hit("t", 0.5);
        let mut value = serde_json::to_value(&hit).unwrap();
        value["score"] = serde_json::json!(42.0);
        let back: ResultHit = serde_json::from_value(value).unwrap();
        assert!((back.score - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn with_full_text_builder() {
        let hit = hit("t", 0.8).with_full_text("body").unwrap();
        assert_eq!(hit.full_text.as_deref(), Some("body"));
    }

    #[test]
    fn with_full_text_rejects_oversized_payload() {
        let oversized = "x".repeat(ResultHit::MAX_FULL_TEXT_BYTES + 1);
        let err = hit("t", 0.8).with_full_text(oversized).unwrap_err();
        assert!(err.is_permanent());
        assert!(err.to_string().contains("full_text"));
    }

    #[test]
    fn with_metadata_preserves_ordering() {
        let hit = hit("t", 0.8)
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
    fn new_hit_has_no_freshness_receipt_until_evaluated() {
        assert!(hit("t", 0.8).freshness.is_none());
    }

    #[test]
    fn with_freshness_attaches_the_receipt() {
        use crate::freshness::{FreshnessBasis, FreshnessDecision, FreshnessPolicy};

        let decision = FreshnessDecision {
            accepted: false,
            policy: FreshnessPolicy::Strict,
            basis: FreshnessBasis::UnknownRejected,
        };
        let hit = hit("t", 0.8).with_freshness(decision);
        assert_eq!(hit.freshness, Some(decision));
    }

    #[test]
    fn has_strong_citation_true_for_authoritative_high() {
        let hit = hit("t", 0.9);
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
        )
        .unwrap();
        assert!(!hit.has_strong_citation());
    }

    fn sample_result() -> ResearchResult {
        let hit_a = ResultHit::new(
            "Paper A",
            "abstract",
            url("https://example.org/a"),
            vec![sample_citation()],
            0.9,
        )
        .unwrap();
        let hit_b = ResultHit::new(
            "Paper B",
            "abstract",
            url("https://example.org/b"),
            vec![sample_citation()],
            0.7,
        )
        .unwrap();
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
    fn top_hit_ignores_nan_scored_hits() {
        // WHY: `max_by` returns the LAST element among ties, so a
        // comparator mapping NaN comparisons to Equal would let a
        // trailing NaN-scored hit displace the true maximum. The scores
        // [0.9, 0.3, NaN] must yield the 0.9 hit.
        let mut nan_hit = hit("NaN hit", 0.5);
        nan_hit.score = f32::NAN;
        let r = ResearchResult::new(
            "q",
            QueryShape::GeneralResearch,
            vec![hit("Best", 0.9), hit("Mid", 0.3), nan_hit],
            Vec::new(),
            CostTracking::default(),
            "k",
        );
        assert_eq!(r.top_hit().unwrap().title, "Best");
    }

    #[test]
    fn top_hit_all_nan_still_returns_a_hit() {
        let mut a = hit("A", 0.5);
        a.score = f32::NAN;
        let mut b = hit("B", 0.5);
        b.score = f32::NAN;
        let r = ResearchResult::new(
            "q",
            QueryShape::GeneralResearch,
            vec![a, b],
            Vec::new(),
            CostTracking::default(),
            "k",
        );
        assert!(r.top_hit().is_some());
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
        let hit = hit("title", 0.5)
            .with_full_text("body")
            .unwrap()
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
