//! Per-call constraints and the value types that travel with them.
//!
//! [`SearchConstraints`] is passed into every [`super::Provider::search`]
//! call. It captures caller intent that isn't already in the query string
//! itself: budget ceiling, freshness window, allowed languages, domain
//! allow/deny lists.
//!
//! [`DeepDepth`], [`ResearchStatus`], and [`TaskId`] are value types used
//! by the [`super::DeepResearch`] trait for its asynchronous task
//! lifecycle. [`PageContent`] is the normalized output of a single
//! [`super::Crawler::fetch_page`] call.

use std::time::Duration;

use jiff::Timestamp;
use language_tags::LanguageTag;
use serde::{Deserialize, Serialize};
use snafu::ensure;
use url::Url;

use crate::BudgetConstraint;
use crate::error::{OversizedPayloadSnafu, Result};

/// Per-call constraints supplied by the caller.
///
/// Every field is optional except `max_results` and `budget`. Unset fields
/// mean "no constraint" (the provider's default window / language / domain
/// scope applies).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct SearchConstraints {
    /// Maximum number of hits the provider may return. Providers that
    /// can't honour this exactly must return at most this many.
    pub max_results: usize,

    /// Only accept hits whose `accessed_at` (or the provider's equivalent
    /// publish timestamp) falls within the last `freshness_window`. `None`
    /// means no freshness filter.
    pub freshness_window: Option<Duration>,

    /// Preferred content language (BCP-47 language tag). Providers that
    /// don't support language filtering ignore this field.
    pub language: Option<LanguageTag>,

    /// If set, only hits from these domains are acceptable. Each entry is
    /// a suffix match (e.g. `.edu` matches `mit.edu` and `foo.mit.edu`).
    /// Matching is ASCII case-insensitive (RFC 4343) and ignores a
    /// trailing root dot on either side.
    pub domain_allowlist: Option<Vec<String>>,

    /// Domains to reject outright. Each entry is a suffix match with the
    /// same case-insensitive, trailing-dot-tolerant semantics as
    /// [`SearchConstraints::domain_allowlist`].
    pub domain_denylist: Option<Vec<String>>,

    /// Budget ceiling for this call. See [`BudgetConstraint`] for the
    /// cap hierarchy.
    pub budget: BudgetConstraint,
}

impl SearchConstraints {
    /// Build with minimum required fields (`max_results` + budget). All
    /// other fields start `None`.
    #[must_use]
    pub fn new(max_results: usize, budget: BudgetConstraint) -> Self {
        Self {
            max_results,
            freshness_window: None,
            language: None,
            domain_allowlist: None,
            domain_denylist: None,
            budget,
        }
    }

    /// Builder: set freshness window.
    #[must_use]
    pub fn with_freshness(mut self, window: Duration) -> Self {
        self.freshness_window = Some(window);
        self
    }

    /// Builder: set preferred language.
    #[must_use]
    pub fn with_language(mut self, tag: LanguageTag) -> Self {
        self.language = Some(tag);
        self
    }

    /// Builder: set domain allowlist.
    #[must_use]
    pub fn with_allowlist(mut self, domains: Vec<String>) -> Self {
        self.domain_allowlist = Some(domains);
        self
    }

    /// Builder: set domain denylist.
    #[must_use]
    pub fn with_denylist(mut self, domains: Vec<String>) -> Self {
        self.domain_denylist = Some(domains);
        self
    }

    /// Whether the given URL's host matches the domain allow / deny rules.
    ///
    /// Semantics: denylist is checked first (any match = reject). Then
    /// allowlist: if present, the host must match at least one entry. If
    /// allowlist is absent, any non-denied host is accepted. Comparison is
    /// ASCII case-insensitive on both the host and the configured entries.
    ///
    /// A URL without a host (e.g. `file:/`) is rejected when the
    /// allowlist is set, and accepted otherwise.
    #[must_use]
    pub fn permits_url(&self, url: &Url) -> bool {
        let Some(host) = url.host_str() else {
            return self.domain_allowlist.is_none();
        };
        if let Some(deny) = &self.domain_denylist {
            if deny.iter().any(|suffix| matches_suffix(host, suffix)) {
                return false;
            }
        }
        if let Some(allow) = &self.domain_allowlist {
            return allow.iter().any(|suffix| matches_suffix(host, suffix));
        }
        true
    }
}

