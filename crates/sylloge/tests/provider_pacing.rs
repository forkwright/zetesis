//! Pacing and concurrency on one provider instance, on a paused tokio
//! clock. TLS over in-memory pipes stands in for each provider host, and
//! the connector records when every connection opened and closed, so a
//! test reads the provider's request schedule directly. Nothing here
//! touches a socket.

#![expect(clippy::unwrap_used, reason = "test assertions must fail loudly")]

#[expect(
    dead_code,
    reason = "the shared transport fixture serves several test crates; this one uses part of it"
)]
mod fixture;

use std::collections::{HashMap, VecDeque};
use std::io;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::Duration;

use fixture::{Reply, ScriptedResolver, SeenRequest, raw, serve, tls_identity};
use serde_json::json;
use sylloge::{
    AcquisitionLimits, Arxiv, AttemptOutcome, BoxFut, BudgetConstraint, ConnectError,
    ConnectedStream, Connector, Error, Provider, ProviderAnswer, QueryShape, Router, SchemePolicy,
    SearchConstraints, StaticAcquirer, TrustAnchors, Wikipedia,
};
use tokio::io::{AsyncRead, AsyncWrite, DuplexStream, ReadBuf};
use tokio::time::Instant;
use tokio_rustls::TlsAcceptor;
use url::Position;

const ARXIV_DOCUMENTED: &[u8] =
    include_bytes!("fixtures/providers/arxiv/search_documented_shape.xml");

const ARXIV_HOST: &str = "export.arxiv.org";
const WIKIPEDIA_HOST: &str = "en.wikipedia.org";
const WIKIPEDIA_AGENT: &str = "wiki-caller/1.0 (https://example.org/wiki-contact) sylloge/0.0";
const QUERY: &str = "stable identity";

/// One scripted connection: how long the origin holds its response once
/// the TLS handshake is done, and the response it then sends.
struct Step {
    hold: Duration,
    reply: Reply,
}

fn step(hold_ms: u64, reply: Reply) -> Step {
    Step {
        hold: Duration::from_millis(hold_ms),
        reply,
    }
}

/// What the connector saw, in order, stamped with the time since the test
/// began on the tokio clock.
type Timeline = Arc<Mutex<Vec<(Duration, String)>>>;

/// Serves each connection over an in-memory pipe behind TLS, answering the
/// next scripted step, and records when each connection opened and closed.
struct PipeConnector {
    acceptor: TlsAcceptor,
    target: String,
    script: Mutex<VecDeque<Step>>,
    requests: Arc<Mutex<Vec<SeenRequest>>>,
    timeline: Timeline,
    start: Instant,
    opened: AtomicUsize,
}

impl PipeConnector {
    fn record(timeline: &Timeline, start: Instant, event: String) {
        timeline.lock().unwrap().push((start.elapsed(), event));
    }

    fn timeline(&self) -> Vec<(Duration, String)> {
        self.timeline.lock().unwrap().clone()
    }
}

impl Connector for PipeConnector {
    fn connect(
        &self,
        _addr: SocketAddr,
        _timeout: Duration,
    ) -> BoxFut<'_, Result<Box<dyn ConnectedStream>, ConnectError>> {
        let id = self.opened.fetch_add(1, Ordering::SeqCst) + 1;
        Self::record(&self.timeline, self.start, format!("open {id}"));
        let next = self.script.lock().unwrap().pop_front();
        let (client, server) = tokio::io::duplex(64 * 1024);
        let (acceptor, target, requests) = (
            self.acceptor.clone(),
            self.target.clone(),
            Arc::clone(&self.requests),
        );
        tokio::spawn(async move {
            let Ok(tls) = acceptor.accept(server).await else {
                return;
            };
            let Some(Step { hold, reply }) = next else {
                return;
            };
            tokio::time::sleep(hold).await;
            serve(tls, &HashMap::from([(target, reply)]), &requests, None).await;
        });
        let stream = Tracked {
            inner: client,
            id,
            timeline: Arc::clone(&self.timeline),
            start: self.start,
        };
        Box::pin(async move { Ok(Box::new(stream) as Box<dyn ConnectedStream>) })
    }
}

