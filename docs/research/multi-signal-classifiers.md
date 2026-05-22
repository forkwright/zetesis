# Multi-Signal Classifier Notes

Zetesis classifiers should combine independent evidence signals into an
inspectable decision record. A classifier is not allowed to emit a bare label
without the source-backed signals that produced it.

## Decision Record

Each classifier result should carry:

- `label` - the selected classification.
- `confidence` - normalized score in the classifier's documented range.
- `signals` - ordered evidence entries used by the decision.
- `thresholds` - the accept, reject, and needs-review cutoffs.
- `score_breakdown` - final weighted total, per-signal contributions, and
  margin to the nearest threshold.
- `source_refs` - citations or provider result identifiers for every factual
  signal.
- `version` - classifier name and ruleset version.

Signals should be independent where practical. If two signals derive from the
same provider payload, mark the shared source so reviewers can see correlated
evidence instead of treating it as separate support.

## Signal Shape

Each signal should include:

- `id` - stable signal identifier.
- `weight` - configured contribution to the final score.
- `score` - observed signal score before weighting.
- `contribution` - signed weighted effect after applying `weight`, `score`, and
  `direction`.
- `direction` - `supports`, `opposes`, or `neutral`.
- `evidence` - short explanation tied to `source_refs`.
- `missing_policy` - whether absence is neutral, negative, or review-blocking.

Weights belong in versioned configuration, not inline call sites. Runtime code
may tune thresholds by classifier version, but it should not silently change
weights based on provider availability.

## Review Rules

Reviewers must be able to answer four questions from a saved classifier result:

1. Which sources contributed to the label?
2. Which signal had the largest effect on the score?
3. Which missing signals changed the review path?
4. Which test fixture proves this exact threshold behavior?

Any classifier that cannot answer those questions should return `needs_review`
until its decision record is complete.

For review, sort `score_breakdown` by absolute contribution and show the
threshold margin next to the selected label. That lets reviewers inspect whether
the result was driven by a single dominant signal, several weak signals, missing
evidence policy, or a near-threshold score that should stay in manual review.

## Tests

Before a classifier is exported to consumers, add fixtures for:

- one clear positive result;
- one clear negative result;
- one boundary result near each threshold;
- one result with a missing high-weight signal;
- one result with conflicting high-weight signals.

Fixtures should assert both the final label and the per-signal contribution
list. This keeps future weight or threshold changes visible in review.