impl Default for SearchConstraints {
    /// Sensible permissive default: 10 results, free-only budget, no
    /// filters.
    fn default() -> Self {
        Self::new(10, BudgetConstraint::default())
    }
}

fn matches_suffix(host: &str, suffix: &str) -> bool {
    // WHY: domain names are case-insensitive (RFC 4343) and a trailing
    // root dot is semantically empty, so both sides are normalized before
    // comparison — otherwise a caller-supplied ".EDU" or "Tracker.Example"
    // entry would silently never match the always-lowercase host the url
    // crate produces. A leading dot on the suffix is stripped (".edu" and
    // "edu" both mean: any host equal to or ending in ".edu").
    let host = host.strip_suffix('.').unwrap_or(host).to_ascii_lowercase();
    let suffix = suffix.strip_prefix('.').unwrap_or(suffix);
    let suffix = suffix
        .strip_suffix('.')
        .unwrap_or(suffix)
        .to_ascii_lowercase();
    if suffix.is_empty() {
        return false;
    }
    if host == suffix {
        return true;
    }
    let Some(prefix_len) = host.len().checked_sub(suffix.len()) else {
        return false;
    };
    if prefix_len == 0 {
        // Length-equal was caught above; here host is shorter than suffix.
        return false;
    }
    match host.get(..prefix_len) {
        Some(prefix) => prefix.ends_with('.') && host.ends_with(suffix.as_str()),
        None => false,
    }
}

/// Opaque identifier for an in-flight deep-research task.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TaskId(String);

impl TaskId {
    /// Construct from a provider-supplied string. No validation: the
    /// provider owns the format.
    #[must_use]
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    /// Access the inner string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for TaskId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<&str> for TaskId {
    fn from(s: &str) -> Self {
        Self::new(s)
    }
}

impl AsRef<str> for TaskId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

/// Depth hint for a deep-research call.
///
/// The deep-research orchestrator translates depth into concrete
/// budget: more sources, more synthesis rounds, more time. Providers are
/// free to ignore the hint if they don't parameterize their pipeline on
/// depth.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[non_exhaustive]
#[serde(rename_all = "snake_case")]
pub enum DeepDepth {
    /// Shallow: a handful of sources, single-pass synthesis. Fast.
    Shallow,
    /// Standard: dozens of sources, 2-3 synthesis passes. Default.
    #[default]
    Standard,
    /// Deep: hundreds of sources, extended synthesis. Overnight batch
    /// preferred.
    Deep,
    /// Exhaustive: maximum breadth allowed by the budget. Typically
    /// reserved for operator-authorized critical queries.
    Exhaustive,
}

impl DeepDepth {
    /// Stable lowercase identifier.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Shallow => "shallow",
            Self::Standard => "standard",
            Self::Deep => "deep",
            Self::Exhaustive => "exhaustive",
        }
    }
}

/// Lifecycle state of an in-flight deep-research task.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ResearchStatus {
    /// Task is queued but no work has started.
    Pending,
    /// Task is executing. Optional progress percent `0..=100`.
    Running {
        /// Optional progress percentage (clamped to 0..=100 on construction).
        progress_pct: Option<u8>,
    },
    /// Task finished successfully. Fetch the result via
    /// [`super::DeepResearch::fetch`].
    Ready {
        /// Timestamp the task completed.
        completed_at: Timestamp,
    },
    /// Task failed. The `message` is a human-readable description; the
    /// caller may also get a structured [`super::Error`] from `fetch()`.
    Failed {
        /// Human-readable failure description.
        message: String,
    },
    /// Task was cancelled (by the caller or by the orchestrator for
    /// budget / timeout reasons).
    Cancelled,
}

impl ResearchStatus {
    /// Whether [`super::DeepResearch::fetch`] is safe to call now.
    #[must_use]
    pub const fn is_ready(&self) -> bool {
        matches!(self, Self::Ready { .. })
    }

    /// Whether the task has reached a terminal state (no further status
    /// changes expected).
    #[must_use]
    pub const fn is_terminal(&self) -> bool {
        matches!(
            self,
            Self::Ready { .. } | Self::Failed { .. } | Self::Cancelled
        )
    }

