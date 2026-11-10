# Claude / Agent Guidelines: Inscribe

Read CONSTRAINTS.md before writing code. Do not weaken it to make a change pass.

- **Check fast**: `cargo check --workspace --all-targets` && `cargo clippy --workspace --all-targets -- -D warnings` && `cargo fmt --all -- --check`
- **Floor**: No `#[allow(...)]`, no `#[ignore]`, no `todo!()` or `unimplemented!()` in production paths, no secrets.
- **Safety**: Drive Guard must strictly reject OS system disks and internal non-removable media.
