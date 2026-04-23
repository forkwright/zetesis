//! Citation and provenance records.
//!
//! Every hit that zetesis surfaces carries at least one [`Citation`]
//! describing where the material came from, when it was fetched, and how
//! confident the provider was in its relevance to the query. The zetesis
//! "no synthesis without citation" principle depends on this type being the
//! single source of provenance truth.

use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use url::Url;

/// Kind of source material a citation points at.
///
/// Used by the router to weight results (a peer-reviewed [`SourceKind::Journal`]
/// hit for an [`super::QueryShape::AcademicLiterature`] query is worth more
/// than a matching [`SourceKind::Web`] hit) and by downstream consumers to
/// decide whether to trust the content without further review.
///
/// `#[non_exhaustive]` — new source kinds are a minor-version change. Keep
/// match arms explicit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[non_exhaustive]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    /// Preprint server (arXiv, bioRxiv, SSRN). Not peer-reviewed.
    Preprint,
    /// Peer-reviewed journal article. Highest confidence for scholarly
    /// shapes.
    Journal,
    /// Generic web page. Lowest provenance weight unless further qualified.
    Web,
    /// Wikipedia or other collaboratively-edited reference wiki.
    Wiki,
    /// Patent document (USPTO / EPO / WIPO).
    Patent,
    /// Legal document (statute, case law, regulation).
    Legal,
    /// Social media post or forum thread. Lowest weight.
    Social,
    /// News article from a news organization's feed.
    News,
    /// Source-code repository or commit (GitHub, GitLab, forge).
    Code,
    /// Dataset description or dataset card (HuggingFace, Zenodo, Kaggle).
    Dataset,
    /// Government filing or official document (SEC EDGAR, Companies House).
    Filing,
    /// Book or book chapter (Google Books, Project Gutenberg).
    Book,
}

impl SourceKind {
    /// Stable lowercase identifier suitable for cache keys and telemetry.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Preprint => "preprint",
            Self::Journal => "journal",
            Self::Web => "web",
            Self::Wiki => "wiki",
            Self::Patent => "patent",
            Self::Legal => "legal",
            Self::Social => "social",
            Self::News => "news",
            Self::Code => "code",
            Self::Dataset => "dataset",
            Self::Filing => "filing",
            Self::Book => "book",
        }
    }

    /// Whether consumers should treat this kind as authoritative without
    /// further corroboration.
    ///
    /// Peer-reviewed, legal, and filing sources are authoritative. Web,
    /// social, and preprint sources are not.
    #[must_use]
    pub const fn is_authoritative(self) -> bool {
        matches!(
            self,
            Self::Journal | Self::Legal | Self::Filing | Self::Patent,
        )
    }
}

/// Provenance record for a single retrieved document.
///
/// Every [`super::ResultHit`] carries one or more citations. The confidence
/// score is the provider's own relevance score normalized to `0.0..=1.0`
/// (providers that do not surface scores must supply `1.0` for hits they
/// consider primary and `0.5` or lower for supplemental material). The
/// `content_type` captures the MIME of the fetched payload so the crawler
/// layer (future phase) can decide whether to run HTML extraction, PDF
/// parsing, or raw text handling.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct Citation {
    /// Canonical URL the material was fetched from. Must be absolute and
    /// parse as a valid URL.
    pub source_url: Url,

    /// Timestamp the provider returned the hit to zetesis.
    pub accessed_at: Timestamp,

    /// Kind of source the URL points at.
    pub source_kind: SourceKind,

    /// Provider-supplied confidence that this citation is relevant to the
    /// query, normalized to `0.0..=1.0`. Values outside the range are
    /// clamped by [`Citation::new`].
    pub confidence: f32,

    /// MIME content type of the payload (e.g. `text/html`, `application/pdf`,
    /// `application/json`). `None` if the provider did not surface one.
    pub content_type: Option<String>,
}

