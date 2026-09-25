# Consumer contracts from real call sites

Phase 00 S2. Frozen on 2026-09-25 against these revisions:

| Repository | Revision | Visibility |
|------------|----------|------------|
| forkwright/dioptron | `4e66c96f595a108873edfa1c5c280a7638eee228` | Public; design documents only, no Rust code |
| forkwright/akroasis | `73bb86efd2b854d2b64bb58b5510e366cb4d7580` | Public |
| forkwright/kanon | `adc099467851211380e2887c313f6aba1514008f` | Seat-owned |
| forkwright/zetesis | `0bff055193ba5bc0d594bb0468f0c4e682f7e39d` | This repository at the base of this change |

Consumer-private sources were reviewed; their details are held outside this
repository.

This document extends the [contract baseline](contract-baseline.md) from
Phase 00 S1. It references the baseline's evidence envelope (section 7),
resource dimensions (section 9), cancellation (section 10) and error
classification (section 11) instead of restating them.

Evidence marks: an unmarked statement about a cited file was checked by
reading the cited lines at the frozen revision. **(inferred)** marks a
consequence reasoned from checked text but not executed. **(executed)** marks
a claim checked by running a scratch copy of the cited code outside the
repository. Dioptron, akroasis and kanon paths are relative to their own
repository roots. Unprefixed paths are zetesis paths.

## Answer

- **Tool-hosting consumers.** §1 states what zetesis must guarantee so a
  consumer that exposes static fetch to an agent loop reaches parity: 20
  requirements mapped onto the `Resolver` and `Connector` seams, plus the
  handoff contract. The consumer's current behavior was reviewed as the
  parity baseline and is held outside this repository.
- **Cited results.** No cited-result path into consumer knowledge admission
  exists yet. Consumers own admission; the envelope supplies URL, digests and
  fingerprint for citation (§1.3).
- **Producer corrections.** The review found four design points and one seam
  rule that change the Phase 01 contract (§2).
- **Dioptron D12/D7.** The five-step adapter protocol maps onto evidence
  envelope v1 without new producer surface. §3 freezes what zetesis
  guarantees at each step and splits the dependency gate into producer-side
  and seat-side facts.
- **Akroasis, granted inference, Kanon Zeugma.** None calls zetesis today.
  This document publishes no trait or method names for them or for the six
  planned roles; §4 lists the facts each owner must supply. Kanon's
  substrate registry describes zetesis as "six adapter traits", which
  conflicts with that rule (question K1).
- **Coverage.** No Tier-0 provider is implemented at `0bff055`, so §5 is a
  plan-to-task map, not a measurement. Discovery of fresh material has no
  Tier-0 route and stays an explicit gap while paid routing is disabled.

## Plan text this stage satisfies

> | Consumer boundary | Required contract and independent witness |
> |---|---|
> | Aletheia organon search/fetch | Current WebSearchExecutor/WebFetchExecutor request/egress/cancel limits; cited result through episteme and next-turn recall |
> | Dioptron D12/D7 | Authorize/reserve before call; verbatim acquisition envelope plus separate tenant attribution; settle once after crash/retry |
> | Akroasis collector/evidence | kryphos credential handle, configured egress/persona, tekmerion evidence receipt; correlation/defensive decisions remain Akroasis-owned |
> | Granted inference | Consumer model/intent/privacy request, Logismos admission/placement/execution outcome, Tropos revocation/drain; no host-mode command here |
> | Kanon Zeugma | Bounded tool schema, capability grant, versioned registration and consumer conformance; no code copied into Angelos |
>
> The planned roles CredentialProvider, EgressRouter, TaskSource,
> RetrospectiveTarget, BriefingSink and FactEmitter require real
> method/semantic contracts only as their first caller is implemented. Do not
> publish invented method names or a new adapter crate before locating the
> existing consumer owner. Freeze pre/postconditions, unknown outcomes,
> idempotency, auth context and failure behavior at each handoff. Use one
> facade and producer-first release/pin/conformance changes.
>
> Resolve coverage with representative sanitized consumer tasks: academic,
> reference, freshness-sensitive, entity and adversarial/contradictory
> evidence. Document a supported subset when coverage fails. Paid routing
> remains disabled until current endpoint economics and durable budget proof;
> model throughput only gates deep inquiry, not source search or anonymous
> static acquisition. Numerical thresholds and policy defaults require
> measured demand and an immutable policy revision; old arbitrary
> dollar/token/GPU numbers do not carry forward as acceptance.

