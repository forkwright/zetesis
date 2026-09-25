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
use std::io::Write as _;
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
    Acquisition, AcquisitionFailure, AcquisitionLimits, BodyRecord, BudgetConstraint,
    CharsetSource, ConnectAttempt, ConnectOutcome, ContentCoding, DowngradePolicy, Error,
    ErrorClass, ExtractionRecord, Outcome, PartialReason, ResponseRecord, SchemePolicy,
    SearchConstraints, StaticAcquirer, TrustAnchors,
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

    async fn get(&self, url: &str) -> Acquisition {
        self.acquirer
            .acquire(&Url::parse(url).unwrap(), &free(), None)
            .await
            .unwrap()
    }
}

fn failure(transfer: &Acquisition) -> &AcquisitionFailure {
    transfer.envelope().failure().unwrap_or_else(|| {
        panic!(
            "expected a failure, got {:?}",
            transfer.envelope().outcome()
        )
    })
}

fn attempt(address: &str, result: ConnectOutcome) -> ConnectAttempt {
    serde_json::from_value(serde_json::json!({ "addr": address, "result": result })).unwrap()
}

/// Whether `listener` has a connection waiting. Checked after `acquire`
/// returned, so any connection the acquirer made is already queued.
fn has_pending_connection(listener: &TcpListener) -> bool {
    listener.accept().now_or_never().is_some()
}

/// Lowercase hex SHA-256 of `bytes`, computed with `ring` directly rather
/// than through the crate under test.
fn sha256(bytes: &[u8]) -> String {
    let digest = ring::digest::digest(&ring::digest::SHA256, bytes);
    digest.as_ref().iter().fold(String::new(), |mut out, byte| {
        let _ = write!(out, "{byte:02x}");
        out
    })
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

    assert_eq!(
        transfer.envelope().outcome(),
        &Outcome::Complete,
        "anonymous GET must complete"
    );
    let response = transfer.envelope().response().unwrap();
    assert_eq!(response.status(), 200, "origin status is recorded");
    assert_eq!(transfer.body(), b"hello", "body is returned verbatim");
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
        transfer.envelope().final_url().map(Url::as_str),
        Some("http://public.example/doc?x=1#frag"),
        "the final URL is the single hop's URL"
    );
    let [hop] = transfer.envelope().hops() else {
        panic!("one hop expected: {:?}", transfer.envelope().hops());
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
        Some("gzip, deflate"),
        "exactly the codings the decoder removes are requested"
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

    assert_eq!(transfer.body(), b"secure".as_slice(), "TLS body");
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
    let tls = transfer.envelope().hops()[0].tls().unwrap();
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
    assert_eq!(
        tls.peer_leaf_sha256(),
        sha256(&identity.leaf_der),
        "leaf fingerprint recorded"
    );
}

