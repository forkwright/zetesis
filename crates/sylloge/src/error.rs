//! Error taxonomy for the zetesis provider surface.
//!
//! The enum is deliberately flat so consumers can match without walking a
//! nested shape. Every variant carries an implicit [`snafu::Location`] for
//! on-fire diagnostics, and the [`Error::is_transient`] accessor lets
//! callers implement retry/backoff without case-analysing every variant
//! name.
//!
//! The taxonomy follows the convention in `basanos/standards/STORAGE.md`
//! § Error Handling: transient (retry-safe), permanent (don't retry),
//! fatal (corruption / operator intervention required).

use jiff::Timestamp;
use snafu::Snafu;

use crate::budget::BudgetScope;

/// Per-crate `Result` alias.
pub type Result<T> = std::result::Result<T, Error>;

/// Errors surfaced by the zetesis provider surface.
///
/// `#[non_exhaustive]` — new variants are a minor-version change. Callers
/// matching on the enum must include a wildcard arm.
#[derive(Debug, Snafu)]
#[snafu(visibility(pub))]
#[non_exhaustive]
pub enum Error {
    /// The provider returned an error response that is not otherwise
    /// classified (HTTP 5xx without more context, parse failure, malformed
    /// response body). Treated as transient by default because most such
    /// failures clear on retry.
    #[snafu(display("provider '{provider}' failed: {message}"))]
    ProviderFailure {
        /// Provider identifier matching `Provider::name()`.
        provider: String,
        /// Human-readable description of the failure.
        message: String,
        /// Source location captured at the point the error was built.
        #[snafu(implicit)]
        location: snafu::Location,
    },

    /// The provider returned a rate-limit response (HTTP 429 or the
    /// provider's equivalent). The caller should retry after
    /// `retry_after_ms` if set; an unset `retry_after_ms` means the caller
    /// should use exponential backoff.
    #[snafu(display("provider '{provider}' rate limited: retry after {retry_after_ms:?} ms"))]
    RateLimited {
        /// Provider identifier.
        provider: String,
        /// Provider-supplied retry delay in milliseconds. `None` means the
        /// provider did not surface a Retry-After header.
        retry_after_ms: Option<u64>,
        /// Source location captured at the point the error was built.
        #[snafu(implicit)]
        location: snafu::Location,
    },

