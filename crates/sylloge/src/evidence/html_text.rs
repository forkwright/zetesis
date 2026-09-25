//! `zetesis.html_text`: static HTML and plain-text extraction with exact
//! source spans.
//!
//! The extractor turns a decoded UTF-8 source document into ordered text
//! [`Segment`]s. Each segment carries the byte range of the source it was
//! derived from, so a consumer can slice the source and see exactly which
//! markup produced which text, and a replay can prove the same bytes yield
//! the same segments.
//!
//! Rules (version 1):
//! - Block-level elements (paragraphs, headings, list items, table cells,
//!   `br`, ...) end the current segment; inline elements do not.
//! - `script`, `style`, `template`, `iframe`, `noembed`, `noframes`, and
//!   `xmp` content is skipped. `noscript` content is kept: this surface
//!   never runs scripts, so a browser without scripting would show it.
//! - `title` and `textarea` are text-only (RCDATA): character references
//!   decode, tags do not open.
//! - Comments, doctypes, CDATA sections, and processing instructions
//!   produce no text.
//! - Character references decode per the WHATWG table, including the
//!   legacy forms without a semicolon and the numeric remapping rules.
//! - Runs of ASCII whitespace in the source collapse to one space and are
//!   trimmed at segment edges; decoded references (including `&nbsp;`)
//!   are kept as decoded.
//! - Text is budgeted in UTF-8 bytes of the joined output (segments joined
//!   by `\n`). When the budget runs out the extraction stops at an exact
//!   character boundary and reports [`Extraction::truncated`].
//!
//! Changing any of these rules changes extraction output and therefore
//! bumps [`EXTRACTOR_VERSION`].

use serde::{Deserialize, Serialize};

/// Stable identifier of this extractor, recorded in evidence.
pub const EXTRACTOR_ID: &str = "zetesis.html_text";

/// Version of the extraction rules above. Output for the same source
/// bytes is identical within one version.
pub const EXTRACTOR_VERSION: u32 = 1;

/// One extracted text segment and the source bytes it came from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct Segment {
    /// Byte offset of the first source byte this segment's text came from.
    pub start: usize,
    /// Byte offset one past the last source byte this segment's text came
    /// from.
    pub end: usize,
    /// The extracted text.
    pub text: String,
}

impl Segment {
    /// This segment with its span moved `offset` bytes later.
    #[must_use]
    pub(crate) fn shifted(&self, offset: usize) -> Self {
        Self {
            start: self.start + offset,
            end: self.end + offset,
            text: self.text.clone(),
        }
    }
}

/// The result of one extraction.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Extraction {
    /// Segments in document order.
    pub segments: Vec<Segment>,
    /// Whether the text budget stopped extraction before the source ended.
    pub truncated: bool,
    /// Whether the document contained any `script` element. Recorded so an
    /// empty extraction can say "the document had scripts and no static
    /// text" without claiming scripts were required.
    pub saw_script: bool,
}

impl Extraction {
    /// The extracted text: segments joined by `\n`.
    #[must_use]
    pub fn text(&self) -> String {
        let mut out = String::with_capacity(self.text_len());
        for (i, segment) in self.segments.iter().enumerate() {
            if i > 0 {
                out.push('\n');
            }
            out.push_str(&segment.text);
        }
        out
    }

    /// Length in bytes of [`Extraction::text`], without building it.
    #[must_use]
    pub fn text_len(&self) -> usize {
        let body: usize = self.segments.iter().map(|s| s.text.len()).sum();
        body + self.segments.len().saturating_sub(1)
    }
}

/// Elements whose start or end tag ends the current segment.
const BLOCK_ELEMENTS: [&str; 44] = [
    "address",
    "article",
    "aside",
    "blockquote",
    "body",
    "br",
    "caption",
    "dd",
    "details",
    "dialog",
    "div",
    "dl",
    "dt",
    "fieldset",
    "figcaption",
    "figure",
    "footer",
    "form",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "head",
    "header",
    "hgroup",
    "hr",
    "html",
    "legend",
    "li",
    "main",
    "nav",
    "ol",
    "option",
    "p",
    "pre",
    "section",
    "summary",
    "table",
    "td",
    "th",
    "tr",
    "ul",
];

