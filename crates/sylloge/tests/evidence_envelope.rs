//! Phase 01 S2 evidence envelope fixtures: the golden envelope, schema
//! identity and version refusal, fingerprint verification, and replay.
//!
//! Every expected span, digest, and fingerprint here was computed
//! independently of this crate from the fixture bytes in
//! `tests/fixtures/evidence/`: `page.html` is the source document and
//! `page.html.gz` its gzip wire form (written with a fixed mtime so the
//! wire bytes never change). `restated_fingerprint` restates the
//! fingerprint rule from the schema as a second check on the literals.

#![expect(clippy::unwrap_used, reason = "test assertions must fail loudly")]

use std::fmt::Write as _;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use sylloge::{
    Acquisition, AcquisitionLimits, BoxFut, BudgetConstraint, CharsetSource, ConnectError,
    ConnectedStream, Connector, ContentCoding, EVIDENCE_SCHEMA_ID, EVIDENCE_SCHEMA_VERSION,
    EvidenceEnvelope, ExtractorId, Media, Outcome, ReplayOutcome, Resolver, SchemePolicy,
    SearchConstraints, StaticAcquirer, TrustAnchors, replay,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use url::Url;

const PAGE: &[u8] = include_bytes!("fixtures/evidence/page.html");
const PAGE_GZ: &[u8] = include_bytes!("fixtures/evidence/page.html.gz");
const GOLDEN: &str = include_str!("fixtures/evidence/golden_envelope.json");
const GOLDEN_FAILED: &str = include_str!("fixtures/evidence/golden_failed_envelope.json");
const RELEASE_RECORD: &str = include_str!("../../../docs/design/static-acquisition-release.md");
const ACQUISITION_TESTS: &str = include_str!("static_acquisition.rs");
const ENVELOPE_TESTS: &str = include_str!("evidence_envelope.rs");

const URL: &str = "http://public.example/tides?station=7";
const WIRE_SHA256: &str = "cb169b3b23ad8767099566b7093ab6b02af83093dd67d96064af0007091eb6ec";
const PAGE_SHA256: &str = "1226d22d2282599bdaec3a6432f2b476d22f4b6814ebff27886a57c6d1f3cc81";
const TEXT_SHA256: &str = "a9fdae16ee21cd3d1b35710df5c4bb84c92b14b627dc2d12c50d5b2093293ada";
const FINGERPRINT: &str = "sha256:eda01f2d750b7e845802f0271a556d276f862da6f96515263a602558a04d7476";
/// The golden fingerprint with the extractor version field set to `2`.
const FINGERPRINT_EXTRACTOR_V2: &str =
    "sha256:ccb7d32771222242a1ae28a920943e48817ca5dfc97f2ca97e9cb1b251e7a032";
const FAILED_FINGERPRINT: &str =
    "sha256:2711f0ecd20dc93b599845153ef1b2fad4bf5f82c58002d0b9eb4cfee2186046";

/// Answers every host with one documentation-public address.
struct PublicResolver;

impl Resolver for PublicResolver {
    fn resolve(&self, _host: &str, _port: u16) -> std::io::Result<Vec<IpAddr>> {
        Ok(vec!["8.8.8.8".parse().unwrap()])
    }
}

/// Serves one fixed response over an in-memory pipe for any address.
struct CannedConnector(Arc<Vec<u8>>);

impl Connector for CannedConnector {
    fn connect(
        &self,
        _addr: SocketAddr,
        _timeout: Duration,
    ) -> BoxFut<'_, Result<Box<dyn ConnectedStream>, ConnectError>> {
        let response = Arc::clone(&self.0);
        Box::pin(async move {
            let (client, mut server) = tokio::io::duplex(64 * 1024);
            tokio::spawn(async move {
                let mut head = Vec::new();
                let mut byte = [0_u8; 1];
                while !head.ends_with(b"\r\n\r\n") && server.read(&mut byte).await.unwrap() > 0 {
                    head.push(byte[0]);
                }
                server.write_all(&response).await.unwrap();
                server.shutdown().await.unwrap();
            });
            let stream: Box<dyn ConnectedStream> = Box::new(client);
            Ok(stream)
        })
    }
}

