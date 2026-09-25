//! Shape-based routing across registered providers; see [`Router`].

use std::collections::BTreeSet;
use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use jiff::Timestamp;
use serde_json::Value;
use snafu::ensure;
use tokio::time::Instant;
use url::Url;

use crate::acquisition::saturating_millis;
use crate::constraints::{DomainRule, SearchConstraints};
use crate::cost::{CostTracking, ProviderSpend};
use crate::digest::Sha256;
use crate::error::{Error, InvalidConstraintSnafu, InvalidQuerySnafu, Result, UnsupportedSnafu};
use crate::freshness::{FreshnessPolicy, PublicationTimeCapability};
use crate::net_policy::parse_domain_rules;
use crate::pacing::within_deadline;
use crate::provider::Provider;
use crate::providers::{
    DoiIdentity, META_ARXIV_DOI, META_ARXIV_ID, META_AUTHORS, META_DOI, META_PAGEID,
    META_S2_PAPER_ID, META_YEAR, classify_doi, collapse_whitespace, normalize_arxiv_id,
};
use crate::query::QueryShape;
use crate::result::{
    AttemptOutcome, ProvenanceEntry, ProviderAttempt, RefusalReason, ResearchResult, ResultHit,
    cmp_score,
};
use crate::tier::ProviderTier;

/// Domain tag opening every cache-key encoding.
const CACHE_KEY_DOMAIN: &[u8] = b"zetesis.sylloge.router.cache_key.v1";

/// Metadata key: provider that surfaced the hit.
const META_PROVIDER: &str = "provider";
/// Metadata key: records merged into this hit.
const META_MERGED_RECORDS: &str = "merged_records";
/// Metadata key: URLs of records that resemble this hit but disagree.
const META_CONFLICTS_WITH: &str = "conflicts_with";

