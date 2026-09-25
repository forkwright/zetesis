//! The first provider cohort wired through the static acquirer, against
//! local TLS origins that stand in for each provider's host: the request
//! each provider sends, how each answer and each transport failure maps,
//! and the evidence identity behind every answer. Nothing here leaves
//! loopback: a scripted resolver answers each provider host with a public
//! address that the recording connector routes to the local origin.

#![expect(clippy::unwrap_used, reason = "test assertions must fail loudly")]

#[expect(
    dead_code,
    reason = "the shared transport fixture serves several test crates; this one uses part of it"
)]
mod fixture;

use std::io::Write as _;
use std::sync::Arc;
use std::time::Duration;

use fixture::{
    Origin, RecordingConnector, Reply, Route, ScriptedResolver, StallSignals, raw, tls_identity,
};
use sylloge::{
    AcquisitionLimits, Arxiv, AttemptOutcome, BudgetConstraint, Error, EvidenceState, Provider,
    ProviderAnswer, QueryShape, ResearchResult, Router, SchemePolicy, SearchConstraints,
    SemanticScholar, StaticAcquirer, TrustAnchors, Wikipedia,
};
use url::{Position, Url};

const S2_DOCUMENTED: &[u8] =
    include_bytes!("fixtures/providers/semantic_scholar/search_documented_shape.json");
const ARXIV_DOCUMENTED: &[u8] =
    include_bytes!("fixtures/providers/arxiv/search_documented_shape.xml");

const S2_HOST: &str = "api.semanticscholar.org";
const ARXIV_HOST: &str = "export.arxiv.org";
const WIKIPEDIA_HOST: &str = "en.wikipedia.org";

/// The acquirer's own User-Agent.
const ACQUIRER_AGENT: &str = "zetesis-fixture/0.0 (https://example.org/contact)";
/// The User-Agent a Wikipedia caller configures, distinct from the
/// acquirer's so the test can tell which one was sent.
const WIKIPEDIA_AGENT: &str = "wiki-caller/1.0 (https://example.org/wiki-contact) sylloge/0.0";

const QUERY: &str = "stable identity";
const PUBLIC: &str = "8.8.8.8:443";

/// The exact request header names, in order: no credential, cookie, or
/// referer, and nothing a provider added beyond its documented request.
const REQUEST_HEADERS: [&str; 5] = [
    "host",
    "user-agent",
    "accept",
    "accept-encoding",
    "connection",
];

fn limits(deadline: Duration) -> AcquisitionLimits {
    AcquisitionLimits::new(
        SchemePolicy::HttpsOnly,
        0,
        Duration::from_secs(5),
        deadline,
        1024 * 1024,
    )
    .unwrap()
}

fn constraints() -> SearchConstraints {
    SearchConstraints::new(3, BudgetConstraint::free_only())
}

/// A local TLS origin standing in for `host`, and an acquirer that reaches
/// it only through the scripted resolver and recording connector.
struct Wired {
    origin: Origin,
    connector: Arc<RecordingConnector>,
    acquirer: Arc<StaticAcquirer>,
}

async fn wired(host: &str, routes: Vec<(String, Reply)>, limits: AcquisitionLimits) -> Wired {
    let identity = tls_identity(host);
    let routes: Vec<(&str, Reply)> = routes
        .iter()
        .map(|(path, reply)| (path.as_str(), reply.clone()))
        .collect();
    let origin = Origin::https(routes, Arc::clone(&identity.server)).await;
    let connector = RecordingConnector::new();
    connector.route(PUBLIC, Route::Tcp(origin.local()));
    let resolver = ScriptedResolver::new().answer(host, &["8.8.8.8"]);
    let acquirer = StaticAcquirer::builder(
        limits,
        TrustAnchors::from_der([identity.ca_der.as_slice()]).unwrap(),
    )
    .connector(Arc::clone(&connector) as _)
    .resolver(Arc::new(resolver))
    .user_agent(ACQUIRER_AGENT)
    .build()
    .unwrap();
    Wired {
        origin,
        connector,
        acquirer: Arc::new(acquirer),
    }
}