    /// The caller's [`super::BudgetConstraint`] would be exceeded by the
    /// attempted call. No provider call was made; the router short-circuited
    /// at the budget layer.
    #[snafu(display(
        "budget exceeded ({scope:?}): attempted spend {attempted_micro_cents} micro-cents against cap {cap_micro_cents} ({remaining_micro_cents} remaining)"
    ))]
    BudgetExceeded {
        /// Which configured ceiling was violated.
        scope: BudgetScope,
        /// Paid spend the router attempted (micro-cents).
        attempted_micro_cents: u64,
        /// Cap that would have been breached (micro-cents).
        cap_micro_cents: u64,
        /// Allowance remaining in `scope` before this attempt, in
        /// micro-cents (i.e. what a smaller request could still spend).
        remaining_micro_cents: u64,
        /// For a rolling-window scope ([`BudgetScope::PerConsumerDay`],
        /// [`BudgetScope::PerFleetDay`]), the instant the window next has
        /// room. `None` for scopes with no reset
        /// ([`BudgetScope::PerQuery`], [`BudgetScope::PerAgentLifetime`],
        /// [`BudgetScope::PaidTierDisabled`]) -- see
        /// [`Error::is_transient`], which uses this field to classify the
        /// denial.
        resets_at: Option<Timestamp>,
        /// Source location captured at the point the error was built.
        #[snafu(implicit)]
        location: snafu::Location,
    },

    /// The provider's free-tier quota has been exhausted for this window.
    /// Distinct from [`Error::RateLimited`]: a quota exhaustion is
    /// window-scoped (per day, per month) where a rate-limit is
    /// instantaneous (per second, per burst). Transient: the quota comes
    /// back when the provider's window resets, so the caller may retry
    /// after a (typically long) delay or fall through to another provider.
    #[snafu(display("provider '{provider}' free-tier quota exhausted"))]
    QuotaExhausted {
        /// Provider identifier.
        provider: String,
        /// Optional hint at the window type (`per_day`, `per_month`, etc.).
        window: Option<String>,
        /// Source location captured at the point the error was built.
        #[snafu(implicit)]
        location: snafu::Location,
    },

    /// The provider rejected the call because authentication was missing
    /// or invalid. Permanent: the caller must fix their credentials.
    #[snafu(display("provider '{provider}' unauthorized: {message}"))]
    Unauthorized {
        /// Provider identifier.
        provider: String,
        /// Human-readable detail.
        message: String,
        /// Source location captured at the point the error was built.
        #[snafu(implicit)]
        location: snafu::Location,
    },

    /// The provider did not respond within the configured deadline.
    /// Transient: the caller may retry (possibly with a longer deadline).
    #[snafu(display("provider '{provider}' timed out after {timeout_ms} ms"))]
    Timeout {
        /// Provider identifier.
        provider: String,
        /// Timeout that triggered (milliseconds).
        timeout_ms: u64,
        /// Source location captured at the point the error was built.
        #[snafu(implicit)]
        location: snafu::Location,
    },

    /// The query string violated the provider's syntactic requirements
    /// (empty, too long, contains invalid characters). Permanent; the
    /// caller must fix the query.
    #[snafu(display("invalid query: {reason}"))]
    InvalidQuery {
        /// Explanation of why the query was rejected.
        reason: String,
        /// Source location captured at the point the error was built.
        #[snafu(implicit)]
        location: snafu::Location,
    },

    /// Transient I/O failure at the network layer (connection reset, DNS
    /// blip, temporary TLS handshake error). Retry-safe.
    #[snafu(display("transient I/O failure: {message}"))]
    TransientIo {
        /// Human-readable description.
        message: String,
        /// Source location captured at the point the error was built.
        #[snafu(implicit)]
        location: snafu::Location,
    },

    /// Permanent I/O failure (DNS record gone, certificate permanently
    /// invalid, endpoint removed). Not retry-safe.
    #[snafu(display("permanent I/O failure: {message}"))]
    PermanentIo {
        /// Human-readable description.
        message: String,
        /// Source location captured at the point the error was built.
        #[snafu(implicit)]
        location: snafu::Location,
    },

    /// Fatal corruption detected inside zetesis's own state (cache index
    /// broken, ledger unreadable, deserialization of a previously-persisted
    /// record fails). Requires operator intervention; never transient.
    #[snafu(display("fatal corruption: {message}"))]
    FatalCorruption {
        /// Description of the corruption.
        message: String,
        /// Source location captured at the point the error was built.
        #[snafu(implicit)]
        location: snafu::Location,
    },

    /// The operation is not supported by the chosen provider (e.g. calling
    /// [`super::Crawler::fetch_page`] on a provider that only implements
    /// [`super::Provider`]). Permanent; the caller picked the wrong
    /// trait.
    #[snafu(display("operation not supported: {reason}"))]
    Unsupported {
        /// Explanation (what was attempted, what the provider offers
        /// instead).
        reason: String,
        /// Source location captured at the point the error was built.
        #[snafu(implicit)]
        location: snafu::Location,
    },

    /// The deep-research task exists but has not reached a terminal state,
    /// so its result is not available yet. Transient: poll via
    /// [`super::DeepResearch::poll`] and retry the fetch once
    /// [`super::ResearchStatus::is_ready`] holds.
    #[snafu(display("deep-research task '{task}' is not ready: {detail}"))]
    TaskNotReady {
        /// Task identifier as supplied by the caller.
        task: String,
        /// Current lifecycle detail (state name plus query context).
        detail: String,
        /// Source location captured at the point the error was built.
        #[snafu(implicit)]
        location: snafu::Location,
    },

    /// The deep-research task cannot serve the requested operation from its
    /// current lifecycle state: it failed, was cancelled, is unknown to the
    /// backend, or is in a conflicting state (e.g. executing a task that is
    /// not pending). Permanent: this task will never produce a result;
    /// submit a new task instead.
    #[snafu(display("deep-research task '{task}' unavailable: {reason}"))]
    TaskUnavailable {
        /// Task identifier as supplied by the caller.
        task: String,
        /// Why the task cannot serve the operation.
        reason: String,
        /// Source location captured at the point the error was built.
        #[snafu(implicit)]
        location: snafu::Location,
    },

    /// A [`super::ResultHit`] was constructed with zero citations,
    /// violating the "no synthesis without source provenance" invariant.
    /// Permanent: the caller must supply at least one [`super::Citation`].
    #[snafu(display("hit '{title}' has no citations; every hit must carry provenance"))]
    MissingCitations {
        /// Title of the hit that lacked provenance.
        title: String,
        /// Source location captured at the point the error was built.
        #[snafu(implicit)]
        location: snafu::Location,
    },

    /// A payload exceeded one of the crate's defensive size caps (untrusted
    /// provider responses must not exhaust memory). Permanent for this
    /// payload: the caller must truncate or reject it upstream.
    #[snafu(display("{what} is {len} bytes, exceeding the {max}-byte cap"))]
    OversizedPayload {
        /// Which payload tripped the cap (e.g. `page body`).
        what: String,
        /// Observed payload size in bytes.
        len: usize,
        /// The cap that was exceeded, in bytes.
        max: usize,
        /// Source location captured at the point the error was built.
        #[snafu(implicit)]
        location: snafu::Location,
    },

    /// The URL's host is rejected by the caller's domain allow/deny
    /// constraints. Permanent: retrying the same URL cannot succeed under
    /// the same [`super::SearchConstraints`]. `sylloge`'s own
    /// [`super::SearchConstraints::check_url`] reports a domain
    /// allow/deny mismatch as [`Error::UnsafeTarget`] instead (it is one
    /// rejection reason among several the same call checks); this variant
    /// stays available for a [`super::Crawler`] implementation with its
    /// own domain policy layered on top.
    #[snafu(display("domain denied by constraints: {url}"))]
    DomainDenied {
        /// The rejected URL.
        url: String,
        /// Source location captured at the point the error was built.
        #[snafu(implicit)]
        location: snafu::Location,
    },

    /// The URL failed [`super::SearchConstraints::check_url`]'s
    /// fail-closed network-target policy: a disallowed scheme, userinfo,
    /// a missing host, a resolved address in a blocked range (loopback,
    /// private, link-local, unspecified, multicast, or reserved), or a
    /// domain allow/deny mismatch. Permanent: retrying the same URL
    /// cannot succeed under the same [`super::SearchConstraints`] (the
    /// only way to admit a blocked address is a separately supplied
    /// [`super::LocalTargetAuthorization`], an authority change rather than
    /// caller-data mutation or a retry).
    #[snafu(display("network-target policy rejected {url}: {reason}"))]
    UnsafeTarget {
        /// The rejected URL.
        url: String,
        /// Which check failed and why.
        reason: String,
        /// Source location captured at the point the error was built.
        #[snafu(implicit)]
        location: snafu::Location,
    },
}