fn golden_limits() -> AcquisitionLimits {
    AcquisitionLimits::new(
        SchemePolicy::HttpAndHttps,
        0,
        Duration::from_secs(5),
        Duration::from_secs(30),
        64 * 1024,
    )
    .unwrap()
    .with_max_decoded_bytes(1024 * 1024)
    .unwrap()
    .with_max_text_bytes(4096)
    .unwrap()
}

/// The origin's response: `page.html.gz` with a cookie that must never be
/// recorded.
fn gzip_response() -> Vec<u8> {
    let mut response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\n\
         Content-Encoding: gzip\r\nContent-Length: {}\r\nETag: \"tide-1\"\r\n\
         Last-Modified: Tue, 22 Sep 2026 10:00:00 GMT\r\n\
         Set-Cookie: session=fixture-secret\r\nConnection: close\r\n\r\n",
        PAGE_GZ.len()
    )
    .into_bytes();
    response.extend_from_slice(PAGE_GZ);
    response
}

async fn acquire(url: &str) -> Acquisition {
    // NOTE: a throwaway anchor; the fixture is plain http.
    let anchor = rcgen::generate_simple_self_signed(vec!["unused.example".to_owned()]).unwrap();
    let acquirer = StaticAcquirer::builder(
        golden_limits(),
        TrustAnchors::from_der([anchor.cert.der().as_ref()]).unwrap(),
    )
    .resolver(Arc::new(PublicResolver))
    .connector(Arc::new(CannedConnector(Arc::new(gzip_response()))))
    .build()
    .unwrap();
    acquirer
        .acquire(
            &Url::parse(url).unwrap(),
            &SearchConstraints::new(1, BudgetConstraint::free_only()),
            None,
        )
        .await
        .unwrap()
}

fn golden() -> EvidenceEnvelope {
    serde_json::from_str(GOLDEN).unwrap()
}

/// The golden record as a mutable JSON value, for building altered copies.
fn golden_json() -> serde_json::Value {
    serde_json::from_str(GOLDEN).unwrap()
}

fn decode(value: serde_json::Value) -> Result<EvidenceEnvelope, serde_json::Error> {
    serde_json::from_value(value)
}

/// Lowercase hex SHA-256 of `bytes`, computed with `ring` directly.
fn hex_sha256(bytes: &[u8]) -> String {
    let mut out = String::new();
    for byte in ring::digest::digest(&ring::digest::SHA256, bytes).as_ref() {
        write!(out, "{byte:02x}").unwrap();
    }
    out
}

/// The fingerprint rule restated from the schema: SHA-256 over each field,
/// a present field as `0x01`, its byte length as a big-endian `u64`, and
/// its UTF-8 bytes; an absent field as the single byte `0x00`.
fn restated_fingerprint(fields: &[Option<&str>]) -> String {
    let mut hash = ring::digest::Context::new(&ring::digest::SHA256);
    for field in fields {
        match field {
            Some(value) => {
                hash.update(&[1]);
                hash.update(&u64::try_from(value.len()).unwrap().to_be_bytes());
                hash.update(value.as_bytes());
            }
            None => hash.update(&[0]),
        }
    }
    let mut out = String::from("sha256:");
    for byte in hash.finish().as_ref() {
        write!(out, "{byte:02x}").unwrap();
    }
    out
}

// -- Golden envelope: frozen serialized form, pinned in both directions. --

#[test]
fn golden_envelope_decodes_and_re_encodes_byte_for_byte() {
    for (name, golden) in [("complete", GOLDEN), ("failed", GOLDEN_FAILED)] {
        let envelope: EvidenceEnvelope = serde_json::from_str(golden).unwrap();
        let encoded = serde_json::to_string_pretty(&envelope).unwrap() + "\n";
        assert_eq!(
            encoded, golden,
            "{name}: the stored form must not drift from the golden fixture"
        );
    }
}

