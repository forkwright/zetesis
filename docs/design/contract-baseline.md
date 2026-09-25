# Contract baseline

At the baseline revision, Zetesis is a typed Rust library with in-memory
mechanics. The type system enforces first-hop target validation, local-target
authority, citation presence at construction, and error classification.
Redirect validation, budget authority, provider behavior, and most task
lifecycle transitions are caller convention. No HTTP client, persistence,
cache, provider adapter, or model execution exists.

This document records that baseline, the source and API changes Phase 00 S1
and Phase 01 make, and the contracts later phases implement against. It is
licensed under [CC BY-NC-ND 4.0](../../LICENSE-DOCS).

## 1. Scope and baseline

| Item | Value |
|------|-------|
| Baseline revision | `a3a82b6dfa714a8a35e7df26956be3e0a5c398f5` on `main` (2026-09-16) |
| Workspace version | `0.0.6` |
| Public surface | 61 items re-exported by `crates/sylloge/src/lib.rs:55-78`, re-exported unchanged by `crates/zetesis/src/lib.rs:6-19`, plus the `steelman` (`elenkhos`) and `briefing` (`synopsis`) aliases at `crates/zetesis/src/lib.rs:5` and `:20` |
| Runtime dependencies of `sylloge` | `jiff`, `language-tags`, `serde`, `serde_json`, `snafu`, `url` (no HTTP, storage, or async runtime crate) |
| Consumer dependencies | None. No consumer declares a Cargo dependency on Zetesis at this revision. |
| Accepted consumer boundary | Dioptron `docs/design/zetesis-acquisition-boundary.md` |
| Trackers | Static acquisition: [zetesis#48](https://github.com/forkwright/zetesis/issues/48). Paid-research authorization: [zetesis#47](https://github.com/forkwright/zetesis/issues/47). |

Source citations use `file:line` at the baseline revision. Paths are relative
to `crates/sylloge/src/` unless a path is given.

> **Superseded by Phase 03 S2.** Phase 03 S2 added the `Router`, provider
> attempt receipts, and request builders and parsers for Semantic Scholar,
> arXiv, and Wikipedia. Cells marked *Phase 03 S2* in sections 3.1, 3.4,
> 3.8, and 6 state the contract after that change. Every other cell, and
> every `file:line` citation, describes the baseline revision.

## 2. Enforcement legend

| Status | Meaning |
|--------|---------|
| **Implemented** | Enforced at the baseline by the type system, a constructor, a deserializer, or a state machine, and exercised by a test. |
| **Planned (phase)** | Contract defined in this document; implemented in the named phase or tracked issue. Nothing enforces it yet. |
| **Convention only** | Documented expectation that nothing in the crate enforces. A caller or implementation can violate it and still compile and pass the tests. |

"Fixture" in a row means the implementation exists only to drive
deterministic tests. It is not evidence of live HTTP, persistence, or model
behavior.

## 3. Public API inventory

### 3.1 Traits and aliases

| Item | Enforced | Caller convention today | Status |
|------|----------|-------------------------|--------|
| `BoxFut` (`provider.rs:25`) | `Send` boxed future; keeps the three traits dyn-compatible. | None. | Implemented |
| `Provider` (`provider.rs:51-84`) | Object safety; `search(query: &str, constraints: &SearchConstraints)` signature (`provider.rs:79-83`). `publication_time_capability` defaults to `Unsupported` (`provider.rs:67-69`). *Phase 03 S2:* `query_shapes` defaults to none, so a provider that declares no shape is never routed. | Unique lowercase `name()` (`provider.rs:35-37`); honoring `max_results`, domain lists, language, and freshness; cancellation safety when the future is dropped (`provider.rs:44-50`). `search` receives no consumer identity, ledger handle, attempt identity, or credential reference, so it cannot authorize or charge anything. | Trait implemented; no provider adapter exists. *Phase 03 S2:* request builders and parsers exist for the Tier-0 cohort; their adapters land with the transport wiring |
| `DeepResearch` (`deep.rs:52-94`) | Object safety; `submit`/`poll`/`fetch`/`cancel` signatures. | Every per-state rule in `deep.rs:25-51`: prompt `submit`, idempotent `poll`, `TaskNotReady` versus `TaskUnavailable` on `fetch`, idempotent `cancel`. `submit(query, depth)` (`deep.rs:63`) carries no budget, constraints, consumer identity, or idempotency key. | Trait implemented; `LocalDeepResearch` is the only implementation |
| `Crawler` (`crawler.rs:79-103`) | The request URL must be a `ValidatedTarget` (`crawler.rs:98-102`); a bare `Url` does not compile (compile-fail doctest, `crawler.rs:65-71`). | Checking every redirect target, propagating the check error, and connecting only to `ValidatedTarget::addrs` (`crawler.rs:25-45`, restated as prose-only at `crawler.rs:73-78`). The separate `constraints` argument need not match the constraints the target was validated under. The module doc names external extractors (Firecrawl, trafilatura) as implementation owners (`crawler.rs:5-8`), which the accepted boundary supersedes. | Convention only past the first hop; retired in Phase 01 S1 |
| `Resolver` (`net_policy.rs:225-236`) | Synchronous `resolve(host, port) -> io::Result<Vec<IpAddr>>`. Every returned address is still classified by the target policy, so a resolver can narrow but not widen what passes. | Offloading blocking implementations from an async executor. | Implemented |
| `SystemResolver` (`net_policy.rs:244-254`) | OS lookup through `ToSocketAddrs`. | Blocking DNS; async callers must use `spawn_blocking` (`net_policy.rs:238-243`). | Implemented |

### 3.2 Constraints and network-target policy

| Item | Enforced | Caller convention today | Status |
|------|----------|-------------------------|--------|
| `SearchConstraints` (`constraints.rs:33-74`) | `#[non_exhaustive]` blocks struct literals outside the crate; `deny_unknown_fields` rejects unknown keys, including the removed `allow_local_targets` flag (`constraints.rs:35`, test at `:820-825`). `language` parses as a BCP-47 tag. `Default` is 10 results with a free-only budget (`constraints.rs:152-161`). | All fields are `pub` and mutable. `max_results` accepts any `usize`, including 0. Domain entries are stored as given. An entry that normalizes to empty never matches (`constraints.rs:179-181`), so it is silently ignored in a denylist. Matching is ASCII-lowercase suffix comparison against the parsed host (`constraints.rs:166-196`), so a non-ASCII entry never matches the punycode host the URL parser produces, and IPv4 literal hosts match by trailing octets. | Implemented as a value; entry canonicalization changes in Phase 00 S1 (section 4.1) |
| `SearchConstraints::check_url`, `check_url_with` (`net_policy.rs:56-58`, `:102-104`) | Order: scheme in `{http, https}`, no userinfo, host present, resolution non-empty, no resolved address in a blocked range, denylist, allowlist (`net_policy.rs:130-216`). IPv4-mapped and IPv4-compatible IPv6 are classified as IPv4 (`net_policy.rs:374-381`). Resolver failure maps to transient `TransientIo`; every policy rejection maps to permanent `UnsafeTarget`. | Calling it on redirect targets. No port policy exists. Every resolver error is classified transient, including a deliberate refusal by a consumer's resolver wrapper. | Implemented for one URL |
| `ValidatedTarget` (`net_policy.rs:284-307`) | Private fields, `#[non_exhaustive]`, no serde, and no constructor other than the three `check_url*` methods; cannot be forged or retargeted (compile-fail doctest, `net_policy.rs:277-283`). `addrs()` is non-empty. | The proof carries no timestamp and no record of the constraints or authority it was issued under; a holder may keep it past the DNS answer's lifetime. | Implemented |
| `LocalTargetAuthorization` (`net_policy.rs:37-41`) | Private field, not `Clone`, no serde, no public constructor (compile-fail doctest, `net_policy.rs:33-36`). Bypasses address-range classification only; scheme, userinfo, resolution, and domain checks still apply (`net_policy.rs:117-121`). | No public mint exists, so `check_url_with_local_authorization` (`net_policy.rs:122-128`) is unreachable from other crates. It accepts only `SystemResolver`, so a consumer resolver wrapper cannot be combined with local authority. | Implemented; minting authority not defined |
| `PageContent` (`constraints.rs:353-540`) | Private fields; `new`, `with_extracted_text`, and the deserializer share the limits `MAX_URL_BYTES` 8 KiB, `MAX_CONTENT_TYPE_BYTES` 1 KiB, `MAX_BODY_BYTES` 10 MiB, `MAX_TEXT_BYTES` 4 MiB (`constraints.rs:391-403`). | The limits apply after the caller has already allocated the buffer; this is not streaming protection (`constraints.rs:340-346`). | Implemented; retired in Phase 01 S1 (the limit values move to `AcquisitionLimits` ceilings) |

### 3.3 Budget and spend

| Item | Enforced | Caller convention today | Status |
|------|----------|-------------------------|--------|
| `BudgetConstraint` (`budget.rs:199-224`) | `#[non_exhaustive]` plus `with_*` builders (`budget.rs:259-297`). `Default` is `free_only()`, which denies any non-zero spend (`budget.rs:229-237`, `:449-456`). A cap of `0` means the cap is disabled, not zero (`budget.rs:175-179`); only `allow_paid_tier = false` expresses zero spend. | Fields are `pub`. Nothing binds a constraint to an authenticated consumer; the caller presents whatever constraint it likes. | Implemented as arithmetic |
| `BudgetConstraint::phase_zero_default` (`budget.rs:239-257`) | Returns paid-enabled caps of $0.05 per query, $5 per rolling day, and $20 per-agent lifetime. | Its doc claims to match a Phase 0 requirement. Per the reopen comment on zetesis#47, the governing requirement named a $20 fleet-day cap, not a per-agent lifetime cap, and cited a different identifier. | Removed in Phase 00 S1 |
| `BudgetConstraint::permits` (`budget.rs:317-342`) | Pure check over query, consumer-day, and agent-lifetime scopes. | Not atomic with a later `SpendLedger::record` (`budget.rs:312-315`); does not check the fleet-day scope. | Implemented |
| `BudgetConstraint::try_reserve` (`budget.rs:376-446`) | Checks every scope in order and records into both ledgers only if all pass; a denial mutates neither ledger. Denial carries scope, remaining allowance, and reset time. | Atomic only for a caller holding `&mut` to both ledgers across the call (`budget.rs:354-362`). Single-phase: the recorded amount is final, has no identity, and cannot be reconciled to actual cost; nothing survives a restart (`budget.rs:363-368`). The test `try_reserve_concurrent_75_plus_75_never_exceeds_100_cap` makes two sequential calls on one `&mut` pair; it demonstrates ordering, not cross-thread or cross-process exclusion. | Implemented in memory; durable identity-bound ledger tracked by zetesis#47 |
| `SpendLedger` (`budget.rs:53-140`) | Private fields; `record` ignores zero and saturates (`budget.rs:69-80`); `prune_expired` keeps the lifetime total (`budget.rs:136-139`). | Derived `Deserialize` accepts a lifetime total below the sum of its events. The ledger is a caller-owned value, not an authority. | Implemented; malformed-record rejection added in Phase 00 S1 |
| `SpendEvent` (`budget.rs:36-43`) | `#[non_exhaustive]`. | `pub` fields. | Implemented |
| `BudgetScope` (`budget.rs:152-171`) | Names the violated scope: query, consumer-day, fleet-day, agent-lifetime, paid tier disabled. | None. | Implemented |
| `DAY_WINDOW` (`budget.rs:33`) | 24-hour rolling window. | None. | Implemented |
| `CostTracking` (`cost.rs:128-205`) | `add` and `from_line_items` sum duplicate providers with saturation (`cost.rs:145-167`); `BTreeMap` gives deterministic key order. | `by_provider` is `pub`; a map key can differ from its entry's `provider_id` through direct insertion or deserialization. It is a per-call report, not a charge. | Implemented; key/entry mismatch rejected on deserialize in Phase 00 S1 |
| `ProviderSpend` (`cost.rs:81-126`) | Unit is named micro-cents and defined as 1 USD = 10,000,000 units (`cost.rs:88-91`), which is 10^-7 USD, not 10^-8. | `pub` fields; free-tier unit meaning is provider-specific (`cost.rs:93-97`). | Implemented |
| `ProviderId` (`cost.rs:16-79`) | Transparent string. | No validation; provider owns the format. | Implemented |

### 3.4 Citation, result, and provenance

| Item | Enforced | Caller convention today | Status |
|------|----------|-------------------------|--------|
| `SourceKind` (`citation.rs:24-87`) | Closed `#[non_exhaustive]` set; `is_authoritative` is true for journal, legal, filing, and patent (`citation.rs:81-86`). | The doc comment at `citation.rs:78-79` omits patent. | Implemented |
| `Citation` (`citation.rs:98-182`) | `new` and the deserializer clamp `confidence` into `0.0..=1.0` and map NaN to 0 (`citation.rs:129`, `:146-166`). `published_at` defaults to `Unknown` at construction and on decode (`citation.rs:120-121`). | `pub` fields can be mutated after construction. `source_url` is any absolute URL; it is not a validated target. | Implemented |
| `ResultHit` (`result.rs:27-165`) | `new` and the deserializer require at least one citation (`result.rs:91-104`, `:172-184`) and clamp `score` (`result.rs:56`, `:105-109`). `with_full_text` caps `full_text` at 4 MiB (`result.rs:128-140`). | `full_text` has no cap on deserialize (`result.rs:44`). `pub` fields allow clearing `citations` after construction. Hit ordering is by convention (`result.rs:217-218`). | Implemented at construction and decode only |
| `ResearchResult` (`result.rs:206-304`) | Serializable envelope; `top_hit` ranks NaN last (`result.rs:281-283`). | *Phase 03 S2:* `Router` derives `cache_key` from the whitespace-normalized query, the shape, and the constraints with canonicalized domain lists, and records one attempt receipt per routed provider; `evidence_state` distinguishes answered, no evidence, and unanswered. A result built outside the router carries a caller-supplied key and may have empty provenance. No cache layer exists. No schema version. | Implemented as a value |
| `ProvenanceEntry` (`result.rs:306-326`) | *Phase 03 S2:* provider id plus an optional citation and an optional attempt receipt (route ordinal, tier, outcome: answered with drop counts, empty, failed with its error class, timed out, or refused); decoding rejects an entry with neither. | *Phase 03 S2:* no durable attempt identity or endpoint policy revision is recorded. | Implemented as a value |
| `QueryShape` (`query.rs:26-105`) | Closed set; `as_str` matches serde; `tolerates_stale_cache` (`query.rs:102-104`). | *Phase 03 S2:* `Router` routes each shape to the registered providers that declare it; a shape no provider declares is an explicit `Unsupported` error. No classifier exists. | Implemented as a value |
| `ProviderTier` (`tier.rs:14-73`) | Ordering, `is_paid` for Tier 1 and Tier 3 (`tier.rs:54-56`), `fallback_priority`. | *Phase 03 S2:* `Router` refuses every paid tier with a typed receipt, whatever the budget allows, until the durable ledger exists. The doc at `tier.rs:24-26` says Tier 1 is attempted after a Tier 0 miss, which contradicts current policy: paid tiers are disabled until explicitly configured and reserved, and a Tier-0 miss never enables them. | Implemented as a value |

### 3.5 Freshness

| Item | Enforced | Caller convention today | Status |
|------|----------|-------------------------|--------|
| `PublicationTime`, `PublicationPrecision`, `PublicationProvenance` (`freshness.rs:17-73`) | Retrieval time and publication time are separate types; default is `Unknown`. | None. | Implemented |
| `FreshnessPolicy` (`freshness.rs:78-91`) | Default `Strict` rejects unknown publication time. | None. | Implemented |
| `evaluate_freshness`, `SearchConstraints::evaluate_freshness` (`freshness.rs:157-190`, `constraints.rs:134-149`) | Pure, deterministic decision. | Nothing calls it automatically; enforcement happens only where a caller invokes it. | Implemented |
| `FreshnessDecision`, `FreshnessBasis` (`freshness.rs:94-128`) | Returned by `evaluate_freshness`. | `pub` fields and `Deserialize`, and `ResultHit::with_freshness` accepts any value, so an attached decision is caller-attested, not proof. | Implemented as a value |
| `PublicationTimeCapability` (`freshness.rs:132-145`) | Explicit `Unsupported` default on `Provider`. | Providers must override it truthfully. | Implemented |

### 3.6 Deep-research task types

| Item | Enforced | Caller convention today | Status |
|------|----------|-------------------------|--------|
| `TaskId` (`constraints.rs:198-233`) | Opaque string. | No validation; provider owns the format (`constraints.rs:203-208`). | Implemented as a value |
| `DeepDepth` (`constraints.rs:235-269`) | Closed set; default `Standard`. | A hint; backends may ignore it. The offline fixture maps it to 1-4 iterations (`fixture.rs:128-135`). | Implemented as a value |
| `ResearchStatus` (`constraints.rs:271-336`) | `running()` clamps progress to 100 (`constraints.rs:332-335`); `is_ready`, `is_terminal`. | The `Running` variant and the deserializer accept any `u8`; the field doc says "clamped on construction" (`constraints.rs:280`), which holds only for `running()`. | Implemented; values above 100 rejected on deserialize in Phase 00 S1 |
| `LocalDeepResearch` (`local_deep_research.rs:27-334`) | In-memory `Mutex<HashMap>`. `cancel_task` guards terminal states (`:117-140`). `execute_offline` claims only a pending task and commits only if still running (`:181-233`). Store holds at most 1024 tasks and applies transient backpressure when full of in-flight tasks (`:259-271`). Empty queries are rejected (`:252-257`). | `mark_running`, `complete_task`, and `fail_task` (`:66-104`) do not check the current state, so any caller can move a cancelled or ready task back to running, ready, or failed. Task ids come from a per-instance counter that restarts at 1 (`:273-274`), so they are not unique across instances or restarts. `submit` has no idempotency key. Eviction removes ready tasks whose results were never fetched (`:260-262`). `execute_offline` stamps `Timestamp::now()` (`:229`). Nothing survives process exit. | Implemented in memory; not a durable service |

### 3.7 Offline fixture

| Item | Enforced | Caller convention today | Status |
|------|----------|-------------------------|--------|
| `QueryGenerator`, `SourceRetriever`, `Synthesizer` (`fixture.rs:24-46`) | Synchronous seams for the five-node loop. | Determinism depends on the seam implementations. | Fixture |
| `OfflineFixture` (`fixture.rs:48-126`) | Runs generate, retrieve, summarize, reflect, finalize up to the depth cap; emits a synthesis hit only when at least one source citation exists (`fixture.rs:162-189`); records zero paid spend and one free unit per step (`fixture.rs:137-139`). | It calls no model and no network. Its `cache_key` format (`fixture.rs:116-122`) is a fixture convention, not a query identity. | Fixture |

### 3.8 Errors

| Item | Enforced | Caller convention today | Status |
|------|----------|-------------------------|--------|
| `Error` (`error.rs:25-287`) | Flat `#[non_exhaustive]` snafu enum with an implicit location on every variant; `Send + Sync + 'static` asserted at compile time (`error.rs:292-295`). | Context selectors are public (`error.rs:26`), so any crate can build any variant. `Error` is not serializable. | Implemented |
| `ErrorClass`, `Error::class` (`error.rs:297-366`) | Every variant maps to exactly one of transient, permanent, fatal. `BudgetExceeded` is transient only when it carries a reset time. *Phase 03 S2:* `ErrorClass` is serializable (snake case) for attempt receipts. | None. | Implemented |
| `Result` (`error.rs:19`) and the 17 `*Snafu` selectors re-exported at `lib.rs:61-67` | Construction paths for the variants above. | None. | Implemented |

### 3.9 Existing tests are starting fixtures

| Test file | What it demonstrates | What it does not demonstrate |
|-----------|----------------------|------------------------------|
| `crates/sylloge/tests/trait_object_safety.rs` | The three traits work through `Arc<dyn _>`; a stub crawler that follows the redirect convention rejects a metadata-address redirect. | Any real HTTP client, redirect handling inside a transport, or connection binding. |
| `crates/sylloge/tests/constraint_composition.rs` | Builder composition, serde round trip, empty allowlist denies everything, freshness through constraints. | Canonical domain entries or rejection of unusable entries. |
| `crates/sylloge/tests/budget_reservation.rs` | `try_reserve` ordering, fleet scope, denial without mutation, typed denial data. | Cross-thread or cross-process exclusion, durability, reservation identity, retry after a lost response. |
| `crates/sylloge/tests/cross_type_invariants.rs` | Cost, citation, tier, and shape values compose; CBOR round trip of metadata. | Routing or cache behavior. |
| `crates/sylloge/tests/error_classification.rs` | Every listed variant classifies to exactly one class; selectors are reachable. | Classification of acquisition failures, which do not exist yet. |
| `crates/sylloge/tests/local_deep_research.rs` | In-memory lifecycle, cancellation rules, backpressure, offline loop output. | Models, search, persistence, restart behavior, or guarded `mark_running`/`complete_task`/`fail_task`. |
| Compile-fail doctests (`crawler.rs:65-71`, `net_policy.rs:33-36`, `net_policy.rs:277-283`, `constraints.rs:347-352`) | Forgery and mutation paths do not compile. `crates/zetesis/tests/docs_consistency.rs` asserts that the gate runs `cargo test --workspace --doc`. | Runtime behavior. |

### 3.10 Source documentation drift

These Rust doc comments disagreed with the accepted boundaries at the
baseline revision. The last column names the change that corrects each.

| Location | Drift | Corrected by |
|----------|-------|--------------|
| `crawler.rs:1-8`, `lib.rs:14-15`, `constraints.rs:1-13` | `crawler.rs` names external extractors (Firecrawl, trafilatura) as the implementations; the other two describe the `Crawler` surface. The accepted boundary makes Zetesis the owner of anonymous bounded static transfer and extraction. | Phase 01 S1 (`Crawler` and `PageContent` retired) |
| `tier.rs:3-6`, `tier.rs:24-26` | Describe a router that walks tiers until budget runs out and tries Tier 1 after a Tier 0 miss. | This change |
| `provider.rs:4-6` | Stale note about how Tier 0 providers were to be dispatched. | This change |
| `deep.rs:3-5` | Lists hosted and paid deep-research products as expected backends without the authorization boundary. | This change |
| `query.rs` variant docs | Claim routes to paid providers (Brave, Tavily, Exa) that no router implements. | This change |
| `budget.rs:195-198`, `budget.rs:239-244`, `budget.rs:451` | Point callers to `phase_zero_default`. | This change |
| `constraints.rs:280`, `citation.rs:78-79` | Clamp and authoritative-set wording narrower or wider than the code. | This change |

## 4. API delta

### 4.1 Phase 00 S1 source changes (same change set as this document)

| Change | Reason |
|--------|--------|
| Remove `BudgetConstraint::phase_zero_default` and every doc reference to it. Tests that used it construct their budgets explicitly. | It enabled paid routing with caps ($0.05 per query, $5 per day, $20 lifetime) that cite the wrong requirement and the wrong $20 scope (zetesis#47 reopen comment). Paid use must be explicitly configured, not reached through a named default. |
| Canonicalize domain allow and deny entries at the constraint boundary and reject unusable entries with a new permanent `Error::InvalidConstraint`. | Today an entry that normalizes to empty, or that can never match the host form the URL parser produces (for example a non-ASCII entry against a punycode host), is silently ignored. In a denylist that fails open. |
| Reject malformed persisted records on deserialize: `ResearchStatus::Running` with progress above 100, `CostTracking` whose map key differs from its entry's `provider_id`, `SpendLedger` whose lifetime total is below the sum of its events or that holds a zero-spend event, and `ResultHit::full_text` longer than `MAX_FULL_TEXT_BYTES`. | Deserialization is a second construction path that currently bypasses the invariants the constructors hold. |
| Add golden serialized fixtures for the public types a consumer may persist, and tests for malformed, overflow, and invalid-constraint input in `sylloge`. Unknown-version tests belong to the first versioned type, the evidence envelope (Phase 01 S2). | A changed encoding must fail a test instead of silently changing stored data. |

### 4.2 Phase 01 (zetesis#48)

| Slice | Change |
|-------|--------|
| S1 | Add module `sylloge::acquisition`, re-exported through `zetesis`: one concrete `StaticAcquirer`, caller-supplied `AcquisitionLimits` validated against crate ceilings, the public `Connector` seam with `DirectConnector`, and hop records. Transport is private: one hop per request over HTTP/1.1, no connection pool, no proxy environment variables, no automatic redirects, no automatic decompression, TLS through `rustls` with a per-client provider (no process-global state). Remove `Crawler` and `PageContent`. |
| S2 | Add evidence envelope v1 (section 7), fingerprint, `replay`, `gzip` and `deflate` content decoding, and the `zetesis.html_text` extractor. |

### 4.3 Later work

| Work | Where |
|------|-------|
| Durable, identity-bound, fleet-scoped paid-spend ledger with idempotent reserve, settle, and release; authorization carried through both `Provider` and `DeepResearch` | zetesis#47 |
| Storage primitive selection for the ledger and the cache | Phase 03 S1 |
| Deep-inquiry loop against a consumer-granted model contract; resource dimensions for iterations, sources, and tokens; durable task identity | Phase 05 |
| Retrospective (`elenkhos`) and briefing (`synopsis`) behavior | Not scheduled; both crates are marker types today |

## 5. Contract ownership

### 5.1 Dependency direction

- Zetesis depends on no consumer crate. Its workspace dependencies are
  general-purpose libraries only (`Cargo.toml`).
- Consumers (Aletheia, Dioptron, Akroasis) depend on Zetesis by pinning a
  merged, immutable Zetesis commit SHA that Kanon registers or derives. No
  consumer pins Zetesis at the baseline.
- Where Zetesis needs consumer policy, it defines a trait or value type and
  the consumer supplies the implementation at call time (`Resolver` today;
  `Connector`, credential-reference resolution, and the model contract when
  they land). The Cargo edge still points from consumer to Zetesis.
- Logismos (model execution), Tropos (host modes), and Kanon (gate, SHA
  registration, federated tool surfaces) are reached through service or
  registration contracts. Zetesis has no Cargo dependency on them and they
  need none on Zetesis, so no cycle can form.

### 5.2 Ownership table

| Contract | Owner | Zetesis provides | Consumer supplies | Status |
|----------|-------|------------------|-------------------|--------|
| Static acquisition | Zetesis | Target and per-hop validation, one-hop transport, bounded transfer and decoding, static text extraction, evidence envelope, fingerprint, replay | Invocation authority and its own resource reservation, `AcquisitionLimits`, `Resolver` and `Connector` adapters for its egress policy, body custody, verbatim envelope storage. Dioptron keeps sessions, rendering, scripted browsing, and browser actions. | Planned (Phase 01) |
| Provider search | Zetesis | `Provider` trait, normalized `ResearchResult`, citations and provenance, cost report, error class | Query, constraints, consumer scope identifier, credential references, explicit paid authorization | Trait implemented; adapters planned |
| Budget and paid-spend ledger | Zetesis for paid provider spend; each consumer for its own invocation budget | Budget arithmetic today; the durable reservation ledger and its identities when zetesis#47 lands | Authenticated consumer identity, configured caps, the reservation of its own authority (for example Dioptron's invocation budget) | Arithmetic implemented in memory; ledger planned (zetesis#47) |
| Cache | Zetesis | Keyed by query and attempt identity with per-provider freshness windows; never stores secret values | Nothing beyond scope identifiers | Planned (storage in Phase 03 S1) |
| Deep-inquiry model execution | Logismos executes; Zetesis orchestrates | The loop, the task lifecycle, and the model request shape | A model grant: which model binding the loop may use, under which budget. A missing or refused grant fails the task; there is no implicit fallback. | Planned (Phase 05) |
| Research-task lifecycle | Zetesis | `DeepResearch` trait; in-memory `LocalDeepResearch`; durable task identity later | Idempotency key and consumer scope at submit; polling | Trait implemented; durable service planned |
| Retrospective (steel-manning) | Zetesis (`elenkhos`) | Reserved crate boundary | Claims to review | Marker type only |
| Briefing | Zetesis (`synopsis`) | Reserved crate boundary | Audience and delivery | Marker type only |
| Credentials | Operator vault or consumer | Accepts credential references and resolves them per call through a consumer-supplied resolver; never stores, caches, logs, or keys on a value | Credential values, rotation, scope | No credential-carrying type exists |
| Egress | Consumer egress policy; Zetesis target policy | Fail-closed target classification that only `LocalTargetAuthorization` can relax | Stricter policy through `Resolver` (refuse before lookup) and `Connector` (refuse to connect) wrappers | Target policy implemented; `Connector` planned (Phase 01 S1) |
| Knowledge admission | Consumer | Cited results and envelopes as candidates | Classification, admission, retention, confirmation | Consumer-owned |
| Host modes | Tropos | Nothing; Zetesis never selects a host or switches GPU modes | Not applicable | Outside Zetesis |
| Tool federation | Kanon registration plus the consumer agent runtime | A library API | Any tool surface as a registered surface declaration with a pinned manifest | No Zetesis tool surface exists |

### 5.3 Consumer neutrality and explicit refusal

- `SearchConstraints`, `AcquisitionLimits`, and the evidence envelope carry no
  tenant, grant, session, or storage-tier fields. Consumer context enters only
  as opaque scope identifiers in the identities of section 6, and sits beside
  the envelope, never inside it.
- An unsupported capability is a typed refusal, never a silent downgrade or a
  fallback route: `Error::Unsupported` (permanent) for an operation a backend
  does not offer; `BudgetExceeded { scope: PaidTierDisabled }` for paid use
  without authorization; `PublicationTimeCapability::Unsupported` for a
  provider without publication times; a `Partial` or `Failed` envelope
  outcome for content or transfer Zetesis will not handle.
- A consumer adapter treats an unsupported or incompatible Zetesis result as
  an adapter failure. It does not route to a second local implementation
  (Dioptron boundary, "Adapter exclusion").

## 6. Identity and idempotency

No durable identity exists at the baseline. The table defines each identity
separately so that no two share an idempotency rule by accident.

| Identity | Composition | Minted | Idempotency rule | Baseline |
|----------|-------------|--------|------------------|----------|
| Query | Consumer scope id, normalized query, `QueryShape`, constraint digest, tenant, privacy, and egress scope identifiers | Computed deterministically from the request | Same inputs give the same identity; the cache key derives from it | `ResearchResult::cache_key` is a caller-supplied string (`result.rs:229-233`). *Phase 03 S2:* the router derives it from the normalized query, shape, and canonicalized constraints, without consumer or scope identifiers |
| Provider attempt | Query identity, provider id, endpoint policy revision, attempt ordinal | Durably, before the upstream call | A retry after an unknown outcome reuses the attempt identity | None; provenance and cost carry only a provider id. *Phase 03 S2:* router receipts carry provider id, route ordinal, tier, and outcome; no durable identity or endpoint policy revision |
| Reservation | Minted by the ledger, keyed by attempt identity | At reserve | Reserve is idempotent per attempt identity: a second reserve returns the first. Settle and release are idempotent and terminal. Unknown upstream billing is recorded as unknown, never as zero. | `try_reserve` records an anonymous amount (`budget.rs:363-368`) |
| Task | Minted by the research service at submit, bound to a consumer-supplied idempotency key | At submit | A resubmission with the same key after a lost response returns the same task | Provider-owned `TaskId`; per-instance counter in `LocalDeepResearch` |
| Consumer emission | Task or query identity, consumer id, sink id, content digest | At emission | Sink delivery is idempotent on this identity; a fact emission stays a candidate until the consumer confirms it | None |
| Static acquisition observation | The envelope fingerprint identifies content plus transformation | Each `acquire` call is a new observation | Zetesis charges nothing for anonymous GET. The consumer's invocation id and reservation live beside the envelope, never inside it. | None (Phase 01 S2) |

Consequences:

- A retry cannot charge a second reservation: it reuses the attempt
  identity, and reserve is idempotent on that identity.
- A retry cannot create a false second fact: emission is idempotent on its
  identity, and two observations with one fingerprint are two observations
  of the same content, not two facts.
- The constraint digest is computed over the canonicalized constraint value,
  so entries that differ only in spelling that canonicalization removes
  produce one digest. Query normalization rules are fixed and versioned when
  query identity is implemented; changing them changes identities.
- No identity contains a credential value or egress policy content. Only
  references and identifiers enter.

## 7. Evidence envelope v1

Phase 01 S2 implements the envelope; Phase 01 S1 produces the hop records it
contains. Field names are `snake_case` in JSON and CBOR. Only the outermost
record carries a version; nested value types are unversioned and change only
through a new envelope version.

### 7.1 Fields

`EvidenceEnvelope`

| Field | Type | Meaning |
|-------|------|---------|
| `schema` | string, constant `zetesis.static_acquisition` | Schema identifier |
| `schema_version` | `u32`, `1` | Readers reject versions they do not know |
| `producer` | `{ package: "sylloge", version }` | Producing package and its `CARGO_PKG_VERSION` |
| `requested_url` | URL | The caller's target |
| `final_url` | optional URL | URL of the accepted final response; absent when none was accepted |
| `started_at`, `completed_at` | timestamp | Operation bounds |
| `limits` | `AcquisitionLimits` | The exact limit profile applied |
| `hops` | list of `HopRecord` | In order, starting with the requested URL |
| `response` | optional `ResponseRecord` | The final hop's accepted response |
| `body` | optional `BodyRecord` | Sizes and digests; body bytes are not included |
| `extraction` | optional `ExtractionRecord` | Static text extraction result |
| `outcome` | `complete`, `partial { reason }`, or `failed { failure }` | Section 7.2 |
| `fingerprint` | string `sha256:<hex>`, lowercase hex | Section 7.3 |

`HopRecord`

| Field | Type | Meaning |
|-------|------|---------|
| `url` | URL | This hop's URL |
| `resolved` | list of IP addresses | The validated address set for this hop |
| `connect_attempts` | list of `{ addr, result }` | `addr` is a socket address; `result` is `connected`, `denied`, `refused`, `timed_out`, or `error` |
| `tls` | optional `{ protocol_version, server_name, peer_leaf_sha256 }` | TLS facts for this hop |
| `status` | optional `u16` | HTTP status |
| `location` | optional string | Raw `Location` header value |

`ResponseRecord`: `status`, `content_type`, `content_encoding` (list),
`content_length` (optional `u64`), `last_modified`, `etag`, `date`. Selected
headers only; `Set-Cookie` is never recorded.

`BodyRecord`: `wire_bytes`, `wire_sha256`, `decoded_bytes`, `decoded_sha256`,
`complete` (boolean).

`ExtractionRecord`: `extractor` (`{ id: "zetesis.html_text", version: 1 }`),
`media`, `charset` (`{ label, source }` where `source` is `header`, `bom`,
`meta`, or `assumed_utf8`), `source_text_sha256`, `text_sha256`, `text_bytes`,
`segments` (list of `{ start, end, text }`, byte spans into the decoded UTF-8
source), `truncated` (boolean).

### 7.2 Outcomes

| Outcome | When | Envelope |
|---------|------|----------|
| `complete` | Transfer and extraction finished within limits | Yes |
| `partial { reason }` | Transfer succeeded but content could not be fully handled. Reasons: `text_limit_reached`, `unsupported_charset`, `invalid_encoding`, `binary_content`, `empty_body`, `no_static_text` (the document has scripts but no extractable text; not a claim that scripts are required). | Yes |
| `failed { failure }` | A policy or transport failure after at least the initial validation began | Yes, with the hop evidence gathered so far, so the consumer can persist what was attempted |
| `Err(Error)` from `acquire` | Invalid caller input that produced no attempt, such as limits above the crate ceilings | No |
| Cancellation | The caller dropped the future | No (section 10) |

`AcquisitionFailure` kinds and their class:

| Failure | Meaning | Class |
|---------|---------|-------|
| `unsafe_target` | Target policy rejected a hop (userinfo, missing host, blocked address, domain list) | Permanent |
| `scheme_not_allowed` | Hop scheme outside the caller's allowed set | Permanent |
| `downgrade_refused` | `https` to `http` redirect without explicit allowance | Permanent |
| `denied_port` | Port on the Fetch standard's bad-port list | Permanent |
| `redirect_limit` | More redirects than the caller's limit | Permanent |
| `redirect_loop` | A URL repeated within one chain | Permanent |
| `malformed_redirect` | `Location` does not resolve against the current URL | Permanent |
| `resolution_failed` | The resolver could not produce addresses | Transient |
| `egress_denied` | A consumer `Resolver` or `Connector` refused by policy | Permanent |
| `connect_failed` | Connection error or refusal | Transient |
| `connect_timeout` | Per-connect timeout elapsed | Transient |
| `tls_failed` | TLS handshake or certificate verification failed | Permanent |
| `deadline_exceeded` | Whole-operation deadline elapsed | Transient |
| `http_protocol` | Malformed HTTP response | Transient |
| `header_limit` | Response header section exceeded its bound | Permanent |
| `unsupported_content_encoding` | Encoding outside the supported set | Permanent |
| `unsupported_content_type` | Media type outside the supported set | Permanent |
| `wire_limit` | Wire bytes exceeded the limit | Permanent |
| `decoded_limit` | Decoded bytes exceeded the limit | Permanent |
| `interrupted_stream` | The body stream ended or reset before completion | Transient |

Permanent means the same request under the same limits and authority cannot
succeed. Raising a limit within the ceilings is a new request, not a retry.
No acquisition failure is fatal; fatal stays reserved for corruption of
Zetesis-owned state (`FatalCorruption`).

### 7.3 Fingerprint

`fingerprint` is SHA-256 over a domain-separated, length-prefixed encoding of,
in order: schema id, `schema_version`, `requested_url`, `final_url`,
`decoded_sha256`, extractor id, extractor version, `text_sha256`, and the
outcome kind (`complete`, `partial`, `failed`). An absent optional value has
its own marker, distinct from an empty value. Timestamps, addresses, and TLS
details are excluded because they change on every fetch. Same content plus
same transformation gives the same fingerprint. Phase 01 S2 fixes the exact
byte layout and pins it with a golden fixture.

### 7.4 Body custody and replay

- `acquire` returns `Acquisition { envelope, body }`. The body is the decoded
  bytes. The consumer stores it in its own custody keyed by `decoded_sha256`;
  Zetesis keeps no copy.
- The consumer stores the envelope verbatim. Derived indexes may reference it
  but do not replace its bytes, schema identity, producer version, or
  fingerprint (Dioptron boundary, "Adapter protocol").
- `replay(envelope, body)` re-runs the transformation and reports one of:

| Result | Meaning |
|--------|---------|
| `Reproduced` | Body digest matches and re-extraction yields the recorded text digest |
| `VersionMismatch` | The envelope's schema or extractor version is not the one this build implements |
| `DigestMismatch` | The supplied body does not hash to `decoded_sha256` (a custody failure) |
| `ExtractionDrift` | Same versions and same body, different extraction output (a determinism defect) |

## 8. Schema evolution

- Producers emit only the current `schema_version`.
- Readers accept exactly the versions they know and return a typed
  unsupported-version error otherwise. No best-effort decoding of an unknown
  version.
- Any added, removed, renamed, or retyped field, or a changed meaning, is a
  new `schema_version`. Because of that, a reader rejects unknown fields
  inside a known version.
- The extractor carries its own version. A change to extraction output bumps
  the extractor version, not the schema version.
- JSON and CBOR encodings share field names and version semantics.
- Each version has checked-in golden fixtures that never change after
  release.
- A stored envelope is never rewritten to a newer version; consumers keep
  the verbatim original.
- The existing result types (`ResearchResult`, `ResultHit`, `Citation`) carry
  no version. Their evolution so far added defaulted fields
  (`citation.rs:120`, `result.rs:70`). That lenient rule does not apply to the
  evidence envelope.

## 9. Resource dimensions

`AcquisitionLimits` is caller-supplied and validated against crate ceilings.
There are no invented policy defaults. Ceilings come only from landed code or
external standards.

| Operation | Dimension | Unit | Ceiling source | Status |
|-----------|-----------|------|----------------|--------|
| Acquisition | Redirects | count | 20 (Fetch standard redirect limit) | Planned (Phase 01 S1) |
| Acquisition | Connect timeout | duration per attempt | Caller-supplied; no crate ceiling has a landed or external source | Planned (Phase 01 S1) |
| Acquisition | Whole-operation deadline | duration | Caller-supplied; no crate ceiling has a landed or external source | Planned (Phase 01 S1) |
| Acquisition | Header bytes | bytes | HTTP client buffer bound (`hyper` `max_buf_size`) | Planned (Phase 01 S1) |
| Acquisition | Wire bytes | bytes | 10 MiB (`PageContent::MAX_BODY_BYTES`, `constraints.rs:400`) | Planned (Phase 01 S1) |
| Acquisition | Decoded bytes | bytes | 10 MiB (same source) | Planned (Phase 01 S2) |
| Acquisition | Text bytes | bytes | 4 MiB (`PageContent::MAX_TEXT_BYTES`, `constraints.rs:403`) | Planned (Phase 01 S2) |
| Acquisition | URL bytes | bytes | 8 KiB (`PageContent::MAX_URL_BYTES`, `constraints.rs:391`) | Planned (Phase 01 S1) |
| Provider search | Requests | count (`ProviderSpend::request_count`, `u32`) | Provider quota | Recorded per call; not enforced |
| Provider search | Free-tier units | provider-defined (`ProviderSpend::free_tier_units`, `u64`) | Provider quota | Recorded per call; not enforced |
| Provider search | Paid spend | micro-units of USD as landed (`u64`, 1 USD = 10,000,000, `cost.rs:88-91`) | Configured caps | Checked in memory; authority planned (zetesis#47) |
| Deep inquiry | Iterations, sources, tokens | Defined in Phase 05 | Defined in Phase 05 | Planned (Phase 05) |

## 10. Cancellation

| Operation | Rule | Status |
|-----------|------|--------|
| Static acquisition | Dropping the `acquire` future cancels all work for that call; no background task survives; no envelope is produced. Deadline expiry is not cancellation: it yields `failed { deadline_exceeded }` with evidence. The consumer settles or releases its own reservation. | Planned (Phase 01 S1) |
| Provider search | Dropping `search` must not leak partial results or unrecorded spend (`provider.rs:44-50`). Once attempt identity exists, a drop after the request was sent is an unknown outcome; the ledger records it as unknown and a retry reuses the attempt identity. | Convention only; attempt identity planned |
| Deep research, future | Dropping `submit` or `poll` is safe; dropping `fetch` is safe unless the backend deletes on retrieval (`deep.rs:44-51`). | Convention only |
| Deep research, task | `cancel` is idempotent for pending, running, and cancelled tasks and refuses ready or failed tasks (`deep.rs:37-42`). `LocalDeepResearch::cancel_task` implements this; a cancelled task's in-flight offline result is discarded (`local_deep_research.rs:215-227`). | Implemented in `LocalDeepResearch` only |
| Deep inquiry model calls | Task cancellation propagates to model execution through the model contract. | Planned (Phase 05) |

## 11. Error classification

The landed mapping (`error.rs:335-365`):

| Class | Variants |
|-------|----------|
| Transient | `ProviderFailure`, `RateLimited`, `QuotaExhausted`, `Timeout`, `TransientIo`, `TaskNotReady`, `BudgetExceeded` with a reset time |
| Permanent | `BudgetExceeded` without a reset time, `Unauthorized`, `InvalidQuery`, `PermanentIo`, `Unsupported`, `TaskUnavailable`, `MissingCitations`, `OversizedPayload`, `DomainDenied`, `UnsafeTarget` |
| Fatal | `FatalCorruption` |

Rules for new variants and kinds:

- Every new `Error` variant and every `AcquisitionFailure` kind maps to
  exactly one class through `class()`, and the partition test covers it.
- Invalid caller input, including `Error::InvalidConstraint` (Phase 00 S1)
  and limits above the ceilings, is permanent.
- An unknown schema version on read is permanent.
- A consumer egress refusal is permanent. At the baseline, `check_url_with`
  maps every resolver error to transient `TransientIo`
  (`net_policy.rs:164-169`), so a refusal expressed through a `Resolver`
  wrapper currently classifies as transient. Phase 01 S1 must give the
  resolver a way to signal refusal distinctly from lookup failure.
- `Partial` outcomes are not errors and have no class.

## 12. Storage, credentials, and egress

### 12.1 Storage

Zetesis applies the Kanon storage decision tree (`STORAGE-TIERS`):

| Data | Tree answer | Owner |
|------|-------------|-------|
| Paid-spend ledger, reservations, provider attempts, task records | Transactional state: `pinax` | Zetesis (planned) |
| Cached provider responses and other blobs | Content-addressed blobs: `koina` + `fjall` | Zetesis (planned) |
| Acquisition bodies and envelopes | Consumer custody | Consumer |
| Credentials | Not stored by Zetesis | Operator vault or consumer |
| Admitted facts | Not stored by Zetesis | Consumer (knowledge admission) |

- No storage primitive is selected at the baseline, and no storage crate is a
  dependency. `koina` + `fjall` in earlier Zetesis docs is a candidate, not
  an implemented dependency and not a license compatibility proof: each
  candidate must pass `deny.toml` before it is added.
- Raw `fjall`, `redb`, or SQLite use is a standards violation except as a
  named migration exception with a tracking issue.
- Phase 03 S1 verifies whether `pinax` and `koina` are consumable. If they
  are not, Phase 03 S1 is blocked upstream and tracked by an issue in this
  repository; no substitute store is introduced.

### 12.2 Credentials and egress

- Zetesis accepts credential references (opaque identifiers) and resolves
  them per call through a consumer-supplied resolver. A resolved value lives
  only for that call. No serializable type, identity, cache key, envelope,
  log line, or error message carries a credential value.
- Egress policy enters identities and cache keys only as a scope identifier
  and revision, never as policy content, so cache entries cannot cross egress
  scopes and the policy itself is never a cached value.
- A consumer expresses egress policy through `Resolver` (refuse before
  lookup) and, from Phase 01 S1, `Connector` (refuse before connect). Both
  can only narrow the Zetesis target policy. Only `LocalTargetAuthorization`
  widens it, and no public mint exists.

## 13. Phase 00 S1 acceptance mapping

| Acceptance item | Where it is met |
|-----------------|-----------------|
| Reviewed source and API delta | Sections 3 and 4 of this document, plus the Phase 00 S1 source changes |
| Deterministic serialized fixtures | Golden fixtures added in `sylloge` (section 4.1) |
| Malformed, unknown-version, overflow, and invalid-constraint tests in the owning packages | Malformed, overflow, and invalid-constraint tests in `sylloge` (section 4.1). Unknown-version tests land with the first versioned type in Phase 01 S2. |
| Contract ownership table with no consumer cycle | Section 5 |
| Stale Phase-0, scaffold, pricing, and host-command instructions retired | `README.md`, `CLAUDE.md`, `AGENTS.md`, `SECURITY.md`, `llms.txt`, `_llm/`, and `docs/research/deep-research-provider-decision.md` in the same change set; Rust doc drift listed in section 3.10 |
