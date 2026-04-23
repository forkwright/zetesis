//! Integration-level error-classification coverage.
//!
//! Unit tests inside `error.rs` exercise every variant's accessor; this
//! file exercises the *contract* callers will rely on: that
//! `is_transient` plus `is_permanent` plus `is_fatal` partition every
//! possible error value, and that the selectors are usable through the
//! re-exported paths a downstream crate will actually use.

#![allow(clippy::unwrap_used)]
#![allow(clippy::expect_used)]

use zetesis_api::{
    BudgetExceededSnafu, Error, FatalCorruptionSnafu, InvalidQuerySnafu, PermanentIoSnafu,
    ProviderFailureSnafu, QuotaExhaustedSnafu, RateLimitedSnafu, TimeoutSnafu, TransientIoSnafu,
    UnauthorizedSnafu, UnsupportedSnafu,
};

fn all_variants() -> Vec<Error> {
    vec![
        ProviderFailureSnafu {
            provider: "p".to_owned(),
            message: "m".to_owned(),
        }
        .build(),
        RateLimitedSnafu {
            provider: "p".to_owned(),
            retry_after_ms: None::<u64>,
        }
        .build(),
        BudgetExceededSnafu {
            attempted_micro_cents: 1_u64,
            cap_micro_cents: 0_u64,
        }
        .build(),
        QuotaExhaustedSnafu {
            provider: "p".to_owned(),
            window: None::<String>,
        }
        .build(),
        UnauthorizedSnafu {
            provider: "p".to_owned(),
            message: "m".to_owned(),
        }
        .build(),
        TimeoutSnafu {
            provider: "p".to_owned(),
            timeout_ms: 1_u64,
        }
        .build(),
        InvalidQuerySnafu {
            reason: "m".to_owned(),
        }
        .build(),
        TransientIoSnafu {
            message: "m".to_owned(),
        }
        .build(),
        PermanentIoSnafu {
            message: "m".to_owned(),
        }
        .build(),
        FatalCorruptionSnafu {
            message: "m".to_owned(),
        }
        .build(),
        UnsupportedSnafu {
            reason: "m".to_owned(),
        }
        .build(),
    ]
}

#[test]
fn every_variant_classifies_to_exactly_one_class() {
    for err in all_variants() {
        let classes = [err.is_transient(), err.is_permanent(), err.is_fatal()];
        let count = classes.iter().filter(|x| **x).count();
        assert_eq!(
            count, 1,
            "error did not partition cleanly: {err:?} => {classes:?}"
        );
    }
}

#[test]
fn display_contains_variant_context() {
    let e = ProviderFailureSnafu {
        provider: "semantic_scholar".to_owned(),
        message: "HTTP 503".to_owned(),
    }
    .build();
    let s = format!("{e}");
    assert!(s.contains("semantic_scholar"));
    assert!(s.contains("HTTP 503"));
}

#[test]
fn exposes_snafu_selectors_through_crate_root() {
    // WHY: providers will call builders as
    // `zetesis_api::ProviderFailureSnafu { .. }.build()`. The `visibility(pub)`
    // attribute on the enum publishes these selectors; this test makes
    // sure the spelling survives re-export via lib.rs.
    let _e: Error = ProviderFailureSnafu {
        provider: "x".to_owned(),
        message: "y".to_owned(),
    }
    .build();
}

#[test]
fn transient_set_is_expected_members() {
    let transients: Vec<&'static str> = all_variants()
        .into_iter()
        .filter(Error::is_transient)
        .map(|e| match e {
            Error::ProviderFailure { .. } => "ProviderFailure",
            Error::RateLimited { .. } => "RateLimited",
            Error::Timeout { .. } => "Timeout",
            Error::TransientIo { .. } => "TransientIo",
            _ => unreachable!("is_transient returned true for unexpected variant"),
        })
        .collect();
    let mut sorted = transients.clone();
    sorted.sort_unstable();
    assert_eq!(
        sorted,
        ["ProviderFailure", "RateLimited", "Timeout", "TransientIo"]
    );
}

#[test]
fn fatal_set_has_only_corruption() {
    let fatal: Vec<&'static str> = all_variants()
        .into_iter()
        .filter(Error::is_fatal)
        .map(|e| match e {
            Error::FatalCorruption { .. } => "FatalCorruption",
            _ => unreachable!("is_fatal returned true for unexpected variant"),
        })
        .collect();
    assert_eq!(fatal, ["FatalCorruption"]);
}

#[test]
fn permanent_set_is_expected_members() {
    let mut perms: Vec<&'static str> = all_variants()
        .into_iter()
        .filter(Error::is_permanent)
        .map(|e| match e {
            Error::BudgetExceeded { .. } => "BudgetExceeded",
            Error::QuotaExhausted { .. } => "QuotaExhausted",
            Error::Unauthorized { .. } => "Unauthorized",
            Error::InvalidQuery { .. } => "InvalidQuery",
            Error::PermanentIo { .. } => "PermanentIo",
            Error::Unsupported { .. } => "Unsupported",
            _ => unreachable!("is_permanent returned true for unexpected variant"),
        })
        .collect();
    perms.sort_unstable();
    assert_eq!(
        perms,
        [
            "BudgetExceeded",
            "InvalidQuery",
            "PermanentIo",
            "QuotaExhausted",
            "Unauthorized",
            "Unsupported"
        ]
    );
}
