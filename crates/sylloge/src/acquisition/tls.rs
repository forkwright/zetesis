//! TLS client configuration and the per-hop handshake.
//!
//! The `ring` crypto provider is set on each [`ClientConfig`]; nothing is
//! installed process-wide. ALPN offers only `http/1.1`, and session
//! resumption is disabled so separate acquisitions cannot be linked by a
//! resumed session.

use std::fmt;
use std::sync::Arc;

use rustls::client::Resumption;
use rustls::pki_types::{CertificateDer, ServerName};
use rustls::{ClientConfig, ProtocolVersion, RootCertStore};
use snafu::ensure;
use tokio_rustls::TlsConnector;
use url::{Host, Url};

use super::connector::ConnectedStream;
use super::record::{AcquisitionFailure, TlsRecord};
use crate::error::{InvalidConstraintSnafu, PermanentIoSnafu, Result};

/// Root certificates an acquirer trusts for `https` hops.
#[derive(Clone)]
pub struct TrustAnchors {
    roots: Arc<RootCertStore>,
}

impl TrustAnchors {
    /// Load the operating system's trust store through
    /// `rustls-native-certs` (which honors `SSL_CERT_FILE` and
    /// `SSL_CERT_DIR` when set). Entries that fail to parse are skipped, as
    /// long as at least one usable root remains.
    ///
    /// WARNING: reads certificate files synchronously. Async callers build
    /// the anchors once at startup or through `spawn_blocking`.
    ///
    /// # Errors
    ///
    /// Returns [`crate::Error::PermanentIo`] when the store yields no
    /// usable root certificate.
    pub fn native() -> Result<Self> {
        let loaded = rustls_native_certs::load_native_certs();
        let mut roots = RootCertStore::empty();
        let (added, _skipped) = roots.add_parsable_certificates(loaded.certs);
        ensure!(
            added > 0,
            PermanentIoSnafu {
                message: format!(
                    "no usable root certificate in the native trust store ({} load errors)",
                    loaded.errors.len()
                ),
            }
        );
        Ok(Self {
            roots: Arc::new(roots),
        })
    }

    /// Trust exactly the given DER-encoded certificates.
    ///
    /// # Errors
    ///
    /// Returns [`crate::Error::InvalidConstraint`] when `certificates` is empty or
    /// any entry is not a usable trust anchor.
    pub fn from_der<I, B>(certificates: I) -> Result<Self>
    where
        I: IntoIterator<Item = B>,
        B: AsRef<[u8]>,
    {
        let mut roots = RootCertStore::empty();
        for der in certificates {
            roots
                .add(CertificateDer::from(der.as_ref().to_vec()))
                .map_err(|source| {
                    InvalidConstraintSnafu {
                        field: "trust_anchors",
                        reason: format!("certificate rejected: {source}"),
                    }
                    .build()
                })?;
        }
        ensure!(
            !roots.is_empty(),
            InvalidConstraintSnafu {
                field: "trust_anchors",
                reason: "no certificate supplied",
            }
        );
        Ok(Self {
            roots: Arc::new(roots),
        })
    }

    /// Number of trusted roots.
    #[must_use]
    pub fn len(&self) -> usize {
        self.roots.len()
    }

    /// Whether no root is trusted. Always `false` for a constructed value.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.roots.is_empty()
    }

    pub(crate) fn connector(&self) -> Result<TlsConnector> {
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let mut config = ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .map_err(|source| {
                InvalidConstraintSnafu {
                    field: "trust_anchors",
                    reason: format!("TLS configuration rejected: {source}"),
                }
                .build()
            })?
            .with_root_certificates(Arc::clone(&self.roots))
            .with_no_client_auth();
        config.alpn_protocols = vec![b"http/1.1".to_vec()];
        config.resumption = Resumption::disabled();
        Ok(TlsConnector::from(Arc::new(config)))
    }
}

impl fmt::Debug for TrustAnchors {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TrustAnchors")
            .field("roots", &self.roots.len())
            .finish()
    }
}

