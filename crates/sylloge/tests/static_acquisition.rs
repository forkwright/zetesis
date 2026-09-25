//! Static acquisition against an independent local fixture.
//!
//! Every test observes the connector (which records each address it is
//! asked to dial) or a local listener, not only the returned failure: a
//! refused target must never have been dialed. Public-classified addresses
//! (8.8.8.8, 9.9.9.9) are routed by the fixture to loopback origins; nothing
//! leaves the host.

#![expect(clippy::unwrap_used, reason = "test assertions must fail loudly")]

mod fixture;

use std::fmt::Write as _;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use futures::FutureExt as _;
use tokio::net::TcpListener;
use url::{Host, Url};

use fixture::{
    Origin, RecordingConnector, Reply, Route, ScriptedResolver, StallSignals, TlsIdentity, addr,
    duplex, ip, ok, raw, redirect, tls_identity,
};
use sylloge::{
    AcquisitionFailure, AcquisitionLimits, BudgetConstraint, ConnectAttempt, ConnectOutcome,
    DowngradePolicy, Error, ErrorClass, ResponseRecord, SchemePolicy, SearchConstraints,
    StaticAcquirer, Transfer, TransferOutcome, TrustAnchors,
};

/// Generous per-attempt and whole-operation bounds for tests that must not
/// time out.
const CONNECT: Duration = Duration::from_secs(5);
const DEADLINE: Duration = Duration::from_secs(30);
const BODY: u64 = 64 * 1024;

fn limits(schemes: SchemePolicy, max_redirects: u32) -> AcquisitionLimits {
    AcquisitionLimits::new(schemes, max_redirects, CONNECT, DEADLINE, BODY).unwrap()
}

/// Constraints with a zero-cap, paid-disabled budget: anonymous acquisition
/// needs no paid authorization.
fn free() -> SearchConstraints {
    SearchConstraints::new(1, BudgetConstraint::free_only())
}

struct Harness {
    connector: Arc<RecordingConnector>,
    resolver: Arc<ScriptedResolver>,
    acquirer: StaticAcquirer,
}

impl Harness {
    fn new(limits: AcquisitionLimits, resolver: ScriptedResolver) -> Self {
        Self::with_identity(limits, resolver, &tls_identity("unused.example"))
    }

    fn with_identity(
        limits: AcquisitionLimits,
        resolver: ScriptedResolver,
        identity: &TlsIdentity,
    ) -> Self {
        let connector = RecordingConnector::new();
        let resolver = Arc::new(resolver);
        let acquirer = StaticAcquirer::builder(
            limits,
            TrustAnchors::from_der([identity.ca_der.as_slice()]).unwrap(),
        )
        .connector(Arc::clone(&connector) as _)
        .resolver(Arc::clone(&resolver) as _)
        .build()
        .unwrap();
        Self {
            connector,
            resolver,
            acquirer,
        }
    }

    async fn get(&self, url: &str) -> Transfer {
        self.acquirer
            .acquire(&Url::parse(url).unwrap(), &free(), None)
            .await
            .unwrap()
    }
}

fn failure(transfer: &Transfer) -> &AcquisitionFailure {
    transfer
        .failure()
        .unwrap_or_else(|| panic!("expected a failure, got {:?}", transfer.outcome()))
}

fn attempt(address: &str, result: ConnectOutcome) -> ConnectAttempt {
    serde_json::from_value(serde_json::json!({ "addr": address, "result": result })).unwrap()
}

/// Whether `listener` has a connection waiting. Checked after `acquire`
/// returned, so any connection the acquirer made is already queued.
fn has_pending_connection(listener: &TcpListener) -> bool {
    listener.accept().now_or_never().is_some()
}

// -- Positive fixtures: anonymous GET succeeds without paid authorization. --