/// Elements whose content is raw text that never becomes extracted text.
const SKIPPED_RAW_TEXT: [&str; 7] = [
    "iframe", "noembed", "noframes", "script", "style", "template", "xmp",
];

/// Elements whose content is text only (character references decode, tags
/// do not open).
const RCDATA: [&str; 2] = ["textarea", "title"];

/// Extract text segments from a decoded HTML document.
#[must_use]
pub fn extract_html(source: &str, max_text_bytes: usize) -> Extraction {
    let mut out = Builder::new(max_text_bytes);
    let mut saw_script = false;
    let len = source.len();
    let mut text_start = 0;
    let mut i = 0;

    while i < len && !out.full {
        let Some(lt) = find_byte(source, i, b'<') else {
            break;
        };
        let markup = classify_markup(source, lt);
        if matches!(markup, Markup::Literal) {
            i = lt + 1;
            continue;
        }
        out.text(source, text_start, lt);
        i = match markup {
            Markup::Literal => lt + 1,
            Markup::Ignored { end } => end,
            Markup::EndTag { name, end } => {
                if is_block(&name) {
                    out.boundary();
                }
                end
            }
            Markup::StartTag { name, end } => {
                if is_block(&name) {
                    out.boundary();
                }
                if name == "script" {
                    saw_script = true;
                }
                if SKIPPED_RAW_TEXT.contains(&name.as_str()) {
                    let (_, after) = find_end_tag(source, end, &name);
                    after
                } else if RCDATA.contains(&name.as_str()) {
                    let (content_end, after) = find_end_tag(source, end, &name);
                    out.boundary();
                    out.text(source, end, content_end);
                    out.boundary();
                    after
                } else if name == "plaintext" {
                    out.text(source, end, len);
                    len
                } else {
                    end
                }
            }
        };
        text_start = i;
    }
    if !out.full {
        out.text(source, text_start.min(len), len);
    }
    out.finish(saw_script)
}

/// Extract text segments from a plain-text document: one segment per
/// non-blank line, whitespace collapsed like HTML text.
#[must_use]
pub fn extract_plain(source: &str, max_text_bytes: usize) -> Extraction {
    let mut out = Builder::new(max_text_bytes);
    let mut line_start = 0;
    for (offset, byte) in source.bytes().enumerate() {
        if out.full {
            break;
        }
        if byte == b'\n' {
            out.plain_line(source, line_start, offset);
            out.boundary();
            line_start = offset + 1;
        }
    }
    if !out.full {
        out.plain_line(source, line_start, source.len());
    }
    out.finish(false)
}

#[derive(Debug)]
enum Markup {
    /// `<` that opens no markup; it is text.
    Literal,
    /// A comment, doctype, CDATA section, or processing instruction.
    Ignored {
        end: usize,
    },
    StartTag {
        name: String,
        end: usize,
    },
    EndTag {
        name: String,
        end: usize,
    },
}

/// Classify the markup opening at `lt` (a `<` byte).
fn classify_markup(source: &str, lt: usize) -> Markup {
    let bytes = source.as_bytes();
    let after = lt + 1;
    match bytes.get(after) {
        Some(b'!') => {
            let end = if source
                .get(after..)
                .is_some_and(|rest| rest.starts_with("!--"))
            {
                // The shortest comments `<!-->` and `<!--->` close at once.
                let body = after + 3;
                ["-->", "--!>"]
                    .iter()
                    .filter_map(|close| {
                        find_str(source, body.saturating_sub(1), close).map(|p| p + close.len())
                    })
                    .min()
                    .map_or(source.len(), |p| p.max(body + 1))
            } else {
                find_byte(source, after, b'>').map_or(source.len(), |p| p + 1)
            };
            Markup::Ignored { end }
        }
        Some(b'?') => Markup::Ignored {
            end: find_byte(source, after, b'>').map_or(source.len(), |p| p + 1),
        },
        Some(b'/') => match bytes.get(after + 1) {
            Some(b) if b.is_ascii_alphabetic() => {
                let (name, name_end) = tag_name(source, after + 1);
                Markup::EndTag {
                    name,
                    end: tag_end(source, name_end),
                }
            }
            // `</>` is dropped; `</` followed by anything else is a bogus
            // comment up to the next `>`.
            Some(b'>') => Markup::Ignored { end: after + 2 },
            Some(_) => Markup::Ignored {
                end: find_byte(source, after, b'>').map_or(source.len(), |p| p + 1),
            },
            None => Markup::Literal,
        },
        Some(b) if b.is_ascii_alphabetic() => {
            let (name, name_end) = tag_name(source, after);
            Markup::StartTag {
                name,
                end: tag_end(source, name_end),
            }
        }
        _ => Markup::Literal,
    }
}

