# Security

Zetesis is pre-release and does not yet ship implementation crates.

Report security issues privately to the maintainer. Do not file public issues for
credentials, provider tokens, prompt-injection bypasses, cache poisoning, or data
exfiltration findings.

Security-sensitive design constraints:

- Provider credentials belong to the operator vault or consumer adapter, not to
  zetesis config files.
- Paid-provider access must be budget-gated and disabled by default.
- Research outputs must preserve source provenance; synthesized claims without
  citations are not acceptable output.
- Cache keys and stored results must not include raw secrets.
