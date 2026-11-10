# Agent Guidelines for Inscribe

Read CONSTRAINTS.md before writing code. Do not weaken it to make a change pass.

## Core Rules
1. **Never suppress compiler or clippy lints**: Never add `#[allow(...)]` or `#![allow(...)]` to silence checks without explicit human sign-off.
2. **Never skip or weaken tests**: Do not add `#[ignore]`, delete tests, or strip assertions to get to green.
3. **No stubs in production**: Do not leave `todo!()`, `unimplemented!()`, or panic stubs in production paths.
4. **Enforce Drive Guard safety**: Ensure system root partitions, internal NVMe/SATA drives, and non-removable media are never targetable.
5. **Zero disk buffering**: All archive decompression must stream directly to the block writer without creating temporary files on host storage.