/// Routes a query to the registered providers that serve its shape.
///
/// A router holds providers in their registration order. For a query of
/// shape `S`, the route is every registered provider that declares `S` in
/// [`Provider::query_shapes`], in that order; a shape no provider declares
/// is an explicit [`crate::Error::Unsupported`] gap, never filler from a
/// provider built for something else. Registering the first cohort as
/// Wikipedia, Semantic Scholar, arXiv gives the routes:
///
/// | Shape | Route |
/// |---|---|
/// | `academic_literature` | `semantic_scholar`, `arxiv` |
/// | `quick_factual` | `wikipedia` |
/// | `general_research` | `wikipedia`, `semantic_scholar` |
/// | `semantic_discovery` | `semantic_scholar` |
/// | every other shape | unsupported |
///
/// # Attempts
///
/// Every provider on the route is called in order and gets one receipt in
/// [`ResearchResult::provenance`] ([`ProvenanceEntry::attempt`]): answered
/// (with how many hits were dropped and why, and how many malformed
/// records the provider dropped), empty, failed (with the error's class),
/// timed out, or refused. [`ResearchResult::malformed_records`] sums the
/// malformed counts, and [`ResearchResult::evidence_state`] reads the
/// receipts to tell "nothing found" from "nobody answered". A transient or
/// permanent failure of one provider does not stop the others; only a
/// fatal error aborts the route.
///
/// Each attempt has the router's per-attempt timeout as its deadline, and
/// the provider call runs with that deadline in scope. Pacing and
/// connection limits belong to each provider instance, not to the router
/// (see [`Provider`]): a first-cohort provider that would have to wait for
/// a pacing slot opening after the deadline (its documented interval, or
/// a `Retry-After` it was sent) fails at once as rate-limited without
/// calling upstream, and that is receipted as a failed attempt. A call
/// still running at the deadline, including one waiting for a connection
/// whose release time is unknown, is cancelled and receipted as timed out.
/// A receipt names the fingerprints of the evidence envelopes the
/// provider's call produced; the bodies are not kept.
///
/// Only [`ProviderTier::Tier0Free`] and [`ProviderTier::Tier2SelfHosted`]
/// providers are called. Every other tier is refused unconditionally,
/// whatever the caller's [`crate::BudgetConstraint`] says: paid spend needs
/// a durable ledger that can reserve and settle per attempt, which does not
/// exist yet, and a Tier-0 miss never enables paid use. Under a freshness
/// window with [`FreshnessPolicy::Strict`], a provider whose
/// [`Provider::publication_time_capability`] is
/// [`PublicationTimeCapability::Unsupported`] is refused too, since none of
/// its hits could pass. A refused provider is not called and records
/// nothing. Each attempt records the requests its provider reports sending
/// ([`crate::ProviderAnswer::requests_sent`]) as free requests in
/// [`ResearchResult::cost_spent`]; a timed-out attempt records one,
/// because its request may have left, and undercounting would understate
/// use of the provider's quota.
///
/// # Screening and merging
///
/// Each returned hit is screened before merging: its URL host against the
/// caller's domain deny and allow lists (the same rules
/// [`SearchConstraints::check_url`] applies, without DNS), and its primary
/// citation against the caller's freshness window, whose receipt is
/// attached to the kept hit. Kept hits are then merged:
///
/// 1. Hits sharing a stable identity (`doi`, `arxiv_id`, `s2_paper_id`, or
///    `pageid` on the same host) merge, unless another identity of the same
///    kind disagrees. An arXiv-registered DOI (`10.48550/arXiv.<id>`) counts
///    as the arXiv identity it names, never as a `doi`, so a preprint's
///    arXiv DOI and its journal DOI do not conflict.
/// 2. Otherwise hits whose normalized titles (case-folded, punctuation
///    removed, whitespace collapsed) match merge only when both declare the
///    same `year`, share at least one normalized author family name, and
///    no identity disagrees. A family name is the part of an `authors`
///    entry before its first comma (`Alpha, A.`), or else its last word
///    (`Alice Alpha`), ignoring a trailing `Jr`, `Sr`, `II`, `III`, or
///    `IV`, normalized like a title.
///
/// A merged hit keeps the first record's fields, adds any stable identity
/// (`doi`, `arxiv_id`, `s2_paper_id`) it lacked from the records it
/// absorbed, gains their citations as corroboration, keeps the higher
/// score, and lists each
/// absorbed record (provider, URL, title, metadata) under
/// `merged_records`. Records that look alike but disagree (an identity
/// conflict, or equal titles with different years) are both kept and name
/// each other's URL under `conflicts_with`; so are equal titles with equal
/// years and no shared author family name. Every hit names the provider
/// that surfaced it under `provider`. Hits are ordered by score, highest
/// first, and cut to `max_results`; a short answer stays short.
///
/// # Cache key
///
/// [`ResearchResult::cache_key`] is `sha256:` plus the hex SHA-256 of a
/// domain-separated, length-prefixed encoding of the query (whitespace
/// collapsed, case kept, because query syntax can be case-sensitive), the
/// shape, and the JSON-serialized constraints with each domain list
/// canonicalized, sorted, and deduplicated. Equal inputs give equal keys.
/// The key covers only what the router is given; it is not the full query
/// identity, which also carries consumer and scope identifiers. A result a
/// provider returns directly carries the default shape and an empty cache
/// key; the router fills in both, with the shape it routed.
pub struct Router {
    providers: Vec<Arc<dyn Provider>>,
    attempt_timeout: Duration,
}

impl fmt::Debug for Router {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Router")
            .field(
                "providers",
                &self
                    .providers
                    .iter()
                    .map(|provider| provider.name())
                    .collect::<Vec<_>>(),
            )
            .field("attempt_timeout", &self.attempt_timeout)
            .finish()
    }
}

impl Router {
    /// Register `providers`, whose order is the order every route tries
    /// them in, with `attempt_timeout` bounding each provider attempt,
    /// pacing and connection waits included.
    ///
    /// # Errors
    ///
    /// [`crate::Error::InvalidConstraint`] with field `providers` when two
    /// providers share a name (receipts and cost lines are keyed by name,
    /// so they would be indistinguishable), and with field
    /// `attempt_timeout` when it is zero or so long that a deadline that
    /// far from now cannot be represented on the clock.
    pub fn new(providers: Vec<Arc<dyn Provider>>, attempt_timeout: Duration) -> Result<Self> {
        ensure!(
            !attempt_timeout.is_zero(),
            InvalidConstraintSnafu {
                field: "attempt_timeout",
                reason: "must be greater than zero",
            }
        );
        ensure!(
            Instant::now().checked_add(attempt_timeout).is_some(),
            InvalidConstraintSnafu {
                field: "attempt_timeout",
                reason: "a deadline that far from now cannot be represented",
            }
        );
        let mut names = BTreeSet::new();
        for provider in &providers {
            ensure!(
                names.insert(provider.name()),
                InvalidConstraintSnafu {
                    field: "providers",
                    reason: format!("provider name `{}` is registered twice", provider.name()),
                }
            );
        }
        Ok(Self {
            providers,
            attempt_timeout,
        })
    }