#[tokio::test]
async fn anonymous_get_to_public_target_succeeds_without_paid_authorization() {
    let origin = Origin::http(vec![("/doc?x=1", ok("hello"))]).await;
    let h = Harness::new(
        limits(SchemePolicy::HttpAndHttps, 0),
        ScriptedResolver::new().answer("public.example", &["8.8.8.8"]),
    );
    h.connector.route("8.8.8.8:80", Route::Tcp(origin.local()));

    let transfer = h.get("http://public.example/doc?x=1#frag").await;

    let TransferOutcome::Complete { response, body } = transfer.outcome() else {
        panic!("anonymous GET must complete: {:?}", transfer.outcome());
    };
    assert_eq!(response.status(), 200, "origin status is recorded");
    assert_eq!(body, b"hello", "body is returned verbatim");
    assert_eq!(response.content_type(), Some("text/plain"), "content type");
    assert_eq!(
        response.etag(),
        Some("\"fixture\""),
        "etag is a selected header"
    );
    assert!(
        !serde_json::to_string(response).unwrap().contains("tracker"),
        "Set-Cookie must never be recorded"
    );
    assert_eq!(
        transfer.final_url().map(Url::as_str),
        Some("http://public.example/doc?x=1#frag"),
        "the final URL is the single hop's URL"
    );
    let [hop] = transfer.hops() else {
        panic!("one hop expected: {:?}", transfer.hops());
    };
    assert_eq!(hop.resolved(), [ip("8.8.8.8")], "validated address set");
    assert_eq!(
        hop.connect_attempts(),
        [attempt("8.8.8.8:80", ConnectOutcome::Connected)],
        "exactly one attempt, to the validated address"
    );
    assert_eq!(hop.status(), Some(200), "hop status");

    let [seen] = origin.requests().try_into().unwrap();
    assert_eq!(
        seen.target(),
        "/doc?x=1",
        "origin-form target without fragment"
    );
    assert_eq!(
        seen.header_names(),
        [
            "host",
            "user-agent",
            "accept",
            "accept-encoding",
            "connection"
        ],
        "no cookie, credential, referer, or body header is sent"
    );
    assert_eq!(
        seen.header("host").as_deref(),
        Some("public.example"),
        "Host"
    );
    assert_eq!(
        seen.header("accept-encoding").as_deref(),
        Some("identity"),
        "no compressed transfer is requested"
    );
    assert!(
        seen.header("user-agent").unwrap().starts_with("zetesis/"),
        "default user agent names zetesis"
    );
}

#[tokio::test]
async fn anonymous_https_get_sends_url_host_as_sni_and_host() {
    let identity = tls_identity("public.example");
    let origin = Origin::https(vec![("/", ok("secure"))], Arc::clone(&identity.server)).await;
    let h = Harness::with_identity(
        limits(SchemePolicy::HttpsOnly, 0),
        ScriptedResolver::new().answer("public.example", &["8.8.8.8"]),
        &identity,
    );
    h.connector.route("8.8.8.8:443", Route::Tcp(origin.local()));

    let transfer = h.get("https://public.example/").await;

    assert_eq!(transfer.body(), Some(b"secure".as_slice()), "TLS body");
    let [seen] = origin.requests().try_into().unwrap();
    assert_eq!(
        seen.sni.as_deref(),
        Some("public.example"),
        "SNI carries the URL host, not the connected address"
    );
    assert_eq!(
        seen.header("host").as_deref(),
        Some("public.example"),
        "Host"
    );
    let tls = transfer.hops()[0].tls().unwrap();
    assert_eq!(
        tls.server_name(),
        "public.example",
        "verified name recorded"
    );
    assert_eq!(
        tls.protocol_version(),
        "TLSv1.3",
        "negotiated version recorded"
    );
    let leaf = ring::digest::digest(&ring::digest::SHA256, &identity.leaf_der);
    let expected = leaf.as_ref().iter().fold(String::new(), |mut out, byte| {
        let _ = write!(out, "{byte:02x}");
        out
    });
    assert_eq!(
        tls.peer_leaf_sha256(),
        expected,
        "leaf fingerprint recorded"
    );
}

#[tokio::test]
async fn redirect_status_without_location_is_final() {
    let origin = Origin::http(vec![(
        "/",
        raw(b"HTTP/1.1 302 Found\r\nContent-Length: 2\r\nConnection: close\r\n\r\nno"),
    )])
    .await;
    let h = Harness::new(
        limits(SchemePolicy::HttpAndHttps, 3),
        ScriptedResolver::new(),
    );
    h.connector.route("8.8.8.8:80", Route::Tcp(origin.local()));

    let transfer = h.get("http://8.8.8.8/").await;

    assert_eq!(
        transfer.response().map(ResponseRecord::status),
        Some(302),
        "a 3xx without Location is the final response"
    );
    assert_eq!(transfer.body(), Some(b"no".as_slice()), "its body is kept");
}

// -- Public-to-private redirect. --

#[tokio::test]
async fn redirect_to_loopback_literal_is_refused_without_connecting() {
    let forbidden = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = forbidden.local_addr().unwrap().port();
    let location = format!("http://127.0.0.1:{port}/admin");
    let origin = Origin::http(vec![("/", redirect(302, &location))]).await;
    let h = Harness::new(
        limits(SchemePolicy::HttpAndHttps, 5),
        ScriptedResolver::new(),
    );
    h.connector.route("8.8.8.8:80", Route::Tcp(origin.local()));

    let transfer = h.get("http://8.8.8.8/").await;

    assert!(
        matches!(failure(&transfer), AcquisitionFailure::UnsafeTarget { .. }),
        "loopback redirect target must be unsafe: {:?}",
        transfer.outcome()
    );
    assert_eq!(
        h.connector.attempts(),
        [addr("8.8.8.8:80")],
        "the connector was never asked for the loopback address"
    );
    assert!(
        !has_pending_connection(&forbidden),
        "no socket reached the loopback listener"
    );
    let [first, refused] = transfer.hops() else {
        panic!("two hops expected: {:?}", transfer.hops());
    };
    assert_eq!(
        first.location(),
        Some(location.as_str()),
        "Location recorded"
    );
    assert!(
        refused.resolved().is_empty() && refused.connect_attempts().is_empty(),
        "the refused hop has no validated address and no attempt"
    );
}