#[test]
fn golden_fingerprints_match_the_independent_computation() {
    assert_eq!(
        restated_fingerprint(&[
            Some(EVIDENCE_SCHEMA_ID),
            Some("1"),
            Some(URL),
            Some(URL),
            Some(PAGE_SHA256),
            Some("zetesis.html_text"),
            Some("1"),
            Some(TEXT_SHA256),
            Some("complete"),
        ]),
        FINGERPRINT,
        "restated rule agrees with the independently computed literal"
    );
    assert_eq!(golden().fingerprint(), FINGERPRINT, "complete golden");
    let failed: EvidenceEnvelope = serde_json::from_str(GOLDEN_FAILED).unwrap();
    assert_eq!(
        restated_fingerprint(&[
            Some(EVIDENCE_SCHEMA_ID),
            Some("1"),
            Some("http://public.example/tides"),
            None,
            None,
            None,
            None,
            None,
            Some("failed:unsafe_target"),
        ]),
        FAILED_FINGERPRINT,
        "absent fields carry their own marker"
    );
    assert_ne!(
        restated_fingerprint(&[None]),
        restated_fingerprint(&[Some("")]),
        "an absent field never collides with an empty one"
    );
    assert_eq!(failed.fingerprint(), FAILED_FINGERPRINT, "failed golden");
}

#[tokio::test]
async fn acquisition_produces_the_golden_evidence() {
    let acquisition = acquire(URL).await;
    let live = acquisition.envelope();
    let golden = golden();

    assert_eq!(
        live.fingerprint(),
        FINGERPRINT,
        "same content, same identity"
    );
    assert_eq!(live.schema_version(), EVIDENCE_SCHEMA_VERSION, "schema");
    assert_eq!(live.requested_url(), golden.requested_url(), "requested");
    assert_eq!(live.final_url(), golden.final_url(), "final target");
    assert_eq!(live.limits(), golden.limits(), "limit profile");
    assert_eq!(live.hops(), golden.hops(), "hop validation evidence");
    assert_eq!(live.response(), golden.response(), "selected metadata");
    assert_eq!(live.body(), golden.body(), "source byte identity");
    assert_eq!(live.extraction(), golden.extraction(), "spans and text");
    assert_eq!(live.outcome(), &Outcome::Complete, "completeness");
    assert_eq!(acquisition.body(), PAGE, "the decoded bytes are the page");

    let body = live.body().unwrap();
    assert_eq!(body.coding, ContentCoding::Gzip, "coding removed");
    assert_eq!(
        (body.wire_bytes, body.decoded_bytes),
        (190, 217),
        "wire and decoded sizes of the fixture files"
    );
    assert_eq!(body.wire_sha256, WIRE_SHA256, "digest of page.html.gz");
    assert_eq!(body.decoded_sha256, PAGE_SHA256, "digest of page.html");

    let extraction = live.extraction().unwrap();
    assert_eq!(extraction.media, Media::Html, "media");
    assert_eq!(
        (extraction.charset.label.as_str(), extraction.charset.source),
        ("utf-8", CharsetSource::Header),
        "the header charset takes precedence over the meta declaration"
    );
    let spans: Vec<(usize, usize, &str)> = extraction
        .segments
        .iter()
        .map(|s| (s.start, s.end, s.text.as_str()))
        .collect();
    assert_eq!(
        spans,
        [
            (57, 69, "Tidal record"),
            (139, 153, "Harbour levels"),
            (162, 197, "Readings & notes for the café."),
        ],
        "hand-computed spans; the script's markup-like text is skipped"
    );
    assert_eq!(
        &PAGE[162..197],
        "Readings &amp; notes for the café.".as_bytes(),
        "a span covers the source bytes, character reference included"
    );
    assert_eq!(
        (extraction.text_bytes, extraction.text_sha256.as_str()),
        (59, TEXT_SHA256),
        "joined text identity"
    );

    let stored = serde_json::to_string(live).unwrap();
    assert!(
        !stored.contains("fixture-secret") && !stored.to_ascii_lowercase().contains("cookie"),
        "no cookie reaches the envelope"
    );
}

#[tokio::test]
async fn refused_acquisition_produces_the_failed_golden_evidence() {
    let acquisition = acquire("http://user:secret@public.example/tides").await;
    let live = acquisition.envelope();
    let golden: EvidenceEnvelope = serde_json::from_str(GOLDEN_FAILED).unwrap();

    assert_eq!(live.fingerprint(), FAILED_FINGERPRINT, "failure identity");
    assert_eq!(
        live.requested_url(),
        golden.requested_url(),
        "userinfo gone"
    );
    assert_eq!(live.hops(), golden.hops(), "no resolution or connection");
    assert_eq!(live.outcome(), golden.outcome(), "typed failure");
    assert!(
        !serde_json::to_string(live).unwrap().contains("secret"),
        "the credential never enters the envelope"
    );
    assert!(acquisition.body().is_empty(), "no body bytes");
}