This document sets no numeric default. Every ceiling named below is a landed
crate constant or an external standard (baseline section 9).

## 1. Producer contract for tool-hosting consumers

A tool-hosting consumer offers static fetch to an agent loop behind its own
approval and egress policy. Aletheia is the first consumer of this contract.
The requirements below apply to any consumer of that shape. Each row is a
requirement on zetesis; the "consumer supplies" column names what crosses the
seam.

### 1.1 Requirements

| Id | Requirement on zetesis | Seam | Consumer supplies | Failure kind or evidence |
|----|------------------------|------|-------------------|--------------------------|
| R1 | For a domain host, consult the caller's `Resolver` once per hop before any socket. A refusal by a consumer `Resolver` wrapper stops the hop before lookup (deny before DNS). | `Resolver` wrapper | A wrapper that refuses under its deny policy, and when any candidate address is outside its allowlist | `egress_denied`, distinct from `resolution_failed` (baseline section 11 records the current transient mapping at `crates/sylloge/src/net_policy.rs:167-175`) |
| R2 | Connect only through the caller's `Connector`, and only to an address in that hop's validated set. The `Connector` sees each address before its socket opens, so allowlist checks and address binding hold. | `Connector` | An implementation that refuses addresses its policy does not allow | Connect attempt result `denied`; `egress_denied` |
| R3 | Apply R2 to IP-literal hosts. The target policy never calls the `Resolver` for an IPv4 or IPv6 literal (`net_policy.rs:164-166`), so the `Connector` is the only consumer checkpoint for such a hop (§2). | `Connector` | Same as R2 | Same as R2 |
| R4 | Check the scheme on every hop against the caller's allowed set, for example https only. | Acquisition policy | The allowed set | `scheme_not_allowed` |
| R5 | Refuse an `https` to `http` redirect unless the caller explicitly allows downgrade. The default is refusal. | Acquisition policy | An explicit allowance, only if needed | `downgrade_refused` |
| R6 | Reject userinfo in the requested URL and in every redirect target. Never turn it into a credential. | Target policy (`net_policy.rs:144-150`) | Nothing | `unsafe_target` |
| R7 | Resolve once per hop, classify every resolved address, keep no connection pool, and never reuse an address from an earlier hop. | Target policy (`net_policy.rs:185-195`) | Nothing | `unsafe_target` |
| R8 | Apply the caller's domain denylist and allowlist on every hop. | `SearchConstraints` (`net_policy.rs:197-214`) | The lists | `unsafe_target` |
| R9 | Refuse ports on the Fetch standard's bad-port list. | Acquisition policy | Nothing | `denied_port` |
| R10 | Follow redirects only in the acquirer's own loop: resolve `Location` against the current URL, apply R1 to R9 per hop, enforce the caller's limit within the ceiling of 20, and detect a repeated URL. No library auto-redirect. | Acquirer | The redirect limit | `redirect_limit`, `redirect_loop`, `malformed_redirect` |
| R11 | Send a fixed request header set on every hop: `Host`, one `User-Agent`, `Accept`, `Accept-Encoding`, `Connection: close`. No credential, cookie, `Authorization`, `Referer` or caller-supplied header, so nothing can be forwarded to a redirect target. | Acquirer | Nothing | None |
| R12 | Send exactly one `User-Agent` field carrying the caller-supplied string, or one producer identifier `zetesis/<version>` when the caller supplies none. | Acquirer | Its user-agent string | None |
| R13 | Never read proxy environment variables. A consumer that needs a proxy expresses it as a `Connector`. | Transport | A proxy `Connector`, if any | None |
| R14 | Apply one whole-operation deadline and one per-connect timeout, both caller-supplied, with no crate default (baseline section 9). Deadline expiry is a failure with evidence, not cancellation (baseline section 10). | Limits | Both durations | `deadline_exceeded`, `connect_timeout` |
| R15 | Enforce wire, decoded and text limits while streaming, within ceilings of 10 MiB, 10 MiB and 4 MiB (`PageContent::MAX_BODY_BYTES` and `MAX_TEXT_BYTES`, `crates/sylloge/src/constraints.rs:474`, `477`). Record every cut: `extraction.truncated` with `partial { text_limit_reached }`, and `failed { wire_limit }` or `failed { decoded_limit }` for a body over its ceiling. Never truncate silently. | Limits | Values within the ceilings | The named fields and kinds |
| R16 | Decode text by the declared charset, BOM or meta charset; decode character references; emit valid UTF-8 with byte spans into the decoded source (`zetesis.html_text` version 1, baseline section 7.1). | Extractor | Nothing | `partial { unsupported_charset }`, `partial { invalid_encoding }` |
| R17 | Publish the accepted media types and content encodings. Anything outside them is a typed failure or `partial { binary_content }`. | Extractor | The media types it needs | `unsupported_content_type`, `unsupported_content_encoding` |
| R18 | Reach local or private targets only with `LocalTargetAuthorization`, never on the strength of URL or header data. | Target policy (`net_policy.rs:37-41`) | Nothing; no public mint exists | `unsafe_target` |
| R19 | Return a typed failure with a class, stable within a schema version (baseline sections 7.2 and 11). | Outcome | Its own mapping from kinds to user-facing errors | None |
| R20 | Cancel the call when the future is dropped: no envelope, and no socket, timer or I/O task survives. An in-flight lookup is the one exception (§2, P3). | Cancellation | Its own record of the dropped call | None |

