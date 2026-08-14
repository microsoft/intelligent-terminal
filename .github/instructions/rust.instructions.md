---
description: 'Concise Rust coding conventions for this repository'
applyTo: '**/*.rs'
---

# Rust conventions

Follow the existing crate's architecture and style before introducing a new
pattern or dependency.

## Implementation

- Prefer clear ownership and borrowing over cloning or unnecessary allocation.
- Use domain types and enums to make invalid states difficult to represent.
- Accept `&str` or borrowed values when ownership is not required.
- Keep iterators lazy when that improves clarity; use direct loops when they
  are easier to read.
- Keep async work non-blocking. Move blocking filesystem or process work to the
  crate's established blocking boundary.
- Avoid `unsafe`. When it is required, minimize its scope and document the
  safety invariant.
- Do not add abstraction, traits, builders, or dependencies without a concrete
  need in the changed code.

## Errors

- Return `Result` for recoverable failures and add context at I/O, process,
  parsing, and protocol boundaries.
- Use the crate's existing error type and conventions; do not introduce a new
  error-handling dependency solely for a local change.
- Avoid `unwrap`, `expect`, and panics in production paths unless an invariant
  is both local and demonstrably impossible to violate.
- Do not silently discard errors. Log, propagate, or explicitly document why
  an error is intentionally ignored.

## Documentation and tests

- Comment invariants, non-obvious concurrency, and compatibility constraints;
  do not narrate straightforward code.
- Add focused tests for behavior changes and regressions. Keep unit tests near
  the code unless the crate already uses an integration-test boundary.
- Do not require every private item to have documentation or every change to
  introduce a new abstraction.

## Validation

- Run `cargo fmt` for changed Rust code.
- Run the smallest relevant tests while iterating, then the crate's required
  test command.
- Run Clippy only when the crate already uses it or the task requires it; do
  not perform unrelated cleanup.
- Keep builds warning-free under the repository's configured toolchain.
