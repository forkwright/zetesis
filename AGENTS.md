<!--
scope: zetesis dispatch conventions and agent entry points
defers_to: CLAUDE.md for repo conventions and design principles; kanon standards for universal engineering policy
tightens: gate discipline (truthful Gate-Passed trailers, no history rewrites, no AI indicators)
-->

# zetesis — agent entry point

Read CLAUDE.md first for repo conventions and design principles.

## Entry points

- `README.md` — purpose, boundaries, consumer map
- `CLAUDE.md` — design principles, repo conventions, gotchas
- `_llm/architecture.toml` — planned layers and crate roles
- `_llm/current_state.toml` — current phase, open threads
- `_llm/decisions.toml` — accepted design decisions
- `_llm/glossary.toml` — domain vocabulary
- `crates/sylloge/src/lib.rs` — full public surface re-exports

## Current state

Phase 1 scaffold. Four-crate workspace is present: `zetesis`, `sylloge`, `elenkhos`, and `synopsis`.
`sylloge` owns the provider/result/budget/citation/deep-research surface. `zetesis` re-exports it as the facade.
`elenkhos` and `synopsis` are marker-type scaffolds holding their crate boundary.
`LocalDeepResearch` has the in-memory task lifecycle and offline five-node loop fixture for deterministic testing without network calls.
Real logismos/HTTP integration is deferred — see `_llm/current_state.toml` `[[open_threads]]` for current tracker status.

## Open work

See `_llm/current_state.toml` `[[open_threads]]` — the single authoritative record of open implementation trackers. Do not restate issue numbers or status here; a second authored copy is what let this section cite a closed issue as open work.

## Gate

Every PR commit must carry a truthful `Gate-Passed:` trailer. Run locally first:

```sh
~/.cargo/bin/cargo fmt --all -- --check
~/.cargo/bin/cargo check --workspace --all-targets
~/.cargo/bin/cargo clippy --workspace --all-targets -- -D warnings
~/.cargo/bin/cargo test --workspace --all-targets
```

Then commit with: `Gate-Passed: kanon-ci/local 1.85`

## Forbidden

- Do not rewrite git history unilaterally (log a GitHub issue instead).
- Do not merge release-please PRs (operator-decided).
- Do not add AI indicators to commits, code, or comments.