#[tokio::test]
async fn redirect_status_without_location_is_final() {
    let origin = Origin::http(vec![(
        "/",
        raw(b"HTTP/1.1 302 Found\r\nContent-Type: text/plain\r\nContent-Length: 2\r\nConnection: close\r\n\r\nno"),
    )])
    .await;
    let h = Harness::new(
        limits(SchemePolicy::HttpAndHttps, 3),
        ScriptedResolver::new(),
    );
    h.connector.route("8.8.8.8:80", Route::Tcp(origin.local()));

    let transfer = h.get("http://8.8.8.8/").await;

    assert_eq!(
        transfer.envelope().response().map(ResponseRecord::status),
        Some(302),
        "a 3xx without Location is the final response"
    );
    assert_eq!(transfer.body(), b"no".as_slice(), "its body is kept");
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
        transfer.envelope().outcome()
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
    let [first, refused] = transfer.envelope().hops() else {
        panic!("two hops expected: {:?}", transfer.envelope().hops());
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
            transfer.envelope().outcome()
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
            transfer.envelope().outcome()
        );
        assert!(
            matches!(
                transfer.envelope().hops()[0].url().host(),
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
        transfer.envelope().outcome()
    );
    assert_eq!(
        transfer.envelope().hops()[1].url().as_str(),
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
        transfer.envelope().outcome()
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

    assert_eq!(transfer.body(), b"moved".as_slice(), "second hop served");
    for hop in transfer.envelope().hops() {
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
    assert_eq!(
        transfer.envelope().hops().len(),
        3,
        "no hop for the refused target"
    );
    assert_eq!(
        transfer.envelope().hops()[2].location(),
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
        b"plaintext".as_slice(),
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
            transfer.envelope().outcome()
        );
        assert_eq!(
            transfer.envelope().hops().len(),
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
        transfer.envelope().hops()[0].connect_attempts(),
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
    let [hop] = transfer.envelope().hops() else {
        panic!("one hop expected: {:?}", transfer.envelope().hops());
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
                head: b"HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: 104857600\r\n\r\n"
                    .to_vec(),
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
    for (header, refused) in [
        ("br", "br"),
        ("gzip, gzip", "gzip, gzip"),
        ("gzip\r\nContent-Encoding: deflate", "gzip, deflate"),
    ] {
        let h = Harness::new(
            limits(SchemePolicy::HttpAndHttps, 0),
            ScriptedResolver::new(),
        );
        let head = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Encoding: {header}\r\n\
             Content-Length: 3\r\n\r\nabc"
        );
        h.connector
            .route("8.8.8.8:80", duplex(vec![("/", raw(head.as_bytes()))]));

        let transfer = h.get("http://8.8.8.8/").await;

        assert_eq!(
            failure(&transfer),
            &AcquisitionFailure::UnsupportedContentEncoding {
                coding: refused.to_owned()
            },
            "{header:?}: a coding outside gzip/deflate, or stacked codings, is refused"
        );
        assert!(
            transfer.body().is_empty() && transfer.envelope().body().is_none(),
            "{header:?}: no body is kept"
        );
    }
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
        transfer.envelope().hops()[0].connect_attempts(),
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
        refused.envelope().outcome()
    );
    assert_eq!(
        refused.envelope().requested_url().as_str(),
        "http://public.example/",
        "the credential is removed from the recorded request"
    );
    let evidence = serde_json::to_string(refused.envelope().hops()).unwrap();
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
        transfer.envelope().outcome()
    );
    assert_eq!(
        transfer.envelope().hops().len(),
        1,
        "the refused target never became a hop"
    );
    assert_eq!(
        h.connector.attempts(),
        [addr("8.8.8.8:80")],
        "9.9.9.9 never dialed"
    );
    let evidence = serde_json::to_string(transfer.envelope()).unwrap();
    assert!(
        !evidence.contains("origin-secret"),
        "the redirect's credential is not recorded either: {evidence}"
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
async fn domain_lists_refuse_a_host_before_any_lookup() {
    // WHY: the rules match the URL host, never an address, so a denied or
    // unlisted host is refused without its name reaching the resolver.
    for (constraints, case) in [
        (
            free().with_denylist(vec!["denied.example".to_owned()]),
            "denylisted",
        ),
        (
            free().with_allowlist(vec!["other.example".to_owned()]),
            "not allowlisted",
        ),
    ] {
        let h = Harness::new(
            limits(SchemePolicy::HttpAndHttps, 0),
            ScriptedResolver::new().answer("denied.example", &["8.8.8.8"]),
        );

        let acquisition = h
            .acquirer
            .acquire(
                &Url::parse("http://denied.example/").unwrap(),
                &constraints,
                None,
            )
            .await
            .unwrap();

        assert!(
            matches!(
                failure(&acquisition),
                AcquisitionFailure::UnsafeTarget { .. }
            ),
            "{case}: refused by the target policy: {:?}",
            acquisition.envelope().outcome()
        );
        assert!(
            h.resolver.calls().is_empty(),
            "{case}: no DNS lookup for a refused host: {:?}",
            h.resolver.calls()
        );
        assert!(h.connector.attempts().is_empty(), "{case}: nothing dialed");
    }
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

// -- Phase 01 S2: bounded transfer and content evidence. --

/// The URL every fixture in this section is served at.
const ORIGIN: &str = "http://8.8.8.8/";

/// A harness whose origin at 8.8.8.8:80 answers `/` with `reply` over an
/// in-memory pipe, so `bytes_read` counts exactly what the client took.
fn serving(limits: AcquisitionLimits, reply: Reply) -> Harness {
    let h = Harness::new(limits, ScriptedResolver::new());
    h.connector.route("8.8.8.8:80", duplex(vec![("/", reply)]));
    h
}

/// `head` (through its blank line) and then `body`, written verbatim before
/// the origin closes.
fn head_then(head: &str, body: &[u8]) -> Reply {
    let mut bytes = head.as_bytes().to_vec();
    bytes.extend_from_slice(body);
    Reply::Raw(bytes)
}

/// A `200 OK` with `headers` (each ending in CRLF), an exact
/// `Content-Length`, and `body`.
fn ok_body(headers: &str, body: &[u8]) -> Reply {
    let head = format!(
        "HTTP/1.1 200 OK\r\n{headers}Content-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    head_then(&head, body)
}

/// A close-delimited `head` followed by filler until the client goes away:
/// a client that read the body would run on to the wire limit.
fn endless_after(head: &str) -> Reply {
    Reply::Endless {
        head: head.as_bytes().to_vec(),
        written: Arc::new(AtomicU64::new(0)),
    }
}

/// The response-head buffer `tight_limits` applies. hyper never holds more
/// than this before the body is consumed, so a transfer refused at the head
/// reads at most this many bytes, however much body the origin offers.
fn header_bytes() -> u64 {
    u64::try_from(AcquisitionLimits::MIN_HEADER_BYTES).unwrap()
}

fn gzip(data: &[u8], level: flate2::Compression) -> Vec<u8> {
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), level);
    encoder.write_all(data).unwrap();
    encoder.finish().unwrap()
}

/// `data` in the zlib format, which is what HTTP `deflate` names.
fn zlib(data: &[u8]) -> Vec<u8> {
    let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(data).unwrap();
    encoder.finish().unwrap()
}

fn body_record(transfer: &Acquisition) -> &BodyRecord {
    transfer.envelope().body().unwrap_or_else(|| {
        panic!(
            "expected a body record: {:?}",
            transfer.envelope().outcome()
        )
    })
}

fn extraction(transfer: &Acquisition) -> &ExtractionRecord {
    transfer.envelope().extraction().unwrap_or_else(|| {
        panic!(
            "expected an extraction: {:?}",
            transfer.envelope().outcome()
        )
    })
}

fn partial(transfer: &Acquisition) -> &PartialReason {
    let Outcome::Partial { reason } = transfer.envelope().outcome() else {
        panic!(
            "expected a partial outcome, got {:?}",
            transfer.envelope().outcome()
        );
    };
    reason
}

/// Segments as `(start, end, text)`, for comparison with literals.
fn spans(record: &ExtractionRecord) -> Vec<(usize, usize, &str)> {
    record
        .segments
        .iter()
        .map(|segment| (segment.start, segment.end, segment.text.as_str()))
        .collect()
}

/// A stopped transfer keeps no body: no record, no bytes, no extraction.
fn assert_no_body(transfer: &Acquisition, case: &str) {
    assert!(
        transfer.envelope().body().is_none(),
        "{case}: no body record"
    );
    assert!(transfer.body().is_empty(), "{case}: no body bytes");
    assert!(
        transfer.envelope().extraction().is_none(),
        "{case}: no extraction"
    );
}

/// 256 of these lines are 5120 bytes of text that compresses well.
const LINE: &str = "compressed evidence\n";

#[tokio::test]
async fn gzip_body_under_wire_limit_but_over_decoded_limit_keeps_no_body() {
    let wire = gzip(LINE.repeat(256).as_bytes(), flate2::Compression::default());
    assert!(
        u64::try_from(wire.len()).unwrap() < BODY,
        "precondition: {} wire bytes fit the wire limit",
        wire.len()
    );
    let reply = ok_body(
        "Content-Type: text/plain\r\nContent-Encoding: gzip\r\n",
        &wire,
    );
    let decoded_limit = |max| {
        limits(SchemePolicy::HttpAndHttps, 0)
            .with_max_decoded_bytes(max)
            .unwrap()
    };

    let h = serving(decoded_limit(5_119), reply.clone());
    let transfer = h.get(ORIGIN).await;

    assert_eq!(
        failure(&transfer),
        &AcquisitionFailure::DecodedLimit {
            max_decoded_bytes: 5_119
        },
        "5120 decoded bytes cross a 5119-byte ceiling"
    );
    assert_no_body(&transfer, "decoded limit");
    assert_eq!(
        transfer.envelope().response().unwrap().content_encoding(),
        ["gzip"],
        "the accepted head is recorded"
    );
    assert_eq!(
        transfer.envelope().final_url().map(Url::as_str),
        Some(ORIGIN),
        "the final URL is the hop whose head was accepted"
    );

    let h = serving(decoded_limit(5_120), reply);
    let transfer = h.get(ORIGIN).await;

    assert_eq!(
        transfer.envelope().outcome(),
        &Outcome::Complete,
        "a body that exactly fills the decoded ceiling is accepted"
    );
    assert_eq!(
        body_record(&transfer).decoded_bytes,
        5_120,
        "all of it is kept"
    );
}

#[tokio::test]
async fn decompression_bomb_stops_reading_at_the_decoded_ceiling() {
    let bomb = gzip(&vec![0_u8; 8 * 1024 * 1024], flate2::Compression::default());
    let wire = u64::try_from(bomb.len()).unwrap();
    assert!(
        wire < 16 * 1024,
        "precondition: 8 MiB of zeros is only {wire} bytes on the wire"
    );
    // NOTE: `Reply::Endless` writes `head` verbatim and then filler until the
    // client goes away. Carrying the bomb in it gives the body an unbounded
    // tail, so a client that kept reading would run on to the wire limit.
    let mut head = b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Encoding: gzip\r\n\
                     Connection: close\r\n\r\n"
        .to_vec();
    head.extend_from_slice(&bomb);
    let written = Arc::new(AtomicU64::new(0));
    let h = serving(
        tight_limits().with_max_decoded_bytes(64 * 1024).unwrap(),
        Reply::Endless {
            head,
            written: Arc::clone(&written),
        },
    );
    let started = std::time::Instant::now();

    let transfer = h.get(ORIGIN).await;

    let elapsed = started.elapsed();
    assert_eq!(
        failure(&transfer),
        &AcquisitionFailure::DecodedLimit {
            max_decoded_bytes: 65_536
        },
        "the bomb stops at the decoded ceiling, not the wire limit"
    );
    assert_no_body(&transfer, "bomb");
    let read = h.connector.bytes_read();
    assert!(
        read <= wire + header_bytes(),
        "reading stopped within the bomb: read {read} bytes for a {wire}-byte bomb"
    );
    assert!(
        written.load(Ordering::SeqCst) <= read + 2 * header_bytes(),
        "the origin could not push much of the tail past what was read"
    );
    // WHY: a generous wall-clock bound; stopping at 64 KiB takes
    // milliseconds, and the bound only catches a decoder that ran on.
    assert!(
        elapsed < Duration::from_secs(5),
        "the bomb was refused quickly: {elapsed:?}"
    );
}

#[tokio::test]
async fn compressed_body_over_wire_limit_is_wire_limit() {
    // WHY: stored (uncompressed) deflate blocks keep the wire size above the
    // text size, so only the wire ceiling can trip.
    let wire = gzip(
        LINE.repeat(16 * 1024).as_bytes(),
        flate2::Compression::none(),
    );
    assert!(
        u64::try_from(wire.len()).unwrap() > 4 * BODY,
        "precondition: {} wire bytes are far over the wire limit",
        wire.len()
    );
    let coded = "Content-Type: text/plain\r\nContent-Encoding: gzip\r\n";

    let streamed = serving(
        tight_limits(),
        head_then(
            &format!("HTTP/1.1 200 OK\r\n{coded}Connection: close\r\n\r\n"),
            &wire,
        ),
    );
    let transfer = streamed.get(ORIGIN).await;
    assert_eq!(
        failure(&transfer),
        &AcquisitionFailure::WireLimit {
            max_body_bytes: BODY
        },
        "a close-delimited compressed body stops at the wire limit"
    );
    assert_no_body(&transfer, "streamed");
    let read = streamed.connector.bytes_read();
    assert!(
        read <= BODY + 2 * header_bytes(),
        "reading stopped at the limit: read {read} bytes of {}",
        wire.len()
    );

    let declared = serving(tight_limits(), ok_body(coded, &wire));
    let transfer = declared.get(ORIGIN).await;
    assert_eq!(
        failure(&transfer),
        &AcquisitionFailure::WireLimit {
            max_body_bytes: BODY
        },
        "a declared compressed length over the limit is refused"
    );
    assert_no_body(&transfer, "declared");
    assert!(
        declared.connector.bytes_read() <= header_bytes(),
        "the declared body was not read: {} bytes",
        declared.connector.bytes_read()
    );
}

/// A page served under a content coding: 3 + 19 + 4 = 26 bytes, whose text
/// `Compressed evidence` spans bytes 3..22.
const CODED_PAGE: &str = "<p>Compressed evidence</p>";

/// Assert the evidence for [`CODED_PAGE`] sent as `wire` under `coding`.
fn assert_coded_page(transfer: &Acquisition, coding: ContentCoding, wire: &[u8]) {
    assert_eq!(
        transfer.envelope().outcome(),
        &Outcome::Complete,
        "{coding:?} page completes"
    );
    assert_eq!(
        transfer.body(),
        CODED_PAGE.as_bytes(),
        "{coding:?}: the decoded bytes are the plain page"
    );
    let body = body_record(transfer);
    assert_eq!(body.coding, coding, "the removed coding is recorded");
    assert_eq!(
        body.wire_bytes,
        u64::try_from(wire.len()).unwrap(),
        "{coding:?}: wire count is the coded bytes sent"
    );
    assert_eq!(
        body.wire_sha256,
        sha256(wire),
        "{coding:?}: wire digest is over the coded bytes sent"
    );
    assert_eq!(body.decoded_bytes, 26, "{coding:?}: decoded count");
    assert_eq!(
        body.decoded_sha256,
        sha256(b"<p>Compressed evidence</p>"),
        "{coding:?}: decoded digest is over the plain page"
    );
    let text = extraction(transfer);
    assert_eq!(
        spans(text),
        [(3, 22, "Compressed evidence")],
        "{coding:?}: text and span index the decoded bytes"
    );
    assert_eq!(
        text.text_sha256,
        sha256(b"Compressed evidence"),
        "{coding:?}: text digest"
    );
}

#[tokio::test]
async fn gzip_body_records_coding_and_exact_digests() {
    let wire = gzip(CODED_PAGE.as_bytes(), flate2::Compression::default());
    let h = serving(
        limits(SchemePolicy::HttpAndHttps, 0),
        ok_body(
            "Content-Type: text/html\r\nContent-Encoding: gzip\r\n",
            &wire,
        ),
    );

    let transfer = h.get(ORIGIN).await;

    assert_coded_page(&transfer, ContentCoding::Gzip, &wire);
}

#[tokio::test]
async fn deflate_body_is_zlib_and_records_coding_and_exact_digests() {
    let wire = zlib(CODED_PAGE.as_bytes());
    let h = serving(
        limits(SchemePolicy::HttpAndHttps, 0),
        ok_body(
            "Content-Type: text/html\r\nContent-Encoding: deflate\r\n",
            &wire,
        ),
    );

    let transfer = h.get(ORIGIN).await;

    assert_coded_page(&transfer, ContentCoding::Deflate, &wire);
}

#[tokio::test]
async fn x_gzip_is_accepted_as_gzip() {
    let wire = gzip(CODED_PAGE.as_bytes(), flate2::Compression::default());
    let h = serving(
        limits(SchemePolicy::HttpAndHttps, 0),
        ok_body(
            "Content-Type: text/html\r\nContent-Encoding: x-gzip\r\n",
            &wire,
        ),
    );

    let transfer = h.get(ORIGIN).await;

    assert_coded_page(&transfer, ContentCoding::Gzip, &wire);
    assert_eq!(
        transfer.envelope().response().unwrap().content_encoding(),
        ["x-gzip"],
        "the head records the coding as received"
    );
}

#[tokio::test]
async fn content_length_understating_the_body_keeps_only_declared_bytes() {
    let h = serving(
        limits(SchemePolicy::HttpAndHttps, 0),
        head_then(
            "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 5\r\n\
             Connection: close\r\n\r\n",
            b"helloSMUGGLED-TAIL",
        ),
    );

    let transfer = h.get(ORIGIN).await;

    assert_eq!(
        transfer.envelope().outcome(),
        &Outcome::Complete,
        "the declared body completes"
    );
    assert_eq!(
        transfer.body(),
        b"hello",
        "only the declared bytes are body"
    );
    let body = body_record(&transfer);
    assert_eq!(
        (body.wire_bytes, body.decoded_bytes),
        (5, 5),
        "both counts stop at the declared length"
    );
    assert_eq!(body.wire_sha256, sha256(b"hello"), "wire digest");
    assert_eq!(body.decoded_sha256, sha256(b"hello"), "decoded digest");
    assert_eq!(
        spans(extraction(&transfer)),
        [(0, 5, "hello")],
        "text comes from the declared bytes"
    );
    let evidence = serde_json::to_string(transfer.envelope()).unwrap();
    assert!(
        !evidence.contains("SMUGGLED"),
        "bytes past the declared length never enter the evidence: {evidence}"
    );
}

#[tokio::test]
async fn content_length_overstating_the_body_is_interrupted_stream() {
    let h = serving(
        limits(SchemePolicy::HttpAndHttps, 0),
        head_then(
            "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 100\r\n\
             Connection: close\r\n\r\n",
            b"only ten b",
        ),
    );

    let transfer = h.get(ORIGIN).await;

    let stopped = failure(&transfer);
    assert!(
        matches!(stopped, AcquisitionFailure::InterruptedStream { .. }),
        "a close before the declared length is an interrupted stream: {stopped:?}"
    );
    assert_eq!(stopped.class(), ErrorClass::Transient, "retryable");
    assert_no_body(&transfer, "short body");
    let response = transfer.envelope().response().unwrap();
    assert_eq!(
        (response.status(), response.content_length()),
        (200, Some(100)),
        "the accepted head is kept"
    );
    assert_eq!(
        transfer.envelope().final_url().map(Url::as_str),
        Some(ORIGIN),
        "the final URL is the hop whose head was accepted"
    );
}

#[tokio::test]
async fn chunked_body_cut_mid_chunk_is_interrupted_stream() {
    // NOTE: A complete 5-byte chunk, then a 10-byte chunk cut after 5 bytes.
    let h = serving(
        limits(SchemePolicy::HttpAndHttps, 0),
        head_then(
            "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nTransfer-Encoding: chunked\r\n\
             Connection: close\r\n\r\n",
            b"5\r\nhello\r\na\r\n01234",
        ),
    );

    let transfer = h.get(ORIGIN).await;

    let stopped = failure(&transfer);
    assert!(
        matches!(stopped, AcquisitionFailure::InterruptedStream { .. }),
        "a chunk cut short is an interrupted stream: {stopped:?}"
    );
    assert_eq!(
        transfer.envelope().outcome().failure_class(),
        Some(ErrorClass::Transient),
        "an interrupted stream is transient"
    );
    assert_no_body(&transfer, "cut chunk");
    let [hop] = transfer.envelope().hops() else {
        panic!("one hop expected: {:?}", transfer.envelope().hops());
    };
    assert_eq!(
        hop.connect_attempts(),
        [attempt("8.8.8.8:80", ConnectOutcome::Connected)],
        "the connection is recorded"
    );
    assert_eq!(hop.status(), Some(200), "the hop status is recorded");
    assert_eq!(
        transfer.envelope().response().map(ResponseRecord::status),
        Some(200),
        "the response head is recorded"
    );
}

#[tokio::test]
async fn unsupported_header_charset_is_partial_with_the_body_kept() {
    // NOTE: "日本" in Shift_JIS: the label is refused before the bytes are judged.
    let page = b"<p>\x93\xfa\x96\x7b</p>";
    let h = serving(
        limits(SchemePolicy::HttpAndHttps, 0),
        ok_body("Content-Type: text/html; charset=shift_jis\r\n", page),
    );

    let transfer = h.get(ORIGIN).await;

    assert_eq!(
        partial(&transfer),
        &PartialReason::UnsupportedCharset {
            label: "shift_jis".to_owned()
        },
        "a label outside the UTF-8 subset is reported, not decoded"
    );
    assert!(
        transfer.envelope().extraction().is_none(),
        "no text is extracted"
    );
    let body = body_record(&transfer);
    assert_eq!(body.decoded_bytes, 11, "the body is kept whole");
    assert_eq!(body.decoded_sha256, sha256(page), "body digest");
    assert_eq!(transfer.body(), page, "the body bytes are returned");
}

#[tokio::test]
async fn invalid_utf8_is_partial_at_the_first_invalid_offset() {
    // NOTE: `<p>` 0..3, "na" 3..5, "ï" 5..7, "ve " 7..10, then 0xFF at 10.
    let page = b"<p>na\xC3\xAFve \xFF</p>";
    let h = serving(
        limits(SchemePolicy::HttpAndHttps, 0),
        ok_body("Content-Type: text/html; charset=utf-8\r\n", page),
    );

    let transfer = h.get(ORIGIN).await;

    assert_eq!(
        partial(&transfer),
        &PartialReason::InvalidEncoding { valid_up_to: 10 },
        "the first invalid byte is located"
    );
    assert!(
        transfer.envelope().extraction().is_none(),
        "no lossy text is extracted"
    );
    let body = body_record(&transfer);
    assert_eq!(body.decoded_bytes, 15, "the body is kept whole");
    assert_eq!(body.decoded_sha256, sha256(page), "body digest");
}

#[tokio::test]
async fn meta_windows_1252_on_an_ascii_page_is_complete() {
    // NOTE: `<meta charset="windows-1252">` 0..29, `<p>` 29..32, text 32..43.
    let page = b"<meta charset=\"windows-1252\"><p>Plain ASCII</p>";
    let h = serving(
        limits(SchemePolicy::HttpAndHttps, 0),
        ok_body("Content-Type: text/html\r\n", page),
    );

    let transfer = h.get(ORIGIN).await;

    assert_eq!(
        transfer.envelope().outcome(),
        &Outcome::Complete,
        "ASCII bytes decode the same in windows-1252 and UTF-8"
    );
    let text = extraction(&transfer);
    assert_eq!(
        (text.charset.label.as_str(), text.charset.source),
        ("windows-1252", CharsetSource::Meta),
        "the meta declaration is the recorded decision"
    );
    assert_eq!(spans(text), [(32, 43, "Plain ASCII")], "text and span");
}

#[tokio::test]
async fn pdf_content_type_is_refused_before_the_body() {
    let h = serving(
        tight_limits(),
        endless_after(
            "HTTP/1.1 200 OK\r\nContent-Type: application/pdf\r\nConnection: close\r\n\r\n",
        ),
    );

    let transfer = h.get(ORIGIN).await;

    assert_eq!(
        failure(&transfer),
        &AcquisitionFailure::UnsupportedContentType {
            media_type: Some("application/pdf".to_owned())
        },
        "PDF is not a media type the acquirer extracts"
    );
    assert!(
        h.connector.bytes_read() <= header_bytes(),
        "the endless body was not read: {} bytes",
        h.connector.bytes_read()
    );
    assert_no_body(&transfer, "pdf");
    assert_eq!(
        transfer.envelope().response().unwrap().content_type(),
        Some("application/pdf"),
        "the refused head is recorded"
    );
}

#[tokio::test]
async fn missing_content_type_on_a_non_empty_body_is_refused_before_the_body() {
    let h = serving(
        tight_limits(),
        endless_after("HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n"),
    );

    let transfer = h.get(ORIGIN).await;

    assert_eq!(
        failure(&transfer),
        &AcquisitionFailure::UnsupportedContentType { media_type: None },
        "a body without a media type is refused"
    );
    assert!(
        h.connector.bytes_read() <= header_bytes(),
        "the endless body was not read: {} bytes",
        h.connector.bytes_read()
    );
    assert_no_body(&transfer, "no content type");
}

#[tokio::test]
async fn no_content_without_content_type_is_empty_body() {
    let h = serving(
        limits(SchemePolicy::HttpAndHttps, 0),
        head_then("HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n", b""),
    );

    let transfer = h.get(ORIGIN).await;

    assert_eq!(
        partial(&transfer),
        &PartialReason::EmptyBody,
        "a 204 needs no media type and has no text"
    );
    let body = body_record(&transfer);
    assert_eq!(body.coding, ContentCoding::Identity, "nothing to decode");
    assert_eq!(
        (body.wire_bytes, body.decoded_bytes),
        (0, 0),
        "an empty body is recorded"
    );
    assert_eq!(body.decoded_sha256, sha256(b""), "digest of nothing");
    assert!(transfer.envelope().extraction().is_none(), "no extraction");
    assert_eq!(
        transfer.envelope().response().map(ResponseRecord::status),
        Some(204),
        "status recorded"
    );
}

#[tokio::test]
async fn nul_byte_inside_the_sniff_window_is_binary_content() {
    // NOTE: The NUL is byte 1444, the last of the 1445 sniffed bytes.
    let mut inside = vec![b'a'; 1_444];
    inside.push(0);
    let h = serving(
        limits(SchemePolicy::HttpAndHttps, 0),
        ok_body("Content-Type: text/plain\r\n", &inside),
    );

    let transfer = h.get(ORIGIN).await;

    assert_eq!(
        partial(&transfer),
        &PartialReason::BinaryContent,
        "a binary data byte in the sniff window"
    );
    assert!(
        transfer.envelope().extraction().is_none(),
        "no text is extracted"
    );
    assert_eq!(
        body_record(&transfer).decoded_bytes,
        1_445,
        "the body is kept"
    );

    // NOTE: One byte later, the NUL is outside the window.
    let mut outside = vec![b'a'; 1_445];
    outside.push(0);
    let h = serving(
        limits(SchemePolicy::HttpAndHttps, 0),
        ok_body("Content-Type: text/plain\r\n", &outside),
    );

    let transfer = h.get(ORIGIN).await;

    assert_eq!(
        transfer.envelope().outcome(),
        &Outcome::Complete,
        "only the first 1445 bytes are sniffed"
    );
}

#[tokio::test]
async fn zero_content_length_html_is_empty_body() {
    let h = serving(
        limits(SchemePolicy::HttpAndHttps, 0),
        ok_body("Content-Type: text/html\r\n", b""),
    );

    let transfer = h.get(ORIGIN).await;

    assert_eq!(partial(&transfer), &PartialReason::EmptyBody, "no bytes");
    let body = body_record(&transfer);
    assert_eq!(
        (body.wire_bytes, body.decoded_bytes),
        (0, 0),
        "an empty body is recorded"
    );
    assert!(transfer.envelope().extraction().is_none(), "no extraction");
    assert_eq!(
        transfer.envelope().response().unwrap().content_length(),
        Some(0),
        "declared length recorded"
    );
}

#[tokio::test]
async fn script_only_page_is_no_static_text() {
    let page = b"<html><body><script>render()</script></body></html>";
    let h = serving(
        limits(SchemePolicy::HttpAndHttps, 0),
        ok_body("Content-Type: text/html\r\n", page),
    );

    let transfer = h.get(ORIGIN).await;

    assert_eq!(
        partial(&transfer),
        &PartialReason::NoStaticText,
        "scripts and nothing else"
    );
    let text = extraction(&transfer);
    assert!(text.segments.is_empty(), "no segments: {:?}", text.segments);
    assert_eq!(text.text_bytes, 0, "no text");
    assert_eq!(text.text_sha256, sha256(b""), "digest of no text");
    assert!(!text.truncated, "not a text-limit stop");
    assert_eq!(transfer.body(), page, "the body is kept");
}

#[tokio::test]
async fn text_ceiling_truncates_extraction() {
    // NOTE: Budget 8: "alpha" (5), one space (6), then "be" of "beta" (8).
    let h = serving(
        limits(SchemePolicy::HttpAndHttps, 0)
            .with_max_text_bytes(8)
            .unwrap(),
        ok_body("Content-Type: text/plain\r\n", b"alpha beta gamma"),
    );

    let transfer = h.get(ORIGIN).await;

    assert_eq!(
        partial(&transfer),
        &PartialReason::TextLimitReached,
        "the text ceiling stopped extraction"
    );
    let text = extraction(&transfer);
    assert!(text.truncated, "the extraction says it was cut");
    assert!(
        text.text_bytes <= 8,
        "{} bytes over the cap",
        text.text_bytes
    );
    assert_eq!(spans(text), [(0, 8, "alpha be")], "text stops at the cap");
    assert_eq!(text.text_sha256, sha256(b"alpha be"), "text digest");
    assert_eq!(
        body_record(&transfer).decoded_bytes,
        16,
        "the body is kept whole"
    );
}

#[tokio::test]
async fn extracted_spans_index_the_decoded_body() {
    // NOTE: `<html><body><h1>` 0..16, "Report" 16..22, `</h1><p>` 22..30,
    // "Alpha &amp; <b>beta" 30..49, `</b></p></body></html>` 49..71.
    let page = b"<html><body><h1>Report</h1><p>Alpha &amp; <b>beta</b></p></body></html>";
    let h = serving(
        limits(SchemePolicy::HttpAndHttps, 0),
        ok_body("Content-Type: text/html\r\n", page),
    );

    let transfer = h.get(ORIGIN).await;

    assert_eq!(transfer.envelope().outcome(), &Outcome::Complete, "text");
    let text = extraction(&transfer);
    assert_eq!(
        spans(text),
        [(16, 22, "Report"), (30, 49, "Alpha & beta")],
        "block elements split segments; inline markup does not"
    );
    let sources: Vec<&[u8]> = text
        .segments
        .iter()
        .map(|segment| &transfer.body()[segment.start..segment.end])
        .collect();
    assert_eq!(
        sources,
        [b"Report".as_slice(), b"Alpha &amp; <b>beta".as_slice()],
        "each span slices the source markup of its segment"
    );
    assert_eq!(text.text_bytes, 19, "segments joined by one newline");
    assert_eq!(
        text.text_sha256,
        sha256(b"Report\nAlpha & beta"),
        "text digest"
    );
    assert_eq!(
        text.charset.source,
        CharsetSource::AssumedUtf8,
        "no declaration"
    );
}

#[tokio::test]
async fn spans_after_a_byte_order_mark_index_the_decoded_body() {
    // NOTE: BOM 0..3, `<p>` 3..6, "naïve café" 6..18 (ï and é are two bytes each).
    let page = "\u{feff}<p>naïve café</p>".as_bytes();
    let h = serving(
        limits(SchemePolicy::HttpAndHttps, 0),
        ok_body("Content-Type: text/html\r\n", page),
    );

    let transfer = h.get(ORIGIN).await;

    let text = extraction(&transfer);
    assert_eq!(
        (text.charset.label.as_str(), text.charset.source),
        ("utf-8", CharsetSource::Bom),
        "the byte-order mark decides"
    );
    assert_eq!(spans(text), [(6, 18, "naïve café")], "spans count the BOM");
    assert_eq!(
        &transfer.body()[6..18],
        "naïve café".as_bytes(),
        "the span slices the decoded body, BOM included"
    );
}

#[tokio::test]
async fn cookies_are_never_sent_or_recorded() {
    let h = Harness::new(
        limits(SchemePolicy::HttpAndHttps, 1),
        ScriptedResolver::new(),
    );
    h.connector.route(
        "8.8.8.8:80",
        duplex(vec![
            (
                "/start",
                raw(
                    b"HTTP/1.1 302 Found\r\nSet-Cookie: session=hop-cookie-secret; Path=/\r\n\
                      Location: /final\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                ),
            ),
            (
                "/final",
                ok_body(
                    "Content-Type: text/plain\r\n\
                     Set-Cookie: session=final-cookie-secret; HttpOnly\r\n\
                     Content-Location: http://user:userinfo-secret@8.8.8.8/final\r\n",
                    b"served",
                ),
            ),
        ]),
    );

    let transfer = h.get("http://8.8.8.8/start").await;

    assert_eq!(transfer.body(), b"served", "the redirect was followed");
    let requests = h.connector.duplex_requests();
    assert_eq!(requests.len(), 2, "one request per hop");
    for seen in &requests {
        assert_eq!(
            seen.header_names(),
            [
                "host",
                "user-agent",
                "accept",
                "accept-encoding",
                "connection"
            ],
            "{}: no Cookie header, even after a hop set one",
            seen.target()
        );
    }
    let evidence = serde_json::to_string(transfer.envelope()).unwrap();
    assert!(
        !evidence.contains("cookie-secret") && !evidence.to_lowercase().contains("cookie"),
        "no Set-Cookie value or header is recorded: {evidence}"
    );
    assert!(
        !evidence.contains("userinfo-secret"),
        "no userinfo is recorded: {evidence}"
    );
    for hop in transfer.envelope().hops() {
        assert!(
            hop.url().username().is_empty() && hop.url().password().is_none(),
            "no hop URL carries userinfo: {}",
            hop.url()
        );
    }
}

// -- A credential in a redirect Location never enters evidence. --

#[tokio::test]
async fn redirect_setting_a_cookie_to_a_userinfo_target_records_no_credential() {
    let h = serving(
        limits(SchemePolicy::HttpAndHttps, 1),
        raw(
            b"HTTP/1.1 302 Found\r\nSet-Cookie: session=hop-cookie-secret\r\n\
              Location: http://user:userinfo-secret@9.9.9.9/\r\nContent-Length: 0\r\n\
              Connection: close\r\n\r\n",
        ),
    );

    let transfer = h.get(ORIGIN).await;

    assert!(
        matches!(failure(&transfer), AcquisitionFailure::UnsafeTarget { .. }),
        "a userinfo redirect target is refused: {:?}",
        transfer.envelope().outcome()
    );
    assert_eq!(
        h.connector.attempts(),
        [addr("8.8.8.8:80")],
        "9.9.9.9 never dialed"
    );
    assert_eq!(
        transfer.envelope().hops()[0].location(),
        Some("http://9.9.9.9/"),
        "the refused target is recorded without its userinfo"
    );
    let evidence = serde_json::to_string(transfer.envelope()).unwrap();
    assert!(
        !evidence.contains("cookie-secret"),
        "no Set-Cookie value is recorded: {evidence}"
    );
    assert!(
        !evidence.contains("userinfo-secret"),
        "no userinfo is recorded: {evidence}"
    );
}

#[tokio::test]
async fn userinfo_in_any_location_spelling_is_never_recorded() {
    // WHY: the WHATWG parser finds userinfo in spellings a text scan would
    // miss: another special scheme without slashes (read as an authority;
    // the same scheme without slashes would be a relative path), a
    // scheme-relative reference, and a tab (removed before parsing) inside
    // the userinfo.
    for (location, recorded) in [
        ("https:user:spelling-secret@9.9.9.9/", "https://9.9.9.9/"),
        ("//user:spelling-secret@9.9.9.9/x", "http://9.9.9.9/x"),
        ("http://us\ter:spelling-secret@9.9.9.9/", "http://9.9.9.9/"),
    ] {
        let head = format!(
            "HTTP/1.1 302 Found\r\nLocation: {location}\r\nContent-Length: 0\r\n\
             Connection: close\r\n\r\n"
        );
        let h = serving(limits(SchemePolicy::HttpAndHttps, 1), raw(head.as_bytes()));

        let transfer = h.get(ORIGIN).await;

        assert!(
            matches!(failure(&transfer), AcquisitionFailure::UnsafeTarget { .. }),
            "{location:?}: the target is refused: {:?}",
            transfer.envelope().outcome()
        );
        assert_eq!(
            transfer.envelope().hops()[0].location(),
            Some(recorded),
            "{location:?}: recorded as the target without userinfo"
        );
        let evidence = serde_json::to_string(transfer.envelope()).unwrap();
        assert!(
            !evidence.contains("spelling-secret"),
            "{location:?}: no userinfo is recorded: {evidence}"
        );
    }
}

#[tokio::test]
async fn unparseable_redirect_with_userinfo_records_no_credential() {
    let h = serving(
        limits(SchemePolicy::HttpAndHttps, 1),
        raw(
            b"HTTP/1.1 302 Found\r\nLocation: http://user:userinfo-secret@[::1/\r\n\
              Content-Length: 0\r\nConnection: close\r\n\r\n",
        ),
    );

    let transfer = h.get(ORIGIN).await;

    assert!(
        matches!(
            failure(&transfer),
            AcquisitionFailure::MalformedRedirect { .. }
        ),
        "an unparseable Location is malformed: {:?}",
        transfer.envelope().outcome()
    );
    let evidence = serde_json::to_string(transfer.envelope()).unwrap();
    assert!(
        !evidence.contains("userinfo-secret"),
        "no userinfo is recorded: {evidence}"
    );
}

#[tokio::test]
async fn location_without_userinfo_is_recorded_as_received() {
    let h = serving(
        limits(SchemePolicy::HttpAndHttps, 0),
        raw(b"HTTP/1.1 302 Found\r\nLocation: /users/@alice?x=1\r\n\
              Content-Length: 0\r\nConnection: close\r\n\r\n"),
    );

    let transfer = h.get(ORIGIN).await;

    assert_eq!(
        transfer.envelope().hops()[0].location(),
        Some("/users/@alice?x=1"),
        "a Location without userinfo keeps its received text, '@' in the path included"
    );
}
