//! Evidence records produced by [`super::StaticAcquirer::acquire`].
//!
//! Every record is serializable with `snake_case` field names so the
//! [`crate::EvidenceEnvelope`] embeds it unchanged.

use std::fmt;
use std::net::{IpAddr, SocketAddr};

use serde::{Deserialize, Serialize};
use url::Url;

use crate::error::ErrorClass;

/// Why an acquisition stopped without an accepted response.
///
/// Serialized with a `kind` tag equal to [`AcquisitionFailure::kind`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AcquisitionFailure {
    /// The target failed the [`crate::SearchConstraints`] network-target
    /// policy: userinfo, a resolved address in a blocked range without
    /// [`crate::LocalTargetAuthorization`], or a domain allow/deny
    /// mismatch.
    UnsafeTarget {
        /// Which check failed and why.
        reason: String,
    },
    /// The URL's scheme is outside [`super::SchemePolicy`].
    SchemeNotAllowed {
        /// The refused scheme.
        scheme: String,
    },
    /// A redirect from `https` to `http` under
    /// [`super::DowngradePolicy::Refuse`].
    DowngradeRefused,
    /// The effective port is on the WHATWG Fetch bad-port list.
    DeniedPort {
        /// The refused port.
        port: u16,
    },
    /// Another redirect would exceed
    /// [`super::AcquisitionLimits::max_redirects`].
    RedirectLimit {
        /// The limit that was reached.
        max_redirects: u32,
    },
    /// A redirect targets a URL already requested in this chain.
    RedirectLoop {
        /// The repeated target.
        url: Url,
    },
    /// A redirect `Location` could not be turned into a URL within limits.
    MalformedRedirect {
        /// The `Location` value as received (lossy UTF-8).
        location: String,
        /// Why it was rejected.
        reason: String,
    },
    /// The resolver failed or returned no addresses.
    ResolutionFailed {
        /// The host being resolved.
        host: String,
        /// Resolver detail.
        reason: String,
    },
    /// An egress policy refused the hop: the [`crate::Resolver`] refused the
    /// host before lookup (an error of kind
    /// [`std::io::ErrorKind::PermissionDenied`]), or the
    /// [`super::Connector`] refused every validated address.
    EgressDenied {
        /// The refusal detail (the last one, when several addresses were
        /// refused).
        reason: String,
    },
    /// No validated address accepted a connection.
    ConnectFailed {
        /// Detail of the last failure.
        reason: String,
    },
    /// Every non-denied connection attempt exceeded
    /// [`super::AcquisitionLimits::connect_timeout`].
    ConnectTimeout {
        /// The per-attempt timeout, in milliseconds.
        timeout_ms: u64,
    },
    /// The TLS handshake failed (certificate verification, protocol
    /// mismatch).
    TlsFailed {
        /// rustls detail.
        reason: String,
    },
    /// [`super::AcquisitionLimits::deadline`] elapsed before the operation
    /// finished.
    DeadlineExceeded {
        /// The whole-operation deadline, in milliseconds.
        deadline_ms: u64,
    },
    /// The peer's response violated HTTP/1.1.
    HttpProtocol {
        /// Parser detail.
        reason: String,
    },
    /// The response header section exceeded
    /// [`super::AcquisitionLimits::max_header_bytes`].
    HeaderLimit {
        /// The limit that was exceeded.
        max_header_bytes: usize,
    },
    /// The response used a content coding other than `gzip`, `x-gzip`,
    /// or `deflate`, or stacked more than one coding. Refused before the
    /// body is read.
    UnsupportedContentEncoding {
        /// The refused coding, or the stacked codings joined by `, `.
        coding: String,
    },
    /// The response's media type is not one the acquirer extracts
    /// (`text/html`, `text/plain`), or no `Content-Type` was sent for a
    /// non-empty body. Refused before the body is read.
    UnsupportedContentType {
        /// The media type essence as received, lowercase; `None` when the
        /// header was absent or unparseable.
        media_type: Option<String>,
    },
    /// The response body exceeded
    /// [`super::AcquisitionLimits::max_body_bytes`] on the wire; reading
    /// stopped at the limit.
    WireLimit {
        /// The limit that was exceeded.
        max_body_bytes: u64,
    },
    /// Removing the content coding would exceed
    /// [`super::AcquisitionLimits::max_decoded_bytes`]; decoding stopped at
    /// the limit.
    DecodedLimit {
        /// The limit that was exceeded.
        max_decoded_bytes: u64,
    },
    /// The coded body is corrupt or ends before its trailer.
    CorruptContentEncoding {
        /// Decoder detail.
        reason: String,
    },
    /// The connection closed or failed mid-exchange.
    InterruptedStream {
        /// Transport detail.
        reason: String,
    },
}