    /// Route `query` of `shape` under `constraints`, evaluating freshness
    /// as of `now`. See the [`Router`] documentation for the route,
    /// receipts, screening, merging, and cache key.
    ///
    /// # Errors
    ///
    /// Before any provider is called: [`crate::Error::InvalidQuery`] for a
    /// query with no searchable text, [`crate::Error::InvalidConstraint`]
    /// for `max_results` of 0 or an unusable domain-list entry, and
    /// [`crate::Error::Unsupported`] when no registered provider serves
    /// `shape`. During the route: a fatal provider error, returned as is.
    pub async fn search(
        &self,
        query: &str,
        shape: QueryShape,
        constraints: &SearchConstraints,
        now: Timestamp,
    ) -> Result<ResearchResult> {
        let normalized = collapse_whitespace(query);
        ensure!(
            !normalized.is_empty(),
            InvalidQuerySnafu {
                reason: "query has no searchable text",
            }
        );
        ensure!(
            constraints.max_results > 0,
            InvalidConstraintSnafu {
                field: "max_results",
                reason: "must be at least 1",
            }
        );
        let screen = Screen::new(constraints, now)?;
        let route = self.route(shape)?;
        let cache_key = cache_key(&normalized, shape, &screen.canonical_constraints())?;

        let mut provenance = Vec::with_capacity(route.len());
        let mut collected = Collected::default();
        for (index, provider) in route.into_iter().enumerate() {
            let (outcome, evidence_fingerprints) = self
                .attempt(
                    provider.as_ref(),
                    query,
                    constraints,
                    &screen,
                    &mut collected,
                )
                .await?;
            let attempt = ProviderAttempt {
                ordinal: u32::try_from(index).unwrap_or(u32::MAX),
                tier: provider.tier(),
                outcome,
                evidence_fingerprints,
            };
            provenance.push(ProvenanceEntry::attempt(provider.name(), attempt));
        }

        let hits = merge(collected.candidates, constraints.max_results);
        let mut result =
            ResearchResult::new(query, shape, hits, provenance, collected.cost, cache_key);
        result.malformed_records = collected.malformed_records;
        Ok(result)
    }

    /// One provider attempt, bounded by the per-attempt timeout: its
    /// outcome and the fingerprints of the evidence the provider gathered.
    async fn attempt(
        &self,
        provider: &dyn Provider,
        query: &str,
        constraints: &SearchConstraints,
        screen: &Screen<'_>,
        collected: &mut Collected,
    ) -> Result<(AttemptOutcome, Vec<String>)> {
        if let Some(reason) = refusal(provider, constraints) {
            return Ok((AttemptOutcome::Refused { reason }, Vec::new()));
        }
        let call = provider.search(query, constraints);
        let answer = match Instant::now().checked_add(self.attempt_timeout) {
            Some(deadline) => {
                let bounded = tokio::time::timeout_at(deadline, within_deadline(deadline, call));
                let Ok(answer) = bounded.await else {
                    // WHY: a cancelled call may already have sent its
                    // request. Counting it keeps the recorded use of the
                    // provider's quota from falling below the real use.
                    collected
                        .cost
                        .add(ProviderSpend::new(provider.name(), 0, 1, 1));
                    let timed_out = AttemptOutcome::TimedOut {
                        timeout_ms: saturating_millis(self.attempt_timeout),
                    };
                    return Ok((timed_out, Vec::new()));
                };
                answer
            }
            // NOTE: `new` proved the deadline representable when the router
            // was built; one that no longer is lies beyond any clock this
            // process will read, so the call runs without a bound.
            None => call.await,
        };
        if answer.requests_sent > 0 {
            collected.cost.add(ProviderSpend::new(
                provider.name(),
                0,
                u64::from(answer.requests_sent),
                answer.requests_sent,
            ));
        }
        let outcome = match answer.result {
            Ok(result) => {
                collected.malformed_records = collected
                    .malformed_records
                    .saturating_add(result.malformed_records);
                screen.admit(provider.name(), result, &mut collected.candidates)
            }
            Err(e) if e.is_fatal() => return Err(e),
            Err(e) => failed(&e),
        };
        Ok((outcome, answer.evidence_fingerprints))
    }

