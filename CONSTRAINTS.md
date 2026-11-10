# Constraints: Inscribe

Last reviewed: 2026-11-11 by @aadith

## Floor (Always Enforced, No Setup Required)

- **No new suppression attributes**: No `#[allow(...)]` or `#![allow(...)]` (e.g. `allow(dead_code)`, `allow(unused)`, `allow(clippy::...)`) without explicit documented human sign-off.
- **No unimplemented stubs in production paths**: No `todo!()`, `unimplemented!()`, `panic!()`, or empty `Err`/catch suppression in non-test paths.
- **No skipped or deleted tests**: No `#[ignore]` on tests and no tests deleted without documented justification in commit messages.
- **No stripped assertions**: Assertions must never be removed or weakened to turn a failing test green.
- **No secrets in source**: Zero tokens, passwords, private keys, or credentials committed.
- **Zero compiler warnings**: `cargo check` and `cargo clippy` must pass with zero warnings across all crates and targets.
- **Strict formatting**: `cargo fmt --all -- --check` must pass without drift.
- **Immutable contract**: This file does not get weakened to make a change pass.

## Enforced with Numbers

All checks below are **strictly blocking**: any failure immediately halts progress until resolved.

| Dimension | Rule | Checked by | Runs at | Rationale |
|-----------|------|------------|---------|-----------|
| **Compilation** | Zero compiler errors or warnings | `cargo check --workspace --all-targets` | every edit (<5s) | Prevent defect accumulation |
| **Lint & Idioms** | Zero clippy warnings (`-D warnings`) | `cargo clippy --workspace --all-targets -- -D warnings` | every edit (<5s) | Enforce idiomatic, safe Rust |
| **Code Formatting** | Clean formatting without manual intervention | `cargo fmt --all -- --check` | every edit (<5s) | Consistent formatting across crates |
| **Secrets** | No credentials or private keys in repository | `gitleaks dir . --redact --no-banner` | task end (60-90s), CI | Prevent credential leaks |
| **Test Coverage** | Changed lines in core logic ≥ 80% covered | `cargo llvm-cov --lcov --output-path lcov.info` (or `cargo test`) | task end (60-90s), CI | Prevent untested logic regressions |
| **Dependencies: Vulns** | Zero high or critical security advisories | `cargo audit` | task end, CI | Guard against known crate CVEs |
| **Supply Chain Security** | Zero disallowed licenses, bans, or malicious sources | `cargo deny check` | task end, CI | Mitigate build script attacks (CVE-2026-77649) |
| **Architecture Boundary** | `inscribe-core` strictly headless (zero Slint/GUI dependencies) | `cargo check -p inscribe-core` | every edit (<5s) | Preserve modularity & testability |
| **Performance: Binary Size** | Standalone GUI release binary ≤ 10 MB | `stat -c%s target/release/inscribe-gui` | release build | Core project goal: replace 400MB Electron |
| **Performance: Memory** | Idle application RSS RAM ≤ 20 MB | Process RSS memory inspection on startup | release build | Guarantee ultra-lightweight execution |
| **Accessibility (UI)** | All interactive controls have accessible labels, keyboard focus, and contrast | Slint UI inspection & focus traversal audit | task end | Ensure full keyboard & a11y support |

Every row names the command that produces the verdict. A dimension with a number and no command is an aspiration, not a constraint.

## Lifecycle Time Budgets

| Phase | What Runs | Time Budget | Action on Failure |
|-------|-----------|-------------|-------------------|
| **EDIT (Fast Loop)** | `cargo check`, `cargo clippy`, `cargo fmt`, architecture boundary check | < 5 seconds | Block immediately |
| **TASK END (Verification)** | `cargo test --workspace`, coverage, `cargo audit`, `cargo deny`, `gitleaks` | 60–90 seconds | Block task completion |
| **RELEASE / CI** | Full release build with fat LTO, binary size check (≤10MB), RSS check (≤20MB) | Unlimited | Block release |

## Measured, Not Yet Enforced

| Metric | Target / Baseline | Direction |
|--------|-------------------|-----------|
| Write Throughput | Hardware bus line speed (direct I/O zero-copy streaming) | must not regress |
| GUI Startup Time | < 100 ms to interactive native window | must not grow |
| Full Workspace Test Time | < 30 seconds | must not grow |

## Exceptions

| ID | Rule | Path | Reason | Owner | Expires |
|----|------|------|--------|-------|---------|
| — | None | — | Clean slate starting point | — | — |