### 1.2 Handoff contract

| Field | Contract |
|-------|----------|
| Preconditions | The consumer has run its own approval and any deny pre-check. It supplies the URL, limits within the ceilings, the allowed schemes, a user-agent string, and a `Resolver` wrapper and `Connector` built from its egress policy. It passes no `LocalTargetAuthorization` (R18). |
| Postconditions | `Acquisition { envelope, body }` with outcome `complete`, `partial { reason }` or `failed { failure }` carrying the hop evidence so far; or `Err` only for invalid input that produced no attempt (baseline section 7.2). |
| Unknown outcomes | Cancellation returns nothing. The consumer's own record of the dropped call is the only trace. For an anonymous `GET`, the unknown is only whether the origin saw the request. |
| Idempotency | None at zetesis. Each call is a new observation; the same content and transformation give the same fingerprint. The consumer's call identifier stays beside the envelope, never inside it. |
| Auth context | None crosses. Egress policy crosses only as the `Resolver` wrapper and `Connector`. No credential. |
| Failure behavior | Typed kind with class. Retry is the consumer's decision. |

Retiring a consumer's local static-fetch code after parity is shown is the
consumer's and the seat's decision (K3). Zetesis does not change consumer
code.

### 1.3 Cited results

No cited-result path into consumer knowledge admission exists yet. Consumers
own admission: a zetesis emission is a candidate until the consumer confirms
it (`_llm/decisions.toml:21`; `README.md:51-52`). For citation, the envelope
supplies `requested_url`, `final_url`, `completed_at`, the response `date`
and `last_modified`, `wire_sha256`, `decoded_sha256`, `text_sha256` and
`fingerprint` (baseline sections 7.1 and 7.3). The consumer decides where
those fields live in its knowledge records and how a later turn cites them.