    /// Registered providers that declare `shape`, in registration order.
    fn route(&self, shape: QueryShape) -> Result<Vec<&Arc<dyn Provider>>> {
        let route: Vec<&Arc<dyn Provider>> = self
            .providers
            .iter()
            .filter(|provider| provider.query_shapes().contains(&shape))
            .collect();
        ensure!(
            !route.is_empty(),
            UnsupportedSnafu {
                reason: format!(
                    "no registered provider serves query shape `{}`",
                    shape.as_str()
                ),
            }
        );
        Ok(route)
    }
}

/// What the route's attempts gathered.
#[derive(Default)]
struct Collected {
    cost: CostTracking,
    malformed_records: usize,
    candidates: Vec<Candidate>,
}

/// The receipt for an attempt that ended in a non-fatal error.
fn failed(error: &Error) -> AttemptOutcome {
    AttemptOutcome::Failed {
        class: error.class(),
        message: error.to_string(),
    }
}

/// Why the router will not call `provider` under `constraints`, if it
/// will not.
fn refusal(provider: &dyn Provider, constraints: &SearchConstraints) -> Option<RefusalReason> {
    match provider.tier() {
        ProviderTier::Tier0Free | ProviderTier::Tier2SelfHosted => {}
        // WHY: an allow-list, so a tier added later is refused until routing
        // it is decided.
        _ => return Some(RefusalReason::PaidRoutingUnavailable),
    }
    let strict_window = constraints.freshness_window.is_some()
        && constraints.freshness_policy == FreshnessPolicy::Strict;
    let undated = provider.publication_time_capability() == PublicationTimeCapability::Unsupported;
    (strict_window && undated).then_some(RefusalReason::PublicationTimeUnsupported)
}

/// The caller's domain and freshness screens, parsed once per search.
struct Screen<'c> {
    constraints: &'c SearchConstraints,
    now: Timestamp,
    deny: Option<Vec<DomainRule>>,
    allow: Option<Vec<DomainRule>>,
}

impl<'c> Screen<'c> {
    fn new(constraints: &'c SearchConstraints, now: Timestamp) -> Result<Self> {
        Ok(Self {
            constraints,
            now,
            deny: parse_domain_rules("domain_denylist", constraints.domain_denylist.as_deref())?,
            allow: parse_domain_rules("domain_allowlist", constraints.domain_allowlist.as_deref())?,
        })
    }

    /// The caller's constraints with each domain list replaced by its
    /// canonical entries, sorted and deduplicated, so spellings that parse
    /// to the same rules produce one cache key.
    fn canonical_constraints(&self) -> SearchConstraints {
        let canonical = |rules: &Vec<DomainRule>| {
            let entries: BTreeSet<String> = rules.iter().map(DomainRule::canonical).collect();
            entries.into_iter().collect::<Vec<_>>()
        };
        let mut constraints = self.constraints.clone();
        constraints.domain_denylist = self.deny.as_ref().map(canonical);
        constraints.domain_allowlist = self.allow.as_ref().map(canonical);
        constraints
    }

    /// Screen one provider's hits into `candidates` and describe the
    /// attempt.
    fn admit(
        &self,
        provider: &'static str,
        result: ResearchResult,
        candidates: &mut Vec<Candidate>,
    ) -> AttemptOutcome {
        let malformed_records = result.malformed_records;
        let hits = result.hits;
        let returned = hits.len();
        if returned == 0 && malformed_records == 0 {
            return AttemptOutcome::Empty;
        }
        let (mut by_domain, mut by_freshness, mut uncited) = (0, 0, 0);
        for hit in hits {
            let Some(primary) = hit.citations.first() else {
                uncited += 1;
                continue;
            };
            if !self.domain_allows(&hit.url) {
                by_domain += 1;
                continue;
            }
            let decision = self.constraints.evaluate_freshness(primary, self.now);
            if !decision.accepted {
                by_freshness += 1;
                continue;
            }
            candidates.push(Candidate::new(provider, hit.with_freshness(decision)));
        }
        AttemptOutcome::Answered {
            returned,
            rejected_by_freshness: by_freshness,
            rejected_by_domain: by_domain,
            rejected_uncited: uncited,
            malformed_records,
        }
    }

