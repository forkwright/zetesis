//! Owned anonymous static acquisition: one `GET`, every hop proven.
//!
//! [`StaticAcquirer::acquire`] owns the whole transfer from a
//! caller-supplied URL to a bounded response body, including every
//! redirect. There is no trait to implement and no redirect convention to
//! follow: the acquirer validates each hop itself, connects only to the
//! addresses that validation produced, and records what it did in
//! [`HopRecord`]s. No provider credential, budget, or spend ledger is
//! involved; an anonymous `GET` costs nothing.
//!
//! # Per-hop policy
//!
//! For the requested URL and for every redirect target, before any socket
//! is opened for that hop:
//!
//! 1. the scheme is in [`AcquisitionLimits::schemes`];
//! 2. an `https` to `http` redirect is refused unless
//!    [`AcquisitionLimits::downgrade`] is [`DowngradePolicy::Allow`];
//! 3. the effective port is not on the WHATWG Fetch "bad port" list;
//! 4. the host is resolved once and the full
//!    [`crate::SearchConstraints`] network-target policy runs on the
//!    resolved addresses, producing the hop's [`crate::ValidatedTarget`]
//!    (userinfo, blocked address ranges unless a
//!    [`crate::LocalTargetAuthorization`] is supplied, domain allow/deny).
//!
//! A redirect is followed only from a 301, 302, 303, 307, or 308 response
//! with exactly one `Location`; the target is resolved against the hop URL
//! (a malformed value fails the transfer), must carry no userinfo, and must
//! not exceed [`AcquisitionLimits::max_redirects`] or repeat a URL already
//! requested in the chain. Every check reads the parsed [`url::Url`], whose WHATWG
//! canonical form has already turned alternate IPv4 spellings (decimal,
//! hexadecimal, octal, percent-encoded) into an IP address, so no text
//! spelling reaches an authority decision. Local and private targets are
//! reachable only with a [`crate::LocalTargetAuthorization`] passed to
//! `acquire`; nothing in the URL or in response headers can grant it.
//!
//! # Where DNS and connection binding occur
//!
//! - **DNS**: exactly once per hop, through the configured
//!   [`crate::Resolver`] (default [`crate::SystemResolver`]) on a blocking
//!   thread (`tokio::task::spawn_blocking`). IP-literal hosts are not
//!   resolved. A resolver error of kind
//!   [`std::io::ErrorKind::PermissionDenied`] is an egress refusal
//!   ([`AcquisitionFailure::EgressDenied`]); any other error is
//!   [`AcquisitionFailure::ResolutionFailed`].
//! - **Connection binding**: the hop's [`Connector`] (default
//!   [`DirectConnector`]) is asked for one [`std::net::SocketAddr`] at a
//!   time, built from that hop's [`crate::ValidatedTarget::addrs`] and the
//!   URL's effective port. The connector never receives a host name. The
//!   URL host is still sent as `Host` and, for a domain, as TLS SNI and the
//!   certificate name.
//!
//! A consumer egress adapter implements [`Connector`] and enforces its
//! allow/deny in [`Connector::connect`]: that is the one checkpoint every
//! hop passes. It may also wrap the [`crate::Resolver`] to refuse before
//! lookup, but the resolver is never consulted for an IP-literal host, so a
//! resolver wrapper alone leaves IP-literal targets unenforced. Do not
//! assume an arbitrary proxy preserves binding: a proxy that resolves the
//! host name itself performs a second, unvalidated resolution. See
//! [`Connector`].
//!
//! # Time and cancellation
//!
//! [`AcquisitionLimits::deadline`] bounds the whole operation across every
//! hop; [`AcquisitionLimits::connect_timeout`] bounds each connection
//! attempt. Dropping the `acquire` future cancels everything: the hyper
//! connection is driven inline rather than spawned, so no task outlives the
//! call and the socket closes with the future. The one exception is a DNS
//! lookup already running on a blocking thread, which cannot be
//! interrupted; it runs to completion and its answer is discarded without
//! opening a socket.
//!
//! # Request shape
//!
//! `GET` over HTTP/1.1 with exactly `Host`, `User-Agent` (default
//! `zetesis/<version>`), `Accept: */*`, `Accept-Encoding: identity`, and
//! `Connection: close`. No cookies, credentials, `Referer`, request body,
//! connection pool, proxy environment variables, automatic redirects, or
//! automatic decompression. TLS offers only `http/1.1` through ALPN.
//!
//! # Results
//!
//! `acquire` returns `Err` only for invalid caller input that produced no
//! attempt (a URL longer than [`AcquisitionLimits::max_url_bytes`], an
//! unusable domain-list entry, a deadline that overflows the clock). A
//! requested URL carrying userinfo is refused as
//! [`AcquisitionFailure::UnsafeTarget`] before any lookup and recorded with
//! the userinfo removed, so a credential never enters hop evidence. Every policy refusal and transport
//! failure, including a refused requested URL, is a
//! [`TransferOutcome::Failed`] carrying the hop evidence gathered so far.
//!
//! This module emits no `tracing` events; the returned [`Transfer`] is the
//! record of what happened.

