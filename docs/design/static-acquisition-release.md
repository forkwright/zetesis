# Static acquisition producer release

Phase 01 S3, producer side: the merged Zetesis revision that consumers pin
for static acquisition, the evidence schema it emits, and the conformance
fixtures published at that revision. Kanon registration and each
consumer's compatibility test and adapter are the seat's and the
consumers' steps (`consumer-contracts.md` section 6). This document is
licensed under [CC BY-NC-ND 4.0](../../LICENSE-DOCS).

## Producer revision

| Item | Value |
|------|-------|
| Revision | `5b02d633820547699868234e71fff584e39cd53f` on `main`, merged through [#90](https://github.com/forkwright/zetesis/pull/90) |
| Workspace version at that revision | `0.0.6` (`producer.version` in every envelope it emits) |
| Consumer dependency | The `zetesis` facade only, pinned by revision: `zetesis = { git = "https://github.com/forkwright/zetesis", rev = "5b02d633820547699868234e71fff584e39cd53f" }` |
| Evidence schema | `zetesis.static_acquisition`, version `1` (`EVIDENCE_SCHEMA_ID`, `EVIDENCE_SCHEMA_VERSION`) |
| Extractor | `zetesis.html_text`, version `1` (`ExtractorId::current()`) |

A branch name, a tag, an open pull request, or this document is not a pin.
Only the revision above is. A later revision that keeps schema version 1
and extractor version 1 emits the same fingerprints for the same content;
a consumer still re-runs its compatibility test before moving its pin.

## Conformance fixtures at the revision

The fixture files below are published at the revision. Their SHA-256
digests identify them; `release_record_matches_the_code_and_fixtures` in
`crates/sylloge/tests/evidence_envelope.rs` fails if a file, the schema
version, or the extractor version no longer matches this record. Golden
fixtures never change once released: a changed encoding is a new schema
version with new fixtures.

| File (under `crates/sylloge/tests/fixtures/evidence/`) | SHA-256 | Role |
|------|---------|------|
| `page.html` | `1226d22d2282599bdaec3a6432f2b476d22f4b6814ebff27886a57c6d1f3cc81` | Source document; the decoded body of the complete golden |
| `page.html.gz` | `cb169b3b23ad8767099566b7093ab6b02af83093dd67d96064af0007091eb6ec` | The same document as gzip wire bytes |
| `golden_envelope.json` | `a648e5667e85afd1a3aa52e8b6bb5f201c20fe100ea5b56d55f5a65f3bdfcd12` | Complete envelope, fingerprint `sha256:eda01f2d750b7e845802f0271a556d276f862da6f96515263a602558a04d7476` |
| `golden_failed_envelope.json` | `4025c53c0c94208408af0076255e69cf94a540cb3ff5bac609c8823ded00b671` | Failed envelope (`unsafe_target`), fingerprint `sha256:2711f0ecd20dc93b599845153ef1b2fad4bf5f82c58002d0b9eb4cfee2186046` |

The boundary behaviour the consumer contract names is covered by these
producer tests at the revision:

| Boundary | Tests |
|----------|-------|
| Unsafe targets | `crates/sylloge/tests/static_acquisition.rs`: `redirect_to_loopback_literal_is_refused_without_connecting`, `redirect_to_host_resolving_private_is_refused_without_connecting`, `alternate_address_spellings_are_canonicalized_and_refused_without_connecting`, `dns_rebinding_to_private_between_hops_never_reaches_private_address`, `bad_port_is_refused_without_connecting`, `resolver_refusal_is_egress_denied_without_connecting`, `connector_denial_of_ip_literal_is_egress_denied` |
| Redirect chains | `redirect_loop_is_refused`, `redirect_past_limit_is_refused`, `https_to_http_downgrade_is_refused_by_default`, `malformed_redirect_location_is_refused`, `userinfo_in_any_location_spelling_is_never_recorded` |
| Cancellation and time | `dropping_acquire_future_closes_the_connection`, `stalled_connector_times_out_within_connect_budget`, `silent_origin_hits_whole_operation_deadline` |
| Bounded transfer | `decompression_bomb_stops_reading_at_the_decoded_ceiling`, `content_length_overstating_the_body_is_interrupted_stream`, `pdf_content_type_is_refused_before_the_body` |
| Extraction identity, golden bytes, and replay | `crates/sylloge/tests/evidence_envelope.rs`: `acquisition_produces_the_golden_evidence`, `golden_envelope_decodes_and_re_encodes_byte_for_byte`, `replay_of_the_captured_bytes_reproduces_the_transformation`, `same_source_with_changed_extractor_version_is_a_version_mismatch`, `unknown_schema_version_is_refused` |

## Compatibility evidence a consumer records

Bound to both commits (the consumer's and the producer revision above),
through the consumer's real adapter and storage path:

1. With the pinned facade, decode each golden envelope and re-encode it to
   the same bytes; store it through the consumer's storage and read it back
   byte for byte (the stored form is the JSON serialization, P2).
2. `replay(golden, page.html)` returns `Reproduced` after that round trip,
   and `replay` of the failed golden returns `NothingToReplay`.
3. A record with an unknown `schema_version` is refused by the adapter as
   an adapter failure, never answered through a local fetch stack.
4. The consumer's own boundary tests: unsafe redirect, cancellation,
   budget exhaustion, attribution, and settlement or release, through the
   real adapter (dioptron `docs/design/zetesis-acquisition-boundary.md`).

## Upgrade and rollback

- A new evidence schema version lands producer first, with new golden
  fixtures and a new release record; consumers accept only versions they
  know.
- A consumer moves its pin only after its compatibility test passes at the
  new revision.
- Rollback restores the previous compatible pair (producer revision and
  consumer commit). Stored envelopes are never rewritten; there is no
  permanent alternate fetch engine.