// -- Replay from captured bytes. --

#[test]
fn replay_of_the_captured_bytes_reproduces_the_transformation() {
    assert_eq!(
        replay(&golden(), PAGE),
        ReplayOutcome::Reproduced,
        "same bytes, same extractor version, same output"
    );
}

#[test]
fn replay_of_other_bytes_is_a_digest_mismatch() {
    let mut altered = PAGE.to_vec();
    altered[140] = b'h';
    assert_eq!(
        replay(&golden(), &altered),
        ReplayOutcome::DigestMismatch {
            recorded: PAGE_SHA256.to_owned(),
            supplied: "79c452eb14c07012c1fc39076ea0cb8f704a71405821bbbb19165f3f9d7561e4".to_owned(),
        },
        "bytes that are not the recorded body are named, not re-extracted"
    );
}

#[test]
fn replay_of_a_failed_envelope_has_nothing_to_replay() {
    let failed: EvidenceEnvelope = serde_json::from_str(GOLDEN_FAILED).unwrap();
    assert_eq!(
        replay(&failed, b""),
        ReplayOutcome::NothingToReplay,
        "no body and no extraction were recorded"
    );
}

#[test]
fn same_source_with_changed_extractor_version_is_a_version_mismatch() {
    let mut record = golden_json();
    record["extraction"]["extractor"]["version"] = 2.into();
    record["fingerprint"] = FINGERPRINT_EXTRACTOR_V2.into();
    let envelope = decode(record).unwrap();
    assert_eq!(
        replay(&envelope, PAGE),
        ReplayOutcome::VersionMismatch {
            recorded: ExtractorId {
                id: "zetesis.html_text".to_owned(),
                version: 2,
            },
            current: ExtractorId {
                id: "zetesis.html_text".to_owned(),
                version: 1,
            },
        },
        "a different extractor version is declared, not compared"
    );
}

#[test]
fn extraction_span_drift_is_detected() {
    // WHY: spans are not identity fields, so a shifted span keeps the
    // fingerprint valid; replay is what catches spans the recorded bytes
    // and extractor version do not reproduce.
    let mut record = golden_json();
    record["extraction"]["segments"][1]["start"] = 140.into();
    let envelope = decode(record).unwrap();
    assert_eq!(
        replay(&envelope, PAGE),
        ReplayOutcome::ExtractionDrift {
            first_difference: 1
        },
        "the second segment's span no longer matches its source"
    );

    let mut record = golden_json();
    record["extraction"]["segments"][2]["text"] = "Readings and notes for the café.".into();
    let envelope = decode(record).unwrap();
    assert_eq!(
        replay(&envelope, PAGE),
        ReplayOutcome::ExtractionDrift {
            first_difference: 2
        },
        "segment text that the source does not produce is drift"
    );
}

// -- Schema identity, version, and fingerprint on read. --

#[test]
fn unknown_schema_version_is_refused() {
    for version in [
        serde_json::json!(0),
        serde_json::json!(2),
        serde_json::json!("1"),
    ] {
        let mut record = golden_json();
        record["schema_version"] = version.clone();
        let err = decode(record).unwrap_err().to_string();
        assert!(
            err.contains("unsupported") && err.contains("schema_version"),
            "{version}: a version this reader does not know is refused: {err}"
        );
    }
    let mut record = golden_json();
    record.as_object_mut().unwrap().remove("schema_version");
    assert!(decode(record).is_err(), "a missing version is refused");
}

#[test]
fn foreign_schema_is_refused() {
    let mut record = golden_json();
    record["schema"] = "zetesis.provider_search".into();
    let err = decode(record).unwrap_err().to_string();
    assert!(
        err.contains("not a zetesis.static_acquisition record"),
        "another record type is refused by identity: {err}"
    );
    let mut record = golden_json();
    record.as_object_mut().unwrap().remove("schema");
    assert!(
        decode(record).is_err(),
        "a record without identity is refused"
    );
}