/// The origin-form request target of `url`, which the origin routes on.
fn target(url: &Url) -> String {
    url[Position::BeforePath..].to_owned()
}

/// A response with `headers` and `body`, closing the connection.
fn http(status: &str, headers: &[(&str, &str)], body: &[u8]) -> Reply {
    let mut response = Vec::new();
    write!(
        response,
        "HTTP/1.1 {status}\r\nContent-Length: {}\r\n",
        body.len()
    )
    .unwrap();
    for (name, value) in headers {
        write!(response, "{name}: {value}\r\n").unwrap();
    }
    response.extend_from_slice(b"Connection: close\r\n\r\n");
    response.extend_from_slice(body);
    raw(&response)
}

fn json(status: &str, body: &[u8]) -> Reply {
    http(status, &[("Content-Type", "application/json")], body)
}

fn s2_target() -> String {
    target(&SemanticScholar::request(QUERY, &constraints()).unwrap().url)
}

/// Semantic Scholar wired to an origin answering its search with `reply`.
async fn semantic_scholar(reply: Reply, deadline: Duration) -> (Wired, SemanticScholar) {
    let w = wired(S2_HOST, vec![(s2_target(), reply)], limits(deadline)).await;
    let provider = SemanticScholar::new(Arc::clone(&w.acquirer), Duration::ZERO);
    (w, provider)
}

async fn ask(provider: &dyn Provider) -> ProviderAnswer {
    provider.search(QUERY, &constraints()).await
}

fn titles(result: &ResearchResult) -> Vec<&str> {
    result.hits.iter().map(|hit| hit.title.as_str()).collect()
}

fn assert_one_fingerprint(answer: &ProviderAnswer) -> String {
    assert_eq!(
        answer.evidence_fingerprints.len(),
        1,
        "one acquisition, one envelope fingerprint"
    );
    let fingerprint = answer.evidence_fingerprints.first().unwrap().clone();
    let hex = fingerprint.strip_prefix("sha256:").unwrap();
    assert!(
        hex.len() == 64 && hex.bytes().all(|b| b.is_ascii_hexdigit()),
        "an evidence fingerprint is sha256 hex: {fingerprint}"
    );
    fingerprint
}

#[tokio::test]
async fn semantic_scholar_sends_its_documented_anonymous_request_and_parses_the_answer() {
    let (w, provider) =
        semantic_scholar(json("200 OK", S2_DOCUMENTED), Duration::from_secs(30)).await;
    let answer = ask(&provider).await;
    assert_one_fingerprint(&answer);
    let result = answer.result.unwrap();
    assert_eq!(
        titles(&result),
        [
            "Routing Queries Across Free Scholarly Indexes",
            "Deduplicating Preprints by Stable Identity",
            "A Workshop Paper Without a Journal",
        ],
        "the parsed answer, in relevance order"
    );

    let seen = w.origin.requests();
    assert_eq!(seen.len(), 1, "one request");
    let request = seen.first().unwrap();
    assert_eq!(
        request.target(),
        s2_target(),
        "the documented search request"
    );
    assert_eq!(
        request.sni.as_deref(),
        Some(S2_HOST),
        "TLS to the provider host"
    );
    assert_eq!(
        request.header_names(),
        REQUEST_HEADERS,
        "only the fixed headers"
    );
    assert_eq!(
        request.header("accept").as_deref(),
        Some("application/json"),
        "the provider asks for its media type"
    );
    assert_eq!(
        request.header("user-agent").as_deref(),
        Some(ACQUIRER_AGENT),
        "Semantic Scholar sends the acquirer's User-Agent"
    );
    assert_eq!(
        w.connector.attempts(),
        [fixture::addr(PUBLIC)],
        "one validated address"
    );
}

