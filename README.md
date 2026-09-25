# zetesis

*ζήτησις - systematic inquiry.*

Sovereign research substrate: one Rust interface over research and search providers, with budget enforcement, quota accounting, cited result normalization, and bounded static acquisition. Most of that surface is still planned. The tables below separate what has landed from what has not.

**Status:** pre-release (`0.0.x`). The four-crate workspace has landed. `sylloge` carries the provider, constraint, network-target, citation, result, cost, budget, and deep-research lifecycle types, and `zetesis` re-exports them. No provider adapter, HTTP client, cache, durable ledger, model binding, or daemon exists yet. The [contract baseline](docs/design/contract-baseline.md) records what each public type enforces today and what is caller convention.
**Open work:** `_llm/current_state.toml` lists the public open threads. Roadmap and blocker status are maintained outside this repository.

## Why

Frontier-model built-in search is an opaque black-box priced per-token. Vendor aggregators (Brave / Exa / Tavily / You.com / Valyu / Perplexity) are pay-per-query with little architectural control and costs that compound fast at agent scale.

Zetesis takes a different shape:

- **Free-first routing** across free academic and reference APIs (Semantic Scholar, arXiv, OpenAlex, Crossref, PubMed, Wikipedia). Tier 0 is the default route. Provider adapters are planned.
- **Paid use is opt-in.** Paid providers are disabled until an operator configures them and a reservation authorizes the spend. A Tier-0 miss never enables paid use, and there is no automatic paid fallback. The durable, identity-bound reservation ledger is tracked by [zetesis#47](https://github.com/forkwright/zetesis/issues/47).
- **Self-hosted deep inquiry** (planned). The local-first research loop calls models only through a model contract the consumer grants. Logismos executes the models. Paid model APIs are not a default route.
- **Bounded static acquisition** (planned, [zetesis#48](https://github.com/forkwright/zetesis/issues/48)). Zetesis owns anonymous static GET: target and redirect validation, bounded transfer and decoding, static text extraction, and a versioned evidence envelope.
- **Cited + structured** output always. No synthesis without source provenance. Landed: a `ResultHit` cannot be constructed or decoded without at least one citation.
- **Cache** with per-provider freshness windows (planned). The storage primitive is not selected yet.

## Architecture

| Crate | Landed | Planned |
|-------|--------|---------|
| `zetesis` | Facade re-exporting the `sylloge` surface; `steelman` and `briefing` aliases for the two reserved crates | CLI, daemon binary, consumer adapter wiring |
| `sylloge` | `Provider`, `DeepResearch`, and `Crawler` traits; `SearchConstraints` with the fail-closed network-target check and non-forgeable `ValidatedTarget`; citation, result, freshness, cost, and budget types; in-memory `LocalDeepResearch` with an offline loop fixture | Tier-0 provider adapters, routing, durable reservation ledger, cache, `StaticAcquirer` and the evidence envelope (replacing `Crawler`), deep-inquiry loop against a model contract |
| `elenkhos` | Reserved crate boundary (marker type) | Retrospective steel-manning engine |
| `synopsis` | Reserved crate boundary (marker type) | Briefing synthesizer |

Zetesis depends on no consumer. Consumers pin a merged Zetesis commit. Logismos, Tropos, and Kanon are reached through service or registration contracts, not Cargo dependencies. See the ownership table in the [contract baseline](docs/design/contract-baseline.md#5-contract-ownership).

## Consumer map

- **aletheia** - planned nous-agent research adapter
- **dioptron** - planned static-acquisition consumer adapter for the sovereign web runtime
- **akroasis** - planned broader OSINT public-source research consumer

## Non-goals

- Not a conversational search UI (consumer concern).
- Not a browser. Zetesis owns anonymous, bounded static acquisition and does
  not delegate it to an external extraction service. Dioptron owns
  sessions, rendering, scripted browsing, and browser actions, and stores the
  Zetesis evidence envelope verbatim.
- Not a recursive crawler. There is no link-following loop.
- Not a model runtime. Deep-inquiry model calls go through a consumer-granted
  model contract and Logismos executes them. Zetesis never selects a host or
  switches host modes. Tropos owns host modes.
- Not a knowledge store or admission authority. Zetesis emits cited
  candidates, and consumers decide what becomes a fact.
- Not a vector store (that is `heurēma`).
- Not a credentials manager. The operator vault or consumer owns credential
  values. Zetesis handles references only.

## Development

This README records purpose, boundaries, consumer map, and crate shape. It does not duplicate the live roadmap. Local gate commands are in [AGENTS.md](AGENTS.md).

## Design Notes

- [Contract baseline](docs/design/contract-baseline.md) - public API inventory (enforced versus convention), API delta, contract ownership, identity and idempotency, evidence envelope v1, resource dimensions, cancellation, error classification, and storage.
- [Consumer contracts](docs/design/consumer-contracts.md) - Phase 00 S2 freeze of the producer contract for tool-hosting consumers, the Dioptron static-acquisition handoff, producer design corrections, coverage, and the facts still needed from Akroasis, inference, and Kanon owners.
- [Multi-signal classifiers](docs/research/multi-signal-classifiers.md) - required evidence record shape for classifier designs that combine weighted signals before export.
- [Deep research provider decision](docs/research/deep-research-provider-decision.md) - Phase 05 decision to vendor the local-deep-researcher loop pattern, treat the gpt-researcher `vllm_openai` adapter shape as prior art, and reject open_deep_research as the default contract.

## License

- Code and tooling: [PolyForm Noncommercial 1.0.0](LICENSE).
- Documentation: [CC BY-NC-ND 4.0](LICENSE-DOCS).

<!-- kanon:auto-start -->
## Repository Metadata

- Registry name: `zetesis`
- Description: Kanon-managed forkwright repository `zetesis`.
- Forge repo: `forkwright/zetesis`
- Kanon prefix: `ze`
- Config source: `workflow/kanon.toml [projects.zetesis]`
- Planning state: `projects/zetesis/STATE.md`
- Last state update: `2026-05-25`

Run `kanon docs sync --check --repo zetesis` to verify this generated
section and `kanon docs sync --apply --repo zetesis` to refresh it.

## Blast zone

- Paths explicitly named by the rendered prompt, role, or template input.

## Acceptance verifier

```bash
kanon gate
```
<!-- kanon:auto-end -->