#[test]
fn altered_identity_field_breaks_the_fingerprint() {
    let alterations: [(&str, fn(&mut serde_json::Value)); 5] = [
        ("requested_url", |r| {
            r["requested_url"] = "http://public.example/other".into();
        }),
        ("final_url", |r| r["final_url"] = serde_json::Value::Null),
        ("decoded digest", |r| {
            r["body"]["decoded_sha256"] = WIRE_SHA256.into();
        }),
        ("text digest", |r| {
            r["extraction"]["text_sha256"] = PAGE_SHA256.into();
        }),
        ("outcome", |r| {
            r["outcome"] = serde_json::json!({
                "status": "partial",
                "reason": { "reason": "text_limit_reached" }
            });
        }),
    ];
    for (name, alter) in alterations {
        let mut record = golden_json();
        alter(&mut record);
        let err = decode(record).unwrap_err().to_string();
        assert!(
            err.contains("fingerprint does not match"),
            "{name}: an altered identity field must not decode as evidence: {err}"
        );
    }
}

#[test]
fn unknown_field_is_refused() {
    let mut record = golden_json();
    record["cookies"] = serde_json::json!(["session=x"]);
    assert!(
        decode(record).is_err(),
        "a field outside schema version 1 is refused, not ignored"
    );
    let mut record = golden_json();
    record["body"]["raw"] = "aGVsbG8=".into();
    assert!(decode(record).is_err(), "nested records are closed too");
}

#[test]
fn cbor_encoding_shares_field_names_and_version_rules() {
    let envelope = golden();
    let mut cbor = Vec::new();
    ciborium::into_writer(&envelope, &mut cbor).unwrap();
    let back: EvidenceEnvelope = ciborium::from_reader(cbor.as_slice()).unwrap();
    assert_eq!(back, envelope, "CBOR round trip is exact");

    let mut record = golden_json();
    record["schema_version"] = 2.into();
    let mut cbor = Vec::new();
    ciborium::into_writer(&record, &mut cbor).unwrap();
    let err = ciborium::from_reader::<EvidenceEnvelope, _>(cbor.as_slice()).unwrap_err();
    assert!(
        err.to_string().contains("schema_version"),
        "CBOR applies the same version refusal: {err}"
    );
}

// -- Phase 01 S3: the release record consumers pin against. --

#[test]
fn release_record_matches_the_code_and_fixtures() {
    // WHY: consumers pin the revision, schema, and fixtures this record
    // names. A fixture, schema, or extractor change after release must fail
    // here instead of silently diverging from what was published.
    let schema = format!("`{EVIDENCE_SCHEMA_ID}`, version `{EVIDENCE_SCHEMA_VERSION}`");
    assert!(
        RELEASE_RECORD.contains(&schema),
        "the record names the schema this build emits: {schema}"
    );
    let extractor = ExtractorId::current();
    let extractor = format!("`{}`, version `{}`", extractor.id, extractor.version);
    assert!(
        RELEASE_RECORD.contains(&extractor),
        "the record names the extractor this build runs: {extractor}"
    );
    for (name, bytes) in [
        ("page.html", PAGE),
        ("page.html.gz", PAGE_GZ),
        ("golden_envelope.json", GOLDEN.as_bytes()),
        ("golden_failed_envelope.json", GOLDEN_FAILED.as_bytes()),
    ] {
        let row = format!("| `{name}` | `{}` |", hex_sha256(bytes));
        assert!(
            RELEASE_RECORD.contains(&row),
            "{name}: released fixture bytes changed, or the record is stale ({row})"
        );
    }
    for fingerprint in [FINGERPRINT, FAILED_FINGERPRINT] {
        assert!(
            RELEASE_RECORD.contains(fingerprint),
            "the record carries golden fingerprint {fingerprint}"
        );
    }
}

#[test]
fn release_record_names_only_existing_tests() {
    let boundary = RELEASE_RECORD
        .split("| Boundary | Tests |")
        .nth(1)
        .and_then(|rest| rest.split("\n\n").next())
        .unwrap();
    let names: Vec<&str> = boundary
        .split('`')
        .skip(1)
        .step_by(2)
        .filter(|name| !name.contains('/') && !name.contains('.'))
        .collect();
    assert!(
        names.len() >= 20,
        "the boundary index lists its tests: {names:?}"
    );
    for name in names {
        let definition = format!("fn {name}(");
        assert!(
            ACQUISITION_TESTS.contains(&definition) || ENVELOPE_TESTS.contains(&definition),
            "the release record names a test that does not exist: {name}"
        );
    }
}