impl Citation {
    /// Construct a citation, clamping `confidence` into `0.0..=1.0`.
    ///
    /// Clamping rather than rejecting is deliberate: providers return
    /// out-of-range scores surprisingly often (Semantic Scholar has been
    /// observed emitting `1.0000001`), and a hard failure at this boundary
    /// would drop otherwise-valid hits. The clamped value is always a
    /// conservative approximation.
    #[must_use]
    pub fn new(
        source_url: Url,
        accessed_at: Timestamp,
        source_kind: SourceKind,
        confidence: f32,
        content_type: Option<String>,
    ) -> Self {
        let confidence = if confidence.is_nan() {
            0.0
        } else {
            confidence.clamp(0.0, 1.0)
        };
        Self {
            source_url,
            accessed_at,
            source_kind,
            confidence,
            content_type,
        }
    }

    /// Whether this citation carries an authoritative source kind with
    /// high provider confidence (`>= 0.8`).
    #[must_use]
    pub fn is_strong(&self) -> bool {
        self.source_kind.is_authoritative() && self.confidence >= 0.8
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_url() -> Url {
        Url::parse("https://example.org/paper").unwrap()
    }

    fn sample_ts() -> Timestamp {
        "2026-04-22T00:00:00Z".parse().unwrap()
    }

    #[test]
    fn source_kind_round_trip() {
        let kinds = [
            SourceKind::Preprint,
            SourceKind::Journal,
            SourceKind::Web,
            SourceKind::Wiki,
            SourceKind::Patent,
            SourceKind::Legal,
            SourceKind::Social,
            SourceKind::News,
            SourceKind::Code,
            SourceKind::Dataset,
            SourceKind::Filing,
            SourceKind::Book,
        ];
        for kind in kinds {
            let json = serde_json::to_string(&kind).unwrap();
            let back: SourceKind = serde_json::from_str(&json).unwrap();
            assert_eq!(back, kind);
            assert_eq!(json.trim_matches('"'), kind.as_str());
        }
    }

    #[test]
    fn authoritative_set_is_closed() {
        assert!(SourceKind::Journal.is_authoritative());
        assert!(SourceKind::Legal.is_authoritative());
        assert!(SourceKind::Filing.is_authoritative());
        assert!(SourceKind::Patent.is_authoritative());
        assert!(!SourceKind::Web.is_authoritative());
        assert!(!SourceKind::Preprint.is_authoritative());
        assert!(!SourceKind::Social.is_authoritative());
    }

    #[test]
    fn new_clamps_high_confidence() {
        let c = Citation::new(sample_url(), sample_ts(), SourceKind::Journal, 1.5, None);
        assert!((c.confidence - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn new_clamps_low_confidence() {
        let c = Citation::new(sample_url(), sample_ts(), SourceKind::Web, -0.25, None);
        assert!(c.confidence.abs() < f32::EPSILON);
    }

    #[test]
    fn new_maps_nan_to_zero() {
        let c = Citation::new(sample_url(), sample_ts(), SourceKind::Web, f32::NAN, None);
        assert!(c.confidence.abs() < f32::EPSILON);
    }

    #[test]
    fn is_strong_requires_both_conditions() {
        let journal_high = Citation::new(sample_url(), sample_ts(), SourceKind::Journal, 0.9, None);
        assert!(journal_high.is_strong());

        let journal_low = Citation::new(sample_url(), sample_ts(), SourceKind::Journal, 0.5, None);
        assert!(!journal_low.is_strong());

        let web_high = Citation::new(sample_url(), sample_ts(), SourceKind::Web, 1.0, None);
        assert!(!web_high.is_strong());
    }

    #[test]
    fn citation_serde_round_trip_json() {
        let c = Citation::new(
            sample_url(),
            sample_ts(),
            SourceKind::Journal,
            0.87,
            Some("application/pdf".to_owned()),
        );
        let json = serde_json::to_string(&c).unwrap();
        let back: Citation = serde_json::from_str(&json).unwrap();
        assert_eq!(back, c);
    }
}