#[tokio::test]
async fn redirect_to_host_resolving_private_is_refused_without_connecting() {
    let origin = Origin::http(vec![("/", redirect(307, "http://internal.example/"))]).await;
    let h = Harness::new(
        limits(SchemePolicy::HttpAndHttps, 5),
        ScriptedResolver::new().answer("internal.example", &["10.0.0.7"]),
    );
    h.connector.route("8.8.8.8:80", Route::Tcp(origin.local()));

    let transfer = h.get("http://8.8.8.8/").await;

    let AcquisitionFailure::UnsafeTarget { reason } = failure(&transfer) else {
        panic!(
            "private resolution must be unsafe: {:?}",
            transfer.outcome()
        );
    };
    assert!(
        reason.contains("10.0.0.7"),
        "reason names the address: {reason}"
    );
    assert_eq!(
        h.connector.attempts(),
        [addr("8.8.8.8:80")],
        "10.0.0.7 was never dialed"
    );
    assert_eq!(h.resolver.calls(), ["internal.example"], "resolved once");
}

// -- Encoded and alternate address spellings. --

#[tokio::test]
async fn alternate_address_spellings_are_canonicalized_and_refused_without_connecting() {
    let h = Harness::new(
        limits(SchemePolicy::HttpAndHttps, 0),
        ScriptedResolver::new(),
    );
    for spelling in [
        "http://2130706433/",
        "http://0x7f000001/",
        "http://0x7f.1/",
        "http://0177.0.0.1/",
        "http://127.1/",
        "http://%31%32%37.0.0.1/",
        "http://[::ffff:127.0.0.1]/",
        "http://[::ffff:7f00:1]/",
        "http://[::127.0.0.1]/",
        "http://[::ffff:a9fe:a9fe]/",
    ] {
        let transfer = h.get(spelling).await;
        assert!(
            matches!(failure(&transfer), AcquisitionFailure::UnsafeTarget { .. }),
            "{spelling} must be refused: {:?}",
            transfer.outcome()
        );
        assert!(
            matches!(
                transfer.hops()[0].url().host(),
                Some(Host::Ipv4(_) | Host::Ipv6(_))
            ),
            "{spelling} must canonicalize to an IP address, not a name"
        );
    }
    assert!(h.connector.attempts().is_empty(), "no spelling was dialed");
    assert!(
        h.resolver.calls().is_empty(),
        "no spelling reached DNS as a host name"
    );
}

#[tokio::test]
async fn redirect_with_alternate_spelling_is_canonicalized_and_refused() {
    let origin = Origin::http(vec![("/", redirect(301, "http://0x7f.1/"))]).await;
    let h = Harness::new(
        limits(SchemePolicy::HttpAndHttps, 5),
        ScriptedResolver::new(),
    );
    h.connector.route("8.8.8.8:80", Route::Tcp(origin.local()));

    let transfer = h.get("http://8.8.8.8/").await;

    assert!(
        matches!(failure(&transfer), AcquisitionFailure::UnsafeTarget { .. }),
        "hex loopback Location must be refused: {:?}",
        transfer.outcome()
    );
    assert_eq!(
        transfer.hops()[1].url().as_str(),
        "http://127.0.0.1/",
        "the Location was canonicalized before the decision"
    );
    assert_eq!(
        h.connector.attempts(),
        [addr("8.8.8.8:80")],
        "only the origin"
    );
}

// -- DNS rebinding. --

#[tokio::test]
async fn dns_rebinding_to_private_between_hops_never_reaches_private_address() {
    let origin = Origin::http(vec![
        ("/start", redirect(302, "/next")),
        ("/next", ok("must not be served")),
    ])
    .await;
    let h = Harness::new(
        limits(SchemePolicy::HttpAndHttps, 5),
        ScriptedResolver::new()
            .answer("rebind.example", &["9.9.9.9"])
            .answer("rebind.example", &["10.0.0.9"]),
    );
    h.connector.route("9.9.9.9:80", Route::Tcp(origin.local()));

    let transfer = h.get("http://rebind.example/start").await;

    assert!(
        matches!(failure(&transfer), AcquisitionFailure::UnsafeTarget { .. }),
        "the rebound answer must be refused: {:?}",
        transfer.outcome()
    );
    assert_eq!(
        h.connector.attempts(),
        [addr("9.9.9.9:80")],
        "only the first hop's validated address was dialed"
    );
    assert_eq!(
        h.resolver.calls(),
        ["rebind.example", "rebind.example"],
        "exactly one resolution per hop"
    );
    let targets: Vec<String> = origin
        .requests()
        .iter()
        .map(|r| r.target().to_owned())
        .collect();
    assert_eq!(targets, ["/start"], "the second hop was never requested");
}

