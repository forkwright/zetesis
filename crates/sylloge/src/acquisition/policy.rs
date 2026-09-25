//! Per-hop transfer policy that runs before resolution: scheme, downgrade,
//! and port checks, plus redirect-target resolution.
//!
//! Every check reads the parsed [`Url`], never URL text. `url::Url` is the
//! WHATWG URL Standard's canonical form: the host parser has already turned
//! decimal (`2130706433`), hexadecimal (`0x7f.1`), octal (`0177.0.0.1`),
//! shortened, and percent-encoded IPv4 spellings into `Host::Ipv4`,
//! lowercased the scheme and domain, and dropped a port equal to the
//! scheme's default. Redirect targets are resolved with [`Url::join`], which
//! applies the same parser, so a `Location` value gets the same
//! canonicalization as the requested URL before any authority decision.

use hyper::header::HeaderValue;
use url::Url;

use super::limits::{AcquisitionLimits, DowngradePolicy};
use super::record::AcquisitionFailure;

/// The WHATWG Fetch standard's "bad port" list (section 2.9, "Port
/// blocking"): ports a fetch of an HTTP(S) URL must refuse because they
/// belong to non-HTTP services that a crafted request could talk to.
///
/// Source: <https://fetch.spec.whatwg.org/#bad-port>, Living Standard last
/// updated 2026-09-21, retrieved 2026-09-25. Sorted for binary search.
const BAD_PORTS: [u16; 83] = [
    0, 1, 7, 9, 11, 13, 15, 17, 19, 20, 21, 22, 23, 25, 37, 42, 43, 53, 69, 77, 79, 87, 95, 101,
    102, 103, 104, 109, 110, 111, 113, 115, 117, 119, 123, 135, 137, 139, 143, 161, 179, 389, 427,
    465, 512, 513, 514, 515, 526, 530, 531, 532, 540, 548, 554, 556, 563, 587, 601, 636, 989, 990,
    993, 995, 1719, 1720, 1723, 2049, 3659, 4045, 4190, 5060, 5061, 6000, 6566, 6665, 6666, 6667,
    6668, 6669, 6679, 6697, 10080,
];

/// Whether `port` is a Fetch "bad port".
pub(crate) fn is_bad_port(port: u16) -> bool {
    BAD_PORTS.binary_search(&port).is_ok()
}

/// Run the pre-resolution checks for one hop and return the port the hop
/// connects to.
///
/// Order: scheme in the caller's set, then no `https` to `http` downgrade
/// from `previous` (unless allowed), then the effective port is not a bad
/// port.
pub(crate) fn check_hop(
    url: &Url,
    previous: Option<&Url>,
    limits: &AcquisitionLimits,
) -> Result<u16, AcquisitionFailure> {
    let scheme = url.scheme();
    if !limits.schemes().permits(scheme) {
        return Err(AcquisitionFailure::SchemeNotAllowed {
            scheme: scheme.to_owned(),
        });
    }
    let downgrade = previous.is_some_and(|prev| prev.scheme() == "https") && scheme == "http";
    if downgrade && limits.downgrade() == DowngradePolicy::Refuse {
        return Err(AcquisitionFailure::DowngradeRefused);
    }
    // INVARIANT: only http and https pass the scheme check above, and both
    // have a known default port, so `None` is unreachable; it still fails
    // closed.
    let Some(port) = url.port_or_known_default() else {
        return Err(AcquisitionFailure::UnsafeTarget {
            reason: "URL has no effective port".to_owned(),
        });
    };
    if is_bad_port(port) {
        return Err(AcquisitionFailure::DeniedPort { port });
    }
    Ok(port)
}

/// Whether `url` carries userinfo (a user name or password).
///
/// WHY: the network-target policy refuses userinfo too, but only after
/// resolution, and a hop URL is recorded as evidence. Checking first keeps
/// a URL that can never be fetched from causing a DNS lookup and keeps a
/// credential out of every hop record.
pub(crate) fn has_userinfo(url: &Url) -> bool {
    !url.username().is_empty() || url.password().is_some()
}

/// `url` with its userinfo removed, for recording a refused URL as
/// evidence without the credential it carried.
pub(crate) fn without_userinfo(url: &Url) -> Url {
    let mut redacted = url.clone();
    // NOTE: both setters fail only for a URL that cannot carry userinfo,
    // which then has none to remove.
    let _ = redacted.set_username("");
    let _ = redacted.set_password(None);
    redacted
}