impl AcquisitionFailure {
    /// Stable `snake_case` identifier, equal to the serialized `kind` tag.
    #[must_use]
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::UnsafeTarget { .. } => "unsafe_target",
            Self::SchemeNotAllowed { .. } => "scheme_not_allowed",
            Self::DowngradeRefused => "downgrade_refused",
            Self::DeniedPort { .. } => "denied_port",
            Self::RedirectLimit { .. } => "redirect_limit",
            Self::RedirectLoop { .. } => "redirect_loop",
            Self::MalformedRedirect { .. } => "malformed_redirect",
            Self::ResolutionFailed { .. } => "resolution_failed",
            Self::EgressDenied { .. } => "egress_denied",
            Self::ConnectFailed { .. } => "connect_failed",
            Self::ConnectTimeout { .. } => "connect_timeout",
            Self::TlsFailed { .. } => "tls_failed",
            Self::DeadlineExceeded { .. } => "deadline_exceeded",
            Self::HttpProtocol { .. } => "http_protocol",
            Self::HeaderLimit { .. } => "header_limit",
            Self::UnsupportedContentEncoding { .. } => "unsupported_content_encoding",
            Self::UnsupportedContentType { .. } => "unsupported_content_type",
            Self::WireLimit { .. } => "wire_limit",
            Self::DecodedLimit { .. } => "decoded_limit",
            Self::CorruptContentEncoding { .. } => "corrupt_content_encoding",
            Self::InterruptedStream { .. } => "interrupted_stream",
        }
    }

    /// Retry classification, on the same scale as [`crate::Error::class`].
    ///
    /// Policy refusals and origin behavior that repeats on every attempt
    /// are [`ErrorClass::Permanent`]; resolution, connection, deadline, and
    /// mid-stream failures are [`ErrorClass::Transient`].
    #[must_use]
    pub const fn class(&self) -> ErrorClass {
        match self {
            Self::ResolutionFailed { .. }
            | Self::ConnectFailed { .. }
            | Self::ConnectTimeout { .. }
            | Self::DeadlineExceeded { .. }
            | Self::InterruptedStream { .. } => ErrorClass::Transient,
            Self::UnsafeTarget { .. }
            | Self::SchemeNotAllowed { .. }
            | Self::DowngradeRefused
            | Self::DeniedPort { .. }
            | Self::RedirectLimit { .. }
            | Self::RedirectLoop { .. }
            | Self::MalformedRedirect { .. }
            | Self::EgressDenied { .. }
            | Self::TlsFailed { .. }
            | Self::HttpProtocol { .. }
            | Self::HeaderLimit { .. }
            | Self::UnsupportedContentEncoding { .. }
            | Self::UnsupportedContentType { .. }
            | Self::WireLimit { .. }
            | Self::DecodedLimit { .. }
            | Self::CorruptContentEncoding { .. } => ErrorClass::Permanent,
        }
    }
}

