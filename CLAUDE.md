# zetesis

Sovereign research substrate. Free-first routing across academic + reference APIs; self-hosted orchestration for deep research on local LLMs; budget-capped paid APIs as fallback.

## Status

Phase 0 specification. Design in flight; planning docs live in `forkwright/kanon:projects/zetesis/`:

- vision, STATE, ROADMAP, CLAUDE
- phases/00-spec/PLAN.md (Phase 0 detail)

Implementation starts in Phase 1 with `zetesis-api` trait surface + Tier 0 providers.

## Repository conventions

- Fleet-standard kanon conventions apply: snafu errors, tokio-async, zero blanket clippy suppressions, `#[non_exhaustive]` on every pub enum, `cfg_attr(not(test), deny(unwrap_used/expect_used))` in library crates.
- License: AGPL-3.0. Matches aletheia / harmonia / akroasis. Client-contract (Summus-adjacent) work does NOT go here.
- Workspace member crates under `crates/<crate-name>/`; flat layout (no nested `crates/zetesis/<subcrate>/` pattern unless the workspace grows past ~10 crates).

## Why this repo instead of a kanon crate

Zetesis has at least three active/planned fleet consumers (aletheia, dioptron, akroasis) plus likely non-fleet use. Provider APIs (Brave, Exa, etc.) shift pricing and rate limits frequently, so release cadence naturally differs from kanon's. Public repo from day one avoids forge-private-access friction for any consumer, matches the "spin out when big enough" pattern established by pinax (pending Phase 4 checkpoint).

## Key design principles

- **Free-first.** Tier 0 (free/quality APIs: Semantic Scholar, arXiv, OpenAlex, Crossref, PubMed, Wikipedia) is the default. Tier 1 paid APIs (Brave, Exa, Tavily) are fallbacks, not first choice.
- **Self-hosted orchestration default.** GPT Researcher + `open_deep_research` on logismos local LLMs handle multi-step synthesis. Paid deep-research APIs (You.com, Valyu) reserved for budget-authorized critical queries.
- **Budget is a first-class constraint.** Per-query, per-day, per-agent ceilings. Exceed rejects.
- **Cached by default.** koina+fjall with per-provider freshness windows.
- **Cited + structured.** No synthesis without source provenance.

## Common gotchas

- Free-tier APIs have aggressive rate limits; `zetesis-budget` tracks free-tier quotas separately from paid spend.
- Deep research can blow $10+ in token costs per query if orchestrated against Anthropic/OpenAI. Default backend is local logismos.
- `menos gpu research` mode (Phase 6, in coordination with menos-ops) is exclusive with `menos gpu inference` on the W7900; operator picks.
- License is AGPL-3.0 - downstream consumers must comply; Summus-adjacent client work must not depend on zetesis.

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