mod connector;
mod limits;
mod policy;
mod record;
mod tls;
mod transport;

use std::collections::HashSet;
use std::fmt;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

use hyper::StatusCode;
use hyper::header::{HeaderValue, LOCATION};
use snafu::ensure;
use tokio::time::Instant;
use tokio_rustls::TlsConnector;
use url::{Host, Url};

pub use self::connector::{
    ConnectDeniedSnafu, ConnectError, ConnectIoSnafu, ConnectTimedOutSnafu, ConnectedStream,
    Connector, DirectConnector,
};
pub use self::limits::{AcquisitionLimits, DowngradePolicy, SchemePolicy};
pub use self::record::{
    AcquisitionFailure, ConnectAttempt, ConnectOutcome, HopRecord, ResponseRecord, TlsRecord,
    Transfer, TransferOutcome,
};
pub use self::tls::TrustAnchors;
use self::transport::Exchange;
use crate::constraints::SearchConstraints;
use crate::error::{Error, InvalidConstraintSnafu, Result};
use crate::net_policy::{LocalTargetAuthorization, Resolver, SystemResolver, ValidatedTarget};

/// `User-Agent` sent when the builder is not given one.
const DEFAULT_USER_AGENT: &str = concat!("zetesis/", env!("CARGO_PKG_VERSION"));

/// The one concrete static acquirer. Build with [`StaticAcquirer::builder`].
///
/// Cheap to share: wrap it in an `Arc` and call [`StaticAcquirer::acquire`]
/// concurrently.
pub struct StaticAcquirer {
    limits: AcquisitionLimits,
    resolver: Arc<dyn Resolver + Send + Sync>,
    connector: Arc<dyn Connector>,
    user_agent: HeaderValue,
    tls: TlsConnector,
}

/// Builder for [`StaticAcquirer`].
pub struct StaticAcquirerBuilder {
    limits: AcquisitionLimits,
    trust: TrustAnchors,
    resolver: Arc<dyn Resolver + Send + Sync>,
    connector: Arc<dyn Connector>,
    user_agent: String,
}

impl StaticAcquirerBuilder {
    /// Resolve hosts through `resolver` instead of [`SystemResolver`].
    #[must_use]
    pub fn resolver(mut self, resolver: Arc<dyn Resolver + Send + Sync>) -> Self {
        self.resolver = resolver;
        self
    }

    /// Open connections through `connector` instead of [`DirectConnector`].
    #[must_use]
    pub fn connector(mut self, connector: Arc<dyn Connector>) -> Self {
        self.connector = connector;
        self
    }

    /// Send `user_agent` instead of `zetesis/<version>`.
    #[must_use]
    pub fn user_agent(mut self, user_agent: impl Into<String>) -> Self {
        self.user_agent = user_agent.into();
        self
    }

    /// Build the acquirer.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidConstraint`] when the user agent is not a valid
    /// header value or the TLS configuration is rejected.
    pub fn build(self) -> Result<StaticAcquirer> {
        let user_agent = HeaderValue::from_str(&self.user_agent).map_err(|source| {
            InvalidConstraintSnafu {
                field: "user_agent",
                reason: format!("not a valid header value: {source}"),
            }
            .build()
        })?;
        Ok(StaticAcquirer {
            limits: self.limits,
            resolver: self.resolver,
            connector: self.connector,
            user_agent,
            tls: self.trust.connector()?,
        })
    }
}