#[tokio::test]
async fn arxiv_asks_for_atom_and_parses_the_feed() {
    let request = Arxiv::request(QUERY, &constraints()).unwrap();
    let reply = http(
        "200 OK",
        &[("Content-Type", "application/atom+xml; charset=UTF-8")],
        ARXIV_DOCUMENTED,
    );
    let w = wired(
        ARXIV_HOST,
        vec![(target(&request.url), reply)],
        limits(Duration::from_secs(30)),
    )
    .await;
    let provider = Arxiv::new(Arc::clone(&w.acquirer));
    let answer = ask(&provider).await;
    assert_one_fingerprint(&answer);
    assert_eq!(
        titles(&answer.result.unwrap()),
        [
            "Deduplicating Preprints by Stable Identity",
            "An Old-Style Identifier Without a Version",
        ],
        "the parsed feed"
    );
    let seen = w.origin.requests();
    let request = seen.first().unwrap();
    assert_eq!(
        request.header("accept").as_deref(),
        Some("application/atom+xml"),
        "arXiv is asked for Atom"
    );
    assert_eq!(
        request.header_names(),
        REQUEST_HEADERS,
        "only the fixed headers"
    );
}

#[tokio::test]
async fn wikipedia_sends_the_callers_user_agent_and_decodes_excerpt_entities() {
    let body = br#"{"pages":[{"id":1000010,"key":"Q_and_A","title":"Q and A","excerpt":"&quot;<span class=\"searchmatch\">Q</span>&amp;A&quot; &lt;b&gt; caf&#233;"}]}"#;
    let placeholder = Arc::new(
        StaticAcquirer::builder(
            limits(Duration::from_secs(30)),
            TrustAnchors::from_der([tls_identity("unused.example").ca_der.as_slice()]).unwrap(),
        )
        .build()
        .unwrap(),
    );
    let request = Wikipedia::new(placeholder, WIKIPEDIA_AGENT)
        .unwrap()
        .request(QUERY, &constraints())
        .unwrap();
    let w = wired(
        WIKIPEDIA_HOST,
        vec![(target(&request.url), json("200 OK", body))],
        limits(Duration::from_secs(30)),
    )
    .await;
    let provider = Wikipedia::new(Arc::clone(&w.acquirer), WIKIPEDIA_AGENT).unwrap();
    let result = ask(&provider).await.result.unwrap();
    assert_eq!(
        result.hits.first().map(|hit| hit.snippet.as_str()),
        Some("\"Q&A\" <b> caf\u{e9}"),
        "highlights stripped, then references decoded; decoded `<b>` is text"
    );
    let seen = w.origin.requests();
    assert_eq!(
        seen.first().unwrap().header("user-agent").as_deref(),
        Some(WIKIPEDIA_AGENT),
        "the caller's contact User-Agent replaces the acquirer's"
    );
}

#[tokio::test]
async fn an_error_status_decides_even_when_its_html_body_is_refused() {
    let limited = http(
        "429 Too Many Requests",
        &[("Content-Type", "text/html"), ("Retry-After", "30")],
        b"<p>slow down</p>",
    );
    let (_w, provider) = semantic_scholar(limited, Duration::from_secs(30)).await;
    let answer = ask(&provider).await;
    assert_one_fingerprint(&answer);
    let err = answer.result.unwrap_err();
    assert!(
        matches!(
            err,
            Error::RateLimited {
                retry_after_ms: Some(30_000),
                ..
            }
        ),
        "the recorded 429 and Retry-After decide: {err:?}"
    );

    let denied = http(
        "403 Forbidden",
        &[("Content-Type", "text/html")],
        b"<p>no</p>",
    );
    let (_w, provider) = semantic_scholar(denied, Duration::from_secs(30)).await;
    let err = ask(&provider).await.result.unwrap_err();
    assert!(
        matches!(err, Error::Unauthorized { .. }) && err.is_permanent(),
        "a 403 error page is an authentication denial: {err:?}"
    );
}