/// The client end of a pipe, recording its close when the acquirer drops it.
struct Tracked {
    inner: DuplexStream,
    id: usize,
    timeline: Timeline,
    start: Instant,
}

impl Drop for Tracked {
    fn drop(&mut self) {
        PipeConnector::record(&self.timeline, self.start, format!("close {}", self.id));
    }
}

impl AsyncRead for Tracked {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}

impl AsyncWrite for Tracked {
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

/// A pipe connector for `host` answering `target` with `script`, and an
/// acquirer that reaches `host` only through it.
fn pipe(
    host: &str,
    target: String,
    script: Vec<Step>,
) -> (Arc<PipeConnector>, Arc<StaticAcquirer>) {
    let identity = tls_identity(host);
    let connector = Arc::new(PipeConnector {
        acceptor: TlsAcceptor::from(Arc::clone(&identity.server)),
        target,
        script: Mutex::new(script.into()),
        requests: Arc::new(Mutex::new(Vec::new())),
        timeline: Arc::new(Mutex::new(Vec::new())),
        start: Instant::now(),
        opened: AtomicUsize::new(0),
    });
    let limits = AcquisitionLimits::new(
        SchemePolicy::HttpsOnly,
        0,
        Duration::from_secs(5),
        Duration::from_secs(60),
        1024 * 1024,
    )
    .unwrap();
    let acquirer = StaticAcquirer::builder(
        limits,
        TrustAnchors::from_der([identity.ca_der.as_slice()]).unwrap(),
    )
    .connector(Arc::clone(&connector) as _)
    .resolver(Arc::new(ScriptedResolver::new().answer(host, &["8.8.8.8"])))
    .build()
    .unwrap();
    (connector, Arc::new(acquirer))
}

fn constraints() -> SearchConstraints {
    SearchConstraints::new(3, BudgetConstraint::free_only())
}

fn arxiv_target() -> String {
    let url = Arxiv::request(QUERY, &constraints()).unwrap().url;
    url[Position::BeforePath..].to_owned()
}

/// The Wikipedia search target for [`QUERY`], written out: the request
/// builder needs an instance, and the instance needs this pipe's acquirer.
const WIKIPEDIA_TARGET: &str = "/w/rest.php/v1/search/page?q=stable+identity&limit=3";

fn http(status: &str, headers: &[(&str, &str)], body: &[u8]) -> Reply {
    let mut head = format!("HTTP/1.1 {status}\r\nContent-Length: {}\r\n", body.len());
    for (name, value) in headers {
        head.push_str(name);
        head.push_str(": ");
        head.push_str(value);
        head.push_str("\r\n");
    }
    head.push_str("Connection: close\r\n\r\n");
    let mut bytes = head.into_bytes();
    bytes.extend_from_slice(body);
    raw(&bytes)
}

fn atom() -> Reply {
    http(
        "200 OK",
        &[("Content-Type", "application/atom+xml")],
        ARXIV_DOCUMENTED,
    )
}

fn wiki_json() -> Reply {
    http(
        "200 OK",
        &[("Content-Type", "application/json")],
        br#"{"pages":[]}"#,
    )
}

fn at(ms: u64, event: &str) -> (Duration, String) {
    (Duration::from_millis(ms), event.to_owned())
}

async fn ask(provider: &dyn Provider, query: &str) -> ProviderAnswer {
    provider.search(query, &constraints()).await
}

#[tokio::test(start_paused = true)]
async fn one_arxiv_instance_opens_one_connection_at_a_time() {
    let (connector, acquirer) = pipe(
        ARXIV_HOST,
        arxiv_target(),
        vec![step(5_000, atom()), step(0, atom())],
    );
    let arxiv = Arxiv::new(acquirer);
    let (first, second) = tokio::join!(ask(&arxiv, QUERY), ask(&arxiv, QUERY));
    assert!(
        first.result.is_ok() && second.result.is_ok(),
        "both searches are answered"
    );
    assert_eq!(
        connector.timeline(),
        [
            at(0, "open 1"),
            at(5_000, "close 1"),
            at(5_000, "open 2"),
            at(5_000, "close 2"),
        ],
        "the second connection opens only once the first has closed"
    );
}

#[tokio::test(start_paused = true)]
async fn one_arxiv_instance_starts_requests_three_seconds_apart() {
    let (connector, acquirer) = pipe(
        ARXIV_HOST,
        arxiv_target(),
        vec![step(0, atom()), step(0, atom())],
    );
    let arxiv = Arxiv::new(acquirer);
    assert!(ask(&arxiv, QUERY).await.result.is_ok(), "first answered");
    assert!(ask(&arxiv, QUERY).await.result.is_ok(), "second answered");
    assert_eq!(
        connector.timeline(),
        [
            at(0, "open 1"),
            at(0, "close 1"),
            at(3_000, "open 2"),
            at(3_000, "close 2"),
        ],
        "the documented interval spaces every caller of the instance"
    );
}

#[tokio::test(start_paused = true)]
async fn one_wikipedia_instance_holds_three_connections_at_most() {
    let (connector, acquirer) = pipe(
        WIKIPEDIA_HOST,
        WIKIPEDIA_TARGET.to_owned(),
        vec![
            step(5_000, wiki_json()),
            step(5_000, wiki_json()),
            step(5_000, wiki_json()),
            step(0, wiki_json()),
        ],
    );
    let wikipedia = Wikipedia::new(acquirer, WIKIPEDIA_AGENT).unwrap();
    let (a, b, c, d) = tokio::join!(
        ask(&wikipedia, QUERY),
        ask(&wikipedia, QUERY),
        ask(&wikipedia, QUERY),
        ask(&wikipedia, QUERY),
    );
    for answer in [a, b, c, d] {
        assert!(answer.result.is_ok(), "every search is answered");
    }
    assert_eq!(
        connector.timeline(),
        [
            at(0, "open 1"),
            at(300, "open 2"),
            at(600, "open 3"),
            at(5_000, "close 1"),
            at(5_000, "open 4"),
            at(5_000, "close 4"),
            at(5_300, "close 2"),
            at(5_600, "close 3"),
        ],
        "300 ms apart, and the fourth waits for one of three connections to close"
    );
}

#[tokio::test(start_paused = true)]
async fn a_retry_after_holds_every_caller_of_the_instance() {
    let limited = http(
        "429 Too Many Requests",
        &[("Content-Type", "text/html"), ("Retry-After", "5")],
        b"",
    );
    let (connector, acquirer) = pipe(
        ARXIV_HOST,
        arxiv_target(),
        vec![step(0, limited), step(0, atom())],
    );
    let arxiv = Arxiv::new(acquirer);
    let err = ask(&arxiv, QUERY).await.result.unwrap_err();
    assert!(
        matches!(
            err,
            Error::RateLimited {
                retry_after_ms: Some(5_000),
                ..
            }
        ),
        "the 429 and its Retry-After are reported: {err:?}"
    );
    assert!(ask(&arxiv, QUERY).await.result.is_ok(), "then answered");
    assert_eq!(
        connector.timeline(),
        [
            at(0, "open 1"),
            at(0, "close 1"),
            at(5_000, "open 2"),
            at(5_000, "close 2"),
        ],
        "a direct caller waits out the Retry-After the instance was sent"
    );
}

#[tokio::test(start_paused = true)]
async fn a_hold_past_the_attempt_timeout_is_receipted_as_rate_limited_without_a_call() {
    let limited = http(
        "429 Too Many Requests",
        &[("Content-Type", "text/html"), ("Retry-After", "60")],
        b"",
    );
    let (connector, acquirer) = pipe(
        ARXIV_HOST,
        arxiv_target(),
        vec![step(0, limited), step(0, atom())],
    );
    let router = Router::new(vec![Arc::new(Arxiv::new(acquirer))], Duration::from_secs(2)).unwrap();
    let (now, constraints) = (jiff::Timestamp::now(), constraints());
    let search = || router.search(QUERY, QueryShape::AcademicLiterature, &constraints, now);
    search().await.unwrap();
    let started = Instant::now();
    let second = search().await.unwrap();
    assert_eq!(started.elapsed(), Duration::ZERO, "no waiting");
    let attempt = second.provenance.first().unwrap().attempt.clone().unwrap();
    assert_eq!(
        serde_json::to_value(&attempt.outcome).unwrap(),
        json!({
            "status": "failed",
            "class": "transient",
            "message": "provider 'arxiv' rate limited: retry after Some(60000) ms",
        }),
        "a hold known to end after the deadline is rate limited, not timed out"
    );
    assert_eq!(
        connector.timeline(),
        [at(0, "open 1"), at(0, "close 1")],
        "and the provider is not called"
    );
}

#[tokio::test(start_paused = true)]
async fn a_wait_for_a_connection_past_the_attempt_timeout_is_receipted_as_timed_out() {
    let (connector, acquirer) = pipe(
        ARXIV_HOST,
        arxiv_target(),
        vec![step(5_000, atom()), step(0, atom())],
    );
    let arxiv = Arc::new(Arxiv::new(acquirer));
    let router = Router::new(
        vec![Arc::clone(&arxiv) as Arc<dyn Provider>],
        Duration::from_secs(2),
    )
    .unwrap();
    let (now, constraints) = (jiff::Timestamp::now(), constraints());
    let attempt = router.search(QUERY, QueryShape::AcademicLiterature, &constraints, now);
    let (direct, result) = tokio::join!(ask(arxiv.as_ref(), QUERY), attempt);
    assert!(
        direct.result.is_ok(),
        "the direct caller holds the connection and is answered"
    );
    let attempt = result
        .unwrap()
        .provenance
        .first()
        .unwrap()
        .attempt
        .clone()
        .unwrap();
    assert_eq!(
        attempt.outcome,
        AttemptOutcome::TimedOut { timeout_ms: 2_000 },
        "a wait for a connection has no known end, so it is receipted as timed out"
    );
    assert_eq!(
        connector.timeline(),
        [at(0, "open 1"), at(5_000, "close 1")],
        "the routed attempt never opened a connection"
    );
}

#[tokio::test(start_paused = true)]
async fn a_query_the_request_builder_refuses_spends_no_request_and_no_slot() {
    let (connector, acquirer) = pipe(ARXIV_HOST, arxiv_target(), vec![step(0, atom())]);
    let router = Router::new(
        vec![Arc::new(Arxiv::new(acquirer))],
        Duration::from_secs(30),
    )
    .unwrap();
    let now = jiff::Timestamp::now();
    let refused = router
        .search("\"\"", QueryShape::AcademicLiterature, &constraints(), now)
        .await
        .unwrap();
    let attempt = refused.provenance.first().unwrap().attempt.clone().unwrap();
    assert!(
        matches!(attempt.outcome, AttemptOutcome::Failed { .. }),
        "a query of only quotes is refused by the request builder: {attempt:?}"
    );
    assert_eq!(
        refused.cost_spent.total_requests(),
        0,
        "no request was sent, so none is counted"
    );
    let answered = router
        .search(QUERY, QueryShape::AcademicLiterature, &constraints(), now)
        .await
        .unwrap();
    assert_eq!(answered.cost_spent.total_requests(), 1, "one request sent");
    assert_eq!(
        connector.timeline(),
        [at(0, "open 1"), at(0, "close 1")],
        "the refused query claimed no pacing slot, so the next search starts at once"
    );
}
