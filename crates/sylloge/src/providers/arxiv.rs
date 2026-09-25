//! arXiv query API (`GET /api/query`), answered as an Atom 1.0 feed.

use std::sync::Arc;
use std::time::Duration;

use jiff::Timestamp;
use quick_xml::events::{BytesRef, BytesStart, Event};
use quick_xml::name::ResolveResult;
use quick_xml::{Decoder, NsReader, XmlVersion};
use serde_json::Value;
use url::Url;

use super::{
    DoiIdentity, Endpoint, EndpointPolicy, HitParts, META_ARXIV_DOI, META_ARXIV_ID,
    META_ARXIV_VERSION, META_AUTHORS, META_DOI, META_YEAR, ParsedResponse, ProviderRequest,
    RateLimit, check_status, classify_doi, collapse_whitespace, collect_records, endpoint_url,
    malformed, normalize_arxiv_id, required, result_limit, search_endpoint, validated_query,
};
use crate::acquisition::StaticAcquirer;
use crate::citation::SourceKind;
use crate::constraints::SearchConstraints;
use crate::error::{Error, InvalidQuerySnafu, Result};
use crate::freshness::{
    PublicationPrecision, PublicationProvenance, PublicationTime, PublicationTimeCapability,
};
use crate::pacing::Gate;
use crate::provider::{BoxFut, Provider, ProviderAnswer};
use crate::query::QueryShape;
use crate::result::ResultHit;
use crate::tier::ProviderTier;

const PROVIDER: &str = "arxiv";

/// Media type the query API answers with.
const MEDIA_ATOM: &str = "application/atom+xml";

/// Documented largest slice per request ("slices of at most 2000").
const MAX_RESULTS: usize = 2_000;

/// Documented pacing: one request every three seconds.
const REQUEST_INTERVAL_SECS: u64 = 3;

const ATOM_NS: &[u8] = b"http://www.w3.org/2005/Atom";
const ARXIV_NS: &[u8] = b"http://arxiv.org/schemas/atom";

/// Path prefix of an abstract-page URL, which is what an entry `<id>` is.
const ABSTRACT_PATH: &str = "/abs/";

/// Host of every abstract page; a subdomain of it also qualifies.
const ARXIV_HOST: &str = "arxiv.org";

/// Path of the error document the API names in an error entry's `<id>`.
const ERROR_PATH: &str = "/api/errors";

/// Longest arXiv error code carried into an error message, in characters.
const MAX_ERROR_CODE_CHARS: usize = 120;

/// Length of the year prefix of an RFC 3339 timestamp.
const YEAR_DIGITS: usize = 4;

/// arXiv query API search over article metadata.
///
/// Every query term is searched in all fields (`all:`) and the terms are
/// combined with `AND`, results in the API's relevance order.
///
/// An instance and its clones share one gate: every caller, routed or
/// direct, holds to the documented pacing and connection limit together.
#[derive(Debug, Clone)]
pub struct Arxiv {
    acquirer: Arc<StaticAcquirer>,
    gate: Arc<Gate>,
}

impl Arxiv {
    /// Endpoint policy as documented on 2026-09-25.
    pub const POLICY: EndpointPolicy = EndpointPolicy {
        provider: PROVIDER,
        revision: "arxiv/2026-09-25",
        endpoint: "https://export.arxiv.org/api/query",
        sources: &[
            "https://info.arxiv.org/help/api/user-manual.html",
            "https://info.arxiv.org/help/api/tou.html",
        ],
        retrieved: jiff::civil::date(2026, 9, 25),
        authentication: "none",
        rate_limit: Some(RateLimit {
            requests: 1,
            window: std::time::Duration::from_secs(REQUEST_INTERVAL_SECS),
        }),
        max_concurrent: Some(1),
        documented_limits: "no more than one request every three seconds, one connection at a \
            time, across all of a client's machines; max_results is limited to 30000 in slices \
            of at most 2000, and more than 30000 is an HTTP 400; errors are returned as an Atom \
            feed with a single error entry; rate-limit status codes are not documented",
        terms: "https://info.arxiv.org/help/api/tou.html",
        license: "CC0-1.0",
        query_shapes: &[QueryShape::AcademicLiterature],
        language_scope: "the query API has no language parameter; \
            `SearchConstraints::language` is ignored",
        query_syntax: "search_query takes field prefixes (`ti:`, `au:`, `all:`, ...), boolean \
            operators, grouping, and quoted phrases; each whitespace-separated query term has \
            its quotes and backslashes removed and becomes a quoted `all:` term, and the terms \
            are joined with AND",
    };