// INVARIANT: errors cross task boundaries (`tokio::spawn`, `JoinSet`), so
// `Error` must stay `Send + Sync + 'static`. The compiler proves it here;
// the property cannot silently regress.
const _: () = {
    const fn assert_send_sync<T: Send + Sync + 'static>() {}
    assert_send_sync::<Error>();
};

/// Coarse classification for retry logic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ErrorClass {
    /// Safe to retry (usually after a backoff delay).
    Transient,
    /// Don't retry; caller input or credentials must change.
    Permanent,
    /// System-level corruption; requires operator intervention.
    Fatal,
}

impl Error {
    /// Whether this error is safe to retry.
    ///
    /// Returns `true` for [`Error::ProviderFailure`], [`Error::RateLimited`],
    /// [`Error::QuotaExhausted`], [`Error::Timeout`], [`Error::TransientIo`],
    /// and [`Error::TaskNotReady`]. Returns `false` for every other variant.
    #[must_use]
    pub fn is_transient(&self) -> bool {
        matches!(self.class(), ErrorClass::Transient)
    }

    /// Whether this error indicates fatal system corruption.
    #[must_use]
    pub fn is_fatal(&self) -> bool {
        matches!(self.class(), ErrorClass::Fatal)
    }

    /// Whether this error is a permanent failure of the current call that
    /// the caller should not retry.
    #[must_use]
    pub fn is_permanent(&self) -> bool {
        matches!(self.class(), ErrorClass::Permanent)
    }

