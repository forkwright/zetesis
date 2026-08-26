//! Fail-closed network-target policy for every URL a [`super::Crawler`]
//! implementation fetches or follows a redirect to (zetesis#48).
//!
//! [`SearchConstraints::check_url`], [`SearchConstraints::check_url_with`],
//! and [`SearchConstraints::check_url_with_local_authorization`] are the
//! enforced entry points: scheme allowlist, no userinfo, and --
//! critically -- classification of the RESOLVED address (via [`Resolver`],
//! defaulting to [`SystemResolver`]) rather than the URL's text, which is
//! what stops a hostname that resolves to a loopback/private/link-local
//! address from slipping past a purely textual check (DNS rebinding).

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, ToSocketAddrs};

use snafu::ensure;
use url::{Host, Url};

use crate::constraints::{SearchConstraints, matches_suffix};
use crate::error::{Result, TransientIoSnafu, UnsafeTargetSnafu};

/// Schemes [`SearchConstraints::check_url`] permits. Every other scheme
/// (`file`, `data`, `ftp`, ...) is rejected outright -- see zetesis#48.
const ALLOWED_SCHEMES: [&str; 2] = ["http", "https"];

/// Opaque authority required to permit a local or otherwise blocked network
/// target.
///
/// The capability has no public constructor, is not cloneable, and is not
/// serializable or deserializable. Untrusted query/configuration data therefore
/// cannot manufacture or persist it. No public minting path exists; process
/// wiring must define a real authority boundary before local acquisition can be
/// enabled.
///
/// ```compile_fail
/// use sylloge::LocalTargetAuthorization;
/// let _authorization = LocalTargetAuthorization { _private: () };
/// ```
#[derive(Debug)]
#[non_exhaustive]
pub struct LocalTargetAuthorization {
    _private: (),
}

impl SearchConstraints {
    /// Full fail-closed network-target check using the OS resolver
    /// ([`SystemResolver`]). See [`SearchConstraints::check_url_with`]
    /// for the resolver-injectable form and the full policy description.
    ///
    /// WARNING: performs blocking DNS I/O for a domain-name host (see
    /// [`SystemResolver`]). A caller running inside an async executor
    /// should invoke this via `spawn_blocking` rather than directly on
    /// the executor thread.
    ///
    /// # Errors
    ///
    /// See [`SearchConstraints::check_url_with`].
    pub fn check_url(&self, url: &Url) -> Result<ValidatedTarget> {
        self.check_url_with(url, &SystemResolver)
    }

    /// Full fail-closed network-target check, resolving the host through
    /// `resolver` rather than the OS default (see [`SystemResolver`]).
    /// Injecting a resolver lets a caller pin the connect-time lookup, use
    /// a caching/`DoH` resolver, or (in tests) supply a canned result
    /// without touching the network.
    ///
    /// Enforces, in order:
    /// 1. the scheme is in [`ALLOWED_SCHEMES`] (`http`, `https`);
    /// 2. the URL carries no userinfo;
    /// 3. a host is present (see the WARNING below);
    /// 4. `resolver` resolves the host to concrete address(es), none of which
    ///    may be loopback, private, link-local, unspecified, multicast, or
    ///    reserved. This is checked on the RESOLVED address, never the URL
    ///    text: a hostname that resolves to `127.0.0.1` fails here even though
    ///    its text names no local address, which is what a purely textual check
    ///    misses (DNS rebinding);
    /// 5. the caller's domain allow/denylist, evaluated last so a
    ///    caller-configured allowlist can never re-admit a
    ///    network-unsafe target.
    ///
    /// The host is resolved exactly once per call. Because a DNS answer
    /// can change between this check and a later connect (rebinding), a
    /// compliant [`super::Crawler`] implementation connects to one of
    /// [`ValidatedTarget::addrs`] directly rather than letting its HTTP
    /// client re-resolve the hostname -- and calls this check again
    /// against every redirect target, including the final URL, rather
    /// than trusting the initial URL's pass to cover the whole chain.
    ///
    /// WARNING: for `http`/`https` (the only schemes this reaches), the
    /// `url` crate's WHATWG-compliant parser never produces a hostless
    /// `Url`, so the "host is present" check is unreachable defense in
    /// depth today, not a path this function's own tests can exercise
    /// through an allowed scheme -- see the `file`/`data` scheme
    /// rejection tests for the hostless URLs this crate actually
    /// receives.
    ///
    /// # Errors
    ///
    /// Returns [`crate::Error::UnsafeTarget`] for every policy rejection
    /// (disallowed scheme, userinfo, missing host, an empty or blocked
    /// resolution, domain deny/allow mismatch), and
    /// [`crate::Error::TransientIo`] if `resolver` itself fails.
    pub fn check_url_with(&self, url: &Url, resolver: &dyn Resolver) -> Result<ValidatedTarget> {
        self.check_url_with_policy(url, resolver, None)
    }