    /// Search through `acquirer`, holding every caller of this instance
    /// and its clones to the documented terms: one connection at a time
    /// ([`EndpointPolicy::max_concurrent`]), requests starting at least
    /// three seconds apart ([`EndpointPolicy::rate_limit`]), and any
    /// `Retry-After` the API sends. Separate instances do not share these
    /// limits, so a process should search through one instance.
    #[must_use]
    pub fn new(acquirer: Arc<StaticAcquirer>) -> Self {
        let gate = Gate::new(
            Self::POLICY.min_interval().unwrap_or(Duration::ZERO),
            Self::POLICY.max_concurrent,
        );
        Self {
            acquirer,
            gate: Arc::new(gate),
        }
    }

    /// Build the query request for `query`, asking for at most
    /// `constraints.max_results` entries (capped at the documented 2000).
    ///
    /// The query is treated as plain text: each whitespace-separated term
    /// becomes a quoted `all:` term, so query text cannot inject field
    /// prefixes, boolean operators, or grouping into the search.
    /// `constraints.language` is ignored; the API has no language
    /// parameter.
    ///
    /// # Errors
    ///
    /// [`crate::Error::InvalidQuery`] when the query has no searchable
    /// text; [`crate::Error::InvalidConstraint`] when `max_results` is 0.
    pub fn request(query: &str, constraints: &SearchConstraints) -> Result<ProviderRequest> {
        let query = validated_query(PROVIDER, query)?;
        let terms: Vec<String> = query
            .split_whitespace()
            .map(|term| term.replace(['"', '\\'], ""))
            .filter(|term| !term.is_empty())
            .map(|term| format!("all:\"{term}\""))
            .collect();
        snafu::ensure!(
            !terms.is_empty(),
            InvalidQuerySnafu {
                reason: format!("{PROVIDER}: query has no searchable text once quotes are removed"),
            }
        );
        let limit = result_limit(constraints, MAX_RESULTS)?;
        let mut url = endpoint_url(&Self::POLICY)?;
        url.query_pairs_mut()
            .append_pair("search_query", &terms.join(" AND "))
            .append_pair("start", "0")
            .append_pair("max_results", &limit.to_string())
            .append_pair("sortBy", "relevance")
            .append_pair("sortOrder", "descending");
        Ok(ProviderRequest {
            url,
            headers: vec![("accept", MEDIA_ATOM.to_owned())],
        })
    }

    /// Parse an Atom feed into hits in the API's order.
    ///
    /// Elements are matched by namespace, so unknown elements and
    /// attributes are ignored; the text of markup nested inside a kept
    /// element is kept, and attributes are read whether an element is
    /// written empty or open. An entry is dropped and counted in
    /// [`ParsedResponse::malformed_records`] when it has no `<title>`, when
    /// its `<id>` is not an abstract-page URL on `arxiv.org` or a
    /// subdomain, or when its alternate link is not one or names a
    /// different paper or version than the `<id>`. The citation's publication
    /// time is `<updated>`, the submission time of the version retrieved;
    /// `<published>` (first version) is kept in metadata.
    ///
    /// # Errors
    ///
    /// See the [response mapping](crate#provider-response-mapping). A feed
    /// whose entry is the API's error entry is a
    /// [`crate::Error::InvalidQuery`] carrying the API's error code; a
    /// document that is not a well-formed Atom feed is a
    /// [`crate::Error::ProviderFailure`].
    pub fn parse(
        status: u16,
        headers: &[(&str, &str)],
        body: &[u8],
        accessed_at: Timestamp,
    ) -> Result<ParsedResponse> {
        check_status(PROVIDER, status, headers, accessed_at)?;
        let entries = read_feed(body)?;
        if let Some(code) = entries.iter().find_map(RawEntry::api_error_code) {
            return Err(InvalidQuerySnafu {
                reason: format!("{PROVIDER} reported API error `{code}`"),
            }
            .build());
        }
        collect_records(entries, |entry, rank| entry_hit(entry, rank, accessed_at))
    }
}

