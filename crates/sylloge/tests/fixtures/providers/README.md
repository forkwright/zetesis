# Provider response fixtures

Response bodies for the parser tests in `crates/sylloge/tests/provider_*.rs`
and the router tests in `crates/sylloge/tests/router.rs`. Every file is one
of three kinds:

- **recorded**: a live response body, stored byte for byte as received;
- **documented shape**: invented content laid out in the structure the
  provider's official documentation shows;
- **synthetic**: invented content for a condition the documentation does
  not show (a cut-off body, a missing required field, a renamed field).

In documented-shape and synthetic files, every identifier, DOI, name, and
title is invented for the fixture.

Status-only conditions (rate limiting, authentication denial, server
errors) are tested by status and headers; the tests pair them with a body
that would otherwise parse, to show the status decides. Timeouts belong to
the transport and are not fixtures here.

## semantic_scholar

Documentation: `https://api.semanticscholar.org/graph/v1/swagger.json`
(`/paper/search`, `PaperRelevanceSearchBatch`, `FullPaper`, `Error400`),
read 2026-09-25.

| File | Kind | Condition |
|---|---|---|
| `search_documented_shape.json` | documented shape | three papers: journal article, arXiv preprint, conference paper |
| `search_empty.json` | documented shape | the documented zero-result answer |
| `search_partial.json` | documented shape | default fields only (`paperId`, `title`), and every optional field null |
| `search_schema_changed.json` | synthetic | unknown fields added; `year` and `publicationTypes` gone |
| `search_type_changed.json` | synthetic | a known field (`year`) arrives as a string |
| `search_malformed.json` | synthetic | body cut off mid-record |
| `search_malformed_records.json` | synthetic | one paper without `title` and one with an unparseable `url`, between two complete papers |
| `bad_request_documented.json` | documented shape | the `Error400` example body |
| `rate_limited_recorded_2026-09-25.json` | recorded | HTTP 429 body, 2026-09-25T17:56:32Z, unauthenticated `GET /graph/v1/paper/search`; no `Retry-After` header was sent |
| `forbidden.json` | synthetic | HTTP 403 body |
| `server_error.json` | synthetic | HTTP 500 body |
| `duplicate_candidates.json` | synthetic | the Semantic Scholar side of the router's duplicate cases, including a paper known only by arXiv's DataCite DOI |

## arxiv

Documentation: `https://info.arxiv.org/help/api/user-manual.html`
(sections 3.3 "Outline of an Atom feed" and 3.4 "Errors"), read
2026-09-25. A live request on 2026-09-25 returned HTTP 406 with an empty
body, so no live Atom sample exists.

| File | Kind | Condition |
|---|---|---|
| `search_documented_shape.xml` | documented shape | a versioned entry with DOI, links, and categories; an old-style unversioned identifier |
| `search_empty.xml` | documented shape | a feed with no entries |
| `search_partial.xml` | synthetic | an entry with only `<id>` and `<title>` |
| `search_schema_changed.xml` | synthetic | unknown namespaces, elements, and attributes; entity references and CDATA; no `<published>` |
| `search_malformed.xml` | synthetic | mismatched closing tag |
| `search_truncated.xml` | synthetic | body cut off inside an entry |
| `search_malformed_records.xml` | synthetic | one entry without `<title>` and one whose `<id>` is not an abstract page, between two complete entries |
| `not_atom.xml` | synthetic | an HTML document instead of a feed |
| `api_error_documented.xml` | documented shape | the manual's error-feed example |
| `duplicate_candidates.xml` | synthetic | the arXiv side of the router's duplicate cases, including a journal DOI for a paper Semantic Scholar knows by arXiv's DOI |

## wikipedia

Documentation: `https://www.mediawiki.org/wiki/API:REST_API/Reference`
(search pages), read 2026-09-25.

| File | Kind | Condition |
|---|---|---|
| `search_recorded_2026-09-25.json` | recorded | HTTP 200, 2026-09-25T18:00:02Z, `GET https://en.wikipedia.org/w/rest.php/v1/search/page?q=transformer%20attention&limit=2` |
| `search_empty.json` | documented shape | no pages |
| `search_partial.json` | synthetic | required fields only, and optional fields null |
| `search_schema_changed.json` | synthetic | unknown fields added; `description` gone |
| `search_malformed.json` | synthetic | body cut off mid-string |
| `search_malformed_records.json` | synthetic | one page without `key` and one without `id`, between two complete pages |
| `search_type_changed.json` | synthetic | a known field (`id`) arrives as a string |

The recorded Wikipedia body quotes search excerpts of the English Wikipedia
articles "Transformer (deep learning)"
(`https://en.wikipedia.org/wiki/Transformer_(deep_learning)`) and "Attention
Is All You Need" (`https://en.wikipedia.org/wiki/Attention_Is_All_You_Need`).
That text is by the articles' Wikipedia contributors, listed in each
article's history, and is licensed under CC BY-SA 4.0
(`https://creativecommons.org/licenses/by-sa/4.0/`); it is kept unmodified.