impl StaticAcquirer {
    /// Start building an acquirer that applies `limits` and trusts `trust`
    /// for `https` hops.
    #[must_use]
    pub fn builder(limits: AcquisitionLimits, trust: TrustAnchors) -> StaticAcquirerBuilder {
        StaticAcquirerBuilder {
            limits,
            trust,
            resolver: Arc::new(SystemResolver),
            connector: Arc::new(DirectConnector),
            user_agent: DEFAULT_USER_AGENT.to_owned(),
        }
    }

    /// The transfer profile this acquirer applies.
    #[must_use]
    pub const fn limits(&self) -> &AcquisitionLimits {
        &self.limits
    }

    /// Fetch `url` with an anonymous `GET`, validating and recording every
    /// hop (see the module documentation for the policy).
    ///
    /// `local` is the only way to admit a loopback, private, or otherwise
    /// blocked address; pass `None` unless the process holds that authority
    /// for this call.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidConstraint`] when `url` exceeds
    /// [`AcquisitionLimits::max_url_bytes`], a domain list in `constraints`
    /// has an unusable entry, or the deadline cannot be represented. No DNS
    /// lookup or network attempt is made in any of these cases. Every other
    /// failure is reported in the returned [`Transfer`].
    pub async fn acquire(
        &self,
        url: &Url,
        constraints: &SearchConstraints,
        local: Option<&LocalTargetAuthorization>,
    ) -> Result<Transfer> {
        ensure!(
            url.as_str().len() <= self.limits.max_url_bytes(),
            InvalidConstraintSnafu {
                field: "url",
                reason: format!(
                    "{} bytes, over max_url_bytes {}",
                    url.as_str().len(),
                    self.limits.max_url_bytes()
                ),
            }
        );
        constraints.validate_domain_rules()?;
        let deadline = Instant::now()
            .checked_add(self.limits.deadline())
            .ok_or_else(|| {
                InvalidConstraintSnafu {
                    field: "deadline",
                    reason: "overflows the clock",
                }
                .build()
            })?;

        if policy::has_userinfo(url) {
            // WHY: refused like any unsafe target, but recorded without the
            // userinfo so a credential never enters hop evidence.
            let recorded = policy::without_userinfo(url);
            let failure = AcquisitionFailure::UnsafeTarget {
                reason: "URL carries userinfo, which is never permitted".to_owned(),
            };
            let hops = vec![HopRecord::new(recorded.clone())];
            return Ok(Transfer::new(
                recorded,
                hops,
                TransferOutcome::Failed { failure },
            ));
        }

        let mut done = Vec::new();
        let mut current = HopRecord::new(url.clone());
        let hops = Hops {
            done: &mut done,
            current: &mut current,
        };
        let followed =
            tokio::time::timeout_at(deadline, self.follow(hops, constraints, local)).await;
        done.push(current);

        let outcome = match followed {
            Ok(Ok((response, body))) => TransferOutcome::Complete { response, body },
            Ok(Err(failure)) => TransferOutcome::Failed { failure },
            Err(_elapsed) => TransferOutcome::Failed {
                failure: AcquisitionFailure::DeadlineExceeded {
                    deadline_ms: self.limits.deadline_ms(),
                },
            },
        };
        Ok(Transfer::new(url.clone(), done, outcome))
    }

    /// Walk the redirect chain from `hops.current` until a final response
    /// is accepted or a check fails.
    async fn follow(
        &self,
        hops: Hops<'_>,
        constraints: &SearchConstraints,
        local: Option<&LocalTargetAuthorization>,
    ) -> std::result::Result<(ResponseRecord, Vec<u8>), AcquisitionFailure> {
        let Hops { done, current } = hops;
        let mut visited = HashSet::from([policy::chain_key(current.url())]);
        let mut previous: Option<Url> = None;
        let mut redirects: u32 = 0;
        loop {
            let url = current.url().clone();
            let port = policy::check_hop(&url, previous.as_ref(), &self.limits)?;
            let target = self.validate(&url, port, constraints, local).await?;
            current.set_resolved(target.addrs().to_vec());
            let exchange = self.fetch_hop(&target, port, current).await?;
            let Some(location) = redirect_location(&exchange)? else {
                return accept(exchange, &self.limits).await;
            };
            drop(exchange);
            current.set_location(policy::location_text(&location));
            let next = policy::redirect_target(&url, &location, &self.limits)?;
            if policy::has_userinfo(&next) {
                return Err(AcquisitionFailure::UnsafeTarget {
                    reason: "redirect target carries userinfo".to_owned(),
                });
            }
            if redirects >= self.limits.max_redirects() {
                return Err(AcquisitionFailure::RedirectLimit {
                    max_redirects: self.limits.max_redirects(),
                });
            }
            if !visited.insert(policy::chain_key(&next)) {
                return Err(AcquisitionFailure::RedirectLoop { url: next });
            }
            redirects += 1;
            previous = Some(url);
            done.push(std::mem::replace(current, HopRecord::new(next)));
        }
    }

