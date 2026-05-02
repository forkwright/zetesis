# zetesis

*ζήτησις - systematic inquiry.*

Planned sovereign research substrate for unifying research and search providers behind one Rust interface with budget enforcement, rate-limit management, cited result normalization, and a cache layer.

## Why

Frontier-model built-in search is an opaque black-box priced per-token. Vendor aggregators (Brave / Exa / Tavily / You.com / Valyu / Perplexity) are pay-per-query with little architectural control and costs that compound fast at agent scale.

Zetesis takes a different shape:

- **Free-first routing** across free-quality academic and reference APIs (Semantic Scholar, arXiv, OpenAlex, Crossref, PubMed, Wikipedia). Most research queries resolve here.
- **Self-hosted orchestration** for deep research through a local-first research loop running against local LLMs on dedicated GPU time. Multi-step synthesis should not require paid model APIs by default.
- **Budget-capped paid APIs** (Brave, Exa) as fallback only when Tier 0 misses.
- **Cached by default** with per-provider freshness windows.
- **Cited + structured** output always; no synthesis without source provenance.

## Architecture (planned)

```
zetesis   facade crate, CLI/daemon entrypoints, consumer adapter traits
sylloge   provider abstraction, routing, budget, cache, deep-research loop
elenkhos  retrospective steel-manning engine
synopsis  briefing synthesizer
```

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

Current planning authority lives in kanon project docs. This public README stays stable: purpose, boundaries, consumer map, and the planned crate shape. It should not duplicate the live roadmap.

## License

AGPL-3.0-or-later
