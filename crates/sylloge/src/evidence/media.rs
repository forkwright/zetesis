//! Media type and character encoding determination for extraction.
//!
//! Supported subset (version 1): documents that decode as UTF-8. The
//! encoding is taken, in WHATWG order, from a byte-order mark, the
//! `Content-Type` charset parameter, or (HTML only) a `<meta>` declaration
//! in the first 1024 bytes; with none of those the source must be valid
//! UTF-8. A windows-1252-family label (including `us-ascii` and
//! `iso-8859-1`, which WHATWG maps to windows-1252) is accepted only when
//! every byte is ASCII, where both decodings agree. Any other label, a
//! UTF-16 byte-order mark, or invalid UTF-8 is reported instead of decoded
//! with replacement characters, because lossy decoding would detach the
//! extracted spans from the source bytes.

use serde::{Deserialize, Serialize};

/// Media types this surface extracts text from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
#[serde(rename_all = "snake_case")]
pub enum Media {
    /// `text/html` or `application/xhtml+xml`.
    Html,
    /// `text/plain`.
    PlainText,
}

impl Media {
    /// Classify a media type essence (`type/subtype`, lowercase).
    #[must_use]
    pub fn from_essence(essence: &str) -> Option<Self> {
        match essence {
            "text/html" | "application/xhtml+xml" => Some(Self::Html),
            "text/plain" => Some(Self::PlainText),
            _ => None,
        }
    }
}

/// A parsed `Content-Type` header value.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ContentType {
    /// Lowercase `type/subtype`.
    pub essence: String,
    /// The `charset` parameter, lowercased and unquoted, if present.
    pub charset: Option<String>,
}

impl ContentType {
    /// Parse a `Content-Type` value. Returns `None` when no `type/subtype`
    /// essence is present.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        let mut parts = value.split(';');
        let essence = parts.next()?.trim().to_ascii_lowercase();
        let (kind, subtype) = essence.split_once('/')?;
        if kind.is_empty() || subtype.is_empty() || essence.contains(char::is_whitespace) {
            return None;
        }
        let charset = parts.find_map(|param| {
            let (name, value) = param.split_once('=')?;
            if !name.trim().eq_ignore_ascii_case("charset") {
                return None;
            }
            let value = value.trim().trim_matches('"').trim().to_ascii_lowercase();
            (!value.is_empty()).then_some(value)
        });
        Some(Self { essence, charset })
    }
}

/// Where the character encoding decision came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
#[serde(rename_all = "snake_case")]
pub enum CharsetSource {
    /// A byte-order mark.
    Bom,
    /// The `Content-Type` charset parameter.
    Header,
    /// An HTML `<meta>` declaration.
    Meta,
    /// No declaration; the bytes were valid UTF-8.
    AssumedUtf8,
}

/// The encoding decision recorded in evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct Charset {
    /// The label as declared (lowercase), or `utf-8` for a BOM or the
    /// assumed default.
    pub label: String,
    /// Where the label came from.
    pub source: CharsetSource,
}

/// Why a source could not be decoded for extraction.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum CharsetError {
    /// The declared encoding is outside the supported subset.
    Unsupported {
        /// The decision that selected it.
        charset: Charset,
    },
    /// The bytes are not valid in the selected encoding.
    Invalid {
        /// The decision that selected it.
        charset: Charset,
        /// Byte offset of the first invalid sequence.
        valid_up_to: usize,
    },
}

/// A decoded source ready for extraction.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct DecodedSource<'a> {
    /// The encoding decision.
    pub charset: Charset,
    /// The text after any byte-order mark.
    pub text: &'a str,
    /// Length of the stripped byte-order mark; add it to extraction spans
    /// to index the decoded body bytes.
    pub offset: usize,
}

/// WHATWG labels for UTF-8.
const UTF8_LABELS: [&str; 6] = [
    "unicode-1-1-utf-8",
    "unicode11utf8",
    "unicode20utf8",
    "utf-8",
    "utf8",
    "x-unicode20utf8",
];