impl fmt::Display for AcquisitionFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let kind = self.kind();
        match self {
            Self::UnsafeTarget { reason }
            | Self::EgressDenied { reason }
            | Self::ConnectFailed { reason }
            | Self::TlsFailed { reason }
            | Self::HttpProtocol { reason }
            | Self::CorruptContentEncoding { reason }
            | Self::InterruptedStream { reason } => write!(f, "{kind}: {reason}"),
            Self::SchemeNotAllowed { scheme } => write!(f, "{kind}: {scheme}"),
            Self::DowngradeRefused => f.write_str(kind),
            Self::DeniedPort { port } => write!(f, "{kind}: {port}"),
            Self::RedirectLimit { max_redirects } => write!(f, "{kind}: {max_redirects}"),
            Self::RedirectLoop { url } => write!(f, "{kind}: {url}"),
            Self::MalformedRedirect { location, reason } => {
                write!(f, "{kind}: {location:?}: {reason}")
            }
            Self::ResolutionFailed { host, reason } => write!(f, "{kind}: {host}: {reason}"),
            Self::ConnectTimeout { timeout_ms } => write!(f, "{kind}: {timeout_ms} ms"),
            Self::DeadlineExceeded { deadline_ms } => write!(f, "{kind}: {deadline_ms} ms"),
            Self::HeaderLimit { max_header_bytes } => write!(f, "{kind}: {max_header_bytes}"),
            Self::UnsupportedContentEncoding { coding } => write!(f, "{kind}: {coding}"),
            Self::UnsupportedContentType { media_type } => match media_type {
                Some(media_type) => write!(f, "{kind}: {media_type}"),
                None => write!(f, "{kind}: no media type"),
            },
            Self::WireLimit { max_body_bytes } => write!(f, "{kind}: {max_body_bytes}"),
            Self::DecodedLimit { max_decoded_bytes } => write!(f, "{kind}: {max_decoded_bytes}"),
        }
    }
}

/// Result of one connection attempt to one validated address.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[non_exhaustive]
#[serde(rename_all = "snake_case")]
pub enum ConnectOutcome {
    /// A stream was established.
    Connected,
    /// The [`super::Connector`] refused the address by policy; no socket
    /// was opened.
    Denied,
    /// The peer refused the connection.
    Refused,
    /// The attempt did not complete in time (per-attempt timeout or the
    /// whole-operation deadline).
    TimedOut,
    /// Any other connection failure.
    Error,
}

/// One connection attempt within a hop.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConnectAttempt {
    addr: SocketAddr,
    result: ConnectOutcome,
}

impl ConnectAttempt {
    pub(crate) const fn new(addr: SocketAddr, result: ConnectOutcome) -> Self {
        Self { addr, result }
    }

    pub(crate) fn set_result(&mut self, result: ConnectOutcome) {
        self.result = result;
    }

    /// The address the connector was asked to connect to: one of the hop's
    /// validated addresses and the URL's effective port.
    #[must_use]
    pub const fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// How the attempt ended.
    #[must_use]
    pub const fn result(&self) -> ConnectOutcome {
        self.result
    }
}

/// TLS session facts for an `https` hop.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TlsRecord {
    protocol_version: String,
    server_name: String,
    peer_leaf_sha256: String,
}

impl TlsRecord {
    pub(crate) const fn new(
        protocol_version: String,
        server_name: String,
        peer_leaf_sha256: String,
    ) -> Self {
        Self {
            protocol_version,
            server_name,
            peer_leaf_sha256,
        }
    }

    /// Negotiated protocol version (`TLSv1.2` or `TLSv1.3`).
    #[must_use]
    pub fn protocol_version(&self) -> &str {
        &self.protocol_version
    }

    /// The name the certificate was verified against: the URL host, also
    /// sent as SNI when it is a domain name.
    #[must_use]
    pub fn server_name(&self) -> &str {
        &self.server_name
    }

    /// Lowercase hex SHA-256 of the peer's leaf certificate (DER).
    #[must_use]
    pub fn peer_leaf_sha256(&self) -> &str {
        &self.peer_leaf_sha256
    }
}

/// Evidence for one request target in a redirect chain.
///
/// A hop is recorded as soon as its URL is under consideration, so a
/// target refused before any socket appears with empty `resolved` and
/// `connect_attempts`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HopRecord {
    url: Url,
    resolved: Vec<IpAddr>,
    connect_attempts: Vec<ConnectAttempt>,
    tls: Option<TlsRecord>,
    status: Option<u16>,
    location: Option<String>,
}

impl HopRecord {
    pub(crate) const fn new(url: Url) -> Self {
        Self {
            url,
            resolved: Vec::new(),
            connect_attempts: Vec::new(),
            tls: None,
            status: None,
            location: None,
        }
    }