    /// Check a URL while carrying explicit process authority for local or
    /// otherwise blocked address ranges.
    ///
    /// This is distinct from [`SearchConstraints`] because constraints are
    /// serializable caller data while authority must be supplied at the exact
    /// network-policy boundary. [`LocalTargetAuthorization`] cannot currently
    /// be minted by public callers.
    ///
    /// WARNING: this uses [`SystemResolver`] and therefore performs blocking
    /// DNS I/O for a domain-name host. Async callers must offload the call.
    ///
    /// # Errors
    ///
    /// Returns the same errors as [`SearchConstraints::check_url_with`]. The
    /// capability bypasses only address-range classification; scheme, userinfo,
    /// resolution, and domain allow/deny checks remain mandatory.
    pub fn check_url_with_local_authorization(
        &self,
        url: &Url,
        authorization: &LocalTargetAuthorization,
    ) -> Result<ValidatedTarget> {
        self.check_url_with_policy(url, &SystemResolver, Some(authorization))
    }

    fn check_url_with_policy(
        &self,
        url: &Url,
        resolver: &dyn Resolver,
        local_authorization: Option<&LocalTargetAuthorization>,
    ) -> Result<ValidatedTarget> {
        let scheme = url.scheme();
        ensure!(
            ALLOWED_SCHEMES.contains(&scheme),
            UnsafeTargetSnafu {
                url: url.to_string(),
                reason: format!("scheme '{scheme}' is outside the allowed set {ALLOWED_SCHEMES:?}"),
            }
        );
        ensure!(
            url.username().is_empty() && url.password().is_none(),
            UnsafeTargetSnafu {
                url: url.to_string(),
                reason: "URL carries userinfo, which is never permitted",
            }
        );
        let Some(host) = url.host() else {
            return Err(UnsafeTargetSnafu {
                url: url.to_string(),
                reason: "URL has no host",
            }
            .build());
        };

        let addrs = match host {
            Host::Ipv4(v4) => vec![IpAddr::V4(v4)],
            Host::Ipv6(v6) => vec![IpAddr::V6(v6)],
            Host::Domain(name) => {
                let port = url.port_or_known_default().unwrap_or(0);
                resolver.resolve(name, port).map_err(|source| {
                    TransientIoSnafu {
                        message: format!("DNS resolution failed for '{name}': {source}"),
                    }
                    .build()
                })?
            }
        };
        ensure!(
            !addrs.is_empty(),
            UnsafeTargetSnafu {
                url: url.to_string(),
                reason: "resolution returned no addresses",
            }
        );

        if local_authorization.is_none() {
            for addr in &addrs {
                ensure!(
                    !is_blocked_address(*addr),
                    UnsafeTargetSnafu {
                        url: url.to_string(),
                        reason: format!("resolved address {addr} is in a blocked range"),
                    }
                );
            }
        }

        let host_str = url.host_str().unwrap_or("");
        if let Some(deny) = &self.domain_denylist {
            ensure!(
                !deny.iter().any(|suffix| matches_suffix(host_str, suffix)),
                UnsafeTargetSnafu {
                    url: url.to_string(),
                    reason: "host matches the configured denylist",
                }
            );
        }
        if let Some(allow) = &self.domain_allowlist {
            ensure!(
                allow.iter().any(|suffix| matches_suffix(host_str, suffix)),
                UnsafeTargetSnafu {
                    url: url.to_string(),
                    reason: "host does not match the configured allowlist",
                }
            );
        }

        Ok(ValidatedTarget {
            url: url.clone(),
            addrs,
        })
    }
}