/// WHATWG labels for windows-1252 (which includes `us-ascii` and the
/// ISO-8859-1 names).
const WINDOWS_1252_LABELS: [&str; 17] = [
    "ansi_x3.4-1968",
    "ascii",
    "cp1252",
    "cp819",
    "csisolatin1",
    "ibm819",
    "iso-8859-1",
    "iso-ir-100",
    "iso8859-1",
    "iso88591",
    "iso_8859-1",
    "iso_8859-1:1987",
    "l1",
    "latin1",
    "us-ascii",
    "windows-1252",
    "x-cp1252",
];

/// How many leading bytes the `<meta>` prescan examines (WHATWG).
const PRESCAN_BYTES: usize = 1024;

/// Decide the encoding of `body` and decode it for extraction.
///
/// # Errors
///
/// [`CharsetError::Unsupported`] for an encoding outside the supported
/// subset, [`CharsetError::Invalid`] for bytes invalid in the selected
/// encoding.
pub fn decode_source<'a>(
    body: &'a [u8],
    header_charset: Option<&str>,
    media: Media,
) -> Result<DecodedSource<'a>, CharsetError> {
    if let Some(rest) = body.strip_prefix(b"\xEF\xBB\xBF") {
        let charset = Charset {
            label: "utf-8".to_owned(),
            source: CharsetSource::Bom,
        };
        return utf8(rest, charset, 3);
    }
    if body.starts_with(b"\xFE\xFF") || body.starts_with(b"\xFF\xFE") {
        let label = if body.starts_with(b"\xFE\xFF") {
            "utf-16be"
        } else {
            "utf-16le"
        };
        return Err(CharsetError::Unsupported {
            charset: Charset {
                label: label.to_owned(),
                source: CharsetSource::Bom,
            },
        });
    }
    let declared = header_charset
        .map(|label| (label.to_ascii_lowercase(), CharsetSource::Header))
        .or_else(|| match media {
            Media::Html => meta_charset(body).map(|label| (label, CharsetSource::Meta)),
            Media::PlainText => None,
        });
    let Some((label, source)) = declared else {
        return utf8(
            body,
            Charset {
                label: "utf-8".to_owned(),
                source: CharsetSource::AssumedUtf8,
            },
            0,
        );
    };
    let charset = Charset { label, source };
    if UTF8_LABELS.contains(&charset.label.as_str()) {
        utf8(body, charset, 0)
    } else if WINDOWS_1252_LABELS.contains(&charset.label.as_str()) && body.is_ascii() {
        // WHY: ASCII bytes decode identically in windows-1252 and UTF-8.
        utf8(body, charset, 0)
    } else {
        Err(CharsetError::Unsupported { charset })
    }
}

fn utf8(bytes: &[u8], charset: Charset, offset: usize) -> Result<DecodedSource<'_>, CharsetError> {
    match std::str::from_utf8(bytes) {
        Ok(text) => Ok(DecodedSource {
            charset,
            text,
            offset,
        }),
        Err(e) => Err(CharsetError::Invalid {
            charset,
            valid_up_to: offset + e.valid_up_to(),
        }),
    }
}

/// A simplified WHATWG prescan: the first `<meta>` in the first 1024
/// bytes with a `charset` attribute, or an `http-equiv="content-type"`
/// `content` value carrying `charset=`. A UTF-16 label found this way
/// means UTF-8, as the WHATWG prescan specifies.
fn meta_charset(body: &[u8]) -> Option<String> {
    let head = body.get(..PRESCAN_BYTES.min(body.len()))?;
    let lower: Vec<u8> = head.to_ascii_lowercase();
    let mut from = 0;
    while let Some(pos) = find(&lower, from, b"<meta") {
        let tag_end = find(&lower, pos, b">").unwrap_or(lower.len());
        let tag = String::from_utf8_lossy(lower.get(pos..tag_end)?).into_owned();
        let label = attribute(&tag, "charset").or_else(|| {
            let equiv = attribute(&tag, "http-equiv")?;
            if equiv != "content-type" {
                return None;
            }
            let content = attribute(&tag, "content")?;
            let at = content.find("charset=")?;
            let value = content.get(at + "charset=".len()..)?;
            let value = value.trim_start_matches(['"', '\'']);
            let end = value.find([';', '"', '\'', ' ']).unwrap_or(value.len());
            value.get(..end).map(str::to_owned)
        });
        if let Some(label) = label.filter(|l| !l.is_empty()) {
            return Some(if label.starts_with("utf-16") {
                "utf-8".to_owned()
            } else {
                label
            });
        }
        from = tag_end;
    }
    None
}