    pub(crate) fn set_resolved(&mut self, resolved: Vec<IpAddr>) {
        self.resolved = resolved;
    }

    pub(crate) fn push_attempt(&mut self, attempt: ConnectAttempt) {
        self.connect_attempts.push(attempt);
    }

    /// Replace the result of the most recent attempt (recorded before the
    /// connector was awaited).
    pub(crate) fn settle_last_attempt(&mut self, result: ConnectOutcome) {
        if let Some(attempt) = self.connect_attempts.last_mut() {
            attempt.set_result(result);
        }
    }

    pub(crate) fn set_tls(&mut self, tls: TlsRecord) {
        self.tls = Some(tls);
    }

    pub(crate) fn set_status(&mut self, status: u16) {
        self.status = Some(status);
    }

    pub(crate) fn set_location(&mut self, location: String) {
        self.location = Some(location);
    }

    /// The request target of this hop.
    #[must_use]
    pub const fn url(&self) -> &Url {
        &self.url
    }

    /// The validated address set this hop was permitted to connect to.
    /// Empty when the hop was refused before or during validation.
    #[must_use]
    pub fn resolved(&self) -> &[IpAddr] {
        &self.resolved
    }

    /// Every connection attempt, in order. Each address is one of
    /// [`HopRecord::resolved`].
    #[must_use]
    pub fn connect_attempts(&self) -> &[ConnectAttempt] {
        &self.connect_attempts
    }

    /// TLS facts, for an `https` hop whose handshake completed.
    #[must_use]
    pub const fn tls(&self) -> Option<&TlsRecord> {
        self.tls.as_ref()
    }

    /// Response status, when a response head was received.
    #[must_use]
    pub const fn status(&self) -> Option<u16> {
        self.status
    }

    /// Raw `Location` value (lossy UTF-8) of a redirect response.
    #[must_use]
    pub fn location(&self) -> Option<&str> {
        self.location.as_deref()
    }
}

/// Selected headers of the accepted final response. `Set-Cookie` and every
/// other header are never recorded.
///
/// Header values are recorded as lossy UTF-8; when a header repeats, the
/// first value is kept, except `Content-Encoding`, whose codings from every
/// instance are listed in order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResponseRecord {
    status: u16,
    content_type: Option<String>,
    content_encoding: Vec<String>,
    content_length: Option<u64>,
    last_modified: Option<String>,
    etag: Option<String>,
    date: Option<String>,
    retry_after: Option<String>,
}

impl ResponseRecord {
    pub(crate) fn from_head(status: u16, headers: &hyper::HeaderMap) -> Self {
        use hyper::header::{CONTENT_LENGTH, CONTENT_TYPE, DATE, ETAG, LAST_MODIFIED, RETRY_AFTER};

        let text = |name| {
            headers.get(name).map(|value: &hyper::header::HeaderValue| {
                String::from_utf8_lossy(value.as_bytes()).into_owned()
            })
        };
        Self {
            status,
            content_type: text(CONTENT_TYPE),
            content_encoding: content_codings(headers),
            content_length: headers
                .get(CONTENT_LENGTH)
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.trim().parse().ok()),
            last_modified: text(LAST_MODIFIED),
            etag: text(ETAG),
            date: text(DATE),
            retry_after: text(RETRY_AFTER),
        }
    }

    /// HTTP status code.
    #[must_use]
    pub const fn status(&self) -> u16 {
        self.status
    }

    /// `Content-Type`, if present.
    #[must_use]
    pub fn content_type(&self) -> Option<&str> {
        self.content_type.as_deref()
    }

    /// `Content-Encoding` codings, lowercase, in order. Empty when absent.
    #[must_use]
    pub fn content_encoding(&self) -> &[String] {
        &self.content_encoding
    }

    /// `Content-Length`, if present and numeric.
    #[must_use]
    pub const fn content_length(&self) -> Option<u64> {
        self.content_length
    }

    /// `Last-Modified`, if present.
    #[must_use]
    pub fn last_modified(&self) -> Option<&str> {
        self.last_modified.as_deref()
    }

    /// `ETag`, if present.
    #[must_use]
    pub fn etag(&self) -> Option<&str> {
        self.etag.as_deref()
    }

    /// `Date`, if present.
    #[must_use]
    pub fn date(&self) -> Option<&str> {
        self.date.as_deref()
    }

    /// `Retry-After`, if present, as received. Recorded for any status;
    /// it is meaningful on a 429, a 503, or a redirect.
    #[must_use]
    pub fn retry_after(&self) -> Option<&str> {
        self.retry_after.as_deref()
    }
}