impl Provider for Arxiv {
    fn name(&self) -> &'static str {
        PROVIDER
    }

    fn tier(&self) -> ProviderTier {
        ProviderTier::Tier0Free
    }

    fn query_shapes(&self) -> &[QueryShape] {
        Self::POLICY.query_shapes
    }

    fn publication_time_capability(&self) -> PublicationTimeCapability {
        PublicationTimeCapability::Supported {
            precision: PublicationPrecision::Exact,
        }
    }

    fn search<'a>(
        &'a self,
        query: &'a str,
        constraints: &'a SearchConstraints,
    ) -> BoxFut<'a, ProviderAnswer> {
        let endpoint = Endpoint {
            provider: PROVIDER,
            media: MEDIA_ATOM,
            max_results: MAX_RESULTS,
            parse: Self::parse,
            gate: &self.gate,
        };
        Box::pin(search_endpoint(
            &self.acquirer,
            endpoint,
            Self::request(query, constraints),
            query,
            constraints,
        ))
    }
}

/// Entry children whose text the parser keeps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Field {
    Id,
    Title,
    Summary,
    Published,
    Updated,
    AuthorName,
    Doi,
    JournalRef,
    Comment,
}

/// Where the reader is in the element tree, as far as the parser cares.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tag {
    Feed,
    Entry,
    Author,
    Text(Field),
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ns {
    Atom,
    Arxiv,
    Other,
}

/// One `<entry>` as read from the feed, before validation.
#[derive(Debug, Default)]
struct RawEntry {
    id: Option<String>,
    title: Option<String>,
    summary: Option<String>,
    published: Option<String>,
    updated: Option<String>,
    authors: Vec<String>,
    doi: Option<String>,
    journal_ref: Option<String>,
    comment: Option<String>,
    alternate: Option<String>,
    primary_category: Option<String>,
    categories: Vec<String>,
}

impl RawEntry {
    fn store(&mut self, field: Field, text: String) {
        let slot = match field {
            Field::AuthorName => {
                self.authors.push(text);
                return;
            }
            Field::Id => &mut self.id,
            Field::Title => &mut self.title,
            Field::Summary => &mut self.summary,
            Field::Published => &mut self.published,
            Field::Updated => &mut self.updated,
            Field::Doi => &mut self.doi,
            Field::JournalRef => &mut self.journal_ref,
            Field::Comment => &mut self.comment,
        };
        *slot = Some(text);
    }

    /// The API's error code when this is an error entry: its `<id>` points
    /// at the API's error document, with the code as the fragment. Only
    /// identifier characters are kept, so no free text passes through.
    fn api_error_code(&self) -> Option<String> {
        let id = Url::parse(self.id.as_deref()?.trim()).ok()?;
        if id.path() != ERROR_PATH {
            return None;
        }
        let code: String = id
            .fragment()
            .unwrap_or_default()
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'))
            .take(MAX_ERROR_CODE_CHARS)
            .collect();
        Some(if code.is_empty() {
            "unnamed".to_owned()
        } else {
            code
        })
    }
}

/// Walk the feed and collect its entries.
fn read_feed(body: &[u8]) -> Result<Vec<RawEntry>> {
    let mut reader = NsReader::from_reader(body);
    let mut feed = FeedState::default();
    loop {
        let (ns, event) = match reader.read_resolved_event() {
            Ok((ns, event)) => (namespace(&ns), event),
            Err(e) => return Err(xml_defect(xml_kind(&e), reader.error_position())),
        };
        let at = reader.buffer_position();
        match event {
            Event::Start(start) => feed.open(ns, &start, reader.decoder(), at)?,
            Event::Empty(start) => feed.open_empty(ns, &start, reader.decoder(), at)?,
            Event::Text(t) if feed.capturing() => {
                let text = t.xml10_content().map_err(|_| xml_defect(ENCODING, at))?;
                feed.text.push_str(&text);
            }
            Event::CData(c) if feed.capturing() => {
                let text = c.xml10_content().map_err(|_| xml_defect(ENCODING, at))?;
                feed.text.push_str(&text);
            }
            Event::GeneralRef(r) if feed.capturing() => {
                feed.text.push_str(&resolve_reference(&r, at)?);
            }
            Event::End(_) => feed.close(),
            Event::Eof => return feed.finish(),
            _ => {}
        }
    }
}

