// WHY(zetesis#58): guards against the drift class that let AGENTS.md keep
// citing closed issue #10 as open work after `_llm/current_state.toml`
// diverged from it -- two authored copies of the same tracker fact with no
// shared source. Neither assertion below can be satisfied by restoring that
// pattern.

const AGENTS_MD: &str = include_str!("../../../AGENTS.md");
const CURRENT_STATE_TOML: &str = include_str!("../../../_llm/current_state.toml");

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
