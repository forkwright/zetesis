//! Malformed, overflowing, and invalid-constraint input at the public
//! deserialization and policy boundaries.
//!
//! Every case here is untrusted data a consumer can hand `sylloge`: a
//! persisted record read back, a provider payload, or caller-authored
//! constraints. Each must either decode into a value that upholds the
//! type's documented invariant or be rejected. None may decode into a
//! value the type's own constructors could never produce, and none may
//! silently weaken a caller's policy.

#![expect(clippy::unwrap_used, reason = "test assertions must fail loudly")]

use std::net::{IpAddr, Ipv4Addr};

use url::Url;

use sylloge::{
    BudgetConstraint, CostTracking, Error, ResearchStatus, Resolver, SearchConstraints, SpendLedger,
};

/// Resolves every host to one fixed public-classified address, so these
/// tests exercise domain-list policy rather than address classification
/// or live DNS.
struct PublicResolver;

impl Resolver for PublicResolver {
    fn resolve(&self, _host: &str, _port: u16) -> std::io::Result<Vec<IpAddr>> {
        Ok(vec![IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8))])
    }
}

fn url(s: &str) -> Url {
    Url::parse(s).unwrap()
}

/// Assert `constraints` refuse `target` as an invalid constraint, not as
/// a policy match: an unusable entry must surface as the caller's error.
fn assert_invalid_constraint(constraints: &SearchConstraints, target: &str) {
    let err = constraints
        .check_url_with(&url(target), &PublicResolver)
        .unwrap_err();
    assert!(
        matches!(err, Error::InvalidConstraint { .. }),
        "an unusable entry must fail closed as InvalidConstraint, got {err:?}"
    );
    assert!(
        err.is_permanent(),
        "a malformed constraint never clears on retry"
    );
}

// -- Invalid domain-list constraints must fail closed. --

#[test]
fn denylist_entry_in_unicode_blocks_its_punycode_host() {
    // WHY: the url crate always reports an internationalized host in its
    // ASCII (punycode) form, so a Unicode denylist entry compared as raw
    // text never matches and the denied host is admitted.
    let c = SearchConstraints::default().with_denylist(vec!["bücher.example".to_owned()]);
    let host = url("https://bücher.example/catalog");
    assert_eq!(
        host.host_str(),
        Some("xn--bcher-kva.example"),
        "precondition: the parser canonicalizes the host to punycode"
    );
    assert!(
        c.check_url_with(&host, &PublicResolver).is_err(),
        "a denylisted internationalized host must be rejected"
    );
}

#[test]
fn allowlist_entry_in_unicode_admits_its_punycode_host() {
    let c = SearchConstraints::default().with_allowlist(vec!["bücher.example".to_owned()]);
    assert!(
        c.check_url_with(&url("https://bücher.example/catalog"), &PublicResolver)
            .is_ok(),
        "an allowlisted internationalized host must be admitted"
    );
}

#[test]
fn denylist_wildcard_entry_is_rejected_not_ignored() {
    // WHY: `*.evil.example` names no real host, so as raw text it matches
    // nothing and the denylist silently admits every subdomain it was
    // written to block.
    let c = SearchConstraints::default().with_denylist(vec!["*.evil.example".to_owned()]);
    assert_invalid_constraint(&c, "https://api.evil.example/");
}

#[test]
fn denylist_url_shaped_entry_is_rejected_not_ignored() {
    let c = SearchConstraints::default().with_denylist(vec!["https://evil.example".to_owned()]);
    assert_invalid_constraint(&c, "https://evil.example/");
}

#[test]
fn denylist_empty_entry_is_rejected_not_ignored() {
    let c = SearchConstraints::default().with_denylist(vec![".".to_owned()]);
    assert_invalid_constraint(&c, "https://example.org/");
}

#[test]
fn allowlist_invalid_entry_fails_before_any_resolution() {
    // WHY: an unusable entry is a caller error; it must surface without a
    // DNS lookup, so a misconfigured policy never costs network I/O.
    struct PanickingResolver;
    impl Resolver for PanickingResolver {
        fn resolve(&self, host: &str, _port: u16) -> std::io::Result<Vec<IpAddr>> {
            panic!("resolution must not run for an invalid constraint (asked for {host})");
        }
    }
    let c = SearchConstraints::default().with_allowlist(vec!["example.org:443".to_owned()]);
    let err = c
        .check_url_with(&url("https://example.org/"), &PanickingResolver)
        .unwrap_err();
    assert!(
        matches!(err, Error::InvalidConstraint { .. }),
        "expected InvalidConstraint, got {err:?}"
    );
}

#[test]
fn denylist_ipv4_entry_blocks_its_ipv4_mapped_ipv6_spelling() {
    // WHY: `[::ffff:8.8.8.8]` and `8.8.8.8` are the same destination;
    // address classification already unwraps the mapped form, so the
    // caller's denylist must not be bypassable by respelling the host.
    let c = SearchConstraints::default().with_denylist(vec!["8.8.8.8".to_owned()]);
    assert!(
        c.check_url_with(&url("http://[::ffff:8.8.8.8]/"), &PublicResolver)
            .is_err(),
        "a mapped spelling of a denylisted address must be rejected"
    );
}

#[test]
fn ip_entry_does_not_suffix_match_a_longer_address() {
    // WHY: suffix semantics are for domain labels; `8.8.8` must not act as
    // a prefix-free suffix of `18.8.8.8`, and an IPv4 entry never matches
    // a different address.
    let c = SearchConstraints::default().with_allowlist(vec!["8.8.8.8".to_owned()]);
    assert!(
        c.check_url_with(&url("http://18.8.8.8/"), &PublicResolver)
            .is_err(),
        "an IP entry must match only that exact address"
    );
}