### 1.4 Keyed provider search (deferred)

Zetesis has no search provider implemented and paid routing stays disabled,
so consumers keep their own keyed search routes. Before a consumer routes a
keyed query through zetesis, the zetesis route must:

- keep key custody outside zetesis, resolving a credential reference per
  call (baseline section 12.2);
- send a credential only to the provider's own origin over `https`, never to
  a redirect target;
- bound every response body read, error bodies included;
- honor a caller-supplied deadline.

## 2. Producer design corrections found during this review

These change or sharpen the Phase 01 contract in the baseline.

- **P1. Non-2xx responses.** A non-2xx final response is not a failure kind.
  It is recorded in `response.status` and in the hop's `status`. The outcome
  describes only transfer and extraction completeness, so an error page that
  transfers and extracts within limits is `complete`. The consumer decides
  what a status means.
- **P2. Stored form.** The stored evidence form is the JSON serialization of
  the envelope. CBOR carries the same fields through `ciborium` (baseline
  section 8). Golden fixtures pin the JSON bytes per schema version.
- **P3. Lookup cancellation.** The `Resolver` is synchronous and runs under
  `spawn_blocking` (baseline section 3.1). A blocking task cannot be aborted
  once it starts. After the caller drops the future, an in-flight lookup
  finishes in the background on the blocking pool and its answer is
  discarded. No socket opens and no other effect follows. Baseline section
  10's "no background task survives" holds for sockets, timers and I/O
  tasks; this lookup is the stated exception.
- **P4. Embedded IPv4 in IPv6 transition forms.** At `0bff055`,
  `canonical_ip` unwrapped only the IPv4-mapped and IPv4-compatible forms,
  and `is_blocked_ipv6` had no rule for `64:ff9b::/96` (NAT64) or
  `2002::/16` (6to4): `64:ff9b::7f00:1`, `64:ff9b::a00:1`, `2002:7f00:1::`
  and `2002:a00:1::1`, which embed `127.0.0.1` and `10.0.0.1`, all passed
  classification **(executed)**. Fixed in #87: NAT64, 6to4 and Teredo
  addresses are classified by their embedded IPv4 destination, the
  local-use NAT64 prefix is refused, and the IPv6 documentation and
  discard-only prefixes are blocked.
- **IP-literal hosts skip the `Resolver`.** The target policy builds the
  address set directly for an IPv4 or IPv6 literal (`net_policy.rs:164-166`)
  and calls the `Resolver` only for a domain (`net_policy.rs:167-175`). A
  consumer's deny-before-DNS wrapper therefore never sees an IP-literal hop,
  so the contract requires the consumer's deny and allowlist rules in the
  `Connector` as well (R2, R3).

## 3. Dioptron D12/D7

Sources: `docs/design/zetesis-acquisition-boundary.md` (the boundary, cited
below as `boundary:<line>`), `docs/design/topology.md` (D2, D7, D12),
`docs/requirements.md`, `docs/design/tenancy.md`,
`docs/design/ingest-rules-taxonomy.md`, the accepted decision
`D-zetesis-static-acquisition-boundary` (`_llm/decisions.toml:80-85`), and
the dioptron#66 tracker (`_llm/current_state.toml:38-41`). Dioptron has no
Rust code at `4e66c96` (`_llm/architecture.toml:32`), so every item below is
a contract against design text.

### 3.1 Protocol and producer guarantees