/// Reader state for [`read_feed`].
#[derive(Default)]
struct FeedState {
    /// Open elements, outermost first.
    stack: Vec<Tag>,
    saw_root: bool,
    entries: Vec<RawEntry>,
    entry: RawEntry,
    /// Text of the open [`Tag::Text`] element, including the text of any
    /// markup nested inside it.
    text: String,
}

impl FeedState {
    /// Whether text read now belongs to a kept field: some open element is
    /// a [`Tag::Text`]. Markup nested inside one (`<title>A <b>B</b></title>`)
    /// keeps contributing its text.
    fn capturing(&self) -> bool {
        self.stack.iter().any(|tag| matches!(tag, Tag::Text(_)))
    }

    fn open(&mut self, ns: Ns, start: &BytesStart<'_>, decoder: Decoder, at: u64) -> Result<()> {
        let tag = self.classify_child(ns, start)?;
        if self.stack.last() == Some(&Tag::Entry) {
            read_attributes(&mut self.entry, ns, start, decoder, at)?;
        }
        match tag {
            Tag::Entry => self.entry = RawEntry::default(),
            Tag::Text(_) => self.text.clear(),
            _ => {}
        }
        self.stack.push(tag);
        Ok(())
    }

    fn open_empty(
        &mut self,
        ns: Ns,
        start: &BytesStart<'_>,
        decoder: Decoder,
        at: u64,
    ) -> Result<()> {
        let tag = self.classify_child(ns, start)?;
        if self.stack.last() == Some(&Tag::Entry) {
            read_attributes(&mut self.entry, ns, start, decoder, at)?;
        }
        if let Tag::Text(field) = tag {
            self.entry.store(field, String::new());
        }
        Ok(())
    }
    fn close(&mut self) {
        match self.stack.pop() {
            Some(Tag::Text(field)) => self.entry.store(field, std::mem::take(&mut self.text)),
            Some(Tag::Entry) => self.entries.push(std::mem::take(&mut self.entry)),
            _ => {}
        }
    }

    fn finish(self) -> Result<Vec<RawEntry>> {
        if !self.saw_root {
            return Err(feed_defect("document has no root element"));
        }
        if !self.stack.is_empty() {
            return Err(feed_defect("document ended inside an open element"));
        }
        Ok(self.entries)
    }

    /// Classify an element opened at the current depth. The one root must
    /// be an Atom `<feed>`.
    fn classify_child(&mut self, ns: Ns, start: &BytesStart<'_>) -> Result<Tag> {
        let parent = self.stack.last().copied();
        if parent.is_none() {
            if self.saw_root {
                return Err(feed_defect("content after the root element"));
            }
            self.saw_root = true;
        }
        classify(parent, ns, start)
    }
}

fn feed_defect(defect: &str) -> Error {
    malformed(PROVIDER, format!("Atom feed: {defect}"))
}

fn namespace(resolved: &ResolveResult<'_>) -> Ns {
    match resolved {
        ResolveResult::Bound(ns) if ns.as_ref() == ATOM_NS => Ns::Atom,
        ResolveResult::Bound(ns) if ns.as_ref() == ARXIV_NS => Ns::Arxiv,
        _ => Ns::Other,
    }
}

/// Classify an element opened under `parent`. The root must be an Atom
/// `<feed>`.
fn classify(parent: Option<Tag>, ns: Ns, start: &BytesStart<'_>) -> Result<Tag> {
    let local = start.local_name();
    let name = local.as_ref();
    let tag = match (parent, ns) {
        (None, Ns::Atom) if name == b"feed" => Tag::Feed,
        (None, _) => return Err(feed_defect("root element is not an Atom <feed>")),
        (Some(Tag::Feed), Ns::Atom) if name == b"entry" => Tag::Entry,
        (Some(Tag::Entry), Ns::Atom) => match name {
            b"id" => Tag::Text(Field::Id),
            b"title" => Tag::Text(Field::Title),
            b"summary" => Tag::Text(Field::Summary),
            b"published" => Tag::Text(Field::Published),
            b"updated" => Tag::Text(Field::Updated),
            b"author" => Tag::Author,
            _ => Tag::Other,
        },
        (Some(Tag::Entry), Ns::Arxiv) => match name {
            b"doi" => Tag::Text(Field::Doi),
            b"journal_ref" => Tag::Text(Field::JournalRef),
            b"comment" => Tag::Text(Field::Comment),
            _ => Tag::Other,
        },
        (Some(Tag::Author), Ns::Atom) if name == b"name" => Tag::Text(Field::AuthorName),
        _ => Tag::Other,
    };
    Ok(tag)
}

