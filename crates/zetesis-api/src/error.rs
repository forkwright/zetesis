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

use snafu::Snafu;

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
        "budget exceeded: attempted spend {attempted_micro_cents} micro-cents against cap {cap_micro_cents}"
    ))]
    BudgetExceeded {
        /// Paid spend the router attempted (micro-cents).
        attempted_micro_cents: u64,
        /// Cap that would have been breached (micro-cents).
        cap_micro_cents: u64,
        /// Source location captured at the point the error was built.
        #[snafu(implicit)]
        location: snafu::Location,
    },

    /// The provider's free-tier quota has been exhausted for this window.
    /// Distinct from [`Error::RateLimited`]: a quota exhaustion is
    /// window-scoped (per day, per month) where a rate-limit is
    /// instantaneous (per second, per burst).
    #[snafu(display("provider '{provider}' free-tier quota exhausted"))]
    QuotaExhausted {
        /// Provider identifier.
        provider: String,
        /// Optional hint at the window type ("per_day", "per_month", etc.).
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
}

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
    /// [`Error::Timeout`], and [`Error::TransientIo`]. Returns `false` for
    /// every other variant.
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
            Self::ProviderFailure { .. }
            | Self::RateLimited { .. }
            | Self::Timeout { .. }
            | Self::TransientIo { .. } => ErrorClass::Transient,
            Self::BudgetExceeded { .. }
            | Self::QuotaExhausted { .. }
            | Self::Unauthorized { .. }
            | Self::InvalidQuery { .. }
            | Self::PermanentIo { .. }
            | Self::Unsupported { .. } => ErrorClass::Permanent,
            Self::FatalCorruption { .. } => ErrorClass::Fatal,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use snafu::IntoError;

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
    fn budget_exceeded_is_permanent() {
        let e: Error = BudgetExceededSnafu {
            attempted_micro_cents: 1_000_000_u64,
            cap_micro_cents: 500_000_u64,
        }
        .build();
        assert!(e.is_permanent());
        assert!(!e.is_transient());
    }

    #[test]
    fn quota_exhausted_is_permanent() {
        let e: Error = QuotaExhaustedSnafu {
            provider: "pubmed".to_owned(),
            window: Some("per_day".to_owned()),
        }
        .build();
        assert!(e.is_permanent());
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
    fn classes_are_mutually_exclusive() {
        let errs = [
            ProviderFailureSnafu {
                provider: "p".to_owned(),
                message: "m".to_owned(),
            }
            .build(),
            BudgetExceededSnafu {
                attempted_micro_cents: 1_u64,
                cap_micro_cents: 0_u64,
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
    fn error_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync + 'static>() {}
        assert_send_sync::<Error>();
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