| Step | Dioptron does | Zetesis provides and guarantees | Zetesis does not |
|------|---------------|---------------------------------|------------------|
| 1. Plan and authorize (`boundary:38-39`) | Evaluates tenant, session, grant, budget and credential context through the D12 grant evaluator (`topology.md:53-60`) | Published limit ceilings at the pinned revision. Limits above a ceiling are invalid input: `Err` with no attempt, so dioptron can refuse before reserving | Interpret tenants, grants, sessions or storage tiers (`boundary:31`) |
| 2. Reserve and persist intent in one transaction (`boundary:40-42`) | Commits reservation and intent before any call | Nothing. No reservation, no ledger entry, no charge for anonymous `GET` (`boundary:67-72`); no invocation id is needed or accepted | Take part in the transaction |
| 3. Invoke from the pinned revision (`boundary:43-44`) | Passes target and limits; may pass a `Connector` | Requirements R1 to R20; the exact limits applied, recorded in the envelope's `limits`; typed failures with class; `failed` carries hop evidence; the deadline bounds the call; dropping the future cancels it and yields no envelope | Replace or reinterpret dioptron policy (`boundary:44`) |
| 4. Store the envelope verbatim (`boundary:45-47`) | Stores the envelope's JSON bytes (P2); attaches invocation, tenant, session, grant, budget and credential-context references beside them; stores the body in its custody keyed by `decoded_sha256` | Schema id `zetesis.static_acquisition` and `schema_version` 1; producer package and version; a fingerprint over content and transformation only; the body returned separately; `replay(envelope, body)` reporting reproduced, version mismatch, digest mismatch or extraction drift; readers that reject unknown versions (baseline sections 7 and 8) | Carry its own commit SHA. A crate cannot know its own commit; dioptron attaches the kanon-pinned SHA beside the envelope. Embed any dioptron identity |
| 5. Settle once from the recorded outcome (`boundary:48-49`) | Settles or releases, and audits | The outcome kind, and in `failed` the per-hop connect attempts, which show whether any connection opened (the input to settle versus release) | Decide or record any charge |

Derived dioptron records reference the envelope and never replace its bytes,
schema identity, producer revision or fingerprint (`boundary:51-53`;
`ingest-rules-taxonomy.md:16-21`). Zetesis proposes that the pair (envelope
`producer.version`, dioptron-attached pinned SHA) is the producer revision in
that sentence; D2 asks dioptron to confirm.

### 3.2 Handoff contract

| Field | Contract |
|-------|----------|
| Preconditions | Intent committed (step 2). Limits within ceilings. No `LocalTargetAuthorization`: it has no public constructor (`net_policy.rs:37-41`) and dioptron has named no minting boundary (D10). |
| Postconditions | An acquisition, or `Err` for invalid input with no attempt. |
| Unknown outcomes | Revocation under the killable default (`requirements.md:28`; `tenancy.md:27-31`) drops the future: no envelope. A crash between the committed intent and a stored envelope leaves an unknown outcome. Zetesis holds no state across calls, so there is nothing to query; dioptron reconciles from its intent record (D6, D7). |
| Idempotency | None at the producer. Dioptron's invocation id is the settlement key, so a retry under the same invocation settles once. A retry is a new `acquire` and a new envelope; identical content and transformation give the same fingerprint, so evidence can be deduplicated by fingerprint. |
| Auth context | None crosses (`boundary:15-21`, `31`). Anonymous `GET` carries no credential. |
| Failure behavior | Typed kind with class. An unknown `schema_version` or unsupported result is an adapter failure, never a reason to use a local fetch stack (`boundary:55-65`). |

### 3.3 Dependency and compatibility gate

| Condition (`boundary:80-89`) | Side | Status at this freeze |
|------------------------------|------|-----------------------|
| Producer revision is a merged, immutable zetesis SHA | Producer | Not met. No acquisition module at `0bff055`; `sylloge` exposes only the `Crawler` trait (`crates/sylloge/src/crawler.rs:79`) |
| Kanon registers or derives that SHA | Seat | Not met. Kanon records `consumers = []` and `pin_unresolved = true` for zetesis (kanon `crates/basanos/standards/substrate.toml:239-249`) |
| Real consumer compatibility test binding the SHA to a dioptron revision | Consumer | Not met. No dioptron code |
| Boundary tests for unsafe targets, redirect chains, cancellation and extraction identity | Producer | Not met. Conformance fixtures publish at the merged SHA |
| Boundary tests for budgets, dioptron attribution, and settlement or release | Consumer | Not met |
| No dependency cycle | Seat validates | Producer side holds: zetesis depends on no consumer (baseline section 5.1) |