/// Record the attribute-bearing entry children: the abstract-page link and
/// the categories, whether written as empty or as open elements.
fn read_attributes(
    entry: &mut RawEntry,
    ns: Ns,
    element: &BytesStart<'_>,
    decoder: Decoder,
    at: u64,
) -> Result<()> {
    let local = element.local_name();
    let attribute = |key: &[u8]| -> Result<Option<String>> {
        element
            .try_get_attribute(key)
            .map_err(|_| xml_defect(INVALID_ATTRIBUTE, at))?
            .map(|attr| {
                attr.decoded_and_normalized_value(XmlVersion::Implicit1_0, decoder)
                    .map(|value| value.trim().to_owned())
                    .map_err(|e| xml_defect(xml_kind(&e), at))
            })
            .transpose()
    };
    match (ns, local.as_ref()) {
        (Ns::Atom, b"link") if attribute(b"rel")?.as_deref() == Some("alternate") => {
            entry.alternate = attribute(b"href")?;
        }
        (Ns::Atom, b"category") => entry.categories.extend(attribute(b"term")?),
        (Ns::Arxiv, b"primary_category") => entry.primary_category = attribute(b"term")?,
        _ => {}
    }
    Ok(())
}

/// Expand a character or predefined entity reference inside text.
fn resolve_reference(reference: &BytesRef<'_>, at: u64) -> Result<String> {
    if let Some(ch) = reference
        .resolve_char_ref()
        .map_err(|e| xml_defect(xml_kind(&e), at))?
    {
        return Ok(ch.to_string());
    }
    let name = reference.decode().map_err(|_| xml_defect(ENCODING, at))?;
    quick_xml::escape::resolve_predefined_entity(&name)
        .map(str::to_owned)
        .ok_or_else(|| xml_defect(UNDECLARED_ENTITY, at))
}

/// Defect kinds named in an XML error message.
const ENCODING: &str = "text is not valid UTF-8";
const INVALID_ATTRIBUTE: &str = "malformed attribute";
const UNDECLARED_ENTITY: &str = "reference to an undeclared entity";

/// A feed that is not well-formed XML, named by the kind of defect and its
/// byte position only. quick-xml's own messages quote element names,
/// entity names, and text from the document, which is text the origin
/// sent, so they never reach an error.
fn xml_defect(kind: &str, at: u64) -> Error {
    malformed(PROVIDER, format!("XML at byte {at}: {kind}"))
}

/// The kind of a quick-xml error, without any of the text it carries.
fn xml_kind(error: &quick_xml::Error) -> &'static str {
    match error {
        quick_xml::Error::Io(_) => "read error",
        quick_xml::Error::Syntax(_) => "syntax error",
        quick_xml::Error::IllFormed(_) => "ill-formed document",
        quick_xml::Error::InvalidAttr(_) => INVALID_ATTRIBUTE,
        quick_xml::Error::Encoding(_) => ENCODING,
        quick_xml::Error::Escape(_) => "unusable character or entity reference",
        quick_xml::Error::Namespace(_) => "namespace error",
    }
}

