//! Independent local fixture for static-acquisition tests.
//!
//! - [`Origin`]: a hand-written HTTP/1.1 origin on a loopback listener
//!   (optionally behind TLS), replying from a per-path script and recording
//!   every request head, the SNI it saw, and how many connections it
//!   accepted.
//! - [`RecordingConnector`]: maps public-classified addresses (8.8.8.8,
//!   9.9.9.9, ...) onto local origins and records every address it is asked
//!   for. Unmapped loopback addresses are dialed for real, so a policy
//!   bypass would reach a local listener and show up there; any other
//!   unmapped address is refused without touching the network.
//! - [`ScriptedResolver`]: per-host answer sequences with call recording,
//!   for DNS-rebinding and deny-before-lookup cases.
//!
//! Nothing here contacts a non-loopback address.

#![expect(clippy::unwrap_used, reason = "fixture setup must fail loudly")]

use std::collections::{HashMap, VecDeque};
use std::io;
use std::net::{IpAddr, SocketAddr};
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::Duration;

use snafu::IntoError as _;
use sylloge::{
    BoxFut, ConnectDeniedSnafu, ConnectError, ConnectIoSnafu, ConnectedStream, Connector, Resolver,
};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, DuplexStream, ReadBuf};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Notify;
use tokio_rustls::TlsAcceptor;

/// Largest request head the origin reads before giving up.
const MAX_REQUEST_HEAD: usize = 64 * 1024;

/// Chunk size the endless body writes.
const STREAM_CHUNK: usize = 16 * 1024;

/// Parse an address literal used by a test.
pub fn addr(text: &str) -> SocketAddr {
    text.parse().unwrap()
}

/// Parse an IP literal used by a test.
pub fn ip(text: &str) -> IpAddr {
    text.parse().unwrap()
}

/// What the origin sends for one path.
#[derive(Clone)]
pub enum Reply {
    /// Write these bytes verbatim, then close.
    Raw(Vec<u8>),
    /// Send nothing; signal `request_seen`, then wait for the client to
    /// close and signal `closed`.
    Stall(Arc<StallSignals>),
    /// Send `head`, then body chunks until the client goes away, counting
    /// bytes written.
    Endless {
        /// Response head, including the blank line.
        head: Vec<u8>,
        /// Body bytes the origin managed to write.
        written: Arc<AtomicU64>,
    },
}

/// Signals for a [`Reply::Stall`] exchange.
#[derive(Default)]
pub struct StallSignals {
    /// Notified once the request head has been read.
    pub request_seen: Notify,
    /// Notified once the client closed the connection.
    pub closed: Notify,
}

/// `200 OK` with `body` and a `Content-Length`.
pub fn ok(body: &str) -> Reply {
    Reply::Raw(
        format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\n\
             ETag: \"fixture\"\r\nSet-Cookie: tracker=1\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
        .into_bytes(),
    )
}

/// A redirect with the given status and `Location`.
pub fn redirect(status: u16, location: &str) -> Reply {
    Reply::Raw(
        format!(
            "HTTP/1.1 {status} Redirect\r\nLocation: {location}\r\nContent-Length: 0\r\n\
             Connection: close\r\n\r\n"
        )
        .into_bytes(),
    )
}

/// A raw response head plus body.
pub fn raw(bytes: &[u8]) -> Reply {
    Reply::Raw(bytes.to_vec())
}

/// One request head the origin read.
#[derive(Debug, Clone)]
pub struct SeenRequest {
    /// Request line and headers, as received.
    pub head: String,
    /// SNI presented in the TLS handshake, for a TLS origin.
    pub sni: Option<String>,
}

impl SeenRequest {
    /// The request-target from the request line.
    pub fn target(&self) -> &str {
        self.head.split(' ').nth(1).unwrap_or("")
    }

    /// Header names (lowercase), in order.
    pub fn header_names(&self) -> Vec<String> {
        self.head
            .lines()
            .skip(1)
            .filter_map(|line| line.split_once(':'))
            .map(|(name, _)| name.trim().to_ascii_lowercase())
            .collect()
    }