    /// Deny list first, then allow list, as `check_url` orders them. A URL
    /// with no host cannot match an allow list.
    fn domain_allows(&self, url: &Url) -> bool {
        let host = url.host();
        if let (Some(rules), Some(host)) = (&self.deny, &host) {
            if rules.iter().any(|rule| rule.matches(host)) {
                return false;
            }
        }
        match (&self.allow, &host) {
            (Some(rules), Some(host)) => rules.iter().any(|rule| rule.matches(host)),
            (Some(_), None) => false,
            (None, _) => true,
        }
    }
}

/// Stable identities a hit declares, normalized for comparison.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Identity {
    doi: Option<String>,
    arxiv_id: Option<String>,
    s2_paper_id: Option<String>,
    /// `<host>:<pageid>`: a page id is unique only within one wiki.
    pageid: Option<String>,
}

impl Identity {
    fn of(hit: &ResultHit) -> Self {
        let text = |key: &str| {
            hit.metadata
                .get(key)
                .and_then(Value::as_str)
                .map(|v| v.trim().to_lowercase())
                .filter(|v| !v.is_empty())
        };
        let pageid = hit.metadata.get(META_PAGEID).and_then(|v| match v {
            Value::Number(n) => Some(n.to_string()),
            Value::String(s) if !s.trim().is_empty() => Some(s.trim().to_owned()),
            _ => None,
        });
        let mut ids = Self {
            doi: None,
            arxiv_id: text(META_ARXIV_ID)
                .as_deref()
                .and_then(normalize_arxiv_id)
                .map(|(id, _version)| id),
            s2_paper_id: text(META_S2_PAPER_ID),
            pageid: pageid.map(|id| format!("{}:{id}", hit.url.host_str().unwrap_or_default())),
        };
        // WHY: an arXiv-registered DOI names the arXiv record, so it is that
        // record's arXiv identity; comparing it as a DOI would set it
        // against the publisher DOI of the same work.
        for key in [META_DOI, META_ARXIV_DOI] {
            match text(key).as_deref().and_then(classify_doi) {
                Some(DoiIdentity::Publisher(doi)) => {
                    ids.doi.get_or_insert(doi);
                }
                Some(DoiIdentity::Arxiv { id, .. }) => {
                    ids.arxiv_id.get_or_insert(id);
                }
                None => {}
            }
        }
        ids
    }

    fn kinds(&self) -> [Option<&String>; 4] {
        [
            self.doi.as_ref(),
            self.arxiv_id.as_ref(),
            self.s2_paper_id.as_ref(),
            self.pageid.as_ref(),
        ]
    }

    /// Whether any identity kind both declare is equal.
    fn shares_any(&self, other: &Self) -> bool {
        self.kinds()
            .iter()
            .zip(other.kinds())
            .any(|(a, b)| matches!((a, b), (Some(a), Some(b)) if *a == b))
    }

    /// Whether any identity kind both declare differs.
    fn conflicts(&self, other: &Self) -> bool {
        self.kinds()
            .iter()
            .zip(other.kinds())
            .any(|(a, b)| matches!((a, b), (Some(a), Some(b)) if *a != b))
    }

    /// Fill identities this record lacks from `other`.
    fn absorb(&mut self, other: &Self) {
        for (mine, theirs) in [
            (&mut self.doi, &other.doi),
            (&mut self.arxiv_id, &other.arxiv_id),
            (&mut self.s2_paper_id, &other.s2_paper_id),
            (&mut self.pageid, &other.pageid),
        ] {
            if mine.is_none() {
                mine.clone_from(theirs);
            }
        }
    }
}

/// A screened hit and what merging compares it by.
struct Candidate {
    provider: &'static str,
    hit: ResultHit,
    ids: Identity,
    title: String,
    year: Option<i64>,
    families: BTreeSet<String>,
}

impl Candidate {
    fn new(provider: &'static str, hit: ResultHit) -> Self {
        let families = hit
            .metadata
            .get(META_AUTHORS)
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .filter_map(family_name)
            .collect();
        Self {
            provider,
            ids: Identity::of(&hit),
            title: title_key(&hit.title),
            year: hit.metadata.get(META_YEAR).and_then(Value::as_i64),
            families,
            hit,
        }
    }

    /// Whether the two records name at least one author family in common.
    fn shares_an_author(&self, other: &Self) -> bool {
        !self.families.is_disjoint(&other.families)
    }
}

/// A merged record: the first candidate plus everything absorbed into it.
struct Merged {
    first: Candidate,
    absorbed: Vec<Value>,
    conflicts_with: Vec<String>,
}