#[tokio::test]
async fn a_success_in_another_media_type_fails_as_permanent() {
    let html = http("200 OK", &[("Content-Type", "text/html")], b"<p>portal</p>");
    let (_w, provider) = semantic_scholar(html, Duration::from_secs(30)).await;
    let answer = ask(&provider).await;
    assert_one_fingerprint(&answer);
    let err = answer.result.unwrap_err();
    assert!(
        err.is_permanent() && err.to_string().contains("unsupported_content_type"),
        "a 200 that is not the provider's media type is refused: {err}"
    );
    assert!(
        !err.to_string().contains("portal"),
        "the body is not quoted: {err}"
    );
}

#[tokio::test]
async fn a_corrupt_or_overexpanding_body_fails_as_permanent() {
    let corrupt = http(
        "200 OK",
        &[
            ("Content-Type", "application/json"),
            ("Content-Encoding", "gzip"),
        ],
        b"not gzip at all",
    );
    let (_w, provider) = semantic_scholar(corrupt, Duration::from_secs(30)).await;
    let err = ask(&provider).await.result.unwrap_err();
    assert!(
        err.is_permanent() && err.to_string().contains("corrupt_content_encoding"),
        "an undecodable body: {err}"
    );

    let mut gzip = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::best());
    gzip.write_all(&vec![b' '; 64 * 1024]).unwrap();
    let bomb = http(
        "200 OK",
        &[
            ("Content-Type", "application/json"),
            ("Content-Encoding", "gzip"),
        ],
        &gzip.finish().unwrap(),
    );
    let tight = limits(Duration::from_secs(30))
        .with_max_decoded_bytes(16 * 1024)
        .unwrap();
    let w = wired(S2_HOST, vec![(s2_target(), bomb)], tight).await;
    let provider = SemanticScholar::new(Arc::clone(&w.acquirer), Duration::ZERO);
    let err = ask(&provider).await.result.unwrap_err();
    assert!(
        err.is_permanent() && err.to_string().contains("decoded_limit"),
        "a body that expands past the decoded ceiling: {err}"
    );
}

#[tokio::test]
async fn a_stalled_provider_times_out_at_the_acquisition_deadline_as_transient() {
    let stall = Reply::Stall(Arc::new(StallSignals::default()));
    let (_w, provider) = semantic_scholar(stall, Duration::from_millis(300)).await;
    let answer = ask(&provider).await;
    assert_one_fingerprint(&answer);
    let err = answer.result.unwrap_err();
    assert!(
        matches!(
            err,
            Error::Timeout {
                timeout_ms: 300,
                ..
            }
        ) && err.is_transient(),
        "a provider that never answers times out at the acquisition deadline: {err:?}"
    );
}

#[tokio::test]
async fn an_unreachable_provider_fails_as_transient() {
    let fast_connect = AcquisitionLimits::new(
        SchemePolicy::HttpsOnly,
        0,
        Duration::from_millis(200),
        Duration::from_secs(30),
        1024 * 1024,
    )
    .unwrap();
    let unreachable = wired(S2_HOST, Vec::new(), fast_connect).await;
    unreachable.connector.route(PUBLIC, Route::Stall);
    let provider = SemanticScholar::new(Arc::clone(&unreachable.acquirer), Duration::ZERO);
    let err = ask(&provider).await.result.unwrap_err();
    assert!(
        err.is_transient() && err.to_string().contains("connect_timeout"),
        "a connection that never completes: {err}"
    );
}

