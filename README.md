# zetesis

*ζήτησις - systematic inquiry.*

Sovereign research substrate. Unifies research and search providers (free-quality academic APIs, paid APIs, self-hosted orchestration) behind one Rust interface with budget enforcement, rate-limit management, cited result normalization, and a cache layer.

**Status:** Phase 0 specification. Design in flight; no code yet beyond workspace manifest. See `forkwright/kanon:projects/zetesis/` for planning docs.

## Why

Frontier-model built-in search is an opaque black-box priced per-token. Vendor aggregators (Brave / Exa / Tavily / You.com / Valyu / Perplexity) are pay-per-query with little architectural control and costs that compound fast at agent scale.

Zetesis takes a different shape:

- **Free-first routing** across free-quality academic and reference APIs (Semantic Scholar, arXiv, OpenAlex, Crossref, PubMed, Wikipedia). Most research queries resolve here.
- **Self-hosted orchestration** for deep research via GPT Researcher / LangChain `open_deep_research` running against local LLMs on dedicated GPU time. Multi-step synthesis at zero marginal cost.
- **Budget-capped paid APIs** (Brave, Exa) as fallback only when Tier 0 misses.
- **Cached by default** with per-provider freshness windows.
- **Cited + structured** output always; no synthesis without source provenance.

## Architecture (target)

```
zetesis-api           trait surface: Provider, DeepResearch, Crawler + result types
zetesis-providers     per-provider impls (feature-gated)
zetesis-router        query-shape classifier + tier selection + fallback chains
zetesis-budget        per-query / per-day / per-agent ceilings, cross-provider spend ledger
zetesis-cache         query-hash keying, per-provider freshness windows
zetesis-orchestrator  GPT Researcher + open_deep_research wrappers
zetesis-mcp           MCP tool surface for claude-code + fleet agents
zetesis-cli           operator inspection: budget, cache stats, providers, deep submit
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

Phase 0 design doc in flight. Phase 1 implementation starts with `zetesis-api` trait surface + the first wave of Tier 0 providers (Semantic Scholar, arXiv, OpenAlex, Crossref, PubMed, Wikipedia).

Planning docs live in `forkwright/kanon:projects/zetesis/`:

- `vision.md` - purpose and principles
- `STATE.md` - current phase and locked decisions
- `ROADMAP.md` - 10-phase plan
- `CLAUDE.md` - agent orientation
- `phases/00-spec/PLAN.md` - Phase 0 detail

## License

AGPL-3.0
