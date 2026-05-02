# zetesis

*ζήτησις - systematic inquiry.*

Planned sovereign research substrate for unifying research and search providers behind one Rust interface with budget enforcement, rate-limit management, cited result normalization, and a cache layer.

**Status:** Phase 0 specification. Design in flight; no code yet beyond the workspace manifest.
**Canonical state:** [`forkwright/kanon:projects/zetesis/STATE.md`](https://github.com/forkwright/kanon/blob/main/projects/zetesis/STATE.md)

## Why

Frontier-model built-in search is an opaque black-box priced per-token. Vendor aggregators (Brave / Exa / Tavily / You.com / Valyu / Perplexity) are pay-per-query with little architectural control and costs that compound fast at agent scale.

Zetesis takes a different shape:

- **Free-first routing** across free-quality academic and reference APIs (Semantic Scholar, arXiv, OpenAlex, Crossref, PubMed, Wikipedia). Most research queries resolve here.
- **Self-hosted orchestration** for deep research through a local-first research loop running against local LLMs on dedicated GPU time. Multi-step synthesis should not require paid model APIs by default.
- **Budget-capped paid APIs** (Brave, Exa) as fallback only when Tier 0 misses.
- **Cached by default** with per-provider freshness windows.
- **Cited + structured** output always; no synthesis without source provenance.

## Architecture

> **Phase 0 specification - no crates implemented yet.** The workspace `Cargo.toml` declares `members = []` because no crates have landed. The four-crate decomposition below is the locked design; canonical state and locked decisions live in [`forkwright/kanon:projects/zetesis/STATE.md`](https://github.com/forkwright/kanon/blob/main/projects/zetesis/STATE.md).

| Crate | Role |
|-------|------|
| `zetesis` | Facade, CLI, daemon binary, adapter traits |
| `syllogē` | Provider abstraction, routing, budget, cache, deep-research-orchestrator wrapper |
| `elenkhos` | Retrospective steel-manning engine |
| `synopsis` | Briefing synthesizer |

## Consumer map

- **aletheia** - nous agents call zetesis as their primary research tool
- **dioptron** - sovereign web runtime; D5 tiered knowledge store ingests zetesis results
- **akroasis** - OSINT domain public-source research

## Non-goals

- Not a conversational search UI (consumer concern)
- Not a content crawler (route to Firecrawl / trafilatura when needed)
- Not an LLM synthesis engine (consumer's LLM layer)
- Not a vector store (that is `heurēma`)
- Not a credentials manager (operator vault owns API keys)

## Development

Current planning authority lives in `forkwright/kanon:projects/zetesis/`. This public README stays stable: purpose, boundaries, consumer map, and the locked crate shape. It should not duplicate the live roadmap.

## License

AGPL-3.0-or-later