    /// Full classification.
    #[must_use]
    pub fn class(&self) -> ErrorClass {
        match self {
            // WHY(zetesis#47): a rolling-window BudgetExceeded (consumer-day,
            // fleet-day) genuinely clears at `resets_at` -- retryable, like
            // QuotaExhausted. A query-cap, lifetime-cap, or paid-tier-disabled
            // denial (resets_at: None) never clears on its own and falls
            // into the Permanent arm below instead.
            Self::ProviderFailure { .. }
            | Self::RateLimited { .. }
            | Self::QuotaExhausted { .. }
            | Self::Timeout { .. }
            | Self::TransientIo { .. }
            | Self::TaskNotReady { .. }
            | Self::BudgetExceeded {
                resets_at: Some(_), ..
            } => ErrorClass::Transient,
            Self::BudgetExceeded {
                resets_at: None, ..
            }
            | Self::Unauthorized { .. }
            | Self::InvalidQuery { .. }
            | Self::PermanentIo { .. }
            | Self::Unsupported { .. }
            | Self::TaskUnavailable { .. }
            | Self::MissingCitations { .. }
            | Self::OversizedPayload { .. }
            | Self::DomainDenied { .. }
            | Self::UnsafeTarget { .. } => ErrorClass::Permanent,
            Self::FatalCorruption { .. } => ErrorClass::Fatal,
        }
    }
}

#[cfg(test)]
mod tests {
    use snafu::IntoError;

    use super::*;

    #[test]
    fn provider_failure_is_transient() {
        let e: Error = ProviderFailureSnafu {
            provider: "brave".to_owned(),
            message: "HTTP 502".to_owned(),
        }
        .build();
        assert!(e.is_transient());
        assert!(!e.is_permanent());
        assert!(!e.is_fatal());
    }

    #[test]
    fn rate_limited_is_transient() {
        let e: Error = RateLimitedSnafu {
            provider: "semantic_scholar".to_owned(),
            retry_after_ms: Some(5_000_u64),
        }
        .build();
        assert!(e.is_transient());
    }

    #[test]
    fn budget_exceeded_query_scope_is_permanent() {
        let e: Error = BudgetExceededSnafu {
            scope: BudgetScope::PerQuery,
            attempted_micro_cents: 1_000_000_u64,
            cap_micro_cents: 500_000_u64,
            remaining_micro_cents: 500_000_u64,
            resets_at: None,
        }
        .build();
        assert!(e.is_permanent());
        assert!(!e.is_transient());
    }

    #[test]
    fn budget_exceeded_rolling_window_scope_is_transient() {
        // WHY(zetesis#47): unlike a query/lifetime/paid-tier denial, a
        // rolling-window denial genuinely clears at `resets_at`.
        let e: Error = BudgetExceededSnafu {
            scope: BudgetScope::PerConsumerDay,
            attempted_micro_cents: 1_u64,
            cap_micro_cents: 50_000_000_u64,
            remaining_micro_cents: 0_u64,
            resets_at: Some("2026-07-02T00:00:00Z".parse::<Timestamp>().unwrap()),
        }
        .build();
        assert!(e.is_transient());
        assert!(!e.is_permanent());
    }

    #[test]
    fn quota_exhausted_is_transient() {
        // WHY: quota exhaustion is window-scoped — the quota returns when
        // the provider's window resets — so retry-after-delay is correct,
        // unlike credential or input errors.
        let e: Error = QuotaExhaustedSnafu {
            provider: "pubmed".to_owned(),
            window: Some("per_day".to_owned()),
        }
        .build();
        assert!(e.is_transient());
        assert!(!e.is_permanent());
    }

    #[test]
    fn unauthorized_is_permanent() {
        let e: Error = UnauthorizedSnafu {
            provider: "exa".to_owned(),
            message: "bad API key".to_owned(),
        }
        .build();
        assert!(e.is_permanent());
    }

    #[test]
    fn timeout_is_transient() {
        let e: Error = TimeoutSnafu {
            provider: "arxiv".to_owned(),
            timeout_ms: 30_000_u64,
        }
        .build();
        assert!(e.is_transient());
    }

    #[test]
    fn invalid_query_is_permanent() {
        let e: Error = InvalidQuerySnafu {
            reason: "query too short".to_owned(),
        }
        .build();
        assert!(e.is_permanent());
    }

    #[test]
    fn transient_io_is_transient() {
        let e: Error = TransientIoSnafu {
            message: "connection reset".to_owned(),
        }
        .build();
        assert!(e.is_transient());
    }