/// Resolves a host to its concrete address(es), for
/// [`SearchConstraints::check_url_with`]. [`SearchConstraints::check_url`]
/// uses [`SystemResolver`]; a caller can inject any other implementation
/// -- a caching or `DoH` resolver, or (in tests) a canned result that
/// exercises resolution-dependent policy (e.g. DNS rebinding) without
/// touching the network.
pub trait Resolver {
    /// Resolve `host` (a non-empty hostname; [`SearchConstraints`] has
    /// already handled IP-literal hosts before calling this) to its
    /// concrete address(es). `port` is carried through only for
    /// implementations that build [`std::net::SocketAddr`] directly; it
    /// carries no safety meaning to the caller.
    ///
    /// # Errors
    ///
    /// Any resolution failure (NXDOMAIN, timeout, resolver unreachable).
    fn resolve(&self, host: &str, port: u16) -> std::io::Result<Vec<IpAddr>>;
}

/// The OS resolver, via blocking [`std::net::ToSocketAddrs`].
///
/// WARNING: this performs synchronous, blocking DNS I/O. A caller running
/// inside an async executor must offload it (e.g.
/// `tokio::task::spawn_blocking`) rather than calling
/// [`SearchConstraints::check_url`] directly from the executor thread.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemResolver;

impl Resolver for SystemResolver {
    fn resolve(&self, host: &str, port: u16) -> std::io::Result<Vec<IpAddr>> {
        Ok((host, port)
            .to_socket_addrs()?
            .map(|addr| addr.ip())
            .collect())
    }
}

/// Proof that a URL passed the full network-target policy in
/// [`SearchConstraints::check_url`], [`SearchConstraints::check_url_with`],
/// or [`SearchConstraints::check_url_with_local_authorization`], carrying the
/// resolution that proof was based on.
///
/// This is the enforced capability, not an optional convention:
/// private fields plus the absence of any public constructor other than those
/// three policy checks mean no other crate can build one from scratch or
/// retarget it after validation.
/// [`super::Crawler`] requires one as its entry parameter (see
/// [`super::Crawler::fetch_page`]), so calling it with a URL that was
/// never checked does not compile -- see that trait's `# Enforcement`
/// doctest.
///
/// A compliant [`super::Crawler`] implementation fetches `url` but
/// connects to one of [`ValidatedTarget::addrs`] directly rather than
/// letting its HTTP client re-resolve the hostname -- reusing the
/// validated resolution instead of re-resolving is what closes the
/// check-time/connect-time gap a DNS answer can otherwise change across
/// (rebinding).
///
/// ```compile_fail
/// # use sylloge::ValidatedTarget;
/// # use url::Url;
/// fn retarget(target: &mut ValidatedTarget, url: Url) {
///     target.url = url;
/// }
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ValidatedTarget {
    /// The exact URL this target was validated for. A compliant fetch
    /// issues its request against this URL (path, query, `Host`
    /// header/TLS SNI) while connecting the socket to `addrs`.
    url: Url,
    /// Concrete resolved address(es) that passed the policy. Non-empty.
    addrs: Vec<IpAddr>,
}

impl ValidatedTarget {
    /// The immutable URL for which this policy proof was issued.
    #[must_use]
    pub const fn url(&self) -> &Url {
        &self.url
    }

    /// The immutable, non-empty resolution that passed policy.
    #[must_use]
    pub fn addrs(&self) -> &[IpAddr] {
        &self.addrs
    }
}