#[tokio::test]
async fn dns_answer_change_between_hops_uses_each_hops_validated_set() {
    let first = Origin::http(vec![("/start", redirect(302, "/next"))]).await;
    let second = Origin::http(vec![("/next", ok("moved"))]).await;
    let h = Harness::new(
        limits(SchemePolicy::HttpAndHttps, 5),
        ScriptedResolver::new()
            .answer("moving.example", &["9.9.9.9"])
            .answer("moving.example", &["8.8.8.8"]),
    );
    h.connector.route("9.9.9.9:80", Route::Tcp(first.local()));
    h.connector.route("8.8.8.8:80", Route::Tcp(second.local()));

    let transfer = h.get("http://moving.example/start").await;

    assert_eq!(
        transfer.body(),
        Some(b"moved".as_slice()),
        "second hop served"
    );
    for hop in transfer.hops() {
        for tried in hop.connect_attempts() {
            assert!(
                hop.resolved().contains(&tried.addr().ip()),
                "attempt {tried:?} must use an address validated for its own hop {hop:?}"
            );
        }
    }
    assert_eq!(
        h.connector.attempts(),
        [addr("9.9.9.9:80"), addr("8.8.8.8:80")],
        "each hop dialed only its own answer"
    );
    assert_eq!(
        h.resolver.calls().len(),
        2,
        "one resolution per hop, no re-resolve"
    );
}

// -- Redirect loop and limit. --

#[tokio::test]
async fn redirect_loop_is_refused() {
    let origin = Origin::http(vec![
        ("/a", redirect(302, "/b")),
        ("/b", redirect(302, "/a")),
        ("/self", redirect(308, "/self#elsewhere")),
    ])
    .await;
    let h = Harness::new(
        limits(SchemePolicy::HttpAndHttps, 10),
        ScriptedResolver::new(),
    );
    h.connector.route("8.8.8.8:80", Route::Tcp(origin.local()));

    let transfer = h.get("http://8.8.8.8/a").await;
    assert_eq!(
        failure(&transfer),
        &AcquisitionFailure::RedirectLoop {
            url: Url::parse("http://8.8.8.8/a").unwrap()
        },
        "a -> b -> a is a loop"
    );
    assert_eq!(
        h.connector.attempts().len(),
        2,
        "the loop target is not refetched"
    );

    let transfer = h.get("http://8.8.8.8/self").await;
    assert_eq!(
        failure(&transfer).kind(),
        "redirect_loop",
        "a fragment does not make the same target new"
    );
    assert_eq!(
        h.connector.attempts().len(),
        3,
        "one more fetch, no refetch"
    );
}

#[tokio::test]
async fn redirect_past_limit_is_refused() {
    let origin = Origin::http(vec![
        ("/1", redirect(301, "/2")),
        ("/2", redirect(302, "/3")),
        ("/3", redirect(303, "/4")),
        ("/4", ok("too far")),
    ])
    .await;
    let h = Harness::new(
        limits(SchemePolicy::HttpAndHttps, 2),
        ScriptedResolver::new(),
    );
    h.connector.route("8.8.8.8:80", Route::Tcp(origin.local()));

    let transfer = h.get("http://8.8.8.8/1").await;

    assert_eq!(
        failure(&transfer),
        &AcquisitionFailure::RedirectLimit { max_redirects: 2 },
        "the third redirect exceeds a limit of two"
    );
    assert_eq!(
        h.connector.attempts().len(),
        3,
        "the initial URL plus two redirects"
    );
    assert_eq!(transfer.hops().len(), 3, "no hop for the refused target");
    assert_eq!(
        transfer.hops()[2].location(),
        Some("/4"),
        "refused Location recorded"
    );
}

#[test]
fn redirect_limit_above_ceiling_is_invalid_input() {
    let err = AcquisitionLimits::new(
        SchemePolicy::HttpsOnly,
        AcquisitionLimits::MAX_REDIRECTS_CEILING + 1,
        CONNECT,
        DEADLINE,
        BODY,
    )
    .unwrap_err();
    assert!(
        matches!(err, Error::InvalidConstraint { .. }),
        "21 redirects exceeds the Fetch ceiling, before any acquirer exists: {err:?}"
    );
}

