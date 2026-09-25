//! The versioned evidence envelope for one static acquisition.
//!
//! An [`EvidenceEnvelope`] is what a consumer persists verbatim: the
//! requested and final targets, the hop-by-hop validation evidence, the
//! selected response metadata, the identity of the body bytes, the text
//! extraction with its source spans and extractor version, the outcome, and
//! a deterministic fingerprint. The body bytes themselves travel beside the
//! envelope in [`Acquisition`] so the consumer keeps them in its own custody,
//! keyed by [`BodyRecord::decoded_sha256`].
//!
//! Digests identify bytes. They are integrity evidence, not a verdict that
//! the content is accurate, safe, or authoritative; that judgment belongs
//! to the consumer.
//!
//! The envelope records full URLs, including query strings, exactly as
//! requested and followed (userinfo is never recorded). Whether a query
//! string is sensitive is the consumer's policy: it decides where the
//! envelope is stored and who may read it.
//!
//! Schema evolution: producers emit only [`EVIDENCE_SCHEMA_VERSION`]; decoding
//! accepts exactly that version and refuses any other with a typed message,
//! so a reader never silently misreads a record it does not understand. A
//! new or changed field is a new schema version. The extractor carries its
//! own version: changed extraction output bumps the extractor version in
//! [`ExtractorId`], not the schema.
//!
//! Decoding also recomputes the fingerprint and refuses a record whose
//! identity fields disagree with its stored fingerprint, which catches a
//! corrupted or partially edited record. The fingerprint is not a
//! signature: anyone can recompute it, so authenticity rests on the
//! consumer's custody of the stored record. Fields outside the identity
//! (spans, hops, timestamps) are checked by [`replay`], not by decoding.

use jiff::Timestamp;
use serde::{Deserialize, Deserializer, Serialize};
use url::Url;

use super::decode::{ContentCoding, DecodedBody};
use super::html_text::{self, Segment};
use super::media::{Charset, ContentType, Media};
use crate::acquisition::{AcquisitionFailure, AcquisitionLimits, HopRecord, ResponseRecord};
use crate::digest::{Sha256, sha256_hex};
use crate::error::ErrorClass;

/// Stable identifier of the evidence envelope record type.
pub const EVIDENCE_SCHEMA_ID: &str = "zetesis.static_acquisition";

/// The only evidence envelope schema version this producer emits and
/// decodes.
pub const EVIDENCE_SCHEMA_VERSION: u32 = 1;

/// The package and version that produced an envelope. The consumer attaches
/// the pinned commit it built from; a crate cannot know its own commit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Producer {
    /// Producing package name.
    pub package: String,
    /// Producing package version.
    pub version: String,
}

impl Producer {
    fn current() -> Self {
        Self {
            package: env!("CARGO_PKG_NAME").to_owned(),
            version: env!("CARGO_PKG_VERSION").to_owned(),
        }
    }
}

/// Identity of the decoded body bytes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BodyRecord {
    /// Content coding that was removed.
    pub coding: ContentCoding,
    /// Bytes received on the wire.
    pub wire_bytes: u64,
    /// SHA-256 of the wire bytes, lowercase hex.
    pub wire_sha256: String,
    /// Bytes after removing the content coding.
    pub decoded_bytes: u64,
    /// SHA-256 of the decoded bytes, lowercase hex. The consumer's custody
    /// key for the body.
    pub decoded_sha256: String,
}

impl BodyRecord {
    pub(crate) fn from_decoded(coding: ContentCoding, body: &DecodedBody) -> Self {
        Self {
            coding,
            wire_bytes: body.wire_bytes,
            wire_sha256: body.wire_sha256.clone(),
            decoded_bytes: u64::try_from(body.bytes.len()).unwrap_or(u64::MAX),
            decoded_sha256: body.decoded_sha256.clone(),
        }
    }
}

/// Extractor identity recorded with every extraction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExtractorId {
    /// Extractor identifier.
    pub id: String,
    /// Extraction rules version.
    pub version: u32,
}

impl ExtractorId {
    /// The extractor this build runs.
    #[must_use]
    pub fn current() -> Self {
        Self {
            id: html_text::EXTRACTOR_ID.to_owned(),
            version: html_text::EXTRACTOR_VERSION,
        }
    }
}

