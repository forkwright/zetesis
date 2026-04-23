//! Query-shape classifier enum.
//!
//! `QueryShape` is the coarse category a query falls into. The
//! `zetesis-router` classifier (future phase) assigns a shape to every
//! incoming query and uses it to pick the preferred provider and fallback
//! chain. Keeping the shape in the result itself lets the cache key on it
//! and lets callers reason about "what kind of query was this" after the
//! fact.

use serde::{Deserialize, Serialize};

/// High-level category of a research query.
///
/// The shape does not uniquely identify a provider. Multiple providers can
/// serve the same shape (e.g. both OpenAlex and Semantic Scholar serve
/// [`QueryShape::AcademicLiterature`]); the router's job is to pick which one
/// given budget and availability. Callers who know their query shape can
/// annotate it explicitly; if not supplied, the classifier defaults to
/// [`QueryShape::GeneralResearch`].
///
/// # Stability
///
/// `#[non_exhaustive]` — adding a new shape is a minor-version change.
/// Callers must use a default arm (`_`) or explicit fallback to
/// [`QueryShape::GeneralResearch`] when matching.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[non_exhaustive]
#[serde(rename_all = "snake_case")]
pub enum QueryShape {
    /// Short, entity-centric question expecting a single authoritative fact
    /// (capital of France, symbol for gold, current CEO). Routes to
    /// Wikipedia / Wikidata / Crossref DOI lookup first.
    QuickFactual,

    /// Exploratory query looking for conceptually-similar material without
    /// a single correct answer. Semantic-first: Exa, SearXNG semantic mode,
    /// or vector rerank over OpenAlex results.
    SemanticDiscovery,

    /// Scholarly literature query (papers, citations, authors, venues).
    /// Routes to Semantic Scholar / OpenAlex / Crossref / arXiv / PubMed.
    AcademicLiterature,

    /// Patent search (USPTO, EPO, WIPO). Tier 0 coverage via Google Patents
    /// public data is thin; Tier 1 (Exa with patent filter) typically
    /// wins.
    Patent,

    /// Financial filings, market data, analyst reports (SEC EDGAR for
    /// filings, Tier 1 paid providers for the rest).
    Finance,

    /// Legal research (case law, statutes). Free-tier coverage is thin
    /// outside CourtListener; Tier 1 (Lexis-alternatives) for depth.
    Legal,

    /// News / social / status-page query where results older than hours or
    /// days are stale. Free-tier Wikipedia is useless here; routes to
    /// Brave news, Tavily recency mode, or Common Crawl recent slice.
    FreshnessSensitive,

    /// Default bucket when no more specific shape applies. Router treats it
    /// as "try Tier 0 broad, fall through on miss".
    #[default]
    GeneralResearch,

    /// Source-code / package / repository discovery (GitHub search, crates.io,
    /// package registries). Routes to GitHub API + SearXNG code mode.
    CodeAndPackages,

    /// Dataset / benchmark / corpus discovery (HuggingFace Hub, Kaggle,
    /// Zenodo, OpenML).
    DatasetDiscovery,
}

impl QueryShape {
    /// Stable lowercase identifier suitable for cache keys and telemetry.
    ///
    /// This is the same string that `serde` emits, extracted so router /
    /// budget code can reference it without a serialization round-trip.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::QuickFactual => "quick_factual",
            Self::SemanticDiscovery => "semantic_discovery",
            Self::AcademicLiterature => "academic_literature",
            Self::Patent => "patent",
            Self::Finance => "finance",
            Self::Legal => "legal",
            Self::FreshnessSensitive => "freshness_sensitive",
            Self::GeneralResearch => "general_research",
            Self::CodeAndPackages => "code_and_packages",
            Self::DatasetDiscovery => "dataset_discovery",
        }
    }

    /// Whether this shape tolerates cached results older than a few hours.
    ///
    /// Freshness-sensitive queries should bypass or tighten the cache TTL;
    /// scholarly / reference queries can safely reuse day-old cache.
    #[must_use]
    pub const fn tolerates_stale_cache(self) -> bool {
        !matches!(self, Self::FreshnessSensitive | Self::Finance)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_general_research() {
        assert_eq!(QueryShape::default(), QueryShape::GeneralResearch);
    }

    #[test]
    fn as_str_matches_serde_for_every_variant() {
        // WHY: cache keys and telemetry rely on as_str; drift vs. serde
        // would silently invalidate caches. Keep them aligned.
        let shapes = [
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
        for shape in shapes {
            let json = serde_json::to_string(&shape).unwrap();
            let json_str = json.trim_matches('"');
            assert_eq!(json_str, shape.as_str(), "serde/as_str drift for {shape:?}");
        }
    }

    #[test]
    fn serde_round_trip_every_variant() {
        let shapes = [
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
        for shape in shapes {
            let json = serde_json::to_string(&shape).unwrap();
            let back: QueryShape = serde_json::from_str(&json).unwrap();
            assert_eq!(back, shape, "round-trip failed for {shape:?}");
        }
    }

    #[test]
    fn stale_cache_tolerance_matches_intent() {
        assert!(!QueryShape::FreshnessSensitive.tolerates_stale_cache());
        assert!(!QueryShape::Finance.tolerates_stale_cache());
        assert!(QueryShape::AcademicLiterature.tolerates_stale_cache());
        assert!(QueryShape::QuickFactual.tolerates_stale_cache());
    }

    #[test]
    fn unknown_shape_fails_deserialize() {
        let err = serde_json::from_str::<QueryShape>("\"no_such_shape\"");
        assert!(err.is_err(), "unknown tag must fail");
    }

    #[test]
    fn hash_and_eq_are_consistent() {
        use std::collections::HashSet;
        let mut set = HashSet::new();
        set.insert(QueryShape::Finance);
        assert!(set.contains(&QueryShape::Finance));
        assert!(!set.contains(&QueryShape::Patent));
    }
}
