# Contributing to OxiStore

Thank you for your interest in contributing. OxiStore is part of the
**COOLJAPAN ecosystem** (https://github.com/cool-japan/oxistore), a
family of Pure Rust libraries that replace common C/C++/Fortran-backed
crates with FFI-free implementations.

## Building and testing

```bash
# Build the workspace
cargo build --workspace

# Run the test suite (nextest is required; do not rely on `cargo test` alone)
cargo nextest run --workspace

# Lint — this MUST produce zero warnings before a change is accepted
cargo clippy --all-targets -- -D warnings

# Format
cargo fmt --all
```

## Project rules

These rules are enforced in review and, where possible, in CI:

- **Pure Rust by default.** No new C/C++/Fortran dependency, and no
  non-default C feature, may be added without an explicit, documented
  exception. Prefer existing COOLJAPAN replacements over `-sys` crates.
- **No panics on untrusted input.** Do not add `.unwrap()`, `.expect()`,
  `panic!()`, `unreachable!()`, or `assert!()` on data derived from
  untrusted input outside of test code; return the crate's existing
  typed error instead.
- **Zero clippy warnings.** `cargo clippy --all-targets -- -D warnings`
  must pass cleanly with default features.
- **Workspace dependency inheritance.** Shared dependencies are
  declared once in the workspace `[workspace.dependencies]` table and
  pulled in via `dep.workspace = true`; do not pin ad hoc versions in a
  member crate's `Cargo.toml` when the workspace already centralizes
  that dependency.
- **File size.** Keep individual source files under 2000 lines; split
  oversized files into focused modules.
- **Latest crates.** Prefer the latest versions available on crates.io
  for new or updated dependencies.
- **No hardcoded absolute paths.** Tests must use
  `std::env::temp_dir()` (or an equivalent relative/portable path) for
  any temporary file handling.

## Submitting changes

Open a pull request against the appropriate version branch (not
directly against a release branch, unless the project is pre-0.1.0).
Describe what changed and why, and make sure the build/test/lint
commands above all pass locally first.