/// Text extracted from the decoded body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExtractionRecord {
    /// Which extractor, at which rules version.
    pub extractor: ExtractorId,
    /// The media type extracted as.
    pub media: Media,
    /// The encoding decision.
    pub charset: Charset,
    /// Segments in document order. Spans index the decoded body bytes.
    pub segments: Vec<Segment>,
    /// Bytes of the joined text (segments joined by `\n`).
    pub text_bytes: u64,
    /// SHA-256 of the joined text, lowercase hex.
    pub text_sha256: String,
    /// Whether the text ceiling stopped extraction.
    pub truncated: bool,
}

/// Why a transfer that completed produced incomplete evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum PartialReason {
    /// The text ceiling stopped extraction.
    TextLimitReached,
    /// The body's encoding is outside the supported subset.
    UnsupportedCharset {
        /// The declared label.
        label: String,
    },
    /// The body is not valid in its selected encoding.
    InvalidEncoding {
        /// Byte offset of the first invalid sequence.
        valid_up_to: u64,
    },
    /// The body contains binary data bytes (WHATWG MIME sniffing rule).
    BinaryContent,
    /// The response had an empty body.
    EmptyBody,
    /// The document has script elements and no extractable static text.
    /// This records what was observed; it does not claim scripts were
    /// required.
    NoStaticText,
}

impl PartialReason {
    /// Stable `snake_case` identifier, equal to the serialized `reason` tag.
    #[must_use]
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::TextLimitReached => "text_limit_reached",
            Self::UnsupportedCharset { .. } => "unsupported_charset",
            Self::InvalidEncoding { .. } => "invalid_encoding",
            Self::BinaryContent => "binary_content",
            Self::EmptyBody => "empty_body",
            Self::NoStaticText => "no_static_text",
        }
    }
}

/// How the acquisition ended.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Outcome {
    /// Transfer finished within limits and, for document media, extraction
    /// finished.
    Complete,
    /// The transfer completed but the evidence is incomplete.
    Partial {
        /// Why.
        reason: PartialReason,
    },
    /// The acquisition stopped; the hops record how far it got.
    Failed {
        /// Why.
        failure: AcquisitionFailure,
    },
}

impl Outcome {
    /// Stable identifier used in the fingerprint: `complete`,
    /// `partial:<reason>`, or `failed:<kind>`.
    #[must_use]
    pub fn kind(&self) -> String {
        match self {
            Self::Complete => "complete".to_owned(),
            Self::Partial { reason } => format!("partial:{}", reason.kind()),
            Self::Failed { failure } => format!("failed:{}", failure.kind()),
        }
    }

    /// Retry classification of a failed outcome; `None` otherwise.
    #[must_use]
    pub const fn failure_class(&self) -> Option<ErrorClass> {
        match self {
            Self::Failed { failure } => Some(failure.class()),
            _ => None,
        }
    }
}

/// Evidence for one static acquisition. See the module docs for the
/// schema, evolution, and integrity rules.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EvidenceEnvelope {
    schema: String,
    schema_version: u32,
    producer: Producer,
    requested_url: Url,
    final_url: Option<Url>,
    started_at: Timestamp,
    completed_at: Timestamp,
    limits: AcquisitionLimits,
    hops: Vec<HopRecord>,
    response: Option<ResponseRecord>,
    body: Option<BodyRecord>,
    extraction: Option<ExtractionRecord>,
    outcome: Outcome,
    fingerprint: String,
}

/// The parts an acquisition gathered, before the envelope derives its
/// fingerprint.
pub(crate) struct EnvelopeParts {
    pub(crate) requested_url: Url,
    pub(crate) final_url: Option<Url>,
    pub(crate) started_at: Timestamp,
    pub(crate) completed_at: Timestamp,
    pub(crate) limits: AcquisitionLimits,
    pub(crate) hops: Vec<HopRecord>,
    pub(crate) response: Option<ResponseRecord>,
    pub(crate) body: Option<BodyRecord>,
    pub(crate) extraction: Option<ExtractionRecord>,
    pub(crate) outcome: Outcome,
}

impl EvidenceEnvelope {
    pub(crate) fn seal(parts: EnvelopeParts) -> Self {
        let fingerprint = fingerprint(
            EVIDENCE_SCHEMA_VERSION,
            &parts.requested_url,
            parts.final_url.as_ref(),
            parts.body.as_ref(),
            parts.extraction.as_ref(),
            &parts.outcome,
        );
        Self {
            schema: EVIDENCE_SCHEMA_ID.to_owned(),
            schema_version: EVIDENCE_SCHEMA_VERSION,
            producer: Producer::current(),
            requested_url: parts.requested_url,
            final_url: parts.final_url,
            started_at: parts.started_at,
            completed_at: parts.completed_at,
            limits: parts.limits,
            hops: parts.hops,
            response: parts.response,
            body: parts.body,
            extraction: parts.extraction,
            outcome: parts.outcome,
            fingerprint,
        }
    }