    /// Resolve (once) and run the network-target policy for one hop.
    async fn validate(
        &self,
        url: &Url,
        port: u16,
        constraints: &SearchConstraints,
        local: Option<&LocalTargetAuthorization>,
    ) -> std::result::Result<ValidatedTarget, AcquisitionFailure> {
        let answer = match url.host() {
            Some(Host::Domain(name)) => Some(PinnedAnswer {
                host: name.to_owned(),
                addrs: self.resolve(name, port).await?,
            }),
            // NOTE: IP-literal hosts are classified directly; the policy
            // check never consults the resolver for them.
            Some(Host::Ipv4(_) | Host::Ipv6(_)) | None => None,
        };
        let host = answer.as_ref().map(|a| a.host.clone()).unwrap_or_default();
        let pinned = PinnedResolver(answer);
        constraints
            .check_url_with_policy(url, &pinned, local)
            .map_err(|source| policy_failure(source, host))
    }

    async fn resolve(
        &self,
        host: &str,
        port: u16,
    ) -> std::result::Result<Vec<IpAddr>, AcquisitionFailure> {
        let resolver = Arc::clone(&self.resolver);
        let name = host.to_owned();
        let failed = |reason: String| AcquisitionFailure::ResolutionFailed {
            host: host.to_owned(),
            reason,
        };
        match tokio::task::spawn_blocking(move || resolver.resolve(&name, port)).await {
            Ok(Ok(addrs)) if !addrs.is_empty() => Ok(addrs),
            Ok(Ok(_)) => Err(failed("resolver returned no addresses".to_owned())),
            // WHY: `PermissionDenied` is the `Resolver` contract's signal
            // for an egress policy refusing before lookup; it is a
            // permanent policy decision, not a retryable DNS failure.
            Ok(Err(source)) if source.kind() == std::io::ErrorKind::PermissionDenied => {
                Err(AcquisitionFailure::EgressDenied {
                    reason: format!("resolver refused {host}: {source}"),
                })
            }
            Ok(Err(source)) => Err(failed(source.to_string())),
            Err(source) => Err(failed(format!("resolver task failed: {source}"))),
        }
    }

    /// Connect to the validated target, run TLS for `https`, and send the
    /// request, recording each step on `hop`.
    async fn fetch_hop(
        &self,
        target: &ValidatedTarget,
        port: u16,
        hop: &mut HopRecord,
    ) -> std::result::Result<Exchange, AcquisitionFailure> {
        let url = target.url();
        let mut stream = self.connect(target, port, hop).await?;
        if url.scheme() == "https" {
            let (tls_stream, record) = tls::handshake(&self.tls, url, stream).await?;
            hop.set_tls(record);
            stream = tls_stream;
        }
        let exchange = transport::send_get(stream, url, &self.user_agent, &self.limits).await?;
        hop.set_status(exchange.status().as_u16());
        Ok(exchange)
    }