// -- Downgrade policy. --

struct DowngradeFixture {
    h: Harness,
    plain: Origin,
    _secure: Origin,
}

async fn downgrade_fixture(downgrade: DowngradePolicy) -> DowngradeFixture {
    let identity = tls_identity("secure.example");
    let secure = Origin::https(
        vec![("/", redirect(302, "http://9.9.9.9/plain"))],
        Arc::clone(&identity.server),
    )
    .await;
    let plain = Origin::http(vec![("/plain", ok("plaintext"))]).await;
    let h = Harness::with_identity(
        limits(SchemePolicy::HttpAndHttps, 5).with_downgrade(downgrade),
        ScriptedResolver::new().answer("secure.example", &["8.8.8.8"]),
        &identity,
    );
    h.connector.route("8.8.8.8:443", Route::Tcp(secure.local()));
    h.connector.route("9.9.9.9:80", Route::Tcp(plain.local()));
    DowngradeFixture {
        h,
        plain,
        _secure: secure,
    }
}

#[tokio::test]
async fn https_to_http_downgrade_is_refused_by_default() {
    let f = downgrade_fixture(DowngradePolicy::default()).await;

    let transfer = f.h.get("https://secure.example/").await;

    assert_eq!(
        failure(&transfer),
        &AcquisitionFailure::DowngradeRefused,
        "https -> http is refused unless allowed"
    );
    assert_eq!(
        f.h.connector.attempts(),
        [addr("8.8.8.8:443")],
        "the http target was never dialed"
    );
    assert_eq!(
        f.plain.accepted(),
        0,
        "the plaintext origin saw no connection"
    );
}

#[tokio::test]
async fn https_to_http_downgrade_follows_when_allowed() {
    let f = downgrade_fixture(DowngradePolicy::Allow).await;

    let transfer = f.h.get("https://secure.example/").await;

    assert_eq!(
        transfer.body(),
        Some(b"plaintext".as_slice()),
        "an explicitly allowed downgrade is followed"
    );
    assert_eq!(
        f.h.connector.attempts(),
        [addr("8.8.8.8:443"), addr("9.9.9.9:80")],
        "both hops dialed their validated addresses"
    );
}

// -- Denied port and scheme. --

#[tokio::test]
async fn bad_port_is_refused_without_connecting() {
    let origin = Origin::http(vec![("/", redirect(302, "http://8.8.8.8:6000/"))]).await;
    let h = Harness::new(
        limits(SchemePolicy::HttpAndHttps, 5),
        ScriptedResolver::new(),
    );
    h.connector.route("8.8.8.8:80", Route::Tcp(origin.local()));

    let transfer = h.get("http://8.8.8.8:25/").await;
    assert_eq!(
        failure(&transfer),
        &AcquisitionFailure::DeniedPort { port: 25 },
        "SMTP is a Fetch bad port"
    );
    assert!(
        h.connector.attempts().is_empty(),
        "port 25 was never dialed"
    );

    let transfer = h.get("http://8.8.8.8/").await;
    assert_eq!(
        failure(&transfer),
        &AcquisitionFailure::DeniedPort { port: 6000 },
        "a redirect to X11 is refused too"
    );
    assert_eq!(h.connector.attempts(), [addr("8.8.8.8:80")], "only port 80");
}

#[tokio::test]
async fn scheme_outside_policy_is_refused_before_connecting() {
    let h = Harness::new(limits(SchemePolicy::HttpsOnly, 0), ScriptedResolver::new());
    for (url, scheme) in [("http://8.8.8.8/", "http"), ("ftp://8.8.8.8/", "ftp")] {
        let transfer = h.get(url).await;
        assert_eq!(
            failure(&transfer),
            &AcquisitionFailure::SchemeNotAllowed {
                scheme: scheme.to_owned()
            },
            "{url} is outside https_only"
        );
    }
    assert!(h.connector.attempts().is_empty(), "nothing was dialed");
}

// -- Malformed redirect. --

#[tokio::test]
async fn malformed_redirect_location_is_refused() {
    let origin = Origin::http(vec![
        ("/unparseable", redirect(302, "http://[::1/")),
        (
            "/two",
            raw(b"HTTP/1.1 302 Found\r\nLocation: /a\r\nLocation: /b\r\nContent-Length: 0\r\n\r\n"),
        ),
        (
            "/bytes",
            raw(b"HTTP/1.1 302 Found\r\nLocation: /\xff\xfe\r\nContent-Length: 0\r\n\r\n"),
        ),
    ])
    .await;
    let h = Harness::new(
        limits(SchemePolicy::HttpAndHttps, 5),
        ScriptedResolver::new(),
    );
    h.connector.route("8.8.8.8:80", Route::Tcp(origin.local()));

    for path in ["/unparseable", "/two", "/bytes"] {
        let transfer = h.get(&format!("http://8.8.8.8{path}")).await;
        assert!(
            matches!(
                failure(&transfer),
                AcquisitionFailure::MalformedRedirect { .. }
            ),
            "{path} must be a malformed redirect: {:?}",
            transfer.outcome()
        );
        assert_eq!(
            transfer.hops().len(),
            1,
            "{path}: no hop for an unusable target"
        );
    }
    assert_eq!(
        h.connector.attempts().len(),
        3,
        "each redirecting hop was fetched once and nothing after it"
    );
}