impl Merged {
    fn absorb(&mut self, other: Candidate) {
        let Candidate {
            provider,
            hit: other_hit,
            ids,
            year,
            families,
            ..
        } = other;
        self.first.ids.absorb(&ids);
        if self.first.year.is_none() {
            self.first.year = year;
        }
        self.first.families.extend(families);
        let hit = &mut self.first.hit;
        for citation in other_hit.citations {
            if !hit.citations.contains(&citation) {
                hit.citations.push(citation);
            }
        }
        if cmp_score(other_hit.score, hit.score).is_gt() {
            hit.score = other_hit.score;
        }
        let mut record = serde_json::Map::new();
        record.insert("provider".to_owned(), Value::from(provider));
        record.insert("url".to_owned(), Value::from(other_hit.url.as_str()));
        record.insert("title".to_owned(), Value::from(other_hit.title));
        record.insert(
            "metadata".to_owned(),
            Value::Object(other_hit.metadata.into_iter().collect()),
        );
        self.absorbed.push(Value::Object(record));
    }

    fn note_conflict(&mut self, url: &str) {
        if !self.conflicts_with.iter().any(|u| u == url) {
            self.conflicts_with.push(url.to_owned());
        }
    }

    fn into_hit(self) -> ResultHit {
        let Candidate {
            provider, hit, ids, ..
        } = self.first;
        let mut hit = hit.with_metadata(META_PROVIDER, Value::from(provider));
        for (key, value) in [
            (META_DOI, ids.doi),
            (META_ARXIV_ID, ids.arxiv_id),
            (META_S2_PAPER_ID, ids.s2_paper_id),
        ] {
            if let Some(value) = value {
                hit.metadata
                    .entry(key.to_owned())
                    .or_insert_with(|| Value::from(value));
            }
        }
        if !self.absorbed.is_empty() {
            hit = hit.with_metadata(META_MERGED_RECORDS, Value::from(self.absorbed));
        }
        if !self.conflicts_with.is_empty() {
            hit = hit.with_metadata(META_CONFLICTS_WITH, Value::from(self.conflicts_with));
        }
        hit
    }
}

/// Merge screened candidates, order by score, and cut to `max_results`.
fn merge(candidates: Vec<Candidate>, max_results: usize) -> Vec<ResultHit> {
    let mut merged: Vec<Merged> = Vec::new();
    for candidate in candidates {
        let (target, conflicting) = placement(&merged, &candidate);
        let index = if let Some(record) = target.and_then(|i| merged.get_mut(i)) {
            record.absorb(candidate);
            target
        } else {
            merged.push(Merged {
                first: candidate,
                absorbed: Vec::new(),
                conflicts_with: Vec::new(),
            });
            merged.len().checked_sub(1)
        };
        if let Some(index) = index {
            for other in conflicting {
                mark_conflict(&mut merged, index, other);
            }
        }
    }
    let mut hits: Vec<ResultHit> = merged.into_iter().map(Merged::into_hit).collect();
    hits.sort_by(|a, b| cmp_score(b.score, a.score));
    hits.truncate(max_results);
    hits
}

/// Where `candidate` goes: the record it merges into, if any, and the
/// records it resembles but disagrees with. Stable identities decide
/// first; titles are consulted only when no identity matched.
fn placement(merged: &[Merged], candidate: &Candidate) -> (Option<usize>, Vec<usize>) {
    let mut target = None;
    let mut conflicting = Vec::new();
    for (i, record) in merged.iter().enumerate() {
        if record.first.ids.shares_any(&candidate.ids) {
            if record.first.ids.conflicts(&candidate.ids) {
                conflicting.push(i);
            } else if target.is_none() {
                target = Some(i);
            }
        }
    }
    if target.is_some() || !conflicting.is_empty() || candidate.title.is_empty() {
        return (target, conflicting);
    }
    for (i, record) in merged.iter().enumerate() {
        let first = &record.first;
        if first.title != candidate.title {
            continue;
        }
        match (first.year, candidate.year) {
            (Some(a), Some(b))
                if a == b
                    && first.shares_an_author(candidate)
                    && !first.ids.conflicts(&candidate.ids) =>
            {
                target = target.or(Some(i));
            }
            (Some(_), Some(_)) => conflicting.push(i),
            // WHY: without a year on both sides a title match is no
            // evidence either way, so the records stay apart unmarked.
            _ => {}
        }
    }
    (target, conflicting)
}

