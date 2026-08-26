// WHY(zetesis#58): guards against the drift class that let AGENTS.md keep
// citing closed issue #10 as open work after `_llm/current_state.toml`
// diverged from it -- two authored copies of the same tracker fact with no
// shared source. Neither assertion below can be satisfied by restoring that
// pattern.

const AGENTS_MD: &str = include_str!("../../../AGENTS.md");
const CARGO_TOML: &str = include_str!("../../../Cargo.toml");
const CLAUDE_MD: &str = include_str!("../../../CLAUDE.md");
const CURRENT_STATE_TOML: &str = include_str!("../../../_llm/current_state.toml");
const GATE_WORKFLOW: &str = include_str!("../../../.github/workflows/gate-attestation.yml");
const LICENSE: &str = include_str!("../../../LICENSE");
const LICENSE_DOCS: &str = include_str!("../../../LICENSE-DOCS");
const README_MD: &str = include_str!("../../../README.md");

#[test]
fn agents_md_open_work_does_not_duplicate_issue_state() {
    let after_heading = AGENTS_MD.split("## Open work").nth(1);
    let Some(open_work) = after_heading else {
        panic!("AGENTS.md must have an `## Open work` section");
    };
    let open_work = open_work.split("## Gate").next().unwrap_or(open_work);

    assert!(
        !open_work.contains('#'),
        "AGENTS.md `## Open work` must not hardcode an issue reference (e.g. \
         `#10`) -- point at `_llm/current_state.toml` `[[open_threads]]` \
         instead so tracker state has exactly one authoritative copy"
    );
}

#[test]
fn current_state_open_threads_has_no_bare_issue_binding() {
    assert!(
        !CURRENT_STATE_TOML.contains("\nissue = "),
        "`_llm/current_state.toml` `[[open_threads]]` must not bind a bare \
         `issue = N` field -- that field is not mechanically kept in sync \
         with GitHub issue state and drifted stale for #10 (closed \
         2026-05-29 but still cited here as open); describe status in prose \
         until a derivation mechanism from the tracker exists"
    );
}

#[test]
fn public_license_claims_match_the_licensed_artifact() {
    const CODE_LICENSE_HEADER: &str = "# PolyForm Noncommercial License 1.0.0";
    const CODE_SCOPE: &str = "Code and tooling: [PolyForm Noncommercial 1.0.0](LICENSE).";
    const DOCS_LICENSE_HEADER: &str =
        "Creative Commons Attribution-NonCommercial-NoDerivatives 4.0";
    const DOCS_SCOPE: &str = "Documentation: [CC BY-NC-ND 4.0](LICENSE-DOCS).";
    const CARGO_LICENSE_ASSIGNMENT: &str = "license = \"LicenseRef-PolyForm-Noncommercial-1.0.0\"";

    assert!(
        CARGO_TOML
            .lines()
            .any(|line| line.trim() == CARGO_LICENSE_ASSIGNMENT),
        "Cargo.toml workspace package must assign the PolyForm Noncommercial SPDX reference"
    );
    assert_eq!(
        LICENSE.lines().next(),
        Some(CODE_LICENSE_HEADER),
        "LICENSE must be the named PolyForm Noncommercial code/tooling artifact"
    );
    assert_eq!(
        LICENSE_DOCS.lines().next(),
        Some(DOCS_LICENSE_HEADER),
        "LICENSE-DOCS must be the named CC BY-NC-ND documentation artifact"
    );
    assert!(
        README_MD.contains(CODE_SCOPE) && README_MD.contains(DOCS_SCOPE),
        "README.md must map code/tooling and documentation to their distinct license artifacts"
    );
    assert!(
        CLAUDE_MD.contains(CODE_SCOPE) && CLAUDE_MD.contains(DOCS_SCOPE),
        "CLAUDE.md must preserve the scoped code/tooling and documentation license map"
    );
    let superseded_license = concat!("AG", "PL");
    assert!(
        !README_MD.contains(superseded_license) && !CLAUDE_MD.contains(superseded_license),
        "public repository guidance must not claim superseded copyleft terms"
    );
}

#[test]
fn compile_fail_contracts_are_executed_by_ci() {
    assert!(
        GATE_WORKFLOW.contains("doctest_cmd: \"cargo test --workspace --doc\""),
        "the hybrid gate must execute compile-fail doctests"
    );
}