    /// Value of header `name` (case-insensitive).
    pub fn header(&self, name: &str) -> Option<String> {
        self.head
            .lines()
            .skip(1)
            .filter_map(|line| line.split_once(':'))
            .find(|(key, _)| key.trim().eq_ignore_ascii_case(name))
            .map(|(_, value)| value.trim().to_owned())
    }
}

/// A scripted local HTTP/1.1 origin.
pub struct Origin {
    local: SocketAddr,
    requests: Arc<Mutex<Vec<SeenRequest>>>,
    accepted: Arc<AtomicUsize>,
}

impl Origin {
    /// Serve plain HTTP on a loopback port.
    pub async fn http(routes: Vec<(&str, Reply)>) -> Self {
        Self::start(routes, None).await
    }

    /// Serve HTTPS on a loopback port with `tls`.
    pub async fn https(routes: Vec<(&str, Reply)>, tls: Arc<rustls::ServerConfig>) -> Self {
        Self::start(routes, Some(TlsAcceptor::from(tls))).await
    }

    async fn start(routes: Vec<(&str, Reply)>, tls: Option<TlsAcceptor>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let local = listener.local_addr().unwrap();
        let routes: Arc<HashMap<String, Reply>> = Arc::new(
            routes
                .into_iter()
                .map(|(path, reply)| (path.to_owned(), reply))
                .collect(),
        );
        let requests = Arc::new(Mutex::new(Vec::new()));
        let accepted = Arc::new(AtomicUsize::new(0));
        let (log, count) = (Arc::clone(&requests), Arc::clone(&accepted));
        tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    return;
                };
                count.fetch_add(1, Ordering::SeqCst);
                let (routes, log, tls) = (Arc::clone(&routes), Arc::clone(&log), tls.clone());
                tokio::spawn(async move {
                    match tls {
                        Some(acceptor) => {
                            let Ok(stream) = acceptor.accept(stream).await else {
                                return;
                            };
                            let sni = stream.get_ref().1.server_name().map(str::to_owned);
                            serve(stream, &routes, &log, sni).await;
                        }
                        None => serve(stream, &routes, &log, None).await,
                    }
                });
            }
        });
        Self {
            local,
            requests,
            accepted,
        }
    }

    /// The loopback address the origin listens on.
    pub fn local(&self) -> SocketAddr {
        self.local
    }

    /// Every request head read so far.
    pub fn requests(&self) -> Vec<SeenRequest> {
        self.requests.lock().unwrap().clone()
    }

    /// Connections accepted so far.
    pub fn accepted(&self) -> usize {
        self.accepted.load(Ordering::SeqCst)
    }
}

/// Serve one exchange on `stream` from `routes`.
pub async fn serve<S>(
    mut stream: S,
    routes: &HashMap<String, Reply>,
    log: &Mutex<Vec<SeenRequest>>,
    sni: Option<String>,
) where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let Some(head) = read_head(&mut stream).await else {
        return;
    };
    let seen = SeenRequest { head, sni };
    let reply = routes.get(seen.target()).cloned();
    log.lock().unwrap().push(seen);
    match reply {
        Some(Reply::Raw(bytes)) => {
            let _ = stream.write_all(&bytes).await;
            let _ = stream.shutdown().await;
        }
        Some(Reply::Stall(signals)) => {
            signals.request_seen.notify_one();
            let mut sink = [0_u8; 1024];
            while matches!(stream.read(&mut sink).await, Ok(n) if n > 0) {}
            signals.closed.notify_one();
        }
        Some(Reply::Endless { head, written }) => {
            if stream.write_all(&head).await.is_err() {
                return;
            }
            let chunk = vec![b'x'; STREAM_CHUNK];
            while stream.write_all(&chunk).await.is_ok() {
                written.fetch_add(u64::try_from(STREAM_CHUNK).unwrap(), Ordering::SeqCst);
            }
        }
        None => {
            let _ = stream
                .write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n")
                .await;
        }
    }
}