    /// Try each validated address in order until one connects.
    async fn connect(
        &self,
        target: &ValidatedTarget,
        port: u16,
        hop: &mut HopRecord,
    ) -> std::result::Result<Box<dyn ConnectedStream>, AcquisitionFailure> {
        let timeout = self.limits.connect_timeout();
        let mut failures = ConnectFailures::default();
        for &ip in target.addrs() {
            let addr = SocketAddr::new(ip, port);
            // NOTE: recorded before the attempt is awaited, so an attempt
            // cut off by the whole-operation deadline still appears (as
            // timed out) in the hop evidence.
            hop.push_attempt(ConnectAttempt::new(addr, ConnectOutcome::TimedOut));
            let attempt =
                tokio::time::timeout(timeout, self.connector.connect(addr, timeout)).await;
            let outcome = match attempt {
                Ok(Ok(stream)) => {
                    hop.settle_last_attempt(ConnectOutcome::Connected);
                    return Ok(stream);
                }
                Ok(Err(error)) => failures.record(error),
                Err(_elapsed) => failures.record_timeout(),
            };
            hop.settle_last_attempt(outcome);
        }
        Err(failures.summarize(self.limits.connect_timeout_ms()))
    }
}

impl fmt::Debug for StaticAcquirer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StaticAcquirer")
            .field("limits", &self.limits)
            .field("user_agent", &self.user_agent)
            .finish_non_exhaustive()
    }
}

impl fmt::Debug for StaticAcquirerBuilder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StaticAcquirerBuilder")
            .field("limits", &self.limits)
            .field("trust", &self.trust)
            .field("user_agent", &self.user_agent)
            .finish_non_exhaustive()
    }
}

/// Hop evidence written by the transfer future. It lives outside that
/// future so a deadline that drops the future keeps the hops recorded so
/// far, including the one in progress.
struct Hops<'a> {
    done: &'a mut Vec<HopRecord>,
    current: &'a mut HopRecord,
}

/// The single answer resolved for this hop, handed to the policy check so
/// the check and the connection use the same resolution.
struct PinnedAnswer {
    host: String,
    addrs: Vec<IpAddr>,
}

struct PinnedResolver(Option<PinnedAnswer>);

impl Resolver for PinnedResolver {
    fn resolve(&self, host: &str, _port: u16) -> std::io::Result<Vec<IpAddr>> {
        match &self.0 {
            Some(answer) if answer.host == host => Ok(answer.addrs.clone()),
            _ => Err(std::io::Error::other(format!(
                "{host} was not resolved for this hop"
            ))),
        }
    }
}

/// Map a network-target policy rejection onto the failure taxonomy.
fn policy_failure(source: Error, host: String) -> AcquisitionFailure {
    match source {
        Error::UnsafeTarget { reason, .. } => AcquisitionFailure::UnsafeTarget { reason },
        Error::TransientIo { message, .. } => AcquisitionFailure::ResolutionFailed {
            host,
            reason: message,
        },
        other => AcquisitionFailure::UnsafeTarget {
            reason: other.to_string(),
        },
    }
}

/// What the failed connection attempts of one hop add up to.
#[derive(Default)]
struct ConnectFailures {
    denied: Option<String>,
    timed_out: bool,
    failed: Option<String>,
}

impl ConnectFailures {
    fn record(&mut self, error: ConnectError) -> ConnectOutcome {
        match error {
            ConnectError::Denied { reason, .. } => {
                self.denied = Some(reason);
                ConnectOutcome::Denied
            }
            ConnectError::TimedOut { .. } => self.record_timeout(),
            ConnectError::Io { source, .. } => {
                let outcome = if source.kind() == std::io::ErrorKind::ConnectionRefused {
                    ConnectOutcome::Refused
                } else {
                    ConnectOutcome::Error
                };
                self.failed = Some(source.to_string());
                outcome
            }
        }
    }

    fn record_timeout(&mut self) -> ConnectOutcome {
        self.timed_out = true;
        ConnectOutcome::TimedOut
    }

    /// A socket failure outranks a timeout, which outranks a denial: the
    /// failure kind is `egress_denied` only when every address was denied.
    fn summarize(self, timeout_ms: u64) -> AcquisitionFailure {
        match (self.failed, self.timed_out, self.denied) {
            (Some(reason), _, _) => AcquisitionFailure::ConnectFailed { reason },
            (None, true, _) => AcquisitionFailure::ConnectTimeout { timeout_ms },
            (None, false, Some(reason)) => AcquisitionFailure::EgressDenied { reason },
            (None, false, None) => AcquisitionFailure::ConnectFailed {
                reason: "no validated address to connect to".to_owned(),
            },
        }
    }
}

