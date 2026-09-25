<!--
scope: zetesis dispatch conventions and agent entry points
defers_to: CLAUDE.md for repo conventions and design principles; kanon standards for universal engineering policy
tightens: gate discipline (run the gate before pushing, no history rewrites, no AI indicators)
-->

# zetesis — agent entry point

Read CLAUDE.md first for repo conventions and design principles.

## Entry points

- `README.md` — purpose, boundaries, consumer map
- `CLAUDE.md` — design principles, repo conventions, gotchas
- `docs/design/contract-baseline.md` — public API inventory (enforced versus convention), contract ownership, identity, evidence envelope, storage
- `_llm/architecture.toml` — landed and planned crate roles
- `_llm/current_state.toml` — current phase, open threads
- `_llm/decisions.toml` — accepted design decisions
- `_llm/glossary.toml` — domain vocabulary
- `crates/sylloge/src/lib.rs` — full public surface re-exports

## Current state

Pre-release. Four-crate workspace: `zetesis`, `sylloge`, `elenkhos`, and `synopsis`.
`sylloge` owns the provider, constraint, network-target, citation, result, cost, budget, and deep-research lifecycle types. `zetesis` re-exports them as the facade.
`elenkhos` and `synopsis` are marker types holding their crate boundary.
`LocalDeepResearch` is an in-memory task lifecycle with an offline five-node loop fixture; it calls no model and no network.
`StaticAcquirer` is the only HTTP client (anonymous static GET with per-hop validation, bounded decoding, static extraction, and evidence envelope v1). No provider adapter, cache, durable ledger, or model binding exists. `docs/design/contract-baseline.md` lists what each public type enforces and what is caller convention.

## Open work

See `_llm/current_state.toml` `[[open_threads]]` — the single authoritative record of open implementation trackers. Do not restate issue numbers or status here; a second authored copy is what let this section cite a closed issue as open work.

## Gate

Run locally before pushing:

```sh
cargo fmt --all -- --check
cargo check --workspace --all-targets
cargo clippy --workspace --all-targets -- -D warnings
cargo nextest run --workspace
cargo test --workspace --doc
cargo deny check
```

`cargo test --workspace --doc` runs the compile-fail contracts that `cargo nextest` skips. The acceptance verifier is `kanon gate`.

The `Gate-Passed:` commit trailer is advisory (fleet rule since 2026-09-08). The bar is the forge-published gate-attestation status plus the repository's required checks.

## Forbidden

- Do not rewrite git history unilaterally (log a GitHub issue instead).
- Do not merge release-please PRs (operator-decided).
- Do not add AI indicators to commits, code, or comments.