fn mark_conflict(merged: &mut [Merged], a: usize, b: usize) {
    let url_of = |merged: &[Merged], i: usize| {
        merged
            .get(i)
            .map(|record| record.first.hit.url.as_str().to_owned())
    };
    let (Some(url_a), Some(url_b)) = (url_of(merged, a), url_of(merged, b)) else {
        return;
    };
    if let Some(record) = merged.get_mut(a) {
        record.note_conflict(&url_b);
    }
    if let Some(record) = merged.get_mut(b) {
        record.note_conflict(&url_a);
    }
}

/// Name suffixes that follow a family name rather than being one.
const NAME_SUFFIXES: [&str; 5] = ["jr", "sr", "ii", "iii", "iv"];

/// An author's family name, normalized like a title: the part before the
/// first comma (`Alpha, Alice`), or else the last word (`Alice Alpha`),
/// with a trailing generational suffix skipped. `None` for a name with no
/// letters or digits.
fn family_name(author: &str) -> Option<String> {
    let family = if let Some((family, _given)) = author.split_once(',') {
        title_key(family)
    } else {
        let mut words = author
            .split_whitespace()
            .rev()
            .map(title_key)
            .filter(|word| !word.is_empty());
        let last = words.next()?;
        if NAME_SUFFIXES.contains(&last.as_str()) {
            words.next().unwrap_or(last)
        } else {
            last
        }
    };
    Some(family).filter(|family| !family.is_empty())
}

/// Case-folded title with punctuation removed and whitespace collapsed.
fn title_key(title: &str) -> String {
    let folded: String = title
        .chars()
        .flat_map(char::to_lowercase)
        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .collect();
    collapse_whitespace(&folded)
}

/// `sha256:<hex>` over the domain-separated, length-prefixed query, shape,
/// and constraints.
fn cache_key(query: &str, shape: QueryShape, constraints: &SearchConstraints) -> Result<String> {
    let constraints = serde_json::to_vec(constraints).map_err(|e| {
        InvalidConstraintSnafu {
            field: "constraints",
            reason: format!("cannot be encoded for the cache key: {e}"),
        }
        .build()
    })?;
    let mut hasher = Sha256::new();
    hasher.update(CACHE_KEY_DOMAIN);
    for part in [query.as_bytes(), shape.as_str().as_bytes(), &constraints] {
        let len = u64::try_from(part.len()).unwrap_or(u64::MAX);
        hasher.update(&len.to_be_bytes());
        hasher.update(part);
    }
    Ok(format!("sha256:{}", hasher.finish_hex()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn title_key_folds_case_punctuation_and_whitespace() {
        assert_eq!(
            title_key("  Attention Is All You Need!  "),
            title_key("attention is all you need"),
            "case, trailing punctuation, and padding do not matter"
        );
        assert_eq!(
            title_key("BERT: Pre-training of\nDeep Bidirectional Transformers"),
            "bert pre training of deep bidirectional transformers",
            "punctuation becomes a word break"
        );
    }

    #[test]
    fn family_name_reads_both_name_orders_and_skips_suffixes() {
        for (author, family) in [
            ("Alice Alpha", Some("alpha")),
            ("ALPHA, A.", Some("alpha")),
            ("Martin Luther King Jr.", Some("king")),
            ("Eli Stand-in", Some("stand in")),
            ("Plato", Some("plato")),
            ("  ", None),
            ("-, A.", None),
        ] {
            assert_eq!(family_name(author).as_deref(), family, "{author:?}");
        }
    }

    #[test]
    fn cache_key_is_a_prefixed_sha256_hex_digest() {
        let key = cache_key("q", QueryShape::QuickFactual, &SearchConstraints::default()).unwrap();
        let hex = key.strip_prefix("sha256:").unwrap();
        assert_eq!(hex.len(), 64, "256 bits as hex");
        assert!(
            hex.chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
            "lowercase hex: {key}"
        );
    }

    #[test]
    fn cache_key_length_prefixes_prevent_boundary_collisions() {
        // WHY: without length prefixes, moving bytes between adjacent
        // parts could produce the same concatenation.
        let a = cache_key(
            "quick",
            QueryShape::QuickFactual,
            &SearchConstraints::default(),
        );
        let b = cache_key(
            "quickquick",
            QueryShape::QuickFactual,
            &SearchConstraints::default(),
        );
        assert_ne!(a.unwrap(), b.unwrap(), "different queries, different keys");
    }
}