    /// Schema version of this record.
    #[must_use]
    pub const fn schema_version(&self) -> u32 {
        self.schema_version
    }

    /// The package and version that produced it.
    #[must_use]
    pub const fn producer(&self) -> &Producer {
        &self.producer
    }

    /// The URL the caller asked for.
    #[must_use]
    pub const fn requested_url(&self) -> &Url {
        &self.requested_url
    }

    /// The URL whose response was accepted, when one was.
    #[must_use]
    pub const fn final_url(&self) -> Option<&Url> {
        self.final_url.as_ref()
    }

    /// When the acquisition started.
    #[must_use]
    pub const fn started_at(&self) -> Timestamp {
        self.started_at
    }

    /// When the acquisition ended.
    #[must_use]
    pub const fn completed_at(&self) -> Timestamp {
        self.completed_at
    }

    /// The exact limit profile applied.
    #[must_use]
    pub const fn limits(&self) -> &AcquisitionLimits {
        &self.limits
    }

    /// Hop-by-hop validation evidence, in request order.
    #[must_use]
    pub fn hops(&self) -> &[HopRecord] {
        &self.hops
    }

    /// Selected metadata of the accepted response.
    #[must_use]
    pub const fn response(&self) -> Option<&ResponseRecord> {
        self.response.as_ref()
    }

    /// Identity of the body bytes.
    #[must_use]
    pub const fn body(&self) -> Option<&BodyRecord> {
        self.body.as_ref()
    }

    /// The text extraction.
    #[must_use]
    pub const fn extraction(&self) -> Option<&ExtractionRecord> {
        self.extraction.as_ref()
    }

    /// How the acquisition ended.
    #[must_use]
    pub const fn outcome(&self) -> &Outcome {
        &self.outcome
    }

    /// The failure, when the acquisition stopped.
    #[must_use]
    pub const fn failure(&self) -> Option<&AcquisitionFailure> {
        match &self.outcome {
            Outcome::Failed { failure } => Some(failure),
            Outcome::Complete | Outcome::Partial { .. } => None,
        }
    }

    /// `sha256:<hex>` over the content and transformation identity: the
    /// schema id and version, requested and final URLs, decoded body
    /// digest, extractor id and version, text digest, and outcome kind.
    /// Timestamps, addresses, TLS details, header values, and spans are not
    /// part of it.
    #[must_use]
    pub fn fingerprint(&self) -> &str {
        &self.fingerprint
    }
}

