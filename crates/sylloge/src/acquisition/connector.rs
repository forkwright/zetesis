//! The [`Connector`] seam: where connection binding occurs.
//!
//! [`super::StaticAcquirer`] asks a connector for one stream to one
//! [`SocketAddr`] at a time, and only for addresses taken from the current
//! hop's [`crate::ValidatedTarget::addrs`] plus the URL's effective port.
//! The connector never sees a host name, so it has nothing to re-resolve.
//!
//! A consumer egress adapter (an address allowlist, an egress router)
//! implements [`Connector`]: it refuses an address with
//! [`ConnectError::Denied`] or opens the stream through its own path.
//! `connect` is the one checkpoint every hop passes, so the adapter's
//! allow/deny decision must be enforced there. A [`crate::Resolver`]
//! wrapper that refuses before lookup can add an earlier deny-before-DNS
//! refusal, but it is never consulted for an IP-literal host, so it cannot
//! be the only enforcement point.

use std::net::SocketAddr;
use std::time::Duration;

use snafu::{ResultExt, Snafu};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpStream;

use crate::provider::BoxFut;

/// A connected byte stream the acquirer can run TLS and HTTP/1.1 over.
///
/// Blanket-implemented for every `AsyncRead + AsyncWrite + Send + Unpin`
/// type, including `tokio::net::TcpStream`, `tokio::io::DuplexStream`, and
/// `Box<dyn ConnectedStream>`.
pub trait ConnectedStream: AsyncRead + AsyncWrite + Send + Unpin {}

impl<T> ConnectedStream for T where T: AsyncRead + AsyncWrite + Send + Unpin {}

/// Why a [`Connector`] produced no stream.
#[derive(Debug, Snafu)]
#[snafu(visibility(pub))]
#[non_exhaustive]
pub enum ConnectError {
    /// The connector's egress policy refused this address. No socket was
    /// opened.
    #[snafu(context(name(ConnectDeniedSnafu)), display("egress denied: {reason}"))]
    Denied {
        /// Why the address was refused.
        reason: String,
        /// Source location captured at the point the error was built.
        #[snafu(implicit)]
        location: snafu::Location,
    },
    /// The connection attempt failed at the socket layer.
    #[snafu(context(name(ConnectIoSnafu)), display("connect failed: {source}"))]
    Io {
        /// The socket error.
        source: std::io::Error,
        /// Source location captured at the point the error was built.
        #[snafu(implicit)]
        location: snafu::Location,
    },
    /// The attempt did not complete within the timeout the connector was
    /// given.
    #[snafu(context(name(ConnectTimedOutSnafu)), display("connect timed out"))]
    TimedOut {
        /// Source location captured at the point the error was built.
        #[snafu(implicit)]
        location: snafu::Location,
    },
}

/// Opens the transport stream for one validated address.
///
/// # Contract
///
/// - `connect` is where a consumer egress policy is enforced. Every hop
///   reaches it, including hops whose URL host is an IP literal, which the
///   [`crate::Resolver`] never sees. An adapter whose allow/deny lives only
///   in a resolver wrapper lets IP-literal targets through; it must refuse
///   denied addresses here with [`ConnectError::Denied`], before opening a
///   socket.
/// - `connect` opens a stream to exactly `addr`. It never resolves a name,
///   substitutes another address, or reuses a pooled stream opened for a
///   different address. The acquirer's DNS-rebinding guarantee rests on
///   this: `addr` is the one the network-target policy validated.
/// - `timeout` is the caller's per-attempt budget. The acquirer also
///   enforces it around the returned future, so a connector that ignores
///   it cannot stall the hop past it.
/// - Dropping the returned future abandons the attempt; implementations
///   must not leave a task running that finishes the connection later.
///
/// An adapter that tunnels through a proxy must pass the validated IP
/// address to the proxy (for example a SOCKS5 request with an IP address,
/// not a domain). A proxy that resolves the URL host itself (an HTTP
/// forward proxy, SOCKS with remote DNS) does not preserve this contract:
/// it performs a second, unvalidated resolution.
pub trait Connector: Send + Sync {
    /// Open a stream to `addr` within `timeout`.
    ///
    /// # Errors
    ///
    /// The returned future resolves to [`ConnectError::Denied`] for a policy
    /// refusal, [`ConnectError::TimedOut`] when `timeout` elapses, and
    /// [`ConnectError::Io`] for any socket failure.
    fn connect(
        &self,
        addr: SocketAddr,
        timeout: Duration,
    ) -> BoxFut<'_, Result<Box<dyn ConnectedStream>, ConnectError>>;
}

/// Plain TCP via `tokio::net::TcpStream::connect(addr)`: no proxy, no proxy
/// environment variables, no resolution.
#[derive(Debug, Clone, Copy, Default)]
pub struct DirectConnector;

impl Connector for DirectConnector {
    fn connect(
        &self,
        addr: SocketAddr,
        timeout: Duration,
    ) -> BoxFut<'_, Result<Box<dyn ConnectedStream>, ConnectError>> {
        Box::pin(async move {
            let stream = tokio::time::timeout(timeout, TcpStream::connect(addr))
                .await
                .map_err(|_elapsed| ConnectTimedOutSnafu.build())?
                .context(ConnectIoSnafu)?;
            let stream: Box<dyn ConnectedStream> = Box::new(stream);
            Ok(stream)
        })
    }
}