#[tokio::test]
async fn a_provider_call_past_the_router_timeout_is_receipted_as_timed_out() {
    let stall = Reply::Stall(Arc::new(StallSignals::default()));
    let (_w, provider) = semantic_scholar(stall, Duration::from_secs(30)).await;
    let router = Router::new(vec![Arc::new(provider)], Duration::from_millis(200)).unwrap();
    let result = router
        .search(
            QUERY,
            QueryShape::SemanticDiscovery,
            &constraints(),
            jiff::Timestamp::now(),
        )
        .await
        .unwrap();
    let attempt = result.provenance.first().unwrap().attempt.clone().unwrap();
    assert_eq!(
        attempt.outcome,
        AttemptOutcome::TimedOut { timeout_ms: 200 },
        "the router's deadline cancels the acquisition and receipts it"
    );
    assert!(
        attempt.evidence_fingerprints.is_empty(),
        "a cancelled acquisition leaves no envelope"
    );
    assert_eq!(result.evidence_state(), EvidenceState::Unanswered);
}

#[tokio::test]
async fn receipts_name_the_evidence_behind_each_attempt() {
    let (w, provider) =
        semantic_scholar(json("200 OK", S2_DOCUMENTED), Duration::from_secs(30)).await;
    let direct = assert_one_fingerprint(&ask(&provider).await);
    let router = Router::new(vec![Arc::new(provider)], Duration::from_secs(30)).unwrap();
    let result = router
        .search(
            QUERY,
            QueryShape::SemanticDiscovery,
            &constraints(),
            jiff::Timestamp::now(),
        )
        .await
        .unwrap();
    let attempt = result.provenance.first().unwrap().attempt.clone().unwrap();
    assert_eq!(
        attempt.evidence_fingerprints,
        [direct],
        "the same content fetched the same way has the same evidence identity"
    );
    assert_eq!(
        w.origin.requests().len(),
        2,
        "both calls reached the provider"
    );
}

#[tokio::test]
async fn caller_domain_lists_screen_hits_not_the_providers_endpoint() {
    let (w, provider) =
        semantic_scholar(json("200 OK", S2_DOCUMENTED), Duration::from_secs(30)).await;
    let router = Router::new(vec![Arc::new(provider)], Duration::from_secs(30)).unwrap();
    let arxiv_only = constraints().with_allowlist(vec!["arxiv.org".to_owned()]);
    let result = router
        .search(
            QUERY,
            QueryShape::SemanticDiscovery,
            &arxiv_only,
            jiff::Timestamp::now(),
        )
        .await
        .unwrap();
    assert_eq!(
        w.origin.requests().len(),
        1,
        "the provider's API host is still asked"
    );
    assert!(
        matches!(
            result
                .provenance
                .first()
                .unwrap()
                .attempt
                .as_ref()
                .unwrap()
                .outcome,
            AttemptOutcome::Answered {
                returned: 3,
                rejected_by_domain: 3,
                ..
            }
        ),
        "the allow list then screens the hits"
    );
}

#[tokio::test]
async fn a_denied_provider_endpoint_is_refused_before_any_connection() {
    let (w, provider) =
        semantic_scholar(json("200 OK", S2_DOCUMENTED), Duration::from_secs(30)).await;
    let router = Router::new(vec![Arc::new(provider)], Duration::from_secs(30)).unwrap();
    let denied = constraints().with_denylist(vec!["semanticscholar.org".to_owned()]);
    let result = router
        .search(
            QUERY,
            QueryShape::SemanticDiscovery,
            &denied,
            jiff::Timestamp::now(),
        )
        .await
        .unwrap();
    let attempt = result.provenance.first().unwrap().attempt.clone().unwrap();
    assert_eq!(
        serde_json::to_value(&attempt.outcome).unwrap(),
        serde_json::json!({
            "status": "failed",
            "class": "permanent",
            "message": "permanent I/O failure: semantic_scholar: acquisition failed: unsafe_target",
        }),
        "the caller's deny list covers the provider's own host"
    );
    assert!(w.connector.attempts().is_empty(), "no connection attempted");
    assert!(w.origin.requests().is_empty(), "nothing reached the host");
}