/// The `Location` to follow, if this response is a redirect. A redirect
/// status without `Location` is a final response.
fn redirect_location(
    exchange: &Exchange,
) -> std::result::Result<Option<HeaderValue>, AcquisitionFailure> {
    let redirect = matches!(
        exchange.status(),
        StatusCode::MOVED_PERMANENTLY
            | StatusCode::FOUND
            | StatusCode::SEE_OTHER
            | StatusCode::TEMPORARY_REDIRECT
            | StatusCode::PERMANENT_REDIRECT
    );
    if !redirect {
        return Ok(None);
    }
    let mut values = exchange.headers().get_all(LOCATION).iter();
    let Some(first) = values.next() else {
        return Ok(None);
    };
    if values.next().is_some() {
        return Err(AcquisitionFailure::MalformedRedirect {
            location: policy::location_text(first),
            reason: "response carries more than one Location".to_owned(),
        });
    }
    Ok(Some(first.clone()))
}

/// Accept the final response: record its head, refuse content codings this
/// acquirer does not decode, and read the bounded body.
async fn accept(
    exchange: Exchange,
    limits: &AcquisitionLimits,
) -> std::result::Result<(ResponseRecord, Vec<u8>), AcquisitionFailure> {
    let response = ResponseRecord::from_head(exchange.status().as_u16(), exchange.headers());
    if let Some(coding) = response
        .content_encoding()
        .iter()
        .find(|coding| coding.as_str() != "identity")
    {
        return Err(AcquisitionFailure::UnsupportedContentEncoding {
            coding: coding.clone(),
        });
    }
    let body = exchange.read_body(limits.max_body_bytes()).await?;
    Ok((response, body))
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    use super::*;

    /// A trust anchor is required to build an acquirer; these tests only
    /// use `http`, so any parseable certificate will do.
    fn throwaway_anchor() -> TrustAnchors {
        let minted = rcgen::generate_simple_self_signed(vec!["unused.example".to_owned()]).unwrap();
        TrustAnchors::from_der([minted.cert.der().as_ref()]).unwrap()
    }

    fn acquirer() -> StaticAcquirer {
        let limits = AcquisitionLimits::new(
            SchemePolicy::HttpAndHttps,
            0,
            Duration::from_secs(5),
            Duration::from_secs(30),
            1024,
        )
        .unwrap();
        StaticAcquirer::builder(limits, throwaway_anchor())
            .build()
            .unwrap()
    }

    /// Bind a loopback listener for a local origin.
    async fn loopback_origin() -> (SocketAddr, TcpListener) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        (listener.local_addr().unwrap(), listener)
    }

    /// Accept one connection, read its request head, and answer `200 OK`.
    async fn answer_once(listener: &TcpListener) {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut head = Vec::new();
        let mut byte = [0_u8; 1];
        while !head.ends_with(b"\r\n\r\n") && stream.read(&mut byte).await.unwrap() > 0 {
            head.push(byte[0]);
        }
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nlocal")
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn loopback_target_requires_local_authorization() {
        // WHY: the capability is the only path to a loopback target; the
        // same URL, constraints, and connector are refused without it and
        // served with it. Only crate-internal tests can mint it.
        let (local, listener) = loopback_origin().await;
        let url = Url::parse(&format!("http://{local}/")).unwrap();
        let acquirer = acquirer();
        let constraints = SearchConstraints::default();

        let refused = acquirer.acquire(&url, &constraints, None).await.unwrap();
        assert!(
            matches!(
                refused.failure(),
                Some(AcquisitionFailure::UnsafeTarget { .. })
            ),
            "loopback without authority must be refused: {:?}",
            refused.outcome()
        );
        assert!(
            refused.hops()[0].connect_attempts().is_empty(),
            "no connection was attempted without authority"
        );
        assert!(
            futures::FutureExt::now_or_never(listener.accept()).is_none(),
            "the loopback listener saw no connection"
        );

        let authority = LocalTargetAuthorization::for_crate_tests();
        let (served, ()) = tokio::join!(
            acquirer.acquire(&url, &constraints, Some(&authority)),
            answer_once(&listener),
        );
        let served = served.unwrap();
        assert_eq!(
            served.body(),
            Some(b"local".as_slice()),
            "the explicit authority admits the loopback target"
        );
    }
}