// WHY: several ranges relevant to SSRF (240.0.0.0/4 reserved,
// 198.18.0.0/15 benchmarking, RFC 6598 shared/carrier-grade-NAT space,
// 192.0.0.0/24 IETF protocol assignment) are exposed by
// `std::net::Ipv4Addr` only behind the unstable `ip` feature, so
// classification is hand-rolled on octets instead of composing
// `Ipv4Addr::is_*`.
fn is_blocked_ipv4(ip: Ipv4Addr) -> bool {
    let [leading, second, third, _fourth] = ip.octets();

    let this_network = leading == 0;
    let rfc1918_10 = leading == 10;
    let loopback = leading == 127;
    let carrier_grade_nat = leading == 100 && (64..=127).contains(&second);
    let link_local = leading == 169 && second == 254;
    let rfc1918_172 = leading == 172 && (16..=31).contains(&second);
    let ietf_protocol_assignment = leading == 192 && second == 0 && third == 0;
    let documentation_test_net_1 = leading == 192 && second == 0 && third == 2;
    let rfc1918_192 = leading == 192 && second == 168;
    let benchmarking = leading == 198 && (18..=19).contains(&second);
    let documentation_test_net_2 = leading == 198 && second == 51 && third == 100;
    let documentation_test_net_3 = leading == 203 && second == 0 && third == 113;
    // NOTE: 224.0.0.0/4 multicast and 240.0.0.0/4 reserved (which
    // includes the 255.255.255.255 broadcast address) are contiguous, so
    // one comparison covers both.
    let multicast_reserved_or_broadcast = leading >= 224;

    this_network
        || rfc1918_10
        || loopback
        || carrier_grade_nat
        || link_local
        || rfc1918_172
        || ietf_protocol_assignment
        || documentation_test_net_1
        || rfc1918_192
        || benchmarking
        || documentation_test_net_2
        || documentation_test_net_3
        || multicast_reserved_or_broadcast
}

// WHY: `Ipv6Addr::is_unique_local`/`is_unicast_link_local` are unstable
// (`ip` feature); fc00::/7 and fe80::/10 are matched on the leading
// segment directly instead.
fn is_blocked_ipv6(ip: Ipv6Addr) -> bool {
    let loopback = ip.is_loopback();
    let unspecified = ip.is_unspecified();
    let multicast = ip.is_multicast();
    let [leading_segment, ..] = ip.segments();
    let unique_local = leading_segment & 0xfe00 == 0xfc00;
    let link_local = leading_segment & 0xffc0 == 0xfe80;

    loopback || unspecified || multicast || unique_local || link_local
}