/// Hostile bodies whose text must never reach an error or a receipt.
const HOSTILE: [(&str, &str, &[u8]); 6] = [
    (
        S2_HOST,
        "application/json",
        include_bytes!("fixtures/providers/semantic_scholar/search_hostile_type_changed.json"),
    ),
    (
        S2_HOST,
        "application/json",
        include_bytes!("fixtures/providers/semantic_scholar/search_hostile_malformed.json"),
    ),
    (
        WIKIPEDIA_HOST,
        "application/json",
        include_bytes!("fixtures/providers/wikipedia/search_hostile_type_changed.json"),
    ),
    (
        WIKIPEDIA_HOST,
        "application/json",
        include_bytes!("fixtures/providers/wikipedia/search_hostile_malformed.json"),
    ),
    (
        ARXIV_HOST,
        "application/atom+xml",
        include_bytes!("fixtures/providers/arxiv/search_hostile_mismatched_end.xml"),
    ),
    (
        ARXIV_HOST,
        "application/atom+xml",
        include_bytes!("fixtures/providers/arxiv/search_hostile_entity.xml"),
    ),
];

#[tokio::test]
async fn hostile_body_text_never_reaches_an_attempt_receipt() {
    for (host, media, body) in HOSTILE {
        let reply = http("200 OK", &[("Content-Type", media)], body);
        let (target, shape) = match host {
            S2_HOST => (s2_target(), QueryShape::SemanticDiscovery),
            ARXIV_HOST => (
                target(&Arxiv::request(QUERY, &constraints()).unwrap().url),
                QueryShape::AcademicLiterature,
            ),
            _ => (
                "/w/rest.php/v1/search/page?q=stable+identity&limit=3".to_owned(),
                QueryShape::QuickFactual,
            ),
        };
        let w = wired(host, vec![(target, reply)], limits(Duration::from_secs(30))).await;
        let provider: Arc<dyn Provider> = match host {
            S2_HOST => Arc::new(SemanticScholar::new(
                Arc::clone(&w.acquirer),
                Duration::ZERO,
            )),
            ARXIV_HOST => Arc::new(Arxiv::new(Arc::clone(&w.acquirer))),
            _ => Arc::new(Wikipedia::new(Arc::clone(&w.acquirer), WIKIPEDIA_AGENT).unwrap()),
        };
        let router = Router::new(vec![provider], Duration::from_secs(30)).unwrap();
        let result = router
            .search(QUERY, shape, &constraints(), jiff::Timestamp::now())
            .await
            .unwrap();
        let attempt = result.provenance.first().unwrap().attempt.clone().unwrap();
        let AttemptOutcome::Failed { message, .. } = &attempt.outcome else {
            panic!("{host}: a hostile body fails the attempt: {attempt:?}");
        };
        assert!(
            message.contains("malformed response")
                && !message.contains("IGNORE")
                && !message.contains("INSTRUCTIONS"),
            "{host}: the receipt names the defect, never the body text: {message}"
        );
    }
}

#[tokio::test]
async fn a_provider_returns_no_more_hits_than_it_was_asked_for() {
    let two = SearchConstraints::new(2, BudgetConstraint::free_only());
    let request = SemanticScholar::request(QUERY, &two).unwrap();
    let w = wired(
        S2_HOST,
        vec![(target(&request.url), json("200 OK", S2_DOCUMENTED))],
        limits(Duration::from_secs(30)),
    )
    .await;
    let provider = SemanticScholar::new(Arc::clone(&w.acquirer), Duration::ZERO);
    let result = provider.search(QUERY, &two).await.result.unwrap();
    assert_eq!(
        titles(&result),
        [
            "Routing Queries Across Free Scholarly Indexes",
            "Deduplicating Preprints by Stable Identity",
        ],
        "an upstream that ignores `limit=2` still yields the first two, in rank order"
    );
}