/// Deterministic identity of what was acquired and how it was transformed.
///
/// SHA-256 over this sequence of fields, in order: the schema id, schema
/// version, requested URL, final URL, decoded body digest, extractor id,
/// extractor version, text digest, and outcome kind ([`Outcome::kind`]).
/// A present field is the byte `0x01`, its UTF-8 length as a big-endian
/// `u64`, then its bytes; an absent field is the single byte `0x00`, so
/// absence never collides with an empty value. Timestamps, addresses, TLS
/// details, header values, and spans are excluded: they vary per fetch, or
/// follow from the fields above, without changing what was acquired.
fn fingerprint(
    schema_version: u32,
    requested_url: &Url,
    final_url: Option<&Url>,
    body: Option<&BodyRecord>,
    extraction: Option<&ExtractionRecord>,
    outcome: &Outcome,
) -> String {
    let version = schema_version.to_string();
    let extractor_version = extraction.map(|e| e.extractor.version.to_string());
    let outcome = outcome.kind();
    let fields: [Option<&str>; 9] = [
        Some(EVIDENCE_SCHEMA_ID),
        Some(&version),
        Some(requested_url.as_str()),
        final_url.map(Url::as_str),
        body.map(|b| b.decoded_sha256.as_str()),
        extraction.map(|e| e.extractor.id.as_str()),
        extractor_version.as_deref(),
        extraction.map(|e| e.text_sha256.as_str()),
        Some(&outcome),
    ];
    let mut hash = Sha256::new();
    for field in fields {
        match field {
            Some(value) => {
                let len = u64::try_from(value.len()).unwrap_or(u64::MAX);
                hash.update(&[1]);
                hash.update(&len.to_be_bytes());
                hash.update(value.as_bytes());
            }
            None => hash.update(&[0]),
        }
    }
    format!("sha256:{}", hash.finish_hex())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EnvelopeWire {
    schema: String,
    schema_version: u32,
    producer: Producer,
    requested_url: Url,
    final_url: Option<Url>,
    started_at: Timestamp,
    completed_at: Timestamp,
    limits: AcquisitionLimits,
    hops: Vec<HopRecord>,
    response: Option<ResponseRecord>,
    body: Option<BodyRecord>,
    extraction: Option<ExtractionRecord>,
    outcome: Outcome,
    fingerprint: String,
}

impl<'de> Deserialize<'de> for EvidenceEnvelope {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        // WHY: check identity and version before anything else is trusted;
        // a newer or foreign record must be refused, not partially read.
        let value = serde_json::Value::deserialize(deserializer)?;
        let schema = value.get("schema").and_then(serde_json::Value::as_str);
        if schema != Some(EVIDENCE_SCHEMA_ID) {
            return Err(serde::de::Error::custom(format!(
                "not a {EVIDENCE_SCHEMA_ID} record (schema {schema:?})"
            )));
        }
        let version = value
            .get("schema_version")
            .and_then(serde_json::Value::as_u64);
        if version != Some(u64::from(EVIDENCE_SCHEMA_VERSION)) {
            return Err(serde::de::Error::custom(format!(
                "unsupported {EVIDENCE_SCHEMA_ID} schema_version {version:?}; this reader supports {EVIDENCE_SCHEMA_VERSION}"
            )));
        }
        let wire: EnvelopeWire = serde_json::from_value(value).map_err(serde::de::Error::custom)?;
        let expected = fingerprint(
            wire.schema_version,
            &wire.requested_url,
            wire.final_url.as_ref(),
            wire.body.as_ref(),
            wire.extraction.as_ref(),
            &wire.outcome,
        );
        if expected != wire.fingerprint {
            return Err(serde::de::Error::custom(
                "fingerprint does not match the recorded content and transformation",
            ));
        }
        Ok(Self {
            schema: wire.schema,
            schema_version: wire.schema_version,
            producer: wire.producer,
            requested_url: wire.requested_url,
            final_url: wire.final_url,
            started_at: wire.started_at,
            completed_at: wire.completed_at,
            limits: wire.limits,
            hops: wire.hops,
            response: wire.response,
            body: wire.body,
            extraction: wire.extraction,
            outcome: wire.outcome,
            fingerprint: wire.fingerprint,
        })
    }
}

/// The result of one [`crate::StaticAcquirer::acquire`] call: the envelope
/// to persist verbatim and the decoded body bytes for the consumer's own
/// custody, keyed by [`BodyRecord::decoded_sha256`].
///
/// Only the acquirer builds one, so its evidence is what the acquirer
/// actually validated, attempted, and read.
///
/// ```compile_fail
/// # use sylloge::{Acquisition, EvidenceEnvelope};
/// fn forge(envelope: EvidenceEnvelope) -> Acquisition {
///     Acquisition { envelope, body: Vec::new() }
/// }
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Acquisition {
    envelope: EvidenceEnvelope,
    body: Vec<u8>,
}

impl Acquisition {
    pub(crate) const fn new(envelope: EvidenceEnvelope, body: Vec<u8>) -> Self {
        Self { envelope, body }
    }

    /// The evidence envelope.
    #[must_use]
    pub const fn envelope(&self) -> &EvidenceEnvelope {
        &self.envelope
    }

    /// The decoded body bytes; empty when no body was read. Their SHA-256
    /// is [`BodyRecord::decoded_sha256`].
    #[must_use]
    pub fn body(&self) -> &[u8] {
        &self.body
    }

    /// Split into the envelope and the body bytes.
    #[must_use]
    pub fn into_parts(self) -> (EvidenceEnvelope, Vec<u8>) {
        (self.envelope, self.body)
    }
}

/// How many leading bytes the binary-data check examines: the WHATWG MIME
/// sniffing resource header length.
const SNIFF_BYTES: usize = 1445;

/// Whether `byte` is a WHATWG "binary data byte".
const fn is_binary_data_byte(byte: u8) -> bool {
    matches!(byte, 0x00..=0x08 | 0x0B | 0x0E..=0x1A | 0x1C..=0x1F)
}