    /// Stable lowercase name of the lifecycle state (matches the serde
    /// `state` tag).
    #[must_use]
    pub const fn state_name(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running { .. } => "running",
            Self::Ready { .. } => "ready",
            Self::Failed { .. } => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    /// Build a running status, clamping progress to `0..=100`.
    #[must_use]
    pub fn running(progress_pct: Option<u8>) -> Self {
        let progress_pct = progress_pct.map(|p| p.min(100));
        Self::Running { progress_pct }
    }
}

/// Output of a single [`super::Crawler::fetch_page`] call.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct PageContent {
    /// The URL that was actually fetched. May differ from the caller's
    /// URL after redirect chains.
    pub final_url: Url,

    /// MIME content type of the returned payload (e.g. `text/html`,
    /// `application/pdf`).
    pub content_type: String,

    /// Raw body bytes. Capped at [`PageContent::MAX_BODY_BYTES`] by
    /// [`PageContent::new`].
    pub body: Vec<u8>,

    /// Extracted plain-text rendering of the body, if the crawler could
    /// produce one. HTML extractors typically fill this; PDF pipelines
    /// leave it `None` unless configured for OCR. Capped at
    /// [`PageContent::MAX_TEXT_BYTES`] by
    /// [`PageContent::with_extracted_text`].
    pub extracted_text: Option<String>,

    /// Timestamp the fetch completed.
    pub fetched_at: Timestamp,
}

impl PageContent {
    /// Maximum accepted `body` size in bytes. Crawled pages come from
    /// untrusted origins; anything beyond this cap is rejected at
    /// construction rather than buffered.
    pub const MAX_BODY_BYTES: usize = 10 * 1024 * 1024;

    /// Maximum accepted `extracted_text` size in bytes.
    pub const MAX_TEXT_BYTES: usize = 4 * 1024 * 1024;

    /// Construct a page-content record. `extracted_text` starts `None`;
    /// use [`PageContent::with_extracted_text`] to attach it.
    ///
    /// # Errors
    ///
    /// Returns [`crate::Error::OversizedPayload`] when `body` exceeds
    /// [`PageContent::MAX_BODY_BYTES`].
    pub fn new(
        final_url: Url,
        content_type: impl Into<String>,
        body: Vec<u8>,
        fetched_at: Timestamp,
    ) -> Result<Self> {
        ensure!(
            body.len() <= Self::MAX_BODY_BYTES,
            OversizedPayloadSnafu {
                what: "page body",
                len: body.len(),
                max: Self::MAX_BODY_BYTES,
            }
        );
        Ok(Self {
            final_url,
            content_type: content_type.into(),
            body,
            extracted_text: None,
            fetched_at,
        })
    }

    /// Builder: attach extracted plain-text.
    ///
    /// # Errors
    ///
    /// Returns [`crate::Error::OversizedPayload`] when `text` exceeds
    /// [`PageContent::MAX_TEXT_BYTES`].
    pub fn with_extracted_text(mut self, text: impl Into<String>) -> Result<Self> {
        let text = text.into();
        ensure!(
            text.len() <= Self::MAX_TEXT_BYTES,
            OversizedPayloadSnafu {
                what: "extracted text",
                len: text.len(),
                max: Self::MAX_TEXT_BYTES,
            }
        );
        self.extracted_text = Some(text);
        Ok(self)
    }

    /// Whether the content type looks like HTML.
    #[must_use]
    pub fn is_html(&self) -> bool {
        self.content_type.starts_with("text/html")
    }