Since this freeze, the two producer rows are met: the producer revision,
schema, and conformance fixtures are recorded in
[static-acquisition-release.md](static-acquisition-release.md).

## 4. Boundaries without a located caller

Per the PLAN, no trait or method names are published for these boundaries.
Each subsection records what exists and what the owner must supply.

### 4.1 Akroasis collector and evidence

What exists at `73bb86e`:

- No reference to zetesis anywhere in the repository.
- The OSINT collection crate `skopos` is planned and not shipped
  (`README.md:31`); the privacy and proxy crate `lethe` is planned
  (`README.md:37`).
- `kryphos` is a vault whose read returns decrypted secret bytes
  (`DecryptedEntry.secret`, `crates/kryphos/src/storage.rs:162-173`). No
  opaque credential-handle type exists.
- `tekmerion` has versioned effect receipts: a durable intent before the
  effect, then a closed outcome set that includes `RecoveryRequired` for an
  uncertain intent after restart (`crates/tekmerion/src/effect_receipt.rs:18-40`;
  sink at `crates/tekmerion/src/effect_receipt_state.rs:17-34`). Caller
  authority claims can bind an opaque persona reference
  (`crates/tekmerion/src/caller.rs:141`, `202-207`).
- No egress router and no persona-to-transport mapping exist.

Facts needed from akroasis owners:

1. The first collector that calls zetesis: crate, verb, and whether the
   call is anonymous `GET` or a keyed provider query.
2. How a credential crosses the boundary, given that `kryphos` returns
   bytes: does zetesis ever receive bytes, or only a reference that an
   akroasis component resolves at connect time?
3. Which component maps a persona to a transport, and whether it can honor
   the `Connector` rule (connect to exactly one resolved address) or needs
   proxy-side DNS.
4. Whether one acquisition maps to one `tekmerion` intent and outcome pair,
   which envelope fields enter the receipt digests, and how
   `RecoveryRequired` relates to a zetesis unknown outcome.
5. Confirmation that zetesis returns evidence only; correlation and
   defensive decisions stay in akroasis.

### 4.2 Granted inference

What exists:

- Zetesis's deep-research surface is an in-memory lifecycle with offline
  fixture seams and no inference backend (`crates/sylloge/src/lib.rs:68`,
  `73`; the "Deep inquiry backend" open thread, `_llm/current_state.toml:69`).
- Zetesis is not a model runtime and never selects a host or switches host
  modes; Tropos owns host modes (`README.md:48-50`).
- Kanon's substrate registry records logismos's contract as an embedding
  service facade (kanon `crates/basanos/standards/substrate.toml:313-325`).
  The logismos repository was not inspected.
- No Tropos contract was located in the inspected repositories.

Facts needed from the logismos and Tropos owners:

1. The consumer request: model, intent and privacy class, and whether the
   consumer or zetesis on its behalf submits it.
2. The admission, placement and execution outcome set, including an unknown
   outcome and the identity a retry reuses.
3. Revocation versus drain for an in-flight deep-inquiry step, and what
   zetesis records when either happens.
4. Confirmation that the grant gates deep inquiry only, never source search
   or anonymous static acquisition.

### 4.3 Kanon Zeugma

What exists in kanon (seat-owned, reviewed at `adc0994`): a registry
declaration for a repository's federated tool surface (surface kind, contract
path, parity gate, optional call defaults), operational federation rows, and
a pinned manifest derived from them, with mounting in Angelos planned for a
later wave.

Zetesis declares no federated surface and has no CLI or MCP binary (the
facade crate holds only `crates/zetesis/src/lib.rs`).

Facts needed from the kanon owner:

