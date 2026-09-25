//! Per-call constraints and the value types that travel with them.
//!
//! [`SearchConstraints`] is passed into every [`super::Provider::search`]
//! call. It captures caller intent that isn't already in the query string
//! itself: budget ceiling, freshness window, allowed languages, domain
//! allow/deny lists, and -- via [`SearchConstraints::check_url`] -- the
//! fail-closed network-target policy [`super::StaticAcquirer`] applies to
//! every hop before fetching it or following a redirect to it.
//!
//! [`DeepDepth`], [`ResearchStatus`], and [`TaskId`] are value types used
//! by the [`super::DeepResearch`] trait for its asynchronous task
//! lifecycle.

use std::net::IpAddr;
use std::time::Duration;

use jiff::Timestamp;
use language_tags::LanguageTag;
use serde::{Deserialize, Deserializer, Serialize};
use url::Host;

use crate::BudgetConstraint;
use crate::citation::Citation;
use crate::error::{InvalidConstraintSnafu, Result};
use crate::freshness::{self, FreshnessBasis, FreshnessDecision, FreshnessPolicy};
use crate::net_policy::canonical_ip;

/// Per-call constraints supplied by the caller.
///
/// Every field is optional except `max_results` and `budget`. Unset fields
/// mean "no constraint" (the provider's default window / language / domain
/// scope applies).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
#[serde(deny_unknown_fields)]
pub struct SearchConstraints {
    /// Maximum number of hits the provider may return. Providers that
    /// can't honour this exactly must return at most this many.
    pub max_results: usize,

    /// Only accept hits whose [`crate::Citation::published_at`] falls
    /// within the last `freshness_window`, subject to `freshness_policy`
    /// when it is [`crate::PublicationTime::Unknown`]. `None` means no
    /// freshness filter. See [`SearchConstraints::evaluate_freshness`] for
    /// the enforcement rule -- retrieval time
    /// ([`crate::Citation::accessed_at`]) is never treated as content
    /// freshness on its own.
    pub freshness_window: Option<Duration>,

    /// How to treat a citation with [`crate::PublicationTime::Unknown`]
    /// when `freshness_window` is set. Defaults to
    /// [`FreshnessPolicy::Strict`]. Irrelevant when `freshness_window` is
    /// `None`.
    pub freshness_policy: FreshnessPolicy,

    /// Preferred content language (BCP-47 language tag). Providers that
    /// don't support language filtering ignore this field.
    pub language: Option<LanguageTag>,

    /// If set, only hits from these domains are acceptable. A domain entry
    /// is a suffix match (e.g. `.edu` matches `mit.edu` and `foo.mit.edu`);
    /// an IP-address entry matches only that address, in any spelling.
    /// Entries are canonicalized with the same host parser URLs go through
    /// (case-insensitive per RFC 4343, internationalized names as punycode,
    /// optional leading or trailing dot), and an entry that names no host
    /// (empty, a wildcard, a URL, a host with a port) makes
    /// [`SearchConstraints::check_url`] fail with
    /// [`crate::Error::InvalidConstraint`] instead of matching nothing.
    pub domain_allowlist: Option<Vec<String>>,

    /// Domains to reject outright, with the same entry semantics and
    /// fail-closed validation as [`SearchConstraints::domain_allowlist`].
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
            freshness_policy: FreshnessPolicy::default(),
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

    /// Builder: set the unknown-publication-time policy (see
    /// [`SearchConstraints::freshness_policy`]).
    #[must_use]
    pub fn with_freshness_policy(mut self, policy: FreshnessPolicy) -> Self {
        self.freshness_policy = policy;
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

    /// Evaluate `citation` against `freshness_window` and
    /// `freshness_policy` as of `now`. `freshness_window: None` always
    /// accepts. This is the single enforcement point for the freshness
    /// window -- see [`crate::evaluate_freshness`] for the rule applied
    /// once a window is configured.
    #[must_use]
    pub fn evaluate_freshness(&self, citation: &Citation, now: Timestamp) -> FreshnessDecision {
        let Some(window) = self.freshness_window else {
            return FreshnessDecision {
                accepted: true,
                policy: self.freshness_policy,
                basis: FreshnessBasis::NoWindowConfigured,
            };
        };
        freshness::evaluate_freshness(
            &citation.published_at,
            citation.accessed_at,
            window,
            self.freshness_policy,
            now,
        )
    }
}

impl Default for SearchConstraints {
    /// Sensible permissive default: 10 results, free-only budget, no
    /// domain filters. Permissive stops at the domain layer, so
    /// [`SearchConstraints::check_url`] still fails closed on
    /// loopback/private/link-local/unspecified/multicast/reserved targets
    /// even with every other field left at its default.
    fn default() -> Self {
        Self::new(10, BudgetConstraint::default())
    }
}

/// One canonical entry of [`SearchConstraints::domain_allowlist`] or
/// [`SearchConstraints::domain_denylist`].
///
/// Entries are parsed with the same WHATWG host parser the `url` crate
/// applies to every request URL, so both sides of a comparison are in one
/// canonical form: ASCII-lowercase, internationalized names in punycode,
/// alternate IPv4 spellings resolved, and IPv4-embedding IPv6 addresses
/// unwrapped. An entry that cannot name a host is an
/// [`crate::Error::InvalidConstraint`], never a silent "matches nothing":
/// that silence is what let a Unicode, wildcard, or URL-shaped denylist
/// entry admit the very host it was written to block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DomainRule {
    /// A domain name; matches itself and every subdomain.
    Suffix(String),
    /// An IP address; matches only that address, in any spelling.
    Address(IpAddr),
}