// -- Malformed persisted or provider-supplied records must be rejected. --

#[test]
fn research_status_progress_above_one_hundred_is_rejected() {
    // WHY: `ResearchStatus::running` clamps progress to 0..=100; a decoded
    // value must not carry a percentage the constructor cannot produce.
    let json = r#"{"state":"running","progress_pct":250}"#;
    assert!(
        serde_json::from_str::<ResearchStatus>(json).is_err(),
        "progress above 100 percent is malformed"
    );
}

#[test]
fn cost_tracking_entry_keyed_under_another_provider_is_rejected() {
    // WHY: `CostTracking::add` keys every line item by its own
    // `provider_id`; a decoded ledger whose key and entry disagree would
    // attribute one provider's spend to another.
    let json = r#"{"by_provider":{"brave":{"provider_id":"exa","paid_micro_cents":5,"free_tier_units":0,"request_count":1}}}"#;
    assert!(
        serde_json::from_str::<CostTracking>(json).is_err(),
        "a line item keyed under a different provider is malformed"
    );
}

#[test]
fn spend_ledger_lifetime_below_its_events_is_rejected() {
    // WHY: the lifetime total is never pruned, so it can never be smaller
    // than the events still held. A decoded ledger claiming otherwise
    // under-reports spend to the lifetime cap.
    let json = r#"{"lifetime_paid_micro_cents":0,"events":[{"at":"2026-07-01T00:00:00Z","paid_micro_cents":100}]}"#;
    let decoded = serde_json::from_str::<SpendLedger>(json);
    assert!(
        decoded.is_err(),
        "a lifetime total below the recorded events is malformed: {decoded:?}"
    );
}

#[test]
fn spend_ledger_consistent_record_decodes() {
    let json = r#"{"lifetime_paid_micro_cents":150,"events":[{"at":"2026-07-01T00:00:00Z","paid_micro_cents":100}]}"#;
    let ledger = serde_json::from_str::<SpendLedger>(json).unwrap();
    assert_eq!(
        ledger.lifetime_paid_micro_cents(),
        150,
        "a lifetime total that includes pruned spend is valid"
    );
}

// -- Numeric overflow at the deserialization boundary. --

#[test]
fn budget_cap_beyond_u64_is_rejected() {
    let json = r#"{"per_query_cap_micro_cents":18446744073709551616,"per_day_cap_micro_cents":0,"per_fleet_day_cap_micro_cents":0,"per_agent_cap_micro_cents":0,"allow_paid_tier":false}"#;
    assert!(
        serde_json::from_str::<BudgetConstraint>(json).is_err(),
        "a cap one past u64::MAX must not wrap or saturate silently"
    );
}

#[test]
fn negative_budget_cap_is_rejected() {
    let json = r#"{"per_query_cap_micro_cents":-1,"per_day_cap_micro_cents":0,"per_fleet_day_cap_micro_cents":0,"per_agent_cap_micro_cents":0,"allow_paid_tier":false}"#;
    assert!(
        serde_json::from_str::<BudgetConstraint>(json).is_err(),
        "a negative cap is malformed"
    );
}

#[test]
fn freshness_window_whose_nanos_overflow_seconds_is_rejected() {
    let mut value = serde_json::to_value(SearchConstraints::default()).unwrap();
    value["freshness_window"] = serde_json::json!({"secs": u64::MAX, "nanos": 1_000_000_000_u32});
    assert!(
        serde_json::from_value::<SearchConstraints>(value).is_err(),
        "a duration that overflows u64 seconds must be rejected"
    );
}

#[test]
fn malformed_language_tag_is_rejected() {
    let mut value = serde_json::to_value(SearchConstraints::default()).unwrap();
    value["language"] = serde_json::Value::String("not a tag!".to_owned());
    assert!(
        serde_json::from_value::<SearchConstraints>(value).is_err(),
        "an invalid BCP-47 tag must be rejected at decode"
    );
}

#[test]
fn search_constraints_unknown_field_is_rejected() {
    let mut value = serde_json::to_value(SearchConstraints::default()).unwrap();
    value["allow_paid_fallback"] = serde_json::Value::Bool(true);
    assert!(
        serde_json::from_value::<SearchConstraints>(value).is_err(),
        "an unknown constraint must not be silently dropped"
    );
}

#[test]
fn result_hit_full_text_over_the_cap_is_rejected_at_decode() {
    // WHY: `with_full_text` caps the body at `MAX_FULL_TEXT_BYTES`, but the
    // field is `pub` and deserializable, so a decoded provider payload
    // could carry an unbounded body past the documented cap.
    let hit = sylloge::ResultHit::new(
        "t",
        "s",
        url("https://example.org/"),
        vec![sylloge::Citation::new(
            url("https://example.org/"),
            "2026-07-01T00:00:00Z".parse().unwrap(),
            sylloge::SourceKind::Web,
            1.0,
            None,
        )],
        0.5,
    )
    .unwrap();
    let mut value = serde_json::to_value(&hit).unwrap();
    value["full_text"] =
        serde_json::Value::String("x".repeat(sylloge::ResultHit::MAX_FULL_TEXT_BYTES + 1));
    assert!(
        serde_json::from_value::<sylloge::ResultHit>(value).is_err(),
        "a decoded full_text over the cap must be rejected"
    );
}