1. Which surface kind zetesis should declare and where its contract file
   lives.
2. The parity-gate command zetesis must provide.
3. The capability grant that permits zetesis tools, and how a later
   budget-bearing call is authorized.
4. How the Zeugma per-call timeout composes with zetesis deadlines.
5. Confirmation that the manifest derives from the zetesis contract and no
   zetesis code is copied into Angelos.

### 4.4 Planned roles

| Role | First caller located | Contract published here |
|------|----------------------|-------------------------|
| CredentialProvider | No | None |
| EgressRouter | No. Consumer egress policy maps onto the static-acquisition `Resolver` and `Connector` seams (§1), which are concrete producer seams, not this role | None |
| TaskSource | No | None |
| RetrospectiveTarget | No | None |
| BriefingSink | No | None |
| FactEmitter | No | None |

Kanon's substrate entry for zetesis states the contract as "research
mechanism with consumer-supplied policy adapters; six adapter traits", a
version policy of an "adapter trait surface frozen via decision record
before v1", and a breaking-change protocol keyed to adapter trait changes
(kanon `crates/basanos/standards/substrate.toml:252-255`). The PLAN forbids
publishing invented method names or a new adapter crate before the consumer
owner is located, so that text describes no located contract. This
repository names only the seams with a located use: `Resolver` today, and
`Connector`, credential-reference resolution and the model contract when they
land (baseline section 5.1). Dioptron lists a different set of six traits for
its own peer-integration crate (dioptron `docs/design/topology.md:62-71`);
the two counts may have been conflated **(inferred)**. Question K1 carries
this to the seat.

## 5. Coverage

Rules applied: paid routing stays disabled; no numeric threshold or policy
default is set here; model throughput gates deep inquiry only.

No provider is implemented at `0bff055`: the only `Provider`
implementations are a doc stub and a test stub
(`crates/sylloge/src/provider.rs:94`;
`crates/sylloge/tests/trait_object_safety.rs:25`). The table maps
representative tasks to planned surfaces. Measured coverage is later
acceptance evidence, and provider capabilities named here are verified when
each provider lands.

| Class | Sanitized task | Planned route | Tier-0 coverage | Supported subset | Explicit gap |
|-------|----------------|---------------|-----------------|------------------|--------------|
| Academic | "Peer-reviewed and preprint work on evaluating retrieval-augmented generation since 2023, with DOIs and citation counts." | `AcademicLiterature`, intended Tier-0 sources Semantic Scholar and arXiv (`crates/sylloge/src/query.rs:39-41`); the wider Tier-0 list adds OpenAlex, Crossref and PubMed (`crates/sylloge/src/tier.rs:20-24`) | Planned | Metadata, abstract and DOI or URL per hit with provenance; open-access full text through static acquisition of the hit URL | Paywalled full text |
| Reference | "Definition and origin of the term Byzantine fault tolerance." | `QuickFactual`, intended Tier-0 source Wikipedia (`query.rs:30-33`); static acquisition of the cited page | Planned | Encyclopedic summary with article URL and retrieval time | Structured facts from a knowledge graph, which no Tier-0 source provides |
| Freshness-sensitive | "What changed in a named open-source library's release published this week?" and "Is a public status page reporting an incident now?" | `FreshnessSensitive`, documented as an explicit gap while paid routing is disabled (`query.rs:52-55`) | None for discovery | Static acquisition of a URL the consumer already holds, with freshness from the response dates and `Citation.published_at` (`crates/sylloge/src/freshness.rs`) | Discovering fresh URLs. Consumers keep their own search routes meanwhile (§1.4) |
| Entity | "Which institution publishes a named research group's papers, and what are its identifiers?" | `QuickFactual` or `AcademicLiterature`: scholarly author and institution records, Wikipedia | Partial | Scholarly entities, and entities with an encyclopedia article | Organizations and people outside those sources. Zetesis does not resolve private individuals; entity correlation stays with the consumer |
| Adversarial or contradictory | "A retracted paper and the papers that still cite it" and "a page whose claim contradicts the source it cites." | `AcademicLiterature` plus static acquisition of each side | Partial | Both sides retrieved with per-source provenance (`ProvenanceEntry`, `Citation`) and verbatim envelopes; injected or cloaked content is recorded as served | Detecting and resolving contradictions. Consumer-owned (consumer knowledge pipelines; dioptron D7 contradiction evidence); `elenkhos` is a reserved marker crate (`crates/elenkhos/src/lib.rs:1-7`) |