/// Whether `addr` falls in a blocked range (loopback, private,
/// link-local, unspecified, multicast, or reserved). Both RFC 4291 IPv6
/// forms that embed an IPv4 address in the low 32 bits -- the IPv4-mapped
/// form (`::ffff:a.b.c.d`, high 80 bits zero then 16 bits of `0xffff`)
/// AND the deprecated IPv4-compatible form (`::a.b.c.d`, high 96 bits
/// all zero) -- are unwrapped and classified as their embedded IPv4
/// address, so `::ffff:127.0.0.1` and `::127.0.0.1` are both blocked the
/// same as `127.0.0.1` -- see zetesis#48's mapped-address fixture.
/// [`Ipv6Addr::to_ipv4`] (not the narrower `to_ipv4_mapped`, which
/// recognizes only the mapped form) unwraps both.
fn is_blocked_address(addr: IpAddr) -> bool {
    match addr {
        IpAddr::V4(v4) => is_blocked_ipv4(v4),
        IpAddr::V6(v6) => v6
            .to_ipv4()
            .map_or_else(|| is_blocked_ipv6(v6), is_blocked_ipv4),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::BudgetConstraint;

    /// Test [`Resolver`] that returns a fixed address set regardless of
    /// the hostname asked for -- lets domain-allow/deny-suffix tests use
    /// whatever hostname strings they like without depending on live DNS.
    struct FixedResolver(Vec<IpAddr>);

    impl Resolver for FixedResolver {
        fn resolve(&self, _host: &str, _port: u16) -> std::io::Result<Vec<IpAddr>> {
            Ok(self.0.clone())
        }
    }

    /// A [`FixedResolver`] resolving to a single known-public address
    /// (Google Public DNS), for tests exercising domain-suffix logic
    /// rather than address classification.
    fn public_resolver() -> FixedResolver {
        FixedResolver(vec![IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8))])
    }

    #[test]
    fn check_url_exact_match() {
        let c = SearchConstraints::new(10, BudgetConstraint::default())
            .with_allowlist(vec!["example.org".to_owned()]);
        let r = public_resolver();
        assert!(
            c.check_url_with(&Url::parse("https://example.org/x").unwrap(), &r)
                .is_ok()
        );
        assert!(
            c.check_url_with(&Url::parse("https://example.com/x").unwrap(), &r)
                .is_err()
        );
    }

    #[test]
    fn check_url_suffix_match() {
        let c = SearchConstraints::new(10, BudgetConstraint::default())
            .with_allowlist(vec![".edu".to_owned()]);
        let r = public_resolver();
        assert!(
            c.check_url_with(&Url::parse("https://mit.edu/").unwrap(), &r)
                .is_ok()
        );
        assert!(
            c.check_url_with(&Url::parse("https://cs.mit.edu/").unwrap(), &r)
                .is_ok()
        );
        assert!(
            c.check_url_with(&Url::parse("https://mit.com/").unwrap(), &r)
                .is_err()
        );
    }

    #[test]
    fn check_url_mixed_case_denylist_blocks() {
        let c = SearchConstraints::new(10, BudgetConstraint::default())
            .with_denylist(vec!["Tracker.Example".to_owned()]);
        let r = public_resolver();
        assert!(
            c.check_url_with(&Url::parse("https://tracker.example/pixel").unwrap(), &r)
                .is_err()
        );
        assert!(
            c.check_url_with(&Url::parse("https://sub.tracker.example/x").unwrap(), &r)
                .is_err()
        );
        assert!(
            c.check_url_with(&Url::parse("https://other.example/x").unwrap(), &r)
                .is_ok()
        );
    }

    #[test]
    fn check_url_mixed_case_allowlist_matches() {
        let c = SearchConstraints::new(10, BudgetConstraint::default())
            .with_allowlist(vec![".EDU".to_owned()]);
        let r = public_resolver();
        assert!(
            c.check_url_with(&Url::parse("https://mit.edu/").unwrap(), &r)
                .is_ok()
        );
        assert!(
            c.check_url_with(&Url::parse("https://mit.com/").unwrap(), &r)
                .is_err()
        );
    }

    #[test]
    fn check_url_accepts_trailing_root_dot_domain() {
        // Fully-qualified hosts with a trailing root dot are the same
        // domain as their bare form -- see matches_suffix's own tests in
        // constraints.rs for the string-matching half of this.
        let c = SearchConstraints::new(10, BudgetConstraint::default())
            .with_allowlist(vec![".edu".to_owned()]);
        assert!(
            c.check_url_with(
                &Url::parse("https://mit.edu./").unwrap(),
                &public_resolver()
            )
            .is_ok()
        );
    }

    #[test]
    fn check_url_denylist_rejects() {
        let c = SearchConstraints::new(10, BudgetConstraint::default())
            .with_denylist(vec!["evil.example".to_owned()]);
        let r = public_resolver();
        assert!(
            c.check_url_with(&Url::parse("https://evil.example/x").unwrap(), &r)
                .is_err()
        );
        assert!(
            c.check_url_with(&Url::parse("https://good.example/x").unwrap(), &r)
                .is_ok()
        );
    }

    #[test]
    fn check_url_deny_takes_precedence() {
        let c = SearchConstraints::new(10, BudgetConstraint::default())
            .with_allowlist(vec!["example.org".to_owned()])
            .with_denylist(vec!["bad.example.org".to_owned()]);
        let r = public_resolver();
        assert!(
            c.check_url_with(&Url::parse("https://example.org/").unwrap(), &r)
                .is_ok()
        );
        assert!(
            c.check_url_with(&Url::parse("https://bad.example.org/").unwrap(), &r)
                .is_err()
        );
    }

    #[test]
    fn check_url_suffix_prefix_boundary() {
        // "mit.edu" must not match "badmit.edu" (no dot boundary).
        let c = SearchConstraints::new(10, BudgetConstraint::default())
            .with_allowlist(vec!["mit.edu".to_owned()]);
        let r = public_resolver();
        assert!(
            c.check_url_with(&Url::parse("https://badmit.edu/").unwrap(), &r)
                .is_err()
        );
        assert!(
            c.check_url_with(&Url::parse("https://cs.mit.edu/").unwrap(), &r)
                .is_ok()
        );
    }

    #[test]
    fn dot_prefix_and_bare_suffix_equivalent() {
        let c_dot = SearchConstraints::default().with_allowlist(vec![".edu".to_owned()]);
        let c_bare = SearchConstraints::default().with_allowlist(vec!["edu".to_owned()]);
        let url = Url::parse("https://cs.mit.edu/").unwrap();
        let r = public_resolver();
        assert_eq!(
            c_dot.check_url_with(&url, &r).is_ok(),
            c_bare.check_url_with(&url, &r).is_ok()
        );
    }

    // -- Network-target policy (zetesis#48): one negative-case fixture per
    // rejected class, plus the DNS-rebinding and capability-escape-hatch
    // fixtures the issue calls out by name. --

    #[test]
    fn check_url_rejects_file_scheme() {
        // WHY: the exact SSRF repro from zetesis#48 -- a crawler blindly
        // following a provider-supplied `file:` URL reads local disk.
        let c = SearchConstraints::default();
        let err = c
            .check_url(&Url::parse("file:///etc/passwd").unwrap())
            .unwrap_err();
        assert!(err.is_permanent());
        assert!(format!("{err}").contains("scheme"));
    }

    #[test]
    fn check_url_rejects_data_scheme() {
        let c = SearchConstraints::default();
        let err = c
            .check_url(&Url::parse("data:text/plain,secret").unwrap())
            .unwrap_err();
        assert!(err.is_permanent());
    }

    #[test]
    fn check_url_rejects_ftp_scheme() {
        // WHY: proves the policy is an allowlist (only http/https), not a
        // denylist of the two schemes the issue happened to name.
        let c = SearchConstraints::default();
        assert!(
            c.check_url(&Url::parse("ftp://example.org/x").unwrap())
                .is_err()
        );
    }

    #[test]
    fn check_url_rejects_userinfo() {
        let c = SearchConstraints::default();
        let err = c
            .check_url(&Url::parse("https://user:pass@8.8.8.8/").unwrap())
            .unwrap_err();
        assert!(err.is_permanent());
        assert!(format!("{err}").contains("userinfo"));
    }

    #[test]
    fn check_url_rejects_loopback_ipv4() {
        // WHY: the exact SSRF repro from zetesis#48.
        let c = SearchConstraints::default();
        let err = c
            .check_url(&Url::parse("http://127.0.0.1/admin").unwrap())
            .unwrap_err();
        assert!(err.is_permanent());
        assert!(format!("{err}").contains("127.0.0.1"));
    }

    #[test]
    fn check_url_rejects_loopback_ipv6() {
        let c = SearchConstraints::default();
        assert!(
            c.check_url(&Url::parse("http://[::1]/admin").unwrap())
                .is_err()
        );
    }

    #[test]
    fn check_url_rejects_link_local_metadata_ipv4() {
        // WHY: the cloud instance-metadata endpoint from zetesis#48 --
        // reachable, this is instance-credential theft.
        let c = SearchConstraints::default();
        let err = c
            .check_url(&Url::parse("http://169.254.169.254/latest/meta-data/").unwrap())
            .unwrap_err();
        assert!(err.is_permanent());
        assert!(format!("{err}").contains("169.254.169.254"));
    }

    #[test]
    fn check_url_rejects_rfc1918_ranges() {
        let c = SearchConstraints::default();
        for target in [
            "http://10.1.2.3/",
            "http://172.16.5.5/",
            "http://192.168.1.1/",
        ] {
            assert!(
                c.check_url(&Url::parse(target).unwrap()).is_err(),
                "{target} must be rejected"
            );
        }
    }

    #[test]
    fn check_url_rejects_carrier_grade_nat() {
        let c = SearchConstraints::default();
        assert!(
            c.check_url(&Url::parse("http://100.64.0.1/").unwrap())
                .is_err()
        );
    }

    #[test]
    fn check_url_rejects_unspecified_ipv4() {
        let c = SearchConstraints::default();
        assert!(
            c.check_url(&Url::parse("http://0.0.0.0/").unwrap())
                .is_err()
        );
    }

    #[test]
    fn check_url_rejects_multicast_ipv4() {
        let c = SearchConstraints::default();
        assert!(
            c.check_url(&Url::parse("http://224.0.0.1/").unwrap())
                .is_err()
        );
    }

    #[test]
    fn check_url_rejects_reserved_ipv4() {
        let c = SearchConstraints::default();
        assert!(
            c.check_url(&Url::parse("http://240.0.0.1/").unwrap())
                .is_err()
        );
    }

    #[test]
    fn check_url_rejects_broadcast_ipv4() {
        let c = SearchConstraints::default();
        assert!(
            c.check_url(&Url::parse("http://255.255.255.255/").unwrap())
                .is_err()
        );
    }

    #[test]
    fn check_url_rejects_ipv6_unspecified() {
        let c = SearchConstraints::default();
        assert!(c.check_url(&Url::parse("http://[::]/").unwrap()).is_err());
    }

    #[test]
    fn check_url_rejects_ipv6_unique_local() {
        let c = SearchConstraints::default();
        assert!(
            c.check_url(&Url::parse("http://[fc00::1]/").unwrap())
                .is_err()
        );
    }

    #[test]
    fn check_url_rejects_ipv6_link_local() {
        let c = SearchConstraints::default();
        assert!(
            c.check_url(&Url::parse("http://[fe80::1]/").unwrap())
                .is_err()
        );
    }

    #[test]
    fn check_url_rejects_ipv6_multicast() {
        let c = SearchConstraints::default();
        assert!(
            c.check_url(&Url::parse("http://[ff02::1]/").unwrap())
                .is_err()
        );
    }

    #[test]
    fn check_url_rejects_ipv4_mapped_ipv6_loopback() {
        // WHY: the mapped-address fixture zetesis#48 names by name -- a
        // textual host-string check would see only the IPv6 literal and
        // miss the embedded loopback address.
        let c = SearchConstraints::default();
        let err = c
            .check_url(&Url::parse("http://[::ffff:127.0.0.1]/").unwrap())
            .unwrap_err();
        assert!(err.is_permanent());
    }

    #[test]
    fn check_url_permits_ipv4_mapped_ipv6_public() {
        // WHY: contrast case -- mapped addresses are unwrapped and
        // classified, not blanket-rejected as a form.
        let c = SearchConstraints::default();
        assert!(
            c.check_url(&Url::parse("http://[::ffff:8.8.8.8]/").unwrap())
                .is_ok()
        );
    }

    #[test]
    fn check_url_rejects_ipv4_compatible_ipv6_loopback() {
        // WHY: the deprecated RFC 4291 "IPv4-compatible" embedding
        // (`::a.b.c.d`, all-zero high 96 bits, distinct from the
        // IPv4-mapped `::ffff:a.b.c.d` form) is valid `Host::Ipv6` literal
        // syntax and must be unwrapped and classified the same as its
        // mapped counterpart -- `to_ipv4_mapped` alone does not see it.
        let c = SearchConstraints::default();
        let err = c
            .check_url(&Url::parse("http://[::127.0.0.1]/admin").unwrap())
            .unwrap_err();
        assert!(err.is_permanent());
    }

    #[test]
    fn check_url_rejects_ipv4_compatible_ipv6_metadata() {
        // WHY: the same embedding for the cloud metadata IP the mapped-form
        // fixture above already covers -- proves this isn't loopback-only.
        let c = SearchConstraints::default();
        assert!(
            c.check_url(&Url::parse("http://[::169.254.169.254]/latest/meta-data/").unwrap())
                .is_err()
        );
    }

    #[test]
    fn check_url_permits_ipv4_compatible_ipv6_public() {
        // WHY: contrast case -- the compatible form is unwrapped and
        // classified, not blanket-rejected as a form, mirroring
        // check_url_permits_ipv4_mapped_ipv6_public.
        let c = SearchConstraints::default();
        assert!(
            c.check_url(&Url::parse("http://[::8.8.8.8]/").unwrap())
                .is_ok()
        );
    }

    #[test]
    fn check_url_dns_rebinding_rejects_when_resolved_address_is_blocked() {
        // WHY: the fixture zetesis#48 requires -- a hostname whose text
        // names no local address at all resolves, at check time, to a
        // loopback address. A purely textual check (matching the old
        // domain-suffix-only `permits_url`) cannot see this; the check
        // must run on the resolver's output.
        let c = SearchConstraints::default();
        let rebinding = FixedResolver(vec![IpAddr::V4(Ipv4Addr::LOCALHOST)]);
        let err = c
            .check_url_with(
                &Url::parse("https://looks-external.example/").unwrap(),
                &rebinding,
            )
            .unwrap_err();
        assert!(err.is_permanent());
        assert!(format!("{err}").contains("127.0.0.1"));
    }

    #[test]
    fn check_url_dns_rebinding_permits_when_resolved_address_is_public() {
        // WHY: contrast case for the fixture above -- the same hostname
        // shape passes when it genuinely resolves to a public address.
        let c = SearchConstraints::default();
        let url = Url::parse("https://looks-external.example/").unwrap();
        assert!(c.check_url_with(&url, &public_resolver()).is_ok());
    }

    #[test]
    fn check_url_with_system_resolver_rejects_localhost() {
        // WHY: proves the production wiring end-to-end -- `check_url`
        // (the real `SystemResolver`, no injected fake) resolves
        // "localhost" via the OS (no live network required; loopback
        // resolves locally) and still rejects it.
        let c = SearchConstraints::default();
        let err = c
            .check_url(&Url::parse("http://localhost/").unwrap())
            .unwrap_err();
        assert!(err.is_permanent());
    }

    #[test]
    fn local_target_authorization_permits_loopback() {
        let c = SearchConstraints::default();
        let authorization = LocalTargetAuthorization { _private: () };
        assert!(
            c.check_url_with_local_authorization(
                &Url::parse("http://127.0.0.1/admin").unwrap(),
                &authorization,
            )
            .is_ok()
        );
    }

    #[test]
    fn local_target_authorization_does_not_bypass_domain_denylist() {
        // WHY: the two checks are independent layers -- the local-target
        // escape hatch only reopens the address-class check, never the
        // caller's own domain policy.
        let c = SearchConstraints::default().with_denylist(vec!["127.0.0.1".to_owned()]);
        let authorization = LocalTargetAuthorization { _private: () };
        assert!(
            c.check_url_with_local_authorization(
                &Url::parse("http://127.0.0.1/admin").unwrap(),
                &authorization,
            )
            .is_err()
        );
    }

    #[test]
    fn check_url_ok_returns_resolved_addresses() {
        // WHY: a compliant Crawler connects to these addresses directly
        // (see the trait contract in crawler.rs) rather than
        // re-resolving; this proves the check actually exposes them.
        let c = SearchConstraints::default();
        let url = Url::parse("http://8.8.8.8/").unwrap();
        let target = c.check_url(&url).unwrap();
        assert_eq!(target.addrs(), [IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8))]);
        assert_eq!(target.url(), &url);
    }
}