// -- Cancellation. --

#[tokio::test]
async fn dropping_acquire_future_closes_the_connection() {
    let signals = Arc::new(StallSignals::default());
    let origin = Origin::http(vec![("/", Reply::Stall(Arc::clone(&signals)))]).await;
    let h = Harness::new(
        limits(SchemePolicy::HttpAndHttps, 0),
        ScriptedResolver::new(),
    );
    h.connector.route("8.8.8.8:80", Route::Tcp(origin.local()));
    let url = Url::parse("http://8.8.8.8/").unwrap();
    let constraints = free();

    tokio::select! {
        transfer = h.acquirer.acquire(&url, &constraints, None) => {
            panic!("a stalled origin cannot complete the transfer: {transfer:?}");
        }
        () = signals.request_seen.notified() => {}
    }
    // NOTE: leaving `select!` dropped the acquire future.
    let closed = tokio::time::timeout(Duration::from_secs(10), signals.closed.notified()).await;
    assert!(
        closed.is_ok(),
        "the origin must observe the connection closed after the drop"
    );
    assert_eq!(
        h.connector.attempts(),
        [addr("8.8.8.8:80")],
        "no connection attempt happened after cancellation"
    );
    assert_eq!(origin.accepted(), 1, "exactly one socket was ever opened");
}

// -- Connect timeout and whole-operation deadline (paused clock). --

#[tokio::test(start_paused = true)]
async fn stalled_connector_times_out_within_connect_budget() {
    let connect = Duration::from_secs(2);
    let limits = AcquisitionLimits::new(
        SchemePolicy::HttpAndHttps,
        0,
        connect,
        Duration::from_secs(60),
        BODY,
    )
    .unwrap();
    let h = Harness::new(
        limits,
        ScriptedResolver::new().answer("slow.example", &["8.8.8.8", "9.9.9.9"]),
    );
    h.connector.route("8.8.8.8:80", Route::Stall);
    h.connector.route("9.9.9.9:80", Route::Stall);
    let started = tokio::time::Instant::now();

    let transfer = h.get("http://slow.example/").await;

    assert_eq!(
        failure(&transfer),
        &AcquisitionFailure::ConnectTimeout { timeout_ms: 2_000 },
        "a connector that never completes hits the per-attempt timeout"
    );
    assert_eq!(
        started.elapsed(),
        connect * 2,
        "each of the two validated addresses got exactly one connect budget"
    );
    assert_eq!(
        transfer.hops()[0].connect_attempts(),
        [
            attempt("8.8.8.8:80", ConnectOutcome::TimedOut),
            attempt("9.9.9.9:80", ConnectOutcome::TimedOut),
        ],
        "both attempts recorded as timed out"
    );
}

#[tokio::test(start_paused = true)]
async fn silent_origin_hits_whole_operation_deadline() {
    let deadline = Duration::from_secs(10);
    let limits = AcquisitionLimits::new(
        SchemePolicy::HttpAndHttps,
        0,
        Duration::from_secs(2),
        deadline,
        BODY,
    )
    .unwrap();
    let h = Harness::new(limits, ScriptedResolver::new());
    let signals = Arc::new(StallSignals::default());
    h.connector
        .route("8.8.8.8:80", duplex(vec![("/", Reply::Stall(signals))]));
    let started = tokio::time::Instant::now();

    let transfer = h.get("http://8.8.8.8/").await;

    assert_eq!(
        failure(&transfer),
        &AcquisitionFailure::DeadlineExceeded {
            deadline_ms: 10_000
        },
        "an origin that never sends a head exhausts the deadline"
    );
    assert_eq!(
        started.elapsed(),
        deadline,
        "the deadline bounds the operation"
    );
    let [hop] = transfer.hops() else {
        panic!("one hop expected: {:?}", transfer.hops());
    };
    assert_eq!(
        hop.connect_attempts(),
        [attempt("8.8.8.8:80", ConnectOutcome::Connected)],
        "evidence up to the deadline is kept"
    );
    assert_eq!(hop.status(), None, "no response head arrived");
    assert_eq!(
        h.connector.duplex_requests().len(),
        1,
        "the request reached the origin before the deadline"
    );
}

// -- Wire and header limits. --

