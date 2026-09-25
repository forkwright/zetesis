//! Semantic Scholar Academic Graph paper relevance search.

use jiff::Timestamp;
use jiff::civil::Date;
use jiff::tz::TimeZone;
use serde::Deserialize;
use serde_json::Value;
use url::Url;

use super::{
    EndpointPolicy, HitParts, META_ARXIV_ID, META_AUTHORS, META_CORPUS_ID, META_DOI,
    META_S2_PAPER_ID, META_VENUE, META_YEAR, ProviderRequest, accept_json, bounded_detail,
    check_status, collapse_whitespace, endpoint_url, malformed, normalize_arxiv_id, normalize_doi,
    required, result_limit, validated_query,
};
use crate::citation::SourceKind;
use crate::constraints::SearchConstraints;
use crate::error::{InvalidQuerySnafu, Result};
use crate::freshness::{PublicationPrecision, PublicationProvenance, PublicationTime};
use crate::query::QueryShape;
use crate::result::ResultHit;

const PROVIDER: &str = "semantic_scholar";

/// Documented per-request maximum for `limit` ("Must be <= 100").
const MAX_LIMIT: usize = 100;

/// Fields requested per paper. Without `fields` the endpoint returns only
/// `paperId` and `title`.
const FIELDS: &str = "paperId,corpusId,externalIds,url,title,abstract,venue,year,\
publicationDate,publicationTypes,authors";

/// Paper resource path, the hit URL when a paper carries no website `url`.
const PAPER_RESOURCE_BASE: &str = "https://api.semanticscholar.org/graph/v1/paper/";

/// DOI prefix arXiv registers for its own preprints.
const ARXIV_DOI_PREFIX: &str = "10.48550/arxiv.";

/// `publicationTypes` values that mark a journal article. The query
/// filter documents `JournalArticle`; the response schema's example spells
/// it `Journal Article`.
const JOURNAL_ARTICLE_TYPES: [&str; 2] = ["JournalArticle", "Journal Article"];

/// Semantic Scholar paper relevance search (`GET /graph/v1/paper/search`).
///
/// A plain-text relevance search over the Academic Graph; not the bulk
/// search endpoint, which returns an unranked listing. Requests carry no
/// API key, so they draw on the pool shared by all unauthenticated users.
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub struct SemanticScholar;

impl SemanticScholar {
    /// Endpoint policy as documented on 2026-09-25.
    pub const POLICY: EndpointPolicy = EndpointPolicy {
        provider: PROVIDER,
        revision: "semantic_scholar/2026-09-25",
        endpoint: "https://api.semanticscholar.org/graph/v1/paper/search",
        sources: &[
            "https://api.semanticscholar.org/graph/v1/swagger.json",
            "https://www.semanticscholar.org/product/api",
            "https://www.semanticscholar.org/product/api/tutorial",
            "https://www.semanticscholar.org/product/api/license",
        ],
        retrieved: jiff::civil::date(2026, 9, 25),
        authentication: "optional `x-api-key` header; requests are sent without one",
        rate_limit: None,
        max_concurrent: None,
        documented_limits: "unauthenticated requests share 1000 requests per second among all \
            unauthenticated users and may be throttled; a key's introductory limit is 1 request \
            per second; relevance search returns at most 1,000 ranked results and 10 MB per \
            response; limit must be <= 100",
        terms: "https://www.semanticscholar.org/product/api/license",
        license: "attribution: Semantic Scholar",
        query_shapes: &[
            QueryShape::AcademicLiterature,
            QueryShape::GeneralResearch,
            QueryShape::SemanticDiscovery,
        ],
    };

    /// Build the search request for `query`, asking for at most
    /// `constraints.max_results` papers (capped at the endpoint's 100).
    ///
    /// ASCII hyphens become spaces: the endpoint documents that hyphenated
    /// query terms yield no matches and says to replace them with spaces.
    ///
    /// # Errors
    ///
    /// [`crate::Error::InvalidQuery`] when the query has no searchable
    /// text; [`crate::Error::InvalidConstraint`] when `max_results` is 0.
    pub fn request(query: &str, constraints: &SearchConstraints) -> Result<ProviderRequest> {
        let query = validated_query(PROVIDER, query)?;
        let query = collapse_whitespace(&query.replace('-', " "));
        snafu::ensure!(
            !query.is_empty(),
            InvalidQuerySnafu {
                reason: format!(
                    "{PROVIDER}: query has no searchable text once hyphens are removed"
                ),
            }
        );
        let limit = result_limit(constraints, MAX_LIMIT)?;
        let mut url = endpoint_url(&Self::POLICY)?;
        url.query_pairs_mut()
            .append_pair("query", &query)
            .append_pair("limit", &limit.to_string())
            .append_pair("fields", FIELDS);
        Ok(ProviderRequest {
            url,
            headers: vec![accept_json()],
        })
    }