/// Read a request head up to the blank line.
async fn read_head<S: AsyncRead + Unpin>(stream: &mut S) -> Option<String> {
    let mut head = Vec::new();
    let mut byte = [0_u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        if head.len() > MAX_REQUEST_HEAD || stream.read(&mut byte).await.ok()? == 0 {
            return None;
        }
        head.push(byte[0]);
    }
    String::from_utf8(head).ok()
}

/// How [`RecordingConnector`] handles one address.
#[derive(Clone)]
pub enum Route {
    /// Dial this local address.
    Tcp(SocketAddr),
    /// Hand one end of an in-memory duplex pipe to the client and serve the
    /// other end from these routes.
    Duplex(Arc<HashMap<String, Reply>>),
    /// Never complete the connection.
    Stall,
    /// Refuse by egress policy.
    Deny,
}

/// Build a [`Route::Duplex`].
pub fn duplex(routes: Vec<(&str, Reply)>) -> Route {
    Route::Duplex(Arc::new(
        routes
            .into_iter()
            .map(|(path, reply)| (path.to_owned(), reply))
            .collect(),
    ))
}

/// A [`Connector`] that records every address it is asked for.
#[derive(Default)]
pub struct RecordingConnector {
    routes: Mutex<HashMap<SocketAddr, Route>>,
    attempts: Mutex<Vec<SocketAddr>>,
    bytes_read: Arc<AtomicU64>,
    duplex_log: Arc<Mutex<Vec<SeenRequest>>>,
}

impl RecordingConnector {
    /// An empty connector.
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Route `public` to `route`.
    pub fn route(&self, public: &str, route: Route) {
        self.routes.lock().unwrap().insert(addr(public), route);
    }

    /// Every address asked for, in order.
    pub fn attempts(&self) -> Vec<SocketAddr> {
        self.attempts.lock().unwrap().clone()
    }

    /// Bytes the client read through streams this connector returned.
    pub fn bytes_read(&self) -> u64 {
        self.bytes_read.load(Ordering::SeqCst)
    }

    /// Request heads served over duplex routes.
    pub fn duplex_requests(&self) -> Vec<SeenRequest> {
        self.duplex_log.lock().unwrap().clone()
    }
}

impl Connector for RecordingConnector {
    fn connect(
        &self,
        addr: SocketAddr,
        _timeout: Duration,
    ) -> BoxFut<'_, Result<Box<dyn ConnectedStream>, ConnectError>> {
        self.attempts.lock().unwrap().push(addr);
        let route = self.routes.lock().unwrap().get(&addr).cloned();
        let counter = Arc::clone(&self.bytes_read);
        let log = Arc::clone(&self.duplex_log);
        Box::pin(async move {
            let route = match route {
                Some(route) => route,
                // WHY: a loopback address is dialed for real so that a
                // policy bypass would reach a local listener and be
                // observable there, not only in `attempts`.
                None if addr.ip().is_loopback() => Route::Tcp(addr),
                None => {
                    return Err(ConnectIoSnafu.into_error(io::Error::new(
                        io::ErrorKind::ConnectionRefused,
                        "fixture: unmapped address",
                    )));
                }
            };
            let stream: Box<dyn ConnectedStream> = match route {
                Route::Tcp(local) => {
                    let tcp = TcpStream::connect(local)
                        .await
                        .map_err(|source| ConnectIoSnafu.into_error(source))?;
                    Box::new(Counting::new(tcp, counter))
                }
                Route::Duplex(routes) => {
                    let (client, server) = tokio::io::duplex(STREAM_CHUNK);
                    tokio::spawn(serve_duplex(server, routes, log));
                    Box::new(Counting::new(client, counter))
                }
                Route::Stall => std::future::pending().await,
                Route::Deny => {
                    return ConnectDeniedSnafu {
                        reason: "fixture egress policy",
                    }
                    .fail();
                }
            };
            Ok(stream)
        })
    }
}

async fn serve_duplex(
    server: DuplexStream,
    routes: Arc<HashMap<String, Reply>>,
    log: Arc<Mutex<Vec<SeenRequest>>>,
) {
    serve(server, &routes, &log, None).await;
}

/// Stream wrapper counting bytes read by the client.
struct Counting<S> {
    inner: S,
    read: Arc<AtomicU64>,
}

