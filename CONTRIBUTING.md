# Contributing to Editage

Thank you for helping. Editage is small on purpose, and people use it to hold
text they care about. Most of this guide is about keeping it predictable and
easy to audit.

## Before you open a pull request

Run these from the repository root. CI runs the same checks on every push
and pull request (see [.github/workflows/ci.yml](.github/workflows/ci.yml)),
plus `cargo deny check`.

```sh
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace -- --include-ignored
```

`--include-ignored` also runs the interoperability test that decrypts
Editage's output with the reference age CLI. It needs `age`
(`brew install age`) and `expect` (part of macOS). Without them, run
`cargo test --workspace` and say so in the pull request.

If your change touches the user interface, also go through the relevant
parts of [docs/manual-test-checklist.md](docs/manual-test-checklist.md) by
hand and say in the pull request which items you checked. Automated tests do
not cover the AppKit frontend.

The interoperability fixtures in `tests/fixtures/` are made by the reference
age CLI with `tests/fixtures/generate-fixtures.sh`.

## Changes to storage or security behaviour

A change that affects how documents are read, encrypted, saved, locked, or
kept in memory, or what the application writes anywhere, must update in the
same pull request:

1. the tests that cover the behaviour (add tests for new behaviour);
2. [docs/security-model.md](docs/security-model.md);
3. [docs/save-protocol.md](docs/save-protocol.md), if the save transaction is
   affected. That document must match `run_save_transaction` in
   `crates/editage-core/src/save.rs` step by step;
4. the user-facing description of the mechanism. The "How saving works" text
   in the Security Inspector is generated from `SaveStage::mechanism_step` in
   `crates/editage-core/src/save.rs`. If a stage changes, its sentence must
   change with it. The same applies to the other user-facing text in
   `presentation.rs` and `security_state.rs`: it must describe what the code
   does, and nothing more.

Reviewers will ask for these if they are missing. A behaviour change whose
documentation still describes the old behaviour is treated as a bug.

## Security bugs

Please report security problems privately, as described in
[SECURITY.md](SECURITY.md).

Every security fix comes with a regression test that fails without the fix.
These tests are permanent: do not delete or weaken them. If one needs to
change because the design changed, explain why in the pull request.

## Code review

- Prefer obvious code to condensed code. A reviewer who has not seen the code
  before should be able to follow what happens to a plaintext buffer or a
  passphrase by reading it top to bottom.
- Name things after what they do. Make copies of secrets visible at the call
  site (see `Passphrase::duplicate_for_operation`).
- Keep errors structured (`EditorError`) until `presentation.rs` turns them
  into sentences. Do not format errors into strings deep in the core.
- Do not add wording that claims more than the code does. For example, write
  "requests zeroization of the buffers it controls", not "securely wiped".

## Dependencies

Every new dependency must answer the question "Why do we need this?" in the
pull request, and the answer goes into the dependency table in
[docs/architecture.md](docs/architecture.md). Prefer the standard library.
Prefer a few lines of plain code to a crate. Enable only the features you
use. `cargo deny check` (see [deny.toml](deny.toml)) must pass. Networking
crates are rejected outright: Editage makes no network requests.

## `unsafe`

- `unsafe` belongs only in the platform layer: system calls and platform
  APIs: a few `libc` calls in `crates/editage-core/src/storage.rs` and the
  Objective-C interop in the AppKit frontend (`apps/macos/`).
- Keep each `unsafe` block as small as possible.
- Every `unsafe` block has a `// SAFETY:` comment that explains why the call is
  sound.

## Secrets and `Debug`

Never derive `Debug` (or `Display`, `Serialize`, `Clone`) on a type that
holds a passphrase, a credential or plaintext, or on a type that contains one.
Write a manual `Debug` implementation that redacts the secret, as the types in
`secrets.rs`, `SaveJob`, `UnlockJob` and `DocumentSession` do. Tests check
that `Debug` output does not contain secrets; add a similar test for any new
type that holds one.

## Diagnostics

The in-memory diagnostics history must never contain document text,
passphrases, clipboard contents, selections or search strings. Add a typed
`DiagnosticEvent` variant for a new event rather than a free-form message.
Fields that hold free text may only be filled from `EditorError`'s `Display`
output.

## Licence

By contributing, you agree that your contribution is licensed under the MIT
license and the Apache License, Version 2.0, as described in the
[README](README.md#license).