impl DomainRule {
    /// Parse one caller-supplied entry from constraint `field`.
    ///
    /// A leading dot (".edu") and a trailing root dot are optional and mean
    /// the same as the bare name.
    ///
    /// # Errors
    ///
    /// Returns [`crate::Error::InvalidConstraint`] when the entry is empty,
    /// contains a wildcard, has an empty label, or is not a host name or IP
    /// address (for example a URL or a host with a port).
    pub(crate) fn parse(field: &str, entry: &str) -> Result<Self> {
        let invalid = |reason: String| {
            InvalidConstraintSnafu {
                field: field.to_owned(),
                reason: format!("entry {entry:?}: {reason}"),
            }
            .build()
        };
        let bare = entry.strip_prefix('.').unwrap_or(entry);
        let bare = bare.strip_suffix('.').unwrap_or(bare);
        if bare.is_empty() {
            return Err(invalid("names no host".to_owned()));
        }
        if bare.contains('*') {
            return Err(invalid(
                "wildcards are not supported; an entry already matches every subdomain".to_owned(),
            ));
        }
        if let Ok(ip) = bare.parse::<IpAddr>() {
            return Ok(Self::Address(canonical_ip(ip)));
        }
        match Host::parse(bare) {
            Ok(Host::Domain(domain)) => {
                if domain.split('.').any(str::is_empty) {
                    return Err(invalid("contains an empty label".to_owned()));
                }
                Ok(Self::Suffix(domain))
            }
            Ok(Host::Ipv4(v4)) => Ok(Self::Address(IpAddr::V4(v4))),
            Ok(Host::Ipv6(v6)) => Ok(Self::Address(canonical_ip(IpAddr::V6(v6)))),
            Err(e) => Err(invalid(format!("not a host name or IP address ({e})"))),
        }
    }

    /// The rule's canonical spelling: the punycode, lowercase domain, or
    /// the canonical address. Two entries that parse to the same rule have
    /// the same canonical spelling.
    pub(crate) fn canonical(&self) -> String {
        match self {
            Self::Suffix(domain) => domain.clone(),
            Self::Address(addr) => addr.to_string(),
        }
    }

