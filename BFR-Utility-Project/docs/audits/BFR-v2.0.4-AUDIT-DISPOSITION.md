# BFR Macro & Tracking Utility — v2.0.4 Audit Disposition

**Status:** CLOSED — PASS WITH OBSERVATIONS  
**Release:** v2.0.4  
**Audit scope:** Read-only source and existing-test review of the exact v2.0.4 release tag  
**Disposition date:** 2026-11-14

## Executive disposition

The v2.0.4 independent audit is formally closed. The overall result is **PASS WITH OBSERVATIONS**. No Critical, High, or Medium findings were identified. The remaining observations are documented technical debt, conditional risks, or unconfirmed edge cases; none currently justifies a v2.0.4 hotfix.

The phrase “passed with flying colours” may be used as an informal summary of the outcome, but the precise audit classification remains **PASS WITH OBSERVATIONS**, not an assertion that every possible edge case has been eliminated.

Keep v2.0.4 stable and frozen. Continue the existing v2.0.5 development scope without expanding it to accommodate speculative audit fixes.

## Finding-by-finding disposition

| ID | Finding | Final assessment | Disposition |
|---|---|---|---|
| L-01 | Windows-only crates in unconditional dependencies | Portability concern; no demonstrated Windows runtime defect | Defer. Consider target-specific dependencies if non-Windows build/test support becomes a requirement. |
| L-02 | Local WebView2 DevTools endpoint | Loopback-bound and endpoint-validated; local-process hardening remains possible | Defer defense-in-depth hardening. No internet-wide exposure was demonstrated. |
| L-03 | Stop macro playback race/cancellation | Narrow residual race around sequential CDP mouse events; discovery is not promptly cancellable. Existing post-viewport generation check is present. | Retain as a low-severity edge case. No deterministic reproduction was performed. |
| L-04 | Single-run reward de-duplication fallback | Missing Begin Quest timestamp can leave a single-run completion without a dedupe key; duplicate creation was not reproduced. | Retain as unconfirmed. Do not introduce speculative hash/time-bucket deduplication without authoritative game identifiers or reproduction evidence. |
| L-05 | Reward history cache retry | Retry behavior and regression test are already present in v2.0.4 and v2.0.5 | Withdraw finding. No change required. |
| L-06 | Invalid settings block saves | Intentional fail-safe preserves malformed/unreadable settings rather than overwriting them | Defer recovery UX. Preserve the current data-protection behavior. |
| L-07 | Run History durability and clearing | No per-record forced sync; clearing has staging and rollback protections | Defer. Keep current clearing behavior; document durability policy before considering sync changes. |
| L-08 | Unbounded application log | Active application log has no size limit/rotation | Defer bounded log rotation/archive to a later release. |
| L-09 | Trusted game origin/native command boundary | Conditional trust risk if the trusted game origin is compromised; no unrestricted remote native command bundle was identified | Defer same-origin context validation and rate limiting as defense-in-depth. |
| L-10 | Release pipeline | Next-release workflow could improve tag/commit verification, action/toolchain pinning, and provenance | Do not alter the v2.0.4 workflow. Harden the next release workflow. |

## Validation evidence and limitations

- The independent audit report recorded 64/64 JavaScript tests passing, zero vulnerabilities in the JavaScript dependency audit, and zero known vulnerabilities in the Rust dependency advisory check.
- The original auditor could not run Rust tests or Clippy because its Linux build failed before test execution.
- The auditor used scratch-harness tests on copied functions with synthetic data; those are not equivalent to full application runtime tests.
- Windows runtime, installer, upgrade/uninstall, WebView2, and CDP playback were not independently tested by that auditor.
- The user's separate hands-on Windows verification of v2.0.4 recorded 12/12 checks passed. This is retained as separate project evidence and was not repeated as part of this follow-up.
- This follow-up was a read-only source and existing-test review. It did not execute cargo test, Clippy, npm tests, Linux/Windows builds, installer tests, or runtime playback tests.
- L-04 was not reproduced using live game traffic or sanitized reward-response samples.
- No performance benchmark was run. The previously closed Stage 4 performance benchmarking workstream remains closed.

## Release and development decisions

### v2.0.4
- Remains stable and frozen.
- No hotfix is warranted by the reviewed observations.
- Reconsider a v2.0.4.x hotfix only if a material user-impacting defect is reproduced.

### v2.0.5
- Continue the existing development scope.
- Do not add speculative audit fixes or broaden scope based on unconfirmed observations.
- Do not weaken corrupt-settings preservation or Run History clearing safeguards.
- Stage 4 performance benchmarking remains closed.
- Apply the normal review, regression-test, and validation process to planned v2.0.5 changes.

### Later release candidates
1. Guided settings recovery that backs up invalid settings before offering regeneration.
2. Bounded application log rotation and archival.
3. A documented Run History durability policy, with any sync change justified by evidence and I/O evaluation.
4. Cancellation-aware CDP discovery and cancellation checks during mouse-event dispatch.
5. Reward de-duplication based on an authoritative server run identifier, if available.
6. Trusted-origin context validation and rate limiting.
7. Tag-driven release workflow with verified tag-to-commit identity, SHA-pinned actions/toolchain, and verifiable build provenance.
8. Target-specific dependency cleanup if non-Windows build/test support is required.

## Repository change record

This disposition is a documentation-only audit closure record. It does not modify application source code, configuration, dependencies, lockfiles, release workflows, user data, or the v2.0.4 release artifact.

**Final conclusion:** v2.0.4 independent audit CLOSED — **PASS WITH OBSERVATIONS**. Remaining observations are deferred or unconfirmed and are not current release blockers.