/// Resolve a `Location` header value against the current hop's URL.
///
/// Follows Fetch's location URL rules: parse relative to the current URL,
/// and inherit the current fragment when the target has none. The result
/// must fit `max_url_bytes`.
pub(crate) fn redirect_target(
    current: &Url,
    location: &HeaderValue,
    limits: &AcquisitionLimits,
) -> Result<Url, AcquisitionFailure> {
    let raw = location_evidence(current, location);
    let Ok(text) = std::str::from_utf8(location.as_bytes()) else {
        return Err(AcquisitionFailure::MalformedRedirect {
            location: raw,
            reason: "Location is not valid UTF-8".to_owned(),
        });
    };
    let mut next = current
        .join(text)
        .map_err(|source| AcquisitionFailure::MalformedRedirect {
            location: raw.clone(),
            reason: source.to_string(),
        })?;
    if next.fragment().is_none() {
        next.set_fragment(current.fragment());
    }
    if next.as_str().len() > limits.max_url_bytes() {
        return Err(AcquisitionFailure::MalformedRedirect {
            location: raw,
            reason: format!(
                "resolved target is {} bytes, over max_url_bytes {}",
                next.as_str().len(),
                limits.max_url_bytes()
            ),
        });
    }
    Ok(next)
}

/// Recorded in place of an unparseable `Location` that might carry a
/// credential.
const WITHHELD_LOCATION: &str = "[withheld: unparseable Location containing '@']";

/// A `Location` value as recorded in evidence, with no credential in it.
///
/// A value that resolves against `current` to a URL with userinfo is
/// recorded as that URL without its userinfo. A value that does not
/// resolve and contains `@` is withheld, because only the URL parser can
/// say where its userinfo would be. Anything else is recorded as received
/// (lossy UTF-8).
///
/// WHY: the WHATWG parser finds userinfo in spellings a text scan misses
/// (another special scheme without slashes, a scheme-relative reference, a
/// tab inside the userinfo), so the parser, not a pattern, decides.
pub(crate) fn location_evidence(current: &Url, location: &HeaderValue) -> String {
    let received = String::from_utf8_lossy(location.as_bytes());
    let resolved = std::str::from_utf8(location.as_bytes())
        .ok()
        .and_then(|text| current.join(text).ok());
    match resolved {
        Some(target) if has_userinfo(&target) => without_userinfo(&target).into(),
        None if received.contains('@') => WITHHELD_LOCATION.to_owned(),
        Some(_) | None => received.into_owned(),
    }
}

