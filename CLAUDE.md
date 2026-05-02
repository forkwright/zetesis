<!--
scope: zetesis repo conventions (research substrate: Tier-0 free APIs, self-hosted deep research, budget ledger)
defers_to: ~/menos-ops/CLAUDE.md for machine topology (including `menos gpu research` mode); ~/.claude/CLAUDE.md for operator principles; kanon standards for universal engineering policy
tightens: free-first routing discipline, budget as first-class constraint, caching-by-default via koina+fjall
-->

# zetesis

Sovereign research substrate. Free-first routing across academic + reference APIs; self-hosted orchestration for deep research on local LLMs; budget-capped paid APIs as fallback.

## Status

Phase 0 specification. Design in flight; planning authority lives in kanon project docs.

Implementation starts in Phase 1 with the four-crate workspace: `zetesis`, `sylloge`, `elenkhos`, and `synopsis`.

## Repository conventions

- Fleet-standard kanon conventions apply: snafu errors, tokio-async, no blanket clippy suppressions, `#[non_exhaustive]` on every pub enum, `cfg_attr(not(test), deny(unwrap_used/expect_used))` in library crates.
- License: AGPL-3.0-or-later. Matches the fleet default. Client-contract (Summus-adjacent) work does NOT go here.
- Workspace member crates under `crates/<crate-name>/`; flat layout (no nested `crates/zetesis/<subcrate>/` pattern unless the workspace grows past ~10 crates).

## Why this repo instead of a kanon crate

Zetesis has at least three active/planned fleet consumers (aletheia, dioptron, akroasis) plus likely non-fleet use. Provider APIs (Brave, Exa, etc.) shift pricing and rate limits frequently, so release cadence naturally differs from kanon's. Public repo from day one avoids forge-private-access friction for any consumer, matches the "spin out when big enough" pattern established by pinax (pending Phase 4 checkpoint).

## Key design principles

- **Free-first.** Tier 0 (free/quality APIs: Semantic Scholar, arXiv, OpenAlex, Crossref, PubMed, Wikipedia) is the default. Tier 1 paid APIs (Brave, Exa, Tavily) are fallbacks, not first choice.
- **Self-hosted orchestration default.** The planned deep-research surface vendors the local-first loop pattern into Rust against logismos-compatible local LLMs. Paid deep-research APIs (You.com, Valyu) stay reserved for budget-authorized critical queries.
- **Budget is a first-class constraint.** Per-query, per-day, per-agent ceilings. Exceed rejects.
- **Cached by default.** koina+fjall with per-provider freshness windows.
- **Cited + structured.** No synthesis without source provenance.

## Common gotchas

- Free-tier APIs have aggressive rate limits; `sylloge` tracks free-tier quotas separately from paid spend.
- Deep research can blow $10+ in token costs per query if orchestrated against Anthropic/OpenAI. Default backend is local logismos.
- `menos gpu research` mode (Phase 6, in coordination with menos-ops) is exclusive with `menos gpu inference` on the W7900; operator picks.
- License is AGPL-3.0-or-later - downstream consumers must comply; Summus-adjacent client work must not depend on zetesis.

## Related

| Project | Relationship |
|---------|-------------|
| aletheia (nous agents) | Primary consumer |
| dioptron | Consumer for web-runtime knowledge ingestion |
| akroasis | Consumer for OSINT public-source research |
| logismos | Self-hosted orchestration backend |
| koina + fjall | Cache + budget ledger persistence |
| heurēma | Future semantic rerank of Tier 0 results |
| menos-ops | `menos gpu research` mode owner |
| hermeneus (inside aletheia) | Sibling primitive: hermeneus unifies LLM providers, zetesis unifies research providers |