/// Extract text from a decoded body. Shared by acquisition and replay so
/// both run the exact same transformation.
///
/// Returns the extraction record (absent when no text could be extracted
/// at all) and the partial reason, if the evidence is incomplete.
pub(crate) fn extract(
    media: Media,
    header_charset: Option<&str>,
    body: &[u8],
    max_text_bytes: usize,
) -> (Option<ExtractionRecord>, Option<PartialReason>) {
    if body.is_empty() {
        return (None, Some(PartialReason::EmptyBody));
    }
    let sniffed = body.get(..SNIFF_BYTES.min(body.len())).unwrap_or(body);
    if sniffed.iter().copied().any(is_binary_data_byte) {
        return (None, Some(PartialReason::BinaryContent));
    }
    let source = match super::media::decode_source(body, header_charset, media) {
        Ok(source) => source,
        Err(super::media::CharsetError::Unsupported { charset }) => {
            return (
                None,
                Some(PartialReason::UnsupportedCharset {
                    label: charset.label,
                }),
            );
        }
        Err(super::media::CharsetError::Invalid { valid_up_to, .. }) => {
            return (
                None,
                Some(PartialReason::InvalidEncoding {
                    valid_up_to: u64::try_from(valid_up_to).unwrap_or(u64::MAX),
                }),
            );
        }
    };
    let extracted = match media {
        Media::Html => html_text::extract_html(source.text, max_text_bytes),
        Media::PlainText => html_text::extract_plain(source.text, max_text_bytes),
    };
    let text = extracted.text();
    let segments = extracted
        .segments
        .iter()
        .map(|segment| segment.shifted(source.offset))
        .collect();
    let reason = if extracted.truncated {
        Some(PartialReason::TextLimitReached)
    } else if extracted.segments.is_empty() && extracted.saw_script {
        Some(PartialReason::NoStaticText)
    } else {
        None
    };
    let record = ExtractionRecord {
        extractor: ExtractorId::current(),
        media,
        charset: source.charset,
        segments,
        text_bytes: u64::try_from(text.len()).unwrap_or(u64::MAX),
        text_sha256: sha256_hex(text.as_bytes()),
        truncated: extracted.truncated,
    };
    (Some(record), reason)
}

/// The result of replaying an envelope's transformation over body bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ReplayOutcome {
    /// The bytes match and the current extractor reproduces the recorded
    /// extraction exactly.
    Reproduced,
    /// The envelope recorded no body or no extraction; there is nothing to
    /// replay.
    NothingToReplay,
    /// The supplied bytes are not the recorded body.
    DigestMismatch {
        /// The recorded decoded-body digest.
        recorded: String,
        /// The digest of the supplied bytes.
        supplied: String,
    },
    /// The envelope was produced by a different extractor or rules
    /// version, so a difference would not be evidence of drift.
    VersionMismatch {
        /// The recorded extractor.
        recorded: ExtractorId,
        /// This build's extractor.
        current: ExtractorId,
    },
    /// Same bytes, same extractor version, different output: the
    /// transformation is not reproducible and the envelope cannot be
    /// trusted as a record of it.
    ExtractionDrift {
        /// Index of the first segment that differs (or the shorter length).
        first_difference: usize,
    },
}

/// Re-run an envelope's recorded transformation over `body` and report
/// whether it reproduces.
#[must_use]
pub fn replay(envelope: &EvidenceEnvelope, body: &[u8]) -> ReplayOutcome {
    let (Some(recorded_body), Some(recorded)) = (envelope.body(), envelope.extraction()) else {
        return ReplayOutcome::NothingToReplay;
    };
    let supplied = sha256_hex(body);
    if supplied != recorded_body.decoded_sha256 {
        return ReplayOutcome::DigestMismatch {
            recorded: recorded_body.decoded_sha256.clone(),
            supplied,
        };
    }
    let current = ExtractorId::current();
    if recorded.extractor != current {
        return ReplayOutcome::VersionMismatch {
            recorded: recorded.extractor.clone(),
            current,
        };
    }
    let header_charset = envelope
        .response()
        .and_then(ResponseRecord::content_type)
        .and_then(ContentType::parse)
        .and_then(|ct| ct.charset);
    let max_text = usize::try_from(envelope.limits().max_text_bytes()).unwrap_or(usize::MAX);
    let (again, _) = extract(recorded.media, header_charset.as_deref(), body, max_text);
    let Some(again) = again else {
        return ReplayOutcome::ExtractionDrift {
            first_difference: 0,
        };
    };
    if again == *recorded {
        return ReplayOutcome::Reproduced;
    }
    let first_difference = recorded
        .segments
        .iter()
        .zip(&again.segments)
        .position(|(a, b)| a != b)
        .unwrap_or_else(|| recorded.segments.len().min(again.segments.len()));
    ReplayOutcome::ExtractionDrift { first_difference }
}