    #[test]
    fn permanent_io_is_permanent() {
        let e: Error = PermanentIoSnafu {
            message: "DNS NXDOMAIN".to_owned(),
        }
        .build();
        assert!(e.is_permanent());
    }

    #[test]
    fn fatal_corruption_is_fatal() {
        let e: Error = FatalCorruptionSnafu {
            message: "ledger checksum mismatch".to_owned(),
        }
        .build();
        assert!(e.is_fatal());
        assert!(!e.is_transient());
        assert!(!e.is_permanent());
    }

    #[test]
    fn unsupported_is_permanent() {
        let e: Error = UnsupportedSnafu {
            reason: "crawler not implemented".to_owned(),
        }
        .build();
        assert!(e.is_permanent());
    }

    #[test]
    fn task_not_ready_is_transient() {
        let e: Error = TaskNotReadySnafu {
            task: "local-deep-research-1".to_owned(),
            detail: "state=running".to_owned(),
        }
        .build();
        assert!(e.is_transient());
        assert!(!e.is_permanent());
    }

    #[test]
    fn task_unavailable_is_permanent() {
        let e: Error = TaskUnavailableSnafu {
            task: "local-deep-research-1".to_owned(),
            reason: "task failed: fixture failure".to_owned(),
        }
        .build();
        assert!(e.is_permanent());
        assert!(!e.is_transient());
    }

    #[test]
    fn missing_citations_is_permanent() {
        let e: Error = MissingCitationsSnafu {
            title: "uncited hit".to_owned(),
        }
        .build();
        assert!(e.is_permanent());
    }

    #[test]
    fn oversized_payload_is_permanent() {
        let e: Error = OversizedPayloadSnafu {
            what: "page body".to_owned(),
            len: 11_000_000_usize,
            max: 10_485_760_usize,
        }
        .build();
        assert!(e.is_permanent());
        let s = format!("{e}");
        assert!(s.contains("11000000"));
        assert!(s.contains("10485760"));
    }

    #[test]
    fn domain_denied_is_permanent() {
        let e: Error = DomainDeniedSnafu {
            url: "https://tracker.example/pixel".to_owned(),
        }
        .build();
        assert!(e.is_permanent());
        assert!(format!("{e}").contains("tracker.example"));
    }

    #[test]
    fn unsafe_target_is_permanent() {
        let e: Error = UnsafeTargetSnafu {
            url: "http://169.254.169.254/latest/meta-data/".to_owned(),
            reason: "resolved address 169.254.169.254 is in a blocked range".to_owned(),
        }
        .build();
        assert!(e.is_permanent());
        let s = format!("{e}");
        assert!(s.contains("169.254.169.254"));
        assert!(s.contains("blocked range"));
    }

    #[test]
    fn classes_are_mutually_exclusive() {
        let errs = [
            ProviderFailureSnafu {
                provider: "p".to_owned(),
                message: "m".to_owned(),
            }
            .build(),
            BudgetExceededSnafu {
                scope: BudgetScope::PerQuery,
                attempted_micro_cents: 1_u64,
                cap_micro_cents: 0_u64,
                remaining_micro_cents: 0_u64,
                resets_at: None,
            }
            .build(),
            FatalCorruptionSnafu {
                message: "m".to_owned(),
            }
            .build(),
        ];
        for e in &errs {
            let t = usize::from(e.is_transient());
            let p = usize::from(e.is_permanent());
            let f = usize::from(e.is_fatal());
            assert_eq!(t + p + f, 1, "classes must be mutually exclusive: {e:?}");
        }
    }

    #[test]
    fn display_format_is_informative() {
        let e: Error = ProviderFailureSnafu {
            provider: "brave".to_owned(),
            message: "502 Bad Gateway".to_owned(),
        }
        .build();
        let s = format!("{e}");
        assert!(s.contains("brave"));
        assert!(s.contains("502"));
    }

    #[test]
    fn into_error_bridge_compiles() {
        // WHY: verifies the snafu selectors are usable both as build() and
        // as IntoError::into_error, which is the shape providers will use
        // with `.context()` on a lower-level error.
        let io: std::io::Error = std::io::Error::new(std::io::ErrorKind::ConnectionReset, "reset");
        let e: Error = TransientIoSnafu {
            message: io.to_string(),
        }
        .into_error(snafu::NoneError);
        assert!(e.is_transient());
    }
}