    /// Whether this rule matches the canonical `host` of a parsed URL.
    pub(crate) fn matches(&self, host: &Host<&str>) -> bool {
        match (self, host) {
            (Self::Suffix(suffix), Host::Domain(name)) => {
                let name = name.strip_suffix('.').unwrap_or(name);
                name == suffix
                    || name
                        .strip_suffix(suffix.as_str())
                        .is_some_and(|prefix| prefix.ends_with('.'))
            }
            (Self::Address(rule), Host::Ipv4(v4)) => *rule == IpAddr::V4(*v4),
            (Self::Address(rule), Host::Ipv6(v6)) => *rule == canonical_ip(IpAddr::V6(*v6)),
            _ => false,
        }
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
        /// Optional progress percentage (clamped to 0..=100 on construction;
        /// a decoded value above 100 is rejected).
        #[serde(deserialize_with = "percent_at_most_100")]
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

/// Reject a decoded progress percentage [`ResearchStatus::running`] could
/// never produce.
///
/// WHY: `ResearchStatus` is a pub enum, so serde is a second construction
/// path; without this a persisted or backend-supplied status could carry
/// `progress_pct: 250` past the documented 0..=100 invariant.
fn percent_at_most_100<'de, D>(deserializer: D) -> std::result::Result<Option<u8>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Option::<u8>::deserialize(deserializer)?;
    match value {
        Some(pct) if pct > 100 => Err(serde::de::Error::invalid_value(
            serde::de::Unexpected::Unsigned(u64::from(pct)),
            &"a percentage in 0..=100",
        )),
        _ => Ok(value),
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
        assert_eq!(c.freshness_policy, FreshnessPolicy::Strict);
    }

    #[test]
    fn builders_compose() {
        let c = SearchConstraints::new(
            25,
            BudgetConstraint::free_only()
                .with_per_query_cap(500_000)
                .with_paid_tier_allowed(true),
        )
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

    fn rule(entry: &str) -> DomainRule {
        DomainRule::parse("domain_allowlist", entry).unwrap()
    }

    fn domain(name: &str) -> Host<&str> {
        Host::Domain(name)
    }

    #[test]
    fn domain_rule_is_case_insensitive() {
        // WHY: domain names are case-insensitive (RFC 4343); the url
        // crate always produces lowercase hosts, so a mixed-case entry
        // that only matched byte-for-byte would silently never fire.
        assert!(
            rule(".EDU").matches(&domain("mit.edu")),
            ".EDU matches mit.edu"
        );
        assert!(
            rule("MIT.EDU").matches(&domain("mit.edu")),
            "MIT.EDU matches itself"
        );
        assert!(
            rule("MIT.edu").matches(&domain("cs.mit.edu")),
            "subdomains match"
        );
        assert!(
            !rule("MIT.EDU").matches(&domain("badmit.edu")),
            "label boundary holds"
        );
    }

    #[test]
    fn domain_rule_tolerates_trailing_root_dot() {
        // Fully-qualified hosts with a trailing root dot are the same
        // domain; entries written FQDN-style must match too. See
        // `net_policy::tests::check_url_accepts_trailing_root_dot_domain`
        // for the `check_url_with` integration of this.
        assert!(
            rule(".edu").matches(&domain("mit.edu.")),
            "host root dot ignored"
        );
        assert!(
            rule("edu.").matches(&domain("mit.edu")),
            "entry root dot ignored"
        );
        assert!(
            rule("mit.edu.").matches(&domain("mit.edu.")),
            "both root dots ignored"
        );
    }

    #[test]
    fn domain_rule_rejects_entries_that_name_no_host() {
        // WHY: an entry that cannot match any host used to be accepted and
        // then match nothing, which fails OPEN for a denylist.
        for entry in [
            "",
            ".",
            "..",
            "..edu",
            "a..b",
            "*.example.org",
            "https://example.org",
            "example.org/path",
            "example.org:443",
            "two words.example",
        ] {
            let err = DomainRule::parse("domain_denylist", entry).unwrap_err();
            assert!(
                matches!(err, crate::Error::InvalidConstraint { .. }),
                "{entry:?} must be an invalid constraint, got {err:?}"
            );
        }
    }

    #[test]
    fn domain_rule_canonical_spelling_is_shared_by_equivalent_entries() {
        assert_eq!(
            rule(".EXAMPLE.org.").canonical(),
            rule("example.org").canonical(),
            "case and optional dots do not change the canonical spelling"
        );
        assert_eq!(
            rule("0x7f.1").canonical(),
            "127.0.0.1",
            "an alternate IPv4 spelling canonicalizes to dotted decimal"
        );
    }

    #[test]
    fn domain_rule_canonicalizes_internationalized_names() {
        assert_eq!(
            rule("Bücher.Example"),
            DomainRule::Suffix("xn--bcher-kva.example".to_owned()),
            "a Unicode entry canonicalizes to the punycode form URL hosts use"
        );
    }

    #[test]
    fn domain_rule_matches_ip_addresses_exactly_in_any_spelling() {
        let r = rule("127.0.0.1");
        assert!(
            r.matches(&Host::Ipv4(std::net::Ipv4Addr::LOCALHOST)),
            "the same address matches"
        );
        assert!(
            r.matches(&Host::Ipv6("::ffff:127.0.0.1".parse().unwrap())),
            "the IPv4-mapped spelling of the same address matches"
        );
        assert!(
            !r.matches(&Host::Ipv4("127.0.0.2".parse().unwrap())),
            "a different address does not match"
        );
        assert!(
            !r.matches(&domain("127.0.0.1.example")),
            "an address rule never suffix-matches a domain"
        );
        assert_eq!(
            rule("0x7f.1"),
            DomainRule::Address("127.0.0.1".parse().unwrap()),
            "alternate IPv4 spellings canonicalize like URL hosts"
        );
        assert_eq!(
            rule("::1"),
            rule("[::1]"),
            "bare and bracketed IPv6 entries are the same rule"
        );
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
    fn search_constraints_serde_round_trip() {
        let c = SearchConstraints::new(
            5,
            BudgetConstraint::free_only()
                .with_per_query_cap(500_000)
                .with_paid_tier_allowed(true),
        )
        .with_freshness(Duration::from_secs(3600))
        .with_language("en".parse().unwrap())
        .with_allowlist(vec!["example.org".to_owned()]);
        let json = serde_json::to_string(&c).unwrap();
        let back: SearchConstraints = serde_json::from_str(&json).unwrap();
        assert_eq!(back, c);
    }

    #[test]
    fn search_constraints_rejects_removed_local_target_flag() {
        let mut value = serde_json::to_value(SearchConstraints::default()).unwrap();
        value["allow_local_targets"] = serde_json::Value::Bool(true);
        let err = serde_json::from_value::<SearchConstraints>(value).unwrap_err();
        assert!(err.to_string().contains("allow_local_targets"));
    }
}