/// Every coding listed across all `Content-Encoding` headers, lowercase.
pub(crate) fn content_codings(headers: &hyper::HeaderMap) -> Vec<String> {
    headers
        .get_all(hyper::header::CONTENT_ENCODING)
        .iter()
        .flat_map(|value| {
            String::from_utf8_lossy(value.as_bytes())
                .split(',')
                .map(|coding| coding.trim().to_ascii_lowercase())
                .filter(|coding| !coding.is_empty())
                .collect::<Vec<_>>()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failure_kind_matches_serde_tag() {
        let failures = [
            AcquisitionFailure::DowngradeRefused,
            AcquisitionFailure::DeniedPort { port: 25 },
            AcquisitionFailure::WireLimit { max_body_bytes: 1 },
            AcquisitionFailure::UnsupportedContentEncoding {
                coding: "br".to_owned(),
            },
            AcquisitionFailure::UnsupportedContentType {
                media_type: Some("application/pdf".to_owned()),
            },
            AcquisitionFailure::UnsupportedContentType { media_type: None },
            AcquisitionFailure::DecodedLimit {
                max_decoded_bytes: 1,
            },
            AcquisitionFailure::CorruptContentEncoding {
                reason: "invalid gzip header".to_owned(),
            },
        ];
        for failure in failures {
            let json = serde_json::to_value(&failure).unwrap();
            assert_eq!(
                json["kind"],
                failure.kind(),
                "serde tag and kind() must agree for {failure:?}"
            );
            let back: AcquisitionFailure = serde_json::from_value(json).unwrap();
            assert_eq!(back, failure, "failure must round-trip");
        }
    }

    #[test]
    fn failure_classes_split_policy_from_transport() {
        assert_eq!(
            AcquisitionFailure::DowngradeRefused.class(),
            ErrorClass::Permanent,
            "a policy refusal never clears on retry"
        );
        assert_eq!(
            AcquisitionFailure::ConnectTimeout { timeout_ms: 1 }.class(),
            ErrorClass::Transient,
            "a connect timeout may clear on retry"
        );
    }

    #[test]
    fn hop_record_serializes_snake_case_fields() {
        let mut hop = HopRecord::new(Url::parse("https://8.8.8.8/").unwrap());
        hop.set_resolved(vec!["8.8.8.8".parse().unwrap()]);
        hop.push_attempt(ConnectAttempt::new(
            "8.8.8.8:443".parse().unwrap(),
            ConnectOutcome::Connected,
        ));
        hop.set_status(302);
        hop.set_location("/next".to_owned());
        let json = serde_json::to_value(&hop).unwrap();
        assert_eq!(
            json["connect_attempts"][0]["result"], "connected",
            "attempt results are snake_case"
        );
        assert_eq!(
            json["resolved"][0], "8.8.8.8",
            "addresses serialize as text"
        );
        let back: HopRecord = serde_json::from_value(json).unwrap();
        assert_eq!(back, hop, "hop must round-trip");
    }

    #[test]
    fn content_codings_split_and_lowercase_every_instance() {
        let mut headers = hyper::HeaderMap::new();
        headers.append(
            hyper::header::CONTENT_ENCODING,
            hyper::header::HeaderValue::from_static("GZIP, identity"),
        );
        headers.append(
            hyper::header::CONTENT_ENCODING,
            hyper::header::HeaderValue::from_static("br"),
        );
        assert_eq!(
            content_codings(&headers),
            ["gzip", "identity", "br"],
            "codings from every header instance, in order"
        );
    }
}