    /// Body size in bytes.
    #[must_use]
    pub fn body_len(&self) -> usize {
        self.body.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_constraints_default_is_free_only() {
        let c = SearchConstraints::default();
        assert_eq!(c.max_results, 10);
        assert!(!c.budget.allow_paid_tier);
        assert!(c.freshness_window.is_none());
    }

    #[test]
    fn builders_compose() {
        let c = SearchConstraints::new(25, BudgetConstraint::phase_zero_default())
            .with_freshness(Duration::from_secs(86_400))
            .with_language("en-US".parse().unwrap())
            .with_allowlist(vec![".edu".to_owned()])
            .with_denylist(vec!["spam.example".to_owned()]);
        assert_eq!(c.max_results, 25);
        assert_eq!(c.freshness_window, Some(Duration::from_secs(86_400)));
        assert_eq!(c.language.as_ref().unwrap().as_str(), "en-US");
        assert_eq!(c.domain_allowlist.as_ref().unwrap().len(), 1);
        assert_eq!(c.domain_denylist.as_ref().unwrap().len(), 1);
    }

    #[test]
    fn permits_url_exact_match() {
        let c = SearchConstraints::new(10, BudgetConstraint::default())
            .with_allowlist(vec!["example.org".to_owned()]);
        assert!(c.permits_url(&Url::parse("https://example.org/x").unwrap()));
        assert!(!c.permits_url(&Url::parse("https://example.com/x").unwrap()));
    }

    #[test]
    fn permits_url_suffix_match() {
        let c = SearchConstraints::new(10, BudgetConstraint::default())
            .with_allowlist(vec![".edu".to_owned()]);
        assert!(c.permits_url(&Url::parse("https://mit.edu/").unwrap()));
        assert!(c.permits_url(&Url::parse("https://cs.mit.edu/").unwrap()));
        assert!(!c.permits_url(&Url::parse("https://mit.com/").unwrap()));
    }

    #[test]
    fn matches_suffix_is_case_insensitive() {
        // WHY: domain names are case-insensitive (RFC 4343); the url
        // crate always produces lowercase hosts, so a mixed-case entry
        // that only matched byte-for-byte would silently never fire.
        assert!(matches_suffix("mit.edu", ".EDU"));
        assert!(matches_suffix("mit.edu", "MIT.EDU"));
        assert!(matches_suffix("cs.mit.edu", "MIT.edu"));
        assert!(!matches_suffix("badmit.edu", "MIT.EDU"));
    }

    #[test]
    fn permits_url_mixed_case_denylist_blocks() {
        let c = SearchConstraints::new(10, BudgetConstraint::default())
            .with_denylist(vec!["Tracker.Example".to_owned()]);
        assert!(!c.permits_url(&Url::parse("https://tracker.example/pixel").unwrap()));
        assert!(!c.permits_url(&Url::parse("https://sub.tracker.example/x").unwrap()));
        assert!(c.permits_url(&Url::parse("https://other.example/x").unwrap()));
    }

    #[test]
    fn permits_url_mixed_case_allowlist_matches() {
        let c = SearchConstraints::new(10, BudgetConstraint::default())
            .with_allowlist(vec![".EDU".to_owned()]);
        assert!(c.permits_url(&Url::parse("https://mit.edu/").unwrap()));
        assert!(!c.permits_url(&Url::parse("https://mit.com/").unwrap()));
    }

    #[test]
    fn matches_suffix_tolerates_trailing_root_dot() {
        // Fully-qualified hosts with a trailing root dot are the same
        // domain; entries written FQDN-style must match too.
        assert!(matches_suffix("mit.edu.", ".edu"));
        assert!(matches_suffix("mit.edu", "edu."));
        assert!(matches_suffix("mit.edu.", "mit.edu."));
        let c = SearchConstraints::new(10, BudgetConstraint::default())
            .with_allowlist(vec![".edu".to_owned()]);
        assert!(c.permits_url(&Url::parse("https://mit.edu./").unwrap()));
    }

    #[test]
    fn matches_suffix_rejects_degenerate_entries() {
        // "." and "" normalize to an empty suffix, which must never match
        // (an empty suffix would otherwise allow every host).
        assert!(!matches_suffix("mit.edu", "."));
        assert!(!matches_suffix("mit.edu", ""));
        assert!(!matches_suffix("mit.edu", ".."));
    }

    #[test]
    fn permits_url_denylist_rejects() {
        let c = SearchConstraints::new(10, BudgetConstraint::default())
            .with_denylist(vec!["evil.example".to_owned()]);
        assert!(!c.permits_url(&Url::parse("https://evil.example/x").unwrap()));
        assert!(c.permits_url(&Url::parse("https://good.example/x").unwrap()));
    }

    #[test]
    fn permits_url_deny_takes_precedence() {
        let c = SearchConstraints::new(10, BudgetConstraint::default())
            .with_allowlist(vec!["example.org".to_owned()])
            .with_denylist(vec!["bad.example.org".to_owned()]);
        assert!(c.permits_url(&Url::parse("https://example.org/").unwrap()));
        assert!(!c.permits_url(&Url::parse("https://bad.example.org/").unwrap()));
    }

    #[test]
    fn permits_url_suffix_prefix_boundary() {
        // "mit.edu" must not match "badmit.edu" (no dot boundary).
        let c = SearchConstraints::new(10, BudgetConstraint::default())
            .with_allowlist(vec!["mit.edu".to_owned()]);
        assert!(!c.permits_url(&Url::parse("https://badmit.edu/").unwrap()));
        assert!(c.permits_url(&Url::parse("https://cs.mit.edu/").unwrap()));
    }

    #[test]
    fn task_id_round_trip() {
        let id = TaskId::new("deep-42");
        assert_eq!(id.as_str(), "deep-42");
        assert_eq!(id.to_string(), "deep-42");
        let from_str: TaskId = "deep-43".into();
        assert_eq!(from_str.as_str(), "deep-43");
        assert_eq!(id.as_ref(), "deep-42");

        let json = serde_json::to_string(&id).unwrap();
        let back: TaskId = serde_json::from_str(&json).unwrap();
        assert_eq!(back, id);
    }

    #[test]
    fn deep_depth_round_trip() {
        for depth in [
            DeepDepth::Shallow,
            DeepDepth::Standard,
            DeepDepth::Deep,
            DeepDepth::Exhaustive,
        ] {
            let json = serde_json::to_string(&depth).unwrap();
            let back: DeepDepth = serde_json::from_str(&json).unwrap();
            assert_eq!(back, depth);
            assert_eq!(json.trim_matches('"'), depth.as_str());
        }
    }

    #[test]
    fn deep_depth_default_is_standard() {
        assert_eq!(DeepDepth::default(), DeepDepth::Standard);
    }

    #[test]
    fn research_status_is_ready() {
        let r = ResearchStatus::Ready {
            completed_at: "2026-04-22T00:00:00Z".parse().unwrap(),
        };
        assert!(r.is_ready());
        assert!(r.is_terminal());
    }

    #[test]
    fn research_status_terminal_set() {
        assert!(!ResearchStatus::Pending.is_terminal());
        assert!(!ResearchStatus::running(Some(50)).is_terminal());
        assert!(
            ResearchStatus::Ready {
                completed_at: "2026-04-22T00:00:00Z".parse().unwrap()
            }
            .is_terminal()
        );
        assert!(
            ResearchStatus::Failed {
                message: "x".to_owned()
            }
            .is_terminal()
        );
        assert!(ResearchStatus::Cancelled.is_terminal());
    }

    #[test]
    fn research_status_state_names_match_serde_tags() {
        let cases: [(ResearchStatus, &str); 5] = [
            (ResearchStatus::Pending, "pending"),
            (ResearchStatus::running(None), "running"),
            (
                ResearchStatus::Ready {
                    completed_at: "2026-04-22T00:00:00Z".parse().unwrap(),
                },
                "ready",
            ),
            (
                ResearchStatus::Failed {
                    message: "x".to_owned(),
                },
                "failed",
            ),
            (ResearchStatus::Cancelled, "cancelled"),
        ];
        for (status, expected) in cases {
            assert_eq!(status.state_name(), expected);
            let json = serde_json::to_value(&status).unwrap();
            assert_eq!(json["state"], expected);
        }
    }

    #[test]
    fn running_clamps_progress() {
        let r = ResearchStatus::running(Some(250));
        if let ResearchStatus::Running { progress_pct } = r {
            assert_eq!(progress_pct, Some(100));
        } else {
            unreachable!("running() must return Running");
        }
    }

    #[test]
    fn research_status_serde_round_trip() {
        let statuses = [
            ResearchStatus::Pending,
            ResearchStatus::running(Some(10)),
            ResearchStatus::running(None),
            ResearchStatus::Ready {
                completed_at: "2026-04-22T00:00:00Z".parse().unwrap(),
            },
            ResearchStatus::Failed {
                message: "timed out".to_owned(),
            },
            ResearchStatus::Cancelled,
        ];
        for s in statuses {
            let json = serde_json::to_string(&s).unwrap();
            let back: ResearchStatus = serde_json::from_str(&json).unwrap();
            assert_eq!(back, s);
        }
    }

    #[test]
    fn page_content_is_html() {
        let p = PageContent {
            final_url: Url::parse("https://example.org/").unwrap(),
            content_type: "text/html; charset=utf-8".to_owned(),
            body: b"<html></html>".to_vec(),
            extracted_text: Some(String::new()),
            fetched_at: "2026-04-22T00:00:00Z".parse().unwrap(),
        };
        assert!(p.is_html());
        assert_eq!(p.body_len(), 13);
    }

    #[test]
    fn page_content_non_html() {
        let p = PageContent {
            final_url: Url::parse("https://example.org/a.pdf").unwrap(),
            content_type: "application/pdf".to_owned(),
            body: vec![0_u8; 100],
            extracted_text: None,
            fetched_at: "2026-04-22T00:00:00Z".parse().unwrap(),
        };
        assert!(!p.is_html());
        assert_eq!(p.body_len(), 100);
    }

    #[test]
    fn page_content_new_rejects_oversized_body() {
        // WHY: provider payloads are untrusted; the cap is the defense
        // against a hostile or broken origin exhausting memory.
        let err = PageContent::new(
            Url::parse("https://example.org/huge").unwrap(),
            "text/html",
            vec![0_u8; PageContent::MAX_BODY_BYTES + 1],
            "2026-04-22T00:00:00Z".parse().unwrap(),
        )
        .unwrap_err();
        assert!(err.is_permanent());
        assert!(err.to_string().contains("page body"));
    }

    #[test]
    fn with_extracted_text_rejects_oversized_text() {
        let page = PageContent::new(
            Url::parse("https://example.org/").unwrap(),
            "text/html",
            b"<html></html>".to_vec(),
            "2026-04-22T00:00:00Z".parse().unwrap(),
        )
        .unwrap();
        let err = page
            .with_extracted_text("x".repeat(PageContent::MAX_TEXT_BYTES + 1))
            .unwrap_err();
        assert!(err.is_permanent());
        assert!(err.to_string().contains("extracted text"));
    }

    #[test]
    fn search_constraints_serde_round_trip() {
        let c = SearchConstraints::new(5, BudgetConstraint::phase_zero_default())
            .with_freshness(Duration::from_secs(3600))
            .with_language("en".parse().unwrap())
            .with_allowlist(vec!["example.org".to_owned()]);
        let json = serde_json::to_string(&c).unwrap();
        let back: SearchConstraints = serde_json::from_str(&json).unwrap();
        assert_eq!(back, c);
    }

    #[test]
    fn page_content_serde_round_trip() {
        let p = PageContent {
            final_url: Url::parse("https://example.org/").unwrap(),
            content_type: "text/html".to_owned(),
            body: b"hello".to_vec(),
            extracted_text: Some("hello".to_owned()),
            fetched_at: "2026-04-22T00:00:00Z".parse().unwrap(),
        };
        let json = serde_json::to_string(&p).unwrap();
        let back: PageContent = serde_json::from_str(&json).unwrap();
        assert_eq!(back, p);
    }

    #[test]
    fn permits_url_no_host_without_allowlist() {
        let c = SearchConstraints::default();
        // `data:` URLs have no host; with no allowlist they're permitted.
        let url = Url::parse("data:text/plain,hi").unwrap();
        assert!(c.permits_url(&url));
    }

    #[test]
    fn permits_url_no_host_with_allowlist_rejected() {
        let c = SearchConstraints::default().with_allowlist(vec!["example.org".to_owned()]);
        let url = Url::parse("data:text/plain,hi").unwrap();
        assert!(!c.permits_url(&url));
    }

    #[test]
    fn dot_prefix_and_bare_suffix_equivalent() {
        let c_dot = SearchConstraints::default().with_allowlist(vec![".edu".to_owned()]);
        let c_bare = SearchConstraints::default().with_allowlist(vec!["edu".to_owned()]);
        let url = Url::parse("https://cs.mit.edu/").unwrap();
        assert_eq!(c_dot.permits_url(&url), c_bare.permits_url(&url));
    }
}