## 6. Release, pin and conformance order

1. Producer: the acquisition module lands in `sylloge` and is re-exported
   by the `zetesis` facade. Consumers depend on the facade only.
2. Producer: conformance fixtures publish at the merged SHA (unsafe
   targets, redirect chains, cancellation, extraction identity, golden
   envelope bytes and replay).
3. Seat: kanon records the SHA and the consumer.
4. Consumer: a compatibility test binds the pinned SHA, then the adapter
   lands.
5. An envelope change is a new `schema_version`, producer first. Consumers
   accept only versions they know.

## 7. Open questions

### Dioptron owners

1. **D1.** P2 makes the JSON serialization the stored evidence form. Does
   dioptron store those bytes as "the envelope bytes" for audit, with CBOR
   used only in transit?
2. **D2.** Does the pair (envelope producer version, dioptron-attached
   kanon-pinned SHA) satisfy "producer revision" in `boundary:51-53`?
3. **D3.** Does anonymous static acquisition need Tor, SOCKS or proxy
   transport (`requirements.md:71`; `topology.md:9-12`)? The `Connector`
   connects to one locally resolved address; Tor would need resolution
   through Tor as well.
4. **D4.** What does a dry run (`requirements.md:97`) of static
   acquisition return, given that target validation resolves DNS? Only the
   pre-resolution checks (scheme, userinfo, port, limits)?
5. **D5.** Zetesis returns one completed acquisition, not a stream
   (`requirements.md:98`). Acceptable for this verb?
6. **D6.** Killable revocation keeps "partial state preserved as audit
   fact" (`requirements.md:28`), but cancellation yields no envelope and no
   hop evidence. Is dioptron's own intent record enough, or does the verb
   need evidence up to the kill?
7. **D7.** After a crash between intent and envelope, does dioptron settle
   as consumed or release, and does a retry reuse the invocation id?
8. **D8.** P1 records a non-2xx final response in the envelope's status
   fields with a completeness outcome. Does dioptron store those envelopes
   as acquisition evidence, or treat a status class as an adapter failure?
9. **D9.** Which limits profile does dioptron pass, and does it vary by
   tenant grant or rule?
10. **D10.** Does the operator need local or intranet targets on this
    route? If so, which process mints `LocalTargetAuthorization`?
11. **D11.** Is the body retained (encrypted at rest, `requirements.md:118`)
    beside the envelope so replay stays possible?
12. **D12.** Is PDF or other document extraction (`requirements.md:37`) in
    scope for the static route's first version, given that the boundary
    assigns static extraction to zetesis and the first extractor is HTML
    text?

### Seat

1. **K1.** Replace the "six adapter traits" contract text, version policy
   and breaking-change protocol in the kanon substrate entry with contracts
   tied to located callers (static acquisition first)?
2. **K2.** When the acquisition SHA merges, record dioptron, and each other
   consumer once its compatibility test exists, with the exact SHA.
3. **K3.** Schedule retirement of each consumer's local static-fetch code
   after that consumer's parity test passes. The inventory for the
   consumer-private source is held outside this repository.

## 8. Claims not verified by execution

- The consequences marked **(inferred)** above: the possible conflation of
  the two six-trait lists.
- The logismos repository and any Tropos surface were not inspected.
- Third-party provider capabilities in §5 are planning inputs, checked when
  each provider lands.