/// Read an ASCII tag name starting at `start`, lowercased.
fn tag_name(source: &str, start: usize) -> (String, usize) {
    let bytes = source.as_bytes();
    let mut end = start;
    while let Some(&b) = bytes.get(end) {
        if b.is_ascii_whitespace() || b == b'/' || b == b'>' {
            break;
        }
        end += 1;
    }
    let name = source.get(start..end).unwrap_or("").to_ascii_lowercase();
    (name, end)
}

/// Offset just past the `>` that closes a tag whose attributes start at
/// `from`, skipping `>` inside quoted attribute values.
fn tag_end(source: &str, from: usize) -> usize {
    let bytes = source.as_bytes();
    let mut i = from;
    let mut quote: Option<u8> = None;
    while let Some(&b) = bytes.get(i) {
        match quote {
            Some(q) if b == q => quote = None,
            None if b == b'"' || b == b'\'' => {
                // A quote opens a value only after `=`; elsewhere it is an
                // ordinary attribute-name byte.
                let prev = source
                    .get(..i)
                    .map(str::trim_end)
                    .and_then(|s| s.as_bytes().last().copied());
                if prev == Some(b'=') {
                    quote = Some(b);
                }
            }
            None if b == b'>' => return i + 1,
            _ => {}
        }
        i += 1;
    }
    source.len()
}

/// Find the end tag `</name` closing a raw-text or RCDATA element whose
/// content starts at `from`. Returns (content end, offset after the end
/// tag). An unclosed element runs to the end of the source.
fn find_end_tag(source: &str, from: usize, name: &str) -> (usize, usize) {
    let bytes = source.as_bytes();
    let mut i = from;
    while let Some(lt) = find_str(source, i, "</") {
        let name_start = lt + 2;
        let candidate = source.get(name_start..name_start + name.len());
        let follows = bytes.get(name_start + name.len());
        let closes = candidate.is_some_and(|c| c.eq_ignore_ascii_case(name))
            && follows.is_none_or(|b| b.is_ascii_whitespace() || *b == b'/' || *b == b'>');
        if closes {
            return (lt, tag_end(source, name_start + name.len()));
        }
        i = lt + 2;
    }
    (source.len(), source.len())
}

fn is_block(name: &str) -> bool {
    BLOCK_ELEMENTS.contains(&name)
}

fn find_byte(source: &str, from: usize, needle: u8) -> Option<usize> {
    source
        .as_bytes()
        .get(from..)?
        .iter()
        .position(|&b| b == needle)
        .map(|p| p + from)
}

fn find_str(source: &str, from: usize, needle: &str) -> Option<usize> {
    source.get(from..)?.find(needle).map(|p| p + from)
}

/// End offset of the character reference starting at `amp` (an `&`), or
/// `None` when no reference shape follows.
fn reference_end(source: &str, amp: usize) -> Option<usize> {
    let bytes = source.as_bytes();
    let mut i = amp + 1;
    if bytes.get(i) == Some(&b'#') {
        i += 1;
        let hex = matches!(bytes.get(i), Some(b'x' | b'X'));
        if hex {
            i += 1;
        }
        let digits_start = i;
        while bytes.get(i).is_some_and(|b| {
            if hex {
                b.is_ascii_hexdigit()
            } else {
                b.is_ascii_digit()
            }
        }) {
            i += 1;
        }
        if i == digits_start {
            return None;
        }
    } else {
        let name_start = i;
        while bytes.get(i).is_some_and(u8::is_ascii_alphanumeric) {
            i += 1;
        }
        if i == name_start {
            return None;
        }
    }
    if bytes.get(i) == Some(&b';') {
        i += 1;
    }
    Some(i)
}

