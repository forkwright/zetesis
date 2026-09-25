# Security

Zetesis is pre-release. It ships a library surface (`sylloge`, re-exported by
the `zetesis` facade). Its only network client is `StaticAcquirer`
(anonymous static GET), through which the Tier-0 providers (Semantic
Scholar, arXiv, Wikipedia) fetch; no cache or durable ledger exists yet. See
[docs/design/contract-baseline.md](docs/design/contract-baseline.md) for what
each public type enforces today.

Report security issues privately to the maintainer. Do not file public issues for
credentials, provider tokens, prompt-injection bypasses, cache poisoning,
server-side request forgery, or data exfiltration findings.

Security-sensitive design constraints:

- Provider credentials belong to the operator vault or consumer adapter, not to
  zetesis config files. Zetesis handles credential references only; no
  serializable type, cache key, evidence envelope, log line, or error message
  carries a credential value.
- Paid-provider access is disabled until explicitly configured and a
  reservation authorizes the spend. A free-tier miss never enables paid use.
- Network targets fail closed: only `http` and `https`, no userinfo, and no
  resolved loopback, private, link-local, unspecified, multicast, reserved, or
  documentation address, including one embedded in an IPv6 transition form
  (IPv4-mapped, NAT64, 6to4, Teredo). A local target requires `LocalTargetAuthorization`, which has no
  public constructor and cannot be deserialized.
- Static acquisition (planned) validates every redirect hop before connecting,
  connects only to the validated addresses, and never records cookies or
  request credentials in its evidence envelope.
- Research outputs must preserve source provenance; synthesized claims without
  citations are not acceptable output.
- Cache keys and stored results must not include raw secrets or egress policy
  content; egress scope enters only as an identifier.
