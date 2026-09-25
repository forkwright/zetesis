//! Bounded `Content-Encoding` decoding with streaming limits and digests.
//!
//! A [`BodyDecoder`] consumes wire chunks as they arrive and enforces two
//! separate ceilings before anything is buffered past them: the wire bytes
//! received and the decoded bytes produced. Decompression writes into a
//! sink that refuses to grow past the decoded ceiling, so a decompression
//! bomb stops at the ceiling instead of allocating its expansion. SHA-256
//! digests of both the wire bytes and the decoded bytes are computed on the
//! way through.
//!
//! Supported codings are exactly the ones a request advertises: `gzip`
//! (and its `x-gzip` alias) and `deflate` (the zlib format RFC 9110
//! specifies). Anything else, or more than one coding stacked on one body,
//! is refused rather than guessed at.

use std::io::Write;

use serde::{Deserialize, Serialize};

/// The `Accept-Encoding` value matching what [`BodyDecoder`] can decode.
pub const ACCEPT_ENCODING: &str = "gzip, deflate";

/// The content coding applied to a response body.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
#[serde(rename_all = "snake_case")]
pub enum ContentCoding {
    /// No coding (absent header, or only `identity`).
    Identity,
    /// `gzip` / `x-gzip` (RFC 1952).
    Gzip,
    /// `deflate`: the zlib format (RFC 1950), per RFC 9110.
    Deflate,
}

/// Why a body could not be decoded within its limits.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum DecodeError {
    /// More wire bytes arrived than the wire ceiling allows.
    WireLimit {
        /// The ceiling in bytes.
        max: u64,
    },
    /// Decoding would produce more bytes than the decoded ceiling allows.
    DecodedLimit {
        /// The ceiling in bytes.
        max: u64,
    },
    /// The coding is not one this decoder supports.
    UnsupportedCoding {
        /// The coding token as received (lowercased).
        coding: String,
    },
    /// More than one coding was applied; stacked codings are refused.
    StackedCodings {
        /// The coding tokens in the order received (lowercased).
        codings: Vec<String>,
    },
    /// The coded stream is corrupt or ends before its trailer.
    Corrupt {
        /// Decoder detail.
        detail: String,
    },
}

/// Parse the `Content-Encoding` header values of one response.
///
/// # Errors
///
/// Returns [`DecodeError::UnsupportedCoding`] for any coding other than
/// `identity`, `gzip`, `x-gzip`, or `deflate`, and
/// [`DecodeError::StackedCodings`] when more than one non-identity coding
/// is present.
pub fn parse_content_encoding<'a, I>(values: I) -> Result<ContentCoding, DecodeError>
where
    I: IntoIterator<Item = &'a str>,
{
    let codings: Vec<String> = values
        .into_iter()
        .flat_map(|value| value.split(','))
        .map(|token| token.trim().to_ascii_lowercase())
        .filter(|token| !token.is_empty() && token != "identity")
        .collect();
    match codings.as_slice() {
        [] => Ok(ContentCoding::Identity),
        [only] => match only.as_str() {
            "gzip" | "x-gzip" => Ok(ContentCoding::Gzip),
            "deflate" => Ok(ContentCoding::Deflate),
            other => Err(DecodeError::UnsupportedCoding {
                coding: other.to_owned(),
            }),
        },
        _ => Err(DecodeError::StackedCodings { codings }),
    }
}

/// A decoded body and the identity of the bytes it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct DecodedBody {
    /// The decoded bytes.
    pub bytes: Vec<u8>,
    /// Wire bytes received.
    pub wire_bytes: u64,
    /// SHA-256 of the wire bytes, lowercase hex.
    pub wire_sha256: String,
    /// SHA-256 of the decoded bytes, lowercase hex.
    pub decoded_sha256: String,
}

/// Streaming decoder enforcing wire and decoded ceilings.
pub struct BodyDecoder {
    max_wire: u64,
    max_decoded: u64,
    wire_bytes: u64,
    wire_hash: crate::digest::Sha256,
    inner: Inner,
}

enum Inner {
    Identity(CappedSink),
    Gzip(flate2::write::GzDecoder<CappedSink>),
    Deflate(flate2::write::ZlibDecoder<CappedSink>),
}

impl BodyDecoder {
    /// Start decoding a body with `coding`, refusing more than `max_wire`
    /// wire bytes or `max_decoded` decoded bytes.
    #[must_use]
    pub fn new(coding: ContentCoding, max_wire: u64, max_decoded: u64) -> Self {
        let sink = CappedSink::new(max_decoded);
        let inner = match coding {
            ContentCoding::Identity => Inner::Identity(sink),
            ContentCoding::Gzip => Inner::Gzip(flate2::write::GzDecoder::new(sink)),
            ContentCoding::Deflate => Inner::Deflate(flate2::write::ZlibDecoder::new(sink)),
        };
        Self {
            max_wire,
            max_decoded,
            wire_bytes: 0,
            wire_hash: crate::digest::Sha256::new(),
            inner,
        }
    }