/// Accumulates segments under the text budget.
struct Builder {
    segments: Vec<Segment>,
    current: Option<Segment>,
    /// Source range of whitespace seen since the last content, if any.
    pending_space: Option<(usize, usize)>,
    budget: usize,
    /// Bytes of finished segments plus their `\n` separators.
    used: usize,
    truncated: bool,
    full: bool,
}

impl Builder {
    fn new(budget: usize) -> Self {
        Self {
            segments: Vec::new(),
            current: None,
            pending_space: None,
            budget,
            used: 0,
            truncated: false,
            full: false,
        }
    }

    /// Bytes still available to the current segment's text.
    fn remaining(&self) -> usize {
        let separator = usize::from(!self.segments.is_empty());
        let current = self.current.as_ref().map_or(0, |s| s.text.len());
        self.budget
            .saturating_sub(self.used)
            .saturating_sub(separator)
            .saturating_sub(current)
    }

    /// Feed the raw HTML text in `source[start..end]`.
    fn text(&mut self, source: &str, start: usize, end: usize) {
        let bytes = source.as_bytes();
        let mut i = start;
        while i < end && !self.full {
            let Some(&b) = bytes.get(i) else { break };
            if b.is_ascii_whitespace() {
                let mut j = i;
                while j < end && bytes.get(j).is_some_and(u8::is_ascii_whitespace) {
                    j += 1;
                }
                self.whitespace(i, j);
                i = j;
            } else if b == b'&' {
                let decoded = reference_end(source, i)
                    .filter(|&e| e <= end)
                    .and_then(|ref_end| {
                        let raw = source.get(i..ref_end)?;
                        let decoded = htmlize::unescape(raw);
                        (decoded != raw).then(|| (ref_end, decoded.into_owned()))
                    });
                if let Some((ref_end, text)) = decoded {
                    self.content(i, ref_end, &text, false);
                    i = ref_end;
                } else {
                    // Not a known reference: the `&` is literal text.
                    self.content(i, i + 1, "&", true);
                    i += 1;
                }
            } else {
                let mut j = i;
                while j < end
                    && bytes
                        .get(j)
                        .is_some_and(|c| !c.is_ascii_whitespace() && *c != b'&')
                {
                    j += 1;
                }
                // INVARIANT: `i` and `j` sit on ASCII bytes or `end`, all of
                // which are UTF-8 character boundaries.
                let run = source.get(i..j).unwrap_or("");
                self.content(i, j, run, true);
                i = j;
            }
        }
    }

    /// Feed one plain-text line: collapse whitespace, no references.
    fn plain_line(&mut self, source: &str, start: usize, end: usize) {
        let bytes = source.as_bytes();
        let mut i = start;
        while i < end && !self.full {
            let Some(&b) = bytes.get(i) else { break };
            let mut j = i;
            let space = b.is_ascii_whitespace();
            while j < end
                && bytes
                    .get(j)
                    .is_some_and(|c| c.is_ascii_whitespace() == space)
            {
                j += 1;
            }
            if space {
                self.whitespace(i, j);
            } else {
                self.content(i, j, source.get(i..j).unwrap_or(""), true);
            }
            i = j;
        }
    }

    fn whitespace(&mut self, start: usize, end: usize) {
        if self.current.is_some() && self.pending_space.is_none() {
            self.pending_space = Some((start, end));
        }
    }

    /// Append `text` derived from `source[start..end]`. A `splittable`
    /// piece maps one source byte to one text byte and may be cut at a
    /// character boundary when the budget runs out; any other piece is
    /// all or nothing.
    fn content(&mut self, start: usize, end: usize, text: &str, splittable: bool) {
        if let Some((space_start, space_end)) = self.pending_space.take() {
            if self.remaining() == 0 {
                self.stop();
                return;
            }
            self.append(space_start, space_end, " ");
        }
        let room = self.remaining();
        if text.len() <= room {
            self.append(start, end, text);
            return;
        }
        if splittable {
            let mut cut = room;
            while cut > 0 && !text.is_char_boundary(cut) {
                cut -= 1;
            }
            if cut > 0 {
                self.append(start, start + cut, text.get(..cut).unwrap_or(""));
            }
        }
        self.stop();
    }

