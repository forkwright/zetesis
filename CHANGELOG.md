# Changelog

## [0.1.0](https://github.com/forkwright/zetesis/compare/v0.0.6...v0.1.0) (2026-09-25)


### ⚠ BREAKING CHANGES

* **sylloge:** Provider has a single search method returning ProviderAnswer. ProviderAttempt gains evidence fingerprints; ErrorClass is serializable.
* **sylloge:** acquire returns Acquisition; Transfer and TransferOutcome are removed. AcquisitionLimits gains max_decoded_bytes and max_text_bytes.
* **sylloge:** Crawler and PageContent are removed; use StaticAcquirer. Their limit values are AcquisitionLimits ceilings.
* **sylloge:** BudgetConstraint::phase_zero_default is removed; start from free_only() and set ceilings with the with_* builders.

### Features

* **sylloge:** bound transfer and return versioned evidence envelopes ([#90](https://github.com/forkwright/zetesis/issues/90)) ([5b02d63](https://github.com/forkwright/zetesis/commit/5b02d633820547699868234e71fff584e39cd53f))
* **sylloge:** own static acquisition target proof through every hop ([#89](https://github.com/forkwright/zetesis/issues/89)) ([851a430](https://github.com/forkwright/zetesis/commit/851a4309e165c6f2fc4836194621dd7a24386137))
* **sylloge:** route the free provider cohort through the static acquirer ([#92](https://github.com/forkwright/zetesis/issues/92)) ([474f7fc](https://github.com/forkwright/zetesis/commit/474f7fcf1d8fec4e8676f2c61bf9a4f1f4da9403))


### Bug Fixes

* **sylloge:** block IPv4 destinations carried in IPv6 transition forms ([#87](https://github.com/forkwright/zetesis/issues/87)) ([dc164e9](https://github.com/forkwright/zetesis/commit/dc164e9ae806b10c5c7b15ae13b009b159b32ba9))
* **sylloge:** fail closed on unusable input and record the contract ([#85](https://github.com/forkwright/zetesis/issues/85)) ([0bff055](https://github.com/forkwright/zetesis/commit/0bff055193ba5bc0d594bb0468f0c4e682f7e39d))

## [0.0.6](https://github.com/forkwright/zetesis/compare/v0.0.5...v0.0.6) (2026-08-26)


### Bug Fixes

* **sylloge:** harden static acquisition boundaries ([#78](https://github.com/forkwright/zetesis/issues/78)) ([c922b43](https://github.com/forkwright/zetesis/commit/c922b43b909cf43179b2bab6730ca40e9f7edf0b))

## [0.0.5](https://github.com/forkwright/zetesis/compare/v0.0.4...v0.0.5) (2026-08-09)


### Bug Fixes

* **docs:** stop citing closed [#10](https://github.com/forkwright/zetesis/issues/10) as zetesis open work ([#62](https://github.com/forkwright/zetesis/issues/62)) ([924e196](https://github.com/forkwright/zetesis/commit/924e196fe3ee3dd092d4e0724e6056a6c7ee301c)), closes [#58](https://github.com/forkwright/zetesis/issues/58)
* **sylloge:** make budget check-and-record atomic, add fleet scope and a custom-budget builder ([#59](https://github.com/forkwright/zetesis/issues/59)) ([1795570](https://github.com/forkwright/zetesis/commit/1795570d5804adbc634589f89b404d34cf55eaca)), closes [#47](https://github.com/forkwright/zetesis/issues/47)
* **sylloge:** promote deep-research cancellation to the trait, make it state-aware ([#60](https://github.com/forkwright/zetesis/issues/60)) ([36bebff](https://github.com/forkwright/zetesis/commit/36bebffb10b08c8c626f7874360b806d24d18226)), closes [#49](https://github.com/forkwright/zetesis/issues/49)
* **sylloge:** separate publication time from retrieval time in freshness checks ([#61](https://github.com/forkwright/zetesis/issues/61)) ([e478aaf](https://github.com/forkwright/zetesis/commit/e478aaf2dd082978596fb3e30bc333cf05e75968)), closes [#50](https://github.com/forkwright/zetesis/issues/50)

## [0.0.4](https://github.com/forkwright/zetesis/compare/v0.0.3...v0.0.4) (2026-08-03)


### Bug Fixes

* **gate-attestation:** scope a documented concurrency suppression ([#57](https://github.com/forkwright/zetesis/issues/57)) ([dcdaeea](https://github.com/forkwright/zetesis/commit/dcdaeea97d4a15b043811b68b4d70bb2398a1bbd)), closes [#44](https://github.com/forkwright/zetesis/issues/44)
* **release:** derive the path-dep version pins release-please patches ([#54](https://github.com/forkwright/zetesis/issues/54)) ([357751b](https://github.com/forkwright/zetesis/commit/357751bdcf978f7c81ec72a2c717787710a3b8a0))

## [0.0.3](https://github.com/forkwright/zetesis/compare/v0.0.2...v0.0.3) (2026-07-29)


### Bug Fixes

* **release:** patch Cargo.lock on release like the rest of the fleet ([#52](https://github.com/forkwright/zetesis/issues/52)) ([5f71f79](https://github.com/forkwright/zetesis/commit/5f71f79f9873e486e7ff43b40ad2eba60bc323da))

## [0.0.2](https://github.com/forkwright/zetesis/compare/v0.0.1...v0.0.2) (2026-07-08)


### Features

* **_llm:** add T0 corpus per [#667](https://github.com/forkwright/zetesis/issues/667) / [#673](https://github.com/forkwright/zetesis/issues/673) fleet rollout ([#8](https://github.com/forkwright/zetesis/issues/8)) ([76a968c](https://github.com/forkwright/zetesis/commit/76a968c1c12a8a22fe51c92686a09c0e42efd9a3))
* **sylloge:** add local deep research lifecycle scaffold ([#23](https://github.com/forkwright/zetesis/issues/23)) ([194a5aa](https://github.com/forkwright/zetesis/commit/194a5aa91f361ad308a9fa200d3a836fbc63c7f6))
* **sylloge:** add offline deep research loop fixture ([#25](https://github.com/forkwright/zetesis/issues/25)) ([9f12464](https://github.com/forkwright/zetesis/commit/9f124644f8beb39e9154bdd688e8eafb7fd69f9a))
* **zetesis:** translate phase 1 scaffold ([9c95990](https://github.com/forkwright/zetesis/commit/9c95990e65c08fb57bcb3ec17174fddd657a8726))


### Bug Fixes

* **sylloge:** enforce budget windows, provenance, and retry semantics from wave-1 audit ([#36](https://github.com/forkwright/zetesis/issues/36)) ([4653c12](https://github.com/forkwright/zetesis/commit/4653c12a30ffc176c82792f3eed5bf4b5508c960)), closes [#30](https://github.com/forkwright/zetesis/issues/30) [#31](https://github.com/forkwright/zetesis/issues/31) [#32](https://github.com/forkwright/zetesis/issues/32) [#33](https://github.com/forkwright/zetesis/issues/33) [#34](https://github.com/forkwright/zetesis/issues/34) [#35](https://github.com/forkwright/zetesis/issues/35)
* **zetesis:** bump every workspace self-dep requirement in release-please ([#42](https://github.com/forkwright/zetesis/issues/42)) ([00bf869](https://github.com/forkwright/zetesis/commit/00bf86971469fa65956ef94e9a25a44f9407b62f))
* **zetesis:** bump workspace elenkhos dep requirement in release-please ([#40](https://github.com/forkwright/zetesis/issues/40)) ([f955365](https://github.com/forkwright/zetesis/commit/f95536580f35d4f59ee77344563cad12c6695027))

## Changelog