/// Run the TLS handshake for `url` over `stream`, verifying the peer
/// against the URL host and sending that host as SNI (for a domain name).
pub(crate) async fn handshake(
    connector: &TlsConnector,
    url: &Url,
    stream: Box<dyn ConnectedStream>,
) -> std::result::Result<(Box<dyn ConnectedStream>, TlsRecord), AcquisitionFailure> {
    let (server_name, name_text) = server_name(url)?;
    let tls = connector
        .connect(server_name, stream)
        .await
        .map_err(|source| handshake_failure(&source))?;
    let (_, session) = tls.get_ref();
    let protocol_version = match session.protocol_version() {
        Some(ProtocolVersion::TLSv1_3) => "TLSv1.3".to_owned(),
        Some(ProtocolVersion::TLSv1_2) => "TLSv1.2".to_owned(),
        Some(other) => format!("{other:?}"),
        None => "unknown".to_owned(),
    };
    let peer_leaf_sha256 = session
        .peer_certificates()
        .and_then(<[CertificateDer<'_>]>::first)
        .map(|leaf| hex_sha256(leaf.as_ref()))
        .unwrap_or_default();
    let record = TlsRecord::new(protocol_version, name_text, peer_leaf_sha256);
    let stream: Box<dyn ConnectedStream> = Box::new(tls);
    Ok((stream, record))
}

fn server_name(
    url: &Url,
) -> std::result::Result<(ServerName<'static>, String), AcquisitionFailure> {
    match url.host() {
        Some(Host::Domain(domain)) => ServerName::try_from(domain.to_owned())
            .map(|name| (name, domain.to_owned()))
            .map_err(|source| AcquisitionFailure::TlsFailed {
                reason: format!("host is not a valid TLS server name: {source}"),
            }),
        Some(Host::Ipv4(ip)) => Ok((ServerName::IpAddress(ip.into()), ip.to_string())),
        Some(Host::Ipv6(ip)) => Ok((ServerName::IpAddress(ip.into()), ip.to_string())),
        None => Err(AcquisitionFailure::TlsFailed {
            reason: "URL has no host".to_owned(),
        }),
    }
}

fn handshake_failure(source: &std::io::Error) -> AcquisitionFailure {
    use std::io::ErrorKind;

    let is_tls_error = source
        .get_ref()
        .is_some_and(|inner| inner.downcast_ref::<rustls::Error>().is_some());
    match source.kind() {
        ErrorKind::ConnectionReset
        | ErrorKind::ConnectionAborted
        | ErrorKind::BrokenPipe
        | ErrorKind::UnexpectedEof
            if !is_tls_error =>
        {
            AcquisitionFailure::InterruptedStream {
                reason: format!("connection closed during TLS handshake: {source}"),
            }
        }
        _ => AcquisitionFailure::TlsFailed {
            reason: source.to_string(),
        },
    }
}

/// Lowercase hex SHA-256 of `bytes`.
pub(crate) fn hex_sha256(bytes: &[u8]) -> String {
    use std::fmt::Write as _;

    let digest = ring::digest::digest(&ring::digest::SHA256, bytes);
    digest
        .as_ref()
        .iter()
        .fold(String::with_capacity(64), |mut out, byte| {
            // NOTE: writing to a String cannot fail.
            let _ = write!(out, "{byte:02x}");
            out
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_der_rejects_empty_and_garbage() {
        let empty: [&[u8]; 0] = [];
        assert!(
            TrustAnchors::from_der(empty).is_err(),
            "an empty anchor set trusts nothing and is refused"
        );
        assert!(
            TrustAnchors::from_der([b"not a certificate".as_slice()]).is_err(),
            "unparseable DER must be refused"
        );
    }

    #[test]
    fn hex_sha256_matches_known_vector() {
        assert_eq!(
            hex_sha256(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
            "FIPS 180-2 test vector for SHA-256(\"abc\")"
        );
    }

    #[test]
    fn server_name_preserves_url_host() {
        let (name, text) =
            server_name(&Url::parse("https://Public.Example:8443/").unwrap()).unwrap();
        assert_eq!(text, "public.example", "SNI uses the canonical URL host");
        assert!(
            matches!(name, ServerName::DnsName(_)),
            "a domain host verifies as a DNS name"
        );
        let (name, text) =
            server_name(&Url::parse("https://[2001:4860:4860::8888]/").unwrap()).unwrap();
        assert_eq!(
            text, "2001:4860:4860::8888",
            "an IPv6 host verifies as an address"
        );
        assert!(
            matches!(name, ServerName::IpAddress(_)),
            "an IP literal is never sent as SNI"
        );
    }
}