    fn append(&mut self, start: usize, end: usize, text: &str) {
        match &mut self.current {
            Some(segment) => {
                segment.end = end;
                segment.text.push_str(text);
            }
            None => {
                self.current = Some(Segment {
                    start,
                    end,
                    text: text.to_owned(),
                });
            }
        }
    }

    fn stop(&mut self) {
        self.truncated = true;
        self.full = true;
    }

    /// End the current segment, if any.
    fn boundary(&mut self) {
        self.pending_space = None;
        if let Some(segment) = self.current.take() {
            let separator = usize::from(!self.segments.is_empty());
            self.used += separator + segment.text.len();
            self.segments.push(segment);
        }
    }

    fn finish(mut self, saw_script: bool) -> Extraction {
        self.boundary();
        Extraction {
            segments: self.segments,
            truncated: self.truncated,
            saw_script,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BIG: usize = 1 << 20;

    fn seg(start: usize, end: usize, text: &str) -> Segment {
        Segment {
            start,
            end,
            text: text.to_owned(),
        }
    }

    #[test]
    fn inline_markup_stays_in_one_segment_with_exact_span() {
        // `<p>` 0..3, "Hello " 3..9, `<b>` 9..12, "world" 12..17.
        let src = "<p>Hello <b>world</b></p>";
        let out = extract_html(src, BIG);
        assert_eq!(
            out.segments,
            [seg(3, 17, "Hello world")],
            "one inline segment"
        );
        assert_eq!(
            src.get(3..17),
            Some("Hello <b>world"),
            "span covers the source run"
        );
        assert!(!out.truncated, "fits the budget");
    }

    #[test]
    fn block_elements_split_segments() {
        let src = "<h1>Title</h1><p>One</p><ul><li>a</li><li>b</li></ul>";
        let texts: Vec<_> = extract_html(src, BIG)
            .segments
            .into_iter()
            .map(|s| s.text)
            .collect();
        assert_eq!(texts, ["Title", "One", "a", "b"], "each block is a segment");
    }

    #[test]
    fn script_and_style_content_is_skipped_even_with_closing_markup_inside() {
        let src = "<p>a</p><script>var x = \"</p><p>no\";</script><style>p{}</style><p>b</p>";
        let out = extract_html(src, BIG);
        let texts: Vec<_> = out.segments.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(texts, ["a", "b"], "raw text never becomes content");
        assert!(out.saw_script, "the script element is recorded");
    }

    #[test]
    fn noscript_content_is_kept() {
        let out = extract_html("<noscript><p>Enable nothing</p></noscript>", BIG);
        assert_eq!(
            out.text(),
            "Enable nothing",
            "no script runs here, so noscript shows"
        );
    }

    #[test]
    fn character_references_decode_per_the_whatwg_table() {
        // Named with and without `;`, legacy prefix, numeric windows-1252
        // remap, and an unknown name kept literally.
        let src = "<p>Fish &amp; chips &copy2026 &notit; &#x80; &#65 &bogus;</p>";
        let out = extract_html(src, BIG);
        assert_eq!(
            out.text(),
            "Fish & chips ©2026 ¬it; € A &bogus;",
            "references decode like a browser's data state"
        );
        assert_eq!(
            out.segments[0].start, 3,
            "span starts at the first text byte"
        );
        assert_eq!(out.segments[0].end, src.len() - 4, "span ends before </p>");
    }

    #[test]
    fn nbsp_survives_whitespace_collapse() {
        let out = extract_html("<p>a &nbsp; b\n\n  c</p>", BIG);
        assert_eq!(
            out.text(),
            "a \u{a0} b c",
            "only ASCII whitespace collapses"
        );
    }

    #[test]
    fn comments_doctype_and_cdata_produce_no_text() {
        let src = "<!DOCTYPE html><!-- <p>hidden</p> -->a<![CDATA[x]]><?pi?>b<!---->c";
        assert_eq!(
            extract_html(src, BIG).text(),
            "abc",
            "only character data remains"
        );
    }

    #[test]
    fn unclosed_comment_hides_the_rest() {
        assert_eq!(
            extract_html("a<!-- b", BIG).text(),
            "a",
            "an open comment runs to EOF"
        );
    }

    #[test]
    fn quoted_attribute_value_may_contain_a_closing_bracket() {
        let src = "<a title=\"x>y\" href='/a>b'>link</a>";
        let out = extract_html(src, BIG);
        assert_eq!(
            out.segments,
            [seg(27, 31, "link")],
            "the tag ends at the real >"
        );
    }

    #[test]
    fn stray_less_than_is_text() {
        assert_eq!(
            extract_html("a < b <3 </", BIG).text(),
            "a < b <3 </",
            "no markup opens"
        );
    }

    #[test]
    fn title_and_textarea_are_text_only() {
        let src = "<title>A &amp; <b>B</b></title><textarea><p>raw</p></textarea>";
        let texts: Vec<_> = extract_html(src, BIG)
            .segments
            .into_iter()
            .map(|s| s.text)
            .collect();
        assert_eq!(
            texts,
            ["A & <b>B</b>", "<p>raw</p>"],
            "tags do not open in RCDATA"
        );
    }

    #[test]
    fn template_content_is_inert() {
        assert_eq!(
            extract_html("<p>a</p><template><p>t</p></template><p>b</p>", BIG).text(),
            "a\nb",
            "template content is not rendered"
        );
    }

    #[test]
    fn multibyte_text_spans_are_byte_offsets() {
        // "naïve café": ï and é are two bytes each, so 6 + 1 + 5 = 12 bytes.
        let src = "<p>naïve café</p>";
        let out = extract_html(src, BIG);
        assert_eq!(
            out.segments,
            [seg(3, 15, "naïve café")],
            "spans count bytes"
        );
        assert_eq!(
            src.get(3..15),
            Some("naïve café"),
            "the span slices the source"
        );
    }

    #[test]
    fn budget_truncates_at_an_exact_character_boundary() {
        // Budget 7: "héllo" is 6 bytes; the space would make 7; "w" does
        // not fit.
        let src = "<p>héllo wörld</p>";
        let out = extract_html(src, 7);
        assert!(out.truncated, "the budget ran out");
        assert_eq!(out.text(), "héllo ", "text stops at the budget");
        assert!(out.text_len() <= 7, "never exceeds the budget");
        let cut = extract_html(src, 8);
        assert_eq!(
            cut.segments,
            [seg(3, 11, "héllo w")],
            "a run splits at a char boundary"
        );
        let mid = extract_html(src, 9);
        assert_eq!(mid.text(), "héllo w", "ö (2 bytes) is not split");
    }

    #[test]
    fn budget_counts_segment_separators() {
        let out = extract_html("<p>ab</p><p>cd</p>", 4);
        assert_eq!(out.text(), "ab\nc", "the newline separator is budgeted");
        assert!(out.truncated, "the second segment was cut");
    }

    #[test]
    fn empty_and_whitespace_documents_yield_no_segments() {
        for src in ["", "   \n\t", "<html><body>  </body></html>"] {
            let out = extract_html(src, BIG);
            assert!(out.segments.is_empty(), "{src:?} has no text");
            assert!(!out.truncated, "{src:?} was not truncated");
        }
    }

    #[test]
    fn plaintext_element_consumes_the_rest() {
        assert_eq!(
            extract_html("<p>a</p><plaintext><p>b</p>", BIG).text(),
            "a\n<p>b</p>",
            "plaintext never closes"
        );
    }

    #[test]
    fn plain_text_yields_one_segment_per_non_blank_line() {
        let src = "first  line\r\n\n  second\tline  \nthird";
        let out = extract_plain(src, BIG);
        assert_eq!(
            out.segments,
            [
                seg(0, 11, "first line"),
                seg(16, 27, "second line"),
                seg(30, 35, "third")
            ],
            "whitespace collapses within lines; blank lines vanish"
        );
    }

    #[test]
    fn same_input_extracts_identically() {
        let src = "<div><p>x &amp; y</p><br>z</div>";
        assert_eq!(
            extract_html(src, BIG),
            extract_html(src, BIG),
            "extraction is deterministic"
        );
    }
}