impl<S> Counting<S> {
    fn new(inner: S, read: Arc<AtomicU64>) -> Self {
        Self { inner, read }
    }
}

impl<S: AsyncRead + Unpin> AsyncRead for Counting<S> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let before = buf.filled().len();
        let polled = Pin::new(&mut self.inner).poll_read(cx, buf);
        let added = buf.filled().len() - before;
        self.read
            .fetch_add(u64::try_from(added).unwrap(), Ordering::SeqCst);
        polled
    }
}

impl<S: AsyncWrite + Unpin> AsyncWrite for Counting<S> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

/// One scripted resolver answer.
#[derive(Clone)]
pub enum Answer {
    /// Resolve to these addresses.
    Addrs(Vec<IpAddr>),
    /// Refuse by egress policy before lookup.
    Deny,
}

/// A [`Resolver`] answering from per-host scripts. Each call consumes the
/// next answer; the last answer repeats.
#[derive(Default)]
pub struct ScriptedResolver {
    answers: Mutex<HashMap<String, VecDeque<Answer>>>,
    calls: Mutex<Vec<String>>,
}

impl ScriptedResolver {
    /// An empty resolver.
    pub fn new() -> Self {
        Self::default()
    }

    /// Append an answer for `host`.
    pub fn answer(self, host: &str, addrs: &[&str]) -> Self {
        self.push(host, Answer::Addrs(addrs.iter().map(|a| ip(a)).collect()))
    }

    /// Append a deny-before-lookup refusal for `host`.
    pub fn deny(self, host: &str) -> Self {
        self.push(host, Answer::Deny)
    }

    fn push(self, host: &str, answer: Answer) -> Self {
        self.answers
            .lock()
            .unwrap()
            .entry(host.to_owned())
            .or_default()
            .push_back(answer);
        self
    }

    /// Every host asked for, in order.
    pub fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }
}

impl Resolver for ScriptedResolver {
    fn resolve(&self, host: &str, _port: u16) -> io::Result<Vec<IpAddr>> {
        self.calls.lock().unwrap().push(host.to_owned());
        let mut answers = self.answers.lock().unwrap();
        let script = answers
            .get_mut(host)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "fixture: no such host"))?;
        let answer = if script.len() > 1 {
            script.pop_front().unwrap()
        } else {
            script.front().cloned().unwrap()
        };
        match answer {
            Answer::Addrs(addrs) => Ok(addrs),
            Answer::Deny => Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "fixture: egress policy refuses this host",
            )),
        }
    }
}

/// A locally minted CA and a server identity for `host` signed by it.
pub struct TlsIdentity {
    /// DER of the CA certificate, for `TrustAnchors::from_der`.
    pub ca_der: Vec<u8>,
    /// DER of the leaf certificate the server presents.
    pub leaf_der: Vec<u8>,
    /// Server configuration presenting the leaf.
    pub server: Arc<rustls::ServerConfig>,
}

/// Mint a throwaway CA and a leaf for `host`.
pub fn tls_identity(host: &str) -> TlsIdentity {
    use rcgen::{BasicConstraints, CertificateParams, IsCa, Issuer, KeyPair};
    use rustls::pki_types::{PrivateKeyDer, PrivatePkcs8KeyDer};

    let ca_key = KeyPair::generate().unwrap();
    let mut ca_params = CertificateParams::new(Vec::<String>::new()).unwrap();
    ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    let ca_cert = ca_params.self_signed(&ca_key).unwrap();
    let issuer = Issuer::new(ca_params, ca_key);

    let leaf_key = KeyPair::generate().unwrap();
    let leaf = CertificateParams::new(vec![host.to_owned()])
        .unwrap()
        .signed_by(&leaf_key, &issuer)
        .unwrap();
    let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(leaf_key.serialize_der()));
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut server = rustls::ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(vec![leaf.der().clone()], key)
        .unwrap();
    server.alpn_protocols = vec![b"http/1.1".to_vec()];
    TlsIdentity {
        ca_der: ca_cert.der().to_vec(),
        leaf_der: leaf.der().to_vec(),
        server: Arc::new(server),
    }
}