fn find(haystack: &[u8], from: usize, needle: &[u8]) -> Option<usize> {
    haystack
        .get(from..)?
        .windows(needle.len())
        .position(|w| w == needle)
        .map(|p| p + from)
}

/// Value of attribute `name` in a lowercased `<meta ...` tag string.
fn attribute(tag: &str, name: &str) -> Option<String> {
    attributes(tag)
        .into_iter()
        .find(|(attr, _)| attr == name)
        .map(|(_, value)| value)
}

/// Tokenize the attributes of a tag string (starting at `<name`), honoring
/// quoted values so text inside one value never reads as another attribute.
fn attributes(tag: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut rest = tag
        .trim_start_matches('<')
        .trim_start_matches(|c: char| !c.is_whitespace() && c != '/');
    loop {
        rest = rest.trim_start_matches(|c: char| c.is_whitespace() || c == '/');
        if rest.is_empty() {
            return out;
        }
        let name_end = rest
            .find(|c: char| c.is_whitespace() || c == '=' || c == '/')
            .unwrap_or(rest.len());
        let name = rest.get(..name_end).unwrap_or("").to_owned();
        rest = rest.get(name_end..).unwrap_or("").trim_start();
        let Some(after_eq) = rest.strip_prefix('=') else {
            out.push((name, String::new()));
            continue;
        };
        let after_eq = after_eq.trim_start();
        let (value, remainder) = if let Some(q @ ('"' | '\'')) = after_eq.chars().next() {
            let body = after_eq.get(1..).unwrap_or("");
            let close = body.find(q).unwrap_or(body.len());
            (
                body.get(..close).unwrap_or(""),
                body.get(close + 1..).unwrap_or(""),
            )
        } else {
            let end = after_eq.find(char::is_whitespace).unwrap_or(after_eq.len());
            (
                after_eq.get(..end).unwrap_or(""),
                after_eq.get(end..).unwrap_or(""),
            )
        };
        out.push((name, value.trim().to_owned()));
        rest = remainder;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decision(label: &str, source: CharsetSource) -> Charset {
        Charset {
            label: label.to_owned(),
            source,
        }
    }

    #[test]
    fn content_type_parses_essence_and_charset() {
        let ct = ContentType::parse("Text/HTML; Charset=\"UTF-8\"").unwrap();
        assert_eq!(ct.essence, "text/html", "essence lowercased");
        assert_eq!(
            ct.charset.as_deref(),
            Some("utf-8"),
            "charset unquoted, lowercased"
        );
        assert_eq!(
            ContentType::parse("text/plain").unwrap().charset,
            None,
            "no charset parameter"
        );
        assert!(ContentType::parse("garbage").is_none(), "no essence");
        assert!(ContentType::parse("text/").is_none(), "empty subtype");
    }

    #[test]
    fn media_essences_map_to_extractors() {
        assert_eq!(Media::from_essence("text/html"), Some(Media::Html), "html");
        assert_eq!(
            Media::from_essence("application/xhtml+xml"),
            Some(Media::Html),
            "xhtml"
        );
        assert_eq!(
            Media::from_essence("text/plain"),
            Some(Media::PlainText),
            "plain"
        );
        assert_eq!(Media::from_essence("application/pdf"), None, "unsupported");
    }

    #[test]
    fn bom_overrides_the_header_label() {
        let src = decode_source(b"\xEF\xBB\xBFhi", Some("iso-8859-2"), Media::Html).unwrap();
        assert_eq!(
            src.charset,
            decision("utf-8", CharsetSource::Bom),
            "BOM wins"
        );
        assert_eq!(src.text, "hi", "BOM stripped");
        assert_eq!(src.offset, 3, "spans shift past the BOM");
    }

    #[test]
    fn utf16_bom_is_unsupported() {
        let err = decode_source(b"\xFF\xFEh\x00", None, Media::Html).unwrap_err();
        assert_eq!(
            err,
            CharsetError::Unsupported {
                charset: decision("utf-16le", CharsetSource::Bom)
            },
            "UTF-16 is outside the subset"
        );
    }

    #[test]
    fn header_label_is_used_before_meta() {
        let body = b"<meta charset=\"koi8-r\">text";
        let src = decode_source(body, Some("utf-8"), Media::Html).unwrap();
        assert_eq!(
            src.charset,
            decision("utf-8", CharsetSource::Header),
            "header wins"
        );
    }

    #[test]
    fn meta_declarations_are_found_in_the_prescan_window() {
        let charset_attr = decode_source(b"<head><meta charset='UTF-8'>", None, Media::Html);
        assert_eq!(
            charset_attr.unwrap().charset,
            decision("utf-8", CharsetSource::Meta),
            "charset attribute"
        );
        let http_equiv =
            b"<meta http-equiv=\"Content-Type\" content=\"text/html; charset=windows-1251\">";
        assert_eq!(
            decode_source(http_equiv, None, Media::Html).unwrap_err(),
            CharsetError::Unsupported {
                charset: decision("windows-1251", CharsetSource::Meta)
            },
            "http-equiv content charset"
        );
        let mut late = vec![b' '; PRESCAN_BYTES];
        late.extend_from_slice(b"<meta charset=koi8-r>");
        assert_eq!(
            decode_source(&late, None, Media::Html)
                .unwrap()
                .charset
                .source,
            CharsetSource::AssumedUtf8,
            "a declaration past 1024 bytes is not seen"
        );
    }

    #[test]
    fn meta_charset_text_inside_another_attribute_value_is_not_an_attribute() {
        let body = b"<meta name=\"x\" content=\"a; charset=koi8-r\">";
        assert_eq!(
            decode_source(body, None, Media::Html)
                .unwrap()
                .charset
                .source,
            CharsetSource::AssumedUtf8,
            "charset= inside a quoted value is data"
        );
    }

    #[test]
    fn meta_utf16_label_means_utf8() {
        let src = decode_source(b"<meta charset=utf-16le>ok", None, Media::Html).unwrap();
        assert_eq!(
            src.charset,
            decision("utf-8", CharsetSource::Meta),
            "WHATWG rule"
        );
    }

    #[test]
    fn plain_text_ignores_meta_like_content() {
        let src = decode_source(b"<meta charset=koi8-r>", None, Media::PlainText).unwrap();
        assert_eq!(
            src.charset.source,
            CharsetSource::AssumedUtf8,
            "no prescan for text/plain"
        );
    }

    #[test]
    fn windows_1252_family_label_accepts_only_ascii_bytes() {
        let ascii = decode_source(b"plain ascii", Some("us-ascii"), Media::PlainText).unwrap();
        assert_eq!(ascii.text, "plain ascii", "ASCII decodes identically");
        let high = decode_source(b"caf\xE9", Some("iso-8859-1"), Media::PlainText).unwrap_err();
        assert_eq!(
            high,
            CharsetError::Unsupported {
                charset: decision("iso-8859-1", CharsetSource::Header)
            },
            "non-ASCII windows-1252 is outside the subset"
        );
    }

    #[test]
    fn invalid_utf8_is_reported_with_its_offset() {
        let err = decode_source(b"ok\xC3(", Some("utf-8"), Media::Html).unwrap_err();
        assert_eq!(
            err,
            CharsetError::Invalid {
                charset: decision("utf-8", CharsetSource::Header),
                valid_up_to: 2
            },
            "the first invalid byte is located"
        );
    }

    #[test]
    fn undeclared_invalid_utf8_is_invalid_not_guessed() {
        let err = decode_source(b"\x80\x81", None, Media::Html).unwrap_err();
        assert!(
            matches!(err, CharsetError::Invalid { .. }),
            "no silent fallback to a legacy encoding: {err:?}"
        );
    }
}