/// Key identifying a request target within one redirect chain: the URL
/// without its fragment, which is never sent.
pub(crate) fn chain_key(url: &Url) -> String {
    let mut key = url.clone();
    key.set_fragment(None);
    key.into()
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::acquisition::limits::SchemePolicy;

    fn limits(schemes: SchemePolicy) -> AcquisitionLimits {
        AcquisitionLimits::new(
            schemes,
            5,
            Duration::from_secs(1),
            Duration::from_secs(5),
            1024,
        )
        .unwrap()
    }

    fn url(text: &str) -> Url {
        Url::parse(text).unwrap()
    }

    #[test]
    fn bad_port_list_is_sorted_and_complete() {
        assert!(
            BAD_PORTS.windows(2).all(|pair| pair[0] < pair[1]),
            "binary search needs a strictly ascending list"
        );
        for port in [0, 25, 6000, 10_080] {
            assert!(is_bad_port(port), "{port} is on the Fetch bad-port list");
        }
        for port in [80, 443, 8080, 8443] {
            assert!(!is_bad_port(port), "{port} is an ordinary web port");
        }
    }

    #[test]
    fn check_hop_rejects_scheme_outside_policy() {
        let failure = check_hop(
            &url("http://8.8.8.8/"),
            None,
            &limits(SchemePolicy::HttpsOnly),
        )
        .unwrap_err();
        assert_eq!(
            failure,
            AcquisitionFailure::SchemeNotAllowed {
                scheme: "http".to_owned()
            },
            "http must be refused under https_only"
        );
    }

    #[test]
    fn has_userinfo_detects_user_or_password() {
        assert!(has_userinfo(&url("https://user@8.8.8.8/")), "user name");
        assert!(has_userinfo(&url("https://:secret@8.8.8.8/")), "password");
        assert!(!has_userinfo(&url("https://8.8.8.8/")), "no userinfo");
    }

    #[test]
    fn without_userinfo_strips_the_credential_only() {
        assert_eq!(
            without_userinfo(&url("https://user:secret@8.8.8.8/a?b#c")).as_str(),
            "https://8.8.8.8/a?b#c",
            "everything but the userinfo is kept"
        );
    }

    #[test]
    fn check_hop_refuses_downgrade_by_default() {
        let failure = check_hop(
            &url("http://8.8.8.8/"),
            Some(&url("https://8.8.8.8/")),
            &limits(SchemePolicy::HttpAndHttps),
        )
        .unwrap_err();
        assert_eq!(
            failure,
            AcquisitionFailure::DowngradeRefused,
            "https to http must be refused unless allowed"
        );
    }

    #[test]
    fn check_hop_allows_downgrade_when_opted_in() {
        let allow = limits(SchemePolicy::HttpAndHttps).with_downgrade(DowngradePolicy::Allow);
        let port = check_hop(
            &url("http://8.8.8.8/"),
            Some(&url("https://8.8.8.8/")),
            &allow,
        )
        .unwrap();
        assert_eq!(port, 80, "an allowed downgrade connects to the http port");
    }

    #[test]
    fn check_hop_uses_canonical_default_port() {
        let port = check_hop(
            &url("HTTPS://8.8.8.8:443/"),
            None,
            &limits(SchemePolicy::HttpsOnly),
        )
        .unwrap();
        assert_eq!(port, 443, "an explicit default port canonicalizes away");
    }

    #[test]
    fn redirect_target_inherits_fragment_and_resolves_relative() {
        let next = redirect_target(
            &url("https://8.8.8.8/a/b#frag"),
            &HeaderValue::from_static("../c?q=1"),
            &limits(SchemePolicy::HttpsOnly),
        )
        .unwrap();
        assert_eq!(
            next.as_str(),
            "https://8.8.8.8/c?q=1#frag",
            "relative Location resolves against the hop URL"
        );
    }

    #[test]
    fn redirect_target_canonicalizes_alternate_ipv4_spelling() {
        let next = redirect_target(
            &url("https://8.8.8.8/"),
            &HeaderValue::from_static("http://0x7f.1/"),
            &limits(SchemePolicy::HttpAndHttps),
        )
        .unwrap();
        assert_eq!(
            next.host(),
            Some(url::Host::Ipv4(std::net::Ipv4Addr::LOCALHOST)),
            "hex shorthand must parse to the loopback address, not a domain"
        );
    }

    #[test]
    fn redirect_target_rejects_unparseable_location() {
        let failure = redirect_target(
            &url("https://8.8.8.8/"),
            &HeaderValue::from_static("http://[::1/"),
            &limits(SchemePolicy::HttpsOnly),
        )
        .unwrap_err();
        assert!(
            matches!(failure, AcquisitionFailure::MalformedRedirect { .. }),
            "an unparseable Location is malformed: {failure:?}"
        );
    }

    #[test]
    fn location_evidence_never_carries_userinfo() {
        let current = url("http://8.8.8.8/a");
        for (location, recorded) in [
            ("/users/@alice", "/users/@alice"),
            ("http://user:pw@9.9.9.9/", "http://9.9.9.9/"),
            ("//user:pw@9.9.9.9/x", "http://9.9.9.9/x"),
            ("https:user:pw@9.9.9.9/", "https://9.9.9.9/"),
            ("http://user:pw@[::1/", WITHHELD_LOCATION),
            ("http://[::1/", "http://[::1/"),
        ] {
            let value = HeaderValue::from_str(location).unwrap();
            assert_eq!(
                location_evidence(&current, &value),
                recorded,
                "{location:?}"
            );
        }
        let tabbed = HeaderValue::from_bytes(b"http://us\ter:pw@9.9.9.9/").unwrap();
        assert_eq!(
            location_evidence(&current, &tabbed),
            "http://9.9.9.9/",
            "a tab inside the userinfo is removed before parsing, not a disguise"
        );
        let invalid = HeaderValue::from_bytes(b"http://user:pw@\xff/").unwrap();
        assert_eq!(
            location_evidence(&current, &invalid),
            WITHHELD_LOCATION,
            "non-UTF-8 with '@' cannot be parsed, so it is withheld"
        );
    }

    #[test]
    fn chain_key_ignores_fragment() {
        assert_eq!(
            chain_key(&url("https://8.8.8.8/a#one")),
            chain_key(&url("https://8.8.8.8/a#two")),
            "fragments are never sent, so they cannot distinguish targets"
        );
    }
}