    /// Parse a search response into hits in relevance order.
    ///
    /// Unknown fields are ignored and optional fields may be absent or
    /// null; `paperId` and `title` are required on every paper.
    ///
    /// # Errors
    ///
    /// See the [response mapping](crate#provider-response-mapping); a paper without `paperId` or
    /// `title`, or with an unparseable `url`, is a
    /// [`crate::Error::ProviderFailure`].
    pub fn parse(
        status: u16,
        headers: &[(&str, &str)],
        body: &[u8],
        accessed_at: Timestamp,
    ) -> Result<Vec<ResultHit>> {
        check_status(PROVIDER, status, headers, accessed_at)?;
        let batch: SearchBatch = serde_json::from_slice(body)
            .map_err(|e| malformed(PROVIDER, format!("JSON: {}", bounded_detail(e))))?;
        batch
            .data
            .into_iter()
            .enumerate()
            .map(|(rank, paper)| paper_hit(paper, rank, accessed_at))
            .collect()
    }
}

#[derive(Deserialize)]
struct SearchBatch {
    data: Vec<Paper>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Paper {
    #[serde(rename = "paperId")]
    id: Option<String>,
    corpus_id: Option<u64>,
    external_ids: Option<ExternalIds>,
    url: Option<String>,
    title: Option<String>,
    #[serde(rename = "abstract")]
    abstract_text: Option<String>,
    venue: Option<String>,
    year: Option<i64>,
    publication_date: Option<String>,
    publication_types: Option<Vec<String>>,
    authors: Option<Vec<Author>>,
}

#[derive(Deserialize)]
struct ExternalIds {
    #[serde(rename = "DOI")]
    doi: Option<String>,
    #[serde(rename = "ArXiv")]
    arxiv: Option<String>,
}

#[derive(Deserialize)]
struct Author {
    name: Option<String>,
}

fn paper_hit(paper: Paper, rank: usize, accessed_at: Timestamp) -> Result<ResultHit> {
    let position = rank.saturating_add(1);
    let paper_id = required(PROVIDER, paper.id, position, "paperId")?;
    let title = required(PROVIDER, paper.title, position, "title")?;
    let url = match paper.url.as_deref() {
        Some(raw) => Url::parse(raw).map_err(|e| {
            malformed(
                PROVIDER,
                format!("result {position} has an unusable `url`: {e}"),
            )
        })?,
        None => paper_resource_url(&paper_id)?,
    };
    let (doi, arxiv) = paper.external_ids.map_or((None, None), |ids| {
        (
            ids.doi.as_deref().and_then(normalize_doi),
            ids.arxiv.as_deref().and_then(normalize_arxiv_id),
        )
    });
    let source_kind = source_kind(
        paper.publication_types.as_deref().unwrap_or_default(),
        doi.as_deref(),
        arxiv.is_some(),
    );

    let mut metadata = vec![(META_S2_PAPER_ID, Value::from(paper_id))];
    metadata.extend(doi.map(|doi| (META_DOI, Value::from(doi))));
    metadata.extend(arxiv.map(|(id, _)| (META_ARXIV_ID, Value::from(id))));
    metadata.extend(paper.corpus_id.map(|id| (META_CORPUS_ID, Value::from(id))));
    metadata.extend(paper.year.map(|year| (META_YEAR, Value::from(year))));
    metadata.extend(
        paper
            .venue
            .map(|venue| collapse_whitespace(&venue))
            .filter(|venue| !venue.is_empty())
            .map(|venue| (META_VENUE, Value::from(venue))),
    );
    metadata.extend(
        paper
            .authors
            .map(|authors| (META_AUTHORS, Value::from(author_names(authors)))),
    );
    metadata.extend(
        paper
            .publication_types
            .map(|types| ("publication_types", Value::from(types))),
    );
    metadata.extend(
        paper
            .publication_date
            .clone()
            .map(|date| ("publication_date", Value::from(date))),
    );

    HitParts {
        title,
        snippet: paper
            .abstract_text
            .map(|text| collapse_whitespace(&text))
            .unwrap_or_default(),
        url,
        published_at: publication_time(paper.publication_date.as_deref()),
        source_kind,
        rank,
        accessed_at,
    }
    .into_hit(&SemanticScholar::POLICY, metadata)
}

fn paper_resource_url(paper_id: &str) -> Result<Url> {
    let mut url = Url::parse(PAPER_RESOURCE_BASE)
        .map_err(|e| malformed(PROVIDER, format!("paper resource base does not parse: {e}")))?;
    url.path_segments_mut()
        .map_err(|()| malformed(PROVIDER, "paper resource base cannot take a path"))?
        .pop_if_empty()
        .push(paper_id);
    Ok(url)
}

/// Conservative source kind: `Journal` only when the provider typed the
/// paper a journal article; `Preprint` when arXiv is its only publication
/// identity (no DOI, or only arXiv's own DOI); `Web` otherwise.
fn source_kind(publication_types: &[String], doi: Option<&str>, has_arxiv: bool) -> SourceKind {
    if publication_types
        .iter()
        .any(|t| JOURNAL_ARTICLE_TYPES.contains(&t.as_str()))
    {
        return SourceKind::Journal;
    }
    let doi_is_arxiv_or_absent = doi.is_none_or(|doi| doi.starts_with(ARXIV_DOI_PREFIX));
    if has_arxiv && doi_is_arxiv_or_absent {
        return SourceKind::Preprint;
    }
    SourceKind::Web
}

/// `publicationDate` as a date-only publication time. A year alone is not
/// a date, so a paper with only `year` stays `Unknown`.
fn publication_time(date: Option<&str>) -> PublicationTime {
    date.and_then(|raw| raw.trim().parse::<Date>().ok())
        .and_then(|date| date.to_zoned(TimeZone::UTC).ok())
        .map_or(PublicationTime::Unknown, |midnight| {
            PublicationTime::Known {
                at: midnight.timestamp(),
                precision: PublicationPrecision::DateOnly,
                provenance: PublicationProvenance::ProviderDeclared,
            }
        })
}

fn author_names(authors: Vec<Author>) -> Vec<String> {
    authors
        .into_iter()
        .filter_map(|author| author.name)
        .map(|name| collapse_whitespace(&name))
        .filter(|name| !name.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn types(names: &[&str]) -> Vec<String> {
        names.iter().map(|&n| n.to_owned()).collect()
    }

    #[test]
    fn journal_article_type_marks_a_journal() {
        assert_eq!(
            source_kind(&types(&["JournalArticle", "Review"]), None, true),
            SourceKind::Journal,
            "documented filter spelling"
        );
        assert_eq!(
            source_kind(&types(&["Journal Article"]), None, false),
            SourceKind::Journal,
            "response-example spelling"
        );
    }

    #[test]
    fn arxiv_only_identity_marks_a_preprint() {
        assert_eq!(
            source_kind(&[], None, true),
            SourceKind::Preprint,
            "arXiv id and no DOI"
        );
        assert_eq!(
            source_kind(&[], Some("10.48550/arxiv.1706.03762"), true),
            SourceKind::Preprint,
            "arXiv's own DOI is still an arXiv identity"
        );
    }

    #[test]
    fn anything_else_is_web() {
        assert_eq!(
            source_kind(&types(&["Conference"]), Some("10.1000/x"), true),
            SourceKind::Web,
            "a publisher DOI without a journal type is not proof of a journal"
        );
        assert_eq!(source_kind(&[], None, false), SourceKind::Web, "no signals");
    }

    #[test]
    fn year_alone_is_not_a_publication_time() {
        assert_eq!(
            publication_time(None),
            PublicationTime::Unknown,
            "no date field"
        );
        assert_eq!(
            publication_time(Some("2017")),
            PublicationTime::Unknown,
            "a bare year is not a date"
        );
    }

    #[test]
    fn publication_date_is_a_date_only_provider_declared_time() {
        assert_eq!(
            publication_time(Some("2017-06-12")),
            PublicationTime::Known {
                at: "2017-06-12T00:00:00Z".parse().unwrap(),
                precision: PublicationPrecision::DateOnly,
                provenance: PublicationProvenance::ProviderDeclared,
            },
            "midnight UTC of the declared date"
        );
    }
}