    /// Feed the next wire chunk.
    ///
    /// # Errors
    ///
    /// [`DecodeError::WireLimit`] when the chunk would take the wire total
    /// past its ceiling (the chunk is not consumed),
    /// [`DecodeError::DecodedLimit`] when decoding would pass the decoded
    /// ceiling, and [`DecodeError::Corrupt`] for a malformed coded stream.
    pub fn push(&mut self, chunk: &[u8]) -> Result<(), DecodeError> {
        let len = u64::try_from(chunk.len()).unwrap_or(u64::MAX);
        let total = self.wire_bytes.saturating_add(len);
        if total > self.max_wire {
            return Err(DecodeError::WireLimit { max: self.max_wire });
        }
        self.wire_bytes = total;
        self.wire_hash.update(chunk);
        let written = match &mut self.inner {
            Inner::Identity(sink) => sink.write_all(chunk),
            Inner::Gzip(decoder) => decoder.write_all(chunk),
            Inner::Deflate(decoder) => decoder.write_all(chunk),
        };
        written.map_err(|e| self.classify(&e))
    }

    /// Finish the body: verify the coded stream ended cleanly and return
    /// the decoded bytes with their digests.
    ///
    /// # Errors
    ///
    /// [`DecodeError::Corrupt`] when the coded stream is truncated or its
    /// trailer does not verify; [`DecodeError::DecodedLimit`] when flushing
    /// the final block passes the decoded ceiling.
    pub fn finish(self) -> Result<DecodedBody, DecodeError> {
        let max_decoded = self.max_decoded;
        let sink = match self.inner {
            Inner::Identity(sink) => Ok(sink),
            Inner::Gzip(decoder) => decoder.finish(),
            Inner::Deflate(decoder) => decoder.finish(),
        };
        let sink = sink.map_err(|e| {
            if is_cap_error(&e) {
                DecodeError::DecodedLimit { max: max_decoded }
            } else {
                corrupt(&e)
            }
        })?;
        let bytes = sink.into_bytes();
        let decoded_sha256 = crate::digest::sha256_hex(&bytes);
        Ok(DecodedBody {
            bytes,
            wire_bytes: self.wire_bytes,
            wire_sha256: self.wire_hash.finish_hex(),
            decoded_sha256,
        })
    }

    fn classify(&self, e: &std::io::Error) -> DecodeError {
        if is_cap_error(e) {
            DecodeError::DecodedLimit {
                max: self.max_decoded,
            }
        } else {
            corrupt(e)
        }
    }
}

fn corrupt(e: &std::io::Error) -> DecodeError {
    DecodeError::Corrupt {
        detail: e.to_string(),
    }
}

/// Marker message distinguishing a ceiling refusal from a codec error.
const CAP_MESSAGE: &str = "decoded body ceiling reached";

fn is_cap_error(e: &std::io::Error) -> bool {
    e.kind() == std::io::ErrorKind::FileTooLarge && e.to_string() == CAP_MESSAGE
}

/// A byte sink that refuses any write taking it past its ceiling.
struct CappedSink {
    bytes: Vec<u8>,
    max: u64,
}

impl CappedSink {
    fn new(max: u64) -> Self {
        Self {
            bytes: Vec::new(),
            max,
        }
    }

    fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }
}

impl Write for CappedSink {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let held = u64::try_from(self.bytes.len()).unwrap_or(u64::MAX);
        let incoming = u64::try_from(buf.len()).unwrap_or(u64::MAX);
        if held.saturating_add(incoming) > self.max {
            return Err(std::io::Error::new(
                std::io::ErrorKind::FileTooLarge,
                CAP_MESSAGE,
            ));
        }
        self.bytes.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;

    use super::*;

    fn gzip(data: &[u8]) -> Vec<u8> {
        let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        e.write_all(data).unwrap();
        e.finish().unwrap()
    }

    fn zlib(data: &[u8]) -> Vec<u8> {
        let mut e = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        e.write_all(data).unwrap();
        e.finish().unwrap()
    }

    fn decode_all(
        coding: ContentCoding,
        wire: &[u8],
        chunk: usize,
        max_wire: u64,
        max_decoded: u64,
    ) -> Result<DecodedBody, DecodeError> {
        let mut d = BodyDecoder::new(coding, max_wire, max_decoded);
        for piece in wire.chunks(chunk) {
            d.push(piece)?;
        }
        d.finish()
    }

    #[test]
    fn content_encoding_header_parses_supported_codings() {
        assert_eq!(
            parse_content_encoding([]),
            Ok(ContentCoding::Identity),
            "absent"
        );
        assert_eq!(
            parse_content_encoding(["identity"]),
            Ok(ContentCoding::Identity),
            "identity only"
        );
        assert_eq!(
            parse_content_encoding(["GZip"]),
            Ok(ContentCoding::Gzip),
            "case-insensitive"
        );
        assert_eq!(
            parse_content_encoding(["x-gzip"]),
            Ok(ContentCoding::Gzip),
            "legacy alias"
        );
        assert_eq!(
            parse_content_encoding(["deflate"]),
            Ok(ContentCoding::Deflate),
            "deflate"
        );
    }