/// The entry as a cited hit; `None` when it lacks a title or a usable
/// abstract-page identity.
fn entry_hit(entry: RawEntry, rank: usize, accessed_at: Timestamp) -> Result<Option<ResultHit>> {
    let (Some(id), Some(title)) = (required(entry.id), required(entry.title)) else {
        return Ok(None);
    };
    let Some((arxiv_id, id_version)) = abstract_identity(&id) else {
        return Ok(None);
    };
    let (page, version) = match entry.alternate.filter(|href| !href.is_empty()) {
        Some(href) => match abstract_identity(&href) {
            Some((link_id, link_version))
                if link_id == arxiv_id && agree(id_version, link_version) =>
            {
                (href, id_version.or(link_version))
            }
            _ => return Ok(None),
        },
        None => (id, id_version),
    };
    let Ok(url) = Url::parse(page.trim()) else {
        return Ok(None);
    };
    let published = entry.published.map(|p| p.trim().to_owned());

    let mut metadata = vec![(META_ARXIV_ID, Value::from(arxiv_id))];
    metadata.extend(version.map(|v| (META_ARXIV_VERSION, Value::from(v))));
    metadata.extend(
        entry
            .doi
            .as_deref()
            .and_then(classify_doi)
            .map(|doi| match doi {
                DoiIdentity::Publisher(doi) => (META_DOI, Value::from(doi)),
                DoiIdentity::Arxiv { doi, .. } => (META_ARXIV_DOI, Value::from(doi)),
            }),
    );
    let authors = names(entry.authors);
    if !authors.is_empty() {
        metadata.push((META_AUTHORS, Value::from(authors)));
    }
    metadata.extend(
        published
            .as_deref()
            .and_then(submission_year)
            .map(|year| (META_YEAR, Value::from(year))),
    );
    metadata.extend(published.map(|p| ("published", Value::from(p))));
    metadata.extend(text_value("journal_ref", entry.journal_ref));
    metadata.extend(text_value("comment", entry.comment));
    metadata.extend(text_value("primary_category", entry.primary_category));
    if !entry.categories.is_empty() {
        metadata.push(("categories", Value::from(entry.categories)));
    }

    HitParts {
        title,
        snippet: entry
            .summary
            .map(|s| collapse_whitespace(&s))
            .unwrap_or_default(),
        url,
        published_at: version_time(entry.updated.as_deref()),
        source_kind: SourceKind::Preprint,
        rank,
        accessed_at,
    }
    .into_hit(&Arxiv::POLICY, metadata)
}

/// The arXiv identifier and version named by an abstract-page URL on
/// `arxiv.org` or one of its subdomains; `None` for any other URL.
fn abstract_identity(url: &str) -> Option<(String, Option<u32>)> {
    let url = Url::parse(url.trim()).ok()?;
    let host = url.host_str()?;
    let on_arxiv = host == ARXIV_HOST
        || host
            .strip_suffix(ARXIV_HOST)
            .is_some_and(|sub| sub.ends_with('.'));
    if !on_arxiv {
        return None;
    }
    normalize_arxiv_id(url.path().strip_prefix(ABSTRACT_PATH)?)
}

/// Whether two versions of one identifier agree: equal, or one unstated.
fn agree(a: Option<u32>, b: Option<u32>) -> bool {
    a.zip(b).is_none_or(|(a, b)| a == b)
}

/// `<updated>`, the submission time of the retrieved version.
fn version_time(updated: Option<&str>) -> PublicationTime {
    updated
        .and_then(|raw| raw.trim().parse::<Timestamp>().ok())
        .map_or(PublicationTime::Unknown, |at| PublicationTime::Known {
            at,
            precision: PublicationPrecision::Exact,
            provenance: PublicationProvenance::ProviderDeclared,
        })
}

/// Year of first submission, as written in `<published>`.
fn submission_year(published: &str) -> Option<i64> {
    published.get(..YEAR_DIGITS)?.parse().ok()
}

fn names(authors: Vec<String>) -> Vec<String> {
    authors
        .into_iter()
        .map(|name| collapse_whitespace(&name))
        .filter(|name| !name.is_empty())
        .collect()
}

fn text_value(key: &'static str, text: Option<String>) -> Option<(&'static str, Value)> {
    text.map(|t| collapse_whitespace(&t))
        .filter(|t| !t.is_empty())
        .map(|t| (key, Value::from(t)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn abstract_identity_reads_old_and_new_style_ids() {
        assert_eq!(
            abstract_identity("http://arxiv.org/abs/hep-ex/0307015"),
            Some(("hep-ex/0307015".to_owned(), None)),
            "old-style, unversioned"
        );
        assert_eq!(
            abstract_identity("http://arxiv.org/abs/2101.00001v3"),
            Some(("2101.00001".to_owned(), Some(3))),
            "new-style, versioned"
        );
        assert_eq!(
            abstract_identity("http://arxiv.org/api/errors#bad"),
            None,
            "an error document is not an abstract page"
        );
    }

    #[test]
    fn submission_year_reads_the_written_year() {
        assert_eq!(
            submission_year("2003-07-07T13:46:39-04:00"),
            Some(2003),
            "year as written, before any offset conversion"
        );
        assert_eq!(submission_year("20"), None, "too short");
    }
}