/// A close-delimited 200 head for an endless body.
const ENDLESS_HEAD: &[u8] =
    b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nConnection: close\r\n\r\n";

fn tight_limits() -> AcquisitionLimits {
    limits(SchemePolicy::HttpAndHttps, 0)
        .with_max_header_bytes(AcquisitionLimits::MIN_HEADER_BYTES)
        .unwrap()
}

#[tokio::test]
async fn body_over_limit_stops_reading() {
    let written = Arc::new(AtomicU64::new(0));
    let h = Harness::new(tight_limits(), ScriptedResolver::new());
    h.connector.route(
        "8.8.8.8:80",
        duplex(vec![(
            "/",
            Reply::Endless {
                head: ENDLESS_HEAD.to_vec(),
                written: Arc::clone(&written),
            },
        )]),
    );

    let transfer = h.get("http://8.8.8.8/").await;

    assert_eq!(
        failure(&transfer),
        &AcquisitionFailure::WireLimit {
            max_body_bytes: BODY
        },
        "an endless body stops at the wire limit"
    );
    let header = u64::try_from(AcquisitionLimits::MIN_HEADER_BYTES).unwrap();
    let read = h.connector.bytes_read();
    assert!(
        read <= BODY + 2 * header,
        "reading stopped near the limit: read {read} bytes for a {BODY}-byte limit"
    );
    assert!(
        written.load(Ordering::SeqCst) <= read + 2 * header,
        "the origin could not push much past what was read"
    );
}

#[tokio::test]
async fn declared_length_over_limit_stops_before_body() {
    let h = Harness::new(tight_limits(), ScriptedResolver::new());
    h.connector.route(
        "8.8.8.8:80",
        duplex(vec![(
            "/",
            Reply::Endless {
                head: b"HTTP/1.1 200 OK\r\nContent-Length: 104857600\r\n\r\n".to_vec(),
                written: Arc::new(AtomicU64::new(0)),
            },
        )]),
    );

    let transfer = h.get("http://8.8.8.8/").await;

    assert_eq!(
        failure(&transfer),
        &AcquisitionFailure::WireLimit {
            max_body_bytes: BODY
        },
        "a declared length over the limit is refused"
    );
    let header = u64::try_from(AcquisitionLimits::MIN_HEADER_BYTES).unwrap();
    assert!(
        h.connector.bytes_read() <= 2 * header,
        "the body was not read: {} bytes",
        h.connector.bytes_read()
    );
}

#[tokio::test]
async fn oversized_response_head_is_header_limit() {
    let filler = "a".repeat(20 * 1024);
    let head = format!("HTTP/1.1 200 OK\r\nX-Filler: {filler}\r\nContent-Length: 0\r\n\r\n");
    let h = Harness::new(tight_limits(), ScriptedResolver::new());
    h.connector
        .route("8.8.8.8:80", duplex(vec![("/", raw(head.as_bytes()))]));

    let transfer = h.get("http://8.8.8.8/").await;

    assert_eq!(
        failure(&transfer),
        &AcquisitionFailure::HeaderLimit {
            max_header_bytes: AcquisitionLimits::MIN_HEADER_BYTES
        },
        "a head larger than max_header_bytes is refused"
    );
}

#[tokio::test]
async fn unsupported_content_encoding_is_refused() {
    let h = Harness::new(
        limits(SchemePolicy::HttpAndHttps, 0),
        ScriptedResolver::new(),
    );
    h.connector.route(
        "8.8.8.8:80",
        duplex(vec![(
            "/",
            raw(b"HTTP/1.1 200 OK\r\nContent-Encoding: gzip\r\nContent-Length: 3\r\n\r\nabc"),
        )]),
    );

    let transfer = h.get("http://8.8.8.8/").await;

    assert_eq!(
        failure(&transfer),
        &AcquisitionFailure::UnsupportedContentEncoding {
            coding: "gzip".to_owned()
        },
        "an identity-only acquirer refuses a coded body"
    );
}

// -- Egress refusal. --

#[tokio::test]
async fn resolver_refusal_is_egress_denied_without_connecting() {
    let h = Harness::new(
        limits(SchemePolicy::HttpAndHttps, 0),
        ScriptedResolver::new().deny("denied.example"),
    );

    let transfer = h.get("http://denied.example/").await;

    let refusal = failure(&transfer);
    assert_eq!(
        refusal.kind(),
        "egress_denied",
        "a resolver refusal is egress"
    );
    assert_eq!(
        refusal.class(),
        ErrorClass::Permanent,
        "a policy refusal is not retried as a DNS blip"
    );
    assert!(h.connector.attempts().is_empty(), "nothing was dialed");
    assert_eq!(h.resolver.calls(), ["denied.example"], "refused at lookup");
}