    #[test]
    fn content_encoding_header_refuses_unknown_and_stacked_codings() {
        assert_eq!(
            parse_content_encoding(["br"]),
            Err(DecodeError::UnsupportedCoding {
                coding: "br".to_owned()
            }),
            "brotli is not advertised and not decoded"
        );
        assert_eq!(
            parse_content_encoding(["gzip, gzip"]),
            Err(DecodeError::StackedCodings {
                codings: vec!["gzip".to_owned(), "gzip".to_owned()]
            }),
            "stacked codings are ambiguous"
        );
        assert!(
            matches!(
                parse_content_encoding(["deflate", "gzip"]),
                Err(DecodeError::StackedCodings { .. })
            ),
            "codings split across header lines still stack"
        );
    }

    #[test]
    fn gzip_body_decodes_with_independent_digests() {
        let wire = gzip(b"hello");
        let body = decode_all(ContentCoding::Gzip, &wire, 3, 1024, 1024).unwrap();
        assert_eq!(body.bytes, b"hello", "decoded bytes");
        assert_eq!(
            body.wire_bytes,
            u64::try_from(wire.len()).unwrap(),
            "wire count"
        );
        // Independent value: SHA-256("hello") from the published test vector.
        assert_eq!(
            body.decoded_sha256, "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824",
            "decoded digest"
        );
        assert_eq!(
            body.wire_sha256,
            crate::digest::sha256_hex(&wire),
            "wire digest"
        );
    }

    #[test]
    fn identity_body_digest_matches_known_vector() {
        let body = decode_all(ContentCoding::Identity, b"", 1, 0, 0).unwrap();
        assert_eq!(
            body.decoded_sha256, "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
            "SHA-256 of empty input"
        );
        assert_eq!(
            body.wire_sha256, body.decoded_sha256,
            "identity wire == decoded"
        );
    }

    #[test]
    fn deflate_body_uses_the_zlib_format() {
        let body = decode_all(ContentCoding::Deflate, &zlib(b"zlib data"), 4, 1024, 1024).unwrap();
        assert_eq!(body.bytes, b"zlib data", "zlib-wrapped deflate decodes");
    }

    #[test]
    fn decompression_bomb_stops_at_the_decoded_ceiling() {
        // 8 MiB of zeros compresses to a few KiB.
        let wire = gzip(&vec![0_u8; 8 * 1024 * 1024]);
        assert!(
            wire.len() < 64 * 1024,
            "precondition: the bomb is small on the wire"
        );
        let err = decode_all(ContentCoding::Gzip, &wire, 1024, 1024 * 1024, 64 * 1024).unwrap_err();
        assert_eq!(
            err,
            DecodeError::DecodedLimit { max: 64 * 1024 },
            "decoded ceiling trips"
        );
    }

    #[test]
    fn decoded_ceiling_is_never_exceeded_in_memory() {
        let mut d = BodyDecoder::new(ContentCoding::Gzip, u64::MAX, 4096);
        let wire = gzip(&vec![7_u8; 100_000]);
        let mut tripped = false;
        for piece in wire.chunks(512) {
            if d.push(piece).is_err() {
                tripped = true;
                break;
            }
        }
        assert!(tripped, "the ceiling must trip");
        if let Inner::Gzip(decoder) = &d.inner {
            assert!(
                decoder.get_ref().bytes.len() <= 4096,
                "buffer stayed under the ceiling"
            );
        }
    }

    #[test]
    fn wire_ceiling_counts_compressed_bytes_crossing_mid_chunk() {
        let wire = gzip(b"compressed limit crossing");
        let max = u64::try_from(wire.len()).unwrap() - 1;
        let err = decode_all(ContentCoding::Gzip, &wire, 4, max, 1024).unwrap_err();
        assert_eq!(
            err,
            DecodeError::WireLimit { max },
            "one byte over the wire ceiling"
        );
    }

    #[test]
    fn truncated_gzip_stream_is_corrupt() {
        let wire = gzip(b"interrupted stream body");
        let cut = wire.get(..wire.len() - 6).unwrap();
        let err = decode_all(ContentCoding::Gzip, cut, 8, 1024, 1024).unwrap_err();
        assert!(
            matches!(err, DecodeError::Corrupt { .. }),
            "missing trailer: {err:?}"
        );
    }

    #[test]
    fn garbage_labelled_gzip_is_corrupt() {
        let err =
            decode_all(ContentCoding::Gzip, b"<html>not gzip</html>", 8, 1024, 1024).unwrap_err();
        assert!(
            matches!(err, DecodeError::Corrupt { .. }),
            "not a gzip stream: {err:?}"
        );
    }

    #[test]
    fn identity_body_obeys_both_ceilings() {
        let err = decode_all(ContentCoding::Identity, &[1_u8; 10], 3, 9, 100).unwrap_err();
        assert_eq!(err, DecodeError::WireLimit { max: 9 }, "wire ceiling");
        let err = decode_all(ContentCoding::Identity, &[1_u8; 10], 3, 100, 9).unwrap_err();
        assert_eq!(err, DecodeError::DecodedLimit { max: 9 }, "decoded ceiling");
    }
}