#[tokio::test]
async fn connector_denial_of_ip_literal_is_egress_denied() {
    let h = Harness::new(
        limits(SchemePolicy::HttpAndHttps, 0),
        ScriptedResolver::new(),
    );
    h.connector.route("8.8.8.8:80", Route::Deny);

    let transfer = h.get("http://8.8.8.8/").await;

    assert_eq!(
        failure(&transfer).kind(),
        "egress_denied",
        "connector refusal"
    );
    assert_eq!(
        transfer.hops()[0].connect_attempts(),
        [attempt("8.8.8.8:80", ConnectOutcome::Denied)],
        "the attempt is recorded as denied"
    );
    assert!(
        h.resolver.calls().is_empty(),
        "an IP literal never reaches the resolver, so only connect() can enforce egress"
    );
}

// -- Caller input and future shape. --

#[tokio::test]
async fn url_over_limit_is_invalid_input_without_attempt() {
    let h = Harness::new(
        limits(SchemePolicy::HttpAndHttps, 0)
            .with_max_url_bytes(32)
            .unwrap(),
        ScriptedResolver::new(),
    );
    let long = Url::parse(&format!("http://8.8.8.8/{}", "a".repeat(64))).unwrap();

    let err = h.acquirer.acquire(&long, &free(), None).await.unwrap_err();

    assert!(
        matches!(err, Error::InvalidConstraint { .. }),
        "an over-long URL is invalid input: {err:?}"
    );
    assert!(h.connector.attempts().is_empty(), "nothing was dialed");
}

#[tokio::test]
async fn userinfo_never_enters_a_request_or_hop_record() {
    let origin = Origin::http(vec![(
        "/",
        redirect(302, "http://user:origin-secret@9.9.9.9/"),
    )])
    .await;
    let h = Harness::new(
        limits(SchemePolicy::HttpAndHttps, 5),
        ScriptedResolver::new().answer("public.example", &["8.8.8.8"]),
    );
    h.connector.route("8.8.8.8:80", Route::Tcp(origin.local()));

    let refused = h.get("http://user:caller-secret@public.example/").await;
    assert!(
        matches!(failure(&refused), AcquisitionFailure::UnsafeTarget { .. }),
        "a requested URL with userinfo is an unsafe target: {:?}",
        refused.outcome()
    );
    assert_eq!(
        refused.requested_url().as_str(),
        "http://public.example/",
        "the credential is removed from the recorded request"
    );
    let evidence = serde_json::to_string(refused.hops()).unwrap();
    assert!(
        !evidence.contains("secret") && !format!("{refused:?}").contains("secret"),
        "no hop record carries the credential: {evidence}"
    );
    assert!(h.resolver.calls().is_empty(), "no lookup for a refused URL");
    assert!(h.connector.attempts().is_empty(), "nothing dialed for it");

    let transfer = h.get("http://8.8.8.8/").await;
    assert!(
        matches!(failure(&transfer), AcquisitionFailure::UnsafeTarget { .. }),
        "a redirect target with userinfo is refused: {:?}",
        transfer.outcome()
    );
    assert_eq!(
        transfer.hops().len(),
        1,
        "the refused target never became a hop"
    );
    assert_eq!(
        h.connector.attempts(),
        [addr("8.8.8.8:80")],
        "9.9.9.9 never dialed"
    );
}

#[tokio::test]
async fn unusable_domain_list_is_rejected_before_resolution() {
    let h = Harness::new(
        limits(SchemePolicy::HttpAndHttps, 0),
        ScriptedResolver::new().answer("public.example", &["8.8.8.8"]),
    );
    let constraints = free().with_denylist(vec![".".to_owned()]);

    let err = h
        .acquirer
        .acquire(
            &Url::parse("http://public.example/").unwrap(),
            &constraints,
            None,
        )
        .await
        .unwrap_err();

    assert!(
        matches!(err, Error::InvalidConstraint { .. }),
        "an entry that names no host fails closed: {err:?}"
    );
    assert!(h.resolver.calls().is_empty(), "no DNS lookup happened");
    assert!(h.connector.attempts().is_empty(), "nothing was dialed");
}

#[tokio::test]
async fn acquire_future_is_send() {
    fn assert_send<T: Send>(_: &T) {}
    fn assert_shareable<T: Send + Sync>() {}

    assert_shareable::<StaticAcquirer>();
    let h = Harness::new(
        limits(SchemePolicy::HttpAndHttps, 0),
        ScriptedResolver::new(),
    );
    let url = Url::parse("http://8.8.8.8/").unwrap();
    let constraints = free();
    let future = h.acquirer.acquire(&url, &constraints, None);
    assert_send(&future);
    drop(future);
    assert!(
        h.connector.attempts().is_empty(),
        "an unpolled future does nothing"
    );
}
